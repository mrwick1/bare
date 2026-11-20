// Host support for structured clone (HTML "StructuredSerializeInternal"). Included by `eval.rs` (`include!`).
//
// The engine knows which object is a Date, Map, Set, RegExp or Error: that state is in side tables the host cannot read.
// `clone_class` answers it, and the `clone_new_*` functions make the new objects. The walk itself is the host's
// (`crates/bare/src/browser_navigation_state.rs`), because the limits and the `DataCloneError` are the host's.
// An Error has no internal slot here: an ordinary object whose prototype chain holds an `Error.prototype` is an Error.

/// What an object is for structured clone.
#[derive(Clone, Copy, Debug)]
pub enum CloneClass {
    /// An ordinary object, an array or an arguments object: copied by own enumerable keys, the prototype is lost.
    Plain,
    Date(f64),
    /// `(source, flags)`.
    RegExp(StrId, StrId),
    Map,
    Set,
    Error,
    /// A Boolean, Number, String or BigInt wrapper (the primitive).
    Wrapper(Value),
    Buffer,
    /// A typed array or a `DataView`.
    View,
    /// A Promise, a Proxy, a WeakMap, a WeakRef, an iterator, a generator, an Intl object, a Symbol wrapper, a TextEncoder, a
    /// TextDecoder or a host-indexed object: `DataCloneError`.
    Other,
}

impl Interp<'_> {
    /// The class of `o` for structured clone. `o` is not callable (the caller checked).
    pub fn clone_class(&self, o: ObjId) -> CloneClass {
        match self.heap.exotic_kind(o) {
            Some(ExoticKind::Buffer) => return CloneClass::Buffer,
            Some(ExoticKind::View) => return CloneClass::View,
            Some(ExoticKind::Primitive) => {
                return match self.heap.primitive_value(o) {
                    Some(Value::Symbol(_)) | None => CloneClass::Other,
                    Some(v) => CloneClass::Wrapper(v),
                };
            }
            Some(_) => return CloneClass::Other,
            None => {}
        }
        if let Some(&t) = self.date.times.get(&o) {
            return CloneClass::Date(t);
        }
        if let Some(c) = self.sc.colls.get(&o) {
            return if c.is_map { CloneClass::Map } else { CloneClass::Set };
        }
        if let Some(r) = self.ext.regexps.get(&o) {
            return CloneClass::RegExp(r.source, r.flags);
        }
        if self.promise.map.contains_key(&o)
            || self.weak.colls.contains_key(&o)
            || self.weak.refs.contains_key(&o)
            || self.weak.regs.contains_key(&o)
            || self.intl.dtfs.contains_key(&o)
            || self.intl.nfs.contains_key(&o)
            || self.intl.colls.contains_key(&o)
            || self.intl.prs.contains_key(&o)
            || self.intl.segs.contains_key(&o)
            || self.intl.segments.contains_key(&o)
            || self.intl.seg_iters.contains_key(&o)
            || self.sc.iters.contains_key(&o)
            || self.sc.helpers.contains_key(&o)
            || self.aw.gens.contains_key(&o)
        {
            return CloneClass::Other;
        }
        let mut p = self.heap.get_prototype_of(o).ok().flatten();
        for _ in 0..64 {
            let Some(q) = p else { break };
            if q == self.error_protos[0] {
                return CloneClass::Error;
            }
            p = self.heap.get_prototype_of(q).ok().flatten();
        }
        CloneClass::Plain
    }

    /// The live entries of a Map or Set, in order. A Set gives `(key, undefined)`.
    pub fn clone_coll_entries(&self, o: ObjId) -> Vec<(Value, Value)> {
        self.sc.colls.get(&o).map_or_else(Vec::new, |c| c.entries.iter().filter(|e| e.live).map(|e| (e.key, e.val)).collect())
    }

    pub fn clone_new_date(&mut self, t: f64) -> Result<ObjId, Error> {
        match self.date_new(t, self.date.proto)? {
            Value::Object(o) => Ok(o),
            _ => unreachable!(),
        }
    }

    pub fn clone_new_regexp(&mut self, source: StrId, flags: StrId) -> Result<ObjId, Error> {
        self.regexp_create(source, flags)
    }

    pub fn clone_new_coll(&mut self, is_map: bool) -> Result<ObjId, Error> {
        self.coll_new(is_map, if is_map { self.sc.map_proto } else { self.sc.set_proto })
    }

    pub fn clone_coll_put(&mut self, o: ObjId, k: Value, v: Value) -> Result<(), Error> {
        self.coll_set(o, k, v)
    }

    /// A new, empty object with the prototype of the Error type `name` (`Error` for an unknown name).
    pub fn clone_new_error(&mut self, name: &[u16]) -> Result<ObjId, Error> {
        let i = ERR_NAMES.iter().position(|n| n.encode_utf16().eq(name.iter().copied())).unwrap_or(0);
        Ok(h!(self, self.heap.new_object(Some(self.error_protos[i]))))
    }

    /// A new wrapper object of the primitive `v`.
    pub fn clone_new_wrapper(&mut self, v: Value) -> Result<ObjId, Error> {
        match self.to_object(v)? {
            Value::Object(o) => Ok(o),
            _ => unreachable!(),
        }
    }

    /// A new view of `ty` over `buffer`. `ty` is a typed-array class or `Elem::Data`. The caller checked the range.
    pub fn clone_new_view(&mut self, buffer: ObjId, offset: usize, len: usize, ty: Elem) -> Result<ObjId, Error> {
        let p = self.bin_proto(ty);
        Ok(h!(self, self.heap.new_typed_view(Some(p), buffer, offset, len, ty)))
    }
}

impl From<Elem> for bare_js_api::ViewType {
    fn from(e: Elem) -> Self {
        use bare_js_api::ViewType as V;
        match e {
            Elem::I8 => V::I8,
            Elem::U8 => V::U8,
            Elem::U8C => V::U8C,
            Elem::I16 => V::I16,
            Elem::U16 => V::U16,
            Elem::I32 => V::I32,
            Elem::U32 => V::U32,
            Elem::F32 => V::F32,
            Elem::F64 => V::F64,
            Elem::BI64 => V::BI64,
            Elem::BU64 => V::BU64,
            Elem::Data => V::Data,
        }
    }
}

impl From<bare_js_api::ViewType> for Elem {
    fn from(v: bare_js_api::ViewType) -> Self {
        use bare_js_api::ViewType as V;
        match v {
            V::I8 => Elem::I8,
            V::U8 => Elem::U8,
            V::U8C => Elem::U8C,
            V::I16 => Elem::I16,
            V::U16 => Elem::U16,
            V::I32 => Elem::I32,
            V::U32 => Elem::U32,
            V::F32 => Elem::F32,
            V::F64 => Elem::F64,
            V::BI64 => Elem::BI64,
            V::BU64 => Elem::BU64,
            V::Data => Elem::Data,
        }
    }
}

/// The structured-clone methods of the interface, over the inherent ones above. The inherent method wins in a call through `Interp::`.
impl<'p> bare_js_api::CloneOps for Interp<'p> {
    fn object_id(&self, v: bare_js_api::Local<'_>) -> Option<u64> {
        match self.api_at(v) {
            Value::Object(o) => Some((o.gn as u64) << 32 | o.index as u64),
            _ => None,
        }
    }

    fn clone_class<'s>(&mut self, v: bare_js_api::Local<'_>) -> bare_js_api::CloneClass<'s> {
        use bare_js_api::CloneClass as C;
        let Value::Object(o) = self.api_at(v) else { return C::Other };
        match Interp::clone_class(self, o) {
            CloneClass::Plain => {
                if self.heap.is_array(o).unwrap_or(false) {
                    let len = match self.heap.get_own_property(o, self.k_length) {
                        Ok(Some(Property::Data { value: Value::Number(n), .. })) => n as u32,
                        _ => 0,
                    };
                    C::Array(len)
                } else {
                    C::Plain
                }
            }
            CloneClass::Date(t) => C::Date(t),
            CloneClass::RegExp(s, f) => C::RegExp(self.api_push(Value::String(s)), self.api_push(Value::String(f))),
            CloneClass::Map => C::Map,
            CloneClass::Set => C::Set,
            CloneClass::Error => C::Error,
            CloneClass::Wrapper(p) => C::Wrapper(self.api_push(p)),
            CloneClass::Buffer => C::Buffer,
            CloneClass::View => C::View,
            CloneClass::Other => C::Other,
        }
    }

    fn clone_keys<'s>(&mut self, obj: bare_js_api::Local<'_>) -> Result<Vec<bare_js_api::CloneKey<'s>>, bare_js_api::Error<'s>> {
        use bare_js_api::CloneKey as K;
        let r = self.api_obj(obj).and_then(|o| Ok(h!(self, self.heap.own_keys(o))));
        match r {
            Ok(keys) => Ok(keys
                .into_iter()
                .filter_map(|k| match k {
                    PropertyKey::Index(i) => Some(K::Index(i)),
                    PropertyKey::String(s) => Some(K::Name(self.api_push(Value::String(s)))),
                    PropertyKey::Symbol(_) => None,
                })
                .collect()),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn clone_get<'s>(&mut self, obj: bare_js_api::Local<'_>, key: bare_js_api::CloneKey<'_>) -> Result<Option<bare_js_api::Local<'s>>, bare_js_api::Error<'s>> {
        use bare_js_api::CloneKey as K;
        let r = self.api_obj(obj).and_then(|o| {
            let k = match key {
                K::Index(i) => PropertyKey::Index(i),
                K::Name(l) => match self.api_at(l) {
                    // `clone_keys` made it from a string key: an atom that is not an index.
                    Value::String(s) => PropertyKey::String(s),
                    _ => return Err(self.type_error("not a key")),
                },
            };
            match h!(self, self.heap.get_own_property(o, k)) {
                Some(p) if p.enumerable() => self.get_value(Value::Object(o), k).map(Some),
                _ => Ok(None),
            }
        });
        match r {
            Ok(v) => Ok(v.map(|v| self.api_push(v))),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn clone_define<'s>(&mut self, obj: bare_js_api::Local<'_>, name: bare_js_api::CloneName<'_>, value: bare_js_api::Local<'_>) -> Result<(), bare_js_api::Error<'s>> {
        use bare_js_api::CloneName as N;
        let r = self.api_obj(obj).and_then(|o| {
            let k = match name {
                N::Index(i) => PropertyKey::Index(i),
                N::Units(u) => {
                    let s = h!(self, self.heap.intern_units(u));
                    h!(self, self.heap.key_from_string(s))
                }
            };
            self.define_data(o, k, self.api_at(value), true, true, true)
        });
        r.map_err(|e| self.api_err(e))
    }

    fn own_data<'s>(&mut self, obj: bare_js_api::Local<'_>, key: bare_js_api::Local<'_>) -> Result<Option<bare_js_api::Local<'s>>, bare_js_api::Error<'s>> {
        let r = self.api_obj(obj).and_then(|o| {
            let k = self.api_key(key)?;
            Ok(match h!(self, self.heap.get_own_property(o, k)) {
                Some(Property::Data { value, .. }) => Some(value),
                _ => None,
            })
        });
        match r {
            Ok(v) => Ok(v.map(|v| self.api_push(v))),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn clone_coll_entries<'s>(&mut self, coll: bare_js_api::Local<'_>) -> Vec<(bare_js_api::Local<'s>, bare_js_api::Local<'s>)> {
        let Value::Object(o) = self.api_at(coll) else { return Vec::new() };
        Interp::clone_coll_entries(self, o).into_iter().map(|(k, v)| (self.api_push(k), self.api_push(v))).collect()
    }

    fn clone_new_date<'s>(&mut self, t: f64) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        match Interp::clone_new_date(self, t) {
            Ok(o) => Ok(self.api_push(Value::Object(o))),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn clone_new_regexp<'s>(&mut self, source: bare_js_api::Local<'_>, flags: bare_js_api::Local<'_>) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        let (Value::String(s), Value::String(f)) = (self.api_at(source), self.api_at(flags)) else {
            let e = self.type_error("not a string");
            return Err(self.api_err(e));
        };
        match Interp::clone_new_regexp(self, s, f) {
            Ok(o) => Ok(self.api_push(Value::Object(o))),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn clone_new_coll<'s>(&mut self, is_map: bool) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        match Interp::clone_new_coll(self, is_map) {
            Ok(o) => Ok(self.api_push(Value::Object(o))),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn clone_coll_put<'s>(&mut self, coll: bare_js_api::Local<'_>, key: bare_js_api::Local<'_>, value: bare_js_api::Local<'_>) -> Result<(), bare_js_api::Error<'s>> {
        let r = self.api_obj(coll).and_then(|o| Interp::clone_coll_put(self, o, self.api_at(key), self.api_at(value)));
        r.map_err(|e| self.api_err(e))
    }

    fn clone_new_error<'s>(&mut self, name: &[u16]) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        match Interp::clone_new_error(self, name) {
            Ok(o) => Ok(self.api_push(Value::Object(o))),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn clone_new_wrapper<'s>(&mut self, v: bare_js_api::Local<'_>) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        match Interp::clone_new_wrapper(self, self.api_at(v)) {
            Ok(o) => Ok(self.api_push(Value::Object(o))),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn clone_new_view<'s>(&mut self, buffer: bare_js_api::Local<'_>, offset: usize, len: usize, ty: bare_js_api::ViewType) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        let r = self.api_obj(buffer).and_then(|b| Interp::clone_new_view(self, b, offset, len, ty.into()));
        match r {
            Ok(o) => Ok(self.api_push(Value::Object(o))),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn new_array_buffer<'s>(&mut self, bytes: Vec<u8>) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        match Interp::new_array_buffer(self, bytes) {
            Ok(v) => Ok(self.api_push(v)),
            Err(e) => Err(self.api_err(e)),
        }
    }

    fn buffer_len(&self, v: bare_js_api::Local<'_>) -> Option<usize> {
        match self.api_at(v) {
            Value::Object(o) => self.heap.buffer_len(o),
            _ => None,
        }
    }

    fn buffer_bytes(&self, v: bare_js_api::Local<'_>) -> Option<&[u8]> {
        match self.api_at(v) {
            Value::Object(o) => self.heap.bytes(o),
            _ => None,
        }
    }

    fn buffer_detached(&self, v: bare_js_api::Local<'_>) -> bool {
        matches!(self.api_at(v), Value::Object(o) if self.heap.buffer_detached(o))
    }

    fn view_parts<'s>(&mut self, v: bare_js_api::Local<'_>) -> Option<(bare_js_api::Local<'s>, usize, usize)> {
        let Value::Object(o) = self.api_at(v) else { return None };
        let (b, off, len) = self.heap.view_parts(o)?;
        Some((self.api_push(Value::Object(b)), off, len))
    }

    fn view_type(&self, v: bare_js_api::Local<'_>) -> Option<bare_js_api::ViewType> {
        match self.api_at(v) {
            Value::Object(o) => self.heap.view_type(o).map(Into::into),
            _ => None,
        }
    }

    fn view_detached(&self, v: bare_js_api::Local<'_>) -> bool {
        matches!(self.api_at(v), Value::Object(o) if self.heap.view_detached(o))
    }

    fn bigint_parts(&self, v: bare_js_api::Local<'_>) -> Option<(bool, Vec<u64>)> {
        self.heap.bigint_parts(self.api_at(v))
    }

    fn bigint_from_parts<'s>(&mut self, negative: bool, limbs: Vec<u64>) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        match self.heap.bigint_from_parts(negative, limbs) {
            Ok(v) => Ok(self.api_push(v)),
            Err(e) => {
                let e = self.herr(e);
                Err(self.api_err(e))
            }
        }
    }
}
