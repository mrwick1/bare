//! Load test for the iterator helpers, the Set methods and `Promise.withResolvers`. One case at one scale per process,
//! so the peak memory belongs to that run.
//!
//!   iterator_load CASE SCALE    SCALE: 1, 10, 100 (n = 10,000 x SCALE)
//!   CASE: chain | chain_forof | flatmap | generic | array_chain | set_union | set_inter | set_diff | set_sym | set_like |
//!         set_union_like | resolvers | promise_new
//!
//! Each case runs a setup script and then times one operation script. The operation's value is a checksum, checked
//! against the same computation done in Rust. `array_chain` is the baseline of `chain` (the Array methods), `generic` runs
//! the helpers over a hand-written iterator object (no fast path), `promise_new` is the baseline of `resolvers`.
//! It prints the setup and operation time, peak resident memory (from /proc), objects allocated by the operation and
//! the steps it took. `tools/js/iterator_load.sh` runs the matrix with caps.

use bare_js::eval::*;
use bare_js::heap::Heap;
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

/// (setup, operation, expected checksum). A and B are Sets of n/2 numbers that share n/4 of them.
fn program(case: &str, n: usize) -> (String, String, f64) {
    let (h, q) = (n / 2, n / 4);
    let nf = n as f64;
    let arr = format!("var a = []; for (var i = 0; i < {n}; i++) a.push(i);");
    let sets = format!("var A = new Set(), B = new Set(); for (var i = 0; i < {h}; i++) {{ A.add(i); B.add(i + {q}); }}");
    let like = "var like = { size: B.size, has: function (x) { return B.has(x); }, keys: function () { return B.values(); } };";
    let even_sum = (q * (q + 1)) as f64; // the first q even numbers of 1..=n
    match case {
        "chain" => (arr, format!("a.values().map(function (x) {{ return x + 1; }}).filter(function (x) {{ return x % 2 === 0; }}).take({q}).reduce(function (s, x) {{ return s + x; }}, 0)"), even_sum),
        "chain_forof" => (arr, format!("var s = 0; for (var x of a.values().map(function (x) {{ return x + 1; }}).filter(function (x) {{ return x % 2 === 0; }}).take({q})) s += x; s"), even_sum),
        "flatmap" => (arr, "a.values().flatMap(function (x) { return [x, x]; }).drop(10).reduce(function (s, x) { return s + x; }, 0)".into(), nf * (nf - 1.0) - 20.0),
        "generic" => (
            format!("var src = {{ i: 0, next: function () {{ return this.i < {n} ? {{ value: this.i++, done: false }} : {{ value: undefined, done: true }}; }} }};"),
            "Iterator.prototype.map.call(src, function (x) { return x + 1; }).reduce(function (s, x) { return s + x; }, 0)".into(),
            nf * (nf + 1.0) / 2.0,
        ),
        "array_chain" => (arr, format!("a.map(function (x) {{ return x + 1; }}).filter(function (x) {{ return x % 2 === 0; }}).slice(0, {q}).reduce(function (s, x) {{ return s + x; }}, 0)"), even_sum),
        "set_union" => (sets, "A.union(B).size".into(), (h + q) as f64),
        "set_inter" => (sets, "A.intersection(B).size".into(), q as f64),
        "set_diff" => (sets, "A.difference(B).size".into(), q as f64),
        "set_sym" => (sets, "A.symmetricDifference(B).size".into(), (2 * q) as f64),
        "set_like" => (format!("{sets} {like}"), "A.intersection(like).size".into(), q as f64),
        "set_union_like" => (format!("{sets} {like}"), "A.union(like).size".into(), (h + q) as f64),
        "resolvers" => ("var s = 0;".into(), format!("for (var i = 0; i < {q}; i++) {{ var d = Promise.withResolvers(); d.promise.then(function (v) {{ s += v; }}); d.resolve(1); }}"), q as f64),
        "promise_new" => ("var s = 0;".into(), format!("for (var i = 0; i < {q}; i++) {{ var res; var p = new Promise(function (r) {{ res = r; }}); p.then(function (v) {{ s += v; }}); res(1); }}"), q as f64),
        _ => die(format!("unknown case {case}")),
    }
}

fn run_case(case: String, scale: usize) {
    let n = 10_000 * scale;
    let (setup, op, want) = program(&case, n);
    let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 2_000_000_000, ..Default::default() }).unwrap_or_else(|e| die(format!("setup {e:?}")));
    exec::set(&mut it);
    let t = Instant::now();
    if let Err(e) = it.run_script(&setup) {
        die(format!("setup script {e:?}"));
    }
    let t_setup = t.elapsed();
    let (a0, s0) = (it.heap.stats().total_object_allocs, it.stats().steps);
    let t = Instant::now();
    let r = it.run_script(&op).unwrap_or_else(|e| die(format!("op {e:?}")));
    let jobs = if case == "resolvers" || case == "promise_new" { it.drain_microtasks().unwrap_or_else(|e| die(format!("drain {e:?}"))) } else { 0 };
    let t_op = t.elapsed();
    let got = match (case.as_str(), r) {
        ("resolvers" | "promise_new", _) => match it.run_script("s") {
            Ok(Value::Number(x)) => x,
            other => die(format!("read s {other:?}")),
        },
        (_, Value::Number(x)) => x,
        (_, other) => die(format!("op value {other:?}")),
    };
    if got != want {
        die(format!("{case} x{scale}: checksum {got} != {want}"));
    }
    let (a1, s1) = (it.heap.stats().total_object_allocs, it.stats().steps);
    println!(
        "{case:14} x{scale:<3} n={n:<8} setup={:>8.1}ms op={:>8.1}ms jobs={jobs:<7} peak_rss={:.1} MiB | objs alloc in op={} | steps in op={}",
        t_setup.as_secs_f64() * 1e3,
        t_op.as_secs_f64() * 1e3,
        proc_kib("VmHWM:") as f64 / 1024.0,
        a1 - a0,
        s1 - s0,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: iterator_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    std::thread::Builder::new().stack_size(16 << 20).spawn(move || run_case(name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
