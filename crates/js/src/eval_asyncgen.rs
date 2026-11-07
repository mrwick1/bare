// Async generators (`async function*`, `async *m(){}`), async iteration (`for await`, async `yield*`) and the
// async-from-sync iterator. Included by `eval.rs` (`include!`), so it shares that module's imports and private items.
//
// An async generator runs on the async machine (`eval_async.rs`), as a generator does, with `Run::agen` set. An
// `await` suspends the run in the activation table, as in an async function. A `yield` first awaits its value
// (`Fr::AYield`) and then ends a machine step with `Out::Yield`.
//
// The object state follows the spec (AsyncGeneratorEnqueue, Resume, Yield, CompleteStep, AwaitReturn, DrainQueue):
// - `aw.agens` maps a generator object to an `AGen`: the state and the queue of requests. A request is the resume
//   kind and its promise. `next`, `return` and `throw` always give a promise. A request settles in queue order.
// - A `yield` settles the first request. If more requests wait, the run goes on at once with the next one; else it
//   waits in `AgSt::Yield`. A `return(v)` resume awaits `v` first (`Fr::ARet`). A `return x` statement awaits `x`:
//   the preflight marks every `return` with a value in an async generator, so it runs on the machine.
// - The end of the body settles the first request, then drains the queue: `next` gets `{undefined, true}`, `throw`
//   rejects, and `return(v)` awaits `v` through two native reactions (`PFn::AgRet`).
//
// Async iteration (`for await` and async `yield*`) keeps the iteration in a `Slot::It` with `ItKind::Generic`. `It::i`
// is 1 when the iterator is a sync iterator: its steps then run the async-from-sync rules (`afs_call`), with a real
// promise, a value wrapper and native reactions (`PFn::AfsUnwrap`, `PFn::AfsClose`), so the order of jobs is the
// order of the spec. The async-from-sync iterator object itself is never visible, so none is made.
//
// GC contract: a suspended run of an async generator is reachable through its generator object (`gen_trace`), or
// through the promise it awaits, as an async function is. The queue keeps its values and promises.

/// The state of an async generator object.
enum AgSt {
    /// Parameters are bound; the body has not started.
    Start(Box<Run>),
    /// Suspended at a `yield`.
    Yield(Box<Run>),
    /// Running, or suspended at an `await` (the run is then in the activation table).
    Running,
    /// The body is done; a `return` request waits for its value (the spec's draining-queue state).
    Draining,
    Done,
}

struct AGen {
    st: AgSt,
    queue: std::collections::VecDeque<(GenHow, ObjId)>,
}

#[derive(Clone, Copy)]
struct AgIntr {
    /// `%AsyncGeneratorFunction.prototype%`.
    fn_proto: ObjId,
    /// `%AsyncGeneratorPrototype%`.
    proto: ObjId,
    /// `%AsyncIteratorPrototype%`.
    aiter: ObjId,
}

impl GenHow {
    fn value(self) -> Value {
        match self {
            GenHow::Next(v) | GenHow::Return(v) | GenHow::Throw(v) => v,
        }
    }
}

impl<'p> Interp<'p> {
    /// The intrinsics of async generators. The first async generator function makes them.
    fn ag_intr(&mut self) -> Res<AgIntr> {
        if let Some(g) = self.aw.ag_intr {
            return Ok(g);
        }
        let aiter = h!(self, self.heap.new_object(Some(self.object_proto)));
        self.sc_sym_method(aiter, self.wk_key(1), "[Symbol.asyncIterator]", ScFn::IterSelf, 0, true)?;
        let fn_proto = h!(self, self.heap.new_object(Some(self.function_proto)));
        let proto = h!(self, self.heap.new_object(Some(aiter)));
        self.def(fn_proto, self.k_prototype, Value::Object(proto), false, false, true)?;
        self.sc_tag(fn_proto, "AsyncGeneratorFunction")?;
        self.def(proto, self.k_constructor, Value::Object(fn_proto), false, false, true)?;
        self.sc_method(proto, "next", ScFn::AgNext, 1)?;
        self.sc_method(proto, "return", ScFn::AgReturn, 1)?;
        self.sc_method(proto, "throw", ScFn::AgThrow, 1)?;
        self.sc_tag(proto, "AsyncGenerator")?;
        let g = AgIntr { fn_proto, proto, aiter };
        self.aw.ag_intr = Some(g);
        self.fn_ctor_of(3, fn_proto)?;
        Ok(g)
    }

    /// A new async generator function object `o`: its prototype and its own `prototype` object.
    fn ag_function(&mut self, o: ObjId) -> Res<()> {
        let g = self.ag_intr()?;
        h!(self, self.heap.set_prototype_of(o, Some(g.fn_proto)));
        let p = h!(self, self.heap.new_object(Some(g.proto)));
        self.def(o, self.k_prototype, Value::Object(p), true, false, false)
    }

    /// Call an async generator function: bind the parameters (an error throws here) and give a new generator object.
    fn call_agen(&mut self, (script, fid): (u32, FuncId), fobj: ObjId, cenv: u32, self_binding: bool, this: Value, args: &[Value]) -> Res<Value> {
        let (saved_env, saved_strict, saved_script) = (self.env, self.strict, self.cur);
        let env = self.new_env(cenv, true)?;
        self.env = env;
        self.select(script as usize);
        let r = self.in_frame(script, Where::Func(fid), false, |it| {
            let f = *it.prog.func(fid);
            it.bind_call(fobj, fid, env, self_binding, this, args)?;
            if let Body::Block(l) = f.body {
                it.declare_scope(l, true)?;
            }
            let proto = match it.get_prop(Value::Object(fobj), it.k_prototype)? {
                Value::Object(p) => p,
                _ => it.ag_intr()?.proto,
            };
            let g = h!(it, it.heap.new_object(Some(proto)));
            let run = Run { script, fid, strict: it.strict, agen: true, result: g, call_env: env, cur_env: env, awaiting: None, stack: Vec::new(), slots: Vec::new() };
            it.aw.agens.insert(g, AGen { st: AgSt::Start(Box::new(run)), queue: Default::default() });
            Ok(Value::Object(g))
        });
        if r.is_err() {
            self.release(env);
        }
        self.select(saved_script);
        self.env = saved_env;
        self.strict = saved_strict;
        r
    }

    fn ag_state(&mut self, g: ObjId) -> &mut AGen {
        self.aw.agens.get_mut(&g).expect("async generator")
    }

    /// `next`, `return` and `throw` of an async generator: a new promise for the request.
    fn ag_method(&mut self, this: Value, how: GenHow) -> Res<Value> {
        let p = self.new_promise()?;
        let g = match this {
            Value::Object(o) if self.aw.agens.contains_key(&o) => o,
            _ => {
                let t = self.display(this);
                let e = self.type_err(&format!("next method called on incompatible receiver {t}"));
                self.reject_promise(p, thrown(e)?)?;
                return Ok(Value::Object(p));
            }
        };
        let st = std::mem::replace(&mut self.ag_state(g).st, AgSt::Running);
        match (st, how) {
            (AgSt::Done, GenHow::Next(_)) => {
                self.ag_state(g).st = AgSt::Done;
                let r = self.sc_iter_result(Value::Undefined, true)?;
                self.resolve_promise(p, r)?;
            }
            (st @ (AgSt::Start(_) | AgSt::Done), GenHow::Throw(v)) => {
                if let AgSt::Start(run) = st {
                    self.run_discard(*run);
                }
                self.ag_state(g).st = AgSt::Done;
                self.reject_promise(p, v)?;
            }
            (st @ (AgSt::Start(_) | AgSt::Done), GenHow::Return(_)) => {
                if let AgSt::Start(run) = st {
                    self.run_discard(*run);
                }
                let a = self.ag_state(g);
                a.st = AgSt::Draining;
                a.queue.push_back((how, p));
                self.ag_await_return(g)?;
            }
            (AgSt::Start(run), _) => {
                self.ag_state(g).queue.push_back((how, p));
                self.ag_resume(g, run, false, how)?;
            }
            (AgSt::Yield(run), _) => {
                self.ag_state(g).queue.push_back((how, p));
                self.ag_resume(g, run, true, how)?;
            }
            (st, _) => {
                let a = self.ag_state(g);
                a.st = st;
                a.queue.push_back((how, p));
            }
        }
        Ok(Value::Object(p))
    }

    /// AsyncGeneratorResume: run the body from its start or from its `yield` with the request `how`.
    fn ag_resume(&mut self, g: ObjId, mut run: Box<Run>, started: bool, how: GenHow) -> Res<()> {
        if self.depth >= self.limits.max_call_depth {
            self.ag_state(g).st = if started { AgSt::Yield(run) } else { AgSt::Start(run) };
            return Err(Error::Limit(LimitKind::CallDepth));
        }
        let (saved_env, saved_strict, saved_script) = (self.env, self.strict, self.cur);
        self.depth += 1;
        self.select(run.script as usize);
        self.env = run.cur_env;
        self.strict = run.strict;
        let (script, fid) = (run.script, run.fid);
        let r = self.in_frame(script, Where::Func(fid), false, |it| {
            let r = it.ag_step(&mut run, started, how);
            it.async_finish(*run, r)
        });
        self.depth -= 1;
        self.select(saved_script);
        self.env = saved_env;
        self.strict = saved_strict;
        r
    }

    /// Run the machine from the start of the body, or from a `yield` with the resume `how`.
    fn ag_step(&mut self, run: &mut Run, started: bool, how: GenHow) -> Res<Out> {
        let first = if !started {
            let Body::Block(l) = self.prog.func(run.fid).body else { return Err(unsupported("async generator body", Span::default())) };
            self.fpush(run, Fr::Body)?;
            match self.list_from(run, l, 0) {
                Ok(Step::Next(n)) => n,
                Ok(Step::Out(o)) => return Ok(o),
                Err(Error::Throw(v)) => Next::Abr(Abr::Throw(v)),
                Err(e) => return Err(e),
            }
        } else {
            match how {
                GenHow::Next(v) => Next::Val(v),
                GenHow::Throw(v) => Next::Abr(Abr::Throw(v)),
                // AsyncGeneratorUnwrapYieldResumption: await the value, then return it.
                GenHow::Return(v) => match self.fpush(run, Fr::ARet).and_then(|()| self.await_value(v)) {
                    Ok(Step::Out(o)) => return Ok(o),
                    Ok(Step::Next(n)) => n,
                    Err(Error::Throw(e)) => Next::Abr(Abr::Throw(e)),
                    Err(e) => return Err(e),
                },
            }
        };
        self.drive(run, first)
    }

    /// The end of a machine run of an async generator (from `async_finish`).
    #[inline(never)]
    fn ag_after(&mut self, mut run: Run, mut r: Res<Out>) -> Res<()> {
        let g = run.result;
        loop {
            match r {
                Ok(Out::Wait(p)) => return self.suspend(run, Ok(p)),
                Ok(Out::Ready(v)) => return self.suspend(run, Err(v)),
                Ok(Out::Yield(v, _)) => {
                    self.ag_settle(g, Ok(v), false)?;
                    let Some(&(how, _)) = self.ag_state(g).queue.front() else {
                        run.cur_env = self.env;
                        self.ag_state(g).st = AgSt::Yield(Box::new(run));
                        return Ok(());
                    };
                    r = self.ag_step(&mut run, true, how);
                }
                Ok(Out::Done(v) | Out::Threw(v)) => {
                    let ok = matches!(r, Ok(Out::Done(_)));
                    self.run_discard(run);
                    self.ag_state(g).st = AgSt::Draining;
                    self.ag_settle(g, if ok { Ok(v) } else { Err(v) }, true)?;
                    return self.ag_drain(g);
                }
                Err(e) => {
                    self.run_discard(run);
                    self.ag_state(g).st = AgSt::Done;
                    return Err(e);
                }
            }
        }
    }

    /// AsyncGeneratorCompleteStep: settle the first request with a value (`Ok`) or a rejection (`Err`).
    fn ag_settle(&mut self, g: ObjId, r: Result<Value, Value>, done: bool) -> Res<()> {
        let Some((_, p)) = self.ag_state(g).queue.pop_front() else { return Ok(()) };
        match r {
            Ok(v) => {
                let o = self.sc_iter_result(v, done)?;
                self.resolve_promise(p, o)
            }
            Err(v) => self.reject_promise(p, v),
        }
    }

    /// AsyncGeneratorDrainQueue: settle the requests that wait after the end of the body.
    fn ag_drain(&mut self, g: ObjId) -> Res<()> {
        loop {
            let Some(&(how, _)) = self.ag_state(g).queue.front() else {
                self.ag_state(g).st = AgSt::Done;
                return Ok(());
            };
            match how {
                GenHow::Return(_) => return self.ag_await_return(g),
                GenHow::Next(_) => self.ag_settle(g, Ok(Value::Undefined), true)?,
                GenHow::Throw(v) => self.ag_settle(g, Err(v), true)?,
            }
        }
    }

    /// AsyncGeneratorAwaitReturn: the first request is a `return(v)`. Settle it with the awaited `v`.
    fn ag_await_return(&mut self, g: ObjId) -> Res<()> {
        let v = self.ag_state(g).queue.front().map_or(Value::Undefined, |r| r.0.value());
        let pr = match self.promise_resolve(v) {
            Ok(Value::Object(pr)) => pr,
            Ok(_) => return Err(unsupported("await", Span::default())),
            Err(e) => {
                let e = thrown(e)?;
                self.ag_settle(g, Err(e), true)?;
                return self.ag_drain(g);
            }
        };
        let ok = self.pfn(PFn::AgRet { g, reject: false }, "", 1)?;
        let err = self.pfn(PFn::AgRet { g, reject: true }, "", 1)?;
        let derived = self.new_promise()?;
        self.perform_then(pr, Value::Object(ok), Value::Object(err), derived)
    }

    /// The reactions of `ag_await_return`.
    fn ag_ret_settled(&mut self, g: ObjId, reject: bool, v: Value) -> Res<Value> {
        if self.aw.agens.contains_key(&g) {
            self.ag_settle(g, if reject { Err(v) } else { Ok(v) }, true)?;
            self.ag_drain(g)?;
        }
        Ok(Value::Undefined)
    }

    // ------------------------------------------------------------ async iteration

    /// GetMethod: `undefined` for a nullish property, a TypeError for one that is not callable.
    fn get_method(&mut self, v: Value, k: PropertyKey) -> Res<Value> {
        let m = self.get_prop(v, k)?;
        if is_nullish(m) {
            return Ok(Value::Undefined);
        }
        if !self.is_callable(m) {
            let t = self.display(m);
            return Err(self.type_err(&format!("{t} is not a function")));
        }
        Ok(m)
    }

    /// GetIterator(v, async). A sync iterator gets `It::i == 1`: its steps run the async-from-sync rules.
    fn aiter_open(&mut self, v: Value) -> Res<It> {
        let mut m = self.get_method(v, self.wk_key(1))?;
        let sync = matches!(m, Value::Undefined);
        if sync {
            m = self.get_method(v, self.sc.k_iterator)?;
            if matches!(m, Value::Undefined) {
                let t = self.display(v);
                return Err(self.type_err(&format!("{t} is not async iterable")));
            }
        }
        let iter = self.call_fn(m, v, &[])?;
        if !matches!(iter, Value::Object(_)) {
            return Err(self.type_err("Result of the Symbol.asyncIterator method is not an object"));
        }
        let next = self.get_prop(iter, self.sc.k_next)?;
        Ok(It { src: v, i: sync as usize, kind: ItKind::Generic { iter, next }, done: false })
    }

    /// Call `next` (op 0), `return` (op 1) or `throw` (op 2) of an async iteration: the value to await. `None` when
    /// the method is missing (only for `return` and `throw` of an async iterator).
    fn aiter_call(&mut self, it: &It, op: u8, a: Option<Value>) -> Res<Option<Value>> {
        let ItKind::Generic { iter, next } = it.kind else { return Err(unsupported("async iteration", Span::default())) };
        if it.i == 1 {
            return self.afs_call(iter, next, op, a).map(Some);
        }
        let m = match op {
            0 => next,
            1 => self.get_method(iter, self.sc.k_return)?,
            _ => {
                let k = h!(self, self.heap.key_from_str("throw"));
                self.get_method(iter, k)?
            }
        };
        if op != 0 && matches!(m, Value::Undefined) {
            return Ok(None);
        }
        let args = a.as_slice();
        Ok(Some(self.call_fn(m, iter, args)?))
    }

    /// One step of the async-from-sync iterator over `iter` (`next` is its `next`): a new promise.
    fn afs_call(&mut self, iter: Value, next: Value, op: u8, a: Option<Value>) -> Res<Value> {
        let p = self.new_promise()?;
        let r = (|| -> Res<Option<(Value, bool)>> {
            let args = a.as_slice();
            let m = match op {
                0 => next,
                1 => self.get_method(iter, self.sc.k_return)?,
                _ => {
                    let k = h!(self, self.heap.key_from_str("throw"));
                    self.get_method(iter, k)?
                }
            };
            if matches!(m, Value::Undefined) {
                if op == 1 {
                    let o = self.sc_iter_result(a.unwrap_or(Value::Undefined), true)?;
                    self.resolve_promise(p, o)?;
                    return Ok(None);
                }
                // No `throw`: close the iterator to give it a chance to clean up, then reject.
                self.iter_close(&mut it_direct(iter, next))?;
                return Err(self.type_err("The iterator does not provide a 'throw' method"));
            }
            let r = self.call_fn(m, iter, args)?;
            if !matches!(r, Value::Object(_)) {
                let t = self.display(r);
                return Err(self.type_err(&format!("Iterator result {t} is not an object")));
            }
            Ok(Some((r, op != 1)))
        })();
        match r {
            Ok(None) => {}
            Ok(Some((r, close))) => self.afs_continue(p, r, iter, next, close)?,
            Err(e) => self.reject_promise(p, thrown(e)?)?,
        }
        Ok(Value::Object(p))
    }

    /// AsyncFromSyncIteratorContinuation: settle `p` with the awaited value of the sync result `r`.
    fn afs_continue(&mut self, p: ObjId, r: Value, iter: Value, next: Value, close: bool) -> Res<()> {
        let x = (|| -> Res<()> {
            let d = self.get_prop_nat(r, self.sc.k_done)?;
            let done = self.truthy(d)?;
            let v = self.get_prop_nat(r, self.k_value)?;
            let w = match self.promise_resolve(v) {
                Ok(Value::Object(w)) => w,
                Ok(_) => return Err(unsupported("await", Span::default())),
                Err(e) if !done && close => return Err(self.iter_close_throw(&mut it_direct(iter, next), e)),
                Err(e) => return Err(e),
            };
            let ok = Value::Object(self.pfn(PFn::AfsUnwrap { done }, "", 1)?);
            let err = if done || !close { Value::Undefined } else { Value::Object(self.pfn(PFn::AfsClose { iter, next }, "", 1)?) };
            self.perform_then(w, ok, err, p)
        })();
        match x {
            Err(e) => self.reject_promise(p, thrown(e)?),
            ok => ok,
        }
    }

    // ---- the machine frames, kept out of the hot `deliver` and `unwind` of `eval_async.rs`

    /// `yield` (`delegate` false) or `yield*` of the value `v` in an async generator. A plain `yield` awaits `v` first.
    #[inline(never)]
    fn ag_yield(&mut self, run: &mut Run, delegate: bool, base: u32, v: Value) -> Res<Step> {
        if delegate {
            let it = self.aiter_open(v)?;
            self.spush(run, Slot::It(it))?;
            return self.adeleg_send(run, base, GenHow::Next(Value::Undefined));
        }
        self.fpush(run, Fr::AYield)?;
        self.await_value(v)
    }

    /// The value `v` reached the frame `fr` of an async generator or a `for await`.
    #[inline(never)]
    fn ag_deliver(&mut self, run: &mut Run, fr: Fr, v: Value) -> Res<Step> {
        let not_object = |it: &mut Self| {
            let t = it.display(v);
            Err(it.type_err(&format!("Iterator result {t} is not an object")))
        };
        match fr {
            Fr::Deleg { base } => self.adeleg_send(run, base, GenHow::Next(v)),
            Fr::Ret => {
                self.fpush(run, Fr::ARet)?;
                self.await_value(v)
            }
            Fr::AYield => Ok(Step::Out(Out::Yield(v, false))),
            Fr::ARet => nx(Next::Abr(Abr::Return(v))),
            Fr::ADeleg { base, ret } => self.adeleg_result(run, base, ret, v),
            Fr::ADelegClose { base } => {
                self.strunc(run, base as usize);
                if !matches!(v, Value::Object(_)) {
                    return not_object(self);
                }
                Err(self.type_err("The iterator does not provide a 'throw' method"))
            }
            Fr::AOfN { s, lb, base } => self.aof_result(run, s, lb, base, v),
            Fr::AOfB { s, lb, base } => self.aof_next(run, s, lb, base),
            Fr::AClose { kind, label, base } => {
                let held = if kind >= 3 { sval(run, base as usize) } else { Value::Undefined };
                self.strunc(run, base as usize);
                if kind == 4 {
                    return nx(Next::Abr(Abr::Throw(held)));
                }
                if !matches!(v, Value::Object(_)) {
                    return not_object(self);
                }
                match kind {
                    0 => val(Value::Undefined),
                    1 => nx(Next::Abr(Abr::Break(label))),
                    2 => nx(Next::Abr(Abr::Continue(label))),
                    _ => nx(Next::Abr(Abr::Return(held))),
                }
            }
            _ => Err(unsupported("async machine", Span::default())),
        }
    }

    /// The completion `abr` unwinds through the frame `fr` of an async generator or a `for await`. `None`: go on.
    #[inline(never)]
    fn ag_unwind(&mut self, run: &mut Run, fr: Fr, abr: Abr) -> Res<Option<Step>> {
        match fr {
            Fr::Deleg { base } => match abr {
                Abr::Throw(v) => return self.adeleg_send(run, base, GenHow::Throw(v)).map(Some),
                Abr::Return(v) => return self.adeleg_send(run, base, GenHow::Return(v)).map(Some),
                _ => self.strunc(run, base as usize),
            },
            Fr::ADeleg { base, .. } | Fr::ADelegClose { base } => self.strunc(run, base as usize),
            Fr::AOfB { s, lb, base } => {
                if matches!(abr, Abr::Continue(l) if lb.has(l)) {
                    return self.aof_next(run, s, lb, base).map(Some);
                }
                if let Some(step) = self.aof_close(run, lb, base, abr)? {
                    return Ok(Some(step));
                }
                if matches!(abr, Abr::Break(l) if lb.has(l)) {
                    return val(Value::Undefined).map(Some);
                }
            }
            Fr::AClose { kind, base, .. } => {
                // The await of the result of `return` failed: a throw that closed the loop wins.
                let held = if kind >= 3 { sval(run, base as usize) } else { Value::Undefined };
                self.strunc(run, base as usize);
                if kind == 4 {
                    return nx(Next::Abr(Abr::Throw(held))).map(Some);
                }
            }
            _ => {}
        }
        Ok(None)
    }

    // ---- async yield*

    /// Send the received value or completion `how` to the inner iteration of an async `yield*` (slot `base`).
    fn adeleg_send(&mut self, run: &mut Run, base: u32, how: GenHow) -> Res<Step> {
        let b = base as usize;
        let Slot::It(it) = run.slots[b] else { return Err(unsupported("yield*", Span::default())) };
        let (op, ret) = match how {
            GenHow::Next(_) => (0, false),
            GenHow::Return(_) => (1, true),
            GenHow::Throw(_) => (2, false),
        };
        let r = match self.aiter_call(&it, op, Some(how.value())) {
            Ok(r) => r,
            Err(e) => {
                self.strunc(run, b);
                return Err(e);
            }
        };
        let Some(r) = r else {
            if let GenHow::Return(v) = how {
                self.strunc(run, b);
                self.fpush(run, Fr::ARet)?;
                return self.await_value(v);
            }
            // No `throw`: close the inner iterator (AsyncIteratorClose), then throw a TypeError.
            let ItKind::Generic { iter, .. } = it.kind else { return Err(unsupported("yield*", Span::default())) };
            let c = self.get_method(iter, self.sc.k_return).and_then(|m| if matches!(m, Value::Undefined) { Ok(None) } else { self.call_fn(m, iter, &[]).map(Some) });
            return match c {
                Ok(Some(c)) => {
                    self.fpush(run, Fr::ADelegClose { base })?;
                    self.await_value(c)
                }
                Ok(None) => {
                    self.strunc(run, b);
                    Err(self.type_err("The iterator does not provide a 'throw' method"))
                }
                Err(e) => {
                    self.strunc(run, b);
                    Err(e)
                }
            };
        };
        self.fpush(run, Fr::ADeleg { base, ret })?;
        self.await_value(r)
    }

    /// The awaited inner result `r` of an async `yield*`: yield its value, or end the `yield*` when it is done.
    fn adeleg_result(&mut self, run: &mut Run, base: u32, ret: bool, r: Value) -> Res<Step> {
        let b = base as usize;
        let x = (|| -> Res<(bool, Value)> {
            if !matches!(r, Value::Object(_)) {
                let t = self.display(r);
                return Err(self.type_err(&format!("Iterator result {t} is not an object")));
            }
            let d = self.get_prop_nat(r, self.sc.k_done)?;
            let done = self.truthy(d)?;
            Ok((done, self.get_prop_nat(r, self.k_value)?))
        })();
        match x {
            Ok((false, v)) => {
                self.fpush(run, Fr::Deleg { base })?;
                Ok(Step::Out(Out::Yield(v, false)))
            }
            Ok((true, v)) => {
                // Node 22 does not await the value of a done `return` result (it awaits the value when there is no `return`).
                self.strunc(run, b);
                if ret { nx(Next::Abr(Abr::Return(v))) } else { val(v) }
            }
            Err(e) => {
                self.strunc(run, b);
                Err(e)
            }
        }
    }

    // ---- for await

    /// Start a `for await` over the value `v` of its right side.
    #[inline(never)]
    fn aof_start(&mut self, run: &mut Run, s: StmtId, lb: Lbl, v: Value) -> Res<Step> {
        let (outer, base) = (self.env, run.slots.len() as u32);
        let it = self.aiter_open(v)?;
        self.spush(run, Slot::It(it))?;
        self.fpush(run, Fr::OfScope { outer, cur: NONE, base })?;
        self.aof_next(run, s, lb, base)
    }

        /// Call `next` of the `for await` iteration in slot `base` and await its result.
    fn aof_next(&mut self, run: &mut Run, s: StmtId, lb: Lbl, base: u32) -> Res<Step> {
        self.tick()?;
        let Slot::It(it) = run.slots[base as usize] else { return Err(unsupported("for await", Span::default())) };
        let r = self.aiter_call(&it, 0, None)?.unwrap_or(Value::Undefined);
        self.fpush(run, Fr::AOfN { s, lb, base })?;
        self.await_value(r)
    }

    /// The awaited result `r` of `next`: end the loop, or bind the value and run the body.
    fn aof_result(&mut self, run: &mut Run, s: StmtId, lb: Lbl, base: u32, r: Value) -> Res<Step> {
        if !matches!(r, Value::Object(_)) {
            let t = self.display(r);
            return Err(self.type_err(&format!("Iterator result {t} is not an object")));
        }
        let d = self.get_prop_nat(r, self.sc.k_done)?;
        if self.truthy(d)? {
            return val(Value::Undefined);
        }
        let v = self.get_prop_nat(r, self.k_value)?;
        // A binding error closes the iterator, so the body frame comes first.
        self.of_body(run, s, v, Fr::AOfB { s, lb, base }, true)
    }

    /// AsyncIteratorClose for the abrupt completion `abr` of a `for await` body. `Some` when it awaits `return`.
    fn aof_close(&mut self, run: &mut Run, lb: Lbl, base: u32, abr: Abr) -> Res<Option<Step>> {
        let Slot::It(it) = run.slots[base as usize] else { return Err(unsupported("for await", Span::default())) };
        let r = match self.aiter_call(&it, 1, None) {
            Ok(None) => return Ok(None),
            Ok(Some(r)) => r,
            Err(e @ Error::Throw(_)) => {
                return if matches!(abr, Abr::Throw(_)) { Ok(None) } else { Err(e) };
            }
            Err(e) => return Err(e),
        };
        let nb = run.slots.len() as u32;
        let (kind, label) = match abr {
            Abr::Break(l) if lb.has(l) => (0, 0),
            Abr::Break(l) => (1, l),
            Abr::Continue(l) => (2, l),
            Abr::Return(v) => {
                self.spush(run, Slot::V(v))?;
                (3, 0)
            }
            Abr::Throw(v) => {
                self.spush(run, Slot::V(v))?;
                (4, 0)
            }
        };
        self.fpush(run, Fr::AClose { kind, label, base: nb })?;
        Ok(Some(match self.await_value(r) {
            Ok(st) => st,
            Err(Error::Throw(e)) => Step::Next(Next::Abr(Abr::Throw(e))),
            Err(e) => return Err(e),
        }))
    }

    // ------------------------------------------------------------ GC

    /// A reached async generator object keeps its suspended run and its queue.
    fn ag_trace(&self, o: ObjId, work: &mut Vec<Value>, env_work: &mut Vec<u32>) {
        let Some(a) = self.aw.agens.get(&o) else { return };
        if let AgSt::Start(run) | AgSt::Yield(run) = &a.st {
            run_trace(run, work, env_work);
        }
        for &(how, p) in &a.queue {
            work.extend([how.value(), Value::Object(p)]);
        }
    }
}
