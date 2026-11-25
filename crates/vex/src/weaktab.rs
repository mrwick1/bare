//! The table of a `WeakMap` or a `WeakSet` (`bi_collections.rs`): an open-addressing hash table over the stub layout that the collector
//! reads as an ephemeron table (`gc.rs`, `Layout::WeakTable`).
//!
//! The cell is the header, `cap: u32` (the number of entries, a power of two), `used: u32`, and then `cap` entries of two values: the key
//! and the value. The key is an object, and the table hashes its address: the heap does not move cells. A key that was never used is
//! [`Value::HOLE`] and ends a probe. A removed entry has the key [`Value::UNDEFINED`] (a tombstone), which is also what the collector
//! writes over an entry whose key died, so it needs no other code here. `used` counts the entries that are not holes; it stays above
//! the live count until a rehash ([`WeakTab::grown`]), and a rehash is due before `used` passes three quarters of `cap`.
//!
//! The collector marks the value of an entry only when the key is marked (a fixed point in the one mark loop, `gc.rs`), so a value
//! that names its own key does not keep it, and the cost of the weak part of a collection grows with the weak entries, not the heap.
//!
//! Type invariant of [`WeakTab`]: it points to a live `WeakTable` cell made by [`WeakTab::new`]. The owner keeps it reachable. No
//! collection may run inside a call of this module.
use crate::cell::{CellHeader, Layout};
use crate::heap::Heap;
use crate::value::{Kind, Value};
use std::ptr::NonNull;

const CAP_AT: usize = CellHeader::SIZE;
const USED_AT: usize = CellHeader::SIZE + 4;
const ENTRIES_AT: usize = 16;
/// The smallest table.
const MIN_CAP: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WeakTab(NonNull<u8>);

/// A 32-bit hash of 64 bits (two multiplications, as the finalizer of MurmurHash3). One multiplication leaves the low bits of the result
/// nearly the same for keys that differ in the low bits only (the integers 0..100000 filled 12% of the buckets).
#[inline]
pub(crate) fn mix(mut x: u64) -> u32 {
    x ^= x >> 32;
    x = x.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 29;
    (x.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) as u32
}

/// The first place that a probe for `key` looks.
#[inline]
fn spot(key: Value, mask: usize) -> usize {
    mix(key.bits()) as usize & mask
}

impl WeakTab {
    /// A table that holds `live` entries below its limit. `None` when out of memory.
    pub fn new(heap: &mut Heap, live: usize) -> Option<WeakTab> {
        let cap = ((live + 1) * 2).next_power_of_two().max(MIN_CAP);
        let p = heap.alloc(ENTRIES_AT + 16 * cap)?;
        // SAFETY: the new cell has room for the header, the two counters and `cap` entries; all of them are written here.
        unsafe {
            p.cast::<CellHeader>().write(CellHeader::new(Layout::WeakTable, 0));
            p.as_ptr().add(CAP_AT).cast::<u32>().write(u32::try_from(cap).ok()?);
            p.as_ptr().add(USED_AT).cast::<u32>().write(0);
            let e = p.as_ptr().add(ENTRIES_AT).cast::<Value>();
            for i in 0..cap {
                e.add(2 * i).write(Value::HOLE);
                e.add(2 * i + 1).write(Value::UNDEFINED);
            }
        }
        Some(WeakTab(p))
    }

    /// The table of a value that [`WeakTab::value`] made.
    ///
    /// # Safety
    /// If `v` is a pointer value of kind `Object`, its cell is a live cell of this heap.
    #[inline]
    pub unsafe fn from_value(v: Value) -> Option<WeakTab> {
        let (Kind::Object, p) = v.as_pointer()? else { return None };
        // SAFETY: by the contract of this function.
        let l = unsafe { p.cast::<CellHeader>().read() }.layout();
        (l == Layout::WeakTable).then_some(WeakTab(p))
    }

    #[inline]
    pub fn value(self) -> Value {
        Value::pointer(Kind::Object, self.0)
    }

    #[inline]
    fn cap(self) -> usize {
        // SAFETY: by the type invariant the cell has `cap` after its header.
        unsafe { self.0.as_ptr().add(CAP_AT).cast::<u32>().read() as usize }
    }

    #[inline]
    fn used(self) -> usize {
        // SAFETY: as in `cap`.
        unsafe { self.0.as_ptr().add(USED_AT).cast::<u32>().read() as usize }
    }

    #[inline]
    fn set_used(self, n: usize) {
        // SAFETY: as in `cap`.
        unsafe { self.0.as_ptr().add(USED_AT).cast::<u32>().write(n as u32) }
    }

    #[inline]
    fn entry(self, i: usize) -> *mut Value {
        debug_assert!(i < self.cap());
        // SAFETY: entry `i` is inside the cell (`i < cap`); the pointer is only computed here.
        unsafe { self.0.as_ptr().add(ENTRIES_AT).cast::<Value>().add(2 * i) }
    }

    #[inline]
    fn key(self, i: usize) -> Value {
        // SAFETY: an entry of the cell holds two initialized values.
        unsafe { self.entry(i).read() }
    }

    /// The index of the entry for `key` (an object), if there is one.
    #[inline]
    pub fn find(self, key: Value) -> Option<usize> {
        let mask = self.cap() - 1;
        let mut i = spot(key, mask);
        // `used < cap` always (a rehash comes first), so the probe meets a hole.
        loop {
            let k = self.key(i);
            if k == key {
                return Some(i);
            }
            if k.is_hole() {
                return None;
            }
            i = (i + 1) & mask;
        }
    }

    /// The value of entry `i`.
    #[inline]
    pub fn val(self, i: usize) -> Value {
        // SAFETY: as in `key`.
        unsafe { self.entry(i).add(1).read() }
    }

    #[inline]
    pub fn set_val(self, i: usize, v: Value) {
        // SAFETY: as in `key`.
        unsafe { self.entry(i).add(1).write(v) }
    }

    /// Remove entry `i`: a tombstone.
    #[inline]
    pub fn remove(self, i: usize) {
        // SAFETY: as in `key`.
        unsafe {
            self.entry(i).write(Value::UNDEFINED);
            self.entry(i).add(1).write(Value::UNDEFINED);
        }
    }

    /// Is a rehash due before the next [`WeakTab::add`]?
    #[inline]
    pub fn full(self) -> bool {
        (self.used() + 1) * 4 > self.cap() * 3
    }

    /// Add `key` with `v`. The key is not in the table, and the table is not [`WeakTab::full`].
    pub fn add(self, key: Value, v: Value) {
        debug_assert!(!self.full() && self.find(key).is_none());
        let mask = self.cap() - 1;
        let mut i = spot(key, mask);
        // The first place that is free: a tombstone is reused, a hole is taken (and counted).
        loop {
            let k = self.key(i);
            if k.is_hole() {
                self.set_used(self.used() + 1);
                break;
            }
            if k.as_pointer().is_none() {
                break;
            }
            i = (i + 1) & mask;
        }
        // SAFETY: as in `key`.
        unsafe {
            self.entry(i).write(key);
            self.entry(i).add(1).write(v);
        }
    }

    /// A new table with the live entries of this one, with room to grow. `None` when out of memory. The old table is garbage.
    pub fn grown(self, heap: &mut Heap) -> Option<WeakTab> {
        let live = (0..self.cap()).filter(|&i| self.key(i).as_pointer().is_some()).count();
        let t = WeakTab::new(heap, live + 1)?;
        for i in (0..self.cap()).filter(|&i| self.key(i).as_pointer().is_some()) {
            t.add(self.key(i), self.val(i));
        }
        Some(t)
    }

    /// The key of the first live entry (the only one of a `WeakRef`).
    pub fn first_key(self) -> Option<Value> {
        (0..self.cap()).map(|i| self.key(i)).find(|k| k.as_pointer().is_some())
    }

    /// The number of live entries.
    pub fn len(self) -> usize {
        (0..self.cap()).filter(|&i| self.key(i).as_pointer().is_some()).count()
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

/// The table of a value, if it is one. The invariant of `Realm::obj` holds: each pointer in a register, a context or an object is a live
/// cell of the realm heap.
#[inline]
pub(crate) fn weaktab_of(v: Value) -> Option<WeakTab> {
    // SAFETY: by the invariant above.
    unsafe { WeakTab::from_value(v) }
}
