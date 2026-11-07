//! Load test for `Promise.all`, `allSettled`, `race` and `any`. One case at one scale per process, so the peak
//! memory belongs to that run.
//!
//!   promise_all_load CASE SCALE    CASE: all | settled | race | any | anyerr | many | thenable    SCALE: 1, 10, 100
//!
//! One scale step is 1000 items. Each case builds a small program, runs it, drains the microtask queue and checks the
//! result against the same computation done in Rust (the checksum). A wrong checksum exits with status 1. It prints
//! wall time per phase, CPU time and peak resident memory of the process (from /proc), heap allocations, steps, the
//! records and slots of the combinators, and what an explicit `collect` frees.

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
    let arr = format!("var a = []; for (var i = 0; i < {n}; i++) a.push(i % 3 == 0 ? i : i % 3 == 1 ? Promise.resolve(i) : mk(i)); function mk(k) {{ return {{ then: function (ok) {{ ok(k); }} }}; }}");
    let tri = nf * (nf - 1.0) / 2.0;
    match case {
        "all" => (format!("var out = [0]; {arr} Promise.all(a).then(function (v) {{ for (var k = 0; k < v.length; k++) out[0] += v[k]; }});{reader}"), tri),
        "settled" => (format!("var out = [0]; {arr} Promise.allSettled(a).then(function (v) {{ for (var k = 0; k < v.length; k++) out[0] += v[k].value + (v[k].status == 'fulfilled' ? 0 : 1e9); }});{reader}"), tri),
        "race" => (format!("var out = [0]; {arr} Promise.race(a).then(function (v) {{ out[0] = v + 1; }});{reader}"), 1.0),
        "any" => (format!("var out = [0]; {arr} Promise.any(a).then(function (v) {{ out[0] = v + 1; }});{reader}"), 1.0),
        "anyerr" => (format!("var out = [0]; var a = []; for (var i = 0; i < {n}; i++) a.push(i % 2 ? Promise.reject(i) : {{ then: function (ok, no) {{ no(0); no(1); }} }}); Promise.any(a).catch(function (e) {{ out[0] = e.errors.length + (e instanceof AggregateError ? 1 : 0); }});{reader}"), nf + 1.0),
        "many" => (format!("var out = [0]; for (var j = 0; j < {n} / 10; j++) Promise.all([j, Promise.resolve(1), {{ then: function (ok) {{ ok(2); }} }}, Promise.reject(7).catch(function () {{ return 3; }}), 4, 5, 6, 7, 8, 9]).then(function (v) {{ out[0] += v[0] + v[9]; }});{reader}"), (0..n / 10).map(|j| (j + 9) as f64).sum()),
        "thenable" => (format!("var out = [0]; var a = []; for (var i = 0; i < {n}; i++) a.push({{ then: function (ok, no) {{ ok(1); ok(2); no(3); }} }}); Promise.all(a).then(function (v) {{ out[0] = v.length; }});{reader}"), nf),
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
    let agg_before = it.agg_stats();
    let tg = Instant::now();
    it.collect(&[reader]).unwrap_or_else(|e| die(format!("suspended collect {e:?}")));
    let t_suspended = tg.elapsed();
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
    if it.agg_stats() != AggStats::default() { die(format!("aggregate metadata retained: {:?}", it.agg_stats())); }
    let cpu = cpu_ms();
    println!(
        "{case:9} x{scale:<3} n={n:<7} jobs={jobs:<7} queued_before_drain={queued:<7} run={:>7.1}ms suspended_gc={:>6.1}ms drain={:>7.1}ms collect={:>6.1}ms | cpu(whole process)={cpu:>7.1}ms peakRSS={}MiB | heap objs alloc={} strs alloc={} live_before={} live_after={} freed={} | agg_records={} agg_items={} | steps={} functions_before={} functions_after={} setup={:.1}ms parse={:.1}ms",
        t_run.as_secs_f64() * 1e3,
        t_suspended.as_secs_f64() * 1e3,
        t_drain.as_secs_f64() * 1e3,
        t_collect.as_secs_f64() * 1e3,
        proc_kib("VmHWM:") / 1024,
        before.total_object_allocs,
        before.total_string_allocs,
        before.live_objects,
        after.live_objects,
        freed.objects,
        agg_before.records,
        agg_before.items,
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
        die("usage: promise_all_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    std::thread::Builder::new().stack_size(8 << 20).spawn(move || run_case(name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
