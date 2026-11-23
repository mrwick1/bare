//! Tests of the wake hooks (`parallel::on_end`): each page loop has its own, and a hook ends with its handle.

use crate::parallel::{on_end, wake};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn each_hook_is_called_until_its_handle_drops() {
    let (a, b) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (a2, b2) = (a.clone(), b.clone());
    let ha = on_end(move || {
        a2.fetch_add(1, Ordering::SeqCst);
    });
    let hb = on_end(move || {
        b2.fetch_add(1, Ordering::SeqCst);
    });
    wake();
    assert!(a.load(Ordering::SeqCst) >= 1 && b.load(Ordering::SeqCst) >= 1, "both loops wake");
    drop(ha);
    let before = a.load(Ordering::SeqCst);
    wake();
    assert_eq!(a.load(Ordering::SeqCst), before, "a dropped hook is not called");
    assert!(b.load(Ordering::SeqCst) >= 2);
    drop(hb);
}
