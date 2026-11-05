//! Load test for tagged templates. One case at one scale per process, so the peak memory belongs to that run.
//!
//!   tagged_load CASE SCALE    CASE: call | plain | sites | css    SCALE: 1, 10, 100
//!
//! Each case runs a small program and checks its result against the same computation done in Rust (the checksum).
//! `plain` is the baseline: the `call` loop with an ordinary call and a strings array made once. `sites` makes n/10
//! distinct sites, each called once, so its memory is the cost per site. It prints wall time, peak resident memory
//! (from /proc), heap objects and steps. `tools/js/tagged_load.sh` runs the matrix with caps.

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

/// (source, expected checksum). The program's value is the checksum.
fn program(case: &str, n: usize) -> (String, f64) {
    let tri = |k: usize| (k * (k.max(1) - 1) / 2) as f64;
    match case {
        "call" => (format!("function t(s, a) {{ return s.length + a; }} var x = 0; for (var i = 0; i < {n}; i++) x += t`a${{i}}b`; x"), tri(n) + 2.0 * n as f64),
        "plain" => (format!("function t(s, a) {{ return s.length + a; }} var S = ['a', 'b']; var x = 0; for (var i = 0; i < {n}; i++) x += t(S, i); x"), tri(n) + 2.0 * n as f64),
        "sites" => {
            let k = n / 10;
            let mut s = String::from("function t(s, a) { return s.length + a; } var x = 0;");
            for i in 0..k {
                s.push_str(&format!(" x += t`p{i}${{{i}}}q`;"));
            }
            s.push_str(" x");
            (s, tri(k) + 2.0 * k as f64)
        }
        "css" => (format!("function css(s, ...v) {{ var o = s[0]; for (var j = 0; j < v.length; j++) o += v[j] + s[j + 1]; return o; }} var x = 0; for (var i = 0; i < {n}; i++) x += css`color: ${{i % 10}}; margin: ${{4}}px;`.length; x"), n as f64 * 22.0),
        _ => die(format!("unknown case {case}")),
    }
}

fn run_case(case: String, scale: usize) {
    let n = 10_000 * scale;
    let (src, want) = program(&case, n);
    let prog = parse_script(&src, Limits::default()).unwrap_or_else(|e| die(format!("parse {e}")));
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 2_000_000_000, ..Default::default() }).unwrap_or_else(|e| die(format!("setup {e:?}")));
    let t = Instant::now();
    let got = match it.run() {
        Ok(Value::Number(x)) => x,
        other => die(format!("run {other:?}")),
    };
    let t_run = t.elapsed();
    if got != want {
        die(format!("{case} x{scale}: checksum {got} != {want}"));
    }
    let st = it.heap.stats();
    let freed = it.collect(&[]).unwrap_or_else(|e| die(format!("collect {e:?}")));
    println!(
        "{case:6} x{scale:<3} n={n:<8} run={:>8.1}ms peak_rss={:.1} MiB | heap objs alloc={} live={} freed={} bytes={} | steps={}",
        t_run.as_secs_f64() * 1e3,
        proc_kib("VmHWM:") as f64 / 1024.0,
        st.total_object_allocs,
        st.live_objects,
        freed.objects,
        st.bytes,
        it.stats().steps,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: tagged_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    std::thread::Builder::new().stack_size(16 << 20).spawn(move || run_case(name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
