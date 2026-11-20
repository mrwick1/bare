//! The table of the codes of a program, which can grow while the interpreter runs it (a lazy compile: `Op::Lazy`).
//!
//! The loop of `Vm::run` holds `&Codes`, and so do the runs above it when a native calls a script function (`Realm::call_function`).
//! A lazy compile must still add codes and replace a stub, in any of these runs. So the table is append-only and its entries never
//! move: the table holds a pointer to each `Code`, and a `Code` stays where it was made. A `&Code` is valid while the `Codes` lives.
//!
//! Where the `Code` values live: in *chunks*, buffers of `Code` that never grow after they are made (a loader hands over its
//! `Vec<Code>` as a chunk, without a copy; the spare room at its end serves later single codes). That is one heap block for each
//! compile, not one for each code: a block for each of the 44 functions of the prelude cost 22 K instructions at start (+1.2%).
//!
//! - [`Codes::push_all`] adds the codes of a chunk. [`Codes::set`] puts a new code in a slot and *retires* the old one: a reader that
//!   still holds `&Code` of the old code reads stale, valid data.
//! - [`Codes::free_retired`] needs `&mut self`, so no `&Code` can exist then. It drops the old code (its slot gets an empty `Code`)
//!   and the slot serves the next `set`. A chunk is given back to the allocator only when the `Codes` drops.
//!
//! SAFETY of the whole module. `Codes` has no `Send` or `Sync` (raw pointers and `UnsafeCell`), so one thread uses it. The tables are
//! reached only inside the methods below, and no method calls out while it holds a reference to a table, so no two references to a
//! table exist at once. A slot is read through `&Code` only while it is in the table or retired. It is written only when it is
//! beyond the used part of its chunk (nobody has a pointer to it), or in `free_retired` (`&mut self`: no reader), or when it is in
//! `free` (it left the table and the retired list in `free_retired`, so no reader remains). No method gives `&mut Code`.
use crate::bytecode::Code;
use std::cell::UnsafeCell;
use std::mem::ManuallyDrop;
use std::ops::Index;

/// A buffer of `Code`: `used` slots hold a valid `Code`, up to `cap` are room. It is the buffer of a `Vec<Code>` that `Slots` took.
struct Chunk {
    ptr: *mut Code,
    used: usize,
    cap: usize,
}

/// The slots of a new chunk when a single code does not fit in the room that is left.
const ROOM: usize = 16;

#[derive(Default)]
struct Slots {
    chunks: Vec<Chunk>,
    /// Slots that hold an empty `Code` and are out of the table: `set` takes one before it takes room from a chunk.
    free: Vec<*mut Code>,
    /// Slots that `set` replaced. A `&Code` may still point to one until `free_retired`.
    retired: Vec<*mut Code>,
}

impl Slots {
    /// Take the buffer of `v` as a chunk. Gives the pointers to its codes.
    fn adopt(&mut self, v: Vec<Code>) -> impl Iterator<Item = *mut Code> + use<> {
        let mut v = ManuallyDrop::new(v);
        let (ptr, used, cap) = (v.as_mut_ptr(), v.len(), v.capacity());
        self.chunks.push(Chunk { ptr, used, cap });
        // SAFETY: `i < used <= cap`, inside the buffer.
        (0..used).map(move |i| unsafe { ptr.add(i) })
    }

    /// A slot that holds `c`: a free one, else room in the last chunk, else room in a new chunk.
    fn alloc(&mut self, c: Code) -> *mut Code {
        if let Some(p) = self.free.pop() {
            // SAFETY: a free slot holds an empty valid `Code` and no one reads it (see the module comment).
            unsafe { *p = c };
            return p;
        }
        if self.chunks.last().is_none_or(|k| k.used == k.cap) {
            self.adopt(Vec::with_capacity(ROOM)).for_each(drop);
        }
        let k = self.chunks.last_mut().expect("a chunk with room");
        // SAFETY: `used < cap`, so the slot is inside the buffer, and it is beyond the used part: no pointer to it exists.
        let p = unsafe {
            let p = k.ptr.add(k.used);
            p.write(c);
            p
        };
        k.used += 1;
        p
    }
}

#[derive(Default)]
pub struct Codes {
    table: UnsafeCell<Vec<*mut Code>>,
    slots: UnsafeCell<Slots>,
}

impl Codes {
    pub fn len(&self) -> usize {
        // SAFETY: see the module comment; the reference ends in this statement.
        unsafe { (&*self.table.get()).len() }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, i: usize) -> Option<&Code> {
        // SAFETY: the pointer is copied out of the table (the reference to the table ends here). The slot is not written while it is
        // in the table, so a shared reference to it is valid for the borrow of `self`.
        unsafe { (&*self.table.get()).get(i).map(|&p| &*p) }
    }

    /// The code at `i`, with no bounds check: the loop reads the code of a frame that is on its stack, and `enter!` and `Vm::start` checked
    /// the index when they made the frame.
    ///
    /// # Safety
    /// `i < self.len()`. The table never shrinks, so an index that was checked once stays valid.
    #[inline(always)]
    pub unsafe fn at(&self, i: usize) -> &Code {
        debug_assert!(i < self.len());
        // SAFETY: `i` is in the table by the contract; the rest is as `get`.
        unsafe { &**(&*self.table.get()).get_unchecked(i) }
    }

    /// The codes in index order. The table may grow while the iterator lives: it stops at the length that it saw first.
    pub fn iter(&self) -> impl Iterator<Item = &Code> {
        (0..self.len()).map(|i| &self[i])
    }

    /// Add the codes of `v` at the end of the table and give the index of the first. Takes `&self`: a reader that holds `&Code` is not
    /// affected (only the table of pointers may move, and no one holds it).
    pub fn push_all(&self, v: Vec<Code>) -> usize {
        // SAFETY: no reference to the table or to the slots exists (module comment); `adopt` does not touch a slot that is in use.
        unsafe {
            let (t, s) = (&mut *self.table.get(), &mut *self.slots.get());
            let first = t.len();
            t.extend(s.adopt(v));
            first
        }
    }

    /// Put `c` at index `i` and retire the code that was there. A `&Code` of the old code stays valid until [`Codes::free_retired`].
    /// Panics when `i` is out of range.
    pub fn set(&self, i: usize, c: Code) {
        assert!(i < self.len(), "no code at this index");
        // SAFETY: as `push_all`. The old slot is moved to `retired`, not written.
        unsafe {
            let (t, s) = (&mut *self.table.get(), &mut *self.slots.get());
            let p = s.alloc(c);
            s.retired.push(std::mem::replace(&mut t[i], p));
        }
    }

    /// Whether `set` left a slot for `free_retired`.
    pub fn has_retired(&self) -> bool {
        // SAFETY: see the module comment.
        unsafe { !(&*self.slots.get()).retired.is_empty() }
    }

    /// Drop the codes that `set` replaced and keep their slots for the next `set`. `&mut self` shows that no `&Code` exists.
    pub fn free_retired(&mut self) {
        let s = self.slots.get_mut();
        for p in s.retired.drain(..) {
            // SAFETY: `p` is a slot with a valid `Code` that is in no table and that no reader holds (`&mut self`). The assignment
            // drops the old code and leaves an empty one, so a drop of the chunk finds a valid `Code` in each used slot.
            unsafe { *p = Code::new(0, 0) };
            s.free.push(p);
        }
    }
}

impl Index<usize> for Codes {
    type Output = Code;
    #[inline(always)]
    fn index(&self, i: usize) -> &Code {
        // SAFETY: as `get`.
        unsafe { &*(&*self.table.get())[i] }
    }
}

impl From<Vec<Code>> for Codes {
    fn from(v: Vec<Code>) -> Codes {
        let mut slots = Slots::default();
        let table = slots.adopt(v).collect();
        Codes { table: UnsafeCell::new(table), slots: UnsafeCell::new(slots) }
    }
}

impl Drop for Codes {
    fn drop(&mut self) {
        for k in self.slots.get_mut().chunks.drain(..) {
            // SAFETY: the chunk is the buffer of a `Vec<Code>` that `adopt` took (`ptr` and `cap`), and its first `used` slots hold a
            // valid `Code` each (a retired slot still holds its old code, a free one an empty code).
            drop(unsafe { Vec::from_raw_parts(k.ptr, k.used, k.cap) });
        }
    }
}

impl std::fmt::Debug for Codes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

#[cfg(test)]
#[path = "codes_tests.rs"]
mod tests;
