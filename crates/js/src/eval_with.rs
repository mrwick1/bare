// The `with` statement: an object environment record (ECMA-262 9.1.1.2, 14.11). Included by `eval.rs` (`include!`), so it shares that
// module's imports.
//
// Static side (`w_with`): the body is a `Barrier` (`eval_scope.rs`). Every site in it that must leave the body (also inside a function
// or an arrow made in it) is dynamic, so it runs the full search. A site that a scope inside the body declares stays resolved: no
// object environment lies between it and its scope. The function that holds the `with` gets `FL_NOFR`: the full search cannot read a
// frame slot. A site outside the body does not change: the object environment is in the chain of the body only.
//
// Run time: `exec_with` makes an environment with one binding, `WITH_NAME`, with the flag `WITHB` and the object as its value. It is
// an ordinary environment otherwise, so a closure that captures it keeps it, and the collector traces the object. `Interp::with_any`
// turns on at the first `with`. It is the only cost for code that has none: the plain search (`lookup`) does not change. A dynamic
// site (`find_name_slow`) asks `lookup_dyn`: with the flag off it is `lookup`; with it on, `lookup_with` also asks each object
// environment on the way (HasBinding: `HasProperty`, then `@@unscopables`).
//
// A hit returns the binding of the object environment. It has no `INIT` and no `MUT`, so `get_name` and `assign_name` leave their hot
// path at the check that they make already, and the cold functions `get_name_uninit` and `assign_name_refused` see `WITHB`: they do
// GetBindingValue and SetMutableBinding on the object (each asks `HasProperty` again, as the specification says). An error of
// HasBinding (a proxy trap, a getter of `@@unscopables`) cannot go through the return value of the search. It waits in
// `HScope::with_err`, and the cold function raises it. A call through a name (`link_base`) takes the object that the read left in
// `HScope::with_this` as `this` (WithBaseObject).
//
// Deviations: a name is resolved when the code reads it, not before the right side of an assignment runs. `x++` and `x += 1` resolve
// the name twice (a proxy sees two `has` calls). `with` in a generator or an async function is not
// supported: its walk gives `Unsupported`.

/// The flag of the one binding of an object environment. No `INIT` and no `MUT`.
const WITHB: u8 = 16;
/// The name of that binding. No identifier has it.
const WITH_NAME: u32 = NONE;

impl<'p> Interp<'p> {
    /// The first walk of `with (obj) body`, before its children: the gate for generators and async functions, and the flag that keeps the
    /// function out of frame slots. The children are walked in `w_stmt_inner`, with the other statements: a separate recursive function
    /// here changed the inlining of the whole walker, and every script pays for that.
    #[cold]
    #[inline(never)]
    fn w_with_gate(&mut self, span: Span) -> Res<()> {
        let mut s = self.sc_cur;
        while s != NONE {
            if let RKind::Fn(fid, _) = self.sc_all[s as usize].kind {
                let f = self.prog.func(fid);
                if f.is_async() || f.is_generator() {
                    return Err(unsupported("with statement in a generator or an async function", span));
                }
                break;
            }
            s = self.sc_all[s as usize].parent;
        }
        self.fl |= FL_NOFR;
        Ok(())
    }

    /// Run `with (obj) body`.
    #[inline(never)]
    fn exec_with(&mut self, obj: ExprId, body: StmtId, top: bool) -> Ex {
        if top {
            self.completion = Value::Undefined;
        }
        let v = self.eval_expr(obj)?;
        let o = self.to_object(v)?;
        let outer = self.env;
        let env = self.new_env(outer, false)?;
        self.envs[env as usize].bindings.push(Binding { name: WITH_NAME, flags: WITHB, value: o });
        self.with_any = true;
        self.env = env;
        let r = self.exec(body);
        self.env = outer;
        self.release(env);
        r
    }

    /// The full search of a dynamic site.
    #[inline(always)]
    fn lookup_dyn(&mut self, n: u32) -> Option<(u32, usize)> {
        if self.with_any { self.lookup_with(n) } else { self.lookup(n) }
    }

    /// `lookup` that also asks the object environments on the way. A hit in one is its binding. If the question threw, the error waits
    /// in `HScope::with_err` and the hit is the same.
    #[cold]
    #[inline(never)]
    fn lookup_with(&mut self, n: u32) -> Option<(u32, usize)> {
        self.hs.with_err = None;
        let mut e = self.env;
        while e != NONE {
            if let Some(i) = self.find_in(e, n) {
                return Some((e, i));
            }
            if let Some(Binding { flags, value: Value::Object(o), .. }) = self.envs[e as usize].bindings.first().copied()
                && flags & WITHB != 0
            {
                match self.with_has(o, n) {
                    Ok(false) => {}
                    Ok(true) => return Some((e, 0)),
                    Err(err) => {
                        self.hs.with_err = Some(err);
                        return Some((e, 0));
                    }
                }
            }
            e = self.envs[e as usize].parent;
        }
        None
    }

    /// HasBinding of an object environment (with a `with` environment): `HasProperty`, and then not blocked by `@@unscopables`.
    fn with_has(&mut self, o: ObjId, n: u32) -> Res<bool> {
        let key = self.name_key(n)?;
        if !self.o_has_property(o, key)? {
            return Ok(false);
        }
        let u = self.get_prop(Value::Object(o), self.wk_key(WK_UNSCOPABLES))?;
        if matches!(u, Value::Object(_)) {
            let blocked = self.get_prop(u, key)?;
            if self.truthy(blocked)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// GetBindingValue of an object environment. `obj` is the value of its binding.
    #[cold]
    #[inline(never)]
    fn with_get(&mut self, n: u32, obj: Value) -> Res<Value> {
        if let Some(err) = self.hs.with_err.take() {
            return Err(err);
        }
        let Value::Object(o) = obj else { return Ok(Value::Undefined) };
        let key = self.name_key(n)?;
        // The property may be gone since HasBinding: a sloppy read then gives `undefined`, a strict one throws.
        let v = if self.o_has_property(o, key)? {
            self.get_prop(obj, key)?
        } else if self.strict {
            return Err(self.not_defined(n));
        } else {
            Value::Undefined
        };
        self.hs.with_this = Some(o);
        Ok(v)
    }

    #[cold]
    fn not_defined(&mut self, n: u32) -> Error {
        let m = format!("{} is not defined", self.names[n as usize]);
        self.mk_err(REFERENCE_ERROR, &m)
    }

    /// SetMutableBinding of the object environment `e`.
    #[cold]
    #[inline(never)]
    fn with_set(&mut self, n: u32, e: u32, v: Value) -> Res<()> {
        if let Some(err) = self.hs.with_err.take() {
            return Err(err);
        }
        let obj = self.envs[e as usize].bindings[0].value;
        let key = self.name_key(n)?;
        if let Value::Object(o) = obj
            && !self.o_has_property(o, key)?
            && self.strict
        {
            return Err(self.not_defined(n));
        }
        self.put_prop(obj, key, v)
    }

    /// `delete name` when some `with` ran: a name in an object environment deletes the property. A name in a plain environment is
    /// not deletable. `None`: no environment has it, so the global object decides.
    #[cold]
    #[inline(never)]
    fn delete_with(&mut self, n: u32) -> Res<Option<bool>> {
        let Some((e, i)) = self.lookup_with(n) else { return Ok(None) };
        if self.envs[e as usize].bindings[i].flags & WITHB == 0 {
            return Ok(Some(false));
        }
        if let Some(err) = self.hs.with_err.take() {
            return Err(err);
        }
        let Value::Object(o) = self.envs[e as usize].bindings[0].value else { return Ok(Some(true)) };
        let key = self.name_key(n)?;
        let ok = hxw!(self, self.heap.delete(o, key), self.px_delete(o, key)?);
        Ok(Some(ok))
    }

    /// `typeof name` when some `with` ran: the name is resolved once (a second resolution could meet another `@@unscopables` answer).
    #[cold]
    #[inline(never)]
    fn typeof_with(&mut self, n: u32, a: ExprId) -> Res<Value> {
        let v = match self.find_name(n, a.index() as u32) {
            Some((e, i)) => {
                let b = self.envs[e as usize].bindings[i];
                if b.flags & INIT != 0 { b.value } else { self.get_name_uninit(n, b.flags, b.value)? }
            }
            None => {
                let key = self.name_key(n)?;
                if !hx!(self, self.heap.has_property_lazy(self.global, key)) && (self.hs.scope.is_empty() || self.hscope_find(n)?.is_none()) {
                    Value::Undefined
                } else {
                    self.get_global(n)?
                }
            }
        };
        let t = self.type_of(v);
        Ok(Value::String(h!(self, self.heap.intern_str(t))))
    }

    /// `link_base` of a name when some `with` ran: the callee and its `this`. A name found in an object environment has that object as
    /// `this`.
    #[cold]
    #[inline(never)]
    fn link_name_with(&mut self, e: ExprId) -> Res<Option<(Value, Value)>> {
        self.hs.with_this = None;
        let f = self.get_name(self.expr_name[e.index()], e.index() as u32)?;
        Ok(Some((f, self.hs.with_this.take().map_or(Value::Undefined, Value::Object))))
    }
}
