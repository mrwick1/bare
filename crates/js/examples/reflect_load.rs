//! Load test for `Object.getOwnPropertyNames`, `Reflect.ownKeys` and `Reflect.construct`. One case at one scale per
//! process, so the peak memory belongs to that run.
//!
//!   reflect_load CASE SCALE    CASE: construct_fn | construct_class | construct_args | keys | views | nested | cross_script    SCALE: 1, 10, 100
//!
//! A case is a list of script stages. `collect` runs after every stage, and its time is added up. The last stage ends
//! with a number that is checked against the same computation done in Rust. A wrong checksum exits with status 1.
//! It prints run and collect time, process CPU and peak RSS (from /proc), and the interpreter steps.
//! `tools/js/reflect_load.sh` runs the matrix.

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

/// (stages, expected checksum of the last stage).
fn program(case: &str, n: usize) -> (Vec<String>, f64) {
    let nf = n as f64;
    let tri = nf * (nf - 1.0) / 2.0;
    let s = |v: &[&str]| v.iter().map(|x| x.replace("{n}", &n.to_string())).collect::<Vec<_>>();
    match case {
        // new.target differs from the target on every other call; the checksum counts both the field and the prototype
        "construct_fn" => (s(&["function F(a) { this.a = a; this.nt = new.target === G ? 1 : 0; } function G() {} var out = 0;
            for (var i = 0; i < {n}; i++) { var o = i % 2 ? Reflect.construct(F, [i], G) : Reflect.construct(F, [i]); out += o.a + o.nt + (o instanceof G ? 1 : 0); } out;"]),
            { let (mut t, mut i) = (0.0, 0usize); while i < n { let odd = i % 2 == 1; t += i as f64 + if odd { 2.0 } else { 0.0 }; i += 1; } t }),
        "construct_class" => (s(&["class A { constructor(v) { this.v = v; this.k = new.target.name === 'C' ? 1 : 0; } } class B extends A { constructor(v) { super(v + 1); this.w = 2; } } class C {} var out = 0;
            for (var i = 0; i < {n}; i++) { var o = Reflect.construct(B, [i], C); out += o.v + o.k + o.w + (o instanceof C ? 1 : 0); } out;"]), tri + nf * 5.0),
        // 1000 elements per list: the list is read, charged and passed on as the arguments of each call
        "construct_args" => {
            let reps = (n / 1000).max(1);
            (s(&["var list = []; for (var i = 0; i < 1000; i++) list.push(i); function F() { var t = 0; for (var i = 0; i < arguments.length; i++) t += arguments[i]; this.t = t; } var out = 0;",
                &format!("for (var r = 0; r < {reps}; r++) out += Reflect.construct(F, list).t; out;")]), reps as f64 * 499500.0)
        }
        "keys" => {
            let reps = n / 10;
            (s(&["var o = {}; for (var i = 0; i < 100; i++) o['k' + i] = i; var sym = Symbol('s'); o[sym] = 1; Object.defineProperty(o, 'hidden', { value: 1 }); var out = 0;",
                &format!("for (var r = 0; r < {reps}; r++) out += Object.getOwnPropertyNames(o).length + Reflect.ownKeys(o).length; out;")]), reps as f64 * (101.0 + 102.0))
        }
        "views" => {
            let reps = n / 10;
            (s(&["var u = new Uint8Array(256); var s = 'abcdefghijklmnopqrstuvwxyz'; var out = 0;",
                &format!("for (var r = 0; r < {reps}; r++) out += Object.getOwnPropertyNames(u).length + Reflect.ownKeys(u).length + Object.getOwnPropertyNames(s).length; out;")]), reps as f64 * (256.0 + 256.0 + 27.0))
        }
        // 20 nested constructions (depth 40 with the natives, under the cap of 100), repeated
        "nested" => {
            let reps = (n / 20).max(1);
            (s(&["function N(d) { this.d = d; if (d > 0) this.c = Reflect.construct(N, [d - 1]); } var out = 0;",
                &format!("for (var r = 0; r < {reps}; r++) {{ var o = Reflect.construct(N, [19]); while (o) {{ out += o.d; o = o.c; }} }} out;")]), reps as f64 * 190.0)
        }
        // classes from one script, constructors from another, a collection between
        "cross_script" => (s(&["class A { constructor(v) { this.v = v; } } function G() {} var keep = [];",
            "class B extends A { constructor(v) { super(v); this.b = 1; } } function mk(v) { return Reflect.construct(B, [v], G); }",
            "for (var i = 0; i < {n}; i++) { if (i % 100 === 0) keep.push(mk(i)); else mk(i); } var out = 0; for (var i = 0; i < keep.length; i++) out += keep[i].v + keep[i].b + (keep[i] instanceof G ? 1 : 0); out;"]), {
            let k = n.div_ceil(100) as f64;
            (0..n).step_by(100).map(|i| i as f64 + 2.0).sum::<f64>() + 0.0 * k
        }),
        _ => die(format!("unknown case {case}")),
    }
}

fn run_case(case: String, scale: usize) {
    let n = 1000 * scale;
    let (stages, want) = program(&case, n);
    let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 2_000_000_000, ..Default::default() }).unwrap_or_else(|e| die(format!("setup {e:?}")));
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
        die("usage: reflect_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    std::thread::Builder::new().stack_size(8 << 20).spawn(move || run_case(name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
