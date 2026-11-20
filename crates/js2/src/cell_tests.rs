// Tests of `cell.rs`. Miri runs them in CI.
use super::*;

fn addr(s: &Shape) -> usize {
    std::ptr::from_ref(s) as usize
}

#[test]
fn the_header_is_8_bytes() {
    assert_eq!(CellHeader::SIZE, 8);
}

#[test]
fn layout_and_flags_round_trip() {
    for layout in [Layout::OneByte, Layout::TwoByte, Layout::Rope, Layout::Indirect] {
        for flags in 0..=FLAG_MAX.min(7) {
            let h = CellHeader::new(layout, flags);
            assert_eq!((h.layout(), h.flags()), (layout, flags));
            // A change of one part keeps the other.
            assert_eq!((h.with_flags(5).layout(), h.with_flags(5).flags()), (layout, 5));
            let o = h.with_layout(Layout::Rope);
            assert_eq!((o.layout(), o.flags()), (Layout::Rope, flags));
        }
    }
}

#[test]
fn a_shape_gives_its_cells_the_layout_flags_and_slots() {
    let mut heap = Heap::default();
    let mut shapes = Shapes::default();
    let a = shapes.add(&mut heap, Shape { layout: Layout::Object, flags: 1, slots: 3 }).unwrap();
    let b = shapes.add(&mut heap, Shape { layout: Layout::Object, flags: 0, slots: 7 }).unwrap();
    let (ha, hb) = (shapes.header(a), shapes.header(b));
    assert_eq!(shapes.len(), 2);
    assert_eq!((ha.layout(), ha.flags(), shapes.of(ha).slots), (Layout::Object, 1, 3));
    assert_eq!((hb.layout(), hb.flags(), shapes.of(hb).slots), (Layout::Object, 0, 7));
    assert_eq!(ha, shapes.header(a));
    assert_ne!(ha, hb);
    assert_ne!(ha.shape_key(), hb.shape_key());
    // A string never matches the shape of an inline cache.
    let s = CellHeader::new(Layout::OneByte, 0);
    assert!(s.shape_key() != ha.shape_key() && s.shape_key() != hb.shape_key());
}

#[test]
fn a_shape_that_a_marked_cell_uses_survives_a_collection() {
    let mut heap = Heap::default();
    let mut shapes = Shapes::default();
    let a = shapes.add(&mut heap, Shape { layout: Layout::Object, flags: 0, slots: 11 }).unwrap();
    let b = shapes.add(&mut heap, Shape { layout: Layout::Object, flags: 0, slots: 22 }).unwrap();
    let (ha, hb) = (shapes.header(a), shapes.header(b));
    let (addr_a, addr_b) = (addr(shapes.of(ha)), addr(shapes.of(hb)));
    let cell = heap.alloc(32).unwrap();
    // SAFETY: the cell has 32 bytes, and the header is its first 8.
    unsafe { cell.as_ptr().cast::<CellHeader>().write(ha) };
    heap.start_mark();
    // SAFETY: `cell` is a live cell of `heap`.
    unsafe { heap.mark(cell) };
    shapes.note_live(&mut heap, ha);
    shapes.finish_marks(&mut heap);
    heap.finish_mark();
    // The lazy sweep frees the shape cell that nothing used: new cells of its class reach it, and never reach the live one.
    let mut seen_b = false;
    for _ in 0..8000 {
        let p = heap.alloc(16).unwrap().as_ptr() as usize;
        assert_ne!(p, addr_a, "a live shape cell was reused");
        seen_b |= p == addr_b;
    }
    assert!(seen_b, "the unused shape cell was not freed");
    assert_eq!(shapes.of(ha).slots, 11);
}
