//! Objects (`docs/design-engine-rewrite.md` 6.4, 6.9 items 5 and 8, 6.11 item 14; plan step S1.9).
//!
//! An object is one cell: the header of `cell.rs` (a 32-bit shape id and the layout code), then inline value slots. The shape says
//! which key is in which slot (`shapes.rs`); the object holds only the values. In JS terms: `{a: 1, b: 2}` is a header and two words.
//!
//! | Layout | Cell | Words after the header |
//! | --- | --- | --- |
//! | `Object` | header, `cap` inline words | `cap` is odd and at least `shape.inline()` |
//! | `Indexed` | header, the elements word, `cap` inline words | `cap` is even and at least `shape.inline()` |
//!
//! `cap` is `inline` rounded up so that the cell is a multiple of 16 bytes, which the allocator needs anyway (`heap.rs`): the
//! rounding is free room. An empty object is 16 bytes. An object of 8 inline slots is 80 bytes and has 9.
//!
//! - **Overflow record, only for an object that grows.** There is no pointer word in a cell that does not need one. When an object
//!   gets more slots than `cap`, its last inline word stops being a value: it holds the pointer to a `Slots` record, and the value
//!   that was in it moves to the record's first value. Slot `i` is inline for `i < cap - 1` and is `record[i - (cap - 1)]` after.
//!   The header flag `F_OVER` says the record exists. For every tree shape the flag is set exactly when `shape.slots > cap`, so a
//!   cache that holds an object's shape knows where a slot is without reading the flag. (A dictionary object may hold a record
//!   before it needs it; a cache does not fill for a dictionary shape.) Design 6.11 item 14 compares this with a pointer in every
//!   object: it costs 8 bytes in every object, and this form costs nothing in objects that do not grow.
//! - **Elements in one tagged layout.** An `Indexed` object (an array, `arguments`) has the elements word after the header: the
//!   pointer to an `Elements` record, or `undefined` when there is none. The record is `[header with capacity][len: u32][count: u32]
//!   [cap values]`, and every value in it is a [`Value`], so a number, a string and an object need no other kind. `len` is the array
//!   length. Two forms, told by the header flag `F_SPARSE`:
//!   - dense: element `i` is value `i`; a missing element is [`Value::HOLE`]; values at or above `len` are holes. `len` and `cap`
//!     do not depend on each other (`new Array(1e6)` is a record of capacity 0 and length 1,000,000);
//!   - sparse: `count` pairs of (index as a number, value), sorted by index, found by binary search.
//!
//!   A write more than [`DENSE_GAP`] above the capacity of a dense record makes it sparse. A sparse record that has at least one
//!   element in four of the indexes up to its last goes back to dense. A plain `Object` has no elements word: its integer keys are
//!   properties of its shape (the sorted order of `ownKeys` is the business of the built-ins, S3).
//! - A record is a cell with no shape (its shape word holds the capacity). Every value in it is valid at all times (holes and
//!   `undefined` fill the unused room), so the collector can scan the whole record (`gc.rs`: it scans a long one in slices).
//!
//! Failure: a function that allocates returns `None` when the heap has no memory, and then the object is as it was. A record that
//! outgrew is replaced by a bigger one; the old one is garbage.
//!
//! Type invariant of [`Obj`]: it points to a live cell of layout `Object` or `Indexed` made by [`Obj::new`] whose shape id names a
//! live shape. The owner of an `Obj` keeps it reachable. No collection may run inside a call of this module.
use crate::cell::{CellHeader, Layout};
use crate::heap::Heap;
use crate::shapes::{A_ACC, ShapeId, Shapes};
use crate::string::Str;
use crate::value::{Kind, Value};
use std::ptr::{self, NonNull};

const WORD: usize = 8;
/// Object header flag: the last inline word holds the pointer to a `Slots` record.
const F_OVER: u32 = 1;
/// Object header flag: a script can call the object (a function cell, S2.6). A shape change keeps it.
pub const F_CALLABLE: u32 = 2;
/// Object header flag: the function is a class constructor (a plain call throws).
pub const F_CLASS: u32 = 4;
/// Object header flag: the function is the constructor of a class with `extends` (its `this` is the hole until `super()` returns).
pub const F_DERIVED: u32 = 8;
/// Object header flag: the function is a built-in written in Rust (S2.7). Its slot 0 is the index in `Realm::natives`, not a code index.
pub const F_NATIVE: u32 = 16;
/// `Elements` header flag: the record is sparse.
const F_SPARSE: u32 = 1;
/// A write at an index this far above the capacity of a dense record makes it sparse.
pub const DENSE_GAP: usize = 1024;
/// A dense record never has more values than this.
pub const MAX_DENSE: usize = 1 << 27;
/// Smallest capacities that growth allocates.
const MIN_OVER: usize = 4;
const MIN_ELEMS: usize = 4;
/// Where the values of a record start.
pub(crate) const SLOTS_AT: usize = CellHeader::SIZE;
pub(crate) const ELEMS_AT: usize = CellHeader::SIZE + WORD;

/// The offset of the first inline slot and the number of inline words of an object of `layout` whose shape has `inline` slots.
pub(crate) fn geometry(layout: Layout, inline: u16) -> (usize, usize) {
    match layout {
        Layout::Indexed => (CellHeader::SIZE + WORD, (inline as usize).max(1).next_multiple_of(2)),
        _ => (CellHeader::SIZE, inline as usize | 1),
    }
}

/// The bytes of an object cell before the allocator rounds them up to a size class.
pub fn cell_bytes(layout: Layout, inline: u16) -> usize {
    let (base, cap) = geometry(layout, inline);
    base + WORD * cap
}

/// The number of value words after the header of an object cell: the collector scans all of them.
pub(crate) fn words(layout: Layout, inline: u16) -> usize {
    let (base, cap) = geometry(layout, inline);
    (base - CellHeader::SIZE) / WORD + cap
}

/// A record cell (`Slots` or `Elements`). Only a function of this module makes one.
#[derive(Clone, Copy)]
struct Rec(NonNull<u8>);

impl Rec {
    fn head(self) -> CellHeader {
        // SAFETY: a record cell starts with its header.
        unsafe { self.0.cast::<CellHeader>().read() }
    }

    fn cap(self) -> usize {
        self.head().cap() as usize
    }

    fn value(self) -> Value {
        Value::pointer(Kind::Object, self.0)
    }

    /// The first value of a `Slots` record.
    fn slots(self) -> *mut Value {
        self.0.as_ptr().wrapping_add(SLOTS_AT).cast()
    }

    /// The first value of an `Elements` record.
    fn vals(self) -> *mut Value {
        self.0.as_ptr().wrapping_add(ELEMS_AT).cast()
    }

    fn sparse(self) -> bool {
        self.head().flags() & F_SPARSE != 0
    }

    fn u32_at(self, off: usize) -> *mut u32 {
        self.0.as_ptr().wrapping_add(off).cast()
    }

    /// The array length.
    fn len(self) -> u32 {
        // SAFETY: an `Elements` record has `len` after the header.
        unsafe { self.u32_at(CellHeader::SIZE).read() }
    }

    fn set_len(self, n: u32) {
        // SAFETY: as in `len`.
        unsafe { self.u32_at(CellHeader::SIZE).write(n) }
    }

    /// The pairs of a sparse record.
    fn count(self) -> usize {
        // SAFETY: an `Elements` record has `count` after `len`.
        unsafe { self.u32_at(CellHeader::SIZE + 4).read() as usize }
    }

    fn set_count(self, n: usize) {
        // SAFETY: as in `count`.
        unsafe { self.u32_at(CellHeader::SIZE + 4).write(n as u32) }
    }

    fn get(self, i: usize) -> Value {
        debug_assert!(i < self.cap());
        // SAFETY: `i` is below the capacity, and every value is initialized.
        unsafe { self.vals().add(i).read() }
    }

    fn put(self, i: usize, v: Value) {
        debug_assert!(i < self.cap());
        // SAFETY: as in `get`.
        unsafe { self.vals().add(i).write(v) }
    }

    /// The index of pair `p` of a sparse record.
    fn key(self, p: usize) -> u32 {
        self.get(2 * p).as_f64().expect("a key is a number") as u32
    }

    /// Binary search for index `i` in a sparse record: `Ok(pair)` or `Err(where it goes)`.
    fn find(self, i: u32) -> Result<usize, usize> {
        let (mut lo, mut hi) = (0, self.count());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.key(mid).cmp(&i) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Equal => return Ok(mid),
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        Err(lo)
    }
}

/// A new record of `cap` values, with its first `n` copied from `src` and the rest `fill`. An `Elements` record has `len` and
/// `count` set to 0.
///
/// # Safety
/// `src` points to `n` values (it may be null when `n` is 0), and `n <= cap`.
unsafe fn make_record(heap: &mut Heap, layout: Layout, flags: u32, cap: usize, fill: Value, src: *const Value, n: usize) -> Option<Rec> {
    debug_assert!(n <= cap);
    let cap32 = u32::try_from(cap).ok()?;
    let at = if layout == Layout::Elements { ELEMS_AT } else { SLOTS_AT };
    let p = heap.alloc(at + WORD * cap)?;
    let r = Rec(p);
    // SAFETY: the new cell has room for the header, the prefix and `cap` values; `src` does not overlap it (another cell).
    unsafe {
        p.cast::<CellHeader>().write(CellHeader::record(layout, flags, cap32));
        let v = p.as_ptr().add(at).cast::<Value>();
        if n > 0 {
            ptr::copy_nonoverlapping(src, v, n);
        }
        for i in n..cap {
            v.add(i).write(fill);
        }
    }
    if layout == Layout::Elements {
        r.set_len(0);
        r.set_count(0);
    }
    Some(r)
}

/// An object cell. Copy it freely; see the type invariant in the module comment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Obj(NonNull<u8>);

impl Obj {
    /// The object of a value.
    ///
    /// # Safety
    /// If `v` is a pointer value of kind `Object`, its cell is a live cell of this heap.
    pub unsafe fn from_value(v: Value) -> Option<Obj> {
        let (Kind::Object, p) = v.as_pointer()? else { return None };
        // SAFETY: by the contract of this function.
        let l = unsafe { p.cast::<CellHeader>().read() }.layout();
        matches!(l, Layout::Object | Layout::Indexed).then_some(Obj(p))
    }

    /// The object at a cell address that holds a live object cell of this module.
    #[cfg(test)]
    pub(crate) fn from_cell(p: NonNull<u8>) -> Obj {
        Obj(p)
    }

    pub fn cell(self) -> NonNull<u8> {
        self.0
    }

    #[inline]
    pub fn value(self) -> Value {
        Value::pointer(Kind::Object, self.0)
    }

    /// As [`Obj::from_value`], with the shape id of the object: the interpreter reads the header once for the test and the cache compare.
    ///
    /// # Safety
    /// As for [`Obj::from_value`].
    #[inline]
    pub unsafe fn from_value_shape(v: Value) -> Option<(Obj, u32)> {
        let (Kind::Object, p) = v.as_pointer()? else { return None };
        // SAFETY: by the contract of this function.
        let h = unsafe { p.cast::<CellHeader>().read() };
        matches!(h.layout(), Layout::Object | Layout::Indexed).then_some((Obj(p), h.shape_id()))
    }

    /// Can a script call the object? (A function cell: [`Obj::set_callable`].)
    #[inline]
    pub fn is_callable(self) -> bool {
        self.head().flags() & F_CALLABLE != 0
    }

    pub fn set_callable(self) {
        self.set_flag(F_CALLABLE);
    }

    /// Does the header have the flag bit `f` (`F_CLASS`, `F_DERIVED`)?
    #[inline]
    pub fn has_flag(self, f: u32) -> bool {
        self.head().flags() & f != 0
    }

    /// Set the flag bit `f`. A shape change keeps it.
    pub fn set_flag(self, f: u32) {
        let h = self.head();
        self.set_head(h.with_flags(h.flags() | f));
    }

    /// The place of slot `slot` for every object that has the shape of `self` (a cache entry, S2.6). Bits 0..9: the byte offset in the
    /// cell of the inline word. Above: 0 for an inline slot, else 1 plus the index in the overflow record, and the offset is that of
    /// the last inline word, which holds the pointer to the record. A cache that holds the shape id of an object knows the place without
    /// reading the header flag (see the module comment). Use it for a tree shape only (a dictionary shape may have a record early).
    pub fn loc(self, shapes: &Shapes, slot: u32) -> u32 {
        let h = self.head();
        let s = shapes.of(h);
        debug_assert!(slot < s.slots && !s.is_dict());
        let (base, cap) = geometry(h.layout(), s.inline());
        let i = slot as usize;
        if i < cap - 1 || (i == cap - 1 && s.slots as usize <= cap) {
            (base + WORD * i) as u32
        } else {
            (base + WORD * (cap - 1)) as u32 | ((i - (cap - 1) + 1) as u32) << 9
        }
    }

    /// The value at a place made by [`Obj::loc`].
    ///
    /// # Safety
    /// `loc` was made by `Obj::loc` for an object with the same shape id as `self`, and that shape is a tree shape.
    #[inline]
    pub unsafe fn read_loc(self, loc: u32) -> Value {
        let at = self.0.as_ptr().wrapping_add((loc & 511) as usize).cast::<Value>();
        // SAFETY: by the contract, the word is a slot of this cell, or the pointer to its overflow record.
        unsafe {
            match loc >> 9 {
                0 => at.read(),
                k => Rec(at.read().as_pointer().expect("a record pointer").1).slots().add(k as usize - 1).read(),
            }
        }
    }

    /// Write the value at a place made by [`Obj::loc`].
    ///
    /// # Safety
    /// As for [`Obj::read_loc`].
    #[inline]
    pub unsafe fn write_loc(self, loc: u32, v: Value) {
        let at = self.0.as_ptr().wrapping_add((loc & 511) as usize).cast::<Value>();
        // SAFETY: as in `read_loc`.
        unsafe {
            match loc >> 9 {
                0 => at.write(v),
                k => Rec(at.read().as_pointer().expect("a record pointer").1).slots().add(k as usize - 1).write(v),
            }
        }
    }

    /// Give the object the shape with id `id` that a cache recorded for an add: the new shape has the same layout, the object has room
    /// in its inline slots for its slots (the cache checked that when it filled), and the caller has written the new slot.
    ///
    /// # Safety
    /// `id` is a live tree shape with the layout of the object and no more slots than the inline words, as the cache entry recorded.
    #[inline]
    pub unsafe fn set_shape_id(self, id: u32) {
        let h = self.head();
        self.set_head(CellHeader::object(id, h.layout(), h.flags()));
    }

    #[inline]
    fn head(self) -> CellHeader {
        // SAFETY: by the type invariant `self` is a live object cell, which starts with its header.
        unsafe { self.0.cast::<CellHeader>().read() }
    }

    #[inline]
    fn set_head(self, h: CellHeader) {
        // SAFETY: as in `head`. No reference to the header exists.
        unsafe { self.0.cast::<CellHeader>().write(h) }
    }

    fn set_shape(self, shapes: &Shapes, id: ShapeId) {
        self.set_head(shapes.header(id).with_flags(self.head().flags()));
    }

    /// The first inline slot, and the inline words of the cell.
    #[inline]
    fn inline(self, shapes: &Shapes) -> (*mut Value, usize) {
        let h = self.head();
        let (base, cap) = geometry(h.layout(), shapes.of(h).inline());
        (self.0.as_ptr().wrapping_add(base).cast(), cap)
    }

    /// The record of the overflow slots. The last inline word points to it (the flag is set).
    fn over(inl: *mut Value, cap: usize) -> Rec {
        // SAFETY: with `F_OVER` set the last inline word is the pointer to a `Slots` record.
        let (_, p) = unsafe { inl.add(cap - 1).read() }.as_pointer().expect("a record pointer");
        Rec(p)
    }

    /// A new object with shape `id`. Its slots (the shape's `slots`) are all `undefined`; an `Indexed` object has no elements.
    pub fn new(heap: &mut Heap, shapes: &Shapes, id: ShapeId) -> Option<Obj> {
        let s = shapes.get(id);
        let (base, cap) = geometry(s.layout, s.inline());
        let slots = s.slots as usize;
        let p = heap.alloc(base + WORD * cap)?;
        let rec = if slots > cap {
            // SAFETY: no values are copied.
            Some(unsafe { make_record(heap, Layout::Slots, 0, slots - (cap - 1), Value::UNDEFINED, ptr::null(), 0)? })
        } else {
            None
        };
        let o = Obj(p);
        o.set_head(shapes.header(id).with_flags(rec.is_some() as u32 * F_OVER));
        let v = p.as_ptr().wrapping_add(CellHeader::SIZE).cast::<Value>();
        for i in 0..(base - CellHeader::SIZE) / WORD + cap {
            // SAFETY: the cell has these words after its header.
            unsafe { v.add(i).write(Value::UNDEFINED) };
        }
        if let Some(r) = rec {
            // SAFETY: the last inline word.
            unsafe { o.inline(shapes).0.add(cap - 1).write(r.value()) };
        }
        Some(o)
    }

    /// The shape of the object.
    pub fn shape(self, shapes: &Shapes) -> ShapeId {
        shapes.id_of(self.head())
    }

    /// The prototype of the object.
    pub fn proto(self, shapes: &Shapes) -> Option<NonNull<u8>> {
        shapes.of(self.head()).proto()
    }

    /// Give the object another prototype (`Object.setPrototypeOf`): one shape change, no slot moves.
    pub fn set_proto(self, shapes: &mut Shapes, proto: Option<NonNull<u8>>) {
        let id = shapes.with_proto(shapes.id_of(self.head()), proto);
        self.set_shape(shapes, id);
    }

    /// The location of slot `i`. Panics when the shape has no such slot.
    #[inline]
    fn slot_ptr(self, shapes: &Shapes, i: u32) -> *mut Value {
        let h = self.head();
        let s = shapes.of(h);
        assert!(i < s.slots, "slot {i} is not in the shape");
        let (base, cap) = geometry(h.layout(), s.inline());
        let inl: *mut Value = self.0.as_ptr().wrapping_add(base).cast();
        let i = i as usize;
        if i < cap - 1 || h.flags() & F_OVER == 0 {
            inl.wrapping_add(i)
        } else {
            let r = Obj::over(inl, cap);
            debug_assert!(i - (cap - 1) < r.cap());
            r.slots().wrapping_add(i - (cap - 1))
        }
    }

    /// The value of slot `i` (a slot that the shape has; an accessor has two).
    #[inline]
    pub fn slot(self, shapes: &Shapes, i: u32) -> Value {
        // SAFETY: `slot_ptr` gives a slot of the cell or of its record; every value there is initialized.
        unsafe { self.slot_ptr(shapes, i).read() }
    }

    #[inline]
    pub fn set_slot(self, shapes: &Shapes, i: u32, v: Value) {
        // SAFETY: as in `slot`.
        unsafe { self.slot_ptr(shapes, i).write(v) }
    }

    /// Make room for `to` slots. The object keeps its shape: the caller changes it next. Nothing changes when this fails.
    fn reserve(self, heap: &mut Heap, shapes: &Shapes, to: usize) -> Option<()> {
        let h = self.head();
        let (inl, cap) = self.inline(shapes);
        if to <= cap {
            return Some(());
        }
        let need = to - (cap - 1);
        // SAFETY: the last inline word is a slot of the cell.
        let last = unsafe { inl.add(cap - 1) };
        if h.flags() & F_OVER == 0 {
            // SAFETY: one value is copied, from the last inline word.
            let r = unsafe { make_record(heap, Layout::Slots, 0, need.max(MIN_OVER), Value::UNDEFINED, last, 1)? };
            // SAFETY: `last` is in the cell.
            unsafe { last.write(r.value()) };
            self.set_head(h.with_flags(h.flags() | F_OVER));
        } else {
            let old = Obj::over(inl, cap);
            if need > old.cap() {
                let ncap = need.max(old.cap() + old.cap() / 2);
                // SAFETY: `old` has `old.cap()` initialized values.
                let r = unsafe { make_record(heap, Layout::Slots, 0, ncap, Value::UNDEFINED, old.slots(), old.cap())? };
                // SAFETY: `last` is in the cell.
                unsafe { last.write(r.value()) };
            }
        }
        Some(())
    }

    /// Add the property `key` with attributes `attrs`. `vals` has the value, or the getter and the setter for an accessor
    /// (`A_ACC`). The key is not in the object. Returns the first slot of the property; `None` when out of memory, or when the shapes
    /// are at their cap and no dictionary shape can be made (the object is unchanged).
    pub fn add(self, heap: &mut Heap, shapes: &mut Shapes, key: Str, attrs: u8, vals: &[Value]) -> Option<u32> {
        debug_assert!(vals.len() == 1 + (attrs & A_ACC != 0) as usize);
        let id = shapes.id_of(self.head());
        let s = shapes.get(id);
        let (slots, dict) = (s.slots, s.is_dict());
        self.reserve(heap, shapes, slots as usize + vals.len())?;
        let (new, slot) = if dict {
            (id, shapes.dict_add(id, key, attrs).slot())
        } else if let Some(n) = shapes.add(id, key, attrs) {
            (n, slots)
        } else {
            let d = shapes.to_dict(id);
            if !shapes.get(d).is_dict() {
                return None;
            }
            (d, shapes.dict_add(d, key, attrs).slot())
        };
        self.set_shape(shapes, new);
        for (j, &v) in vals.iter().enumerate() {
            self.set_slot(shapes, slot + j as u32, v);
        }
        Some(slot)
    }

    /// Delete the property `key`. False when the object has no such key. The slots of the property are cleared. In a small tree shape
    /// the later slots move down (`Removed::shifts`); an object that has fallen back into its inline slots drops its overflow record.
    pub fn remove(self, shapes: &mut Shapes, key: Str) -> bool {
        let id = shapes.id_of(self.head());
        let old = shapes.get(id).slots;
        let Some(r) = shapes.remove(id, key) else { return false };
        let (at, w) = (r.entry.slot(), r.entry.width());
        if r.shifts {
            for i in at..old - w {
                self.set_slot(shapes, i, self.slot(shapes, i + w));
            }
            (old - w..old).for_each(|i| self.set_slot(shapes, i, Value::UNDEFINED));
        } else {
            (at..at + w).for_each(|i| self.set_slot(shapes, i, Value::UNDEFINED));
        }
        self.set_shape(shapes, r.shape);
        let h = self.head();
        let (inl, cap) = self.inline(shapes);
        let n = shapes.of(h);
        if !n.is_dict() && h.flags() & F_OVER != 0 && n.slots as usize <= cap {
            // The record's first value is the one that was in the last inline word (`undefined` if that slot is unused now).
            // SAFETY: the record has a first value; the last inline word is in the cell.
            unsafe { inl.add(cap - 1).write(Obj::over(inl, cap).slots().read()) };
            self.set_head(h.with_flags(h.flags() & !F_OVER));
        }
        true
    }

    /// Change the attributes of `key` (`defineProperty`, `freeze`, `seal`): the object gets its own dictionary shape. `vals` are the
    /// new slot values: one for a data property, two (getter, setter) for an accessor, as in [`Obj::add`]. A change between data and
    /// accessor moves the property to fresh slots. False when the object has no such key or no dictionary shape can be made.
    pub fn reconfigure(self, heap: &mut Heap, shapes: &mut Shapes, key: Str, attrs: u8, vals: &[Value]) -> bool {
        debug_assert!(vals.len() == 1 + (attrs & A_ACC != 0) as usize);
        let id = shapes.id_of(self.head());
        let Some(old) = shapes.find(id, key) else { return false };
        if old.width() as usize != vals.len() && self.reserve(heap, shapes, shapes.get(id).slots as usize + vals.len()).is_none() {
            return false;
        }
        let Some((d, old, new)) = shapes.reconfigure(id, key, attrs) else { return false };
        // The old slots are cleared before the shape changes (the old shape has them), the new ones are written after.
        if new.slot() != old.slot() {
            (old.slot()..old.slot() + old.width()).for_each(|i| self.set_slot(shapes, i, Value::UNDEFINED));
        }
        self.set_shape(shapes, d);
        for (j, &v) in vals.iter().enumerate() {
            self.set_slot(shapes, new.slot() + j as u32, v);
        }
        true
    }

    /// The bytes of the overflow record, or 0.
    pub fn overflow_cap(self, shapes: &Shapes) -> usize {
        let (inl, cap) = self.inline(shapes);
        if self.head().flags() & F_OVER == 0 { 0 } else { Obj::over(inl, cap).cap() }
    }

    // ---- elements ----

    fn elems_word(self) -> *mut Value {
        assert!(self.head().layout() == Layout::Indexed, "the object has no elements word");
        self.0.as_ptr().wrapping_add(CellHeader::SIZE).cast()
    }

    fn elems(self) -> Option<Rec> {
        // SAFETY: an `Indexed` object has the elements word after its header.
        let (_, p) = unsafe { self.elems_word().read() }.as_pointer()?;
        Some(Rec(p))
    }

    fn set_elems(self, r: Rec) {
        // SAFETY: as in `elems`.
        unsafe { self.elems_word().write(r.value()) }
    }

    /// Has the object an elements word (is it an array)? `get_element` and `set_element` need it.
    pub fn is_indexed(self) -> bool {
        self.head().layout() == Layout::Indexed
    }

    /// The length of the array. 0 when there are no elements.
    pub fn len(self) -> u32 {
        self.elems().map_or(0, Rec::len)
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// True when the elements are in the sparse form.
    pub fn is_sparse(self) -> bool {
        self.elems().is_some_and(Rec::sparse)
    }

    /// The capacity of the elements record in values (a sparse record: 2 for each pair).
    pub fn elements_cap(self) -> usize {
        self.elems().map_or(0, Rec::cap)
    }

    /// A new `Indexed` object with the elements `values`, in a record of exactly that capacity. `None` when out of memory or when
    /// there are more than [`MAX_DENSE`] values.
    pub fn array(heap: &mut Heap, shapes: &Shapes, id: ShapeId, values: &[Value]) -> Option<Obj> {
        if values.len() > MAX_DENSE {
            return None;
        }
        let o = Obj::new(heap, shapes, id)?;
        assert!(o.head().layout() == Layout::Indexed);
        if !values.is_empty() {
            // SAFETY: `values` has `values.len()` values.
            let r = unsafe { make_record(heap, Layout::Elements, 0, values.len(), Value::HOLE, values.as_ptr(), values.len())? };
            r.set_len(values.len() as u32);
            o.set_elems(r);
        }
        Some(o)
    }

    /// Element `i`, or `None` when it does not exist (a hole, or past the end).
    pub fn get_element(self, i: u32) -> Option<Value> {
        let r = self.elems()?;
        let h = r.head();
        let v = if h.flags() & F_SPARSE != 0 {
            r.get(2 * r.find(i).ok()? + 1)
        } else if (i as usize) < h.cap() as usize {
            r.get(i as usize)
        } else {
            return None;
        };
        (!v.is_hole()).then_some(v)
    }

    /// Set element `i` (not `u32::MAX`, which is no array index); the length becomes at least `i + 1`.
    pub fn set_element(self, heap: &mut Heap, i: u32, v: Value) -> Option<()> {
        assert!(i != u32::MAX, "u32::MAX is not an array index");
        let iu = i as usize;
        let r = match self.elems() {
            Some(r) if r.sparse() => return self.sparse_set(heap, r, i, v),
            Some(r) if iu < r.cap() => r,
            Some(r) if iu - r.cap() < DENSE_GAP && iu < MAX_DENSE => {
                let ncap = (iu + 1).max(r.cap() + r.cap() / 2).max(MIN_ELEMS);
                self.regrow(heap, Some(r), ncap)?
            }
            Some(r) => {
                let s = to_sparse(heap, r)?;
                self.set_elems(s);
                return self.sparse_set(heap, s, i, v);
            }
            None if iu < DENSE_GAP => self.regrow(heap, None, (iu + 1).max(MIN_ELEMS))?,
            None => {
                // SAFETY: no values are copied.
                let s = unsafe { make_record(heap, Layout::Elements, F_SPARSE, 2 * MIN_ELEMS, Value::HOLE, ptr::null(), 0)? };
                self.set_elems(s);
                return self.sparse_set(heap, s, i, v);
            }
        };
        r.put(iu, v);
        r.set_len(r.len().max(i + 1));
        Some(())
    }

    /// A dense record of `ncap` values that replaces `old` (or is the first one), with the same `len`; the object points to it.
    fn regrow(self, heap: &mut Heap, old: Option<Rec>, ncap: usize) -> Option<Rec> {
        let (src, n) = old.map_or((ptr::null(), 0), |r| (r.vals().cast_const(), r.cap()));
        // SAFETY: `old` has `n` initialized values.
        let r = unsafe { make_record(heap, Layout::Elements, 0, ncap, Value::HOLE, src, n)? };
        r.set_len(old.map_or(0, Rec::len));
        self.set_elems(r);
        Some(r)
    }

    fn sparse_set(self, heap: &mut Heap, mut r: Rec, i: u32, v: Value) -> Option<()> {
        match r.find(i) {
            Ok(p) => r.put(2 * p + 1, v),
            Err(p) => {
                let n = r.count();
                if 2 * (n + 1) > r.cap() {
                    let ncap = (2 * (n + 1)).max(r.cap() + r.cap() / 2);
                    // SAFETY: `r` has `r.cap()` initialized values.
                    let g = unsafe { make_record(heap, Layout::Elements, F_SPARSE, ncap, Value::HOLE, r.vals(), r.cap())? };
                    g.set_len(r.len());
                    g.set_count(n);
                    self.set_elems(g);
                    r = g;
                }
                // SAFETY: the record has room for pair `n`; the pairs from `p` move up by one.
                unsafe { ptr::copy(r.vals().add(2 * p), r.vals().add(2 * p + 2), 2 * (n - p)) };
                r.put(2 * p, Value::number(i as f64));
                r.put(2 * p + 1, v);
                r.set_count(n + 1);
                r.set_len(r.len().max(i + 1));
            }
        }
        // One element in four of the indexes up to the last: dense is smaller, and the lookup is a load.
        let n = r.count();
        let last = r.key(n - 1) as usize;
        if last < MAX_DENSE
            && last < 4 * n
            && let Some(d) = to_dense(heap, r, last + 1)
        {
            self.set_elems(d);
        }
        Some(())
    }

    /// Delete element `i`: it becomes a hole. The length stays.
    pub fn delete_element(self, i: u32) {
        let Some(r) = self.elems() else { return };
        if !r.sparse() {
            if (i as usize) < r.cap() {
                r.put(i as usize, Value::HOLE);
            }
        } else if let Ok(p) = r.find(i) {
            let n = r.count();
            // SAFETY: the pairs after `p` move down by one pair.
            unsafe { ptr::copy(r.vals().add(2 * p + 2), r.vals().add(2 * p), 2 * (n - p - 1)) };
            r.put(2 * (n - 1), Value::HOLE);
            r.put(2 * (n - 1) + 1, Value::HOLE);
            r.set_count(n - 1);
        }
    }

    /// Set the array length. A shorter length deletes the elements at or above it. A longer one adds holes: no memory is spent on them.
    pub fn set_len(self, heap: &mut Heap, n: u32) -> Option<()> {
        let r = match self.elems() {
            Some(r) => r,
            None if n == 0 => return Some(()),
            None => self.regrow(heap, None, 0)?,
        };
        if n < r.len() {
            if r.sparse() {
                let p = r.find(n).unwrap_or_else(|p| p);
                (2 * p..2 * r.count()).for_each(|k| r.put(k, Value::HOLE));
                r.set_count(p);
            } else {
                (n as usize..r.cap()).for_each(|k| r.put(k, Value::HOLE));
            }
        }
        r.set_len(n);
        Some(())
    }
}

/// A context: the variables of a scope that a closure captured (`PushCtx`, S2.6). One `Slots` record with no shape: slot 0 is the
/// parent context (or `undefined`), the variables follow. The collector scans it like any record. A script never sees one: a value
/// that points to it is not an object for [`Obj::from_value`].
///
/// Type invariant: it points to a live `Slots` cell made by [`Ctx::new`]. The owner keeps it reachable. No collection may run inside
/// a call of this module.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ctx(NonNull<u8>);

impl Ctx {
    /// A context of `n` slots, all `fill`. `None` when out of memory.
    pub fn new(heap: &mut Heap, n: usize, fill: Value) -> Option<Ctx> {
        // SAFETY: no values are copied.
        let r = unsafe { make_record(heap, Layout::Slots, 0, n, fill, ptr::null(), 0)? };
        Some(Ctx(r.0))
    }

    /// The context of a value.
    ///
    /// # Safety
    /// If `v` is a pointer value of kind `Object` that is not an object, it is a live `Slots` cell made by [`Ctx::new`].
    #[inline]
    pub unsafe fn from_value(v: Value) -> Option<Ctx> {
        let (Kind::Object, p) = v.as_pointer()? else { return None };
        // SAFETY: by the contract of this function.
        let l = unsafe { p.cast::<CellHeader>().read() }.layout();
        (l == Layout::Slots).then_some(Ctx(p))
    }

    #[inline]
    pub fn value(self) -> Value {
        Value::pointer(Kind::Object, self.0)
    }

    /// The number of slots.
    #[inline]
    pub fn len(self) -> usize {
        Rec(self.0).cap()
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn get(self, i: usize) -> Value {
        let r = Rec(self.0);
        assert!(i < r.cap(), "slot {i} of {}", r.cap());
        // SAFETY: `i` is below the capacity, and every value is initialized.
        unsafe { r.slots().add(i).read() }
    }

    #[inline]
    pub fn set(self, i: usize, v: Value) {
        let r = Rec(self.0);
        assert!(i < r.cap(), "slot {i} of {}", r.cap());
        // SAFETY: as in `get`.
        unsafe { r.slots().add(i).write(v) }
    }

    /// A copy of the context (`CopyCtx`). `None` when out of memory.
    pub fn copy(self, heap: &mut Heap) -> Option<Ctx> {
        let r = Rec(self.0);
        // SAFETY: `r` has `cap` initialized values.
        let c = unsafe { make_record(heap, Layout::Slots, 0, r.cap(), Value::UNDEFINED, r.slots(), r.cap())? };
        Some(Ctx(c.0))
    }
}

/// A sparse record with the elements of the dense record `r`.
fn to_sparse(heap: &mut Heap, r: Rec) -> Option<Rec> {
    let n = (0..r.cap()).filter(|&i| !r.get(i).is_hole()).count();
    let cap = (2 * (n + 1)).max(2 * MIN_ELEMS);
    // SAFETY: no values are copied.
    let s = unsafe { make_record(heap, Layout::Elements, F_SPARSE, cap, Value::HOLE, ptr::null(), 0)? };
    for (p, i) in (0..r.cap()).filter(|&i| !r.get(i).is_hole()).enumerate() {
        s.put(2 * p, Value::number(i as f64));
        s.put(2 * p + 1, r.get(i));
    }
    s.set_count(n);
    s.set_len(r.len());
    Some(s)
}

/// A dense record of `cap` values with the pairs of the sparse record `r`.
fn to_dense(heap: &mut Heap, r: Rec, cap: usize) -> Option<Rec> {
    // SAFETY: no values are copied.
    let d = unsafe { make_record(heap, Layout::Elements, 0, cap, Value::HOLE, ptr::null(), 0)? };
    for p in 0..r.count() {
        d.put(r.key(p) as usize, r.get(2 * p + 1));
    }
    d.set_len(r.len());
    Some(d)
}

#[cfg(test)]
#[path = "object_tests.rs"]
mod tests;
