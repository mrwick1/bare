//! A cell of plain words for RegExp programs, ArrayBuffer bytes, and BigInt magnitudes.
//!
//! A [`Blob`] is the header (`Layout::Blob`, the length in words) and then that many `u64` words. It has no edge, so the collector marks it
//! and never traces it. It has no finalizer. A RegExp program has the kind `Object` as a context has. A BigInt uses its own value tag
//! and puts the sign in a header flag.
//!
//! A Blob points to a live cell made by [`Blob::new`], [`Blob::new_bigint`], or [`Blob::zeroed_bytes`]. The owner keeps it reachable.
//! A byte slice borrows the heap, which prevents collection. A slice from [`Blob::words`] must stay within a region where collection cannot run.
use crate::cell::{CellHeader, Layout};
use crate::heap::Heap;
use crate::value::{Kind, Value};
use std::ptr::{self, NonNull};

const WORDS_AT: usize = CellHeader::SIZE;
const NEGATIVE: u32 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Blob(NonNull<u8>);

impl Blob {
    /// A new cell with a copy of `words`. `None` when out of memory or when the image is longer than a cell can say.
    pub fn new(heap: &mut Heap, words: &[u64]) -> Option<Blob> {
        Self::with_flags(heap, words, 0)
    }

    fn with_flags(heap: &mut Heap, words: &[u64], flags: u32) -> Option<Blob> {
        let n = u32::try_from(words.len()).ok()?;
        let p = heap.alloc(WORDS_AT + 8 * words.len())?;
        // SAFETY: the new cell has room for the header and `words.len()` words; `words` is another memory (not this cell).
        unsafe {
            p.cast::<CellHeader>().write(CellHeader::blob(n).with_flags(flags));
            ptr::copy_nonoverlapping(words.as_ptr(), p.as_ptr().add(WORDS_AT).cast::<u64>(), words.len());
        }
        Some(Blob(p))
    }

    /// Allocate a byte cell. The blob header counts whole words.
    pub(crate) fn zeroed_bytes(heap: &mut Heap, len: usize) -> Option<Blob> {
        let n = len.div_ceil(8);
        let words = u32::try_from(n).ok()?;
        let p = heap.alloc_zeroed(WORDS_AT.checked_add(n.checked_mul(8)?)?)?;
        // SAFETY: the new cell has room for its header.
        unsafe { p.cast::<CellHeader>().write(CellHeader::blob(words)) };
        Some(Blob(p))
    }

    /// # Safety
    /// This is a live byte cell of heap with at least len bytes.
    /// Access to its data must be tied to this borrow of heap.
    pub(crate) unsafe fn bytes<'h>(self, _heap: &'h Heap, len: usize) -> &'h [u8] {
        // SAFETY: the caller guarantees the cell, bounds and borrow.
        unsafe { std::slice::from_raw_parts(self.0.as_ptr().add(WORDS_AT), len) }
    }

    /// # Safety
    /// Follow the requirements of bytes, with exclusive access to the cell.
    pub(crate) unsafe fn bytes_mut<'h>(self, _heap: &'h mut Heap, len: usize) -> &'h mut [u8] {
        // SAFETY: the caller guarantees the cell, bounds and exclusive borrow.
        unsafe { std::slice::from_raw_parts_mut(self.0.as_ptr().add(WORDS_AT), len) }
    }

    /// # Safety
    /// Both byte cells are live in heap and the ranges fit. There
    /// must be no live reference to either range. The ranges can overlap.
    pub(crate) unsafe fn copy_bytes(self, _heap: &mut Heap, from: usize, to: Blob, at: usize, len: usize) {
        // SAFETY: the caller guarantees the cells, ranges and access.
        unsafe { ptr::copy(self.0.as_ptr().add(WORDS_AT + from), to.0.as_ptr().add(WORDS_AT + at), len) }
    }

    /// Store the sign and normalized magnitude of a BigInt.
    pub(crate) fn new_bigint(heap: &mut Heap, a: crate::bigint::Sv<'_>) -> Option<Value> {
        let b = Self::with_flags(heap, a.mag, if a.neg { NEGATIVE } else { 0 })?;
        Some(Value::pointer(Kind::BigInt, b.0))
    }

    /// # Safety
    /// A BigInt value must name a live cell made by `new_bigint`.
    pub(crate) unsafe fn from_bigint(v: Value) -> Option<Blob> {
        let (Kind::BigInt, p) = v.as_pointer()? else { return None };
        Some(Blob(p))
    }

    pub(crate) fn bigint(&self) -> crate::bigint::Sv<'_> {
        // SAFETY: the live cell starts with a header as described in the type invariant.
        let neg = unsafe { self.0.cast::<CellHeader>().read() }.flags() & NEGATIVE != 0;
        crate::bigint::Sv { neg, mag: self.words() }
    }

    /// The blob of a value that [`Blob::value`] made.
    ///
    /// # Safety
    /// If `v` is a pointer value of kind `Object`, its cell is a live cell of this heap.
    #[inline]
    pub unsafe fn from_value(v: Value) -> Option<Blob> {
        let (Kind::Object, p) = v.as_pointer()? else { return None };
        // SAFETY: by the contract of this function.
        let l = unsafe { p.cast::<CellHeader>().read() }.layout();
        (l == Layout::Blob).then_some(Blob(p))
    }

    #[inline]
    pub fn value(self) -> Value {
        Value::pointer(Kind::Object, self.0)
    }

    /// The words of an immutable blob. The owner must keep the cell live for
    /// the borrow. Mutable byte cells use the heap-borrowing methods.
    #[inline]
    pub fn words(&self) -> &[u64] {
        // SAFETY: by the type invariant the cell is live; its header holds the length and the words follow it (`Blob::new`). The cell
        // is 16-aligned and the words start 8 bytes in, so they are 8-aligned. Nothing writes the words after `new`.
        unsafe {
            let n = self.0.cast::<CellHeader>().read().blob_len() as usize;
            std::slice::from_raw_parts(self.0.as_ptr().add(WORDS_AT).cast::<u64>(), n)
        }
    }

    /// The address of the cell, for a table keyed by it.
    #[inline]
    pub fn addr(self) -> usize {
        self.0.as_ptr().addr()
    }
}

#[cfg(test)]
#[path = "blob_tests.rs"]
mod tests;
