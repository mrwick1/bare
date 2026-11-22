use super::*;

#[test]
fn task_budget_grows_after_survival_and_shrinks_after_reclaiming() {
    let mut heap = Heap::default();
    let mut live = Vec::new();
    for _ in 0..16 {
        live.push(heap.alloc(MAX_SMALL + 1).unwrap());
    }
    assert!(heap.task_due(), "the default samples early");
    assert!(!heap.due(), "the idle floor does not change");

    heap.start_mark();
    for cell in live {
        // SAFETY: Each pointer came from this heap and was not freed.
        assert!(unsafe { heap.mark(cell) });
    }
    heap.finish_mark();
    for _ in 0..64 {
        heap.alloc(MAX_SMALL + 1).unwrap();
    }
    assert!(!heap.task_due(), "the live sample gives the task more room");

    heap.start_mark();
    heap.finish_mark();
    for _ in 0..64 {
        heap.alloc(MAX_SMALL + 1).unwrap();
    }
    assert!(heap.task_due(), "reclaiming the heap lowers the trigger");
    assert!(!heap.due());
}

#[test]
fn finishing_the_lazy_sweep_does_not_count_as_allocation_growth() {
    let mut heap = Heap::default();
    let cell = heap.alloc(32).unwrap();
    heap.start_mark();
    // SAFETY: The cell came from this heap and has not been freed.
    assert!(unsafe { heap.mark(cell) });
    heap.finish_mark();

    heap.alloc(MAX_SMALL + 1).unwrap();
    let grown = heap.grown_bytes();
    heap.start_mark();
    assert_eq!(heap.grown_bytes(), grown);
    heap.finish_mark();
}
