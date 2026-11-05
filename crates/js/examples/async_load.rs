//! Load test for async functions and `await`. One case at one scale per process, so the peak
//! memory belongs to that run.
//!
//!   async_load CASE SCALE    CASE: seq | fanout | nested | trycatch | pending | closures | switch | ifchain    SCALE: 1, 10, 100
//!
//! Each case builds a small program, runs it, drains the microtask queue and checks the result against the same
//! computation done in Rust (the checksum). A `collect` runs while every activation is still suspended. A wrong checksum exits with status 1. It prints wall time per phase,
//! CPU time and peak resident memory of the process (from /proc), the managed-heap allocations, steps, the
//! promises tracked, and what an explicit `collect` frees. `tools/js/async_load.sh` runs the matrix with caps.

use bare_js::eval::*;
use bare_js::heap::Heap;
use bare_js::parse::{parse_script, Limits};
use bare_js::value::Value;
use std::time::Instant;

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
/// Cases that keep one activation per item use n/2 items, so the default cap of 65536 activations holds at 100x.
fn program(case: &str, n: usize) -> (String, f64) {
    let reader = "(function () { return out[0]; })";
    let m = n / 2;
    let mf = m as f64;
    let tri = |k: usize| (k * (k - 1) / 2) as f64;
    match case {
        "seq" => (format!("var out = [0]; async function f() {{ for (var i = 0; i < {n}; i++) out[0] += await i; }} f();{reader}"), tri(n)),
        "fanout" => (format!("var out = [0]; async function f(k) {{ var a = await k; var b = await k; out[0] += a + b; }} for (var i = 0; i < {m}; i++) f(i);{reader}"), 2.0 * tri(m)),
        "nested" => (format!("var out = [0]; async function d(k) {{ if (k == 0) return 1; return 1 + await d(k - 1); }} async function r() {{ for (var i = 0; i < {}; i++) {{ var v = await d(40); out[0] += v; }} }} r();{reader}", n / 40), (n / 40) as f64 * 41.0),
        "trycatch" => (format!("var out = [0]; async function f(k) {{ try {{ await Promise.reject(k); }} catch (e) {{ out[0] += e; }} finally {{ await 0; }} }} for (var i = 0; i < {m}; i++) f(i);{reader}"), tri(m)),
        "pending" => (format!("var out = [0]; var res; var p = new Promise(function (r) {{ res = r; }}); async function f() {{ var v = await p; out[0] += v; }} for (var i = 0; i < {m}; i++) f(); res(2);{reader}"), 2.0 * mf),
        "closures" => (format!("var out = [0]; async function f(k) {{ var big = {{ k: k }}; await 0; out[0] += big.k; }} for (var i = 0; i < {m}; i++) f(i);{reader}"), tri(m)),
        "switch" => (format!("var out = [0]; async function f(k) {{ switch (await k % 4) {{ case 0: let a = k; await 0; out[0] += a; case 1: out[0] += 1; break; case await 2: out[0] += 2; break; default: out[0] += 3; }} }} for (var i = 0; i < {m}; i++) f(i);{reader}"), sw_sum(m)),
        "ifchain" => (format!("var out = [0]; async function f(k) {{ var r = await k % 4; if (r === 0) {{ let a = k; await 0; out[0] += a; out[0] += 1; }} else if (r === 1) out[0] += 1; else if (r === await 2) out[0] += 2; else out[0] += 3; }} for (var i = 0; i < {m}; i++) f(i);{reader}"), sw_sum(m)),
        _ => die(format!("unknown case {case}")),
    }
}

/// The sum the `switch` and `ifchain` cases compute.
fn sw_sum(m: usize) -> f64 {
    (0..m).map(|k| [k + 1, 1, 2, 3][k % 4] as f64).sum()
}

fn run_case(case: String, scale: usize) {
    let n = 1000 * scale;
    let (src, want) = program(&case, n);
    let t0 = Instant::now();
    let prog = parse_script(&src, Limits::default()).unwrap_or_else(|e| die(format!("parse {e}")));
    let t_parse = t0.elapsed();
    let t1 = Instant::now();
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 2_000_000_000, ..Default::default() }).unwrap_or_else(|e| die(format!("setup {e:?}")));
    let t_setup = t1.elapsed();
    let t2 = Instant::now();
    let reader = it.run().unwrap_or_else(|e| die(format!("run {e:?}")));
    let t_run = t2.elapsed();
    let (queued, tracked) = (it.pending_jobs(), it.stats());
    let (act, st) = (it.async_stats(), it.heap.stats());
    let t_gc = Instant::now();
    let mid = it.collect(&[reader]).unwrap_or_else(|e| die(format!("mid collect {e:?}")));
    let t_gc = t_gc.elapsed();
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
        "{case:9} x{scale:<3} n={n:<7} jobs={jobs:<7} queued_before_drain={queued:<7} run={:>7.1}ms drain={:>7.1}ms collect={:>6.1}ms | cpu(whole process)={cpu:>7.1}ms peakRSS={}MiB | heap objs alloc={} strs alloc={} live_before={} live_after={} freed={} | suspended={} frames={} slots={} mid_gc={:.1}ms mid_freed={} (live {}) | steps={} functions_before={} functions_after={} setup={:.1}ms parse={:.1}ms",
        t_run.as_secs_f64() * 1e3,
        t_drain.as_secs_f64() * 1e3,
        t_collect.as_secs_f64() * 1e3,
        proc_kib("VmHWM:") / 1024,
        before.total_object_allocs,
        before.total_string_allocs,
        before.live_objects,
        after.live_objects,
        freed.objects,
        act.activations,
        act.frames,
        act.slots,
        t_gc.as_secs_f64() * 1e3,
        mid.objects,
        st.live_objects,
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
        die("usage: async_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    std::thread::Builder::new().stack_size(16 << 20).spawn(move || run_case(name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
