//! `bare-vex` behind `bare-js-api` (plan step S2.8, `docs/internal/design-js2-host-api.md`): `Realm` is the engine, so a host function, which is a
//! native and has only `&mut Realm`, reaches the engine type. The state of the interface is one field of the realm, [`ApiState`].
//!
//! **Handles.** A `Local` is a slot number. A pushed value is a slot of `ApiState::stack` (above four shared seeds: `undefined`, `null`,
//! `false`, `true`, which hold no cell and have the numbers `u32::MAX - 3..`). The `this` and the arguments of a running host call are
//! named where they are: slot `WIN` is `this` and `WIN + 1 + i` is argument `i`, read through a [`Win`] record in the frame of the
//! trampoline ([`host_call`]). So a call copies and pushes nothing. A `Persistent` is a slot of `ApiState::persistent`; the live bits
//! are in `bare_js_api::Slots`. `Realm::collect` marks the stack, the live persistent slots and the whole `Win` chain.
//!
//! **Strings.** `Engine::as_str(&self)` cannot flatten a rope, so a string that a handle names is flat or indirect: [`Realm::a_push`]
//! flattens a rope, and the trampoline flattens the ropes of the window.
//!
//! **Calls into script.** A host function runs with `win` set. Inside it (or inside any native: `nest > 0`) a call goes through
//! `Realm::call_function`, which holds the collector until it returns (`reenter.rs`). At the top, with no native on the Rust stack, a
//! call starts a run on a pooled machine with no hold, so a browser task collects inside itself; the host holds only handles, and the
//! handle stack is a root. This is why a collection never runs while `win` is set today. The window is still a root: a safepoint that
//! a later step adds needs it.
//!
//! **Errors.** `Error::Throw` is `Err(value)`. An abort is `Err(STOP)` with the reason in `ApiState::abort`: a script cannot catch it,
//! and the top-level call gives it back as `Error::Abort`. The step budget is `Realm::fuel`; a top-level call outside a [`Engine::task`]
//! starts it from [`Realm::set_budget`].
//!
//! **Promises and microtasks.** The host settles a promise with `resolve_promise` and `reject_promise` (a settled one is left alone), reads
//! it with `promise_status`, and attaches handlers with `promise_then` and `promise_react`. `drain_microtasks` and `checkpoint` run the
//! jobs of `promise.rs` (`Realm::drain_jobs`) and call the hooks of the host: `between_jobs` before each job and once at the end,
//! `report_exception` for a throw of a `queueMicrotask` callback, `promise_rejections` after the queue is empty (the jobs that it queues
//! run in the same call). A `checkpoint` from a host call or a native does nothing; a `drain_microtasks` there is `Unsupported`. A stop
//! of the drain (budget, `max_jobs_per_drain`, an error of `between_jobs`) is sticky until `clear_failed_job`.
//!
//! Not here yet: iteration, modules, buffers, owners with `trace` and `prune`, host-filled objects,
//! structured clone, `Symbol`, and `run_script` (`Realm::run_text` runs a script but computes no completion value; the compiler has
//! none yet). Each of these answers `Abort::Unsupported(name)`.
use crate::bi_object::{Desc, Own};
use crate::builtins::Native;
use crate::cell::Layout;
use crate::interp::STOP;
use crate::interp_rt::{ErrKind, Key, R, Realm};
use crate::gc::Marker;
use crate::object::{F_CLASS, Obj};
use crate::promise::DrainStop;
use crate::shapes::{A_C, A_E, A_W};
use crate::string::{Chars, Str};
use crate::value::{Kind, Value};
use bare_js_api::{Abort, Args, Attrs, Def, DefKind, Engine, Error, Host, HostFn, Intrinsic, Iter, Local, Own as ApiOwn, Slots, Type};
use std::ptr::NonNull;
use std::rc::Rc;

/// An own property of an object, as `Realm::raw_own` reads it.
#[derive(Clone, Copy, Debug)]
pub enum RawOwn {
    Data { value: Value, writable: bool, enumerable: bool, configurable: bool },
    Accessor { get: Option<Value>, set: Option<Value>, enumerable: bool, configurable: bool },
}

// A child module, so that it reads the private state and helpers of this one.
#[path = "api_script.rs"]
mod api_script;
#[path = "api_inspect.rs"]
mod api_inspect;
pub(crate) use api_script::eval_native;

/// The shared slots at the start of the handle stack. They have the numbers `u32::MAX - 3..=u32::MAX`, so a pushed value of logical
/// index `n` is at the physical index `n + 4` and `stack_len` is 0 for an empty stack.
const SEEDS: usize = 4;
const SLOT_UNDEFINED: u32 = u32::MAX - 3;
const SLOT_NULL: u32 = u32::MAX - 2;
const SLOT_FALSE: u32 = u32::MAX - 1;
/// The slot number of `this` in a host call; argument `i` is `WIN + 1 + i`.
const WIN: u32 = 1 << 31;
/// The most nested [`Engine::task`] calls.
const MAX_TASKS: u32 = 100;

/// The `this` and the arguments of a running host call. It lives in the frame of [`host_call`]; `ApiState::win` points at the innermost
/// one. `args` is the slice that the caller holds: it is neither written nor moved while the call runs.
struct Win {
    this: Value,
    args: *const Value,
    len: u32,
    prev: *const Win,
}

/// The state of the interface in a realm.
pub(crate) struct ApiState {
    stack: Vec<Value>,
    win: *const Win,
    /// The persistent table: a slot whose bit in `slots` is clear is free, and its old value is no root.
    persistent: Vec<Value>,
    slots: Rc<Slots>,
    host: Option<Rc<dyn Host<Realm>>>,
    /// The address of the value in `host`: the trampoline reads it with no `Rc` clone. Set only by `set_host`.
    host_ptr: Option<NonNull<dyn Host<Realm>>>,
    /// Why the last run stopped, when it was not a limit.
    abort: Option<Abort>,
    /// `call_scoped`: the objects that the names of handler code are looked up in (`api_script.rs`), and the code of each handler text.
    scope: Vec<Obj>,
    handlers: std::collections::HashMap<String, usize>,
    /// The iterator prototype (made on demand).
    pub(crate) iter_proto: Value,
    /// The index in `Realm::natives` of the trampoline of a plain host function and of a host constructor (`usize::MAX`: not made).
    natives: [usize; 2],
    /// The step budget of a task, and the nesting of [`Engine::task`].
    budget: u64,
    tasks: u32,
    pub(crate) external_live: usize,
    pub(crate) external_since: usize,
    pub(crate) freed: Option<crate::api_raw::RawCollected>,
    pub(crate) clock: Clock,
    /// Owners, host-filled objects and lazy members (`api_host.rs`).
    pub(crate) hs: crate::api_host::HostState,
}

/// The wall clock source of this realm.
pub enum Clock {
    System,
    Virtual(f64),
    Host(Option<Box<dyn Fn() -> f64>>),
}

impl Default for ApiState {
    fn default() -> Self {
        ApiState {
            stack: vec![Value::UNDEFINED, Value::NULL, Value::FALSE, Value::TRUE],
            win: std::ptr::null(),
            persistent: Vec::new(),
            slots: Default::default(),
            host: None,
            host_ptr: None,
            abort: None,
            scope: Vec::new(),
            handlers: Default::default(),
            iter_proto: Value::UNDEFINED,
            natives: [usize::MAX; 2],
            budget: u64::MAX,
            tasks: 0,
            external_live: 0,
            external_since: 0,
            freed: None,
            clock: Clock::System,
            hs: Default::default(),
        }
    }
}

impl ApiState {
    /// Mark the roots of the interface: the handle stack, the live persistent slots and the windows of the running host calls.
    pub(crate) fn mark(&self, m: &mut Marker) {
        m.values(&self.stack[SEEDS..]);
        m.value(self.iter_proto);
        for o in &self.scope {
            m.value(o.value());
        }
        let mut w = self.win;
        // SAFETY: each `Win` is in the frame of a `host_call` that is still running, and its `args` slice is held by the caller of
        // that call (`host_call` links a window before the call and unlinks it after, also when the host panics).
        while let Some(x) = unsafe { w.as_ref() } {
            m.value(x.this);
            m.values(unsafe { std::slice::from_raw_parts(x.args, x.len as usize) });
            w = x.prev;
        }
        self.slots.for_each(|id| m.value(self.persistent[id as usize]));
        self.hs.mark(m);
    }

    /// The address of the host (`api_host.rs` reads it without an `Rc` clone).
    pub(crate) fn host_ptr(&self) -> Option<NonNull<dyn Host<Realm>>> {
        self.host_ptr
    }

    /// Record why the run stops (`Err(STOP)`).
    pub(crate) fn set_abort(&mut self, a: Abort) {
        self.abort.get_or_insert(a);
    }

    /// The host edges of the owners that the marking reached since the last round (`gc::Roots::owners`).
    pub(crate) fn trace_owners(&mut self, live: &crate::gc::Live, round: u32, out: &mut Vec<Value>) -> usize {
        let keys = self.hs.reached(live, round);
        if let (false, Some(h)) = (keys.is_empty(), self.host_ptr) {
            // SAFETY: as in `host_call`: the pointer is into the `Rc` of `host`, which lives as long as `self`.
            let h = unsafe { h.as_ref() };
            for &k in &keys {
                h.trace(k, &mut bare_js_api::Tracer::new(out));
            }
        }
        keys.len()
    }

    /// Drop the owners that the marking did not reach and give their keys to the host (`gc::Roots::weak`).
    pub(crate) fn prune_owners(&mut self, live: &crate::gc::Live) {
        if let Some(count) = &mut self.freed {
            live.visit_dead(|h| match h.layout() {
                Layout::Object | Layout::Indexed => count.objects += 1,
                Layout::OneByte | Layout::TwoByte if h.flags() & crate::string::FLAG_SYMBOL != 0 => count.symbols += 1,
                Layout::OneByte | Layout::TwoByte | Layout::Rope | Layout::Indirect => count.strings += 1,
                _ => {}
            });
        }
        let dead = self.hs.prune(live);
        if let (false, Some(h)) = (dead.is_empty(), self.host_ptr) {
            // SAFETY: as in `trace_owners`.
            unsafe { h.as_ref() }.prune(&dead);
        }
    }
}

/// Why `Realm::run_text` stopped.
#[derive(Clone, Copy, Debug)]
pub enum RunError {
    /// A thrown value, not rooted: use it before the next call into the engine.
    Throw(Value),
    Abort(Abort),
}

impl Realm {
    /// The value of a persistent handle (`Engine::persistent` for `&self`).
    pub fn raw_persistent(&self, id: u32) -> Value {
        self.api.persistent[id as usize]
    }

    /// An own property with its attributes, for a host that keeps `Value`s: no getter runs and no proxy trap. `None`: there is none.
    pub fn raw_own(&mut self, obj: Value, key: Value) -> Result<Option<RawOwn>, Error<'static>> {
        let (ol, kl) = (self.a_push(obj), self.a_push(key));
        let (o, k) = (self.a_obj(ol)?, self.a_key(kl)?);
        Ok(self.get_own_h(o, k).map_err(|e| self.a_err(e))?.map(|p| {
            let (enumerable, configurable) = (p.attrs() & A_E != 0, p.attrs() & A_C != 0);
            match p {
                Own::Data(value, a) => RawOwn::Data { value, writable: a & A_W != 0, enumerable, configurable },
                Own::Acc(g, s, _) => RawOwn::Accessor { get: Some(g).filter(|v| !v.is_undefined()), set: Some(s).filter(|v| !v.is_undefined()), enumerable, configurable },
            }
        }))
    }

    /// The step budget of a task: a top-level call outside [`Engine::task`] starts with this many back edges (default: no limit).
    pub fn set_budget(&mut self, steps: u64) {
        self.api.budget = steps;
    }

    /// Compile `src` as a script of this realm and run it with the global object as `this`. For tests and tools: it computes no
    /// completion value. A syntax error is a thrown `SyntaxError`.
    pub fn run_text(&mut self, src: &str) -> Result<(), RunError> {
        self.api.abort = None;
        match self.script_code(src, Default::default()).and_then(|ci| self.run_code(ci, false)) {
            Ok(_) => Ok(()),
            Err(STOP) => Err(RunError::Abort(self.api.abort.take().unwrap_or(Abort::Limit("the step budget")))),
            Err(e) => Err(RunError::Throw(e)),
        }
    }

    #[inline]
    pub(crate) fn a_push<'s>(&mut self, v: Value) -> Local<'s> {
        if let Some(s) = Realm::str_of(v)
            && s.is_rope()
        {
            self.flat(s);
        }
        self.api.stack.push(v);
        Local::from_slot((self.api.stack.len() - SEEDS - 1) as u32)
    }

    /// The value of a handle: a slot of the stack (one bounds check), else a slot of the running call.
    #[inline]
    pub(crate) fn a_at(&self, l: Local<'_>) -> Value {
        match self.api.stack.get(l.slot().wrapping_add(SEEDS as u32) as usize) {
            Some(&x) => x,
            None => self.a_win(l.slot()),
        }
    }

    /// The value of a slot of the running call. A slot that names nothing is a bug of the host: a panic, never a read outside the slice.
    #[inline(never)]
    fn a_win(&self, slot: u32) -> Value {
        // SAFETY: see `ApiState::mark`.
        let (Some(w), Some(k)) = (unsafe { self.api.win.as_ref() }, slot.checked_sub(WIN)) else { panic!("a handle that is no slot of the handle stack") };
        match k.checked_sub(1) {
            None => w.this,
            // SAFETY: `i < len`, inside the slice that the caller of the host call holds.
            Some(i) if i < w.len => unsafe { *w.args.add(i as usize) },
            Some(_) => panic!("an argument handle past the arguments of the call"),
        }
    }

    /// Borrow a range of arguments from the current host call.
    pub fn raw_args_slice(&self, a: Args<'_>, from: usize, to: usize) -> Option<&[Value]> {
        let n = to.min(a.len()).saturating_sub(from);
        let k = a.get(from)?.slot().checked_sub(WIN + 1)? as usize;
        // SAFETY: the window and its arguments live for the host call, as in ApiState::mark.
        let w = unsafe { self.api.win.as_ref() }?;
        if n == 0 || k >= w.len as usize || n > w.len as usize - k {
            return None;
        }
        // SAFETY: the range was checked against the slice held by the caller.
        Some(unsafe { std::slice::from_raw_parts(w.args.add(k), n) })
    }

    /// Is idle collection due after heap or external allocation?
    pub fn raw_gc_due(&self) -> bool {
        self.nest == 0 && self.api.tasks == 0
            && (self.heap.due() || self.api.external_since > (4usize << 20).max(self.api.external_live / 4))
    }

    /// An error of the engine as an error of the interface: a thrown value is a local, the stop marker is the abort that was recorded.
    pub(crate) fn a_err<'s>(&mut self, e: Value) -> Error<'s> {
        if e == STOP { Error::Abort(self.api.abort.take().unwrap_or(Abort::Limit("the step budget"))) } else { Error::Throw(self.a_push(e)) }
    }

    fn a_ret<'s>(&mut self, r: R<Value>) -> Result<Local<'s>, Error<'s>> {
        match r {
            Ok(v) => Ok(self.a_push(v)),
            Err(e) => Err(self.a_err(e)),
        }
    }

    fn a_unsup<'s>(&mut self, what: &'static str) -> Error<'s> {
        self.api.abort = Some(Abort::Unsupported(what));
        Error::Abort(Abort::Unsupported(what))
    }

    pub(crate) fn a_throw<'s>(&mut self, kind: ErrKind, msg: &str) -> Error<'s> {
        let e = self.error(kind, msg);
        self.a_err(e)
    }

    pub(crate) fn a_obj<'s>(&mut self, l: Local<'_>) -> Result<Obj, Error<'s>> {
        match Realm::obj(self.a_at(l)) {
            Some(o) => Ok(o),
            None => Err(self.a_throw(ErrKind::Type, "not an object")),
        }
    }

    fn a_key<'s>(&mut self, l: Local<'_>) -> Result<Key, Error<'s>> {
        let v = self.a_at(l);
        self.key_of(v).map_err(|e| self.a_err(e))
    }

    fn a_args(&self, args: &[Local<'_>]) -> Vec<Value> {
        args.iter().map(|&l| self.a_at(l)).collect()
    }

    /// Is a native or a host call on the Rust stack? A call from there goes through `call_function`, which holds the collector.
    #[inline]
    fn a_nested(&self) -> bool {
        !self.api.win.is_null() || self.nest > 0
    }

    /// `f.call(this, ...args)`: nested inside a host call or a native, else a run on a pooled machine that may collect.
    fn a_call(&mut self, f: Value, this: Value, args: &[Value]) -> R<Value> {
        if self.a_nested() {
            return self.call_function(f, this, args);
        }
        if self.api.tasks == 0 {
            self.fuel = self.api.budget;
        }
        self.call_held(f, this, args)
    }

    /// `Construct(f, args, nt)` for a native constructor, a plain function or a class (a class runs as a nested call that holds the collector).
    fn a_construct(&mut self, f: Value, args: &[Value], nt: Value) -> R<Value> {
        if self.native_id(f).is_some() {
            let saved = (self.callee, self.nt, self.tail.take(), std::mem::take(&mut self.tail_args));
            let r = self.construct_native(f, args, nt);
            (self.callee, self.nt, self.tail, self.tail_args) = saved;
            return r;
        }
        let Some((ci, _)) = self.function_new(f) else { return self.type_error("not a constructor") };
        if Realm::obj(f).is_some_and(|o| o.has_flag(F_CLASS)) {
            // As `Reflect.construct` does: a nested call that holds the collector until it returns.
            return self.construct_script(f, args, nt);
        }
        if self.prog[ci].noctor {
            return self.type_error("not a constructor");
        }
        let this = self.new_this(nt)?;
        let r = self.a_call(f, this, args)?;
        Ok(if Realm::obj(r).is_some() { r } else { this })
    }

    /// The `length` and the `name` of a host function (`Realm::materialize`).
    pub(crate) fn host_props(&self, o: Obj) -> Option<(f64, String)> {
        let id = self.native_id(o.value())?;
        if !self.api.natives.contains(&id) {
            return None;
        }
        let len = o.slot(&self.shapes, 2).as_int()? >> 2;
        Some((len as f64, Realm::str_of(o.slot(&self.shapes, 3)).map(|s| self.text(s)).unwrap_or_default()))
    }

    /// The promise that a host settles: `Unsupported` when `p` is no promise, `None` when it is settled already (the host settles once;
    /// a second call does nothing).
    fn a_pending_promise<'s>(&mut self, what: &'static str, p: Local<'_>) -> Result<Option<Obj>, Error<'s>> {
        let Some(o) = Realm::promise_obj(self.a_at(p)) else { return Err(self.a_unsup(what)) };
        Ok(Realm::promise_status(self, o.value()).filter(|s| s.0 == 0).map(|_| o))
    }

    /// `PerformPromiseThen(p, ok, err)` with a derived promise that nobody holds (a throw of a handler is an unhandled rejection).
    fn a_then<'s>(&mut self, p: Obj, ok: Value, err: Value) -> Result<(), Error<'s>> {
        self.promise_then_host(p, ok, err).map_err(|e| self.a_err(e))
    }

    /// Run the jobs and the hooks of a checkpoint (the order of the old engine): the jobs, then the rejections, then the jobs that the
    /// listeners queued. `n` is the count of jobs.
    fn a_drain(&mut self, host: Option<&dyn Host<Realm>>, n: &mut u32) -> Result<(), DrainStop> {
        let Some(h) = host else { return self.drain_jobs(n, None, &mut |_, _| {}) };
        self.drain_jobs(n, Some(&mut |rt| rt.a_between(h)), &mut |rt, v| rt.a_report(h, v))?;
        if !self.rejections_wait() {
            return Ok(());
        }
        self.a_notify(h)?;
        self.drain_jobs(n, Some(&mut |rt| rt.a_between(h)), &mut |rt, v| rt.a_report(h, v))
    }

    /// An error of a hook as the reason of a drain stop. A thrown value is rooted by `Promises::failed`, an abort by `ApiState::abort`.
    fn a_stop(&mut self, e: Error<'_>) -> DrainStop {
        match e {
            Error::Throw(l) => DrainStop::Throw(self.a_at(l)),
            Error::Abort(a) => {
                self.api.abort.get_or_insert(a);
                DrainStop::Fuel
            }
        }
    }

    /// `Host::between_jobs`. A script that it runs gets a budget of its own (a call at the top starts one): the budget of the checkpoint
    /// comes back after.
    fn a_between(&mut self, h: &dyn Host<Realm>) -> Result<(), DrainStop> {
        let fuel = self.fuel;
        let r = bare_js_api::enter(self, |s| h.between_jobs(s).map_err(|e| s.engine().a_stop(e)));
        self.fuel = fuel;
        r
    }

    /// `Host::report_exception` for a throw of a `queueMicrotask` callback. The hook runs as a host call of the checkpoint: one task
    /// level, so a script that it runs shares the budget.
    fn a_report(&mut self, h: &dyn Host<Realm>, v: Value) {
        self.api.tasks += 1;
        bare_js_api::enter(self, |s| {
            let thrown = s.engine().a_push(v);
            h.report_exception(s, thrown);
        });
        self.api.tasks -= 1;
    }

    /// `Host::promise_rejections` (HTML "notify about rejected promises"). An error is the result of the checkpoint, and is not sticky.
    #[cold]
    fn a_notify(&mut self, h: &dyn Host<Realm>) -> Result<(), DrainStop> {
        let r = self.take_rejections();
        if r.unhandled.is_empty() && r.handled.is_empty() && r.dropped == 0 {
            return Ok(());
        }
        self.api.tasks += 1;
        let out = bare_js_api::enter(self, |s| {
            let e = s.engine();
            let u: Vec<_> = r.unhandled.iter().map(|&v| e.a_push(v)).collect();
            let hd: Vec<_> = r.handled.iter().map(|&v| e.a_push(v)).collect();
            h.promise_rejections(s, &u, &hd, r.dropped).map_err(|e| s.engine().a_stop(e))
        });
        self.api.tasks -= 1;
        out
    }

    fn host_native(&mut self, ctor: bool) -> usize {
        let i = ctor as usize;
        if self.api.natives[i] == usize::MAX {
            self.natives.push(Native { f: host_call, ctor, len: 0, name: "" });
            self.api.natives[i] = self.natives.len() - 1;
        }
        self.api.natives[i]
    }

    /// A setter that the interpreter cannot enter (a host function): run it in a nested call. Not a function: a `TypeError`.
    #[cold]
    #[inline(never)]
    pub(crate) fn call_setter_slow(&mut self, f: Value, this: Value, v: Value) -> R<()> {
        if !Realm::obj(f).is_some_and(|o| o.is_callable()) {
            return self.type_error("a setter is not a function");
        }
        self.call_function(f, this, &[v]).map(|_| ())
    }

    /// The function object of host id `id`, named `name` (a string value).
    fn host_cell(&mut self, name: Value, id: u32, length: u32, kind: HostFn) -> Obj {
        let nid = self.host_native(kind != HostFn::Method);
        let cell = self.native_cell(nid);
        cell.set_slot(&self.shapes, 1, Value::int(id as i32));
        cell.set_slot(&self.shapes, 2, Value::int(kind as i32 | (length.min(1 << 20) as i32) << 2));
        cell.set_slot(&self.shapes, 3, name);
        cell
    }
}

/// Undoes the window link when the host call ends, by return or by panic: a caller that catches the panic keeps the realm, and a
/// later collection must not read this dead frame.
struct WinGuard<'a> {
    rt: &'a mut Realm,
    prev: *const Win,
}

impl Drop for WinGuard<'_> {
    #[inline]
    fn drop(&mut self) {
        self.rt.api.win = self.prev;
    }
}

/// The native that every host function runs. The function cell keeps the host `id` in slot 1, the kind and the `length` in slot 2
/// and the name in slot 3.
fn host_call(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (callee, nt) = (rt.callee, rt.nt);
    let (id, meta) = {
        let o = Realm::obj(callee).expect("a host function");
        (o.slot(&rt.shapes, 1).as_int().expect("a host id") as u32, o.slot(&rt.shapes, 2).as_int().expect("a host kind"))
    };
    let is_new = !nt.is_undefined();
    if meta & 3 == HostFn::Ctor as i32 && is_new && nt != callee {
        return rt.type_error("a host constructor cannot be extended");
    }
    let Some(h) = rt.api.host_ptr else {
        rt.api.abort = Some(Abort::Unsupported("a host function without a host"));
        return Err(STOP);
    };
    // SAFETY: `h` points into the `Rc` of `api.host`, which `set_host` does not replace while a host call runs (it checks `win`).
    let h = unsafe { h.as_ref() };
    for v in std::iter::once(this).chain(args.iter().copied()) {
        if let Some(s) = Realm::str_of(v)
            && s.is_rope()
        {
            rt.flat(s);
        }
    }
    debug_assert!(args.len() < (1 << 30));
    let win = Win { this, args: args.as_ptr(), len: args.len() as u32, prev: rt.api.win };
    rt.api.win = &win;
    let g = WinGuard { rt, prev: win.prev };
    bare_js_api::enter(&mut *g.rt, |s| match h.call(s, id, Local::from_slot(WIN), Args::from_slots(WIN + 1, args.len() as u32), is_new) {
        Ok(l) => Ok(s.engine().a_at(l)),
        Err(Error::Throw(l)) => Err(s.engine().a_at(l)),
        Err(Error::Abort(a)) => {
            s.engine().api.abort.get_or_insert(a);
            Err(STOP)
        }
    })
}

#[path = "api_clone.rs"]
mod api_clone;

impl Engine for Realm {
    fn clone_out<'s>(&mut self, v: Local<'_>, o: &bare_js_api::CloneOpts<'_, '_>, h: &mut dyn bare_js_api::CloneHost<Self>) -> Result<bare_js_api::Cloned, Error<'s>> {
        let v = self.a_at(v);
        bare_js_api::enter(self, |s| {
            let rt = s.engine();
            api_clone::write(rt, v, o, h).map_err(|e| match e {
                Error::Throw(l) => Ok(rt.a_at(l)),
                Error::Abort(a) => Err(a),
            })
        }).map_err(|e| match e { Ok(v) => Error::Throw(self.a_push(v)), Err(a) => Error::Abort(a) })
    }

    fn clone_in<'s>(&mut self, c: &mut bare_js_api::Cloned, h: &mut dyn bare_js_api::CloneHost<Self>) -> Result<bare_js_api::CloneIn<'s>, Error<'s>> {
        api_clone::load(self, c, h)
    }

    fn clone_copy<'s>(&mut self, v: Local<'_>, l: &bare_js_api::CloneLimits, h: &mut dyn bare_js_api::CloneHost<Self>) -> Result<(Local<'s>, usize), Error<'s>> {
        let mut c = self.clone_out(v, &bare_js_api::CloneOpts { limits: *l, transfer: &[] }, h)?;
        Ok((self.clone_in(&mut c, h)?.value, c.cost()))
    }

    type Raw = Value;

    #[inline]
    fn stack_len(&self) -> u32 {
        (self.api.stack.len() - SEEDS) as u32
    }

    #[inline]
    fn stack_truncate(&mut self, len: u32) {
        self.api.stack.truncate(len as usize + SEEDS);
    }

    fn stack_set(&mut self, to: u32, from: u32) {
        let v = self.a_at(Local::from_slot(from));
        self.api.stack[to as usize + SEEDS] = v;
    }

    fn persist(&mut self, v: Local<'_>) -> u32 {
        let x = self.a_at(v);
        let a = &mut self.api;
        let id = a.slots.take();
        match a.persistent.get_mut(id as usize) {
            Some(slot) => *slot = x,
            None => a.persistent.push(x),
        }
        id
    }

    fn persistent<'s>(&mut self, id: u32) -> Local<'s> {
        self.a_push(self.api.persistent[id as usize])
    }

    fn slots(&self) -> &Rc<Slots> {
        &self.api.slots
    }

    #[inline]
    fn undefined<'s>(&mut self) -> Local<'s> {
        Local::from_slot(SLOT_UNDEFINED)
    }

    #[inline]
    fn null<'s>(&mut self) -> Local<'s> {
        Local::from_slot(SLOT_NULL)
    }

    #[inline]
    fn boolean<'s>(&mut self, b: bool) -> Local<'s> {
        Local::from_slot(SLOT_FALSE + b as u32)
    }

    #[inline]
    fn number<'s>(&mut self, n: f64) -> Local<'s> {
        self.a_push(Realm::num(n))
    }

    fn string<'s>(&mut self, text: &str) -> Result<Local<'s>, Error<'s>> {
        match Str::from_str(&mut self.heap, text) {
            Ok(s) => Ok(self.a_push(s.value())),
            Err(_) => Err(Error::Abort(Abort::Limit("a string that is too long or no memory"))),
        }
    }

    fn type_of(&self, v: Local<'_>) -> Type {
        let v = self.a_at(v);
        if v.is_undefined() {
            Type::Undefined
        } else if v.is_null() {
            Type::Null
        } else if v.as_bool().is_some() {
            Type::Boolean
        } else if v.is_number() {
            Type::Number
        } else {
            match v.as_pointer() {
                Some((Kind::String, _)) => Type::String,
                Some((Kind::Symbol, _)) => Type::Symbol,
                Some((Kind::BigInt, _)) => Type::BigInt,
                Some((Kind::Object, _)) if Realm::obj(v).is_some_and(|o| o.is_callable()) => Type::Function,
                Some((Kind::Object, _)) => Type::Object,
                None => Type::Undefined,
            }
        }
    }

    fn as_bool(&self, v: Local<'_>) -> Option<bool> {
        self.a_at(v).as_bool()
    }

    fn as_number(&self, v: Local<'_>) -> Option<f64> {
        self.a_at(v).as_f64()
    }

    fn as_str(&self, v: Local<'_>) -> Option<bare_js_api::Str<'_>> {
        let s = Realm::str_of(self.a_at(v))?;
        // SAFETY: the characters are in the cell of the string, not in the local `s` that `chars` borrows. A handle names a flat or
        // indirect string (`a_push` and `host_call` flatten), and the cell lives until the next collection; `&self` rules that out.
        match s.chars()? {
            Chars::One(b) => Some(bare_js_api::Str::Latin1(unsafe { &*(b as *const [u8]) })),
            Chars::Two(u) => Some(bare_js_api::Str::Utf16(unsafe { &*(u as *const [u16]) })),
        }
    }

    fn global<'s>(&mut self) -> Local<'s> {
        self.a_push(self.global.value())
    }

    fn new_object<'s>(&mut self) -> Result<Local<'s>, Error<'s>> {
        let o = Obj::new(&mut self.heap, &self.shapes, self.state.s_object).expect("memory");
        Ok(self.a_push(o.value()))
    }

    fn get<'s>(&mut self, obj: Local<'_>, key: Local<'_>) -> Result<Local<'s>, Error<'s>> {
        let (o, k) = (self.a_at(obj), self.a_key(key)?);
        let r = self.get_prop(o, k);
        self.a_ret(r)
    }

    fn set<'s>(&mut self, obj: Local<'_>, key: Local<'_>, value: Local<'_>) -> Result<(), Error<'s>> {
        let (o, k, v) = (self.a_at(obj), self.a_key(key)?, self.a_at(value));
        let r = match self.set_prop_g(o, k, v) {
            Ok(crate::interp_rt::Put::Done) => Ok(()),
            Ok(crate::interp_rt::Put::Fail) => self.type_error("cannot assign to a read-only property or a property with no setter"),
            Ok(crate::interp_rt::Put::Set(f)) => self.a_call(f, o, &[v]).map(|_| ()),
            Err(e) => Err(e),
        };
        r.map_err(|e| self.a_err(e))
    }

    fn define<'s>(&mut self, obj: Local<'_>, key: Local<'_>, value: Local<'_>, attrs: Attrs) -> Result<(), Error<'s>> {
        let (o, k, v) = (self.a_obj(obj)?, self.a_key(key)?, self.a_at(value));
        let d = Desc { value: Some(v), w: Some(attrs.writable), e: Some(attrs.enumerable), c: Some(attrs.configurable), ..Default::default() };
        self.a_define(o, k, &d)
    }

    fn call<'s>(&mut self, f: Local<'_>, this: Local<'_>, args: &[Local<'_>]) -> Result<Local<'s>, Error<'s>> {
        let (f, this, args) = (self.a_at(f), self.a_at(this), self.a_args(args));
        let r = self.a_call(f, this, &args);
        self.a_ret(r)
    }

    fn host_function<'s>(&mut self, name: &str, id: u32, length: u32, kind: HostFn) -> Result<Local<'s>, Error<'s>> {
        assert!(id < 1 << 31, "a host id is below 2^31");
        let name = self.string(name);
        let cell = self.host_cell(name, id, length, kind);
        Ok(self.a_push(cell.value()))
    }

    fn install_table<'s>(&mut self, targets: &[Local<'_>], id_base: u32, defs: &'static [Def]) -> Result<(), Error<'s>> {
        assert!(id_base < 1 << 31 && targets.len() <= 32, "a host id is below 2^31, and a table has at most 32 targets");
        let mut objs = [None; 32];
        for (o, &t) in objs.iter_mut().zip(targets) {
            *o = Some(self.a_obj(t)?);
        }
        let mut text = String::new();
        for d in defs {
            let k = Key::Name(self.intern(d.name));
            let bit = |on: bool, b: u8| if on { b } else { 0 };
            let attrs = bit(d.attrs.writable, A_W) | bit(d.attrs.enumerable, A_E) | bit(d.attrs.configurable, A_C);
            let fns = match d.kind {
                DefKind::Method { id, len } => {
                    let Key::Name(name) = k else { unreachable!() };
                    [Some(self.host_cell(name.value(), id_base + id, len, HostFn::Method).value()), None]
                }
                DefKind::Accessor { get, set } => {
                    text.clear();
                    text.push_str("get ");
                    text.push_str(d.name);
                    let name = self.string(&text);
                    let g = self.host_cell(name, id_base + get, 0, HostFn::Method).value();
                    text.replace_range(..3, "set");
                    let st = set.map(|id| {
                        let name = self.string(&text);
                        self.host_cell(name, id_base + id, 1, HostFn::Method).value()
                    });
                    [Some(g), st]
                }
                DefKind::Both { id } => {
                    let Key::Name(name) = k else { unreachable!() };
                    let f = self.host_cell(name.value(), id_base + id, 0, HostFn::Method).value();
                    [Some(f), Some(f)]
                }
                DefKind::Number(n) => [Some(Value::number(n)), None],
            };
            for (i, o) in objs.iter().enumerate() {
                let (true, Some(o)) = (d.on >> i & 1 != 0, *o) else { continue };
                let r = match d.kind {
                    DefKind::Accessor { .. } | DefKind::Both { .. } => {
                        let g = fns[0].map_or(Ok(()), |g| self.define_accessor(o, k, g, false, d.attrs.enumerable));
                        g.and_then(|()| fns[1].map_or(Ok(()), |st| self.define_accessor(o, k, st, true, d.attrs.enumerable)))
                    }
                    _ => self.define_attrs(o, k, attrs, fns[0].expect("a value")),
                };
                r.map_err(|e| self.a_err(e))?;
            }
        }
        Ok(())
    }

    fn set_host(&mut self, host: Rc<dyn Host<Realm>>) {
        assert!(self.api.win.is_null(), "set_host inside a host call");
        if self.api.host.as_ref().is_some_and(|current| Rc::ptr_eq(current, &host)) {
            return;
        }
        self.api.host_ptr = NonNull::new(Rc::as_ptr(&host) as *mut dyn Host<Realm>);
        self.api.host = Some(host);
    }

    fn raw(&self, v: Local<'_>) -> Value {
        self.a_at(v)
    }

    fn raw_local<'s>(&mut self, r: &Value) -> Local<'s> {
        self.a_push(*r)
    }

    fn set_host_owner<'s>(&mut self, obj: Local<'_>, key: u32) -> Result<(), Error<'s>> {
        let o = self.a_obj(obj)?;
        self.hs_owner(o, key);
        Ok(())
    }

    fn host_key(&self, obj: Local<'_>) -> Option<u32> {
        self.hs_key(self.a_at(obj))
    }

    fn new_host_indexed<'s>(&mut self, proto: Option<Local<'_>>, key: u32) -> Result<Local<'s>, Error<'s>> {
        self.hs_new(proto, 1, key)
    }

    fn new_host_named<'s>(&mut self, proto: Option<Local<'_>>, key: u32, open: bool) -> Result<Local<'s>, Error<'s>> {
        self.hs_new(proto, 2 + open as u64, key)
    }

    fn set_lazy<'s>(&mut self, obj: Local<'_>, chain: bool) -> Result<(), Error<'s>> {
        let o = self.a_obj(obj)?;
        self.hs_set_lazy(o, chain);
        Ok(())
    }

    fn clear_lazy(&mut self, obj: Local<'_>) {
        if let Some(o) = Realm::obj(self.a_at(obj)) {
            self.hs_clear_lazy(o);
        }
    }

    fn new_promise<'s>(&mut self) -> Result<Local<'s>, Error<'s>> {
        let p = Realm::new_promise(self);
        Ok(self.a_push(p.value()))
    }

    fn resolve_promise<'s>(&mut self, p: Local<'_>, v: Local<'_>) -> Result<(), Error<'s>> {
        let Some(p) = self.a_pending_promise("resolve_promise", p)? else { return Ok(()) };
        let v = self.a_at(v);
        Realm::resolve_promise(self, p, v).map_err(|e| self.a_err(e))
    }

    fn reject_promise<'s>(&mut self, p: Local<'_>, reason: Local<'_>) -> Result<(), Error<'s>> {
        let Some(p) = self.a_pending_promise("reject_promise", p)? else { return Ok(()) };
        let r = self.a_at(reason);
        Realm::reject_promise(self, p, r);
        Ok(())
    }

    fn promise_then<'s>(&mut self, p: Local<'_>, ok: Local<'_>, err: Local<'_>) -> Result<(), Error<'s>> {
        let Some(p) = Realm::promise_obj(self.a_at(p)) else { return Err(self.a_unsup("promise_then")) };
        let (ok, err) = (self.a_at(ok), self.a_at(err));
        self.a_then(p, ok, err)
    }

    fn promise_react<'s>(&mut self, x: Local<'_>, ok: Local<'_>, err: Local<'_>) -> Result<(), Error<'s>> {
        let (x, ok, err) = (self.a_at(x), self.a_at(ok), self.a_at(err));
        let p = match self.promise_resolve_default(x) {
            Ok(p) => p,
            Err(e) => return Err(self.a_err(e)),
        };
        self.a_then(p, ok, err)
    }

    fn promise_status<'s>(&mut self, v: Local<'_>) -> Option<bare_js_api::PromiseState<'s>> {
        use bare_js_api::PromiseState;
        Some(match Realm::promise_status(self, self.a_at(v))? {
            (0, _) => PromiseState::Pending,
            (1, x) => PromiseState::Fulfilled(self.a_push(x)),
            (_, x) => PromiseState::Rejected(self.a_push(x)),
        })
    }

    fn queue_microtask<'s>(&mut self, cb: Local<'_>) -> Result<(), Error<'s>> {
        let cb = self.a_at(cb);
        self.queue_callback(cb).map_err(|e| self.a_err(e))
    }

    /// From a host call that a script function called, or a native: nothing runs (the HTML rule). Else it is `drain_microtasks`.
    fn checkpoint<'s>(&mut self) -> Result<usize, Error<'s>> {
        if self.a_nested() { Ok(0) } else { self.drain_microtasks() }
    }

    fn drain_microtasks<'s>(&mut self) -> Result<usize, Error<'s>> {
        if self.a_nested() {
            return Err(self.a_unsup("drain_microtasks during a call"));
        }
        if self.api.tasks == 0 {
            self.fuel = self.api.budget;
        }
        // One clone of the host for the whole checkpoint: a hook that calls `set_host` cannot free it under the drain.
        let host = self.api.host.clone();
        let mut n = 0;
        let r = self.a_drain(host.as_deref(), &mut n);
        match r {
            Ok(()) => Ok(n as usize),
            Err(DrainStop::Limit) => Err(Error::Abort(Abort::Limit("the microtask drain"))),
            Err(DrainStop::Fuel) => Err(self.a_err(STOP)),
            Err(DrainStop::Throw(v)) => Err(self.a_err(v)),
        }
    }

    fn pending_jobs(&self) -> usize {
        Realm::pending_jobs(self)
    }

    fn clear_failed_job(&mut self) {
        Realm::clear_failed_job(self);
    }

    fn iter_open<'s>(&mut self, v: Local<'_>) -> Result<Option<Iter<'s>>, Error<'s>> {
        self.a_iter_open(v)
    }

    fn iter_next<'s>(&mut self, it: &mut Iter<'s>) -> Result<Option<Local<'s>>, Error<'s>> {
        self.a_iter_next(it)
    }

    fn iter_fail<'s>(&mut self, it: &mut Iter<'s>, err: Error<'s>) -> Error<'s> {
        self.a_iter_fail(it, err)
    }

    fn run_script<'s>(&mut self, source: &str) -> Result<Local<'s>, Error<'s>> {
        self.a_run_script(source)
    }

    fn compile_function<'s>(&mut self, name: &str, params: &str, body: &str) -> Result<Local<'s>, Error<'s>> {
        self.a_compile_function(name, params, body)
    }

    fn call_scoped<'s>(&mut self, f: Local<'_>, this: Local<'_>, args: &[Local<'_>], scope: &[Local<'_>]) -> Result<Local<'s>, Error<'s>> {
        self.a_call_scoped(f, this, args, scope)
    }

    fn eval_global<'s>(&mut self, src: Local<'_>) -> Result<Local<'s>, Error<'s>> {
        self.a_eval_global(src)
    }

    fn eval_function<'s>(&mut self) -> Result<Local<'s>, Error<'s>> {
        Ok(self.a_eval_function())
    }

    fn symbol_description<'s>(&mut self, v: Local<'_>) -> Result<Option<Local<'s>>, Error<'s>> {
        self.a_symbol_description(v)
    }

    fn module_parse<'s>(&mut self, source: &str, url: &str) -> Result<u32, Error<'s>> {
        self.api_module_parse(source, url)
    }

    fn module_requests(&self, m: u32) -> Vec<String> {
        self.md_requests(m)
    }

    fn module_set_requested(&mut self, m: u32, deps: &[u32]) {
        self.md_set_requested(m, deps);
    }

    fn module_url(&self, m: u32) -> &str {
        self.md_url(m)
    }

    fn module_link<'s>(&mut self, root: u32) -> Result<(), Error<'s>> {
        self.api_module_link(root)
    }

    fn module_evaluate<'s>(&mut self, m: u32) -> Result<Local<'s>, Error<'s>> {
        self.api_module_evaluate(m)
    }

    fn take_dynamic_imports(&mut self) -> Vec<bare_js_api::ImportRequest> {
        self.api_take_imports()
    }

    fn finish_dynamic_import<'s>(&mut self, p: &bare_js_api::Persistent, m: u32) -> Result<(), Error<'s>> {
        self.api_finish_import(p, m)
    }

    fn reject_dynamic_import<'s>(&mut self, p: &bare_js_api::Persistent, reason: Local<'_>) -> Result<(), Error<'s>> {
        self.api_reject_import(p, reason)
    }

    fn fail_dynamic_import<'s>(&mut self, p: &bare_js_api::Persistent, message: &str) -> Result<(), Error<'s>> {
        self.api_fail_import(p, message)
    }

    /// Nothing yet: a safepoint needs the frames of the caller to be walkable, and a host function has none.
    fn safepoint(&mut self) {}

    fn type_error<'s>(&mut self, msg: &str) -> Error<'s> {
        self.a_throw(ErrKind::Type, msg)
    }

    fn range_error<'s>(&mut self, msg: &str) -> Error<'s> {
        self.a_throw(ErrKind::Range, msg)
    }

    fn charge<'s>(&mut self, n: u64) -> Result<(), Error<'s>> {
        match self.fuel.checked_sub(n) {
            Some(left) => {
                self.fuel = left;
                Ok(())
            }
            None => Err(Error::Abort(Abort::Limit("the step budget"))),
        }
    }

    fn bigint_limbs(&self, v: Local<'_>) -> usize {
        Realm::big_of(self.a_at(v)).map_or(0, |b| b.words().len())
    }

    fn to_number<'s>(&mut self, v: Local<'_>) -> Result<f64, Error<'s>> {
        let v = self.a_at(v);
        Realm::to_number(self, v).map_err(|e| self.a_err(e))
    }

    fn to_string<'s>(&mut self, v: Local<'_>) -> Result<Local<'s>, Error<'s>> {
        let v = self.a_at(v);
        let r = self.to_str(v).map(Str::value);
        self.a_ret(r)
    }

    fn to_boolean(&self, v: Local<'_>) -> bool {
        Realm::truthy(self.a_at(v))
    }

    fn new_array<'s>(&mut self, items: &[Local<'_>]) -> Result<Local<'s>, Error<'s>> {
        let vals = self.a_args(items);
        let a = Realm::new_array(self, &vals);
        Ok(self.a_push(a))
    }

    fn is_array<'s>(&mut self, v: Local<'_>) -> Result<bool, Error<'s>> {
        self.is_array_value(self.a_at(v)).map_err(|e| self.a_err(e))
    }

    fn intrinsic<'s>(&mut self, which: Intrinsic) -> Local<'s> {
        let v = match which {
            Intrinsic::ObjectPrototype => self.object_proto.value(),
            Intrinsic::ArrayPrototype => self.array_proto.value(),
            Intrinsic::ErrorPrototype => self.error_proto.value(),
            Intrinsic::IteratorPrototype => self.iterator_prototype(),
            Intrinsic::SymbolIterator => self.wk[crate::bi_symbol::ITERATOR].key_value(),
            Intrinsic::SymbolToStringTag => self.wk[crate::bi_symbol::TO_STRING_TAG].key_value(),
        };
        self.a_push(v)
    }

    fn get_prototype_of<'s>(&mut self, obj: Local<'_>) -> Result<Option<Local<'s>>, Error<'s>> {
        let o = self.a_obj(obj)?;
        let p = self.get_proto_internal(o).map_err(|e| self.a_err(e))?;
        Ok(p.map(|p| self.a_push(p.value())))
    }

    fn set_prototype_of<'s>(&mut self, obj: Local<'_>, proto: Option<Local<'_>>) -> Result<bool, Error<'s>> {
        let o = self.a_obj(obj)?;
        let p = match proto {
            None => None,
            Some(p) => Some(self.a_obj(p)?),
        };
        self.set_proto_checked(o, p).map_err(|e| self.a_err(e))
    }

    fn define_accessor<'s>(&mut self, obj: Local<'_>, key: Local<'_>, get: Option<Local<'_>>, set: Option<Local<'_>>, attrs: Attrs) -> Result<(), Error<'s>> {
        let (o, k) = (self.a_obj(obj)?, self.a_key(key)?);
        let mut fns = [Value::UNDEFINED; 2];
        for (slot, f) in fns.iter_mut().zip([get, set]) {
            if let Some(f) = f {
                *slot = self.a_at(f);
            }
            if !slot.is_undefined() && !Realm::obj(*slot).is_some_and(|f| f.is_callable()) {
                return Err(self.a_throw(ErrKind::Type, "an accessor is not a function"));
            }
        }
        let d = Desc { get: Some(fns[0]), set: Some(fns[1]), e: Some(attrs.enumerable), c: Some(attrs.configurable), ..Default::default() };
        self.a_define(o, k, &d)
    }

    fn new_array_buffer<'s>(&mut self, bytes: Vec<u8>) -> Result<Local<'s>, Error<'s>> {
        let proto = self.buffer_intrinsic("ArrayBuffer");
        let r = self.new_buffer(proto, bytes.len(), None).map(|o| {
            self.buffer_bytes_mut(o).expect("buffer").copy_from_slice(&bytes);
            o.value()
        });
        self.a_ret(r)
    }

    fn new_typed_array<'s>(&mut self, elem: bare_js_api::Elem, bytes: Vec<u8>) -> Result<Local<'s>, Error<'s>> {
        let Some(elem) = crate::bi_typed::Element::from_api(elem) else {
            return Err(Error::Abort(Abort::Unsupported("typed array element type")));
        };
        let width = elem.width();
        let r = if bytes.len() % width != 0 {
            crate::bi_buffer::range(self, "unaligned typed array byte length")
        } else {
            let p = self.buffer_intrinsic(elem.name());
            self.alloc_typed(p, elem, bytes.len() / width).map(|o| {
                let view = crate::bi_buffer::View::of(o).expect("typed array");
                self.buffer_bytes_mut(view.buffer).expect("buffer").copy_from_slice(&bytes);
                o.value()
            })
        };
        self.a_ret(r)
    }

    fn bytes(&self, value: Local<'_>) -> Option<&[u8]> {
        let (b, span) = self.byte_range(Self::obj(self.a_at(value))?)?;
        self.buffer_bytes(b)?.get(span)
    }

    fn bytes_mut(&mut self, value: Local<'_>) -> Option<&mut [u8]> {
        let (b, span) = self.byte_range(Self::obj(self.a_at(value))?)?;
        self.buffer_bytes_mut(b)?.get_mut(span)
    }

    fn view<'s>(&mut self, value: Local<'_>) -> Option<bare_js_api::View<'s>> {
        let v = crate::bi_buffer::View::of(Self::obj(self.a_at(value))?)?;
        let len = v.len();
        Some(bare_js_api::View {
            buffer: self.a_push(v.buffer.value()),
            offset: len.map_or(0, |_| v.offset),
            len: len.unwrap_or(0),
            elem: v.elem.map_or(bare_js_api::Elem::Data, crate::bi_typed::Element::api),
        })
    }

    fn is_detached(&self, value: Local<'_>) -> bool {
        Self::obj(self.a_at(value)).is_some_and(|o| self.buffer_detached(o))
    }

    fn view_get(&self, value: Local<'_>, i: usize) -> Option<f64> {
        let view = crate::bi_buffer::View::of(Self::obj(self.a_at(value))?)?;
        if view.elem?.is_big() {
            return None;
        }
        view.read(self, i).map(f64::from_bits)
    }

    fn proto_from_new_target<'s>(&mut self, default: Local<'_>) -> Result<Local<'s>, Error<'s>> {
        let (callee, nt) = (self.callee, self.nt);
        if nt.is_undefined() || nt == callee {
            return Ok(self.a_push(self.a_at(default)));
        }
        let p = self.get_prop(nt, Key::Name(self.k_prototype)).map_err(|e| self.a_err(e))?;
        let p = if Realm::obj(p).is_some() { p } else { self.object_proto.value() };
        Ok(self.a_push(p))
    }

    fn string_utf16<'s>(&mut self, units: &[u16]) -> Result<Local<'s>, Error<'s>> {
        match Str::new(&mut self.heap, Chars::Two(units)) {
            Ok(s) => Ok(self.a_push(s.value())),
            Err(_) => Err(Error::Abort(Abort::Limit("a string that is too long or no memory"))),
        }
    }

    fn wall_now_ms(&self) -> f64 {
        self.clock_now_ms()
    }

    fn own_kind<'s>(&mut self, obj: Local<'_>, key: Local<'_>) -> Result<Option<ApiOwn<'s>>, Error<'s>> {
        let (o, k) = (self.a_obj(obj)?, self.a_key(key)?);
        Ok(match self.get_own_h(o, k).map_err(|e| self.a_err(e))? {
            None => None,
            Some(Own::Data(v, _)) => Some(ApiOwn::Data(self.a_push(v))),
            Some(Own::Acc(g, s, _)) => {
                let get = Some(g).filter(|v| !v.is_undefined()).map(|v| self.a_push(v));
                let set = Some(s).filter(|v| !v.is_undefined()).map(|v| self.a_push(v));
                Some(ApiOwn::Accessor { get, set })
            }
        })
    }

    fn set_integrity<'s>(&mut self, obj: Local<'_>, frozen: bool) -> Result<bool, Error<'s>> {
        let o = self.a_obj(obj)?;
        if o.has_flag(crate::object::F_HOST) {
            return Ok(false);
        }
        self.lock(o, frozen).map(|()| true).map_err(|e| self.a_err(e))
    }

    fn has_own<'s>(&mut self, obj: Local<'_>, key: Local<'_>) -> Result<bool, Error<'s>> {
        let (o, k) = (self.a_obj(obj)?, self.a_key(key)?);
        self.get_own_h(o, k).map(|p| p.is_some()).map_err(|e| self.a_err(e))
    }

    fn own_keys<'s>(&mut self, obj: Local<'_>) -> Result<Vec<Local<'s>>, Error<'s>> {
        let o = self.a_obj(obj)?;
        let keys = self.own_key_list_h(o, false).map_err(|e| self.a_err(e))?;
        Ok(keys.into_iter().map(|k| self.a_push(k)).collect())
    }

    fn own_enumerable_data<'s>(&mut self, obj: Local<'_>, key: Local<'_>) -> Result<Option<Local<'s>>, Error<'s>> {
        let (o, k) = (self.a_obj(obj)?, self.a_key(key)?);
        match self.get_own_h(o, k).map_err(|e| self.a_err(e))? {
            Some(p) if p.attrs() & A_E != 0 => {
                let r = self.get_prop(o.value(), k);
                self.a_ret(r).map(Some)
            }
            _ => Ok(None),
        }
    }

    fn is_callable(&self, v: Local<'_>) -> bool {
        Realm::obj(self.a_at(v)).is_some_and(|o| o.is_callable())
    }

    fn bind<'s>(&mut self, target: Local<'_>, this: Local<'_>) -> Result<Local<'s>, Error<'s>> {
        let (t, this) = (self.a_at(target), self.a_at(this));
        if !Realm::obj(t).is_some_and(|o| o.is_callable()) {
            return Err(self.a_throw(ErrKind::Type, "bind needs a function"));
        }
        let b = self.native_cell(self.bound_id as usize);
        b.set_slot(&self.shapes, 1, t);
        b.set_slot(&self.shapes, 2, this);
        b.set_slot(&self.shapes, 3, Value::UNDEFINED);
        Ok(self.a_push(b.value()))
    }

    fn task<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> Result<R, Abort> {
        if self.api.tasks >= MAX_TASKS {
            return Err(Abort::Limit("the call depth"));
        }
        if self.api.tasks == 0 {
            self.fuel = self.api.budget;
        }
        self.api.tasks += 1;
        let r = f(self);
        self.api.tasks -= 1;
        Ok(r)
    }

    fn construct<'s>(&mut self, f: Local<'_>, args: &[Local<'_>], new_target: Option<Local<'_>>) -> Result<Local<'s>, Error<'s>> {
        let f = self.a_at(f);
        let nt = new_target.map_or(f, |n| self.a_at(n));
        let args = self.a_args(args);
        let r = self.a_construct(f, &args, nt);
        self.a_ret(r)
    }

    fn array_len<'s>(&mut self, v: Local<'_>) -> Result<Option<u32>, Error<'s>> {
        Ok(Realm::obj(self.a_at(v)).filter(|&o| Realm::is_array(self, o)).map(|o| o.len()))
    }

    fn new_error<'s>(&mut self, proto: Local<'_>, msg: &str) -> Result<Local<'s>, Error<'s>> {
        let p = self.a_obj(proto)?;
        let m = self.string(msg);
        let e = self.make_error(p, Some(m));
        self.capture_here(e, None, false, true);
        Ok(self.a_push(e))
    }
}

impl Realm {
    fn a_define<'s>(&mut self, o: Obj, k: Key, d: &Desc) -> Result<(), Error<'s>> {
        match self.define_own(o, k, d) {
            Ok(true) => Ok(()),
            Ok(false) => Err(self.a_throw(ErrKind::Type, "cannot define the property")),
            Err(e) => Err(self.a_err(e)),
        }
    }
}

#[path = "api_module.rs"]
mod modules;

#[cfg(test)]
#[allow(unsafe_code)]
#[path = "api_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "api_clone_tests.rs"]
mod clone_tests;

#[cfg(test)]
#[path = "api_error_stack_tests.rs"]
mod error_stack_tests;
