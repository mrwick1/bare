// `Iterator`, `Iterator.from` and the iterator helpers (ES2025). Included by `eval.rs` (`include!`), so it shares that
// module's imports and private items. The iterator protocol (`It`, `iter_open`, `iter_next`, `iter_close`) is in
// `eval_symbols.rs`.
//
// Supported: the abstract constructor `Iterator` (a subclass works, `new Iterator()` throws), `Iterator.from`,
// `Iterator.prototype` with `map`, `filter`, `take`, `drop`, `flatMap` (lazy: they return an iterator helper),
// `reduce`, `toArray`, `forEach`, `some`, `every`, `find`, `@@iterator`, and the accessors `constructor` and
// `@@toStringTag` (with the setter of the spec, which ignores the prototype itself). `%IteratorPrototype%` is
// `Iterator.prototype`, so Array, String, Map, Set, generator and helper iterators all inherit the methods.
//
// Order of checks follows Node 22 (V8 12.4): the receiver must be an object, then the callable is checked, then
// `next` is read once (GetIteratorDirect). A bad argument does not close the receiver (later spec versions do).
// V8 12.4 names the method, not the value, when a callable is missing (`string "Iterator.prototype.map" is not a
// function`). The messages copy it.
//
// Design
// - An iterator helper is a small state machine in `sc.helpers` (one `Helper` of 136 bytes per helper object), not
//   a generator on the async machine. The states are the spec's: suspended-start, suspended-yield, executing and
//   completed. A reentrant `next` throws "Generator is already running". A throw inside the helper completes it.
// - A helper steps its source through `iter_next`, so a source that is an array, Map, Set, generator or another helper
//   needs no result object per value. `for-of`, spread and the consumers step a helper through `iter_native_step`
//   the same way. A chain `a.values().map(f).filter(g).take(n)` read by `for-of` makes no result object at all.
//   Reading `next()` by hand makes the one result object the spec requires.
// - `flatMap` opens its inner iterable with `iter_open`, so an inner array or string uses the same fast paths.
// - Closing follows the spec. `take` closes the source when its count is reached (on the next `next()`), `return()`
//   closes the inner and then the outer iterator, a throwing callback closes the source and rethrows its own error,
//   and an error of `next` of the source never closes it.
// - `Iterator.from` returns an iterator that already inherits from `Iterator.prototype`, else a wrapper object
//   (`%WrapForValidIteratorPrototype%`) that forwards `next` and `return`.
//
// GC contract: a helper keeps its source, `next`, callback and inner iteration alive. `sc_trace` traces them and
// `sc_prune` drops the entries of dead helpers. Helpers count in the `max_iterators` cap with the other iterators.

#[derive(Clone, Copy, PartialEq, Eq)]
enum HKind {
    Map,
    Filter,
    Take,
    Drop,
    FlatMap,
    /// The wrapper of `Iterator.from`.
    Wrap,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HState {
    Start,
    Yield,
    Running,
    Done,
}

/// One iterator helper (or wrapper): the source, its `next`, the callback and what the kind needs to go on.
#[derive(Clone, Copy)]
struct Helper {
    kind: HKind,
    state: HState,
    iter: Value,
    next: Value,
    f: Value,
    /// `map`, `filter`, `flatMap`: the counter. `take`, `drop`: the count left (infinity stays infinity).
    n: f64,
    /// `flatMap`: the inner iteration that is running.
    inner: Option<It>,
}

/// What an `It` keeps alive.
fn it_trace(it: &It, work: &mut Vec<Value>) {
    work.push(it.src);
    match it.kind {
        ItKind::Plain => {}
        ItKind::Coll { owner, .. } => work.push(Value::Object(owner)),
        ItKind::Generic { iter, next } => work.extend([iter, next]),
    }
}

/// An iteration over an iterator object whose `next` is known (GetIteratorDirect).
fn it_direct(iter: Value, next: Value) -> It {
    It { src: iter, i: 0, kind: ItKind::Generic { iter, next }, done: false }
}

impl ScFn {
    fn is_iter(&self) -> bool {
        use ScFn::*;
        matches!(
            self,
            IterCtor | IterFrom | IterMap | IterFilter | IterTake | IterDrop | IterFlatMap | IterReduce | IterToArray | IterForEach | IterSome | IterEvery | IterFind | IterHelperNext | IterHelperReturn | IterWrapNext | IterWrapReturn | IterCtorGet | IterCtorSet | IterTagGet | IterTagSet
        )
    }
}

impl<'p> Interp<'p> {
    // ------------------------------------------------------------ install

    fn sc_accessor(&mut self, on: ObjId, key: PropertyKey, get: (&str, ScFn), set: (&str, ScFn)) -> Res<()> {
        let g = self.sc_native(get.1, get.0, 0)?;
        let s = self.sc_native(set.1, set.0, 1)?;
        let d = PropertyDescriptor { get: Some(Some(g)), set: Some(Some(s)), enumerable: Some(false), configurable: Some(true), ..Default::default() };
        h!(self, self.heap.define_own_property(on, key, &d));
        Ok(())
    }

    /// `Iterator`, `Iterator.from`, the methods of `Iterator.prototype` and the two helper prototypes. The array,
    /// string, Map, Set and generator iterator prototypes already inherit from `sc.iter_proto`.
    fn install_iterator(&mut self) -> Res<()> {
        let ip = self.sc.iter_proto;
        let ctor = self.sc_native(ScFn::IterCtor, "Iterator", 0)?;
        self.sc.iter_ctor = ctor;
        self.def(ctor, self.k_prototype, Value::Object(ip), false, false, false)?;
        let k = h!(self, self.heap.key_from_str("Iterator"));
        self.def(self.global, k, Value::Object(ctor), true, false, true)?;
        self.sc_method(ctor, "from", ScFn::IterFrom, 1)?;
        self.sc_accessor(ip, self.k_constructor, ("get constructor", ScFn::IterCtorGet), ("set constructor", ScFn::IterCtorSet))?;
        for (n, f, l) in [
            ("reduce", ScFn::IterReduce, 1),
            ("toArray", ScFn::IterToArray, 0),
            ("forEach", ScFn::IterForEach, 1),
            ("some", ScFn::IterSome, 1),
            ("every", ScFn::IterEvery, 1),
            ("find", ScFn::IterFind, 1),
            ("map", ScFn::IterMap, 1),
            ("filter", ScFn::IterFilter, 1),
            ("take", ScFn::IterTake, 1),
            ("drop", ScFn::IterDrop, 1),
            ("flatMap", ScFn::IterFlatMap, 1),
        ] {
            self.sc_method(ip, n, f, l)?;
        }
        self.sc_accessor(ip, self.sc.k_to_string_tag, ("get [Symbol.toStringTag]", ScFn::IterTagGet), ("set [Symbol.toStringTag]", ScFn::IterTagSet))?;
        let hp = h!(self, self.heap.new_object(Some(ip)));
        self.sc.helper_proto = hp;
        self.sc.f_helper_next = self.sc_method(hp, "next", ScFn::IterHelperNext, 0)?;
        self.sc_method(hp, "return", ScFn::IterHelperReturn, 0)?;
        self.sc_tag(hp, "Iterator Helper")?;
        let wp = h!(self, self.heap.new_object(Some(ip)));
        self.sc.wrap_proto = wp;
        self.sc_method(wp, "next", ScFn::IterWrapNext, 0)?;
        self.sc_method(wp, "return", ScFn::IterWrapReturn, 0)?;
        Ok(())
    }

    // ------------------------------------------------------------ dispatch

    fn call_iter(&mut self, f: ScFn, this: Value, args: &[Value], is_new: bool) -> Res<Value> {
        let (a0, a1) = (arg(args, 0), arg(args, 1));
        match f {
            ScFn::IterCtor => {
                if !is_new {
                    return Err(self.type_err("Constructor Iterator requires 'new'"));
                }
                // `construct_nt_inner` handles a subclass. A direct `new Iterator()` ends here.
                Err(self.type_err("Abstract class Iterator not directly constructable"))
            }
            ScFn::IterFrom => self.iter_from(a0),
            ScFn::IterMap | ScFn::IterFilter | ScFn::IterFlatMap => {
                let (kind, name) = match f {
                    ScFn::IterMap => (HKind::Map, "map"),
                    ScFn::IterFilter => (HKind::Filter, "filter"),
                    _ => (HKind::FlatMap, "flatMap"),
                };
                let (iter, next) = self.it_receiver(this, name, Some(a0))?;
                self.helper_new(kind, self.sc.helper_proto, iter, next, a0, 0.0)
            }
            ScFn::IterTake | ScFn::IterDrop => {
                let (kind, name) = if f == ScFn::IterTake { (HKind::Take, "take") } else { (HKind::Drop, "drop") };
                self.it_this(this, name)?;
                let n = self.coerce_number(a0)?;
                let n = n.trunc();
                if n.is_nan() || n < 0.0 {
                    let t = self.display(a0);
                    return Err(self.mk_err(RANGE_ERROR, &format!("{t} must be positive")));
                }
                let (iter, next) = self.it_receiver(this, name, None)?;
                self.helper_new(kind, self.sc.helper_proto, iter, next, Value::Undefined, n)
            }
            ScFn::IterReduce => {
                let (iter, next) = self.it_receiver(this, "reduce", Some(a0))?;
                let mut it = it_direct(iter, next);
                let (mut acc, mut c) = if args.len() >= 2 {
                    (a1, 0.0)
                } else {
                    match self.iter_next(&mut it)? {
                        Some(v) => (v, 1.0),
                        None => return Err(self.type_err("Reduce of a done iterator with no initial value")),
                    }
                };
                while let Some(v) = self.iter_next(&mut it)? {
                    match self.call_fn(a0, Value::Undefined, &[acc, v, Value::Number(c)]) {
                        Ok(r) => acc = r,
                        Err(e) => return Err(self.iter_close_throw(&mut it, e)),
                    }
                    c += 1.0;
                }
                Ok(acc)
            }
            ScFn::IterToArray => {
                let (iter, next) = self.it_receiver(this, "toArray", None)?;
                let mut it = it_direct(iter, next);
                let mut out = Vec::new();
                while let Some(v) = self.iter_next(&mut it)? {
                    self.iter_buffer_room(out.len())?;
                    out.push(v);
                }
                self.new_array_from(&out)
            }
            ScFn::IterForEach | ScFn::IterSome | ScFn::IterEvery | ScFn::IterFind => {
                let name = match f {
                    ScFn::IterForEach => "forEach",
                    ScFn::IterSome => "some",
                    ScFn::IterEvery => "every",
                    _ => "find",
                };
                let (iter, next) = self.it_receiver(this, name, Some(a0))?;
                let mut it = it_direct(iter, next);
                let mut c = 0.0;
                while let Some(v) = self.iter_next(&mut it)? {
                    let r = match self.call_fn(a0, Value::Undefined, &[v, Value::Number(c)]) {
                        Ok(r) => r,
                        Err(e) => return Err(self.iter_close_throw(&mut it, e)),
                    };
                    c += 1.0;
                    let t = match f {
                        ScFn::IterForEach => continue,
                        _ => self.truthy(r)?,
                    };
                    match (f, t) {
                        (ScFn::IterSome, true) => {
                            self.iter_close(&mut it)?;
                            return Ok(Value::Bool(true));
                        }
                        (ScFn::IterEvery, false) => {
                            self.iter_close(&mut it)?;
                            return Ok(Value::Bool(false));
                        }
                        (ScFn::IterFind, true) => {
                            self.iter_close(&mut it)?;
                            return Ok(v);
                        }
                        _ => {}
                    }
                }
                Ok(match f {
                    ScFn::IterSome => Value::Bool(false),
                    ScFn::IterEvery => Value::Bool(true),
                    _ => Value::Undefined,
                })
            }
            ScFn::IterHelperNext => {
                let Value::Object(o) = this else { return Err(self.helper_incompatible("next", this)) };
                match self.helper_step(o)? {
                    None => self.sc_iter_result(Value::Undefined, true),
                    Some(v) => self.sc_iter_result(v, false),
                }
            }
            ScFn::IterHelperReturn => self.helper_return(this),
            ScFn::IterWrapNext | ScFn::IterWrapReturn => {
                let name = if f == ScFn::IterWrapNext { "next" } else { "return" };
                let h = match this {
                    Value::Object(o) => self.sc.helpers.get(&o).filter(|h| h.kind == HKind::Wrap).copied(),
                    _ => None,
                };
                let Some(h) = h else { return Err(self.type_err(&format!("Method %WrapForValidIteratorPrototype%.{name} called on incompatible receiver {}", self.display(this)))) };
                if f == ScFn::IterWrapNext {
                    return self.call_fn(h.next, h.iter, &[]);
                }
                let m = self.get_prop(h.iter, self.sc.k_return)?;
                if is_nullish(m) {
                    return self.sc_iter_result(Value::Undefined, true);
                }
                if !self.is_callable(m) {
                    return Err(self.type_err("iterator.return is not a function"));
                }
                self.call_fn(m, h.iter, &[])
            }
            ScFn::IterCtorGet => Ok(Value::Object(self.sc.iter_ctor)),
            ScFn::IterTagGet => Ok(Value::String(h!(self, self.heap.intern_str("Iterator")))),
            ScFn::IterCtorSet | ScFn::IterTagSet => {
                // SetterThatIgnoresPrototypeProperties(this, %Iterator.prototype%, p, v)
                let (key, text) = if f == ScFn::IterCtorSet { (self.k_constructor, "constructor") } else { (self.sc.k_to_string_tag, "Symbol(Symbol.toStringTag)") };
                let Value::Object(o) = this else {
                    let what = if f == ScFn::IterCtorSet { "constructor" } else { "[Symbol.toStringTag]" };
                    return Err(self.type_err(&format!("set Iterator.prototype{}{what} called on non-object", if f == ScFn::IterCtorSet { "." } else { "" })));
                };
                if o == self.sc.iter_proto {
                    return Err(self.type_err(&format!("Cannot assign to read only property '{text}' of object '[object Object]'")));
                }
                if h!(self, self.heap.get_own_property(o, key)).is_none() {
                    self.def(o, key, a0, true, true, true)?;
                } else {
                    self.put_prop(this, key, a0)?;
                }
                Ok(Value::Undefined)
            }
            _ => unreachable!("not an iterator function"),
        }
    }

    // ------------------------------------------------------------ argument checks

    fn it_this(&mut self, this: Value, name: &str) -> Res<()> {
        if matches!(this, Value::Object(_)) {
            return Ok(());
        }
        Err(self.type_err(&format!("Iterator.prototype.{name} called on non-object")))
    }

    /// The checks every method makes, in the order of Node 22: `this` is an object, the callable (when the method has
    /// one) is callable, then `next` is read once.
    fn it_receiver(&mut self, this: Value, name: &str, callable: Option<Value>) -> Res<(Value, Value)> {
        self.it_this(this, name)?;
        if let Some(f) = callable
            && !self.is_callable(f)
        {
            return Err(self.type_err(&format!("string \"Iterator.prototype.{name}\" is not a function")));
        }
        let next = self.get_prop(this, self.sc.k_next)?;
        Ok((this, next))
    }

    fn helper_incompatible(&mut self, name: &str, this: Value) -> Error {
        let t = self.display(this);
        self.type_err(&format!("Method Iterator Helper.prototype.{name} called on incompatible receiver {t}"))
    }

    // ------------------------------------------------------------ Iterator.from and the helpers

    /// `GetIteratorFlattenable(obj, iterate-strings)` as an iterator object and its `next`. `Iterator.from` needs the
    /// object itself, so no fast path.
    fn iter_flattenable(&mut self, obj: Value, who: &str) -> Res<(Value, Value)> {
        if !matches!(obj, Value::Object(_) | Value::String(_)) {
            return Err(self.type_err(&format!("{who} called on non-object")));
        }
        let m = self.get_prop(obj, self.sc.k_iterator)?;
        let iter = if is_nullish(m) {
            obj
        } else {
            if !self.is_callable(m) {
                return Err(self.type_err("Symbol.iterator is not a function"));
            }
            self.call_fn(m, obj, &[])?
        };
        if !matches!(iter, Value::Object(_)) {
            return Err(self.type_err("Result of the Symbol.iterator method is not an object"));
        }
        let next = self.get_prop(iter, self.sc.k_next)?;
        Ok((iter, next))
    }

    fn iter_from(&mut self, o: Value) -> Res<Value> {
        let (iter, next) = self.iter_flattenable(o, "Iterator.from")?;
        if self.ordinary_has_instance(Value::Object(self.sc.iter_ctor), iter)? {
            return Ok(iter);
        }
        self.helper_new(HKind::Wrap, self.sc.wrap_proto, iter, next, Value::Undefined, 0.0)
    }

    fn helper_new(&mut self, kind: HKind, proto: ObjId, iter: Value, next: Value, f: Value, n: f64) -> Res<Value> {
        if self.sc.iters.len() + self.sc.helpers.len() >= self.sc.limits.max_iterators {
            return Err(Error::Limit(LimitKind::Sc(ScLimit::Iterators)));
        }
        let o = h!(self, self.heap.new_object(Some(proto)));
        self.sc.helpers.insert(o, Helper { kind, state: HState::Start, iter, next, f, n, inner: None });
        Ok(Value::Object(o))
    }

    /// `next` of a helper: the next value, or `None` when it is done. A helper that is running throws.
    fn helper_step(&mut self, o: ObjId) -> Res<Option<Value>> {
        let st = match self.sc.helpers.get_mut(&o) {
            Some(h) if h.kind != HKind::Wrap => std::mem::replace(&mut h.state, HState::Running),
            _ => return Err(self.helper_incompatible("next", Value::Object(o))),
        };
        match st {
            HState::Running => return Err(self.type_err("Generator is already running")),
            HState::Done => {
                if let Some(h) = self.sc.helpers.get_mut(&o) {
                    h.state = HState::Done;
                }
                return Ok(None);
            }
            _ => {}
        }
        let Some(mut h) = self.sc.helpers.get(&o).copied() else { return Ok(None) };
        let r = self.helper_run(&mut h);
        h.state = if matches!(r, Ok(Some(_))) { HState::Yield } else { HState::Done };
        if let Some(slot) = self.sc.helpers.get_mut(&o) {
            *slot = h;
        }
        r
    }

    /// The source's next value (a throw of `next` is not closed: the source is done).
    fn h_step(&mut self, h: &Helper) -> Res<Option<Value>> {
        let mut it = it_direct(h.iter, h.next);
        self.iter_next(&mut it)
    }

    /// IteratorClose of the source with a normal completion.
    fn h_close(&mut self, h: &Helper) -> Res<()> {
        let mut it = it_direct(h.iter, h.next);
        self.iter_close(&mut it)
    }

    /// IteratorClose of the source with a throw completion: `err` wins.
    fn h_close_throw(&mut self, h: &Helper, err: Error) -> Error {
        let mut it = it_direct(h.iter, h.next);
        self.iter_close_throw(&mut it, err)
    }

    /// Run the helper to its next value (`None`: it completed).
    fn helper_run(&mut self, h: &mut Helper) -> Res<Option<Value>> {
        match h.kind {
            HKind::Map => {
                let Some(v) = self.h_step(h)? else { return Ok(None) };
                let c = h.n;
                h.n += 1.0;
                match self.call_fn(h.f, Value::Undefined, &[v, Value::Number(c)]) {
                    Ok(m) => Ok(Some(m)),
                    Err(e) => Err(self.h_close_throw(h, e)),
                }
            }
            HKind::Filter => loop {
                let Some(v) = self.h_step(h)? else { return Ok(None) };
                let c = h.n;
                h.n += 1.0;
                let keep = self.call_fn(h.f, Value::Undefined, &[v, Value::Number(c)]).and_then(|r| self.truthy(r));
                match keep {
                    Ok(true) => return Ok(Some(v)),
                    Ok(false) => {}
                    Err(e) => return Err(self.h_close_throw(h, e)),
                }
            },
            HKind::Take => {
                if h.n == 0.0 {
                    self.h_close(h)?;
                    return Ok(None);
                }
                if h.n != f64::INFINITY {
                    h.n -= 1.0;
                }
                self.h_step(h)
            }
            HKind::Drop => {
                while h.n > 0.0 {
                    if h.n != f64::INFINITY {
                        h.n -= 1.0;
                    }
                    if self.h_step(h)?.is_none() {
                        return Ok(None);
                    }
                }
                self.h_step(h)
            }
            HKind::FlatMap => loop {
                if let Some(mut inner) = h.inner {
                    match self.iter_next(&mut inner) {
                        Ok(Some(v)) => {
                            h.inner = Some(inner);
                            return Ok(Some(v));
                        }
                        Ok(None) => h.inner = None,
                        Err(e) => return Err(self.h_close_throw(h, e)),
                    }
                }
                let Some(v) = self.h_step(h)? else { return Ok(None) };
                let c = h.n;
                h.n += 1.0;
                let inner = self.call_fn(h.f, Value::Undefined, &[v, Value::Number(c)]).and_then(|m| self.flat_inner(m));
                match inner {
                    Ok(it) => h.inner = Some(it),
                    Err(e) => return Err(self.h_close_throw(h, e)),
                }
            },
            HKind::Wrap => unreachable!("a wrapper is not stepped"),
        }
    }

    /// `GetIteratorFlattenable(mapped, reject-strings)` for `flatMap`, with the fast paths of `iter_open`.
    fn flat_inner(&mut self, m: Value) -> Res<It> {
        if !matches!(m, Value::Object(_)) {
            return Err(self.type_err("Iterator.prototype.flatMap called on non-object"));
        }
        if let Some(it) = self.iter_open(m)? {
            return Ok(it);
        }
        let next = self.get_prop(m, self.sc.k_next)?;
        Ok(it_direct(m, next))
    }

    /// `return()` of a helper: close the inner iteration and the source, then complete.
    fn helper_return(&mut self, this: Value) -> Res<Value> {
        let o = match this {
            Value::Object(o) if self.sc.helpers.get(&o).is_some_and(|h| h.kind != HKind::Wrap) => o,
            _ => return Err(self.helper_incompatible("return", this)),
        };
        let Some(mut h) = self.sc.helpers.get(&o).copied() else { return self.sc_iter_result(Value::Undefined, true) };
        let st = h.state;
        if st == HState::Running {
            return Err(self.type_err("Generator is already running"));
        }
        // Executing while the iterators close, completed after.
        if let Some(slot) = self.sc.helpers.get_mut(&o) {
            slot.state = if st == HState::Start { HState::Done } else { HState::Running };
        }
        let r = (|| -> Res<()> {
            match st {
                HState::Start => self.h_close(&h),
                HState::Yield => {
                    if let Some(mut inner) = h.inner.take()
                        && let Err(e) = self.iter_close(&mut inner)
                    {
                        return Err(self.h_close_throw(&h, e));
                    }
                    self.h_close(&h)
                }
                _ => Ok(()),
            }
        })();
        if let Some(slot) = self.sc.helpers.get_mut(&o) {
            slot.state = HState::Done;
            slot.inner = None;
        }
        r?;
        self.sc_iter_result(Value::Undefined, true)
    }
}
