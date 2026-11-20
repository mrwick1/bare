//! Micro row for the allocator (plan step S1.3): N allocations of 32 bytes in rounds of 100,000. After each round a collection
//! keeps the last 1,000 cells (marked) and frees the rest; the next round reuses them through the lazy sweep. `malloc` runs the
//! same pattern with `Box` and the C library: it frees the 99,000 cells of each round at the end of the round.
//! `try` is `heap` with `Heap::try_alloc` (S1.6: the same fast path, plus the task limit on the slow path). `task` makes N objects of
//! 2 slots with `alloc_in_task`, keeps the last 1,000 in a native frame, and lets the trigger collect (no manual collection).
//! Count with `tools/js/instr.sh`; subtract N = 0 for the cost per allocation.
//!   heap_load heap|try|task|malloc N
use bare_js2::atoms::{Atoms, SharedAtoms};
use bare_js2::cell::{CellHeader, Layout, Shapes};
use bare_js2::gc::Marker;
use bare_js2::heap::Heap;
use bare_js2::roots::{Frames, RootMaps, alloc_in_task};
use bare_js2::value::{Kind, Value};
use std::hint::black_box;
use std::sync::Arc;

const ROUND: usize = 100_000;
const KEEP: usize = 1_000;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let n: usize = a.get(2).and_then(|s| s.parse().ok()).expect("usage: heap_load heap|try|task|malloc N");
    match a[1].as_str() {
        "heap" | "try" => {
            let try_alloc = a[1] == "try";
            let mut h = Heap::default();
            let mut round = Vec::with_capacity(ROUND);
            for i in 0..n {
                let p = if try_alloc { h.try_alloc(32) } else { h.alloc(32) }.expect("memory");
                // SAFETY: the cell has 32 bytes.
                unsafe { p.as_ptr().write(i as u8) };
                round.push(black_box(p));
                if round.len() == ROUND {
                    h.start_mark();
                    for &p in &round[ROUND - KEEP..] {
                        // SAFETY: a live cell of `h`.
                        unsafe { h.mark(p) };
                    }
                    h.finish_mark();
                    round.clear();
                }
            }
            println!("{} allocations={n} mapped={}", a[1], h.mapped_bytes());
        }
        "task" => {
            let (mut h, mut shapes, mut atoms) = (Heap::default(), Shapes::default(), Atoms::new(Arc::new(SharedAtoms::default())));
            let shape = shapes.root(Layout::Object, None, 2);
            let (mut frames, codes): (Frames, Vec<RootMaps>) = (Frames::default(), Vec::new());
            frames.push_native(KEEP);
            let mut collections = 0;
            for i in 0..n {
                let mut roots = |m: &mut Marker| {
                    collections += 1;
                    m.shape(shape);
                    frames.walk(m, &codes);
                };
                let p = alloc_in_task(&mut h, &mut shapes, &mut atoms, &mut roots, CellHeader::SIZE + 16).expect("memory");
                // SAFETY: a new cell of 24 bytes: the header and 2 slots.
                unsafe {
                    p.cast::<CellHeader>().write(shapes.header(shape));
                    p.as_ptr().add(CellHeader::SIZE).cast::<Value>().write(Value::int(i as i32));
                    p.as_ptr().add(CellHeader::SIZE + 8).cast::<Value>().write(Value::UNDEFINED);
                }
                frames.regs()[i % KEEP] = Value::pointer(Kind::Object, p);
            }
            println!("task allocations={n} collections={collections} mapped={} grown={}", h.mapped_bytes(), h.grown_bytes());
        }
        "malloc" => {
            let mut kept: Vec<Box<[u8; 32]>> = Vec::new();
            let mut round = Vec::with_capacity(ROUND);
            for i in 0..n {
                round.push(black_box(Box::new([i as u8; 32])));
                if round.len() == ROUND {
                    kept = round.split_off(ROUND - KEEP);
                    round.clear();
                }
            }
            println!("malloc allocations={n} kept={}", kept.len());
        }
        _ => panic!("usage: heap_load heap|try|task|malloc N"),
    }
}
