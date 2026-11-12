//! Load test for generators. One case at one scale per process, so the peak memory belongs to that run.
//!
//!   gen_load CASE SCALE    CASE: next | forof | iterobj | spread | spawn | suspended | delegate | finally    SCALE: 1, 10, 100
//!
//! Each case runs a small program and checks its result against the same computation done in Rust (the checksum).
//! `iterobj` is the baseline: the `forof` loop over a hand-written iterator object. A `collect` runs while the
//! `suspended` generators are still alive. It prints wall time, peak resident memory (from /proc), heap
//! allocations, steps and the generator table. `tools/js/gen_load.sh` runs the matrix with caps.

use bare_js::eval::*;
use bare_js::heap::Heap;
use bare_js::parse::{parse_script, Limits};
use bare_js::value::Value;
use std::time::Instant;
#[path = "common/exec.rs"]
mod exec;

fn die(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1)
}

fn proc_kib(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(key)).and_then(|r| r.split_whitespace().next()).and_then(|n| n.parse().ok()).unwrap_or(0)
}

/// (source, expected checksum). The program's value is the checksum. `suspended` keeps n/20 generators, 5 frames each, so
/// the default cap of 2^20 frames holds at 100x.
fn program(case: &str, n: usize) -> (String, f64) {
    let tri = |k: usize| (k * (k.max(1) - 1) / 2) as f64;
    let range = "function* range(n) { for (var i = 0; i < n; i++) yield i; }";
    match case {
        "next" => (format!("{range} var it = range({n}), s = 0, r; while (!(r = it.next()).done) s += r.value; s"), tri(n)),
        "forof" => (format!("{range} var s = 0; for (var x of range({n})) s += x; s"), tri(n)),
        "iterobj" => (format!("function range(n) {{ var i = 0; return {{ [Symbol.iterator]() {{ return this; }}, next() {{ return i < n ? {{ value: i++, done: false }} : {{ value: undefined, done: true }}; }} }}; }} var s = 0; for (var x of range({n})) s += x; s"), tri(n)),
        "spread" => (format!("{range} var s = 0; for (var k = 0; k < {}; k++) {{ var a = [...range(100)]; s += a[99]; }} s", n / 100), (n / 100) as f64 * 99.0),
        "spawn" => (format!("function* two(k) {{ yield k; yield 1; }} var s = 0; for (var i = 0; i < {n}; i++) {{ for (var x of two(i)) s += x; }} s"), tri(n) + n as f64),
        "suspended" => (format!("function* g(k) {{ var o = {{ k: k }}; for (const x of [1, 2]) {{ try {{ yield o.k + x; }} finally {{ o.k = 0; }} }} }} var keep = []; for (var i = 0; i < {}; i++) {{ var it = g(i); it.next(); keep.push(it); }} var s = 0; for (var i = 0; i < keep.length; i++) s += keep[i].next().value; s", n / 20), 2.0 * (n / 20) as f64),
        "delegate" => (format!("{range} function* wrap(d, n) {{ if (d === 0) yield* range(n); else yield* wrap(d - 1, n); }} var s = 0; for (var x of wrap(5, {})) s += x; s", n / 5), tri(n / 5)),
        "finally" => (format!("var s = 0; function* g(k) {{ try {{ yield k; yield 0; }} finally {{ s += 1; }} }} for (var i = 0; i < {n}; i++) {{ for (var x of g(i)) {{ s += x; break; }} }} s"), tri(n) + n as f64),
        _ => die(format!("unknown case {case}")),
    }
}

fn run_case(case: String, scale: usize) {
    let n = 10_000 * scale;
    let (src, want) = program(&case, n);
    let prog = parse_script(&src, Limits::default()).unwrap_or_else(|e| die(format!("parse {e}")));
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 2_000_000_000, ..Default::default() }).unwrap_or_else(|e| die(format!("setup {e:?}")));
    exec::set(&mut it);
    let t = Instant::now();
    let got = match it.run() {
        Ok(Value::Number(x)) => x,
        other => die(format!("run {other:?}")),
    };
    let t_run = t.elapsed();
    if got != want {
        die(format!("{case} x{scale}: checksum {got} != {want}"));
    }
    let (st, gs) = (it.heap.stats(), it.async_stats());
    let freed = it.collect(&[]).unwrap_or_else(|e| die(format!("collect {e:?}")));
    let after = it.async_stats();
    println!(
        "{case:9} x{scale:<3} n={n:<8} run={:>8.1}ms peak_rss={:.1} MiB | heap objs alloc={} live={} freed={} | generators={} frames={} slots={} after_gc={} | steps={}",
        t_run.as_secs_f64() * 1e3,
        proc_kib("VmHWM:") as f64 / 1024.0,
        st.total_object_allocs,
        st.live_objects,
        freed.objects,
        gs.generators,
        gs.frames,
        gs.slots,
        after.generators,
        it.stats().steps,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: gen_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    std::thread::Builder::new().stack_size(16 << 20).spawn(move || run_case(name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
