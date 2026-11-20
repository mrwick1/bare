//! Micro rows for the cell header (plan step S1.4): object cells of 32 bytes (header and 3 words) with S shapes.
//!   header_load alloc N S     N allocations in rounds of 100,000; each round a collection keeps the last 1,000 cells and their shapes
//!   header_load mark R S      R collections of a graph of 100,000 object and leaf cells, each cell visited once (S shapes)
//!   header_load hit N         N property reads with a shape check that always hits (one cached shape)
//!   header_load miss N S      N property reads whose cell has one of S shapes, so the check fails and the shape is read
//!   header_load flags N       N flag changes on string cells (read the flags, set a bit, write the header back), as `Str::mark_atom` does
//! Count with `tools/js/instr.sh` and subtract the row with N = 0 (or R = 0). Build with and without `--features shape-ptr`.
use bare_js2::cell::{CellHeader, Layout, Shape, Shapes};
use bare_js2::heap::Heap;
use std::hint::black_box;
use std::ptr::NonNull;

const ROUND: usize = 100_000;
const KEEP: usize = 1_000;
const CELLS: usize = 100_000;

fn rng(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

fn shapes(h: &mut Heap, n: usize) -> (Shapes, Vec<CellHeader>) {
    let mut sh = Shapes::default();
    let hs = (0..n.max(1)).map(|i| {
        let r = sh.add(h, Shape { layout: Layout::Object, flags: 1, slots: 2 + (i % 5) as u32 }).expect("memory");
        sh.header(r)
    });
    let hs = hs.collect();
    (sh, hs)
}

fn write(p: NonNull<u8>, hd: CellHeader, a: u64, b: u64) {
    // SAFETY: a cell of 32 bytes: the header and 3 words.
    unsafe {
        p.as_ptr().cast::<CellHeader>().write(hd);
        let w = p.as_ptr().cast::<u64>();
        w.add(1).write(a);
        w.add(2).write(b);
    }
}

fn read(p: NonNull<u8>) -> (CellHeader, u64, u64) {
    // SAFETY: as in `write`.
    unsafe {
        let w = p.as_ptr().cast::<u64>();
        (p.as_ptr().cast::<CellHeader>().read(), w.add(1).read(), w.add(2).read())
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let arg = |i: usize| -> usize { a.get(i).and_then(|s| s.parse().ok()).expect("usage: header_load alloc N S | mark R S | hit N | miss N S | flags N") };
    let mut h = Heap::default();
    match a[1].as_str() {
        "alloc" => {
            let (n, s) = (arg(2), arg(3));
            let (mut sh, hs) = shapes(&mut h, s);
            let mut round = Vec::with_capacity(ROUND);
            for i in 0..n {
                let p = h.alloc(32).expect("memory");
                write(p, hs[i % hs.len()], i as u64, 0);
                round.push(black_box(p));
                if round.len() == ROUND {
                    h.start_mark();
                    for &p in &round[ROUND - KEEP..] {
                        // SAFETY: a live cell of `h`.
                        unsafe { h.mark(p) };
                        sh.note_live(&mut h, read(p).0);
                    }
                    sh.finish_marks(&mut h);
                    h.finish_mark();
                    round.clear();
                }
            }
            println!("alloc n={n} shapes={} mapped={} table={}", sh.len(), h.mapped_bytes(), sh.table_bytes());
        }
        "mark" => {
            let (r, s) = (arg(2), arg(3));
            let (mut sh, hs) = shapes(&mut h, s);
            let mut seed = 88172645463325252u64;
            let cells: Vec<NonNull<u8>> = (0..CELLS).map(|_| h.alloc(32).expect("memory")).collect();
            for (i, &p) in cells.iter().enumerate() {
                // Three cells in four are objects with two children; the rest are leaves (a one-byte string).
                if i % 4 != 3 {
                    let (x, y) = (cells[rng(&mut seed) as usize % CELLS], cells[rng(&mut seed) as usize % CELLS]);
                    write(p, hs[rng(&mut seed) as usize % hs.len()], x.as_ptr() as u64, y.as_ptr() as u64);
                } else {
                    write(p, CellHeader::new(Layout::OneByte, 0), 0, 0);
                }
            }
            let mut marked = 0;
            let mut stack: Vec<NonNull<u8>> = Vec::new();
            for _ in 0..r {
                h.start_mark();
                for &root in cells.iter().step_by(97) {
                    // SAFETY: a live cell of `h`.
                    if unsafe { h.mark(root) } {
                        stack.push(root);
                    }
                }
                while let Some(p) = stack.pop() {
                    marked += 1;
                    let (hd, x, y) = read(p);
                    if hd.layout() == Layout::Object {
                        sh.note_live(&mut h, hd);
                        for c in [x, y] {
                            let c = NonNull::new(c as *mut u8).expect("child");
                            // SAFETY: a child is a cell of `h` that the graph keeps.
                            if unsafe { h.mark(c) } {
                                stack.push(c);
                            }
                        }
                    }
                }
                sh.finish_marks(&mut h);
                h.finish_mark();
            }
            println!("mark rounds={r} visited={marked} shapes={} mapped={} table={}", sh.len(), h.mapped_bytes(), sh.table_bytes());
        }
        "hit" | "miss" => {
            let n = arg(2);
            let s = if a[1] == "miss" { arg(3) } else { 1 };
            let (sh, hs) = shapes(&mut h, s);
            let mut seed = 88172645463325252u64;
            let cells: Vec<NonNull<u8>> = (0..CELLS).map(|_| h.alloc(32).expect("memory")).collect();
            for &p in &cells {
                write(p, hs[rng(&mut seed) as usize % hs.len()], 1, 2);
            }
            // The cache of the site holds the first shape (the shape of cell 0 when s = 1).
            let cached = black_box(hs[0]).shape_key();
            let mut sum = 0u64;
            for i in 0..n {
                let (hd, x, _) = read(cells[i % CELLS]);
                if hd.shape_key() == cached {
                    sum = sum.wrapping_add(x);
                } else {
                    sum = sum.wrapping_add(x).wrapping_add(sh.of(hd).slots as u64);
                }
            }
            println!("{} n={n} shapes={s} sum={sum}", a[1]);
        }
        "flags" => {
            let n = arg(2);
            let cells: Vec<NonNull<u8>> = (0..CELLS).map(|_| h.alloc(32).expect("memory")).collect();
            for (i, &p) in cells.iter().enumerate() {
                write(p, CellHeader::new(if i % 2 == 0 { Layout::OneByte } else { Layout::TwoByte }, (i % 2) as u32), 0, 0);
            }
            let mut sum = 0u64;
            for i in 0..n {
                let p = cells[i % CELLS];
                let (hd, x, _) = read(p);
                let hd = hd.with_flags(hd.flags() ^ black_box(2));
                write(p, hd, x, 0);
                sum += hd.flags() as u64;
            }
            println!("flags n={n} sum={sum}");
        }
        _ => panic!("usage: header_load alloc N S | mark R S | hit N | miss N S | flags N"),
    }
}
