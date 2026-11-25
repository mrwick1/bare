use crate::bi_buffer::{buffer_slot, instance_proto, range, relative, species_ctor, construct, to_index};
use crate::builtins::arg;
use crate::interp_rt::{ErrKind, R, Realm};
use crate::lazy::buffer_ctor;
use crate::object::{K_BUFFER, Obj};
use crate::value::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, atomic::{AtomicBool, Ordering}};
use std::time::{Duration, Instant};

pub(crate) const SHARED_BUFFER: i32 = -3;
static GLOBALS: &[crate::lazy::Ent] = &[buffer_ctor("SharedArrayBuffer", 1, SHARED_BUFFER)];

/// A backing block can outlive any one agent's heap.
#[derive(Clone)]
pub struct SharedBuffer(Arc<Block>);

struct Block {
    max: Option<usize>,
    state: Mutex<BlockState>,
    changed: Condvar,
}

struct BlockState {
    bytes: Vec<u8>,
    waiters: HashMap<usize, VecDeque<Arc<Ticket>>>,
}

impl SharedBuffer {
    fn new(len: usize, max: Option<usize>) -> Option<Self> {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(len).ok()?;
        bytes.resize(len, 0);
        Some(Self(Arc::new(Block {
            max,
            state: Mutex::new(BlockState { bytes, waiters: HashMap::new() }),
            changed: Condvar::new(),
        })))
    }

    pub(crate) fn len(&self) -> usize {
        self.0.state.lock().unwrap().bytes.len()
    }

    pub(crate) fn read<T>(&self, read: impl FnOnce(&[u8]) -> T) -> T {
        read(&self.0.state.lock().unwrap().bytes)
    }

    pub(crate) fn write<T>(&self, write: impl FnOnce(&mut [u8]) -> T) -> T {
        write(&mut self.0.state.lock().unwrap().bytes)
    }

    pub(crate) fn copy(&self, from: usize, target: &Self, to: usize, len: usize) {
        let source_id = Arc::as_ptr(&self.0) as usize;
        let target_id = Arc::as_ptr(&target.0) as usize;
        match source_id.cmp(&target_id) {
            std::cmp::Ordering::Equal => self.write(|bytes| bytes.copy_within(from..from + len, to)),
            std::cmp::Ordering::Less => self.read(|src| target.write(|dst| dst[to..to + len].copy_from_slice(&src[from..from + len]))),
            std::cmp::Ordering::Greater => target.write(|dst| self.read(|src| dst[to..to + len].copy_from_slice(&src[from..from + len]))),
        }
    }

    pub(crate) fn same_block(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub(crate) fn wait(&self, offset: usize, expected: &[u8], timeout: Option<Duration>, control: Option<&Test262AgentControl>) -> Option<&'static str> {
        let mut state = self.0.state.lock().unwrap();
        if state.bytes[offset..offset + expected.len()] != *expected { return Some("not-equal") }
        if timeout == Some(Duration::ZERO) { return Some("timed-out") }
        if let Some(control) = control {
            *control.0.blocked.lock().unwrap() = Some(self.clone());
        }
        let ticket = Arc::new(Ticket { ready: AtomicBool::new(false), signal: None });
        state.waiters.entry(offset).or_default().push_back(ticket.clone());
        let start = Instant::now();
        let result = loop {
            if control.is_some_and(Test262AgentControl::is_stopped) { break None; }
            if ticket.ready.load(Ordering::Relaxed) { break Some("ok"); }
            if let Some(timeout) = timeout {
                let Some(left) = timeout.checked_sub(start.elapsed()) else { break Some("timed-out") };
                state = self.0.changed.wait_timeout(state, left).unwrap().0;
            } else {
                state = self.0.changed.wait(state).unwrap();
            }
        };
        Pending::remove(&mut state, offset, &ticket);
        if let Some(control) = control { *control.0.blocked.lock().unwrap() = None; }
        result
    }

    fn notify(&self, offset: usize, count: usize, mut on_notify: impl FnMut(&Arc<Ticket>)) -> usize {
        let mut state = self.0.state.lock().unwrap();
        let Some(queue) = state.waiters.get_mut(&offset) else { return 0 };
        let n = count.min(queue.len());
        let mut wake_hosts: Vec<fn()> = Vec::new();
        for ticket in queue.drain(..n) {
            ticket.ready.store(true, Ordering::Release);
            if let Some(signal) = &ticket.signal {
                signal.wake();
                if let Some(wake) = signal.wake_host
                    && !wake_hosts.iter().any(|&other| std::ptr::fn_addr_eq(wake, other))
                {
                    wake_hosts.push(wake);
                }
            }
            on_notify(&ticket);
        }
        if queue.is_empty() { state.waiters.remove(&offset); }
        if n != 0 { self.0.changed.notify_all(); }
        drop(state);
        for wake in wake_hosts { wake(); }
        n
    }
}

#[derive(Clone, Default)]
pub struct Test262AgentControl(Arc<Control>);

#[derive(Default)]
struct Control {
    stopped: AtomicBool,
    blocked: Mutex<Option<SharedBuffer>>,
}

impl Test262AgentControl {
    pub fn stop(&self) {
        self.0.stopped.store(true, Ordering::Release);
        let block = self.0.blocked.lock().unwrap().clone();
        if let Some(block) = block {
            let _state = block.0.state.lock().unwrap();
            block.0.changed.notify_all();
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.0.stopped.load(Ordering::Acquire)
    }
}

struct Ticket {
    ready: AtomicBool,
    signal: Option<Arc<Signal>>,
}

#[derive(Default)]
struct Signal {
    wake_host: Option<fn()>,
    version: Mutex<u64>,
    changed: Condvar,
}

impl Signal {
    fn wake(&self) {
        let mut version = self.version.lock().unwrap();
        *version = version.wrapping_add(1);
        self.changed.notify_all();
    }
}

pub(crate) struct AsyncWaits {
    signal: Arc<Signal>,
    pending: Vec<Pending>,
}

struct Pending {
    buffer: SharedBuffer,
    offset: usize,
    ticket: Arc<Ticket>,
    started: Instant,
    timeout: Option<Duration>,
    promise: Value,
}

impl Pending {
    fn status(&self) -> Option<&'static str> {
        let mut state = self.buffer.0.state.lock().unwrap();
        if self.ticket.ready.load(Ordering::Relaxed) { return Some("ok") }
        if self.timeout.is_some_and(|t| self.started.elapsed() >= t) {
            Self::remove(&mut state, self.offset, &self.ticket);
            return Some("timed-out");
        }
        None
    }

    fn remove(state: &mut BlockState, offset: usize, ticket: &Arc<Ticket>) {
        if let Some(queue) = state.waiters.get_mut(&offset) {
            queue.retain(|t| !Arc::ptr_eq(t, ticket));
            if queue.is_empty() { state.waiters.remove(&offset); }
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        let mut state = self.buffer.0.state.lock().unwrap();
        Self::remove(&mut state, self.offset, &self.ticket);
    }
}

impl AsyncWaits {
    pub(crate) fn mark(&self, marker: &mut crate::gc::Marker) {
        for entry in &self.pending { marker.value(entry.promise); }
    }
}

impl Realm {
    pub(crate) fn wait_async(&mut self, buffer: SharedBuffer, offset: usize, expected: &[u8], timeout: Option<Duration>) -> R<Value> {
        let mut state = buffer.0.state.lock().unwrap();
        let immediate = if state.bytes[offset..offset + expected.len()] != *expected {
            Some("not-equal")
        } else if timeout == Some(Duration::ZERO) {
            Some("timed-out")
        } else {
            None
        };
        let (async_result, value) = if let Some(immediate) = immediate {
            drop(state);
            (false, self.string(immediate))
        } else {
            let shared = self.shared_memory.as_mut().unwrap();
            let waits = shared.waits.get_or_insert_with(|| AsyncWaits {
                signal: Arc::new(Signal { wake_host: shared.wake_host, ..Default::default() }),
                pending: Vec::new(),
            });
            let ticket = Arc::new(Ticket { ready: AtomicBool::new(false), signal: Some(waits.signal.clone()) });
            state.waiters.entry(offset).or_default().push_back(ticket.clone());
            drop(state);
            let promise = self.new_promise().value();
            self.shared_memory.as_mut().unwrap().waits.as_mut().unwrap().pending.push(Pending {
                buffer, offset, ticket, started: Instant::now(), timeout, promise,
            });
            (true, promise)
        };
        let result = crate::bi_buffer::plain_proto(self, self.object_proto);
        self.set_own(result, "async", Value::bool(async_result));
        self.set_own(result, "value", value);
        Ok(result.value())
    }

    /// Time until a pending async wait is ready, or None if there is no deadline.
    pub fn shared_wait_delay(&self) -> Option<Duration> {
        let waits = self.shared_memory.as_ref()?.waits.as_ref()?;
        waits.pending.iter().filter_map(|p| {
            if p.ticket.ready.load(Ordering::Acquire) {
                Some(Duration::ZERO)
            } else {
                p.timeout.map(|timeout| timeout.saturating_sub(p.started.elapsed()))
            }
        }).min()
    }

    /// Deliver one event on the owning agent. Call with zero budget to avoid blocking.
    pub fn deliver_shared_wait(&mut self, budget: Duration) -> R<bool> {
        let Some(waits) = self.shared_memory.as_mut().and_then(|shared| shared.waits.as_mut()) else {
            return Ok(false);
        };
        if waits.pending.is_empty() { return Ok(false) }
        let started = Instant::now();
        let next = loop {
            // Record the version before inspecting the backing blocks. No signal
            // lock is held while taking a block lock.
            let before = *waits.signal.version.lock().unwrap();
            if let Some((i, status)) = waits.pending.iter().enumerate().find_map(|(i, p)| p.status().map(|s| (i, s))) {
                break Some((waits.pending.remove(i).promise, status));
            }
            let Some(left) = budget.checked_sub(started.elapsed()) else { break None };
            let delay = waits.pending.iter().filter_map(|p| p.timeout.map(|t| t.saturating_sub(p.started.elapsed()))).fold(left, Duration::min);
            let version = waits.signal.version.lock().unwrap();
            if *version == before {
                drop(waits.signal.changed.wait_timeout(version, delay).unwrap());
            }
        };
        let Some((promise, status)) = next else { return Ok(false) };
        self.resolve_wait(promise, status)?;
        Ok(true)
    }

    pub(crate) fn notify_shared_waiters(&mut self, object: Obj, offset: usize, count: usize) -> R<usize> {
        let Some(buffer) = self.shared_buffer(object) else { return Ok(0) };
        let own_signal = self.shared_memory.as_ref().unwrap().waits.as_ref().map(|waits| &waits.signal);
        let mut local = Vec::new();
        let notified = buffer.notify(offset, count, |ticket| {
            if ticket.signal.as_ref().zip(own_signal).is_some_and(|(a, b)| Arc::ptr_eq(a, b)) {
                local.push(ticket.clone());
            }
        });
        // Same-agent notifications queue promise reactions before Atomics.notify returns.
        // Release the block lock before resolving any promise.
        for ticket in local {
            let pending = &mut self.shared_memory.as_mut().unwrap().waits.as_mut().unwrap().pending;
            let index = pending.iter().position(|p| Arc::ptr_eq(&p.ticket, &ticket)).expect("local waiter");
            let promise = pending.remove(index).promise;
            self.resolve_wait(promise, "ok")?;
        }
        Ok(notified)
    }

    fn resolve_wait(&mut self, promise: Value, status: &'static str) -> R<()> {
        let value = self.string(status);
        self.resolve_promise(Self::obj(promise).expect("pending promise"), value)?;
        Ok(())
    }
}

pub(crate) type SharedBlocks = HashMap<u64, (Value, SharedBuffer)>;

#[derive(Default)]
pub(crate) struct SharedMemory {
    pub(crate) blocks: SharedBlocks,
    pub(crate) waits: Option<AsyncWaits>,
    pub(crate) can_block: bool,
    pub(crate) control: Option<Test262AgentControl>,
    wake_host: Option<fn()>,
}

pub(crate) fn is_shared(object: Obj) -> bool {
    object.is_kind(K_BUFFER) && buffer_slot(object, 0) == Value::TRUE
}

impl Realm {
    pub fn install_test262_shared_memory(&mut self, can_block: bool) {
        self.install_shared_memory(can_block, None);
    }

    /// Enable shared memory for an agent the host has authorized. The wake hook
    /// runs outside the backing lock and must only signal the host event loop.
    pub fn install_shared_memory(&mut self, can_block: bool, wake_host: Option<fn()>) {
        if self.shared_memory.is_none() {
            self.shared_memory = Some(Box::new(SharedMemory { can_block, wake_host, ..Default::default() }));
            self.install_shared_globals();
        }
    }

    pub(crate) fn install_shared_globals(&mut self) {
        if self.shared_memory.is_some() {
            self.lazy_table(self.global, GLOBALS);
            crate::bi_atomics::install(self);
        }
    }

    pub fn set_test262_agent_control(&mut self, control: Test262AgentControl) {
        self.shared_memory.as_mut().expect("shared memory installed").control = Some(control);
    }

    pub fn install_test262_agent_method(&mut self, name: &'static str, len: u8, callback: crate::builtins::NativeFn) -> R<()> {
        let host = self.get_named(self.global.value(), "$262")?;
        let agent = self.get_named(host, "agent")?;
        let Some(agent) = Self::obj(agent) else { return self.type_error("Test262 agent object is missing") };
        self.method(agent, name, len, callback);
        Ok(())
    }

    pub fn exported_shared_buffer(&self, value: Value) -> Option<SharedBuffer> {
        Self::obj(value).and_then(|o| self.shared_buffer(o)).cloned()
    }

    pub fn import_shared_buffer(&mut self, buffer: SharedBuffer) -> R<Value> {
        if self.shared_memory.is_none() {
            return self.type_error("shared memory is unavailable in this agent");
        }
        let proto = self.buffer_intrinsic("SharedArrayBuffer");
        self.wrap_shared_buffer(proto, buffer)
    }

    pub(crate) fn shared_buffer(&self, o: Obj) -> Option<&SharedBuffer> {
        self.shared_memory.as_ref()?.blocks.get(&o.value().bits()).map(|(_, b)| b)
    }

    fn wrap_shared_buffer(&mut self, proto: Obj, buffer: SharedBuffer) -> R<Value> {
        let max = buffer.0.max.map_or(Value::UNDEFINED, |n| Value::int(n as i32));
        let len = Value::int(buffer.len() as i32);
        let object = self.buffer_object(proto, K_BUFFER, &[Value::TRUE, len, max])?.value();
        self.shared_memory.as_mut().expect("shared memory installed").blocks.insert(object.bits(), (object, buffer));
        Ok(object)
    }
}

fn shared(rt: &mut Realm, value: Value) -> R<SharedBuffer> {
    rt.exported_shared_buffer(value).ok_or_else(|| rt.error(ErrKind::Type, "receiver is not a SharedArrayBuffer"))
}

pub(crate) fn constructor(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    if rt.nt.is_undefined() { return rt.type_error("SharedArrayBuffer requires new") }
    let len = to_index(rt, arg(args, 0))?;
    let options = arg(args, 1);
    let option = if Realm::obj(options).is_some() { rt.get_named(options, "maxByteLength")? } else { Value::UNDEFINED };
    let max = if option.is_undefined() { None } else { Some(to_index(rt, option)?) };
    if max.is_some_and(|n| n < len) {
        return range(rt, "invalid SharedArrayBuffer length");
    }
    let proto = instance_proto(rt)?;
    if len > i32::MAX as usize || max.is_some_and(|n| n > i32::MAX as usize) {
        return range(rt, "invalid SharedArrayBuffer length");
    }
    let Some(buffer) = SharedBuffer::new(len, max) else { return range(rt, "out of memory") };
    rt.wrap_shared_buffer(proto, buffer)
}

pub(crate) fn byte_length(rt: &mut Realm, this: Value, _: &[Value]) -> R<Value> {
    Ok(Realm::num(shared(rt, this)?.len() as f64))
}

pub(crate) fn max_byte_length(rt: &mut Realm, this: Value, _: &[Value]) -> R<Value> {
    let b = shared(rt, this)?;
    Ok(Realm::num(b.0.max.unwrap_or_else(|| b.len()) as f64))
}

pub(crate) fn growable(rt: &mut Realm, this: Value, _: &[Value]) -> R<Value> {
    Ok(Value::bool(shared(rt, this)?.0.max.is_some()))
}

pub(crate) fn grow(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let buffer = shared(rt, this)?;
    let Some(max) = buffer.0.max else { return rt.type_error("SharedArrayBuffer is fixed-length") };
    let len = to_index(rt, arg(args, 0))?;
    let mut state = buffer.0.state.lock().unwrap();
    let old = state.bytes.len();
    if len < old || len > max { return range(rt, "invalid SharedArrayBuffer length") }
    if state.bytes.try_reserve_exact(len - old).is_err() { return range(rt, "out of memory") }
    state.bytes.resize(len, 0);
    Ok(Value::UNDEFINED)
}

pub(crate) fn slice(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let source = shared(rt, this)?;
    let len = source.len();
    let start = relative(rt, arg(args, 0), len, 0)?;
    let end = relative(rt, arg(args, 1), len, len)?;
    let count = end.saturating_sub(start);
    let ctor = species_ctor(rt, this)?;
    let result = if ctor.is_undefined() {
        let Some(block) = SharedBuffer::new(count, None) else { return range(rt, "out of memory") };
        rt.import_shared_buffer(block)?
    } else {
        construct(rt, ctor, &[Realm::num(count as f64)])?
    };
    let target = shared(rt, result)?;
    if source.same_block(&target) || target.len() < count {
        return rt.type_error("invalid SharedArrayBuffer species result");
    }
    source.copy(start, &target, 0, count);
    Ok(result)
}

#[cfg(test)]
#[path = "shared_memory_tests.rs"]
mod tests;
