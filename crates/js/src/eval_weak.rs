// WeakMap and WeakSet of the evaluator. Included by `eval.rs` (`include!`). Symbols, the iterator protocol and the install
// helpers are in `eval_symbols.rs`; the collector hooks are in `Interp::collect` (`eval.rs`).
//
// Supported: `new WeakMap(iterable)`, `new WeakSet(iterable)` (the iterable goes through the real iterator protocol and
// the `set`/`add` method is looked up and called), `WeakMap.prototype.{get,set,has,delete}`, `WeakSet.prototype.{add,has,delete}`,
// `@@toStringTag`, branding (a method on another receiver is a TypeError). `get`, `has` and `delete` of a non-object
// are `undefined` or `false`. `set` and `add` of a primitive are a TypeError.
// Gates (never silent)
// - A non-registered Symbol (a well-known symbol too) is a valid weak key in Node 22. bare does not collect symbols by
//   reachability from a key, so `set` and `add` of one stop with `Unsupported`, which script cannot catch. A registered
//   symbol is a TypeError, as in the spec. `has`, `get` and `delete` of any symbol are `undefined` or `false`: none is stored.
// - A subclass (`class extends WeakMap`) is refused with `Unsupported` when constructed, like Map and Set: the constructor
//   has no `new.target` yet (`ScFn::is_ctor`, `native_no_nt`).
// - `WeakRef` and `FinalizationRegistry` are absent.
//
// Storage: one `WeakColl` per object in `weak.colls`, keyed by the object handle. A key is an `ObjId`. Handles carry a
// generation, so a freed slot that is reused can never match an old key. `Interp::collect` does not initially root keys or values. The public
// `roots` method conservatively lists every stored value; `collect` calls it only after weak entries are pruned.
//
// Collector: ephemerons. `Interp::collect` calls `weak_trace` for every object it marks. If that object is a WeakMap, each
// entry whose key is already marked gives its value to the work list, and each other entry waits in `pending[key]`. If that
// object is a key with entries waiting, those values are released. Every entry is visited once when its map is marked and
// released at most once, so the work is linear in the entries of the reachable maps, in any marking order. Chains of
// maps reach a fixed point because the loop only ends when the work list is empty. `weak_prune` then drops every entry
// whose key was not marked, and every unreachable map, before the heap sweep. A WeakSet has no value to trace.

/// What `Interp::collect` keeps: the keys of a WeakMap and WeakSet are never roots.
struct WeakColl {
    is_map: bool,
    map: FxMap<ObjId, Value>,
}

struct WeakState {
    colls: FxMap<ObjId, WeakColl>,
    /// Entries of all weak collections.
    total: usize,
    map_proto: ObjId,
    set_proto: ObjId,
    map_ctor: ObjId,
    set_ctor: ObjId,
    /// The intrinsic `set` and `add`, so the constructor can skip the call.
    map_set: ObjId,
    set_add: ObjId,
    /// Entries looked at by the collector (maps marked plus values released), for tests of the linear bound.
    visits: std::cell::Cell<u64>,
}

impl WeakState {
    fn new(o: ObjId) -> WeakState {
        WeakState { colls: FxMap::default(), total: 0, map_proto: o, set_proto: o, map_ctor: o, set_ctor: o, map_set: o, set_add: o, visits: Default::default() }
    }
}

impl ScFn {
    fn is_weak(&self) -> bool {
        matches!(self, ScFn::WeakMapCtor | ScFn::WeakMapGet | ScFn::WeakMapSet | ScFn::WeakMapHas | ScFn::WeakMapDelete | ScFn::WeakSetCtor | ScFn::WeakSetAdd | ScFn::WeakSetHas | ScFn::WeakSetDelete)
    }
}

impl<'p> Interp<'p> {
    /// Weak Maps and Sets the evaluator tracks and their entries (until `collect` drops the dead ones).
    pub fn weak_tracked(&self) -> (usize, usize) {
        (self.weak.colls.len(), self.weak.total)
    }

    /// Entries the collector has looked at in all collections so far.
    pub fn weak_visits(&self) -> u64 {
        self.weak.visits.get()
    }

    // ------------------------------------------------------------ GC

    /// Intrinsics that must outlive deleted globals.
    fn weak_roots(&self, out: &mut Vec<Value>) {
        let w = &self.weak;
        out.extend([w.map_proto, w.set_proto, w.map_ctor, w.set_ctor, w.map_set, w.set_add].map(Value::Object));
    }

    /// Conservative storage roots for direct heap collection. `Interp::collect` calls this after weak pruning.
    fn weak_all_values(&self, out: &mut Vec<Value>) {
        for (&o, c) in &self.weak.colls {
            out.push(Value::Object(o));
            for (&k, &v) in &c.map {
                out.push(Value::Object(k));
                out.push(v);
            }
        }
    }

    /// Called when the collector has just marked `o`. Gives the work list the values `o` now makes reachable.
    fn weak_trace(&self, o: ObjId, seen: &FxSet<ObjId>, pending: &mut FxMap<ObjId, Vec<Value>>, work: &mut Vec<Value>) {
        if let Some(c) = self.weak.colls.get(&o).filter(|c| c.is_map) {
            self.weak.visits.set(self.weak.visits.get() + c.map.len() as u64);
            for (&k, &v) in &c.map {
                if seen.contains(&k) {
                    work.push(v);
                } else {
                    pending.entry(k).or_default().push(v);
                }
            }
        }
        if !pending.is_empty()
            && let Some(vs) = pending.remove(&o)
        {
            self.weak.visits.set(self.weak.visits.get() + vs.len() as u64);
            work.extend(vs);
        }
    }

    /// Drop unreachable collections and the entries whose key was not marked.
    fn weak_prune(&mut self, seen: &FxSet<ObjId>) {
        let w = &mut self.weak;
        w.colls.retain(|o, _| seen.contains(o));
        w.total = 0;
        for c in w.colls.values_mut() {
            let n = c.map.len();
            c.map.retain(|k, _| seen.contains(k));
            if c.map.len() * 4 < n {
                c.map.shrink_to_fit();
            }
            w.total += c.map.len();
        }
    }

    // ------------------------------------------------------------ install

    fn install_weak(&mut self) -> Res<()> {
        let op = self.object_proto;
        for is_map in [true, false] {
            let name = if is_map { "WeakMap" } else { "WeakSet" };
            let proto = h!(self, self.heap.new_object(Some(op)));
            let ctor = self.sc_native(if is_map { ScFn::WeakMapCtor } else { ScFn::WeakSetCtor }, name, 0)?;
            self.sc_global(name, ctor, proto)?;
            let methods: &[(&str, ScFn, u32)] = if is_map {
                &[("delete", ScFn::WeakMapDelete, 1), ("get", ScFn::WeakMapGet, 1), ("set", ScFn::WeakMapSet, 2), ("has", ScFn::WeakMapHas, 1)]
            } else {
                &[("delete", ScFn::WeakSetDelete, 1), ("has", ScFn::WeakSetHas, 1), ("add", ScFn::WeakSetAdd, 1)]
            };
            let mut adder = proto;
            for &(n, f, l) in methods {
                let o = self.sc_method(proto, n, f, l)?;
                if matches!(f, ScFn::WeakMapSet | ScFn::WeakSetAdd) {
                    adder = o;
                }
            }
            self.sc_tag(proto, name)?;
            let w = &mut self.weak;
            if is_map {
                (w.map_proto, w.map_ctor, w.map_set) = (proto, ctor, adder);
            } else {
                (w.set_proto, w.set_ctor, w.set_add) = (proto, ctor, adder);
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ natives

    fn this_weak(&mut self, this: Value, is_map: bool, method: &str) -> Res<ObjId> {
        if let Value::Object(o) = this
            && matches!(self.weak.colls.get(&o), Some(c) if c.is_map == is_map)
        {
            return Ok(o);
        }
        let (kind, t) = (if is_map { "WeakMap" } else { "WeakSet" }, self.display(this));
        Err(self.type_err(&format!("Method {kind}.prototype.{method} called on incompatible receiver {t}")))
    }

    /// The object a weak entry is stored under. A primitive is a TypeError. A non-registered symbol is valid in the
    /// spec and is gated here (see the header).
    fn weak_key(&mut self, v: Value, is_map: bool) -> Res<ObjId> {
        match v {
            Value::Object(o) => return Ok(o),
            Value::Symbol(s) => {
                let d = h!(self, self.heap.symbol_description(s));
                if !matches!(d, Some(d) if self.sc.registry.get(&d) == Some(&s)) {
                    return Err(unsupported("non-registered Symbol as a weak key", Span::default()));
                }
            }
            _ => {}
        }
        let t = self.display(v);
        Err(self.type_err(&if is_map { format!("Invalid value used as weak map key: {t}") } else { format!("Invalid value used in weak set: {t}") }))
    }

    fn weak_new(&mut self, is_map: bool, proto: ObjId) -> Res<ObjId> {
        if self.weak.colls.len() >= self.sc.limits.max_weak_collections {
            return Err(Error::Limit(LimitKind::Sc(ScLimit::WeakCollections)));
        }
        let o = h!(self, self.heap.new_object(Some(proto)));
        self.weak.colls.insert(o, WeakColl { is_map, map: FxMap::default() });
        Ok(o)
    }

    fn weak_put(&mut self, o: ObjId, k: ObjId, v: Value) -> Res<()> {
        let (max, max_total, total) = (self.sc.limits.max_weak_entries, self.sc.limits.max_total_weak_entries, self.weak.total);
        let Some(c) = self.weak.colls.get_mut(&o) else { return Err(self.type_err("incompatible receiver")) };
        if let Some(slot) = c.map.get_mut(&k) {
            *slot = v;
            return Ok(());
        }
        if c.map.len() >= max || total >= max_total {
            return Err(Error::Limit(LimitKind::Sc(ScLimit::WeakEntries)));
        }
        c.map.insert(k, v);
        self.weak.total += 1;
        Ok(())
    }

    fn call_weak(&mut self, f: ScFn, this: Value, args: &[Value], is_new: bool) -> Res<Value> {
        let (a0, a1) = (arg(args, 0), arg(args, 1));
        let is_map = matches!(f, ScFn::WeakMapCtor | ScFn::WeakMapGet | ScFn::WeakMapSet | ScFn::WeakMapHas | ScFn::WeakMapDelete);
        match f {
            ScFn::WeakMapCtor | ScFn::WeakSetCtor => {
                if !is_new {
                    return Err(self.type_err(&format!("Constructor {} requires 'new'", if is_map { "WeakMap" } else { "WeakSet" })));
                }
                let o = self.weak_new(is_map, if is_map { self.weak.map_proto } else { self.weak.set_proto })?;
                if !is_nullish(a0) {
                    self.weak_fill(o, is_map, a0)?;
                }
                Ok(Value::Object(o))
            }
            _ => {
                let o = self.this_weak(this, is_map, match f {
                    ScFn::WeakMapGet => "get",
                    ScFn::WeakMapSet => "set",
                    ScFn::WeakSetAdd => "add",
                    ScFn::WeakMapHas | ScFn::WeakSetHas => "has",
                    _ => "delete",
                })?;
                self.tick()?;
                match f {
                    ScFn::WeakMapSet | ScFn::WeakSetAdd => {
                        let k = self.weak_key(a0, is_map)?;
                        self.weak_put(o, k, if is_map { a1 } else { Value::Undefined })?;
                        Ok(this)
                    }
                    _ => {
                        // A primitive is never stored, so it is absent. A symbol cannot be stored either (gated).
                        let Value::Object(k) = a0 else { return Ok(if matches!(f, ScFn::WeakMapGet) { Value::Undefined } else { Value::Bool(false) }) };
                        let c = self.weak.colls.get_mut(&o).expect("branded");
                        Ok(match f {
                            ScFn::WeakMapGet => c.map.get(&k).copied().unwrap_or(Value::Undefined),
                            ScFn::WeakMapHas | ScFn::WeakSetHas => Value::Bool(c.map.contains_key(&k)),
                            _ => {
                                let gone = c.map.remove(&k).is_some();
                                self.weak.total -= gone as usize;
                                Value::Bool(gone)
                            }
                        })
                    }
                }
            }
        }
    }

    /// The constructor's iterable, as in `coll_fill`: each item goes to the `set` (`add`) method found on the new object.
    fn weak_fill(&mut self, o: ObjId, is_map: bool, iterable: Value) -> Res<()> {
        let adder = self.get_prop(Value::Object(o), if is_map { self.sc.k_set } else { self.sc.k_add })?;
        if !self.is_callable(adder) {
            let t = self.display(adder);
            let (m, k) = if is_map { ("set", "WeakMap") } else { ("add", "WeakSet") };
            return Err(self.type_err(&format!("'{t}' returned for property '{m}' of object '#<{k}>' is not a function")));
        }
        let intrinsic = matches!(adder, Value::Object(a) if a == if is_map { self.weak.map_set } else { self.weak.set_add });
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
                    (item, Value::Undefined)
                };
                // The intrinsic adder does nothing observable, so it is called directly.
                if intrinsic {
                    self.tick()?;
                    let k = self.weak_key(k, is_map)?;
                    return self.weak_put(o, k, v);
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
