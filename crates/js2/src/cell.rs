//! The header at the start of every heap cell (`docs/design-engine-rewrite.md` 6.11 item 12), 8 bytes.
//!
//! The header is two 32-bit words (plan step S1.4 chose this form over a raw 8-byte shape pointer; `docs/progress.md`, 2026-11-20):
//! - `shape`: the id of the shape of an object, an index into [`crate::shapes::Shapes`]. A string has shape 0.
//! - `bits`: the layout code in the low 8 bits and 24 flag bits above it. A cell can change its flags (an atom mark, a rope that
//!   becomes `Indirect`) with no change of shape.
//!
//! Code outside this file never reads the header words: it calls [`CellHeader::new`], [`CellHeader::layout`] and
//! [`CellHeader::flags`]. The cell layouts in `string.rs` use [`CellHeader::SIZE`] for every offset. `shapes.rs` makes an object header
//! with [`CellHeader::object`].
//!
//! A cache compares [`CellHeader::shape_key`] (one load, no table). Only code that needs the contents of the shape calls
//! `Shapes::of` (a table load and then the shape). The marker reads the layout from `bits`, not from the shape, and calls
//! `Shapes::note_live`, which sets one bit.

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
    /// A weak table (a `WeakMap` or `WeakSet`): entries of a weak key and a value. The collector treats it as an ephemeron table.
    WeakTable = 17,
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
            17 => Layout::WeakTable,
            // A header of this crate never holds another code.
            _ => unreachable!(),
        }
    }
}

/// The table of shapes moved to `shapes.rs` (S1.8). This keeps the path of S1.4 for the code that names it.
pub use crate::shapes::Shapes;

/// Flag bits that fit the header: 24.
pub const FLAG_MAX: u32 = (1 << 24) - 1;

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

    /// The header of an object cell with shape id `shape` (made by `Shapes::header`).
    #[inline]
    pub(crate) fn object(shape: u32, layout: Layout, flags: u32) -> CellHeader {
        debug_assert!(flags <= FLAG_MAX);
        CellHeader { shape, bits: flags << 8 | layout as u32 }
    }

    /// The shape id of the cell: 0 for a string.
    #[inline]
    pub fn shape_id(self) -> u32 {
        self.shape
    }

    /// The word that an inline cache compares: the shape id.
    #[inline]
    pub fn shape_key(self) -> u64 {
        self.shape as u64
    }
}

const _: () = assert!(CellHeader::SIZE == 8 && std::mem::align_of::<CellHeader>() <= 8);

#[cfg(test)]
#[path = "cell_tests.rs"]
mod tests;
