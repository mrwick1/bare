//! JavaScript strings in heap cells (`docs/design-engine-rewrite.md` 6.10 items 13 and 14, plan step S1.7).
//!
//! A string is a cell of the allocator in `heap.rs` that starts with a [`CellHeader`] and a prefix of two words: the length in
//! code units and a cached hash (0 until it is computed). The header layout says what follows:
//!
//! | Layout | After the prefix (offset 16) | Cell size |
//! | --- | --- | --- |
//! | `OneByte` | `len` bytes: the units, all below 256 (Latin-1) | `16 + len` |
//! | `TwoByte` | `len` 16-bit units; at least one unit is 256 or more | `16 + 2 len` |
//! | `Rope` | the left and the right string (8-byte addresses) | 32 |
//! | `Indirect` | the flat string that this rope became (and 0) | 32 |
//!
//! - Characters are always inline in the cell: a short string is one allocation of one size class, with no pointer to follow. A
//!   string above [`heap::MAX_SMALL`](crate::heap::MAX_SMALL) takes a large cell of its own.
//! - A flat string has the narrowest form that holds its units. So two flat strings of the same length and different forms are
//!   different strings, and a comparison needs no widening. A rope has the flag `FLAG_WIDE` when it will flatten to two bytes.
//! - [`Str::concat`] copies when the result is shorter than [`ROPE_MIN`] units and otherwise makes a rope node: no copy, one
//!   32-byte cell, so `s += x` in a loop costs O(1) per step. [`Str::flatten`] copies a rope once, with an explicit stack (a rope
//!   may be a million nodes deep), and turns the root into `Indirect`, which drops its two children. Later reads follow the pointer.
//! - Atoms are flat strings with `FLAG_ATOM`; see `atoms.rs`. A string of the shared table also has `FLAG_SHARED`: the collector
//!   never marks it (see [`Str::is_shared`]).
//!
//! Type invariant of [`Str`]: it points to a live string cell. The only way to make one from a [`Value`] is the unsafe
//! [`Str::from_value`]. A collection frees a cell that nothing marks, so the owner of a `Str` keeps it reachable.
//!
//! Threads: a cell is written only by the realm that owns its heap, except that a shared atom is complete (its hash is set) before
//! any other thread sees it, and is never written again.
use crate::cell::{CellHeader, Layout};
use crate::heap::Heap;
use crate::value::{Kind, Value};
use std::ptr::{self, NonNull};

/// The longest string in units (the same limit as V8, so the byte size fits 31 bits).
pub const MAX_LEN: usize = (1 << 29) - 24;
/// A concatenation of fewer units is copied; a longer one is a rope.
pub const ROPE_MIN: usize = 32;

/// A rope that flattens to two bytes.
const FLAG_WIDE: u32 = 1;
/// The string is an atom of its realm: the one cell for its content.
pub const FLAG_ATOM: u32 = 2;
/// The atom lives in the table that all realms share, outside the heap of any realm.
pub const FLAG_SHARED: u32 = 4 | FLAG_ATOM;

#[repr(C)]
struct Prefix {
    head: CellHeader,
    len: u32,
    hash: u32,
}

#[repr(C)]
struct RopeCell {
    prefix: Prefix,
    left: *mut u8,
    right: *mut u8,
}

/// The offset of the characters of a flat string.
const DATA: usize = std::mem::size_of::<Prefix>();
const _: () = assert!(DATA == 16 && std::mem::size_of::<RopeCell>() == 32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The result would have more than [`MAX_LEN`] units.
    TooLong,
    /// The heap gave no memory.
    Memory,
}

pub type R<T> = Result<T, Error>;

/// The units of a flat string, or the units to look up or make a string from.
#[derive(Clone, Copy, Debug)]
pub enum Chars<'a> {
    One(&'a [u8]),
    Two(&'a [u16]),
}

impl Chars<'_> {
    #[inline]
    pub fn len(&self) -> usize {
        match self {
            Chars::One(b) => b.len(),
            Chars::Two(u) => u.len(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn units(&self) -> impl Iterator<Item = u16> + '_ {
        let (a, b) = match self {
            Chars::One(x) => (Some(x.iter().map(|&c| c as u16)), None),
            Chars::Two(x) => (None, Some(x.iter().copied())),
        };
        a.into_iter().flatten().chain(b.into_iter().flatten())
    }

    /// The hash of the units. It does not depend on the form: the same units give the same hash. It is never 0.
    pub fn hash(&self) -> u32 {
        #[inline]
        fn mix(h: u32, u: u32) -> u32 {
            (h.rotate_left(5) ^ u).wrapping_mul(0x9E37_79B9)
        }
        let mut h = self.len() as u32;
        match self {
            Chars::One(b) => b.iter().for_each(|&c| h = mix(h, c as u32)),
            Chars::Two(u) => u.iter().for_each(|&c| h = mix(h, c as u32)),
        }
        h ^= h >> 16;
        h = h.wrapping_mul(0x85EB_CA6B);
        h ^= h >> 13;
        h.max(1)
    }

    /// The same units. The forms may differ.
    pub fn same(&self, other: &Chars) -> bool {
        match (self, other) {
            (Chars::One(a), Chars::One(b)) => a == b,
            (Chars::Two(a), Chars::Two(b)) => a == b,
            _ => self.len() == other.len() && self.units().eq(other.units()),
        }
    }
}

/// A string cell. Copy it freely; see the type invariant in the module comment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Str(NonNull<u8>);

impl Str {
    /// The string of a value.
    ///
    /// # Safety
    /// If `v` is a pointer value of kind `String`, its cell is a live string cell made by this module.
    #[inline]
    pub unsafe fn from_value(v: Value) -> Option<Str> {
        match v.as_pointer() {
            Some((Kind::String, p)) => Some(Str(p)),
            _ => None,
        }
    }

    /// The string at a cell address that holds a live string cell of this module.
    pub(crate) fn from_cell(p: NonNull<u8>) -> Str {
        Str(p)
    }

    #[inline]
    pub fn value(self) -> Value {
        Value::pointer(Kind::String, self.0)
    }

    /// The address of the cell, for the collector (`Heap::mark`).
    #[inline]
    pub fn cell(self) -> NonNull<u8> {
        self.0
    }

    #[inline]
    fn prefix(self) -> *mut Prefix {
        self.0.as_ptr().cast()
    }

    #[inline]
    fn head(self) -> CellHeader {
        // SAFETY: by the type invariant `self` is a live string cell, which starts with a `Prefix`.
        unsafe { ptr::addr_of!((*self.prefix()).head).read() }
    }

    #[inline]
    fn set_head(self, h: CellHeader) {
        // SAFETY: as in `head`. No reference to the header exists.
        unsafe { ptr::addr_of_mut!((*self.prefix()).head).write(h) }
    }

    /// The length in units.
    #[inline]
    pub fn len(self) -> usize {
        // SAFETY: as in `head`.
        unsafe { ptr::addr_of!((*self.prefix()).len).read() as usize }
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// The cached hash; 0 when it is not computed yet. Only a flat string has one.
    #[inline]
    pub fn cached_hash(self) -> u32 {
        // SAFETY: as in `head`.
        unsafe { ptr::addr_of!((*self.prefix()).hash).read() }
    }

    /// Cache the hash of a flat string. A shared atom gets it before it is published, so no write is ever on a shared cell later.
    pub(crate) fn set_hash(self, h: u32) {
        // SAFETY: as in `head`.
        unsafe { ptr::addr_of_mut!((*self.prefix()).hash).write(h) }
    }

    /// True for a rope that no [`Str::flatten`] has copied.
    #[inline]
    pub fn is_rope(self) -> bool {
        self.head().layout() == Layout::Rope
    }

    /// True when the flat form has 16-bit units.
    #[inline]
    pub fn is_two_byte(self) -> bool {
        match self.head().layout() {
            Layout::OneByte => false,
            Layout::TwoByte => true,
            Layout::Rope => self.head().flags() & FLAG_WIDE != 0,
            Layout::Indirect => self.resolve().is_two_byte(),
            Layout::Object => unreachable!("a string cell has a string layout"),
        }
    }

    #[inline]
    pub fn is_atom(self) -> bool {
        self.head().flags() & FLAG_ATOM != 0
    }

    /// True for an atom of the shared table. The collector must not mark or free such a cell: it is outside every heap it walks.
    #[inline]
    pub fn is_shared(self) -> bool {
        self.head().flags() & FLAG_SHARED == FLAG_SHARED
    }

    /// Set `flags` on a flat string (the atom flags).
    pub(crate) fn add_flags(self, flags: u32) {
        let h = self.head();
        self.set_head(h.with_flags(h.flags() | flags));
    }

    /// A rope that was flattened gives its flat string; any other string gives itself.
    #[inline]
    pub fn resolve(self) -> Str {
        if self.head().layout() == Layout::Indirect { self.child(0) } else { self }
    }

    #[inline]
    fn child(self, i: usize) -> Str {
        // SAFETY: only a rope or an indirect string has children, and `i` is 0 or 1; the child is a live string cell.
        unsafe {
            let c = self.0.as_ptr().cast::<RopeCell>();
            let p = if i == 0 { ptr::addr_of!((*c).left).read() } else { ptr::addr_of!((*c).right).read() };
            Str(NonNull::new_unchecked(p))
        }
    }

    /// The strings that this one keeps alive, for the collector to mark: the two parts of a rope, or the flat string of an
    /// indirect one. A flat string has none. A flattened rope no longer keeps its parts.
    pub fn children(self) -> [Option<Str>; 2] {
        match self.head().layout() {
            Layout::Rope => [Some(self.child(0)), Some(self.child(1))],
            Layout::Indirect => [Some(self.child(0)), None],
            _ => [None, None],
        }
    }

    /// The units of a flat string (or of the flat string of an indirect one). `None` for a rope that is not flattened yet.
    ///
    /// The slice is valid until the next collection that does not reach this string.
    #[inline]
    pub fn chars(&self) -> Option<Chars<'_>> {
        let s = self.resolve();
        let n = s.len();
        // SAFETY: a flat string has `n` units of its form after the prefix, aligned (16-byte cells, offset 16).
        unsafe {
            let d = s.0.as_ptr().add(DATA);
            match s.head().layout() {
                Layout::OneByte => Some(Chars::One(std::slice::from_raw_parts(d, n))),
                Layout::TwoByte => Some(Chars::Two(std::slice::from_raw_parts(d.cast::<u16>(), n))),
                _ => None,
            }
        }
    }

    /// The unit at `i` of a flat string; `None` when `i` is out of range or the string is a rope that is not flattened.
    #[inline]
    pub fn unit_at(&self, i: usize) -> Option<u16> {
        match self.chars()? {
            Chars::One(b) => b.get(i).map(|&c| c as u16),
            Chars::Two(u) => u.get(i).copied(),
        }
    }

    fn alloc(heap: &mut Heap, layout: Layout, flags: u32, len: usize, size: usize) -> R<Str> {
        let p = heap.alloc(size).ok_or(Error::Memory)?;
        let s = Str(p);
        // SAFETY: the cell has at least `size >= 16` bytes, and nothing refers to it yet.
        unsafe { s.prefix().write(Prefix { head: CellHeader::new(layout, flags), len: len as u32, hash: 0 }) };
        Ok(s)
    }

    /// A new flat string with the units of `c`, in the narrowest form.
    pub fn new(heap: &mut Heap, c: Chars) -> R<Str> {
        let n = c.len();
        if n > MAX_LEN {
            return Err(Error::TooLong);
        }
        match c {
            Chars::One(b) => {
                let s = Str::alloc(heap, Layout::OneByte, 0, n, DATA + n)?;
                // SAFETY: the new cell has `n` bytes after the prefix; `b` is another memory (the source is not this cell).
                unsafe { ptr::copy_nonoverlapping(b.as_ptr(), s.0.as_ptr().add(DATA), n) };
                Ok(s)
            }
            Chars::Two(u) if u.iter().all(|&x| x < 256) => {
                let s = Str::alloc(heap, Layout::OneByte, 0, n, DATA + n)?;
                for (i, &x) in u.iter().enumerate() {
                    // SAFETY: `i < n`.
                    unsafe { s.0.as_ptr().add(DATA + i).write(x as u8) };
                }
                Ok(s)
            }
            Chars::Two(u) => {
                let s = Str::alloc(heap, Layout::TwoByte, 0, n, DATA + 2 * n)?;
                // SAFETY: the new cell has `n` 16-bit units after the prefix.
                unsafe { ptr::copy_nonoverlapping(u.as_ptr(), s.0.as_ptr().add(DATA).cast::<u16>(), n) };
                Ok(s)
            }
        }
    }

    /// A new flat string from UTF-8 text.
    pub fn from_str(heap: &mut Heap, s: &str) -> R<Str> {
        if s.is_ascii() {
            return Str::new(heap, Chars::One(s.as_bytes()));
        }
        let u: Vec<u16> = s.encode_utf16().collect();
        Str::new(heap, Chars::Two(&u))
    }

    /// `a + b`. The result is a copy when it is shorter than [`ROPE_MIN`], and a rope node otherwise.
    pub fn concat(heap: &mut Heap, a: Str, b: Str) -> R<Str> {
        let (a, b) = (a.resolve(), b.resolve());
        let (la, lb) = (a.len(), b.len());
        if lb == 0 {
            return Ok(a);
        }
        if la == 0 {
            return Ok(b);
        }
        let n = la + lb;
        if n > MAX_LEN {
            return Err(Error::TooLong);
        }
        let wide = a.is_two_byte() || b.is_two_byte();
        if n >= ROPE_MIN {
            let s = Str::alloc(heap, Layout::Rope, if wide { FLAG_WIDE } else { 0 }, n, std::mem::size_of::<RopeCell>())?;
            // SAFETY: the cell is a new rope cell of 32 bytes.
            unsafe {
                let c = s.0.as_ptr().cast::<RopeCell>();
                ptr::addr_of_mut!((*c).left).write(a.0.as_ptr());
                ptr::addr_of_mut!((*c).right).write(b.0.as_ptr());
            }
            return Ok(s);
        }
        // Short, so both are flat: a rope has at least `ROPE_MIN` units.
        let s = Str::alloc(heap, if wide { Layout::TwoByte } else { Layout::OneByte }, 0, n, DATA + (n << wide as usize))?;
        let mut end = n;
        for x in [b, a] {
            end -= x.len();
            s.write_leaf(x, end);
        }
        Ok(s)
    }

    /// Copy the units of the flat string `leaf` to unit `at` of this new flat string.
    fn write_leaf(self, leaf: Str, at: usize) {
        let wide_dst = self.head().layout() == Layout::TwoByte;
        let n = leaf.len();
        // SAFETY: `leaf` is a flat string, `self` has room for `n` units from `at`, and the cells are different.
        unsafe {
            let d = self.0.as_ptr().add(DATA);
            let s = leaf.0.as_ptr().add(DATA);
            match (wide_dst, leaf.head().layout()) {
                (false, Layout::OneByte) => ptr::copy_nonoverlapping(s, d.add(at), n),
                (true, Layout::TwoByte) => ptr::copy_nonoverlapping(s.cast::<u16>(), d.cast::<u16>().add(at), n),
                (true, Layout::OneByte) => (0..n).for_each(|i| d.cast::<u16>().add(at + i).write(s.add(i).read() as u16)),
                _ => unreachable!("a flat string is never wider than the rope that holds it"),
            }
        }
    }

    /// The flat string with the units of this one. A flat string gives itself. A rope is copied once into a new cell, and then
    /// becomes `Indirect`, so a second call, and every other string that holds this rope, finds the copy.
    pub fn flatten(self, heap: &mut Heap) -> R<Str> {
        let root = self.resolve();
        if root.head().layout() != Layout::Rope {
            return Ok(root);
        }
        let wide = root.is_two_byte();
        let n = root.len();
        let flat = Str::alloc(heap, if wide { Layout::TwoByte } else { Layout::OneByte }, 0, n, DATA + (n << wide as usize))?;
        // Fill from the end. The right part is copied at once when it is a leaf and the left part is pushed otherwise, so a rope
        // that grew by `s += x` (left-deep) needs no stack, and the stack of any shape is a heap vector, not the call stack.
        let mut pos = n;
        let mut stack: Vec<Str> = Vec::new();
        let mut node = root;
        loop {
            let node_r = node.resolve();
            if node_r.head().layout() == Layout::Rope {
                let (l, r) = (node_r.child(0), node_r.child(1).resolve());
                if r.head().layout() == Layout::Rope {
                    stack.push(l);
                    node = r;
                } else {
                    pos -= r.len();
                    flat.write_leaf(r, pos);
                    node = l;
                }
                continue;
            }
            pos -= node_r.len();
            flat.write_leaf(node_r, pos);
            match stack.pop() {
                Some(x) => node = x,
                None => break,
            }
        }
        debug_assert_eq!(pos, 0);
        // SAFETY: `root` is a rope cell of 32 bytes. Its header changes layout and keeps its flags; the left word now holds the
        // flat string, the right word 0.
        unsafe {
            let c = root.0.as_ptr().cast::<RopeCell>();
            ptr::addr_of_mut!((*c).left).write(flat.0.as_ptr());
            ptr::addr_of_mut!((*c).right).write(ptr::null_mut());
        }
        root.set_head(root.head().with_layout(Layout::Indirect));
        Ok(flat)
    }

    /// The hash of the units (see [`Chars::hash`]), cached in the cell. A rope is flattened first.
    pub fn hash(self, heap: &mut Heap) -> R<u32> {
        let s = self.flatten(heap)?;
        let h = s.cached_hash();
        if h != 0 {
            return Ok(h);
        }
        let h = s.chars().expect("flat").hash();
        s.set_hash(h);
        Ok(h)
    }

    /// True when the two strings have the same units. A rope is flattened.
    pub fn equals(heap: &mut Heap, a: Str, b: Str) -> R<bool> {
        let (a, b) = (a.resolve(), b.resolve());
        if a == b {
            return Ok(true);
        }
        // The forms are narrowest, so a different length or form means different units.
        if a.len() != b.len() || a.is_two_byte() != b.is_two_byte() {
            return Ok(false);
        }
        let (ha, hb) = (a.cached_hash(), b.cached_hash());
        if ha != 0 && hb != 0 && ha != hb {
            return Ok(false);
        }
        let (a, b) = (a.flatten(heap)?, b.flatten(heap)?);
        Ok(a.chars().expect("flat").same(&b.chars().expect("flat")))
    }
}

#[cfg(test)]
#[path = "string_tests.rs"]
mod tests;
