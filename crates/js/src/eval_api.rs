// The old engine behind the host interface `bare-js-api` (plan step S0.3). Included by `eval.rs`.
//
// The handle stack and the persistent table are two vectors of `Value` in the realm; `roots` and `collect` treat the stack and the live
// persistent slots as roots. This engine collects only between tasks, so `safepoint` does nothing. An engine error that is not an
// exception crosses the interface as `Abort`; the original `Error` waits in `ApiState::abort`, so a host that passes the abort on
// gives back the same error.

/// The realm state of the host interface.
#[derive(Default)]
pub(crate) struct ApiState<'p> {
    stack: Vec<Value>,
    /// The persistent table. A slot whose bit in `slots` is clear is free; its old value is no root and waits to be overwritten.
    persistent: Vec<Value>,
    slots: Rc<bare_js_api::Slots>,
    host: Option<Rc<dyn bare_js_api::Host<Interp<'p>>>>,
    abort: Option<Error>,
    /// The objects with a host key (`set_host_owner`), in no order. `collect` calls the interface host's `trace` for the reached
    /// ones, and its `prune` with the keys of the dead ones. A list, not a map: a new owner is one push. `collect` writes the keys
    /// into its mark table first, so marking an object finds its key with no extra lookup.
    owners: Vec<(ObjId, u32)>,
}

impl ApiState<'_> {
    fn values(&self, out: &mut Vec<Value>) {
        out.extend_from_slice(&self.stack);
        self.slots.for_each(|id| out.push(self.persistent[id as usize]));
    }
}

impl<'p> Interp<'p> {
    /// Note the owners in `seen` before a collection marks, so that marking an owner finds its key.
    pub(crate) fn api_owners(&self, seen: &mut Seen) {
        for &(o, k) in &self.api.owners {
            seen.set_owner(o, k);
        }
    }

    /// The interface host's references of the owner with host key `key`. `collect` calls it when it marks the owner. The heap marks
    /// its objects again from roots, so the values also go to `roots` (as the old host's values do).
    #[cold]
    pub(crate) fn api_trace(&self, key: u32, roots: &mut Vec<Value>, work: &mut Vec<Value>) {
        if let Some(h) = &self.api.host {
            let from = roots.len();
            h.trace(key, &mut bare_js_api::Tracer::new(roots));
            work.extend_from_slice(&roots[from..]);
        }
    }

    /// Drop the owners that `collect` did not reach, and give their keys to the interface host.
    pub(crate) fn api_prune(&mut self, live: impl Fn(&ObjId) -> bool) {
        if self.api.owners.is_empty() {
            return;
        }
        let mut dead = Vec::new();
        self.api.owners.retain(|&(o, k)| live(&o) || {
            dead.push(k);
            false
        });
        if let (false, Some(h)) = (dead.is_empty(), self.api.host.clone()) {
            h.prune(&dead);
        }
    }

    /// A traced reference of `v`, for host code that still holds values (plan step S0.4).
    #[inline]
    pub fn trace_value(&self, v: Value) -> bare_js_api::Traced<Value> {
        bare_js_api::Traced::from_raw(v)
    }

    /// The value of a traced reference, for host code that still holds values (plan step S0.4).
    #[inline]
    pub fn traced_value(&self, t: &bare_js_api::Traced<Value>) -> Value {
        *t.raw()
    }

    /// A store into a traced reference, for host code that still holds values (plan step S0.4).
    #[inline]
    pub fn store_value(&mut self, t: &mut bare_js_api::Traced<Value>, v: Value) {
        t.set_raw(v);
    }

    /// Let `host` answer for the indexed and named objects of this realm (`Heap::new_indexed`, `Heap::new_named`) by their host
    /// keys: the interface host's `indexed_*` and `named_*` methods.
    pub fn set_api_sources<H: for<'q> bare_js_api::Host<Interp<'q>> + 'static>(&mut self, host: Rc<H>) {
        self.heap.set_indexed_source(Rc::new(ApiSource(host.clone())));
        self.heap.set_named_source(Rc::new(ApiSource(host)));
    }

    /// `set_host_owner` for host code that still holds values (plan step S0.4).
    pub fn set_host_owner_obj(&mut self, o: ObjId, key: u32) {
        self.api.owners.push((o, key));
    }

    /// A persistent handle of `v`, for host code that still holds values (plan step S0.4).
    #[inline]
    pub fn persist_value(&mut self, v: Value) -> bare_js_api::Persistent {
        bare_js_api::Persistent::from_id(self.api_persist(v), self.api.slots.clone())
    }

    #[inline]
    fn api_persist(&mut self, v: Value) -> u32 {
        let a = &mut *self.api;
        let id = a.slots.take();
        match a.persistent.get_mut(id as usize) {
            Some(slot) => *slot = v,
            None => a.persistent.push(v),
        }
        id
    }

    /// The value of a persistent handle of this realm, for host code that still holds values (plan step S0.4).
    #[inline]
    pub fn persistent_value(&self, p: &bare_js_api::Persistent) -> Value {
        self.api.persistent[p.id() as usize]
    }

    /// The length of the persistent table: live handles and free slots.
    pub fn persistent_slots(&self) -> usize {
        self.api.persistent.len()
    }

    fn api_push<'s>(&mut self, v: Value) -> bare_js_api::Local<'s> {
        self.api.stack.push(v);
        bare_js_api::Local::from_slot(self.api.stack.len() as u32 - 1)
    }

    fn api_at(&self, v: bare_js_api::Local<'_>) -> Value {
        self.api.stack[v.slot() as usize]
    }

    fn api_key(&mut self, key: bare_js_api::Local<'_>) -> Result<PropertyKey, Error> {
        let k = self.api_at(key);
        Ok(h!(self, self.heap.to_property_key(k)))
    }

    fn api_obj(&mut self, v: bare_js_api::Local<'_>) -> Result<ObjId, Error> {
        match self.api_at(v) {
            Value::Object(o) => Ok(o),
            _ => Err(self.type_error("not an object")),
        }
    }

    /// An error of this engine as an error of the interface.
    pub fn api_err<'s>(&mut self, e: Error) -> bare_js_api::Error<'s> {
        use bare_js_api::Abort;
        let a = match e {
            Error::Throw(v) => return bare_js_api::Error::Throw(self.api_push(v)),
            Error::Unsupported(u) => Abort::Unsupported(u.what),
            Error::Limit(_) | Error::Heap(_) => Abort::Limit("a limit of the old engine"),
        };
        self.api.abort = Some(e);
        bare_js_api::Error::Abort(a)
    }

    /// The heap asked for a lazy member (`Heap::set_lazy_on`, `set_lazy_chain`) with the reserved `index` of `NeedsHost`: ask the
    /// interface host `h` (`Host::lazy_member`). A name that is not a string key (a symbol) is no ask, as before.
    #[cold]
    fn api_lazy(&mut self, h: &dyn bare_js_api::Host<Interp<'p>>, owner: ObjId, index: u32) -> Result<(), Error> {
        use bare_js_api::Lazy;
        let key;
        let what = match index {
            crate::heap::LAZY_ALL => Lazy::All,
            crate::heap::LAZY_TAG => Lazy::Symbols,
            _ => {
                let Some(k) = self.heap.lazy_on_key() else { return Ok(()) };
                key = k;
                Lazy::Name(&key)
            }
        };
        let base = self.api.stack.len();
        self.api.stack.push(Value::Object(owner));
        let r = bare_js_api::enter(self, |s| match h.lazy_member(s, bare_js_api::Local::from_slot(base as u32), what) {
            Ok(()) => Ok(()),
            Err(bare_js_api::Error::Throw(v)) => Err(Error::Throw(s.engine().api_at(v))),
            Err(bare_js_api::Error::Abort(a)) => Err(s.engine().api.abort.take().unwrap_or(match a {
                bare_js_api::Abort::Limit(w) | bare_js_api::Abort::Unsupported(w) => unsupported(w, Span::default()),
            })),
        });
        self.api.stack.truncate(base);
        r
    }

    /// An error of the interface as an error of this engine: the inverse of `api_err`.
    fn api_unerr(&mut self, e: bare_js_api::Error<'_>) -> Error {
        match e {
            bare_js_api::Error::Throw(v) => Error::Throw(self.api_at(v)),
            bare_js_api::Error::Abort(a) => self.api.abort.take().unwrap_or(match a {
                bare_js_api::Abort::Limit(w) | bare_js_api::Abort::Unsupported(w) => unsupported(w, Span::default()),
            }),
        }
    }

    /// An iteration of the engine as an iteration of the interface. The source, owner, iterator and `next` go onto the handle stack
    /// (roots); the position goes into `state`: word 0 the index (`Plain`) or the cursor sequence (`Coll`), word 1 the cursor epoch,
    /// word 2 the kind in bits 0 and 1, `done` in bit 2, what in bits 3 and 4, the cursor position in bits 32 to 63.
    fn api_iter<'s>(&mut self, it: It) -> bare_js_api::Iter<'s> {
        let (a, b, kind, w0, w1, pos, what) = match it.kind {
            ItKind::Plain => {
                let a = self.api_push(it.src);
                (a, a, 0, it.i as u64, 0, 0, 0)
            }
            ItKind::Coll { owner, what, cur } => {
                let a = self.api_push(Value::Object(owner));
                (a, a, 1, cur.next_seq, cur.epoch, cur.pos, what as u64)
            }
            ItKind::Generic { iter, next } => {
                let (a, b) = (self.api_push(iter), self.api_push(next));
                (a, b, 2, 0, 0, 0, 0)
            }
        };
        bare_js_api::Iter::from_parts(a, b, [w0, w1, kind | (it.done as u64) << 2 | what << 3 | (pos as u64) << 32])
    }

    /// The engine's iteration of an iteration of the interface.
    fn api_it(&self, it: &bare_js_api::Iter<'_>) -> It {
        let (a, b, [w0, w1, w2]) = it.parts();
        let (a, pos) = (self.api_at(a), (w2 >> 32) as u32);
        let kind = match w2 & 3 {
            0 => ItKind::Plain,
            1 => {
                let what = match w2 >> 3 & 3 {
                    0 => IterWhat::Keys,
                    1 => IterWhat::Values,
                    _ => IterWhat::Entries,
                };
                let Value::Object(owner) = a else { unreachable!("a collection iteration keeps its owner") };
                ItKind::Coll { owner, what, cur: Cursor { next_seq: w0, pos, epoch: w1 } }
            }
            _ => ItKind::Generic { iter: a, next: self.api_at(b) },
        };
        It { src: a, i: w0 as usize, kind, done: w2 >> 2 & 1 == 1 }
    }

    /// The position of `it` back into the iteration of the interface (the locals do not change).
    fn api_iter_save(it: &It, out: &mut bare_js_api::Iter<'_>) {
        let (w0, w1, pos, what, kind) = match it.kind {
            ItKind::Plain => (it.i as u64, 0, 0, 0, 0),
            ItKind::Coll { what, cur, .. } => (cur.next_seq, cur.epoch, cur.pos, what as u64, 1),
            ItKind::Generic { .. } => (0, 0, 0, 0, 2),
        };
        out.set_state([w0, w1, kind | (it.done as u64) << 2 | what << 3 | (pos as u64) << 32]);
    }

    /// `call_host` for a realm whose host came through the interface. Kept apart, so the old host path does not change.
    #[cold]
    fn call_api_host(&mut self, id: u32, this: Value, args: &[Value], is_new: bool, nt: Option<ObjId>) -> Result<Value, Error> {
        let Some(h) = self.api.host.clone() else { return Err(unsupported("host function without a host", Span::default())) };
        let saved = std::mem::replace(&mut self.host_nt, nt);
        let r = self.api_call_host(&*h, id, this, args, is_new);
        self.host_nt = saved;
        r
    }

    /// Run host function `id` of the interface host `h`: `this` and the arguments go onto the handle stack for the call.
    fn api_call_host(&mut self, h: &dyn bare_js_api::Host<Interp<'p>>, id: u32, this: Value, args: &[Value], is_new: bool) -> Result<Value, Error> {
        let base = self.api.stack.len();
        self.api.stack.push(this);
        self.api.stack.extend_from_slice(args);
        let r = bare_js_api::enter(self, |s| {
            let this = bare_js_api::Local::from_slot(base as u32);
            let args = bare_js_api::Args::from_slots(base as u32 + 1, args.len() as u32);
            let r = h.call(s, id, this, args, is_new);
            let it = s.engine();
            match r {
                Ok(v) => Ok(it.api_at(v)),
                Err(bare_js_api::Error::Throw(v)) => Err(Error::Throw(it.api_at(v))),
                Err(bare_js_api::Error::Abort(a)) => Err(it.api.abort.take().unwrap_or(match a {
                    bare_js_api::Abort::Limit(w) | bare_js_api::Abort::Unsupported(w) => unsupported(w, Span::default()),
                })),
            }
        });
        self.api.stack.truncate(base);
        r
    }
}

impl<'p> bare_js_api::Engine for Interp<'p> {
    type Raw = Value;

    fn stack_len(&self) -> u32 {
        self.api.stack.len() as u32
    }

    fn stack_truncate(&mut self, len: u32) {
        self.api.stack.truncate(len as usize);
    }

    fn stack_set(&mut self, to: u32, from: u32) {
        self.api.stack[to as usize] = self.api.stack[from as usize];
    }

    fn persist(&mut self, v: bare_js_api::Local<'_>) -> u32 {
        self.api_persist(self.api_at(v))
    }

    fn persistent<'s>(&mut self, id: u32) -> bare_js_api::Local<'s> {
        self.api_push(self.api.persistent[id as usize])
    }

    fn slots(&self) -> &Rc<bare_js_api::Slots> {
        &self.api.slots
    }

    fn undefined<'s>(&mut self) -> bare_js_api::Local<'s> {
        self.api_push(Value::Undefined)
    }

    fn null<'s>(&mut self) -> bare_js_api::Local<'s> {
        self.api_push(Value::Null)
    }

    fn boolean<'s>(&mut self, b: bool) -> bare_js_api::Local<'s> {
        self.api_push(Value::Bool(b))
    }

    fn number<'s>(&mut self, n: f64) -> bare_js_api::Local<'s> {
        self.api_push(Value::Number(n))
    }

    fn string<'s>(&mut self, text: &str) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        match self.heap.shared_text(text) {
            Ok(s) => Ok(self.api_push(Value::String(s))),
            Err(e) => {
                let e = self.herr(e);
                Err(self.api_err(e))
            }
        }
    }

    fn type_of(&self, v: bare_js_api::Local<'_>) -> bare_js_api::Type {
        use bare_js_api::Type;
        match self.api_at(v) {
            Value::Undefined => Type::Undefined,
            Value::Null => Type::Null,
            Value::Bool(_) => Type::Boolean,
            Value::Number(_) => Type::Number,
            Value::String(_) => Type::String,
            Value::Symbol(_) => Type::Symbol,
            Value::BigInt(_) | Value::BigHeap(_) => Type::BigInt,
            v @ Value::Object(_) => {
                if self.is_callable_value(v) {
                    Type::Function
                } else {
                    Type::Object
                }
            }
        }
    }

    fn as_bool(&self, v: bare_js_api::Local<'_>) -> Option<bool> {
        if let Value::Bool(b) = self.api_at(v) { Some(b) } else { None }
    }

    fn as_number(&self, v: bare_js_api::Local<'_>) -> Option<f64> {
        if let Value::Number(n) = self.api_at(v) { Some(n) } else { None }
    }

    fn as_str(&self, v: bare_js_api::Local<'_>) -> Option<bare_js_api::Str<'_>> {
        let Value::String(s) = self.api_at(v) else { return None };
        self.heap.str_units(s).ok().map(bare_js_api::Str::Utf16)
    }

    fn global<'s>(&mut self) -> bare_js_api::Local<'s> {
        self.api_push(Value::Object(self.global))
    }

    fn new_object<'s>(&mut self) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        match self.heap.new_object(Some(self.object_proto)) {
            Ok(o) => Ok(self.api_push(Value::Object(o))),
            Err(e) => {
                let e = self.herr(e);
                Err(self.api_err(e))
            }
        }
    }

    fn get<'s>(&mut self, obj: bare_js_api::Local<'_>, key: bare_js_api::Local<'_>) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        let r = self.api_key(key).and_then(|k| self.get_value(self.api_at(obj), k));
        match r {
            Ok(v) => Ok(self.api_push(v)),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn set<'s>(&mut self, obj: bare_js_api::Local<'_>, key: bare_js_api::Local<'_>, value: bare_js_api::Local<'_>) -> Result<(), bare_js_api::Error<'s>> {
        let r = self.api_key(key).and_then(|k| self.put_value(self.api_at(obj), k, self.api_at(value)));
        r.map_err(|e| self.api_err(e))
    }

    fn define<'s>(&mut self, obj: bare_js_api::Local<'_>, key: bare_js_api::Local<'_>, value: bare_js_api::Local<'_>, a: bare_js_api::Attrs) -> Result<(), bare_js_api::Error<'s>> {
        let r = self.api_obj(obj).and_then(|o| {
            let k = self.api_key(key)?;
            self.define_data(o, k, self.api_at(value), a.writable, a.enumerable, a.configurable)
        });
        r.map_err(|e| self.api_err(e))
    }

    fn call<'s>(&mut self, f: bare_js_api::Local<'_>, this: bare_js_api::Local<'_>, args: &[bare_js_api::Local<'_>]) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        let args: Vec<Value> = args.iter().map(|&a| self.api_at(a)).collect();
        match Interp::call(self, self.api_at(f), self.api_at(this), &args) {
            Ok(v) => Ok(self.api_push(v)),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn host_function<'s>(&mut self, name: &str, id: u32, length: u32) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        match Interp::host_function(self, id, name, length, false) {
            Ok(o) => Ok(self.api_push(Value::Object(o))),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn set_host(&mut self, host: Rc<dyn bare_js_api::Host<Self>>) {
        self.api.host = Some(host);
    }

    fn raw(&self, v: bare_js_api::Local<'_>) -> Value {
        self.api_at(v)
    }

    fn raw_local<'s>(&mut self, r: &Value) -> bare_js_api::Local<'s> {
        self.api_push(*r)
    }

    fn set_host_owner<'s>(&mut self, obj: bare_js_api::Local<'_>, key: u32) -> Result<(), bare_js_api::Error<'s>> {
        match self.api_obj(obj) {
            Ok(o) => {
                self.api.owners.push((o, key));
                Ok(())
            }
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn new_promise<'s>(&mut self) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        match Interp::host_new_promise(self) {
            Ok(p) => Ok(self.api_push(p)),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn resolve_promise<'s>(&mut self, p: bare_js_api::Local<'_>, v: bare_js_api::Local<'_>) -> Result<(), bare_js_api::Error<'s>> {
        Interp::host_resolve_promise(self, self.api_at(p), self.api_at(v)).map_err(|e| self.api_err(e))
    }

    fn reject_promise<'s>(&mut self, p: bare_js_api::Local<'_>, reason: bare_js_api::Local<'_>) -> Result<(), bare_js_api::Error<'s>> {
        Interp::host_reject_promise(self, self.api_at(p), self.api_at(reason)).map_err(|e| self.api_err(e))
    }

    fn promise_then<'s>(&mut self, p: bare_js_api::Local<'_>, ok: bare_js_api::Local<'_>, err: bare_js_api::Local<'_>) -> Result<(), bare_js_api::Error<'s>> {
        Interp::host_promise_then(self, self.api_at(p), self.api_at(ok), self.api_at(err)).map_err(|e| self.api_err(e))
    }

    fn promise_react<'s>(&mut self, x: bare_js_api::Local<'_>, ok: bare_js_api::Local<'_>, err: bare_js_api::Local<'_>) -> Result<(), bare_js_api::Error<'s>> {
        Interp::host_promise_react(self, self.api_at(x), self.api_at(ok), self.api_at(err)).map_err(|e| self.api_err(e))
    }

    fn promise_status<'s>(&mut self, v: bare_js_api::Local<'_>) -> Option<bare_js_api::PromiseState<'s>> {
        use bare_js_api::PromiseState;
        Some(match Interp::promise_status(self, self.api_at(v))? {
            PromiseStatus::Pending => PromiseState::Pending,
            PromiseStatus::Fulfilled(x) => PromiseState::Fulfilled(self.api_push(x)),
            PromiseStatus::Rejected(x) => PromiseState::Rejected(self.api_push(x)),
        })
    }

    fn queue_microtask<'s>(&mut self, cb: bare_js_api::Local<'_>) -> Result<(), bare_js_api::Error<'s>> {
        Interp::queue_microtask(self, self.api_at(cb)).map_err(|e| self.api_err(e))
    }

    fn checkpoint<'s>(&mut self) -> Result<usize, bare_js_api::Error<'s>> {
        Interp::host_checkpoint(self).map_err(|e| self.api_err(e))
    }

    fn drain_microtasks<'s>(&mut self) -> Result<usize, bare_js_api::Error<'s>> {
        Interp::drain_microtasks(self).map_err(|e| self.api_err(e))
    }

    fn pending_jobs(&self) -> usize {
        Interp::pending_jobs(self)
    }

    fn clear_failed_job(&mut self) {
        Interp::clear_failed_job(self);
    }

    fn iter_open<'s>(&mut self, v: bare_js_api::Local<'_>) -> Result<Option<bare_js_api::Iter<'s>>, bare_js_api::Error<'s>> {
        match Interp::host_iter_open(self, self.api_at(v)) {
            Ok(it) => Ok(it.map(|h| self.api_iter(h.0))),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn iter_next<'s>(&mut self, it: &mut bare_js_api::Iter<'s>) -> Result<Option<bare_js_api::Local<'s>>, bare_js_api::Error<'s>> {
        let mut h = HostIter(self.api_it(it));
        let r = Interp::host_iter_next(self, &mut h);
        Self::api_iter_save(&h.0, it);
        match r {
            Ok(v) => Ok(v.map(|v| self.api_push(v))),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn iter_fail<'s>(&mut self, it: &mut bare_js_api::Iter<'s>, err: bare_js_api::Error<'s>) -> bare_js_api::Error<'s> {
        let err = self.api_unerr(err);
        let mut h = HostIter(self.api_it(it));
        let err = Interp::host_iter_fail(self, &mut h, err);
        Self::api_iter_save(&h.0, it);
        self.api_err(err)
    }

    // Modules (plan step S0.4). Over the `module_*` methods of this engine; a module is its index.

    fn module_parse<'s>(&mut self, source: &str, url: &str) -> Result<u32, bare_js_api::Error<'s>> {
        Interp::module_parse(self, source, url).map_err(|e| self.api_err(e))
    }

    fn module_requests(&self, m: u32) -> Vec<String> {
        Interp::module_requests(self, m)
    }

    fn module_set_requested(&mut self, m: u32, deps: &[u32]) {
        Interp::module_set_requested(self, m, deps);
    }

    fn module_url(&self, m: u32) -> &str {
        Interp::module_url(self, m)
    }

    fn module_link<'s>(&mut self, root: u32) -> Result<(), bare_js_api::Error<'s>> {
        Interp::module_link(self, root).map_err(|e| self.api_err(e))
    }

    fn module_evaluate<'s>(&mut self, m: u32) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        match Interp::module_evaluate(self, m) {
            Ok(p) => Ok(self.api_push(Value::Object(p))),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn take_dynamic_imports(&mut self) -> Vec<bare_js_api::ImportRequest> {
        Interp::take_dynamic_imports(self)
            .into_iter()
            .map(|r| bare_js_api::ImportRequest { promise: self.persist_value(Value::Object(r.promise)), specifier: r.specifier, referrer: r.referrer })
            .collect()
    }

    fn finish_dynamic_import<'s>(&mut self, p: &bare_js_api::Persistent, m: u32) -> Result<(), bare_js_api::Error<'s>> {
        let Value::Object(p) = self.persistent_value(p) else { unreachable!("an import promise is an object") };
        Interp::finish_dynamic_import(self, p, m).map_err(|e| self.api_err(e))
    }

    fn reject_dynamic_import<'s>(&mut self, p: &bare_js_api::Persistent, reason: bare_js_api::Local<'_>) -> Result<(), bare_js_api::Error<'s>> {
        let Value::Object(p) = self.persistent_value(p) else { unreachable!("an import promise is an object") };
        let reason = self.api_at(reason);
        Interp::reject_dynamic_import(self, p, Error::Throw(reason)).map_err(|e| self.api_err(e))
    }

    fn fail_dynamic_import<'s>(&mut self, p: &bare_js_api::Persistent, message: &str) -> Result<(), bare_js_api::Error<'s>> {
        let Value::Object(p) = self.persistent_value(p) else { unreachable!("an import promise is an object") };
        Interp::fail_dynamic_import(self, p, message).map_err(|e| self.api_err(e))
    }

    fn safepoint(&mut self) {}

    // The base set of handler methods (plan step S0.4).

    fn type_error<'s>(&mut self, msg: &str) -> bare_js_api::Error<'s> {
        let e = Interp::type_error(self, msg);
        self.api_err(e)
    }

    fn range_error<'s>(&mut self, msg: &str) -> bare_js_api::Error<'s> {
        let e = Interp::range_error(self, msg);
        self.api_err(e)
    }

    fn charge<'s>(&mut self, n: u64) -> Result<(), bare_js_api::Error<'s>> {
        Interp::charge(self, n).map_err(|e| self.api_err(e))
    }

    fn to_number<'s>(&mut self, v: bare_js_api::Local<'_>) -> Result<f64, bare_js_api::Error<'s>> {
        self.to_js_number(self.api_at(v)).map_err(|e| self.api_err(e))
    }

    fn to_string<'s>(&mut self, v: bare_js_api::Local<'_>) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        match self.to_js_string(self.api_at(v)) {
            Ok(s) => Ok(self.api_push(Value::String(s))),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn to_boolean(&self, v: bare_js_api::Local<'_>) -> bool {
        self.to_js_bool(self.api_at(v)).unwrap_or(false)
    }

    fn new_array<'s>(&mut self, items: &[bare_js_api::Local<'_>]) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        let items: Vec<Value> = items.iter().map(|&i| self.api_at(i)).collect();
        match self.array_from(&items) {
            Ok(a) => Ok(self.api_push(a)),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn is_array<'s>(&mut self, v: bare_js_api::Local<'_>) -> Result<bool, bare_js_api::Error<'s>> {
        match self.api_at(v) {
            Value::Object(o) => self.o_is_array(o).map_err(|e| self.api_err(e)),
            _ => Ok(false),
        }
    }

    fn intrinsic<'s>(&mut self, which: bare_js_api::Intrinsic) -> bare_js_api::Local<'s> {
        use bare_js_api::Intrinsic as I;
        let v = match which {
            I::ObjectPrototype => Value::Object(self.object_proto),
            I::ArrayPrototype => Value::Object(self.array_proto),
            I::IteratorPrototype => Value::Object(self.sc.iter_proto),
            I::ErrorPrototype => Value::Object(self.error_protos[0]),
            I::SymbolIterator => match self.sc.k_iterator {
                PropertyKey::Symbol(s) => Value::Symbol(s),
                // The realm keeps `Symbol.iterator` as a symbol key.
                _ => unreachable!("Symbol.iterator is a symbol"),
            },
        };
        self.api_push(v)
    }

    fn get_prototype_of<'s>(&mut self, obj: bare_js_api::Local<'_>) -> Result<Option<bare_js_api::Local<'s>>, bare_js_api::Error<'s>> {
        let r = self.api_obj(obj).and_then(|o| self.o_get_prototype_of(o));
        match r {
            Ok(p) => Ok(p.map(|p| self.api_push(Value::Object(p)))),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn set_prototype_of<'s>(&mut self, obj: bare_js_api::Local<'_>, proto: Option<bare_js_api::Local<'_>>) -> Result<bool, bare_js_api::Error<'s>> {
        let r = self.api_obj(obj).and_then(|o| {
            let p = proto.map(|p| self.api_obj(p)).transpose()?;
            self.o_set_prototype_of(o, p)
        });
        r.map_err(|e| self.api_err(e))
    }

    fn define_accessor<'s>(&mut self, obj: bare_js_api::Local<'_>, key: bare_js_api::Local<'_>, get: Option<bare_js_api::Local<'_>>, set: Option<bare_js_api::Local<'_>>, a: bare_js_api::Attrs) -> Result<(), bare_js_api::Error<'s>> {
        let r = self.api_obj(obj).and_then(|o| {
            let k = self.api_key(key)?;
            let fun = |it: &mut Self, f: Option<bare_js_api::Local<'_>>| match f {
                None => Ok(None),
                Some(f) if it.is_callable(it.api_at(f)) => it.api_obj(f).map(Some),
                Some(_) => Err(it.type_error("accessor is not a function")),
            };
            let d = PropertyDescriptor { get: Some(fun(self, get)?), set: Some(fun(self, set)?), enumerable: Some(a.enumerable), configurable: Some(a.configurable), ..Default::default() };
            if self.o_define_own_property(o, k, &d)? { Ok(()) } else { Err(self.type_error("Cannot redefine property")) }
        });
        r.map_err(|e| self.api_err(e))
    }

    fn proto_from_new_target<'s>(&mut self, default: bare_js_api::Local<'_>) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        let r = self.api_obj(default).and_then(|d| self.host_proto_from_new_target(d));
        match r {
            Ok(p) => Ok(self.api_push(Value::Object(p))),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn string_utf16<'s>(&mut self, units: &[u16]) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        match self.heap.intern_units(units) {
            Ok(s) => Ok(self.api_push(Value::String(s))),
            Err(e) => {
                let e = self.herr(e);
                Err(self.api_err(e))
            }
        }
    }

    fn wall_now_ms(&self) -> f64 {
        Interp::wall_now_ms(self)
    }

    fn has_own<'s>(&mut self, obj: bare_js_api::Local<'_>, key: bare_js_api::Local<'_>) -> Result<bool, bare_js_api::Error<'s>> {
        let r = self.api_obj(obj).and_then(|o| {
            let k = self.api_key(key)?;
            match self.heap.peek_own_property(o, k) {
                Ok(p) => Ok(p.is_some()),
                Err(HeapError::Proxy(_)) => Ok(false),
                Err(e) => Err(self.herr(e)),
            }
        });
        r.map_err(|e| self.api_err(e))
    }

    fn own_keys<'s>(&mut self, obj: bare_js_api::Local<'_>) -> Result<Vec<bare_js_api::Local<'s>>, bare_js_api::Error<'s>> {
        let r = self.api_obj(obj).and_then(|o| {
            let keys = self.o_own_keys(o)?;
            self.charge(keys.len() as u64)?;
            let mut out = Vec::with_capacity(keys.len());
            for k in keys {
                out.push(h!(self, self.heap.key_to_value(k)));
            }
            Ok(out)
        });
        match r {
            Ok(vs) => Ok(vs.into_iter().map(|v| self.api_push(v)).collect()),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn own_enumerable_data<'s>(&mut self, obj: bare_js_api::Local<'_>, key: bare_js_api::Local<'_>) -> Result<Option<bare_js_api::Local<'s>>, bare_js_api::Error<'s>> {
        let r = self.api_obj(obj).and_then(|o| {
            let k = self.api_key(key)?;
            match self.o_get_own_property(o, k)? {
                Some(p) if p.enumerable() => self.get_value(Value::Object(o), k).map(Some),
                _ => Ok(None),
            }
        });
        match r {
            Ok(v) => Ok(v.map(|v| self.api_push(v))),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn is_callable(&self, v: bare_js_api::Local<'_>) -> bool {
        self.is_callable_value(self.api_at(v))
    }
}

/// The heap's sources, answered by an interface host (`Interp::set_api_sources`). The heap gives the asked object and its host key;
/// the host gets only the key.
struct ApiSource<H>(Rc<H>);

impl<H: for<'q> bare_js_api::Host<Interp<'q>>> ApiSource<H> {
    /// The host as its own type: the calls are static, so the compiler can inline the host's answer here.
    fn host(&self) -> &impl bare_js_api::Host<Interp<'static>> {
        &*self.0
    }
}

use bare_js_api::Host as _;

fn pending(owner: ObjId, p: bare_js_api::Pending) -> HeapError {
    match p {
        bare_js_api::Pending::Busy => HeapError::HostUnavailable,
        bare_js_api::Pending::Fill(index) => HeapError::NeedsHost { owner, index },
    }
}

impl<H: for<'q> bare_js_api::Host<Interp<'q>>> crate::heap::IndexedSource for ApiSource<H> {
    fn indexed_len(&self, owner: ObjId, key: u32) -> Result<usize, HeapError> {
        self.host().indexed_len(&Value::Object(owner), key).map_err(|p| pending(owner, p))
    }
    fn indexed_item(&self, owner: ObjId, key: u32, index: usize) -> Result<crate::heap::Slot, HeapError> {
        use crate::heap::Slot;
        Ok(match self.host().indexed_item(&Value::Object(owner), key, index).map_err(|p| pending(owner, p))? {
            bare_js_api::Item::Absent => Slot::Absent,
            bare_js_api::Item::Missing => Slot::Missing,
            bare_js_api::Item::Value(Value::Object(o)) => Slot::Ready(o),
            bare_js_api::Item::Value(Value::String(s)) => Slot::Str(s),
            // An indexed property is an object or a string; the host gives no other value.
            bare_js_api::Item::Value(_) => Slot::Absent,
        })
    }
    fn indexed_name(&self, owner: ObjId, key: u32, name: &[u16]) -> Result<Option<u32>, HeapError> {
        self.host().indexed_name(&Value::Object(owner), key, name).map_err(|p| pending(owner, p))
    }
    fn indexed_names(&self, owner: ObjId, key: u32) -> Result<Vec<Vec<u16>>, HeapError> {
        self.host().indexed_names(&Value::Object(owner), key).map_err(|p| pending(owner, p))
    }
}

impl<H: for<'q> bare_js_api::Host<Interp<'q>>> crate::heap::NamedSource for ApiSource<H> {
    fn named_has(&self, owner: ObjId, hk: u32, key: &[u16]) -> Result<bool, HeapError> {
        self.host().named_has(&Value::Object(owner), hk, key).map_err(|p| pending(owner, p))
    }
    fn named_get(&self, owner: ObjId, hk: u32, key: &[u16]) -> Result<Option<crate::heap::NamedValue>, HeapError> {
        Ok(self.host().named_get(&Value::Object(owner), hk, key).map_err(|p| pending(owner, p))?.map(|v| match v {
            bare_js_api::Named::Text(t) => crate::heap::NamedValue::Text(t),
            bare_js_api::Named::Value(v) => crate::heap::NamedValue::Value(v),
        }))
    }
    fn named_len(&self, owner: ObjId, hk: u32) -> Result<usize, HeapError> {
        self.host().named_len(&Value::Object(owner), hk).map_err(|p| pending(owner, p))
    }
    fn named_keys(&self, owner: ObjId, hk: u32) -> Result<Vec<Vec<u16>>, HeapError> {
        self.host().named_keys(&Value::Object(owner), hk).map_err(|p| pending(owner, p))
    }
}
