// Tests of `object.rs`. Miri runs them in CI, so the counts shrink under it.
use super::*;
use crate::atoms::{Atoms, SharedAtoms};
use crate::gc::{Marker, SLICE, Stats, collect};
use crate::shapes::{A_C, A_E, A_W, MAX_PROPS};
use crate::string::Chars;
use std::collections::BTreeMap;
use std::sync::Arc;

const MIRI: bool = cfg!(miri);
const WRITE: u8 = A_W | A_E | A_C;

struct Realm {
    heap: Heap,
    shapes: Shapes,
    atoms: Atoms,
    keys: Vec<Str>,
}

impl Realm {
    fn new() -> Realm {
        Realm { heap: Heap::default(), shapes: Shapes::default(), atoms: Atoms::new(Arc::new(SharedAtoms::default())), keys: Vec::new() }
    }

    fn key(&mut self, i: usize) -> Str {
        while self.keys.len() <= i {
            let name = format!("k{}", self.keys.len());
            self.keys.push(self.atoms.intern(&mut self.heap, Chars::One(name.as_bytes())).unwrap());
        }
        self.keys[i]
    }

    /// An empty object with `inline` inline slots (a shape of its own root).
    fn object(&mut self, inline: u16) -> Obj {
        let root = self.shapes.root(Layout::Object, None, inline);
        Obj::new(&mut self.heap, &self.shapes, root).unwrap()
    }

    fn array(&mut self, values: &[Value]) -> Obj {
        let root = self.shapes.root(Layout::Indexed, None, 0);
        Obj::array(&mut self.heap, &self.shapes, root, values).unwrap()
    }

    /// Add the property `k{i}` with value `v`.
    fn add(&mut self, o: Obj, i: usize, v: Value) -> u32 {
        let k = self.key(i);
        o.add(&mut self.heap, &mut self.shapes, k, WRITE, &[v]).unwrap()
    }

    fn slots(&self, o: Obj) -> u32 {
        self.shapes.of(o.head()).slots
    }

    /// Collect with `roots`, and the keys (their atoms must live: the test holds them).
    fn collect(&mut self, roots: &[Value]) -> Stats {
        let keys: Vec<Value> = self.keys.iter().map(|k| k.value()).collect();
        collect(&mut self.heap, &mut self.shapes, &mut self.atoms, &mut |m: &mut Marker| {
            m.values(&keys);
            m.values(roots);
        })
    }
}

fn int(i: usize) -> Value {
    Value::int(i as i32)
}

#[test]
fn the_cell_is_the_header_and_the_inline_words_rounded_to_16_bytes() {
    // `Object`: an odd number of words, so that 8 + 8 cap is a multiple of 16.
    for (inline, cap, bytes) in [(0, 1, 16), (1, 1, 16), (2, 3, 32), (4, 5, 48), (8, 9, 80), (30, 31, 256)] {
        assert_eq!(geometry(Layout::Object, inline), (8, cap), "inline {inline}");
        assert_eq!(cell_bytes(Layout::Object, inline), bytes);
        assert_eq!(words(Layout::Object, inline), cap);
    }
    // `Indexed`: the elements word, then an even number of words.
    for (inline, cap, bytes) in [(0, 2, 32), (2, 2, 32), (3, 4, 48), (30, 30, 256)] {
        assert_eq!(geometry(Layout::Indexed, inline), (16, cap), "inline {inline}");
        assert_eq!(cell_bytes(Layout::Indexed, inline), bytes);
        assert_eq!(words(Layout::Indexed, inline), cap + 1);
    }
}

#[test]
fn a_new_object_has_undefined_slots_and_the_rounding_room_is_usable() {
    let mut r = Realm::new();
    // Inline 8 means 9 usable slots: nine properties and no overflow record.
    let o = r.object(8);
    assert_eq!(o.head().layout(), Layout::Object);
    for i in 0..9 {
        assert_eq!(r.add(o, i, int(i * 10)), i as u32);
        assert_eq!(o.overflow_cap(&r.shapes), 0);
    }
    assert!((0..9).all(|i| o.slot(&r.shapes, i as u32) == int(i * 10)));
    // The tenth takes the overflow record, and no value is lost.
    r.add(o, 9, int(90));
    assert!(o.overflow_cap(&r.shapes) >= 2);
    assert!((0..10).all(|i| o.slot(&r.shapes, i as u32) == int(i * 10)));
    // Objects that add the same keys share a shape.
    let p = r.object(8);
    (0..10).for_each(|i| {
        r.add(p, i, Value::UNDEFINED);
    });
    assert_eq!(o.shape(&r.shapes), p.shape(&r.shapes));
    assert!(p.proto(&r.shapes).is_none());
}

#[test]
fn an_object_that_grows_keeps_every_value_across_the_boundary_at_every_step() {
    let mut r = Realm::new();
    for inline in [0u16, 1, 2, 3, 8] {
        let o = r.object(inline);
        let cap = geometry(Layout::Object, inline).1;
        for n in 0..40 {
            r.add(o, n, int(n + 1000));
            for i in 0..=n {
                assert_eq!(o.slot(&r.shapes, i as u32), int(i + 1000), "inline {inline}, {n} added, slot {i}");
            }
            // The record exists exactly when the shape has more slots than the cell has words.
            assert_eq!(o.overflow_cap(&r.shapes) > 0, n + 1 > cap, "inline {inline}, {} slots", n + 1);
        }
        // Writes go to the right place on both sides of the boundary.
        (0..40).for_each(|i| o.set_slot(&r.shapes, i as u32, int(i + 5000)));
        assert!((0..40).all(|i| o.slot(&r.shapes, i as u32) == int(i + 5000)));
    }
}

#[test]
fn the_overflow_record_grows_by_half_and_never_below_what_is_needed() {
    let mut r = Realm::new();
    let o = r.object(2);
    let mut caps = Vec::new();
    for n in 0..60 {
        r.add(o, n, int(n));
        let c = o.overflow_cap(&r.shapes);
        if caps.last() != Some(&c) && c > 0 {
            caps.push(c);
        }
        // The record holds the slots from `cap - 1` on.
        assert!(c == 0 || c >= (n + 1) - 2, "{c} values for {} slots", n + 1);
    }
    // A handful of records for 60 slots, not one for each slot.
    assert!(caps.len() <= 9, "{caps:?}");
    assert_eq!(caps[0], MIN_OVER);
    // A new object of a shape that has the slots gets an exact record.
    let p = Obj::new(&mut r.heap, &r.shapes, o.shape(&r.shapes)).unwrap();
    assert_eq!(p.overflow_cap(&r.shapes), 60 - 2);
    assert!((0..60).all(|i| p.slot(&r.shapes, i) == Value::UNDEFINED));
}

#[test]
fn an_accessor_at_the_boundary_keeps_both_halves() {
    let mut r = Realm::new();
    // cap 3: slots 0 and 1 are inline values, slot 2 is the last inline word. An accessor at slots 2 and 3 straddles the boundary.
    let o = r.object(2);
    r.add(o, 0, int(10));
    r.add(o, 1, int(11));
    let k = r.key(2);
    let slot = o.add(&mut r.heap, &mut r.shapes, k, WRITE & !A_W | A_ACC, &[int(20), int(21)]).unwrap();
    assert_eq!(slot, 2);
    assert_eq!((0..4).map(|i| o.slot(&r.shapes, i)).collect::<Vec<_>>(), [int(10), int(11), int(20), int(21)]);
    r.add(o, 3, int(30));
    assert_eq!((0..5).map(|i| o.slot(&r.shapes, i)).collect::<Vec<_>>(), [int(10), int(11), int(20), int(21), int(30)]);
}

#[test]
fn deleting_the_last_property_clears_it_and_drops_the_record_when_the_object_fits_again() {
    let mut r = Realm::new();
    let o = r.object(2);
    (0..6).for_each(|i| {
        r.add(o, i, int(i + 1));
    });
    assert!(o.overflow_cap(&r.shapes) > 0);
    for n in (0..6).rev() {
        let k = r.key(n);
        assert!(o.remove(&mut r.shapes, k));
        assert_eq!(r.slots(o) as usize, n);
        assert!((0..n).all(|i| o.slot(&r.shapes, i as u32) == int(i + 1)), "{n} left");
        // The record is gone once the slots fit in the three inline words.
        assert_eq!(o.overflow_cap(&r.shapes) > 0, n > 3, "{n} left");
    }
    let k = r.key(0);
    assert!(!o.remove(&mut r.shapes, k));
}

#[test]
fn deleting_a_key_in_the_middle_of_a_small_object_moves_the_later_slots_down() {
    let mut r = Realm::new();
    let o = r.object(2);
    (0..7).for_each(|i| {
        r.add(o, i, int(i + 1));
    });
    let k = r.key(1);
    assert!(o.remove(&mut r.shapes, k));
    // The object has the shape of one that never had `k1`, and its values moved down across the boundary.
    let p = r.object(2);
    for i in [0, 2, 3, 4, 5, 6] {
        r.add(p, i, Value::UNDEFINED);
    }
    assert_eq!(o.shape(&r.shapes), p.shape(&r.shapes));
    let want = [1, 3, 4, 5, 6, 7];
    assert!(want.iter().enumerate().all(|(i, &v)| o.slot(&r.shapes, i as u32) == int(v)));
    // The value of the freed last slot is not kept alive: the record's room past the slots is `undefined`.
    r.add(o, 1, int(99));
    assert_eq!(o.slot(&r.shapes, 6), int(99));
    // Down to the three inline words: delete four more.
    for i in [0, 2, 3, 4] {
        let k = r.key(i);
        assert!(o.remove(&mut r.shapes, k));
    }
    assert_eq!(o.overflow_cap(&r.shapes), 0);
    assert_eq!((0..3).map(|i| o.slot(&r.shapes, i)).collect::<Vec<_>>(), [int(6), int(7), int(99)]);
}

#[test]
fn a_dictionary_object_keeps_its_slots_and_reuses_a_deleted_one() {
    let mut r = Realm::new();
    let o = r.object(4);
    let n = MAX_PROPS + 72;
    for i in 0..n {
        r.add(o, i, int(i));
    }
    assert!(r.shapes.get(o.shape(&r.shapes)).is_dict());
    assert!((0..n).all(|i| o.slot(&r.shapes, i as u32) == int(i)));
    // Delete a key in the middle: the other slots stay where they are, and the next property takes the freed slot.
    let k = r.key(50);
    assert!(o.remove(&mut r.shapes, k));
    assert!((0..n).filter(|&i| i != 50).all(|i| o.slot(&r.shapes, i as u32) == int(i)));
    assert_eq!(o.slot(&r.shapes, 50), Value::UNDEFINED);
    assert_eq!(r.add(o, 500, int(5)), 50);
    assert_eq!(r.slots(o) as usize, n);
    assert_eq!(o.slot(&r.shapes, 50), int(5));
    // And it can still grow.
    r.add(o, 501, int(6));
    assert_eq!(o.slot(&r.shapes, n as u32), int(6));
}

#[test]
fn a_property_that_gets_attributes_keeps_its_value_and_a_kind_change_moves_it() {
    let mut r = Realm::new();
    let o = r.object(2);
    (0..4).for_each(|i| {
        r.add(o, i, int(i + 1));
    });
    // `freeze`: a data property loses `A_W`. Same slots, own dictionary shape.
    let k1 = r.key(1);
    assert!(o.reconfigure(&mut r.heap, &mut r.shapes, k1, A_E | A_C, &[int(2)]));
    assert!(r.shapes.get(o.shape(&r.shapes)).is_dict());
    assert_eq!(r.shapes.find(o.shape(&r.shapes), k1).unwrap().attrs(), A_E | A_C);
    assert!((0..4).all(|i| o.slot(&r.shapes, i) == int(i as usize + 1)));
    // Data to accessor: two fresh slots at the end, across the boundary; the old slot is cleared.
    let k0 = r.key(0);
    assert!(o.reconfigure(&mut r.heap, &mut r.shapes, k0, A_E | A_C | A_ACC, &[int(100), int(101)]));
    let e = r.shapes.find(o.shape(&r.shapes), k0).unwrap();
    assert_eq!((e.slot(), e.width()), (4, 2));
    assert_eq!((o.slot(&r.shapes, 4), o.slot(&r.shapes, 5)), (int(100), int(101)));
    assert_eq!(o.slot(&r.shapes, 0), Value::UNDEFINED);
    assert!((1..4).all(|i| o.slot(&r.shapes, i) == int(i as usize + 1)));
    let missing = r.key(9);
    assert!(!o.reconfigure(&mut r.heap, &mut r.shapes, missing, WRITE, &[Value::UNDEFINED]));
}

#[test]
fn a_prototype_change_is_a_shape_change() {
    let mut r = Realm::new();
    let proto = r.object(1);
    let o = r.object(2);
    r.add(o, 0, int(1));
    r.add(o, 1, int(2));
    r.add(o, 2, int(3));
    o.set_proto(&mut r.shapes, Some(proto.cell()));
    assert_eq!(o.proto(&r.shapes), Some(proto.cell()));
    assert!((0..3).all(|i| o.slot(&r.shapes, i) == int(i as usize + 1)));
    o.set_proto(&mut r.shapes, None);
    assert_eq!(o.proto(&r.shapes), None);
    // Two objects of one shape that get the same prototype have the same shape.
    let (p, q) = (r.object(2), r.object(2));
    for x in [p, q] {
        (0..3).for_each(|i| {
            r.add(x, i, int(0));
        });
        x.set_proto(&mut r.shapes, Some(proto.cell()));
    }
    assert_eq!(p.shape(&r.shapes), q.shape(&r.shapes));
}

#[test]
#[should_panic(expected = "not in the shape")]
fn a_slot_past_the_shape_is_refused() {
    let mut r = Realm::new();
    let o = r.object(2);
    r.add(o, 0, int(1));
    o.slot(&r.shapes, 1);
}

// ---- elements ----

fn elements(o: Obj, upto: u32) -> Vec<Option<Value>> {
    (0..upto).map(|i| o.get_element(i)).collect()
}

#[test]
fn an_array_literal_has_a_record_of_exactly_its_values() {
    let mut r = Realm::new();
    let a = r.array(&[int(1), Value::UNDEFINED, int(3)]);
    assert_eq!((a.len(), a.elements_cap(), a.is_sparse()), (3, 3, false));
    assert_eq!(elements(a, 4), [Some(int(1)), Some(Value::UNDEFINED), Some(int(3)), None]);
    let e = r.array(&[]);
    assert_eq!((e.len(), e.elements_cap()), (0, 0));
    assert_eq!(e.get_element(0), None);
    // `undefined` is an element; a hole is not.
    assert_eq!(a.get_element(1), Some(Value::UNDEFINED));
}

#[test]
fn push_grows_the_record_by_half() {
    let mut r = Realm::new();
    let a = r.array(&[]);
    let n = if MIRI { 300 } else { 20_000 };
    let mut caps = Vec::new();
    for i in 0..n {
        a.set_element(&mut r.heap, i as u32, int(i)).unwrap();
        if caps.last() != Some(&a.elements_cap()) {
            caps.push(a.elements_cap());
        }
        assert_eq!(a.len() as usize, i + 1);
    }
    assert!(!a.is_sparse());
    assert!((0..n).all(|i| a.get_element(i as u32) == Some(int(i))));
    assert_eq!(a.get_element(n as u32), None);
    assert_eq!(caps[0], MIN_ELEMS);
    // Growth by 1.5: the capacity ends within 1.5 of the length, and the number of records is logarithmic.
    assert!(a.elements_cap() <= n + n / 2 + 1, "{}", a.elements_cap());
    assert!(caps.len() <= if MIRI { 14 } else { 25 }, "{} records", caps.len());
}

#[test]
fn a_write_past_the_end_leaves_holes_and_a_delete_makes_one() {
    let mut r = Realm::new();
    let a = r.array(&[]);
    a.set_element(&mut r.heap, 5, int(5)).unwrap();
    assert_eq!(a.len(), 6);
    assert_eq!(elements(a, 7), [None, None, None, None, None, Some(int(5)), None]);
    a.set_element(&mut r.heap, 1, int(1)).unwrap();
    a.delete_element(5);
    assert_eq!(a.len(), 6);
    assert_eq!(elements(a, 6), [None, Some(int(1)), None, None, None, None]);
    a.delete_element(1000);
    assert!(!a.is_sparse());
}

#[test]
fn the_length_can_move_without_memory_for_the_holes() {
    let mut r = Realm::new();
    let a = r.array(&[int(0), int(1), int(2), int(3)]);
    // `a.length = 2` deletes the elements at or above 2; growing again shows holes, not the old values.
    a.set_len(&mut r.heap, 2).unwrap();
    assert_eq!((a.len(), a.get_element(2)), (2, None));
    a.set_len(&mut r.heap, 4).unwrap();
    assert_eq!(elements(a, 4), [Some(int(0)), Some(int(1)), None, None]);
    // `new Array(1e6)`: a record of no values.
    let b = r.array(&[]);
    b.set_len(&mut r.heap, 1_000_000).unwrap();
    assert_eq!((b.len(), b.elements_cap(), b.is_sparse()), (1_000_000, 0, false));
    assert_eq!(b.get_element(999_999), None);
    // Filling it from the start stays dense.
    for i in 0..3000 {
        b.set_element(&mut r.heap, i, int(i as usize)).unwrap();
    }
    assert!(!b.is_sparse());
    assert_eq!((b.len(), b.get_element(2999)), (1_000_000, Some(int(2999))));
    b.set_len(&mut r.heap, 10).unwrap();
    assert_eq!((b.len(), b.get_element(9), b.get_element(10)), (10, Some(int(9)), None));
    b.set_len(&mut r.heap, 0).unwrap();
    assert_eq!(elements(b, 3), [None, None, None]);
    let c = r.array(&[]);
    c.set_len(&mut r.heap, 0).unwrap();
    assert_eq!(c.elements_cap(), 0);
}

#[test]
fn a_far_write_makes_the_record_sparse_and_a_dense_fill_makes_it_dense_again() {
    let mut r = Realm::new();
    let a = r.array(&[int(0), int(1)]);
    a.set_element(&mut r.heap, 5_000, int(5)).unwrap();
    assert!(a.is_sparse());
    assert_eq!(a.len(), 5_001);
    assert_eq!((a.get_element(0), a.get_element(1), a.get_element(5_000), a.get_element(4_999)), (Some(int(0)), Some(int(1)), Some(int(5)), None));
    // The record has room for a few pairs, not 5,000 values.
    assert!(a.elements_cap() <= 16, "{}", a.elements_cap());
    // The largest index and an index in the middle.
    a.set_element(&mut r.heap, u32::MAX - 1, int(9)).unwrap();
    a.set_element(&mut r.heap, 100_000, int(8)).unwrap();
    assert_eq!(a.len(), u32::MAX);
    assert_eq!((a.get_element(u32::MAX - 1), a.get_element(100_000), a.get_element(5_000)), (Some(int(9)), Some(int(8)), Some(int(5))));
    a.delete_element(100_000);
    assert_eq!(a.get_element(100_000), None);
    // Truncate: the pairs at or above the length go.
    a.set_len(&mut r.heap, 5_000).unwrap();
    assert_eq!((a.len(), a.get_element(5_000), a.get_element(1)), (5_000, None, Some(int(1))));
    // One element in four of the indexes up to the last: dense again.
    let b = r.array(&[]);
    b.set_element(&mut r.heap, 3_000, int(0)).unwrap();
    assert!(b.is_sparse());
    for i in (0..3_000).rev() {
        b.set_element(&mut r.heap, i, int(i as usize)).unwrap();
    }
    assert!(!b.is_sparse());
    assert_eq!(b.len(), 3_001);
    assert!((1..3_000).all(|i| b.get_element(i) == Some(int(i as usize))));
}

#[test]
#[should_panic(expected = "not an array index")]
fn the_last_u32_is_not_an_array_index() {
    let mut r = Realm::new();
    r.array(&[]).set_element(&mut r.heap, u32::MAX, int(1)).unwrap();
}

#[test]
#[should_panic(expected = "no elements word")]
fn a_plain_object_has_no_elements() {
    let mut r = Realm::new();
    let o = r.object(2);
    o.len();
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) % n
    }
}

#[test]
fn elements_follow_a_map_through_random_writes_deletes_and_length_changes() {
    for seed in 1..=if MIRI { 2u64 } else { 6 } {
        let mut r = Realm::new();
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let a = r.array(&[]);
        let mut model: BTreeMap<u32, Value> = BTreeMap::new();
        let mut len = 0u32;
        let steps = if MIRI { 300 } else { 6_000 };
        for step in 0..steps {
            // Indexes in a small range, a medium one, and anywhere.
            let i = match rng.below(10) {
                0..=5 => rng.below(40) as u32,
                6..=8 => rng.below(4_000) as u32,
                _ => rng.below(u32::MAX as u64 - 1) as u32,
            };
            match rng.below(10) {
                0..=6 => {
                    let v = int(step);
                    a.set_element(&mut r.heap, i, v).unwrap();
                    model.insert(i, v);
                    len = len.max(i + 1);
                }
                7 => {
                    a.delete_element(i);
                    model.remove(&i);
                }
                8 => {
                    let n = if rng.below(2) == 0 { rng.below(50) as u32 } else { rng.below(5_000) as u32 };
                    a.set_len(&mut r.heap, n).unwrap();
                    model.retain(|&k, _| k < n);
                    len = n;
                }
                _ => {
                    // Push at the end.
                    if len < u32::MAX - 1 {
                        let v = int(step);
                        a.set_element(&mut r.heap, len, v).unwrap();
                        model.insert(len, v);
                        len += 1;
                    }
                }
            }
            assert_eq!(a.len(), len, "seed {seed} step {step}");
            assert_eq!(a.get_element(i), model.get(&i).copied(), "seed {seed} step {step} index {i}");
            if step % 50 == 0 {
                assert!(model.iter().all(|(&k, &v)| a.get_element(k) == Some(v)), "seed {seed} step {step}");
                assert!((0..20).all(|k| a.get_element(k) == model.get(&k).copied()));
            }
        }
    }
}

// ---- the collector ----

#[test]
fn a_collection_follows_inline_slots_the_overflow_record_and_the_elements() {
    let mut r = Realm::new();
    // The keys first: `Realm::collect` roots them, so they are in the base.
    r.key(8);
    let base = r.collect(&[]);
    let leaf = |r: &mut Realm| r.object(1);
    let holder = r.object(2);
    // Values in the inline words, at the boundary, and in the record.
    let kept: Vec<Obj> = (0..8).map(|_| leaf(&mut r)).collect();
    for (i, o) in kept.iter().enumerate() {
        r.add(holder, i, o.value());
    }
    let dead = leaf(&mut r);
    let arr = r.array(&[]);
    let in_array: Vec<Obj> = (0..6).map(|_| leaf(&mut r)).collect();
    for (i, o) in in_array.iter().enumerate() {
        arr.set_element(&mut r.heap, i as u32 * 3, o.value()).unwrap();
    }
    let sparse_leaf = leaf(&mut r);
    arr.set_element(&mut r.heap, 7_000, sparse_leaf.value()).unwrap();
    assert!(arr.is_sparse());
    let a2 = r.array(&[holder.value(), arr.value()]);
    let s = r.collect(&[a2.value()]);
    // The cells: the array `a2` and its record, the holder and its record, 8 objects; `arr` and its record, 6 + 1 objects.
    assert_eq!(s.marked, base.marked + 2 + 2 + 8 + 2 + 7);
    // The live cells are not handed out again.
    let _ = dead;
    let fresh: Vec<NonNull<u8>> = (0..200).map(|_| leaf(&mut r).cell()).collect();
    assert!(kept.iter().chain(&in_array).chain([&sparse_leaf]).all(|o| !fresh.contains(&o.cell())));
    // The values are intact.
    assert!(kept.iter().enumerate().all(|(i, o)| holder.slot(&r.shapes, i as u32) == o.value()));
    assert!(in_array.iter().enumerate().all(|(i, o)| arr.get_element(i as u32 * 3) == Some(o.value())));
    assert_eq!(arr.get_element(7_000), Some(sparse_leaf.value()));
    // Without the root everything goes, and the record cells with it.
    r.collect(&[]);
    assert_eq!(r.collect(&[]).live_bytes, base.live_bytes);
}

#[test]
fn a_collection_frees_a_record_that_an_object_outgrew() {
    let mut r = Realm::new();
    let o = r.object(1);
    for n in 0..100 {
        r.add(o, n, int(n));
    }
    // Growth made garbage: the old records. A collection frees them and keeps the current one.
    let s = r.collect(&[o.value()]);
    // The object, its record, and the key atoms of its shape.
    assert_eq!(s.marked, 2 + 100);
    let t = r.collect(&[o.value()]);
    assert_eq!(t.live_bytes, s.live_bytes);
    assert!((0..100).all(|n| o.slot(&r.shapes, n) == int(n as usize)));
}

#[test]
fn a_long_array_of_objects_is_scanned_in_slices_and_its_dead_elements_die() {
    let mut r = Realm::new();
    let n = if MIRI { 3 * SLICE } else { 100 * SLICE };
    let leaf = r.object(0);
    let a = r.array(&[]);
    for i in 0..n {
        let o = if i % 2 == 0 { Obj::new(&mut r.heap, &r.shapes, leaf.shape(&r.shapes)).unwrap().value() } else { int(i) };
        a.set_element(&mut r.heap, i as u32, o).unwrap();
    }
    let s = r.collect(&[a.value()]);
    // The array, its record and the leaf objects of the even indexes: the odd ones are integers.
    assert_eq!(s.marked, 2 + (n / 2) as u64);
    // The work list holds the slice, not the array: well under the 8 bytes for each element that a full push needs.
    assert!(s.work_bytes <= (SLICE / crate::gc::CHUNK + 1) * crate::gc::CHUNK * 8 + 24 * 2, "{} bytes", s.work_bytes);
    assert!(s.work_bytes < n / 2 * 8, "{} bytes for {} cells", s.work_bytes, n / 2);
    // Truncating the array frees the elements above the new length: they are holes, and the leaves die.
    a.set_len(&mut r.heap, 10).unwrap();
    let t = r.collect(&[a.value()]);
    assert!(t.marked < 20, "{} cells", t.marked);
}

#[test]
fn a_sparse_record_longer_than_a_slice_is_scanned_whole() {
    let mut r = Realm::new();
    let base = r.collect(&[]);
    let a = r.array(&[]);
    // A sparse array with more pairs than one slice holds (a pair is two values).
    let m = SLICE;
    let mut objs = Vec::new();
    for i in 0..m {
        let o = r.object(0);
        a.set_element(&mut r.heap, (1_000_000 + i * 5000) as u32, o.value()).unwrap();
        objs.push(o);
    }
    assert!(a.is_sparse() && a.elements_cap() > SLICE);
    let s = r.collect(&[a.value()]);
    assert_eq!(s.marked, base.marked + 2 + m as u64);
    for (i, o) in objs.iter().enumerate() {
        assert_eq!(a.get_element((1_000_000 + i * 5000) as u32), Some(o.value()));
    }
}

#[test]
fn from_value_takes_objects_and_not_records() {
    let mut r = Realm::new();
    let o = r.object(1);
    let a = r.array(&[int(1)]);
    // SAFETY: the values are made here and live.
    unsafe {
        assert_eq!(Obj::from_value(o.value()), Some(o));
        assert_eq!(Obj::from_value(a.value()), Some(a));
        assert_eq!(Obj::from_value(int(1)), None);
        // The elements word is an object-kind pointer to a record; `from_value` refuses it.
        let rec = a.elems_word().read();
        assert!(rec.as_pointer().is_some());
        assert_eq!(Obj::from_value(rec), None);
    }
}

#[test]
fn the_kind_is_one_exclusive_field_and_no_flag_shares_a_bit_with_it() {
    let mut r = Realm::new();
    let o = r.object(1);
    assert_eq!(o.kind(), K_NONE);
    let all = [K_MAP, K_SET, K_WMAP, K_WSET, K_MITER, K_SITER, K_SITER_STR, K_WRAP, K_PROMISE, K_GEN, K_AGEN, K_REGEXP, K_DATE];
    for (i, k) in all.into_iter().enumerate() {
        assert_eq!(k as usize, i + 1, "the kinds are the numbers 1 to 13");
        o.set_flag(F_HOST | F_LAZYM | F_LAZYTAB | F_PROTO | F_OVER | F_NOEXT);
        o.set_kind(k);
        assert_eq!(o.kind(), k);
        for other in all {
            assert_eq!(o.is_kind(other), other == k);
        }
        assert!(o.has_flag(F_HOST | F_LAZYM | F_LAZYTAB | F_PROTO | F_OVER | F_NOEXT), "a kind does not clear a flag");
        o.clear_flag(F_HOST | F_LAZYM | F_LAZYTAB);
        assert_eq!(o.kind(), k, "clearing a flag keeps the kind");
        o.clear_flag(F_PROTO | F_OVER | F_NOEXT);
    }
    // A flag set and cleared on its own never touches the kind: each single flag is outside the kind field.
    o.set_kind(K_AGEN);
    for f in [F_HOST, F_LAZYM, F_LAZYTAB, F_PROTO, F_OVER, F_NOEXT, F_CALLABLE, F_CLASS, F_DERIVED, F_NATIVE, F_LAZY, F_FNPROPS, F_SLOWIDX] {
        o.set_flag(f);
        o.clear_flag(f);
        assert!(o.is_kind(K_AGEN));
    }
}
