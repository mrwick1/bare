// Tests of `heap.rs`. Miri runs them in CI (blocks come from `std::alloc` there), so the counts stay small.
use super::*;
use std::collections::HashSet;

fn addr(p: NonNull<u8>) -> usize {
    p.as_ptr() as usize
}

/// Fill the `size` bytes at `p` with `tag`.
fn fill(p: NonNull<u8>, size: usize, tag: u8) {
    // SAFETY: the heap gave at least `size` bytes at `p`.
    unsafe { ptr::write_bytes(p.as_ptr(), tag, size) };
}

fn holds(p: NonNull<u8>, size: usize, tag: u8) -> bool {
    // SAFETY: as in `fill`.
    unsafe { std::slice::from_raw_parts(p.as_ptr(), size) }.iter().all(|&b| b == tag)
}

#[test]
fn each_request_takes_the_smallest_class_that_fits() {
    for (size, want) in [(0, 16), (1, 16), (16, 16), (17, 32), (100, 128), (257, 384), (1025, 2048), (8192, 8192)] {
        assert_eq!(CLASSES[CLASS_OF[usize::div_ceil(size, 16)] as usize], want, "{size}");
    }
}

#[test]
fn cells_are_aligned_distinct_and_do_not_overlap() {
    let mut h = Heap::default();
    let sizes = [1, 24, 40, 100, 300, 1000, 5000, 8192];
    let cells: Vec<(NonNull<u8>, usize)> = (0..200).map(|i| sizes[i % sizes.len()]).map(|s| (h.alloc(s).unwrap(), s)).collect();
    for (i, &(p, s)) in cells.iter().enumerate() {
        assert_eq!(addr(p) % 16, 0);
        fill(p, s, i as u8);
    }
    for (i, &(p, s)) in cells.iter().enumerate() {
        assert!(holds(p, s, i as u8), "cell {i} was overwritten");
    }
    assert_eq!(cells.iter().map(|c| addr(c.0)).collect::<HashSet<_>>().len(), cells.len());
}

#[test]
fn a_lazy_sweep_gives_back_exactly_the_unmarked_cells() {
    let mut h = Heap::default();
    let cells: Vec<NonNull<u8>> = (0..100).map(|_| h.alloc(32).unwrap()).collect();
    let mapped = h.mapped_bytes();
    h.start_mark();
    for (i, &p) in cells.iter().enumerate() {
        if i % 2 == 0 {
            // SAFETY: `p` is a live cell of `h`.
            assert!(unsafe { h.mark(p) });
            // SAFETY: as above.
            assert!(!unsafe { h.mark(p) }, "a second mark is not new");
            fill(p, 32, 7);
        }
    }
    h.finish_mark();
    let dead: HashSet<usize> = cells.iter().skip(1).step_by(2).map(|&p| addr(p)).collect();
    let again: HashSet<usize> = (0..50).map(|_| addr(h.alloc(32).unwrap())).collect();
    assert_eq!(again, dead, "the 50 new cells are the 50 unmarked ones");
    assert_eq!(h.mapped_bytes(), mapped, "no new block");
    assert!(cells.iter().step_by(2).all(|&p| holds(p, 32, 7)), "the marked cells are untouched");
}

#[test]
fn empty_blocks_go_to_the_pool_and_the_rest_back_to_the_os() {
    let mut h = Heap::default();
    let per_block = (BLOCK - FIRST) / 8192;
    let blocks = POOL_KEEP + 6;
    let cells: Vec<NonNull<u8>> = (0..blocks * per_block).map(|_| h.alloc(8192).unwrap()).collect();
    assert_eq!(h.mapped_bytes(), blocks * BLOCK);
    // One cell stays alive: its block stays.
    h.start_mark();
    // SAFETY: a live cell of `h`.
    unsafe { h.mark(cells[0]) };
    h.finish_mark();
    h.start_mark();
    // SAFETY: as above.
    unsafe { h.mark(cells[0]) };
    h.finish_mark();
    assert_eq!(h.mapped_bytes(), (1 + POOL_KEEP) * BLOCK, "the live block and the pool stay mapped");
    // The pool serves new blocks before the OS does.
    for _ in 0..POOL_KEEP * per_block {
        h.alloc(8192).unwrap();
    }
    assert_eq!(h.mapped_bytes(), (1 + POOL_KEEP) * BLOCK);
}

#[test]
fn a_large_object_has_its_own_pages_and_lives_while_marked() {
    let mut h = Heap::default();
    let a = h.alloc(100_000).unwrap();
    let b = h.alloc(MAX_SMALL + 1).unwrap();
    let len_a = (FIRST + 100_000).next_multiple_of(BLOCK);
    assert_eq!(h.mapped_bytes(), len_a + BLOCK);
    fill(a, 100_000, 3);
    h.start_mark();
    // SAFETY: a live large object of `h`.
    assert!(unsafe { h.mark(a) });
    h.finish_mark();
    assert_eq!(h.mapped_bytes(), len_a, "the unmarked one went back to the OS");
    assert!(holds(a, 100_000, 3));
    let _ = b;
    h.start_mark();
    h.finish_mark();
    assert_eq!(h.mapped_bytes(), 0);
}
