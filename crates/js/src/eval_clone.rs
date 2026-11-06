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
        match self.date_new(t)? {
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
