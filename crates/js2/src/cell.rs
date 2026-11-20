//! The header at the start of every heap cell (`docs/design-engine-rewrite.md` 6.11 item 12), 8 bytes.
//!
//! The header holds a layout code and flag bits. Code outside this file never reads the header words: it calls [`CellHeader::new`],
//! [`CellHeader::layout`] and [`CellHeader::flags`]. Plan step S1.4 compares two forms of the first word: a 32-bit shape id (this
//! file, now) and a raw 8-byte shape pointer. If the pointer wins, only this file changes: the header stays 8 bytes, `layout` reads
//! the layout code from the shape that the pointer names, and `flags` moves to the shape or to a second word of the cell. The
//! cell layouts in `string.rs` use [`CellHeader::SIZE`] for every offset.
//!
//! The collector's mark bit is not in the header: it is in the block header of the allocator (`heap.rs`).

/// What the rest of a cell holds. A string layout is below 16; object layouts will use the codes from 16.
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct CellHeader {
    layout: u32,
    flags: u32,
}

impl CellHeader {
    /// The size of the header in bytes.
    pub const SIZE: usize = std::mem::size_of::<CellHeader>();

    #[inline]
    pub const fn new(layout: Layout, flags: u32) -> CellHeader {
        CellHeader { layout: layout as u32, flags }
    }

    #[inline]
    pub fn layout(self) -> Layout {
        match self.layout {
            1 => Layout::OneByte,
            2 => Layout::TwoByte,
            3 => Layout::Rope,
            4 => Layout::Indirect,
            // A header of this crate never holds another code.
            _ => unreachable!("not a string layout"),
        }
    }

    #[inline]
    pub fn flags(self) -> u32 {
        self.flags
    }

    #[inline]
    pub fn with_layout(self, layout: Layout) -> CellHeader {
        CellHeader { layout: layout as u32, ..self }
    }

    #[inline]
    pub fn with_flags(self, flags: u32) -> CellHeader {
        CellHeader { flags, ..self }
    }
}

const _: () = assert!(CellHeader::SIZE == 8 && std::mem::align_of::<CellHeader>() <= 8);
