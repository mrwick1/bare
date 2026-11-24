//! Async generators, `for await` and the async-from-sync iterator (`docs/internal/design-js2-async.md` 6.4; plan step A2). Safe code over
//! [`Realm`], as `generator.rs` is. The compiler side is `compile_asyncgen.rs`.
//!
//! **The object** is an *activation* like that of a generator, of the kind `K_AGEN` (and not `K_GEN`: no `for-of` steps it, and
//! `gen_request` does not accept it) and six hidden slots. Slots 0 to 3 are those of a generator (`generator.rs`). Slot 4 is the queue
//! of requests, an array of triples `(mode, value, promise)`: `next`, `throw` and `return` add one and give its promise. Slot 5 is the
//! promise of the call that entered the frame, which the frame gives to that caller at its next suspension.
//!
//! **States** (slot 0): the four of a generator, and [`G_AWAIT`] (the frame is saved at an `await`; for a script the generator is still
//! executing, so a request only waits in the queue) and [`G_AWAIT_RET`] (a `return(v)` waits for the promise of `v`).
//!
//! **Who runs what.** A request on a generator that waits at a `yield` or has not started enters the frame: the native `next` asks for
//! `Outcome::Enter` as the native of a generator does, and the loop pushes the frame. The frame runs until it suspends. `Yield` settles
//! the promise of the first request, and goes on at once when another request waits (it needs no suspension); `Await` makes the
//! activation the reaction of a promise, and its job enters the frame again ([`Realm::agen_job`]); `RetGen` settles the first request
//! with `done: true`, then settles the others ([`Realm::agen_drain`]). A throw out of the body ends in `RetGen` with `c` = 1: the
//! code of an async generator has the catch-all handler of an async function.
use crate::builtins::{HIDDEN, NativeFn, arg};
use crate::bytecode::Code;
use crate::generator::{G_AWAIT_RET, G_DONE, G_RUNNING, G_START, G_SUSPENDED, MODE_NEXT, MODE_RETURN, MODE_THROW};
use crate::interp::{REC, STOP};
use crate::interp_rt::{ErrKind, Key, R, Realm};
use crate::object::{K_AGEN, Obj};
use crate::shapes::{A_C, ShapeId};
use crate::value::Value;

/// What `AYsCall` found: the call was made (its result is to be awaited), or the iterator has no `throw` or no `return`.
pub(crate) enum YsCall {
    Call(Value),
    NoThrow,
    NoReturn,
}

/// The objects that async generators need, made when the first async generator function is (`ag_proto`). All are rooted constants.
#[derive(Default)]
pub(crate) struct AgRealm {
    /// `%AsyncGeneratorPrototype%`.
    pub proto: Option<Obj>,
    /// `%AsyncGeneratorFunction.prototype%`.
    pub func_proto: Option<Obj>,
    /// `%AsyncIteratorPrototype%`.
    pub iter_proto: Option<Obj>,
    /// `%AsyncFromSyncIteratorPrototype%`, its shape (pinned) and its `next`.
    pub afs_proto: Option<Obj>,
    pub afs_shape: Option<ShapeId>,
    pub afs_next: Option<Value>,
    /// The indexes in `Realm::natives` of the two closures of the continuation of an async-from-sync step.
    pub id_unwrap: usize,
    pub id_close: usize,
}

impl Realm {
    fn agen(v: Value) -> Option<Obj> {
        Realm::obj(v).filter(|o| o.is_kind(K_AGEN))
    }

    fn agen_state(a: Obj) -> i32 {
        a.inline_slot(0).as_int().expect("the state of an async generator")
    }

    fn agen_set(a: Obj, s: i32) {
        a.set_inline_slot(0, Value::int(s));
    }

    // ---- the queue ----

    fn aq_push(&mut self, a: Obj, mode: i32, v: Value, p: Value) {
        let q = match Realm::obj(a.inline_slot(4)) {
            Some(q) => q,
            None => {
                let q = Obj::array(&mut self.heap, &self.shapes, self.s_array, &[]).expect("memory");
                a.set_inline_slot(4, q.value());
                q
            }
        };
        let n = q.len();
        for (i, x) in [Value::int(mode), v, p].into_iter().enumerate() {
            q.set_element(&mut self.heap, n + i as u32, x).expect("memory");
        }
    }

    /// The first request: `(mode, value, promise)`.
    fn aq_head(a: Obj) -> Option<(i32, Value, Value)> {
        let q = Realm::obj(a.inline_slot(4))?;
        if q.len() < 3 {
            return None;
        }
        let g = |i| q.get_element(i).unwrap_or(Value::UNDEFINED);
        Some((g(0).as_int().expect("a mode"), g(1), g(2)))
    }

    fn aq_pop(&mut self, a: Obj) {
        let q = Realm::obj(a.inline_slot(4)).expect("a queue");
        let n = q.len();
        for i in 3..n {
            let v = q.get_element(i).unwrap_or(Value::UNDEFINED);
            q.set_element(&mut self.heap, i - 3, v).expect("memory");
        }
        q.set_len(&mut self.heap, n.saturating_sub(3)).expect("memory");
    }

    /// `AsyncGeneratorCompleteStep`: take the first request and settle its promise with `{ value: v, done }` (or reject it with `v`).
    fn agen_complete(&mut self, a: Obj, v: Value, done: bool, reject: bool) -> R<()> {
        let Some((_, _, p)) = Realm::aq_head(a) else { return Ok(()) };
        self.aq_pop(a);
        let Some(po) = Realm::promise_obj(p) else { return Ok(()) };
        if reject {
            self.reject_promise(po, v);
            return Ok(());
        }
        let r = self.iter_result(v, done);
        self.resolve_promise(po, r)
    }

    /// `AsyncGeneratorDrainQueue`: the generator is done; answer the requests that wait. A `return(v)` waits for `v` first.
    fn agen_drain(&mut self, a: Obj) -> R<()> {
        while let Some((mode, v, _)) = Realm::aq_head(a) {
            match mode {
                MODE_RETURN => {
                    Realm::agen_set(a, G_AWAIT_RET);
                    return self.agen_await_return(a);
                }
                MODE_THROW => self.agen_complete(a, v, true, true)?,
                _ => self.agen_complete(a, Value::UNDEFINED, true, false)?,
            }
        }
        Ok(())
    }

    /// `AsyncGeneratorAwaitReturn`: the first request is a `return(v)` on a finished generator: wait for `v`, then settle it.
    fn agen_await_return(&mut self, a: Obj) -> R<()> {
        let (_, v, _) = Realm::aq_head(a).expect("a return request");
        let r = self.promise_resolve_default(v).and_then(|p| self.perform_then(p, a.value(), a.value(), Value::UNDEFINED));
        match r {
            Err(e) if e == STOP => Err(e),
            Err(e) => {
                Realm::agen_set(a, G_DONE);
                self.agen_complete(a, e, true, true)?;
                self.agen_drain(a)
            }
            Ok(()) => Ok(()),
        }
    }

    /// The job of a promise that the activation waits for: an `await` (`G_AWAIT`: enter the frame with the value) or a `return(v)`
    /// (`G_AWAIT_RET`: settle the request).
    pub(crate) fn agen_job(&mut self, act: Value, v: Value, reject: bool) -> R<()> {
        let a = Realm::agen(act).expect("an async generator");
        if Realm::agen_state(a) == G_AWAIT_RET {
            Realm::agen_set(a, G_DONE);
            self.agen_complete(a, v, true, reject)?;
            return self.agen_drain(a);
        }
        Realm::agen_set(a, G_RUNNING);
        a.set_inline_slot(2, v);
        a.set_inline_slot(3, Value::int(if reject { MODE_THROW } else { MODE_NEXT }));
        self.resume_raw(act).map(drop)
    }

    /// The frame cannot go on (it cannot be entered, or it threw out of its catch-all handler): the generator is done, the first request fails.
    pub(crate) fn agen_abort(&mut self, a: Obj, e: Value) {
        crate::generator::finish(a);
        let _ = self.agen_complete(a, e, true, true);
        let _ = self.agen_drain(a);
    }

    // ---- the ops ----

    /// `Yield` in an async generator: the first request gets `{ value: v, done: false }`. When another request waits, the frame goes on
    /// at once with it (`None`: the registers of the list got the sent value and the mode); else it is saved and the caller gets `Some`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn agen_yield(&mut self, stack: &mut [Value], fp: usize, ci: usize, code: &Code, tbl: u32, resume: usize, v: Value) -> R<Option<Value>> {
        let a = Realm::agen(stack[fp - REC + 8]).expect("the activation of an async generator frame");
        self.agen_complete(a, v, false, false)?;
        let list = code.save_list(tbl).expect("the save list of a suspension");
        if let Some((mode, sv, _)) = Realm::aq_head(a) {
            stack[fp + list.sent as usize] = sv;
            stack[fp + list.mode as usize] = Value::int(mode);
            return Ok(None);
        }
        let fr = Realm::ctx(a.inline_slot(1)).expect("the record of an async generator");
        crate::generator::save(stack, fp, ci, code, tbl, resume, fr);
        Realm::agen_set(a, G_SUSPENDED);
        Ok(Some(a.inline_slot(5)))
    }

    /// `RetGen` in an async generator (`reject`: a throw out of the body): settle the first request and answer the others. Gives what the
    /// caller of the frame receives.
    pub(crate) fn agen_return(&mut self, stack: &[Value], fp: usize, v: Value, reject: bool) -> R<Value> {
        let a = Realm::agen(stack[fp - REC + 8]).expect("the activation of an async generator frame");
        crate::generator::finish(a);
        let p = a.inline_slot(5);
        self.agen_complete(a, v, true, reject)?;
        self.agen_drain(a)?;
        Ok(p)
    }

    // ---- the natives ----

    /// `next`, `throw` and `return` of an async generator (27.6.1.2 to 4).
    fn agen_request(&mut self, this: Value, mode: i32, v: Value) -> R<Value> {
        let p = self.new_promise();
        let Some(a) = Realm::agen(this) else {
            let e = self.error(ErrKind::Type, "not an async generator");
            self.reject_promise(p, e);
            return Ok(p.value());
        };
        let mut st = Realm::agen_state(a);
        if mode == MODE_THROW && st == G_START {
            crate::generator::finish(a);
            st = G_DONE;
        }
        if st == G_DONE && mode != MODE_RETURN {
            if mode == MODE_THROW {
                self.reject_promise(p, v);
            } else {
                let r = self.iter_result(Value::UNDEFINED, true);
                self.resolve_promise(p, r)?;
            }
            return Ok(p.value());
        }
        self.aq_push(a, mode, v, p.value());
        if mode == MODE_RETURN && (st == G_START || st == G_DONE) {
            Realm::agen_set(a, G_AWAIT_RET);
            self.agen_await_return(a)?;
        } else if st == G_START || st == G_SUSPENDED {
            Realm::agen_set(a, G_RUNNING);
            a.set_inline_slot(2, v);
            a.set_inline_slot(3, Value::int(mode));
            a.set_inline_slot(5, p.value());
            // The loop (or `call_held`) enters the frame; the frame gives `p` back at its first suspension.
            self.tail = Some((this, Value::HOLE));
            return Ok(Value::UNDEFINED);
        }
        Ok(p.value())
    }

    /// The promise that the frame of `g` just gave back (`Realm::resume_script`): an async generator gives the promise of its request.
    pub(crate) fn agen_entry_promise(g: Value) -> Option<Value> {
        Realm::agen(g).map(|a| a.inline_slot(5))
    }

    /// `%AsyncGeneratorPrototype%`, made with the objects around it at the first ask.
    pub(crate) fn ag_proto(&mut self) -> Obj {
        if self.genr.ag.proto.is_none() {
            self.make_ag_protos();
        }
        self.genr.ag.proto.expect("made")
    }

    #[cold]
    fn make_ag_protos(&mut self) {
        // `%AsyncIteratorPrototype%` holds `[Symbol.asyncIterator]() { return this }`.
        let ai = Obj::new(&mut self.heap, &self.shapes, self.s_object).expect("memory");
        let f = self.new_native(this_fn, false, 0, "[Symbol.asyncIterator]");
        self.define_attrs(ai, Key::Name(self.wk[crate::bi_symbol::ASYNC_ITERATOR]), HIDDEN, f.value()).expect("define");
        let shape = self.shapes.root(crate::cell::Layout::Object, Some(ai.cell()), 4);
        let gp = Obj::new(&mut self.heap, &self.shapes, shape).expect("memory");
        for (n, f) in [("next", agen_next as NativeFn), ("return", agen_return), ("throw", agen_throw)] {
            self.method(gp, n, 1, f);
        }
        let fshape = self.shapes.root(crate::cell::Layout::Object, Some(self.function_proto.cell()), 4);
        let fp = Obj::new(&mut self.heap, &self.shapes, fshape).expect("memory");
        let tag = Key::Name(self.wk[crate::bi_symbol::TO_STRING_TAG]);
        let (k_proto, k_ctor) = (self.k_prototype, self.k_constructor);
        let (t1, t2) = (self.string("AsyncGeneratorFunction"), self.string("AsyncGenerator"));
        self.define_attrs(fp, Key::Name(k_proto), A_C, gp.value()).expect("define");
        self.define_attrs(fp, tag, A_C, t1).expect("define");
        self.define_attrs(gp, Key::Name(k_ctor), A_C, fp.value()).expect("define");
        self.define_attrs(gp, tag, A_C, t2).expect("define");
        self.dynamic_ctor(fp, "AsyncGeneratorFunction", crate::builtins::async_generator_function_ctor);
        self.consts.extend([ai.value(), gp.value(), fp.value()]);
        (self.genr.ag.proto, self.genr.ag.func_proto, self.genr.ag.iter_proto) = (Some(gp), Some(fp), Some(ai));
    }

    /// `Closure` made the async generator function `f`: it inherits `%AsyncGeneratorFunction.prototype%`.
    #[cold]
    pub(crate) fn make_async_generator_function(&mut self, f: Value) {
        self.ag_proto();
        let fp = self.genr.ag.func_proto.expect("made");
        let o = Realm::obj(f).expect("a function");
        o.set_proto(&mut self.shapes, Some(fp.cell()));
    }

    // ---- `for await` and `yield*` ----

    /// `GetIterator(v, async)`: the iterator and its `next`. A value with no `Symbol.asyncIterator` gives its sync iterator, wrapped in an
    /// async-from-sync iterator.
    #[cold]
    pub(crate) fn aiter_open(&mut self, v: Value) -> R<(Value, Value)> {
        let key = |rt: &Realm, i: usize| Key::Name(rt.wk[i]);
        let method = |rt: &mut Realm, i: usize| -> R<Value> {
            let m = if v.is_undefined() || v.is_null() { Value::UNDEFINED } else { rt.get_prop(v, key(rt, i))? };
            if m.is_undefined() || m.is_null() {
                return Ok(Value::UNDEFINED);
            }
            if !Realm::obj(m).is_some_and(|o| o.is_callable()) {
                return rt.type_error("the iterator method is not a function");
            }
            Ok(m)
        };
        let m = method(self, crate::bi_symbol::ASYNC_ITERATOR)?;
        let sync = m.is_undefined();
        let m = if sync { method(self, crate::bi_symbol::ITERATOR)? } else { m };
        if m.is_undefined() {
            return self.type_error("the value is not iterable");
        }
        let it = self.call_function(m, v, &[])?;
        if Realm::obj(it).is_none() {
            return self.type_error("the result of the iterator method is not an object");
        }
        let next = self.get_named(it, "next")?;
        if !sync {
            return Ok((it, next));
        }
        let w = self.afs_new(it, next);
        Ok((w, self.genr.ag.afs_next.expect("made")))
    }

    /// The step of a `for await` loop or of `yield*` with a value: `next.call(it, ...)`, not awaited.
    pub(crate) fn aiter_call(&mut self, it: Value, next: Value, args: &[Value]) -> R<Value> {
        self.call_function(next, it, args)
    }

    /// `done` and `value` of the awaited result `r`: `(value, done)`. `value_when_done`: read `value` also when `done` (`yield*`).
    pub(crate) fn aiter_unpack(&mut self, r: Value, value_when_done: bool) -> R<(Value, bool)> {
        if Realm::obj(r).is_none() {
            return self.type_error("the result of an async iterator is not an object");
        }
        let d = self.get_named(r, "done")?;
        let done = Realm::truthy(d);
        if done && !value_when_done {
            return Ok((Value::UNDEFINED, true));
        }
        Ok((self.get_named(r, "value")?, done))
    }

    /// The first half of `AsyncIteratorClose`: the result of `return()` (to be awaited), or `None` when the iterator has no `return`. For a
    /// throw (`throwing`) an error of the getter or of the call is dropped.
    pub(crate) fn aiter_close_call(&mut self, it: Value, throwing: bool) -> R<Option<Value>> {
        let drop_err = |e: Value| throwing && e != STOP;
        let ret = match self.get_named(it, "return") {
            Err(e) if drop_err(e) => return Ok(None),
            r => r?,
        };
        if ret.is_undefined() || ret.is_null() {
            return Ok(None);
        }
        match self.call_function(ret, it, &[]) {
            Err(e) if drop_err(e) => Ok(None),
            r => r.map(Some),
        }
    }

    /// `AYsCall`: send `sent` in `mode` to the async iterator.
    pub(crate) fn ays_call(&mut self, it: Value, next: Value, mode: i32, sent: Value) -> R<YsCall> {
        if mode == MODE_NEXT {
            return self.call_function(next, it, &[sent]).map(YsCall::Call);
        }
        let name = if mode == MODE_THROW { "throw" } else { "return" };
        let t = self.get_named(it, name)?;
        if t.is_undefined() || t.is_null() {
            return Ok(if mode == MODE_THROW { YsCall::NoThrow } else { YsCall::NoReturn });
        }
        self.call_function(t, it, &[sent]).map(YsCall::Call)
    }

    // ---- the async-from-sync iterator ----

    /// `CreateAsyncFromSyncIterator`: an object that holds the sync iterator and its `next`.
    fn afs_new(&mut self, it: Value, next: Value) -> Value {
        if self.genr.ag.afs_proto.is_none() {
            self.make_afs();
        }
        let shape = self.genr.ag.afs_shape.expect("made");
        let o = Obj::new(&mut self.heap, &self.shapes, shape).expect("memory");
        o.set_inline_slot(0, it);
        o.set_inline_slot(1, next);
        o.value()
    }

    #[cold]
    fn make_afs(&mut self) {
        self.ag_proto();
        let ai = self.genr.ag.iter_proto.expect("made");
        let shape = self.shapes.root(crate::cell::Layout::Object, Some(ai.cell()), 2);
        let p = Obj::new(&mut self.heap, &self.shapes, shape).expect("memory");
        for (n, f) in [("next", afs_next as NativeFn), ("return", afs_return), ("throw", afs_throw)] {
            self.method(p, n, 1, f);
        }
        let next = self.get_named(p.value(), "next").expect("next");
        let mut s = self.shapes.root(crate::cell::Layout::Object, Some(p.cell()), 2);
        for k in self.genr.keys[..2].iter().copied() {
            s = self.shapes.add(s, k, 0).expect("shape");
        }
        self.pins.push(s);
        self.natives.push(crate::builtins::Native { f: afs_unwrap, ctor: false, len: 1, name: "" });
        self.natives.push(crate::builtins::Native { f: afs_close, ctor: false, len: 1, name: "" });
        let n = self.natives.len();
        self.consts.extend([p.value(), next]);
        let ag = &mut self.genr.ag;
        (ag.afs_proto, ag.afs_shape, ag.afs_next, ag.id_unwrap, ag.id_close) = (Some(p), Some(s), Some(next), n - 2, n - 1);
    }

    /// `AsyncFromSyncIteratorContinuation`: the answer of the sync iterator `r` becomes a promise of `{ value, done }` with the value awaited.
    fn afs_continue(&mut self, r: Value, p: Obj, it: Value, close: bool) -> R<Value> {
        let r = (|| -> R<(Value, bool)> {
            let d = self.get_named(r, "done")?;
            Ok((self.get_named(r, "value")?, Realm::truthy(d)))
        })();
        let (value, done) = match r {
            Ok(x) => x,
            Err(e) if e == STOP => return Err(e),
            Err(e) => {
                self.reject_promise(p, e);
                return Ok(p.value());
            }
        };
        let wrapper = match self.promise_resolve_default(value) {
            Ok(w) => w,
            Err(e) if e == STOP => return Err(e),
            Err(e) => {
                if !done && close {
                    self.iter_close(it, Value::NULL, true)?;
                }
                self.reject_promise(p, e);
                return Ok(p.value());
            }
        };
        let (id_unwrap, id_close) = (self.genr.ag.id_unwrap, self.genr.ag.id_close);
        let on_ok = self.native_cell(id_unwrap);
        on_ok.set_slot(&self.shapes, 1, Value::bool(done));
        let on_err = if done || !close {
            Value::UNDEFINED
        } else {
            let c = self.native_cell(id_close);
            c.set_slot(&self.shapes, 1, it);
            c.value()
        };
        if let Err(e) = self.perform_then(wrapper, on_ok.value(), on_err, p.value()) {
            if e == STOP {
                return Err(e);
            }
            self.reject_promise(p, e);
        }
        Ok(p.value())
    }

    /// The sync iterator and its `next` of an async-from-sync iterator.
    fn afs_parts(this: Value) -> (Value, Value) {
        let o = Realm::obj(this).expect("an async-from-sync iterator");
        (o.inline_slot(0), o.inline_slot(1))
    }

    /// Reject `p` with the error `e` of a step, or pass a stop on.
    fn afs_fail(&mut self, p: Obj, e: Value) -> R<Value> {
        if e == STOP {
            return Err(e);
        }
        self.reject_promise(p, e);
        Ok(p.value())
    }
}

fn this_fn(_rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    Ok(this)
}

fn agen_next(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    rt.agen_request(this, MODE_NEXT, arg(args, 0))
}

fn agen_return(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    rt.agen_request(this, MODE_RETURN, arg(args, 0))
}

fn agen_throw(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    rt.agen_request(this, MODE_THROW, arg(args, 0))
}

fn afs_next(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let p = rt.new_promise();
    let (it, next) = Realm::afs_parts(this);
    let r = if args.is_empty() { rt.call_function(next, it, &[]) } else { rt.call_function(next, it, &[args[0]]) };
    match r {
        Err(e) => rt.afs_fail(p, e),
        Ok(r) if Realm::obj(r).is_none() => {
            let e = rt.error(ErrKind::Type, "the result of the iterator is not an object");
            rt.afs_fail(p, e)
        }
        Ok(r) => rt.afs_continue(r, p, it, true),
    }
}

fn afs_return(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let p = rt.new_promise();
    let (it, _) = Realm::afs_parts(this);
    let ret = match rt.get_named(it, "return") {
        Ok(r) => r,
        Err(e) => return rt.afs_fail(p, e),
    };
    if ret.is_undefined() || ret.is_null() {
        let r = rt.iter_result(arg(args, 0), true);
        rt.resolve_promise(p, r)?;
        return Ok(p.value());
    }
    let r = if args.is_empty() { rt.call_function(ret, it, &[]) } else { rt.call_function(ret, it, &[args[0]]) };
    match r {
        Err(e) => rt.afs_fail(p, e),
        Ok(r) if Realm::obj(r).is_none() => {
            let e = rt.error(ErrKind::Type, "the result of return() is not an object");
            rt.afs_fail(p, e)
        }
        Ok(r) => rt.afs_continue(r, p, it, false),
    }
}

fn afs_throw(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let p = rt.new_promise();
    let (it, _) = Realm::afs_parts(this);
    let t = match rt.get_named(it, "throw") {
        Ok(t) => t,
        Err(e) => return rt.afs_fail(p, e),
    };
    if t.is_undefined() || t.is_null() {
        // The iterator cannot be thrown into: close it, then fail with a TypeError (or with the error of the close).
        return match rt.iter_close(it, Value::NULL, false) {
            Err(e) => rt.afs_fail(p, e),
            Ok(()) => {
                let e = rt.error(ErrKind::Type, "the iterator does not have a throw method");
                rt.afs_fail(p, e)
            }
        };
    }
    match rt.call_function(t, it, &[arg(args, 0)]) {
        Err(e) => rt.afs_fail(p, e),
        Ok(r) if Realm::obj(r).is_none() => {
            let e = rt.error(ErrKind::Type, "the result of throw() is not an object");
            rt.afs_fail(p, e)
        }
        Ok(r) => rt.afs_continue(r, p, it, true),
    }
}

/// The reaction of the awaited value of a sync step: `{ value, done }` (slot 1 holds `done`).
fn afs_unwrap(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let f = Realm::obj(rt.callee).expect("a closure");
    let done = f.slot(&rt.shapes, 1) == Value::bool(true);
    Ok(rt.iter_result(arg(args, 0), done))
}

/// The rejection of the awaited value of a sync step that is not the last: close the sync iterator (slot 1), then fail with the same error.
fn afs_close(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let f = Realm::obj(rt.callee).expect("a closure");
    let it = f.slot(&rt.shapes, 1);
    rt.iter_close(it, Value::NULL, true)?;
    Err(arg(args, 0))
}

#[cfg(test)]
#[path = "asyncgen_tests.rs"]
mod tests;
