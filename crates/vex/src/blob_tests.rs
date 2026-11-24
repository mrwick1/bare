// Tests of `blob.rs`. Miri runs them in CI.
use super::*;

#[test]
fn a_blob_keeps_its_words_and_is_not_an_object() {
    let mut h = Heap::default();
    let words: Vec<u64> = (0..100).map(|i| i * 0x0101_0101_0101).collect();
    let b = Blob::new(&mut h, &words).unwrap();
    assert_eq!(b.words(), &words[..]);
    assert_eq!(b.words().as_ptr().addr() % 8, 0);
    // SAFETY: `b` is live.
    assert_eq!(unsafe { Blob::from_value(b.value()) }, Some(b));
    // SAFETY: `b` is live.
    assert!(unsafe { crate::object::Obj::from_value(b.value()) }.is_none());
    // SAFETY: a number is not a pointer.
    assert!(unsafe { Blob::from_value(Value::int(3)) }.is_none());
}

#[test]
fn an_empty_blob_and_a_large_one_work() {
    let mut h = Heap::default();
    let e = Blob::new(&mut h, &[]).unwrap();
    assert!(e.words().is_empty());
    // More than the largest small cell: a mapping of its own.
    let big = vec![7u64; 5000];
    let b = Blob::new(&mut h, &big).unwrap();
    assert_eq!(b.words().len(), 5000);
    assert!(b.words().iter().all(|&w| w == 7));
}

#[test]
fn the_collector_keeps_a_marked_blob_and_does_not_trace_it() {
    use crate::atoms::{Atoms, SharedAtoms};
    use crate::gc::collect;
    use crate::shapes::Shapes;
    use std::sync::Arc;
    let mut h = Heap::default();
    let mut shapes = Shapes::default();
    let mut atoms = Atoms::new(Arc::new(SharedAtoms::default()));
    // Words that look like pointers must not be followed.
    let b = Blob::new(&mut h, &[u64::MAX, 0x7FF8_0000_0000_0001, 12345]).unwrap();
    let dead = Blob::new(&mut h, &[1, 2, 3]).unwrap();
    let _ = dead;
    let mut roots = |m: &mut crate::gc::Marker| m.value(b.value());
    let s = collect(&mut h, &mut shapes, &mut atoms, &mut roots);
    assert_eq!(s.marked, 1);
    assert_eq!(b.words(), &[u64::MAX, 0x7FF8_0000_0000_0001, 12345]);
}


#[test]
fn gc_marks_bigint_words_as_a_leaf() {
    use crate::atoms::{Atoms, SharedAtoms};
    use crate::bigint::Sv;
    use crate::gc::collect;
    use crate::shapes::Shapes;
    use std::sync::Arc;

    let mut h = Heap::default();
    let mut shapes = Shapes::default();
    let mut atoms = Atoms::new(Arc::new(SharedAtoms::default()));
    let words = [u64::MAX, 0xFFFE_0000_0000_0001];
    let v = Blob::new_bigint(&mut h, Sv { neg: true, mag: &words }).unwrap();
    let mut roots = |m: &mut crate::gc::Marker| m.value(v);
    let stats = collect(&mut h, &mut shapes, &mut atoms, &mut roots);
    assert_eq!(stats.marked, 1);
    // SAFETY: the value is a BigInt that the root kept alive.
    let b = unsafe { Blob::from_bigint(v) }.unwrap();
    assert!(b.bigint().neg);
    assert_eq!(b.bigint().mag, words);
}
