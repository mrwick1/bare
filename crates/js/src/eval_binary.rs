// Byte buffers and text codecs of the evaluator. Included by `eval.rs` (`include!`), so it shares that module's
// imports and private items. The storage and the integer-indexed property rules are in `heap_binary.rs`; the
// UTF-8 codec is in `text.rs`. This file is the JavaScript surface and the checks.
//
// Supported:
// - `ArrayBuffer(length)`, `.byteLength`, `.slice(begin, end)`, `ArrayBuffer.isView`.
// - `Uint8Array(length | buffer, byteOffset, length | typedArray | array | array-like)`, the getters `buffer`, `byteLength`,
//   `byteOffset`, `length`, `BYTES_PER_ELEMENT`, `set(source, offset)`, `subarray` (shared bytes), `slice` (copy),
//   `indexOf` (strict equality, see `eval_binary_search.rs`), integer indexing (read, write, `in`, `delete`,
//   `Object.keys`, `Object.getOwnPropertyDescriptor`, `defineProperty`) and iteration.
//   `new Uint8Array(iterable)` takes every value through the shared iterator protocol of `eval_symbols.rs` (an object whose
//   `@@iterator` is not undefined or null), then converts each. `values`, `keys`, `entries` and `[@@iterator]` of
//   `%TypedArray%.prototype` make the shared array iterator (no second engine); it reads the view's own length.
//   `%TypedArray%` is not global, but `Object.getPrototypeOf(Uint8Array)` finds it, as in a browser.
// - `TextEncoder` (`encoding`, `encode`, `encodeInto`) and `TextDecoder` (`encoding`, `fatal`, `ignoreBOM`, `decode` with
//   `stream`). UTF-8 only. A known WHATWG label of another encoding is `Unsupported`; an unknown label is a `RangeError`.
// Gates (explicit `Unsupported`, never silent): resizable `ArrayBuffer` options, `resize`, `transfer`, and the typed-array
// methods that are not above (`fill`, `map`, `from`, `of`, ...). `@@toStringTag` is bound for the four classes
// (a data property for `ArrayBuffer`, `TextEncoder` and `TextDecoder`, a getter on `%TypedArray%.prototype`). `slice` and
// `subarray` read `constructor` and `@@species`: `undefined`, `null` and the intrinsic class give the default; any other
// constructor is `Unsupported`. A class that extends one of these constructors is `Unsupported` when it is constructed.
// Absent: `SharedArrayBuffer`, `DataView`, every other typed-array family and detached buffers.
//
// Limits: backing bytes are capped for one buffer and for all live buffers (`Interp::set_binary_limits`; errors
// `Heap(BufferLength)` and `Heap(BufferBytes)`), every size is checked before the allocation or the copy, and every
// pass over bytes or elements consumes steps (one step for 64 bytes copied or 16 bytes encoded or decoded, one for
// each element read through a JavaScript property). A limit is never a JavaScript exception.
//
// GC: the buffer bytes are owned by the heap object and are freed by `collect`. A view marks its buffer. The five
// prototypes are roots.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BinFn {
    AbCtor,
    AbIsView,
    AbByteLength,
    AbSlice,
    TaCtor,
    TaBuffer,
    TaByteLength,
    TaByteOffset,
    TaLength,
    TaSet,
    TaSubarray,
    TaSlice,
    TaIndexOf,
    TaValues,
    TaKeys,
    TaEntries,
    TaTag,
    U8Ctor,
    EncCtor,
    EncEncoding,
    EncEncode,
    EncEncodeInto,
    DecCtor,
    DecEncoding,
    DecFatal,
    DecIgnoreBom,
    DecDecode,
    Gate(&'static str),
}

impl BinFn {
    fn ctor_name(self) -> Option<&'static str> {
        Some(match self {
            BinFn::AbCtor => "ArrayBuffer",
            BinFn::U8Ctor => "Uint8Array",
            BinFn::EncCtor => "TextEncoder",
            BinFn::DecCtor => "TextDecoder",
            _ => return None,
        })
    }
}

#[derive(Clone, Copy)]
struct BinProtos {
    ab: ObjId,
    ta: ObjId,
    u8: ObjId,
    enc: ObjId,
    dec: ObjId,
    /// The intrinsic `ArrayBuffer` and `Uint8Array`: the defaults of `SpeciesConstructor`.
    ab_c: ObjId,
    u8_c: ObjId,
}

#[derive(Default)]
struct BinaryState {
    protos: Option<BinProtos>,
}

const TA_GATES: [(&str, &str); 18] = [
    ("fill", "TypedArray.prototype.fill"), ("copyWithin", "TypedArray.prototype.copyWithin"), ("reverse", "TypedArray.prototype.reverse"),
    ("sort", "TypedArray.prototype.sort"), ("lastIndexOf", "TypedArray.prototype.lastIndexOf"),
    ("includes", "TypedArray.prototype.includes"), ("join", "TypedArray.prototype.join"), ("map", "TypedArray.prototype.map"),
    ("filter", "TypedArray.prototype.filter"), ("forEach", "TypedArray.prototype.forEach"), ("reduce", "TypedArray.prototype.reduce"),
    ("every", "TypedArray.prototype.every"), ("some", "TypedArray.prototype.some"), ("find", "TypedArray.prototype.find"),
    ("findIndex", "TypedArray.prototype.findIndex"), ("at", "TypedArray.prototype.at"), ("toString", "TypedArray.prototype.toString"),
    ("toReversed", "TypedArray.prototype.toReversed"),
];

const MAX_SAFE: f64 = 9007199254740991.0;

impl<'p> Interp<'p> {
    // ------------------------------------------------------------ host API

    /// Cap the backing bytes: all live buffers together, and one buffer. The defaults are 256 MiB each.
    pub fn set_binary_limits(&mut self, total_bytes: usize, single_bytes: usize) {
        self.heap.set_buffer_limits(total_bytes, single_bytes);
    }

    /// A new `ArrayBuffer` that owns `bytes` (no copy).
    pub fn new_array_buffer(&mut self, bytes: Vec<u8>) -> Result<Value, Error> {
        let ab = self.bp().ab;
        Ok(Value::Object(h!(self, self.heap.new_buffer_from(Some(ab), bytes))))
    }

    /// A new `Uint8Array` over a new buffer that owns `bytes` (no copy). For code that hands network data to a script.
    pub fn new_uint8_array(&mut self, bytes: Vec<u8>) -> Result<Value, Error> {
        let p = self.bp();
        let len = bytes.len();
        let buf = h!(self, self.heap.new_buffer_from(Some(p.ab), bytes));
        Ok(Value::Object(h!(self, self.heap.new_view(Some(p.u8), buf, 0, len))))
    }

    /// The bytes of an `ArrayBuffer` or `Uint8Array` value, borrowed with no copy. `None` for any other value.
    pub fn bytes_of(&self, v: Value) -> Option<&[u8]> {
        let Value::Object(o) = v else { return None };
        self.heap.bytes(o)
    }

    // ------------------------------------------------------------ setup

    fn bp(&self) -> BinProtos {
        self.bin.protos.expect("install_binary ran")
    }

    fn binary_roots(&self, out: &mut Vec<Value>) {
        if let Some(p) = self.bin.protos {
            out.extend([p.ab, p.ta, p.u8, p.enc, p.dec, p.ab_c, p.u8_c].map(Value::Object));
        }
    }

    fn bin_ctor(&mut self, name: &str, f: BinFn, len: u32, proto: ObjId) -> Res<ObjId> {
        let c = self.native(Nat::Bin(f), name, len)?;
        self.def(c, self.k_prototype, Value::Object(proto), false, false, false)?;
        self.def(proto, self.k_constructor, Value::Object(c), true, false, true)?;
        Ok(c)
    }

    fn bin_global(&mut self, name: &str, c: ObjId) -> Res<()> {
        let k = h!(self, self.heap.key_from_str(name));
        self.def(self.global, k, Value::Object(c), true, false, true)
    }

    fn bin_method(&mut self, on: ObjId, name: &str, f: BinFn, len: u32) -> Res<()> {
        self.method(on, name, Nat::Bin(f), len).map(drop)
    }

    /// An accessor property. ECMAScript built-ins are not enumerable; WebIDL attributes (`TextEncoder`) are.
    fn bin_getter(&mut self, on: ObjId, name: &str, f: BinFn, enumerable: bool) -> Res<()> {
        let g = self.native(Nat::Bin(f), &format!("get {name}"), 0)?;
        let k = h!(self, self.heap.key_from_str(name));
        let d = PropertyDescriptor { get: Some(Some(g)), set: Some(None), enumerable: Some(enumerable), configurable: Some(true), ..Default::default() };
        h!(self, self.heap.define_own_property(on, k, &d));
        Ok(())
    }

    fn install_binary(&mut self) -> Res<()> {
        let op = self.object_proto;
        let proto = |s: &mut Self, parent: ObjId| -> Res<ObjId> { Ok(h!(s, s.heap.new_object(Some(parent)))) };
        let (ab_p, ta_p) = (proto(self, op)?, proto(self, op)?);
        let u8_p = proto(self, ta_p)?;
        let (enc_p, dec_p) = (proto(self, op)?, proto(self, op)?);
        self.bin.protos = Some(BinProtos { ab: ab_p, ta: ta_p, u8: u8_p, enc: enc_p, dec: dec_p, ab_c: ab_p, u8_c: u8_p });

        let species = PropertyKey::Symbol(self.sc.wk[WK_SPECIES]);
        let ab = self.bin_ctor("ArrayBuffer", BinFn::AbCtor, 1, ab_p)?;
        self.sc_getter(ab, species, "get [Symbol.species]", ScFn::Species)?;
        self.sc_tag(ab_p, "ArrayBuffer")?;
        self.bin_global("ArrayBuffer", ab)?;
        self.bin_method(ab, "isView", BinFn::AbIsView, 1)?;
        self.bin_getter(ab_p, "byteLength", BinFn::AbByteLength, false)?;
        self.bin_method(ab_p, "slice", BinFn::AbSlice, 2)?;
        for (n, w) in [("resize", "ArrayBuffer.prototype.resize"), ("transfer", "ArrayBuffer.prototype.transfer"), ("transferToFixedLength", "ArrayBuffer.prototype.transferToFixedLength")] {
            self.bin_method(ab_p, n, BinFn::Gate(w), 0)?;
        }

        // %TypedArray%: the shared constructor and prototype. Not a global.
        let ta = self.bin_ctor("TypedArray", BinFn::TaCtor, 0, ta_p)?;
        for (n, f) in [("buffer", BinFn::TaBuffer), ("byteLength", BinFn::TaByteLength), ("byteOffset", BinFn::TaByteOffset), ("length", BinFn::TaLength)] {
            self.bin_getter(ta_p, n, f, false)?;
        }
        self.bin_method(ta_p, "set", BinFn::TaSet, 1)?;
        self.bin_method(ta_p, "subarray", BinFn::TaSubarray, 2)?;
        self.bin_method(ta_p, "slice", BinFn::TaSlice, 2)?;
        self.bin_method(ta_p, "indexOf", BinFn::TaIndexOf, 1)?;
        // `values` is `[Symbol.iterator]`; the iterator is the shared array iterator of `eval_symbols.rs`.
        let tv = self.native(Nat::Bin(BinFn::TaValues), "values", 0)?;
        let kv = h!(self, self.heap.key_from_str("values"));
        self.def(ta_p, kv, Value::Object(tv), true, false, true)?;
        self.def(ta_p, self.sc.k_iterator, Value::Object(tv), true, false, true)?;
        self.bin_method(ta_p, "keys", BinFn::TaKeys, 0)?;
        self.bin_method(ta_p, "entries", BinFn::TaEntries, 0)?;
        let tag = self.native(Nat::Bin(BinFn::TaTag), "get [Symbol.toStringTag]", 0)?;
        let d = PropertyDescriptor { get: Some(Some(tag)), set: Some(None), enumerable: Some(false), configurable: Some(true), ..Default::default() };
        h!(self, self.heap.define_own_property(ta_p, self.sc.k_to_string_tag, &d));
        self.sc_getter(ta, species, "get [Symbol.species]", ScFn::Species)?;
        for (n, w) in TA_GATES {
            self.bin_method(ta_p, n, BinFn::Gate(w), 0)?;
        }
        self.bin_method(ta, "from", BinFn::Gate("TypedArray.from"), 1)?;
        self.bin_method(ta, "of", BinFn::Gate("TypedArray.of"), 0)?;

        let u8c = self.bin_ctor("Uint8Array", BinFn::U8Ctor, 3, u8_p)?;
        h!(self, self.heap.set_prototype_of(u8c, Some(ta)));
        let k = h!(self, self.heap.key_from_str("BYTES_PER_ELEMENT"));
        self.def(u8c, k, Value::Number(1.0), false, false, false)?;
        self.def(u8_p, k, Value::Number(1.0), false, false, false)?;
        self.bin_global("Uint8Array", u8c)?;
        if let Some(p) = &mut self.bin.protos {
            p.ab_c = ab;
            p.u8_c = u8c;
        }

        let enc = self.bin_ctor("TextEncoder", BinFn::EncCtor, 0, enc_p)?;
        self.bin_global("TextEncoder", enc)?;
        self.sc_tag(enc_p, "TextEncoder")?;
        self.bin_getter(enc_p, "encoding", BinFn::EncEncoding, true)?;
        self.bin_method(enc_p, "encode", BinFn::EncEncode, 0)?;
        self.bin_method(enc_p, "encodeInto", BinFn::EncEncodeInto, 2)?;
        let dec = self.bin_ctor("TextDecoder", BinFn::DecCtor, 0, dec_p)?;
        self.bin_global("TextDecoder", dec)?;
        self.sc_tag(dec_p, "TextDecoder")?;
        self.bin_getter(dec_p, "encoding", BinFn::DecEncoding, true)?;
        self.bin_getter(dec_p, "fatal", BinFn::DecFatal, true)?;
        self.bin_getter(dec_p, "ignoreBOM", BinFn::DecIgnoreBom, true)?;
        self.bin_method(dec_p, "decode", BinFn::DecDecode, 0)?;
        Ok(())
    }

    // ------------------------------------------------------------ helpers

    /// Consume steps for a pass over `bytes` bytes: one step for each `1 << shift`. Checked before the pass.
    fn work(&mut self, bytes: usize, shift: u32) -> Res<()> {
        self.steps = self.steps.saturating_add((bytes >> shift) as u64);
        if self.steps > self.step_limit {
            return Err(Error::Limit(LimitKind::Steps));
        }
        Ok(())
    }

    /// ToIntegerOrInfinity.
    fn num_integer(&mut self, v: Value) -> Res<f64> {
        let n = self.coerce_number(v)?;
        Ok(if n.is_nan() { 0.0 } else { n.trunc() + 0.0 })
    }

    /// ToIndex: a length or an offset. A `RangeError` with `msg` when it is negative or over 2^53 - 1.
    fn num_index(&mut self, v: Value, msg: &str) -> Res<usize> {
        let n = self.num_integer(v)?;
        if !(0.0..=MAX_SAFE).contains(&n) {
            return Err(self.mk_err(RANGE_ERROR, msg));
        }
        Ok(n as usize)
    }

    /// ToLength.
    fn num_length(&mut self, v: Value) -> Res<f64> {
        Ok(self.num_integer(v)?.clamp(0.0, MAX_SAFE))
    }

    /// A relative index (`slice`, `subarray`): negative counts from the end. Clamped to `0..=len`.
    fn rel_index(n: f64, len: usize) -> usize {
        if n < 0.0 { (len as f64 + n).max(0.0) as usize } else { n.min(len as f64) as usize }
    }

    fn incompatible(&mut self, what: &str, this: Value) -> Error {
        let t = self.display(this);
        self.type_err(&format!("Method {what} called on incompatible receiver {t}"))
    }

    /// The view behind `this`: `(object, buffer, byte offset, length)`.
    fn this_view(&mut self, this: Value, what: &str) -> Res<(ObjId, ObjId, usize, usize)> {
        if let Value::Object(o) = this
            && let Some((b, off, len)) = self.heap.view_parts(o)
        {
            return Ok((o, b, off, len));
        }
        Err(self.incompatible(what, this))
    }

    /// `SpeciesConstructor(o, default)`: reads `constructor`, then `constructor[@@species]` (each once). `undefined`, `null`
    /// and the intrinsic `default` give the default. Another constructor is `Unsupported` (`what`): it would need
    /// `Construct` with result checks. A value that is not a constructor is a `TypeError`.
    fn bin_species(&mut self, o: ObjId, default: ObjId, what: &'static str) -> Res<()> {
        let c = match self.get_prop(Value::Object(o), self.k_constructor)? {
            Value::Undefined => return Ok(()),
            c @ Value::Object(_) => c,
            _ => return Err(self.type_err("object.constructor is not a constructor")),
        };
        match self.get_prop(c, self.wk_key(WK_SPECIES))? {
            Value::Undefined | Value::Null => Ok(()),
            Value::Object(s) if s == default => Ok(()),
            Value::Object(s) if self.is_constructor(s) => Err(unsupported(what, Span::default())),
            _ => Err(self.type_err("object.constructor[Symbol.species] is not a constructor")),
        }
    }

    fn new_u8(&mut self, len: usize) -> Res<ObjId> {
        let p = self.bp();
        let buf = h!(self, self.heap.new_buffer(Some(p.ab), len));
        Ok(h!(self, self.heap.new_view(Some(p.u8), buf, 0, len)))
    }

    fn put_u8(&mut self, view: ObjId, i: usize, n: f64) {
        self.heap.view_set(view, i, to_uint32(n) as u8);
    }

    /// Options of a WebIDL dictionary: `undefined` and `null` are empty; an object is read; anything else is a TypeError.
    fn dict_arg(&mut self, v: Value, what: &str) -> Res<Option<Value>> {
        match v {
            Value::Undefined | Value::Null => Ok(None),
            Value::Object(_) => Ok(Some(v)),
            _ => Err(self.type_err(&format!("The provided value is not of type '{what}'"))),
        }
    }

    fn flag(&mut self, dict: Option<Value>, name: &str) -> Res<bool> {
        let Some(d) = dict else { return Ok(false) };
        let k = h!(self, self.heap.key_from_str(name));
        let v = self.get_prop(d, k)?;
        self.truthy(v)
    }

    // ------------------------------------------------------------ dispatch

    fn call_binary(&mut self, f: BinFn, this: Value, args: &[Value], is_new: bool) -> Res<Value> {
        use BinFn::*;
        let (a0, a1, a2) = (arg(args, 0), arg(args, 1), arg(args, 2));
        if let Some(n) = f.ctor_name()
            && !is_new
        {
            return Err(self.type_err(&format!("Constructor {n} requires 'new'")));
        }
        let p = if self.bin.protos.is_some() { Some(self.bp()) } else { None };
        match f {
            Gate(what) => Err(unsupported(what, Span::default())),
            AbCtor => {
                let len = self.num_index(a0, "Invalid array buffer length")?;
                if let Value::Object(_) = a1 {
                    let k = h!(self, self.heap.key_from_str("maxByteLength"));
                    if !matches!(self.get_prop(a1, k)?, Value::Undefined) {
                        return Err(unsupported("resizable ArrayBuffer", Span::default()));
                    }
                }
                Ok(Value::Object(h!(self, self.heap.new_buffer(Some(p.unwrap().ab), len))))
            }
            AbIsView => Ok(Value::Bool(matches!(a0, Value::Object(o) if self.heap.exotic_kind(o) == Some(ExoticKind::View)))),
            AbByteLength => match this {
                Value::Object(o) if self.heap.buffer_len(o).is_some() => Ok(Value::Number(self.heap.buffer_len(o).unwrap() as f64)),
                _ => Err(self.incompatible("ArrayBuffer.prototype.byteLength", this)),
            },
            AbSlice => {
                let Value::Object(o) = this else { return Err(self.incompatible("ArrayBuffer.prototype.slice", this)) };
                let Some(len) = self.heap.buffer_len(o) else { return Err(self.incompatible("ArrayBuffer.prototype.slice", this)) };
                let first = self.num_integer(a0)?;
                let first = Self::rel_index(first, len);
                let last = if matches!(a1, Value::Undefined) { len } else { let e = self.num_integer(a1)?; Self::rel_index(e, len) };
                self.bin_species(o, p.unwrap().ab_c, "ArrayBuffer species constructor other than ArrayBuffer")?;
                let last = last.max(first);
                self.work(last - first, 6)?;
                Ok(Value::Object(h!(self, self.heap.new_buffer_copy(Some(p.unwrap().ab), o, first, last))))
            }
            TaCtor => Err(self.type_err("Abstract class TypedArray not directly constructable")),
            TaBuffer => {
                let (_, b, _, _) = self.this_view(this, "get TypedArray.prototype.buffer")?;
                Ok(Value::Object(b))
            }
            TaByteLength | TaLength => {
                let (_, _, _, len) = self.this_view(this, "get TypedArray.prototype.length")?;
                Ok(Value::Number(len as f64))
            }
            TaByteOffset => {
                let (_, _, off, _) = self.this_view(this, "get TypedArray.prototype.byteOffset")?;
                Ok(Value::Number(off as f64))
            }
            U8Ctor => self.u8_construct(a0, a1, a2),
            TaValues | TaKeys | TaEntries => {
                let (what, w) = match f {
                    TaKeys => (IterWhat::Keys, "%TypedArray%.prototype.keys"),
                    TaEntries => (IterWhat::Entries, "%TypedArray%.prototype.entries"),
                    _ => (IterWhat::Values, "%TypedArray%.prototype.values"),
                };
                self.this_view(this, w)?;
                self.sc_array_iter(this, what)
            }
            TaTag => match this {
                Value::Object(o) if self.heap.exotic_kind(o) == Some(ExoticKind::View) => self.str_value("Uint8Array"),
                _ => Ok(Value::Undefined),
            },
            TaSet => self.ta_set(this, a0, a1),
            TaIndexOf => self.ta_index_of(this, a0, a1),
            TaSubarray => {
                let (o, buf, off, len) = self.this_view(this, "%TypedArray%.prototype.subarray")?;
                let first = self.num_integer(a0)?;
                let first = Self::rel_index(first, len);
                let last = if matches!(a1, Value::Undefined) { len } else { let e = self.num_integer(a1)?; Self::rel_index(e, len) };
                self.bin_species(o, p.unwrap().u8_c, "typed array species constructor other than Uint8Array")?;
                let n = last.saturating_sub(first);
                Ok(Value::Object(h!(self, self.heap.new_view(Some(p.unwrap().u8), buf, off + first, n))))
            }
            TaSlice => {
                let (o, _, _, len) = self.this_view(this, "%TypedArray%.prototype.slice")?;
                let first = self.num_integer(a0)?;
                let first = Self::rel_index(first, len);
                let last = if matches!(a1, Value::Undefined) { len } else { let e = self.num_integer(a1)?; Self::rel_index(e, len) };
                self.bin_species(o, p.unwrap().u8_c, "typed array species constructor other than Uint8Array")?;
                let last = last.max(first);
                self.work(last - first, 6)?;
                let p = p.unwrap();
                let buf = h!(self, self.heap.new_buffer_copy(Some(p.ab), o, first, last));
                Ok(Value::Object(h!(self, self.heap.new_view(Some(p.u8), buf, 0, last - first))))
            }
            EncCtor => Ok(Value::Object(h!(self, self.heap.new_encoder(Some(p.unwrap().enc))))),
            EncEncoding => self.brand(this, ExoticKind::Encoder, "TextEncoder").and_then(|_| self.str_value("utf-8")),
            EncEncode => {
                self.brand(this, ExoticKind::Encoder, "TextEncoder")?;
                let s = if matches!(a0, Value::Undefined) { h!(self, self.heap.intern_str("")) } else { self.coerce_string(a0)? };
                let n = crate::text::utf8_len(h!(self, self.heap.str_units(s)));
                self.work(n, 4)?;
                let p = p.unwrap();
                let buf = h!(self, self.heap.new_buffer(Some(p.ab), n));
                h!(self, self.heap.encode_utf8_into(s, buf));
                Ok(Value::Object(h!(self, self.heap.new_view(Some(p.u8), buf, 0, n))))
            }
            EncEncodeInto => {
                self.brand(this, ExoticKind::Encoder, "TextEncoder")?;
                let s = self.coerce_string(a0)?;
                let dest = match a1 {
                    Value::Object(d) if self.heap.exotic_kind(d) == Some(ExoticKind::View) => d,
                    _ => return Err(self.type_err("The \"dest\" argument must be an instance of Uint8Array.")),
                };
                let n = h!(self, self.heap.str_len(s)).min(self.heap.bytes(dest).map_or(0, <[u8]>::len).saturating_mul(1));
                self.work(n, 4)?;
                let (read, written) = h!(self, self.heap.encode_utf8_into(s, dest));
                let r = h!(self, self.heap.new_object(Some(self.object_proto)));
                for (name, v) in [("read", read), ("written", written)] {
                    let k = h!(self, self.heap.key_from_str(name));
                    self.def(r, k, Value::Number(v as f64), true, true, true)?;
                }
                Ok(Value::Object(r))
            }
            DecCtor => self.decoder_construct(a0, a1),
            DecEncoding => self.brand(this, ExoticKind::Decoder, "TextDecoder").and_then(|_| self.str_value("utf-8")),
            DecFatal | DecIgnoreBom => {
                self.brand(this, ExoticKind::Decoder, "TextDecoder")?;
                let Value::Object(o) = this else { unreachable!() };
                let d = self.heap.decoder(o).expect("branded");
                Ok(Value::Bool(if f == DecFatal { d.fatal } else { d.ignore_bom }))
            }
            DecDecode => self.decoder_decode(this, a0, a1),
        }
    }

    fn brand(&mut self, this: Value, kind: ExoticKind, class: &str) -> Res<()> {
        match this {
            Value::Object(o) if self.heap.exotic_kind(o) == Some(kind) => Ok(()),
            _ => {
                let t = self.display(this);
                Err(self.type_err(&format!("Value of \"this\" must be of type {class}, got {t}")))
            }
        }
    }

    // ------------------------------------------------------------ Uint8Array

    fn u8_construct(&mut self, a0: Value, a1: Value, a2: Value) -> Res<Value> {
        let Value::Object(src) = a0 else {
            let len = self.num_index(a0, "Invalid typed array length")?;
            return Ok(Value::Object(self.new_u8(len)?));
        };
        match self.heap.exotic_kind(src) {
            Some(ExoticKind::Buffer) => {
                let buf_len = self.heap.buffer_len(src).expect("buffer");
                let off = self.num_index(a1, "Start offset is outside the bounds of the buffer")?;
                let len = if matches!(a2, Value::Undefined) {
                    if off > buf_len {
                        return Err(self.mk_err(RANGE_ERROR, &format!("Start offset {off} is outside the bounds of the buffer")));
                    }
                    buf_len - off
                } else {
                    let n = self.num_index(a2, "Invalid typed array length")?;
                    if off.checked_add(n).is_none_or(|e| e > buf_len) {
                        return Err(self.mk_err(RANGE_ERROR, &format!("Invalid typed array length: {n}")));
                    }
                    n
                };
                if off > buf_len {
                    return Err(self.mk_err(RANGE_ERROR, &format!("Start offset {off} is outside the bounds of the buffer")));
                }
                Ok(Value::Object(h!(self, self.heap.new_view(Some(self.bp().u8), src, off, len))))
            }
            Some(ExoticKind::View) => {
                let len = self.heap.view_parts(src).expect("view").2;
                self.work(len, 6)?;
                let p = self.bp();
                let buf = h!(self, self.heap.new_buffer_copy(Some(p.ab), src, 0, len));
                Ok(Value::Object(h!(self, self.heap.new_view(Some(p.u8), buf, 0, len))))
            }
            _ if let Some(mut it) = self.iter_open(a0)? => {
                // An iterable (`@@iterator` is not undefined or null): take every value first, then convert each one.
                // `IteratorToList` does not close the iterator on an error. The values are bounded before they grow.
                let cap = self.heap.buffer_limits().1;
                let mut vals = Vec::new();
                while let Some(v) = self.iter_next(&mut it)? {
                    if vals.len() >= cap {
                        return Err(Error::Limit(LimitKind::Heap(Limit::BufferLength)));
                    }
                    self.iter_buffer_room(vals.len())?;
                    vals.push(v);
                }
                let out = self.new_u8(vals.len())?;
                for (i, v) in vals.into_iter().enumerate() {
                    self.tick()?;
                    let n = self.coerce_number(v)?;
                    self.put_u8(out, i, n);
                }
                Ok(Value::Object(out))
            }
            _ => {
                // An array-like: the length, then one read and one conversion for each index.
                let lv = self.get_prop(a0, self.k_length)?;
                let n = self.num_length(lv)?;
                if n > self.heap.buffer_limits().1 as f64 {
                    return Err(Error::Limit(LimitKind::Heap(Limit::BufferLength)));
                }
                let n = n as usize;
                let out = self.new_u8(n)?;
                for i in 0..n {
                    self.tick()?;
                    let v = self.get_prop(a0, PropertyKey::Index(i as u32))?;
                    let x = self.coerce_number(v)?;
                    self.put_u8(out, i, x);
                }
                Ok(Value::Object(out))
            }
        }
    }

    fn ta_set(&mut self, this: Value, source: Value, offset: Value) -> Res<Value> {
        let (target, _, _, tlen) = self.this_view(this, "%TypedArray%.prototype.set")?;
        let off = self.num_integer(offset)?;
        let oob = |s: &mut Self| s.mk_err(RANGE_ERROR, "offset is out of bounds");
        if off < 0.0 {
            return Err(oob(self));
        }
        if let Value::Object(s) = source
            && let Some((_, _, slen)) = self.heap.view_parts(s)
        {
            if off + slen as f64 > tlen as f64 {
                return Err(oob(self));
            }
            self.work(slen, 6)?;
            let ok = self.heap.view_copy(s, 0, target, off as usize, slen);
            debug_assert!(ok);
            return Ok(Value::Undefined);
        }
        if is_nullish(source) {
            return Err(self.type_err("Cannot convert undefined or null to object"));
        }
        let lv = self.get_prop(source, self.k_length)?;
        let n = self.num_length(lv)?;
        if off + n > tlen as f64 {
            return Err(oob(self));
        }
        let off = off as usize;
        for i in 0..n as usize {
            self.tick()?;
            let v = self.get_prop(source, PropertyKey::Index(i as u32))?;
            let x = self.coerce_number(v)?;
            self.put_u8(target, off + i, x);
        }
        Ok(Value::Undefined)
    }

    // ------------------------------------------------------------ TextDecoder

    fn decoder_construct(&mut self, label: Value, options: Value) -> Res<Value> {
        // WebIDL converts both arguments first; the label lookup (and its RangeError) comes after.
        let lab = if matches!(label, Value::Undefined) { None } else { Some(self.coerce_string(label)?) };
        let dict = self.dict_arg(options, "TextDecoderOptions")?;
        let fatal = self.flag(dict, "fatal")?;
        let ignore_bom = self.flag(dict, "ignoreBOM")?;
        if let Some(s) = lab {
            self.work(h!(self, self.heap.str_len(s)), 4)?;
            match crate::text::label(h!(self, self.heap.str_units(s))) {
                crate::text::Label::Utf8 => {}
                crate::text::Label::Other(_) => return Err(unsupported("TextDecoder encoding other than utf-8", Span::default())),
                crate::text::Label::Invalid => {
                    let t = h!(self, self.heap.to_string_lossy(s));
                    return Err(self.mk_err(RANGE_ERROR, &format!("The \"{t}\" encoding is not supported")));
                }
            }
        }
        Ok(Value::Object(h!(self, self.heap.new_decoder(Some(self.bp().dec), Decoder::new(fatal, ignore_bom)))))
    }

    fn decoder_decode(&mut self, this: Value, input: Value, options: Value) -> Res<Value> {
        self.brand(this, ExoticKind::Decoder, "TextDecoder")?;
        let Value::Object(me) = this else { unreachable!() };
        let src = match input {
            Value::Undefined => None,
            Value::Object(o) if self.heap.bytes(o).is_some() => Some(o),
            _ => return Err(self.type_err("The \"input\" argument must be an instance of ArrayBuffer or ArrayBufferView.")),
        };
        let dict = self.dict_arg(options, "TextDecodeOptions")?;
        let stream = self.flag(dict, "stream")?;
        // All JavaScript has run. Read the state and the bytes now; nothing runs until they are used.
        let mut st = self.heap.decoder(me).ok_or(Error::Heap(HeapError::Stale))?;
        if !st.streaming {
            st.reset();
        }
        st.streaming = stream;
        let bom_at_start = st.bom_seen;
        let len = src.and_then(|o| self.heap.bytes(o)).map_or(0, <[u8]>::len);
        let lim = self.heap.limits();
        let cap = lim.max_string_units.min(lim.max_bytes / 2);
        self.work(len, 4)?;
        let mut out: Vec<u16> = Vec::new();
        out.try_reserve_exact(len.saturating_add(4).min(cap)).map_err(|_| Error::Limit(LimitKind::Heap(Limit::Bytes)))?;
        let bytes = src.and_then(|o| self.heap.bytes(o)).unwrap_or(&[]);
        match crate::text::decode_utf8_bounded(&mut st, bytes, !stream, &mut out, cap) {
            Err(crate::text::DecodeError::OutputLimit) => {
                return Err(Error::Limit(LimitKind::Heap(if cap == lim.max_string_units { Limit::StringLength } else { Limit::Bytes })));
            }
            Err(crate::text::DecodeError::Malformed) => {
                // A fatal error drops this chunk; a newly seen BOM in the failed call does not count.
                st.reset();
                st.bom_seen = bom_at_start;
                st.streaming = stream;
                self.heap.set_decoder(me, st);
                return Err(self.type_err("The encoded data was not valid for encoding utf-8"));
            }
            Ok(()) => {}
        }
        let s = h!(self, self.heap.intern_vec(out));
        self.heap.set_decoder(me, st);
        Ok(Value::String(s))
    }
}
