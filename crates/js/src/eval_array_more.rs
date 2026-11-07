// The Array.prototype methods of batch 1. Included by `eval.rs` (`include!`), so it shares that module's imports and
// private items. The natives are `Nat::Ao(AoFn::Arr..)`; `call_ao` forwards them to `call_array_more`. The helpers
// (`ao_*`) are the ones of `eval_array_object.rs`.
//
// Supported
// - `reverse`, `at`, `flat`, `flatMap`, `findLast`, `findLastIndex`, `lastIndexOf`, `reduceRight`, `copyWithin`,
//   `toReversed`, `toSorted`, `toSpliced`, `with`, `toLocaleString` and `Array.prototype[@@unscopables]`.
// - Receivers are generic array-likes, as in `eval_array_object.rs`: `length` is read once (ToLength), holes follow
//   HasProperty (`reverse`, `flat`, `flatMap`, `lastIndexOf`, `reduceRight`, `copyWithin`) or read as `undefined` (`findLast`,
//   `findLastIndex`, and the four copying methods), every write is a strict `Set` or `DeletePropertyOrThrow`, and getters
//   run in the order of the specification (`toReversed` and `findLast` read from the end).
// - `flat` and `flatMap` read `constructor` and `@@species` as ArraySpeciesCreate does, then build the result in a list and
//   store it in one step (no script sees the result before it is done). A cyclic array stops at the traversal-depth limit.
// - The copying methods (`toReversed`, `toSorted`, `toSpliced`, `with`) make a plain array, check its length (RangeError above
//   2^32-1) before they read, and fail on a message equal to V8's. `toSorted` shares the merge sort of `sort`.
//
// Fast paths (they answer only when the generic algorithm cannot behave differently, see `Heap::dense_elems` and
// `Heap::edit_dense`; no script runs inside them)
// - `reverse` and `copyWithin` change the dense element vector in place. `lastIndexOf` scans it backwards. `flat` walks
//   nested dense arrays. `toReversed`, `toSpliced` and `with` copy it. The work is charged after the fact: one step per
//   8 elements read (32 moved), and `ao_reserve` first, so a range longer than the budget does nothing.
//
// Gates (explicit `Unsupported`, never silent)
// - The shared species gate of `flat` and `flatMap` (see `ao_species`).
// Known differences: `toLocaleString` has no cycle guard (V8 prints "" for an array that holds itself; this stops at the
// traversal-depth limit, as `join` does) and it needs `toLocaleString` on each element (`Number.prototype.toLocaleString`
// is not bound yet).

/// The names of `Array.prototype[@@unscopables]`.
const ARRAY_UNSCOPABLES: [&str; 16] = [
    "at", "copyWithin", "entries", "fill", "find", "findIndex", "findLast", "findLastIndex", "flat", "flatMap", "includes", "keys", "toReversed", "toSorted", "toSpliced", "values",
];

/// How `flat_dense` ended.
enum FlatFast {
    Done,
    /// The walk met something it does not do (a hole before a getter, a deep or cyclic array, a big result): the generic walk runs.
    Fallback,
}

/// `FlattenIntoArray` over plain dense arrays only (no script can run). `work` counts the elements read; the walk gives up
/// (`Fallback`) past `budget` of them, past `max` results or 64 levels, so a cyclic array or a huge one is bounded by the
/// generic walk, which charges every element.
#[allow(clippy::too_many_arguments)]
fn flat_dense(heap: &Heap, o: ObjId, len: u64, depth: f64, level: u32, out: &mut Vec<Option<Value>>, max: usize, budget: u64, work: &mut u64) -> Result<FlatFast, HeapError> {
    let Some(e) = heap.dense_elems(o, len)? else { return Ok(FlatFast::Fallback) };
    for x in e {
        *work += 1;
        let Some(v) = *x else { continue };
        if *work > budget || out.len() >= max {
            return Ok(FlatFast::Fallback);
        }
        if depth > 0.0
            && let Value::Object(c) = v
            && let Some(n) = heap.array_length(c)?
        {
            if level >= 64 {
                return Ok(FlatFast::Fallback);
            }
            if let FlatFast::Fallback = flat_dense(heap, c, n as u64, depth - 1.0, level + 1, out, max, budget, work)? {
                return Ok(FlatFast::Fallback);
            }
        } else {
            out.push(Some(v));
        }
    }
    Ok(FlatFast::Done)
}

impl<'p> Interp<'p> {
    fn install_array_more(&mut self, ap: ObjId) -> Res<()> {
        use AoFn::*;
        for (name, f, len) in [
            ("reverse", ArrReverse, 0), ("at", ArrAt, 1), ("flat", ArrFlat, 0), ("flatMap", ArrFlatMap, 1), ("findLast", ArrFindLast, 1),
            ("findLastIndex", ArrFindLastIndex, 1), ("lastIndexOf", ArrLastIndexOf, 1), ("reduceRight", ArrReduceRight, 1),
            ("copyWithin", ArrCopyWithin, 2), ("toReversed", ArrToReversed, 0), ("toSorted", ArrToSorted, 1), ("toSpliced", ArrToSpliced, 2),
            ("with", ArrWith, 2), ("toLocaleString", ArrToLocaleString, 0),
        ] {
            self.method(ap, name, Nat::Ao(f), len)?;
        }
        let un = h!(self, self.heap.new_object(None));
        for name in ARRAY_UNSCOPABLES {
            let k = h!(self, self.heap.key_from_str(name));
            self.def(un, k, Value::Bool(true), true, true, true)?;
        }
        self.def(ap, self.wk_key(WK_UNSCOPABLES), Value::Object(un), false, false, true)
    }

    fn call_array_more(&mut self, f: AoFn, this: Value, args: &[Value]) -> Res<Value> {
        use AoFn::*;
        let (a0, a1) = (arg(args, 0), arg(args, 1));
        match f {
            ArrReverse => self.ao_reverse(this),
            ArrAt => {
                self.ao_cvt(this)?;
                let len = self.length_of(this)?;
                let rel = self.ao_int(a0)?;
                let k = if rel >= 0.0 { rel } else { len as f64 + rel };
                if k < 0.0 || k >= len as f64 {
                    return Ok(Value::Undefined);
                }
                let key = self.index_key(k as u64)?;
                self.get_prop(this, key)
            }
            ArrFlat => self.ao_flat(this, a0, None),
            ArrFlatMap => self.ao_flat(this, Value::Undefined, Some((a0, a1))),
            ArrFindLast | ArrFindLastIndex => self.ao_find_last(f == ArrFindLastIndex, this, a0, a1),
            ArrLastIndexOf => self.ao_last_index_of(this, args),
            ArrReduceRight => self.ao_reduce_right(this, args),
            ArrCopyWithin => self.ao_copy_within(this, a0, a1, arg(args, 2)),
            ArrToReversed => self.ao_to_reversed(this),
            ArrToSorted => self.ao_to_sorted(this, a0),
            ArrToSpliced => self.ao_to_spliced(this, args),
            ArrWith => self.ao_with(this, a0, a1),
            _ => self.ao_to_locale_string(this, a0, a1),
        }
    }

    // ------------------------------------------------------------ helpers

    /// `ToObject` of the receiver for a method V8 words as "Cannot convert undefined or null to object".
    fn ao_cvt(&mut self, this: Value) -> Res<()> {
        if is_nullish(this) {
            return Err(self.type_err("Cannot convert undefined or null to object"));
        }
        Ok(())
    }

    /// V8's text for a callback that is not callable in the newer methods: `undefined`, `string "s"`, `number 1`, `object null`.
    fn ao_not_fn(&mut self, v: Value) -> Error {
        let t = match v {
            Value::Undefined => "undefined".to_string(),
            Value::Null => "object null".to_string(),
            Value::Bool(b) => format!("boolean {b}"),
            Value::Number(_) => format!("number {}", self.display(v)),
            Value::String(_) => format!("string \"{}\"", self.display(v)),
            Value::Symbol(_) => "symbol".to_string(),
            Value::Object(_) => "object".to_string(),
            Value::BigInt(_) | Value::BigHeap(_) => format!("bigint {}", self.display(v)),
        };
        self.type_err(&format!("{t} is not a function"))
    }

    fn ao_callable_v8(&mut self, f: Value) -> Res<()> {
        if self.is_callable(f) { Ok(()) } else { Err(self.ao_not_fn(f)) }
    }

    /// The array length of a copying method: RangeError above 2^32-1, as `ArrayCreate`.
    fn ao_copy_len(&mut self, len: u64) -> Res<()> {
        if len > u32::MAX as u64 {
            return Err(self.mk_err(RANGE_ERROR, "Invalid array length"));
        }
        Ok(())
    }

    /// A new plain array that holds `vals` and has length `len`.
    fn ao_array_of(&mut self, vals: Vec<Option<Value>>, len: u64) -> Res<Value> {
        let a = self.ao_new_array(0)?;
        if !h!(self, self.heap.fill_array(a, vals, len as u32)) {
            return Err(unsupported("array result", Span::default()));
        }
        Ok(Value::Object(a))
    }

    /// Every element of a plain dense array, a hole read as `undefined`, for the copying methods. `None` when `this` is not one.
    fn ao_dense_copy(&mut self, this: Value, len: u64) -> Res<Option<Vec<Option<Value>>>> {
        let Value::Object(o) = this else { return Ok(None) };
        if len > MAX_INDEX as u64 {
            return Ok(None);
        }
        self.ao_reserve(len / 8)?;
        self.ao_temp(len as usize, 16)?;
        let Some(e) = h!(self, self.heap.dense_elems(o, len)) else { return Ok(None) };
        let v: Vec<Option<Value>> = e.iter().map(|x| Some(x.unwrap_or(Value::Undefined))).collect();
        self.spend(len / 8 + 1)?;
        Ok(Some(v))
    }

    // ------------------------------------------------------------ reverse and copyWithin

    fn ao_reverse(&mut self, this: Value) -> Res<Value> {
        self.ao_cvt(this)?;
        let Value::Object(o) = this else { unreachable!("`ao_recv` made the receiver an object") };
        let len = self.length_of(this)?;
        if len > 1 && len <= MAX_INDEX as u64 && h!(self, self.heap.edit_dense(o, len as u32, |e| e.reverse())) {
            self.spend(len / 32 + 1)?;
            return Ok(this);
        }
        let middle = len / 2;
        self.ao_reserve(middle)?;
        for lower in 0..middle {
            self.tick()?;
            let upper = len - lower - 1;
            let (lk, uk) = (self.index_key(lower)?, self.index_key(upper)?);
            let lv = if self.ao_has(this, lk)? { Some(self.get_prop(this, lk)?) } else { None };
            let uv = if self.ao_has(this, uk)? { Some(self.get_prop(this, uk)?) } else { None };
            match (lv, uv) {
                (Some(l), Some(u)) => {
                    self.ao_set(this, lk, u)?;
                    self.ao_set(this, uk, l)?;
                }
                (None, Some(u)) => {
                    self.ao_set(this, lk, u)?;
                    self.ao_delete(this, uk)?;
                }
                (Some(l), None) => {
                    self.ao_delete(this, lk)?;
                    self.ao_set(this, uk, l)?;
                }
                (None, None) => {}
            }
        }
        Ok(this)
    }

    fn ao_copy_within(&mut self, this: Value, target: Value, start: Value, end: Value) -> Res<Value> {
        self.ao_cvt(this)?;
        let Value::Object(o) = this else { unreachable!("`ao_recv` made the receiver an object") };
        let len = self.length_of(this)?;
        let to = self.ao_rel(target, len, 0)?;
        let from = self.ao_rel(start, len, 0)?;
        let fin = self.ao_rel(end, len, len)?;
        let count = fin.saturating_sub(from).min(len - to);
        if count == 0 {
            return Ok(this);
        }
        if len <= MAX_INDEX as u64 && h!(self, self.heap.edit_dense(o, len as u32, |e| e.copy_within(from as usize..(from + count) as usize, to as usize))) {
            self.spend(count / 32 + 1)?;
            return Ok(this);
        }
        self.ao_reserve(count)?;
        // A forward copy would read what it has written when the ranges overlap and `to` is above `from`: copy backwards.
        if from < to && to < from + count {
            for i in (0..count).rev() {
                self.ao_move(this, from + i, to + i)?;
            }
        } else {
            for i in 0..count {
                self.ao_move(this, from + i, to + i)?;
            }
        }
        Ok(this)
    }

    // ------------------------------------------------------------ searching

    fn ao_find_last(&mut self, index: bool, this: Value, cb: Value, this_arg: Value) -> Res<Value> {
        self.ao_this(this, if index { "findLastIndex" } else { "findLast" })?;
        let len = self.length_of(this)?;
        self.ao_callable_v8(cb)?;
        for i in (0..len).rev() {
            self.tick()?;
            let k = self.index_key(i)?;
            let v = self.get_prop(this, k)?;
            let r = self.call_fn(cb, this_arg, &[v, Value::Number(i as f64), this])?;
            if self.truthy(r)? {
                return Ok(if index { Value::Number(i as f64) } else { v });
            }
        }
        Ok(if index { Value::Number(-1.0) } else { Value::Undefined })
    }

    fn ao_last_index_of(&mut self, this: Value, args: &[Value]) -> Res<Value> {
        self.ao_cvt(this)?;
        let len = self.length_of(this)?;
        let none = Value::Number(-1.0);
        if len == 0 {
            return Ok(none);
        }
        let x = arg(args, 0);
        let n = if args.len() > 1 { self.ao_int(args[1])? } else { len as f64 - 1.0 };
        let from = if n >= 0.0 { n.min(len as f64 - 1.0) } else { len as f64 + n };
        if from < 0.0 {
            return Ok(none);
        }
        let mut k = from as u64;
        if let Value::Object(o) = this
            && len <= MAX_INDEX as u64
        {
            self.ao_reserve((k + 1) / 8)?;
            if let Some(e) = h!(self, self.heap.dense_elems(o, len)) {
                let found = e[..=k as usize].iter().rposition(|y| matches!(y, Some(v) if v.strict_equals(x)));
                self.spend((k + 1) / 8 + 1)?;
                return Ok(found.map_or(none, |i| Value::Number(i as f64)));
            }
        }
        loop {
            self.tick()?;
            let key = self.index_key(k)?;
            if self.ao_has(this, key)? {
                let v = self.get_prop(this, key)?;
                if v.strict_equals(x) {
                    return Ok(Value::Number(k as f64));
                }
            }
            if k == 0 {
                return Ok(none);
            }
            k -= 1;
        }
    }

    fn ao_reduce_right(&mut self, this: Value, args: &[Value]) -> Res<Value> {
        self.ao_this(this, "reduceRight")?;
        let len = self.length_of(this)?;
        let cb = arg(args, 0);
        self.ao_callable_v8(cb)?;
        let mut k = len; // the next index to look at is `k - 1`
        let mut acc = if args.len() >= 2 {
            args[1]
        } else {
            loop {
                if k == 0 {
                    return Err(self.type_err("Reduce of empty array with no initial value"));
                }
                self.tick()?;
                k -= 1;
                let key = self.index_key(k)?;
                if self.ao_has(this, key)? {
                    break self.get_prop(this, key)?;
                }
            }
        };
        while k > 0 {
            self.tick()?;
            k -= 1;
            let key = self.index_key(k)?;
            if self.ao_has(this, key)? {
                let v = self.get_prop(this, key)?;
                acc = self.call_fn(cb, Value::Undefined, &[acc, v, Value::Number(k as f64), this])?;
            }
        }
        Ok(acc)
    }

    // ------------------------------------------------------------ flat and flatMap

    /// `flat(depth)` (`mapper` is `None`) and `flatMap(callback, thisArg)` (`mapper` is `(callback, thisArg)`, depth 1).
    fn ao_flat(&mut self, this: Value, depth_arg: Value, mapper: Option<(Value, Value)>) -> Res<Value> {
        self.ao_cvt(this)?;
        let len = self.length_of(this)?;
        let depth = match mapper {
            Some((f, _)) => {
                self.ao_callable_v8(f)?;
                1.0
            }
            None if matches!(depth_arg, Value::Undefined) => 1.0,
            None => self.ao_int(depth_arg)?.max(0.0),
        };
        let (a, plain) = self.ao_species_new(this, 0)?;
        let mut out: Vec<Option<Value>> = Vec::new();
        let mut fast = false;
        if let (Value::Object(o), None) = (this, mapper)
            && len <= MAX_INDEX as u64
        {
            let max = self.heap.limits().max_bytes / 4 / 16;
            let budget = self.step_limit.saturating_sub(self.steps).saturating_mul(8);
            let mut work = 0u64;
            if let FlatFast::Done = h!(self, flat_dense(&self.heap, o, len, depth, 0, &mut out, max, budget, &mut work)) {
                self.spend(work / 8 + 1)?;
                fast = true;
            } else {
                out.clear();
            }
        }
        if !fast {
            self.ao_flatten(&mut out, this, len, depth, mapper)?;
        }
        if plain {
            let n = out.len() as u32;
            if !h!(self, self.heap.fill_array(a, out, n)) {
                return Err(unsupported("array result", Span::default()));
            }
        } else {
            // A species result may be a proxy, a filled or a frozen array: CreateDataPropertyOrThrow for each element.
            for (i, v) in out.into_iter().enumerate() {
                if let Some(v) = v {
                    let k = self.index_key(i as u64)?;
                    self.ao_create(a, k, v)?;
                }
            }
        }
        Ok(Value::Object(a))
    }

    /// FlattenIntoArray. Nested arrays recurse through `descend`, so a cyclic array ends at the traversal-depth limit.
    fn ao_flatten(&mut self, out: &mut Vec<Option<Value>>, src: Value, len: u64, depth: f64, mapper: Option<(Value, Value)>) -> Res<()> {
        self.ao_reserve(len)?;
        for i in 0..len {
            self.tick()?;
            let k = self.index_key(i)?;
            if !self.ao_has(src, k)? {
                continue;
            }
            let mut el = self.get_prop(src, k)?;
            if let Some((f, t)) = mapper {
                el = self.call_fn(f, t, &[el, Value::Number(i as f64), src])?;
            }
            let nested = match el {
                Value::Object(c) if depth > 0.0 => self.o_is_array(c)?,
                _ => false,
            };
            if nested {
                let n = self.length_of(el)?;
                self.descend(|s| s.ao_flatten(out, el, n, depth - 1.0, None))?;
            } else {
                self.ao_temp(out.len() + 1, 16)?;
                out.push(Some(el));
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ the copying methods

    fn ao_to_reversed(&mut self, this: Value) -> Res<Value> {
        self.ao_cvt(this)?;
        let len = self.length_of(this)?;
        self.ao_copy_len(len)?;
        if let Some(mut v) = self.ao_dense_copy(this, len)? {
            v.reverse();
            return self.ao_array_of(v, len);
        }
        self.ao_reserve(len)?;
        self.ao_temp(len as usize, 16)?;
        let mut v = Vec::with_capacity(len as usize);
        for k in (0..len).rev() {
            self.tick()?;
            let key = self.index_key(k)?;
            v.push(Some(self.get_prop(this, key)?));
        }
        self.ao_array_of(v, len) // read from the last element to the first: that is the result order
    }

    fn ao_to_sorted(&mut self, this: Value, cmp: Value) -> Res<Value> {
        if !matches!(cmp, Value::Undefined) && !self.is_callable(cmp) {
            let t = self.ao_sort_text(cmp);
            return Err(self.type_err(&format!("The comparison function must be either a function or undefined: {t}")));
        }
        self.ao_cvt(this)?;
        let len = self.length_of(this)?;
        self.ao_copy_len(len)?;
        self.ao_reserve(len)?;
        self.ao_temp(len as usize, 16 + 8 + 4 + 4)?;
        let mut vals: Vec<Value> = Vec::with_capacity(len as usize);
        let mut undef = 0u64;
        match self.ao_dense_copy(this, len)? {
            Some(v) => {
                for x in v.into_iter().flatten() {
                    if matches!(x, Value::Undefined) { undef += 1 } else { vals.push(x) }
                }
            }
            None => {
                for i in 0..len {
                    self.tick()?;
                    let k = self.index_key(i)?;
                    let v = self.get_prop(this, k)?;
                    if matches!(v, Value::Undefined) { undef += 1 } else { vals.push(v) }
                }
            }
        }
        let idx = self.ao_merge_sort(cmp, &vals)?;
        let mut out: Vec<Option<Value>> = Vec::with_capacity(len as usize);
        out.extend(idx.iter().map(|&p| Some(vals[p as usize])));
        out.extend(std::iter::repeat_n(Some(Value::Undefined), undef as usize));
        self.spend(len / 8 + 1)?;
        self.ao_array_of(out, len)
    }

    /// The text of a bad comparator in the error of `sort` and `toSorted`: V8 shows an object as `#<Object>`.
    fn ao_sort_text(&mut self, v: Value) -> String {
        match v {
            Value::Object(o) if matches!(self.heap.is_array(o), Ok(true)) => "[object Array]".to_string(),
            Value::Object(_) => "#<Object>".to_string(),
            _ => self.display(v),
        }
    }

    fn ao_to_spliced(&mut self, this: Value, args: &[Value]) -> Res<Value> {
        self.ao_cvt(this)?;
        let len = self.length_of(this)?;
        let start = self.ao_rel(arg(args, 0), len, 0)?;
        let items = args.get(2..).unwrap_or(&[]);
        let skip = match args.len() {
            0 => 0,
            1 => len - start,
            _ => self.ao_int(args[1])?.max(0.0).min((len - start) as f64) as u64,
        };
        let new_len = len - skip + items.len() as u64;
        if new_len > MAX_SAFE_LEN {
            return Err(self.type_err("Invalid array length"));
        }
        self.ao_copy_len(new_len)?;
        self.ao_reserve(new_len)?;
        self.ao_temp(new_len as usize, 16)?;
        if let Some(mut v) = self.ao_dense_copy(this, len)? {
            v.splice(start as usize..(start + skip) as usize, items.iter().map(|&x| Some(x)));
            return self.ao_array_of(v, new_len);
        }
        let mut v: Vec<Option<Value>> = Vec::with_capacity(new_len as usize);
        for i in 0..start {
            self.tick()?;
            let k = self.index_key(i)?;
            v.push(Some(self.get_prop(this, k)?));
        }
        v.extend(items.iter().map(|&x| Some(x)));
        for r in start + skip..len {
            self.tick()?;
            let k = self.index_key(r)?;
            v.push(Some(self.get_prop(this, k)?));
        }
        self.ao_array_of(v, new_len)
    }

    fn ao_with(&mut self, this: Value, index: Value, value: Value) -> Res<Value> {
        self.ao_cvt(this)?;
        let len = self.length_of(this)?;
        let rel = self.ao_int(index)?;
        let actual = if rel >= 0.0 { rel } else { len as f64 + rel };
        if actual >= len as f64 || actual < 0.0 {
            return Err(self.mk_err(RANGE_ERROR, &format!("Invalid index : {}", number_to_string(rel))));
        }
        self.ao_copy_len(len)?;
        let actual = actual as u64;
        if let Some(mut v) = self.ao_dense_copy(this, len)? {
            v[actual as usize] = Some(value);
            return self.ao_array_of(v, len);
        }
        self.ao_reserve(len)?;
        self.ao_temp(len as usize, 16)?;
        let mut v: Vec<Option<Value>> = Vec::with_capacity(len as usize);
        for k in 0..len {
            self.tick()?;
            if k == actual {
                v.push(Some(value));
            } else {
                let key = self.index_key(k)?;
                v.push(Some(self.get_prop(this, key)?));
            }
        }
        self.ao_array_of(v, len)
    }

    // ------------------------------------------------------------ toLocaleString

    /// ECMA-402 `Array.prototype.toLocaleString`: each element that is not `undefined` or `null` gives
    /// `ToString(Invoke(element, "toLocaleString", « locales, options »))`, joined by ",".
    fn ao_to_locale_string(&mut self, this: Value, locales: Value, options: Value) -> Res<Value> {
        self.ao_cvt(this)?;
        let len = self.length_of(this)?;
        let k_tls = h!(self, self.heap.key_from_str("toLocaleString"));
        let comma = h!(self, self.heap.intern_str(","));
        let mut out: Vec<u16> = Vec::new();
        for i in 0..len {
            self.tick()?;
            if i > 0 {
                self.append_string(&mut out, h!(self, self.heap.str_units(comma)))?;
            }
            let key = self.index_key(i)?;
            let el = self.get_prop(this, key)?;
            if is_nullish(el) {
                continue;
            }
            let m = self.get_prop(el, k_tls)?;
            if !self.is_callable(m) {
                return Err(self.ao_not_fn(m));
            }
            let r = self.call_fn(m, el, &[locales, options])?;
            let s = self.coerce_string(r)?;
            self.append_string(&mut out, h!(self, self.heap.str_units(s)))?;
        }
        self.new_str(out)
    }
}
