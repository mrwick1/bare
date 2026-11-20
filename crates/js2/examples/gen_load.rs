//! Workload for the sticky-mark experiment (plan step S1.10). A mutator makes small objects in rounds (one round is one task) and keeps
//! a share of them in a ring array, so a kept object dies when its slot is overwritten. Between rounds a policy decides the collection.
//!   gen_load POLICY SURV LIFE ROUNDS [NURSERY_KB] [MUT] [STATIC]
//! - POLICY: `none` (never collect), `full` (the rule of the old engine: `Heap::due`), `fulln` (a full collection each NURSERY of
//!   growth), `gen` (a minor collection each NURSERY of growth, a full one when the old bytes plus a nursery pass the limit of `full`: 8 MiB or 1.75 times the live bytes of the last full collection).
//!   Build with `RUSTFLAGS="--cfg base"` for the tree that has no minor collection: `gen` is then not available.
//! - SURV: the percent of new objects that are kept. LIFE: rounds that a kept object lives (the ring holds LIFE * SURV% * 20,000).
//! - MUT: stores of a young pointer into a random kept (old) object for each round; default 200.
//! - STATIC: objects of a long-lived binary tree (the old data of the page) that exists from the start; default 0. Run with ROUNDS = 0
//!   for the cost of the setup.
//! One round makes 20,000 objects of 4 slots (48 bytes): 960,000 bytes, 4 stores each, plus the ring store of a kept one.
#![allow(unexpected_cfgs)]
use bare_js2::atoms::{Atoms, SharedAtoms};
use bare_js2::cell::Layout;
use bare_js2::gc::{Marker, collect};
use bare_js2::heap::Heap;
use bare_js2::object::Obj;
use bare_js2::shapes::{A_C, A_E, A_W, Shapes};
use bare_js2::string::Chars;
use bare_js2::value::Value;
use std::sync::Arc;

const PER_ROUND: usize = 20_000;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let arg = |i: usize, d: usize| a.get(i).map_or(d, |s| s.parse().expect("number"));
    let policy = a.get(1).map_or("", String::as_str).to_string();
    let (surv, life, rounds, nursery, muts, statics) = (arg(2, 22), arg(3, 3), arg(4, 100), arg(5, 2048) << 10, arg(6, 200), arg(7, 0));
    let (mut heap, mut shapes, mut atoms) = (Heap::default(), Shapes::default(), Atoms::new(Arc::new(SharedAtoms::default())));
    let mut shape = shapes.root(Layout::Object, None, 4);
    for i in 0..4 {
        let k = atoms.intern(&mut heap, Chars::One(format!("k{i}").as_bytes())).expect("memory");
        shape = shapes.add(shape, k, A_W | A_E | A_C).expect("memory");
    }
    let arr_shape = shapes.root(Layout::Indexed, None, 0);
    // Pins: an object of the shape (the shapes live while an object has them) and the ring.
    let pin = Obj::new(&mut heap, &shapes, shape).expect("memory");
    let ring = Obj::array(&mut heap, &shapes, arr_shape, &[]).expect("memory");
    // The static tree: object i names 2i+1 and 2i+2. It is old after the first full collection.
    let tree: Vec<Obj> = (0..statics).map(|_| Obj::new(&mut heap, &shapes, shape).expect("memory")).collect();
    for (i, o) in tree.iter().enumerate() {
        for k in 0..4 {
            let v = if k < 2 { tree.get(2 * i + 1 + k).map_or(Value::int(0), |c| c.value()) } else { Value::int(k as i32) };
            o.set_slot(&shapes, k as u32, v);
        }
    }
    let tree_root = tree.first().map_or(Value::UNDEFINED, |o| o.value());
    drop(tree);
    collect(&mut heap, &mut shapes, &mut atoms, &mut |m: &mut Marker| m.values(&[pin.value(), ring.value(), tree_root]));
    let cap = (life * surv * PER_ROUND / 100).max(1);
    let (mut minors, mut fulls, mut marked, mut remembered) = (0u64, 0u64, 0u64, 0usize);
    let (mut peak, mut live_full, mut old_bytes) = (0usize, heap.marked_bytes(), heap.marked_bytes());
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (mut next_slot, mut kept) = (0usize, 0usize);
    let mut sample = 0usize;
    let full_limit = |live: usize| (8usize << 20).max(live / 4 * 7);
    for _ in 0..rounds {
        let mut newest = Value::UNDEFINED;
        for _ in 0..PER_ROUND {
            let o = Obj::new(&mut heap, &shapes, shape).expect("memory");
            o.set_slot(&shapes, 0, Value::int(0));
            for k in 1..4 {
                o.set_slot(&shapes, k, Value::int(k as i32));
            }
            newest = o.value();
            if rng.next() % 100 < surv as u64 {
                ring.set_element(&mut heap, next_slot as u32, o.value()).expect("memory");
                next_slot = (next_slot + 1) % cap;
                kept += 1;
            }
            sample += 1;
            if sample % 500 == 0 {
                peak = peak.max(heap.mapped_bytes());
            }
        }
        // Stores of a young pointer into kept objects: most are old by now.
        for _ in 0..muts {
            let j = (rng.next() % cap.min(kept.max(1)) as u64) as u32;
            // SAFETY: the ring holds live objects of this heap.
            if let Some(o) = ring.get_element(j).and_then(|v| unsafe { Obj::from_value(v) }) {
                o.set_slot(&shapes, 1, newest);
            }
        }
        peak = peak.max(heap.mapped_bytes());
        let roots = [pin.value(), ring.value(), tree_root];
        match policy.as_str() {
            "none" => {}
            "full" if heap.due() => {
                marked += collect(&mut heap, &mut shapes, &mut atoms, &mut |m: &mut Marker| m.values(&roots)).marked;
                fulls += 1;
            }
            "fulln" if heap.grown_bytes() >= nursery => {
                marked += collect(&mut heap, &mut shapes, &mut atoms, &mut |m: &mut Marker| m.values(&roots)).marked;
                fulls += 1;
            }
            #[cfg(not(base))]
            "gen" if heap.grown_bytes() >= nursery => {
                if old_bytes + nursery > full_limit(live_full) {
                    marked += collect(&mut heap, &mut shapes, &mut atoms, &mut |m: &mut Marker| m.values(&roots)).marked;
                    fulls += 1;
                    live_full = heap.marked_bytes();
                } else {
                    let s = bare_js2::gc::collect_minor(&mut heap, &mut shapes, &mut |m: &mut Marker| m.values(&roots));
                    marked += s.marked;
                    remembered += s.remembered;
                    minors += 1;
                }
                old_bytes = heap.marked_bytes();
            }
            _ => {}
        }
    }
    println!("{policy} surv={surv} life={life} rounds={rounds} nursery={}KiB mut={muts} minors={minors} fulls={fulls} marked={marked} remembered={remembered} peak_mapped={peak} end_mapped={} end_marked={}", nursery >> 10, heap.mapped_bytes(), heap.marked_bytes());
    let _ = (&mut live_full, &mut old_bytes, &mut remembered, &mut minors, full_limit(0));
}
