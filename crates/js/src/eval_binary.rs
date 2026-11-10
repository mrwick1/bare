// Byte buffers, typed arrays, `DataView` and text codecs of the evaluator. Included by `eval.rs` (`include!`), so it shares
// that module's imports and private items. The storage and the integer-indexed property rules are in `heap_binary.rs`; the
// UTF-8 codec is in `text.rs`; the typed-array methods and `DataView` are in `eval_typed.rs`. This file is the setup, the
// dispatch, `ArrayBuffer`, the text codecs and the typed-array constructors.
//
// Supported:
// - `ArrayBuffer(length)`, `.byteLength`, `.maxByteLength`, `.resizable` (always false), `.detached`, `.slice(begin, end)`,
//   `.transfer(length)`, `.transferToFixedLength(length)` (the old buffer is detached: length 0, its views read as empty),
//   `ArrayBuffer.isView`.
// - The ten typed-array classes `Int8Array`, `Uint8Array`, `Uint8ClampedArray`, `Int16Array`, `Uint16Array`, `Int32Array`,
//   `Uint32Array`, `Float32Array`, `Float64Array`, `BigInt64Array`, `BigUint64Array`, each with `BYTES_PER_ELEMENT` on the
//   constructor and the prototype, over the shared `%TypedArray%` (`Object.getPrototypeOf(Uint8Array)`, not a global).
//   Elements stay in the byte buffer in little-endian order; a view is a buffer handle, a byte offset, a length and a type.
//   Constructor forms: `(length)`, `(buffer, byteOffset, length)`, `(typedArray)`, `(iterable)` (the shared iterator protocol
//   of `eval_symbols.rs`: an object whose `@@iterator` is not undefined or null) and `(arrayLike)`.
//   `%TypedArray%.from` (iterables, including generators, array-likes, a map function) and `.of`.
//   `%TypedArray%.prototype`: `buffer`, `byteLength`, `byteOffset`, `length`, `at`, `copyWithin`, `entries`, `every`,
//   `fill`, `filter`, `find`, `findIndex`, `findLast`, `findLastIndex`, `forEach`, `includes`, `indexOf`, `join`, `keys`,
//   `lastIndexOf`, `map`, `reduce`, `reduceRight`, `reverse`, `set`, `slice`, `some`, `sort`, `subarray`, `toLocaleString`,
//   `toReversed`, `toSorted`, `toString` (the function of `Array.prototype.toString`), `values`, `with`, `[@@iterator]` and
//   the `[@@toStringTag]` getter. Integer indexing (read, write, `in`, `delete`, `Object.keys`,
//   `Object.getOwnPropertyDescriptor`, `defineProperty`) and iteration.
// - `DataView(buffer, byteOffset, byteLength)`, `buffer`, `byteLength`, `byteOffset`, and `get` and `set` for `Int8`, `Uint8`,
//   `Int16`, `Uint16`, `Int32`, `Uint32`, `Float32` and `Float64`, with the `littleEndian` flag.
// - `TextEncoder` (`encoding`, `encode`, `encodeInto`) and `TextDecoder` (`encoding`, `fatal`, `ignoreBOM`, `decode` with
//   `stream`). UTF-8 only. A known WHATWG label of another encoding is `Unsupported`; an unknown label is a `RangeError`.
// Gates (explicit `Unsupported`, never silent): resizable `ArrayBuffer` options and `resize`; a value that is not a BigInt
// written to a `BigInt64Array` or `BigUint64Array` element by a path that calls `heap.set` or `heap.define_own_property`
// directly (`put_prop`, `Reflect.set` and `Object.defineProperty` convert it first).
// `slice`, `subarray`, `map` and `filter` read `constructor` and `@@species`: `undefined`, `null` and the intrinsic class
// give the default; any other constructor is `Unsupported`. A class that extends one of these constructors is `Unsupported`
// when it is constructed. Absent: `SharedArrayBuffer`.
//
// Limits: backing bytes are capped for one buffer and for all live buffers (`Interp::set_binary_limits`; errors
// `Heap(BufferLength)` and `Heap(BufferBytes)`), every size is checked before the allocation or the copy, and every
// pass over bytes or elements consumes steps (one step for 64 bytes copied, filled, sorted or searched, or 16 bytes encoded
// or decoded, one for each element read through a JavaScript property or passed to a callback). A limit is never a
// JavaScript exception.
//
// GC: the buffer bytes are owned by the heap object and are freed by `collect`. A view marks its buffer. The prototypes and
// the constructors are roots.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BinFn {
    AbCtor,
    AbIsView,
    AbByteLength,
    AbMaxByteLength,
    AbResizable,
    AbDetached,
    AbSlice,
    /// `transfer` (false) and `transferToFixedLength` (true). The buffers here are never resizable, so both do the same.
    AbTransfer(bool),
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
    TaFrom,
    TaOf,
    /// A `%TypedArray%.prototype` method of `eval_typed.rs`.
    Ta(TaM),
    /// The constructor of one typed-array class.
    TyCtor(Elem),
    DvCtor,
    DvBuffer,
    DvByteLength,
    DvByteOffset,
    DvGet(Elem),
    DvSet(Elem),
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

/// The `%TypedArray%.prototype` methods of `eval_typed.rs` (the others have their own `BinFn`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TaM {
    At,
    CopyWithin,
    Every,
    Fill,
    Filter,
    Find,
    FindIndex,
    FindLast,
    FindLastIndex,
    ForEach,
    Includes,
    Join,
    LastIndexOf,
    Map,
    Reduce,
    ReduceRight,
    Reverse,
    Any,
    Sort,
    ToLocaleString,
    ToReversed,
    ToSorted,
    With,
}

/// The method table of `%TypedArray%.prototype`: name, function, `length`, the qualified name for messages, and whether V8
/// words a wrong receiver as "this is not a typed array." (`true`) or "Method ... called on incompatible receiver" (`false`).
const TA_METHODS: [(&str, TaM, u32, &str, bool); 23] = [
    ("at", TaM::At, 1, "%TypedArray%.prototype.at", false), ("copyWithin", TaM::CopyWithin, 2, "%TypedArray%.prototype.copyWithin", true),
    ("every", TaM::Every, 1, "%TypedArray%.prototype.every", true), ("fill", TaM::Fill, 1, "%TypedArray%.prototype.fill", true),
    ("filter", TaM::Filter, 1, "%TypedArray%.prototype.filter", true), ("find", TaM::Find, 1, "%TypedArray%.prototype.find", true),
    ("findIndex", TaM::FindIndex, 1, "%TypedArray%.prototype.findIndex", true), ("findLast", TaM::FindLast, 1, "%TypedArray%.prototype.findLast", true),
    ("findLastIndex", TaM::FindLastIndex, 1, "%TypedArray%.prototype.findLastIndex", true), ("forEach", TaM::ForEach, 1, "%TypedArray%.prototype.forEach", true),
    ("includes", TaM::Includes, 1, "%TypedArray%.prototype.includes", true), ("join", TaM::Join, 1, "%TypedArray%.prototype.join", false),
    ("lastIndexOf", TaM::LastIndexOf, 1, "%TypedArray%.prototype.lastIndexOf", true), ("map", TaM::Map, 1, "%TypedArray%.prototype.map", true),
    ("reduce", TaM::Reduce, 1, "%TypedArray%.prototype.reduce", true), ("reduceRight", TaM::ReduceRight, 1, "%TypedArray%.prototype.reduceRight", true),
    ("reverse", TaM::Reverse, 0, "%TypedArray%.prototype.reverse", true), ("some", TaM::Any, 1, "%TypedArray%.prototype.some", true),
    ("sort", TaM::Sort, 1, "%TypedArray%.prototype.sort", false), ("toLocaleString", TaM::ToLocaleString, 0, "%TypedArray%.prototype.toLocaleString", false),
    ("toReversed", TaM::ToReversed, 0, "%TypedArray%.prototype.toReversed", false), ("toSorted", TaM::ToSorted, 1, "%TypedArray%.prototype.toSorted", false),
    ("with", TaM::With, 2, "%TypedArray%.prototype.with", true),
];

impl TaM {
    /// The qualified name and the receiver wording of the method.
    fn info(self) -> (&'static str, bool) {
        let e = &TA_METHODS[self as usize]; // the table has the order of the enum
        debug_assert!(e.1 == self);
        (e.3, e.4)
    }
}

impl BinFn {
    fn ctor_name(self) -> Option<&'static str> {
        Some(match self {
            BinFn::AbCtor => "ArrayBuffer",
            BinFn::TyCtor(t) => t.name(),
            BinFn::DvCtor => "DataView",
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
    /// The constructor of `%TypedArray%`.
    ta_c: ObjId,
    dv: ObjId,
    enc: ObjId,
    dec: ObjId,
    /// The intrinsic `ArrayBuffer` and typed-array classes: the defaults of `SpeciesConstructor`.
    ab_c: ObjId,
    /// The prototype and the constructor of each typed-array class, in the order of `TYPED`.
    ty: [ObjId; 11],
    ty_c: [ObjId; 11],
}

#[derive(Default)]
struct BinaryState {
    protos: Option<BinProtos>,
}

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
        let (ab, u8p) = (self.bp().ab, self.bin_proto(Elem::U8));
        let len = bytes.len();
        let buf = h!(self, self.heap.new_buffer_from(Some(ab), bytes));
        Ok(Value::Object(h!(self, self.heap.new_view(Some(u8p), buf, 0, len))))
    }

    /// A new typed array of type `ty` over a new buffer that owns `bytes` (no copy). `bytes.len()` is a multiple of the element size.
    pub fn new_typed_array(&mut self, ty: Elem, bytes: Vec<u8>) -> Result<Value, Error> {
        let (ab, p) = (self.bp().ab, self.bin_proto(ty));
        let len = bytes.len() / ty.size();
        let buf = h!(self, self.heap.new_buffer_from(Some(ab), bytes));
        Ok(Value::Object(h!(self, self.heap.new_typed_view(Some(p), buf, 0, len, ty))))
    }

    /// The bytes of an `ArrayBuffer`, a typed array or a `DataView` value, borrowed with no copy. `None` for any other value.
    pub fn bytes_of(&self, v: Value) -> Option<&[u8]> {
        let Value::Object(o) = v else { return None };
        self.heap.bytes(o)
    }

    // ------------------------------------------------------------ setup

    fn bp(&self) -> BinProtos {
        self.bin.protos.expect("install_binary ran")
    }

    /// The prototype of a typed-array class (or `DataView`).
    fn bin_proto(&self, ty: Elem) -> ObjId {
        let p = self.bin.protos.as_ref().expect("install_binary ran");
        if ty == Elem::Data { p.dv } else { p.ty[ty.index()] }
    }

    /// The intrinsic constructor of a typed-array class.
    fn bin_class(&self, ty: Elem) -> ObjId {
        self.bin.protos.as_ref().expect("install_binary ran").ty_c[ty.index()]
    }

    fn binary_roots(&self, out: &mut Vec<Value>) {
        if let Some(p) = &self.bin.protos {
            out.extend([p.ab, p.ta, p.ta_c, p.dv, p.enc, p.dec, p.ab_c].into_iter().chain(p.ty).chain(p.ty_c).map(Value::Object));
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
        self.bin_getter_named(on, name, &format!("get {name}"), f, enumerable)
    }

    fn bin_getter_named(&mut self, on: ObjId, name: &str, fname: &str, f: BinFn, enumerable: bool) -> Res<()> {
        let g = self.native(Nat::Bin(f), fname, 0)?;
        let k = h!(self, self.heap.key_from_str(name));
        let d = PropertyDescriptor { get: Some(Some(g)), set: Some(None), enumerable: Some(enumerable), configurable: Some(true), ..Default::default() };
        h!(self, self.heap.define_own_property(on, k, &d));
        Ok(())
    }

    fn install_binary(&mut self) -> Res<()> {
        let op = self.object_proto;
        let proto = |s: &mut Self, parent: ObjId| -> Res<ObjId> { Ok(h!(s, s.heap.new_object(Some(parent)))) };
        let (ab_p, ta_p, dv_p) = (proto(self, op)?, proto(self, op)?, proto(self, op)?);
        let mut ty_p = [ab_p; 11];
        for p in &mut ty_p {
            *p = proto(self, ta_p)?;
        }
        let (enc_p, dec_p) = (proto(self, op)?, proto(self, op)?);
        self.bin.protos = Some(BinProtos { ab: ab_p, ta: ta_p, ta_c: ta_p, dv: dv_p, enc: enc_p, dec: dec_p, ab_c: ab_p, ty: ty_p, ty_c: ty_p });

        let species = PropertyKey::Symbol(self.sc.wk[WK_SPECIES]);
        let ab = self.bin_ctor("ArrayBuffer", BinFn::AbCtor, 1, ab_p)?;
        self.sc_getter(ab, species, "get [Symbol.species]", ScFn::Species)?;
        self.sc_tag(ab_p, "ArrayBuffer")?;
        self.bin_global("ArrayBuffer", ab)?;
        self.bin_method(ab, "isView", BinFn::AbIsView, 1)?;
        self.bin_getter(ab_p, "byteLength", BinFn::AbByteLength, false)?;
        self.bin_method(ab_p, "slice", BinFn::AbSlice, 2)?;
        self.bin_getter(ab_p, "maxByteLength", BinFn::AbMaxByteLength, false)?;
        self.bin_getter(ab_p, "resizable", BinFn::AbResizable, false)?;
        self.bin_method(ab_p, "resize", BinFn::Gate("ArrayBuffer.prototype.resize"), 1)?;
        self.bin_method(ab_p, "transfer", BinFn::AbTransfer(false), 0)?;
        self.bin_method(ab_p, "transferToFixedLength", BinFn::AbTransfer(true), 0)?;
        self.bin_getter(ab_p, "detached", BinFn::AbDetached, false)?;

        // %TypedArray%: the shared constructor and prototype. Not a global.
        let ta = self.bin_ctor("TypedArray", BinFn::TaCtor, 0, ta_p)?;
        if let Some(p) = &mut self.bin.protos {
            p.ta_c = ta;
        }
        self.bin_getter_named(ta_p, "buffer", "get buffer", BinFn::TaBuffer, false)?;
        for (n, f) in [("byteLength", BinFn::TaByteLength), ("byteOffset", BinFn::TaByteOffset), ("length", BinFn::TaLength)] {
            self.bin_getter(ta_p, n, f, false)?;
        }
        self.bin_method(ta_p, "set", BinFn::TaSet, 1)?;
        self.bin_method(ta_p, "subarray", BinFn::TaSubarray, 2)?;
        self.bin_method(ta_p, "slice", BinFn::TaSlice, 2)?;
        self.bin_method(ta_p, "indexOf", BinFn::TaIndexOf, 1)?;
        for (n, m, len, _, _) in TA_METHODS {
            self.bin_method(ta_p, n, BinFn::Ta(m), len)?;
        }
        // `values` is `[Symbol.iterator]`; the iterator is the shared array iterator of `eval_symbols.rs`.
        let tv = self.native(Nat::Bin(BinFn::TaValues), "values", 0)?;
        let kv = h!(self, self.heap.key_from_str("values"));
        self.def(ta_p, kv, Value::Object(tv), true, false, true)?;
        self.def(ta_p, self.sc.k_iterator, Value::Object(tv), true, false, true)?;
        self.bin_method(ta_p, "keys", BinFn::TaKeys, 0)?;
        self.bin_method(ta_p, "entries", BinFn::TaEntries, 0)?;
        // `toString` is the very function of `Array.prototype.toString`.
        let kts = h!(self, self.heap.key_from_str("toString"));
        if let Some(Property::Data { value, .. }) = h!(self, self.heap.get_own_property(self.array_proto, kts)) {
            self.def(ta_p, kts, value, true, false, true)?;
        }
        let tag = self.native(Nat::Bin(BinFn::TaTag), "get [Symbol.toStringTag]", 0)?;
        let d = PropertyDescriptor { get: Some(Some(tag)), set: Some(None), enumerable: Some(false), configurable: Some(true), ..Default::default() };
        h!(self, self.heap.define_own_property(ta_p, self.sc.k_to_string_tag, &d));
        self.sc_getter(ta, species, "get [Symbol.species]", ScFn::Species)?;
        self.bin_method(ta, "from", BinFn::TaFrom, 1)?;
        self.bin_method(ta, "of", BinFn::TaOf, 0)?;

        let k = h!(self, self.heap.key_from_str("BYTES_PER_ELEMENT"));
        let mut classes = [ab; 11];
        for ty in TYPED {
            let c = self.bin_ctor(ty.name(), BinFn::TyCtor(ty), 3, ty_p[ty.index()])?;
            h!(self, self.heap.set_prototype_of(c, Some(ta)));
            self.def(c, k, Value::Number(ty.size() as f64), false, false, false)?;
            self.def(ty_p[ty.index()], k, Value::Number(ty.size() as f64), false, false, false)?;
            self.bin_global(ty.name(), c)?;
            classes[ty.index()] = c;
        }
        if let Some(p) = &mut self.bin.protos {
            p.ab_c = ab;
            p.ty_c = classes;
        }

        let dv = self.bin_ctor("DataView", BinFn::DvCtor, 1, dv_p)?;
        self.bin_global("DataView", dv)?;
        self.sc_tag(dv_p, "DataView")?;
        for (n, f) in [("buffer", BinFn::DvBuffer), ("byteLength", BinFn::DvByteLength), ("byteOffset", BinFn::DvByteOffset)] {
            self.bin_getter(dv_p, n, f, false)?;
        }
        for ty in TYPED {
            let suffix = ty.name().trim_end_matches("Array");
            if ty == Elem::U8C {
                continue;
            }
            self.bin_method(dv_p, &format!("get{suffix}"), BinFn::DvGet(ty), 1)?;
            self.bin_method(dv_p, &format!("set{suffix}"), BinFn::DvSet(ty), 2)?;
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
        let t = self.recv_text(this);
        self.type_err(&format!("Method {what} called on incompatible receiver {t}"))
    }

    /// The typed array or `DataView` behind `this`: `(object, buffer, byte offset, length)`. A `DataView` is not a typed
    /// array: it fails here with the others.
    fn this_view(&mut self, this: Value, what: &str) -> Res<(ObjId, ObjId, usize, usize)> {
        if let Value::Object(o) = this
            && let Some((b, off, len)) = self.heap.view_parts(o)
            && self.heap.view_type(o) != Some(Elem::Data)
        {
            return Ok((o, b, off, len));
        }
        Err(self.incompatible(what, this))
    }

    /// `SpeciesConstructor(o, default)`: reads `constructor`, then `constructor[@@species]` (each once). `undefined`, `null`
    /// and the intrinsic `default` give `None`. Another constructor is given back for the caller to call. A value that is
    /// not a constructor is a `TypeError`.
    fn bin_species(&mut self, o: ObjId, default: ObjId) -> Res<Option<ObjId>> {
        let c = match self.get_prop(Value::Object(o), self.k_constructor)? {
            Value::Undefined => return Ok(None),
            c @ Value::Object(_) => c,
            _ => return Err(self.type_err("The .constructor property is not an object")),
        };
        match self.get_prop(c, self.wk_key(WK_SPECIES))? {
            Value::Undefined | Value::Null => Ok(None),
            Value::Object(s) if s == default => Ok(None),
            Value::Object(s) if self.is_constructor(s) => Ok(Some(s)),
            _ => Err(self.type_err("object.constructor[Symbol.species] is not a constructor")),
        }
    }

    /// A new typed array of `len` elements over a new zeroed buffer.
    fn new_typed(&mut self, ty: Elem, len: usize) -> Res<ObjId> {
        let (ab, tp) = (self.bp().ab, self.bin_proto(ty));
        let Some(bytes) = len.checked_mul(ty.size()) else { return Err(Error::Limit(LimitKind::Heap(Limit::BufferLength))) };
        let buf = h!(self, self.heap.new_buffer(Some(ab), bytes));
        Ok(h!(self, self.heap.new_typed_view(Some(tp), buf, 0, len, ty)))
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
        match f {
            Gate(what) => Err(unsupported(what, Span::default())),
            AbCtor => {
                let nt = self.cls.native_nt.take();
                let len = self.num_index(a0, "Invalid array buffer length")?;
                if let Value::Object(_) = a1 {
                    let k = h!(self, self.heap.key_from_str("maxByteLength"));
                    if !matches!(self.get_prop(a1, k)?, Value::Undefined) {
                        // A feature probe may catch this refusal; it must not stop the page.
                        return Err(self.type_err("resizable ArrayBuffer is not supported"));
                    }
                }
                let proto = self.nt_proto(nt, self.bp().ab)?;
                Ok(Value::Object(h!(self, self.heap.new_buffer(Some(proto), len))))
            }
            AbIsView => Ok(Value::Bool(matches!(a0, Value::Object(o) if self.heap.exotic_kind(o) == Some(ExoticKind::View)))),
            AbByteLength | AbMaxByteLength => {
                let what = if f == AbByteLength { "get ArrayBuffer.prototype.byteLength" } else { "get ArrayBuffer.prototype.maxByteLength" };
                match this {
                    Value::Object(o) if let Some(n) = self.heap.buffer_len(o) => Ok(Value::Number(n as f64)),
                    _ => Err(self.incompatible(what, this)),
                }
            }
            AbResizable | AbDetached => {
                let what = if f == AbResizable { "get ArrayBuffer.prototype.resizable" } else { "get ArrayBuffer.prototype.detached" };
                match this {
                    Value::Object(o) if self.heap.buffer_len(o).is_some() => Ok(Value::Bool(f == AbDetached && self.heap.buffer_detached(o))),
                    _ => Err(self.incompatible(what, this)),
                }
            }
            AbSlice => {
                let Value::Object(o) = this else { return Err(self.incompatible("ArrayBuffer.prototype.slice", this)) };
                let Some(len) = self.heap.buffer_len(o) else { return Err(self.incompatible("ArrayBuffer.prototype.slice", this)) };
                if self.heap.buffer_detached(o) {
                    return Err(self.type_err("Cannot perform ArrayBuffer.prototype.slice on a detached ArrayBuffer"));
                }
                let first = self.num_integer(a0)?;
                let first = Self::rel_index(first, len);
                let last = if matches!(a1, Value::Undefined) { len } else { let e = self.num_integer(a1)?; Self::rel_index(e, len) };
                let ab = self.bp();
                let species = self.bin_species(o, ab.ab_c)?;
                if self.heap.buffer_detached(o) {
                    return Err(self.type_err("Cannot perform ArrayBuffer.prototype.slice on a detached ArrayBuffer"));
                }
                let last = last.max(first);
                self.work(last - first, 6)?;
                let Some(c) = species else { return Ok(Value::Object(h!(self, self.heap.new_buffer_copy(Some(ab.ab), o, first, last)))) };
                let n = self.ao_construct(Value::Object(c), &[Value::Number((last - first) as f64)])?;
                if self.heap.exotic_kind(n) != Some(ExoticKind::Buffer) {
                    return Err(self.incompatible("ArrayBuffer.prototype.slice", Value::Object(n)));
                }
                if self.heap.buffer_detached(n) {
                    return Err(self.type_err("Cannot perform ArrayBuffer.prototype.slice on a detached ArrayBuffer"));
                }
                if n == o {
                    return Err(self.type_err("ArrayBuffer subclass returned this from species constructor"));
                }
                if self.heap.buffer_len(n).is_none_or(|l| l < last - first) {
                    return Err(self.type_err("Derived ArrayBuffer constructor created a buffer which was too small"));
                }
                let bytes = self.heap.bytes(o).and_then(|b| b.get(first..last)).map(<[u8]>::to_vec).unwrap_or_default();
                if let Some(dst) = self.heap.bytes_mut(n) {
                    dst[..bytes.len()].copy_from_slice(&bytes);
                }
                Ok(Value::Object(n))
            }
            AbTransfer(fixed) => self.ab_transfer(this, a0, fixed),
            TaCtor => Err(self.type_err("Abstract class TypedArray not directly constructable")),
            TaBuffer => {
                let (_, b, _, _) = self.this_view(this, "get %TypedArray%.prototype.buffer")?;
                Ok(Value::Object(b))
            }
            TaByteLength => {
                let (o, _, _, len) = self.this_view(this, "get TypedArray.prototype.byteLength")?;
                let ty = self.heap.view_type(o).expect("view");
                Ok(Value::Number(if self.heap.view_detached(o) { 0.0 } else { (len * ty.size()) as f64 }))
            }
            TaLength => {
                let (o, _, _, len) = self.this_view(this, "get TypedArray.prototype.length")?;
                Ok(Value::Number(if self.heap.view_detached(o) { 0.0 } else { len as f64 }))
            }
            TaByteOffset => {
                let (o, _, off, _) = self.this_view(this, "get TypedArray.prototype.byteOffset")?;
                Ok(Value::Number(if self.heap.view_detached(o) { 0.0 } else { off as f64 }))
            }
            TyCtor(ty) => {
                let nt = self.cls.native_nt.take();
                let v = self.ty_construct(ty, a0, a1, a2)?;
                self.nt_patch(nt, self.bin_proto(ty), v)
            }
            TaFrom => self.ta_from(this, a0, a1, a2),
            TaOf => self.ta_of(this, args),
            Ta(m) => self.ta_method(m, this, args),
            DvCtor => {
                let nt = self.cls.native_nt.take();
                let v = self.dv_construct(a0, a1, a2)?;
                self.nt_patch(nt, self.bp().dv, v)
            }
            DvBuffer | DvByteLength | DvByteOffset => self.dv_getter(f, this),
            DvGet(ty) => self.dv_get(ty, this, a0, a1),
            DvSet(ty) => self.dv_set(ty, this, a0, a1, a2),
            TaValues | TaKeys | TaEntries => {
                let (what, w) = match f {
                    TaKeys => (IterWhat::Keys, "%TypedArray%.prototype.keys"),
                    TaEntries => (IterWhat::Entries, "%TypedArray%.prototype.entries"),
                    _ => (IterWhat::Values, "%TypedArray%.prototype.values"),
                };
                self.this_ta(this, w, true)?;
                self.sc_array_iter(this, what)
            }
            TaTag => match this {
                Value::Object(o) => match self.heap.view_type(o) {
                    Some(ty) if ty != Elem::Data => self.str_value(ty.name()),
                    _ => Ok(Value::Undefined),
                },
                _ => Ok(Value::Undefined),
            },
            TaSet => self.ta_set(this, a0, a1),
            TaIndexOf => self.ta_search(Search::IndexOf, this, args),
            TaSubarray => self.ta_subarray(this, a0, a1),
            TaSlice => self.ta_slice(this, a0, a1),
            EncCtor => {
                let nt = self.cls.native_nt.take();
                let proto = self.nt_proto(nt, self.bp().enc)?;
                Ok(Value::Object(h!(self, self.heap.new_encoder(Some(proto)))))
            }
            EncEncoding => self.brand(this, ExoticKind::Encoder, "TextEncoder").and_then(|_| self.str_value("utf-8")),
            EncEncode => {
                self.brand(this, ExoticKind::Encoder, "TextEncoder")?;
                let s = if matches!(a0, Value::Undefined) { h!(self, self.heap.intern_str("")) } else { self.coerce_string(a0)? };
                let n = crate::text::utf8_len(h!(self, self.heap.str_units(s)));
                self.work(n, 4)?;
                let (ab, u8p) = (self.bp().ab, self.bin_proto(Elem::U8));
                let buf = h!(self, self.heap.new_buffer(Some(ab), n));
                h!(self, self.heap.encode_utf8_into(s, buf));
                Ok(Value::Object(h!(self, self.heap.new_view(Some(u8p), buf, 0, n))))
            }
            EncEncodeInto => {
                self.brand(this, ExoticKind::Encoder, "TextEncoder")?;
                let s = self.coerce_string(a0)?;
                let dest = match a1 {
                    Value::Object(d) if self.heap.view_type(d) == Some(Elem::U8) => d,
                    other => {
                        let found = match other {
                            Value::Object(d) => match self.heap.view_type(d) {
                                Some(ty) => format!(" Received an instance of {}", ty.name()),
                                None if self.heap.buffer_len(d).is_some() => " Received an instance of ArrayBuffer".into(),
                                None => String::new(),
                            },
                            _ => String::new(),
                        };
                        return Err(self.type_err(&format!("The \"dest\" argument must be an instance of Uint8Array.{found}")));
                    }
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
            DecCtor => {
                let nt = self.cls.native_nt.take();
                let v = self.decoder_construct(a0, a1)?;
                self.nt_patch(nt, self.bp().dec, v)
            }
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

    /// `ArrayBuffer.prototype.transfer` and `transferToFixedLength`: the bytes move to a new buffer, cut or zero-padded to
    /// the new length (default: the old length). The old buffer is detached.
    fn ab_transfer(&mut self, this: Value, new_len: Value, fixed: bool) -> Res<Value> {
        let what = if fixed { "ArrayBuffer.prototype.transferToFixedLength" } else { "ArrayBuffer.prototype.transfer" };
        let Value::Object(o) = this else { return Err(self.incompatible(what, this)) };
        let Some(old) = self.heap.buffer_len(o) else { return Err(self.incompatible(what, this)) };
        let n = if matches!(new_len, Value::Undefined) {
            old
        } else {
            let x = self.num_integer(new_len)?;
            if x < 0.0 {
                return Err(self.mk_err(RANGE_ERROR, "Invalid array buffer length"));
            }
            if x > MAX_SAFE {
                return Err(self.mk_err(RANGE_ERROR, &format!("{what}: Invalid length parameter")));
            }
            x as usize
        };
        if self.heap.buffer_detached(o) {
            return Err(self.type_err(&format!("Cannot perform {what} on a detached ArrayBuffer")));
        }
        self.work(old.min(n), 6)?;
        Ok(Value::Object(h!(self, self.heap.buffer_transfer(Some(self.bp().ab), o, n))))
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
