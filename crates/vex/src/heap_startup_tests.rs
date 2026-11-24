use super::*;
use crate::cell::{CellHeader, Layout};

#[test]
fn first_block_links_a_prefix_and_extends_at_the_same_growth_budget() {
    let mut heap = Heap::default();
    const SIZE: usize = 32;
    let c = CLASS_OF[SIZE / 16] as usize;
    let first = heap.alloc(SIZE).unwrap();
    let mut p = heap.free[c];
    let mut linked = 1;
    while !p.is_null() {
        // SAFETY: p is a free cell in the first block.
        p = unsafe { p.cast::<*mut u8>().read() };
        linked += 1;
    }
    let full = (BLOCK - FIRST) / SIZE;
    assert!(linked < full);
    assert_eq!(heap.grown_bytes(), full * SIZE);
    let grown = heap.grown_bytes();
    heap.set_trigger(Trigger { min: grown, slack: 0 });
    for i in 1..full {
        let next = heap.try_alloc(SIZE).unwrap();
        assert_eq!(next.as_ptr(), first.as_ptr().wrapping_add(i * SIZE));
    }
    assert!(heap.try_alloc(SIZE).is_none());
    assert_eq!(heap.grown_bytes(), grown);
    assert_eq!(heap.mapped_bytes(), BLOCK);
}

#[test]
fn dead_visitor_skips_the_unlinked_tail_before_and_after_sweep() {
    let mut heap = Heap::default();
    let live = heap.alloc(32).unwrap();
    let dead = heap.alloc(32).unwrap();
    // SAFETY: These cells have room for a cell header and have not been freed.
    unsafe {
        live.cast::<CellHeader>().as_ptr().write(CellHeader::new(Layout::OneByte, 1));
        dead.cast::<CellHeader>().as_ptr().write(CellHeader::new(Layout::OneByte, 2));
    }
    heap.start_mark();
    // SAFETY: live is still allocated in this heap.
    unsafe { heap.mark(live) };
    let mut seen = Vec::new();
    heap.visit_dead(|h| seen.push(h.flags()));
    assert_eq!(seen, [2]);
    heap.finish_mark();

    heap.start_mark();
    seen.clear();
    heap.visit_dead(|h| seen.push(h.flags()));
    assert_eq!(seen, [1]);
    heap.finish_mark();
}
