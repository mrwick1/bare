//! Micro row for the collector (plan step S1.5): N objects of 4 slots (`object.rs`, 5 inline words), all reachable from one root, then R
//! collections of the whole graph. `chain`: slot 0 links to the next object and slots 1 to 3 to random objects. `tree`: slots 0
//! and 1 link to the children of a binary tree, slots 2 and 3 are integers. Prints the peak scratch of the collection.
//! Count with `tools/js/instr.sh`; subtract R = 0 for the cost of R collections.
//!   gc_load chain|tree N R
use bare_vex::atoms::{Atoms, SharedAtoms};
use bare_vex::cell::{Layout, Shapes};
use bare_vex::shapes::{A_C, A_E, A_W};
use bare_vex::string::Chars;
use bare_vex::gc::{Marker, collect};
use bare_vex::heap::Heap;
use bare_vex::object::Obj;
use bare_vex::value::Value;
use std::sync::Arc;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (mode, n, r): (&str, usize, usize) = match (a.get(1), a.get(2).and_then(|s| s.parse().ok()), a.get(3).and_then(|s| s.parse().ok())) {
        (Some(m), Some(n), Some(r)) if m == "chain" || m == "tree" => (if m == "chain" { "chain" } else { "tree" }, n, r),
        _ => panic!("usage: gc_load chain|tree N R"),
    };
    let (mut heap, mut shapes, mut atoms) = (Heap::default(), Shapes::default(), Atoms::new(Arc::new(SharedAtoms::default())));
    // The shape of every object: the keys k0 to k3. The keys are atoms and a root of the collection keeps them through the shape.
    let mut shape = shapes.root(Layout::Object, None, 4);
    let keys: Vec<_> = (0..4).map(|i| atoms.intern(&mut heap, Chars::One(format!("k{i}").as_bytes())).expect("memory")).collect();
    for &k in &keys {
        shape = shapes.add(shape, k, A_W | A_E | A_C).expect("memory");
    }
    let cells: Vec<Obj> = (0..n).map(|_| Obj::new(&mut heap, &shapes, shape).expect("memory")).collect();
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    for (i, &p) in cells.iter().enumerate() {
        let link = |j: usize| cells[j.min(n - 1)].value();
        let mut rnd = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 11) as usize % n
        };
        let slots = if mode == "chain" { [link(i + 1), link(rnd()), link(rnd()), link(rnd())] } else { [link(2 * i + 1), link(2 * i + 2), Value::int(1), Value::int(2)] };
        for (k, v) in slots.into_iter().enumerate() {
            p.set_slot(&shapes, k as u32, v);
        }
    }
    let root = cells[0].value();
    // A tree leaf links to a cell beyond the end: `link` clamps it to the last cell, which is fine for marking.
    drop(cells);
    let mut last = Default::default();
    for _ in 0..r {
        last = collect(&mut heap, &mut shapes, &mut atoms, &mut |m: &mut Marker| m.value(root));
    }
    println!("{mode} objects={n} collections={r} {last:?}");
}
