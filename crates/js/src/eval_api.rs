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
    fn api_err<'s>(&mut self, e: Error) -> bare_js_api::Error<'s> {
        use bare_js_api::Abort;
        let a = match e {
            Error::Throw(v) => return bare_js_api::Error::Throw(self.api_push(v)),
            Error::Unsupported(u) => Abort::Unsupported(u.what),
            Error::Limit(_) | Error::Heap(_) => Abort::Limit("a limit of the old engine"),
        };
        self.api.abort = Some(e);
        bare_js_api::Error::Abort(a)
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

    fn safepoint(&mut self) {}
}
