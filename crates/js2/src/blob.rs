//! A cell of plain words: the compiled program of a RegExp (`docs/design-js2-regexp.md`, decision 4).
//!
//! A [`Blob`] is the header (`Layout::Blob`, the length in words) and then that many `u64` words. It has no edge, so the collector marks it
//! and never traces it. It has no finalizer and no Rust owner: the sweep frees it when no object points to it. A script never sees one:
//! its value has the kind `Object` as a context has (`object.rs`), and `Obj::from_value` does not take it (the layout is not an object).
//!
//! Type invariant of [`Blob`]: it points to a live `Blob` cell made by [`Blob::new`]. The owner keeps it reachable. No collection may
//! run while a slice that [`Blob::words`] gave is in use: the collector runs at the `gate!` of an op only, and a native never reaches it.
use crate::cell::{CellHeader, Layout};
use crate::heap::Heap;
use crate::value::{Kind, Value};
use std::ptr::{self, NonNull};

const WORDS_AT: usize = CellHeader::SIZE;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Blob(NonNull<u8>);

impl Blob {
    /// A new cell with a copy of `words`. `None` when out of memory or when the image is longer than a cell can say.
    pub fn new(heap: &mut Heap, words: &[u64]) -> Option<Blob> {
        let n = u32::try_from(words.len()).ok()?;
        let p = heap.alloc(WORDS_AT + 8 * words.len())?;
        // SAFETY: the new cell has room for the header and `words.len()` words; `words` is another memory (not this cell).
        unsafe {
            p.cast::<CellHeader>().write(CellHeader::blob(n));
            ptr::copy_nonoverlapping(words.as_ptr(), p.as_ptr().add(WORDS_AT).cast::<u64>(), words.len());
        }
        Some(Blob(p))
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

    /// The words of the cell. The slice borrows the handle, not the heap: see the type invariant for how long it is good.
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
