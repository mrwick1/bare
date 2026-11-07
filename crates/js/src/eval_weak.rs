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
// - `WeakRef` and `FinalizationRegistry` hold a non-registered Symbol as a target or token only through the same
//   gate (`Unsupported`): bare does not collect symbols by reachability.
//
// WeakRef and FinalizationRegistry (ES2021)
// - `new WeakRef(target)`, `deref()`, `new FinalizationRegistry(cleanup)`, `register(target, held[, token])` and
//   `unregister(token)`, with V8's messages. A subclass is a gate, as for Map and Set.
// - A WeakRef keeps its target out of the roots. `weak_prune` clears the target of a ref whose target the collector
//   did not mark, so `deref()` then gives `undefined`. The spec's KeepDuringJob list is empty at every collection:
//   `collect` runs only between jobs (depth 0), and a job ends before the host collects.
// - A registry keeps its callback and the held values of all cells alive while the registry is reachable (a held value
//   that refers to its target keeps the target alive, as in the spec). Targets and tokens are weak. `weak_prune` sets
//   the target of a dead cell to `None`. The cell stays until `run_finalizers` or `unregister` removes it, as in the
//   spec, where `unregister` also removes a cell whose target is gone.
// - Cleanup callbacks: a collection never calls JavaScript. The host calls [`Interp::run_finalizers`] as a task after
//   a collection. It runs the callback of each dead cell once, with the held value, and reports a throw to the host.
//   A host that never calls it gets the "never run" behaviour that the spec allows. The registry is not rooted by its
//   pending cells: a registry that became unreachable is dropped with its cells and calls nothing, as in the spec.
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

/// One `register` call. `target` and `token` are weak: `None` after the collector found them dead (or no token).
struct Cell {
    target: Option<ObjId>,
    held: Value,
    token: Option<ObjId>,
}

struct Registry {
    cleanup: Value,
    cells: Vec<Cell>,
}

struct WeakState {
    colls: FxMap<ObjId, WeakColl>,
    /// WeakRef objects and their targets (`None` once collected).
    refs: FxMap<ObjId, Option<ObjId>>,
    regs: FxMap<ObjId, Registry>,
    ref_proto: ObjId,
    reg_proto: ObjId,
    ref_ctor: ObjId,
    reg_ctor: ObjId,
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
        WeakState { colls: FxMap::default(), refs: FxMap::default(), regs: FxMap::default(), ref_proto: o, reg_proto: o, ref_ctor: o, reg_ctor: o, total: 0, map_proto: o, set_proto: o, map_ctor: o, set_ctor: o, map_set: o, set_add: o, visits: Default::default() }
    }
}

impl ScFn {
    fn is_weak(&self) -> bool {
        matches!(self, ScFn::WeakMapCtor | ScFn::WeakMapGet | ScFn::WeakMapSet | ScFn::WeakMapHas | ScFn::WeakMapDelete | ScFn::WeakSetCtor | ScFn::WeakSetAdd | ScFn::WeakSetHas | ScFn::WeakSetDelete | ScFn::WeakRefCtor | ScFn::WeakRefDeref | ScFn::FinRegCtor | ScFn::FinRegRegister | ScFn::FinRegUnregister)
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
        out.extend([w.map_proto, w.set_proto, w.map_ctor, w.set_ctor, w.map_set, w.set_add, w.ref_proto, w.reg_proto, w.ref_ctor, w.reg_ctor].map(Value::Object));
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
        for (&o, &t) in &self.weak.refs {
            out.push(Value::Object(o));
            out.extend(t.map(Value::Object));
        }
        for (&o, r) in &self.weak.regs {
            out.extend([Value::Object(o), r.cleanup]);
            for c in &r.cells {
                out.push(c.held);
                out.extend(c.target.into_iter().chain(c.token).map(Value::Object));
            }
        }
    }

    /// Called when the collector has just marked `o`. Gives the work list the values `o` now makes reachable.
    fn weak_trace(&self, o: ObjId, seen: &Seen, pending: &mut FxMap<ObjId, Vec<Value>>, work: &mut Vec<Value>) {
        // A reachable registry keeps its callback and the held values of all its cells (also of dead ones).
        if !self.weak.regs.is_empty()
            && let Some(r) = self.weak.regs.get(&o)
        {
            work.push(r.cleanup);
            work.extend(r.cells.iter().map(|c| c.held));
        }
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
    fn weak_prune(&mut self, seen: &Seen) {
        let w = &mut self.weak;
        w.colls.retain(|o, _| seen.contains(o));
        w.refs.retain(|o, t| {
            if !seen.contains(o) {
                return false;
            }
            if t.is_some_and(|x| !seen.contains(&x)) {
                *t = None;
            }
            true
        });
        w.regs.retain(|o, _| seen.contains(o));
        w.total = 0;
        for r in w.regs.values_mut() {
            for c in &mut r.cells {
                c.target = c.target.filter(|x| seen.contains(x));
                c.token = c.token.filter(|x| seen.contains(x));
            }
            w.total += r.cells.len();
        }
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
        self.install_weak_refs()
    }

    fn install_weak_refs(&mut self) -> Res<()> {
        let op = self.object_proto;
        let rp = h!(self, self.heap.new_object(Some(op)));
        let rc = self.sc_native(ScFn::WeakRefCtor, "WeakRef", 1)?;
        self.sc_global("WeakRef", rc, rp)?;
        self.sc_method(rp, "deref", ScFn::WeakRefDeref, 0)?;
        self.sc_tag(rp, "WeakRef")?;
        let gp = h!(self, self.heap.new_object(Some(op)));
        let gc = self.sc_native(ScFn::FinRegCtor, "FinalizationRegistry", 1)?;
        self.sc_global("FinalizationRegistry", gc, gp)?;
        self.sc_method(gp, "register", ScFn::FinRegRegister, 2)?;
        self.sc_method(gp, "unregister", ScFn::FinRegUnregister, 1)?;
        self.sc_tag(gp, "FinalizationRegistry")?;
        let w = &mut self.weak;
        (w.ref_proto, w.reg_proto, w.ref_ctor, w.reg_ctor) = (rp, gp, rc, gc);
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
        if matches!(f, ScFn::WeakRefCtor | ScFn::WeakRefDeref | ScFn::FinRegCtor | ScFn::FinRegRegister | ScFn::FinRegUnregister) {
            return self.call_weak_ref(f, this, args, is_new);
        }
        let (a0, a1) = (arg(args, 0), arg(args, 1));
        let is_map = matches!(f, ScFn::WeakMapCtor | ScFn::WeakMapGet | ScFn::WeakMapSet | ScFn::WeakMapHas | ScFn::WeakMapDelete);
        match f {
            ScFn::WeakMapCtor | ScFn::WeakSetCtor => {
                if !is_new {
                    return Err(self.type_err(&format!("Constructor {} requires 'new'", if is_map { "WeakMap" } else { "WeakSet" })));
                }
                let nt = self.cls.native_nt.take();
                let proto = self.nt_proto(nt, if is_map { self.weak.map_proto } else { self.weak.set_proto })?;
                let o = self.weak_new(is_map, proto)?;
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

    // ------------------------------------------------------------ WeakRef and FinalizationRegistry

    /// A valid weak target or token. An object is. A non-registered Symbol is valid in the spec and is gated here.
    /// Everything else, a registered symbol too, is `None`.
    fn weak_target(&mut self, v: Value) -> Res<Option<ObjId>> {
        match v {
            Value::Object(o) => Ok(Some(o)),
            Value::Symbol(s) => {
                let d = h!(self, self.heap.symbol_description(s));
                if matches!(d, Some(d) if self.sc.registry.get(&d) == Some(&s)) {
                    return Ok(None);
                }
                Err(unsupported("non-registered Symbol as a WeakRef or FinalizationRegistry target", Span::default()))
            }
            _ => Ok(None),
        }
    }

    fn weak_room(&self, extra: usize) -> Res<()> {
        let w = &self.weak;
        if w.colls.len() + w.refs.len() + w.regs.len() >= self.sc.limits.max_weak_collections && extra == 0 {
            return Err(Error::Limit(LimitKind::Sc(ScLimit::WeakCollections)));
        }
        if w.total + extra > self.sc.limits.max_total_weak_entries {
            return Err(Error::Limit(LimitKind::Sc(ScLimit::WeakEntries)));
        }
        Ok(())
    }

    fn call_weak_ref(&mut self, f: ScFn, this: Value, args: &[Value], is_new: bool) -> Res<Value> {
        let (a0, a1) = (arg(args, 0), arg(args, 1));
        match f {
            ScFn::WeakRefCtor => {
                if !is_new {
                    return Err(self.type_err("Constructor WeakRef requires 'new'"));
                }
                let nt = self.cls.native_nt.take();
                let Some(t) = self.weak_target(a0)? else { return Err(self.type_err("WeakRef: invalid target")) };
                self.weak_room(0)?;
                let proto = self.nt_proto(nt, self.weak.ref_proto)?;
                let o = h!(self, self.heap.new_object(Some(proto)));
                self.weak.refs.insert(o, Some(t));
                Ok(Value::Object(o))
            }
            ScFn::WeakRefDeref => {
                if let Value::Object(o) = this
                    && let Some(&t) = self.weak.refs.get(&o)
                {
                    return Ok(t.map_or(Value::Undefined, Value::Object));
                }
                let t = self.display(this);
                Err(self.type_err(&format!("Method WeakRef.prototype.deref called on incompatible receiver {t}")))
            }
            ScFn::FinRegCtor => {
                if !is_new {
                    return Err(self.type_err("Constructor FinalizationRegistry requires 'new'"));
                }
                let nt = self.cls.native_nt.take();
                if !self.is_callable(a0) {
                    return Err(self.type_err("FinalizationRegistry: cleanup must be callable"));
                }
                self.weak_room(0)?;
                let proto = self.nt_proto(nt, self.weak.reg_proto)?;
                let o = h!(self, self.heap.new_object(Some(proto)));
                self.weak.regs.insert(o, Registry { cleanup: a0, cells: Vec::new() });
                Ok(Value::Object(o))
            }
            _ => {
                let name = if f == ScFn::FinRegRegister { "register" } else { "unregister" };
                let Some(r) = (match this {
                    Value::Object(o) if self.weak.regs.contains_key(&o) => Some(o),
                    _ => None,
                }) else {
                    let t = self.display(this);
                    return Err(self.type_err(&format!("Method FinalizationRegistry.prototype.{name} called on incompatible receiver {t}")));
                };
                if f == ScFn::FinRegRegister {
                    let Some(target) = self.weak_target(a0)? else { return Err(self.type_err("FinalizationRegistry.prototype.register: invalid target")) };
                    if matches!(a1, Value::Object(h) if h == target) {
                        return Err(self.type_err("FinalizationRegistry.prototype.register: target and holdings must not be same"));
                    }
                    let a2 = arg(args, 2);
                    let token = if matches!(a2, Value::Undefined) {
                        None
                    } else {
                        match self.weak_target(a2)? {
                            Some(t) => Some(t),
                            None => {
                                let t = self.display(a2);
                                return Err(self.type_err(&format!("Invalid unregisterToken ('{t}')")));
                            }
                        }
                    };
                    self.weak_room(1)?;
                    self.weak.regs.get_mut(&r).expect("registry").cells.push(Cell { target: Some(target), held: a1, token });
                    self.weak.total += 1;
                    return Ok(Value::Undefined);
                }
                let Some(token) = self.weak_target(a0)? else {
                    let t = self.display(a0);
                    return Err(self.type_err(&format!("Invalid unregisterToken ('{t}')")));
                };
                let cells = &mut self.weak.regs.get_mut(&r).expect("registry").cells;
                let n = cells.len();
                cells.retain(|c| c.token != Some(token));
                let gone = n - cells.len();
                self.weak.total -= gone;
                self.charge_steps(n as u64)?;
                Ok(Value::Bool(gone > 0))
            }
        }
    }

    /// Run the cleanup callbacks of the registry cells whose target a collection found dead. The host calls this as a
    /// task after [`Interp::collect`]. Each dead cell is removed and its callback runs once with the held value. A
    /// throw goes to `Host::report_exception`. Returns the number of callbacks run. Refused while a call is active.
    pub fn run_finalizers(&mut self) -> Result<usize, Error> {
        if self.depth != 0 {
            return Err(unsupported("run_finalizers during a call", Span::default()));
        }
        self.begin();
        let mut jobs: Vec<(Value, Value)> = Vec::new();
        for r in self.weak.regs.values_mut() {
            let cb = r.cleanup;
            r.cells.retain(|c| {
                let dead = c.target.is_none();
                if dead {
                    jobs.push((cb, c.held));
                }
                !dead
            });
        }
        self.weak.total -= jobs.len();
        let n = jobs.len();
        for (cb, held) in jobs {
            self.tick()?;
            self.in_job(|it| match it.call_fn(cb, Value::Undefined, &[held]) {
                Ok(_) => Ok(()),
                Err(e) => {
                    let v = thrown(e)?;
                    if let Some(h) = it.host.clone() {
                        h.report_exception(it, v);
                    }
                    Ok(())
                }
            })?;
        }
        Ok(n)
    }
}
