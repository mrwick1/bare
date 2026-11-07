// `Promise.all`, `allSettled`, `race`, `any` and `AggregateError`. Included by `eval.rs` (`include!`), so it shares that
// module's imports and private items. The Promise core is in `eval_promise.rs`.
//
// Supported on the intrinsic `Promise` only (`intrinsic_this`: another `this` is an explicit `Unsupported`, a primitive is a
// `TypeError`). The steps are the ones of the spec:
// - `C.resolve` is read once, before the iteration (`GetPromiseResolve`), and called with `this = C` for every item.
// - The iterable is read with the shared iterator engine (`iter_open`, `iter_next`). An ordinary error of the iterator,
//   of `resolve`, of a getter or of `then` rejects the result promise. `IteratorClose` runs for an error of `resolve`
//   or of `then`, not for an error of `next`. A limit, `Unsupported` or a heap fault ends the call at once and runs no
//   more JavaScript (no close, no reject).
// - Each item gets `then(onFulfilled, onRejected)` through `Invoke`, so a thenable and a promise with its own `then`
//   both work. Intrinsic Promise reactions run as jobs. An overridden `resolve` can supply a synchronous `then`.
//   Empty `all`, `allSettled` and `any` settle before returning; their reactions still run as jobs.
// - Order is the input order. The element functions have a once flag per item (`allSettled`: one flag for the pair).
//   The result promise is settled through its own resolving pair, so its "already resolved" flag is shared with the
//   functions that `race` hands to `then`.
// - `any` rejects with an `AggregateError` (message "All promises were rejected", own `errors` array). The global
//   `AggregateError(errors, message)` is installed too: `errors` is read as an iterable (bounded by `max_iter_buffer`),
//   `options.cause` is installed before reading the errors iterable, and a class cannot extend it.
//
// State and GC. The "remaining" counter, the values and the once flags of one call live in a record in
// `promise.agg.recs`, keyed by a counter that is never reused. An element function holds only that key and its index.
// `pfn_trace` makes the record's values and result promise reachable from the function, and `agg_prune` drops the
// records that no live element function names. So an unreachable cycle (record -> value -> promise -> reaction ->
// function -> record) is freed, and a stale key can never meet a new record.
//
// Caps ([`AggLimits`], each is a distinct `Error::Limit(LimitKind::Agg(_))`, checked before the thing grows):
// live records, slots of all live records together, items one call may take. Work is charged: one step per item
// (the iterator), one for the item's own bookkeeping, one per element of a result array.

/// Which cap of the combinators ended a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggLimit {
    /// Live aggregate records.
    Records,
    /// Slots of all live records together.
    Items,
    /// Items one combinator call may take from its iterable.
    ItemsPerCall,
}

#[derive(Clone, Copy, Debug)]
pub struct AggLimits {
    pub max_records: usize,
    pub max_items: usize,
    pub max_items_per_call: usize,
}

impl Default for AggLimits {
    fn default() -> Self {
        AggLimits { max_records: 1 << 16, max_items: 1 << 20, max_items_per_call: 1 << 18 }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Comb {
    All,
    AllSettled,
    Race,
    Any,
}

/// The state of one `all`, `allSettled` or `any` call. `values` hold the results (or the errors for `any`) in input order.
struct Agg {
    kind: Comb,
    promise: ObjId,
    pair: u64,
    values: Vec<Value>,
    called: Vec<bool>,
    remaining: usize,
}

#[derive(Default)]
struct AggState {
    limits: AggLimits,
    recs: FxMap<u64, Agg>,
    next: u64,
    items: usize,
    #[cfg(test)]
    traced_values: std::cell::Cell<usize>,
    k_resolve: Option<PropertyKey>,
    k_status: Option<PropertyKey>,
    k_reason: Option<PropertyKey>,
    k_errors: Option<PropertyKey>,
    s_fulfilled: Option<StrId>,
    s_rejected: Option<StrId>,
    s_all_rejected: Option<StrId>,
    err_proto: Option<ObjId>,
    err_ctor: Option<ObjId>,
}

/// Counters for tools and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AggStats {
    pub records: usize,
    pub items: usize,
}

impl<'p> Interp<'p> {
    pub fn set_agg_limits(&mut self, l: AggLimits) {
        self.promise.agg.limits = l;
    }

    pub fn agg_stats(&self) -> AggStats {
        AggStats { records: self.promise.agg.recs.len(), items: self.promise.agg.items }
    }

    // ------------------------------------------------------------ setup

    fn install_combinators(&mut self) -> Res<()> {
        let ctor = self.promise.ctor.expect("promise ctor");
        for (name, k) in [("all", Comb::All), ("allSettled", Comb::AllSettled), ("race", Comb::Race), ("any", Comb::Any)] {
            self.pmethod(ctor, name, PFn::Comb(k), 1)?;
        }
        self.promise.agg.k_resolve = Some(h!(self, self.heap.key_from_str("resolve")));
        self.promise.agg.k_status = Some(h!(self, self.heap.key_from_str("status")));
        self.promise.agg.k_reason = Some(h!(self, self.heap.key_from_str("reason")));
        self.promise.agg.k_errors = Some(h!(self, self.heap.key_from_str("errors")));
        self.promise.agg.s_fulfilled = Some(h!(self, self.heap.intern_str("fulfilled")));
        self.promise.agg.s_rejected = Some(h!(self, self.heap.intern_str("rejected")));
        self.promise.agg.s_all_rejected = Some(h!(self, self.heap.intern_str("All promises were rejected")));
        // AggregateError: a native error type whose prototype chain is the one of `Error`.
        let base = self.error_protos[0];
        let proto = h!(self, self.heap.new_object(Some(base)));
        let c = self.pfn(PFn::AggErrCtor, "AggregateError", 2)?;
        if let Some(Property::Data { value: Value::Object(error_ctor), .. }) = h!(self, self.heap.get_own_property(base, self.k_constructor)) {
            h!(self, self.heap.set_prototype_of(c, Some(error_ctor)));
        }
        self.def(c, self.k_prototype, Value::Object(proto), false, false, false)?;
        self.def(proto, self.k_constructor, Value::Object(c), true, false, true)?;
        let name = h!(self, self.heap.intern_str("AggregateError"));
        self.def(proto, self.k_name, Value::String(name), true, false, true)?;
        let empty = h!(self, self.heap.intern_str(""));
        self.def(proto, self.k_message, Value::String(empty), true, false, true)?;
        let k = h!(self, self.heap.key_from_str("AggregateError"));
        self.def(self.global, k, Value::Object(c), true, false, true)?;
        self.promise.agg.err_proto = Some(proto);
        self.promise.agg.err_ctor = Some(c);
        Ok(())
    }

    // ------------------------------------------------------------ tracing (GC)

    fn agg_roots(&self, out: &mut Vec<Value>) {
        let a = &self.promise.agg;
        for k in [a.k_resolve, a.k_status, a.k_reason, a.k_errors].into_iter().flatten() {
            if let PropertyKey::String(s) = k {
                out.push(Value::String(s));
            }
        }
        out.extend([a.s_fulfilled, a.s_rejected, a.s_all_rejected].into_iter().flatten().map(Value::String));
        out.extend(a.err_proto.map(Value::Object));
        out.extend(a.err_ctor.map(Value::Object));
    }

    /// The values a native function holds: the Promise family captures, and for an element function its record.
    fn pfn_trace(&self, pf: PFn, seen: &mut FxSet<u64>, work: &mut Vec<Value>) {
        pf.trace(work);
        if let PFn::AggElem { agg, .. } = pf
            && seen.insert(agg)
            && let Some(r) = self.promise.agg.recs.get(&agg)
        {
            #[cfg(test)]
            self.promise.agg.traced_values.set(self.promise.agg.traced_values.get() + r.values.len() + 1);
            work.push(Value::Object(r.promise));
            work.extend_from_slice(&r.values);
        }
    }

    /// Everything of every record, for `roots` (after `agg_prune`, so only live records are there).
    fn agg_all_values(&self, out: &mut Vec<Value>) {
        for r in self.promise.agg.recs.values() {
            out.push(Value::Object(r.promise));
            out.extend_from_slice(&r.values);
        }
    }

    /// Drop the records that no surviving element function names (`funcs` was pruned first). Gives the pairs of the
    /// records that stay: their "already resolved" flag must live as long as the record.
    fn agg_prune(&mut self) -> Vec<u64> {
        let live: FxSet<u64> = self.funcs.values().filter_map(|c| if let Callee::Promise(PFn::AggElem { agg, .. }) = c { Some(*agg) } else { None }).collect();
        let a = &mut self.promise.agg;
        a.recs.retain(|id, _| live.contains(id));
        a.items = a.recs.values().map(|r| r.called.len()).sum();
        a.recs.values().map(|r| r.pair).collect()
    }

    // ------------------------------------------------------------ the combinators

    fn promise_combinator(&mut self, kind: Comb, this: Value, args: &[Value]) -> Res<Value> {
        self.object_this(this)?;
        let (p, res, rej, pair) = self.new_capability(this)?;
        match self.comb_run(kind, this, arg(args, 0), p, (res, rej, pair)) {
            Ok(()) => Ok(Value::Object(p)),
            Err(Error::Throw(v)) => {
                self.cap_reject(p, pair, v)?;
                Ok(Value::Object(p))
            }
            Err(e) => Err(e),
        }
    }

    /// Steps 3 to 5 of the spec: `GetPromiseResolve`, `GetIterator`, then the loop with `IteratorClose` on an ordinary error.
    fn comb_run(&mut self, kind: Comb, c: Value, iterable: Value, p: ObjId, fns: (Value, Value, u64)) -> Res<()> {
        let k_resolve = self.promise.agg.k_resolve.expect("installed");
        let resolve_fn = self.get_prop(c, k_resolve)?;
        if !self.is_callable(resolve_fn) {
            return Err(self.type_err("Promise resolve or reject function is not callable"));
        }
        let mut it = self.iter_start_val(iterable)?;
        match self.comb_loop(kind, c, resolve_fn, &mut it, p, fns) {
            Ok(()) => Ok(()),
            Err(e) => Err(self.iter_close_throw(&mut it, e)),
        }
    }

    fn comb_loop(&mut self, kind: Comb, c: Value, resolve_fn: Value, it: &mut It, p: ObjId, (res, rej, pair): (Value, Value, u64)) -> Res<()> {
        let agg = if kind == Comb::Race { None } else { Some(self.agg_new(kind, p, pair)?) };
        let mut count = 0usize;
        loop {
            let Some(next) = self.iter_next(it)? else {
                return match agg {
                    Some(id) => self.agg_decrement(id),
                    None => Ok(()),
                };
            };
            if count >= self.promise.agg.limits.max_items_per_call {
                return Err(Error::Limit(LimitKind::Agg(AggLimit::ItemsPerCall)));
            }
            count += 1;
            self.charge_steps(1)?;
            let index = match agg {
                Some(id) => self.agg_slot(id)?,
                None => 0,
            };
            let np = self.call_fn(resolve_fn, c, &[next])?;
            let (on_ok, on_err) = match (kind, agg) {
                (Comb::Race, _) => (res, rej),
                (Comb::All, Some(id)) => (self.agg_elem_fn(id, index, false)?, rej),
                (Comb::AllSettled, Some(id)) => (self.agg_elem_fn(id, index, false)?, self.agg_elem_fn(id, index, true)?),
                (_, Some(id)) => (res, self.agg_elem_fn(id, index, true)?),
                (_, None) => unreachable!("a record exists for every kind but race"),
            };
            if let Some(id) = agg {
                self.promise.agg.recs.get_mut(&id).expect("record").remaining += 1;
            }
            self.invoke_then(np, &[on_ok, on_err])?;
        }
    }

    // ------------------------------------------------------------ records

    fn agg_new(&mut self, kind: Comb, promise: ObjId, pair: u64) -> Res<u64> {
        let a = &mut self.promise.agg;
        if a.recs.len() >= a.limits.max_records {
            return Err(Error::Limit(LimitKind::Agg(AggLimit::Records)));
        }
        let id = a.next;
        a.next = id.checked_add(1).ok_or(Error::Limit(LimitKind::Agg(AggLimit::Records)))?;
        a.recs.insert(id, Agg { kind, promise, pair, values: Vec::new(), called: Vec::new(), remaining: 1 });
        Ok(id)
    }

    /// Add one slot (an `undefined` value, a clear once flag). The caps are checked before the vectors grow.
    fn agg_slot(&mut self, id: u64) -> Res<u32> {
        let a = &mut self.promise.agg;
        if a.items >= a.limits.max_items {
            return Err(Error::Limit(LimitKind::Agg(AggLimit::Items)));
        }
        let r = a.recs.get_mut(&id).expect("record");
        let index = u32::try_from(r.called.len()).map_err(|_| Error::Limit(LimitKind::Agg(AggLimit::ItemsPerCall)))?;
        r.values.push(Value::Undefined);
        r.called.push(false);
        a.items += 1;
        Ok(index)
    }

    fn agg_elem_fn(&mut self, agg: u64, index: u32, reject: bool) -> Res<Value> {
        self.charge_steps(1)?;
        Ok(Value::Object(self.pfn(PFn::AggElem { agg, index, reject }, "", 1)?))
    }

    /// One element function was called. A second call of the same item (or of its pair) does nothing.
    fn agg_elem(&mut self, agg: u64, index: u32, reject: bool, x: Value) -> Res<Value> {
        let Some(r) = self.promise.agg.recs.get(&agg) else { return Ok(Value::Undefined) };
        let i = index as usize;
        if r.called.get(i).is_none_or(|&c| c) {
            return Ok(Value::Undefined);
        }
        let v = if r.kind == Comb::AllSettled { self.settled_object(reject, x)? } else { x };
        let r = self.promise.agg.recs.get_mut(&agg).expect("record");
        r.called[i] = true;
        r.values[i] = v;
        self.agg_decrement(agg)?;
        Ok(Value::Undefined)
    }

    /// `{ status: "fulfilled", value }` or `{ status: "rejected", reason }`.
    fn settled_object(&mut self, reject: bool, x: Value) -> Res<Value> {
        self.charge_steps(1)?;
        let a = &self.promise.agg;
        let (k_status, k_reason, s) = (a.k_status.expect("installed"), a.k_reason.expect("installed"), if reject { a.s_rejected } else { a.s_fulfilled });
        let o = h!(self, self.heap.new_object(Some(self.object_proto)));
        self.def(o, k_status, Value::String(s.expect("installed")), true, true, true)?;
        self.def(o, if reject { k_reason } else { self.k_value }, x, true, true, true)?;
        Ok(Value::Object(o))
    }

    /// One pending item is done (or the loop's own count). At zero the result promise settles.
    fn agg_decrement(&mut self, id: u64) -> Res<()> {
        let r = self.promise.agg.recs.get_mut(&id).expect("record");
        r.remaining -= 1;
        if r.remaining != 0 {
            return Ok(());
        }
        let (kind, promise, pair) = (r.kind, r.promise, r.pair);
        let values = std::mem::take(&mut r.values);
        self.charge_steps(values.len() as u64)?;
        if kind == Comb::Any {
            let e = self.aggregate_error(&values)?;
            self.cap_reject(promise, pair, e)
        } else {
            let arr = self.new_array_from(&values)?;
            self.cap_resolve(promise, pair, arr)
        }
    }

    /// Call the result promise's resolve function: a no-op if its pair was already used.
    fn cap_resolve(&mut self, p: ObjId, pair: u64, v: Value) -> Res<()> {
        if pair == u64::MAX { self.call_cap(p, v, false) } else if self.pair_done(pair) { Ok(()) } else { self.resolve_promise(p, v) }
    }

    fn cap_reject(&mut self, p: ObjId, pair: u64, v: Value) -> Res<()> {
        if pair == u64::MAX { self.call_cap(p, v, true) } else if self.pair_done(pair) { Ok(()) } else { self.reject_promise(p, v) }
    }

    /// Call the resolve or reject function of the capability of `p` (made by a constructor other than `Promise`).
    fn call_cap(&mut self, p: ObjId, v: Value, reject: bool) -> Res<()> {
        let (res, rej) = self.promise.caps[&p];
        self.call_fn(if reject { rej } else { res }, Value::Undefined, &[v]).map(drop)
    }

    // ------------------------------------------------------------ AggregateError

    /// The error `Promise.any` rejects with.
    fn aggregate_error(&mut self, errors: &[Value]) -> Res<Value> {
        let a = &self.promise.agg;
        let (proto, msg) = (a.err_proto.expect("installed"), a.s_all_rejected.expect("installed"));
        let o = h!(self, self.heap.new_object(Some(proto)));
        self.def(o, self.k_message, Value::String(msg), true, false, true)?;
        self.install_errors(o, errors)?;
        Ok(Value::Object(o))
    }

    fn install_errors(&mut self, o: ObjId, errors: &[Value]) -> Res<()> {
        let k = self.promise.agg.k_errors.expect("installed");
        let arr = self.new_array_from(errors)?;
        self.def(o, k, arr, true, false, true)
    }

    /// `AggregateError(errors, message)`, with or without `new`.
    fn agg_error_ctor(&mut self, args: &[Value]) -> Res<Value> {
        let nt = self.cls.native_nt.take();
        let proto = self.nt_proto(nt, self.promise.agg.err_proto.expect("installed"))?;
        let o = h!(self, self.heap.new_object(Some(proto)));
        let msg = arg(args, 1);
        if !matches!(msg, Value::Undefined) {
            let m = self.coerce_string(msg)?;
            self.def(o, self.k_message, Value::String(m), true, false, true)?;
        }
        if let Value::Object(options) = arg(args, 2) {
            let cause = h!(self, self.heap.key_from_str("cause"));
            if h!(self, self.heap.has_property(options, cause)) {
                let value = self.get_prop(Value::Object(options), cause)?;
                self.def(o, cause, value, true, false, true)?;
            }
        }
        let list = self.iter_to_vec(arg(args, 0))?;
        self.charge_steps(list.len() as u64)?;
        self.install_errors(o, &list)?;
        self.capture_stack(o)?;
        Ok(Value::Object(o))
    }
}
