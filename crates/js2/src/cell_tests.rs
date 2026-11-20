// Tests of `cell.rs`. Miri runs them in CI.
use super::*;

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
fn an_object_header_holds_the_shape_id() {
    let h = CellHeader::object(7, Layout::Object, 3);
    assert_eq!((h.shape_id(), h.shape_key(), h.layout(), h.flags()), (7, 7, Layout::Object, 3));
    // A string never matches the shape of an inline cache.
    assert_eq!(CellHeader::new(Layout::OneByte, 0).shape_key(), 0);
}
