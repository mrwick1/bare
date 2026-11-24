//! Executed ops, pairs and triples of a script (plan step S2.9). Needs the feature `op-count`:
//!   cargo run --release -p bare-vex --features op-count --example op_count -- [--top N] FILE...
//! FILE is an `eval_load` program as `eval_load CASE SCALE --source` prints it (the expected result on the first line, then the source),
//! or a script whose first line is `//= RESULT` (`examples/data/bundle_like.js`).
//! The result is checked. The prelude and the compile are not counted: the counters start when the main code of the script starts.
//! For each file it prints the dispatches, the most frequent ops, the most frequent pairs (any control flow), the most frequent
//! fusable pairs and triples (the second and third op follow the first in the code and no jump lands on them), and the dispatches that
//! greedy fusion of the top pairs and triples would remove, measured on the recorded run.
use bare_vex::opcount::{self, Counts};
use bare_vex::script;
use std::collections::HashSet;

/// Dispatches that fusing `pairs` and `triples` would remove from `trace`: left to right, a triple first, then a pair.
fn removed(trace: &[u16], pairs: &HashSet<(u8, u8)>, triples: &HashSet<(u8, u8, u8)>) -> u64 {
    let (mut i, mut n) = (0, 0u64);
    let link = |j: usize| trace[j] & 0x100 != 0;
    while i < trace.len() {
        if i + 2 < trace.len() && link(i + 1) && link(i + 2) && triples.contains(&(trace[i] as u8, trace[i + 1] as u8, trace[i + 2] as u8)) {
            n += 2;
            i += 3;
        } else if i + 1 < trace.len() && link(i + 1) && pairs.contains(&(trace[i] as u8, trace[i + 1] as u8)) {
            n += 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    n
}

fn pct(n: u64, of: u64) -> f64 {
    n as f64 * 100.0 / of.max(1) as f64
}

fn main() {
    let mut a = std::env::args().skip(1).peekable();
    let top = if a.next_if(|s| s == "--top").is_some() { a.next().and_then(|n| n.parse().ok()).unwrap_or(12) } else { 12 };
    for file in a {
        let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{file}: {e}"));
        // A file of `eval_load` is the expected result, a newline and the source. Any other file starts with `//= RESULT`.
        let (want, src) = match text.strip_prefix("//= ") {
            Some(rest) => (rest.split('\n').next().unwrap_or(""), text.as_str()),
            None => text.split_once('\n').expect("expected result, newline, source"),
        };
        let mut vm = script::load(&script::eval_program(src)).unwrap_or_else(|e| panic!("{file}: {e}"));
        opcount::reset();
        opcount::record(400_000_000);
        if vm.run_main(0).is_err() {
            panic!("{file}: the script threw");
        }
        let got = vm.global_text("__result");
        let (c, trace) = (opcount::snapshot(), opcount::take_trace());
        println!("## {file}: result {} ({got})", if got == want { "ok" } else { "WRONG" });
        let d = c.dispatches;
        println!("dispatches {d}, wide {} ({:.2}%), trace {}", c.wide, pct(c.wide, d), if trace.len() as u64 == d { "complete" } else { "cut" });
        let mut ops: Vec<(usize, u64)> = c.ops.iter().copied().enumerate().filter(|x| x.1 > 0).collect();
        ops.sort_by_key(|x| std::cmp::Reverse(x.1));
        println!("ops: {}", ops.iter().take(top).map(|(o, n)| format!("{} {n} ({:.1}%)", Counts::name(*o), pct(*n, d))).collect::<Vec<_>>().join(", "));
        let list = |t: &[u64]| {
            let mut v: Vec<(usize, u64)> = t.iter().copied().enumerate().filter(|x| x.1 > 0).collect();
            v.sort_by_key(|x| std::cmp::Reverse(x.1));
            v
        };
        let (rp, fp) = (list(&c.pairs), list(&c.fpairs));
        let pn = |k: usize| format!("{}+{}", Counts::name(k >> 8), Counts::name(k & 255));
        println!("pairs, any flow: {}", rp.iter().take(top).map(|(k, n)| format!("{} {:.1}%", pn(*k), pct(*n, d))).collect::<Vec<_>>().join(", "));
        println!("pairs, fusable: {}", fp.iter().take(top).map(|(k, n)| format!("{} {:.1}%", pn(*k), pct(*n, d))).collect::<Vec<_>>().join(", "));
        let mut tr: Vec<(u32, u64)> = c.ftriples.iter().map(|(k, v)| (*k, *v)).collect();
        tr.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
        let tn = |k: u32| format!("{}+{}+{}", Counts::name((k >> 16) as usize), Counts::name(((k >> 8) & 255) as usize), Counts::name((k & 255) as usize));
        println!("triples, fusable: {}", tr.iter().take(top).map(|(k, n)| format!("{} {:.1}%", tn(*k), pct(*n, d))).collect::<Vec<_>>().join(", "));
        let fused: u64 = c.fpairs.iter().sum();
        println!("fusable pairs {:.1}% of dispatches (counted at each position, so overlapping pairs count twice)", pct(fused, d));
        let pset = |k: usize| fp.iter().take(k).map(|(p, _)| ((*p >> 8) as u8, *p as u8)).collect::<HashSet<_>>();
        let tset = |k: usize| tr.iter().take(k).map(|(p, _)| ((*p >> 16) as u8, (*p >> 8) as u8, *p as u8)).collect::<HashSet<_>>();
        if trace.len() as u64 == d {
            for (kp, kt) in [(4, 0), (8, 0), (16, 0), (32, 0), (0, 8), (8, 8), (16, 16), (32, 32)] {
                let r = removed(&trace, &pset(kp), &tset(kt));
                println!("fuse top {kp} pairs + top {kt} triples: removes {r} dispatches = {:.1}%", pct(r, d));
            }
        }
    }
}
