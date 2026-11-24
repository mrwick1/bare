// Tests of `shapes.rs`. Miri runs them in CI, so the counts shrink under it.
use super::*;
use crate::atoms::{Atoms, SharedAtoms};
use crate::string::Chars;
use std::sync::Arc;

const WRITE: u8 = A_W | A_E | A_C;
const BIG: usize = if cfg!(miri) { 40 } else { 300 };

struct Realm {
    heap: Heap,
    atoms: Atoms,
    shapes: Shapes,
}

impl Realm {
    fn new() -> Realm {
        Realm { heap: Heap::default(), atoms: Atoms::new(Arc::new(SharedAtoms::default())), shapes: Shapes::default() }
    }

    fn key(&mut self, name: &str) -> Str {
        self.atoms.intern(&mut self.heap, Chars::One(name.as_bytes())).unwrap()
    }

    fn keys(&mut self, n: usize) -> Vec<Str> {
        (0..n).map(|i| self.key(&format!("k{i}"))).collect()
    }

    fn chain(&mut self, root: ShapeId, keys: &[Str]) -> ShapeId {
        keys.iter().fold(root, |s, &k| self.shapes.add(s, k, WRITE).unwrap())
    }

    fn object(&mut self, s: ShapeId) -> NonNull<u8> {
        let p = self.heap.alloc(32).unwrap();
        // SAFETY: a cell of 32 bytes; the header is its first 8.
        unsafe { p.as_ptr().cast::<CellHeader>().write(self.shapes.header(s)) };
        p
    }

    /// A collection in which the objects `live` are marked. `check` runs after `finish_marks`, before the sweep of the heap. Returns the
    /// prototypes that `note_live` gave.
    fn collect(&mut self, live: &[NonNull<u8>], check: impl FnOnce(&Heap, &Shapes)) -> Vec<NonNull<u8>> {
        let mut protos = Vec::new();
        self.heap.start_mark();
        for &c in live {
            // SAFETY: `c` is a live cell of the heap with an object header.
            unsafe { self.heap.mark(c) };
            // SAFETY: as above.
            let h = unsafe { c.as_ptr().cast::<CellHeader>().read() };
            protos.extend(self.shapes.note_live(&mut self.heap, h));
        }
        self.shapes.finish_marks(&mut self.heap);
        check(&self.heap, &self.shapes);
        self.heap.finish_mark();
        protos
    }
}

fn marked(h: &Heap, k: Str) -> bool {
    // SAFETY: `k` is an atom cell of `h`.
    unsafe { h.is_marked(k.cell()) }
}

#[test]
fn a_root_is_made_once_for_each_layout_prototype_and_capacity() {
    let mut r = Realm::new();
    let (p, q) = (r.heap.alloc(16).unwrap(), r.heap.alloc(16).unwrap());
    let a = r.shapes.root(Layout::Object, Some(p), 4);
    assert_eq!(r.shapes.root(Layout::Object, Some(p), 4), a);
    assert_ne!(r.shapes.root(Layout::Object, Some(q), 4), a);
    assert_ne!(r.shapes.root(Layout::Object, Some(p), 5), a);
    assert_ne!(r.shapes.root(Layout::Object, None, 4), a);
    assert_eq!(r.shapes.root(Layout::Object, Some(p), 1000), r.shapes.root(Layout::Object, Some(p), MAX_INLINE));
    let s = r.shapes.get(a);
    assert_eq!((s.proto(), s.inline(), s.slots, s.layout, s.is_dict()), (Some(p), 4, 0, Layout::Object, false));
    let none = r.shapes.root(Layout::Object, None, 0);
    assert_eq!(r.shapes.get(none).proto(), None);
}

#[test]
fn a_transition_is_made_once_and_gives_the_next_slot() {
    let mut r = Realm::new();
    let ks = r.keys(4);
    let root = r.shapes.root(Layout::Object, None, 4);
    let abc = r.chain(root, &ks[..3]);
    assert_eq!(r.chain(root, &ks[..3]), abc);
    assert_eq!(r.shapes.count(), 4);
    assert_eq!(r.shapes.get(abc).slots, 3);
    assert_eq!(r.shapes.len_of(abc), 3);
    for (i, &k) in ks[..3].iter().enumerate() {
        let e = r.shapes.find(abc, k).unwrap();
        assert_eq!((e.key(), e.slot(), e.attrs()), (k, i as u32, WRITE));
    }
    assert!(r.shapes.find(abc, ks[3]).is_none());
    // The shape before has only the keys before.
    let ab = r.chain(root, &ks[..2]);
    assert!(r.shapes.find(ab, ks[2]).is_none() && r.shapes.find(ab, ks[1]).is_some());
    assert_eq!(r.shapes.entries(abc).map(|e| e.key()).collect::<Vec<_>>(), ks[..3]);
    // The same key with other attributes is another transition.
    assert_ne!(r.shapes.add(ab, ks[2], A_W).unwrap(), abc);
    assert!(r.shapes.cacheable(abc));
}

#[test]
fn an_accessor_takes_two_slots() {
    let mut r = Realm::new();
    let ks = r.keys(3);
    let root = r.shapes.root(Layout::Object, None, 4);
    let a = r.shapes.add(root, ks[0], A_ACC | A_E | A_C).unwrap();
    let b = r.shapes.add(a, ks[1], WRITE).unwrap();
    assert_eq!(r.shapes.get(b).slots, 3);
    let e = r.shapes.find(b, ks[1]).unwrap();
    assert_eq!((e.slot(), e.width()), (2, 1));
    let e = r.shapes.find(b, ks[0]).unwrap();
    assert_eq!((e.slot(), e.width(), e.attrs() & A_ACC), (0, 2, A_ACC));
}

#[test]
fn a_branch_copies_only_its_prefix_and_a_second_root_copies_nothing() {
    let mut r = Realm::new();
    let ks = r.keys(4);
    let (p, q) = (r.heap.alloc(16).unwrap(), r.heap.alloc(16).unwrap());
    let root = r.shapes.root(Layout::Object, Some(p), 4);
    let ab = r.chain(root, &ks[..2]);
    assert_eq!((r.shapes.stats().tables, r.shapes.stats().entries), (1, 2));
    // Another prototype, another capacity: the same keys, the same table.
    let other = r.shapes.root(Layout::Object, Some(q), 8);
    let ab2 = r.chain(other, &ks[..2]);
    let third = r.shapes.root(Layout::Object, None, 2);
    let ab3 = r.chain(third, &ks[..2]);
    assert_eq!((r.shapes.stats().tables, r.shapes.stats().entries), (1, 2));
    assert!(ab != ab2 && ab != ab3 && ab2 != ab3);
    assert_eq!(r.shapes.get(ab2).inline(), 8);
    assert_eq!(r.shapes.get(ab3).proto(), None);
    // A different second key is a branch: the table is copied up to the branch point.
    let ac = {
        let a = r.shapes.parent(ab).unwrap();
        r.shapes.add(a, ks[2], WRITE).unwrap()
    };
    assert_ne!(ac, ab);
    assert_eq!((r.shapes.stats().tables, r.shapes.stats().entries), (2, 4));
    // The old chain still finds its own keys, and the branch does not see them.
    assert!(r.shapes.find(ab, ks[1]).is_some() && r.shapes.find(ab, ks[2]).is_none());
    assert!(r.shapes.find(ac, ks[2]).is_some() && r.shapes.find(ac, ks[1]).is_none());
}

#[test]
fn a_long_table_has_a_hash_index() {
    let mut r = Realm::new();
    let ks = r.keys(MAX_PROPS);
    let root = r.shapes.root(Layout::Object, None, MAX_INLINE);
    let all = r.chain(root, &ks);
    for (i, &k) in ks.iter().enumerate() {
        assert_eq!(r.shapes.find(all, k).unwrap().slot(), i as u32, "key {i}");
    }
    let absent = r.key("absent");
    assert!(r.shapes.find(all, absent).is_none());
    // A shorter shape of the chain does not find the keys that come after it.
    let mid = r.shapes.parent(all).unwrap();
    assert!(r.shapes.find(mid, ks[MAX_PROPS - 1]).is_none());
    assert!(r.shapes.find(mid, ks[MAX_PROPS - 2]).is_some());
    // The 129th property does not fit the tree.
    assert_eq!(r.shapes.add(all, absent, WRITE), None);
    // A branch in the middle of a long chain copies the prefix and has its own index.
    let mut at = all;
    while r.shapes.len_of(at) > 60 {
        at = r.shapes.parent(at).unwrap();
    }
    let branch = r.shapes.add(at, absent, WRITE).unwrap();
    assert_eq!(r.shapes.find(branch, absent).unwrap().slot(), 60);
    assert!(r.shapes.find(branch, ks[60]).is_none() && r.shapes.find(branch, ks[59]).is_some());
}

#[test]
fn a_literal_has_its_final_shape_and_no_slack() {
    let mut r = Realm::new();
    let ks = r.keys(3);
    let l = r.shapes.literal(Layout::Object, None, &ks).unwrap();
    assert_eq!((r.shapes.get(l).inline(), r.shapes.get(l).slots), (3, 3));
    assert_eq!(r.shapes.literal(Layout::Object, None, &ks), Some(l));
    // Same keys from a constructor with slack: another shape, one table.
    let root = r.shapes.root(Layout::Object, None, 8);
    let c = r.chain(root, &ks);
    assert_ne!(c, l);
    assert_eq!(r.shapes.stats().tables, 1);
}

#[test]
fn a_small_object_stays_in_the_tree_when_it_deletes_a_key() {
    let mut r = Realm::new();
    let ks = r.keys(4);
    let root = r.shapes.root(Layout::Object, None, 4);
    let ab = r.chain(root, &ks[..2]);
    let abc = r.shapes.add(ab, ks[2], WRITE).unwrap();
    // The last key: the parent shape, no slot moves.
    let rm = r.shapes.remove(abc, ks[2]).unwrap();
    assert_eq!((rm.shape, rm.entry.slot(), rm.shifts), (ab, 2, false));
    let absent = r.key("absent");
    assert!(r.shapes.remove(abc, absent).is_none());
    // The first key: the shape of `b, c` from the root, the same shape as for an object that never had `a`; the later slots move down.
    let rm = r.shapes.remove(abc, ks[0]).unwrap();
    assert_eq!((rm.entry.slot(), rm.shifts), (0, true));
    assert_eq!(rm.shape, r.chain(root, &ks[1..3]));
    assert_eq!(r.shapes.find(rm.shape, ks[1]).unwrap().slot(), 0);
    assert_eq!(r.shapes.find(rm.shape, ks[2]).unwrap().slot(), 1);
    assert_eq!(r.shapes.get(rm.shape).slots, 2);
    // A key in the middle, with an accessor after it: the accessor keeps its two slots.
    let acc = r.shapes.add(ab, ks[3], A_ACC | A_E | A_C).unwrap();
    let rm = r.shapes.remove(acc, ks[0]).unwrap();
    assert_eq!(r.shapes.get(rm.shape).slots, 3);
    assert_eq!((r.shapes.find(rm.shape, ks[1]).unwrap().slot(), r.shapes.find(rm.shape, ks[3]).unwrap().slot()), (0, 1));
    assert_eq!(r.shapes.stats().dicts, 0);
    // A shape with no parent (after a prototype change) goes to a dictionary when it deletes its last key.
    let p = r.heap.alloc(16).unwrap();
    let moved = r.shapes.with_proto(ab, Some(p));
    let rm = r.shapes.remove(moved, ks[1]).unwrap();
    assert!(r.shapes.get(rm.shape).is_dict());
}

#[test]
fn a_big_object_goes_to_a_dictionary_when_it_deletes_a_key_in_the_middle() {
    let mut r = Realm::new();
    let ks = r.keys(SCAN_MAX + 4);
    let root = r.shapes.root(Layout::Object, None, MAX_INLINE);
    let all = r.chain(root, &ks);
    let n = ks.len();
    // The last key still goes back.
    let rm = r.shapes.remove(all, ks[n - 1]).unwrap();
    assert!(!r.shapes.get(rm.shape).is_dict());
    let rm = r.shapes.remove(all, ks[3]).unwrap();
    let d = rm.shape;
    assert!(r.shapes.get(d).is_dict() && !r.shapes.cacheable(d) && d != all);
    assert_eq!((rm.entry.slot(), rm.shifts), (3, false));
    assert!(r.shapes.find(d, ks[3]).is_none());
    assert_eq!(r.shapes.find(d, ks[4]).unwrap().slot(), 4);
    assert_eq!(r.shapes.len_of(d), n - 1);
    // The tree shape is unchanged.
    assert_eq!(r.shapes.len_of(all), n);
    assert!(r.shapes.find(all, ks[3]).is_some());
    // A new key takes the freed slot, and the shape keeps its id.
    let k = r.key("new");
    let e = r.shapes.dict_add(d, k, WRITE);
    assert_eq!(e.slot(), 3);
    assert_eq!(r.shapes.get(d).slots as usize, n);
    let order: Vec<Str> = r.shapes.entries(d).map(|e| e.key()).collect();
    assert_eq!(order.len(), n);
    assert_eq!((order[2], order[3], order[n - 1]), (ks[2], ks[4], k));
    // Each object that goes to a dictionary gets its own shape.
    assert_ne!(r.shapes.to_dict(all), d);
    assert_eq!(r.shapes.to_dict(d), d);
}

#[test]
fn a_big_dictionary_keeps_its_slots_and_packs_its_table() {
    let mut r = Realm::new();
    let ks = r.keys(BIG);
    let root = r.shapes.root(Layout::Object, None, 0);
    let one = r.shapes.add(root, ks[0], WRITE).unwrap();
    let d = r.shapes.to_dict(one);
    let slots: Vec<u32> = ks[1..].iter().map(|&k| r.shapes.dict_add(d, k, WRITE).slot()).collect();
    assert_eq!(slots, (1..BIG as u32).collect::<Vec<_>>());
    for (i, &k) in ks.iter().enumerate() {
        assert_eq!(r.shapes.find(d, k).unwrap().slot(), i as u32);
    }
    // Delete 3 of 4: every deleted slot is reused once, nothing else moves.
    let mut gone = Vec::new();
    for (i, &k) in ks.iter().enumerate().skip(1) {
        if i % 4 != 0 {
            gone.push(r.shapes.remove(d, k).unwrap().entry.slot());
        }
    }
    for (i, &k) in ks.iter().enumerate() {
        assert_eq!(r.shapes.find(d, k).map(|e| e.slot()), (i % 4 == 0).then_some(i as u32), "key {i}");
    }
    assert_eq!(r.shapes.len_of(d), ks.len().div_ceil(4));
    assert_eq!(r.shapes.entries(d).count(), ks.len().div_ceil(4));
    let before = r.shapes.get(d).slots;
    let mut reused = Vec::new();
    for i in 0..gone.len() {
        let k = r.key(&format!("new{i}"));
        reused.push(r.shapes.dict_add(d, k, WRITE).slot());
    }
    reused.sort_unstable();
    gone.sort_unstable();
    assert_eq!(reused, gone);
    assert_eq!(r.shapes.get(d).slots, before);
}

#[test]
fn an_attribute_change_goes_to_a_dictionary_and_a_kind_change_moves_the_slots() {
    let mut r = Realm::new();
    let ks = r.keys(3);
    let root = r.shapes.root(Layout::Object, None, 4);
    let abc = r.chain(root, &ks);
    // Freeze a property: same slot, other attributes.
    let (d, old, new) = r.shapes.reconfigure(abc, ks[1], A_E).unwrap();
    assert!(r.shapes.get(d).is_dict());
    assert_eq!((old.slot(), new.slot(), new.attrs()), (1, 1, A_E));
    assert_eq!(r.shapes.find(d, ks[1]).unwrap().attrs(), A_E);
    assert_eq!(r.shapes.find(abc, ks[1]).unwrap().attrs(), WRITE);
    // Data to accessor: two fresh slots, and the old one is free for another key.
    let (d2, old, new) = r.shapes.reconfigure(d, ks[1], A_ACC | A_E | A_C).unwrap();
    assert_eq!(d2, d);
    assert_eq!((old.slot(), new.slot(), new.width()), (1, 3, 2));
    assert_eq!(r.shapes.get(d).slots, 5);
    let n = r.key("n");
    assert_eq!(r.shapes.dict_add(d, n, WRITE).slot(), 1);
    let absent = r.key("absent");
    assert!(r.shapes.reconfigure(d, absent, A_W).is_none());
}

#[test]
fn a_prototype_change_makes_one_shape_with_the_same_table() {
    let mut r = Realm::new();
    let ks = r.keys(2);
    let (p, q) = (r.heap.alloc(16).unwrap(), r.heap.alloc(16).unwrap());
    let root = r.shapes.root(Layout::Object, Some(p), 4);
    let ab = r.chain(root, &ks);
    let tables = r.shapes.stats().tables;
    let to_q = r.shapes.with_proto(ab, Some(q));
    assert_ne!(to_q, ab);
    assert_eq!(r.shapes.with_proto(ab, Some(q)), to_q);
    assert_eq!(r.shapes.with_proto(ab, Some(p)), ab);
    assert_eq!(r.shapes.stats().tables, tables);
    let s = r.shapes.get(to_q);
    assert_eq!((s.proto(), s.slots, s.inline(), r.shapes.len_of(to_q)), (Some(q), 2, 4, 2));
    assert_eq!(r.shapes.find(to_q, ks[1]).unwrap().slot(), 1);
    // An add from it goes on, and its parent is not the old shape.
    let k = r.key("c");
    let abc = r.shapes.add(to_q, k, WRITE).unwrap();
    assert_eq!((r.shapes.get(abc).proto(), r.shapes.get(abc).slots), (Some(q), 3));
    // An empty shape gives the root of the new prototype.
    assert_eq!(r.shapes.with_proto(root, Some(q)), r.shapes.root(Layout::Object, Some(q), 4));
    // A dictionary shape changes in place.
    let d = r.shapes.to_dict(ab);
    assert_eq!(r.shapes.with_proto(d, Some(q)), d);
    assert_eq!(r.shapes.get(d).proto(), Some(q));
}

#[test]
fn the_header_of_a_shape_names_the_shape() {
    let mut r = Realm::new();
    let ks = r.keys(1);
    let root = r.shapes.root(Layout::Object, None, 2);
    let a = r.chain(root, &ks);
    let (hr, ha) = (r.shapes.header(root), r.shapes.header(a));
    assert_ne!(hr.shape_key(), ha.shape_key());
    assert_eq!((ha.layout(), ha.flags(), r.shapes.id_of(ha)), (Layout::Object, 0, a));
    assert_eq!(r.shapes.of(ha).slots, 1);
    // A string never matches the shape of an inline cache.
    assert!(CellHeader::new(Layout::OneByte, 0).shape_key() != ha.shape_key());
}

#[test]
fn a_collection_frees_the_shapes_that_no_live_object_uses() {
    let mut r = Realm::new();
    let ks = r.keys(6);
    let root = r.shapes.root(Layout::Object, None, 4);
    let ab = r.chain(root, &ks[..2]);
    let abc = r.shapes.add(ab, ks[2], WRITE).unwrap();
    let abd = r.shapes.add(ab, ks[3], WRITE).unwrap();
    let ef = r.chain(root, &ks[4..]);
    let (o1, o2) = (r.object(abc), r.object(ef));
    let _dead = r.object(abd);
    assert_eq!(r.shapes.count(), 7);
    let epoch = r.shapes.epoch();
    r.collect(&[o1, o2], |h, s| {
        // The key atoms of the shapes that stay are marked; the key that only the freed shape used is not.
        assert!([0, 1, 2, 4, 5].iter().all(|&i| marked(h, ks[i])));
        assert!(!marked(h, ks[3]));
        assert_eq!(s.count(), 6);
    });
    assert_ne!(r.shapes.epoch(), epoch);
    // abc keeps its parents (root, a, ab), the other chain stays, `abd` is gone.
    assert_eq!(r.shapes.count(), 6);
    for (s, n) in [(abc, 3), (ef, 2)] {
        assert_eq!(r.shapes.len_of(s), n);
        assert_eq!(r.shapes.entries(s).count(), n);
    }
    assert!(r.shapes.find(ab, ks[1]).is_some());
    // The table of `abd` was freed; the same key makes a new one.
    assert_eq!(r.shapes.stats().tables, 2);
    let abd2 = r.shapes.add(ab, ks[3], WRITE).unwrap();
    assert_eq!(r.shapes.find(abd2, ks[3]).unwrap().slot(), 2);
    assert_eq!(r.shapes.stats().tables, 3);
    // A second collection with nothing live frees everything.
    r.collect(&[], |_, s| assert_eq!(s.count(), 0));
    assert_eq!((r.shapes.stats().shapes, r.shapes.stats().tables, r.shapes.stats().entries), (0, 1, 0));
    // The ids are reused.
    let root2 = r.shapes.root(Layout::Object, None, 4);
    assert!(root2.raw() as usize <= 8);
}

#[test]
fn a_collection_keeps_the_prototype_of_a_live_shape_once() {
    let mut r = Realm::new();
    let (p, q) = (r.heap.alloc(16).unwrap(), r.heap.alloc(16).unwrap());
    let ks = r.keys(1);
    let a = r.shapes.root(Layout::Object, Some(p), 2);
    let a = r.chain(a, &ks);
    let b = r.shapes.root(Layout::Object, Some(q), 2);
    let b = r.chain(b, &ks);
    let objs = [r.object(a), r.object(a), r.object(a), r.object(b)];
    let protos = r.collect(&objs, |_, _| {});
    assert_eq!(protos, [p, q]);
    // The bits are clear after a collection, so the next one reports them again.
    assert_eq!(r.collect(&objs[..1], |_, _| {}), [p]);
}

#[test]
fn a_dictionary_shape_lives_while_its_object_does() {
    let mut r = Realm::new();
    let ks = r.keys(SCAN_MAX + 2);
    let root = r.shapes.root(Layout::Object, None, MAX_INLINE);
    let abc = r.chain(root, &ks);
    let d = r.shapes.remove(abc, ks[0]).unwrap().shape;
    let (tables, epoch) = (r.shapes.stats().tables, r.shapes.epoch());
    let o = r.object(d);
    r.collect(&[o], |h, _| assert!(marked(h, ks[1]) && marked(h, ks[SCAN_MAX + 1]) && !marked(h, ks[0])));
    // The tree shape was freed, the dictionary shape stays. The deleted key is in no live table now.
    assert!(r.shapes.get(d).is_dict());
    assert_eq!(r.shapes.entries(d).map(|e| e.key()).collect::<Vec<_>>(), ks[1..]);
    assert_eq!(r.shapes.stats().dicts, 1);
    assert_eq!(r.shapes.stats().tables, 2);
    assert!(tables > 1 && r.shapes.epoch() != epoch);
    // The dictionary shape of an object that died is freed with its table, and that does not change the epoch.
    let epoch = r.shapes.epoch();
    r.collect(&[], |_, _| {});
    assert_eq!((r.shapes.stats().dicts, r.shapes.stats().tables, r.shapes.count()), (0, 1, 0));
    assert_eq!(r.shapes.epoch(), epoch);
}

#[test]
fn a_key_from_the_shared_table_is_not_marked_in_the_realm_heap() {
    let shared = Arc::new(SharedAtoms::default());
    let mut r = Realm::new();
    r.atoms = Atoms::new(shared.clone());
    let k = shared.intern(Chars::One(b"shared")).unwrap();
    assert!(k.is_shared());
    let root = r.shapes.root(Layout::Object, None, 1);
    let s = r.shapes.add(root, k, WRITE).unwrap();
    let o = r.object(s);
    r.collect(&[o], |_, s| assert_eq!(s.count(), 2));
    assert_eq!(r.shapes.find(s, k).unwrap().key(), k);
}

#[test]
fn slack_tracking_settles_at_the_most_slots_used_and_widens_twice() {
    let mut s = Site::new(8);
    assert_eq!((s.inline(), s.settled()), (8, false));
    for used in [2, 3, 3, 2, 3, 3, 2] {
        assert!(!s.observe(used));
        assert_eq!(s.inline(), 8);
    }
    // The eighth run settles: the capacity is the most that was used.
    assert!(s.observe(3));
    assert_eq!((s.inline(), s.settled()), (3, true));
    assert!(!s.observe(3));
    // An object that needs more widens it; twice at most.
    assert!(s.observe(5));
    assert_eq!(s.inline(), 5);
    assert!(s.observe(9));
    assert_eq!(s.inline(), 9);
    assert!(!s.observe(12));
    assert_eq!(s.inline(), 9);
    // A big initial value and a big use are capped.
    assert_eq!(Site::new(1000).inline(), MAX_INLINE);
    let mut t = Site::new(4);
    (0..SLACK_RUNS).for_each(|_| _ = t.observe(1000));
    assert_eq!(t.inline(), MAX_INLINE);
}

#[test]
fn a_site_that_settles_gets_a_root_that_wastes_less() {
    let mut r = Realm::new();
    let ks = r.keys(3);
    let mut site = Site::new(8);
    let before = r.shapes.root(Layout::Object, None, site.inline());
    let first = r.chain(before, &ks);
    for _ in 0..SLACK_RUNS {
        site.observe(3);
    }
    let after = r.shapes.root(Layout::Object, None, site.inline());
    let second = r.chain(after, &ks);
    assert_ne!(first, second);
    assert_eq!((r.shapes.get(first).inline(), r.shapes.get(second).inline()), (8, 3));
    // One descriptor table for both.
    assert_eq!((r.shapes.stats().tables, r.shapes.stats().entries), (1, 3));
}
