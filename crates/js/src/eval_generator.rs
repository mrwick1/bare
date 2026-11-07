// Generators (`function*`, `*m(){}`, `yield`, `yield*`). Included by `eval.rs` (`include!`), so it shares that
// module's imports and private items.
//
// A generator runs on the async machine (`eval_async.rs`). The preflight marks `yield` as it marks `await`, so
// only the nodes that contain a `yield` become frames, and every other subtree runs on the ordinary evaluator.
// A `yield` ends a machine step with `Out::Yield`. The run then waits in the generator's entry, and the next
// `next(v)`, `return(v)` or `throw(v)` drives it again synchronously, with `Next::Val(v)`,
// `Next::Abr(Abr::Return(v))` or `Next::Abr(Abr::Throw(v))`. A `finally` and a for-of close run as in an async
// function.
//
// - A call binds the parameters at once (errors throw at the call), then makes the generator object. Its
//   prototype is `F.prototype` if that is an object, else `%GeneratorPrototype%`.
// - `aw.gens` maps a generator object to its state. A suspended run is boxed: a finished generator costs one
//   16-byte entry. The run counts in the frame and slot caps of `AsyncLimits`, not as an activation.
// - `%GeneratorFunction.prototype%` and `%GeneratorPrototype%` are made when the first generator function is.
//   `%GeneratorFunction%` (the constructor that compiles source text) does not exist.
// - `yield*` keeps the inner iteration in a `Slot::It`. A generic inner iterator gets `next`, `throw` and `return`
//   with the received value, and its result objects pass through unchanged. An array, string, Map or Set
//   iteration steps without result objects, ignores the received value and has no `throw`.
// - `for-of`, spread and destructuring over a generator whose `next` is the intrinsic step it directly, without a
//   result object per value (`iter_native_step`).
//
// GC contract: a suspended run is reachable only through its generator object. `collect` traces the run when it
// reaches the object (`gen_trace`) and drops the entries of dead objects (`gen_prune`).

/// The state of one generator object.
enum Gen {
    /// Parameters are bound; the body has not started.
    Start(Box<Run>),
    /// Suspended at a `yield`.
    Yield(Box<Run>),
    Running,
    Done,
}

#[derive(Clone, Copy)]
struct GenIntr {
    /// `%GeneratorFunction.prototype%`: the prototype of every generator function.
    fn_proto: ObjId,
    /// `%GeneratorPrototype%`.
    proto: ObjId,
    next: ObjId,
}

/// How a generator is resumed.
#[derive(Clone, Copy)]
enum GenHow {
    Next(Value),
    Return(Value),
    Throw(Value),
}

/// What a resume gives: a value and `done`, or an inner result object of `yield*` that passes through.
enum GenOut {
    Item(Value, bool),
    Raw(Value),
}

impl<'p> Interp<'p> {
    fn gen_intr(&mut self) -> Res<GenIntr> {
        if let Some(g) = self.aw.gen_intr {
            return Ok(g);
        }
        let fn_proto = h!(self, self.heap.new_object(Some(self.function_proto)));
        let proto = h!(self, self.heap.new_object(Some(self.sc.iter_proto)));
        self.def(fn_proto, self.k_prototype, Value::Object(proto), false, false, true)?;
        self.sc_tag(fn_proto, "GeneratorFunction")?;
        self.fn_ctor_of(1, fn_proto)?;
        self.def(proto, self.k_constructor, Value::Object(fn_proto), false, false, true)?;
        let next = self.sc_method(proto, "next", ScFn::GenNext, 1)?;
        self.sc_method(proto, "return", ScFn::GenReturn, 1)?;
        self.sc_method(proto, "throw", ScFn::GenThrow, 1)?;
        self.sc_tag(proto, "Generator")?;
        let g = GenIntr { fn_proto, proto, next };
        self.aw.gen_intr = Some(g);
        Ok(g)
    }

    /// `%AsyncFunction.prototype%`: the prototype of every async function. The first async function makes it.
    fn async_fn_proto(&mut self) -> Res<ObjId> {
        if let Some(p) = self.aw.afn_proto {
            return Ok(p);
        }
        let p = h!(self, self.heap.new_object(Some(self.function_proto)));
        self.sc_tag(p, "AsyncFunction")?;
        self.fn_ctor_of(2, p)?;
        self.aw.afn_proto = Some(p);
        Ok(p)
    }

    /// The constructor `GeneratorFunction` (kind 1), `AsyncFunction` (kind 2) or `AsyncGeneratorFunction` (kind 3) for
    /// the prototype `proto`.
    fn fn_ctor_of(&mut self, kind: u8, proto: ObjId) -> Res<()> {
        let c = self.native(Nat::FnCtor(kind), ["", "GeneratorFunction", "AsyncFunction", "AsyncGeneratorFunction"][kind as usize], 1)?;
        h!(self, self.heap.set_prototype_of(c, self.aw.fn_ctor));
        self.def(c, self.k_prototype, Value::Object(proto), false, false, false)?;
        self.def(proto, self.k_constructor, Value::Object(c), false, false, true)
    }

    /// A new generator function object `o`: its prototype and its own `prototype` object (no `constructor`).
    fn gen_function(&mut self, o: ObjId) -> Res<()> {
        let g = self.gen_intr()?;
        h!(self, self.heap.set_prototype_of(o, Some(g.fn_proto)));
        let p = h!(self, self.heap.new_object(Some(g.proto)));
        self.def(o, self.k_prototype, Value::Object(p), true, false, false)
    }

    /// Call a generator function: bind the parameters and give back a new generator object.
    fn call_generator(&mut self, (script, fid): (u32, FuncId), fobj: ObjId, cenv: u32, self_binding: bool, this: Value, args: &[Value]) -> Res<Value> {
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
                _ => it.gen_intr()?.proto,
            };
            let g = h!(it, it.heap.new_object(Some(proto)));
            let run = Run { script, fid, strict: it.strict, agen: false, result: g, call_env: env, cur_env: env, awaiting: None, stack: Vec::new(), slots: Vec::new() };
            it.aw.gens.insert(g, Gen::Start(Box::new(run)));
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

    /// `next`, `return` and `throw` of a generator object.
    fn gen_resume(&mut self, g: Value, how: GenHow) -> Res<GenOut> {
        let st = match g {
            Value::Object(o) => self.aw.gens.get_mut(&o).map(|s| std::mem::replace(s, Gen::Running)),
            _ => None,
        };
        let Value::Object(g) = g else { return Err(self.type_err("next method called on incompatible receiver")) };
        let (mut run, started) = match st {
            None => return Err(self.type_err("next method called on incompatible receiver")),
            Some(Gen::Running) => return Err(self.type_err("Generator is already running")),
            Some(Gen::Yield(run)) => (run, true),
            Some(Gen::Start(run)) if matches!(how, GenHow::Next(_)) => (run, false),
            Some(s) => {
                // Not started or done: `return` and `throw` complete it without running the body.
                if let Gen::Start(run) = s {
                    self.run_discard(*run);
                }
                self.aw.gens.insert(g, Gen::Done);
                return match how {
                    GenHow::Next(_) => Ok(GenOut::Item(Value::Undefined, true)),
                    GenHow::Return(v) => Ok(GenOut::Item(v, true)),
                    GenHow::Throw(v) => Err(Error::Throw(v)),
                };
            }
        };
        if self.depth >= self.limits.max_call_depth {
            self.aw.gens.insert(g, if started { Gen::Yield(run) } else { Gen::Start(run) });
            return Err(Error::Limit(LimitKind::CallDepth));
        }
        let (saved_env, saved_strict, saved_script) = (self.env, self.strict, self.cur);
        self.depth += 1;
        self.select(run.script as usize);
        self.env = run.cur_env;
        self.strict = run.strict;
        let (script, fid) = (run.script, run.fid);
        let r = self.in_frame(script, Where::Func(fid), false, |it| {
            let first = if started {
                match how {
                    GenHow::Next(v) => Next::Val(v),
                    GenHow::Return(v) => Next::Abr(Abr::Return(v)),
                    GenHow::Throw(v) => Next::Abr(Abr::Throw(v)),
                }
            } else {
                let Body::Block(l) = it.prog.func(fid).body else { return Err(unsupported("generator body", Span::default())) };
                it.fpush(&mut run, Fr::Body)?;
                match it.list_from(&mut run, l, 0) {
                    Ok(Step::Next(n)) => n,
                    Ok(Step::Out(o)) => return Ok(o),
                    Err(Error::Throw(v)) => Next::Abr(Abr::Throw(v)),
                    Err(e) => return Err(e),
                }
            };
            it.drive(&mut run, first)
        });
        run.cur_env = self.env;
        self.depth -= 1;
        self.select(saved_script);
        self.env = saved_env;
        self.strict = saved_strict;
        let out = match r {
            Ok(Out::Yield(v, raw)) => {
                self.aw.gens.insert(g, Gen::Yield(run));
                return Ok(if raw { GenOut::Raw(v) } else { GenOut::Item(v, false) });
            }
            Ok(Out::Done(v)) => Ok(GenOut::Item(v, true)),
            Ok(Out::Threw(v)) => Err(Error::Throw(v)),
            Ok(Out::Wait(_) | Out::Ready(_)) => Err(unsupported("await in a generator", Span::default())),
            Err(e) => Err(e),
        };
        self.run_discard(*run);
        self.aw.gens.insert(g, Gen::Done);
        out
    }

    /// The native `next`, `return` and `throw`.
    fn gen_method(&mut self, this: Value, how: GenHow) -> Res<Value> {
        match self.gen_resume(this, how)? {
            GenOut::Item(v, done) => self.sc_iter_result(v, done),
            GenOut::Raw(r) => Ok(r),
        }
    }

    /// `iter_native_step` for a generator whose `next` is the intrinsic: the value, or `None` when done.
    fn gen_step(&mut self, io: ObjId, no: ObjId) -> Res<Option<Option<Value>>> {
        if self.aw.gen_intr.is_none_or(|g| g.next != no) || !self.aw.gens.contains_key(&io) {
            return Ok(None);
        }
        match self.gen_resume(Value::Object(io), GenHow::Next(Value::Undefined))? {
            GenOut::Item(_, true) => Ok(Some(None)),
            GenOut::Item(v, false) => Ok(Some(Some(v))),
            GenOut::Raw(r) => {
                let d = self.get_prop(r, self.sc.k_done)?;
                if self.truthy(d)? {
                    return Ok(Some(None));
                }
                Ok(Some(Some(self.get_prop(r, self.k_value)?)))
            }
        }
    }

    // ------------------------------------------------------------ yield* on the machine

    /// One step of `yield*`: send the received value or completion to the inner iteration in slot `base`. The
    /// frame `Fr::Deleg` is already popped. A done inner result gives the value of `yield*` (or, for `return`,
    /// continues the return); any other result yields again.
    fn deleg_step(&mut self, run: &mut Run, base: u32, how: GenHow) -> Res<Step> {
        enum D {
            Yield(Value, bool),
            Value(Value),
            Return(Value),
        }
        let b = base as usize;
        let Slot::It(mut it) = run.slots[b] else { return Err(unsupported("yield*", Span::default())) };
        let r = (|| -> Res<D> {
            let ItKind::Generic { iter, next } = it.kind else {
                // A built-in iteration: no result objects, no `throw`, and `return` is the close of a for-of.
                return match how {
                    GenHow::Next(_) => Ok(match self.iter_next(&mut it)? {
                        Some(v) => D::Yield(v, false),
                        None => D::Value(Value::Undefined),
                    }),
                    GenHow::Return(v) => self.iter_close(&mut it).map(|()| D::Return(v)),
                    GenHow::Throw(_) => {
                        self.iter_close(&mut it)?;
                        Err(self.type_err("The iterator does not provide a 'throw' method"))
                    }
                };
            };
            let (m, a) = match how {
                GenHow::Next(v) => (next, v),
                GenHow::Throw(v) => {
                    let k = h!(self, self.heap.key_from_str("throw"));
                    let m = self.get_prop(iter, k)?;
                    if is_nullish(m) {
                        self.iter_close(&mut it)?;
                        return Err(self.type_err("The iterator does not provide a 'throw' method"));
                    }
                    (m, v)
                }
                GenHow::Return(v) => {
                    let m = self.get_prop(iter, self.sc.k_return)?;
                    if is_nullish(m) {
                        return Ok(D::Return(v));
                    }
                    (m, v)
                }
            };
            let r = self.call_fn(m, iter, &[a])?;
            if !matches!(r, Value::Object(_)) {
                let t = self.display(r);
                return Err(self.type_err(&format!("Iterator result {t} is not an object")));
            }
            let d = self.get_prop(r, self.sc.k_done)?;
            if !self.truthy(d)? {
                return Ok(D::Yield(r, true));
            }
            let v = self.get_prop(r, self.k_value)?;
            Ok(if let GenHow::Return(_) = how { D::Return(v) } else { D::Value(v) })
        })();
        if let Ok(D::Yield(v, raw)) = r {
            run.slots[b] = Slot::It(it);
            self.fpush(run, Fr::Deleg { base })?;
            return Ok(Step::Out(Out::Yield(v, raw)));
        }
        self.strunc(run, b);
        match r? {
            D::Value(v) => val(v),
            D::Return(v) => nx(Next::Abr(Abr::Return(v))),
            D::Yield(..) => unreachable!(),
        }
    }
}

impl<'p> Interp<'p> {
    // ------------------------------------------------------------ GC

    fn gen_roots(&self, work: &mut Vec<Value>) {
        if let Some(g) = self.aw.gen_intr {
            work.extend([g.fn_proto, g.proto, g.next].map(Value::Object));
        }
        work.extend(self.aw.fn_ctor.into_iter().chain(self.aw.afn_proto).map(Value::Object));
        if let Some(g) = self.aw.ag_intr {
            work.extend([g.fn_proto, g.proto, g.aiter].map(Value::Object));
        }
    }

    /// A reached generator object keeps its suspended run.
    fn gen_trace(&self, o: ObjId, work: &mut Vec<Value>, env_work: &mut Vec<u32>) {
        if !self.aw.agens.is_empty() {
            self.ag_trace(o, work, env_work);
        }
        if self.aw.gens.is_empty() {
            return;
        }
        if let Some(Gen::Start(run) | Gen::Yield(run)) = self.aw.gens.get(&o) {
            run_trace(run, work, env_work);
        }
    }

    /// Drop the entries of dead generator objects. Their environments are freed by `collect`.
    fn gen_prune(&mut self, seen: &Seen) {
        let (mut frames, mut slots) = (0, 0);
        self.aw.gens.retain(|o, g| {
            let keep = seen.contains(o);
            if let (false, Gen::Start(run) | Gen::Yield(run)) = (keep, &*g) {
                frames += run.stack.len();
                slots += run.slots.len();
            }
            keep
        });
        self.aw.agens.retain(|o, g| {
            let keep = seen.contains(o);
            if let (false, AgSt::Start(run) | AgSt::Yield(run)) = (keep, &g.st) {
                frames += run.stack.len();
                slots += run.slots.len();
            }
            keep
        });
        self.aw.frames -= frames;
        self.aw.slots -= slots;
    }

    /// Every value of every suspended generator, for `roots`.
    fn gen_all_values(&self, out: &mut Vec<Value>) {
        self.gen_roots(out);
        let mut scratch = Vec::new();
        for g in self.aw.gens.values() {
            if let Gen::Start(run) | Gen::Yield(run) = g {
                run_trace(run, out, &mut scratch);
            }
        }
        for &o in self.aw.agens.keys() {
            self.ag_trace(o, out, &mut scratch);
        }
    }
}
