// Map and Set of the evaluator. Included by `eval.rs` (`include!`). The iterator plumbing and the caps are in `eval_symbols.rs`.
//
// Supported: `new Map(iterable)`, `new Set(iterable)` (the iterable goes through the real iterator protocol and the
// `set`/`add` method is looked up and called, as in the spec), `get/set/has/delete/clear/size/forEach/keys/values/entries`,
// `@@iterator`, `@@toStringTag`, map and set iterator objects (`next`, `@@iterator`, `@@toStringTag`), SameValueZero
// (`NaN` equals `NaN`, `+0` equals `-0`, `-0` is stored as `+0`), insertion order, and live iteration: an iterator
// or `forEach` sees entries added later, skips entries deleted before it reaches them, and sees an entry that was
// deleted and added again as a new, later entry. `clear` keeps iterators alive; entries added after it are visited.
// Gates: calling `Map` or `Set` without `new` is a TypeError. A subclass (`class extends Map`, `Reflect.construct` with
// a `newTarget`) needs `new.target`, which the evaluator does not have: the class worker must pass the prototype
// of `newTarget` into `coll_new`. `Map.groupBy` is absent. The set methods of ES2025 are in `eval_set_methods.rs`. WeakMap and WeakSet are in `eval_weak.rs`.
//
// Storage: one `Coll` per Map or Set object, in `sc.colls`, keyed by the object handle. `entries` is an append-only list.
// A deleted entry is a hole until a compaction. `index` finds a key in O(1). Each entry has a sequence number that only
// grows; an iterator remembers the sequence number it reached, not a position, so compaction and `clear` need no
// registry of live iterators. Metadata of an unreachable Map or Set is dropped by `Interp::collect`.

/// Hasher of the key index. `Fx` alone is not enough here: the bits of an integer-valued `f64` have their low bits
/// all zero, and the table picks a bucket from the low bits. The xor-shift folds the high bits down.
#[derive(Default)]
struct HHash(u64);
impl Hasher for HHash {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, b: &[u8]) {
        for &x in b {
            self.write_u64(x as u64);
        }
    }
    fn write_u32(&mut self, n: u32) {
        self.write_u64(n as u64);
    }
    fn write_u64(&mut self, n: u64) {
        let x = (self.0.rotate_left(5) ^ n).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        self.0 = x ^ (x >> 32);
    }
    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }
}
type HMap<K, V> = HashMap<K, V, BuildHasherDefault<HHash>>;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum HKey {
    Undef,
    Null,
    Bool(bool),
    Num(u64),
    Str(StrId),
    Sym(SymId),
    Obj(ObjId),
    Big(i64),
    BigHeap(BigId),
}

/// SameValueZero as a hash key: strings are interned, so a handle is the content; `NaN` has one key and `-0` is `+0`.
fn hkey(v: Value) -> HKey {
    match v {
        Value::Undefined => HKey::Undef,
        Value::Null => HKey::Null,
        Value::Bool(b) => HKey::Bool(b),
        Value::Number(n) => HKey::Num(if n.is_nan() { 0x7ff8_0000_0000_0000 } else if n == 0.0 { 0 } else { n.to_bits() }),
        Value::String(s) => HKey::Str(s),
        Value::Symbol(s) => HKey::Sym(s),
        Value::Object(o) => HKey::Obj(o),
        Value::BigInt(n) => HKey::Big(n),
        Value::BigHeap(b) => HKey::BigHeap(b),
    }
}

/// `-0` is stored as `+0` (SameValueZero).
fn norm_zero(v: Value) -> Value {
    if matches!(v, Value::Number(n) if n == 0.0) { Value::Number(0.0) } else { v }
}

struct Entry {
    seq: u64,
    key: Value,
    val: Value,
    live: bool,
}

struct Coll {
    is_map: bool,
    /// Ascending `seq`. Deleted entries stay (with `live == false`) until a compaction.
    entries: Vec<Entry>,
    index: HMap<HKey, u32>,
    live: usize,
    next_seq: u64,
    /// Changes when positions change (compaction, `clear`).
    epoch: u64,
}

impl Interp<'_> {
    // ------------------------------------------------------------ storage

    fn this_coll(&mut self, this: Value, is_map: bool, method: &str) -> Res<ObjId> {
        if let Value::Object(o) = this
            && matches!(self.sc.colls.get(&o), Some(c) if c.is_map == is_map)
        {
            return Ok(o);
        }
        let (kind, t) = (if is_map { "Map" } else { "Set" }, self.display(this));
        Err(self.type_err(&format!("Method {kind}.prototype.{method} called on incompatible receiver {t}")))
    }

    /// Make the Map or Set object. `proto` is the intrinsic prototype (a subclass passes its own).
    fn coll_new(&mut self, is_map: bool, proto: ObjId) -> Res<ObjId> {
        if self.sc.colls.len() >= self.sc.limits.max_collections {
            return Err(Error::Limit(LimitKind::Sc(ScLimit::Collections)));
        }
        let o = h!(self, self.heap.new_object(Some(proto)));
        self.sc.colls.insert(o, Coll { is_map, entries: Vec::new(), index: HMap::default(), live: 0, next_seq: 0, epoch: 0 });
        Ok(o)
    }

    fn coll_get(&self, o: ObjId, k: Value) -> Option<&Entry> {
        let c = self.sc.colls.get(&o)?;
        c.entries.get(*c.index.get(&hkey(k))? as usize)
    }

    fn coll_set(&mut self, o: ObjId, k: Value, v: Value) -> Res<()> {
        let k = norm_zero(k);
        let hk = hkey(k);
        let (max, max_total) = (self.sc.limits.max_entries.min(u32::MAX as usize), self.sc.limits.max_total_entries);
        let Some(c) = self.sc.colls.get_mut(&o) else { return Err(self.type_err("incompatible receiver")) };
        if let Some(&p) = c.index.get(&hk) {
            c.entries[p as usize].val = v;
            return Ok(());
        }
        let full = c.entries.len() >= max || self.sc.total_entries >= max_total;
        if full && c.live < c.entries.len() {
            self.coll_compact(o)?;
        }
        let total = self.sc.total_entries;
        let Some(c) = self.sc.colls.get_mut(&o) else { return Err(self.type_err("incompatible receiver")) };
        if c.entries.len() >= max {
            return Err(Error::Limit(LimitKind::Sc(ScLimit::Entries)));
        }
        if total >= max_total {
            return Err(Error::Limit(LimitKind::Sc(ScLimit::TotalEntries)));
        }
        c.index.insert(hk, c.entries.len() as u32);
        c.entries.push(Entry { seq: c.next_seq, key: k, val: v, live: true });
        c.next_seq += 1;
        c.live += 1;
        self.sc.total_entries += 1;
        Ok(())
    }

    fn coll_delete(&mut self, o: ObjId, k: Value) -> Res<bool> {
        let Some(c) = self.sc.colls.get_mut(&o) else { return Err(self.type_err("incompatible receiver")) };
        let Some(p) = c.index.remove(&hkey(k)) else { return Ok(false) };
        let e = &mut c.entries[p as usize];
        (e.live, e.key, e.val) = (false, Value::Undefined, Value::Undefined);
        c.live -= 1;
        if c.entries.len() >= 32 && c.live * 2 < c.entries.len() {
            self.coll_compact(o)?;
        }
        Ok(true)
    }

    fn coll_clear(&mut self, o: ObjId) -> Res<()> {
        let n = self.sc.colls.get(&o).map_or(0, |c| c.entries.len());
        self.charge_steps(n as u64)?;
        if let Some(c) = self.sc.colls.get_mut(&o) {
            c.entries = Vec::new();
            c.index = HMap::default();
            c.live = 0;
            c.epoch += 1;
            self.sc.total_entries -= n;
        }
        Ok(())
    }

    /// Drop the holes and rebuild the index. The work is charged: it is proportional to the entries moved.
    fn coll_compact(&mut self, o: ObjId) -> Res<()> {
        let n = self.sc.colls.get(&o).map_or(0, |c| c.entries.len());
        self.charge_steps(n as u64)?;
        let Some(c) = self.sc.colls.get_mut(&o) else { return Ok(()) };
        c.entries.retain(|e| e.live);
        c.index.clear();
        for (i, e) in c.entries.iter().enumerate() {
            c.index.insert(hkey(e.key), i as u32);
        }
        c.entries.shrink_to(c.entries.len() * 2);
        c.index.shrink_to(c.entries.len() * 2);
        c.epoch += 1;
        self.sc.total_entries -= n - c.entries.len();
        Ok(())
    }

    /// The next live entry at or after the cursor. Holes it skips are charged.
    fn coll_next(&mut self, o: ObjId, cur: &mut Cursor) -> Res<Option<(Value, Value)>> {
        let Some(c) = self.sc.colls.get(&o) else { return Ok(None) };
        let mut pos = if cur.epoch == c.epoch { cur.pos as usize } else { c.entries.partition_point(|e| e.seq < cur.next_seq) };
        let start = pos;
        while pos < c.entries.len() && !c.entries[pos].live {
            pos += 1;
        }
        let found = c.entries.get(pos).map(|e| (e.seq, e.key, e.val));
        let epoch = c.epoch;
        cur.epoch = epoch;
        cur.pos = pos as u32;
        if let Some((seq, ..)) = found {
            cur.pos = pos as u32 + 1;
            cur.next_seq = seq + 1;
        }
        self.charge_steps((pos - start) as u64)?;
        Ok(found.map(|(_, k, v)| (k, v)))
    }

    /// The value an iterator of kind `what` gives for an entry. A Set entry has `key == val`.
    fn coll_item(&mut self, what: IterWhat, k: Value, v: Value) -> Res<Value> {
        Ok(match what {
            IterWhat::Keys => k,
            IterWhat::Values => v,
            IterWhat::Entries => self.new_array_from(&[k, v])?,
        })
    }

    // ------------------------------------------------------------ natives

    fn call_coll(&mut self, f: ScFn, this: Value, args: &[Value], is_new: bool) -> Res<Value> {
        let (a0, a1) = (arg(args, 0), arg(args, 1));
        match f {
            ScFn::MapCtor | ScFn::SetCtor => {
                let is_map = f == ScFn::MapCtor;
                if !is_new {
                    return Err(self.type_err(&format!("Constructor {} requires 'new'", if is_map { "Map" } else { "Set" })));
                }
                let nt = self.cls.native_nt.take();
                let proto = self.nt_proto(nt, if is_map { self.sc.map_proto } else { self.sc.set_proto })?;
                let o = self.coll_new(is_map, proto)?;
                if !is_nullish(a0) {
                    self.coll_fill(o, is_map, a0)?;
                }
                Ok(Value::Object(o))
            }
            ScFn::MapGet | ScFn::MapHas | ScFn::MapDelete | ScFn::MapSet | ScFn::MapClear | ScFn::MapSize | ScFn::MapForEach | ScFn::MapKeys | ScFn::MapValues | ScFn::MapEntries | ScFn::MapIterNext => {
                self.map_or_set(f, true, this, a0, a1)
            }
            _ => self.map_or_set(f, false, this, a0, a1),
        }
    }

    fn map_or_set(&mut self, f: ScFn, is_map: bool, this: Value, a0: Value, a1: Value) -> Res<Value> {
        if matches!(f, ScFn::MapIterNext | ScFn::SetIterNext) {
            return self.coll_iter_next(this, is_map);
        }
        let o = self.this_coll(this, is_map, match f {
            ScFn::MapGet => "get",
            ScFn::MapSet => "set",
            ScFn::SetAdd => "add",
            ScFn::MapHas | ScFn::SetHas => "has",
            ScFn::MapDelete | ScFn::SetDelete => "delete",
            ScFn::MapClear | ScFn::SetClear => "clear",
            ScFn::MapSize | ScFn::SetSize => "size",
            ScFn::MapForEach | ScFn::SetForEach => "forEach",
            ScFn::MapKeys => "keys",
            ScFn::MapValues | ScFn::SetValues => "values",
            _ => "entries",
        })?;
        match f {
            ScFn::MapGet => Ok(self.coll_get(o, a0).map_or(Value::Undefined, |e| e.val)),
            ScFn::MapSet => {
                self.coll_set(o, a0, a1)?;
                Ok(this)
            }
            ScFn::SetAdd => {
                let k = norm_zero(a0);
                self.coll_set(o, k, k)?;
                Ok(this)
            }
            ScFn::MapHas | ScFn::SetHas => Ok(Value::Bool(self.coll_get(o, a0).is_some())),
            ScFn::MapDelete | ScFn::SetDelete => Ok(Value::Bool(self.coll_delete(o, a0)?)),
            ScFn::MapClear | ScFn::SetClear => {
                self.coll_clear(o)?;
                Ok(Value::Undefined)
            }
            ScFn::MapSize | ScFn::SetSize => Ok(Value::Number(self.sc.colls.get(&o).map_or(0, |c| c.live) as f64)),
            ScFn::MapForEach | ScFn::SetForEach => {
                if !self.is_callable(a0) {
                    let t = self.display(a0);
                    return Err(self.type_err(&format!("{t} is not a function")));
                }
                let mut cur = Cursor::default();
                while let Some((k, v)) = self.coll_next(o, &mut cur)? {
                    self.call_fn(a0, a1, &[if is_map { v } else { k }, k, this])?;
                }
                Ok(Value::Undefined)
            }
            ScFn::MapKeys => self.coll_iter(o, IterWhat::Keys),
            ScFn::MapValues | ScFn::SetValues => self.coll_iter(o, IterWhat::Values),
            _ => self.coll_iter(o, IterWhat::Entries),
        }
    }

    /// An iterator object over a Map or Set. Set entries have `val == key`, so Keys and Values agree.
    fn coll_iter(&mut self, o: ObjId, what: IterWhat) -> Res<Value> {
        let (is_map, epoch) = self.sc.colls.get(&o).map_or((true, 0), |c| (c.is_map, c.epoch));
        let proto = if is_map { self.sc.map_iter_proto } else { self.sc.set_iter_proto };
        self.sc_track_iter(proto, IterState::Coll { owner: o, what, cur: Cursor { epoch, ..Default::default() }, done: false })
    }

    fn coll_iter_next(&mut self, this: Value, is_map: bool) -> Res<Value> {
        let Value::Object(it) = this else { return Err(self.coll_bad_next(is_map)) };
        match self.coll_iter_step(it, is_map)? {
            None => self.sc_iter_result(Value::Undefined, true),
            Some(item) => self.sc_iter_result(item, false),
        }
    }

    fn coll_bad_next(&mut self, is_map: bool) -> Error {
        self.type_err(&format!("next method called on incompatible receiver {}", if is_map { "Map Iterator" } else { "Set Iterator" }))
    }

    /// One step of a Map or Set iterator object: the next item, or `None` when it is done.
    fn coll_iter_step(&mut self, it: ObjId, is_map: bool) -> Res<Option<Value>> {
        let Some(&IterState::Coll { owner, what, mut cur, done }) = self.sc.iters.get(&it) else { return Err(self.coll_bad_next(is_map)) };
        if !matches!(self.sc.colls.get(&owner), Some(c) if c.is_map == is_map) {
            return Err(self.coll_bad_next(is_map));
        }
        if done {
            return Ok(None);
        }
        let step = self.coll_next(owner, &mut cur)?;
        if let Some(IterState::Coll { cur: c, done: d, .. }) = self.sc.iters.get_mut(&it) {
            *c = cur;
            *d = step.is_none();
        }
        match step {
            None => Ok(None),
            Some((k, v)) => Ok(Some(self.coll_item(what, k, if is_map { v } else { k })?)),
        }
    }

    /// The constructor's iterable: each item is added with the `set` (`add`) method found on the new object.
    fn coll_fill(&mut self, o: ObjId, is_map: bool, iterable: Value) -> Res<()> {
        let adder = self.get_prop(Value::Object(o), if is_map { self.sc.k_set } else { self.sc.k_add })?;
        if !self.is_callable(adder) {
            let t = self.display(adder);
            return Err(self.type_err(&format!("'{t}' returned for property '{}' of object '#<{}>' is not a function", if is_map { "set" } else { "add" }, if is_map { "Map" } else { "Set" })));
        }
        let intrinsic = matches!(adder, Value::Object(a) if a == if is_map { self.sc.f_map_set } else { self.sc.f_set_add });
        let mut it = self.iter_start_val(iterable)?;
        while let Some(item) = self.iter_next(&mut it)? {
            let r = (|| -> Res<()> {
                let (k, v) = if is_map {
                    if !matches!(item, Value::Object(_)) {
                        let t = self.display(item);
                        return Err(self.type_err(&format!("Iterator value {t} is not an entry object")));
                    }
                    (self.get_prop(item, PropertyKey::Index(0))?, self.get_prop(item, PropertyKey::Index(1))?)
                } else {
                    (item, item)
                };
                // The intrinsic adder does nothing observable, so it is called directly.
                if intrinsic {
                    self.tick()?;
                    let k = norm_zero(k);
                    return self.coll_set(o, k, if is_map { v } else { k });
                }
                if is_map { self.call_fn(adder, Value::Object(o), &[k, v]) } else { self.call_fn(adder, Value::Object(o), &[k]) }.map(|_| ())
            })();
            if let Err(e) = r {
                return Err(self.iter_close_throw(&mut it, e));
            }
        }
        Ok(())
    }
}
