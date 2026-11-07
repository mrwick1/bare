//! A default `ReadableStream` made by script: `new ReadableStream({start, pull, cancel}, {highWaterMark})`, its
//! controller (`enqueue`, `close`, `error`, `desiredSize`) and the default reader (`read`, `cancel`, `releaseLock`).
//!
//! How it works
//! - The algorithms follow the Streams Standard for a default stream with the built-in "one chunk, size 1" strategy:
//!   queue, `started`, `pulling`, `pullAgain`, `closeRequested`, read requests, `ShouldCallPull`. The state of every stream,
//!   controller and reader is Rust data keyed by its JS object (`State`). `Host::trace` reports what each object keeps
//!   (queue chunks, source callbacks, pending read promises, the stream and reader links), `Host::prune` drops the entries of dead
//!   objects. A handle is an `ObjId`, which the heap checks by generation, so a dead owner never aliases a new one.
//! - Promise reactions use the evaluator's own job queue: `Interp::host_promise_react` does `PromiseResolve` and
//!   `PerformPromiseThen` with host functions bound to the controller (`Interp::host_bind`). There is no second microtask
//!   queue, and no script-visible `Promise`, `then` or `bind` is read, so a script that deletes or replaces them changes nothing.
//! - User code (`start`, `pull`, `cancel`, an object's `constructor` getter) runs with no `RefCell` borrow held. Every algorithm
//!   copies what it needs out of `State`, drops the borrow, then calls out, then borrows again and re-checks.
//! - A callback that throws follows the standard: `start` throws out of the constructor, `pull` and `cancel` turn into a
//!   rejected promise (`pull`: the stream errors; `cancel`: `cancel()` rejects).
//!
//! Limits (`StreamLimits`) end the call with `Error::Limit(LimitKind::Host(name))`, never a JS exception: live streams,
//! live readers, queued chunks of one stream, queued bytes of all streams (the length of `ArrayBuffer` and typed-array chunks;
//! other values count as zero bytes but one chunk), pending read requests of one reader, and source callbacks in the life of
//! the browser. Work is charged to the step budget (a chunk costs one step and one step for each 64 bytes). After any limit,
//! `Unsupported` or heap fault, the stream layer is failed for good: each later entry point returns the same error and no
//! callback runs. Objects count until the embedder collects them (`Browser::collect`): the VM never collects by itself.
//!
//! Gates (explicit `Unsupported`, nothing is faked): `type: "bytes"`, `autoAllocateChunkSize`, a `size` strategy function,
//! BYOB readers, `tee`, `pipeTo`, `pipeThrough`, `reader.closed` and subclassing. `new ReadableStreamDefaultReader(stream)` is an
//! illegal constructor (use `getReader()`). A stream whose `highWaterMark` is not 1 works (any number from 0 up to `Infinity`).
//! Deviation: a stream made without `pull` never starts a pull cycle (nothing could observe it).
//! `response.body` is a different object (see `browser_fetch.rs`): the methods on the prototypes dispatch on which one `this` is.
//!
//! Node oracle for the semantics: `tools/js/streams_oracle.mjs` over `tools/fixtures/js/streams-cases.txt`.

use crate::browser::{Browser, Dom, IdMap, R, illegal_constructor, unsupported};
use bare_js::eval::Error;
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::VecDeque;

/// Hard bounds of the stream layer. Each is checked before the thing grows.
#[derive(Clone, Copy, Debug)]
pub struct StreamLimits {
    /// Live streams (each has one controller).
    pub max_streams: usize,
    /// Live readers, released ones too, until they are collected.
    pub max_readers: usize,
    /// Queued chunks of one stream.
    pub max_queue_chunks: usize,
    /// Bytes of queued `ArrayBuffer` and typed-array chunks, all streams together.
    pub max_queued_bytes: usize,
    /// Pending read requests of one reader.
    pub max_pending_reads: usize,
    /// Calls of `start`, `pull` and `cancel` in the life of the browser.
    pub max_callbacks: u64,
}

impl Default for StreamLimits {
    fn default() -> Self {
        StreamLimits { max_streams: 4096, max_readers: 4096, max_queue_chunks: 1 << 14, max_queued_bytes: 64 << 20, max_pending_reads: 1024, max_callbacks: 1 << 22 }
    }
}

/// Counters for tests and the load tool.
#[derive(Clone, Copy, Debug, Default)]
pub struct StreamStats {
    pub streams: usize,
    pub readers: usize,
    pub queued_chunks: usize,
    pub queued_bytes: usize,
    pub pending_reads: usize,
    pub callbacks: u64,
}

// ---------------------------------------------------------------------------------------------- state

enum St {
    Readable,
    Closed,
    Errored(Value),
}

struct Stream {
    st: St,
    ctl: ObjId,
    reader: Option<ObjId>,
}

struct Ctl {
    stream: ObjId,
    source: Value,
    start: Value,
    pull: Value,
    cancel: Value,
    /// Chunks with the byte cost that was charged for each.
    queue: VecDeque<(Value, usize)>,
    qbytes: usize,
    hwm: f64,
    started: bool,
    pulling: bool,
    pull_again: bool,
    close_requested: bool,
    /// The bound reactions of `pull`, made on the first pull.
    pull_fns: Option<(ObjId, ObjId)>,
}

struct Rdr {
    /// `None` after `releaseLock`.
    stream: Option<ObjId>,
    /// Pending read promises, oldest first.
    reads: VecDeque<Value>,
}

#[derive(Default)]
struct State {
    streams: IdMap<ObjId, Stream>,
    ctls: IdMap<ObjId, Ctl>,
    readers: IdMap<ObjId, Rdr>,
    queued_bytes: usize,
    callbacks: u64,
    fatal: Option<Error>,
}

/// The templates of the host reactions. `this` of each bound copy is the controller, or the promise of a `cancel()` call.
#[derive(Clone, Copy)]
struct Fns {
    start_ok: ObjId,
    start_err: ObjId,
    pull_ok: ObjId,
    pull_err: ObjId,
    cancel_ok: ObjId,
    cancel_err: ObjId,
    stream_proto: ObjId,
    reader_proto: ObjId,
    ctl_proto: ObjId,
}

/// The stream state of one `Dom`.
#[derive(Default)]
pub(crate) struct Streams {
    st: RefCell<State>,
    lim: Cell<StreamLimits>,
    fns: OnceCell<Fns>,
}

impl Streams {
    pub(crate) fn trace(&self, owner: ObjId, out: &mut Vec<Value>) {
        let st = self.st.borrow();
        if let Some(s) = st.streams.get(&owner) {
            out.push(Value::Object(s.ctl));
            out.extend(s.reader.map(Value::Object));
            if let St::Errored(e) = s.st {
                out.push(e);
            }
        } else if let Some(c) = st.ctls.get(&owner) {
            out.push(Value::Object(c.stream));
            out.extend([c.source, c.start, c.pull, c.cancel]);
            out.extend(c.queue.iter().map(|q| q.0));
            if let Some((a, b)) = c.pull_fns {
                out.extend([Value::Object(a), Value::Object(b)]);
            }
        } else if let Some(r) = st.readers.get(&owner) {
            out.extend(r.stream.map(Value::Object));
            out.extend(r.reads.iter().copied());
        }
    }

    pub(crate) fn prune(&self, live: &dyn Fn(ObjId) -> bool) {
        let mut st = self.st.borrow_mut();
        st.streams.retain(|&o, _| live(o));
        st.ctls.retain(|&o, _| live(o));
        st.readers.retain(|&o, _| live(o));
        st.queued_bytes = st.ctls.values().map(|c| c.qbytes).sum();
    }

    pub(crate) fn roots(&self, out: &mut Vec<Value>) {
        if let Some(f) = self.fns.get() {
            out.extend([f.start_ok, f.start_err, f.pull_ok, f.pull_err, f.cancel_ok, f.cancel_err, f.ctl_proto].map(Value::Object));
        }
    }
}

fn lim(d: &Dom) -> StreamLimits {
    d.streams.lim.get()
}

fn fns(d: &Dom) -> Fns {
    *d.streams.fns.get().expect("installed")
}

/// Run an entry point: refuse after a fatal error, and make a non-JS error fatal for good.
fn enter<T>(d: &Dom, f: impl FnOnce() -> R<T>) -> R<T> {
    if let Some(e) = d.streams.st.borrow().fatal {
        return Err(e);
    }
    let r = f();
    if let Err(e) = &r && !matches!(e, Error::Throw(_)) {
        d.streams.st.borrow_mut().fatal = Some(*e);
    }
    r
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

fn prop(d: &Dom, it: &mut Interp<'_>, o: Value, name: &str) -> R<Value> {
    let k = d.key(it, name)?;
    it.get_value(o, k)
}

fn thrown(e: Error) -> R<Value> {
    match e {
        Error::Throw(v) => Ok(v),
        e => Err(e),
    }
}

fn type_value(it: &mut Interp<'_>, msg: &str) -> R<Value> {
    let e = it.type_error(msg);
    thrown(e)
}

fn iter_result(d: &Dom, it: &mut Interp<'_>, value: Value, done: bool) -> R<Value> {
    it.charge(1)?;
    let o = d.obj(it, it.object_prototype())?;
    let (kv, kd) = (d.key(it, "value")?, d.key(it, "done")?);
    it.define_data(o, kv, value, true, true, true)?;
    it.define_data(o, kd, Value::Bool(done), true, true, true)?;
    Ok(Value::Object(o))
}

fn resolve_with(d: &Dom, it: &mut Interp<'_>, p: Value, value: Value, done: bool) -> R<()> {
    let r = iter_result(d, it, value, done)?;
    it.host_resolve_promise(p, r)
}

/// Call a user callback of the source. `Ok(Err(v))` is a JS exception. Counted and charged.
fn invoke(d: &Dom, it: &mut Interp<'_>, f: Value, this: Value, args: &[Value]) -> R<Result<Value, Value>> {
    {
        let mut st = d.streams.st.borrow_mut();
        if st.callbacks >= lim(d).max_callbacks {
            return Err(d.lim("stream-callbacks"));
        }
        st.callbacks += 1;
    }
    it.charge(2)?;
    match it.call(f, this, args) {
        Ok(v) => Ok(Ok(v)),
        Err(Error::Throw(v)) => Ok(Err(v)),
        Err(e) => Err(e),
    }
}

/// React to the outcome of a promise-returning callback. A throw becomes a rejected promise, as the standard says.
fn react(it: &mut Interp<'_>, r: Result<Value, Value>, ok: ObjId, err: ObjId) -> R<()> {
    let x = match r {
        Ok(v) => v,
        Err(e) => {
            let p = it.host_new_promise()?;
            it.host_reject_promise(p, e)?;
            p
        }
    };
    it.host_promise_react(x, Value::Object(ok), Value::Object(err))
}

// ---------------------------------------------------------------------------------------------- algorithms

fn should_pull(st: &State, c: &Ctl) -> bool {
    let Some(s) = st.streams.get(&c.stream) else { return false };
    if !matches!(s.st, St::Readable) || c.close_requested || !c.started {
        return false;
    }
    if let Some(r) = s.reader.and_then(|r| st.readers.get(&r))
        && !r.reads.is_empty()
    {
        return true;
    }
    c.hwm - c.queue.len() as f64 > 0.0
}

/// `ReadableStreamDefaultControllerCallPullIfNeeded`.
fn pull_if_needed(d: &Dom, it: &mut Interp<'_>, ctl: ObjId) -> R<()> {
    let (pull, source, cached) = {
        let mut st = d.streams.st.borrow_mut();
        let Some(c) = st.ctls.get(&ctl) else { return Ok(()) };
        // A stream with no `pull` can never start a cycle that anyone could see.
        if matches!(c.pull, Value::Undefined) || !should_pull(&st, c) {
            return Ok(());
        }
        let c = st.ctls.get_mut(&ctl).expect("checked");
        if c.pulling {
            c.pull_again = true;
            return Ok(());
        }
        c.pulling = true;
        (c.pull, c.source, c.pull_fns)
    };
    let (ok, err) = match cached {
        Some(p) => p,
        None => {
            let f = fns(d);
            let p = (it.host_bind(f.pull_ok, Value::Object(ctl))?, it.host_bind(f.pull_err, Value::Object(ctl))?);
            if let Some(c) = d.streams.st.borrow_mut().ctls.get_mut(&ctl) {
                c.pull_fns = Some(p);
            }
            p
        }
    };
    let r = invoke(d, it, pull, source, &[Value::Object(ctl)])?;
    match react(it, r, ok, err) {
        Err(Error::Throw(v)) => controller_error(d, it, ctl, v),
        r => r,
    }
}

fn clear_algorithms(c: &mut Ctl) {
    c.source = Value::Undefined;
    c.start = Value::Undefined;
    c.pull = Value::Undefined;
    c.cancel = Value::Undefined;
    c.pull_fns = None;
}

fn reset_queue(st: &mut State, ctl: ObjId) {
    if let Some(c) = st.ctls.get_mut(&ctl) {
        st.queued_bytes -= c.qbytes;
        c.qbytes = 0;
        c.queue.clear();
    }
}

/// Make the stream closed and take its pending read requests.
fn close_stream(st: &mut State, stream: ObjId) -> Vec<Value> {
    let Some(s) = st.streams.get_mut(&stream) else { return Vec::new() };
    s.st = St::Closed;
    match s.reader.and_then(|r| st.readers.get_mut(&r)) {
        Some(r) => r.reads.drain(..).collect(),
        None => Vec::new(),
    }
}

/// `ClearAlgorithms`, then `ReadableStreamClose`, then settle the pending reads with `done`.
fn finish_close(d: &Dom, it: &mut Interp<'_>, ctl: ObjId) -> R<()> {
    let reads = {
        let mut st = d.streams.st.borrow_mut();
        let Some(c) = st.ctls.get_mut(&ctl) else { return Ok(()) };
        clear_algorithms(c);
        let s = c.stream;
        close_stream(&mut st, s)
    };
    for p in reads {
        resolve_with(d, it, p, Value::Undefined, true)?;
    }
    Ok(())
}

/// `ReadableStreamDefaultControllerError`.
fn controller_error(d: &Dom, it: &mut Interp<'_>, ctl: ObjId, e: Value) -> R<()> {
    let reads = {
        let mut st = d.streams.st.borrow_mut();
        let Some(c) = st.ctls.get(&ctl) else { return Ok(()) };
        let sid = c.stream;
        if !st.streams.get(&sid).is_some_and(|s| matches!(s.st, St::Readable)) {
            return Ok(());
        }
        reset_queue(&mut st, ctl);
        if let Some(c) = st.ctls.get_mut(&ctl) {
            clear_algorithms(c);
        }
        let s = st.streams.get_mut(&sid).expect("checked");
        s.st = St::Errored(e);
        match s.reader.and_then(|r| st.readers.get_mut(&r)) {
            Some(r) => r.reads.drain(..).collect::<Vec<_>>(),
            None => Vec::new(),
        }
    };
    for p in reads {
        it.host_reject_promise(p, e)?;
    }
    Ok(())
}

/// `ReadableStreamCancel`. Returns the promise `cancel()` gives.
fn cancel_stream(d: &Dom, it: &mut Interp<'_>, stream: ObjId, reason: Value) -> R<Value> {
    enum C {
        Closed,
        Errored(Value),
        Go { cancel: Value, source: Value, reads: Vec<Value> },
    }
    let c = {
        let mut st = d.streams.st.borrow_mut();
        match st.streams.get(&stream).map(|s| (&s.st, s.ctl)) {
            None => C::Closed,
            Some((St::Closed, _)) => C::Closed,
            Some((St::Errored(e), _)) => C::Errored(*e),
            Some((St::Readable, ctl)) => {
                let reads = close_stream(&mut st, stream);
                reset_queue(&mut st, ctl);
                let c = st.ctls.get_mut(&ctl).expect("a stream has a controller");
                let (cancel, source) = (c.cancel, c.source);
                clear_algorithms(c);
                C::Go { cancel, source, reads }
            }
        }
    };
    let p = it.host_new_promise()?;
    match c {
        C::Closed => it.host_resolve_promise(p, Value::Undefined)?,
        C::Errored(e) => it.host_reject_promise(p, e)?,
        C::Go { cancel, source, reads } => {
            for r in reads {
                resolve_with(d, it, r, Value::Undefined, true)?;
            }
            let r = if matches!(cancel, Value::Undefined) { Ok(Value::Undefined) } else { invoke(d, it, cancel, source, &[reason])? };
            let f = fns(d);
            let (ok, err) = (it.host_bind(f.cancel_ok, p)?, it.host_bind(f.cancel_err, p)?);
            match react(it, r, ok, err) {
                Err(Error::Throw(v)) => it.host_reject_promise(p, v)?,
                r => r?,
            }
        }
    }
    Ok(p)
}

// ---------------------------------------------------------------------------------------------- reactions

fn this_obj(this: Value) -> Option<ObjId> {
    match this {
        Value::Object(o) => Some(o),
        _ => None,
    }
}

fn start_ok(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    enter(d, || {
        let Some(ctl) = this_obj(this) else { return Ok(Value::Undefined) };
        if let Some(c) = d.streams.st.borrow_mut().ctls.get_mut(&ctl) {
            c.started = true;
        }
        pull_if_needed(d, it, ctl)?;
        Ok(Value::Undefined)
    })
}

fn error_reaction(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    enter(d, || {
        if let Some(ctl) = this_obj(this) {
            controller_error(d, it, ctl, arg(a, 0))?;
        }
        Ok(Value::Undefined)
    })
}

fn pull_ok(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    enter(d, || {
        let Some(ctl) = this_obj(this) else { return Ok(Value::Undefined) };
        let again = match d.streams.st.borrow_mut().ctls.get_mut(&ctl) {
            Some(c) => {
                c.pulling = false;
                std::mem::take(&mut c.pull_again)
            }
            None => false,
        };
        if again {
            pull_if_needed(d, it, ctl)?;
        }
        Ok(Value::Undefined)
    })
}

fn cancel_ok(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    enter(d, || {
        it.host_resolve_promise(this, Value::Undefined)?;
        Ok(Value::Undefined)
    })
}

fn cancel_err(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    enter(d, || {
        it.host_reject_promise(this, arg(a, 0))?;
        Ok(Value::Undefined)
    })
}

// ---------------------------------------------------------------------------------------------- constructor

fn callback_option(d: &Dom, it: &mut Interp<'_>, src: Value, name: &str) -> R<Value> {
    let v = prop(d, it, src, name)?;
    if !matches!(v, Value::Undefined) && !it.is_callable_value(v) {
        return Err(it.type_error(&format!("Failed to construct 'ReadableStream': The {name} member is not a function")));
    }
    Ok(v)
}

pub(crate) fn ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    enter(d, || construct(d, it, a))
}

fn construct(d: &Dom, it: &mut Interp<'_>, a: &[Value]) -> R<Value> {
    let (src, strategy) = (arg(a, 0), arg(a, 1));
    let (mut start, mut pull, mut cancel) = (Value::Undefined, Value::Undefined, Value::Undefined);
    if !matches!(src, Value::Undefined) {
        if !matches!(src, Value::Object(_)) {
            return Err(it.type_error("Failed to construct 'ReadableStream': The underlying source is not an object"));
        }
        // Dictionary members are read in alphabetical order, as WebIDL does.
        if !matches!(prop(d, it, src, "autoAllocateChunkSize")?, Value::Undefined) {
            return Err(Error::Unsupported(unsupported("ReadableStream autoAllocateChunkSize (byte streams)")));
        }
        cancel = callback_option(d, it, src, "cancel")?;
        pull = callback_option(d, it, src, "pull")?;
        start = callback_option(d, it, src, "start")?;
        let ty = prop(d, it, src, "type")?;
        if !matches!(ty, Value::Undefined) {
            let s = it.to_js_string(ty)?;
            let s = it.heap.to_string_lossy(s).unwrap_or_default();
            if s == "bytes" {
                return Err(Error::Unsupported(unsupported("ReadableStream type: \"bytes\" (byte streams and BYOB)")));
            }
            return Err(it.type_error("Failed to construct 'ReadableStream': The provided value is not a valid enum value of type ReadableStreamType"));
        }
    }
    let mut hwm = 1.0;
    if !matches!(strategy, Value::Undefined | Value::Null) {
        if !matches!(strategy, Value::Object(_)) {
            return Err(it.type_error("Failed to construct 'ReadableStream': The queuing strategy is not an object"));
        }
        let h = prop(d, it, strategy, "highWaterMark")?;
        if !matches!(h, Value::Undefined) {
            hwm = it.to_js_number(h)?;
            if hwm.is_nan() || hwm < 0.0 {
                return Err(it.range_error("Failed to construct 'ReadableStream': Invalid highWaterMark"));
            }
        }
        let size = prop(d, it, strategy, "size")?;
        if !matches!(size, Value::Undefined) {
            if !it.is_callable_value(size) {
                return Err(it.type_error("Failed to construct 'ReadableStream': The size member is not a function"));
            }
            return Err(Error::Unsupported(unsupported("ReadableStream strategy.size (custom queuing strategy)")));
        }
    }
    let l = lim(d);
    {
        let st = d.streams.st.borrow();
        if st.streams.len() >= l.max_streams {
            return Err(d.lim("stream-objects"));
        }
    }
    it.charge(4)?;
    let f = fns(d);
    let (s, c) = (d.obj(it, f.stream_proto)?, d.obj(it, f.ctl_proto)?);
    let (ok, err) = (it.host_bind(f.start_ok, Value::Object(c))?, it.host_bind(f.start_err, Value::Object(c))?);
    {
        let mut st = d.streams.st.borrow_mut();
        st.streams.insert(s, Stream { st: St::Readable, ctl: c, reader: None });
        st.ctls.insert(c, Ctl { stream: s, source: src, start, pull, cancel, queue: VecDeque::new(), qbytes: 0, hwm, started: false, pulling: false, pull_again: false, close_requested: false, pull_fns: None });
    }
    let started = if matches!(start, Value::Undefined) { Ok(Value::Undefined) } else { invoke(d, it, start, src, &[Value::Object(c)])? };
    match started {
        // A controller can escape through start() even when that callback throws. Its stream state stays valid
        // until collection proves it unreachable.
        Err(e) => return Err(Error::Throw(e)),
        Ok(v) => {
            // start returns any, not Promise<void>. Resolve a fresh promise with it, so assimilation is a job.
            let p = it.host_new_promise()?;
            it.host_resolve_promise(p, v)?;
            it.host_promise_then(p, Value::Object(ok), Value::Object(err))?;
        }
    }
    Ok(Value::Object(s))
}

// ---------------------------------------------------------------------------------------------- stream methods

fn is_stream(d: &Dom, this: Value) -> Option<ObjId> {
    this_obj(this).filter(|o| d.streams.st.borrow().streams.contains_key(o))
}

fn is_reader(d: &Dom, this: Value) -> Option<ObjId> {
    this_obj(this).filter(|o| d.streams.st.borrow().readers.contains_key(o))
}

pub(crate) fn locked(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    enter(d, || {
        let Some(s) = is_stream(d, this) else { return crate::browser_fetch::stream_locked(d, it, this, a) };
        Ok(Value::Bool(d.streams.st.borrow().streams.get(&s).is_some_and(|s| s.reader.is_some())))
    })
}

pub(crate) fn get_reader(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    enter(d, || {
        let options = arg(a, 0);
        let mut byob = false;
        if !matches!(options, Value::Undefined | Value::Null) {
            if !matches!(options, Value::Object(_)) { return Err(it.type_error("Reader options must be a dictionary")); }
            let mode = prop(d, it, options, "mode")?;
            if !matches!(mode, Value::Undefined) {
                let mode = it.to_js_string(mode)?;
                if it.heap.to_string_lossy(mode).unwrap_or_default() != "byob" {
                    return Err(it.type_error("Invalid readable stream reader mode"));
                }
                byob = true;
            }
        }
        let Some(s) = is_stream(d, this) else {
            if byob {
                crate::browser_fetch::stream_locked(d, it, this, &[])?;
                return Err(Error::Unsupported(unsupported("ReadableStream BYOB reader")));
            }
            return crate::browser_fetch::stream_get_reader(d, it, this, &[]);
        };
        if byob { return Err(Error::Unsupported(unsupported("ReadableStream BYOB reader"))); }
        {
            let st = d.streams.st.borrow();
            if st.streams.get(&s).is_some_and(|s| s.reader.is_some()) {
                drop(st);
                return Err(it.type_error("ReadableStream is locked"));
            }
            if st.readers.len() >= lim(d).max_readers {
                return Err(d.lim("stream-readers"));
            }
        }
        it.charge(2)?;
        let r = d.obj(it, fns(d).reader_proto)?;
        let mut st = d.streams.st.borrow_mut();
        st.readers.insert(r, Rdr { stream: Some(s), reads: VecDeque::new() });
        if let Some(s) = st.streams.get_mut(&s) {
            s.reader = Some(r);
        }
        Ok(Value::Object(r))
    })
}

pub(crate) fn cancel(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    enter(d, || {
        let Some(s) = is_stream(d, this) else { return crate::browser_fetch::stream_cancel(d, it, this, a) };
        if d.streams.st.borrow().streams.get(&s).is_some_and(|s| s.reader.is_some()) {
            let e = type_value(it, "Cannot cancel a stream that already has a reader")?;
            let p = it.host_new_promise()?;
            it.host_reject_promise(p, e)?;
            return Ok(p);
        }
        cancel_stream(d, it, s, arg(a, 0))
    })
}

// ---------------------------------------------------------------------------------------------- reader methods

pub(crate) fn read(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    enter(d, || {
        let Some(rdr) = is_reader(d, this) else { return crate::browser_fetch::reader_read(d, it, this, a) };
        enum Out {
            Released,
            Done,
            Err(Value),
            Chunk(Value, ObjId, bool),
            Pending(ObjId),
            Full,
        }
        it.charge(1)?;
        let p = it.host_new_promise()?;
        let max = lim(d).max_pending_reads;
        let out = {
            let mut st = d.streams.st.borrow_mut();
            let st = &mut *st;
            match st.readers.get(&rdr).and_then(|r| r.stream) {
                None => Out::Released,
                Some(s) => match st.streams.get(&s) {
                    None => Out::Released,
                    Some(Stream { st: St::Closed, .. }) => Out::Done,
                    Some(Stream { st: St::Errored(e), .. }) => Out::Err(*e),
                    Some(Stream { ctl, .. }) => {
                        let ctl = *ctl;
                        let c = st.ctls.get_mut(&ctl).expect("a stream has a controller");
                        if let Some((chunk, cost)) = c.queue.pop_front() {
                            c.qbytes -= cost;
                            st.queued_bytes -= cost;
                            Out::Chunk(chunk, ctl, c.close_requested && c.queue.is_empty())
                        } else {
                            let r = st.readers.get_mut(&rdr).expect("checked");
                            if r.reads.len() >= max {
                                Out::Full
                            } else {
                                r.reads.push_back(p);
                                Out::Pending(ctl)
                            }
                        }
                    }
                },
            }
        };
        match out {
            Out::Released => {
                let e = type_value(it, "This readable stream reader has been released and cannot be used to read from its previous owner stream")?;
                it.host_reject_promise(p, e)?;
            }
            Out::Done => resolve_with(d, it, p, Value::Undefined, true)?,
            Out::Err(e) => it.host_reject_promise(p, e)?,
            Out::Full => return Err(d.lim("stream-reads")),
            Out::Chunk(chunk, ctl, close) => {
                if close {
                    finish_close(d, it, ctl)?;
                } else {
                    pull_if_needed(d, it, ctl)?;
                }
                resolve_with(d, it, p, chunk, false)?;
            }
            Out::Pending(ctl) => pull_if_needed(d, it, ctl)?,
        }
        Ok(p)
    })
}

pub(crate) fn reader_cancel(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    enter(d, || {
        let Some(rdr) = is_reader(d, this) else { return crate::browser_fetch::reader_cancel(d, it, this, a) };
        let stream = d.streams.st.borrow().readers.get(&rdr).and_then(|r| r.stream);
        match stream {
            Some(s) => cancel_stream(d, it, s, arg(a, 0)),
            None => {
                let e = type_value(it, "This readable stream reader has been released and cannot be used to cancel its previous owner stream")?;
                let p = it.host_new_promise()?;
                it.host_reject_promise(p, e)?;
                Ok(p)
            }
        }
    })
}

pub(crate) fn release(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    enter(d, || {
        let Some(rdr) = is_reader(d, this) else { return crate::browser_fetch::reader_release(d, it, this, a) };
        let reads = {
            let mut st = d.streams.st.borrow_mut();
            let Some(r) = st.readers.get_mut(&rdr) else { return Ok(Value::Undefined) };
            let Some(s) = r.stream.take() else { return Ok(Value::Undefined) };
            let reads: Vec<Value> = r.reads.drain(..).collect();
            if let Some(s) = st.streams.get_mut(&s) {
                s.reader = None;
            }
            reads
        };
        for p in reads {
            let e = type_value(it, "Reader was released")?;
            it.host_reject_promise(p, e)?;
        }
        Ok(Value::Undefined)
    })
}

// ---------------------------------------------------------------------------------------------- controller methods

fn brand_ctl(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<ObjId> {
    match this_obj(this).filter(|o| d.streams.st.borrow().ctls.contains_key(o)) {
        Some(o) => Ok(o),
        None => Err(it.type_error("Illegal invocation")),
    }
}

/// `CanCloseOrEnqueue`.
fn can_close_or_enqueue(st: &State, c: &Ctl) -> bool {
    !c.close_requested && st.streams.get(&c.stream).is_some_and(|s| matches!(s.st, St::Readable))
}

fn ctl_enqueue(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    enter(d, || {
        let ctl = brand_ctl(d, it, this)?;
        let chunk = arg(a, 0);
        let bytes = it.bytes_of(chunk).map_or(0, |b| b.len());
        it.charge(1 + (bytes / 64) as u64)?;
        let l = lim(d);
        let direct = {
            let mut st = d.streams.st.borrow_mut();
            let st = &mut *st;
            let c = st.ctls.get(&ctl).expect("checked");
            if !can_close_or_enqueue(st, c) {
                None
            } else {
                let waiting = st.streams.get(&c.stream).and_then(|s| s.reader).and_then(|r| st.readers.get_mut(&r)).and_then(|r| r.reads.pop_front());
                match waiting {
                    Some(p) => Some(Some(p)),
                    None => {
                        if c.queue.len() >= l.max_queue_chunks {
                            return Err(d.lim("stream-queue"));
                        }
                        if st.queued_bytes.saturating_add(bytes) > l.max_queued_bytes {
                            return Err(d.lim("stream-bytes"));
                        }
                        let c = st.ctls.get_mut(&ctl).expect("checked");
                        c.queue.push_back((chunk, bytes));
                        c.qbytes += bytes;
                        st.queued_bytes += bytes;
                        Some(None)
                    }
                }
            }
        };
        match direct {
            None => return Err(it.type_error("Invalid state: Controller is already closed")),
            Some(Some(p)) => resolve_with(d, it, p, chunk, false)?,
            Some(None) => {}
        }
        pull_if_needed(d, it, ctl)?;
        Ok(Value::Undefined)
    })
}

fn ctl_close(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    enter(d, || {
        let ctl = brand_ctl(d, it, this)?;
        it.charge(1)?;
        let empty = {
            let mut st = d.streams.st.borrow_mut();
            let c = st.ctls.get(&ctl).expect("checked");
            if !can_close_or_enqueue(&st, c) {
                None
            } else {
                let c = st.ctls.get_mut(&ctl).expect("checked");
                c.close_requested = true;
                Some(c.queue.is_empty())
            }
        };
        match empty {
            None => return Err(it.type_error("Invalid state: Controller is already closed")),
            Some(true) => finish_close(d, it, ctl)?,
            Some(false) => {}
        }
        Ok(Value::Undefined)
    })
}

fn ctl_error(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    enter(d, || {
        let ctl = brand_ctl(d, it, this)?;
        it.charge(1)?;
        controller_error(d, it, ctl, arg(a, 0))?;
        Ok(Value::Undefined)
    })
}

fn ctl_desired_size(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    enter(d, || {
        let ctl = brand_ctl(d, it, this)?;
        let st = d.streams.st.borrow();
        let c = st.ctls.get(&ctl).expect("checked");
        Ok(match st.streams.get(&c.stream).map(|s| &s.st) {
            Some(St::Errored(_)) | None => Value::Null,
            Some(St::Closed) => Value::Number(0.0),
            Some(St::Readable) => Value::Number(c.hwm - c.queue.len() as f64),
        })
    })
}

fn gate_closed(_: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    Err(Error::Unsupported(unsupported("ReadableStreamDefaultReader.closed")))
}

pub(crate) fn reader_closed(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    enter(d, || {
        if is_reader(d, this).is_some() { return gate_closed(d, it, this, a); }
        crate::browser_fetch::gate_reader_closed(d, it, this, a)
    })
}

pub(crate) fn gate(d: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    enter(d, || Err(Error::Unsupported(unsupported("ReadableStream tee, pipeTo, pipeThrough and async iteration"))))
}

// ---------------------------------------------------------------------------------------------- install

/// Make the host reactions and the controller prototype. `fixed` gets every object that must live as long as the realm.
/// `stream_proto` and `reader_proto` are the prototypes of `ReadableStream` and the default reader.
pub(crate) fn install(d: &Dom, it: &mut Interp<'_>, stream_proto: ObjId, reader_proto: ObjId, fixed: &mut Vec<ObjId>) -> R<()> {
    let f = |it: &mut Interp<'_>, h, name: &str| d.add_fn(it, h, name, 1, false);
    let start_ok_f = f(it, start_ok, "startOk")?;
    let start_err = f(it, error_reaction, "startErr")?;
    let pull_ok_f = f(it, pull_ok, "pullOk")?;
    let pull_err = f(it, error_reaction, "pullErr")?;
    let cancel_ok_f = f(it, cancel_ok, "cancelOk")?;
    let cancel_err_f = f(it, cancel_err, "cancelErr")?;
    let op = it.object_prototype();
    let ctl_proto = d.obj(it, op)?;
    for (name, h, len) in [("enqueue", ctl_enqueue as crate::browser::H, 1), ("close", ctl_close, 0), ("error", ctl_error, 1)] {
        let k = d.key(it, name)?;
        let m = d.add_fn(it, h, name, len, false)?;
        it.define_data(ctl_proto, k, Value::Object(m), true, false, true)?;
    }
    let (k, g) = (d.key(it, "desiredSize")?, d.add_fn(it, ctl_desired_size, "get desiredSize", 0, false)?);
    d.define_accessor(it, ctl_proto, k, g, None)?;
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    let ctor = d.add_fn(it, illegal_constructor, "ReadableStreamDefaultController", 0, false)?;
    it.define_data(ctor, kp, Value::Object(ctl_proto), false, false, false)?;
    it.define_data(ctl_proto, kc, Value::Object(ctor), true, false, true)?;
    let global = it.global_object();
    let k = d.key(it, "ReadableStreamDefaultController")?;
    it.define_data(global, k, Value::Object(ctor), true, false, true)?;
    fixed.extend([ctl_proto, ctor]);
    let _ = d.streams.fns.set(Fns { start_ok: start_ok_f, start_err, pull_ok: pull_ok_f, pull_err, cancel_ok: cancel_ok_f, cancel_err: cancel_err_f, stream_proto, reader_proto, ctl_proto });
    Ok(())
}

impl Browser {
    /// Replace the limits of the stream layer. Call it before scripts run.
    pub fn configure_streams(&mut self, limits: StreamLimits) {
        self.dom.streams.lim.set(limits);
    }

    pub fn stream_stats(&self) -> StreamStats {
        let st = self.dom.streams.st.borrow();
        StreamStats {
            streams: st.streams.len(),
            readers: st.readers.len(),
            queued_chunks: st.ctls.values().map(|c| c.queue.len()).sum(),
            queued_bytes: st.queued_bytes,
            pending_reads: st.readers.values().map(|r| r.reads.len()).sum(),
            callbacks: st.callbacks,
        }
    }
}
