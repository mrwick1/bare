// Structured clone (`docs/design-host-clone.md`, plan step 3). Included by `eval.rs` (`include!`).
//
// The engine walks plain data itself (`clone_out`) and writes a record with `bare_js_api::clone`. It builds the graph back from a record
// (`clone_in`). The host sees only host objects, the transfer list and the error (`CloneHost`). The engine knows which object is a Date,
// Map, Set, RegExp or Error: that state is in side tables the host cannot read. `clone_class` answers it, and the `clone_new_*`
// functions make the new objects. This is the generic path: every property goes through `own_keys`, `get_own_property`, `get_value`
// and `define_data`. An Error has no internal slot here: an ordinary object whose prototype chain holds an `Error.prototype` is an Error.

use bare_js_api::clone as cl;

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

// ---------- the walk: `Engine::clone_out` ----------

/// An identity of an object for `cl::IdMap`: the whole handle, so a reused slot is another object.
fn clone_obj_key(o: ObjId) -> u64 {
    (o.gn as u64) << 32 | o.index as u64
}

/// An identity of a key string for the key table of a record. A key is an atom, so the handle is the content.
fn clone_str_key(s: StrId) -> u64 {
    (s.gn as u64) << 32 | s.index as u64
}

struct CloneWalk<'a, 'p> {
    w: cl::RecordWriter,
    m: cl::Meter,
    /// The objects that got a number (an object to its id), in the order of the record.
    ids: cl::IdMap,
    /// The listed objects (an object to its place in the transfer list). Used only when `listed` is not 0.
    xfer: cl::IdMap,
    listed: usize,
    h: &'a mut dyn bare_js_api::CloneHost<Interp<'p>>,
}

impl<'p> CloneWalk<'_, 'p> {
    /// Charge the steps that the cost model has added. The old walk charged each step when it happened; the total is the same.
    fn flush(&mut self, it: &mut Interp<'p>) -> Result<(), Error> {
        match self.m.take_steps() {
            0 => Ok(()),
            n => it.charge(n),
        }
    }

    /// The result of a charge of the cost model: the steps of the charge go to the budget, or the limit becomes the error. The steps that
    /// came before the failing byte charge go first, so the step budget can still win, as in the old walk.
    fn done(&mut self, it: &mut Interp<'p>, r: Result<(), bare_js_api::Abort>) -> Result<(), Error> {
        match r {
            Ok(()) => self.flush(it),
            Err(a) => {
                self.flush(it)?;
                Err(match a {
                    bare_js_api::Abort::Limit(name) => Error::Limit(LimitKind::Host(name)),
                    bare_js_api::Abort::Unsupported(what) => unsupported(what, Span::default()),
                })
            }
        }
    }

    /// The `DataCloneError` that the host makes for `what`.
    fn refuse(&mut self, it: &mut Interp<'p>, what: bare_js_api::Refused) -> Error {
        let h = &mut *self.h;
        bare_js_api::enter(it, |s| {
            let e = h.data_clone_error(s, what);
            s.engine().api_unerr(e)
        })
    }

    fn string(&mut self, it: &mut Interp<'p>, s: StrId) -> Result<(), Error> {
        let len = it.heap.str_len(s).map_err(Error::Heap)?;
        let r = self.m.string(len);
        self.done(it, r)?;
        match it.heap.str_units(s) {
            Ok(u) => {
                self.w.string(u);
                Ok(())
            }
            Err(e) => Err(it.heap_error(e)),
        }
    }

    fn val(&mut self, it: &mut Interp<'p>, v: Value, depth: usize) -> Result<(), Error> {
        self.m.value();
        self.flush(it)?;
        match v {
            Value::Undefined => self.w.undefined(),
            Value::Null => self.w.null(),
            Value::Bool(b) => self.w.boolean(b),
            Value::Number(n) => self.w.number(n),
            Value::Symbol(_) => return Err(self.refuse(it, bare_js_api::Refused::Symbol)),
            Value::BigInt(_) | Value::BigHeap(_) => {
                let (negative, limbs) = it.heap.bigint_parts(v).expect("a BigInt");
                let r = self.m.bigint(limbs.len());
                self.done(it, r)?;
                self.w.bigint(negative, &limbs);
            }
            Value::String(s) => self.string(it, s)?,
            Value::Object(o) => self.object(it, v, o, depth)?,
        }
        Ok(())
    }

    fn object(&mut self, it: &mut Interp<'p>, v: Value, o: ObjId, depth: usize) -> Result<(), Error> {
        use bare_js_api::Refused;
        let key = clone_obj_key(o);
        if let Some(id) = self.ids.get(key) {
            self.w.reference(id);
            return Ok(());
        }
        if self.listed != 0
            && let Some(i) = self.xfer.get(key)
        {
            let r = self.m.moved(it.heap.buffer_len(o).unwrap_or(0));
            self.done(it, r)?;
            let id = self.w.moved(i);
            self.ids.insert(key, id);
            return Ok(());
        }
        if it.is_callable_value(v) {
            return Err(self.refuse(it, Refused::Function));
        }
        if self.h.is_host_object(&v) {
            return Err(self.refuse(it, Refused::Object));
        }
        let class = it.clone_class(o);
        if matches!(class, CloneClass::Other) {
            return Err(self.refuse(it, Refused::Object));
        }
        let r = self.m.node(depth);
        self.done(it, r)?;
        match class {
            CloneClass::Plain => {
                if it.heap.is_array(o).map_err(Error::Heap)? {
                    let len = match it.heap.get_own_property(o, it.k_length) {
                        Ok(Some(Property::Data { value: Value::Number(n), .. })) => n as u32,
                        _ => 0,
                    };
                    let id = self.w.array_sparse(len);
                    self.ids.insert(key, id);
                } else {
                    let id = self.w.object();
                    self.ids.insert(key, id);
                }
                self.props(it, o, depth)?;
                self.w.end();
            }
            CloneClass::Date(t) => {
                let id = self.w.date(t);
                self.ids.insert(key, id);
            }
            CloneClass::RegExp(s, f) => {
                for x in [s, f] {
                    let len = it.heap.str_len(x).map_err(Error::Heap)?;
                    let r = self.m.string(len);
                    self.done(it, r)?;
                }
                match (it.heap.str_units(s), it.heap.str_units(f)) {
                    (Ok(s), Ok(f)) => {
                        let id = self.w.regexp(s, f);
                        self.ids.insert(key, id);
                    }
                    (Err(e), _) | (_, Err(e)) => return Err(it.heap_error(e)),
                }
            }
            CloneClass::Map | CloneClass::Set => {
                let is_map = matches!(class, CloneClass::Map);
                let id = if is_map { self.w.map() } else { self.w.set() };
                self.ids.insert(key, id);
                for (k, val) in it.clone_coll_entries(o) {
                    let r = self.m.entry();
                    self.done(it, r)?;
                    self.val(it, k, depth + 1)?;
                    if is_map {
                        self.val(it, val, depth + 1)?;
                    }
                }
                self.w.end();
            }
            CloneClass::Error => {
                let name = it.get_value(v, it.k_name)?;
                let mut empty: &[u16] = &[];
                if let Value::String(s) = name {
                    let len = it.heap.str_len(s).map_err(Error::Heap)?;
                    let r = self.m.string(len);
                    self.done(it, r)?;
                    empty = match it.heap.str_units(s) {
                        Ok(u) => u,
                        Err(e) => return Err(it.heap_error(e)),
                    };
                }
                let id = self.w.error(empty);
                self.ids.insert(key, id);
                let k_cause = it.heap.key_from_str("cause").map_err(|e| it.heap_error(e))?;
                // `message` goes through `ToString`, `stack` is kept only when it is a string, and all three are own data properties.
                for (k, f, to_str, only_str) in [(it.k_message, cl::ErrorField::Message, true, false), (it.k_stack, cl::ErrorField::Stack, false, true), (k_cause, cl::ErrorField::Cause, false, false)] {
                    let Some(Property::Data { value, .. }) = it.heap.get_own_property(o, k).map_err(Error::Heap)? else { continue };
                    if only_str && !matches!(value, Value::String(_)) {
                        continue;
                    }
                    let value = if to_str { Value::String(it.to_js_string(value)?) } else { value };
                    self.w.error_field(f);
                    self.val(it, value, depth + 1)?;
                }
                self.w.end();
            }
            CloneClass::Wrapper(p) => {
                let id = self.w.wrapper();
                self.ids.insert(key, id);
                self.val(it, p, depth + 1)?;
            }
            CloneClass::Buffer => {
                if it.heap.buffer_detached(o) {
                    return Err(self.refuse(it, Refused::DetachedBuffer));
                }
                let n = it.heap.buffer_len(o).expect("a buffer");
                let r = self.m.buffer(n);
                self.done(it, r)?;
                self.w.reserve(n);
                let id = self.w.buffer(it.heap.bytes(o).expect("a buffer"));
                self.ids.insert(key, id);
            }
            CloneClass::View => {
                let (buf, off, len) = it.heap.view_parts(o).expect("a view");
                if it.heap.view_detached(o) {
                    return Err(self.refuse(it, Refused::DetachedBuffer));
                }
                let ty = it.heap.view_type(o).expect("a view");
                self.w.view(ty.index() as u8, off, len);
                // The buffer is a `Ref`, a `Buf` or a `Moved`. The view takes its number after it.
                self.val(it, Value::Object(buf), depth + 1)?;
                let id = self.w.view_done();
                self.ids.insert(key, id);
            }
            CloneClass::Other => unreachable!(),
        }
        Ok(())
    }

    /// The own enumerable string keys of a plain object or array, with their values.
    fn props(&mut self, it: &mut Interp<'p>, o: ObjId, depth: usize) -> Result<(), Error> {
        let keys = match it.heap.own_keys(o) {
            Ok(k) => k,
            Err(e) => return Err(it.heap_error(e)),
        };
        for key in keys {
            self.m.step(1);
            self.flush(it)?;
            if matches!(key, PropertyKey::Symbol(_)) {
                continue;
            }
            // Own and enumerable now, as the spec checks just before each `[[Get]]`.
            match it.heap.get_own_property(o, key) {
                Ok(Some(p)) if p.enumerable() => {}
                Ok(_) => continue,
                Err(e) => return Err(it.heap_error(e)),
            }
            let r = match key {
                PropertyKey::Index(i) => {
                    let r = self.m.key_index();
                    self.w.key_index(i);
                    r
                }
                PropertyKey::String(s) => match it.heap.str_units(s) {
                    Ok(u) => {
                        let r = self.m.key_name(u.len());
                        self.w.key(clone_str_key(s), u);
                        r
                    }
                    Err(e) => return Err(it.heap_error(e)),
                },
                PropertyKey::Symbol(_) => unreachable!(),
            };
            self.done(it, r)?;
            let val = it.get_value(Value::Object(o), key)?;
            self.val(it, val, depth + 1)?;
        }
        Ok(())
    }
}

impl<'p> Interp<'p> {
    /// `Engine::clone_out` over the values of this engine.
    fn clone_out_impl(&mut self, v: Value, o: &bare_js_api::CloneOpts<'_, '_>, h: &mut dyn bare_js_api::CloneHost<Interp<'p>>) -> Result<bare_js_api::Cloned, Error> {
        use bare_js_api::{Moved, Refused};
        let list: Vec<ObjId> = o.transfer.iter().filter_map(|&l| if let Value::Object(x) = self.api_at(l) { Some(x) } else { None }).collect();
        let mut walk = CloneWalk { w: cl::RecordWriter::new(), m: cl::Meter::new(o.limits), ids: cl::IdMap::default(), xfer: cl::IdMap::default(), listed: list.len(), h };
        for (i, &x) in list.iter().enumerate() {
            walk.xfer.insert(clone_obj_key(x), i as u32);
        }
        walk.val(self, v, 0)?;
        // A listed item that the graph does not use still costs its bytes.
        for &x in &list {
            if walk.ids.get(clone_obj_key(x)).is_none() {
                let r = walk.m.unused(self.heap.buffer_len(x).unwrap_or(0));
                walk.done(self, r)?;
            }
        }
        let is_buf = |it: &Interp<'_>, x: ObjId| matches!(it.heap.exotic_kind(x), Some(ExoticKind::Buffer));
        for &x in &list {
            let ok = if is_buf(self, x) { !self.heap.buffer_detached(x) } else { walk.h.can_transfer(&Value::Object(x)) };
            if !ok {
                return Err(walk.refuse(self, Refused::DetachedTransfer));
            }
        }
        let mut moved = Vec::with_capacity(list.len());
        for &x in &list {
            moved.push(if is_buf(self, x) {
                let bytes = self.heap.bytes(x).expect("a buffer").to_vec();
                self.heap.buffer_transfer(None, x, 0).map_err(|e| self.heap_error(e))?;
                Moved::Buffer(bytes)
            } else {
                Moved::Host(walk.h.transfer_out(&Value::Object(x)))
            });
        }
        let cost = walk.m.bytes();
        Ok(bare_js_api::Cloned::new(walk.w.finish(), moved, cost))
    }
}

// ---------- the reader: `Engine::clone_in` ----------

/// A moved item of a record, as the reader holds it.
enum CloneItem {
    /// The bytes of a transferred buffer, until its `Moved` tag makes the object.
    Buf(Vec<u8>),
    /// The new object of a host item.
    Host(Value),
}

struct CloneRd<'r> {
    r: cl::RecordReader<'r>,
    /// The objects of the record by number.
    objs: Vec<ObjId>,
    keys: Vec<PropertyKey>,
    items: Vec<CloneItem>,
    k_cause: PropertyKey,
    op: ObjId,
    ap: ObjId,
}

macro_rules! rd {
    ($e:expr) => {
        $e.map_err(|_| unsupported("damaged clone record", Span::default()))?
    };
}

impl<'p> Interp<'p> {
    /// `Engine::clone_in`: the graph and the new objects of the host items (in list order). No script runs: every object is new.
    fn clone_in_impl(&mut self, c: &mut bare_js_api::Cloned, h: &mut dyn bare_js_api::CloneHost<Interp<'p>>) -> Result<(Value, Vec<Value>), Error> {
        use bare_js_api::Moved;
        let mut items = Vec::new();
        let mut host_in = Vec::new();
        let mut kinds = Vec::new();
        for m in c.take_moved() {
            match m {
                Moved::Buffer(b) => kinds.push(Some(b)),
                Moved::Host(x) => {
                    kinds.push(None);
                    host_in.push(x);
                }
            }
        }
        // The new objects of the host items come first, as the ports did before the nodes.
        let hosts: Vec<Value> = if host_in.is_empty() {
            Vec::new()
        } else {
            bare_js_api::enter(self, |s| match h.transfer_in(s, host_in) {
                Ok(ls) => Ok(ls.iter().map(|&l| s.engine().api_at(l)).collect()),
                Err(e) => Err(s.engine().api_unerr(e)),
            })?
        };
        let mut next = hosts.iter();
        for k in kinds {
            items.push(match k {
                Some(b) => CloneItem::Buf(b),
                None => CloneItem::Host(*next.next().ok_or_else(|| unsupported("damaged clone record", Span::default()))?),
            });
        }
        let k_cause = h!(self, self.heap.key_from_str("cause"));
        let (op, ap) = (self.object_prototype(), self.array_prototype());
        let mut st = CloneRd { r: cl::RecordReader::new(c.bytes()), objs: Vec::new(), keys: Vec::new(), items, k_cause, op, ap };
        let root = self.clone_rv(&mut st)?;
        rd!(st.r.finish());
        Ok((root, hosts))
    }

    fn clone_str(&mut self, t: cl::Text<'_>) -> Result<StrId, Error> {
        let n = t.len();
        let mut small = [0u16; 64];
        let mut big = Vec::new();
        let buf: &mut [u16] = if n <= 64 {
            &mut small[..n]
        } else {
            big.resize(n, 0);
            &mut big
        };
        match t {
            cl::Text::Latin1(b) => buf.iter_mut().zip(b).for_each(|(d, &c)| *d = c as u16),
            cl::Text::Utf16(b) => buf.iter_mut().zip(b.chunks_exact(2)).for_each(|(d, c)| *d = u16::from_le_bytes([c[0], c[1]])),
        }
        Ok(h!(self, self.heap.intern_units(buf)))
    }

    fn clone_key(&mut self, st: &mut CloneRd<'_>, k: cl::Key<'_>) -> Result<PropertyKey, Error> {
        Ok(match k {
            cl::Key::Index(i) => PropertyKey::Index(i),
            cl::Key::Ref(n) => st.keys[n as usize],
            cl::Key::New(t) => {
                let s = self.clone_str(t)?;
                let k = h!(self, self.heap.key_from_string(s));
                st.keys.push(k);
                k
            }
        })
    }

    fn clone_obj(&mut self, st: &mut CloneRd<'_>, o: ObjId) -> Result<Value, Error> {
        st.objs.push(o);
        self.charge(1)?;
        Ok(Value::Object(o))
    }

    fn clone_rv(&mut self, st: &mut CloneRd<'_>) -> Result<Value, Error> {
        let tag = rd!(st.r.tag());
        self.clone_rv_tag(st, tag)
    }

    /// The properties of a plain object or an array, up to `End`.
    fn clone_props(&mut self, st: &mut CloneRd<'_>, o: ObjId) -> Result<(), Error> {
        while let Some(k) = rd!(st.r.key()) {
            self.charge(1)?;
            let key = self.clone_key(st, k)?;
            let val = self.clone_rv(st)?;
            self.define_data(o, key, val, true, true, true)?;
        }
        Ok(())
    }

    fn clone_rv_tag(&mut self, st: &mut CloneRd<'_>, tag: cl::Tag) -> Result<Value, Error> {
        use cl::Tag;
        Ok(match tag {
            Tag::Undef => Value::Undefined,
            Tag::Null => Value::Null,
            Tag::True => Value::Bool(true),
            Tag::False => Value::Bool(false),
            Tag::Int => Value::Number(rd!(st.r.int()) as f64),
            Tag::Num => Value::Number(rd!(st.r.num())),
            Tag::Str8 | Tag::Str16 => {
                let t = rd!(st.r.text(tag));
                Value::String(self.clone_str(t)?)
            }
            Tag::Big => {
                let mut limbs = Vec::new();
                let negative = rd!(st.r.bigint(&mut limbs));
                h!(self, self.heap.bigint_from_parts(negative, limbs))
            }
            Tag::Ref => {
                let id = rd!(st.r.reference());
                Value::Object(*st.objs.get(id as usize).ok_or_else(|| unsupported("damaged clone record", Span::default()))?)
            }
            Tag::Obj => {
                let o = h!(self, self.heap.new_object(Some(st.op)));
                let v = self.clone_obj(st, o)?;
                self.clone_props(st, o)?;
                v
            }
            Tag::Arr | Tag::ArrD => {
                let o = h!(self, self.heap.new_array(Some(st.ap)));
                let v = self.clone_obj(st, o)?;
                let len = if tag == Tag::Arr {
                    rd!(st.r.sparse_len())
                } else {
                    let n = rd!(st.r.dense_len());
                    for i in 0..n {
                        let t = rd!(st.r.tag());
                        if t == Tag::Hole {
                            continue;
                        }
                        self.charge(1)?;
                        let val = self.clone_rv_tag(st, t)?;
                        self.define_data(o, PropertyKey::Index(i as u32), val, true, true, true)?;
                    }
                    n as u32
                };
                self.clone_props(st, o)?;
                self.put_value(v, self.k_length, Value::Number(len as f64))?;
                v
            }
            Tag::Date => {
                let o = self.clone_new_date(rd!(st.r.num()))?;
                self.clone_obj(st, o)?
            }
            Tag::RegExp => {
                let (s, f) = rd!(st.r.regexp());
                let (s, f) = (self.clone_str(s)?, self.clone_str(f)?);
                let o = self.clone_new_regexp(s, f)?;
                self.clone_obj(st, o)?
            }
            Tag::Map | Tag::Set => {
                let is_map = tag == Tag::Map;
                let o = self.clone_new_coll(is_map)?;
                let v = self.clone_obj(st, o)?;
                while !rd!(st.r.end()) {
                    let k = self.clone_rv(st)?;
                    let val = if is_map { self.clone_rv(st)? } else { k };
                    self.charge(1)?;
                    self.clone_coll_put(o, k, val)?;
                }
                v
            }
            Tag::Error => {
                let name = rd!(st.r.string()).to_vec();
                let o = self.clone_new_error(&name)?;
                let v = self.clone_obj(st, o)?;
                while let Some(f) = rd!(st.r.error_field()) {
                    let key = match f {
                        cl::ErrorField::Message => self.k_message,
                        cl::ErrorField::Stack => self.k_stack,
                        cl::ErrorField::Cause => st.k_cause,
                    };
                    let val = self.clone_rv(st)?;
                    self.define_data(o, key, val, true, false, true)?;
                }
                v
            }
            Tag::Wrapper => {
                let p = self.clone_rv(st)?;
                let o = self.clone_new_wrapper(p)?;
                self.clone_obj(st, o)?
            }
            Tag::Buf => {
                let b = rd!(st.r.buffer()).to_vec();
                let Value::Object(o) = self.new_array_buffer(b)? else { unreachable!() };
                self.clone_obj(st, o)?
            }
            Tag::View => {
                let (ty, off, len) = rd!(st.r.view());
                let Value::Object(buf) = self.clone_rv(st)? else { return Err(unsupported("damaged clone record", Span::default())) };
                st.r.view_done();
                let ty = match ty as usize {
                    11 => Elem::Data,
                    i if i < TYPED.len() => TYPED[i],
                    _ => return Err(unsupported("damaged clone record", Span::default())),
                };
                let o = self.clone_new_view(buf, off, len, ty)?;
                self.clone_obj(st, o)?
            }
            Tag::Moved => {
                let i = rd!(st.r.moved()) as usize;
                let o = match st.items.get_mut(i) {
                    Some(CloneItem::Buf(b)) => match self.new_array_buffer(std::mem::take(b))? {
                        Value::Object(o) => o,
                        _ => unreachable!(),
                    },
                    Some(CloneItem::Host(Value::Object(o))) => *o,
                    _ => return Err(unsupported("damaged clone record", Span::default())),
                };
                self.clone_obj(st, o)?
            }
            _ => return Err(unsupported("damaged clone record", Span::default())),
        })
    }
}
