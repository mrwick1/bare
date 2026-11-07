//! Byte buffers, typed-array views, `DataView`s and text-codec state, as exotic objects of the heap.
//! A child module of `heap.rs` (`#[path]`), so it uses the heap's private slabs.
//!
//! Layout: an exotic object is an ordinary `Object` with its `exotic` flag set. Its payload lives in
//! `Heap::exotics`, a map from the object's slot index to an [`Exotic`]. So ordinary objects pay one
//! flag and no pointer. Bytes are never one property per element: a buffer owns one `Vec<u8>`, a view owns
//! three numbers, an element type and a handle of its buffer. A `DataView` is a view of the type `Elem::Data`: it has
//! no integer-indexed properties.
//!
//! Elements stay in the buffer in little-endian byte order (all supported targets are little-endian). `load` and `store`
//! convert one element with the exact ECMAScript rules (ToInt32 wrap, round-half-even for `Uint8ClampedArray`, `f32`
//! rounding, canonical NaN). A read is one hash lookup for the view and one for its buffer; there is no per-element value.
//!
//! Rules:
//! - A view keeps its buffer alive (the collector marks the edge). A dead buffer frees its bytes at the
//!   next `collect`, together with its map entry.
//! - Bytes count against `max_bytes` and against two buffer caps (`set_buffer_limits`): one buffer and all live
//!   buffers together. Both are checked before the allocation.
//! - A buffer that is detached (`buffer_transfer`) frees its bytes at once and stays an `ArrayBuffer` of length 0. A view
//!   of it reads as empty: no valid index, no bytes. `bytes` and `bytes_mut` of such a view are empty slices.
//! - Integer-indexed keys of a view follow the spec: a valid index is a writable, enumerable, configurable data
//!   property; an invalid index (out of range, or a canonical numeric string such as `"-0"`, `"1.5"`, `"NaN"`) is
//!   absent, ends a prototype walk, ignores writes and cannot be defined or deleted.

use super::*;

/// What an exotic object holds.
pub(super) enum Exotic {
    Buffer(Vec<u8>),
    /// A buffer whose bytes were moved away (`ArrayBuffer.prototype.transfer`). Length 0.
    Detached,
    /// `buffer` is always a buffer object. `offset` is in bytes and `len` in elements of `ty` (bytes for `Elem::Data`).
    /// `offset + len * size <= buffer length` while the buffer is not detached.
    View { buffer: ObjId, offset: usize, len: usize, ty: Elem },
    Encoder,
    Decoder(Decoder),
    /// An object whose array-index properties are read from the host each time (see `IndexedSource`). It holds no data.
    Indexed,
    /// An object whose string-keyed properties come from the host (see `heap_named.rs`). It holds no data.
    Named,
    /// A primitive wrapper object: its string, number, boolean or symbol (see `heap_prim.rs`).
    Prim(Value),
    /// A `Proxy` (see `heap_proxy.rs`): `(target, handler)`, or `None` after `revoke`.
    Proxy(Option<(ObjId, ObjId)>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExoticKind {
    Buffer,
    View,
    Encoder,
    Decoder,
    Indexed,
    Named,
    /// A String, Number, Boolean or Symbol wrapper object.
    Primitive,
    Proxy,
}

/// The element type of a view. `Data` is a `DataView` (bytes, no integer-indexed properties).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Elem {
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
    Data,
}

/// The ten typed-array types, in the order of `Elem::index`.
pub const TYPED: [Elem; 11] = [Elem::I8, Elem::U8, Elem::U8C, Elem::I16, Elem::U16, Elem::I32, Elem::U32, Elem::F32, Elem::F64, Elem::BI64, Elem::BU64];

impl Elem {
    pub fn size(self) -> usize {
        match self {
            Elem::I8 | Elem::U8 | Elem::U8C | Elem::Data => 1,
            Elem::I16 | Elem::U16 => 2,
            Elem::I32 | Elem::U32 | Elem::F32 => 4,
            Elem::F64 | Elem::BI64 | Elem::BU64 => 8,
        }
    }

    /// The `BigInt64Array` and `BigUint64Array` types: their elements are BigInts, not numbers (`view_load_big`, `view_store_big`).
    pub fn is_big(self) -> bool {
        matches!(self, Elem::BI64 | Elem::BU64)
    }

    pub fn is_float(self) -> bool {
        matches!(self, Elem::F32 | Elem::F64)
    }

    /// The place of the type in `TYPED`.
    pub fn index(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        match self {
            Elem::I8 => "Int8Array",
            Elem::U8 => "Uint8Array",
            Elem::U8C => "Uint8ClampedArray",
            Elem::I16 => "Int16Array",
            Elem::U16 => "Uint16Array",
            Elem::I32 => "Int32Array",
            Elem::U32 => "Uint32Array",
            Elem::F32 => "Float32Array",
            Elem::F64 => "Float64Array",
            Elem::BI64 => "BigInt64Array",
            Elem::BU64 => "BigUint64Array",
            Elem::Data => "DataView",
        }
    }
}

/// Read one element from the first `ty.size()` bytes of `b`, little-endian. The BigInt types give a lossy number: no
/// caller may show it (a BigInt element is read with `view_load_big`).
#[inline]
pub fn load(ty: Elem, b: &[u8]) -> f64 {
    match ty {
        Elem::I8 => b[0] as i8 as f64,
        Elem::U8 | Elem::U8C | Elem::Data => b[0] as f64,
        Elem::I16 => i16::from_le_bytes([b[0], b[1]]) as f64,
        Elem::U16 => u16::from_le_bytes([b[0], b[1]]) as f64,
        Elem::I32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
        Elem::U32 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
        Elem::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
        Elem::F64 => f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        Elem::BI64 => i64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f64,
        Elem::BU64 => u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f64,
    }
}

/// Convert the number `n` to the type and write it to the first `ty.size()` bytes of `b`, little-endian. The integer types
/// wrap (ToInt8 .. ToUint32 are the low bits of ToUint32). `Uint8ClampedArray` clamps and rounds half to even. `Float32`
/// rounds to nearest even. A NaN is written in its canonical form. The BigInt types write nothing (a BigInt element is written with `view_store_big`).
#[inline]
pub fn store(ty: Elem, b: &mut [u8], n: f64) {
    match ty {
        Elem::I8 | Elem::U8 | Elem::Data => b[0] = to_uint32(n) as u8,
        Elem::U8C => b[0] = if n >= 255.0 { 255 } else if n > 0.0 { n.round_ties_even() as u8 } else { 0 },
        Elem::I16 | Elem::U16 => b[..2].copy_from_slice(&(to_uint32(n) as u16).to_le_bytes()),
        Elem::I32 | Elem::U32 => b[..4].copy_from_slice(&to_uint32(n).to_le_bytes()),
        Elem::F32 => b[..4].copy_from_slice(&(if n.is_nan() { f32::NAN } else { n as f32 }).to_le_bytes()),
        Elem::F64 => b[..8].copy_from_slice(&(if n.is_nan() { f64::NAN } else { n }).to_le_bytes()),
        Elem::BI64 | Elem::BU64 => {}
    }
}

fn nan_last(a: bool, b: bool) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (true, true) => Some(std::cmp::Ordering::Equal),
        (true, false) => Some(std::cmp::Ordering::Greater),
        (false, true) => Some(std::cmp::Ordering::Less),
        _ => None,
    }
}

/// Sort the elements of `b` (whole elements of `ty`) in ascending numeric order: `-0` before `+0`, NaN last. Works on a
/// temporary copy of the elements in their own type (no per-element JavaScript value).
pub fn sort_elems(ty: Elem, b: &mut [u8]) {
    macro_rules! go {
        ($t:ty, $n:expr, $cmp:expr) => {{
            let mut v: Vec<$t> = b.chunks_exact($n).map(|c| <$t>::from_le_bytes(c.try_into().unwrap())).collect();
            v.sort_unstable_by($cmp);
            for (c, x) in b.chunks_exact_mut($n).zip(v) {
                c.copy_from_slice(&x.to_le_bytes());
            }
        }};
    }
    match ty {
        Elem::I8 => go!(i8, 1, |a, b| a.cmp(b)),
        Elem::U8 | Elem::U8C | Elem::Data => b.sort_unstable(),
        Elem::I16 => go!(i16, 2, |a, b| a.cmp(b)),
        Elem::U16 => go!(u16, 2, |a, b| a.cmp(b)),
        Elem::I32 => go!(i32, 4, |a, b| a.cmp(b)),
        Elem::U32 => go!(u32, 4, |a, b| a.cmp(b)),
        Elem::F32 => go!(f32, 4, |a, b| nan_last(a.is_nan(), b.is_nan()).unwrap_or_else(|| a.total_cmp(b))),
        Elem::F64 => go!(f64, 8, |a, b| nan_last(a.is_nan(), b.is_nan()).unwrap_or_else(|| a.total_cmp(b))),
        Elem::BI64 => go!(i64, 8, |a, b| a.cmp(b)),
        Elem::BU64 => go!(u64, 8, |a, b| a.cmp(b)),
    }
}

/// What a host says about one index of a host-indexed object right now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    /// No property at this index (it is at or after the length).
    Absent,
    /// A property whose value is this object.
    Ready(ObjId),
    /// A property whose JavaScript object does not exist yet. The heap answers `NeedsHost`; the host makes the object.
    Missing,
}

/// The read side of a host-indexed object (`Heap::new_indexed`). The host keeps the truth; the heap keeps no copy of the
/// length or of the elements, so the answers are live. Both methods must be cheap, must not allocate JavaScript objects,
/// and must not run JavaScript. They get `&self`: an answer cannot change anything. `owner` is the object that was asked,
/// which can be a prototype of the object that the page touched.
pub trait IndexedSource {
    /// How many indices are properties of `owner` now. `Ok(0)`: `owner` is not a live object of this host. `Err`: the host
    /// cannot answer now (for example its state is borrowed). The error stops the script; it is never an empty answer.
    fn indexed_len(&self, owner: ObjId) -> Result<usize, HeapError>;
    /// The property at `index` of `owner` now. `Err` as for `indexed_len`.
    fn indexed_item(&self, owner: ObjId, index: usize) -> Result<Slot, HeapError>;
}

/// State of a UTF-8 `TextDecoder`: the options, and the WHATWG decoder registers (a partly read sequence is at most
/// 3 bytes, kept as `cp`, `needed`, `seen` and the two boundaries). It never grows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Decoder {
    pub fatal: bool,
    pub ignore_bom: bool,
    pub bom_seen: bool,
    /// The previous `decode` call had `stream: true`.
    pub streaming: bool,
    pub cp: u32,
    pub needed: u8,
    pub seen: u8,
    pub lower: u8,
    pub upper: u8,
}

impl Decoder {
    pub fn new(fatal: bool, ignore_bom: bool) -> Decoder {
        Decoder { fatal, ignore_bom, bom_seen: false, streaming: false, cp: 0, needed: 0, seen: 0, lower: 0x80, upper: 0xBF }
    }
    /// A fresh decoder with the same options.
    pub fn reset(&mut self) {
        *self = Decoder::new(self.fatal, self.ignore_bom);
    }
}

pub(super) type ExMap = HashMap<u32, Exotic, BuildHasherDefault<KeyHasher>>;

/// Default caps: 256 MiB for one buffer and for all live buffers together.
pub(super) const DEFAULT_BUFFER_CAP: usize = 256 << 20;

/// Is `key` a canonical numeric string that is not an array index (`"-0"`, `"1.5"`, `"NaN"`, `"4294967295"`)?
/// On a view such a key is an invalid integer index.
fn canonical_numeric(heap: &Heap, key: PropertyKey) -> bool {
    let PropertyKey::String(s) = key else { return false };
    let Ok(u) = heap.str_units(s) else { return false };
    // Cheap filter: a canonical numeric string starts with a digit, `-`, `I` (Infinity) or `N` (NaN).
    match u.first() {
        Some(&c) if (0x30..=0x39).contains(&c) || c == b'-' as u16 || c == b'I' as u16 || c == b'N' as u16 => {}
        _ => return false,
    }
    if u.len() == 2 && u[0] == b'-' as u16 && u[1] == b'0' as u16 {
        return true;
    }
    let n = string_to_number(u);
    let text = number_to_string(n);
    text.len() == u.len() && text.bytes().zip(u).all(|(a, &b)| a as u16 == b)
}

/// Convert `src` (whole elements of `st`) to elements of `dt` in `dst` (the same element count).
fn convert_run(st: Elem, src: &[u8], dt: Elem, dst: &mut [u8]) {
    let (ss, ds) = (st.size(), dt.size());
    for (s, d) in src.chunks_exact(ss).zip(dst.chunks_exact_mut(ds)) {
        store(dt, d, load(st, s));
    }
}

impl Heap {
    /// Set the caps for one buffer and for all live buffers together (bytes). Checked before each allocation.
    pub fn set_buffer_limits(&mut self, total: usize, single: usize) {
        self.max_buffer_bytes = total;
        self.max_buffer_len = single;
    }

    pub fn buffer_limits(&self) -> (usize, usize) {
        (self.max_buffer_bytes, self.max_buffer_len)
    }

    /// Any exotic object alive? Lets hot paths skip the exotic checks.
    pub fn has_exotics(&self) -> bool {
        !self.exotics.is_empty()
    }

    pub(super) fn exotic(&self, o: ObjId) -> Option<&Exotic> {
        let ob = self.objects.get(o.index, o.gn)?;
        if ob.exotic != 0 { self.exotics.get(&o.index) } else { None }
    }

    /// The kind of an exotic object. `None` for an ordinary or stale object.
    pub fn exotic_kind(&self, o: ObjId) -> Option<ExoticKind> {
        Some(match self.exotic(o)? {
            Exotic::Buffer(_) | Exotic::Detached => ExoticKind::Buffer,
            Exotic::View { .. } => ExoticKind::View,
            Exotic::Encoder => ExoticKind::Encoder,
            Exotic::Decoder(_) => ExoticKind::Decoder,
            Exotic::Indexed => ExoticKind::Indexed,
            Exotic::Named => ExoticKind::Named,
            Exotic::Prim(_) => ExoticKind::Primitive,
            Exotic::Proxy(_) => ExoticKind::Proxy,
        })
    }

    /// Check both caps and the heap budget for `n` more buffer bytes.
    fn check_buffer(&self, n: usize) -> R<()> {
        if n > self.max_buffer_len {
            return Err(HeapError::Limit(Limit::BufferLength));
        }
        match self.buffer_bytes.checked_add(n) {
            Some(t) if t <= self.max_buffer_bytes => {}
            _ => return Err(HeapError::Limit(Limit::BufferBytes)),
        }
        let total = n.checked_add(slot_bytes::<Object>()).ok_or(HeapError::Limit(Limit::Bytes))?;
        self.charge(total)
    }

    /// Allocate the object of an exotic payload. Nothing is allocated when it fails.
    pub(super) fn make_exotic(&mut self, proto: Option<ObjId>, ex: impl FnOnce() -> Exotic) -> R<ObjId> {
        let id = self.alloc_object(proto, false)?;
        self.objects.get_mut(id.index, id.gn).expect("fresh object").exotic = EXOTIC;
        let ex = ex();
        if let Exotic::Buffer(v) = &ex {
            self.bytes += v.len();
            self.buffer_bytes += v.len();
        }
        self.exotics.insert(id.index, ex);
        Ok(id)
    }

    /// A buffer of `len` zero bytes.
    pub fn new_buffer(&mut self, proto: Option<ObjId>, len: usize) -> R<ObjId> {
        self.check_buffer(len)?;
        self.make_exotic(proto, || Exotic::Buffer(vec![0; len]))
    }

    /// A buffer that owns `data` (no copy).
    pub fn new_buffer_from(&mut self, proto: Option<ObjId>, data: Vec<u8>) -> R<ObjId> {
        self.check_buffer(data.len())?;
        self.make_exotic(proto, || Exotic::Buffer(data))
    }

    /// A new buffer with a copy of bytes `start..end` of a buffer or view. Both caps are checked before the copy.
    /// `InvalidKey` when the range does not fit `src`.
    pub fn new_buffer_copy(&mut self, proto: Option<ObjId>, src: ObjId, start: usize, end: usize) -> R<ObjId> {
        self.check_opt_obj(proto)?;
        if !self.objects.can_alloc() { return Err(HeapError::Limit(Limit::Objects)); }
        let all = self.bytes(src).ok_or(HeapError::Stale)?;
        let part = all.get(start..end).ok_or(HeapError::InvalidKey)?;
        let n = part.len();
        self.check_buffer(n)?;
        let mut v = Vec::new();
        v.try_reserve_exact(n).map_err(|_| HeapError::Limit(Limit::BufferBytes))?;
        v.extend_from_slice(part);
        self.make_exotic(proto, || Exotic::Buffer(v))
    }

    /// Write the UTF-8 form of string `s` into the buffer or view `dest`, as `TextEncoder.encodeInto` does. Returns
    /// `(units read, bytes written)`. `InvalidKey` when `dest` has no bytes.
    pub fn encode_utf8_into(&mut self, s: StrId, dest: ObjId) -> R<(usize, usize)> {
        let (slot, range) = match self.exotic(dest) {
            Some(Exotic::Detached) => return Ok((0, 0)),
            Some(Exotic::Buffer(v)) => (dest.index, 0..v.len()),
            Some(Exotic::View { buffer, offset, len, ty }) => (buffer.index, *offset..*offset + *len * ty.size()),
            _ => return Err(HeapError::InvalidKey),
        };
        let Heap { strings, exotics, .. } = self;
        let units = &strings.get(s.index, s.gn).ok_or(HeapError::Stale)?.units;
        match exotics.get_mut(&slot) {
            Some(Exotic::Buffer(v)) => Ok(crate::text::encode_into(units, &mut v[range])),
            Some(Exotic::Detached) => Ok((0, 0)),
            _ => Err(HeapError::InvalidKey),
        }
    }

    /// A `Uint8Array`-shaped view of `len` bytes at `offset` of `buffer`. See `new_typed_view`.
    pub fn new_view(&mut self, proto: Option<ObjId>, buffer: ObjId, offset: usize, len: usize) -> R<ObjId> {
        self.new_typed_view(proto, buffer, offset, len, Elem::U8)
    }

    /// A view of `len` elements of type `ty` at byte `offset` of `buffer`. `InvalidKey` when the range does not fit the
    /// buffer, the buffer is detached, or `buffer` is not a buffer. The alignment of `offset` is the caller's rule.
    pub fn new_typed_view(&mut self, proto: Option<ObjId>, buffer: ObjId, offset: usize, len: usize, ty: Elem) -> R<ObjId> {
        let Some(Exotic::Buffer(v)) = self.exotic(buffer) else { return Err(HeapError::InvalidKey) };
        if len.checked_mul(ty.size()).and_then(|n| n.checked_add(offset)).is_none_or(|e| e > v.len()) {
            return Err(HeapError::InvalidKey);
        }
        self.make_exotic(proto, || Exotic::View { buffer, offset, len, ty })
    }

    /// Is `o` a buffer whose bytes were moved away?
    pub fn buffer_detached(&self, o: ObjId) -> bool {
        matches!(self.exotic(o), Some(Exotic::Detached))
    }

    /// `ArrayBuffer.prototype.transfer`: a new buffer (with `proto`) takes the bytes of buffer `src`, cut or zero-padded to
    /// `new_len`. `src` is detached: its bytes are gone and its length is 0. No copy when the length is the same. The caps
    /// and the heap budget are checked before anything moves. `InvalidKey` when `src` is not an attached buffer.
    pub fn buffer_transfer(&mut self, proto: Option<ObjId>, src: ObjId, new_len: usize) -> R<ObjId> {
        self.check_opt_obj(proto)?;
        if !self.objects.can_alloc() {
            return Err(HeapError::Limit(Limit::Objects));
        }
        let Some(Exotic::Buffer(v)) = self.exotic(src) else { return Err(HeapError::InvalidKey) };
        let old = v.len();
        if new_len > self.max_buffer_len {
            return Err(HeapError::Limit(Limit::BufferLength));
        }
        self.check_buffer(new_len.saturating_sub(old))?;
        let Some(Exotic::Buffer(v)) = self.exotics.get_mut(&src.index) else { return Err(HeapError::Stale) };
        let mut data = std::mem::take(v);
        if new_len > old && data.try_reserve_exact(new_len - old).is_err() {
            *v = data;
            return Err(HeapError::Limit(Limit::BufferBytes));
        }
        data.resize(new_len, 0);
        if new_len < old {
            data.shrink_to_fit();
        }
        self.exotics.insert(src.index, Exotic::Detached);
        self.bytes -= old;
        self.buffer_bytes -= old;
        self.make_exotic(proto, || Exotic::Buffer(data))
    }

    /// An object whose integer-index properties come from the host (`set_indexed_source`), each time they are read.
    /// Properties follow the WebIDL legacy platform object rules for an indexed getter without a setter: an index below the
    /// length is an own, enumerable, configurable, non-writable data property; it cannot be written, defined or deleted
    /// (`false`), and a write or define at any index is rejected. An index at or after the length is no property, and the
    /// prototype chain goes on. Keys that are not array indices are ordinary.
    pub fn new_indexed(&mut self, proto: Option<ObjId>) -> R<ObjId> {
        self.make_exotic(proto, || Exotic::Indexed)
    }

    /// Install the host that answers for every `new_indexed` object of this heap.
    /// The heap owns the `Rc`, and drops it with the heap or when a new source replaces it. A heap with a host source is
    /// therefore not `Send` (a DOM is `Rc` and `RefCell`): it stays on the thread that made it. The compiler enforces this.
    /// API cost: a pure `Heap` is `!Send` too. Build it on the thread that uses it. A moved heap does not compile:
    ///
    /// ```compile_fail
    /// fn needs_send<T: Send>(_: T) {}
    /// needs_send(bare_js::heap::Heap::default());
    /// ```
    pub fn set_indexed_source(&mut self, source: std::rc::Rc<dyn IndexedSource>) {
        self.indexed = Some(source);
    }

    fn source(&self) -> R<&std::rc::Rc<dyn IndexedSource>> {
        self.indexed.as_ref().ok_or(HeapError::HostUnavailable)
    }

    /// The number of virtual own keys of `o`: the elements of a typed array (0 for a `DataView` or a detached buffer), the live length of a
    /// host-indexed object, or the live number of names of a host named object.
    pub fn virtual_len(&self, o: ObjId) -> R<usize> {
        match self.exotic(o) {
            Some(Exotic::View { buffer, len, ty, .. }) if *ty != Elem::Data => Ok(if matches!(self.exotics.get(&buffer.index), Some(Exotic::Buffer(_))) { *len } else { 0 }),
            Some(Exotic::Indexed) => self.source()?.indexed_len(o),
            Some(Exotic::Named) => self.named_len(o),
            Some(Exotic::Prim(Value::String(s))) => self.str_len(*s),
            _ => Ok(0),
        }
    }

    pub fn new_encoder(&mut self, proto: Option<ObjId>) -> R<ObjId> {
        self.make_exotic(proto, || Exotic::Encoder)
    }

    pub fn new_decoder(&mut self, proto: Option<ObjId>, d: Decoder) -> R<ObjId> {
        self.make_exotic(proto, || Exotic::Decoder(d))
    }

    pub fn decoder(&self, o: ObjId) -> Option<Decoder> {
        match self.exotic(o)? {
            Exotic::Decoder(d) => Some(*d),
            _ => None,
        }
    }

    pub fn set_decoder(&mut self, o: ObjId, d: Decoder) -> bool {
        match self.objects.get(o.index, o.gn).is_some_and(|ob| ob.exotic != 0).then(|| self.exotics.get_mut(&o.index)).flatten() {
            Some(Exotic::Decoder(slot)) => {
                *slot = d;
                true
            }
            _ => false,
        }
    }

    /// Byte length of a buffer.
    pub fn buffer_len(&self, o: ObjId) -> Option<usize> {
        match self.exotic(o)? {
            Exotic::Buffer(v) => Some(v.len()),
            Exotic::Detached => Some(0),
            _ => None,
        }
    }

    /// `(buffer, byte offset, length)` of a view. The length is in elements (bytes for a `DataView`). The buffer may be detached.
    pub fn view_parts(&self, o: ObjId) -> Option<(ObjId, usize, usize)> {
        match self.exotic(o)? {
            Exotic::View { buffer, offset, len, .. } => Some((*buffer, *offset, *len)),
            _ => None,
        }
    }

    /// The element type of a view (`Elem::Data` for a `DataView`).
    pub fn view_type(&self, o: ObjId) -> Option<Elem> {
        match self.exotic(o)? {
            Exotic::View { ty, .. } => Some(*ty),
            _ => None,
        }
    }

    /// Is the buffer of view `o` detached? `false` for anything that is not a view.
    pub fn view_detached(&self, o: ObjId) -> bool {
        matches!(self.exotic(o), Some(Exotic::View { buffer, .. }) if matches!(self.exotics.get(&buffer.index), Some(Exotic::Detached)))
    }

    /// The bytes of a buffer, or of a view (empty when detached). Borrowed from the heap: no copy.
    pub fn bytes(&self, o: ObjId) -> Option<&[u8]> {
        match self.exotic(o)? {
            Exotic::Buffer(v) => Some(v),
            Exotic::Detached => Some(&[]),
            Exotic::View { buffer, offset, len, ty } => match self.exotics.get(&buffer.index) {
                Some(Exotic::Buffer(v)) => v.get(*offset..*offset + *len * ty.size()),
                Some(Exotic::Detached) => Some(&[]),
                _ => None,
            },
            _ => None,
        }
    }

    /// The bytes of a buffer, or of a view, to write. No JavaScript may run while this borrow is held.
    pub fn bytes_mut(&mut self, o: ObjId) -> Option<&mut [u8]> {
        let (slot, range) = match self.exotic(o)? {
            Exotic::Buffer(_) => (o.index, None),
            Exotic::Detached => return Some(&mut []),
            Exotic::View { buffer, offset, len, ty } => (buffer.index, Some(*offset..*offset + *len * ty.size())),
            _ => return None,
        };
        match self.exotics.get_mut(&slot) {
            Some(Exotic::Buffer(v)) => match range {
                None => Some(&mut v[..]),
                Some(r) => v.get_mut(r),
            },
            Some(Exotic::Detached) => Some(&mut []),
            _ => None,
        }
    }

    /// Copy `n` bytes from view `src` at byte `si` to view `dst` at byte `di`, as `memmove` does (the ranges may overlap when
    /// both views share one buffer). `false`, and nothing copied, if a range does not fit its view.
    pub fn view_copy(&mut self, src: ObjId, si: usize, dst: ObjId, di: usize, n: usize) -> bool {
        let (Some((sb, so, _)), Some((db, d_off, _))) = (self.view_parts(src), self.view_parts(dst)) else { return false };
        let (sl, dl) = (self.bytes(src).map_or(0, <[u8]>::len), self.bytes(dst).map_or(0, <[u8]>::len));
        if si.checked_add(n).is_none_or(|e| e > sl) || di.checked_add(n).is_none_or(|e| e > dl) {
            return false;
        }
        let (s, d) = (so + si, d_off + di);
        if sb == db {
            if let Some(Exotic::Buffer(v)) = self.exotics.get_mut(&sb.index) {
                v.copy_within(s..s + n, d);
            }
            return true;
        }
        // Two buffers: move the source vector out for the copy, so no second borrow of the map is needed.
        let Some(Exotic::Buffer(sv)) = self.exotics.get_mut(&sb.index).map(Exotic::take) else { return false };
        if let Some(Exotic::Buffer(dv)) = self.exotics.get_mut(&db.index) {
            dv[d..d + n].copy_from_slice(&sv[s..s + n]);
        }
        if let Some(Exotic::Buffer(slot)) = self.exotics.get_mut(&sb.index) {
            *slot = sv;
        }
        true
    }

    /// `%TypedArray%.prototype.set` for a typed-array source: write the `n` elements of typed array `src` (from element 0)
    /// to typed array `dst` from element `di`, converting each one from the type of `src` to the type of `dst`. The same
    /// type is one byte copy. Another type converts element by element, through a copy of the source bytes when both
    /// views share a buffer. `false`, and nothing written, if a range does not fit or a BigInt type meets a number type.
    pub fn view_set_from(&mut self, src: ObjId, dst: ObjId, di: usize) -> bool {
        let (Some(st), Some(dt)) = (self.view_type(src), self.view_type(dst)) else { return false };
        let (n, dl) = (self.virtual_len(src).unwrap_or(0), self.virtual_len(dst).unwrap_or(0));
        if st.is_big() != dt.is_big() || di.checked_add(n).is_none_or(|e| e > dl) {
            return false;
        }
        // The same type, or integers of one size (the conversion wraps), keep the bytes. `Uint8ClampedArray` clamps: only a
        // `Uint8Array` source keeps its bytes there.
        if st == dt || (st.size() == dt.size() && !st.is_float() && !dt.is_float() && (dt != Elem::U8C || st == Elem::U8)) {
            return self.view_copy(src, 0, dst, di * dt.size(), n * st.size());
        }
        let Some((sb, so, _)) = self.view_parts(src) else { return false };
        let Some((db, d_off, _)) = self.view_parts(dst) else { return false };
        let (ss, ds) = (st.size(), dt.size());
        let d_at = d_off + di * ds;
        // The source bytes: a copy when the views share a buffer (the ranges may overlap), else the vector is moved out.
        if sb == db {
            let Some(Exotic::Buffer(v)) = self.exotics.get_mut(&db.index) else { return false };
            let tmp = v[so..so + n * ss].to_vec();
            convert_run(st, &tmp, dt, &mut v[d_at..d_at + n * ds]);
            return true;
        }
        let Some(Exotic::Buffer(sv)) = self.exotics.get_mut(&sb.index).map(Exotic::take) else { return false };
        if let Some(Exotic::Buffer(dv)) = self.exotics.get_mut(&db.index) {
            convert_run(st, &sv[so..so + n * ss], dt, &mut dv[d_at..d_at + n * ds]);
        }
        if let Some(Exotic::Buffer(slot)) = self.exotics.get_mut(&sb.index) {
            *slot = sv;
        }
        true
    }

    /// One byte of a view; `None` when `i` is not a valid byte index.
    pub fn view_get(&self, o: ObjId, i: usize) -> Option<u8> {
        self.bytes(o)?.get(i).copied()
    }

    /// Write one byte of a view; `false` when `i` is not a valid byte index.
    pub fn view_set(&mut self, o: ObjId, i: usize, b: u8) -> bool {
        match self.bytes_mut(o).and_then(|s| s.get_mut(i)) {
            Some(slot) => {
                *slot = b;
                true
            }
            None => false,
        }
    }

    /// Element `i` of typed array `o` as a number; `None` when `i` is not a valid index (or the buffer is detached).
    pub fn view_load(&self, o: ObjId, i: usize) -> Option<f64> {
        let Some(Exotic::View { buffer, offset, len, ty }) = self.exotic(o) else { return None };
        if i >= *len {
            return None;
        }
        let Some(Exotic::Buffer(v)) = self.exotics.get(&buffer.index) else { return None };
        let at = offset + i * ty.size();
        Some(load(*ty, v.get(at..at + ty.size())?))
    }

    /// Convert `n` to the element type and write it at index `i`; `false` when `i` is not a valid index.
    pub fn view_store(&mut self, o: ObjId, i: usize, n: f64) -> bool {
        let Some(Exotic::View { buffer, offset, len, ty }) = self.exotic(o) else { return false };
        let (buffer, offset, len, ty) = (*buffer, *offset, *len, *ty);
        if i >= len {
            return false;
        }
        let Some(Exotic::Buffer(v)) = self.exotics.get_mut(&buffer.index) else { return false };
        let at = offset + i * ty.size();
        match v.get_mut(at..at + ty.size()) {
            Some(b) => {
                store(ty, b, n);
                true
            }
            None => false,
        }
    }

    /// The 8 bytes of element `i` of a BigInt typed array as a `u64`; `None` when `i` is not a valid index.
    fn view_raw64(&self, o: ObjId, i: usize) -> Option<(Elem, u64)> {
        let Some(Exotic::View { buffer, offset, len, ty }) = self.exotic(o) else { return None };
        if i >= *len || !ty.is_big() {
            return None;
        }
        let Some(Exotic::Buffer(v)) = self.exotics.get(&buffer.index) else { return None };
        let at = offset + i * 8;
        Some((*ty, u64::from_le_bytes(v.get(at..at + 8)?.try_into().ok()?)))
    }

    /// Element `i` of a BigInt typed array as a BigInt; `None` when `i` is not a valid index (or the buffer is detached).
    /// A `BigUint64Array` value above `i64::MAX` makes a heap entry.
    pub fn view_load_big(&mut self, o: ObjId, i: usize) -> R<Option<Value>> {
        match self.view_raw64(o, i) {
            Some((Elem::BI64, raw)) => Ok(Some(Value::BigInt(raw as i64))),
            Some((_, raw)) => self.bigint_from_u64(raw).map(Some),
            None => Ok(None),
        }
    }

    /// Write the BigInt `v` (wrapped to 64 bits, `BigInt.asUintN(64, v)`) at index `i` of a BigInt typed array; `false` when
    /// `i` is not a valid index.
    pub fn view_store_big(&mut self, o: ObjId, i: usize, v: Value) -> bool {
        let raw = self.bigint_low_u64(v).to_le_bytes();
        let Some(Exotic::View { buffer, offset, len, ty }) = self.exotic(o) else { return false };
        let (buffer, offset, len) = (*buffer, *offset, *len);
        if i >= len || !ty.is_big() {
            return false;
        }
        let Some(Exotic::Buffer(b)) = self.exotics.get_mut(&buffer.index) else { return false };
        match b.get_mut(offset + i * 8..offset + i * 8 + 8) {
            Some(slot) => {
                slot.copy_from_slice(&raw);
                true
            }
            None => false,
        }
    }

    /// `[[Set]]` of a valid or invalid index on a BigInt typed array: only a BigInt is written (`NeedsBigInt` otherwise).
    fn view_set_big(&mut self, o: ObjId, index: Option<usize>, v: Value) -> R<()> {
        if !v.is_bigint() {
            return Err(HeapError::NeedsBigInt);
        }
        if let Some(i) = index {
            self.view_store_big(o, i, v);
        }
        Ok(())
    }

    // ----- integer-indexed property semantics -----

    /// Is `key` an integer-indexed key of the typed array `o` (so a write converts the value with `ToNumber` first)?
    pub fn is_view_index_key(&self, o: ObjId, key: PropertyKey) -> bool {
        self.view_index(o, key).is_some()
    }

    /// `key` is an integer index that a typed array `o` does not have (out of range, or its buffer is detached).
    pub fn is_view_bad_index(&self, o: ObjId, key: PropertyKey) -> bool {
        match self.view_index(o, key) {
            Some(None) => true,
            Some(Some(_)) => self.view_detached(o),
            None => false,
        }
    }

    /// Classify `key` on `o`. `None`: `o` is not a typed array, or the key is not numeric: the ordinary rules apply.
    /// `Some(None)`: an invalid integer index. `Some(Some(i))`: a valid index.
    pub(super) fn view_index(&self, o: ObjId, key: PropertyKey) -> Option<Option<usize>> {
        let Some(Exotic::View { len, ty, .. }) = self.exotic(o) else { return None };
        if *ty == Elem::Data {
            return None;
        }
        match key {
            PropertyKey::Index(i) => Some(((i as usize) < *len).then_some(i as usize)),
            PropertyKey::String(_) if canonical_numeric(self, key) => Some(None),
            _ => None,
        }
    }

    /// `[[GetOwnProperty]]` of a numeric key on a typed array: `Some(property)` or `Some(None)`, `None` for the ordinary
    /// path. A BigInt element is a BigInt value (`view_own_big`).
    pub(super) fn view_own(&self, o: ObjId, key: PropertyKey, want_value: bool) -> R<Option<Option<Property>>> {
        // One view lookup and one buffer lookup: this is the element-read path of a script loop.
        let Some(Exotic::View { buffer, offset, len, ty }) = self.exotic(o) else { return Ok(None) };
        let i = match key {
            _ if *ty == Elem::Data => return Ok(None),
            PropertyKey::Index(i) if (i as usize) < *len => i as usize,
            PropertyKey::Index(_) => return Ok(Some(None)),
            PropertyKey::String(_) if canonical_numeric(self, key) => return Ok(Some(None)),
            _ => return Ok(None),
        };
        if want_value && ty.is_big() {
            return self.view_own_big(*buffer, *offset, i, *ty);
        }
        // A detached buffer has no elements.
        let Some(Exotic::Buffer(v)) = self.exotics.get(&buffer.index) else { return Ok(Some(None)) };
        let value = if want_value {
            let at = offset + i * ty.size();
            match v.get(at..at + ty.size()) {
                Some(b) => Value::Number(load(*ty, b)),
                None => return Ok(Some(None)),
            }
        } else {
            Value::Undefined
        };
        Ok(Some(Some(Property::Data { value, writable: true, enumerable: true, configurable: true })))
    }

    /// The element of a BigInt typed array as a property. `BigInt64Array` and a `BigUint64Array` value up to `i64::MAX` are
    /// inline BigInts. A larger one needs a heap entry: `BigUnsigned` (see `exotic_err`).
    #[cold]
    fn view_own_big(&self, buffer: ObjId, offset: usize, i: usize, ty: Elem) -> R<Option<Option<Property>>> {
        let Some(Exotic::Buffer(v)) = self.exotics.get(&buffer.index) else { return Ok(Some(None)) };
        let at = offset + i * 8;
        let Some(b) = v.get(at..at + 8) else { return Ok(Some(None)) };
        let raw = u64::from_le_bytes(b.try_into().expect("8 bytes"));
        let value = match (ty, i64::try_from(raw)) {
            (Elem::BI64, _) => Value::BigInt(raw as i64),
            (_, Ok(n)) => Value::BigInt(n),
            (_, Err(_)) => return Err(HeapError::BigUnsigned(raw)),
        };
        Ok(Some(Some(Property::Data { value, writable: true, enumerable: true, configurable: true })))
    }

    /// An error of `exotic_own`: a `BigUint64Array` element above `i64::MAX` (see `BigUnsigned`) is its property, anything else
    /// fails. Kept out of line: it is off the path of every property read.
    #[cold]
    #[inline(never)]
    pub(super) fn exotic_err(&mut self, e: HeapError) -> R<Property> {
        let HeapError::BigUnsigned(raw) = e else { return Err(e) };
        let value = self.bigint_from_u64(raw)?;
        Ok(Property::Data { value, writable: true, enumerable: true, configurable: true })
    }

    /// `exotic_own` for a caller that only reads attributes (`peek_own_property`): a large `BigUint64Array` element reads as
    /// `undefined` here, since no heap entry may be made.
    #[inline]
    pub(super) fn exotic_own_peek(&self, o: ObjId, key: PropertyKey) -> R<Option<Option<Property>>> {
        match self.exotic_own(o, key, true) {
            Err(HeapError::BigUnsigned(_)) => Ok(Some(Some(Property::Data { value: Value::Undefined, writable: true, enumerable: true, configurable: true }))),
            r => r,
        }
    }

    /// `[[GetOwnProperty]]` of a key on any exotic object: `Some(r)` ends the lookup there, `None` means the ordinary rules.
    /// `want_value` false is for the callers that only ask whether the property exists (`in`, `delete`): they never make a
    /// `Missing` object, and get a placeholder value.
    pub(super) fn exotic_own(&self, o: ObjId, key: PropertyKey, want_value: bool) -> R<Option<Option<Property>>> {
        if let Some(r) = self.view_own(o, key, want_value)? {
            return Ok(Some(r));
        }
        let i = match (self.exotic(o), key) {
            (Some(Exotic::Indexed), PropertyKey::Index(i)) => i,
            (Some(Exotic::Named), _) => return self.named_exists(o, key),
            (Some(Exotic::Prim(Value::String(s))), _) => return self.prim_str_own(*s, key),
            (Some(Exotic::Proxy(_)), _) => return Err(HeapError::Proxy(o)),
            _ => return Ok(None),
        };
        let value = match self.source()?.indexed_item(o, i as usize)? {
            Slot::Absent => return Ok(None),
            Slot::Ready(v) => Value::Object(v),
            Slot::Missing if want_value => return Err(HeapError::NeedsHost { owner: o, index: i }),
            Slot::Missing => Value::Undefined,
        };
        Ok(Some(Some(Property::Data { value, writable: false, enumerable: true, configurable: true })))
    }

    /// ToNumber of a primitive for an element write. An object needs the evaluator (`NeedsExecution`). A BigInt type gives
    /// `BigElement`: the caller writes a BigInt with `view_set_big`.
    fn to_elem_number(&self, o: ObjId, v: Value) -> R<f64> {
        if matches!(self.exotic(o), Some(Exotic::View { ty, .. }) if ty.is_big()) {
            return Err(HeapError::BigElement);
        }
        match v {
            Value::Symbol(_) => Err(HeapError::SymbolToNumber),
            _ => self.to_number(v),
        }
    }

    /// `[[Set]]` of an integer-indexed key. `Some(Done)` when this object (or, for an ordinary receiver, a view it
    /// inherits from) handles the key. `None`: the ordinary algorithm runs.
    pub(super) fn exotic_set(&mut self, o: ObjId, key: PropertyKey, value: Value) -> R<Option<Set>> {
        // A proxy needs no check here: it is no view, has no prototype, so this returns `None` and `lookup` meets it next.
        if self.obj(o)?.exotic == EXOTIC_NAMED && !matches!(key, PropertyKey::Symbol(_)) && !self.named_hidden(o, key)? {
            return Ok(Some(if self.named_write(o, key, NamedOp::Set(value))? { Set::Done } else { Set::Rejected }));
        }
        if let Some(r) = self.view_index(o, key) {
            // ToNumber runs even for an invalid index
            let n = match self.to_elem_number(o, value) {
                Err(HeapError::BigElement) => return self.view_set_big(o, r, value).map(|()| Some(Set::Done)),
                n => n?,
            };
            if let Some(i) = r {
                self.view_store(o, i, n);
            }
            return Ok(Some(Set::Done));
        }
        // An ordinary receiver with a view in its chain: an invalid integer index stops the write there.
        let mut cur = self.obj(o)?.proto;
        let mut any = false;
        while let Some(c) = cur {
            let ob = self.obj(c)?;
            any |= ob.exotic != 0;
            cur = ob.proto;
        }
        if !any || !matches!(key, PropertyKey::Index(_) | PropertyKey::String(_)) {
            return Ok(None);
        }
        if self.own(self.obj(o)?, key).is_some() {
            return Ok(None);
        }
        let mut cur = self.obj(o)?.proto;
        while let Some(c) = cur {
            let ob = self.obj(c)?;
            if let Some(r) = self.view_index(c, key) {
                return Ok(if r.is_none() { Some(Set::Done) } else { None });
            }
            if self.own(ob, key).is_some() {
                return Ok(None);
            }
            cur = ob.proto;
        }
        Ok(None)
    }

    /// `[[DefineOwnProperty]]` of an integer-indexed key on a typed array. `None`: ordinary rules.
    pub(super) fn view_define(&mut self, o: ObjId, key: PropertyKey, d: &PropertyDescriptor) -> R<Option<bool>> {
        if matches!(key, PropertyKey::Index(_)) && matches!(self.exotic(o), Some(Exotic::Indexed)) {
            return Ok(Some(false)); // no indexed setter: an index cannot be defined, in range or not
        }
        let Some(r) = self.view_index(o, key) else { return Ok(None) };
        let Some(i) = r else { return Ok(Some(false)) };
        if d.configurable == Some(false) || d.enumerable == Some(false) || d.is_accessor() || d.writable == Some(false) {
            return Ok(Some(false));
        }
        if self.view_detached(o) {
            return Ok(Some(false));
        }
        if let Some(v) = d.value {
            match self.to_elem_number(o, v) {
                Err(HeapError::BigElement) => self.view_set_big(o, Some(i), v)?,
                n => {
                    self.view_store(o, i, n?);
                }
            }
        }
        Ok(Some(true))
    }

    /// The indices of a view, ahead of its other own keys. Checks the memory for the key list first.
    pub(super) fn view_keys(&self, o: ObjId) -> R<Vec<PropertyKey>> {
        let len = self.virtual_len(o)?;
        self.charge(len.saturating_mul(std::mem::size_of::<PropertyKey>()))?;
        Ok((0..len.min(MAX_INDEX as usize) as u32).map(PropertyKey::Index).collect())
    }

    /// Drop the payload of every dead exotic object and release its bytes. Called after a sweep.
    pub(super) fn prune_exotics(&mut self) {
        if self.exotics.is_empty() {
            return;
        }
        let Heap { objects, exotics, bytes, buffer_bytes, .. } = self;
        exotics.retain(|&slot, ex| {
            if objects.meta[slot as usize].dense != VACANT {
                return true;
            }
            if let Exotic::Buffer(v) = ex {
                *bytes -= v.len();
                *buffer_bytes -= v.len();
            }
            false
        });
        if exotics.capacity() > 64 && exotics.len() * 4 < exotics.capacity() {
            exotics.shrink_to_fit();
        }
    }
}

impl Exotic {
    /// Move a buffer's vector out, leaving an empty one. Only for `view_copy`.
    fn take(&mut self) -> Exotic {
        match self {
            Exotic::Buffer(v) => Exotic::Buffer(std::mem::take(v)),
            _ => Exotic::Encoder,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_payload_and_object_share_one_heap_budget() {
        for from in [false, true] {
            let mut h = Heap::default();
            h.limits.max_bytes = h.bytes + slot_bytes::<Object>() + 15;
            let before = h.stats();
            let r = if from { h.new_buffer_from(None, vec![0; 16]) } else { h.new_buffer(None, 16) };
            assert_eq!(r, Err(HeapError::Limit(Limit::Bytes)));
            assert_eq!(h.stats().bytes, before.bytes);
            assert_eq!(h.stats().live_objects, before.live_objects);
        }
    }

    #[test]
    fn empty_buffer_copies_still_validate_the_range() {
        let mut h = Heap::default();
        let b = h.new_buffer(None, 4).unwrap();
        let before = h.stats().live_objects;
        for (start, end) in [(5, 5), (3, 2)] {
            assert_eq!(h.new_buffer_copy(None, b, start, end), Err(HeapError::InvalidKey));
        }
        assert_eq!(h.stats().live_objects, before);
    }
}
