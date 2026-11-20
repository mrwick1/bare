//! The collector (`docs/design-engine-rewrite.md` 6.9 item 3 and 6.11 item 3, plan step S1.5): a non-moving mark phase over the
//! heap of `heap.rs`, then the weak tables, then the lazy sweep of the heap.
//!
//! One walk. [`collect`] runs these steps, in this order:
//!
//! 1. [`Heap::start_mark`].
//! 2. The roots, walked in place: [`Roots::walk`] calls [`Marker::value`] for each root where it lives. There is no copy of the
//!    roots into a list.
//! 3. The mark loop: pop a cell, trace it by its layout, and mark each cell it names. A cell is marked **before** it is pushed, so
//!    it is pushed once, and a graph with cycles ends. A cell with no edge (a flat string) is marked and never pushed. A shared atom
//!    (`FLAG_SHARED`) is in no heap of a realm and is skipped.
//! 4. Ephemerons, in the same loop (below).
//! 5. [`Shapes::finish_marks`]: free the shapes and tables that no marked object used, and mark the key atoms of the others. The
//!    one cell that a shape names, its prototype, is marked in step 3: the first marked object of a shape gets it from
//!    [`Shapes::note_live`], and the marker marks it and pushes it on the work list. So no shape is traced after the loop, and
//!    the keys are atoms (leaves), so step 5 needs no second loop. It runs before step 6 because the atom sweep must see the keys.
//! 6. [`Atoms::sweep`], the clear of dead entries in marked weak tables, and [`Roots::weak`] for the weak tables of the host.
//! 7. [`Heap::finish_mark`]. The sweep itself is lazy (`heap.rs`).
//!
//! Scratch memory (measured by [`Stats`]): the mark bits are in the block headers (512 bytes for each 64 KiB block). The work
//! list is a stack of chunks of [`CHUNK`] entries. A chunk is allocated when the last one is full and is freed when it is empty,
//! except one spare that stops allocation at a chunk boundary. The list never rescans the heap, and it is dropped at the end of
//! the collection. Its peak is the number of cells that were found and not traced yet, so a long chain needs one entry and a cell
//! with many edges needs one entry for each edge.
//!
//! The typed trace: [`Marker::trace`] reads the layout from the header (`cell.rs`) and calls the code of that layout. S1.9 adds its
//! layouts as arms of that one `match`.
//!
//! - `OneByte`, `TwoByte`: no edge.
//! - `Rope`, `Indirect`: the children that [`Str::children`] names.
//! - `Object`, `Indexed`: the header, then value words (`object.rs`): the inline slots, and for an `Indexed` object the elements
//!   word first. The pointer to the overflow record and the elements word are values like the others, so the records are marked and
//!   pushed as ordinary cells. The marker also marks the prototype of the shape, once for each shape in a collection.
//! - `Slots`, `Elements`: a record of values; the header holds the capacity, and the marker scans all of it (the unused room holds
//!   holes and `undefined`). A long record is scanned in slices of [`SLICE`] values: after a slice the rest of the range waits in a
//!   small stack, and the marker takes the next slice only when the work list is empty. So an array of a million objects puts at most
//!   [`SLICE`] cells on the work list at a time, not a million.
//! - `WeakTable`: the stub of an ephemeron table (below).
//!
//! Ephemerons (a `WeakMap` entry): the value of an entry is alive only when its key is. The table cell is marked and traced like any
//! cell, but its keys are not edges. When the marker traces a table, for each entry
//! - whose key is marked already, it marks the value;
//! - whose key is not marked, it puts the value in `waiting`, under the address of the key. A value that is not a pointer waits
//!   for nothing and is not stored.
//!
//! When the marker pops any cell and `waiting` is not empty, it looks the cell up there and marks the values that wait for it.
//! (Only a cell with a traced layout can be a key, an object or a symbol, so a leaf string is never looked up.) An entry is
//! looked at once when its table is traced and released at most once, so the work is linear in the entries of the reachable tables,
//! in any marking order, and a chain of tables ends when the work list is empty. After the loop, the entries of each marked table
//! whose key is not marked are cleared (the key and the value become `undefined`), and `waiting` is dropped.
//! A cell that is a key and its own value (or is only reached through its value) is dead, as the spec says.
//!
//! Stub layout of `WeakTable` (S1.9 may build its own): the header, `len: u32`, 4 bytes of padding, and then `len` entries of two
//! values, the key and the value. An entry whose key is not a pointer is empty.
//!
//! Collection runs when the caller decides to: between tasks when [`Heap::due`], and inside a task through `roots::alloc_in_task`
//! (S1.6), which collects when the allocation slow path refuses. It never allocates in the heap.
use crate::atoms::Atoms;
use crate::cell::{CellHeader, Layout, Shapes};
use crate::heap::Heap;
use crate::object;
use crate::shapes::ShapeId;
use crate::string::{FLAG_SHARED, Str};
use crate::value::Value;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::ptr::NonNull;

/// Entries of one chunk of the work list (2 KiB).
pub const CHUNK: usize = 256;
/// Values of a record that the marker scans before it goes back to the work list: a record of a million elements puts at most this
/// many cells on the list at a time.
pub const SLICE: usize = 1024;
/// The offset of the first entry of a `WeakTable` stub, and the offset of its length.
const TABLE_ENTRIES: usize = 16;
const TABLE_LEN: usize = CellHeader::SIZE;

/// What a collection counted. Scratch bytes are the peak of memory that the collection allocated outside the heap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Cells of the realm heap that the mark phase marked (the shared atoms and the shapes are not counted).
    pub marked: u64,
    /// Bytes of the cells that are alive after the collection (`Heap::marked_bytes`).
    pub live_bytes: usize,
    /// Peak bytes of the chunks of the work list (a spare chunk counts).
    pub work_bytes: usize,
    /// Peak bytes of the waiting entries of ephemerons, and of the list of marked weak tables.
    pub weak_bytes: usize,
    /// Entries of weak tables that waited for a key, and how many of them a key released.
    pub waited: u64,
    pub released: u64,
    /// Entries of weak tables that the collection cleared.
    pub cleared: u64,
    /// Entries that the runtime atom table dropped.
    pub atoms_dropped: usize,
}

/// The roots of a collection. The owner of the roots (a realm and the host) implements it.
pub trait Roots {
    /// Call [`Marker::value`] for each root, where it is stored.
    fn walk(&mut self, m: &mut Marker);

    /// After the marking and before the sweep: clear the weak references that the host keeps (a `WeakRef` target, a table of DOM
    /// wrappers). [`Live::value`] says whether a cell survives.
    fn weak(&mut self, _live: &Live) {}
}

impl<F: FnMut(&mut Marker)> Roots for F {
    fn walk(&mut self, m: &mut Marker) {
        self(m)
    }
}

/// Whether a value survives this collection. Valid between the end of the marking and [`Heap::finish_mark`].
pub struct Live<'a>(&'a Heap);

impl Live<'_> {
    /// True for a value that is not a pointer, a shared atom, or a marked cell.
    ///
    /// # Safety
    /// A pointer in `v` is a cell of the heap that is being collected and that no earlier collection freed.
    pub unsafe fn value(&self, v: Value) -> bool {
        match v.as_pointer() {
            None => true,
            // SAFETY: by the contract of this function.
            Some((_, p)) => is_shared(p) || unsafe { self.0.is_marked(p) },
        }
    }
}

/// A shared atom: a flat string of the table that all realms share. It is in no heap that this collector walks.
#[inline]
fn is_shared(p: NonNull<u8>) -> bool {
    // SAFETY: `p` is a cell, and the first 8 bytes of a cell are its header.
    let h = unsafe { p.cast::<CellHeader>().read() };
    matches!(h.layout(), Layout::OneByte | Layout::TwoByte) && h.flags() & FLAG_SHARED == FLAG_SHARED
}

/// The work list: a stack in chunks that are allocated on demand.
#[derive(Default)]
struct Work {
    /// Every chunk but the last is full. The last is not empty, unless it is the only one.
    chunks: Vec<Vec<NonNull<u8>>>,
    /// An emptied chunk, kept for the next push.
    spare: Option<Vec<NonNull<u8>>>,
    /// The most chunks that lived at once.
    peak: usize,
}

impl Work {
    #[inline]
    fn push(&mut self, p: NonNull<u8>) {
        match self.chunks.last_mut() {
            Some(c) if c.len() < CHUNK => c.push(p),
            _ => self.push_slow(p),
        }
    }

    #[cold]
    fn push_slow(&mut self, p: NonNull<u8>) {
        let mut c = self.spare.take().unwrap_or_else(|| Vec::with_capacity(CHUNK));
        c.push(p);
        self.chunks.push(c);
        self.peak = self.peak.max(self.chunks.len() + self.spare.is_some() as usize);
    }

    #[inline]
    fn pop(&mut self) -> Option<NonNull<u8>> {
        let c = self.chunks.last_mut()?;
        let p = c.pop();
        if c.is_empty() && self.chunks.len() > 1 {
            self.spare = self.chunks.pop();
        }
        p
    }
}

/// A hash of an address: the cells are 16-aligned, so drop 4 bits and multiply.
#[derive(Default)]
struct AddrHasher(u64);

impl Hasher for AddrHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, _: &[u8]) {
        unreachable!("only usize keys")
    }

    fn write_usize(&mut self, n: usize) {
        self.0 = ((n >> 4) as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        self.0 ^= self.0 >> 32;
    }
}

const NONE: u32 = u32::MAX;

/// Values of ephemeron entries that wait for a key: `head` maps the address of a key to the first of a chain in `nodes`.
#[derive(Default)]
struct Waiting {
    head: HashMap<usize, u32, BuildHasherDefault<AddrHasher>>,
    nodes: Vec<(Value, u32)>,
}

impl Waiting {
    fn add(&mut self, key: NonNull<u8>, v: Value) {
        let next = self.head.insert(key.as_ptr().addr(), self.nodes.len() as u32).unwrap_or(NONE);
        self.nodes.push((v, next));
    }

    fn bytes(&self) -> usize {
        self.head.capacity() * (std::mem::size_of::<(usize, u32)>() + 1) + self.nodes.capacity() * std::mem::size_of::<(Value, u32)>()
    }
}

/// The state of one mark phase. Roots reach it through [`Roots::walk`].
pub struct Marker<'a> {
    heap: &'a mut Heap,
    shapes: &'a mut Shapes,
    work: Work,
    waiting: Waiting,
    /// Marked weak tables: after the marking, the entries with a dead key are cleared.
    tables: Vec<NonNull<u8>>,
    /// The ranges of long records that wait for their next slice: the first value, the next index and the end.
    big: Vec<(*mut Value, usize, usize)>,
    big_peak: usize,
    marked: u64,
    waited: u64,
    released: u64,
    weak_peak: usize,
}

impl Marker<'_> {
    /// Mark a root, or an edge of a cell. A value that is not a pointer is ignored.
    ///
    /// The pointer of `v` is a cell of the heap, or a shared atom. This holds for each value that a cell or a root keeps (the type
    /// invariant of `Value` in a heap that the collector walks); a stale value is a bug of the owner of the root.
    #[inline]
    pub fn value(&mut self, v: Value) {
        if let Some((_, p)) = v.as_pointer() {
            self.cell(p);
        }
    }

    /// Keep a shape that no marked object may use yet: a shape that code or a native function holds before it makes the first object.
    /// Its prototype is marked, as for the first object of the shape. Another shape is freed by `Shapes::finish_marks`.
    pub fn shape(&mut self, id: ShapeId) {
        let h = self.shapes.header(id);
        if let Some(proto) = self.shapes.note_live(self.heap, h) {
            self.cell(proto);
        }
    }

    /// Mark each value of a slice.
    pub fn values(&mut self, vs: &[Value]) {
        for &v in vs {
            self.value(v);
        }
    }

    #[inline]
    fn cell(&mut self, p: NonNull<u8>) {
        // SAFETY: `p` is a cell: its first 8 bytes are its header.
        let h = unsafe { p.cast::<CellHeader>().read() };
        let leaf = matches!(h.layout(), Layout::OneByte | Layout::TwoByte);
        if leaf && h.flags() & FLAG_SHARED == FLAG_SHARED {
            return;
        }
        // SAFETY: `p` is a cell of this heap that is not free (see `value`).
        if unsafe { self.heap.mark(p) } {
            self.marked += 1;
            if !leaf {
                self.work.push(p);
            }
        }
    }

    /// Mark each cell that the work list holds, and what those cells name, until it is empty.
    fn drain(&mut self) {
        loop {
            while let Some(p) = self.work.pop() {
                self.trace(p);
            }
            // The work list is empty: scan one more slice of the last long record that waits.
            match self.big.pop() {
                Some((base, from, end)) => self.scan(base, from, end),
                None => break,
            }
        }
    }

    /// The typed trace of one cell: its layout says where its edges are.
    fn trace(&mut self, p: NonNull<u8>) {
        if !self.waiting.head.is_empty() {
            self.release(p);
        }
        // SAFETY: as in `cell`.
        let h = unsafe { p.cast::<CellHeader>().read() };
        match h.layout() {
            // A leaf is marked and never pushed.
            Layout::OneByte | Layout::TwoByte => {}
            Layout::Rope | Layout::Indirect => {
                for c in Str::from_cell(p).children().into_iter().flatten() {
                    self.cell(c.cell());
                }
            }
            Layout::Object | Layout::Indexed => self.object(p, h),
            Layout::Slots | Layout::Elements => self.record(p, h),
            Layout::WeakTable => self.weak_table(p),
        }
    }

    /// An object (`object.rs`): the words after its header are values, including the elements word and the pointer to the overflow
    /// record, so those two are ordinary edges. The first object of a shape in a collection also marks the prototype of the shape: a
    /// prototype that only a shape names must live.
    fn object(&mut self, p: NonNull<u8>, h: CellHeader) {
        let s = self.shapes.note_live(self.heap, h);
        if let Some(proto) = s {
            self.cell(proto);
        }
        let n = object::words(h.layout(), self.shapes.of(h).inline());
        let words = p.as_ptr().wrapping_add(CellHeader::SIZE).cast::<Value>();
        for i in 0..n {
            // SAFETY: the cell has `n` value words after its header, and each holds a value (`Obj::new` fills them).
            let v = unsafe { words.add(i).read() };
            self.value(v);
        }
    }

    /// A record of values (`Slots` or `Elements`): scan its capacity. A long record is scanned in slices of [`SLICE`] values (below).
    fn record(&mut self, p: NonNull<u8>, h: CellHeader) {
        let at = if h.layout() == Layout::Elements { object::ELEMS_AT } else { object::SLOTS_AT };
        self.scan(p.as_ptr().wrapping_add(at).cast(), 0, h.cap() as usize);
    }

    /// Mark the values `from..end` after `base`, at most [`SLICE`] of them, and keep the rest of the range for later. Each value that
    /// is a new cell goes on the work list, so a slice adds at most [`SLICE`] entries however long the record is.
    fn scan(&mut self, base: *mut Value, from: usize, end: usize) {
        let stop = end.min(from + SLICE);
        for i in from..stop {
            // SAFETY: the record has `end` values from `base`, and each is initialized.
            let v = unsafe { base.add(i).read() };
            self.value(v);
        }
        if stop < end {
            self.big.push((base, stop, end));
            self.big_peak = self.big_peak.max(self.big.len());
        }
    }

    /// The stub weak table: the keys are not edges. Mark the value of each entry whose key is marked, and make the others wait.
    fn weak_table(&mut self, p: NonNull<u8>) {
        self.tables.push(p);
        let (len, entries) = table(p);
        for i in 0..len {
            // SAFETY: the table has `len` entries after its header, and each holds two values.
            let (k, v) = unsafe { (entries.add(2 * i).read(), entries.add(2 * i + 1).read()) };
            let Some((_, key)) = k.as_pointer() else { continue };
            // SAFETY: a key is a cell of this heap that is not free.
            if unsafe { self.heap.is_marked(key) } {
                self.value(v);
            } else if v.as_pointer().is_some() {
                self.waiting.add(key, v);
                self.waited += 1;
            }
        }
        self.weak_peak = self.weak_peak.max(self.waiting.bytes() + self.tables.capacity() * 8);
    }

    /// `p` is traced: mark the values that waited for it as a key.
    fn release(&mut self, p: NonNull<u8>) {
        let Some(mut i) = self.waiting.head.remove(&p.as_ptr().addr()) else { return };
        while i != NONE {
            let (v, next) = self.waiting.nodes[i as usize];
            self.released += 1;
            self.value(v);
            i = next;
        }
    }
}

/// The length and the first entry of a weak table.
fn table(p: NonNull<u8>) -> (usize, *mut Value) {
    // SAFETY: a weak table has its length after the header, and its entries after the length.
    let len = unsafe { p.as_ptr().wrapping_add(TABLE_LEN).cast::<u32>().read() } as usize;
    (len, p.as_ptr().wrapping_add(TABLE_ENTRIES).cast())
}

/// Collect the heap. `roots` marks every root; `atoms` and `shapes` are the tables of the same realm as `heap`. Nothing may
/// allocate in `heap` during the call. Cells that nothing marked are free afterwards (their sweep is lazy).
///
/// The caller keeps the invariant of the roots: each pointer in a root or a cell is a live cell of `heap` or a shared atom.
pub fn collect(heap: &mut Heap, shapes: &mut Shapes, atoms: &mut Atoms, roots: &mut impl Roots) -> Stats {
    heap.start_mark();
    let mut m = Marker { heap, shapes, work: Work::default(), waiting: Waiting::default(), tables: Vec::new(), big: Vec::new(), big_peak: 0, marked: 0, waited: 0, released: 0, weak_peak: 0 };
    roots.walk(&mut m);
    m.drain();
    m.shapes.finish_marks(m.heap);
    let mut cleared = 0;
    for &t in &m.tables {
        let (len, entries) = table(t);
        for i in 0..len {
            // SAFETY: as in `weak_table`; the key of an entry that is a pointer is a cell that is not free yet.
            unsafe {
                if let Some((_, key)) = entries.add(2 * i).read().as_pointer()
                    && !m.heap.is_marked(key)
                {
                    entries.add(2 * i).write(Value::UNDEFINED);
                    entries.add(2 * i + 1).write(Value::UNDEFINED);
                    cleared += 1;
                }
            }
        }
    }
    let atoms_dropped = atoms.sweep(m.heap);
    roots.weak(&Live(m.heap));
    let Marker { heap, work, big_peak, marked, waited, released, weak_peak, .. } = m;
    heap.finish_mark();
    Stats { marked, live_bytes: heap.marked_bytes(), work_bytes: work.peak * CHUNK * std::mem::size_of::<NonNull<u8>>() + big_peak * std::mem::size_of::<(*mut Value, usize, usize)>(), weak_bytes: weak_peak, waited, released, cleared, atoms_dropped }
}

#[cfg(test)]
#[path = "gc_tests.rs"]
mod tests;
