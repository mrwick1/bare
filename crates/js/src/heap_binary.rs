//! Byte buffers, `Uint8Array` views and text-codec state, as exotic objects of the heap.
//! A child module of `heap.rs` (`#[path]`), so it uses the heap's private slabs.
//!
//! Layout: an exotic object is an ordinary `Object` with its `exotic` flag set. Its payload lives in
//! `Heap::exotics`, a map from the object's slot index to an [`Exotic`]. So ordinary objects pay one
//! flag and no pointer. Bytes are never one property per byte: a buffer owns one `Vec<u8>`, a view owns
//! three numbers and a handle of its buffer.
//!
//! Rules:
//! - A view keeps its buffer alive (the collector marks the edge). A dead buffer frees its bytes at the
//!   next `collect`, together with its map entry.
//! - Bytes count against `max_bytes` and against two buffer caps (`set_buffer_limits`): one buffer and all live
//!   buffers together. Both are checked before the allocation.
//! - Integer-indexed keys of a view follow the spec: a valid index is a writable, enumerable, configurable data
//!   property; an invalid index (out of range, or a canonical numeric string such as `"-0"`, `"1.5"`, `"NaN"`) is
//!   absent, ends a prototype walk, ignores writes and cannot be defined or deleted.

use super::*;

/// What an exotic object holds.
pub(super) enum Exotic {
    Buffer(Vec<u8>),
    /// `buffer` is always a live buffer object. `offset + len <= buffer length`.
    View { buffer: ObjId, offset: usize, len: usize },
    Encoder,
    Decoder(Decoder),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExoticKind {
    Buffer,
    View,
    Encoder,
    Decoder,
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

    fn exotic(&self, o: ObjId) -> Option<&Exotic> {
        let ob = self.objects.get(o.index, o.gn)?;
        if ob.exotic { self.exotics.get(&o.index) } else { None }
    }

    /// The kind of an exotic object. `None` for an ordinary or stale object.
    pub fn exotic_kind(&self, o: ObjId) -> Option<ExoticKind> {
        Some(match self.exotic(o)? {
            Exotic::Buffer(_) => ExoticKind::Buffer,
            Exotic::View { .. } => ExoticKind::View,
            Exotic::Encoder => ExoticKind::Encoder,
            Exotic::Decoder(_) => ExoticKind::Decoder,
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
        self.charge(n)
    }

    /// Allocate the object of an exotic payload. Nothing is allocated when it fails.
    fn make_exotic(&mut self, proto: Option<ObjId>, ex: impl FnOnce() -> Exotic) -> R<ObjId> {
        let id = self.alloc_object(proto, false)?;
        self.objects.get_mut(id.index, id.gn).expect("fresh object").exotic = true;
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
        let n = end.saturating_sub(start);
        self.check_buffer(n)?;
        let all = self.bytes(src).ok_or(HeapError::Stale)?;
        let part = if n == 0 { &[][..] } else { all.get(start..end).ok_or(HeapError::InvalidKey)? };
        let mut v = Vec::new();
        v.try_reserve_exact(n).map_err(|_| HeapError::Limit(Limit::BufferBytes))?;
        v.extend_from_slice(part);
        self.make_exotic(proto, || Exotic::Buffer(v))
    }

    /// Write the UTF-8 form of string `s` into the buffer or view `dest`, as `TextEncoder.encodeInto` does. Returns
    /// `(units read, bytes written)`. `InvalidKey` when `dest` has no bytes.
    pub fn encode_utf8_into(&mut self, s: StrId, dest: ObjId) -> R<(usize, usize)> {
        let (slot, range) = match self.exotic(dest) {
            Some(Exotic::Buffer(v)) => (dest.index, 0..v.len()),
            Some(Exotic::View { buffer, offset, len }) => (buffer.index, *offset..*offset + *len),
            _ => return Err(HeapError::InvalidKey),
        };
        let Heap { strings, exotics, .. } = self;
        let units = &strings.get(s.index, s.gn).ok_or(HeapError::Stale)?.units;
        match exotics.get_mut(&slot) {
            Some(Exotic::Buffer(v)) => Ok(crate::text::encode_into(units, &mut v[range])),
            _ => Err(HeapError::InvalidKey),
        }
    }

    /// A view of `len` bytes at `offset` of `buffer`. `InvalidKey` when the range does not fit the buffer
    /// or `buffer` is not a buffer.
    pub fn new_view(&mut self, proto: Option<ObjId>, buffer: ObjId, offset: usize, len: usize) -> R<ObjId> {
        let Some(Exotic::Buffer(v)) = self.exotic(buffer) else { return Err(HeapError::InvalidKey) };
        if offset.checked_add(len).is_none_or(|e| e > v.len()) {
            return Err(HeapError::InvalidKey);
        }
        self.make_exotic(proto, || Exotic::View { buffer, offset, len })
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
        match self.objects.get(o.index, o.gn).is_some_and(|ob| ob.exotic).then(|| self.exotics.get_mut(&o.index)).flatten() {
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
            _ => None,
        }
    }

    /// `(buffer, byte offset, length)` of a view.
    pub fn view_parts(&self, o: ObjId) -> Option<(ObjId, usize, usize)> {
        match self.exotic(o)? {
            Exotic::View { buffer, offset, len } => Some((*buffer, *offset, *len)),
            _ => None,
        }
    }

    /// The bytes of a buffer, or of a view. Borrowed from the heap: no copy.
    pub fn bytes(&self, o: ObjId) -> Option<&[u8]> {
        match self.exotic(o)? {
            Exotic::Buffer(v) => Some(v),
            Exotic::View { buffer, offset, len } => match self.exotics.get(&buffer.index) {
                Some(Exotic::Buffer(v)) => v.get(*offset..*offset + *len),
                _ => None,
            },
            _ => None,
        }
    }

    /// The bytes of a buffer, or of a view, to write. No JavaScript may run while this borrow is held.
    pub fn bytes_mut(&mut self, o: ObjId) -> Option<&mut [u8]> {
        let (slot, range) = match self.exotic(o)? {
            Exotic::Buffer(_) => (o.index, None),
            Exotic::View { buffer, offset, len } => (buffer.index, Some(*offset..*offset + *len)),
            _ => return None,
        };
        match self.exotics.get_mut(&slot) {
            Some(Exotic::Buffer(v)) => match range {
                None => Some(&mut v[..]),
                Some(r) => v.get_mut(r),
            },
            _ => None,
        }
    }

    /// Copy `n` bytes from view `src` at `si` to view `dst` at `di`, as `memmove` does (the ranges may overlap when
    /// both views share one buffer). `false`, and nothing copied, if a range does not fit its view.
    pub fn view_copy(&mut self, src: ObjId, si: usize, dst: ObjId, di: usize, n: usize) -> bool {
        let (Some((sb, so, sl)), Some((db, d_off, dl))) = (self.view_parts(src), self.view_parts(dst)) else { return false };
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

    /// One byte of a view; `None` when `i` is not a valid index.
    pub fn view_get(&self, o: ObjId, i: usize) -> Option<u8> {
        self.bytes(o)?.get(i).copied()
    }

    /// Write one byte of a view; `false` when `i` is not a valid index.
    pub fn view_set(&mut self, o: ObjId, i: usize, b: u8) -> bool {
        match self.bytes_mut(o).and_then(|s| s.get_mut(i)) {
            Some(slot) => {
                *slot = b;
                true
            }
            None => false,
        }
    }

    // ----- integer-indexed property semantics -----

    /// Is `key` an integer-indexed key of the view `o` (so a write converts the value with `ToNumber` first)?
    pub fn is_view_index_key(&self, o: ObjId, key: PropertyKey) -> bool {
        self.view_index(o, key).is_some()
    }

    /// Classify `key` on `o`. `None`: `o` is not a view, or the key is not numeric: the ordinary rules apply.
    /// `Some(None)`: an invalid integer index. `Some(Some(i))`: a valid index.
    pub(super) fn view_index(&self, o: ObjId, key: PropertyKey) -> Option<Option<usize>> {
        let Some(Exotic::View { len, .. }) = self.exotic(o) else { return None };
        match key {
            PropertyKey::Index(i) => Some(((i as usize) < *len).then_some(i as usize)),
            PropertyKey::String(_) if canonical_numeric(self, key) => Some(None),
            _ => None,
        }
    }

    /// `[[GetOwnProperty]]` of a numeric key on a view: `Some(property)` or `Some(None)`, `None` for the ordinary path.
    pub(super) fn view_own(&self, o: ObjId, key: PropertyKey) -> Option<Option<Property>> {
        Some(self.view_index(o, key)?.and_then(|i| {
            let b = self.view_get(o, i)?;
            Some(Property::Data { value: Value::Number(b as f64), writable: true, enumerable: true, configurable: true })
        }))
    }

    /// ToUint8 of a primitive. An object needs the evaluator (`NeedsExecution`).
    fn to_uint8(&self, v: Value) -> R<u8> {
        Ok(to_uint32(self.to_number(v)?) as u8)
    }

    /// `[[Set]]` of an integer-indexed key. `Some(Done)` when this object (or, for an ordinary receiver, a view it
    /// inherits from) handles the key. `None`: the ordinary algorithm runs.
    pub(super) fn exotic_set(&mut self, o: ObjId, key: PropertyKey, value: Value) -> R<Option<Set>> {
        if let Some(r) = self.view_index(o, key) {
            let b = self.to_uint8(value)?; // ToNumber runs even for an invalid index
            if let Some(i) = r {
                self.view_set(o, i, b);
            }
            return Ok(Some(Set::Done));
        }
        // An ordinary receiver with a view in its chain: an invalid integer index stops the write there.
        let mut cur = self.obj(o)?.proto;
        let mut any = false;
        while let Some(c) = cur {
            let ob = self.obj(c)?;
            any |= ob.exotic;
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

    /// `[[DefineOwnProperty]]` of an integer-indexed key on a view. `None`: ordinary rules.
    pub(super) fn view_define(&mut self, o: ObjId, key: PropertyKey, d: &PropertyDescriptor) -> R<Option<bool>> {
        let Some(r) = self.view_index(o, key) else { return Ok(None) };
        let Some(i) = r else { return Ok(Some(false)) };
        if d.configurable == Some(false) || d.enumerable == Some(false) || d.is_accessor() || d.writable == Some(false) {
            return Ok(Some(false));
        }
        if let Some(v) = d.value {
            let b = self.to_uint8(v)?;
            self.view_set(o, i, b);
        }
        Ok(Some(true))
    }

    /// The indices of a view, ahead of its other own keys. Checks the memory for the key list first.
    pub(super) fn view_keys(&self, o: ObjId) -> R<Vec<PropertyKey>> {
        let Some(Exotic::View { len, .. }) = self.exotic(o) else { return Ok(Vec::new()) };
        self.charge(len.saturating_mul(std::mem::size_of::<PropertyKey>()))?;
        Ok((0..*len as u32).map(PropertyKey::Index).collect())
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
