//! Collection inside a task (`docs/design-engine-rewrite.md` 6.8, 6.11 item 1 and section 10 Q11, plan step S1.6): root maps, the
//! frame stack that they describe, and the allocation that collects.
//!
//! **Trigger.** `heap.rs` counts the bytes of cells that the allocator took since the last collection, on its slow path only. The fast
//! path of [`Heap::alloc`] has no counter. Two limits (see [`crate::heap::Trigger`]): [`Heap::due`] is the limit of a collection between
//! tasks, where the host collects with no JS frame (the roots are the globals and the host's roots). [`Heap::try_alloc`] refuses at that
//! limit plus a slack, and [`alloc_in_task`] then collects with the frames as roots. The slack is 2 times the largest task that the
//! journeys make, so a task collects inside itself only when it grows by more than the slack past the between-task limit.
//!
//! **Root map.** The compiler gives each safepoint of a function (an allocation, a call, a loop back edge) the set of registers that
//! hold a value that the program can still read. A [`RootMaps`] is that set for one function: a bitmap of the registers for each
//! safepoint, kept in one `Vec<u64>` and found by the `pc` of the safepoint with a binary search. No frame stores its map: a frame
//! stores its `pc`, and the search runs only during a collection. A register that is not in the map is dead or not written yet. The
//! collector never reads it, so a frame needs no fill at a call (design 6.10 item 4) and the register may hold the pointer of a cell
//! that the last collection freed.
//!
//! **Frames.** [`Frames`] is the JS stack: one `Vec<Value>` for the registers of all frames and a record `{code, pc, base}` for each
//! frame. [`Frames::walk`] marks the live registers where they are stored. It does not copy them into a list. A frame is walked at the
//! `pc` of its safepoint: the allocation for the top frame, the call for each caller. The register that an allocation writes is not
//! live at its own safepoint. A native frame ([`Frames::push_native`]) keeps temporaries of host code: every register of it is a root.
//!
//! **Rules.** A cell pointer in a Rust local or a Rust `Vec` is not a root. It is stale after any call that can collect
//! ([`alloc_in_task`], or a function that calls it). Host code puts such a value in a native frame, or does not keep it across the
//! call. A shape that was made and that no marked object has yet is freed by the collection too: allocate the cell first and make
//! the shape after it, or keep it with [`crate::gc::Marker::shape`].
use crate::atoms::Atoms;
use crate::gc::{Marker, Roots, collect};
use crate::heap::Heap;
use crate::shapes::Shapes;
use crate::value::Value;
use std::ptr::NonNull;

/// The code index of a native frame: every register is live.
pub const NATIVE: u32 = u32::MAX;

/// The root maps of one function: for each safepoint, the registers that hold a live value.
#[derive(Clone, Debug, Default)]
pub struct RootMaps {
    regs: usize,
    /// The `pc` of each safepoint, in increasing order.
    pcs: Vec<u32>,
    /// `regs.div_ceil(64)` words for each safepoint, in the order of `pcs`. Bit `r` of the words is register `r`.
    words: Vec<u64>,
}

/// The live registers of one safepoint.
#[derive(Clone, Copy, Debug)]
pub struct Map<'a>(&'a [u64]);

impl RootMaps {
    /// Maps for a function with `regs` registers.
    pub fn new(regs: usize) -> RootMaps {
        RootMaps { regs, pcs: Vec::new(), words: Vec::new() }
    }

    pub(crate) fn with_capacity(regs: usize, maps: usize) -> RootMaps {
        RootMaps { regs, pcs: Vec::with_capacity(maps), words: Vec::with_capacity(maps * regs.div_ceil(64)) }
    }

    pub(crate) fn single_word(regs: usize, pcs: Vec<u32>, words: Vec<u64>) -> RootMaps {
        debug_assert!((1..=64).contains(&regs) && pcs.len() == words.len());
        RootMaps { regs, pcs, words }
    }

    pub(crate) fn add_words(&mut self, pc: u32, bits: &[u64]) {
        assert!(self.pcs.last().is_none_or(|&last| last < pc), "safepoints are added in the order of pc");
        let width = self.per_map();
        assert!(bits.len() <= width);
        debug_assert!(bits.len() < width || self.regs % 64 == 0 || bits.last().unwrap() >> (self.regs % 64) == 0);
        let end = self.words.len() + width;
        self.pcs.push(pc);
        self.words.extend_from_slice(bits);
        self.words.resize(end, 0);
    }

    fn per_map(&self) -> usize {
        self.regs.div_ceil(64)
    }

    /// Add the safepoint at `pc` with the live registers `live`. The compiler adds them in the order of `pc`. A register must be below
    /// the register count of the function.
    pub fn add(&mut self, pc: u32, live: impl IntoIterator<Item = usize>) {
        assert!(self.pcs.last().is_none_or(|&l| l < pc), "safepoints are added in the order of pc");
        self.pcs.push(pc);
        let at = self.words.len();
        self.words.resize(at + self.per_map(), 0);
        for r in live {
            assert!(r < self.regs, "register {r} of {}", self.regs);
            self.words[at + r / 64] |= 1 << (r % 64);
        }
    }

    /// The map of the safepoint at `pc`.
    pub fn find(&self, pc: u32) -> Option<Map<'_>> {
        let i = self.pcs.binary_search(&pc).ok()?;
        let w = self.per_map();
        Some(Map(&self.words[i * w..(i + 1) * w]))
    }

    /// The register count of the function.
    pub fn regs(&self) -> usize {
        self.regs
    }

    /// The `pc` of each safepoint, in increasing order.
    pub fn pcs(&self) -> &[u32] {
        &self.pcs
    }

    /// Bytes of the maps (compile-time memory).
    pub fn bytes(&self) -> usize {
        self.pcs.capacity() * 4 + self.words.capacity() * 8
    }
}

impl Map<'_> {
    /// The live registers, in increasing order. The cost is the number of words and live registers, not the register count.
    pub fn slots(self) -> impl Iterator<Item = usize> {
        self.0.iter().enumerate().flat_map(|(i, &w)| {
            let mut w = w;
            std::iter::from_fn(move || {
                (w != 0).then(|| {
                    let b = w.trailing_zeros() as usize;
                    w &= w - 1;
                    i * 64 + b
                })
            })
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    /// The index of the function in the code table (the `codes` of [`Frames::walk`]), or [`NATIVE`].
    code: u32,
    /// The `pc` of the safepoint where the frame is stopped. The interpreter stores it at a call, and the allocation slow path stores it
    /// before it allocates.
    pc: u32,
    /// The index of the first register in `values`.
    base: u32,
}

/// The JS stack: the registers of all frames in one buffer, and the records of the frames.
#[derive(Debug, Default)]
pub struct Frames {
    /// Registers. Only `values[..top]` belong to a frame; the rest keeps what earlier frames wrote.
    values: Vec<Value>,
    top: usize,
    records: Vec<Frame>,
}

impl Frames {
    /// Push a frame of `regs` registers for function `code`. The registers are **not** cleared: they hold what an earlier frame left, and
    /// only the root map says which of them the collector may read.
    pub fn push(&mut self, code: u32, regs: usize) {
        debug_assert!(code != NATIVE);
        self.open(code, regs);
    }

    /// Push a native frame: `regs` registers set to `undefined`, all of them roots.
    pub fn push_native(&mut self, regs: usize) {
        self.open(NATIVE, regs);
        self.values[self.top - regs..self.top].fill(Value::UNDEFINED);
    }

    fn open(&mut self, code: u32, regs: usize) {
        self.records.push(Frame { code, pc: 0, base: self.top as u32 });
        self.top += regs;
        if self.top > self.values.len() {
            self.values.resize(self.top, Value::UNDEFINED);
        }
    }

    /// Pop the top frame.
    pub fn pop(&mut self) {
        self.top = self.records.pop().expect("a frame to pop").base as usize;
    }

    /// The number of frames.
    pub fn depth(&self) -> usize {
        self.records.len()
    }

    /// The registers of the top frame.
    #[inline]
    pub fn regs(&mut self) -> &mut [Value] {
        let base = self.records.last().expect("a frame").base as usize;
        &mut self.values[base..self.top]
    }

    /// Say where the top frame is stopped: the `pc` of its safepoint.
    #[inline]
    pub fn set_pc(&mut self, pc: u32) {
        self.records.last_mut().expect("a frame").pc = pc;
    }

    /// Mark the live registers of every frame, in place. `codes[i]` holds the maps of function `i`. A safepoint with no map is a bug of
    /// the compiler, and it stops here: skipping the frame would free values that the program reads.
    pub fn walk(&self, m: &mut Marker, codes: &[RootMaps]) {
        for (i, f) in self.records.iter().enumerate() {
            let end = self.records.get(i + 1).map_or(self.top, |n| n.base as usize);
            let regs = &self.values[f.base as usize..end];
            if f.code == NATIVE {
                m.values(regs);
            } else {
                let map = codes[f.code as usize].find(f.pc).expect("a root map for each safepoint");
                for r in map.slots() {
                    m.value(regs[r]);
                }
            }
        }
    }
}

/// Allocate a cell of at least `size` bytes inside a task. When the heap has passed the trigger, collect first, with `roots`.
///
/// The collection runs before the cell exists, so the cell needs no root. Every other cell and shape that the caller needs after this
/// call must be reachable from `roots`, or it is freed (see the rules in the module comment). `None` when the OS gives no memory.
#[inline]
pub fn alloc_in_task(heap: &mut Heap, shapes: &mut Shapes, atoms: &mut Atoms, roots: &mut impl Roots, size: usize) -> Option<NonNull<u8>> {
    match heap.try_alloc(size) {
        Some(p) => Some(p),
        None => collect_then_alloc(heap, shapes, atoms, roots, size),
    }
}

#[cold]
fn collect_then_alloc(heap: &mut Heap, shapes: &mut Shapes, atoms: &mut Atoms, roots: &mut impl Roots, size: usize) -> Option<NonNull<u8>> {
    if heap.task_due() {
        collect(heap, shapes, atoms, roots);
    }
    heap.alloc(size)
}

#[cfg(test)]
#[allow(unsafe_code)]
#[path = "roots_tests.rs"]
mod tests;
