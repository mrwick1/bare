//! The numeric typed arrays. Indexed properties and methods use the same view
//! bounds and conversions as the host byte interface.
use crate::bi_buffer::{
    View, buffer_slot, construct, instance_proto, integer, make_ctor, native_param, plain_proto, range, relative, species,
    species_ctor, to_index,
};
use crate::bi_symbol::{ITERATOR, SPECIES, TO_STRING_TAG};
use crate::builtins::arg;
use crate::interp_rt::{ErrKind, Key, R, Realm};
use crate::lazy::{getter, indexed, late_table, m, sym_getter, sym_same};
use crate::object::{K_ARRAY_ITER, K_BUFFER, K_TYPED, Obj};
use crate::string::Chars;
use crate::value::Value;

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub(crate) enum Element {
    I8,
    U8,
    U8C,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
    BI64,
    BU64,
}

impl Element {
    pub(crate) fn from_id(id: i32) -> Self {
        use Element::*;
        [I8, U8, U8C, I16, U16, I32, U32, F32, F64, BI64, BU64][id as usize]
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::I8 => "Int8Array",
            Self::U8 => "Uint8Array",
            Self::U8C => "Uint8ClampedArray",
            Self::I16 => "Int16Array",
            Self::U16 => "Uint16Array",
            Self::I32 => "Int32Array",
            Self::U32 => "Uint32Array",
            Self::F32 => "Float32Array",
            Self::F64 => "Float64Array",
            Self::BI64 => "BigInt64Array",
            Self::BU64 => "BigUint64Array",
        }
    }

    pub(crate) fn width(self) -> usize {
        match self {
            Self::I8 | Self::U8 | Self::U8C => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 | Self::BI64 | Self::BU64 => 8,
        }
    }

    pub(crate) fn is_big(self) -> bool {
        matches!(self, Self::BI64 | Self::BU64)
    }

    fn big_parts(self, bits: u64) -> (bool, u64) {
        if self == Self::BI64 && (bits as i64) < 0 {
            (true, bits.wrapping_neg())
        } else {
            (false, bits)
        }
    }

    pub(crate) fn value(self, rt: &mut Realm, bits: u64) -> R<Value> {
        if !self.is_big() {
            return Ok(Realm::num(f64::from_bits(bits)));
        }
        let (neg, mag) = self.big_parts(bits);
        let limb = [mag];
        rt.make_bigint(crate::bigint::Sv { neg, mag: &limb[..usize::from(mag != 0)] })
    }

    pub(crate) fn convert(self, rt: &mut Realm, v: Value) -> R<u64> {
        if !self.is_big() {
            return rt.to_number(v).map(f64::to_bits);
        }
        let v = rt.to_bigint(v)?;
        let b = Realm::big_of(v).expect("BigInt conversion");
        let a = b.bigint();
        let low = a.mag.first().copied().unwrap_or(0);
        Ok(if a.neg { low.wrapping_neg() } else { low })
    }

    /// The Number bits or the full integer bits, according to the element type.
    pub(crate) fn read(self, bytes: &[u8], little: bool) -> u64 {
        let mut word = [0; 8];
        let n = bytes.len();
        if little {
            word[..n].copy_from_slice(bytes);
        } else {
            word[..n].iter_mut().zip(bytes.iter().rev()).for_each(|(d, s)| *d = *s);
        }
        let bits = u64::from_le_bytes(word);
        match self {
            Self::I8 => bits as i8 as f64,
            Self::U8 | Self::U8C => bits as f64,
            Self::I16 => bits as i16 as f64,
            Self::U16 | Self::U32 => bits as f64,
            Self::I32 => bits as i32 as f64,
            Self::F32 => f32::from_bits(bits as u32) as f64,
            Self::F64 | Self::BI64 | Self::BU64 => return bits,
        }.to_bits()
    }

    pub(crate) fn write(self, bytes: &mut [u8], bits: u64, little: bool) {
        let n = f64::from_bits(bits);
        let bits = match self {
            Self::U8C => n.clamp(0.0, 255.0).round_ties_even() as u64,
            Self::F32 => (n as f32).to_bits() as u64,
            Self::F64 | Self::BI64 | Self::BU64 => bits,
            _ => Realm::to_int32(n) as u64,
        };
        let word = bits.to_le_bytes();
        let n = bytes.len();
        if little {
            bytes.copy_from_slice(&word[..n]);
        } else {
            bytes.iter_mut().zip(word[..n].iter().rev()).for_each(|(d, s)| *d = *s);
        }
    }

    pub(crate) fn api(self) -> bare_js_api::Elem {
        use bare_js_api::Elem;
        match self {
            Self::I8 => Elem::I8,
            Self::U8 => Elem::U8,
            Self::U8C => Elem::U8C,
            Self::I16 => Elem::I16,
            Self::U16 => Elem::U16,
            Self::I32 => Elem::I32,
            Self::U32 => Elem::U32,
            Self::F32 => Elem::F32,
            Self::F64 => Elem::F64,
            Self::BI64 => Elem::BI64,
            Self::BU64 => Elem::BU64,
        }
    }

    pub(crate) fn from_api(elem: bare_js_api::Elem) -> Option<Self> {
        use bare_js_api::Elem;
        Some(match elem {
            Elem::I8 => Self::I8,
            Elem::U8 => Self::U8,
            Elem::U8C => Self::U8C,
            Elem::I16 => Self::I16,
            Elem::U16 => Self::U16,
            Elem::I32 => Self::I32,
            Elem::U32 => Self::U32,
            Elem::F32 => Self::F32,
            Elem::F64 => Self::F64,
            Elem::BI64 => Self::BI64,
            Elem::BU64 => Self::BU64,
            Elem::Data => return None,
        })
    }
}

pub(crate) fn intrinsic(rt: &mut Realm) -> (Obj, Obj) {
    if let Some(c) = rt.ltab.typed_base {
        return (c, crate::bi_buffer::ctor_proto(rt, c));
    }
    let p = plain_proto(rt, rt.object_proto);
    let c = make_ctor(rt, "TypedArray", 0, abstract_ctor, p);
    // Typed arrays share Array.prototype.toString, which uses their join method.
    let name = rt.k_to_string;
    rt.lazy_prep(rt.array_proto, name);
    let to_string = rt.shared_fn(rt.array_proto, "toString").expect("the array toString entry");
    rt.hidden(p, "toString", to_string);
    rt.lazy_table(
        p,
        late_table![
            getter("buffer", "get buffer", buffer),
            getter("length", "get length", length),
            getter("byteLength", "get byteLength", byte_length),
            getter("byteOffset", "get byteOffset", byte_offset),
            sym_getter(TO_STRING_TAG, "get [Symbol.toStringTag]", class_name),
            m("set", 1, set),
            m("subarray", 2, subarray),
            m("slice", 2, slice),
            m("fill", 1, fill),
            indexed("indexOf", 1, search, 0),
            indexed("includes", 1, search, 1),
            m("join", 1, join),
            m("map", 1, map),
            m("filter", 1, filter),
            m("forEach", 1, for_each),
            m("reduce", 1, reduce),
            m("sort", 1, sort),
            m("at", 1, at),
            indexed("keys", 0, iterator, 0),
            indexed("values", 0, iterator, 1),
            indexed("entries", 0, iterator, 2),
            sym_same("values", ITERATOR, "values"),
        ],
    );
    rt.lazy_table(
        c,
        late_table![
            m("from", 1, from),
            m("of", 0, of),
            sym_getter(SPECIES, "get [Symbol.species]", species),
        ],
    );
    rt.keep(c.value());
    rt.ltab.typed_base = Some(c);
    (c, p)
}

fn abstract_ctor(rt: &mut Realm, _this: Value, _args: &[Value]) -> R<Value> {
    rt.type_error("TypedArray is an abstract constructor")
}

/// A canonical numeric key. The inner None is a numeric key that can never
/// name an element, such as "-0", "1.5" or "Infinity".
#[cold]
pub(crate) fn numeric_key(key: Key) -> Option<Option<usize>> {
    let k = match key {
        Key::Idx(i) => return Some(Some(i as usize)),
        Key::Name(k) => k,
    };
    if k.is_symbol() {
        return None;
    }
    let Some(Chars::One(b)) = k.chars() else { return None };
    if !matches!(b.first(), Some(b'0'..=b'9' | b'-' | b'I' | b'N')) {
        return None;
    }
    if b == b"-0" {
        return Some(None);
    }
    let s = std::str::from_utf8(b).ok()?;
    let n: f64 = s.parse().ok()?;
    if Realm::num_to_string(n) != s {
        return None;
    }
    Some((n >= 0.0 && n <= i32::MAX as f64 && n.fract() == 0.0).then_some(n as usize))
}

impl Realm {
    pub(crate) fn typed_get(&mut self, o: Obj, i: usize) -> R<Option<Value>> {
        let view = View::of(o).expect("typed array");
        view.read(self, i).map(|bits| view.elem.expect("element").value(self, bits)).transpose()
    }

    pub(crate) fn typed_set(&mut self, o: Obj, i: Option<usize>, v: Value) -> R<()> {
        let view = View::of(o).expect("typed array");
        let n = view.elem.expect("element").convert(self, v)?;
        if let Some(i) = i {
            view.write(self, i, n);
        }
        Ok(())
    }

    pub(crate) fn typed_len(&self, o: Obj) -> usize {
        View::of(o).and_then(|v| v.len()).unwrap_or(0)
    }

    pub(crate) fn alloc_typed(&mut self, proto: Obj, elem: Element, len: usize) -> R<Obj> {
        let Some(bytes) = len.checked_mul(elem.width()) else {
            return range(self, "invalid typed array length");
        };
        let bp = self.buffer_intrinsic("ArrayBuffer");
        let b = self.new_buffer(bp, bytes, None)?;
        self.new_view(
            proto,
            View {
                buffer: b,
                offset: 0,
                len: Some(len),
                elem: Some(elem),
            },
        )
    }
}

pub(crate) fn typed_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let elem = Element::from_id(native_param(rt));
    let proto = instance_proto(rt)?;
    let v = arg(args, 0);
    let dest = if let Some(o) = Realm::obj(v) {
        match o.kind() {
            K_BUFFER => {
                let view = rt.typed_view_from_args(o, elem, args)?;
                rt.new_view(proto, view)?
            }
            K_TYPED => {
                let (src, len) = validated(rt, v)?;
                let dest = rt.alloc_typed(proto, elem, len)?;
                let dst = View::of(dest).expect("typed array");
                copy(rt, src, 0, dst, 0, len)?;
                dest
            }
            _ => {
                if let Some(values) = iterable(rt, v)? {
                    let dest = rt.alloc_typed(proto, elem, values.len())?;
                    for (i, v) in values.into_iter().enumerate() {
                        rt.typed_set(dest, Some(i), v)?;
                    }
                    dest
                } else {
                    let len = array_like_len(rt, v)?;
                    let dest = rt.alloc_typed(proto, elem, len)?;
                    for i in 0..len {
                        let x = rt.get_prop(v, Key::Idx(i as u32))?;
                        rt.typed_set(dest, Some(i), x)?;
                    }
                    dest
                }
            }
        }
    } else {
        let len = to_index(rt, v)?;
        rt.alloc_typed(proto, elem, len)?
    };
    Ok(dest.value())
}

fn array_like_len(rt: &mut Realm, v: Value) -> R<usize> {
    let n = rt.get_named(v, "length")?;
    let n = integer(rt, n)?.max(0.0);
    if n > i32::MAX as f64 {
        return range(rt, "invalid typed array length");
    }
    Ok(n as usize)
}

fn iterable(rt: &mut Realm, v: Value) -> R<Option<Vec<Value>>> {
    let m = rt.get_prop(v, Key::Name(rt.wk[ITERATOR]))?;
    if m.is_undefined() || m.is_null() {
        return Ok(None);
    }
    let it = rt.call_function(m, v, &[])?;
    if Realm::obj(it).is_none() {
        return rt.type_error("iterator result is not an object");
    }
    let kind = rt.get_named(it, "next")?;
    let mut out = Vec::new();
    while let Some(v) = rt.iter_step(it, kind)? {
        if out.len() == i32::MAX as usize || out.try_reserve(1).is_err() {
            return range(rt, "invalid typed array length");
        }
        out.push(v);
    }
    Ok(Some(out))
}

fn require(rt: &mut Realm, this: Value) -> R<View> {
    Realm::obj(this)
        .filter(|o| o.is_kind(K_TYPED))
        .and_then(View::of)
        .ok_or_else(|| rt.error(ErrKind::Type, "receiver is not a typed array"))
}

fn validated(rt: &mut Realm, this: Value) -> R<(View, usize)> {
    let v = require(rt, this)?;
    let n = v
        .len()
        .ok_or_else(|| rt.error(ErrKind::Type, "typed array is out of bounds"))?;
    Ok((v, n))
}

fn make_len(rt: &mut Realm, c: Value, len: usize) -> R<(Value, View)> {
    let v = construct(rt, c, &[Realm::num(len as f64)])?;
    let (view, actual) = validated(rt, v)?;
    if actual < len {
        return rt.type_error("typed array result is too short");
    }
    Ok((v, view))
}

fn for_species(rt: &mut Realm, this: Value, elem: Element, len: usize) -> R<(Value, View)> {
    let c = species_ctor(rt, this)?;
    if c.is_undefined() {
        let proto = rt.buffer_intrinsic(elem.name());
        let o = rt.alloc_typed(proto, elem, len)?;
        return Ok((o.value(), View::of(o).expect("typed array")));
    }
    let (result, view) = make_len(rt, c, len)?;
    same_content(rt, elem, view.elem.expect("element"))?;
    Ok((result, view))
}

fn same_content(rt: &mut Realm, src: Element, dst: Element) -> R<()> {
    if src.is_big() != dst.is_big() {
        return rt.type_error("cannot mix BigInt and Number typed arrays");
    }
    Ok(())
}

fn finish(rt: &mut Realm, dst: View, i: usize, value: Value) -> R<()> {
    let n = dst.elem.expect("element").convert(rt, value)?;
    dst.write(rt, i, n);
    Ok(())
}

fn from(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    if !rt.is_constructor(this) {
        return rt.type_error("typed array constructor required");
    }
    let src = arg(args, 0);
    let cb = arg(args, 1);
    if !cb.is_undefined() {
        callback(rt, cb)?;
    }
    let cb_this = arg(args, 2);
    let values = iterable(rt, src)?;
    let len = match &values {
        Some(v) => v.len(),
        None => array_like_len(rt, src)?,
    };
    let (result, dst) = make_len(rt, this, len)?;
    for i in 0..len {
        let mut v = match &values {
            Some(v) => v[i],
            None => rt.get_prop(src, Key::Idx(i as u32))?,
        };
        if !cb.is_undefined() {
            v = rt.call_function(cb, cb_this, &[v, Realm::num(i as f64)])?;
        }
        finish(rt, dst, i, v)?;
    }
    Ok(result)
}

fn of(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (result, dst) = make_len(rt, this, args.len())?;
    for (i, &v) in args.iter().enumerate() {
        finish(rt, dst, i, v)?;
    }
    Ok(result)
}

fn buffer(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    Ok(require(rt, this)?.buffer.value())
}

fn length(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let v = require(rt, this)?;
    Ok(Realm::num(v.len().unwrap_or(0) as f64))
}

fn byte_length(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let v = require(rt, this)?;
    Ok(Realm::num((v.len().unwrap_or(0) * v.width()) as f64))
}

fn byte_offset(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let v = require(rt, this)?;
    Ok(Realm::num(v.len().map_or(0, |_| v.offset) as f64))
}

fn class_name(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let e = Realm::obj(this).and_then(View::of).and_then(|v| v.elem);
    Ok(e.map_or(Value::UNDEFINED, |e| rt.string(e.name())))
}

fn copy(rt: &mut Realm, src: View, start: usize, dst: View, at: usize, count: usize) -> R<()> {
    let src_elem = src.elem.expect("element");
    let dst_elem = dst.elem.expect("element");
    same_content(rt, src_elem, dst_elem)?;
    if count == 0 {
        return Ok(());
    }
    if src_elem == dst_elem || src_elem.is_big() {
        let w = src.width();
        rt.copy_buffer_bytes(src.buffer, src.offset + start * w, dst.buffer, dst.offset + at * w, count * w);
    } else if src.buffer == dst.buffer {
        let mut values = Vec::new();
        if values.try_reserve_exact(count).is_err() {
            return range(rt, "out of memory");
        }
        values.extend((start..start + count).map(|i| src.read(rt, i).expect("in bounds")));
        for (i, n) in values.into_iter().enumerate() {
            dst.write(rt, at + i, n);
        }
    } else {
        for i in 0..count {
            dst.write(rt, at + i, src.read(rt, start + i).expect("in bounds"));
        }
    }
    Ok(())
}

fn set(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let dst = require(rt, this)?;
    let offset = integer(rt, arg(args, 1))?;
    if offset < 0.0 {
        return range(rt, "negative typed array offset");
    }
    let src = arg(args, 0);
    let (_, len) = validated(rt, this)?;
    if Realm::obj(src).is_some_and(|o| o.is_kind(K_TYPED)) {
        let (src, n) = validated(rt, src)?;
        if offset + n as f64 > len as f64 {
            return range(rt, "typed array set exceeds bounds");
        }
        copy(rt, src, 0, dst, offset as usize, n)?;
    } else {
        let n = array_like_len(rt, src)?;
        if offset + n as f64 > len as f64 {
            return range(rt, "typed array set exceeds bounds");
        }
        let offset = offset as usize;
        for i in 0..n {
            let x = rt.get_prop(src, Key::Idx(i as u32))?;
            finish(rt, dst, offset + i, x)?;
        }
    }
    Ok(Value::UNDEFINED)
}

fn subarray(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let src = require(rt, this)?;
    let len = src.len().unwrap_or(0);
    let start = relative(rt, arg(args, 0), len, 0)?;
    let end_arg = arg(args, 1);
    let end = relative(rt, end_arg, len, len)?;
    let c = species_ctor(rt, this)?;
    let offset = src.offset + start * src.width();
    let n = if src.len.is_none() && end_arg.is_undefined() {
        None
    } else {
        Some(end.saturating_sub(start))
    };
    let args = [
        src.buffer.value(),
        Realm::num(offset as f64),
        n.map_or(Value::UNDEFINED, |n| Realm::num(n as f64)),
    ];
    if c.is_undefined() {
        let elem = src.elem.expect("typed array");
        let proto = rt.buffer_intrinsic(elem.name());
        let v = rt.typed_view_from_args(src.buffer, elem, &args)?;
        return rt.new_view(proto, v).map(Obj::value);
    }
    let v = construct(rt, c, &args[..if n.is_some() { 3 } else { 2 }])?;
    let (dst, _) = validated(rt, v)?;
    same_content(rt, src.elem.expect("element"), dst.elem.expect("element"))?;
    Ok(v)
}

fn slice(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (src, len) = validated(rt, this)?;
    let start = relative(rt, arg(args, 0), len, 0)?;
    let end = relative(rt, arg(args, 1), len, len)?;
    let count = end.saturating_sub(start);
    let (result, dst) = for_species(rt, this, src.elem.expect("typed array"), count)?;
    if count != 0 {
        let (_, len) = validated(rt, this)?;
        let count = count.min(len.saturating_sub(start));
        if src.elem != dst.elem {
            for i in 0..count {
                dst.write(rt, i, src.read(rt, start + i).expect("in bounds"));
            }
        } else if src.buffer == dst.buffer {
            // Slice reads and writes in order, even if its species aliases the source.
            let width = src.width();
            let from = src.offset + start * width;
            let bytes = rt.buffer_bytes_mut(src.buffer).expect("buffer");
            for i in 0..count * width {
                bytes[dst.offset + i] = bytes[from + i];
            }
        } else {
            copy(rt, src, start, dst, 0, count)?;
        }
    }
    Ok(result)
}

fn fill(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (v, len) = validated(rt, this)?;
    let n = v.elem.expect("element").convert(rt, arg(args, 0))?;
    let start = relative(rt, arg(args, 1), len, 0)?;
    let end = relative(rt, arg(args, 2), len, len)?;
    let (_, len) = validated(rt, this)?;
    for i in start..end.min(len) {
        v.write(rt, i, n);
    }
    Ok(this)
}

fn search(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let includes = native_param(rt) != 0;
    let (v, len) = validated(rt, this)?;
    let target = arg(args, 0);
    let miss = if includes { Value::FALSE } else { Value::int(-1) };
    if len == 0 {
        return Ok(miss);
    }
    let start = relative(rt, arg(args, 1), len, 0)?;
    let elem = v.elem.expect("element");
    let big = Realm::big_of(target);
    for i in start..len {
        let n = v.read(rt, i);
        let found = if let Some(bits) = n {
            if elem.is_big() {
                let (neg, mag) = elem.big_parts(bits);
                big.is_some_and(|b| {
                    let a = b.bigint();
                    a.neg == neg && a.mag.len() <= 1 && a.mag.first().copied().unwrap_or(0) == mag
                })
            } else {
                let n = f64::from_bits(bits);
                target
                    .as_f64()
                    .is_some_and(|x| n == x || (includes && n.is_nan() && x.is_nan()))
            }
        } else {
            includes && target.is_undefined()
        };
        if found {
            return Ok(if includes { Value::TRUE } else { Realm::num(i as f64) });
        }
    }
    Ok(miss)
}

fn join(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (v, len) = validated(rt, this)?;
    let sep = arg(args, 0);
    let sep = if sep.is_undefined() { rt.intern(",") } else { rt.to_str(sep)? };
    let sep = rt.flat(sep);
    let units = sep.chars().expect("flat string");
    let mut out = Vec::new();
    let elem = v.elem.expect("element");
    for i in 0..len {
        if i != 0 {
            out.extend(units.units());
        }
        if let Some(bits) = v.read(rt, i) {
            let text = match elem {
                Element::BI64 => (bits as i64).to_string(),
                Element::BU64 => bits.to_string(),
                _ => Realm::num_to_string(f64::from_bits(bits)),
            };
            out.extend(text.encode_utf16());
        }
    }
    Ok(rt.make_units(&out))
}

fn callback(rt: &mut Realm, f: Value) -> R<()> {
    if !Realm::obj(f).is_some_and(Obj::is_callable) {
        return rt.type_error("callback is not a function");
    }
    Ok(())
}

fn item(rt: &mut Realm, v: View, i: usize) -> R<Value> {
    match v.read(rt, i) {
        Some(bits) => v.elem.expect("element").value(rt, bits),
        None => Ok(Value::UNDEFINED),
    }
}

fn map(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (src, len) = validated(rt, this)?;
    let cb = arg(args, 0);
    callback(rt, cb)?;
    let (result, dst) = for_species(rt, this, src.elem.expect("typed array"), len)?;
    for i in 0..len {
        let element = item(rt, src, i)?;
        let v = rt.call_function(cb, arg(args, 1), &[element, Realm::num(i as f64), this])?;
        finish(rt, dst, i, v)?;
    }
    Ok(result)
}

fn filter(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (src, len) = validated(rt, this)?;
    let cb = arg(args, 0);
    callback(rt, cb)?;
    let this_arg = arg(args, 1);
    let mut kept = Vec::new();
    for i in 0..len {
        let value = item(rt, src, i)?;
        let selected = rt.call_function(cb, this_arg, &[value, Realm::num(i as f64), this])?;
        if Realm::truthy(selected) {
            if kept.try_reserve(1).is_err() {
                return range(rt, "out of memory");
            }
            kept.push(value);
        }
    }
    let (result, dst) = for_species(rt, this, src.elem.expect("typed array"), kept.len())?;
    for (i, value) in kept.into_iter().enumerate() {
        finish(rt, dst, i, value)?;
    }
    Ok(result)
}

fn for_each(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (src, len) = validated(rt, this)?;
    let cb = arg(args, 0);
    callback(rt, cb)?;
    for i in 0..len {
        let element = item(rt, src, i)?;
        rt.call_function(cb, arg(args, 1), &[element, Realm::num(i as f64), this])?;
    }
    Ok(Value::UNDEFINED)
}

fn reduce(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (src, len) = validated(rt, this)?;
    let cb = arg(args, 0);
    callback(rt, cb)?;
    let (mut acc, start) = match args.get(1) {
        Some(&x) => (x, 0),
        None if len != 0 => (item(rt, src, 0)?, 1),
        None => return rt.type_error("reduce of an empty typed array with no initial value"),
    };
    for i in start..len {
        let element = item(rt, src, i)?;
        acc = rt.call_function(cb, Value::UNDEFINED, &[acc, element, Realm::num(i as f64), this])?;
    }
    Ok(acc)
}

fn at(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (v, len) = validated(rt, this)?;
    let i = integer(rt, arg(args, 0))?;
    let i = if i < 0.0 { i + len as f64 } else { i };
    if 0.0 <= i && i < len as f64 {
        item(rt, v, i as usize)
    } else {
        Ok(Value::UNDEFINED)
    }
}

fn sort(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let cb = arg(args, 0);
    if !cb.is_undefined() {
        callback(rt, cb)?;
    }
    let (v, len) = validated(rt, this)?;
    let elem = v.elem.expect("element");
    let mut values = Vec::new();
    if values.try_reserve_exact(len).is_err() {
        return range(rt, "out of memory");
    }
    values.extend((0..len).map(|i| v.read(rt, i).expect("in bounds")));
    if cb.is_undefined() {
        match elem {
            Element::BI64 => values.sort_unstable_by_key(|&bits| bits as i64),
            Element::BU64 => values.sort_unstable(),
            _ => values.sort_unstable_by(|&a, &b| {
                let (a, b) = (f64::from_bits(a), f64::from_bits(b));
                match (a.is_nan(), b.is_nan()) {
                    (false, false) => a.total_cmp(&b),
                    (a, b) => a.cmp(&b),
                }
            }),
        }
    } else {
        // A comparator can throw or be inconsistent. Merge runs without asking
        // Rust's sort to impose an ordering that the callback may not provide.
        let mut merged = Vec::new();
        if merged.try_reserve_exact(len).is_err() {
            return range(rt, "out of memory");
        }
        let mut width = 1;
        while width < len {
            merged.clear();
            for pair in values.chunks(width * 2) {
                let (left, right) = pair.split_at(width.min(pair.len()));
                let (mut i, mut j) = (0, 0);
                while i < left.len() && j < right.len() {
                    let a = elem.value(rt, left[i])?;
                    let b = elem.value(rt, right[j])?;
                    let order = rt.call_function(cb, Value::UNDEFINED, &[a, b])?;
                    if rt.to_number(order)? > 0.0 {
                        merged.push(right[j]);
                        j += 1;
                    } else {
                        merged.push(left[i]);
                        i += 1;
                    }
                }
                merged.extend_from_slice(&left[i..]);
                merged.extend_from_slice(&right[j..]);
            }
            std::mem::swap(&mut merged, &mut values);
            width *= 2;
        }
    }
    for (i, n) in values.into_iter().enumerate() {
        v.write(rt, i, n);
    }
    Ok(this)
}

fn iterator(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    validated(rt, this)?;
    let kind = Value::int(native_param(rt));
    array_iterator(rt, Value::UNDEFINED, &[this, kind])
}

/// Create the common iterator used by Array.prototype and TypedArray.prototype.
pub(crate) fn array_iterator(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let p = Realm::obj(rt.array_iterator_proto()?).expect("iterator prototype");
    rt.buffer_object(p, K_ARRAY_ITER, &[arg(args, 0), Value::int(0), arg(args, 1)])
        .map(Obj::value)
}

pub(crate) fn iterator_next(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let Some(o) = Realm::obj(this).filter(|o| o.is_kind(K_ARRAY_ITER)) else {
        return rt.type_error("receiver is not an array iterator");
    };
    let source = buffer_slot(o, 0);
    let Some(src) = Realm::obj(source) else {
        return Ok(rt.iter_result(Value::UNDEFINED, true));
    };
    let i = buffer_slot(o, 1);
    let index = i.as_f64().expect("index");
    let typed = View::of(src).filter(|v| v.elem.is_some());
    let len = if let Some(v) = typed {
        v.len()
            .ok_or_else(|| rt.error(ErrKind::Type, "typed array is out of bounds"))? as f64
    } else {
        let length = rt.get_prop(source, Key::Name(rt.k_length))?;
        integer(rt, length)?.clamp(0.0, 9_007_199_254_740_991.0)
    };
    if index >= len {
        o.set_slot(&rt.shapes, 0, Value::UNDEFINED);
        return Ok(rt.iter_result(Value::UNDEFINED, true));
    }
    o.set_slot(&rt.shapes, 1, Realm::num(index + 1.0));
    let kind = buffer_slot(o, 2).as_int().expect("iterator kind");
    let value = if kind == 0 {
        i
    } else {
        let element = if let Some(v) = typed {
            item(rt, v, index as usize)?
        } else {
            let key = rt.key_of(i)?;
            rt.get_prop(source, key)?
        };
        if kind == 1 { element } else { rt.new_array(&[i, element]) }
    };
    Ok(rt.iter_result(value, false))
}

#[cfg(test)]
#[path = "bi_bigint_typed_tests.rs"]
mod bigint_tests;

#[cfg(test)]
#[path = "bi_typed_filter_tests.rs"]
mod filter_tests;
