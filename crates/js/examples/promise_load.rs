//! Load test for the Promise subset and the microtask queue. One case at one scale per process, so the peak
//! memory belongs to that run.
//!
//!   promise_load CASE SCALE    CASE: chain | fanout | thenable | finally | reject | ctor | closures    SCALE: 1, 10, 100
//!
//! Each case builds a small program, runs it, drains the microtask queue and checks the result against the same
//! computation done in Rust (the checksum). A wrong checksum exits with status 1. It prints wall time per phase,
//! CPU time and peak resident memory of the process (from /proc), the managed-heap allocations, steps, the
//! promises tracked, and what an explicit `collect` frees. `tools/js/promise_load.sh` runs the matrix with caps.

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

fn cpu_ms() -> f64 {
    // utime + stime of the whole process, in clock ticks of 10 ms (Linux, USER_HZ 100)
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let f: Vec<&str> = s.rsplit_once(')').map_or("", |x| x.1).split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|n| n.parse::<f64>().ok()).unwrap_or(0.0);
    (t(11) + t(12)) * 10.0
}

/// (source, expected checksum). The last expression is a reader: it returns the sum as a number.
fn program(case: &str, n: usize) -> (String, f64) {
    let reader = "(function () { return out[0]; })";
    let nf = n as f64;
    match case {
        "chain" => (format!("var out = [0]; var p = Promise.resolve(0); for (var i = 0; i < {n}; i++) p = p.then(function (v) {{ return v + 1; }}); p.then(function (v) {{ out[0] = v; }});{reader}"), nf),
        "fanout" => (format!("var out = [0]; var res; var p = new Promise(function (r) {{ res = r; }}); for (var i = 0; i < {n}; i++) p.then(function (v) {{ out[0] += v; }}); res(2);{reader}"), 2.0 * nf),
        "thenable" => (format!("var out = [0]; for (var i = 0; i < {n}; i++) Promise.resolve({{ then: function (a) {{ a(1); }} }}).then(function (v) {{ out[0] += v; }});{reader}"), nf),
        "finally" => (format!("var out = [0]; var p = Promise.resolve(1); for (var i = 0; i < {n}; i++) p = p.finally(function () {{ out[0]++; }}); p.then(function (v) {{ out[0] += v; }});{reader}"), nf + 1.0),
        "reject" => (format!("var out = [0]; for (var i = 0; i < {n}; i++) Promise.reject(1).then(function () {{ out[0] += 1000; }}).catch(function (e) {{ out[0] += e; }});{reader}"), nf),
        "ctor" => (format!("var out = [0]; for (var i = 0; i < {n}; i++) new Promise(function (r, j) {{ if (i % 2) r(i); else j(i); }}).then(function (v) {{ out[0] += v; }}, function (e) {{ out[0] -= e; }});{reader}"), (0..n).map(|i| if i % 2 == 1 { i as f64 } else { -(i as f64) }).sum()),
        "closures" => (format!("var out = [0]; for (var i = 0; i < {n}; i++) (function (k) {{ var big = {{ k: k }}; Promise.resolve().then(function () {{ out[0] += big.k; }}); }})(i);{reader}"), (0..n).map(|i| i as f64).sum()),
        _ => die(format!("unknown case {case}")),
    }
}

fn run_case(case: String, scale: usize) {
    let n = 1000 * scale;
    let (src, want) = program(&case, n);
    let t0 = Instant::now();
    let prog = parse_script(&src, Limits::default()).unwrap_or_else(|e| die(format!("parse {e}")));
    let t_parse = t0.elapsed();
    let t1 = Instant::now();
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 2_000_000_000, ..Default::default() }).unwrap_or_else(|e| die(format!("setup {e:?}")));
    exec::set(&mut it);
    let t_setup = t1.elapsed();
    let t2 = Instant::now();
    let reader = it.run().unwrap_or_else(|e| die(format!("run {e:?}")));
    let t_run = t2.elapsed();
    let (queued, tracked) = (it.pending_jobs(), it.stats());
    let t3 = Instant::now();
    let jobs = it.drain_microtasks().unwrap_or_else(|e| die(format!("drain {e:?}")));
    let t_drain = t3.elapsed();
    let got = match it.call(reader, Value::Undefined, &[]) {
        Ok(Value::Number(x)) => x,
        other => die(format!("reader {other:?}")),
    };
    if got != want {
        die(format!("{case} x{scale}: checksum {got} != {want}"));
    }
    let before = it.heap.stats();
    let t4 = Instant::now();
    let freed = it.collect(&[reader]).unwrap_or_else(|e| die(format!("collect {e:?}")));
    let t_collect = t4.elapsed();
    let after = it.heap.stats();
    let cpu = cpu_ms();
    println!(
        "{case:9} x{scale:<3} n={n:<7} jobs={jobs:<7} queued_before_drain={queued:<7} run={:>7.1}ms drain={:>7.1}ms collect={:>6.1}ms | cpu(whole process)={cpu:>7.1}ms peakRSS={}MiB | heap objs alloc={} strs alloc={} live_before={} live_after={} freed={} | steps={} functions_before={} functions_after={} setup={:.1}ms parse={:.1}ms",
        t_run.as_secs_f64() * 1e3,
        t_drain.as_secs_f64() * 1e3,
        t_collect.as_secs_f64() * 1e3,
        proc_kib("VmHWM:") / 1024,
        before.total_object_allocs,
        before.total_string_allocs,
        before.live_objects,
        after.live_objects,
        freed.objects,
        it.stats().steps,
        tracked.functions,
        it.stats().functions,
        t_setup.as_secs_f64() * 1e3,
        t_parse.as_secs_f64() * 1e3,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: promise_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    std::thread::Builder::new().stack_size(16 << 20).spawn(move || run_case(name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
