//! Micro rows for the write barrier of the sticky-mark experiment (plan step S1.10). Count with `tools/js/instr.sh`; subtract `loop`.
//!   barrier_load loop N        the loop with no store
//!   barrier_load young N       N pointer stores into a young object (the barrier tests the flag and skips)
//!   barrier_load oldint N      N integer stores into an old object (the barrier tests the flag and the value tag)
//!   barrier_load oldptr N      N pointer stores into one old object (the first remembers it; the rest see the flag)
//!   barrier_load first N       one pointer store into each of N old objects (N slow paths, each into a cold cell)
//!   barrier_load setup N       the setup of `first` with no store
use bare_js2::atoms::{Atoms, SharedAtoms};
use bare_js2::cell::Layout;
use bare_js2::gc::{Marker, collect};
use bare_js2::heap::Heap;
use bare_js2::object::Obj;
use bare_js2::shapes::{A_C, A_E, A_W, Shapes};
use bare_js2::string::Chars;
use bare_js2::value::Value;
use std::hint::black_box;
use std::sync::Arc;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (row, n): (&str, usize) = (a.get(1).map_or("", String::as_str), a.get(2).and_then(|s| s.parse().ok()).expect("usage: barrier_load ROW N"));
    let (mut heap, mut shapes, mut atoms) = (Heap::default(), Shapes::default(), Atoms::new(Arc::new(SharedAtoms::default())));
    let mut shape = shapes.root(Layout::Object, None, 4);
    for i in 0..4 {
        let k = atoms.intern(&mut heap, Chars::One(format!("k{i}").as_bytes())).expect("memory");
        shape = shapes.add(shape, k, A_W | A_E | A_C).expect("memory");
    }
    let make = |heap: &mut Heap, shapes: &Shapes| Obj::new(heap, shapes, shape).expect("memory");
    let target = make(&mut heap, &shapes);
    let other = make(&mut heap, &shapes);
    let many: Vec<Obj> = if row == "first" || row == "setup" { (0..n).map(|_| make(&mut heap, &shapes)).collect() } else { Vec::new() };
    let mut roots: Vec<Value> = vec![target.value(), other.value()];
    roots.extend(many.iter().map(|o| o.value()));
    if row != "young" {
        // One full collection: every object that the roots name is old afterwards (in the base tree it is only an object).
        collect(&mut heap, &mut shapes, &mut atoms, &mut |m: &mut Marker| m.values(&roots));
    }
    let ptr = other.value();
    match row {
        "loop" | "setup" => {
            for i in 0..n {
                black_box(i);
            }
        }
        "young" | "oldptr" => {
            let t = if row == "young" { make(&mut heap, &shapes) } else { target };
            for i in 0..n {
                t.set_slot(&shapes, black_box(i as u32 & 3), black_box(ptr));
            }
        }
        "oldint" => {
            for i in 0..n {
                target.set_slot(&shapes, black_box(i as u32 & 3), black_box(Value::int(i as i32)));
            }
        }
        "first" => {
            for o in &many {
                o.set_slot(&shapes, black_box(0), black_box(ptr));
            }
        }
        _ => panic!("rows: loop young oldint oldptr first setup"),
    }
    black_box(&many);
}
