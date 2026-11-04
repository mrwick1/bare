//! Load test for WeakMap and WeakSet. One case at one scale per process, so the peak memory belongs to that run.
//!
//!   weak_load CASE SCALE    CASE: map_obj | set_obj | churn | dead_cycles | chain_fwd | chain_rev | closures    SCALE: 1, 10, 100
//!
//! A case is a list of script stages. `collect` runs after every stage, and its time is added up. The last stage ends
//! with a number that is checked against the same computation done in Rust. A wrong checksum exits with status 1.
//! It prints run and collect time, process CPU and peak RSS (from /proc), the weak collections and entries tracked
//! before and after the last collection, and the collector's entry visits. `tools/js/weak_load.sh` runs the matrix.

use bare_js::eval::*;
use bare_js::heap::Heap;
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

/// (stages, expected checksum of the last stage, live weak entries expected after the last collection).
fn program(case: &str, n: usize) -> (Vec<String>, f64, usize) {
    let nf = n as f64;
    let tri = nf * (nf - 1.0) / 2.0;
    let s = |v: &[&str]| v.iter().map(|x| x.replace("{n}", &n.to_string())).collect::<Vec<_>>();
    match case {
        "map_obj" => (s(&["var w = new WeakMap(), ks = []; for (var i = 0; i < {n}; i++) { ks.push({ i: i }); w.set(ks[i], i); }", "var out = 0; for (var i = 0; i < {n}; i++) out += w.get(ks[i]); out;"]), tri, n),
        "set_obj" => (s(&["var w = new WeakSet(), ks = []; for (var i = 0; i < {n}; i++) { ks.push({}); w.add(ks[i]); } for (var i = 0; i < {n}; i += 2) w.delete(ks[i]);", "var out = 0; for (var i = 0; i < {n}; i++) if (w.has(ks[i])) out++; out;"]), (n / 2) as f64, n / 2),
        // keys die every round; the collector prunes them and the entry budget stays flat
        "churn" => (s(&["var w = new WeakMap(), out = 0;", "for (var r = 0; r < 10; r++) { for (var i = 0; i < {n}; i++) w.set({}, i); out += 1; }", "out;"]), 10.0, 0),
        "dead_cycles" => (s(&["var w = new WeakMap(); for (var i = 0; i < {n}; i++) { var k = {}; var v = { k: k }; k.v = v; w.set(k, v); } var k = null, v = null;", "var out = 1; out;"]), 1.0, 0),
        "chain_fwd" | "chain_rev" => {
            let idx = if case == "chain_fwd" { "i" } else { "{n} - 1 - i" };
            (s(&[
                "var ms = [], head = {}; for (var i = 0; i < {n}; i++) ms.push(new WeakMap());",
                &format!("var p = head; for (var i = 0; i < {{n}}; i++) {{ var nx = {{ i: i }}; ms[{idx}].set(p, nx); p = nx; }} p = nx = null;"),
                &format!("var out = 0, q = head; for (var i = 0; i < {{n}}; i++) {{ q = ms[{idx}].get(q); out += q.i; }} out;"),
            ]), tri, n)
        }
        // values are closures over a private environment; only the weak entry keeps it alive
        "closures" => (s(&["var w = new WeakMap(), ks = []; for (var i = 0; i < {n}; i++) { ks.push({}); (function (x) { var box = { x: x }; w.set(ks[x], function () { return box.x; }); })(i); }", "var out = 0; for (var i = 0; i < {n}; i++) out += w.get(ks[i])(); out;"]), tri, n),
        _ => die(format!("unknown case {case}")),
    }
}

fn run_case(case: String, scale: usize) {
    let n = 1000 * scale;
    let (stages, want, want_entries) = program(&case, n);
    let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 2_000_000_000, ..Default::default() }).unwrap_or_else(|e| die(format!("setup {e:?}")));
    let (mut t_run, mut t_collect, mut got, mut tracked, mut freed) = (0.0, 0.0, Value::Undefined, (0, 0), 0);
    let last = stages.len() - 1;
    for (i, src) in stages.iter().enumerate() {
        let t = Instant::now();
        got = it.run_script(src).unwrap_or_else(|e| die(format!("run {e:?}")));
        t_run += t.elapsed().as_secs_f64() * 1e3;
        if i == last {
            tracked = it.weak_tracked();
        }
        let t = Instant::now();
        freed = it.collect(&[got]).unwrap_or_else(|e| die(format!("collect {e:?}"))).objects;
        t_collect += t.elapsed().as_secs_f64() * 1e3;
    }
    if !matches!(got, Value::Number(x) if x == want) {
        die(format!("{case} x{scale}: checksum {} != {want}", it.display(got)));
    }
    let after = it.weak_tracked();
    if after.1 != want_entries {
        die(format!("{case} x{scale}: {} entries after collect, want {want_entries}", after.1));
    }
    println!(
        "{case:12} x{scale:<3} n={n:<7} run={t_run:>8.1}ms collect(all stages)={t_collect:>7.1}ms | cpu(whole process)={:>8.1}ms peakRSS={}MiB | weak before_last_collect={tracked:?} after={after:?} freed_objs_last={freed} visits={} steps={}",
        cpu_ms(),
        proc_kib("VmHWM:") / 1024,
        it.weak_visits(),
        it.stats().steps,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: weak_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    std::thread::Builder::new().stack_size(8 << 20).spawn(move || run_case(name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
