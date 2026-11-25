// Tests of the hash index of a dictionary table under add and delete churn (`shapes.rs`).
use super::*;
use crate::atoms::{Atoms, SharedAtoms};
use crate::string::Chars;
use std::sync::Arc;

const WRITE: u8 = A_W | A_E | A_C;

#[test]
fn a_lookup_ends_after_many_adds_and_deletes_of_new_keys() {
    // Each new key that is added and then deleted again leaves a deleted slot in the index. A lookup of an absent key stops only at an
    // empty slot, so the index must keep some.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut heap = Heap::default();
        let mut atoms = Atoms::new(Arc::new(SharedAtoms::default()));
        let mut shapes = Shapes::default();
        let mut key = |name: &str| atoms.intern(&mut heap, Chars::One(name.as_bytes())).unwrap();
        let ks: Vec<Str> = (0..SCAN_MAX + 4).map(|i| key(&format!("k{i}"))).collect();
        let absent = key("absent");
        let churn: Vec<Str> = (0..200).map(|i| key(&format!("x{i}"))).collect();
        let root = shapes.root(Layout::Object, None, MAX_INLINE);
        let all = ks.iter().fold(root, |s, &k| shapes.add(s, k, WRITE).unwrap());
        let d = shapes.remove(all, ks[3]).unwrap().shape;
        assert!(shapes.get(d).is_dict());
        for &k in &churn {
            shapes.dict_add(d, k, WRITE);
            assert!(shapes.find(d, k).is_some());
            shapes.remove(d, k).unwrap();
        }
        tx.send((shapes.find(d, absent).is_none(), shapes.find(d, ks[5]).is_some())).unwrap();
    });
    let got = rx.recv_timeout(std::time::Duration::from_secs(20)).expect("the lookup of an absent key did not end");
    assert_eq!(got, (true, true));
}
