//! ArrayBuffer and DataView. Each live buffer owns one byte cell. Views trace the buffer
//! through a private slot and resolve its byte cell again after any script call.
use crate::bi_symbol::{SPECIES, TO_STRING_TAG};
use crate::bi_typed::{self, Element};
use crate::blob::Blob;
use crate::builtins::{HIDDEN, NativeFn, arg};
use crate::cell::Layout;
use crate::interp_rt::{ErrKind, Key, R, Realm};
use crate::lazy::{Ent, buffer_ctor, getter, indexed, late_table, m, sym_getter};
use crate::object::{K_BUFFER, K_DATA_VIEW, K_TYPED, Obj};
use crate::shapes::A_C;
use crate::string::Str;
use crate::value::Value;

pub(crate) const ARRAY_BUFFER: i32 = -1;
pub(crate) const DATA_VIEW: i32 = -2;
const MAX_BYTES: usize = i32::MAX as usize;

pub(crate) static GLOBALS: &[Ent] = &[
    buffer_ctor("ArrayBuffer", 1, ARRAY_BUFFER),
    buffer_ctor("DataView", 1, DATA_VIEW),
    buffer_ctor("Int8Array", 3, Element::I8 as i32),
    buffer_ctor("Uint8Array", 3, Element::U8 as i32),
    buffer_ctor("Uint8ClampedArray", 3, Element::U8C as i32),
    buffer_ctor("Int16Array", 3, Element::I16 as i32),
    buffer_ctor("Uint16Array", 3, Element::U16 as i32),
    buffer_ctor("Int32Array", 3, Element::I32 as i32),
    buffer_ctor("Uint32Array", 3, Element::U32 as i32),
    buffer_ctor("Float32Array", 3, Element::F32 as i32),
    buffer_ctor("Float64Array", 3, Element::F64 as i32),
];

/// Bounds of a view are fixed, or extend to the current end of its buffer.
#[derive(Clone, Copy)]
pub(crate) struct View {
    pub(crate) buffer: Obj,
    pub(crate) offset: usize,
    pub(crate) len: Option<usize>,
    pub(crate) elem: Option<Element>,
}

impl View {
    pub(crate) fn of(o: Obj) -> Option<Self> {
        let elem = match o.kind() {
            K_TYPED => Some(Element::from_id(buffer_slot(o, 3).as_int().expect("element type"))),
            K_DATA_VIEW => None,
            _ => return None,
        };
        Some(Self {
            buffer: Realm::obj(buffer_slot(o, 0)).expect("view buffer"),
            offset: buffer_slot(o, 1).as_int().expect("view offset") as usize,
            len: buffer_slot(o, 2).as_int().map(|n| n as usize),
            elem,
        })
    }

    pub(crate) fn width(self) -> usize {
        self.elem.map_or(1, Element::width)
    }

    pub(crate) fn len(self) -> Option<usize> {
        self.len_in(Buffer::of(self.buffer))
    }

    fn len_in(self, buffer: Buffer) -> Option<usize> {
        buffer.blob?;
        let n = buffer.len.checked_sub(self.offset)? / self.width();
        self.len.map_or(Some(n), |l| (l <= n).then_some(l))
    }

    pub(crate) fn is_fixed(self) -> bool {
        self.len.is_some() && Buffer::of(self.buffer).max.is_none()
    }

    pub(crate) fn read(self, rt: &Realm, i: usize) -> Option<f64> {
        let elem = self.elem?;
        let b = Buffer::of(self.buffer);
        if i >= self.len_in(b)? {
            return None;
        }
        let at = self.offset + i * elem.width();
        Some(elem.read(&b.bytes(rt)[at..at + elem.width()], cfg!(target_endian = "little")))
    }

    pub(crate) fn write(self, rt: &mut Realm, i: usize, n: f64) {
        let elem = self.elem.expect("typed array");
        let b = Buffer::of(self.buffer);
        if self.len_in(b).is_some_and(|len| i < len) {
            let at = self.offset + i * elem.width();
            elem.write(&mut b.bytes_mut(rt)[at..at + elem.width()], n, cfg!(target_endian = "little"));
        }
    }
}

#[derive(Clone, Copy)]
struct Buffer {
    blob: Option<Blob>,
    len: usize,
    max: Option<usize>,
}

impl Buffer {
    fn of(o: Obj) -> Self {
        debug_assert!(o.is_kind(K_BUFFER));
        Self {
            blob: Realm::blob_of(buffer_slot(o, 0)),
            len: buffer_slot(o, 1).as_int().expect("byte length") as usize,
            max: buffer_slot(o, 2).as_int().map(|n| n as usize),
        }
    }

    fn bytes(self, rt: &Realm) -> &[u8] {
        // SAFETY: this record came from a private ArrayBuffer slot. Its cell
        // is still live. The borrow of the realm excludes script, GC and writes.
        match self.blob {
            Some(blob) => unsafe { blob.bytes(&rt.heap, self.len) },
            None => &[],
        }
    }

    fn bytes_mut(self, rt: &mut Realm) -> &mut [u8] {
        // SAFETY: as in bytes, with exclusive access for the borrow.
        match self.blob {
            Some(blob) => unsafe { blob.bytes_mut(&mut rt.heap, self.len) },
            None => &mut [],
        }
    }
}

impl Realm {
    /// Allocate internal slots before any public properties. The keys are private
    /// strings outside the atom table. Script keys are atomized before lookup.
    pub(crate) fn buffer_object(&mut self, proto: Obj, kind: u32, slots: &[Value]) -> R<Obj> {
        let keys = match self.ltab.buffer_keys {
            Some(keys) => keys,
            None => {
                let mut keys = [self.k_length; 4];
                for k in &mut keys {
                    *k = Str::from_str(&mut self.heap, "\0buffer").expect("internal name");
                    self.keep(k.value());
                }
                self.ltab.buffer_keys = Some(keys);
                keys
            }
        };
        let mut shape = self.shapes.root(Layout::Object, Some(proto.cell()), 5);
        for &key in &keys[..slots.len()] {
            let Some(next) = self.shapes.add(shape, key, 0) else {
                return range(self, "out of shape space");
            };
            shape = next;
        }
        let Some(o) = Obj::new(&mut self.heap, &self.shapes, shape) else {
            return range(self, "out of memory");
        };
        for (i, &v) in slots.iter().enumerate() {
            o.set_slot(&self.shapes, i as u32, v);
        }
        o.set_kind(kind);
        Ok(o)
    }

    pub(crate) fn make_buffer_ctor(&mut self, name: &'static str, len: u8, id: i32) -> Value {
        let (f, parent): (NativeFn, Obj) = match id {
            ARRAY_BUFFER => (buffer_ctor_call, self.object_proto),
            DATA_VIEW => (data_ctor, self.object_proto),
            _ => (bi_typed::typed_ctor, bi_typed::intrinsic(self).1),
        };
        let proto = plain_proto(self, parent);
        let c = make_ctor(self, name, len, f, proto);
        c.set_slot(&self.shapes, 1, Value::int(id));
        match id {
            ARRAY_BUFFER => {
                tag(self, proto, name);
                self.lazy_table(
                    proto,
                    late_table![
                        getter("byteLength", "get byteLength", byte_length),
                        getter("maxByteLength", "get maxByteLength", max_byte_length),
                        getter("resizable", "get resizable", resizable),
                        getter("detached", "get detached", detached),
                        m("slice", 2, buffer_slice),
                        m("resize", 1, resize),
                        indexed("transfer", 0, transfer, 0),
                        indexed("transferToFixedLength", 0, transfer, 1),
                    ],
                );
                self.lazy_table(
                    c,
                    late_table![m("isView", 1, is_view), sym_getter(SPECIES, "get [Symbol.species]", species),],
                );
            }
            DATA_VIEW => {
                tag(self, proto, name);
                self.lazy_table(
                    proto,
                    late_table![
                        getter("buffer", "get buffer", data_buffer),
                        getter("byteLength", "get byteLength", data_length),
                        getter("byteOffset", "get byteOffset", data_offset),
                        indexed("getInt8", 1, data_get, Element::I8 as i32),
                        indexed("getUint8", 1, data_get, Element::U8 as i32),
                        indexed("getInt16", 1, data_get, Element::I16 as i32),
                        indexed("getUint16", 1, data_get, Element::U16 as i32),
                        indexed("getInt32", 1, data_get, Element::I32 as i32),
                        indexed("getUint32", 1, data_get, Element::U32 as i32),
                        indexed("getFloat32", 1, data_get, Element::F32 as i32),
                        indexed("getFloat64", 1, data_get, Element::F64 as i32),
                        indexed("setInt8", 2, data_set, Element::I8 as i32),
                        indexed("setUint8", 2, data_set, Element::U8 as i32),
                        indexed("setInt16", 2, data_set, Element::I16 as i32),
                        indexed("setUint16", 2, data_set, Element::U16 as i32),
                        indexed("setInt32", 2, data_set, Element::I32 as i32),
                        indexed("setUint32", 2, data_set, Element::U32 as i32),
                        indexed("setFloat32", 2, data_set, Element::F32 as i32),
                        indexed("setFloat64", 2, data_set, Element::F64 as i32),
                    ],
                );
            }
            _ => {
                let elem = Element::from_id(id);
                let base = self.ltab.typed_base.expect("TypedArray constructor");
                self.set_proto_checked(c, Some(base)).expect("prototype");
                let bytes = Key::Name(self.intern("BYTES_PER_ELEMENT"));
                let size = Value::int(elem.width() as i32);
                self.define_attrs(c, bytes, 0, size).expect("define");
                self.define_attrs(proto, bytes, 0, size).expect("define");
            }
        }
        c.value()
    }

    pub(crate) fn buffer_intrinsic(&mut self, name: &'static str) -> Obj {
        let key = self.intern(name);
        self.lazy_prep(self.global, key);
        let c = self.shared_fn(self.global, name).and_then(Realm::obj).expect("buffer constructor");
        ctor_proto(self, c)
    }

    pub(crate) fn new_buffer(&mut self, proto: Obj, len: usize, max: Option<usize>) -> R<Obj> {
        let blob = self.alloc_bytes(len)?;
        self.buffer_object(proto, K_BUFFER, &[blob.value(), Value::int(len as i32), stored_len(max)])
    }

    fn alloc_bytes(&mut self, len: usize) -> R<Blob> {
        if len > MAX_BYTES {
            return range(self, "invalid ArrayBuffer length");
        }
        Blob::zeroed_bytes(&mut self.heap, len).ok_or_else(|| self.error(ErrKind::Range, "out of memory"))
    }

    pub(crate) fn new_view(&mut self, proto: Obj, v: View) -> R<Obj> {
        let kind = if v.elem.is_some() { K_TYPED } else { K_DATA_VIEW };
        let slots = [
            v.buffer.value(),
            Value::int(v.offset as i32),
            stored_len(v.len),
            v.elem.map_or(Value::UNDEFINED, |e| Value::int(e as i32)),
        ];
        self.buffer_object(proto, kind, &slots)
    }

    pub(crate) fn buffer_bytes(&self, o: Obj) -> Option<&[u8]> {
        o.is_kind(K_BUFFER).then(|| Buffer::of(o).bytes(self))
    }

    pub(crate) fn buffer_bytes_mut(&mut self, o: Obj) -> Option<&mut [u8]> {
        o.is_kind(K_BUFFER).then(|| Buffer::of(o).bytes_mut(self))
    }

    pub(crate) fn byte_range(&self, o: Obj) -> Option<(Obj, std::ops::Range<usize>)> {
        if o.is_kind(K_BUFFER) {
            return Some((o, 0..Buffer::of(o).len));
        }
        let v = View::of(o)?;
        Some((v.buffer, v.len().map_or(0..0, |n| v.offset..v.offset + n * v.width())))
    }

    pub(crate) fn buffer_detached(&self, o: Obj) -> bool {
        let buffer = View::of(o).map_or(o, |v| v.buffer);
        buffer.is_kind(K_BUFFER) && Buffer::of(buffer).blob.is_none()
    }

    pub(crate) fn copy_buffer_bytes(&mut self, from: Obj, src: usize, to: Obj, dst: usize, len: usize) {
        let from = Buffer::of(from);
        let to = Buffer::of(to);
        assert!(src <= from.len && len <= from.len - src && dst <= to.len && len <= to.len - dst);
        if len != 0 {
            // SAFETY: both blobs are byte cells and the checked ranges fit their allocated lengths.
            // No borrow of bytes survives a mutable borrow of the realm.
            unsafe {
                from.blob
                    .expect("source")
                    .copy_bytes(&mut self.heap, src, to.blob.expect("target"), dst, len)
            }
        }
    }

    pub(crate) fn typed_view_from_args(&mut self, buffer: Obj, elem: Element, args: &[Value]) -> R<View> {
        let width = elem.width();
        let offset = to_index(self, arg(args, 1))?;
        if offset % width != 0 {
            return range(self, "unaligned byte offset");
        }
        let len_arg = arg(args, 2);
        let len = if len_arg.is_undefined() {
            None
        } else {
            Some(to_index(self, len_arg)?)
        };
        let b = Buffer::of(buffer);
        if b.blob.is_none() {
            return self.type_error("detached ArrayBuffer");
        }
        if offset > b.len {
            return range(self, "view offset exceeds buffer");
        }
        let remaining = b.len - offset;
        let len = match len {
            Some(n) if n > remaining / width => return range(self, "view exceeds buffer"),
            None if b.max.is_none() => {
                if remaining % width != 0 {
                    return range(self, "buffer length is not aligned");
                }
                Some(remaining / width)
            }
            len => len,
        };
        Ok(View {
            buffer,
            offset,
            len,
            elem: Some(elem),
        })
    }
}

/// Buffer objects reserve five inline words. The first four remain inline
/// when the last word becomes an overflow pointer.
#[inline]
pub(crate) fn buffer_slot(o: Obj, i: u32) -> Value {
    debug_assert!(matches!(o.kind(), K_BUFFER | K_DATA_VIEW | K_TYPED | crate::object::K_ARRAY_ITER) && i < 4);
    // SAFETY: buffer_object creates this kind with this prefix and shape
    // changes preserve it. The private keys precede every script property.
    unsafe { o.read_inline(crate::cell::CellHeader::SIZE as u32 + 8 * i) }
}

fn stored_len(n: Option<usize>) -> Value {
    n.map_or(Value::UNDEFINED, |n| Value::int(n as i32))
}

pub(crate) fn range<T>(rt: &mut Realm, msg: &str) -> R<T> {
    Err(rt.error(ErrKind::Range, msg))
}

pub(crate) fn integer(rt: &mut Realm, v: Value) -> R<f64> {
    let n = rt.to_number(v)?;
    Ok(if n.is_nan() { 0.0 } else { n.trunc() })
}

pub(crate) fn to_index(rt: &mut Realm, v: Value) -> R<usize> {
    let n = integer(rt, v)?;
    if !(0.0..=9_007_199_254_740_991.0).contains(&n) || n > usize::MAX as f64 {
        return range(rt, "invalid index");
    }
    Ok(n as usize)
}

pub(crate) fn relative(rt: &mut Realm, arg: Value, len: usize, default: usize) -> R<usize> {
    if arg.is_undefined() {
        return Ok(default);
    }
    let n = integer(rt, arg)?;
    Ok(if n < 0.0 {
        (len as f64 + n).max(0.0) as usize
    } else {
        n.min(len as f64) as usize
    })
}

pub(crate) fn tag(rt: &mut Realm, proto: Obj, name: &str) {
    let value = rt.string(name);
    rt.define_attrs(proto, Key::Name(rt.wk[TO_STRING_TAG]), A_C, value)
        .expect("define");
}

pub(crate) fn plain_proto(rt: &mut Realm, parent: Obj) -> Obj {
    let shape = rt.shapes.root(Layout::Object, Some(parent.cell()), 4);
    Obj::new(&mut rt.heap, &rt.shapes, shape).expect("memory")
}

pub(crate) fn make_ctor(rt: &mut Realm, name: &'static str, len: u8, f: NativeFn, proto: Obj) -> Obj {
    let c = rt.new_native(f, true, len, name);
    c.set_slot(&rt.shapes, 2, proto.value());
    rt.define_attrs(c, Key::Name(rt.k_prototype), 0, proto.value())
        .expect("define");
    rt.define_attrs(proto, Key::Name(rt.k_constructor), HIDDEN, c.value())
        .expect("define");
    c
}

pub(crate) fn ctor_proto(rt: &Realm, c: Obj) -> Obj {
    Realm::obj(c.slot(&rt.shapes, 2)).expect("intrinsic prototype")
}

pub(crate) fn native_param(rt: &Realm) -> i32 {
    Realm::obj(rt.callee)
        .expect("native")
        .slot(&rt.shapes, 1)
        .as_int()
        .expect("native parameter")
}

pub(crate) fn instance_proto(rt: &mut Realm) -> R<Obj> {
    let nt = rt.nt;
    if nt.is_undefined() {
        return rt.type_error("constructor requires new");
    }
    let default = ctor_proto(rt, Realm::obj(rt.callee).expect("native"));
    if rt.callee == nt {
        return Ok(default);
    }
    let p = rt.get_prop(nt, Key::Name(rt.k_prototype))?;
    Ok(Realm::obj(p).unwrap_or(default))
}

pub(crate) fn species(_rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    Ok(this)
}

/// The species constructor, or undefined when the intrinsic is the default.
pub(crate) fn species_ctor(rt: &mut Realm, this: Value) -> R<Value> {
    let c = rt.get_prop(this, Key::Name(rt.k_constructor))?;
    if c.is_undefined() {
        return Ok(c);
    }
    if Realm::obj(c).is_none() {
        return rt.type_error("constructor is not an object");
    }
    let c = rt.get_prop(c, Key::Name(rt.wk[SPECIES]))?;
    if c.is_null() || c.is_undefined() {
        return Ok(Value::UNDEFINED);
    }
    if !rt.is_constructor(c) {
        return rt.type_error("species is not a constructor");
    }
    Ok(c)
}

pub(crate) fn construct(rt: &mut Realm, c: Value, args: &[Value]) -> R<Value> {
    if rt.native_id(c).is_none() {
        return rt.construct_script(c, args, c);
    }
    let saved = (rt.callee, rt.nt);
    let r = rt.construct_native(c, args, c);
    (rt.callee, rt.nt) = saved;
    r
}

fn require_buffer(rt: &mut Realm, this: Value) -> R<Obj> {
    Realm::obj(this)
        .filter(|o| o.is_kind(K_BUFFER))
        .ok_or_else(|| rt.error(ErrKind::Type, "receiver is not an ArrayBuffer"))
}

fn buffer_ctor_call(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    if rt.nt.is_undefined() {
        return rt.type_error("ArrayBuffer requires new");
    }
    let len = to_index(rt, arg(args, 0))?;
    let opts = arg(args, 1);
    let max = if Realm::obj(opts).is_some() {
        let v = rt.get_named(opts, "maxByteLength")?;
        if v.is_undefined() { None } else { Some(to_index(rt, v)?) }
    } else {
        None
    };
    if max.is_some_and(|max| max < len || max > MAX_BYTES) {
        return range(rt, "invalid maxByteLength");
    }
    let proto = instance_proto(rt)?;
    rt.new_buffer(proto, len, max).map(Obj::value)
}

fn is_view(_rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    Ok(Value::bool(
        Realm::obj(arg(args, 0)).is_some_and(|o| matches!(o.kind(), K_TYPED | K_DATA_VIEW)),
    ))
}

fn byte_length(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let o = require_buffer(rt, this)?;
    Ok(Realm::num(Buffer::of(o).len as f64))
}

fn max_byte_length(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let o = require_buffer(rt, this)?;
    let b = Buffer::of(o);
    Ok(Realm::num(if b.blob.is_none() { 0 } else { b.max.unwrap_or(b.len) } as f64))
}

fn resizable(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let o = require_buffer(rt, this)?;
    Ok(Value::bool(Buffer::of(o).max.is_some()))
}

fn detached(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let o = require_buffer(rt, this)?;
    Ok(Value::bool(rt.buffer_detached(o)))
}

fn buffer_slice(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let source = require_buffer(rt, this)?;
    let b = Buffer::of(source);
    if b.blob.is_none() {
        return rt.type_error("detached ArrayBuffer");
    }
    let start = relative(rt, arg(args, 0), b.len, 0)?;
    let end = relative(rt, arg(args, 1), b.len, b.len)?;
    let len = end.saturating_sub(start);
    let c = species_ctor(rt, this)?;
    let result = if c.is_undefined() {
        let proto = rt.buffer_intrinsic("ArrayBuffer");
        rt.new_buffer(proto, len, None)?
    } else {
        let v = construct(rt, c, &[Realm::num(len as f64)])?;
        let o = require_buffer(rt, v)?;
        let b = Buffer::of(o);
        if o == source || b.blob.is_none() || b.len < len {
            return rt.type_error("invalid ArrayBuffer species result");
        }
        o
    };
    let b = Buffer::of(source);
    if b.blob.is_none() {
        return rt.type_error("detached ArrayBuffer");
    }
    let n = len.min(b.len.saturating_sub(start));
    if n != 0 {
        rt.copy_buffer_bytes(source, start, result, 0, n);
    }
    Ok(result.value())
}

fn resize(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = require_buffer(rt, this)?;
    let Some(max) = Buffer::of(o).max else {
        return rt.type_error("ArrayBuffer is fixed-length");
    };
    let len = to_index(rt, arg(args, 0))?;
    let b = Buffer::of(o);
    let Some(old) = b.blob else {
        return rt.type_error("detached ArrayBuffer");
    };
    if len > max {
        return range(rt, "resize exceeds maxByteLength");
    }
    if b.len != len {
        let new = rt.alloc_bytes(len)?;
        // SAFETY: the two byte cells are live and the copied range fits both.
        unsafe { old.copy_bytes(&mut rt.heap, 0, new, 0, len.min(b.len)) };
        o.set_slot(&rt.shapes, 0, new.value());
        o.set_slot(&rt.shapes, 1, Value::int(len as i32));
    }
    Ok(Value::UNDEFINED)
}

fn transfer(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let fixed = native_param(rt) == 1;
    let o = require_buffer(rt, this)?;
    let a = arg(args, 0);
    let len = if a.is_undefined() {
        Buffer::of(o).len
    } else {
        to_index(rt, a)?
    };
    let b = Buffer::of(o);
    let Some(blob) = b.blob else {
        return rt.type_error("detached ArrayBuffer");
    };
    let max = if fixed { None } else { b.max };
    if max.is_some_and(|max| len > max) {
        return range(rt, "transfer exceeds maxByteLength");
    }
    let proto = rt.buffer_intrinsic("ArrayBuffer");
    let result = if len == b.len {
        rt.buffer_object(proto, K_BUFFER, &[blob.value(), Value::int(len as i32), stored_len(max)])?
    } else {
        let result = rt.new_buffer(proto, len, max)?;
        rt.copy_buffer_bytes(o, 0, result, 0, len.min(b.len));
        result
    };
    o.set_slot(&rt.shapes, 0, Value::UNDEFINED);
    o.set_slot(&rt.shapes, 1, Value::int(0));
    Ok(result.value())
}

fn data_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    if rt.nt.is_undefined() {
        return rt.type_error("DataView requires new");
    }
    let buffer = require_buffer(rt, arg(args, 0))?;
    let offset = to_index(rt, arg(args, 1))?;
    let b = Buffer::of(buffer);
    if b.blob.is_none() {
        return rt.type_error("detached ArrayBuffer");
    }
    if offset > b.len {
        return range(rt, "view offset exceeds buffer");
    }
    let len_arg = arg(args, 2);
    let explicit = !len_arg.is_undefined();
    let len = if explicit {
        Some(to_index(rt, len_arg)?)
    } else {
        b.max.is_none().then_some(b.len - offset)
    };
    if len.is_some_and(|n| n > b.len - offset) {
        return range(rt, "view exceeds buffer");
    }
    let proto = instance_proto(rt)?;
    let b = Buffer::of(buffer);
    if b.blob.is_none() {
        return rt.type_error("detached ArrayBuffer");
    }
    if offset > b.len || (explicit && len.is_some_and(|n| n > b.len - offset)) {
        return range(rt, "view exceeds buffer");
    }
    let v = View {
        buffer,
        offset,
        len,
        elem: None,
    };
    rt.new_view(proto, v).map(Obj::value)
}

fn require_data(rt: &mut Realm, this: Value) -> R<View> {
    Realm::obj(this)
        .filter(|o| o.is_kind(K_DATA_VIEW))
        .and_then(|o| View::of(o))
        .ok_or_else(|| rt.error(ErrKind::Type, "receiver is not a DataView"))
}

fn data_buffer(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    Ok(require_data(rt, this)?.buffer.value())
}

fn data_length(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let v = require_data(rt, this)?;
    Ok(Realm::num(v.len().unwrap_or(0) as f64))
}

fn data_offset(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let v = require_data(rt, this)?;
    Ok(Realm::num(v.len().map_or(0, |_| v.offset) as f64))
}

fn data_range(rt: &mut Realm, v: View, offset: usize, width: usize) -> R<std::ops::Range<usize>> {
    let Some(len) = v.len() else {
        return rt.type_error("DataView is out of bounds");
    };
    if offset > len || width > len - offset {
        return range(rt, "DataView offset is out of bounds");
    }
    let start = v.offset + offset;
    Ok(start..start + width)
}

fn data_get(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let v = require_data(rt, this)?;
    let elem = Element::from_id(native_param(rt));
    let at = to_index(rt, arg(args, 0))?;
    let at = data_range(rt, v, at, elem.width())?;
    let little = Realm::truthy(arg(args, 1));
    Ok(Realm::num(elem.read(&rt.buffer_bytes(v.buffer).expect("buffer")[at], little)))
}

fn data_set(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let v = require_data(rt, this)?;
    let elem = Element::from_id(native_param(rt));
    let at = to_index(rt, arg(args, 0))?;
    let n = rt.to_number(arg(args, 1))?;
    let at = data_range(rt, v, at, elem.width())?;
    let little = Realm::truthy(arg(args, 2));
    elem.write(&mut rt.buffer_bytes_mut(v.buffer).expect("buffer")[at], n, little);
    Ok(Value::UNDEFINED)
}

#[cfg(test)]
#[path = "bi_buffer_tests.rs"]
mod tests;
