//! Micro rows for strings (plan step S1.7). Count with `tools/js/instr.sh`; subtract N = 0 for the cost per step.
//!   string_load append|prepend|short|atom N
//! - `append`: `s = s + 'ab'` N times, then one flatten and a read of the last unit (the JS row `s += 'ab'`).
//! - `prepend`: `s = 'ab' + s` N times, then one flatten and a read of the first unit.
//! - `short`: `t = 'ab' + 'cd'` N times, a copy of 4 units each time. After each round of 100,000 a collection frees them all.
//! - `atom`: N interns of one of 1,000 names that the realm already has (a table hit; no allocation).
//!
//! The same JS programs run on QuickJS in `docs/progress.md` (entry S1.7).
use bare_vex::atoms::{Atoms, SharedAtoms};
use bare_vex::heap::Heap;
use bare_vex::string::{Chars, Str};
use std::hint::black_box;
use std::sync::Arc;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let n: usize = a.get(2).and_then(|s| s.parse().ok()).expect("usage: string_load append|prepend|short|atom N");
    let mut h = Heap::default();
    let ab = Str::new(&mut h, Chars::One(b"ab")).unwrap();
    match a[1].as_str() {
        "append" | "prepend" => {
            let front = a[1] == "prepend";
            let mut s = Str::new(&mut h, Chars::One(b"")).unwrap();
            for _ in 0..n {
                s = black_box(if front { Str::concat(&mut h, ab, s) } else { Str::concat(&mut h, s, ab) }.unwrap());
            }
            let f = s.flatten(&mut h).unwrap();
            let unit = f.unit_at(if front { 0 } else { f.len().saturating_sub(1) });
            println!("{} len={} unit={unit:?} mapped={}", a[1], f.len(), h.mapped_bytes());
        }
        "short" => {
            let cd = Str::new(&mut h, Chars::One(b"cd")).unwrap();
            let mut len = 0;
            for i in 0..n {
                len += black_box(Str::concat(&mut h, ab, cd).unwrap()).len();
                if i % 100_000 == 99_999 {
                    h.start_mark();
                    h.finish_mark();
                }
            }
            println!("short len={len} mapped={}", h.mapped_bytes());
        }
        "atom" => {
            let mut atoms = Atoms::new(Arc::new(SharedAtoms::default()));
            let names: Vec<String> = (0..1000).map(|i| format!("name{i}")).collect();
            for x in &names {
                atoms.intern(&mut h, Chars::One(x.as_bytes())).unwrap();
            }
            let mut sum = 0;
            for i in 0..n {
                sum += black_box(atoms.intern(&mut h, Chars::One(names[i % 1000].as_bytes())).unwrap()).len();
            }
            println!("atom sum={sum} atoms={} mapped={}", atoms.runtime().len(), h.mapped_bytes());
        }
        _ => panic!("usage: string_load append|prepend|short|atom N"),
    }
}
