//! Atoms: one cell for each distinct string that is used as a name (`docs/design-engine-rewrite.md` 6.10 item 14, plan step S1.7).
//!
//! Two tables:
//!
//! - [`SharedAtoms`]: the atoms of source text, made once for a session and used by every realm. It owns a [`Heap`] that is never
//!   collected, so a shared atom lives as long as the table, and it has no realm state, so code can name it. Its cells have
//!   [`FLAG_SHARED`]: a realm's collector must not mark them (they are in no heap that it walks). One mutex guards interning.
//!   A shared atom is complete before it is published and never written again, so a realm reads it with no lock.
//! - [`RuntimeAtoms`]: the atoms that a realm makes at run time, in the heap of the realm. The table is weak: it does not keep a
//!   cell alive. [`RuntimeAtoms::sweep`] is the hook for the collector (below).
//!
//! [`Atoms`] joins them for one realm. Its rule keeps one atom for each content in a realm: a lookup goes to the realm table first,
//! then to the shared table, and a new atom is made in the realm only when both miss. If a source atom with the content of a
//! run-time atom appears later, the realm keeps its run-time atom: [`Atoms::adopt`] turns a shared atom into the canonical atom of
//! the realm. Code that is shared by realms therefore names a source atom through `adopt`, once per realm, and never compares a
//! shared atom with a run-time atom by address.
//!
//! The hook for S1.5 (the collector): after the mark phase and before `Heap::finish_mark`, call [`Atoms::sweep`] (or
//! [`RuntimeAtoms::sweep`]) with the heap. It drops the entries whose cell is not marked. The collector must not mark an atom
//! through the table. A shared atom has no mark bit in any realm heap: skip a string when `Str::is_shared` is true.
use crate::heap::Heap;
use crate::string::{Chars, FLAG_ATOM, FLAG_SHARED, R, Str};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, MutexGuard};

const ADDR_MASK: u64 = (1 << 48) - 1;

/// An open-addressing hash table of strings. An entry is the 48-bit address of the cell with the top 16 bits of its hash above it
/// (0 is an empty slot, since no cell has address 0), so a probe that fails does not read the cell.
#[derive(Default)]
struct Table {
    slots: Vec<u64>,
    len: usize,
}

impl Table {
    fn entry(s: Str, hash: u32) -> u64 {
        (hash as u64 >> 16) << 48 | s.cell().as_ptr().expose_provenance() as u64
    }

    fn str_of(e: u64) -> Str {
        // SAFETY (of the `Str` invariant): every entry was made by `entry` from a live atom, and the owner removes an entry
        // before its cell is freed (`sweep`), or never frees it (shared).
        Str::from_cell(NonNull::new(std::ptr::with_exposed_provenance_mut((e & ADDR_MASK) as usize)).expect("not empty"))
    }

    fn find(&self, hash: u32, c: &Chars) -> Option<Str> {
        if self.slots.is_empty() {
            return None;
        }
        let mask = self.slots.len() - 1;
        let tag = (hash >> 16) as u64;
        // Use the high bits for an odd stride. It visits every slot of the power-of-two table and breaks up clusters.
        let step = tag as usize | 1;
        let mut i = hash as usize & mask;
        loop {
            let e = self.slots[i];
            if e == 0 {
                return None;
            }
            if e >> 48 == tag {
                let s = Table::str_of(e);
                if s.chars().is_some_and(|x| x.same(c)) {
                    return Some(s);
                }
            }
            i = (i + step) & mask;
        }
    }

    /// Add an atom that is not in the table. `hash` is its hash.
    fn insert(&mut self, s: Str, hash: u32) {
        if (self.len + 1) * 4 > self.slots.len() * 3 {
            self.rebuild((self.len + 1).next_power_of_two().max(8) * 2, |_| true);
        }
        self.put(Table::entry(s, hash), hash);
        self.len += 1;
    }

    fn put(&mut self, e: u64, hash: u32) {
        let mask = self.slots.len() - 1;
        let step = (hash >> 16) as usize | 1;
        let mut i = hash as usize & mask;
        while self.slots[i] != 0 {
            i = (i + step) & mask;
        }
        self.slots[i] = e;
    }

    /// Rebuild with `cap` slots (a power of two) and the entries for which `keep` is true. Returns how many it dropped.
    fn rebuild(&mut self, cap: usize, mut keep: impl FnMut(Str) -> bool) -> usize {
        let old = std::mem::replace(&mut self.slots, vec![0; cap]);
        let before = self.len;
        self.len = 0;
        for e in old.into_iter().filter(|&e| e != 0) {
            let s = Table::str_of(e);
            if keep(s) {
                self.put(e, s.cached_hash());
                self.len += 1;
            }
        }
        before - self.len
    }
}

struct Shared {
    heap: Heap,
    table: Table,
}

// SAFETY: `Heap` is not `Send` only because it holds raw pointers into its own blocks. The table and its heap are used only
// under the mutex, and no cell is freed before the table is dropped.
unsafe impl Send for Shared {}

/// The atoms of source text, shared by all realms of a session. Put it in an `Arc`.
pub struct SharedAtoms {
    inner: Mutex<Shared>,
}

impl Default for SharedAtoms {
    fn default() -> Self {
        SharedAtoms { inner: Mutex::new(Shared { heap: Heap::default(), table: Table::default() }) }
    }
}

impl SharedAtoms {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        // A panic never leaves the table half changed: `insert` writes the cell before the table.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The shared atom with the units of `c`, if there is one.
    pub fn find(&self, c: &Chars) -> Option<Str> {
        self.lock().table.find(c.hash(), c)
    }

    /// The shared atom with the units of `c`; it is made when it does not exist.
    pub fn intern(&self, c: Chars) -> R<Str> {
        let hash = c.hash();
        let mut g = self.lock();
        if let Some(s) = g.table.find(hash, &c) {
            return Ok(s);
        }
        let s = Str::new(&mut g.heap, c)?;
        s.set_hash(hash);
        s.add_flags(FLAG_SHARED);
        g.table.insert(s, hash);
        Ok(s)
    }

    pub fn len(&self) -> usize {
        self.lock().table.len
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes that the shared heap maps.
    pub fn mapped_bytes(&self) -> usize {
        self.lock().heap.mapped_bytes()
    }
}

/// The weak table of the atoms that one realm makes at run time.
#[derive(Default)]
pub struct RuntimeAtoms {
    table: Table,
}

impl RuntimeAtoms {
    pub fn find(&self, c: &Chars) -> Option<Str> {
        self.table.find(c.hash(), c)
    }

    pub fn len(&self) -> usize {
        self.table.len
    }

    pub fn is_empty(&self) -> bool {
        self.table.len == 0
    }

    /// The hook for the collector: drop the atoms whose cell is not marked. Call it after the mark phase and before
    /// `Heap::finish_mark`, with the heap of this realm. Returns how many entries it dropped. The table shrinks when it is
    /// less than a quarter full.
    pub fn sweep(&mut self, heap: &Heap) -> usize {
        let mut cap = self.table.slots.len();
        let live_cap = |live: usize| (live * 2).next_power_of_two().max(8);
        // The count of live entries is known only after the pass, so the first pass keeps the capacity.
        let dropped = self.table.rebuild(cap, |s| {
            // SAFETY: every atom of the table is a live cell of `heap` until this call frees it by not marking it.
            unsafe { heap.is_marked(s.cell()) }
        });
        if cap > 8 && self.table.len * 4 < cap {
            cap = live_cap(self.table.len);
            self.table.rebuild(cap, |_| true);
        }
        dropped
    }
}

/// The atoms of one realm: its weak table and the shared table.
pub struct Atoms {
    shared: Arc<SharedAtoms>,
    runtime: RuntimeAtoms,
}

impl Atoms {
    pub fn new(shared: Arc<SharedAtoms>) -> Atoms {
        Atoms { shared, runtime: RuntimeAtoms::default() }
    }

    pub fn runtime(&self) -> &RuntimeAtoms {
        &self.runtime
    }

    /// The canonical atom when either table already has these units.
    pub(crate) fn find(&self, c: &Chars) -> Option<Str> {
        let hash = c.hash();
        self.runtime.table.find(hash, c).or_else(|| self.shared.lock().table.find(hash, c))
    }

    /// The atom of this realm with the units of `c`: the realm's own, else the shared one, else a new one in `heap`.
    pub fn intern(&mut self, heap: &mut Heap, c: Chars) -> R<Str> {
        let hash = c.hash();
        if let Some(s) = self.runtime.table.find(hash, &c) {
            return Ok(s);
        }
        if let Some(s) = self.shared.lock().table.find(hash, &c) {
            return Ok(s);
        }
        let s = Str::new(heap, c)?;
        s.set_hash(hash);
        s.add_flags(FLAG_ATOM);
        self.runtime.table.insert(s, hash);
        Ok(s)
    }

    /// The atom of this realm for the units of `s`. A flat string that is not an atom yet becomes the atom in place, with no copy;
    /// a rope is flattened first. A string that is an atom of this realm gives itself.
    pub fn atomize(&mut self, heap: &mut Heap, s: Str) -> R<Str> {
        let s = s.flatten(heap)?;
        if s.is_shared() {
            return Ok(self.adopt(s));
        }
        if s.is_atom() {
            return Ok(s);
        }
        let hash = s.hash(heap)?;
        let c = s.chars().expect("flat");
        if let Some(a) = self.runtime.table.find(hash, &c).or_else(|| self.shared.lock().table.find(hash, &c)) {
            return Ok(a);
        }
        s.add_flags(FLAG_ATOM);
        self.runtime.table.insert(s, hash);
        Ok(s)
    }

    /// The canonical atom of this realm for a shared atom: the realm's own atom with the same units if it has one, else `shared`
    /// itself.
    pub fn adopt(&self, shared: Str) -> Str {
        debug_assert!(shared.is_shared());
        let c = shared.chars().expect("an atom is flat");
        self.runtime.table.find(shared.cached_hash(), &c).unwrap_or(shared)
    }

    /// The hook for the collector: see [`RuntimeAtoms::sweep`].
    pub fn sweep(&mut self, heap: &Heap) -> usize {
        self.runtime.sweep(heap)
    }
}

#[cfg(test)]
#[path = "atoms_tests.rs"]
mod tests;
