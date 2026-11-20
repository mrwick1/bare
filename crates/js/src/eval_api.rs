// The old engine behind the host interface `bare-js-api` (plan step S0.3). Included by `eval.rs`.
//
// The handle stack and the persistent table are two vectors of `Value` in the realm; `roots` and `collect` treat the stack and the live
// persistent slots as roots. This engine collects only between tasks, so `safepoint` does nothing. An engine error that is not an
// exception crosses the interface as `Abort`; the original `Error` waits in `ApiState::abort`, so a host that passes the abort on
// gives back the same error.

/// The shared slots at the start of the handle stack: `undefined`, `null`, `false`, `true`. They sit at the physical indices 0 to 3 and
/// have the slot numbers `u32::MAX - 3` to `u32::MAX`, so a pushed value of logical index `n` is at the physical index `n + 4` and
/// `stack_len` is 0 for an empty stack. `undefined`, `null` and `boolean` push nothing: a handler that answers with one of them costs no push.
const SEEDS: usize = 4;
static SEED_VALUES: [Value; SEEDS] = [Value::Undefined, Value::Null, Value::Bool(false), Value::Bool(true)];
const SLOT_UNDEFINED: u32 = u32::MAX - 3;
const SLOT_NULL: u32 = u32::MAX - 2;
const SLOT_FALSE: u32 = u32::MAX - 1;

/// Slot numbers from here up name the `this` and the arguments of the host call that runs: `WIN` is `this`, `WIN + 1 + i` is argument `i`.
/// The engine reads the argument slots of its caller in place (no copy, no push): a handle names a slot of that slice.
const WIN: u32 = 1 << 31;

/// The `this` and the arguments of a running host call (`call_api_host`). It lives in the frame of that call, and `ApiState::win`
/// points at the innermost one until the call ends. `args` is the slice that the caller of the call holds (`&[Value]`): it is neither
/// written nor moved while the call runs. `roots` reads the whole chain, so a collection inside the call sees `this` and every argument.
struct Win {
    this: Value,
    args: *const Value,
    len: u32,
    prev: *const Win,
}

/// The realm state of the host interface.
pub(crate) struct ApiState<'p> {
    stack: Vec<Value>,
    /// The innermost running host call, or null. See `Win`.
    win: *const Win,
    /// The persistent table. A slot whose bit in `slots` is clear is free; its old value is no root and waits to be overwritten.
    persistent: Vec<Value>,
    slots: Rc<bare_js_api::Slots>,
    host: Option<Rc<dyn bare_js_api::Host<Interp<'p>>>>,
    /// The address of the value in `host`, for `call_api_host`: it skips the offset of an `Rc` to a trait object (that offset reads the
    /// alignment from the vtable). Set only by `set_host`, with `host`.
    host_ptr: Option<std::ptr::NonNull<dyn bare_js_api::Host<Interp<'p>>>>,
    abort: Option<Error>,
    /// The objects with a host key (`set_host_owner`), in no order. `collect` calls the interface host's `trace` for the reached
    /// ones, and its `prune` with the keys of the dead ones. A list, not a map: a new owner is one push. `collect` writes the keys
    /// into its mark table first, so marking an object finds its key with no extra lookup.
    owners: Vec<(ObjId, u32)>,
}

impl Default for ApiState<'_> {
    fn default() -> Self {
        ApiState {
            stack: SEED_VALUES.to_vec(),
            win: std::ptr::null(),
            persistent: Vec::new(),
            slots: Default::default(),
            host: None,
            host_ptr: None,
            abort: None,
            owners: Vec::new(),
        }
    }
}

impl ApiState<'_> {
    fn values(&self, out: &mut Vec<Value>) {
        // The shared constant slots hold no heap value.
        out.extend_from_slice(&self.stack[SEEDS..]);
        let mut w = self.win;
        // SAFETY: each `Win` is in the frame of a `call_api_host` that is still running, and its `args` slice is held by the caller of that
        // call (`call_api_host` links a window before the call and unlinks it after).
        while let Some(x) = unsafe { w.as_ref() } {
            out.push(x.this);
            out.extend_from_slice(unsafe { std::slice::from_raw_parts(x.args, x.len as usize) });
            w = x.prev;
        }
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

    #[inline]
    fn api_push<'s>(&mut self, v: Value) -> bare_js_api::Local<'s> {
        self.api.stack.push(v);
        bare_js_api::Local::from_slot((self.api.stack.len() - SEEDS - 1) as u32)
    }

    /// The value of a handle: a slot of the stack (one bounds check), else a slot of the running call (`api_win`).
    #[inline]
    fn api_at(&self, v: bare_js_api::Local<'_>) -> Value {
        match self.api.stack.get(v.slot().wrapping_add(SEEDS as u32) as usize) {
            Some(&x) => x,
            None => self.api_win(v.slot()),
        }
    }

    /// The value of a slot of the running call: `this` or an argument. A slot that names nothing is a bug of the host: a panic, never
    /// a read outside the slice.
    #[inline]
    fn api_win(&self, slot: u32) -> Value {
        // SAFETY: see `ApiState::values`.
        let (Some(w), Some(k)) = (unsafe { self.api.win.as_ref() }, slot.checked_sub(WIN)) else { panic!("a handle that is no slot of the handle stack") };
        match k.checked_sub(1) {
            None => w.this,
            Some(i) if i < w.len => unsafe { *w.args.add(i as usize) },
            Some(_) => panic!("an argument handle past the arguments of the call"),
        }
    }

    fn api_key(&mut self, key: bare_js_api::Local<'_>) -> Result<PropertyKey, Error> {
        let k = self.api_at(key);
        // A string is the usual key: `key_from_string` checks the handle itself, so skip the `to_string` of `to_property_key`.
        if let Value::String(s) = k {
            return Ok(h!(self, self.heap.key_from_string(s)));
        }
        Ok(h!(self, self.heap.to_property_key(k)))
    }

    fn api_obj(&mut self, v: bare_js_api::Local<'_>) -> Result<ObjId, Error> {
        match self.api_at(v) {
            Value::Object(o) => Ok(o),
            _ => Err(self.type_error("not an object")),
        }
    }

    /// The result of the old engine as a local, or the error of the interface.
    fn api_ret<'s>(&mut self, r: Result<Value, Error>) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        match r {
            Ok(v) => Ok(self.api_push(v)),
            Err(e) => Err(self.api_err(e)),
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
        let r = bare_js_api::enter(self, |s| match h.lazy_member(s, bare_js_api::Local::from_slot((base - SEEDS) as u32), what) {
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
    #[cold]
    pub fn api_unerr(&mut self, e: bare_js_api::Error<'_>) -> Error {
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

    /// `Host::between_jobs` of the interface host `h`. An error comes back as the error of this engine.
    fn api_between_jobs(&mut self, h: &dyn bare_js_api::Host<Interp<'p>>) -> Result<(), Error> {
        bare_js_api::enter(self, |s| h.between_jobs(s).map_err(|e| s.engine().api_unerr(e)))
    }

    /// `Host::report_exception` of the interface host, for the thrown value `v`. No host: the value is dropped.
    fn api_report(&mut self, v: Value) {
        let Some(h) = self.api.host.clone() else { return };
        bare_js_api::enter(self, |s| {
            let thrown = s.engine().api_push(v);
            h.report_exception(s, thrown);
        });
    }

    /// `Host::promise_rejections` of the interface host `h`. The promises go onto the handle stack for the call.
    #[cold]
    fn api_rejections(&mut self, h: &dyn bare_js_api::Host<Interp<'p>>, unhandled: &[Value], handled: &[Value], dropped: u32) -> Result<(), Error> {
        bare_js_api::enter(self, |s| {
            let it = s.engine();
            let u: Vec<_> = unhandled.iter().map(|&v| it.api_push(v)).collect();
            let hd: Vec<_> = handled.iter().map(|&v| it.api_push(v)).collect();
            h.promise_rejections(s, &u, &hd, dropped).map_err(|e| s.engine().api_unerr(e))
        })
    }

    /// `call_host` for a function of the interface host (`Callee::ApiHost`), or for a realm that has no old host. Kept apart, so the old
    /// host path does not change.
    ///
    /// `this` and the arguments are not copied to the handle stack. The host gets handles that name them where they are (`Win`), and the one
    /// scope of the call opens over them: only a value that the handler makes is pushed, and the scope's end drops it. A handle into
    /// the window cannot outlive the call (it is a `Local<'s>` of this scope), the window is no part of the stack (so nothing can move it),
    /// and `ApiState::values` reads the whole chain, so a collection inside the call finds `this` and the arguments as roots.
    #[cold]
    fn call_api_host(&mut self, id: u32, this: Value, args: &[Value], is_new: bool, nt: Option<ObjId>) -> Result<Value, Error> {
        let swap = nt.is_some() || self.host_nt.is_some();
        self.call_api_with(id, this, args, is_new, nt, swap)
    }

    /// `call_api_host` for a plain call (no `new`, no `new.target`), the common one: the checks of the others fold away.
    #[inline(never)]
    fn call_api_plain(&mut self, id: u32, this: Value, args: &[Value]) -> Result<Value, Error> {
        let swap = self.host_nt.is_some();
        self.call_api_with(id, this, args, false, None, swap)
    }

    #[inline(always)]
    fn call_api_with(&mut self, id: u32, this: Value, args: &[Value], is_new: bool, nt: Option<ObjId>, swap: bool) -> Result<Value, Error> {
        let Some(h) = self.api.host_ptr else {
            return Err(unsupported("host function without a host", Span::default()));
        };
        // SAFETY: `h` points into the `Rc` of `api.host`, which `set_host` does not replace while a host call runs (it checks `win`), and
        // which lives as long as the realm. So no `Rc` clone is needed for the call.
        let h = unsafe { h.as_ref() };
        // The common call has no `new.target` and runs under none: nothing to save, set or restore.
        let saved = if swap { std::mem::replace(&mut self.host_nt, nt) } else { None };
        debug_assert!(args.len() < (1 << 30));
        let win = Win { this, args: args.as_ptr(), len: args.len() as u32, prev: self.api.win };
        self.api.win = &win;
        // The guard unlinks the window and restores `new.target` also when the host panics: a caller that catches the panic keeps the
        // realm, and a later collection must not read this dead frame.
        let g = WinGuard { it: self, prev: win.prev, saved: swap.then_some(saved) };
        bare_js_api::enter(&mut *g.it, |s| {
            let r = h.call(s, id, bare_js_api::Local::from_slot(WIN), bare_js_api::Args::from_slots(WIN + 1, args.len() as u32), is_new);
            match r {
                Ok(v) => Ok(s.engine().api_at(v)),
                Err(e) => Err(s.engine().api_unerr(e)),
            }
        })
    }
}

/// Undoes the window link and the `new.target` swap of `call_api_host` when the call ends, by return or by panic.
struct WinGuard<'a, 'p> {
    it: &'a mut Interp<'p>,
    prev: *const Win,
    saved: Option<Option<ObjId>>,
}

impl Drop for WinGuard<'_, '_> {
    #[inline]
    fn drop(&mut self) {
        self.it.api.win = self.prev;
        if let Some(nt) = self.saved {
            self.it.host_nt = nt;
        }
    }
}

/// The heap's element type of an interface element type. `Data` is no typed array.
fn api_elem(e: bare_js_api::Elem) -> Option<Elem> {
    use bare_js_api::Elem as A;
    Some(match e {
        A::I8 => Elem::I8,
        A::U8 => Elem::U8,
        A::U8C => Elem::U8C,
        A::I16 => Elem::I16,
        A::U16 => Elem::U16,
        A::I32 => Elem::I32,
        A::U32 => Elem::U32,
        A::F32 => Elem::F32,
        A::F64 => Elem::F64,
        A::BI64 => Elem::BI64,
        A::BU64 => Elem::BU64,
        A::Data => return None,
    })
}

fn api_elem_of(e: Elem) -> bare_js_api::Elem {
    use bare_js_api::Elem as A;
    match e {
        Elem::I8 => A::I8,
        Elem::U8 => A::U8,
        Elem::U8C => A::U8C,
        Elem::I16 => A::I16,
        Elem::U16 => A::U16,
        Elem::I32 => A::I32,
        Elem::U32 => A::U32,
        Elem::F32 => A::F32,
        Elem::F64 => A::F64,
        Elem::BI64 => A::BI64,
        Elem::BU64 => A::BU64,
        Elem::Data => A::Data,
    }
}

impl<'p> bare_js_api::Engine for Interp<'p> {
    type Raw = Value;

    #[inline]
    fn stack_len(&self) -> u32 {
        (self.api.stack.len() - SEEDS) as u32
    }

    #[inline]
    fn stack_truncate(&mut self, len: u32) {
        self.api.stack.truncate(len as usize + SEEDS);
    }

    fn stack_set(&mut self, to: u32, from: u32) {
        let v = self.api_at(bare_js_api::Local::from_slot(from));
        self.api.stack[to as usize + SEEDS] = v;
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

    #[inline]
    fn undefined<'s>(&mut self) -> bare_js_api::Local<'s> {
        bare_js_api::Local::from_slot(SLOT_UNDEFINED)
    }

    #[inline]
    fn null<'s>(&mut self) -> bare_js_api::Local<'s> {
        bare_js_api::Local::from_slot(SLOT_NULL)
    }

    #[inline]
    fn boolean<'s>(&mut self, b: bool) -> bare_js_api::Local<'s> {
        bare_js_api::Local::from_slot(SLOT_FALSE + b as u32)
    }

    #[inline]
    fn number<'s>(&mut self, n: f64) -> bare_js_api::Local<'s> {
        self.api_push(Value::Number(n))
    }

    #[inline]
    fn string<'s>(&mut self, text: &str) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        // Below `SHARE_MIN` `shared_text` is `intern_str`: skip the call.
        match if text.len() <= crate::heap::SHARE_MIN { self.heap.intern_str(text) } else { self.heap.shared_text(text) } {
            Ok(s) => Ok(self.api_push(Value::String(s))),
            Err(e) => {
                let e = self.herr(e);
                Err(self.api_err(e))
            }
        }
    }

    #[inline]
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

    #[inline]
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

    #[inline]
    fn get_str<'s>(&mut self, obj: bare_js_api::Local<'_>, key: &str) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        let r = match self.heap.key_from_str(key) {
            Ok(k) => self.get_value(self.api_at(obj), k),
            Err(e) => Err(self.herr(e)),
        };
        match r {
            Ok(v) => Ok(self.api_push(v)),
            Err(e) => Err(self.api_err(e)),
        }
    }

    #[inline]
    fn define_str<'s>(&mut self, obj: bare_js_api::Local<'_>, key: &str, value: bare_js_api::Local<'_>, a: bare_js_api::Attrs) -> Result<(), bare_js_api::Error<'s>> {
        let r = self.api_obj(obj).and_then(|o| match self.heap.key_from_str(key) {
            Ok(k) => self.define_data(o, k, self.api_at(value), a.writable, a.enumerable, a.configurable),
            Err(e) => Err(self.herr(e)),
        });
        r.map_err(|e| self.api_err(e))
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
        // Up to 8 arguments sit in an array: no allocation for the usual call.
        let mut buf = [Value::Undefined; 8];
        let heap_args: Vec<Value>;
        let argv: &[Value] = if args.len() <= buf.len() {
            for (d, &a) in buf.iter_mut().zip(args) {
                *d = self.api_at(a);
            }
            &buf[..args.len()]
        } else {
            heap_args = args.iter().map(|&a| self.api_at(a)).collect();
            &heap_args
        };
        match Interp::call(self, self.api_at(f), self.api_at(this), argv) {
            Ok(v) => Ok(self.api_push(v)),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn host_function<'s>(&mut self, name: &str, id: u32, length: u32, kind: bare_js_api::HostFn) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        use bare_js_api::HostFn;
        let (ctor, nt) = (kind != HostFn::Method, kind == HostFn::CtorNewTarget);
        match self.host_function_with(id, name, length, ctor, nt, true) {
            Ok(o) => Ok(self.api_push(Value::Object(o))),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn set_host(&mut self, host: Rc<dyn bare_js_api::Host<Self>>) {
        // `call_api_host` borrows the host without a clone, so it must not change under a running call.
        assert!(self.api.win.is_null(), "set_host inside a host call");
        self.api.host_ptr = Some(std::ptr::NonNull::from(&*host));
        self.api.host = Some(host);
    }

    #[inline]
    fn raw(&self, v: bare_js_api::Local<'_>) -> Value {
        self.api_at(v)
    }

    #[inline]
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

    #[inline]
    fn charge<'s>(&mut self, n: u64) -> Result<(), bare_js_api::Error<'s>> {
        match Interp::charge(self, n) {
            Ok(()) => Ok(()),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn host_key(&self, obj: bare_js_api::Local<'_>) -> Option<u32> {
        match self.api_at(obj) {
            Value::Object(o) => self.heap.host_key(o),
            _ => None,
        }
    }

    fn task<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> Result<R, bare_js_api::Abort> {
        // The closure cannot fail: the only error of `host_scope` is a full call depth.
        self.host_scope(|it| Ok(f(it))).map_err(|_| bare_js_api::Abort::Limit("call depth"))
    }

    fn construct<'s>(&mut self, f: bare_js_api::Local<'_>, args: &[bare_js_api::Local<'_>], new_target: Option<bare_js_api::Local<'_>>) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        let args: Vec<Value> = args.iter().map(|&a| self.api_at(a)).collect();
        let r = new_target.map(|n| self.api_obj(n)).transpose().and_then(|nt| self.host_construct(self.api_at(f), &args, nt));
        self.api_ret(r)
    }

    fn run_script<'s>(&mut self, source: &str) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        let r = Interp::run_script(self, source);
        self.api_ret(r)
    }

    fn compile_function<'s>(&mut self, name: &str, params: &str, body: &str) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        let r = Interp::compile_function(self, name, params, body).map(Value::Object);
        self.api_ret(r)
    }

    fn call_scoped<'s>(&mut self, f: bare_js_api::Local<'_>, this: bare_js_api::Local<'_>, args: &[bare_js_api::Local<'_>], scope: &[bare_js_api::Local<'_>]) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        let args: Vec<Value> = args.iter().map(|&a| self.api_at(a)).collect();
        let r = scope.iter().map(|&l| self.api_obj(l)).collect::<Result<Vec<_>, _>>().and_then(|sc| Interp::call_scoped(self, self.api_at(f), self.api_at(this), &args, &sc));
        self.api_ret(r)
    }

    fn eval_global<'s>(&mut self, src: bare_js_api::Local<'_>) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        let r = Interp::eval_global(self, self.api_at(src));
        self.api_ret(r)
    }

    fn eval_function<'s>(&mut self) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        let r = Interp::eval_function(self).map(Value::Object);
        self.api_ret(r)
    }

    fn new_array_buffer<'s>(&mut self, bytes: Vec<u8>) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        let r = Interp::new_array_buffer(self, bytes);
        self.api_ret(r)
    }

    fn new_typed_array<'s>(&mut self, elem: bare_js_api::Elem, bytes: Vec<u8>) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        let r = match api_elem(elem) {
            Some(ty) => Interp::new_typed_array(self, ty, bytes),
            None => Err(unsupported("a typed array of type Data", Span::default())),
        };
        self.api_ret(r)
    }

    fn bytes(&self, v: bare_js_api::Local<'_>) -> Option<&[u8]> {
        self.bytes_of(self.api_at(v))
    }

    fn bytes_mut(&mut self, v: bare_js_api::Local<'_>) -> Option<&mut [u8]> {
        match self.api_at(v) {
            Value::Object(o) => self.heap.bytes_mut(o),
            _ => None,
        }
    }

    fn view<'s>(&mut self, v: bare_js_api::Local<'_>) -> Option<bare_js_api::View<'s>> {
        let Value::Object(o) = self.api_at(v) else { return None };
        let (buffer, offset, len) = self.heap.view_parts(o)?;
        let elem = api_elem_of(self.heap.view_type(o)?);
        let buffer = self.api_push(Value::Object(buffer));
        Some(bare_js_api::View { buffer, offset, len, elem })
    }

    fn is_detached(&self, v: bare_js_api::Local<'_>) -> bool {
        matches!(self.api_at(v), Value::Object(o) if self.heap.buffer_detached(o) || self.heap.view_detached(o))
    }

    fn view_get(&self, v: bare_js_api::Local<'_>, i: usize) -> Option<f64> {
        match self.api_at(v) {
            Value::Object(o) => self.heap.view_load(o, i),
            _ => None,
        }
    }

    fn bigint_limbs(&self, v: bare_js_api::Local<'_>) -> usize {
        self.heap.bigint_limbs(self.api_at(v))
    }

    fn new_host_indexed<'s>(&mut self, proto: Option<bare_js_api::Local<'_>>, key: u32) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        let r = proto.map(|p| self.api_obj(p)).transpose().and_then(|p| Ok(Value::Object(h!(self, self.heap.new_indexed(p, key)))));
        self.api_ret(r)
    }

    fn new_host_named<'s>(&mut self, proto: Option<bare_js_api::Local<'_>>, key: u32, open: bool) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        let r = proto.map(|p| self.api_obj(p)).transpose().and_then(|p| Ok(Value::Object(h!(self, if open { self.heap.new_named_open(p, key) } else { self.heap.new_named(p, key) }))));
        self.api_ret(r)
    }

    fn set_lazy<'s>(&mut self, obj: bare_js_api::Local<'_>, chain: bool) -> Result<(), bare_js_api::Error<'s>> {
        let r = self.api_obj(obj).and_then(|o| {
            h!(self, if chain { self.heap.set_lazy_chain(o) } else { self.heap.set_lazy_on(o) });
            Ok(())
        });
        r.map_err(|e| self.api_err(e))
    }

    fn clear_lazy(&mut self, obj: bare_js_api::Local<'_>) {
        if let Value::Object(o) = self.api_at(obj) {
            self.heap.clear_lazy_on(o);
        }
    }

    fn own_kind<'s>(&mut self, obj: bare_js_api::Local<'_>, key: bare_js_api::Local<'_>) -> Result<Option<bare_js_api::Own<'s>>, bare_js_api::Error<'s>> {
        use bare_js_api::Own;
        let r = self.api_obj(obj).and_then(|o| {
            let k = self.api_key(key)?;
            match self.heap.peek_own_property(o, k) {
                Ok(p) => Ok(p),
                Err(HeapError::Proxy(_)) => Ok(None),
                Err(e) => Err(self.herr(e)),
            }
        });
        match r {
            Ok(None) => Ok(None),
            Ok(Some(Property::Data { value, .. })) => Ok(Some(Own::Data(self.api_push(value)))),
            Ok(Some(Property::Accessor { get, set, .. })) => {
                let get = get.map(|g| self.api_push(Value::Object(g)));
                let set = set.map(|g| self.api_push(Value::Object(g)));
                Ok(Some(Own::Accessor { get, set }))
            }
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn symbol_description<'s>(&mut self, v: bare_js_api::Local<'_>) -> Result<Option<bare_js_api::Local<'s>>, bare_js_api::Error<'s>> {
        let Value::Symbol(sym) = self.api_at(v) else { let e = self.type_error("not a symbol"); return Err(self.api_err(e)) };
        match self.heap.symbol_description(sym) {
            Ok(d) => Ok(d.map(|d| self.api_push(Value::String(d)))),
            Err(e) => {
                let e = self.herr(e);
                Err(self.api_err(e))
            }
        }
    }

    fn display(&self, v: bare_js_api::Local<'_>) -> String {
        Interp::display(self, self.api_at(v))
    }

    fn inspect(&mut self, v: bare_js_api::Local<'_>, cap: usize, node: &dyn Fn(&Value) -> Option<String>) -> String {
        Interp::inspect(self, self.api_at(v), cap, &|o| node(&Value::Object(o)))
    }

    fn set_integrity<'s>(&mut self, obj: bare_js_api::Local<'_>, frozen: bool) -> Result<bool, bare_js_api::Error<'s>> {
        let r = self.api_obj(obj).and_then(|o| Ok(h!(self, self.heap.set_integrity(o, frozen))));
        r.map_err(|e| self.api_err(e))
    }

    fn array_len<'s>(&mut self, v: bare_js_api::Local<'_>) -> Result<Option<u32>, bare_js_api::Error<'s>> {
        let Value::Object(o) = self.api_at(v) else { return Ok(None) };
        match self.heap.array_length(o) {
            Ok(n) => Ok(n),
            Err(e) => {
                let e = self.herr(e);
                Err(self.api_err(e))
            }
        }
    }

    fn new_error<'s>(&mut self, proto: bare_js_api::Local<'_>, msg: &str) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        let r = self.api_obj(proto).and_then(|p| Interp::new_error(self, p, msg)).map(Value::Object);
        self.api_ret(r)
    }

    fn clone_out<'s>(&mut self, v: bare_js_api::Local<'_>, o: &bare_js_api::CloneOpts<'_, '_>, h: &mut dyn bare_js_api::CloneHost<Self>) -> Result<bare_js_api::Cloned, bare_js_api::Error<'s>> {
        let v = self.api_at(v);
        self.clone_out_impl(v, o, h).map_err(|e| self.api_err(e))
    }

    fn clone_copy<'s>(&mut self, v: bare_js_api::Local<'_>, l: &bare_js_api::CloneLimits, h: &mut dyn bare_js_api::CloneHost<Self>) -> Result<(bare_js_api::Local<'s>, usize), bare_js_api::Error<'s>> {
        let v = self.api_at(v);
        match self.clone_copy_impl(v, l, h) {
            Ok((v, cost)) => Ok((self.api_push(v), cost)),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn clone_in<'s>(&mut self, c: &mut bare_js_api::Cloned, h: &mut dyn bare_js_api::CloneHost<Self>) -> Result<bare_js_api::CloneIn<'s>, bare_js_api::Error<'s>> {
        match self.clone_in_impl(c, h) {
            Ok((v, hosts)) => {
                let value = self.api_push(v);
                Ok(bare_js_api::CloneIn { value, hosts: hosts.into_iter().map(|x| self.api_push(x)).collect() })
            }
            Err(e) => Err(self.api_err(e)),
        }
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

    #[inline]
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
            I::SymbolToStringTag => match self.sc.k_to_string_tag {
                PropertyKey::Symbol(s) => Value::Symbol(s),
                _ => unreachable!("Symbol.toStringTag is a symbol"),
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

    fn bind<'s>(&mut self, target: bare_js_api::Local<'_>, this: bare_js_api::Local<'_>) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        let r = self.api_obj(target).and_then(|t| Interp::host_bind(self, t, self.api_at(this)));
        match r {
            Ok(o) => Ok(self.api_push(Value::Object(o))),
            Err(e) => Err(self.api_err(e)),
        }
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
