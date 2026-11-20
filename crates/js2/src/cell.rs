//! The header at the start of every heap cell (`docs/design-engine-rewrite.md` 6.11 item 12), 8 bytes, and the stub of a shape.
//!
//! The header is two 32-bit words (plan step S1.4 chose this form over a raw 8-byte shape pointer; `docs/progress.md`, 2026-11-20):
//! - `shape`: the id of the shape of an object, an index into [`Shapes`]. A string has shape 0.
//! - `bits`: the layout code in the low 8 bits and 24 flag bits above it. A cell can change its flags (an atom mark, a rope that
//!   becomes `Indirect`) with no change of shape.
//!
//! Code outside this file never reads the header words: it calls [`CellHeader::new`], [`CellHeader::layout`] and
//! [`CellHeader::flags`]. The cell layouts in `string.rs` use [`CellHeader::SIZE`] for every offset.
//!
//! A cache compares [`CellHeader::shape_key`] (one load, no table). Only code that needs the contents of the shape calls
//! [`Shapes::of`] (a table load and then the shape). The marker reads the layout from `bits`, not from the shape, and calls
//! [`Shapes::note_live`], which sets one bit. S1.8 replaces the stub [`Shape`] with the real one.
use crate::heap::Heap;
use std::ptr::NonNull;

/// What the rest of a cell holds. A string layout is below 16; object layouts use the codes from 16.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Layout {
    /// A string of 8-bit characters, inline.
    OneByte = 1,
    /// A string of 16-bit characters, inline.
    TwoByte = 2,
    /// A string of two strings, not yet copied.
    Rope = 3,
    /// A rope that was flattened: it points to the flat string.
    Indirect = 4,
    /// An object: its shape says what its slots are.
    Object = 16,
}

impl Layout {
    #[inline]
    const fn from_code(c: u8) -> Layout {
        match c {
            1 => Layout::OneByte,
            2 => Layout::TwoByte,
            3 => Layout::Rope,
            4 => Layout::Indirect,
            16 => Layout::Object,
            // A header of this crate never holds another code.
            _ => unreachable!(),
        }
    }
}

/// The stub of a shape (S1.8 builds the real one): what the header form needs to be compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Shape {
    pub layout: Layout,
    pub flags: u32,
    /// The number of value slots of an object of this shape.
    pub slots: u32,
}

/// The size of the cell of a shape in the heap.
const SHAPE_CELL: usize = 16;
const _: () = assert!(std::mem::size_of::<Shape>() <= SHAPE_CELL);

/// Flag bits that fit the header: 24.
pub const FLAG_MAX: u32 = (1 << 24) - 1;
/// Shape 0 is no shape: every string cell has it.
const NO_SHAPE: Shape = Shape { layout: Layout::OneByte, flags: 0, slots: 0 };
static NONE: Shape = NO_SHAPE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct CellHeader {
    shape: u32,
    bits: u32,
}

impl CellHeader {
    /// The size of the header in bytes.
    pub const SIZE: usize = std::mem::size_of::<CellHeader>();

    #[inline]
    pub fn new(layout: Layout, flags: u32) -> CellHeader {
        debug_assert!(flags <= FLAG_MAX);
        CellHeader { shape: 0, bits: flags << 8 | layout as u32 }
    }

    #[inline]
    pub fn layout(self) -> Layout {
        Layout::from_code(self.bits as u8)
    }

    #[inline]
    pub fn flags(self) -> u32 {
        self.bits >> 8
    }

    #[inline]
    pub fn with_layout(self, layout: Layout) -> CellHeader {
        CellHeader { bits: self.bits & !0xFF | layout as u32, ..self }
    }

    #[inline]
    pub fn with_flags(self, flags: u32) -> CellHeader {
        debug_assert!(flags <= FLAG_MAX);
        CellHeader { bits: flags << 8 | self.bits & 0xFF, ..self }
    }

    /// The word that an inline cache compares: the shape id.
    #[inline]
    pub fn shape_key(self) -> u64 {
        self.shape as u64
    }
}

const _: () = assert!(CellHeader::SIZE == 8 && std::mem::align_of::<CellHeader>() <= 8);

#[derive(Clone, Copy, Debug)]
pub struct ShapeRef(u32);

/// The table from shape id to shape, and the shapes that live cells use.
pub struct Shapes {
    table: Vec<NonNull<Shape>>,
    /// One bit per id: some marked cell has this shape.
    live: Vec<u64>,
}

impl Default for Shapes {
    fn default() -> Self {
        Shapes { table: vec![NonNull::from(&NONE)], live: vec![0] }
    }
}

impl Shapes {
    /// A shape in a cell of `heap`. `None` when the heap gives no memory or the ids are used up.
    pub fn add(&mut self, heap: &mut Heap, shape: Shape) -> Option<ShapeRef> {
        let id = u32::try_from(self.table.len()).ok()?;
        let p = heap.alloc(SHAPE_CELL)?.cast::<Shape>();
        // SAFETY: a new cell of 16 bytes, aligned to 16.
        unsafe { p.as_ptr().write(shape) };
        self.table.push(p);
        if self.table.len() > self.live.len() * 64 {
            self.live.push(0);
        }
        Some(ShapeRef(id))
    }

    /// The header of an object cell with this shape (it reads the shape for the layout code and the flags).
    pub fn header(&self, r: ShapeRef) -> CellHeader {
        let s = self.get(r.0);
        CellHeader { shape: r.0, bits: s.flags << 8 | s.layout as u32 }
    }

    #[inline]
    fn get(&self, id: u32) -> &Shape {
        // SAFETY: a table entry is the static `NONE` or a shape cell that the cells with its id keep alive (`note_live`).
        unsafe { self.table[id as usize].as_ref() }
    }

    /// The shape of a cell: one load from the table, then one from the shape.
    #[inline]
    pub fn of(&self, h: CellHeader) -> &Shape {
        self.get(h.shape)
    }

    /// A marked object cell with header `h` uses its shape: keep it alive. Only a bit is set; the shape is not read.
    #[inline]
    pub fn note_live(&mut self, _heap: &mut Heap, h: CellHeader) {
        let id = h.shape as usize;
        self.live[id / 64] |= 1 << (id % 64);
    }

    /// After the marking: mark each shape that a cell used, and clear the bits.
    pub fn finish_marks(&mut self, heap: &mut Heap) {
        for (w, bits) in self.live.iter_mut().enumerate() {
            let mut b = std::mem::take(bits);
            while b != 0 {
                let id = w * 64 + b.trailing_zeros() as usize;
                b &= b - 1;
                if id != 0 {
                    // SAFETY: the table entry is a shape cell of `heap`, and it is not free: a live cell used it.
                    unsafe { heap.mark(self.table[id].cast()) };
                }
            }
        }
    }

    pub fn count(&self) -> usize {
        self.table.len() - 1
    }

    /// Bytes of the table and the bits, outside the heap.
    pub fn table_bytes(&self) -> usize {
        self.table.capacity() * 8 + self.live.capacity() * 8
    }
}

#[cfg(test)]
#[path = "cell_tests.rs"]
mod tests;
