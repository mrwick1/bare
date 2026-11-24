//! The allocator of the heap: size-class blocks with lazy sweep (`docs/design-engine-rewrite.md` 6.10 item 15, plan step S1.3).
//!
//! - A block is [`BLOCK`] bytes from the OS (`bare_os::pages`), aligned to its size. Its header holds the cell size and one mark
//!   bit per cell; the cells follow. A cell address with the low bits cleared is its block, so marking needs no table.
//! - A request of at most [`MAX_SMALL`] bytes takes a cell of the smallest of 16 size classes. Each class has a free list that runs
//!   through the free cells (the first 8 bytes of a free cell hold the next one). The fast path pops it.
//! - A larger request gets a mapping of its own, with the same header and one cell.
//! - Collection: [`Heap::start_mark`], then the collector calls [`Heap::mark`] on each live cell, then [`Heap::finish_mark`]. Small
//!   blocks are swept lazily: a block is swept when the class next needs cells, and its unmarked cells join the free list. Large
//!   objects are swept at once.
//! - A block with no live cell after its sweep goes to a pool of [`POOL_KEEP`] blocks; the rest go back to the OS.
//!
//! The trigger (design 6.11 item 1 and section 10 Q11, plan step S1.6) counts bytes of cells that the allocator took since the last
//! collection ([`Heap::grown_bytes`]). The count grows only on the slow path, when a block (a new one or a swept one) puts its free
//! cells on the free list, so the fast path has no counter and no compare. It is an upper bound by at most one block for each class
//! (the cells of a refilled block that no one took yet). The count has two limits, both in [`Trigger`] terms:
//! - [`Heap::due`]: the limit of a collection between tasks. The host asks at a safepoint with an empty JS stack.
//! - [`Heap::try_alloc`] refuses at the task limit. The default starts with a small sample and adjusts after each collection.
//!   A custom trigger uses the between-task limit plus [`Trigger::slack`]. The caller collects with the roots of its frames
//!   (`roots.rs`) and then calls [`Heap::alloc`], which does not check the limit.
//!
//! Address invariant (design 6.1): a [`crate::value::Value`] holds 48 address bits, so a block that the OS maps above that is
//! given back and the allocation fails.
//!
//! The cells hold no meaning here: the content of a new cell is not specified, and the caller writes it.
use bare_os::pages;
use std::ptr::{self, NonNull};

/// The size and alignment of a block.
pub const BLOCK: usize = pages::GRANULE;
/// The largest request that takes a cell of a block.
pub const MAX_SMALL: usize = 8192;
/// Empty blocks kept for reuse; more go back to the OS.
pub const POOL_KEEP: usize = 4;
const CLASSES: [u32; 16] = [16, 32, 48, 64, 80, 96, 128, 160, 192, 256, 384, 512, 1024, 2048, 4096, 8192];
const NCLASS: usize = CLASSES.len();
const MARK_WORDS: usize = 64;
/// Link one 4 KiB prefix, or one cell if it does not fit.
const FIRST_LINK_BYTES: usize = 4096;
/// The default of [`Trigger::min`]: the floor that the old engine uses (`GC_MIN_BYTES`).
pub const TRIGGER_MIN: usize = 8 << 20;
/// Default slack for a fixed [`Trigger`].
pub const TRIGGER_SLACK: usize = 8 << 20;
/// Sample survival before a task builds a large live graph. A second sample gives startup garbage a chance to clear.
const TASK_SAMPLE: usize = 512 << 10;
const TASK_RESAMPLE: usize = 832 << 10;
const TASK_MIN: usize = 2 << 20;
const TASK_RETAINED_HEADROOM: usize = 16 << 20;
/// Amortize repeated scans of a small stable live set over more allocation work.
const TASK_SMALL_LIVE: usize = 64 << 10;
const TASK_GROWTH_PER_LIVE_CELL: usize = 16 << 10;
const ADDRESS_LIMIT: usize = 1 << 48;

#[repr(C)]
struct Header {
    cell: u32,
    /// The small block's cell count, or this large mapping's index in `Heap::large`.
    cells: u32,
    /// Marked cells counted at the end of the last collection.
    live: u32,
    /// Cells in the prefix that has been linked at least once.
    linked: u32,
    /// The length of the mapping: [`BLOCK`] for a block of cells.
    len: usize,
    marks: [u64; MARK_WORDS],
}

/// The offset of the first cell in a block.
const FIRST: usize = std::mem::size_of::<Header>().next_multiple_of(16);
const _: () = assert!((BLOCK - FIRST) / 16 <= MARK_WORDS * 64, "the mark bits cover every cell of the smallest class");

/// The class of each request size, by `(size + 15) / 16`.
const CLASS_OF: [u8; MAX_SMALL / 16 + 1] = {
    let mut t = [0u8; MAX_SMALL / 16 + 1];
    let (mut i, mut c) = (0, 0);
    while i < t.len() {
        while (CLASSES[c] as usize) < i * 16 {
            c += 1;
        }
        t[i] = c as u8;
        i += 1;
    }
    t
};

type Block = NonNull<Header>;

/// The sizes of the trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trigger {
    /// A collection is due when the heap (the live bytes of the last collection and the bytes allocated since) passes the larger of this
    /// and 1.75 times the live bytes. This is the rule of the old engine (`Interp::gc_due`), in bytes of growth.
    pub min: usize,
    /// A task with an explicit trigger may allocate this much past that limit before it collects.
    pub slack: usize,
}

impl Default for Trigger {
    fn default() -> Self {
        Trigger { min: TRIGGER_MIN, slack: TRIGGER_SLACK }
    }
}

pub struct Heap {
    /// The head of the free list of each class; null when it is empty.
    free: [*mut u8; NCLASS],
    /// Blocks swept in this cycle, or made since it began.
    swept: [Vec<Block>; NCLASS],
    /// Blocks that wait for their sweep.
    unswept: [Vec<Block>; NCLASS],
    large: Vec<Block>,
    pool: Vec<Block>,
    mapped: usize,
    /// Bytes of the cells that the last collection marked (see [`Heap::marked_bytes`]).
    marked: usize,
    /// Bytes of cells that the slow path made available since the last collection (see [`Heap::grown_bytes`]).
    grown: usize,
    /// `grown` at which a collection is due, and at which [`Heap::try_alloc`] refuses.
    limit: usize,
    task_limit: usize,
    trigger: Trigger,
    adaptive_task: bool,
}

impl Default for Heap {
    fn default() -> Self {
        let trigger = Trigger::default();
        Heap {
            free: [ptr::null_mut(); NCLASS],
            swept: Default::default(),
            unswept: Default::default(),
            large: Vec::new(),
            pool: Vec::new(),
            mapped: 0,
            marked: 0,
            grown: 0,
            limit: trigger.min,
            task_limit: TASK_SAMPLE,
            trigger,
            adaptive_task: true,
        }
    }
}

impl Heap {
    /// A cell of at least `size` bytes, aligned to 16. `None` when the OS gives no memory or breaks the address invariant.
    #[inline]
    pub fn alloc(&mut self, size: usize) -> Option<NonNull<u8>> {
        if size > MAX_SMALL {
            return self.alloc_large(size, false);
        }
        let c = CLASS_OF[size.div_ceil(16)] as usize;
        let p = self.free[c];
        if p.is_null() {
            return self.alloc_slow(c, false);
        }
        // SAFETY: a free cell holds the next free cell in its first 8 bytes (see `link`).
        self.free[c] = unsafe { p.cast::<*mut u8>().read() };
        // SAFETY: `p` is not null.
        Some(unsafe { NonNull::new_unchecked(p) })
    }

    /// Allocate a zeroed cell. A large cell comes from a new zeroed mapping.
    pub(crate) fn alloc_zeroed(&mut self, size: usize) -> Option<NonNull<u8>> {
        let p = self.alloc(size)?;
        if size <= MAX_SMALL {
            // SAFETY: alloc returned a cell with room for size bytes.
            unsafe { p.as_ptr().write_bytes(0, size) };
        }
        Some(p)
    }

    /// Like [`Heap::alloc`], for code that can collect: `None` when the task limit has passed (see [`Heap::task_due`]) or the OS gives no
    /// memory. The limit is tested on the slow path only, so this costs the same as `alloc` while the free list has cells. After a
    /// refusal at the limit, collect and call [`Heap::alloc`].
    #[inline]
    pub fn try_alloc(&mut self, size: usize) -> Option<NonNull<u8>> {
        if size > MAX_SMALL {
            return self.alloc_large(size, true);
        }
        let c = CLASS_OF[size.div_ceil(16)] as usize;
        let p = self.free[c];
        if p.is_null() {
            return self.alloc_slow(c, true);
        }
        // SAFETY: as in `alloc`.
        self.free[c] = unsafe { p.cast::<*mut u8>().read() };
        // SAFETY: `p` is not null.
        Some(unsafe { NonNull::new_unchecked(p) })
    }

    /// `task`: refuse (return `None`) when the task limit has passed.
    #[cold]
    fn alloc_slow(&mut self, c: usize, task: bool) -> Option<NonNull<u8>> {
        let block = self.swept[c].last().copied().filter(|b| {
            // SAFETY: This is the last swept block of the class, and it belongs to this heap.
            let h = unsafe { b.as_ref() };
            h.linked < h.cells
        });
        if let Some(b) = block {
            // SAFETY: The free list is empty and the tail of this block has never been linked.
            unsafe { self.link(b, c, b.as_ref().cells) };
        } else if task && self.task_due() {
            return None;
        }
        while self.free[c].is_null() {
            match self.unswept[c].pop() {
                Some(b) => self.sweep(b, c),
                None => {
                    let b = self.new_block(c)?;
                    let cell = CLASSES[c] as usize;
                    let cells = (BLOCK - FIRST) / cell;
                    let linked = if self.swept[c].is_empty() { ((FIRST_LINK_BYTES - FIRST) / cell).max(1) } else { cells };
                    // SAFETY: This is a new block. No cell in it has been linked.
                    unsafe { self.link(b, c, linked as u32) };
                    // Charge the whole block now, so extending the list doesn't move the GC trigger.
                    self.grown += cells * cell;
                    self.swept[c].push(b);
                }
            }
        }
        let p = self.free[c];
        // SAFETY: as in `alloc`; the loop ended with a cell on the list.
        self.free[c] = unsafe { p.cast::<*mut u8>().read() };
        // SAFETY: `p` is not null.
        Some(unsafe { NonNull::new_unchecked(p) })
    }

    /// Add cells from the untouched part of a block in address order.
    ///
    /// # Safety
    /// `b` is a block of class `c`, with no active references. The range from its linked count to `end` is free and inside the block.
    unsafe fn link(&mut self, mut b: Block, c: usize, end: u32) {
        // SAFETY: The block is ours and there are no other active references.
        let h = unsafe { b.as_mut() };
        let cell = h.cell as usize;
        let base = b.as_ptr().cast::<u8>().wrapping_add(FIRST);
        let mut p = base.wrapping_add(end as usize * cell);
        let mut head = self.free[c];
        for _ in h.linked..end {
            p = p.wrapping_sub(cell);
            // SAFETY: The cell is inside the block and free, so its first 8 bytes are ours.
            unsafe { p.cast::<*mut u8>().write(head) };
            head = p;
        }
        h.linked = end;
        self.free[c] = head;
    }

    /// Sweep block `b` of class `c`: unmarked cells become free, and the marks are cleared. A block with no mark leaves the class.
    fn sweep(&mut self, b: Block, c: usize) {
        // SAFETY: `b` is a block of this heap, and no other reference to its header is alive.
        let h = unsafe { &mut *b.as_ptr() };
        if h.live == 0 {
            self.release(b);
            return;
        }
        let (cell, cells) = (h.cell as usize, h.cells as usize);
        let marks = &mut h.marks[..cells.div_ceil(64)];
        if h.live == h.cells {
            marks.fill(0);
        } else {
            let base = b.as_ptr().cast::<u8>();
            let mut head = self.free[c];
            for (word, marks) in marks.iter_mut().enumerate().rev() {
                let first = word * 64;
                let mut free = !std::mem::take(marks) & (u64::MAX >> (64 - (cells - first).min(64)));
                while free != 0 {
                    let bit = 63 - free.leading_zeros();
                    free ^= 1u64 << bit;
                    let p = base.wrapping_add(FIRST + (first + bit as usize) * cell);
                    // SAFETY: Each set bit names an unmarked cell in this block. Its cells are on no free list.
                    unsafe { p.cast::<*mut u8>().write(head) };
                    head = p;
                }
            }
            self.free[c] = head;
            h.linked = h.cells;
            self.grown += (cells - h.live as usize) * cell;
        }
        self.swept[c].push(b);
    }

    fn map(&mut self, len: usize) -> Option<NonNull<u8>> {
        let p = pages::map(len).ok()?;
        if p.as_ptr() as usize + len > ADDRESS_LIMIT {
            // SAFETY: the region came from `map` and is not used.
            let _ = unsafe { pages::unmap(p, len) };
            return None;
        }
        self.mapped += len;
        Some(p)
    }

    fn new_block(&mut self, c: usize) -> Option<Block> {
        let b = match self.pool.pop() {
            Some(b) => b,
            None => self.map(BLOCK)?.cast(),
        };
        let cell = CLASSES[c];
        let h = Header { cell, cells: ((BLOCK - FIRST) / cell as usize) as u32, live: 0, linked: 0, len: BLOCK, marks: [0; MARK_WORDS] };
        // SAFETY: the block is ours and starts with room for a header.
        unsafe { b.as_ptr().write(h) };
        Some(b)
    }

    fn release(&mut self, b: Block) {
        if self.pool.len() < POOL_KEEP {
            self.pool.push(b);
        } else {
            self.unmap(b);
        }
    }

    fn unmap(&mut self, b: Block) {
        // SAFETY: `b` is a whole mapping of this heap, and the heap no longer lists it.
        let len = unsafe { (*b.as_ptr()).len };
        // SAFETY: as above.
        let _ = unsafe { pages::unmap(b.cast(), len) };
        self.mapped -= len;
    }

    /// Unmap a large cell that its owner has detached. Small cells wait for the collector.
    ///
    /// # Safety
    /// `p` is a live cell, and no root or cell points to it anymore.
    #[cold]
    pub(crate) unsafe fn release_large(&mut self, p: NonNull<u8>) {
        let base = p.as_ptr().map_addr(|a| a & !(BLOCK - 1)).cast::<Header>();
        // SAFETY: The cell's mapping starts with its header.
        let h = unsafe { &*base };
        if h.cell != 0 {
            return;
        }
        let i = h.cells as usize;
        let Some(&b) = self.large.get(i).filter(|b| b.as_ptr() == base) else {
            return;
        };
        self.large.swap_remove(i);
        if let Some(next) = self.large.get(i) {
            // SAFETY: The swapped mapping belongs to this heap.
            unsafe { (*next.as_ptr()).cells = i as u32 };
        }
        self.unmap(b);
    }

    #[cold]
    fn alloc_large(&mut self, size: usize, task: bool) -> Option<NonNull<u8>> {
        if task && self.task_due() {
            return None;
        }
        let len = (FIRST + size).checked_next_multiple_of(BLOCK)?;
        let b: Block = self.map(len)?.cast();
        self.grown += len;
        // SAFETY: a new mapping with room for the header.
        unsafe { b.as_ptr().write(Header { cell: 0, cells: self.large.len() as u32, live: 0, linked: 0, len, marks: [0; MARK_WORDS] }) };
        self.large.push(b);
        Some(NonNull::new(b.as_ptr().cast::<u8>().wrapping_add(FIRST)).expect("not null"))
    }

    /// Begin a collection: finish the sweep of the last cycle, so every mark bit is clear.
    pub fn start_mark(&mut self) {
        let grown = self.grown;
        for c in 0..NCLASS {
            while let Some(b) = self.unswept[c].pop() {
                self.sweep(b, c);
            }
        }
        // These cells became available during collection, after the allocation sample ended.
        self.grown = grown;
    }

    /// Mark the cell at `p`. True when it was not marked yet.
    ///
    /// # Safety
    /// `p` is a cell that [`Heap::alloc`] of this heap gave, and it is not free.
    #[inline]
    pub unsafe fn mark(&mut self, p: NonNull<u8>) -> bool {
        let h = p.as_ptr().map_addr(|a| a & !(BLOCK - 1)).cast::<Header>();
        // SAFETY: a cell lies in the first block of its mapping, after the header.
        let cell = unsafe { (*h).cell as usize };
        let i = if cell == 0 { 0 } else { (p.as_ptr() as usize - h as usize - FIRST) / cell };
        // SAFETY: as above; `i` is a cell of the block.
        let w = unsafe { &mut (*h).marks[i / 64] };
        let bit = 1u64 << (i % 64);
        let new = *w & bit == 0;
        *w |= bit;
        new
    }

    /// True when the cell at `p` is marked in this collection. A weak table calls it after the marking, before [`Heap::finish_mark`].
    ///
    /// # Safety
    /// As for [`Heap::mark`].
    #[inline]
    pub unsafe fn is_marked(&self, p: NonNull<u8>) -> bool {
        let h = p.as_ptr().map_addr(|a| a & !(BLOCK - 1)).cast::<Header>();
        // SAFETY: a cell lies in the first block of its mapping, after the header.
        let cell = unsafe { (*h).cell as usize };
        let i = if cell == 0 { 0 } else { (p.as_ptr() as usize - h as usize - FIRST) / cell };
        // SAFETY: as above; `i` is a cell of the block.
        unsafe { (*h).marks[i / 64] >> (i % 64) & 1 != 0 }
    }

    /// Visit allocated, unmarked cells after marking and before the lazy sweep.
    pub(crate) fn visit_dead(&self, mut visit: impl FnMut(crate::cell::CellHeader)) {
        for c in 0..NCLASS {
            debug_assert!(self.unswept[c].is_empty(), "start_mark swept all blocks");
            let mut free = self.free[c];
            // A block joins swept when its free cells are prepended to the list. Within the block they follow address order.
            // Cells beyond the linked prefix have never been allocated.
            for &b in self.swept[c].iter().rev() {
                // SAFETY: this block belongs to the heap. The free list separates cells with no object header from allocated cells.
                unsafe {
                    let h = b.as_ref();
                    let base = b.as_ptr().cast::<u8>().add(FIRST);
                    let cells = h.linked as usize;
                    for word in 0..cells.div_ceil(64) {
                        let first = word * 64;
                        let mut unmarked = !h.marks[word] & (u64::MAX >> (64 - (cells - first).min(64)));
                        while unmarked != 0 {
                            let i = first + unmarked.trailing_zeros() as usize;
                            unmarked &= unmarked - 1;
                            let p = base.add(i * h.cell as usize);
                            if p == free {
                                free = p.cast::<*mut u8>().read();
                            } else {
                                visit(p.cast::<crate::cell::CellHeader>().read());
                            }
                        }
                    }
                }
            }
            debug_assert!(free.is_null());
        }
        for &b in &self.large {
            // SAFETY: a large block has one allocated cell.
            unsafe {
                if b.as_ref().marks[0] & 1 == 0 {
                    visit(b.as_ptr().cast::<u8>().add(FIRST).cast::<crate::cell::CellHeader>().read());
                }
            }
        }
    }

    /// End a collection: unmarked large objects go back to the OS, and every block of cells waits for its lazy sweep.
    pub fn finish_mark(&mut self) {
        let mut marked = 0;
        let mut marked_cells = 0;
        for c in 0..NCLASS {
            self.free[c] = ptr::null_mut();
            debug_assert!(self.unswept[c].is_empty());
            std::mem::swap(&mut self.swept[c], &mut self.unswept[c]);
            for &b in &self.unswept[c] {
                // SAFETY: `b` is a block of this heap, with no other reference to its header.
                let h = unsafe { &mut *b.as_ptr() };
                let marks = &h.marks[..(h.cells as usize).div_ceil(64)];
                h.live = if marks.iter().all(|&w| w == 0) { 0 } else { marks.iter().map(|w| w.count_ones()).sum() };
                marked_cells += h.live as usize;
                marked += h.live as usize * h.cell as usize;
            }
        }
        let mut large = std::mem::take(&mut self.large);
        let mut live_index = 0;
        let mut live_bytes = marked;
        large.retain(|&b| {
            // SAFETY: `b` is a large mapping of this heap.
            let h = unsafe { &mut *b.as_ptr() };
            if std::mem::take(&mut h.marks[0]) == 0 {
                self.unmap(b);
                return false;
            }
            live_bytes += h.len;
            h.cells = live_index;
            live_index += 1;
            true
        });
        self.large = large;
        let retained_growth = live_bytes.saturating_sub(self.marked);
        let stable = live_bytes == self.marked;
        let first_sample = self.task_limit == TASK_SAMPLE;
        self.marked = live_bytes;
        self.set_limits();
        if self.adaptive_task {
            // A growing live graph needs more headroom to avoid marking it again too soon.
            // Refills count the free cells of a block, so short intervals can overstate growth.
            self.task_limit = if self.grown != 0 && retained_growth >= self.grown.div_ceil(6) {
                TASK_RETAINED_HEADROOM.max(live_bytes.saturating_mul(2))
            } else if first_sample {
                TASK_RESAMPLE
            } else {
                let budget = TASK_MIN.saturating_sub(live_bytes).max(live_bytes / 4 * 3);
                if stable && live_bytes <= TASK_SMALL_LIVE {
                    budget.max((marked_cells * TASK_GROWTH_PER_LIVE_CELL).min(TASK_RETAINED_HEADROOM))
                } else {
                    budget
                }
            };
        }
        self.grown = 0;
    }

    /// Set the idle and fixed task limits from the last live bytes. Idle collection is due when live + grown passes the larger of
    /// `trigger.min` and 1.75 times live.
    fn set_limits(&mut self) {
        let live = self.marked;
        self.limit = self.trigger.min.max(live / 4 * 7).saturating_sub(live);
        self.task_limit = self.limit.saturating_add(self.trigger.slack);
    }

    /// Stop [`Heap::task_due`] from answering true until [`Heap::release_task`]: a native that calls a script function holds the
    /// collector, so that its Rust locals stay valid (`reenter.rs`). Gives the limit to hand back to `release_task`. The allocation
    /// itself never refuses (`alloc`), so the heap only grows until the call returns.
    pub fn hold_task(&mut self) -> usize {
        std::mem::replace(&mut self.task_limit, usize::MAX)
    }

    /// Hand back the limit that [`Heap::hold_task`] gave. A collection in between would have set it again, and none runs.
    pub fn release_task(&mut self, limit: usize) {
        self.task_limit = limit;
    }

    /// Use a fixed trigger. The limits follow at once.
    pub fn set_trigger(&mut self, t: Trigger) {
        self.trigger = t;
        self.adaptive_task = false;
        self.set_limits();
    }

    /// Bytes of cells that the allocator took since the last collection, counted when a block gives its free cells to the free list
    /// (see the module comment): large objects count their whole mapping.
    pub fn grown_bytes(&self) -> usize {
        self.grown
    }

    /// True when the heap has grown to the limit of a collection between tasks. The host asks at a safepoint with no JS frame.
    #[inline]
    pub fn due(&self) -> bool {
        self.grown >= self.limit
    }

    /// True when the heap has reached the in-task limit.
    #[inline]
    pub fn task_due(&self) -> bool {
        self.grown >= self.task_limit
    }

    /// Use the idle threshold at an in-task safepoint when the task limit is adaptive and collection is permitted.
    pub(crate) fn idle_due_in_task(&self) -> bool {
        self.adaptive_task && self.task_limit != usize::MAX && self.due()
    }

    /// Bytes of the cells that the last [`Heap::finish_mark`] found marked: the size class of a small cell and the whole mapping of a
    /// large one. A cell allocated since then is not counted. With no root, a collection brings it to 0.
    pub fn marked_bytes(&self) -> usize {
        self.marked
    }

    /// Bytes mapped from the OS: blocks in use, pooled blocks and large objects.
    pub fn mapped_bytes(&self) -> usize {
        self.mapped
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        let all: Vec<Block> = self.swept.iter_mut().chain(self.unswept.iter_mut()).flat_map(std::mem::take).chain(std::mem::take(&mut self.large)).chain(std::mem::take(&mut self.pool)).collect();
        for b in all {
            self.unmap(b);
        }
    }
}

#[cfg(test)]
#[path = "heap_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "heap_adaptive_trigger_tests.rs"]
mod adaptive_trigger_tests;

#[cfg(test)]
#[path = "heap_sampling_tests.rs"]
mod sampling_tests;

#[cfg(test)]
#[path = "heap_startup_tests.rs"]
mod startup_tests;
