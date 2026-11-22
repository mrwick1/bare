use super::*;

#[test]
fn low_survival_first_gets_a_second_sample_then_backs_off_for_a_stable_live_set() {
    let mut heap = Heap::default();
    let live: Vec<_> = (0..1000).map(|_| heap.alloc(32).unwrap()).collect();
    for _ in 0..16 {
        heap.alloc(MAX_SMALL + 1).unwrap();
    }
    assert!(heap.task_due());

    heap.start_mark();
    for &cell in &live {
        // SAFETY: The cells came from this heap and have not been freed.
        assert!(unsafe { heap.mark(cell) });
    }
    heap.finish_mark();
    for _ in 0..16 {
        heap.alloc(MAX_SMALL + 1).unwrap();
    }
    assert!(heap.task_due(), "the second sample uses less than 1 MiB");

    heap.start_mark();
    for cell in live {
        // SAFETY: The first collection kept these cells alive.
        assert!(unsafe { heap.mark(cell) });
    }
    heap.finish_mark();
    for _ in 0..64 {
        heap.alloc(MAX_SMALL + 1).unwrap();
    }
    assert!(!heap.task_due(), "the stable live set gets more time between collections");
    assert!(!heap.due(), "the idle trigger stays separate");
}
