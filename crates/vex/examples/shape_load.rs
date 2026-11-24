//! Micro rows for shapes (plan step S1.8).
//!   shape_load add N S      N objects, each adds S keys one by one to the root shape (the transitions exist after the first object)
//!   shape_load find N S     N lookups of a key in a shape of S keys (the keys in turn)
//!   shape_load loop N S     the loop of `find` with no lookup
//!   shape_load literal N S  N calls of `literal` with S keys (a root lookup, then S transitions)
//! Count with `tools/js/instr.sh` and subtract the row with N = 0.
use bare_vex::atoms::{Atoms, SharedAtoms};
use bare_vex::cell::Layout;
use bare_vex::heap::Heap;
use bare_vex::shapes::{Shapes, A_C, A_E, A_W};
use bare_vex::string::Chars;
use std::hint::black_box;
use std::sync::Arc;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let arg = |i: usize| -> usize { a.get(i).and_then(|s| s.parse().ok()).expect("usage: shape_load add|find|loop|literal N S") };
    let (n, s) = (arg(2), arg(3));
    let mut heap = Heap::default();
    let mut atoms = Atoms::new(Arc::new(SharedAtoms::default()));
    let keys: Vec<_> = (0..s).map(|i| atoms.intern(&mut heap, Chars::One(format!("key{i}").as_bytes())).expect("memory")).collect();
    let mut shapes = Shapes::default();
    let root = shapes.root(Layout::Object, None, s as u16);
    let all = keys.iter().fold(root, |sh, &k| shapes.add(sh, k, A_W | A_E | A_C).expect("fits"));
    let mut sum = 0u64;
    match a[1].as_str() {
        "add" => {
            for _ in 0..n {
                let mut sh = black_box(root);
                for &k in &keys {
                    sh = shapes.add(sh, k, A_W | A_E | A_C).expect("fits");
                }
                sum += sh.raw() as u64;
            }
        }
        "find" => {
            for i in 0..n {
                sum += shapes.find(black_box(all), keys[i % s]).map_or(0, |e| e.slot() as u64);
            }
        }
        // The loop of `find` without the lookup: the cost to subtract.
        "loop" => {
            for i in 0..n {
                sum += black_box(all).raw() as u64 + keys[i % s].cell().as_ptr() as u64;
            }
        }
        "literal" => {
            for _ in 0..n {
                sum += shapes.literal(Layout::Object, None, black_box(&keys)).map_or(0, |sh| sh.raw() as u64);
            }
        }
        _ => panic!("usage: shape_load add|find|loop|literal N S"),
    }
    println!("{} n={n} s={s} sum={sum} shapes={}", a[1], shapes.count());
}
