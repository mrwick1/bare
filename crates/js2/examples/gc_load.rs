//! Micro row for the collector (plan step S1.5): N objects of 4 slots (the stub layout), all reachable from one root, then R
//! collections of the whole graph. `chain`: slot 0 links to the next object and slots 1 to 3 to random objects. `tree`: slots 0
//! and 1 link to the children of a binary tree, slots 2 and 3 are integers. Prints the peak scratch of the collection.
//! Count with `tools/js/instr.sh`; subtract R = 0 for the cost of R collections.
//!   gc_load chain|tree N R
use bare_js2::atoms::{Atoms, SharedAtoms};
use bare_js2::cell::{CellHeader, Layout, Shape, Shapes};
use bare_js2::gc::{Marker, collect};
use bare_js2::heap::Heap;
use bare_js2::value::{Kind, Value};
use std::ptr::NonNull;
use std::sync::Arc;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (mode, n, r): (&str, usize, usize) = match (a.get(1), a.get(2).and_then(|s| s.parse().ok()), a.get(3).and_then(|s| s.parse().ok())) {
        (Some(m), Some(n), Some(r)) if m == "chain" || m == "tree" => (if m == "chain" { "chain" } else { "tree" }, n, r),
        _ => panic!("usage: gc_load chain|tree N R"),
    };
    let (mut heap, mut shapes, mut atoms) = (Heap::default(), Shapes::default(), Atoms::new(Arc::new(SharedAtoms::default())));
    let shape = shapes.add(&mut heap, Shape { layout: Layout::Object, flags: 0, slots: 4 }).expect("memory");
    let header = shapes.header(shape);
    let cells: Vec<NonNull<u8>> = (0..n)
        .map(|_| {
            let p = heap.alloc(CellHeader::SIZE + 32).expect("memory");
            // SAFETY: a new cell of 40 bytes: the header and 4 slots.
            unsafe { p.cast::<CellHeader>().write(header) };
            p
        })
        .collect();
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    for (i, &p) in cells.iter().enumerate() {
        let link = |j: usize| Value::pointer(Kind::Object, cells[j.min(n - 1)]);
        let mut rnd = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 11) as usize % n
        };
        let slots = if mode == "chain" { [link(i + 1), link(rnd()), link(rnd()), link(rnd())] } else { [link(2 * i + 1), link(2 * i + 2), Value::int(1), Value::int(2)] };
        for (k, v) in slots.into_iter().enumerate() {
            // SAFETY: the cell has 4 slots after its header.
            unsafe { p.as_ptr().add(CellHeader::SIZE).cast::<Value>().add(k).write(v) };
        }
    }
    let root = Value::pointer(Kind::Object, cells[0]);
    // A tree leaf links to a cell beyond the end: `link` clamps it to the last cell, which is fine for marking.
    drop(cells);
    let mut last = Default::default();
    for _ in 0..r {
        last = collect(&mut heap, &mut shapes, &mut atoms, &mut |m: &mut Marker| m.value(root));
    }
    println!("{mode} objects={n} collections={r} {last:?}");
}
