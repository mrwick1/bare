// Ordinary Array and Object methods and a small Math set. The Array methods of batch 1 (reverse, at, flat, flatMap, findLast,
// findLastIndex, lastIndexOf, reduceRight, copyWithin, toReversed, toSorted, toSpliced, with, toLocaleString) are in
// `eval_array_more.rs`; `call_ao` forwards them. `sort` and `toSorted` share `ao_merge_sort`. Included by `eval.rs` (`include!`), so it shares that
// module's imports and private items. Every native here is `Nat::Ao(AoFn)`; `call_ao` is the one dispatch hook.
//
// Supported
// - Array.prototype: map, filter, find, findIndex, splice, reduce, some, every, slice, includes, indexOf, concat,
//   shift, unshift, sort, fill. Object: assign, entries, values, fromEntries. Array: from, of.
//   Math: abs, floor, round, trunc, max, min.
// - Receivers are generic array-likes, as in the spec: `length` is read once (ToLength), holes and inherited indices
//   follow HasProperty, every write is a strict `Set` or `DeletePropertyOrThrow`, results are made with
//   CreateDataPropertyOrThrow. Callbacks run through `call_fn` with `(value, index, receiver)`.
// - `sort` is stable (merge sort), puts `undefined` last, skips holes, and stops at the first error: no comparator
//   runs after a throw, and the receiver is untouched until the sort has finished.
//
// Symbols (see `eval_symbols.rs`)
// - `map`, `filter`, `slice`, `splice` and `concat` read `constructor` and then `constructor[@@species]`, as ArraySpeciesCreate
//   does. `concat` reads `@@isConcatSpreadable` of every item. `Array.from` and `Object.fromEntries` use the shared
//   iterator machinery (`iter_open`, `iter_next`, `iter_close_throw`): the iterator is closed when the map function
//   or the element creation throws. `Object.assign` copies enumerable symbol keys; `values` and `entries` skip them.
//
// Species: the intrinsic `Array`, `undefined` and `null` give a plain array. Any other constructor (a subclass, a custom
// `constructor` or `@@species`) is called with the length. `flat` and `flatMap` need an array from it (else `Unsupported`).
// A value that is not a constructor is a `TypeError`. `Array.from` and `Array.of` build their result with `new this(...)`
// when `this` is a constructor other than `Array`.
// A primitive receiver is made a wrapper object first (`ao_recv`, ToObject): `Array.prototype.map.call("ab", f)` passes a String
// wrapper as the third argument, `Array.prototype.fill.call(1, 0)` returns a Number object and `Object.assign(1, {a: 2})` too.
// `concat` throws its RangeError (result longer than 2^32-1) before it reads the elements, not after.
//
// Limits (`Error::Limit`, never a JavaScript value): every element visit costs a step. A traversal that must
// visit every index fails with `Steps` before it starts when the range is longer than the remaining steps, so a
// sparse `length` of 2^32 does no work. Temporary storage (sort, entries, values) is capped at a quarter of the heap
// byte limit before it grows.

/// State of this module that GC must trace.
#[derive(Default)]
struct AoState {
    array_ctor: Option<ObjId>,
    number_proto: Option<ObjId>,
    rng: Option<[u64; 2]>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AoFn {
    ArrMap,
    ArrFilter,
    ArrFind,
    ArrFindIndex,
    ArrSplice,
    ArrReduce,
    ArrSome,
    ArrEvery,
    ArrSlice,
    ArrIncludes,
    ArrIndexOf,
    ArrConcat,
    ArrShift,
    ArrUnshift,
    ArrSort,
    ArrFill,
    ObjAssign,
    ObjEntries,
    ObjValues,
    ObjFromEntries,
    ArrFrom,
    ArrOf,
    MathAbs,
    MathFloor,
    MathRound,
    MathTrunc,
    MathMax,
    MathMin,
    MathClz32,
    MathRandom,
    NumToString,
    NumValueOf,
    NumIsNaN,
    /// A one-argument `Math` function: the index into `MATH_UNARY`.
    MathUnary(u8),
    MathAtan2,
    MathPow,
    MathImul,
    MathHypot,
    NumIsFinite,
    NumIsInteger,
    NumIsSafeInteger,
    NumToFixed,
    NumToExponential,
    NumToPrecision,
    ArrReverse,
    ArrAt,
    ArrFlat,
    ArrFlatMap,
    ArrFindLast,
    ArrFindLastIndex,
    ArrLastIndexOf,
    ArrReduceRight,
    ArrCopyWithin,
    ArrToReversed,
    ArrToSorted,
    ArrToSpliced,
    ArrWith,
    ArrToLocaleString,
}

const MAX_SAFE_LEN: u64 = 9007199254740991;

impl<'p> Interp<'p> {
    fn install_array_object(&mut self) -> Res<()> {
        let (op, ap, g) = (self.object_proto, self.array_proto, self.global);
        let Get::Value(Value::Object(ac)) = h!(self, self.heap.get(ap, self.k_constructor)) else { return Err(unsupported("Array constructor missing", Span::default())) };
        let Get::Value(Value::Object(oc)) = h!(self, self.heap.get(op, self.k_constructor)) else { return Err(unsupported("Object constructor missing", Span::default())) };
        self.ao.array_ctor = Some(ac);
        use AoFn::*;
        for (name, f, len) in [
            ("map", ArrMap, 1), ("filter", ArrFilter, 1), ("find", ArrFind, 1), ("findIndex", ArrFindIndex, 1), ("splice", ArrSplice, 2),
            ("reduce", ArrReduce, 1), ("some", ArrSome, 1), ("every", ArrEvery, 1), ("slice", ArrSlice, 2), ("includes", ArrIncludes, 1),
            ("indexOf", ArrIndexOf, 1), ("concat", ArrConcat, 1), ("shift", ArrShift, 0), ("unshift", ArrUnshift, 1), ("sort", ArrSort, 1),
            ("fill", ArrFill, 1),
        ] {
            self.method(ap, name, Nat::Ao(f), len)?;
        }
        for (name, f, len) in [("assign", ObjAssign, 2), ("entries", ObjEntries, 1), ("values", ObjValues, 1), ("fromEntries", ObjFromEntries, 1)] {
            self.method(oc, name, Nat::Ao(f), len)?;
        }
        self.sc_getter(ac, self.wk_key(WK_SPECIES), "get [Symbol.species]", ScFn::Species)?;
        self.install_array_more(ap)?;
        self.method(ac, "from", Nat::Ao(ArrFrom), 1)?;
        self.method(ac, "of", Nat::Ao(ArrOf), 0)?;
        let math = h!(self, self.heap.new_object(Some(op)));
        for (name, f, len) in [("abs", MathAbs, 1), ("floor", MathFloor, 1), ("round", MathRound, 1), ("trunc", MathTrunc, 1), ("max", MathMax, 2), ("min", MathMin, 2)] {
            self.method(math, name, Nat::Ao(f), len)?;
        }
        self.install_numeric(math)?;
        let k = h!(self, self.heap.key_from_str("Math"));
        self.def(g, k, Value::Object(math), true, false, true)
    }

    fn call_ao(&mut self, f: AoFn, this: Value, args: &[Value]) -> Res<Value> {
        use AoFn::*;
        let (a0, a1) = (arg(args, 0), arg(args, 1));
        match f {
            ArrMap | ArrFilter | ArrSome | ArrEvery | ArrFind | ArrFindIndex => {
                let this = self.ao_recv(this)?;
                self.ao_iterate(f, this, a0, a1)
            }
            ArrReduce => {
                let this = self.ao_recv(this)?;
                self.ao_reduce(this, args)
            }
            ArrSplice => {
                let this = self.ao_recv(this)?;
                self.ao_splice(this, args)
            }
            ArrSlice => {
                let this = self.ao_recv(this)?;
                self.ao_slice(this, a0, a1)
            }
            ArrIncludes | ArrIndexOf => {
                let this = self.ao_recv(this)?;
                self.ao_search(f == ArrIncludes, this, a0, a1)
            }
            ArrConcat => {
                let this = self.ao_recv(this)?;
                self.ao_concat(this, args)
            }
            ArrShift => {
                let this = self.ao_recv(this)?;
                self.ao_shift(this)
            }
            ArrUnshift => {
                let this = self.ao_recv(this)?;
                self.ao_unshift(this, args)
            }
            ArrSort => {
                let this = self.ao_recv(this)?;
                self.ao_sort(this, a0)
            }
            ArrFill => {
                let this = self.ao_recv(this)?;
                self.ao_fill(this, a0, a1, arg(args, 2))
            }
            ObjAssign => self.ao_assign(args),
            ObjEntries => self.ao_enum_own(a0, true),
            ObjValues => self.ao_enum_own(a0, false),
            ObjFromEntries => self.ao_from_entries(a0),
            ArrFrom => self.ao_array_from(this, a0, a1, arg(args, 2)),
            ArrOf => {
                let (a, custom) = self.ao_from_target(this, &[Value::Number(args.len() as f64)], 0)?;
                if !custom {
                    return self.new_array_from(args);
                }
                for (i, &v) in args.iter().enumerate() {
                    let k = self.index_key(i as u64)?;
                    self.ao_create(a, k, v)?;
                }
                self.ao_set_len(Value::Object(a), args.len() as u64)?;
                Ok(Value::Object(a))
            }
            MathAbs | MathFloor | MathRound | MathTrunc => {
                let x = self.coerce_number(a0)?;
                Ok(Value::Number(match f {
                    MathAbs => x.abs(),
                    MathFloor => x.floor(),
                    MathTrunc => x.trunc(),
                    _ => math_round(x),
                }))
            }
            ArrReverse | ArrAt | ArrFlat | ArrFlatMap | ArrFindLast | ArrFindLastIndex | ArrLastIndexOf | ArrReduceRight | ArrCopyWithin | ArrToReversed | ArrToSorted | ArrToSpliced | ArrWith | ArrToLocaleString => {
                let this = self.ao_recv(this)?;
                self.call_array_more(f, this, args)
            }
            MathClz32 | MathRandom | NumToString | NumValueOf | NumIsNaN | MathUnary(_) | MathAtan2 | MathPow | MathImul | MathHypot | NumIsFinite | NumIsInteger | NumIsSafeInteger | NumToFixed | NumToExponential | NumToPrecision => self.call_numeric(f, this, args),
            MathMax | MathMin => {
                let is_max = f == MathMax;
                let mut r = if is_max { f64::NEG_INFINITY } else { f64::INFINITY };
                let mut nan = false;
                for &a in args {
                    let x = self.coerce_number(a)?; // every argument is converted, even after a NaN
                    nan |= x.is_nan();
                    let better = if is_max { x > r || (x == 0.0 && r == 0.0 && !x.is_sign_negative()) } else { x < r || (x == 0.0 && r == 0.0 && x.is_sign_negative()) };
                    if better {
                        r = x;
                    }
                }
                Ok(Value::Number(if nan { f64::NAN } else { r }))
            }
        }
    }

    // ------------------------------------------------------------ helpers

    fn ao_this(&mut self, this: Value, name: &str) -> Res<()> {
        if is_nullish(this) {
            return Err(self.type_err(&format!("Array.prototype.{name} called on null or undefined")));
        }
        Ok(())
    }

    fn ao_callable(&mut self, f: Value) -> Res<()> {
        if self.is_callable(f) {
            return Ok(());
        }
        let t = self.display(f);
        Err(self.type_err(&format!("{t} is not a function")))
    }

    /// Fail with `Steps` before a traversal of `n` elements that cannot stop early and would not fit the budget.
    fn ao_reserve(&mut self, n: u64) -> Res<()> {
        if n > self.step_limit.saturating_sub(self.steps) {
            return Err(Error::Limit(LimitKind::Steps));
        }
        Ok(())
    }

    /// Check temporary storage of `count` items of `each` bytes against a quarter of the heap byte limit.
    fn ao_temp(&mut self, count: usize, each: usize) -> Res<()> {
        if count.saturating_mul(each) > self.heap.limits().max_bytes / 4 {
            return Err(Error::Limit(LimitKind::Heap(Limit::Bytes)));
        }
        Ok(())
    }

    /// ToIntegerOrInfinity.
    fn ao_int(&mut self, v: Value) -> Res<f64> {
        let n = self.coerce_number(v)?;
        Ok(if n.is_nan() { 0.0 } else { n.trunc() + 0.0 })
    }

    /// A relative index (`slice`, `splice`) clamped to `0..=len`. `undefined` gives `default`.
    fn ao_rel(&mut self, v: Value, len: u64, default: u64) -> Res<u64> {
        if matches!(v, Value::Undefined) {
            return Ok(default);
        }
        let n = self.ao_int(v)?;
        Ok(if n < 0.0 { (len as f64 + n).max(0.0) as u64 } else { n.min(len as f64) as u64 })
    }

    /// Start of `includes` / `indexOf`: `None` when there is nothing to search.
    fn ao_from_index(&mut self, v: Value, len: u64) -> Res<Option<u64>> {
        let n = self.ao_int(v)?;
        Ok(if n >= len as f64 { None } else if n >= 0.0 { Some(n as u64) } else { Some((len as f64 + n).max(0.0) as u64) })
    }

    fn ao_has(&mut self, this: Value, k: PropertyKey) -> Res<bool> {
        Ok(match this {
            Value::Object(o) => self.o_has_plain(o, k)?,
            _ => false,
        })
    }

    /// Strict `Set`.
    fn ao_set(&mut self, this: Value, k: PropertyKey, v: Value) -> Res<()> {
        let saved = std::mem::replace(&mut self.strict, true);
        let r = self.put_prop(this, k, v);
        self.strict = saved;
        r
    }

    fn ao_set_len(&mut self, this: Value, n: u64) -> Res<()> {
        self.ao_set(this, self.k_length, Value::Number(n as f64))
    }

    /// `DeletePropertyOrThrow`.
    fn ao_delete(&mut self, this: Value, k: PropertyKey) -> Res<()> {
        let ok = match this {
            Value::Object(o) => self.o_delete(o, k)?,
            _ => true,
        };
        if ok {
            return Ok(());
        }
        let t = self.key_text(k);
        Err(self.type_err(&format!("Cannot delete property '{t}' of [object Array]")))
    }

    /// `CreateDataPropertyOrThrow` on an object this module made.
    fn ao_create(&mut self, a: ObjId, k: PropertyKey, v: Value) -> Res<()> {
        if hxw!(self, self.heap.define_own_property(a, k, &PropertyDescriptor::data(v))) {
            return Ok(());
        }
        let t = self.key_text(k);
        Err(self.type_err(&format!("Cannot add property {t}, object is not extensible")))
    }

    fn ao_new_array(&mut self, len: u64) -> Res<ObjId> {
        if len > u32::MAX as u64 {
            return Err(self.mk_err(RANGE_ERROR, "Invalid array length"));
        }
        let a = h!(self, self.heap.new_array(Some(self.array_proto)));
        if len > 0 {
            hxw!(self, self.heap.set(a, self.k_length, Value::Number(len as f64)));
        }
        Ok(a)
    }

    /// ArraySpeciesCreate. `constructor` and then `constructor[@@species]` are read, so getters run in order.
    /// The intrinsic `Array` and an `undefined` or `null` species give a plain array. Any other constructor is called with `len`.
    #[inline(always)]
    fn ao_species(&mut self, this: Value, len: u64) -> Res<ObjId> {
        Ok(self.ao_species_new(this, len)?.0)
    }

    /// Like `ao_species`. The flag is true for a new plain array, false for the result of a species constructor.
    fn ao_species_new(&mut self, this: Value, len: u64) -> Res<(ObjId, bool)> {
        if let Value::Object(o) = this
            && self.o_is_array(o)?
        {
            let mut c = self.get_prop(this, self.k_constructor)?;
            if matches!(c, Value::Object(_)) {
                c = self.get_prop(c, self.wk_key(WK_SPECIES))?;
                if matches!(c, Value::Null) {
                    c = Value::Undefined;
                }
            }
            match c {
                Value::Object(co) if Some(co) == self.ao.array_ctor => {}
                Value::Object(co) if self.is_constructor(co) => return Ok((self.ao_construct(c, &[Value::Number(len as f64)])?, false)),
                Value::Undefined => {}
                _ => return Err(self.type_err("object.constructor[Symbol.species] is not a constructor")),
            }
        }
        Ok((self.ao_new_array(len)?, true))
    }

    fn ao_construct(&mut self, c: Value, args: &[Value]) -> Res<ObjId> {
        match self.construct(c, args, ExprId::from_index(0))? {
            Value::Object(o) => Ok(o),
            _ => unreachable!("construct gives an object"),
        }
    }

    /// The object `Array.from` and `Array.of` fill: `new this(...args)` when `this` is a constructor other than `Array`
    /// (then `true`: the caller sets `length`), else a plain array of `len` holes.
    fn ao_from_target(&mut self, this: Value, args: &[Value], len: u64) -> Res<(ObjId, bool)> {
        match this {
            Value::Object(o) if Some(o) != self.ao.array_ctor && self.is_constructor(o) => Ok((self.ao_construct(this, args)?, true)),
            _ => Ok((self.ao_new_array(len)?, false)),
        }
    }

    /// IsConcatSpreadable: a defined `@@isConcatSpreadable` decides, otherwise an array spreads.
    fn ao_spreadable(&mut self, v: Value) -> Res<bool> {
        let Value::Object(o) = v else { return Ok(false) };
        let s = self.get_prop(v, self.wk_key(WK_CONCAT_SPREADABLE))?;
        if !matches!(s, Value::Undefined) {
            return self.truthy(s);
        }
        self.o_is_array(o)
    }

    // ------------------------------------------------------------ callback methods

    /// map, filter, some, every, find, findIndex.
    fn ao_iterate(&mut self, f: AoFn, this: Value, cb: Value, this_arg: Value) -> Res<Value> {
        let name = match f {
            AoFn::ArrMap => "map",
            AoFn::ArrFilter => "filter",
            AoFn::ArrSome => "some",
            AoFn::ArrEvery => "every",
            AoFn::ArrFind => "find",
            _ => "findIndex",
        };
        self.ao_this(this, name)?;
        let len = self.length_of(this)?;
        self.ao_callable(cb)?;
        let out = match f {
            AoFn::ArrMap => Some(self.ao_species(this, len)?),
            AoFn::ArrFilter => Some(self.ao_species(this, 0)?),
            _ => None,
        };
        if matches!(f, AoFn::ArrMap | AoFn::ArrFilter) {
            self.ao_reserve(len)?;
        }
        let visit_holes = matches!(f, AoFn::ArrFind | AoFn::ArrFindIndex);
        let mut to = 0u64;
        for i in 0..len {
            self.tick()?;
            let k = self.index_key(i)?;
            if !visit_holes && !self.ao_has(this, k)? {
                continue;
            }
            let v = self.get_prop(this, k)?;
            let r = self.call_fn(cb, this_arg, &[v, Value::Number(i as f64), this])?;
            match f {
                AoFn::ArrMap => self.ao_create(out.unwrap(), k, r)?,
                AoFn::ArrFilter => {
                    if self.truthy(r)? {
                        let tk = self.index_key(to)?;
                        self.ao_create(out.unwrap(), tk, v)?;
                        to += 1;
                    }
                }
                AoFn::ArrSome if self.truthy(r)? => return Ok(Value::Bool(true)),
                AoFn::ArrEvery if !self.truthy(r)? => return Ok(Value::Bool(false)),
                AoFn::ArrFind if self.truthy(r)? => return Ok(v),
                AoFn::ArrFindIndex if self.truthy(r)? => return Ok(Value::Number(i as f64)),
                _ => {}
            }
        }
        Ok(match f {
            AoFn::ArrMap | AoFn::ArrFilter => Value::Object(out.unwrap()),
            AoFn::ArrSome | AoFn::ArrFind => if f == AoFn::ArrSome { Value::Bool(false) } else { Value::Undefined },
            AoFn::ArrEvery => Value::Bool(true),
            _ => Value::Number(-1.0),
        })
    }

    fn ao_reduce(&mut self, this: Value, args: &[Value]) -> Res<Value> {
        self.ao_this(this, "reduce")?;
        let len = self.length_of(this)?;
        let cb = arg(args, 0);
        self.ao_callable(cb)?;
        let mut k = 0u64;
        let mut acc = if args.len() >= 2 {
            args[1]
        } else {
            loop {
                if k >= len {
                    return Err(self.type_err("Reduce of empty array with no initial value"));
                }
                self.tick()?;
                let key = self.index_key(k)?;
                k += 1;
                if self.ao_has(this, key)? {
                    break self.get_prop(this, key)?;
                }
            }
        };
        while k < len {
            self.tick()?;
            let key = self.index_key(k)?;
            if self.ao_has(this, key)? {
                let v = self.get_prop(this, key)?;
                acc = self.call_fn(cb, Value::Undefined, &[acc, v, Value::Number(k as f64), this])?;
            }
            k += 1;
        }
        Ok(acc)
    }

    // ------------------------------------------------------------ copying and searching

    fn ao_slice(&mut self, this: Value, start: Value, end: Value) -> Res<Value> {
        self.ao_this(this, "slice")?;
        let len = self.length_of(this)?;
        let mut k = self.ao_rel(start, len, 0)?;
        let fin = self.ao_rel(end, len, len)?;
        let count = fin.saturating_sub(k);
        let a = self.ao_species(this, count)?;
        self.ao_reserve(count)?;
        let mut n = 0u64;
        while k < fin {
            self.tick()?;
            let key = self.index_key(k)?;
            if self.ao_has(this, key)? {
                let v = self.get_prop(this, key)?;
                let nk = self.index_key(n)?;
                self.ao_create(a, nk, v)?;
            }
            k += 1;
            n += 1;
        }
        self.ao_set_len(Value::Object(a), n)?;
        Ok(Value::Object(a))
    }

    fn ao_search(&mut self, includes: bool, this: Value, x: Value, from: Value) -> Res<Value> {
        self.ao_this(this, if includes { "includes" } else { "indexOf" })?;
        let len = self.length_of(this)?;
        let none = Value::Number(-1.0);
        let not_found = if includes { Value::Bool(false) } else { none };
        if len == 0 {
            return Ok(not_found);
        }
        let Some(mut k) = self.ao_from_index(from, len)? else { return Ok(not_found) };
        // Two loops, not one with `if includes` inside: the compiler split them only by luck, and the merged loop cost 10 more
        // instructions for each element (measured on `arrays`).
        if includes {
            while k < len {
                self.tick()?;
                let key = self.index_key(k)?;
                let v = self.get_prop(this, key)?;
                if self.heap.same_value_zero(v, x) {
                    return Ok(Value::Bool(true));
                }
                k += 1;
            }
            return Ok(not_found);
        }
        while k < len {
            self.tick()?;
            let key = self.index_key(k)?;
            if self.ao_has(this, key)? {
                let v = self.get_prop(this, key)?;
                if self.heap.strict_equals(v, x) {
                    return Ok(Value::Number(k as f64));
                }
            }
            k += 1;
        }
        Ok(not_found)
    }

    fn ao_concat(&mut self, this: Value, args: &[Value]) -> Res<Value> {
        self.ao_this(this, "concat")?;
        let a = self.ao_species(this, 0)?;
        let mut n = 0u64;
        for item in std::iter::once(this).chain(args.iter().copied()) {
            self.tick()?;
            if !self.ao_spreadable(item)? {
                if n + 1 > u32::MAX as u64 {
                    return Err(self.mk_err(RANGE_ERROR, "Invalid array length"));
                }
                let k = self.index_key(n)?;
                self.ao_create(a, k, item)?;
                n += 1;
                continue;
            }
            let len = self.length_of(item)?;
            if n + len > u32::MAX as u64 {
                return Err(self.mk_err(RANGE_ERROR, "Invalid array length"));
            }
            self.ao_reserve(len)?;
            for i in 0..len {
                self.tick()?;
                let k = self.index_key(i)?;
                if self.ao_has(item, k)? {
                    let v = self.get_prop(item, k)?;
                    let nk = self.index_key(n + i)?;
                    self.ao_create(a, nk, v)?;
                }
            }
            n += len;
        }
        self.ao_set_len(Value::Object(a), n)?;
        Ok(Value::Object(a))
    }

    // ------------------------------------------------------------ mutation

    /// `to = from` for one element: copy it, or delete the target when the source is a hole.
    fn ao_move(&mut self, this: Value, from: u64, to: u64) -> Res<()> {
        self.tick()?;
        let (fk, tk) = (self.index_key(from)?, self.index_key(to)?);
        if self.ao_has(this, fk)? {
            let v = self.get_prop(this, fk)?;
            self.ao_set(this, tk, v)
        } else {
            self.ao_delete(this, tk)
        }
    }

    /// One-move fast path of `shift`, `unshift` and `splice` for plain dense arrays (see `Heap::splice_dense`; it
    /// answers `None` whenever the generic algorithm could behave differently). The move is charged after the fact:
    /// one step per 32 moved elements, a memmove of 16-byte values being about that fast, plus one per item.
    fn ao_dense(&mut self, this: Value, len: u64, start: u64, del: u64, items: &[Value]) -> Res<Option<Vec<Option<Value>>>> {
        let Value::Object(o) = this else { return Ok(None) };
        if len > u32::MAX as u64 {
            return Ok(None);
        }
        let Some(removed) = h!(self, self.heap.splice_dense(o, len as u32, start as usize, del as usize, items)) else { return Ok(None) };
        self.steps = self.steps.saturating_add(1 + (len - start - del) / 32 + del + items.len() as u64);
        if self.steps > self.step_limit {
            return Err(Error::Limit(LimitKind::Steps));
        }
        Ok(Some(removed))
    }

    fn ao_shift(&mut self, this: Value) -> Res<Value> {
        self.ao_this(this, "shift")?;
        let len = self.length_of(this)?;
        if len == 0 {
            self.ao_set_len(this, 0)?;
            return Ok(Value::Undefined);
        }
        if let Some(removed) = self.ao_dense(this, len, 0, 1, &[])? {
            return Ok(removed[0].unwrap_or(Value::Undefined));
        }
        self.ao_reserve(len)?;
        let first = self.get_prop(this, PropertyKey::Index(0))?;
        for k in 1..len {
            self.ao_move(this, k, k - 1)?;
        }
        let last = self.index_key(len - 1)?;
        self.ao_delete(this, last)?;
        self.ao_set_len(this, len - 1)?;
        Ok(first)
    }

    fn ao_unshift(&mut self, this: Value, args: &[Value]) -> Res<Value> {
        self.ao_this(this, "unshift")?;
        let len = self.length_of(this)?;
        let argc = args.len() as u64;
        if argc > 0 {
            if len + argc > MAX_SAFE_LEN {
                return Err(self.type_err("Invalid array length"));
            }
            if self.ao_dense(this, len, 0, 0, args)?.is_some() {
                return Ok(Value::Number((len + argc) as f64));
            }
            self.ao_reserve(len)?;
            for k in (0..len).rev() {
                self.ao_move(this, k, k + argc)?;
            }
            for (j, &v) in args.iter().enumerate() {
                self.tick()?;
                let key = self.index_key(j as u64)?;
                self.ao_set(this, key, v)?;
            }
        }
        self.ao_set_len(this, len + argc)?;
        Ok(Value::Number((len + argc) as f64))
    }

    fn ao_splice(&mut self, this: Value, args: &[Value]) -> Res<Value> {
        self.ao_this(this, "splice")?;
        let len = self.length_of(this)?;
        let start = self.ao_rel(arg(args, 0), len, 0)?;
        let ic = args.len().saturating_sub(2) as u64;
        let dc = match args.len() {
            0 => 0,
            1 => len - start,
            _ => self.ao_int(args[1])?.max(0.0).min((len - start) as f64) as u64,
        };
        if len + ic - dc > MAX_SAFE_LEN {
            return Err(self.type_err("Invalid array length"));
        }
        let a = self.ao_species(this, dc)?;
        if let Some(removed) = self.ao_dense(this, len, start, dc, args.get(2..).unwrap_or(&[]))? {
            for (k, v) in removed.into_iter().enumerate() {
                if let Some(v) = v {
                    let key = self.index_key(k as u64)?;
                    self.ao_create(a, key, v)?;
                }
            }
            self.ao_set_len(Value::Object(a), dc)?;
            return Ok(Value::Object(a));
        }
        self.ao_reserve((len - start).saturating_add(dc))?;
        for k in 0..dc {
            self.tick()?;
            let fk = self.index_key(start + k)?;
            if self.ao_has(this, fk)? {
                let v = self.get_prop(this, fk)?;
                let tk = self.index_key(k)?;
                self.ao_create(a, tk, v)?;
            }
        }
        self.ao_set_len(Value::Object(a), dc)?;
        if ic < dc {
            for k in start..len - dc {
                self.ao_move(this, k + dc, k + ic)?;
            }
            for k in (len - dc + ic..len).rev() {
                self.tick()?;
                let key = self.index_key(k)?;
                self.ao_delete(this, key)?;
            }
        } else if ic > dc {
            for k in (start..len - dc).rev() {
                self.ao_move(this, k + dc, k + ic)?;
            }
        }
        for (i, &v) in args.iter().skip(2).enumerate() {
            self.tick()?;
            let key = self.index_key(start + i as u64)?;
            self.ao_set(this, key, v)?;
        }
        self.ao_set_len(this, len - dc + ic)?;
        Ok(Value::Object(a))
    }

    // ------------------------------------------------------------ fill

    /// `fill(value, start, end)`: a strict `Set` on each index of the range, in order. The length is read once, and
    /// `start` and `end` are converted after it. The whole range is charged before the first write, so a range
    /// longer than the step budget never writes. The heap checks its byte cap before each element it adds.
    fn ao_fill(&mut self, this: Value, value: Value, start: Value, end: Value) -> Res<Value> {
        self.ao_this(this, "fill")?;
        let len = self.length_of(this)?;
        let k = self.ao_rel(start, len, 0)?;
        let fin = self.ao_rel(end, len, len)?;
        self.ao_reserve(fin.saturating_sub(k))?;
        for i in k..fin {
            self.tick()?;
            let key = self.index_key(i)?;
            self.ao_set(this, key, value)?;
        }
        Ok(this)
    }

    // ------------------------------------------------------------ sort

    fn ao_sort(&mut self, this: Value, cmp: Value) -> Res<Value> {
        if !matches!(cmp, Value::Undefined) && !self.is_callable(cmp) {
            let t = self.display(cmp);
            return Err(self.type_err(&format!("The comparison function must be either a function or undefined: {t}")));
        }
        self.ao_this(this, "sort")?;
        let len = self.length_of(this)?;
        self.ao_reserve(len)?;
        let mut vals: Vec<Value> = Vec::new();
        let mut undef = 0u64;
        for i in 0..len {
            self.tick()?;
            let k = self.index_key(i)?;
            if self.ao_has(this, k)? {
                let v = self.get_prop(this, k)?;
                if matches!(v, Value::Undefined) {
                    undef += 1;
                } else {
                    self.ao_temp(vals.len() + 1, 16 + 8 + 4 + 4)?;
                    vals.push(v);
                }
            }
        }
        let n = vals.len();
        let idx = self.ao_merge_sort(cmp, &vals)?;
        let count = n as u64 + undef;
        for (j, &p) in idx.iter().enumerate() {
            self.tick()?;
            let key = self.index_key(j as u64)?;
            self.ao_set(this, key, vals[p as usize])?;
        }
        for j in n as u64..count {
            self.tick()?;
            let key = self.index_key(j)?;
            self.ao_set(this, key, Value::Undefined)?;
        }
        for j in count..len {
            self.tick()?;
            let key = self.index_key(j)?;
            self.ao_delete(this, key)?;
        }
        Ok(this)
    }

    /// The stable order of `vals` under `cmp` (bottom-up merge sort): the indices of `vals`, smallest first. `vals` holds no
    /// `undefined`. Shared by `sort` and `toSorted`.
    fn ao_merge_sort(&mut self, cmp: Value, vals: &[Value]) -> Res<Vec<u32>> {
        let n = vals.len();
        let mut idx: Vec<u32> = (0..n as u32).collect();
        let mut tmp = idx.clone();
        // Strings of primitives are made once: no user code can see them. Objects convert at each comparison.
        let mut keys: Vec<Option<StrId>> = if matches!(cmp, Value::Undefined) { vec![None; n] } else { Vec::new() };
        let mut width = 1;
        while width < n {
            let mut i = 0;
            while i < n {
                let (mid, hi) = ((i + width).min(n), (i + 2 * width).min(n));
                let (mut l, mut r, mut o) = (i, mid, i);
                while l < mid && r < hi {
                    if self.ao_greater(cmp, vals, &mut keys, idx[l], idx[r])? {
                        tmp[o] = idx[r];
                        r += 1;
                    } else {
                        tmp[o] = idx[l];
                        l += 1;
                    }
                    o += 1;
                }
                tmp[o..o + (mid - l)].copy_from_slice(&idx[l..mid]);
                o += mid - l;
                tmp[o..o + (hi - r)].copy_from_slice(&idx[r..hi]);
                i += 2 * width;
            }
            std::mem::swap(&mut idx, &mut tmp);
            width *= 2;
        }
        Ok(idx)
    }

    /// `compare(vals[x], vals[y]) > 0`. A comparator result that is NaN counts as 0.
    fn ao_greater(&mut self, cmp: Value, vals: &[Value], keys: &mut [Option<StrId>], x: u32, y: u32) -> Res<bool> {
        let (vx, vy) = (vals[x as usize], vals[y as usize]);
        if !matches!(cmp, Value::Undefined) {
            let r = self.call_fn(cmp, Value::Undefined, &[vx, vy])?;
            return Ok(self.coerce_number(r)? > 0.0);
        }
        self.tick()?;
        let sx = self.ao_sort_key(vals, keys, x)?;
        let sy = self.ao_sort_key(vals, keys, y)?;
        Ok(!sx.same(sy) && h!(self, self.heap.str_units(sx)) > h!(self, self.heap.str_units(sy)))
    }

    #[inline] // the compiler inlined it before the proxy change moved other inlining decisions (measured: 1.5% on `sort`)
    fn ao_sort_key(&mut self, vals: &[Value], keys: &mut [Option<StrId>], i: u32) -> Res<StrId> {
        if let Some(s) = keys[i as usize] {
            return Ok(s);
        }
        let v = vals[i as usize];
        let s = self.coerce_string(v)?;
        if !matches!(v, Value::Object(_)) {
            keys[i as usize] = Some(s);
        }
        Ok(s)
    }

    // ------------------------------------------------------------ Object

    fn ao_assign(&mut self, args: &[Value]) -> Res<Value> {
        let target = self.to_object(arg(args, 0))?;
        for &src in args.iter().skip(1) {
            let keys: Vec<PropertyKey> = match src {
                Value::Object(o) => self.property_keys(o)?,
                Value::String(s) => (0..h!(self, self.heap.str_len(s)) as u32).map(PropertyKey::Index).collect(),
                _ => continue,
            };
            self.ao_temp(keys.len(), 16)?;
            for k in keys {
                self.tick()?;
                if let Value::Object(o) = src {
                    match self.o_peek(o, k)? {
                        Some(p) if p.enumerable() => {}
                        _ => continue,
                    }
                }
                let v = self.get_prop(src, k)?;
                self.ao_set(target, k, v)?;
            }
        }
        Ok(target)
    }

    /// `Object.values` (`entries` false) and `Object.entries`.
    fn ao_enum_own(&mut self, src: Value, entries: bool) -> Res<Value> {
        let keys: Vec<PropertyKey> = match src {
            Value::Object(o) => self.property_keys(o)?,
            Value::String(s) => (0..h!(self, self.heap.str_len(s)) as u32).map(PropertyKey::Index).collect(),
            Value::Undefined | Value::Null => return Err(self.type_err("Cannot convert undefined or null to object")),
            _ => Vec::new(),
        };
        self.ao_temp(keys.len(), 32)?;
        let mut out: Vec<Value> = Vec::new();
        for k in keys {
            self.tick()?;
            if matches!(k, PropertyKey::Symbol(_)) {
                continue;
            }
            if let Value::Object(o) = src {
                match self.o_peek(o, k)? {
                    Some(p) if p.enumerable() => {}
                    _ => continue,
                }
            }
            let v = self.get_prop(src, k)?;
            out.push(if entries {
                let kv = h!(self, self.heap.key_to_value(k));
                self.new_array_from(&[kv, v])?
            } else {
                v
            });
        }
        self.new_array_from(&out)
    }

    fn ao_from_entries(&mut self, iterable: Value) -> Res<Value> {
        if is_nullish(iterable) {
            let t = self.display(iterable);
            return Err(self.type_err(&format!("{t} is not iterable")));
        }
        let obj = h!(self, self.heap.new_object(Some(self.object_proto)));
        let mut it = self.iter_start_val(iterable)?;
        while let Some(item) = self.iter_next(&mut it)? {
            if let Err(e) = self.ao_entry(obj, item) {
                return Err(self.iter_close_throw(&mut it, e));
            }
        }
        Ok(Value::Object(obj))
    }

    /// One entry of `Object.fromEntries`.
    fn ao_entry(&mut self, obj: ObjId, item: Value) -> Res<()> {
        if !matches!(item, Value::Object(_)) {
            let t = self.display(item);
            return Err(self.type_err(&format!("Iterator value {t} is not an entry object")));
        }
        let k = self.get_prop(item, PropertyKey::Index(0))?;
        let v = self.get_prop(item, PropertyKey::Index(1))?;
        let key = self.coerce_key(k)?;
        self.ao_create(obj, key, v)
    }

    // ------------------------------------------------------------ Array.from

    fn ao_from_item(&mut self, a: ObjId, k: u64, mapfn: Option<Value>, this_arg: Value, v: Value) -> Res<()> {
        let v = match mapfn {
            Some(f) => self.call_fn(f, this_arg, &[v, Value::Number(k as f64)])?,
            None => v,
        };
        let key = self.index_key(k)?;
        self.ao_create(a, key, v)
    }

    fn ao_array_from(&mut self, this: Value, items: Value, mapfn: Value, this_arg: Value) -> Res<Value> {
        let mapping = !matches!(mapfn, Value::Undefined);
        if mapping {
            self.ao_callable(mapfn)?;
        }
        if is_nullish(items) {
            let t = self.display(items);
            return Err(self.type_err(&format!("{t} is not iterable")));
        }
        // An iterable goes through the shared iterator machinery (`iter_open`: `@@iterator` is read once). A throw
        // from the map function or from creating the element closes the iterator.
        if let Some(mut it) = self.iter_open(items)? {
            let (a, custom) = self.ao_from_target(this, &[], 0)?;
            let mut k = 0u64;
            while let Some(v) = self.iter_next(&mut it)? {
                if let Err(e) = self.ao_from_item(a, k, mapping.then_some(mapfn), this_arg, v) {
                    return Err(self.iter_close_throw(&mut it, e));
                }
                k += 1;
            }
            if custom {
                self.ao_set_len(Value::Object(a), k)?;
            }
            return Ok(Value::Object(a));
        }
        let len = self.length_of(items)?;
        let (a, custom) = self.ao_from_target(this, &[Value::Number(len as f64)], len)?;
        self.ao_reserve(len)?;
        for i in 0..len {
            self.tick()?;
            let key = self.index_key(i)?;
            let v = self.get_prop(items, key)?;
            let v = if mapping { self.call_fn(mapfn, this_arg, &[v, Value::Number(i as f64)])? } else { v };
            self.ao_create(a, key, v)?;
        }
        if custom {
            self.ao_set_len(Value::Object(a), len)?;
        }
        Ok(Value::Object(a))
    }
}

/// `Math.round`: halves go toward +Infinity, and `-0` stays `-0`.
fn math_round(x: f64) -> f64 {
    if !x.is_finite() || x.fract() == 0.0 {
        return x;
    }
    let f = x.floor();
    let r = if x - f >= 0.5 { f + 1.0 } else { f };
    if r == 0.0 && x < 0.0 { -0.0 } else { r }
}
