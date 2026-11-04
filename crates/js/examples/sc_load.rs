//! Load test for Symbol, iterators, Map and Set. One case at one scale per process, so the peak memory belongs to that run.
//!
//!   sc_load CASE SCALE    CASE: map_num | map_str | map_obj | set_dedupe | churn | forof | next_loop | custom_iter | symbol_for | symbol_keys | toprim | foreach_grow    SCALE: 1, 10, 100
//!
//! Each case builds a small program, runs it and checks the result against the same computation done in Rust (the
//! checksum). A wrong checksum exits with status 1. It prints wall time per phase, CPU time and peak resident memory of
//! the process (from /proc), the managed-heap allocations, steps, the Maps/Sets/iterators tracked, and what an explicit
//! `collect` frees. `tools/js/sc_load.sh` runs the matrix with caps.

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
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let f: Vec<&str> = s.rsplit_once(')').map_or("", |x| x.1).split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|n| n.parse::<f64>().ok()).unwrap_or(0.0);
    (t(11) + t(12)) * 10.0
}

/// (source, expected checksum). The last expression is a reader: it returns the checksum as a number.
fn program(case: &str, n: usize) -> (String, f64) {
    let reader = "(function () { return out; })";
    let nf = n as f64;
    let tri = nf * (nf - 1.0) / 2.0;
    match case {
        "map_num" => (format!("var out = 0; var m = new Map(); for (var i = 0; i < {n}; i++) m.set(i, i * 2); for (var i = 0; i < {n}; i++) out += m.get(i); out += m.size;{reader}"), 2.0 * tri + nf),
        "map_str" => (format!("var out = 0; var m = new Map(); for (var i = 0; i < {n}; i++) m.set('key' + i, i); for (var i = 0; i < {n}; i++) out += m.get('key' + i); for (var i = 0; i < {n}; i++) if (m.has('nokey' + i)) out = -1;{reader}"), tri),
        "map_obj" => (format!("var out = 0; var ks = []; var m = new Map(); for (var i = 0; i < {n}; i++) {{ ks.push({{ i: i }}); m.set(ks[i], i); }} for (var i = 0; i < {n}; i++) out += m.get(ks[i]);{reader}"), tri),
        "set_dedupe" => (format!("var out = 0; var s = new Set(); for (var i = 0; i < 2 * {n}; i++) s.add(i % {n}); out = s.size; for (var i = 0; i < {n}; i += 2) s.delete(i); out += s.size;{reader}"), nf + (nf / 2.0).floor()),
        "churn" => (format!("var out = 0; var m = new Map(); for (var r = 0; r < 10; r++) {{ for (var i = 0; i < {n}; i++) m.set(i, r); for (var i = 0; i < {n}; i++) if (i % 10 !== 0) m.delete(i); }} for (var i = 0; i < {n}; i++) out += m.has(i) ? 1 : 0; out += m.size;{reader}"), nf / 10.0 * 2.0),
        "forof" => (format!("var out = 0; var m = new Map(); for (var i = 0; i < {n}; i++) m.set(i, i); for (var [k, v] of m) out += v; for (var v of m.values()) out += v; var s = new Set(m.keys()); for (var x of s) out += x; var a = [...m]; out += a.length;{reader}"), 3.0 * tri + nf),
        "next_loop" => (format!("var out = 0; var m = new Map(); for (var i = 0; i < {n}; i++) m.set(i, i); var it = m.entries(); for (var r = it.next(); !r.done; r = it.next()) out += r.value[1]; var ai = [1, 2, 3][Symbol.iterator](); for (var r = ai.next(); !r.done; r = ai.next()) out += r.value;{reader}"), tri + 6.0),
        "custom_iter" => (format!("var out = 0; var it = {{ [Symbol.iterator]() {{ var i = 0; return {{ next() {{ return i < {n} ? {{ value: i++, done: false }} : {{ value: undefined, done: true }}; }} }}; }} }}; for (var x of it) out += x; var [a, b, ...rest] = it; out += rest.length;{reader}"), tri + nf - 2.0),
        "symbol_for" => (format!("var out = 0; var o = {{}}; for (var i = 0; i < {n}; i++) {{ var s = Symbol.for('k' + (i % 5000)); o[s] = (o[s] || 0) + 1; }} for (var i = 0; i < 5000; i++) out += o[Symbol.for('k' + i)] || 0; out += Symbol.keyFor(Symbol.for('k7')) === 'k7' ? 1 : 0;{reader}"), nf + 1.0),
        "symbol_keys" => (format!("var out = 0; var o = {{}}; var ks = []; for (var i = 0; i < {n}; i++) {{ var s = Symbol('s' + i); ks.push(s); o[s] = i; }} for (var i = 0; i < {n}; i++) out += o[ks[i]];{reader}"), tri),
        "toprim" => (format!("var out = 0; var o = {{ [Symbol.toPrimitive](h) {{ return h === 'number' ? 2 : 1; }} }}; for (var i = 0; i < {n}; i++) out += +o; for (var i = 0; i < {n}; i++) out += o * 1;{reader}"), 4.0 * nf),
        "foreach_grow" => (format!("var out = 0; var m = new Map([[0, 0]]); m.forEach(function (v, k) {{ out += 1; if (k + 1 < {n}) m.set(k + 1, 0); if (k > 2) m.delete(k - 2); }});{reader}"), nf),
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
    let t_setup = t1.elapsed();
    let t2 = Instant::now();
    let reader = it.run().unwrap_or_else(|e| die(format!("run {e:?}")));
    let t_run = t2.elapsed();
    let got = match it.call(reader, Value::Undefined, &[]) {
        Ok(Value::Number(x)) => x,
        other => die(format!("reader {other:?}")),
    };
    if got != want {
        die(format!("{case} x{scale}: checksum {got} != {want}"));
    }
    let before = it.heap.stats();
    let (tc, ti) = it.sc_tracked();
    let slots = it.sc_entry_slots();
    let t4 = Instant::now();
    let freed = it.collect(&[reader]).unwrap_or_else(|e| die(format!("collect {e:?}")));
    let t_collect = t4.elapsed();
    let after = it.heap.stats();
    let cpu = cpu_ms();
    println!(
        "{case:12} x{scale:<3} n={n:<7} run={:>8.1}ms collect={:>6.1}ms | cpu(whole process)={cpu:>8.1}ms peakRSS={}MiB | heap objs alloc={} strs alloc={} syms alloc={} live_before={} live_after={} freed={} | steps={} colls={tc} iters={ti} entry_slots={slots} colls_after={} | setup={:.1}ms parse={:.1}ms",
        t_run.as_secs_f64() * 1e3,
        t_collect.as_secs_f64() * 1e3,
        proc_kib("VmHWM:") / 1024,
        before.total_object_allocs,
        before.total_string_allocs,
        before.total_symbol_allocs,
        before.live_objects,
        after.live_objects,
        freed.objects,
        it.stats().steps,
        it.sc_tracked().0,
        t_setup.as_secs_f64() * 1e3,
        t_parse.as_secs_f64() * 1e3,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: sc_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    std::thread::Builder::new().stack_size(16 << 20).spawn(move || run_case(name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
