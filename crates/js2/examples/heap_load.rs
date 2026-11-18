//! Micro row for the allocator (plan step S1.3): N allocations of 32 bytes in rounds of 100,000. After each round a collection
//! keeps the last 1,000 cells (marked) and frees the rest; the next round reuses them through the lazy sweep. `malloc` runs the
//! same pattern with `Box` and the C library: it frees the 99,000 cells of each round at the end of the round.
//! Count with `tools/js/instr.sh`; subtract N = 0 for the cost per allocation.
//!   heap_load heap|malloc N
use bare_js2::heap::Heap;
use std::hint::black_box;

const ROUND: usize = 100_000;
const KEEP: usize = 1_000;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let n: usize = a.get(2).and_then(|s| s.parse().ok()).expect("usage: heap_load heap|malloc N");
    match a[1].as_str() {
        "heap" => {
            let mut h = Heap::default();
            let mut round = Vec::with_capacity(ROUND);
            for i in 0..n {
                let p = h.alloc(32).expect("memory");
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
            println!("heap allocations={n} mapped={}", h.mapped_bytes());
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
        _ => panic!("usage: heap_load heap|malloc N"),
    }
}
