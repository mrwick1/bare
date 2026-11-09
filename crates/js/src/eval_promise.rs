// Promise and the microtask queue of the evaluator. Included by `eval.rs` (`include!`), so it shares that
// module's imports and private items.
//
// Supported: `new Promise(executor)`, `Promise.resolve`, `Promise.reject`, `Promise.withResolvers`, `Promise.prototype.then`, `.catch`,
// `.finally`. Resolver-once state, thenable assimilation (a job, as in the spec) and self-resolution rejection are exact.
// `Promise.all`, `allSettled`, `race`, `any` and `AggregateError` are in `eval_promise_combinators.rs`.
// Gates (explicit `Unsupported`, never silent): any static call whose `this` is not the intrinsic `Promise`.
// `then`, `catch` and `finally` read `constructor` and its `@@species`:
// `undefined`, `null` and `Promise` give the default and another callable is a gate. `Promise[@@species]` and
// `Promise.prototype[@@toStringTag]` exist. `async`/`await` lives in `eval_async.rs`.
//
// The queue is separate from timers and the network. Nothing runs by itself: the host calls
// [`Interp::drain_microtasks`] after a script or a task. Jobs run FIFO.
//
// GC: a promise is an ordinary heap object. Its state, its reactions and the resolver data live here, in
// `promise.map` and in `Callee::Promise`. `Interp::collect` traces them and drops the entries of unreachable
// promises. The queue, the intrinsic constructor and the cached keys are roots.

/// Limits for promises and the job queue. Every one is checked before the thing grows, and ends the call
/// with a distinct [`Error::Limit`]. A limit is never a JavaScript value.
#[derive(Clone, Copy, Debug)]
pub struct PromiseLimits {
    /// Jobs waiting in the microtask queue.
    pub max_queue: usize,
    /// Promise objects the evaluator tracks (until `collect` drops the unreachable ones).
    pub max_promises: usize,
    /// Reactions stored on pending promises (one per `then` call on a pending promise).
    pub max_reactions: usize,
    /// Jobs one [`Interp::drain_microtasks`] call may run.
    pub max_jobs_per_drain: usize,
}

impl Default for PromiseLimits {
    fn default() -> Self {
        PromiseLimits { max_queue: 1 << 18, max_promises: 1 << 20, max_reactions: 1 << 20, max_jobs_per_drain: 1 << 20 }
    }
}

/// State of a promise, as the host sees it.
#[derive(Clone, Copy, Debug)]
pub enum PromiseStatus {
    Pending,
    Fulfilled(Value),
    Rejected(Value),
}

/// A native function of the Promise family. `Settle`, `FinallyStep` and `Thunk` carry captured values, so a
/// function object that holds one must be traced (see `pfn_trace`).
#[derive(Clone, Copy)]
enum PFn {
    Ctor,
    StaticResolve,
    StaticReject,
    StaticWithResolvers,
    Then,
    Catch,
    Finally,
    /// The resolve or reject function of one resolving pair. `pair` is never reused (a counter), so the
    /// "already resolved" flag cannot attach to a recycled slot.
    Settle { promise: ObjId, pair: u64, reject: bool },
    /// `thenFinally` and `catchFinally`.
    FinallyStep { on_finally: Value, reject: bool },
    /// The same with a species constructor other than `Promise`: `holder` keeps `onFinally` (index 0) and the constructor (index 1).
    FinallyStepC { holder: ObjId, reject: bool },
    /// The executor of `NewPromiseCapability`: it keeps its two arguments in `holder` (index 0 and 1).
    CapExec { holder: ObjId },
    Thunk { value: Value, throw: bool },
    /// `Promise.all`, `allSettled`, `race` and `any` (see `eval_promise_combinators.rs`).
    Comb(Comb),
    /// An element function of one combinator call: the record key (never reused) and the item index.
    AggElem { agg: u64, index: u32, reject: bool },
    AggErrCtor,
    /// A reaction of AsyncGeneratorAwaitReturn for the async generator `g` (`eval_asyncgen.rs`).
    AgRet { g: ObjId, reject: bool },
    /// The `unwrap` reaction of the async-from-sync iterator: the iterator result of the awaited value.
    AfsUnwrap { done: bool },
    /// The `closeIterator` reaction of the async-from-sync iterator: close the sync iterator and throw again.
    AfsClose { iter: Value, next: Value },
}

impl PFn {
    fn trace(&self, work: &mut Vec<Value>) {
        match *self {
            PFn::Settle { promise, .. } => work.push(Value::Object(promise)),
            PFn::FinallyStep { on_finally, .. } => work.push(on_finally),
            PFn::FinallyStepC { holder, .. } => work.push(Value::Object(holder)),
            PFn::CapExec { holder } => work.push(Value::Object(holder)),
            PFn::Thunk { value, .. } => work.push(value),
            PFn::AgRet { g, .. } => work.push(Value::Object(g)),
            PFn::AfsClose { iter, next } => work.extend([iter, next]),
            _ => {}
        }
    }
}

enum PState {
    Pending,
    Fulfilled(Value),
    Rejected(Value),
}

/// One `then` call on a pending promise. A handler that is not callable is `Undefined` (pass through).
struct Reaction {
    on_ok: Value,
    on_err: Value,
    derived: ObjId,
    /// An `await` of an async function activation (`on_ok`, `on_err` and `derived` are then unused).
    act: Option<ActRef>,
}

struct PromiseData {
    seq: u64,
    state: PState,
    reactions: Vec<Reaction>,
    handled: bool,
}

enum Job {
    React { handler: Value, arg: Value, derived: ObjId, rejected: bool },
    Thenable { promise: ObjId, thenable: Value, then: Value },
    /// A host callback from `queueMicrotask`: called with no arguments. An ordinary throw goes to
    /// [`Host::report_exception`] and the queue goes on.
    Callback { cb: Value },
    /// Resume the suspended async function activation `act` (see `eval_async.rs`).
    Resume { act: ActRef, arg: Value, rejected: bool },
}

#[derive(Default)]
struct PromiseState {
    limits: PromiseLimits,
    ctor: Option<ObjId>,
    proto: Option<ObjId>,
    k_then: Option<PropertyKey>,
    map: FxMap<ObjId, PromiseData>,
    queue: std::collections::VecDeque<Job>,
    reactions: usize,
    next_seq: u64,
    next_pair: u64,
    done_pairs: FxSet<u64>,
    /// The resolve and reject functions of a promise that a constructor other than `Promise` made (`new_capability`).
    caps: FxMap<ObjId, (Value, Value)>,
    failed_job: Option<Error>,
    agg: AggState,
}

/// A thrown value becomes `Ok(value)`. Anything else (a limit, Unsupported, a heap fault) stays an error.
fn thrown(e: Error) -> Res<Value> {
    match e {
        Error::Throw(v) => Ok(v),
        e => Err(e),
    }
}

impl<'p> Interp<'p> {
    // ------------------------------------------------------------ host API

    pub fn set_promise_limits(&mut self, l: PromiseLimits) {
        self.promise.limits = l;
    }

    /// Jobs waiting in the microtask queue.
    pub fn pending_jobs(&self) -> usize {
        self.promise.queue.len()
    }

    /// Queue `cb` as a microtask (`queueMicrotask`). It runs FIFO with the promise jobs, in the next checkpoint.
    /// A callback that is not callable is a `TypeError`. A full queue is `Limit(PromiseQueue)`.
    /// If `cb` throws, the value goes to [`Host::report_exception`] and later jobs still run. A limit, `Unsupported`
    /// or a heap fault inside it ends the drain like any job error (later drains repeat it).
    pub fn queue_microtask(&mut self, cb: Value) -> Result<(), Error> {
        if !self.is_callable(cb) {
            return Err(self.type_err("queueMicrotask callback is not a function"));
        }
        self.enqueue(Job::Callback { cb })
    }

    /// The state of a promise value, or `None` if `v` is not a promise.
    pub fn promise_status(&self, v: Value) -> Option<PromiseStatus> {
        let Value::Object(o) = v else { return None };
        Some(match self.promise.map.get(&o)?.state {
            PState::Pending => PromiseStatus::Pending,
            PState::Fulfilled(x) => PromiseStatus::Fulfilled(x),
            PState::Rejected(x) => PromiseStatus::Rejected(x),
        })
    }

    /// Rejected promises that no `then` call ever handled, oldest first. Root them (`extra` of `collect`) if you keep them.
    pub fn unhandled_rejections(&self) -> Vec<Value> {
        let mut v: Vec<(u64, ObjId)> = self.promise.map.iter().filter(|(_, d)| matches!(d.state, PState::Rejected(_)) && !d.handled).map(|(&o, d)| (d.seq, o)).collect();
        v.sort_unstable_by_key(|x| x.0);
        v.into_iter().map(|x| Value::Object(x.1)).collect()
    }

    /// The microtask checkpoint: run queued jobs, oldest first, including jobs that jobs add, until the queue is
    /// empty. Returns the number of jobs run. At most `max_jobs_per_drain` jobs run: if more are queued the
    /// call ends with `Limit(DrainJobs)` and the rest stay queued. A JavaScript exception inside a job rejects
    /// that job's promise. Any `Limit` or other error ends the drain; the job that was running is dropped, so
    /// treat a job error as fatal for this interpreter. Later drains repeat that error instead of
    /// reporting an empty successful checkpoint. A DrainJobs cap before starting a job is resumable.
    /// Refused while a call is active.
    pub fn drain_microtasks(&mut self) -> Result<usize, Error> {
        if self.depth != 0 {
            return Err(unsupported("drain_microtasks during a call", Span::default()));
        }
        self.begin();
        self.drain_jobs()
    }

    /// The loop of `drain_microtasks`, without the depth check and without a new step budget.
    fn drain_jobs(&mut self) -> Result<usize, Error> {
        if let Some(error) = self.promise.failed_job { return Err(error); }
        let mut n = 0usize;
        while let Some(job) = self.promise.queue.pop_front() {
            if n >= self.promise.limits.max_jobs_per_drain {
                self.promise.queue.push_front(job);
                return Err(Error::Limit(LimitKind::DrainJobs));
            }
            n += 1;
            if let Err(error) = self.run_job(job) {
                self.promise.failed_job = Some(error);
                return Err(error);
            }
        }
        Ok(n)
    }

    /// Forget the sticky error of a failed job (see [`Interp::drain_microtasks`]). The host calls it after it chose to abort only the
    /// task that failed: the job that ran is lost (its promise stays pending), the jobs behind it stay queued.
    pub fn clear_failed_job(&mut self) {
        self.promise.failed_job = None;
    }

    // ------------------------------------------------------------ setup

    fn install_promise(&mut self) -> Res<()> {
        let g = self.global;
        let proto = h!(self, self.heap.new_object(Some(self.object_proto)));
        let ctor = self.pfn(PFn::Ctor, "Promise", 1)?;
        self.def(ctor, self.k_prototype, Value::Object(proto), false, false, false)?;
        self.def(proto, self.k_constructor, Value::Object(ctor), true, false, true)?;
        for (name, f) in [("resolve", PFn::StaticResolve), ("reject", PFn::StaticReject)] {
            self.pmethod(ctor, name, f, 1)?;
        }
        self.pmethod(ctor, "withResolvers", PFn::StaticWithResolvers, 0)?;
        self.pmethod(proto, "then", PFn::Then, 2)?;
        self.pmethod(proto, "catch", PFn::Catch, 1)?;
        self.pmethod(proto, "finally", PFn::Finally, 1)?;
        let k = h!(self, self.heap.key_from_str("Promise"));
        self.def(g, k, Value::Object(ctor), true, false, true)?;
        self.sc_getter(ctor, self.wk_key(WK_SPECIES), "get [Symbol.species]", ScFn::Species)?;
        self.sc_tag(proto, "Promise")?;
        self.promise.k_then = Some(h!(self, self.heap.key_from_str("then")));
        self.promise.ctor = Some(ctor);
        self.promise.proto = Some(proto);
        self.install_combinators()
    }

    fn pfn(&mut self, f: PFn, name: &str, len: u32) -> Res<ObjId> {
        let o = self.new_func_object(Callee::Promise(f))?;
        let s = h!(self, self.heap.intern_str(name));
        self.def(o, self.k_length, Value::Number(len as f64), false, false, true)?;
        self.def(o, self.k_name, Value::String(s), false, false, true)?;
        Ok(o)
    }

    fn pmethod(&mut self, on: ObjId, name: &str, f: PFn, len: u32) -> Res<()> {
        let o = self.pfn(f, name, len)?;
        let k = h!(self, self.heap.key_from_str(name));
        self.def(on, k, Value::Object(o), true, false, true)
    }

    // ------------------------------------------------------------ tracing (GC)

    /// Values that stay alive for the whole interpreter: the intrinsics, the cached key and the queued jobs.
    fn promise_roots(&self, out: &mut Vec<Value>) {
        out.extend(self.promise.ctor.map(Value::Object));
        out.extend(self.promise.proto.map(Value::Object));
        if let Some(PropertyKey::String(s)) = self.promise.k_then {
            out.push(Value::String(s));
        }
        self.agg_roots(out);
        for j in &self.promise.queue {
            match *j {
                Job::React { handler, arg, derived, .. } => out.extend([handler, arg, Value::Object(derived)]),
                Job::Thenable { promise, thenable, then } => out.extend([Value::Object(promise), thenable, then]),
                Job::Callback { cb } => out.push(cb),
                Job::Resume { arg, .. } => out.push(arg),
            }
        }
    }

    /// The values a promise object holds (state value, handlers, derived promises).
    fn promise_trace(&self, o: ObjId, work: &mut Vec<Value>) {
        let Some(d) = self.promise.map.get(&o) else { return };
        if let PState::Fulfilled(v) | PState::Rejected(v) = d.state {
            work.push(v);
        }
        for r in &d.reactions {
            work.extend([r.on_ok, r.on_err, Value::Object(r.derived)]);
        }
    }

    /// When the mark work is empty: queue the resolve and reject functions of every reached capability promise that are not reached yet.
    /// `true` if it queued any, so the mark must go on. Done here, not for each object, so a realm without capabilities pays nothing.
    fn caps_trace(&self, seen: &Seen, work: &mut Vec<Value>) -> bool {
        let n = work.len();
        for (&o, &(res, rej)) in &self.promise.caps {
            if seen.contains(&o) {
                work.extend([res, rej].into_iter().filter(|v| matches!(v, Value::Object(x) if !seen.contains(x))));
            }
        }
        work.len() > n
    }

    /// Everything `Heap::collect` must see, for every tracked promise (call after `promise_prune`).
    fn promise_all_values(&self, out: &mut Vec<Value>) {
        for &o in self.promise.map.keys() {
            out.push(Value::Object(o));
            self.promise_trace(o, out);
        }
        self.agg_all_values(out);
        for (&o, &(res, rej)) in &self.promise.caps {
            out.extend([Value::Object(o), res, rej]);
        }
    }

    /// Drop the metadata of promises that `collect` did not reach, and of finished resolving pairs.
    fn promise_prune(&mut self, seen: &Seen) {
        self.promise.map.retain(|o, _| seen.contains(o));
        self.promise.caps.retain(|o, _| seen.contains(o));
        self.promise.reactions = self.promise.map.values().map(|d| d.reactions.len()).sum();
        let agg_pairs = self.agg_prune();
        let mut live: FxSet<u64> = self.funcs.values().filter_map(|c| if let Callee::Promise(PFn::Settle { pair, .. }) = c { Some(*pair) } else { None }).collect();
        live.extend(agg_pairs);
        self.promise.done_pairs.retain(|p| live.contains(p));
    }

    // ------------------------------------------------------------ the natives

    fn call_promise(&mut self, f: PFn, this: Value, args: &[Value], is_new: bool) -> Res<Value> {
        let a0 = arg(args, 0);
        match f {
            PFn::Ctor => {
                if !is_new {
                    return Err(self.type_err("Promise constructor cannot be invoked without 'new'"));
                }
                let nt = self.cls.native_nt.take();
                if !self.is_callable(a0) {
                    let t = self.display(a0);
                    return Err(self.type_err(&format!("Promise resolver {t} is not a function")));
                }
                let p = self.new_promise()?;
                self.nt_patch(nt, self.promise.proto.expect("installed"), Value::Object(p))?;
                let (res, rej, pair) = self.resolving_functions(p)?;
                if let Err(e) = self.call_fn(a0, Value::Undefined, &[res, rej]) {
                    let v = thrown(e)?;
                    if !self.pair_done(pair) {
                        self.reject_promise(p, v)?;
                    }
                }
                Ok(Value::Object(p))
            }
            PFn::StaticResolve => {
                self.object_this(this)?;
                self.promise_resolve_c(this, a0)
            }
            PFn::StaticReject => {
                self.object_this(this)?;
                if matches!(this, Value::Object(c) if Some(c) == self.promise.ctor) {
                    let p = self.new_promise()?;
                    self.reject_promise(p, a0)?;
                    return Ok(Value::Object(p));
                }
                let (p, _, rej, _) = self.new_capability(this)?;
                self.call_fn(rej, Value::Undefined, &[a0])?;
                Ok(Value::Object(p))
            }
            PFn::StaticWithResolvers => {
                if !matches!(this, Value::Object(_)) {
                    return Err(self.type_err("Promise.withResolvers called on non-object"));
                }
                let (p, res, rej, _) = self.new_capability(this)?;
                let r = h!(self, self.heap.new_object(Some(self.object_proto)));
                for (name, v) in [("promise", Value::Object(p)), ("resolve", res), ("reject", rej)] {
                    let k = h!(self, self.heap.key_from_str(name));
                    self.def(r, k, v, true, true, true)?;
                }
                Ok(Value::Object(r))
            }
            PFn::Then => {
                let p = match this {
                    Value::Object(o) if self.promise.map.contains_key(&o) => o,
                    _ => {
                        let t = self.display(this);
                        return Err(self.type_err(&format!("Method Promise.prototype.then called on incompatible receiver {t}")));
                    }
                };
                let derived = match self.species_ctor(this)? {
                    None => self.new_promise()?,
                    Some(c) => self.new_capability(c)?.0,
                };
                self.perform_then(p, a0, arg(args, 1), derived)?;
                Ok(Value::Object(derived))
            }
            PFn::Catch => self.invoke_then(this, &[Value::Undefined, a0]),
            PFn::Finally => {
                if !matches!(this, Value::Object(_)) {
                    return Err(self.type_err("Promise.prototype.finally called on non-object"));
                }
                let c = self.species_ctor(this)?;
                let (on_ok, on_err) = if self.is_callable(a0) {
                    let (ok, err) = match c {
                        None => (PFn::FinallyStep { on_finally: a0, reject: false }, PFn::FinallyStep { on_finally: a0, reject: true }),
                        Some(c) => {
                            let holder = h!(self, self.heap.new_object(None));
                            self.def(holder, PropertyKey::Index(0), a0, true, true, true)?;
                            self.def(holder, PropertyKey::Index(1), c, true, true, true)?;
                            (PFn::FinallyStepC { holder, reject: false }, PFn::FinallyStepC { holder, reject: true })
                        }
                    };
                    (Value::Object(self.pfn(ok, "", 1)?), Value::Object(self.pfn(err, "", 1)?))
                } else {
                    (a0, a0)
                };
                self.invoke_then(this, &[on_ok, on_err])
            }
            PFn::Settle { promise, pair, reject } => {
                if !self.pair_done(pair) {
                    if reject { self.reject_promise(promise, a0)? } else { self.resolve_promise(promise, a0)? }
                }
                Ok(Value::Undefined)
            }
            PFn::CapExec { holder } => {
                let h = Value::Object(holder);
                for i in 0..2 {
                    if !matches!(self.get_prop(h, PropertyKey::Index(i))?, Value::Undefined) {
                        return Err(self.type_err("Promise executor has already been invoked with non-undefined arguments"));
                    }
                }
                self.def(holder, PropertyKey::Index(0), a0, true, true, true)?;
                self.def(holder, PropertyKey::Index(1), arg(args, 1), true, true, true)?;
                Ok(Value::Undefined)
            }
            PFn::FinallyStep { on_finally, reject } => self.finally_step(on_finally, Value::Object(self.promise.ctor.expect("installed")), reject, a0),
            PFn::FinallyStepC { holder, reject } => {
                let h = Value::Object(holder);
                let (on_finally, c) = (self.get_prop(h, PropertyKey::Index(0))?, self.get_prop(h, PropertyKey::Index(1))?);
                self.finally_step(on_finally, c, reject, a0)
            }
            PFn::Thunk { value, throw } => if throw { Err(Error::Throw(value)) } else { Ok(value) },
            PFn::Comb(k) => self.promise_combinator(k, this, args),
            PFn::AggElem { agg, index, reject } => self.agg_elem(agg, index, reject, a0),
            PFn::AggErrCtor => self.agg_error_ctor(args),
            PFn::AgRet { g, reject } => self.ag_ret_settled(g, reject, a0),
            PFn::AfsUnwrap { done } => self.sc_iter_result(a0, done),
            PFn::AfsClose { iter, next } => Err(self.iter_close_throw(&mut it_direct(iter, next), Error::Throw(a0))),
        }
    }

    /// `promise.then(a, b)` as `Invoke` does: `then` is read from the value, so a thenable works too.
    fn invoke_then(&mut self, this: Value, args: &[Value]) -> Res<Value> {
        let k = self.promise.k_then.unwrap_or(self.k_name);
        let then = self.get_prop(this, k)?;
        if !self.is_callable(then) {
            let t = self.display(then);
            return Err(self.type_err(&format!("{t} is not a function")));
        }
        self.call_fn(then, this, args)
    }

    /// `thenFinally` / `catchFinally`: run `onFinally`, then give back the original value or throw the original reason.
    #[inline]
    fn finally_step(&mut self, on_finally: Value, c: Value, reject: bool, v: Value) -> Res<Value> {
        let result = self.call_fn(on_finally, Value::Undefined, &[])?;
        let p = self.promise_resolve_c(c, result)?;
        let thunk = self.pfn(PFn::Thunk { value: v, throw: reject }, "", 0)?;
        self.invoke_then(p, &[Value::Object(thunk)])
    }

    fn object_this(&mut self, this: Value) -> Res<()> {
        if matches!(this, Value::Object(_)) { Ok(()) } else { Err(self.type_err("PromiseResolve called on non-object")) }
    }

    /// `SpeciesConstructor(this, %Promise%)`: `None` for the default (the intrinsic `Promise`).
    /// `constructor` must be undefined or an object. Its `@@species` must be undefined, null or a constructor.
    fn species_ctor(&mut self, this: Value) -> Res<Option<Value>> {
        self.species_ctor_of(this, self.promise.ctor)
    }

    /// SpeciesConstructor with the intrinsic constructor `default`: `None` when the result is `default`.
    fn species_ctor_of(&mut self, this: Value, default: Option<ObjId>) -> Res<Option<Value>> {
        let c = self.get_prop(this, self.k_constructor)?;
        match c {
            Value::Undefined => Ok(None),
            Value::Object(_) => match self.get_prop(c, self.wk_key(WK_SPECIES))? {
                Value::Undefined | Value::Null => Ok(None),
                Value::Object(s) if Some(s) == default => Ok(None),
                s @ Value::Object(so) if self.is_constructor(so) => Ok(Some(s)),
                _ => Err(self.type_err("object.constructor[Symbol.species] is not a constructor")),
            },
            _ => Err(self.type_err("The .constructor property is not an object")),
        }
    }

    /// `NewPromiseCapability(c)`: the promise, its resolve and reject functions, and the id of the resolving pair.
    /// Any other constructor than `Promise` runs with an executor that keeps the two functions. The pair id is then
    /// `u64::MAX` (the functions keep the "already resolved" flag) and `caps` maps the promise to the functions.
    fn new_capability(&mut self, c: Value) -> Res<(ObjId, Value, Value, u64)> {
        let Value::Object(co) = c else { return Err(self.type_err("PromiseResolve called on non-object")) };
        if Some(co) == self.promise.ctor {
            let p = self.new_promise()?;
            let (res, rej, pair) = self.resolving_functions(p)?;
            return Ok((p, res, rej, pair));
        }
        if !self.is_constructor(co) {
            let t = self.display(c);
            return Err(self.type_err(&format!("{t} is not a constructor")));
        }
        let holder = h!(self, self.heap.new_object(None));
        let exec = self.pfn(PFn::CapExec { holder }, "", 2)?;
        let Value::Object(p) = self.construct(c, &[Value::Object(exec)], ExprId::from_index(0))? else { unreachable!("construct gives an object") };
        let (res, rej) = (self.get_prop(Value::Object(holder), PropertyKey::Index(0))?, self.get_prop(Value::Object(holder), PropertyKey::Index(1))?);
        if !self.is_callable(res) || !self.is_callable(rej) {
            return Err(self.type_err("Promise resolve or reject function is not callable"));
        }
        self.promise.caps.insert(p, (res, rej));
        Ok((p, res, rej, u64::MAX))
    }

    /// `PromiseResolve(c, x)`.
    fn promise_resolve_c(&mut self, c: Value, x: Value) -> Res<Value> {
        if let Value::Object(o) = x
            && self.promise.map.contains_key(&o)
            && matches!((self.get_prop(x, self.k_constructor)?, c), (Value::Object(a), Value::Object(b)) if a == b)
        {
            return Ok(x);
        }
        if matches!(c, Value::Object(co) if Some(co) == self.promise.ctor) {
            let p = self.new_promise()?;
            self.resolve_promise(p, x)?;
            return Ok(Value::Object(p));
        }
        let (p, res, _, _) = self.new_capability(c)?;
        self.call_fn(res, Value::Undefined, &[x])?;
        Ok(Value::Object(p))
    }

    /// `PromiseResolve(%Promise%, x)`.
    fn promise_resolve(&mut self, x: Value) -> Res<Value> {
        self.promise_resolve_c(Value::Object(self.promise.ctor.expect("installed")), x)
    }

    /// Settle the promise `derived` of a reaction: through the functions of its capability if it has them.
    #[inline(always)]
    fn react_settle(&mut self, derived: ObjId, v: Value, reject: bool) -> Res<()> {
        if !self.promise.caps.is_empty()
            && let Some(&(res, rej)) = self.promise.caps.get(&derived)
        {
            return self.react_cap(res, rej, v, reject);
        }
        if reject { self.reject_promise(derived, v) } else { self.resolve_promise(derived, v) }
    }

    /// A throw of a user resolve function has no promise to reject: it is dropped, as a failed job is.
    #[cold]
    fn react_cap(&mut self, res: Value, rej: Value, v: Value, reject: bool) -> Res<()> {
        self.call_fn(if reject { rej } else { res }, Value::Undefined, &[v]).map(drop).or_else(|e| thrown(e).map(drop))
    }

    // ------------------------------------------------------------ promise operations

    fn new_promise(&mut self) -> Res<ObjId> {
        if self.promise.map.len() >= self.promise.limits.max_promises {
            return Err(Error::Limit(LimitKind::Promises));
        }
        let seq = self.promise.next_seq;
        self.promise.next_seq = seq.checked_add(1).ok_or(Error::Limit(LimitKind::Promises))?;
        let o = h!(self, self.heap.new_object(self.promise.proto));
        self.promise.map.insert(o, PromiseData { seq, state: PState::Pending, reactions: Vec::new(), handled: false });
        Ok(o)
    }

    /// Fresh resolve and reject functions that share one "already resolved" flag.
    fn resolving_functions(&mut self, p: ObjId) -> Res<(Value, Value, u64)> {
        let pair = self.promise.next_pair;
        self.promise.next_pair = pair.checked_add(1).ok_or(Error::Limit(LimitKind::Functions))?;
        let res = self.pfn(PFn::Settle { promise: p, pair, reject: false }, "", 1)?;
        let rej = self.pfn(PFn::Settle { promise: p, pair, reject: true }, "", 1)?;
        Ok((Value::Object(res), Value::Object(rej), pair))
    }

    /// True if the pair was already used. Otherwise marks it used and gives false.
    fn pair_done(&mut self, pair: u64) -> bool {
        !self.promise.done_pairs.insert(pair)
    }

    /// The promise resolve function body (without the once flag): self-resolution, thenable assimilation or fulfillment.
    fn resolve_promise(&mut self, p: ObjId, x: Value) -> Res<()> {
        let Value::Object(xo) = x else { return self.settle(p, false, x) };
        if xo == p {
            let e = self.type_err("Chaining cycle detected for promise #<Promise>");
            let v = thrown(e)?;
            return self.settle(p, true, v);
        }
        let k = self.promise.k_then.unwrap_or(self.k_name);
        let then = match self.get_prop(x, k) {
            Ok(t) => t,
            Err(e) => {
                let v = thrown(e)?;
                return self.settle(p, true, v);
            }
        };
        if !self.is_callable(then) {
            return self.settle(p, false, x);
        }
        self.enqueue(Job::Thenable { promise: p, thenable: x, then })
    }

    fn reject_promise(&mut self, p: ObjId, v: Value) -> Res<()> {
        self.settle(p, true, v)
    }

    fn enqueue(&mut self, j: Job) -> Res<()> {
        if self.promise.queue.len() >= self.promise.limits.max_queue {
            return Err(Error::Limit(LimitKind::PromiseQueue));
        }
        self.promise.queue.push_back(j);
        Ok(())
    }

    /// Fulfill or reject a pending promise and queue its reactions. The queue room is checked before the state changes.
    fn settle(&mut self, p: ObjId, rejected: bool, v: Value) -> Res<()> {
        let Some(d) = self.promise.map.get(&p) else { return Ok(()) };
        if !matches!(d.state, PState::Pending) {
            return Ok(());
        }
        let n = d.reactions.len();
        if self.promise.queue.len().checked_add(n).is_none_or(|total| total > self.promise.limits.max_queue) {
            return Err(Error::Limit(LimitKind::PromiseQueue));
        }
        self.steps = self.steps.checked_add(n as u64).ok_or(Error::Limit(LimitKind::Steps))?;
        if self.steps > self.step_limit { return Err(Error::Limit(LimitKind::Steps)); }
        let d = self.promise.map.get_mut(&p).expect("checked");
        d.state = if rejected { PState::Rejected(v) } else { PState::Fulfilled(v) };
        let reactions = std::mem::take(&mut d.reactions);
        self.promise.reactions -= n;
        for r in reactions {
            if let Some(act) = r.act {
                self.promise.queue.push_back(Job::Resume { act, arg: v, rejected });
                continue;
            }
            let handler = if rejected { r.on_err } else { r.on_ok };
            self.promise.queue.push_back(Job::React { handler, arg: v, derived: r.derived, rejected });
        }
        Ok(())
    }

    /// `PerformPromiseThen`: store a reaction on a pending promise, or queue a job at once.
    fn perform_then(&mut self, p: ObjId, on_ok: Value, on_err: Value, derived: ObjId) -> Res<()> {
        let on_ok = if self.is_callable(on_ok) { on_ok } else { Value::Undefined };
        let on_err = if self.is_callable(on_err) { on_err } else { Value::Undefined };
        let d = self.promise.map.get(&p).expect("promise");
        match d.state {
            PState::Pending => {
                if self.promise.reactions >= self.promise.limits.max_reactions {
                    return Err(Error::Limit(LimitKind::PromiseReactions));
                }
                self.promise.reactions += 1;
                let rs = &mut self.promise.map.get_mut(&p).expect("promise").reactions;
                // Most promises get one reaction: keep room for one, not the default four.
                if rs.capacity() == 0 {
                    rs.reserve_exact(1);
                }
                rs.push(Reaction { on_ok, on_err, derived, act: None });
            }
            PState::Fulfilled(v) => self.enqueue(Job::React { handler: on_ok, arg: v, derived, rejected: false })?,
            PState::Rejected(v) => self.enqueue(Job::React { handler: on_err, arg: v, derived, rejected: true })?,
        }
        self.promise.map.get_mut(&p).expect("promise").handled = true;
        Ok(())
    }

    fn run_job(&mut self, job: Job) -> Res<()> {
        self.tick()?;
        self.in_job(|it| it.run_job_in(job))
    }

    fn run_job_in(&mut self, job: Job) -> Res<()> {
        match job {
            Job::React { handler, arg, derived, rejected } => {
                if matches!(handler, Value::Undefined) {
                    return self.react_settle(derived, arg, rejected);
                }
                match self.call_fn(handler, Value::Undefined, &[arg]) {
                    Ok(v) => self.react_settle(derived, v, false),
                    Err(e) => {
                        let v = thrown(e)?;
                        self.react_settle(derived, v, true)
                    }
                }
            }
            Job::Callback { cb } => match self.call_fn(cb, Value::Undefined, &[]) {
                Ok(_) => Ok(()),
                Err(e) => {
                    let v = thrown(e)?;
                    if let Some(h) = self.host.clone() {
                        h.report_exception(self, v);
                    }
                    Ok(())
                }
            },
            Job::Resume { act, arg, rejected } => self.async_resume(act, arg, rejected),
            Job::Thenable { promise, thenable, then } => {
                let (res, rej, pair) = self.resolving_functions(promise)?;
                if let Err(e) = self.call_fn(then, thenable, &[res, rej]) {
                    let v = thrown(e)?;
                    if !self.pair_done(pair) {
                        self.reject_promise(promise, v)?;
                    }
                }
                Ok(())
            }
        }
    }
}
