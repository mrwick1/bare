//! Load test for `Proxy`. One case at one scale per process, so the peak memory belongs to that run.
//!
//!   proxy_load CASE SCALE    CASE: get_trap | set_trap | noop | forward | array_mapfilter | reactive | chain | keys    SCALE: 1, 10, 100
//!
//! A case is a list of script stages. `collect` runs after every stage, and its time is added up. The last stage ends
//! with a number that is checked against the same computation done in Rust. A wrong checksum exits with status 1.
//! It prints run and collect time, process CPU and peak RSS (from /proc), and the interpreter steps.
//! `tools/js/proxy_load.sh` runs the matrix.

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

fn cpu_ms() -> f64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let f: Vec<&str> = s.rsplit_once(')').map_or("", |x| x.1).split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|n| n.parse::<f64>().ok()).unwrap_or(0.0);
    (t(11) + t(12)) * 10.0
}

/// (stages, expected checksum of the last stage).
fn program(case: &str, n: usize) -> (Vec<String>, f64) {
    let nf = n as f64;
    let tri = nf * (nf - 1.0) / 2.0;
    let s = |v: &[&str]| v.iter().map(|x| x.replace("{n}", &n.to_string())).collect::<Vec<_>>();
    match case {
        // a `get` trap that computes: one trap call per read
        "get_trap" => (s(&["var p = new Proxy({ v: 1 }, { get: function (t, k) { return t[k] + 1; } }); var out = 0;
            for (var i = 0; i < {n}; i++) out += p.v; out;"]), nf * 2.0),
        // a `set` trap that transforms: one trap call per write, one plain read of the target by the trap
        "set_trap" => (s(&["var t = { x: 0 }; var p = new Proxy(t, { set: function (t, k, v) { t[k] = v + 1; return true; } }); var out = 0;
            for (var i = 0; i < {n}; i++) { p.x = i; out += t.x; } out;"]), tri + nf),
        // no traps at all: every operation forwards to the target inside the engine
        "noop" => (s(&["var p = new Proxy({ x: 0 }, {}); var out = 0; for (var i = 0; i < {n}; i++) { p.x = i; out += p.x; } out;"]), tri),
        // the common logging pattern: traps forward with Reflect
        "forward" => (s(&["var p = new Proxy({ n: 0 }, { get: function (t, k, r) { return Reflect.get(t, k, r); }, set: function (t, k, v, r) { return Reflect.set(t, k, v, r); } });
            for (var i = 0; i < {n}; i++) p.n = p.n + 1; p.n;"]), nf),
        // a 1000-element array behind get and has traps: map and filter make 3000 trap calls per pass
        "array_mapfilter" => {
            let reps = (n / 100).max(1);
            (s(&["var calls = 0; var p = new Proxy((function () { var a = []; for (var i = 0; i < 1000; i++) a.push(i); return a; })(), { get: function (t, k, r) { calls++; return Reflect.get(t, k, r); }, has: function (t, k) { calls++; return Reflect.has(t, k); } }); var out = 0;",
                &format!("for (var r = 0; r < {reps}; r++) out += p.map(function (x) {{ return x + 1; }}).filter(function (x) {{ return x % 2 === 0; }}).length; out + (calls > 0 ? 0 : 1);")]), reps as f64 * 500.0)
        }
        // a Vue/MobX-style store: deep reactive get (tracks), set (triggers), an effect that reads two keys
        "reactive" => (s(&["var deps = {}; var active = null; function reactive(o) { return new Proxy(o, {
              get: function (t, k, r) { if (active) { var d = deps[k] || (deps[k] = []); if (d.indexOf(active) < 0) d.push(active); var v = Reflect.get(t, k, r); return v !== null && typeof v === 'object' ? reactive(v) : v; } return Reflect.get(t, k, r); },
              set: function (t, k, v, r) { var ok = Reflect.set(t, k, v, r); var d = deps[k]; if (d) for (var i = 0; i < d.length; i++) d[i](); return ok; } }); }
            function effect(f) { active = f; f(); active = null; }
            var total = 0; var state = reactive({ count: 0, items: { a: 1 } });
            effect(function () { total += state.count + state.items.a; });
            for (var i = 0; i < {n}; i++) state.count = i; total;"]), 1.0 + tri + nf),
        // 10 proxies in a row, no traps
        "chain" => (s(&["var p = { x: 0 }; for (var j = 0; j < 10; j++) p = new Proxy(p, {}); var out = 0; for (var i = 0; i < {n}; i++) { p.x = i; out += p.x; } out;"]), tri),
        // Object.keys through ownKeys and getOwnPropertyDescriptor traps: 1 + 50 trap calls per call
        "keys" => {
            let reps = (n / 10).max(1);
            (s(&["var t = {}; for (var i = 0; i < 50; i++) t['k' + i] = i; var p = new Proxy(t, { ownKeys: function (t) { return Reflect.ownKeys(t); }, getOwnPropertyDescriptor: function (t, k) { return Reflect.getOwnPropertyDescriptor(t, k); } }); var out = 0;",
                &format!("for (var r = 0; r < {reps}; r++) out += Object.keys(p).length; out;")]), reps as f64 * 50.0)
        }
        _ => die(format!("unknown case {case}")),
    }
}

fn run_case(case: String, scale: usize) {
    let n = 1000 * scale;
    let (stages, want) = program(&case, n);
    let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 2_000_000_000, ..Default::default() }).unwrap_or_else(|e| die(format!("setup {e:?}")));
    exec::set(&mut it);
    let (mut t_run, mut t_collect, mut got) = (0.0, 0.0, Value::Undefined);
    for src in &stages {
        let t = Instant::now();
        got = it.run_script(src).unwrap_or_else(|e| die(format!("run {e:?}")));
        t_run += t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        it.collect(&[got]).unwrap_or_else(|e| die(format!("collect {e:?}")));
        t_collect += t.elapsed().as_secs_f64() * 1e3;
    }
    if !matches!(got, Value::Number(x) if x == want) {
        die(format!("{case} x{scale}: checksum {} != {want}", it.display(got)));
    }
    println!(
        "{case:15} x{scale:<3} n={n:<7} run={t_run:>8.1}ms collect(all stages)={t_collect:>7.1}ms | cpu(whole process)={:>8.1}ms peakRSS={}MiB | steps={}",
        cpu_ms(),
        proc_kib("VmHWM:") / 1024,
        it.stats().steps,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: proxy_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    std::thread::Builder::new().stack_size(8 << 20).spawn(move || run_case(name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
