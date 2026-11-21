//! `Promise`, the microtask queue and the tracking of rejections (`docs/design-js2-async.md` 7.1 to 7.4, plan step P1). Safe code over
//! [`Realm`]; the one place that runs script is [`Realm::drain_jobs`].
//!
//! **A promise** is an `Object` cell with the flag `F_PROMISE` and two hidden slots, 32 bytes in all. Slot 0 is the state: an int, the
//! state in the low two bits (pending, fulfilled, rejected) and the `handled` bit (4). Slot 1 is the value of a settled promise, or,
//! while it is pending, the newest reaction (a list that grows at the head). There is no side table. The slots are the first two of
//! every shape that a promise has (`Promises::shape`, a script that adds a property only appends), and the hidden keys cannot be
//! deleted, so the slot numbers are fixed.
//!
//! **A reaction** is a `Slots` record (`Ctx`) of five values, 48 bytes: `next`, `kind`, `a`, `b` and `derived`. On a pending promise it
//! is `K_THEN` with `a` = `onFulfilled` and `b` = `onRejected`. When the promise settles the reaction is changed in place into a job:
//! `kind` is the new state, `a` is the handler that applies and `b` is the value to give it, and `next` becomes the queue link. So a
//! `then` allocates the reaction and the derived promise and nothing else, and a settle allocates nothing. `derived` is the promise
//! that the `then` returned, a capability record `[promise, resolve, reject]` for a constructor that is not `Promise`, or `undefined`.
//!
//! **The queue** is a FIFO of those records, linked by `next`: `head` and `tail` in [`Promises`]. The collector marks `head`, and the
//! rest is reachable from it. The two other kinds of job are records too: `K_THENABLE` (the promise to resolve, the thenable, its
//! `then`) and `K_CALLBACK` (`queueMicrotask`). The hook for an `await` (design 7.3) is a new kind here: an activation that is its
//! own reaction (step A1).
//!
//! **Who runs jobs.** The host: `Engine::drain_microtasks` and `checkpoint` (`api.rs`), or [`Vm::drain_microtasks`] for tools, both
//! called at the top with the JS stack empty. `Vm::call` and the end of a script never drain. Each job is a separate entry at the top
//! (a pooled machine, no nesting, no hold of the collector), so the collector may run inside a job. The run is [`Realm::drain_jobs`]. The job in progress is `Promises::cur`, a root: the handler, the argument and the derived promise stay reachable
//! through it. A handler that is a native (a resolving function, `Promise.resolve` as a handler) runs through `Realm::call_native`.
//!
//! **Resolving functions** are two native function cells that share the "already resolved" flag without a third cell: slot 1 of each
//! is the promise, slot 2 is the other function. A call clears slot 1 of both. A thenable that is a native promise with the built-in
//! `then` and an own-free `constructor` is followed with a reaction (no functions, no derived promise): the same ticks as the
//! specification, no allocation but the reaction.
//!
//! **Rejections** follow the HTML hook `HostPromiseRejectionTracker`: a promise that is rejected with no handler goes to `rejected`
//! (at most 1,024, the rest is counted in `dropped`). [`Realm::take_rejections`] hands them to the host, and keeps them in `reported`,
//! a weak list (at most 256), so a handler that comes later is reported as `handled`.
//!
//! **Limits** (checked before anything grows, `RangeError`): `max_queue` on a push to the queue from a `then`, a `queueMicrotask` or a
//! resolve, `max_reactions` on a reaction that waits on a pending promise, `max_jobs_per_drain` in the drain (it fails the drain until
//! [`Realm::clear_failed_job`]). A settle moves the waiting reactions to the queue with no check: they were counted when they came.
//! The reaction count is set to 0 at each collection, because a promise that died with reactions on it cannot give them back, so it
//! counts the reactions that were added since the last collection and are still pending.
//!
//! **Not here:** the statics and `finally` are in `promise_statics.rs`. `SpeciesConstructor` is there too (`then` calls it). A constructor
//! that is a script function or class is constructed by `Realm::construct_script`.
use crate::lazy::{Ent, m};
use crate::builtins::{Native, NativeFn, arg};
use crate::cell::Layout;
use crate::gc::Marker;
use crate::heap::Heap;
use crate::interp::{STOP, Vm};
use crate::interp_rt::{Key, R, Realm, Slot};
use crate::object::{Ctx, K_PROMISE, Obj, Spot};
use crate::shapes::{ShapeId, Shapes};
use crate::string::Str;
use crate::value::Value;

const PENDING: i32 = 0;
const FULFILLED: i32 = 1;
const REJECTED: i32 = 2;
const HANDLED: i32 = 4;

/// Reaction record slots.
const NEXT: usize = 0;
const KIND: usize = 1;
const A: usize = 2;
const B: usize = 3;
const D: usize = 4;
const REACTION_WORDS: usize = 5;

/// A reaction on a pending promise: `a` is `onFulfilled`, `b` is `onRejected`.
const K_THEN: i32 = 0;
/// A queued reaction: `kind` is the state of the promise (`FULFILLED` or `REJECTED`), `a` the handler and `b` the value.
const K_FULFIL: i32 = FULFILLED;
const K_REJECT: i32 = REJECTED;
/// `NewPromiseResolveThenableJob`: `a` is the thenable, `b` its `then`, `derived` the promise to resolve.
const K_THENABLE: i32 = 3;
/// `queueMicrotask`: `a` is the callback.
const K_CALLBACK: i32 = 4;

/// The unhandled rejections that wait for the host, and the reported ones that wait for a handler.
const MAX_REJECTED: usize = 1024;
const MAX_REPORTED: usize = 256;

/// The limits of a realm (`design-js2-async.md` 7.5). A test changes them.
#[derive(Clone, Copy, Debug)]
pub struct PromiseLimits {
    pub max_queue: u32,
    pub max_reactions: u32,
    pub max_jobs_per_drain: u32,
}

impl Default for PromiseLimits {
    fn default() -> Self {
        PromiseLimits { max_queue: 1 << 18, max_reactions: 1 << 20, max_jobs_per_drain: 1 << 20 }
    }
}

/// Why a drain stopped early. The failure is sticky: the drain fails the same way until [`Realm::clear_failed_job`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrainStop {
    /// The budget ran out in a job (or the host gave an abort: `ApiState::abort`).
    Fuel,
    /// `max_jobs_per_drain` ran out.
    Limit,
    /// `Host::between_jobs` threw this value. It is a root while the failure lasts.
    Throw(Value),
}

/// What the host gets at the end of a task (HTML "notify about rejected promises").
#[derive(Debug, Default)]
pub struct Rejections {
    /// Promises that were rejected with no handler since the last call, and still have none.
    pub unhandled: Vec<Value>,
    /// Promises that were reported before and got a handler since.
    pub handled: Vec<Value>,
    /// Rejections over the bound of the pending list (`MAX_REJECTED`) since the last call.
    pub dropped: u32,
}

/// The state of the promise built-ins in a realm.
pub(crate) struct Promises {
    pub(crate) proto: Obj,
    pub(crate) shape: ShapeId,
    /// `%Promise%` and the built-in `then` (both are also `consts`).
    pub(crate) ctor: Value,
    pub(crate) then_fn: Value,
    pub(crate) k_then: Str,
    pub(crate) k_state: Str,
    pub(crate) k_result: Str,
    /// Where `Promise.prototype.constructor` is, for the shape that `Promise.prototype` has now (a cache for `then`).
    ctor_spot: Option<Spot>,
    /// The job queue and the job that runs now.
    head: Value,
    tail: Value,
    cur: Value,
    queued: u32,
    reactions: u32,
    rejected: Vec<Value>,
    reported: Vec<Value>,
    handled: Vec<Value>,
    dropped: u32,
    failed: Option<DrainStop>,
    pub(crate) limits: PromiseLimits,
    /// The indexes in `Realm::natives` of the resolving functions and of the executor of a capability.
    id_resolve: usize,
    id_reject: usize,
    pub(crate) id_executor: usize,
    /// The natives of `promise_statics` (`N_ALL` and the others), the built-in getter of `Promise[Symbol.species]` (a root) and
    /// `AggregateError.prototype` (a root).
    pub(crate) ids: [usize; crate::promise_statics::N_IDS],
    pub(crate) species_fn: Value,
    pub(crate) species_spot: Option<Spot>,
    pub(crate) agg_proto: Obj,
}

impl Promises {
    pub(crate) fn new(heap: &mut Heap, shapes: &mut Shapes, base: ShapeId, k_then: Str, k_state: Str, k_result: Str) -> Promises {
        let proto = Obj::new(heap, shapes, base).expect("memory");
        let shape = Promises::shape_of(shapes, proto, k_state, k_result);
        Promises {
            proto,
            shape,
            ctor: Value::UNDEFINED,
            then_fn: Value::UNDEFINED,
            k_then,
            k_state,
            k_result,
            ctor_spot: None,
            head: Value::UNDEFINED,
            tail: Value::UNDEFINED,
            cur: Value::UNDEFINED,
            queued: 0,
            reactions: 0,
            rejected: Vec::new(),
            reported: Vec::new(),
            handled: Vec::new(),
            dropped: 0,
            failed: None,
            limits: PromiseLimits::default(),
            id_resolve: 0,
            id_reject: 0,
            id_executor: 0,
            ids: [0; crate::promise_statics::N_IDS],
            species_fn: Value::UNDEFINED,
            species_spot: None,
            agg_proto: proto,
        }
    }

    /// The shape of a promise whose prototype is `proto`: two hidden slots, state and value.
    fn shape_of(shapes: &mut Shapes, proto: Obj, k_state: Str, k_result: Str) -> ShapeId {
        let root = shapes.root(Layout::Object, Some(proto.cell()), 2);
        let s = shapes.add(root, k_state, 0).expect("shape");
        shapes.add(s, k_result, 0).expect("shape")
    }

    /// Mark what the realm keeps for promises. `reported` is weak: [`Promises::weak`].
    pub(crate) fn walk(&self, m: &mut Marker) {
        m.values(&[self.proto.value(), self.ctor, self.then_fn, self.head, self.cur]);
        m.values(&self.rejected);
        m.values(&self.handled);
        if let Some(DrainStop::Throw(v)) = self.failed {
            m.value(v);
        }
        m.value(self.k_then.value());
        m.value(self.k_state.value());
        m.value(self.k_result.value());
    }

    /// The weak list: `Realm::collect` drops the promises that died from it, after the marking.
    pub(crate) fn weak_list(&mut self) -> &mut Vec<Value> {
        &mut self.reported
    }

    /// The collector ran: the reactions that were counted may be dead.
    pub(crate) fn collected(&mut self) {
        self.reactions = 0;
    }
}

/// The promise of a value.
#[inline]
fn promise_of(v: Value) -> Option<Obj> {
    Realm::obj(v).filter(|o| o.is_kind(K_PROMISE))
}

fn is_callable(v: Value) -> bool {
    Realm::obj(v).is_some_and(|o| o.is_callable())
}

/// A handler that is not callable is `undefined`.
fn handler(v: Value) -> Value {
    if is_callable(v) { v } else { Value::UNDEFINED }
}

impl Realm {
    // ---- promise cells ----

    /// A pending promise with `Promise.prototype`.
    pub fn new_promise(&mut self) -> Obj {
        self.promise_in(self.pr.shape)
    }

    fn promise_in(&mut self, shape: ShapeId) -> Obj {
        Obj::new_promise(&mut self.heap, &self.shapes, shape).expect("memory")
    }

    /// A pending promise with the prototype `proto`.
    fn promise_with_proto(&mut self, proto: Obj) -> Obj {
        if proto == self.pr.proto {
            return self.new_promise();
        }
        let shape = Promises::shape_of(&mut self.shapes, proto, self.pr.k_state, self.pr.k_result);
        self.promise_in(shape)
    }

    #[inline]
    fn state_word(&self, p: Obj) -> i32 {
        p.pslot(0).as_int().expect("a promise state")
    }

    /// The state of a promise as the host sees it: `(state, value)` with state 0 pending, 1 fulfilled, 2 rejected. The value is
    /// `undefined` while it is pending. `None` when `v` is not a promise.
    pub fn promise_status(&self, v: Value) -> Option<(i32, Value)> {
        let p = promise_of(v)?;
        let s = self.state_word(p) & 3;
        Some((s, if s == PENDING { Value::UNDEFINED } else { p.pslot(1) }))
    }

    /// The promise of a value (`None` when it is no native promise).
    pub(crate) fn promise_obj(v: Value) -> Option<Obj> {
        promise_of(v)
    }

    /// Does the host have rejections to hear (`take_rejections` gives something)?
    pub(crate) fn rejections_wait(&self) -> bool {
        !self.pr.rejected.is_empty() || !self.pr.handled.is_empty() || self.pr.dropped != 0
    }

    /// `PerformPromiseThen(p, ok, err)` for the host: handlers that are no functions are skipped, and the derived promise is a new one
    /// that nobody holds (a throw of a handler is then an unhandled rejection). It reads no `then` and no `constructor`.
    pub(crate) fn promise_then_host(&mut self, p: Obj, ok: Value, err: Value) -> R<()> {
        let d = self.new_promise();
        self.perform_then(p, handler(ok), handler(err), d.value())
    }

    /// `PromiseResolve(%Promise%, x)`: `x` when it is a native promise whose `constructor` is `%Promise%` (the read may run a getter), else
    /// a new promise resolved with `x`.
    pub(crate) fn promise_resolve_default(&mut self, x: Value) -> R<Obj> {
        if let Some(xp) = promise_of(x) {
            let c = self.get_prop(x, Key::Name(self.k_constructor))?;
            if c == self.pr.ctor {
                return Ok(xp);
            }
        }
        let p = self.new_promise();
        self.resolve_promise(p, x)?;
        Ok(p)
    }

    /// The number of jobs that wait in the queue.
    pub fn pending_jobs(&self) -> usize {
        self.pr.queued as usize
    }

    /// End the failure of a drain (a limit): the next drain runs.
    pub fn clear_failed_job(&mut self) {
        self.pr.failed = None;
    }

    // ---- reactions and the queue ----

    fn reaction(&mut self, kind: i32, a: Value, b: Value, derived: Value) -> Ctx {
        let r = Ctx::new(&mut self.heap, REACTION_WORDS, Value::UNDEFINED).expect("memory");
        r.set(KIND, Value::int(kind));
        r.set(A, a);
        r.set(B, b);
        r.set(D, derived);
        r
    }

    fn push_job(&mut self, r: Ctx) {
        let pr = &mut self.pr;
        r.set(NEXT, Value::UNDEFINED);
        match Realm::ctx(pr.tail) {
            Some(t) => t.set(NEXT, r.value()),
            None => pr.head = r.value(),
        }
        pr.tail = r.value();
        pr.queued += 1;
    }

    fn pop_job(&mut self) -> Option<Ctx> {
        let r = Realm::ctx(self.pr.head)?;
        self.pr.head = r.get(NEXT);
        if self.pr.head.is_undefined() {
            self.pr.tail = Value::UNDEFINED;
        }
        self.pr.queued -= 1;
        Some(r)
    }

    /// `queueMicrotask(f)`: `TypeError` when `f` is not callable, `RangeError` when the queue is full.
    pub(crate) fn queue_callback(&mut self, f: Value) -> R<()> {
        if !is_callable(f) {
            return self.type_error("queueMicrotask needs a function");
        }
        self.room()?;
        let r = self.reaction(K_CALLBACK, f, Value::UNDEFINED, Value::UNDEFINED);
        self.push_job(r);
        Ok(())
    }

    /// Room for one more job? `RangeError` when not.
    fn room(&mut self) -> R<()> {
        if self.pr.queued >= self.pr.limits.max_queue {
            return Err(self.error(crate::interp_rt::ErrKind::Range, "the microtask queue is full"));
        }
        Ok(())
    }

    // ---- settle and resolve ----

    /// Settle the pending promise `p` with `v` and queue its reactions in the order they were added.
    fn settle(&mut self, p: Obj, st: i32, v: Value) {
        let word = self.state_word(p);
        debug_assert!(word & 3 == PENDING, "settle of a settled promise");
        let mut list = p.pslot(1);
        p.set_pslot(0, Value::int(st | word & HANDLED));
        p.set_pslot(1, v);
        if st == REJECTED && word & HANDLED == 0 {
            self.track_reject(p);
        }
        // The list has the newest first: reverse it, then queue.
        let (mut rev, mut n) = (Value::UNDEFINED, 0u32);
        while let Some(r) = Realm::ctx(list) {
            let next = r.get(NEXT);
            r.set(NEXT, rev);
            (rev, list) = (list, next);
            n += 1;
        }
        self.pr.reactions = self.pr.reactions.saturating_sub(n);
        while let Some(r) = Realm::ctx(rev) {
            let next = r.get(NEXT);
            let h = if st == FULFILLED { r.get(A) } else { r.get(B) };
            r.set(KIND, Value::int(st));
            r.set(A, h);
            r.set(B, v);
            self.push_job(r);
            rev = next;
        }
    }

    pub(crate) fn reject_promise(&mut self, p: Obj, e: Value) {
        self.settle(p, REJECTED, e);
    }

    /// The promise resolve function without its flag: `resolution` fulfills `p`, or `p` follows it when it is a thenable. A throw
    /// is a rejection; only `STOP` (the budget ran out in a getter) is an `Err`.
    pub(crate) fn resolve_promise(&mut self, p: Obj, x: Value) -> R<()> {
        let Some(xo) = Realm::obj(x) else {
            self.settle(p, FULFILLED, x);
            return Ok(());
        };
        if xo == p {
            let e = self.error(crate::interp_rt::ErrKind::Type, "chaining cycle detected for promise");
            self.settle(p, REJECTED, e);
            return Ok(());
        }
        let then = match self.get_prop(x, Key::Name(self.pr.k_then)) {
            Ok(t) => t,
            Err(e) if e == STOP => return Err(e),
            Err(e) => {
                self.settle(p, REJECTED, e);
                return Ok(());
            }
        };
        if !is_callable(then) {
            self.settle(p, FULFILLED, x);
            return Ok(());
        }
        if let Err(e) = self.room() {
            self.settle(p, REJECTED, e);
            return Ok(());
        }
        let r = self.reaction(K_THENABLE, x, then, p.value());
        self.push_job(r);
        Ok(())
    }

    /// The two resolving functions of `p`.
    pub(crate) fn resolving_functions(&mut self, p: Obj) -> (Obj, Obj) {
        let res = self.native_cell(self.pr.id_resolve);
        let rej = self.native_cell(self.pr.id_reject);
        res.set_slot(&self.shapes, 1, p.value());
        res.set_slot(&self.shapes, 2, rej.value());
        rej.set_slot(&self.shapes, 1, p.value());
        rej.set_slot(&self.shapes, 2, res.value());
        (res, rej)
    }

    /// The body of a resolving function `f`: nothing when the pair was used.
    fn resolving_call(&mut self, f: Obj, v: Value, reject: bool) -> R<()> {
        let Some(p) = promise_of(f.slot(&self.shapes, 1)) else { return Ok(()) };
        let other = f.slot(&self.shapes, 2);
        f.set_slot(&self.shapes, 1, Value::UNDEFINED);
        if let Some(o) = Realm::obj(other) {
            o.set_slot(&self.shapes, 1, Value::UNDEFINED);
        }
        if reject {
            self.reject_promise(p, v);
            Ok(())
        } else {
            self.resolve_promise(p, v)
        }
    }

    // ---- then ----

    /// `p.constructor`. A promise with the default shape (no own property, prototype `Promise.prototype`) reads it from the spot of
    /// `Promise.prototype`; the spot is made again when a script changed the shape of the prototype.
    pub(crate) fn promise_constructor(&mut self, p: Obj) -> R<Value> {
        let default = p.shape(&self.shapes) == self.pr.shape;
        if default && let Some(v) = self.pr.ctor_spot.and_then(|s| self.pr.proto.read_spot(s)) {
            return Ok(v);
        }
        let kc = self.k_constructor;
        match self.lookup_g(p, kc) {
            Some(Slot::Data(v)) => {
                if default {
                    let proto = self.pr.proto;
                    self.pr.ctor_spot = self.shapes.find(proto.shape(&self.shapes), kc).and_then(|e| proto.spot(&self.shapes, e.slot()));
                }
                Ok(v)
            }
            _ => self.get_prop(p.value(), Key::Name(kc)),
        }
    }

    /// `PerformPromiseThen`: `derived` is a promise, a capability record or `undefined`.
    pub(crate) fn perform_then(&mut self, p: Obj, on_ok: Value, on_err: Value, derived: Value) -> R<()> {
        let word = self.state_word(p);
        match word & 3 {
            PENDING => {
                if self.pr.reactions >= self.pr.limits.max_reactions {
                    return Err(self.error(crate::interp_rt::ErrKind::Range, "too many promise reactions"));
                }
                let r = self.reaction(K_THEN, on_ok, on_err, derived);
                r.set(NEXT, p.pslot(1));
                p.set_pslot(1, r.value());
                self.pr.reactions += 1;
            }
            st => {
                self.room()?;
                let h = if st == FULFILLED { on_ok } else { on_err };
                let r = self.reaction(st, h, p.pslot(1), derived);
                if st == REJECTED && word & HANDLED == 0 {
                    self.track_handle(p);
                }
                self.push_job(r);
            }
        }
        p.set_pslot(0, Value::int(self.state_word(p) | HANDLED));
        Ok(())
    }

    /// `NewPromiseCapability(c)`: the derived value for `then` and the statics. `%Promise%` gives a plain promise; another constructor
    /// is called with an executor that keeps the two functions, and the result is a record `[promise, resolve, reject]`.
    fn new_capability(&mut self, c: Value) -> R<Value> {
        if c == self.pr.ctor {
            return Ok(self.new_promise().value());
        }
        if !is_callable(c) {
            return self.type_error("not a constructor");
        }
        let ex = self.native_cell(self.pr.id_executor);
        let promise = self.construct_value(c, &[ex.value()])?;
        let (res, rej) = (ex.slot(&self.shapes, 1), ex.slot(&self.shapes, 2));
        if !is_callable(res) || !is_callable(rej) {
            return self.type_error("the promise capability functions are not callable");
        }
        let cap = Ctx::new(&mut self.heap, 3, Value::UNDEFINED).expect("memory");
        cap.set(0, promise);
        cap.set(1, res);
        cap.set(2, rej);
        Ok(cap.value())
    }

    /// The promise of a derived value (`then` returns it).
    fn derived_promise(d: Value) -> Value {
        Realm::ctx(d).map_or(d, |c| c.get(0))
    }

    // ---- rejections ----

    fn track_reject(&mut self, p: Obj) {
        if self.pr.rejected.len() >= MAX_REJECTED {
            self.pr.dropped += 1;
        } else {
            self.pr.rejected.push(p.value());
        }
    }

    /// A handler was added to the rejected promise `p` that had none.
    fn track_handle(&mut self, p: Obj) {
        let v = p.value();
        if let Some(i) = self.pr.rejected.iter().rposition(|x| *x == v) {
            self.pr.rejected.remove(i);
        } else if let Some(i) = self.pr.reported.iter().rposition(|x| *x == v) {
            self.pr.reported.remove(i);
            self.pr.handled.push(v);
        }
    }

    /// The end of a task: the rejections that the host has not seen. The promises are not rooted: the host uses them at once.
    pub fn take_rejections(&mut self) -> Rejections {
        let unhandled = std::mem::take(&mut self.pr.rejected);
        let reported = &mut self.pr.reported;
        // A full list drops its oldest entries (they just lose the later "handled" report), as the old engine does.
        reported.extend(unhandled.iter().copied());
        if reported.len() > MAX_REPORTED {
            reported.drain(..reported.len() - MAX_REPORTED);
        }
        Rejections { unhandled, handled: std::mem::take(&mut self.pr.handled), dropped: std::mem::take(&mut self.pr.dropped) }
    }

    // ---- installation ----

    pub(crate) fn install_promise(&mut self) {
        for (f, len) in [(resolve_fn as NativeFn, 1), (reject_fn, 1), (executor_fn, 2)] {
            self.natives.push(Native { f, ctor: false, len, name: "" });
        }
        let n = self.natives.len();
        (self.pr.id_resolve, self.pr.id_reject, self.pr.id_executor) = (n - 3, n - 2, n - 1);
        let proto = self.pr.proto;
        let c = self.ctor("Promise", 1, promise_ctor, proto);
        self.pr.ctor = c.value();
        let then = self.new_native(promise_then, false, 2, "then");
        self.define_attrs(proto, Key::Name(self.pr.k_then), crate::builtins::HIDDEN, then.value()).expect("define");
        self.pr.then_fn = then.value();
        static PROMISE_PROTO: &[Ent] = &[m("catch", 1, promise_catch)];
        self.lazy_table(proto, PROMISE_PROTO);
        static PROMISE_STATICS: &[Ent] = &[m("resolve", 1, promise_resolve), m("reject", 1, promise_reject)];
        self.lazy_table(c, PROMISE_STATICS);
        let g = self.global;
        static GLOBAL_PROMISE: &[Ent] = &[m("queueMicrotask", 1, queue_microtask)];
        self.lazy_table(g, GLOBAL_PROMISE);
        self.install_promise_statics();
    }
}

// ---- the built-ins ----

/// A resolving function (slots 1 and 2 hold the promise and the other function).
fn resolve_fn(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let f = Realm::obj(rt.callee).expect("a resolving function");
    rt.resolving_call(f, arg(args, 0), false)?;
    Ok(Value::UNDEFINED)
}

fn reject_fn(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let f = Realm::obj(rt.callee).expect("a resolving function");
    rt.resolving_call(f, arg(args, 0), true)?;
    Ok(Value::UNDEFINED)
}

/// The executor of `NewPromiseCapability`: it keeps `resolve` and `reject` in its slots 1 and 2.
fn executor_fn(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let f = Realm::obj(rt.callee).expect("an executor");
    if !f.slot(&rt.shapes, 1).is_undefined() || !f.slot(&rt.shapes, 2).is_undefined() {
        return rt.type_error("the promise capability executor was called twice");
    }
    f.set_slot(&rt.shapes, 1, arg(args, 0));
    f.set_slot(&rt.shapes, 2, arg(args, 1));
    Ok(Value::UNDEFINED)
}

fn promise_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    if Realm::obj(rt.nt).is_none() {
        return rt.type_error("Promise constructor cannot be invoked without 'new'");
    }
    let ex = arg(args, 0);
    if !is_callable(ex) {
        return rt.type_error("Promise resolver is not a function");
    }
    let nt = rt.nt;
    let proto = match Realm::obj(rt.get_prop(nt, Key::Name(rt.k_prototype))?) {
        Some(p) => p,
        None => rt.pr.proto,
    };
    let p = rt.promise_with_proto(proto);
    let (res, rej) = rt.resolving_functions(p);
    match rt.call_function(ex, Value::UNDEFINED, &[res.value(), rej.value()]) {
        Ok(_) => {}
        Err(e) if e == STOP => return Err(e),
        Err(e) => rt.resolving_call(rej, e, true)?,
    }
    Ok(p.value())
}

fn promise_then(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let Some(p) = promise_of(this) else { return rt.type_error("Promise.prototype.then called on a value that is not a promise") };
    let c = rt.species_constructor(p)?;
    let derived = rt.new_capability(c)?;
    rt.perform_then(p, handler(arg(args, 0)), handler(arg(args, 1)), derived)?;
    Ok(Realm::derived_promise(derived))
}

/// `catch(onRejected)` is `this.then(undefined, onRejected)`: a tail call, so a script `then` runs in the same loop.
fn promise_catch(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let then = rt.get_prop(this, Key::Name(rt.pr.k_then))?;
    if !is_callable(then) {
        return rt.type_error("then is not a function");
    }
    rt.tail_args.clear();
    rt.tail_args.extend([Value::UNDEFINED, arg(args, 0)]);
    rt.tail = Some((then, this));
    Ok(Value::UNDEFINED)
}

/// `Promise.resolve(x)`.
fn promise_resolve(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    if Realm::obj(this).is_none() {
        return rt.type_error("Promise.resolve called on a value that is not an object");
    }
    rt.promise_resolve_c(this, arg(args, 0))
}

impl Realm {
    /// `PromiseResolve(c, x)`: `x` itself when it is a promise made by the same constructor, else a new promise resolved with `x`.
    pub(crate) fn promise_resolve_c(&mut self, c: Value, x: Value) -> R<Value> {
        if promise_of(x).is_some() && self.get_prop(x, Key::Name(self.k_constructor))? == c {
            return Ok(x);
        }
        let cap = self.new_capability(c)?;
        match Realm::ctx(cap) {
            None => self.resolve_promise(promise_of(cap).expect("a promise"), x)?,
            Some(c) => {
                self.call_function(c.get(1), Value::UNDEFINED, &[x])?;
            }
        }
        Ok(Realm::derived_promise(cap))
    }
}

fn promise_reject(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    if Realm::obj(this).is_none() {
        return rt.type_error("Promise.reject called on a value that is not an object");
    }
    let cap = rt.new_capability(this)?;
    match Realm::ctx(cap) {
        None => rt.reject_promise(promise_of(cap).expect("a promise"), arg(args, 0)),
        Some(c) => {
            rt.call_function(c.get(2), Value::UNDEFINED, &[arg(args, 0)])?;
        }
    }
    Ok(Realm::derived_promise(cap))
}

fn queue_microtask(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    rt.queue_callback(arg(args, 0))?;
    Ok(Value::UNDEFINED)
}

// ---- running jobs ----

impl Vm {
    /// Run the jobs until the queue is empty. The host calls it at the top (the JS stack is empty), at the end of a task. `on_error`
    /// gets the value that a `queueMicrotask` callback threw (the host reports it) and the throw of a capability function; the drain
    /// goes on. Gives the number of jobs that ran. The browser does not come here: it uses `Engine::drain_microtasks` (`api.rs`).
    pub fn drain_microtasks(&mut self, on_error: &mut dyn FnMut(&mut Realm, Value)) -> Result<u32, DrainStop> {
        self.realm.fuel = self.fuel;
        let mut n = 0;
        let r = self.realm.drain_jobs(&mut n, None, on_error);
        self.fuel = self.realm.fuel;
        r.map(|()| n)
    }
}

/// The hook that runs before each job and once when the queue is empty (`Host::between_jobs`).
pub(crate) type Between<'a> = &'a mut dyn FnMut(&mut Realm) -> Result<(), DrainStop>;

impl Realm {
    /// Run the jobs at the top (no native on the Rust stack) until the queue is empty. Each job is its own entry on a pooled machine, with
    /// no hold, so the collector may run inside it. `n` counts the jobs of the checkpoint across calls. Any stop is sticky.
    pub(crate) fn drain_jobs(&mut self, n: &mut u32, mut between: Option<Between>, on_error: &mut dyn FnMut(&mut Realm, Value)) -> Result<(), DrainStop> {
        if let Some(f) = self.pr.failed {
            return Err(f);
        }
        loop {
            if let Some(b) = between.as_mut()
                && let Err(s) = b(self)
            {
                self.pr.failed = Some(s);
                return Err(s);
            }
            if self.pr.queued == 0 {
                return Ok(());
            }
            if *n >= self.pr.limits.max_jobs_per_drain {
                self.pr.failed = Some(DrainStop::Limit);
                return Err(DrainStop::Limit);
            }
            let r = self.pop_job().expect("a queued job");
            *n += 1;
            self.pr.cur = r.value();
            let done = self.run_job(r, on_error);
            self.pr.cur = Value::UNDEFINED;
            if done.is_err() {
                self.pr.failed = Some(DrainStop::Fuel);
                return Err(DrainStop::Fuel);
            }
        }
    }

    /// Call the handler `f` of a job from the top. A script function (the usual one) starts on a pooled machine at once, with one read of
    /// the function; a built-in or a bound function goes through `call_held`.
    fn job_call(&mut self, f: Value, this: Value, args: &[Value]) -> R<Value> {
        if self.native_id(f).is_some() {
            return self.call_held(f, this, args);
        }
        match self.start_pooled(f, this, args) {
            // A class constructor is callable but no function to run: say why (the message of `call_script`).
            Err(e) if e != STOP && self.function(f).is_none() => self.call_held(f, this, args),
            r => r,
        }
    }

    /// Run one job. Only `STOP` (the budget) is an `Err`: a throw of a handler is the result of its derived promise.
    fn run_job(&mut self, r: Ctx, on_error: &mut dyn FnMut(&mut Realm, Value)) -> R<()> {
        let kind = r.get(KIND).as_int().expect("a job kind");
        match kind {
            K_FULFIL | K_REJECT => {
                let (h, v, d) = (r.get(A), r.get(B), r.get(D));
                // The handler of an `await` is the activation of the async function (it has no derived promise): resume it (`reenter.rs`).
                if d.is_undefined() && Realm::activation(h).is_some() {
                    return self.async_resume(h, v, kind == K_REJECT);
                }
                let res = if h.is_undefined() { if kind == K_FULFIL { Ok(v) } else { Err(v) } } else { self.job_call(h, Value::UNDEFINED, &[v]) };
                match res {
                    Ok(v) => self.settle_derived(d, v, false, on_error),
                    Err(STOP) => Err(STOP),
                    Err(e) => self.settle_derived(d, e, true, on_error),
                }
            }
            K_THENABLE => self.thenable_job(r),
            K_CALLBACK => match self.job_call(r.get(A), Value::UNDEFINED, &[]) {
                Ok(_) => Ok(()),
                Err(STOP) => Err(STOP),
                Err(e) => {
                    on_error(self, e);
                    Ok(())
                }
            },
            _ => unreachable!("a job kind"),
        }
    }

    /// Give the outcome of a handler to the derived value: resolve or reject the promise, or call the function of the capability.
    fn settle_derived(&mut self, d: Value, v: Value, reject: bool, on_error: &mut dyn FnMut(&mut Realm, Value)) -> R<()> {
        if let Some(p) = promise_of(d) {
            if reject {
                self.reject_promise(p, v);
                return Ok(());
            }
            return self.resolve_promise(p, v);
        }
        let Some(c) = Realm::ctx(d) else { return Ok(()) };
        match self.job_call(c.get(if reject { 2 } else { 1 }), Value::UNDEFINED, &[v]) {
            Ok(_) => Ok(()),
            Err(STOP) => Err(STOP),
            Err(e) => {
                on_error(self, e);
                Ok(())
            }
        }
    }

    /// `NewPromiseResolveThenableJob`: call `then` of the thenable with the resolving functions of the promise.
    fn thenable_job(&mut self, r: Ctx) -> R<()> {
        let (x, then, d) = (r.get(A), r.get(B), r.get(D));
        let p = promise_of(d).expect("the promise of a thenable job");
        // A native promise with the built-in `then`, a `constructor` that is `%Promise%` by plain data and the built-in
        // `Promise[Symbol.species]`: the reaction does the work of `then` (`SpeciesConstructor` reads nothing that a script can see).
        if then == self.pr.then_fn
            && let Some(xp) = promise_of(x)
            && matches!(self.lookup_g(xp, self.k_constructor), Some(Slot::Data(c)) if c == self.pr.ctor)
            && self.plain_species(Realm::obj(self.pr.ctor).expect("Promise"))
        {
            // The queue is full: the promise is rejected, as the call of `then` would have thrown.
            if let Err(e) = self.perform_then(xp, Value::UNDEFINED, Value::UNDEFINED, d) {
                self.reject_promise(p, e);
            }
            return Ok(());
        }
        let (res, rej) = self.resolving_functions(p);
        // The collector may run in the call: keep `reject` in the job, which is rooted.
        r.set(B, rej.value());
        match self.job_call(then, x, &[res.value(), rej.value()]) {
            Ok(_) => Ok(()),
            Err(STOP) => Err(STOP),
            Err(e) => {
                let rej = Realm::obj(r.get(B)).expect("the reject function");
                self.resolving_call(rej, e, true)
            }
        }
    }
}

#[cfg(test)]
#[path = "promise_tests.rs"]
mod tests;
