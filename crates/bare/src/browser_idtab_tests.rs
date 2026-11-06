//! Tests of `IdTab`, the table that the host keeps by the slot of an object or a node.

use crate::browser::*;
use bare_html::tree;
use bare_js::heap::{Heap, HeapLimits};

#[test]
fn a_reused_slot_does_not_answer_for_the_old_object() {
    let mut h = Heap::new(HeapLimits::default()).unwrap();
    let a = h.new_object(None).unwrap();
    let mut t: IdTab<_, u32> = IdTab::default();
    t.insert(a, 1);
    assert_eq!(t.get(&a), Some(&1));
    h.collect(&[]).unwrap();
    let b = h.new_object(None).unwrap();
    assert_eq!(a.slot(), b.slot(), "the heap reuses the slot");
    assert_eq!(t.get(&b), None, "the entry of the dead object must not match");
    assert!(!t.contains_key(&b));
    assert_eq!(t.insert(b, 2), None, "the stale entry is replaced and is not the old value of b");
    assert_eq!(t.len(), 1);
    assert_eq!(t.get(&b), Some(&2));
    assert_eq!(t.get(&a), None);
}

#[test]
fn memory_follows_the_keys_and_not_the_largest_slot() {
    let mut h = Heap::new(HeapLimits::default()).unwrap();
    let objs: Vec<_> = (0..50_000).map(|_| h.new_object(None).unwrap()).collect();
    let mut t: IdTab<_, u32> = IdTab::default();
    t.insert(objs[49_999], 7);
    let (dir, pages) = t.size();
    assert_eq!(pages, 1);
    assert!(dir <= 49_999 / 32 + 1, "the directory holds one pointer for each 32 slots: {dir}");
    t.insert(objs[10], 8);
    assert_eq!(t.size().1, 2);
    t.retain(|_, v| *v == 8);
    assert_eq!((t.len(), t.size()), (1, (1, 1)), "the page and the empty end of the directory are freed");
    t.retain(|_, _| false);
    assert_eq!((t.len(), t.size().0, t.size().1), (0, 0, 0));
    assert_eq!(t.get(&objs[10]), None);
}

#[test]
fn nodes_get_insert_and_get_or_insert() {
    let doc = tree::parse("<body><p>a</p><p>b</p></body>");
    let html = doc.children(doc.root()).next().unwrap();
    let ps: Vec<_> = doc.children(html).collect(); // head and body
    let mut t: IdTab<_, u32> = IdTab::default();
    assert_eq!(*t.get_or_insert(ps[0], 5), 5);
    *t.get_or_insert(ps[0], 9) += 1;
    assert_eq!(t.get(&ps[0]), Some(&6));
    assert_eq!(t.get(&ps[1]), None);
    assert_eq!(t.insert(ps[0], 1), Some(6));
    *t.get_mut(&ps[0]).unwrap() = 3;
    assert_eq!(t.len(), 1);
}
