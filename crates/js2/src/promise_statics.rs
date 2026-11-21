//! The statics of `Promise` and `Promise.prototype.finally` (`docs/design-js2-async.md`, plan step P3), in Rust: `all`, `allSettled`,
//! `any`, `race`, `withResolvers`, `try`, `Promise[Symbol.species]`, `AggregateError`, and `SpeciesConstructor` for `then`.
//!
//! **Capability.** [`Cap`] is `[promise, resolve, reject]` as three values. For `%Promise%` the promise is a native promise and the
//! functions are its two resolving functions (`Realm::resolving_functions`). Any other constructor is constructed with the executor of
//! `NewPromiseCapability` (a native constructor, a plain script function or a class: `Realm::construct_script`).
//!
//! **A combinator** is one loop, [`Realm::combine`], for the four kinds. It reads `C.resolve` once, opens the iterable, and for each
//! element calls `C.resolve(next)` and then `.then(...)`. A throw anywhere in the loop closes the iterator and rejects the capability
//! (`IfAbruptRejectPromise`); only `STOP` (the budget) is an `Err` of the call.
//!
//! **State.** The state that the element functions share is one `Slots` record of four values: the array of results, the count of
//! elements that wait (it starts at 1 for the loop), and `resolve` and `reject` of the capability. An element function is a native
//! cell: slot 1 is the state record (cleared when the function was called), slot 2 is the index, slot 3 is the twin of an `allSettled`
//! pair (a call of one clears both). `finally` makes two such cells and a thunk. No collection runs inside a native, and a nested call
//! holds the collector, so the loop keeps its values in Rust locals; the state is reachable from the functions that `then` holds.
//!
//! **Iteration.** The iterator protocol is step G3. Until then [`Realm::open_iter`] follows it for an iterable that has
//! `Symbol.iterator`, and an array or a string without it goes through `Realm::iter_values` (the values of an array are read at the start).
use crate::bi_symbol::{ITERATOR, SPECIES};
use crate::builtins::{Native, NativeFn, arg};
use crate::interp::STOP;
use crate::interp_rt::{ErrKind, Key, R, Realm, Slot};
use crate::object::{Ctx, Obj};
use crate::shapes::A_ACC;
use crate::value::Value;

/// A promise capability.
#[derive(Clone, Copy)]
pub(crate) struct Cap {
    pub(crate) promise: Value,
    pub(crate) resolve: Value,
    pub(crate) reject: Value,
}

/// What a combinator does with the settlements.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    All,
    AllSettled,
    Any,
    Race,
}

/// The native functions of this module: the indexes in `Realm::natives` (`Promises::ids`).
pub(crate) const N_ALL: usize = 0;
pub(crate) const N_SETTLED_OK: usize = 1;
pub(crate) const N_SETTLED_ERR: usize = 2;
pub(crate) const N_ANY: usize = 3;
pub(crate) const N_THEN_FINALLY: usize = 4;
pub(crate) const N_CATCH_FINALLY: usize = 5;
pub(crate) const N_THUNK: usize = 6;
pub(crate) const N_THROWER: usize = 7;
pub(crate) const N_IDS: usize = 8;

/// The state record: the results, the count that waits, `resolve` and `reject`.
const S_VALUES: usize = 0;
const S_REMAINING: usize = 1;
const S_RESOLVE: usize = 2;
const S_REJECT: usize = 3;

/// The values of an iterable that a combinator reads one at a time.
enum Src {
    List(Vec<Value>, usize),
    Iter { it: Value, next: Value, done: bool },
}

impl Realm {
    // ---- constructors and species ----

    /// Is `v` a constructor (a native one, a plain function or a class)?
    fn is_constructor(&self, v: Value) -> bool {
        if let Some(i) = self.native_id(v) {
            return self.natives[i].ctor;
        }
        self.function_new(v).is_some_and(|(ci, _)| !self.prog[ci].noctor)
    }

    /// `Construct(c, args)` from a built-in: a native constructor, a plain function or a class.
    pub(crate) fn construct_value(&mut self, c: Value, args: &[Value]) -> R<Value> {
        if self.native_id(c).is_some() {
            let saved = (self.callee, self.nt, self.tail.take(), std::mem::take(&mut self.tail_args));
            let r = self.construct_native(c, args, c);
            (self.callee, self.nt, self.tail, self.tail_args) = saved;
            return r;
        }
        self.construct_script(c, args, c)
    }

    /// Is `c[Symbol.species]` the built-in getter (it gives `c`, and no script can see the read)? For `%Promise%` itself the answer comes
    /// from a one-entry cache (a `Spot` of the getter slot: a replaced getter shows in the slot, a reconfigured property in the shape).
    #[inline]
    pub(crate) fn plain_species(&mut self, c: Obj) -> bool {
        let (k, want) = (self.wk[SPECIES], self.pr.species_fn);
        if c.value() == self.pr.ctor {
            if let Some(v) = self.pr.species_spot.and_then(|s| c.read_spot(s)) {
                return v == want;
            }
            self.pr.species_spot = self.shapes.find(c.shape(&self.shapes), k).filter(|e| e.attrs() & A_ACC != 0).and_then(|e| c.spot(&self.shapes, e.slot()));
        }
        matches!(self.lookup_g(c, k), Some(Slot::Acc(g, _)) if g == want)
    }

    /// `SpeciesConstructor(p, %Promise%)`. `Promise[Symbol.species]` that is the built-in getter (a read of `this`) costs one lookup.
    #[inline]
    pub(crate) fn species_constructor(&mut self, p: Obj) -> R<Value> {
        let default = self.pr.ctor;
        let c = self.promise_constructor(p)?;
        if c.is_undefined() {
            return Ok(default);
        }
        let Some(co) = Realm::obj(c) else { return self.type_error("the constructor of a promise is not an object") };
        if self.plain_species(co) {
            return Ok(c);
        }
        self.fn_props(co);
        let s = self.get_prop(c, Key::Name(self.wk[SPECIES]))?;
        if s.is_undefined() || s.is_null() {
            Ok(default)
        } else if self.is_constructor(s) {
            Ok(s)
        } else {
            self.type_error("Symbol.species of the constructor of a promise is not a constructor")
        }
    }

    /// `NewPromiseCapability(c)` with the functions as values.
    fn capability(&mut self, c: Value) -> R<Cap> {
        if c == self.pr.ctor {
            let p = self.new_promise();
            let (res, rej) = self.resolving_functions(p);
            return Ok(Cap { promise: p.value(), resolve: res.value(), reject: rej.value() });
        }
        if !self.is_constructor(c) {
            return self.type_error("not a constructor");
        }
        let ex = self.native_cell(self.pr.id_executor);
        let promise = self.construct_value(c, &[ex.value()])?;
        let (res, rej) = (ex.slot(&self.shapes, 1), ex.slot(&self.shapes, 2));
        if !Realm::obj(res).is_some_and(|o| o.is_callable()) || !Realm::obj(rej).is_some_and(|o| o.is_callable()) {
            return self.type_error("the promise capability functions are not callable");
        }
        Ok(Cap { promise, resolve: res, reject: rej })
    }

    /// `IfAbruptRejectPromise`: reject the capability with the thrown `e`, and give its promise. `STOP` goes on.
    fn abrupt(&mut self, cap: Cap, e: Value) -> R<Value> {
        if e == STOP {
            return Err(e);
        }
        self.call_function(cap.reject, Value::UNDEFINED, &[e])?;
        Ok(cap.promise)
    }

    /// `Invoke(p, "then", args)`.
    pub(crate) fn invoke_then(&mut self, p: Value, args: &[Value]) -> R<Value> {
        let then = self.get_prop(p, Key::Name(self.pr.k_then))?;
        self.call_function(then, p, args)
    }

    // ---- iteration ----

    /// `GetIterator(v)`.
    fn open_iter(&mut self, v: Value) -> R<Src> {
        let k = self.wk[ITERATOR];
        let m = self.get_prop(v, Key::Name(k))?;
        if m.is_undefined() || m.is_null() {
            return Ok(Src::List(self.iter_values(v)?, 0));
        }
        if !Realm::obj(m).is_some_and(|o| o.is_callable()) {
            return self.type_error("Symbol.iterator is not a function");
        }
        let it = self.call_function(m, v, &[])?;
        if Realm::obj(it).is_none() {
            return self.type_error("the result of Symbol.iterator is not an object");
        }
        let next = self.get_named(it, "next")?;
        Ok(Src::Iter { it, next, done: false })
    }

    /// The next value, or `None` at the end. After a throw the iterator is done (it is not closed).
    fn step(&mut self, s: &mut Src) -> R<Option<Value>> {
        match s {
            Src::List(v, at) => {
                *at += 1;
                Ok(v.get(*at - 1).copied())
            }
            Src::Iter { it, next, done } => {
                let r = self.step_iter(*it, *next);
                if !matches!(r, Ok(Some(_))) {
                    *done = true;
                }
                r
            }
        }
    }

    fn step_iter(&mut self, it: Value, next: Value) -> R<Option<Value>> {
        let r = self.call_function(next, it, &[])?;
        if Realm::obj(r).is_none() {
            return self.type_error("the result of an iterator is not an object");
        }
        let d = self.get_named(r, "done")?;
        if Realm::truthy(d) {
            return Ok(None);
        }
        self.get_named(r, "value").map(Some)
    }

    /// `IteratorClose` for a throw: the throw of `return` is ignored.
    fn close_iter(&mut self, s: &Src) {
        let Src::Iter { it, done: false, .. } = s else { return };
        if let Ok(ret) = self.get_named(*it, "return")
            && Realm::obj(ret).is_some_and(|o| o.is_callable())
        {
            let _ = self.call_function(ret, *it, &[]);
        }
    }

    /// `IterableToList`.
    pub(crate) fn iterable_to_list(&mut self, v: Value) -> R<Vec<Value>> {
        let mut s = self.open_iter(v)?;
        let mut out = Vec::new();
        while let Some(x) = self.step(&mut s)? {
            out.push(x);
        }
        Ok(out)
    }

    // ---- the combinators ----

    /// The loop of `PerformPromiseAll`, `AllSettled`, `Any` and `Race`. An `Err` is a throw that the caller turns into a rejection.
    fn combine(&mut self, kind: Kind, c: Value, cap: Cap, resolve: Value, src: &mut Src) -> R<()> {
        let state = if kind == Kind::Race {
            None
        } else {
            let st = Ctx::new(&mut self.heap, 4, Value::UNDEFINED).expect("memory");
            st.set(S_VALUES, self.new_array(&[]));
            st.set(S_REMAINING, Value::int(1));
            st.set(S_RESOLVE, cap.resolve);
            st.set(S_REJECT, cap.reject);
            Some(st)
        };
        let mut i = 0u32;
        loop {
            let Some(next) = self.step(src)? else {
                if let Some(st) = state {
                    self.element_done(kind, st)?;
                }
                return Ok(());
            };
            if let Some(st) = state {
                let values = Realm::obj(st.get(S_VALUES)).expect("the results");
                values.set_element(&mut self.heap, i, Value::UNDEFINED).ok_or_else(|| self.error(ErrKind::Range, "out of memory"))?;
            }
            let p = self.call_function(resolve, c, &[next])?;
            let (on_ok, on_err) = match (kind, state) {
                (Kind::Race, _) | (_, None) => (cap.resolve, cap.reject),
                (Kind::All, Some(st)) => (self.element(N_ALL, st, i), cap.reject),
                (Kind::AllSettled, Some(st)) => {
                    let (ok, err) = (self.element(N_SETTLED_OK, st, i), self.element(N_SETTLED_ERR, st, i));
                    Realm::obj(ok).expect("a function").set_slot(&self.shapes, 3, err);
                    Realm::obj(err).expect("a function").set_slot(&self.shapes, 3, ok);
                    (ok, err)
                }
                (Kind::Any, Some(st)) => (cap.resolve, self.element(N_ANY, st, i)),
            };
            if let Some(st) = state {
                st.set(S_REMAINING, Value::int(st.get(S_REMAINING).as_int().expect("a count") + 1));
            }
            self.invoke_then(p, &[on_ok, on_err])?;
            i += 1;
        }
    }

    /// An element function of the native `id`.
    fn element(&mut self, id: usize, st: Ctx, i: u32) -> Value {
        let f = self.native_cell(self.pr.ids[id]);
        f.set_slot(&self.shapes, 1, st.value());
        f.set_slot(&self.shapes, 2, Value::int(i as i32));
        f.set_slot(&self.shapes, 3, Value::UNDEFINED);
        f.value()
    }

    /// One element is done (or the loop is): when none waits, resolve with the results, or reject `any` with an `AggregateError`.
    fn element_done(&mut self, kind: Kind, st: Ctx) -> R<()> {
        let left = st.get(S_REMAINING).as_int().expect("a count") - 1;
        st.set(S_REMAINING, Value::int(left));
        if left != 0 {
            return Ok(());
        }
        let values = st.get(S_VALUES);
        if kind == Kind::Any {
            let e = self.aggregate_error(values);
            self.call_function(st.get(S_REJECT), Value::UNDEFINED, &[e])?;
        } else {
            self.call_function(st.get(S_RESOLVE), Value::UNDEFINED, &[values])?;
        }
        Ok(())
    }

    /// An `AggregateError` with the own property `errors` and no `message`.
    fn aggregate_error(&mut self, errors: Value) -> Value {
        let e = self.make_error(self.pr.agg_proto, None);
        self.hidden(Realm::obj(e).expect("an error"), "errors", errors);
        e
    }

    /// The body of an element function.
    fn element_call(&mut self, kind: Kind, x: Value, settled: Option<bool>) -> R<Value> {
        let f = Realm::obj(self.callee).expect("an element function");
        let Some(st) = Realm::ctx(f.slot(&self.shapes, 1)) else { return Ok(Value::UNDEFINED) };
        f.set_slot(&self.shapes, 1, Value::UNDEFINED);
        if let Some(twin) = Realm::obj(f.slot(&self.shapes, 3)) {
            twin.set_slot(&self.shapes, 1, Value::UNDEFINED);
        }
        let i = f.slot(&self.shapes, 2).as_int().expect("an index") as u32;
        let v = match settled {
            Some(ok) => self.settled_record(ok, x),
            None => x,
        };
        let values = Realm::obj(st.get(S_VALUES)).expect("the results");
        values.set_element(&mut self.heap, i, v).ok_or_else(|| self.error(ErrKind::Range, "out of memory"))?;
        self.element_done(kind, st)?;
        Ok(Value::UNDEFINED)
    }

    /// `{ status, value }` or `{ status, reason }`.
    fn settled_record(&mut self, ok: bool, x: Value) -> Value {
        let o = Obj::new(&mut self.heap, &self.shapes, self.s_object).expect("memory");
        let status = self.string(if ok { "fulfilled" } else { "rejected" });
        for (k, v) in [("status", status), (if ok { "value" } else { "reason" }, x)] {
            let k = self.intern(k);
            self.define(o, Key::Name(k), v).expect("define");
        }
        o.value()
    }

    // ---- installation ----

    pub(crate) fn install_promise_statics(&mut self) {
        for (f, len) in [
            (all_element as NativeFn, 1),
            (settled_ok, 1),
            (settled_err, 1),
            (any_element, 1),
            (then_finally, 1),
            (catch_finally, 1),
            (value_thunk, 0),
            (thrower, 0),
        ] {
            self.natives.push(Native { f, ctor: false, len, name: "" });
        }
        let n = self.natives.len();
        for i in 0..N_IDS {
            self.pr.ids[i] = n - N_IDS + i;
        }
        let (c, proto) = (Realm::obj(self.pr.ctor).expect("Promise"), self.pr.proto);
        self.method(c, "all", 1, promise_all);
        self.method(c, "allSettled", 1, promise_all_settled);
        self.method(c, "any", 1, promise_any);
        self.method(c, "race", 1, promise_race);
        self.method(c, "withResolvers", 0, promise_with_resolvers);
        self.method(c, "try", 1, promise_try);
        self.method(proto, "finally", 1, promise_finally);
        let getter = self.new_native(species_get, false, 0, "get [Symbol.species]");
        self.pr.species_fn = getter.value();
        self.keep(getter.value());
        self.define_accessor(c, Key::Name(self.wk[SPECIES]), getter.value(), false, false).expect("define");
        let tag = self.string("Promise");
        self.define_attrs(proto, Key::Name(self.wk[crate::bi_symbol::TO_STRING_TAG]), crate::shapes::A_C, tag).expect("define");
    }

    /// `AggregateError`: the prototype inherits from `Error.prototype`, the constructor from `Error`.
    pub(crate) fn install_aggregate_error(&mut self, error: Obj) {
        let eshape = self.shapes.root(crate::cell::Layout::Object, Some(self.error_proto.cell()), 4);
        let proto = Obj::new(&mut self.heap, &self.shapes, eshape).expect("memory");
        let name = self.string("AggregateError");
        self.hidden(proto, "name", name);
        let empty = self.string("");
        self.hidden(proto, "message", empty);
        let c = self.ctor("AggregateError", 2, aggregate_ctor, proto);
        c.set_proto(&mut self.shapes, Some(error.cell()));
        self.pr.agg_proto = proto;
        self.keep(proto.value());
    }
}

// ---- the built-ins ----

fn promise_all(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    combinator(rt, this, args, Kind::All, "all")
}

fn promise_all_settled(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    combinator(rt, this, args, Kind::AllSettled, "allSettled")
}

fn promise_any(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    combinator(rt, this, args, Kind::Any, "any")
}

fn promise_race(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    combinator(rt, this, args, Kind::Race, "race")
}

fn combinator(rt: &mut Realm, this: Value, args: &[Value], kind: Kind, name: &str) -> R<Value> {
    if Realm::obj(this).is_none() {
        return rt.type_error(&format!("Promise.{name} called on a value that is not an object"));
    }
    let cap = rt.capability(this)?;
    let k = rt.intern("resolve");
    let resolve = match rt.get_prop(this, Key::Name(k)) {
        Ok(f) if Realm::obj(f).is_some_and(|o| o.is_callable()) => f,
        Ok(_) => {
            let e = rt.error(ErrKind::Type, "Promise resolve is not a function");
            return rt.abrupt(cap, e);
        }
        Err(e) => return rt.abrupt(cap, e),
    };
    let mut src = match rt.open_iter(arg(args, 0)) {
        Ok(s) => s,
        Err(e) => return rt.abrupt(cap, e),
    };
    match rt.combine(kind, this, cap, resolve, &mut src) {
        Ok(()) => Ok(cap.promise),
        Err(e) if e == STOP => Err(e),
        Err(e) => {
            rt.close_iter(&src);
            rt.abrupt(cap, e)
        }
    }
}

fn all_element(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    rt.element_call(Kind::All, arg(args, 0), None)
}

fn settled_ok(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    rt.element_call(Kind::AllSettled, arg(args, 0), Some(true))
}

fn settled_err(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    rt.element_call(Kind::AllSettled, arg(args, 0), Some(false))
}

fn any_element(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    rt.element_call(Kind::Any, arg(args, 0), None)
}

fn species_get(_rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    Ok(this)
}

fn promise_with_resolvers(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    if Realm::obj(this).is_none() {
        return rt.type_error("Promise.withResolvers called on a value that is not an object");
    }
    let cap = rt.capability(this)?;
    let o = Obj::new(&mut rt.heap, &rt.shapes, rt.s_object).expect("memory");
    for (k, v) in [("promise", cap.promise), ("resolve", cap.resolve), ("reject", cap.reject)] {
        let k = rt.intern(k);
        rt.define(o, Key::Name(k), v)?;
    }
    Ok(o.value())
}

/// `Promise.try(f, ...args)`: a throw is a rejection of a new capability, a result goes through `PromiseResolve(C, result)` (a promise of
/// `C` is not wrapped).
fn promise_try(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    if Realm::obj(this).is_none() {
        return rt.type_error("Promise.try called on a value that is not an object");
    }
    match rt.call_function(arg(args, 0), Value::UNDEFINED, args.get(1..).unwrap_or(&[])) {
        Ok(v) => rt.promise_resolve_c(this, v),
        Err(e) if e == STOP => Err(e),
        Err(e) => {
            let cap = rt.capability(this)?;
            rt.call_function(cap.reject, Value::UNDEFINED, &[e])?;
            Ok(cap.promise)
        }
    }
}

// ---- finally ----

fn promise_finally(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let Some(p) = Realm::obj(this) else { return rt.type_error("Promise.prototype.finally called on a value that is not an object") };
    let c = rt.species_constructor(p)?;
    let f = arg(args, 0);
    let (then_f, catch_f) = if Realm::obj(f).is_some_and(|o| o.is_callable()) {
        let (a, b) = (rt.native_cell(rt.pr.ids[N_THEN_FINALLY]), rt.native_cell(rt.pr.ids[N_CATCH_FINALLY]));
        for x in [a, b] {
            x.set_slot(&rt.shapes, 1, f);
            x.set_slot(&rt.shapes, 2, c);
        }
        (a.value(), b.value())
    } else {
        (f, f)
    };
    rt.invoke_then(this, &[then_f, catch_f])
}

/// `thenFinally(value)` and `catchFinally(reason)`: call `onFinally`, resolve its result with the constructor, and then give the value
/// back (or throw the reason).
fn finally_call(rt: &mut Realm, x: Value, id: usize) -> R<Value> {
    let f = Realm::obj(rt.callee).expect("a finally function");
    let (on_finally, c) = (f.slot(&rt.shapes, 1), f.slot(&rt.shapes, 2));
    let result = rt.call_function(on_finally, Value::UNDEFINED, &[])?;
    let p = rt.promise_resolve_c(c, result)?;
    let thunk = rt.native_cell(rt.pr.ids[id]);
    thunk.set_slot(&rt.shapes, 1, x);
    rt.invoke_then(p, &[thunk.value()])
}

fn then_finally(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    finally_call(rt, arg(args, 0), N_THUNK)
}

fn catch_finally(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    finally_call(rt, arg(args, 0), N_THROWER)
}

fn value_thunk(rt: &mut Realm, _this: Value, _args: &[Value]) -> R<Value> {
    Ok(Realm::obj(rt.callee).expect("a thunk").slot(&rt.shapes, 1))
}

fn thrower(rt: &mut Realm, _this: Value, _args: &[Value]) -> R<Value> {
    Err(Realm::obj(rt.callee).expect("a thrower").slot(&rt.shapes, 1))
}

// ---- AggregateError ----

fn aggregate_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let proto = rt.new_proto()?;
    let m = arg(args, 1);
    let msg = if m.is_undefined() { None } else { Some(rt.to_str(m)?.value()) };
    let e = rt.make_error(proto, msg);
    if let Some(opts) = Realm::obj(arg(args, 2)) {
        let k = rt.intern("cause");
        if rt.has_prop(opts.value(), Key::Name(k))? {
            let c = rt.get_prop(opts.value(), Key::Name(k))?;
            rt.hidden(Realm::obj(e).expect("an error"), "cause", c);
        }
    }
    let list = rt.iterable_to_list(arg(args, 0))?;
    let errors = rt.new_array(&list);
    rt.hidden(Realm::obj(e).expect("an error"), "errors", errors);
    Ok(e)
}
