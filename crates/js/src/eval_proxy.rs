// `Proxy` and `Proxy.revocable`, and the internal methods that run the traps. Included by `eval.rs` (`include!`), so it shares
// that module's imports and private items. The heap side is `heap_proxy.rs`; read its header first.
//
// Dispatch
// - The heap never runs a trap. A heap operation that meets a proxy changes nothing and answers `HeapError::Proxy(p)`.
// - Every evaluator path that is routed through the proxy dispatch calls `hx!(s, heap_op, fallback)` or one of the `o_*`
//   methods below. The fallback is the internal method of the proxy `p` (`px_*`), or the generic algorithm of the
//   specification when the proxy is a prototype (`o_get_slow`, `o_set`, `o_has_slow`). A path with no fallback gets `Unsupported`
//   from `herr`: never a wrong answer. The list of paths without a fallback is in docs/resources.md.
// - Cost: an ordinary object takes no new branch. The proxy arm is a cold arm of `hx!` and `hxw!`, and the heap tests its flag
//   where it already tests `exotic != 0`.
//
// What runs
// - All 13 internal methods (`px_get_prototype_of`, ..., `px_construct`) with the invariant checks of ECMA-262 section 10.5,
//   against the target, in spec order. A trap that is `undefined` or `null` forwards to the target. A trap that is not callable is a
//   TypeError. A revoked proxy is a TypeError on every internal method, except `typeof`, `IsArray` of a revoked proxy (TypeError)
//   and the callable bit (kept: a revoked function proxy is still `"function"`).
// - `new Proxy(target, handler)` (TypeError without `new` or with a non-object), `Proxy.revocable`. A proxy is callable when its
//   target is callable, and a constructor when its target is a constructor (`Callee::Proxy`).
// - Recursion (a proxy of a proxy, a trap that touches its own proxy) goes through `descend` and `call_fn`, so it ends in
//   `Limit(TraversalDepth)`, `Limit(CallDepth)` or `Limit(Steps)`: never a native stack overflow.
//
// Gates (never silent)
// - A typed array or a host object as the receiver of a proxied `[[Set]]` follows the generic algorithm, not the special
//   `[[Set]]` of that exotic object, except for an integer index that is not valid (ends the walk with `true`).
// - A native that reads the heap directly (`h!` with no proxy arm) and meets a proxy answers `Unsupported`, never a wrong value.
//   Probing about 130 operations against Node found none left, except those that are `Unsupported` or wrong on a plain object too.
// - The list that `array_like` builds from the result of `ownKeys` has the cap of `array_like` (1 << 20 keys).

/// The 13 traps, in the order of the key table. `name` is the property of the handler.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Trap {
    GetPrototypeOf,
    SetPrototypeOf,
    IsExtensible,
    PreventExtensions,
    GetOwnPropertyDescriptor,
    DefineProperty,
    Has,
    Get,
    Set,
    DeleteProperty,
    OwnKeys,
    Apply,
    Construct,
}

const TRAP_NAMES: [&str; 13] = [
    "getPrototypeOf", "setPrototypeOf", "isExtensible", "preventExtensions", "getOwnPropertyDescriptor", "defineProperty", "has", "get", "set",
    "deleteProperty", "ownKeys", "apply", "construct",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PxFn {
    Ctor,
    Revocable,
}

/// The interned trap names. They are roots (`proxy_roots`). They are made when the first proxy is made (`proxy_create`), so a realm
/// that never makes a proxy pays nothing for them (13 interned strings are about 20 k instructions at startup).
struct ProxyState {
    keys: [PropertyKey; 13],
    ready: bool,
    /// The cache of each trap read on a handler (`Interp::get_prop_ic`): a handler that has no such trap is an absent hit.
    ics: [[Ic; 4]; 13],
}

impl Default for ProxyState {
    fn default() -> Self {
        ProxyState { keys: [PropertyKey::Index(0); 13], ready: false, ics: [[Ic::EMPTY; 4]; 13] }
    }
}

/// A complete property as a full descriptor (every field present), for `IsCompatiblePropertyDescriptor`.
fn prop_desc(p: Property) -> PropertyDescriptor {
    match p {
        Property::Data { value, writable, enumerable, configurable } => {
            PropertyDescriptor { value: Some(value), writable: Some(writable), enumerable: Some(enumerable), configurable: Some(configurable), ..Default::default() }
        }
        Property::Accessor { get, set, enumerable, configurable } => {
            PropertyDescriptor { get: Some(get), set: Some(set), enumerable: Some(enumerable), configurable: Some(configurable), ..Default::default() }
        }
    }
}

impl<'p> Interp<'p> {
    // ------------------------------------------------------------ install and the constructor

    fn proxy_roots(&self, v: &mut Vec<Value>) {
        for k in self.px.keys {
            if let PropertyKey::String(s) = k {
                v.push(Value::String(s));
            }
        }
    }

    fn install_proxy(&mut self) -> Res<()> {
        let c = self.native(Nat::Px(PxFn::Ctor), "Proxy", 2)?;
        self.method(c, "revocable", Nat::Px(PxFn::Revocable), 2)?;
        let k = h!(self, self.heap.key_from_str("Proxy"));
        self.def(self.global, k, Value::Object(c), true, false, true)
    }

    fn call_px(&mut self, f: PxFn, args: &[Value], is_new: bool) -> Res<Value> {
        let (target, handler) = (arg(args, 0), arg(args, 1));
        match f {
            PxFn::Ctor if !is_new => Err(self.type_err("Constructor Proxy requires 'new'")),
            PxFn::Ctor => Ok(Value::Object(self.proxy_create(target, handler)?)),
            PxFn::Revocable => {
                let p = self.proxy_create(target, handler)?;
                let revoke = self.new_func_object(Callee::Revoke(Some(p)))?;
                let empty = h!(self, self.heap.intern_str(""));
                self.def(revoke, self.k_length, Value::Number(0.0), false, false, true)?;
                self.def(revoke, self.k_name, Value::String(empty), false, false, true)?;
                let r = h!(self, self.heap.new_object(Some(self.object_proto)));
                let kp = h!(self, self.heap.key_from_str("proxy"));
                let kr = h!(self, self.heap.key_from_str("revoke"));
                self.def(r, kp, Value::Object(p), true, true, true)?;
                self.def(r, kr, Value::Object(revoke), true, true, true)?;
                Ok(Value::Object(r))
            }
        }
    }

    /// `ProxyCreate`. The proxy is callable when the target is, and a constructor when the target is.
    fn proxy_create(&mut self, target: Value, handler: Value) -> Res<ObjId> {
        let (Value::Object(t), Value::Object(hd)) = (target, handler) else {
            return Err(self.type_err("Cannot create proxy with a non-object as target or handler"));
        };
        if !self.px.ready {
            for (i, n) in TRAP_NAMES.iter().enumerate() {
                self.px.keys[i] = h!(self, self.heap.key_from_str(n));
            }
            self.px.ready = true;
        }
        let callable = self.is_callable(target);
        if callable && self.funcs.len() >= self.limits.max_functions {
            return Err(Error::Limit(LimitKind::Functions));
        }
        let ctor = callable && self.is_constructor(t);
        let p = h!(self, self.heap.new_proxy(t, hd));
        if callable {
            self.funcs.insert(p, Callee::Proxy { ctor });
        }
        Ok(p)
    }

    /// The function that `Proxy.revocable` made. It revokes once, then does nothing.
    fn call_revoke(&mut self, f: ObjId) -> Res<Value> {
        if let Some(Callee::Revoke(slot)) = self.funcs.get_mut(&f)
            && let Some(p) = slot.take()
        {
            h!(self, self.heap.revoke_proxy(p));
        }
        Ok(Value::Undefined)
    }

    // ------------------------------------------------------------ helpers

    /// `(target, handler)` of a live proxy. A revoked proxy is a TypeError for the internal method `op`.
    fn px_parts(&mut self, p: ObjId, op: &str) -> Res<(ObjId, ObjId)> {
        match h!(self, self.heap.proxy_parts(p)) {
            Some(Some(parts)) => Ok(parts),
            _ => Err(self.type_err(&format!("Cannot perform '{op}' on a proxy that has been revoked"))),
        }
    }

    /// The start of every internal method: charge a step, read the parts, and `GetMethod(handler, trap)`.
    fn px_enter(&mut self, p: ObjId, t: Trap) -> Res<(ObjId, ObjId, Option<Value>)> {
        self.tick()?;
        let (target, handler) = self.px_parts(p, TRAP_NAMES[t as usize])?;
        let mut ics = self.px.ics[t as usize];
        let f = self.get_prop_ic(Value::Object(handler), self.px.keys[t as usize], &mut ics);
        self.px.ics[t as usize] = ics;
        let f = f?;
        if is_nullish(f) {
            return Ok((target, handler, None));
        }
        if !self.is_callable(f) {
            let d = self.display(f);
            return Err(self.type_err(&format!("{d} returned for property '{}' of object '#<Object>' is not a function", TRAP_NAMES[t as usize])));
        }
        Ok((target, handler, Some(f)))
    }

    fn px_bool(&mut self, v: Value) -> Res<bool> {
        self.truthy(v)
    }

    fn px_key_value(&mut self, key: PropertyKey) -> Res<Value> {
        Ok(h!(self, self.heap.key_to_value(key)))
    }

    /// `FromPropertyDescriptor` of a partial descriptor: only the fields that are present, in the order of the specification.
    fn px_desc_object(&mut self, d: &PropertyDescriptor) -> Res<Value> {
        let r = h!(self, self.heap.new_object(Some(self.object_proto)));
        let (kv, kw, kg, ks, ke, kc) = (self.k_value, self.k_writable, self.k_get, self.k_set, self.k_enumerable, self.k_configurable);
        if let Some(v) = d.value {
            self.def(r, kv, v, true, true, true)?;
        }
        if let Some(w) = d.writable {
            self.def(r, kw, Value::Bool(w), true, true, true)?;
        }
        if let Some(g) = d.get {
            self.def(r, kg, g.map_or(Value::Undefined, Value::Object), true, true, true)?;
        }
        if let Some(s) = d.set {
            self.def(r, ks, s.map_or(Value::Undefined, Value::Object), true, true, true)?;
        }
        if let Some(e) = d.enumerable {
            self.def(r, ke, Value::Bool(e), true, true, true)?;
        }
        if let Some(c) = d.configurable {
            self.def(r, kc, Value::Bool(c), true, true, true)?;
        }
        Ok(Value::Object(r))
    }

    fn px_invariant(&mut self, trap: &str, why: &str) -> Error {
        self.type_err(&format!("'{trap}' on proxy: {why}"))
    }

    // ------------------------------------------------------------ the dispatch: one method for each internal method

    fn o_get_prototype_of(&mut self, o: ObjId) -> Res<Option<ObjId>> {
        match self.heap.get_prototype_of(o) {
            Ok(p) => Ok(p),
            Err(HeapError::Proxy(p)) => self.px_get_prototype_of(p),
            Err(e) => Err(self.herr(e)),
        }
    }

    fn o_set_prototype_of(&mut self, o: ObjId, proto: Option<ObjId>) -> Res<bool> {
        match self.heap.set_prototype_of(o, proto) {
            Ok(b) => Ok(b),
            Err(HeapError::Proxy(p)) => self.px_set_prototype_of(p, proto),
            Err(e) => Err(self.herr(e)),
        }
    }

    fn o_is_extensible(&mut self, o: ObjId) -> Res<bool> {
        match self.heap.is_extensible(o) {
            Ok(b) => Ok(b),
            Err(HeapError::Proxy(p)) => self.px_is_extensible(p),
            Err(e) => Err(self.herr(e)),
        }
    }

    fn o_prevent_extensions(&mut self, o: ObjId) -> Res<bool> {
        if self.heap.named_open(o) {
            return Ok(false);
        }
        match self.heap.prevent_extensions(o) {
            Ok(()) => Ok(true),
            Err(HeapError::Proxy(p)) => self.px_prevent_extensions(p),
            Err(e) => Err(self.herr(e)),
        }
    }

    /// The slow side of an `o_*` read: a host fill (at most `MAX_FILLS` repeats, as `hx!`), a proxy, or an error. `e` is the answer of the first
    /// call, `again` repeats the heap call and `px` runs the internal method of the proxy. It is out of line so that the fast side
    /// of every `o_*` method is one heap call and a test, and the compiler can inline it into the callers.
    #[cold]
    #[inline(never)]
    fn heap_miss<T>(&mut self, mut e: HeapError, again: impl Fn(&mut Self) -> Result<T, HeapError>, px: impl FnOnce(&mut Self) -> Res<T>) -> Res<T> {
        let mut fills = 0u8;
        loop {
            match e {
                HeapError::NeedsHost { owner, index } if fills < crate::eval::MAX_FILLS => {
                    fills += 1;
                    self.host_fill(owner, index)?;
                }
                HeapError::Proxy(_) => return px(self),
                e => return Err(self.herr(e)),
            }
            match again(self) {
                Ok(v) => return Ok(v),
                Err(n) => e = n,
            }
        }
    }

    /// `heap_miss` for a write: it also answers `NeedsNamed` (as `hxw!`).
    #[cold]
    #[inline(never)]
    fn heap_miss_w<T>(&mut self, mut e: HeapError, again: impl Fn(&mut Self) -> Result<T, HeapError>, px: impl FnOnce(&mut Self) -> Res<T>) -> Res<T> {
        let mut fills = 0u8;
        loop {
            match e {
                HeapError::NeedsHost { .. } | HeapError::NeedsNamed if fills < crate::eval::MAX_FILLS => {
                    fills += 1;
                    self.host_ask(e)?;
                }
                HeapError::Proxy(_) => return px(self),
                e => return Err(self.herr(e)),
            }
            match again(self) {
                Ok(v) => return Ok(v),
                Err(n) => e = n,
            }
        }
    }

    #[inline(always)]
    fn o_get_own_property(&mut self, o: ObjId, key: PropertyKey) -> Res<Option<Property>> {
        match self.heap.get_own_property(o, key) {
            Ok(v) => Ok(v),
            Err(e) => self.heap_miss(e, |s| s.heap.get_own_property(o, key), |s| s.px_get_own_property(o, key)),
        }
    }

    /// The same, for the paths that only need the flags (`peek_own_property` does not make a lazy function prototype).
    #[inline(always)]
    fn o_peek(&mut self, o: ObjId, key: PropertyKey) -> Res<Option<Property>> {
        match self.heap.peek_own_property(o, key) {
            Ok(v) => Ok(v),
            Err(e) => self.heap_miss(e, |s| s.heap.peek_own_property(o, key), |s| s.px_get_own_property(o, key)),
        }
    }

    #[inline(always)]
    fn o_has_own(&mut self, o: ObjId, key: PropertyKey) -> Res<bool> {
        match self.heap.has_own_property(o, key) {
            Ok(v) => Ok(v),
            Err(e) => self.heap_miss(e, |s| s.heap.has_own_property(o, key), |s| Ok(s.px_get_own_property(o, key)?.is_some())),
        }
    }

    #[inline(always)]
    fn o_define_own_property(&mut self, o: ObjId, key: PropertyKey, d: &PropertyDescriptor) -> Res<bool> {
        match self.heap.define_own_property(o, key, d) {
            Ok(v) => Ok(v),
            Err(HeapError::NeedsBigInt) => self.define_big(o, key, d),
            Err(e) => self.heap_miss_w(e, |s| s.heap.define_own_property(o, key, d), |s| s.px_define_own_property(o, key, d)),
        }
    }

    /// `o_define_own_property` of a `BigInt64Array` or `BigUint64Array` element whose `value` is not a BigInt: ToBigInt (as in
    /// `set_miss`), then the define again. A function of its own, so that the proxy path keeps one call level.
    #[cold]
    #[inline(never)]
    fn define_big(&mut self, o: ObjId, key: PropertyKey, d: &PropertyDescriptor) -> Res<bool> {
        let b = self.to_bigint(d.value.unwrap_or(Value::Undefined))?;
        self.o_define_own_property(o, key, &PropertyDescriptor { value: Some(b), ..*d })
    }

    /// `o_has_property` for a key that is never an `on<x>` name (an array index): it does not ask the host.
    #[inline(always)]
    pub(crate) fn o_has_plain(&mut self, o: ObjId, key: PropertyKey) -> Res<bool> {
        match self.heap.has_property(o, key) {
            Ok(v) => Ok(v),
            Err(e) => self.heap_miss(e, |s| s.heap.has_property(o, key), |s| s.o_has_slow(o, key)),
        }
    }

    #[inline(always)]
    fn o_has_property(&mut self, o: ObjId, key: PropertyKey) -> Res<bool> {
        match self.heap.has_property_lazy(o, key) {
            Ok(v) => Ok(v),
            Err(e) => self.heap_miss(e, |s| s.heap.has_property_lazy(o, key), |s| s.o_has_slow(o, key)),
        }
    }

    #[inline(always)]
    fn o_delete(&mut self, o: ObjId, key: PropertyKey) -> Res<bool> {
        match self.heap.delete(o, key) {
            Ok(v) => Ok(v),
            Err(e) => self.heap_miss_w(e, |s| s.heap.delete(o, key), |s| s.px_delete(o, key)),
        }
    }

    #[inline(always)]
    fn o_own_keys(&mut self, o: ObjId) -> Res<Vec<PropertyKey>> {
        match self.heap.own_keys(o) {
            Ok(v) => Ok(v),
            Err(e) => self.heap_miss(e, |s| s.heap.own_keys(o), |s| s.px_own_keys(o)),
        }
    }

    /// `[[Get]](key, receiver)`. The heap answers when no proxy is on the chain.
    fn o_get(&mut self, o: ObjId, key: PropertyKey, receiver: Value) -> Res<Value> {
        match hx!(self, self.heap.get(o, key), Get::Value(self.o_get_slow(o, key, receiver)?)) {
            Get::Value(v) => Ok(v),
            Get::Accessor { getter: Some(g) } => self.call_fn(Value::Object(g), receiver, &[]),
            Get::Accessor { getter: None } => Ok(Value::Undefined),
        }
    }

    /// `[[Set]](key, v, receiver)`. The heap answers when no proxy is on the chain and the receiver is `o`.
    fn o_set_value(&mut self, o: ObjId, key: PropertyKey, v: Value, receiver: Value) -> Res<bool> {
        if matches!(receiver, Value::Object(r) if r == o) {
            return Ok(match hxw!(self, self.heap.set(o, key, v), if self.o_set(o, key, v, receiver)? { Set::Done } else { Set::Rejected }) {
                Set::Done => true,
                Set::Rejected => false,
                Set::Setter(s) => {
                    self.call_fn(Value::Object(s), receiver, &[v])?;
                    true
                }
            });
        }
        self.o_set(o, key, v, receiver)
    }

    /// `IsArray`. A proxy answers for its target, and a revoked proxy is a TypeError.
    fn o_is_array(&mut self, mut o: ObjId) -> Res<bool> {
        loop {
            match self.heap.is_array(o) {
                Ok(b) => return Ok(b),
                Err(HeapError::Proxy(p)) => {
                    self.tick()?;
                    o = self.px_parts(p, "IsArray")?.0;
                }
                Err(e) => return Err(self.herr(e)),
            }
        }
    }

    // ------------------------------------------------------------ the generic algorithms (a proxy on the prototype chain)

    /// `OrdinaryHasProperty` that stops at a proxy.
    #[cold]
    #[inline(never)]
    fn o_has_slow(&mut self, mut o: ObjId, key: PropertyKey) -> Res<bool> {
        loop {
            self.tick()?;
            if self.heap.is_proxy(o) {
                return self.px_has(o, key);
            }
            if hx!(self, self.heap.has_own_property(o, key)) {
                return Ok(true);
            }
            match h!(self, self.heap.get_prototype_of(o)) {
                Some(p) => o = p,
                None => return Ok(false),
            }
        }
    }

    /// `OrdinaryGet` that stops at a proxy.
    #[cold]
    #[inline(never)]
    fn o_get_slow(&mut self, mut o: ObjId, key: PropertyKey, receiver: Value) -> Res<Value> {
        loop {
            self.tick()?;
            if self.heap.is_proxy(o) {
                return self.px_get(o, key, receiver);
            }
            match hx!(self, self.heap.get_own_property(o, key)) {
                Some(Property::Data { value, .. }) => return Ok(value),
                Some(Property::Accessor { get: Some(g), .. }) => return self.call_fn(Value::Object(g), receiver, &[]),
                Some(Property::Accessor { get: None, .. }) => return Ok(Value::Undefined),
                None => match h!(self, self.heap.get_prototype_of(o)) {
                    Some(p) => o = p,
                    None => return Ok(Value::Undefined),
                },
            }
        }
    }

    /// `OrdinarySet` with a receiver (section 10.1.9.2), that stops at a proxy. This is the cold path: it runs when a proxy is the
    /// object, is on the chain, or is the receiver.
    #[cold]
    #[inline(never)]
    fn o_set(&mut self, o: ObjId, key: PropertyKey, mut v: Value, receiver: Value) -> Res<bool> {
        let mut c = o;
        loop {
            self.tick()?;
            if self.heap.is_proxy(c) {
                return self.px_set(c, key, v, receiver);
            }
            if self.heap.has_exotics() && self.heap.is_view_index_key(c, key) {
                if matches!(receiver, Value::Object(r) if r == c) && matches!(v, Value::Object(_)) {
                    v = self.ta_conv(self.heap.view_type(c).unwrap_or(Elem::U8), v)?;
                } else if self.heap.is_view_bad_index(c, key) {
                    return Ok(true);
                }
            }
            match hx!(self, self.heap.get_own_property(c, key)) {
                Some(Property::Data { writable: false, .. }) => return Ok(false),
                Some(Property::Data { .. }) => break,
                Some(Property::Accessor { set: Some(s), .. }) => {
                    self.call_fn(Value::Object(s), receiver, &[v])?;
                    return Ok(true);
                }
                Some(Property::Accessor { set: None, .. }) => return Ok(false),
                None => match h!(self, self.heap.get_prototype_of(c)) {
                    Some(p) => c = p,
                    None => break,
                },
            }
        }
        let Value::Object(r) = receiver else { return Ok(false) };
        match self.o_get_own_property(r, key)? {
            Some(Property::Accessor { .. } | Property::Data { writable: false, .. }) => Ok(false),
            Some(Property::Data { .. }) => self.o_define_own_property(r, key, &PropertyDescriptor { value: Some(v), ..Default::default() }),
            None => self.o_define_own_property(r, key, &PropertyDescriptor::data(v)),
        }
    }

    // ------------------------------------------------------------ the 13 internal methods of a proxy

    #[cold]
    #[inline(never)]
    fn px_get_prototype_of(&mut self, p: ObjId) -> Res<Option<ObjId>> {
        self.descend(|it| {
            let (target, handler, trap) = it.px_enter(p, Trap::GetPrototypeOf)?;
            let Some(trap) = trap else { return it.o_get_prototype_of(target) };
            let r = it.call_fn(trap, Value::Object(handler), &[Value::Object(target)])?;
            let proto = match r {
                Value::Object(o) => Some(o),
                Value::Null => None,
                _ => return Err(it.px_invariant("getPrototypeOf", "trap returned neither object nor null")),
            };
            if it.o_is_extensible(target)? {
                return Ok(proto);
            }
            if it.o_get_prototype_of(target)? != proto {
                return Err(it.px_invariant("getPrototypeOf", "proxy target is non-extensible but the trap did not return its actual prototype"));
            }
            Ok(proto)
        })
    }

    #[cold]
    #[inline(never)]
    fn px_set_prototype_of(&mut self, p: ObjId, proto: Option<ObjId>) -> Res<bool> {
        self.descend(|it| {
            let (target, handler, trap) = it.px_enter(p, Trap::SetPrototypeOf)?;
            let Some(trap) = trap else { return it.o_set_prototype_of(target, proto) };
            let pv = proto.map_or(Value::Null, Value::Object);
            let r = it.call_fn(trap, Value::Object(handler), &[Value::Object(target), pv])?;
            if !it.px_bool(r)? {
                return Ok(false);
            }
            if it.o_is_extensible(target)? {
                return Ok(true);
            }
            if it.o_get_prototype_of(target)? != proto {
                return Err(it.px_invariant("setPrototypeOf", "trap returned truish for setting a new prototype on the non-extensible proxy target"));
            }
            Ok(true)
        })
    }

    #[cold]
    #[inline(never)]
    fn px_is_extensible(&mut self, p: ObjId) -> Res<bool> {
        self.descend(|it| {
            let (target, handler, trap) = it.px_enter(p, Trap::IsExtensible)?;
            let Some(trap) = trap else { return it.o_is_extensible(target) };
            let r = it.call_fn(trap, Value::Object(handler), &[Value::Object(target)])?;
            let result = it.px_bool(r)?;
            if result != it.o_is_extensible(target)? {
                return Err(it.px_invariant("isExtensible", "trap result does not reflect extensibility of proxy target"));
            }
            Ok(result)
        })
    }

    #[cold]
    #[inline(never)]
    fn px_prevent_extensions(&mut self, p: ObjId) -> Res<bool> {
        self.descend(|it| {
            let (target, handler, trap) = it.px_enter(p, Trap::PreventExtensions)?;
            let Some(trap) = trap else { return it.o_prevent_extensions(target) };
            let r = it.call_fn(trap, Value::Object(handler), &[Value::Object(target)])?;
            let result = it.px_bool(r)?;
            if result && it.o_is_extensible(target)? {
                return Err(it.px_invariant("preventExtensions", "trap returned truish but the proxy target is extensible"));
            }
            Ok(result)
        })
    }

    #[cold]
    #[inline(never)]
    fn px_get_own_property(&mut self, p: ObjId, key: PropertyKey) -> Res<Option<Property>> {
        self.descend(|it| {
            let (target, handler, trap) = it.px_enter(p, Trap::GetOwnPropertyDescriptor)?;
            let Some(trap) = trap else { return it.o_get_own_property(target, key) };
            let kv = it.px_key_value(key)?;
            let obj = it.call_fn(trap, Value::Object(handler), &[Value::Object(target), kv])?;
            if !matches!(obj, Value::Object(_) | Value::Undefined) {
                return Err(it.px_invariant("getOwnPropertyDescriptor", "trap returned neither object nor undefined for property"));
            }
            let target_desc = it.o_get_own_property(target, key)?;
            if matches!(obj, Value::Undefined) {
                let Some(td) = target_desc else { return Ok(None) };
                if !td.configurable() {
                    return Err(it.px_invariant("getOwnPropertyDescriptor", "trap returned undefined for a property that is non-configurable on the proxy target"));
                }
                if !it.o_is_extensible(target)? {
                    return Err(it.px_invariant("getOwnPropertyDescriptor", "trap returned undefined for a property of a non-extensible proxy target"));
                }
                return Ok(None);
            }
            let extensible = it.o_is_extensible(target)?;
            let result = it.property_descriptor(obj)?.complete();
            if !Heap::is_compatible_descriptor(extensible, &prop_desc(result), target_desc) {
                return Err(it.px_invariant("getOwnPropertyDescriptor", "trap returned a descriptor that is incompatible with the proxy target"));
            }
            if !result.configurable() {
                match target_desc {
                    None => return Err(it.px_invariant("getOwnPropertyDescriptor", "trap reported non-configurability for a property that does not exist on the proxy target")),
                    Some(td) if td.configurable() => {
                        return Err(it.px_invariant("getOwnPropertyDescriptor", "trap reported non-configurability for a property that is configurable on the proxy target"));
                    }
                    Some(Property::Data { writable: true, .. }) if matches!(result, Property::Data { writable: false, .. }) => {
                        return Err(it.px_invariant("getOwnPropertyDescriptor", "trap reported non-configurable and non-writable for a property that is writable on the proxy target"));
                    }
                    _ => {}
                }
            }
            Ok(Some(result))
        })
    }

    #[cold]
    #[inline(never)]
    fn px_define_own_property(&mut self, p: ObjId, key: PropertyKey, d: &PropertyDescriptor) -> Res<bool> {
        self.descend(|it| {
            let (target, handler, trap) = it.px_enter(p, Trap::DefineProperty)?;
            let Some(trap) = trap else { return it.o_define_own_property(target, key, d) };
            let kv = it.px_key_value(key)?;
            let desc_obj = it.px_desc_object(d)?;
            let r = it.call_fn(trap, Value::Object(handler), &[Value::Object(target), kv, desc_obj])?;
            if !it.px_bool(r)? {
                return Ok(false);
            }
            let target_desc = it.o_get_own_property(target, key)?;
            let extensible = it.o_is_extensible(target)?;
            let setting_config_false = d.configurable == Some(false);
            match target_desc {
                None => {
                    if !extensible {
                        return Err(it.px_invariant("defineProperty", "trap returned truish for adding a property to the non-extensible proxy target"));
                    }
                    if setting_config_false {
                        return Err(it.px_invariant("defineProperty", "trap returned truish for defining non-configurable property which is non-existent in the proxy target"));
                    }
                }
                Some(td) => {
                    if !Heap::is_compatible_descriptor(extensible, d, Some(td)) {
                        return Err(it.px_invariant("defineProperty", "trap returned truish for adding a property that is incompatible with the existing property in the proxy target"));
                    }
                    if setting_config_false && td.configurable() {
                        return Err(it.px_invariant("defineProperty", "trap returned truish for defining non-configurable property which is configurable in the proxy target"));
                    }
                    if matches!(td, Property::Data { configurable: false, writable: true, .. }) && d.writable == Some(false) {
                        return Err(it.px_invariant("defineProperty", "trap returned truish for defining non-configurable property which cannot be non-writable"));
                    }
                }
            }
            Ok(true)
        })
    }

    #[cold]
    #[inline(never)]
    fn px_has(&mut self, p: ObjId, key: PropertyKey) -> Res<bool> {
        self.descend(|it| {
            let (target, handler, trap) = it.px_enter(p, Trap::Has)?;
            let Some(trap) = trap else { return it.o_has_property(target, key) };
            let kv = it.px_key_value(key)?;
            let r = it.call_fn(trap, Value::Object(handler), &[Value::Object(target), kv])?;
            let result = it.px_bool(r)?;
            if !result && let Some(td) = it.o_get_own_property(target, key)? {
                if !td.configurable() {
                    return Err(it.px_invariant("has", "trap returned falsish for a property that is non-configurable on the proxy target"));
                }
                if !it.o_is_extensible(target)? {
                    return Err(it.px_invariant("has", "trap returned falsish for a property of a non-extensible proxy target"));
                }
            }
            Ok(result)
        })
    }

    #[cold]
    #[inline(never)]
    fn px_get(&mut self, p: ObjId, key: PropertyKey, receiver: Value) -> Res<Value> {
        self.descend(|it| {
            let (target, handler, trap) = it.px_enter(p, Trap::Get)?;
            let Some(trap) = trap else { return it.o_get(target, key, receiver) };
            let kv = it.px_key_value(key)?;
            let v = it.call_fn(trap, Value::Object(handler), &[Value::Object(target), kv, receiver])?;
            if let Some(td) = it.o_get_own_property(target, key)?
                && !td.configurable()
            {
                match td {
                    Property::Data { value, writable: false, .. } if !v.same_value(value) => {
                        return Err(it.px_invariant("get", "trap did not return the actual value of a read-only and non-configurable data property of the proxy target"));
                    }
                    Property::Accessor { get: None, .. } if !matches!(v, Value::Undefined) => {
                        return Err(it.px_invariant("get", "trap returned a value for a non-configurable accessor property of the proxy target that has no getter"));
                    }
                    _ => {}
                }
            }
            Ok(v)
        })
    }

    #[cold]
    #[inline(never)]
    fn px_set(&mut self, p: ObjId, key: PropertyKey, v: Value, receiver: Value) -> Res<bool> {
        self.descend(|it| {
            let (target, handler, trap) = it.px_enter(p, Trap::Set)?;
            let Some(trap) = trap else { return it.o_set_value(target, key, v, receiver) };
            let kv = it.px_key_value(key)?;
            let r = it.call_fn(trap, Value::Object(handler), &[Value::Object(target), kv, v, receiver])?;
            if !it.px_bool(r)? {
                return Ok(false);
            }
            if let Some(td) = it.o_get_own_property(target, key)?
                && !td.configurable()
            {
                match td {
                    Property::Data { value, writable: false, .. } if !v.same_value(value) => {
                        return Err(it.px_invariant("set", "trap returned truish for a read-only and non-configurable data property of the proxy target with a different value"));
                    }
                    Property::Accessor { set: None, .. } => {
                        return Err(it.px_invariant("set", "trap returned truish for a non-configurable accessor property of the proxy target that has no setter"));
                    }
                    _ => {}
                }
            }
            Ok(true)
        })
    }

    #[cold]
    #[inline(never)]
    fn px_delete(&mut self, p: ObjId, key: PropertyKey) -> Res<bool> {
        self.descend(|it| {
            let (target, handler, trap) = it.px_enter(p, Trap::DeleteProperty)?;
            let Some(trap) = trap else { return it.o_delete(target, key) };
            let kv = it.px_key_value(key)?;
            let r = it.call_fn(trap, Value::Object(handler), &[Value::Object(target), kv])?;
            if !it.px_bool(r)? {
                return Ok(false);
            }
            let Some(td) = it.o_get_own_property(target, key)? else { return Ok(true) };
            if !td.configurable() {
                return Err(it.px_invariant("deleteProperty", "trap returned truish for a property that is non-configurable on the proxy target"));
            }
            if !it.o_is_extensible(target)? {
                return Err(it.px_invariant("deleteProperty", "trap returned truish for a property of a non-extensible proxy target"));
            }
            Ok(true)
        })
    }

    #[cold]
    #[inline(never)]
    fn px_own_keys(&mut self, p: ObjId) -> Res<Vec<PropertyKey>> {
        self.descend(|it| {
            let (target, handler, trap) = it.px_enter(p, Trap::OwnKeys)?;
            let Some(trap) = trap else { return it.o_own_keys(target) };
            let arr = it.call_fn(trap, Value::Object(handler), &[Value::Object(target)])?;
            if !matches!(arr, Value::Object(_)) {
                return Err(it.type_err("CreateListFromArrayLike called on non-object"));
            }
            // CreateListFromArrayLike(arr, « String, Symbol »): the length and the elements are read through the array.
            let list = it.array_like(arr)?;
            it.ao_temp(list.len(), 24)?;
            let mut keys: Vec<PropertyKey> = Vec::with_capacity(list.len());
            let mut seen: FxSet<PropertyKey> = FxSet::default();
            for v in list {
                it.tick()?;
                if !matches!(v, Value::String(_) | Value::Symbol(_)) {
                    let d = it.display(v);
                    return Err(it.px_invariant("ownKeys", &format!("{d} is not a valid property name")));
                }
                let k = h!(it, it.heap.to_property_key(v));
                if !seen.insert(k) {
                    return Err(it.px_invariant("ownKeys", "trap returned duplicate entries"));
                }
                keys.push(k);
            }
            let extensible = it.o_is_extensible(target)?;
            let target_keys = it.o_own_keys(target)?;
            it.spend(target_keys.len() as u64)?;
            let (mut configurable, mut nonconfigurable) = (Vec::new(), Vec::new());
            for k in target_keys {
                it.tick()?;
                match it.o_get_own_property(target, k)? {
                    Some(d) if !d.configurable() => nonconfigurable.push(k),
                    _ => configurable.push(k),
                }
            }
            if extensible && nonconfigurable.is_empty() {
                return Ok(keys);
            }
            for k in nonconfigurable {
                if !seen.remove(&k) {
                    return Err(it.px_invariant("ownKeys", "trap result did not include a non-configurable key of the proxy target"));
                }
            }
            if extensible {
                return Ok(keys);
            }
            for k in configurable {
                if !seen.remove(&k) {
                    return Err(it.px_invariant("ownKeys", "trap result did not include a key of the non-extensible proxy target"));
                }
            }
            if !seen.is_empty() {
                return Err(it.px_invariant("ownKeys", "trap returned extra keys but proxy target is non-extensible"));
            }
            Ok(keys)
        })
    }

    /// `[[Call]]` of a callable proxy.
    #[cold]
    #[inline(never)]
    fn px_call(&mut self, p: ObjId, this: Value, args: &[Value]) -> Res<Value> {
        let (target, handler, trap) = self.px_enter(p, Trap::Apply)?;
        match trap {
            None => self.call_fn(Value::Object(target), this, args),
            Some(t) => {
                let list = self.new_array_from(args)?;
                self.call_fn(t, Value::Object(handler), &[Value::Object(target), this, list])
            }
        }
    }

    /// `[[Construct]]` of a constructor proxy. `nt` is the `new.target`: the proxy itself for `new p()`.
    #[cold]
    #[inline(never)]
    fn px_construct(&mut self, p: ObjId, args: &[Value], nt: ObjId, callee: ExprId) -> Res<Value> {
        let (target, handler, trap) = self.px_enter(p, Trap::Construct)?;
        match trap {
            None => self.construct_nt(target, args, nt, callee),
            Some(t) => {
                let list = self.new_array_from(args)?;
                let r = self.call_fn(t, Value::Object(handler), &[Value::Object(target), list, Value::Object(nt)])?;
                if !matches!(r, Value::Object(_)) {
                    return Err(self.px_invariant("construct", "trap returned non-object"));
                }
                Ok(r)
            }
        }
    }

    // ------------------------------------------------------------ integrity levels (a proxy has no stored flags)

    /// `SetIntegrityLevel` through the internal methods: `Object.seal` and `Object.freeze` of a proxy.
    #[cold]
    #[inline(never)]
    fn px_set_integrity(&mut self, o: ObjId, frozen: bool) -> Res<bool> {
        if !self.o_prevent_extensions(o)? {
            return Ok(false);
        }
        let keys = self.o_own_keys(o)?;
        for k in keys {
            self.tick()?;
            let d = if frozen {
                match self.o_get_own_property(o, k)? {
                    None => continue,
                    Some(Property::Accessor { .. }) => PropertyDescriptor { configurable: Some(false), ..Default::default() },
                    Some(Property::Data { .. }) => PropertyDescriptor { configurable: Some(false), writable: Some(false), ..Default::default() },
                }
            } else {
                PropertyDescriptor { configurable: Some(false), ..Default::default() }
            };
            if !self.o_define_own_property(o, k, &d)? {
                let t = self.key_text(k);
                return Err(self.type_err(&format!("Cannot redefine property: {t}")));
            }
        }
        Ok(true)
    }

    /// `TestIntegrityLevel` through the internal methods.
    #[cold]
    #[inline(never)]
    fn px_test_integrity(&mut self, o: ObjId, frozen: bool) -> Res<bool> {
        if self.o_is_extensible(o)? {
            return Ok(false);
        }
        for k in self.o_own_keys(o)? {
            self.tick()?;
            match self.o_get_own_property(o, k)? {
                Some(d) if d.configurable() || (frozen && matches!(d, Property::Data { writable: true, .. })) => return Ok(false),
                _ => {}
            }
        }
        Ok(true)
    }

    /// The check of a `for-in` step when a proxy is on the chain: V8 `HasEnumerableProperty`. It walks the chain from `o` and asks each
    /// proxy for its own descriptor, so a key that the proxy no longer reports is skipped. Without a proxy `o_has_property` runs.
    #[cold]
    #[inline(never)]
    fn px_for_in_has(&mut self, mut o: ObjId, key: PropertyKey) -> Res<bool> {
        loop {
            self.tick()?;
            if let Some(p) = self.o_peek(o, key)? {
                return Ok(p.enumerable());
            }
            match self.o_get_prototype_of(o)? {
                Some(p) => o = p,
                None => return Ok(false),
            }
        }
    }

    /// `LengthOfArrayLike` as a data property, for a path that read `length` as an own property (JSON.stringify).
    #[cold]
    #[inline(never)]
    fn px_length_prop(&mut self, o: ObjId) -> Res<Option<Property>> {
        let n = self.length_of(Value::Object(o))?;
        Ok(Some(Property::Data { value: Value::Number(n as f64), writable: true, enumerable: false, configurable: false }))
    }
}
