// Typed arrays and `DataView` of the evaluator: the constructors, `%TypedArray%.from` and `.of`, the methods of
// `%TypedArray%.prototype` and the `DataView` accessors. Included by `eval.rs` (`include!`), so it shares that module's
// imports and private items. The dispatch, the setup and `ArrayBuffer` are in `eval_binary.rs`; the byte storage, the
// element codec (`load`, `store`, `sort_elems`) and the integer-indexed property rules are in `heap_binary.rs`; the search
// methods are in `eval_binary_search.rs`.
//
// Rules of this file:
// - Elements stay in the buffer. A method that does not call JavaScript works on the bytes: `fill` writes one encoded
//   element over the range, `copyWithin` and `set` of the same type are `memmove`, `reverse` swaps elements in place and the
//   default `sort` sorts a copy of the elements in their own type. Only a method that calls back (`map`, `forEach`, `sort` with a
//   comparator, ...) reads an element as a number, one at a time, through `view_load`.
// - A callback can detach the buffer (`transfer`). Every callback method reads each element when it needs it, and an element
//   that is gone reads as `undefined`, as in the spec. A method that is done with all JavaScript checks the buffer again
//   before it writes.
// - The BigInt typed arrays have no element values here (this engine has no BigInt). A method that must read or write an
//   element of one is the gate `BIG_ELEM` (`Unsupported`), unless the array is empty. The methods that move bytes run.
// - Errors and their messages follow V8 (Node 22): the receiver wording of each method is in `TA_METHODS`.
// - `from`, `of`, `map`, `filter`, `slice` and `subarray` make an instance of the intrinsic class of the receiver. A
//   constructor that is not one of the ten intrinsics (a subclass, or a `@@species` other than the default) is `Unsupported`.

/// A typed array as the methods see it: the object, its buffer, the byte offset, the element count and the type.
#[derive(Clone, Copy)]
struct Ta {
    o: ObjId,
    buf: ObjId,
    off: usize,
    len: usize,
    ty: Elem,
}

/// How V8 words a value that is not a function in a typed-array method: `number 1`, `string "x"`, `object null`, `undefined`,
/// `object`, `symbol`.
fn typeof_form(it: &Interp<'_>, v: Value) -> String {
    match v {
        Value::Undefined => "undefined".into(),
        Value::Null => "object null".into(),
        Value::Bool(_) => format!("boolean {}", it.display(v)),
        Value::Number(n) => format!("number {}", number_to_string(if n == 0.0 { 0.0 } else { n })),
        Value::String(_) => format!("string \"{}\"", it.display(v)),
        Value::Symbol(_) => "symbol".into(),
        Value::Object(_) => "object".into(),
    }
}

impl<'p> Interp<'p> {
    // ------------------------------------------------------------ receivers and helpers

    /// An object as V8 words it in a `TypeError`, with no side effect: `#<Name>` (the name of its `constructor`),
    /// `[object Array]`, or `[object Int8Array]` for a typed array. A primitive prints as `display` does.
    fn recv_text(&mut self, v: Value) -> String {
        let Value::Object(o) = v else { return if let Value::Number(n) = v { number_to_string(if n == 0.0 { 0.0 } else { n }) } else { self.display(v) } };
        if self.funcs.contains_key(&o) {
            return "function".into();
        }
        match self.heap.view_type(o) {
            Some(Elem::Data) => return "#<DataView>".into(),
            Some(ty) => return format!("[object {}]", ty.name()),
            None => {}
        }
        if self.heap.is_array(o).unwrap_or(false) {
            return "[object Array]".into();
        }
        let name = (|| {
            let (c, n) = (self.heap.key_from_str("constructor").ok()?, self.heap.key_from_str("name").ok()?);
            let Ok(Get::Value(Value::Object(f))) = self.heap.get(o, c) else { return None };
            let Ok(Get::Value(Value::String(s))) = self.heap.get(f, n) else { return None };
            self.heap.to_string_lossy(s).ok().filter(|s| !s.is_empty())
        })();
        format!("#<{}>", name.unwrap_or_else(|| "Object".into()))
    }

    fn detached_err(&mut self, what: &str) -> Error {
        // V8 words this one wrongly: "findIndexLast".
        let what = if what == "%TypedArray%.prototype.findLastIndex" { "%TypedArray%.prototype.findIndexLast" } else { what };
        self.type_err(&format!("Cannot perform {what} on a detached ArrayBuffer"))
    }

    /// The typed array behind `this`, with no check of its buffer. A `DataView` is not a typed array. `plain` picks the V8
    /// wording of the error.
    fn this_ta_raw(&mut self, this: Value, what: &str, plain: bool) -> Res<Ta> {
        if let Value::Object(o) = this
            && let Some((buf, off, len)) = self.heap.view_parts(o)
            && let Some(ty) = self.heap.view_type(o)
            && ty != Elem::Data
        {
            return Ok(Ta { o, buf, off, len, ty });
        }
        if plain { Err(self.type_err("this is not a typed array.")) } else { Err(self.incompatible(what, this)) }
    }

    /// `ValidateTypedArray`: a typed array whose buffer is not detached.
    fn this_ta(&mut self, this: Value, what: &str, plain: bool) -> Res<Ta> {
        let t = self.this_ta_raw(this, what, plain)?;
        if self.heap.view_detached(t.o) {
            return Err(self.detached_err(what));
        }
        Ok(t)
    }

    /// The gate of a BigInt typed array that has an element to read or write.
    fn big_gate(&self, t: &Ta) -> Res<()> {
        if t.ty.is_big() && t.len > 0 { Err(unsupported(BIG_ELEM, Span::default())) } else { Ok(()) }
    }

    /// Element `i` of a typed array now, or `undefined` when it is gone (out of range or detached).
    fn ta_get(&self, t: &Ta, i: usize) -> Value {
        self.heap.view_load(t.o, i).map_or(Value::Undefined, Value::Number)
    }

    /// Temporary storage of `bytes` bytes for a sort, a filter or a join. It may not exceed a quarter of the heap budget.
    fn ta_temp(&self, bytes: usize) -> Res<()> {
        if bytes > self.heap.limits().max_bytes / 4 {
            return Err(Error::Limit(LimitKind::Heap(Limit::Bytes)));
        }
        Ok(())
    }

    /// A callback that is not a function: V8 words the error with the type of the value (`map` prints the value alone).
    fn ta_callable(&mut self, f: Value, value_only: bool) -> Res<()> {
        if self.is_callable(f) {
            return Ok(());
        }
        let t = if value_only { self.recv_text(f) } else { typeof_form(self, f) };
        Err(self.type_err(&format!("{t} is not a function")))
    }

    /// The length argument of a typed-array constructor (ToIndex). A `RangeError` as V8 words it.
    fn ta_len_arg(&mut self, v: Value) -> Res<usize> {
        let x = self.coerce_number(v)?;
        let n = if x.is_nan() { 0.0 } else { x.trunc() };
        if !(0.0..=MAX_SAFE).contains(&n) {
            return Err(self.mk_err(RANGE_ERROR, &format!("Invalid typed array length: {}", number_to_string(x))));
        }
        Ok(n as usize)
    }

    /// Convert `v` with ToNumber and write it at index `i`. An index that is gone ignores the write.
    fn ta_put(&mut self, o: ObjId, i: usize, v: Value) -> Res<()> {
        let n = self.coerce_number(v)?;
        self.heap.view_store(o, i, n);
        Ok(())
    }

    // ------------------------------------------------------------ constructors

    /// `new Int32Array(...)` and the other nine classes.
    fn ty_construct(&mut self, ty: Elem, a0: Value, a1: Value, a2: Value) -> Res<Value> {
        let Value::Object(src) = a0 else {
            let len = self.ta_len_arg(a0)?;
            return Ok(Value::Object(self.new_typed(ty, len)?));
        };
        let size = ty.size();
        match (self.heap.exotic_kind(src), self.heap.view_type(src)) {
            (Some(ExoticKind::Buffer), _) => {
                let off = {
                    let n = self.num_integer(a1)?;
                    if !(0.0..=MAX_SAFE).contains(&n) {
                        return Err(self.mk_err(RANGE_ERROR, &format!("Start offset {} is outside the bounds of the buffer", number_to_string(n))));
                    }
                    n as usize
                };
                if off % size != 0 {
                    return Err(self.mk_err(RANGE_ERROR, &format!("start offset of {} should be a multiple of {size}", ty.name())));
                }
                let new_len = if matches!(a2, Value::Undefined) { None } else { Some(self.ta_len_arg(a2)?) };
                if self.heap.buffer_detached(src) {
                    return Err(self.detached_err("Construct"));
                }
                let buf_len = self.heap.buffer_len(src).expect("buffer");
                let len = match new_len {
                    None => {
                        if !buf_len.is_multiple_of(size) {
                            return Err(self.mk_err(RANGE_ERROR, &format!("byte length of {} should be a multiple of {size}", ty.name())));
                        }
                        if off > buf_len {
                            return Err(self.mk_err(RANGE_ERROR, &format!("Start offset {off} is outside the bounds of the buffer")));
                        }
                        (buf_len - off) / size
                    }
                    Some(n) => {
                        if n.checked_mul(size).and_then(|b| b.checked_add(off)).is_none_or(|e| e > buf_len) {
                            return Err(self.mk_err(RANGE_ERROR, &format!("Invalid typed array length: {n}")));
                        }
                        n
                    }
                };
                Ok(Value::Object(h!(self, self.heap.new_typed_view(Some(self.bin_proto(ty)), src, off, len, ty))))
            }
            (Some(ExoticKind::View), Some(st)) if st != Elem::Data => {
                if self.heap.view_detached(src) {
                    return Err(self.detached_err("Construct"));
                }
                if st.is_big() != ty.is_big() {
                    return Err(self.type_err("Cannot mix BigInt and other types, use explicit conversions"));
                }
                let n = self.heap.view_parts(src).expect("view").2;
                self.work(n * st.size(), 6)?;
                if st == ty {
                    let ab = self.bp().ab;
                    let buf = h!(self, self.heap.new_buffer_copy(Some(ab), src, 0, n * size));
                    return Ok(Value::Object(h!(self, self.heap.new_typed_view(Some(self.bin_proto(ty)), buf, 0, n, ty))));
                }
                let out = self.new_typed(ty, n)?;
                let ok = self.heap.view_set_from(src, out, 0);
                debug_assert!(ok);
                Ok(Value::Object(out))
            }
            _ if let Some(mut it) = self.iter_open(a0)? => {
                // An iterable (`@@iterator` is not undefined or null): take every value first, then convert each one.
                // `IteratorToList` does not close the iterator on an error. The values are bounded before they grow.
                let cap = self.heap.buffer_limits().1 / size;
                let mut vals = Vec::new();
                while let Some(v) = self.iter_next(&mut it)? {
                    if vals.len() >= cap {
                        return Err(Error::Limit(LimitKind::Heap(Limit::BufferLength)));
                    }
                    self.iter_buffer_room(vals.len())?;
                    vals.push(v);
                }
                if ty.is_big() && !vals.is_empty() {
                    return Err(unsupported(BIG_ELEM, Span::default()));
                }
                let out = self.new_typed(ty, vals.len())?;
                for (i, v) in vals.into_iter().enumerate() {
                    self.tick()?;
                    self.ta_put(out, i, v)?;
                }
                Ok(Value::Object(out))
            }
            _ => {
                // An array-like: the length, then one read and one conversion for each index.
                let lv = self.get_prop(a0, self.k_length)?;
                let n = self.num_length(lv)?;
                if n * size as f64 > self.heap.buffer_limits().1 as f64 {
                    return Err(Error::Limit(LimitKind::Heap(Limit::BufferLength)));
                }
                let n = n as usize;
                if ty.is_big() && n > 0 {
                    return Err(unsupported(BIG_ELEM, Span::default()));
                }
                let out = self.new_typed(ty, n)?;
                for i in 0..n {
                    self.tick()?;
                    let v = self.get_prop(a0, PropertyKey::Index(i as u32))?;
                    self.ta_put(out, i, v)?;
                }
                Ok(Value::Object(out))
            }
        }
    }

    /// The element type of the constructor `this` of `from` and `of`. `%TypedArray%` itself cannot be constructed; any other
    /// constructor would need `Construct` and its result checks.
    fn ta_receiver(&mut self, this: Value) -> Res<Elem> {
        if let Value::Object(c) = this
            && self.is_constructor(c)
        {
            if let Some(ty) = TYPED.into_iter().find(|&t| self.bin_class(t) == c) {
                return Ok(ty);
            }
            if c == self.bp().ta_c {
                return Err(self.type_err("Abstract class TypedArray not directly constructable"));
            }
            return Err(unsupported("%TypedArray%.from and %TypedArray%.of on a constructor that is not a typed-array class", Span::default()));
        }
        let t = self.recv_text(this);
        Err(self.type_err(&format!("{t} is not a constructor")))
    }

    /// `%TypedArray%.from(source, mapfn, thisArg)`.
    fn ta_from(&mut self, this: Value, source: Value, mapfn: Value, this_arg: Value) -> Res<Value> {
        let ty = self.ta_receiver(this)?;
        let mapping = !matches!(mapfn, Value::Undefined);
        if mapping {
            self.ta_callable(mapfn, false)?;
        }
        if is_nullish(source) {
            let t = typeof_form(self, source);
            return Err(self.type_err(&format!("{t} is not iterable (cannot read property Symbol(Symbol.iterator))")));
        }
        let size = ty.size();
        let cap = self.heap.buffer_limits().1 / size;
        let mut vals: Vec<Value> = Vec::new();
        let array_like = if let Some(mut it) = self.iter_open(source)? {
            // The values come first, then the map function runs over them.
            while let Some(v) = self.iter_next(&mut it)? {
                if vals.len() >= cap {
                    return Err(Error::Limit(LimitKind::Heap(Limit::BufferLength)));
                }
                self.iter_buffer_room(vals.len())?;
                vals.push(v);
            }
            false
        } else {
            true
        };
        let len = if array_like {
            let lv = self.get_prop(source, self.k_length)?;
            let n = self.num_length(lv)?;
            if n > cap as f64 {
                return Err(Error::Limit(LimitKind::Heap(Limit::BufferLength)));
            }
            n as usize
        } else {
            vals.len()
        };
        if ty.is_big() && len > 0 {
            return Err(unsupported(BIG_ELEM, Span::default()));
        }
        let out = self.new_typed(ty, len)?;
        for k in 0..len {
            self.tick()?;
            let v = if array_like { self.get_prop(source, PropertyKey::Index(k as u32))? } else { vals.get(k).copied().unwrap_or(Value::Undefined) };
            let v = if mapping { self.call_fn(mapfn, this_arg, &[v, Value::Number(k as f64)])? } else { v };
            self.ta_put(out, k, v)?;
        }
        Ok(Value::Object(out))
    }

    /// `%TypedArray%.of(...items)`.
    fn ta_of(&mut self, this: Value, items: &[Value]) -> Res<Value> {
        let ty = self.ta_receiver(this)?;
        if ty.is_big() && !items.is_empty() {
            return Err(unsupported(BIG_ELEM, Span::default()));
        }
        let out = self.new_typed(ty, items.len())?;
        for (k, &v) in items.iter().enumerate() {
            self.tick()?;
            self.ta_put(out, k, v)?;
        }
        Ok(Value::Object(out))
    }

    // ------------------------------------------------------------ methods that move bytes

    /// `%TypedArray%.prototype.set(source, offset)`.
    fn ta_set(&mut self, this: Value, source: Value, offset: Value) -> Res<Value> {
        let what = "%TypedArray%.prototype.set";
        let t = self.this_ta_raw(this, what, true)?;
        let off = self.num_integer(offset)?;
        let oob = |s: &mut Self| s.mk_err(RANGE_ERROR, "offset is out of bounds");
        if off < 0.0 {
            return Err(oob(self));
        }
        if self.heap.view_detached(t.o) {
            return Err(self.detached_err(what));
        }
        if let Value::Object(s) = source
            && let Some(st) = self.heap.view_type(s)
            && st != Elem::Data
        {
            if self.heap.view_detached(s) {
                return Err(self.detached_err(what));
            }
            if st.is_big() != t.ty.is_big() {
                return Err(self.type_err("Cannot mix BigInt and other types, use explicit conversions"));
            }
            let slen = self.heap.view_parts(s).expect("view").2;
            if off + slen as f64 > t.len as f64 {
                return Err(oob(self));
            }
            self.work(slen * st.size().max(t.ty.size()), 6)?;
            let ok = self.heap.view_set_from(s, t.o, off as usize);
            debug_assert!(ok);
            return Ok(Value::Undefined);
        }
        if is_nullish(source) {
            return Err(self.type_err("Cannot convert undefined or null to object"));
        }
        let lv = self.get_prop(source, self.k_length)?;
        let n = self.num_length(lv)?;
        if off + n > t.len as f64 {
            return Err(oob(self));
        }
        if t.ty.is_big() && n > 0.0 {
            return Err(unsupported(BIG_ELEM, Span::default()));
        }
        let off = off as usize;
        for i in 0..n as usize {
            self.tick()?;
            let v = self.get_prop(source, PropertyKey::Index(i as u32))?;
            self.ta_put(t.o, off + i, v)?;
        }
        Ok(Value::Undefined)
    }

    /// Relative `start` and `end` arguments, as `slice` and `subarray` read them.
    fn ta_range(&mut self, len: usize, a0: Value, a1: Value) -> Res<(usize, usize)> {
        let first = self.num_integer(a0)?;
        let first = Self::rel_index(first, len);
        let last = if matches!(a1, Value::Undefined) { len } else { let e = self.num_integer(a1)?; Self::rel_index(e, len) };
        Ok((first, last))
    }

    fn ta_subarray(&mut self, this: Value, a0: Value, a1: Value) -> Res<Value> {
        let t = self.this_ta_raw(this, "%TypedArray%.prototype.subarray", false)?;
        let len = if self.heap.view_detached(t.o) { 0 } else { t.len };
        let (first, last) = self.ta_range(len, a0, a1)?;
        let class = self.bin_class(t.ty);
        self.bin_species(t.o, class, "typed array species constructor other than the intrinsic class")?;
        if self.heap.view_detached(t.o) {
            return Err(self.detached_err("Construct"));
        }
        let n = last.saturating_sub(first);
        Ok(Value::Object(h!(self, self.heap.new_typed_view(Some(self.bin_proto(t.ty)), t.buf, t.off + first * t.ty.size(), n, t.ty))))
    }

    fn ta_slice(&mut self, this: Value, a0: Value, a1: Value) -> Res<Value> {
        let what = "%TypedArray%.prototype.slice";
        let t = self.this_ta(this, what, false)?;
        let (first, last) = self.ta_range(t.len, a0, a1)?;
        let class = self.bin_class(t.ty);
        self.bin_species(t.o, class, "typed array species constructor other than the intrinsic class")?;
        let last = last.max(first);
        let size = t.ty.size();
        self.work((last - first) * size, 6)?;
        if last > first && self.heap.view_detached(t.o) {
            return Err(self.detached_err(what));
        }
        // A buffer that was detached while `last == first` still gives an empty result.
        let ab = self.bp().ab;
        let buf = if last > first {
            h!(self, self.heap.new_buffer_copy(Some(ab), t.o, first * size, last * size))
        } else {
            h!(self, self.heap.new_buffer(Some(ab), 0))
        };
        Ok(Value::Object(h!(self, self.heap.new_typed_view(Some(self.bin_proto(t.ty)), buf, 0, last - first, t.ty))))
    }

    // ------------------------------------------------------------ the method table

    fn ta_method(&mut self, m: TaM, this: Value, args: &[Value]) -> Res<Value> {
        use TaM::*;
        let (what, plain) = m.info();
        let (a0, a1, a2) = (arg(args, 0), arg(args, 1), arg(args, 2));
        match m {
            Includes => return self.ta_search(Search::Includes, this, args),
            LastIndexOf => return self.ta_search(Search::LastIndexOf, this, args),
            // `sort` and `toSorted` check the comparator before the receiver.
            Sort | ToSorted if !matches!(a0, Value::Undefined) && !self.is_callable(a0) => {
                let t = self.recv_text(a0);
                return Err(self.type_err(&format!("The comparison function must be either a function or undefined: {t}")));
            }
            _ => {}
        }
        let t = self.this_ta(this, what, plain)?;
        match m {
            At => {
                let rel = self.num_integer(a0)?;
                let k = if rel >= 0.0 { rel } else { t.len as f64 + rel };
                if k < 0.0 || k >= t.len as f64 {
                    return Ok(Value::Undefined);
                }
                self.big_gate(&t)?;
                Ok(self.ta_get(&t, k as usize))
            }
            Fill => self.ta_fill(t, a0, a1, a2),
            CopyWithin => self.ta_copy_within(t, a0, a1, a2),
            Reverse => {
                self.work(t.len * t.ty.size(), 6)?;
                let size = t.ty.size();
                if let Some(b) = self.heap.bytes_mut(t.o) {
                    reverse_elems(b, size);
                }
                Ok(this)
            }
            ToReversed => {
                self.work(t.len * t.ty.size(), 6)?;
                let out = self.ta_clone(&t)?;
                let size = t.ty.size();
                if let Some(b) = self.heap.bytes_mut(out) {
                    reverse_elems(b, size);
                }
                Ok(Value::Object(out))
            }
            With => {
                self.big_gate_always(&t)?;
                let rel = self.num_integer(a0)?;
                let idx = if rel >= 0.0 { rel } else { t.len as f64 + rel };
                let v = self.coerce_number(a1)?;
                // `IsValidIntegerIndex` is asked after the value is converted: a detached buffer has no valid index.
                if self.heap.view_detached(t.o) || idx < 0.0 || idx >= t.len as f64 {
                    return Err(self.mk_err(RANGE_ERROR, "Invalid typed array index"));
                }
                self.work(t.len * t.ty.size(), 6)?;
                let out = self.ta_clone(&t)?;
                self.heap.view_store(out, idx as usize, v);
                Ok(Value::Object(out))
            }
            Join => self.ta_join(t, a0, false),
            ToLocaleString => self.ta_join(t, Value::Undefined, true),
            Sort => {
                self.ta_sort_in_place(t, a0)?;
                Ok(this)
            }
            ToSorted => {
                self.work(t.len * t.ty.size(), 6)?;
                let out = self.ta_clone(&t)?;
                let c = Ta { o: out, buf: self.heap.view_parts(out).expect("view").0, off: 0, len: t.len, ty: t.ty };
                self.ta_sort_in_place(c, a0)?;
                Ok(Value::Object(out))
            }
            Every | Any | Find | FindIndex | FindLast | FindLastIndex | ForEach | Map | Filter | Reduce | ReduceRight => self.ta_callback(m, t, this, args),
            Includes | LastIndexOf => unreachable!("handled above"),
        }
    }

    /// The gate for a method that converts its value argument even when the array is empty (`fill`, `with`).
    fn big_gate_always(&self, t: &Ta) -> Res<()> {
        if t.ty.is_big() { Err(unsupported(BIG_ELEM, Span::default())) } else { Ok(()) }
    }

    /// A new typed array of the same type with a copy of the elements of `t`.
    fn ta_clone(&mut self, t: &Ta) -> Res<ObjId> {
        let size = t.ty.size();
        let ab = self.bp().ab;
        let buf = h!(self, self.heap.new_buffer_copy(Some(ab), t.o, 0, t.len * size));
        Ok(h!(self, self.heap.new_typed_view(Some(self.bin_proto(t.ty)), buf, 0, t.len, t.ty)))
    }

    fn ta_fill(&mut self, t: Ta, value: Value, start: Value, end: Value) -> Res<Value> {
        self.big_gate_always(&t)?;
        let n = self.coerce_number(value)?;
        let first = self.num_integer(start)?;
        let first = Self::rel_index(first, t.len);
        let last = if matches!(end, Value::Undefined) { t.len } else { let e = self.num_integer(end)?; Self::rel_index(e, t.len) };
        // The conversions may have detached the buffer.
        if self.heap.view_detached(t.o) {
            return Err(self.detached_err("%TypedArray%.prototype.fill"));
        }
        if last > first {
            let size = t.ty.size();
            self.work((last - first) * size, 6)?;
            let mut one = [0u8; 8];
            store(t.ty, &mut one, n);
            if let Some(b) = self.heap.bytes_mut(t.o) {
                let r = &mut b[first * size..last * size];
                if size == 1 {
                    r.fill(one[0]);
                } else {
                    for c in r.chunks_exact_mut(size) {
                        c.copy_from_slice(&one[..size]);
                    }
                }
            }
        }
        Ok(Value::Object(t.o))
    }

    fn ta_copy_within(&mut self, t: Ta, target: Value, start: Value, end: Value) -> Res<Value> {
        let to = self.num_integer(target)?;
        let to = Self::rel_index(to, t.len);
        let from = self.num_integer(start)?;
        let from = Self::rel_index(from, t.len);
        let fin = if matches!(end, Value::Undefined) { t.len } else { let e = self.num_integer(end)?; Self::rel_index(e, t.len) };
        let count = fin.saturating_sub(from).min(t.len - to);
        if count > 0 {
            if self.heap.view_detached(t.o) {
                return Err(self.detached_err("%TypedArray%.prototype.copyWithin"));
            }
            let size = t.ty.size();
            self.work(count * size, 6)?;
            let ok = self.heap.view_copy(t.o, from * size, t.o, to * size, count * size);
            debug_assert!(ok);
        }
        Ok(Value::Object(t.o))
    }

    // ------------------------------------------------------------ join and toLocaleString

    /// `join(separator)` (`locale` false) and `toLocaleString()` (`locale` true).
    fn ta_join(&mut self, t: Ta, sep: Value, locale: bool) -> Res<Value> {
        let sep: Vec<u16> = if locale || matches!(sep, Value::Undefined) {
            vec![b',' as u16]
        } else {
            let s = self.coerce_string(sep)?;
            h!(self, self.heap.str_units(s)).to_vec()
        };
        if t.ty.is_big() && t.len > 0 {
            return Err(unsupported(BIG_ELEM, Span::default()));
        }
        self.spend(t.len as u64)?;
        let cap = self.heap.limits().max_string_units;
        let mut out: Vec<u16> = Vec::new();
        let mut text = String::new();
        for i in 0..t.len {
            if i > 0 {
                out.extend_from_slice(&sep);
            }
            // An element that is gone (the buffer was detached by a callback) is the empty string.
            let Some(x) = self.heap.view_load(t.o, i) else { continue };
            if locale {
                let key = self.key_to_locale_string();
                let m = self.get_prop(Value::Number(x), key)?;
                if matches!(m, Value::Undefined) {
                    return Err(unsupported("Number.prototype.toLocaleString", Span::default()));
                }
                let r = self.call_fn(m, Value::Number(x), &[])?;
                let s = self.coerce_string(r)?;
                out.extend_from_slice(h!(self, self.heap.str_units(s)));
            } else {
                text.clear();
                if t.ty.is_float() {
                    text.push_str(&number_to_string(x));
                } else {
                    use std::fmt::Write;
                    let _ = write!(text, "{}", x as i64);
                }
                out.extend(text.bytes().map(u16::from));
            }
            if out.len() > cap {
                return Err(Error::Limit(LimitKind::Heap(Limit::StringLength)));
            }
        }
        let s = h!(self, self.heap.intern_vec(out));
        Ok(Value::String(s))
    }

    fn key_to_locale_string(&mut self) -> PropertyKey {
        self.heap.key_from_str("toLocaleString").expect("short key")
    }

    // ------------------------------------------------------------ sort

    fn ta_sort_in_place(&mut self, t: Ta, cmp: Value) -> Res<()> {
        if t.len < 2 {
            return Ok(());
        }
        let size = t.ty.size();
        self.ta_temp(if matches!(cmp, Value::Undefined) { t.len * size } else { t.len * 16 })?;
        if matches!(cmp, Value::Undefined) {
            // `n log n` comparisons of whole elements, charged as one step for each 16 elements per level of the sort.
            let levels = (usize::BITS - t.len.leading_zeros()) as usize;
            self.work(t.len * levels, 4)?;
            if let Some(b) = self.heap.bytes_mut(t.o) {
                sort_elems(t.ty, b);
            }
            return Ok(());
        }
        self.big_gate(&t)?;
        let mut v: Vec<f64> = (0..t.len).map(|i| self.heap.view_load(t.o, i).unwrap_or(0.0)).collect();
        let mut tmp = v.clone();
        // Bottom-up merge sort: stable, and it stops at the first error with the array untouched.
        let n = v.len();
        let mut width = 1;
        while width < n {
            let mut i = 0;
            while i < n {
                let (mid, hi) = ((i + width).min(n), (i + 2 * width).min(n));
                let (mut l, mut r, mut k) = (i, mid, i);
                while l < mid && r < hi {
                    let c = self.call_fn(cmp, Value::Undefined, &[Value::Number(v[l]), Value::Number(v[r])])?;
                    let c = self.coerce_number(c)?;
                    if c > 0.0 {
                        tmp[k] = v[r];
                        r += 1;
                    } else {
                        tmp[k] = v[l];
                        l += 1;
                    }
                    k += 1;
                }
                tmp[k..k + (mid - l)].copy_from_slice(&v[l..mid]);
                k += mid - l;
                tmp[k..k + (hi - r)].copy_from_slice(&v[r..hi]);
                i += 2 * width;
            }
            std::mem::swap(&mut v, &mut tmp);
            width *= 2;
        }
        // The comparator may have detached the buffer: the writes are then no-ops.
        for (i, x) in v.into_iter().enumerate() {
            self.heap.view_store(t.o, i, x);
        }
        Ok(())
    }

    // ------------------------------------------------------------ methods with a callback

    fn ta_callback(&mut self, m: TaM, t: Ta, this: Value, args: &[Value]) -> Res<Value> {
        use TaM::*;
        let (cb, this_arg) = (arg(args, 0), arg(args, 1));
        self.ta_callable(cb, m == Map)?;
        self.big_gate(&t)?;
        let len = t.len;
        let me = Value::Object(t.o);
        let call3 = |s: &mut Self, i: usize| -> Res<(Value, Value)> {
            s.tick()?;
            let v = s.ta_get(&t, i);
            let r = s.call_fn(cb, this_arg, &[v, Value::Number(i as f64), me])?;
            Ok((v, r))
        };
        match m {
            ForEach => {
                for i in 0..len {
                    call3(self, i)?;
                }
                Ok(Value::Undefined)
            }
            Every => {
                for i in 0..len {
                    let (_, r) = call3(self, i)?;
                    if !self.truthy(r)? {
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            }
            Any => {
                for i in 0..len {
                    let (_, r) = call3(self, i)?;
                    if self.truthy(r)? {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }
            Find | FindIndex => {
                for i in 0..len {
                    let (v, r) = call3(self, i)?;
                    if self.truthy(r)? {
                        return Ok(if m == Find { v } else { Value::Number(i as f64) });
                    }
                }
                Ok(if m == Find { Value::Undefined } else { Value::Number(-1.0) })
            }
            FindLast | FindLastIndex => {
                for i in (0..len).rev() {
                    let (v, r) = call3(self, i)?;
                    if self.truthy(r)? {
                        return Ok(if m == FindLast { v } else { Value::Number(i as f64) });
                    }
                }
                Ok(if m == FindLast { Value::Undefined } else { Value::Number(-1.0) })
            }
            Map => {
                let class = self.bin_class(t.ty);
                self.bin_species(t.o, class, "typed array species constructor other than the intrinsic class")?;
                let out = self.new_typed(t.ty, len)?;
                for i in 0..len {
                    let (_, r) = call3(self, i)?;
                    self.ta_put(out, i, r)?;
                }
                Ok(Value::Object(out))
            }
            Filter => {
                self.ta_temp(len * 8)?;
                let mut kept: Vec<f64> = Vec::new();
                for i in 0..len {
                    let (v, r) = call3(self, i)?;
                    if self.truthy(r)?
                        && let Value::Number(x) = v
                    {
                        kept.push(x);
                    }
                }
                let class = self.bin_class(t.ty);
                self.bin_species(t.o, class, "typed array species constructor other than the intrinsic class")?;
                let out = self.new_typed(t.ty, kept.len())?;
                for (i, x) in kept.into_iter().enumerate() {
                    self.heap.view_store(out, i, x);
                }
                Ok(Value::Object(out))
            }
            _ => {
                // reduce and reduceRight
                let right = m == ReduceRight;
                let order = |k: usize| if right { len - 1 - k } else { k };
                let (mut acc, mut k) = if args.len() >= 2 {
                    (args[1], 0)
                } else if len == 0 {
                    return Err(self.type_err("Reduce of empty array with no initial value"));
                } else {
                    (self.ta_get(&t, order(0)), 1)
                };
                while k < len {
                    self.tick()?;
                    let i = order(k);
                    let v = self.ta_get(&t, i);
                    acc = self.call_fn(cb, Value::Undefined, &[acc, v, Value::Number(i as f64), me])?;
                    k += 1;
                }
                let _ = this;
                Ok(acc)
            }
        }
    }

    // ------------------------------------------------------------ DataView

    fn dv_construct(&mut self, buffer: Value, offset: Value, length: Value) -> Res<Value> {
        let b = match buffer {
            Value::Object(b) if self.heap.exotic_kind(b) == Some(ExoticKind::Buffer) => b,
            _ => return Err(self.type_err("First argument to DataView constructor must be an ArrayBuffer")),
        };
        let off = {
            let n = self.num_integer(offset)?;
            if !(0.0..=MAX_SAFE).contains(&n) {
                return Err(self.mk_err(RANGE_ERROR, &format!("Start offset {} is outside the bounds of the buffer", number_to_string(n))));
            }
            n as usize
        };
        if self.heap.buffer_detached(b) {
            return Err(self.detached_err("DataView constructor"));
        }
        let buf_len = self.heap.buffer_len(b).expect("buffer");
        if off > buf_len {
            return Err(self.mk_err(RANGE_ERROR, &format!("Start offset {off} is outside the bounds of the buffer")));
        }
        let len = if matches!(length, Value::Undefined) {
            buf_len - off
        } else {
            let n = self.num_integer(length)?;
            if !(0.0..=MAX_SAFE).contains(&n) {
                return Err(self.mk_err(RANGE_ERROR, &format!("Invalid DataView length {}", number_to_string(n))));
            }
            n as usize
        };
        if self.heap.buffer_detached(b) {
            return Err(self.detached_err("DataView constructor"));
        }
        if off.checked_add(len).is_none_or(|e| e > buf_len) {
            return Err(self.mk_err(RANGE_ERROR, &format!("Invalid DataView length {len}")));
        }
        Ok(Value::Object(h!(self, self.heap.new_typed_view(Some(self.bp().dv), b, off, len, Elem::Data))))
    }

    /// The `DataView` behind `this`: `(object, buffer, byte offset, byte length)`.
    fn this_dv(&mut self, this: Value, what: &str) -> Res<(ObjId, ObjId, usize, usize)> {
        self.dv_parts(this).ok_or_else(|| self.incompatible(what, this))
    }

    /// `(object, buffer, byte offset, byte length)` of a `DataView`. No allocation: the get and set methods are hot.
    fn dv_parts(&self, this: Value) -> Option<(ObjId, ObjId, usize, usize)> {
        let Value::Object(o) = this else { return None };
        let (b, off, len) = self.heap.view_parts(o)?;
        (self.heap.view_type(o) == Some(Elem::Data)).then_some((o, b, off, len))
    }

    fn dv_getter(&mut self, f: BinFn, this: Value) -> Res<Value> {
        let what = match f {
            BinFn::DvBuffer => "get DataView.prototype.buffer",
            BinFn::DvByteLength => "get DataView.prototype.byteLength",
            _ => "get DataView.prototype.byteOffset",
        };
        let (o, b, off, len) = self.this_dv(this, what)?;
        if f == BinFn::DvBuffer {
            return Ok(Value::Object(b));
        }
        if self.heap.view_detached(o) {
            return Err(self.detached_err(what));
        }
        Ok(Value::Number(if f == BinFn::DvByteLength { len } else { off } as f64))
    }

    /// The method name for messages: `DataView.prototype.getInt8`.
    fn dv_name(ty: Elem, set: bool) -> String {
        format!("DataView.prototype.{}{}", if set { "set" } else { "get" }, ty.name().trim_end_matches("Array"))
    }

    fn dv_index(&mut self, v: Value) -> Res<usize> {
        let n = self.num_integer(v)?;
        if !(0.0..=MAX_SAFE).contains(&n) {
            return Err(self.mk_err(RANGE_ERROR, "Offset is outside the bounds of the DataView"));
        }
        Ok(n as usize)
    }

    fn dv_get(&mut self, ty: Elem, this: Value, index: Value, little: Value) -> Res<Value> {
        let Some((o, _, _, len)) = self.dv_parts(this) else { return Err(self.incompatible(&Self::dv_name(ty, false), this)) };
        let at = self.dv_index(index)?;
        let little = self.truthy(little)?;
        if self.heap.view_detached(o) {
            return Err(self.detached_err(&Self::dv_name(ty, false)));
        }
        let size = ty.size();
        if at.checked_add(size).is_none_or(|e| e > len) {
            return Err(self.mk_err(RANGE_ERROR, "Offset is outside the bounds of the DataView"));
        }
        let bytes = self.heap.bytes(o).ok_or(Error::Heap(HeapError::Stale))?;
        let mut raw = [0u8; 8];
        raw[..size].copy_from_slice(&bytes[at..at + size]);
        if !little {
            raw[..size].reverse();
        }
        Ok(Value::Number(load(ty, &raw)))
    }

    fn dv_set(&mut self, ty: Elem, this: Value, index: Value, value: Value, little: Value) -> Res<Value> {
        let Some((o, _, _, len)) = self.dv_parts(this) else { return Err(self.incompatible(&Self::dv_name(ty, true), this)) };
        let at = self.dv_index(index)?;
        let n = self.coerce_number(value)?;
        let little = self.truthy(little)?;
        if self.heap.view_detached(o) {
            return Err(self.detached_err(&Self::dv_name(ty, true)));
        }
        let size = ty.size();
        if at.checked_add(size).is_none_or(|e| e > len) {
            return Err(self.mk_err(RANGE_ERROR, "Offset is outside the bounds of the DataView"));
        }
        let mut raw = [0u8; 8];
        store(ty, &mut raw, n);
        if !little {
            raw[..size].reverse();
        }
        let bytes = self.heap.bytes_mut(o).ok_or(Error::Heap(HeapError::Stale))?;
        bytes[at..at + size].copy_from_slice(&raw[..size]);
        Ok(Value::Undefined)
    }
}

/// Reverse the order of the whole elements of `b` (`size` bytes each), in place.
fn reverse_elems(b: &mut [u8], size: usize) {
    if size == 1 {
        b.reverse();
        return;
    }
    let n = b.len() / size;
    for i in 0..n / 2 {
        let (head, tail) = b.split_at_mut((n - 1 - i) * size);
        head[i * size..(i + 1) * size].swap_with_slice(&mut tail[..size]);
    }
}
