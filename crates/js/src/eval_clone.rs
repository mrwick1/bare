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

// ---------- spike: native record writer and reader for the data classes (design-host-clone.md, step 1) ----------
//
// `clone_out_fast` writes plain objects and dense arrays (plain data properties only, so no getter runs), primitives other than Symbol
// and BigInt, strings and cycles to one byte buffer. For anything else it gives `None` with nothing changed, and the caller runs the
// old walk. `clone_in` builds the graph from the buffer. The cost model and the step charges are the ones of the old walk.

const T_UNDEF: u8 = 0;
const T_NULL: u8 = 1;
const T_TRUE: u8 = 2;
const T_FALSE: u8 = 3;
const T_INT: u8 = 4;
const T_NUM: u8 = 5;
const T_STR8: u8 = 6;
const T_STR16: u8 = 7;
const T_REF: u8 = 8;
const T_OBJ: u8 = 9;
const T_ARRD: u8 = 10;
const T_END: u8 = 11;
const T_KNEW8: u8 = 12;
const T_KNEW16: u8 = 13;
const T_KREF: u8 = 14;

/// A structured clone as bytes, and its cost (the `bytes` of the old `State`).
pub struct CloneRec {
    pub bytes: Box<[u8]>,
    pub cost: usize,
}

#[derive(Default)]
struct IdHash(u64);

impl std::hash::Hasher for IdHash {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, b: &[u8]) {
        for &x in b {
            self.write_u32(x as u32);
        }
    }
    fn write_u32(&mut self, i: u32) {
        self.0 = (self.0.rotate_left(5) ^ i as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

fn put_varint(out: &mut Vec<u8>, mut n: u64) {
    while n >= 0x80 {
        out.push(n as u8 | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

struct CloneW<'a, 'i> {
    it: &'a Interp<'i>,
    host: &'a dyn Fn(ObjId) -> bool,
    out: Vec<u8>,
    ids: Vec<(ObjId, u32)>,
    big: Option<HashMap<ObjId, u32, std::hash::BuildHasherDefault<IdHash>>>,
    keys: Vec<PropertyKey>,
    bytes: usize,
    steps: u64,
    nodes: usize,
    max: (usize, usize, usize),
}

impl CloneW<'_, '_> {
    fn add(&mut self, n: usize) -> Option<()> {
        self.bytes = self.bytes.saturating_add(n);
        (self.bytes <= self.max.0).then_some(())
    }

    fn id_of(&self, o: ObjId) -> Option<u32> {
        match &self.big {
            Some(m) => m.get(&o).copied(),
            None => self.ids.iter().find(|e| e.0 == o).map(|e| e.1),
        }
    }

    fn id_put(&mut self, o: ObjId, id: u32) {
        if let Some(m) = &mut self.big {
            m.insert(o, id);
            return;
        }
        self.ids.push((o, id));
        if self.ids.len() > 8 {
            self.big = Some(self.ids.drain(..).collect());
        }
    }

    fn units(&mut self, t8: u8, t16: u8, u: &[u16]) {
        if u.iter().all(|&c| c < 256) {
            self.out.push(t8);
            put_varint(&mut self.out, u.len() as u64);
            self.out.extend(u.iter().map(|&c| c as u8));
        } else {
            self.out.push(t16);
            put_varint(&mut self.out, u.len() as u64);
            for &c in u {
                self.out.extend_from_slice(&c.to_le_bytes());
            }
        }
    }

    fn key(&mut self, k: PropertyKey, s: StrId) -> Option<()> {
        let u = self.it.heap.str_units(s).ok()?;
        self.steps += 1;
        self.add(16 + u.len() * 2)?;
        match self.keys.iter().position(|&x| x == k) {
            Some(i) => {
                self.out.push(T_KREF);
                put_varint(&mut self.out, i as u64);
            }
            None => {
                self.keys.push(k);
                self.units(T_KNEW8, T_KNEW16, u);
            }
        }
        Some(())
    }

    fn val(&mut self, v: Value, depth: usize) -> Option<()> {
        let it = self.it;
        self.steps += 1;
        match v {
            Value::Undefined => self.out.push(T_UNDEF),
            Value::Null => self.out.push(T_NULL),
            Value::Bool(b) => self.out.push(if b { T_TRUE } else { T_FALSE }),
            Value::Number(n) => {
                if n as i32 as f64 == n && !(n == 0.0 && n.is_sign_negative()) {
                    self.out.push(T_INT);
                    let i = n as i32;
                    put_varint(&mut self.out, ((i << 1) ^ (i >> 31)) as u32 as u64);
                } else {
                    self.out.push(T_NUM);
                    self.out.extend_from_slice(&n.to_le_bytes());
                }
            }
            Value::String(s) => {
                let u = it.heap.str_units(s).ok()?;
                self.add(u.len().saturating_mul(2) + 16)?;
                self.steps += (u.len() / 32) as u64;
                self.units(T_STR8, T_STR16, u);
            }
            Value::Object(o) => {
                if let Some(id) = self.id_of(o) {
                    self.out.push(T_REF);
                    put_varint(&mut self.out, id as u64);
                    return Some(());
                }
                let p = it.heap.clone_peek(o)?;
                if it.is_callable_value(v) || (self.host)(o) || !matches!(it.clone_class(o), CloneClass::Plain) {
                    return None;
                }
                if depth >= self.max.1 || self.nodes >= self.max.2 {
                    return None;
                }
                self.add(48)?;
                let id = self.nodes as u32;
                self.nodes += 1;
                self.id_put(o, id);
                if p.array {
                    self.out.push(T_ARRD);
                    put_varint(&mut self.out, p.elems.len() as u64);
                    // The `length` key, then each index in order, as `own_keys` lists them.
                    self.steps += 1;
                    for e in p.elems {
                        self.steps += 1;
                        self.add(20)?;
                        self.val((*e)?, depth + 1)?;
                    }
                } else {
                    self.out.push(T_OBJ);
                }
                for (k, val) in p.props() {
                    let PropertyKey::String(s) = k else { return None };
                    self.key(k, s)?;
                    self.val(val, depth + 1)?;
                }
                self.out.push(T_END);
            }
            Value::Symbol(_) | Value::BigInt(_) | Value::BigHeap(_) => return None,
        }
        Some(())
    }
}

struct CloneR<'r> {
    b: &'r [u8],
    p: usize,
}

impl CloneR<'_> {
    fn u8(&mut self) -> u8 {
        self.p += 1;
        self.b[self.p - 1]
    }
    fn varint(&mut self) -> u64 {
        let (mut n, mut s) = (0u64, 0);
        loop {
            let c = self.u8();
            n |= ((c & 0x7f) as u64) << s;
            if c < 0x80 {
                return n;
            }
            s += 7;
        }
    }
}

struct CloneS {
    objs: Vec<ObjId>,
    keys: Vec<PropertyKey>,
    kstack: Vec<PropertyKey>,
    vstack: Vec<Value>,
    steps: u64,
    op: ObjId,
    ap: ObjId,
}

impl Interp<'_> {
    /// The record of `v`, or `None` (nothing changed, nothing run) when the graph holds anything but the data classes or is over a limit:
    /// the caller then runs the old walk, which gives the error.
    pub fn clone_out_fast(&mut self, v: Value, bytes: usize, depth: usize, nodes: usize, host: &dyn Fn(ObjId) -> bool) -> Result<Option<CloneRec>, Error> {
        let r = {
            let mut w = CloneW { it: self, host, out: Vec::with_capacity(64), ids: Vec::new(), big: None, keys: Vec::new(), bytes: 0, steps: 0, nodes: 0, max: (bytes, depth, nodes) };
            w.val(v, 0).map(|_| (w.out, w.bytes, w.steps))
        };
        let Some((out, cost, steps)) = r else { return Ok(None) };
        self.charge(steps)?;
        Ok(Some(CloneRec { bytes: out.into_boxed_slice(), cost }))
    }

    /// A new graph from a record of `clone_out_fast`. No JS runs.
    pub fn clone_in(&mut self, rec: &[u8]) -> Result<Value, Error> {
        let mut r = CloneR { b: rec, p: 0 };
        let (op, ap) = (self.object_prototype(), self.array_prototype());
        let mut st = CloneS { objs: Vec::new(), keys: Vec::new(), kstack: Vec::new(), vstack: Vec::new(), steps: 0, op, ap };
        let v = self.clone_rv(&mut r, &mut st)?;
        self.charge(st.steps)?;
        Ok(v)
    }

    fn clone_units(&mut self, r: &mut CloneR<'_>, wide: bool) -> Result<StrId, Error> {
        let n = r.varint() as usize;
        let mut small = [0u16; 64];
        let mut big = Vec::new();
        let buf: &mut [u16] = if n <= 64 {
            &mut small[..n]
        } else {
            big.resize(n, 0);
            &mut big
        };
        for c in buf.iter_mut() {
            *c = if wide {
                r.p += 2;
                u16::from_le_bytes([r.b[r.p - 2], r.b[r.p - 1]])
            } else {
                r.u8() as u16
            };
        }
        Ok(h!(self, self.heap.intern_units(buf)))
    }

    fn clone_key(&mut self, r: &mut CloneR<'_>, st: &mut CloneS) -> Result<PropertyKey, Error> {
        match r.u8() {
            T_KREF => Ok(st.keys[r.varint() as usize]),
            t => {
                let s = self.clone_units(r, t == T_KNEW16)?;
                let k = h!(self, self.heap.key_from_string(s));
                st.keys.push(k);
                Ok(k)
            }
        }
    }

    fn clone_rv(&mut self, r: &mut CloneR<'_>, st: &mut CloneS) -> Result<Value, Error> {
        Ok(match r.u8() {
            T_UNDEF => Value::Undefined,
            T_NULL => Value::Null,
            T_TRUE => Value::Bool(true),
            T_FALSE => Value::Bool(false),
            T_INT => {
                let z = r.varint() as u32;
                Value::Number(((z >> 1) as i32 ^ -((z & 1) as i32)) as f64)
            }
            T_NUM => {
                r.p += 8;
                Value::Number(f64::from_le_bytes(r.b[r.p - 8..r.p].try_into().expect("8 bytes")))
            }
            t @ (T_STR8 | T_STR16) => Value::String(self.clone_units(r, t == T_STR16)?),
            T_REF => Value::Object(st.objs[r.varint() as usize]),
            T_OBJ => {
                let o = h!(self, self.heap.new_object(Some(st.op)));
                st.objs.push(o);
                st.steps += 1;
                let (kb, vb) = (st.kstack.len(), st.vstack.len());
                self.clone_props(r, st)?;
                if !h!(self, self.heap.clone_install(o, &st.kstack[kb..], &st.vstack[vb..])) {
                    for i in 0..st.kstack.len() - kb {
                        self.define_data(o, st.kstack[kb + i], st.vstack[vb + i], true, true, true)?;
                    }
                }
                st.kstack.truncate(kb);
                st.vstack.truncate(vb);
                Value::Object(o)
            }
            T_ARRD => {
                let len = r.varint() as usize;
                let o = h!(self, self.heap.new_array(Some(st.ap)));
                st.objs.push(o);
                st.steps += 1 + len as u64;
                let vb = st.vstack.len();
                for _ in 0..len {
                    let v = self.clone_rv(r, st)?;
                    st.vstack.push(v);
                }
                let elems: Vec<Option<Value>> = st.vstack.drain(vb..).map(Some).collect();
                h!(self, self.heap.fill_array(o, elems, len as u32));
                let kb = st.kstack.len();
                self.clone_props(r, st)?;
                for i in 0..st.kstack.len() - kb {
                    self.define_data(o, st.kstack[kb + i], st.vstack[vb + i], true, true, true)?;
                }
                st.kstack.truncate(kb);
                st.vstack.truncate(vb);
                Value::Object(o)
            }
            t => unreachable!("record tag {t}"),
        })
    }

    /// Keys and values up to `T_END`, pushed on the two stacks.
    fn clone_props(&mut self, r: &mut CloneR<'_>, st: &mut CloneS) -> Result<(), Error> {
        while r.b[r.p] != T_END {
            let k = self.clone_key(r, st)?;
            let v = self.clone_rv(r, st)?;
            st.kstack.push(k);
            st.vstack.push(v);
            st.steps += 1;
        }
        r.p += 1;
        Ok(())
    }
}
