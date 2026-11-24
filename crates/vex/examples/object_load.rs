//! Micro rows for objects (plan step S1.9). Count with `tools/js/instr.sh`; subtract the run with N = 0.
//!   object_load new N      N empty objects with 4 inline slots
//!   object_load add N      N objects, each given 4 properties (subtract `new`, divide by 4 for one `Obj::add`; the transitions are made by the first)
//!   object_load slot N     N reads of an inline slot
//!   object_load over N     N reads of a slot in the overflow record (24 properties, 8 inline)
//!   object_load loop N     the loop of `slot`, `over` and `get` with no object (subtract it from them)
//!   object_load push N     N pushes to one array (growth included)
//!   object_load get N      N reads of an element of a dense array of 1,000
use bare_vex::atoms::{Atoms, SharedAtoms};
use bare_vex::cell::Layout;
use bare_vex::heap::Heap;
use bare_vex::object::Obj;
use bare_vex::shapes::{A_C, A_E, A_W, Shapes};
use bare_vex::string::Chars;
use bare_vex::value::Value;
use std::hint::black_box;
use std::sync::Arc;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (row, n): (&str, usize) = (a.get(1).map_or("", String::as_str), a.get(2).and_then(|s| s.parse().ok()).expect("usage: object_load ROW N"));
    let (mut heap, mut shapes, mut atoms) = (Heap::default(), Shapes::default(), Atoms::new(Arc::new(SharedAtoms::default())));
    let keys: Vec<_> = (0..24).map(|i| atoms.intern(&mut heap, Chars::One(format!("k{i}").as_bytes())).expect("memory")).collect();
    let w = A_W | A_E | A_C;
    let root4 = shapes.root(Layout::Object, None, 4);
    let root8 = shapes.root(Layout::Object, None, 8);
    let arr = shapes.root(Layout::Indexed, None, 0);
    match row {
        "new" => (0..n).for_each(|_| {
            black_box(Obj::new(&mut heap, &shapes, root4).expect("memory"));
        }),
        "add" => (0..n).for_each(|_| {
            let o = Obj::new(&mut heap, &shapes, root4).expect("memory");
            keys[..4].iter().for_each(|&k| {
                black_box(o.add(&mut heap, &mut shapes, k, w, &[Value::int(1)]));
            });
        }),
        "slot" | "over" => {
            let (root, count, at) = if row == "slot" { (root4, 4, 3) } else { (root8, 24, 20) };
            let o = Obj::new(&mut heap, &shapes, root).expect("memory");
            keys[..count].iter().for_each(|&k| {
                o.add(&mut heap, &mut shapes, k, w, &[Value::int(7)]);
            });
            let mut sum = 0;
            for _ in 0..n {
                sum += black_box(o.slot(&shapes, black_box(at))).as_int().unwrap_or(0);
            }
            black_box(sum);
        }
        "loop" => {
            // The skeleton of `slot`, `over` and `get`: the same loop with no object.
            let mut sum = 0;
            for i in 0..n {
                sum += black_box(Some(Value::int(black_box(i as i32)))).and_then(Value::as_int).unwrap_or(0);
            }
            black_box(sum);
        }
        "push" => {
            let o = Obj::new(&mut heap, &shapes, arr).expect("memory");
            for i in 0..n {
                o.set_element(&mut heap, i as u32, Value::int(i as i32)).expect("memory");
            }
            black_box(o.len());
        }
        "get" => {
            let values: Vec<Value> = (0..1000).map(Value::int).collect();
            let o = Obj::array(&mut heap, &shapes, arr, &values).expect("memory");
            let mut sum = 0;
            for i in 0..n {
                sum += black_box(o.get_element(black_box((i % 1000) as u32))).and_then(Value::as_int).unwrap_or(0);
            }
            black_box(sum);
        }
        _ => panic!("rows: new add slot over loop push get"),
    }
}
