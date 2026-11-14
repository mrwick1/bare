// The Set methods of ES2025: `union`, `intersection`, `difference`, `symmetricDifference`, `isSubsetOf`,
// `isSupersetOf` and `isDisjointFrom`. Included by `eval.rs` (`include!`). The storage is in `eval_collections.rs`.
//
// The argument is a "set-like" object: GetSetRecord reads `size`, `has` and `keys` once, in that order. `size` goes
// through ToNumber (NaN is a TypeError, a negative integer a RangeError). `has` and `keys` must be callable.
// The result is a new `Set` with `%Set.prototype%`: the species of the receiver is not used, as in the spec.
//
// Order and mutation follow the spec. A method that walks the receiver (`intersection`, `difference`, `isSubsetOf`,
// `isDisjointFrom` when the receiver is not larger than the argument) uses a cursor over the receiver, so an entry
// that `has` adds is visited and an entry it deletes is skipped. A method that walks the argument calls `keys()`,
// reads `next` once and closes the iterator when it stops early.
//
// Speed: when the argument is a real Set or Map whose `has` (or `keys`) is the intrinsic one, the method calls the
// intrinsic directly. The call has no side effect, so nothing observable changes. `keys` also needs the iterator
// prototype to be intact, as `iter_open` checks. Then no iterator object and no result object is made. The result
// starts as a copy of the receiver's entries (`coll_clone`): a memcpy of the entry list and of the index when the
// receiver has no deleted entries.
//
// `Set.prototype` of a subclass instance is not read: a subclass of `Set` is a gate of the constructor already.

/// GetSetRecord: the argument and the three values read from it.
struct SetRec {
    obj: Value,
    size: f64,
    has: Value,
    keys: Value,
    /// `obj` is a real Set or Map and `has` is its intrinsic `has`.
    fast_has: Option<ObjId>,
    /// `obj` is a real Set or Map and `keys` is its intrinsic `keys` (Set `values` is `keys`).
    fast_keys: Option<ObjId>,
}

impl ScFn {
    fn is_set_op(&self) -> bool {
        use ScFn::*;
        matches!(self, SetUnion | SetIntersection | SetDifference | SetSymDiff | SetIsSubsetOf | SetIsSupersetOf | SetIsDisjointFrom)
    }
}

impl<'p> Interp<'p> {
    fn install_set_methods(&mut self, tproto: ObjId) -> Res<()> {
        for (n, f) in [
            ("union", ScFn::SetUnion),
            ("intersection", ScFn::SetIntersection),
            ("difference", ScFn::SetDifference),
            ("symmetricDifference", ScFn::SetSymDiff),
            ("isSubsetOf", ScFn::SetIsSubsetOf),
            ("isSupersetOf", ScFn::SetIsSupersetOf),
            ("isDisjointFrom", ScFn::SetIsDisjointFrom),
        ] {
            self.sc_method(tproto, n, f, 1)?;
        }
        Ok(())
    }

    fn set_record(&mut self, other: Value, method: &str) -> Res<SetRec> {
        let Value::Object(oo) = other else { return Err(self.type_err(&format!("Set.prototype.{method} argument must be an object"))) };
        let ks = h!(self, self.heap.key_from_str("size"));
        let raw = self.get_prop(other, ks)?;
        let num = self.coerce_number(raw)?;
        if num.is_nan() {
            return Err(self.type_err("The .size property is NaN"));
        }
        let size = num.trunc();
        if size < 0.0 {
            return Err(self.mk_err(RANGE_ERROR, &format!("'{}' is an invalid size", number_to_string(size))));
        }
        let kh = h!(self, self.heap.key_from_str("has"));
        let has = self.get_prop(other, kh)?;
        if !self.is_callable(has) {
            return Err(self.type_err("string \"has\" is not a function"));
        }
        let kk = h!(self, self.heap.key_from_str("keys"));
        let keys = self.get_prop(other, kk)?;
        if !self.is_callable(keys) {
            return Err(self.type_err("string \"keys\" is not a function"));
        }
        let (mut fast_has, mut fast_keys) = (None, None);
        if let (Some(c), Value::Object(h), Value::Object(k)) = (self.sc.colls.get(&oo), has, keys) {
            let s = &self.sc;
            fast_has = (h == if c.is_map { s.f_map_has } else { s.f_set_has }).then_some(oo);
            fast_keys = (k == if c.is_map { s.f_map_keys } else { s.f_set_values }).then_some(oo);
        }
        Ok(SetRec { obj: other, size, has, keys, fast_has, fast_keys })
    }

    /// `ToBoolean(Call(rec.has, rec.obj, « v »))`.
    fn rec_has(&mut self, rec: &SetRec, v: Value) -> Res<bool> {
        if let Some(o) = rec.fast_has {
            self.tick()?;
            return Ok(self.coll_get(o, v)?.is_some());
        }
        let r = self.call_fn(rec.has, rec.obj, &[v])?;
        self.truthy(r)
    }

    /// `GetIteratorFromMethod(rec.obj, rec.keys)`.
    fn rec_keys(&mut self, rec: &SetRec) -> Res<It> {
        if let Some(o) = rec.fast_keys {
            let c = &self.sc.colls[&o];
            let (is_map, epoch) = (c.is_map, c.epoch);
            let (proto, next) = if is_map { (self.sc.map_iter_proto, self.sc.f_map_next) } else { (self.sc.set_iter_proto, self.sc.f_set_next) };
            if self.sc_proto_intact(proto, next)? {
                return Ok(It { src: rec.obj, i: 0, kind: ItKind::Coll { owner: o, what: IterWhat::Keys, cur: Cursor { epoch, ..Default::default() } }, done: false });
            }
        }
        let iter = self.call_fn(rec.keys, rec.obj, &[])?;
        if !matches!(iter, Value::Object(_)) {
            return Err(self.type_err("Result of the keys method is not an object"));
        }
        let next = self.get_prop(iter, self.sc.k_next)?;
        Ok(it_direct(iter, next))
    }

    /// A new Set with the live entries of `o`.
    fn coll_clone(&mut self, o: ObjId) -> Res<ObjId> {
        let (n, holes) = {
            let c = &self.sc.colls[&o];
            (c.live, c.live != c.entries.len())
        };
        self.charge_steps(n as u64)?;
        if n > self.sc.limits.max_entries || self.sc.total_entries.saturating_add(n) > self.sc.limits.max_total_entries {
            return Err(Error::Limit(LimitKind::Sc(if n > self.sc.limits.max_entries { ScLimit::Entries } else { ScLimit::TotalEntries })));
        }
        let r = self.coll_new(false, self.sc.set_proto)?;
        let src = &self.sc.colls[&o];
        let (entries, index, next_seq) = if holes {
            let entries: Vec<Entry> = src.entries.iter().filter(|e| e.live).map(|e| Entry { seq: e.seq, key: e.key, val: e.val, live: true }).collect();
            let mut index = HMap::default();
            index.reserve(n);
            for (i, e) in entries.iter().enumerate() {
                index.insert(hkey(e.key), i as u32);
            }
            (entries, index, src.next_seq)
        } else {
            (src.entries.iter().map(|e| Entry { seq: e.seq, key: e.key, val: e.val, live: true }).collect(), src.index.clone(), src.next_seq)
        };
        if let Some(c) = self.sc.colls.get_mut(&r) {
            c.entries = entries;
            c.index = index;
            c.live = n;
            c.next_seq = next_seq;
        }
        self.sc.total_entries += n;
        Ok(r)
    }

    fn call_set_op(&mut self, f: ScFn, this: Value, other: Value) -> Res<Value> {
        let name = match f {
            ScFn::SetUnion => "union",
            ScFn::SetIntersection => "intersection",
            ScFn::SetDifference => "difference",
            ScFn::SetSymDiff => "symmetricDifference",
            ScFn::SetIsSubsetOf => "isSubsetOf",
            ScFn::SetIsSupersetOf => "isSupersetOf",
            _ => "isDisjointFrom",
        };
        let o = self.this_coll(this, false, name)?;
        let rec = self.set_record(other, name)?;
        let live = |it: &Self| it.sc.colls.get(&o).map_or(0, |c| c.live) as f64;
        match f {
            ScFn::SetUnion => {
                let mut it = self.rec_keys(&rec)?;
                let r = self.coll_clone(o)?;
                while let Some(k) = self.iter_next(&mut it)? {
                    let k = norm_zero(k);
                    self.coll_set(r, k, k)?;
                }
                Ok(Value::Object(r))
            }
            ScFn::SetIntersection => {
                let r = self.coll_new(false, self.sc.set_proto)?;
                if live(self) <= rec.size {
                    let mut cur = Cursor::default();
                    while let Some((k, _)) = self.coll_next(o, &mut cur)? {
                        if self.rec_has(&rec, k)? {
                            self.coll_set(r, k, k)?;
                        }
                    }
                } else {
                    let mut it = self.rec_keys(&rec)?;
                    while let Some(k) = self.iter_next(&mut it)? {
                        if self.coll_get(o, k)?.is_some() {
                            let k = norm_zero(k);
                            self.coll_set(r, k, k)?;
                        }
                    }
                }
                Ok(Value::Object(r))
            }
            ScFn::SetDifference => {
                let r = self.coll_clone(o)?;
                if live(self) <= rec.size {
                    let mut cur = Cursor::default();
                    while let Some((k, _)) = self.coll_next(o, &mut cur)? {
                        if self.rec_has(&rec, k)? {
                            self.coll_delete(r, k)?;
                        }
                    }
                } else {
                    let mut it = self.rec_keys(&rec)?;
                    while let Some(k) = self.iter_next(&mut it)? {
                        self.coll_delete(r, k)?;
                    }
                }
                Ok(Value::Object(r))
            }
            ScFn::SetSymDiff => {
                let mut it = self.rec_keys(&rec)?;
                let r = self.coll_clone(o)?;
                while let Some(k) = self.iter_next(&mut it)? {
                    if self.coll_get(o, k)?.is_some() {
                        self.coll_delete(r, k)?;
                    } else {
                        let k = norm_zero(k);
                        self.coll_set(r, k, k)?;
                    }
                }
                Ok(Value::Object(r))
            }
            ScFn::SetIsSubsetOf => {
                if live(self) > rec.size {
                    return Ok(Value::Bool(false));
                }
                let mut cur = Cursor::default();
                while let Some((k, _)) = self.coll_next(o, &mut cur)? {
                    if !self.rec_has(&rec, k)? {
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            }
            ScFn::SetIsSupersetOf => {
                if live(self) < rec.size {
                    return Ok(Value::Bool(false));
                }
                let mut it = self.rec_keys(&rec)?;
                while let Some(k) = self.iter_next(&mut it)? {
                    if self.coll_get(o, k)?.is_none() {
                        self.iter_close(&mut it)?;
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            }
            _ => {
                if live(self) <= rec.size {
                    let mut cur = Cursor::default();
                    while let Some((k, _)) = self.coll_next(o, &mut cur)? {
                        if self.rec_has(&rec, k)? {
                            return Ok(Value::Bool(false));
                        }
                    }
                } else {
                    let mut it = self.rec_keys(&rec)?;
                    while let Some(k) = self.iter_next(&mut it)? {
                        if self.coll_get(o, k)?.is_some() {
                            self.iter_close(&mut it)?;
                            return Ok(Value::Bool(false));
                        }
                    }
                }
                Ok(Value::Bool(true))
            }
        }
    }
}
