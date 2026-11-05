//! Load test for Intl.DateTimeFormat. One case at one scale per process, so the peak memory belongs to that run.
//!
//!   intl_load CASE SCALE    CASE: make | format | parts | locale | plain    SCALE: 1, 10, 100
//!
//! Each case runs a small program and checks its result against the same computation done in Rust (the checksum).
//! `make` keeps n/20 formatters alive, so its memory is the cost per formatter. `format` calls one formatter n times
//! ("HH:MM", h23, UTC); `parts` calls `formatToParts` n/10 times (4 objects per call, and a run does not collect). `locale` calls `toLocaleTimeString` with options n times (a formatter per call, in
//! Asia/Kolkata). `plain` is the baseline: the same loop with a string concatenation of the same length. It prints wall
//! time, peak resident memory (from /proc), heap objects and steps. `tools/js/intl_load.sh` runs the matrix with caps.
//! `INTL_EMPTY=1` runs `make` with a plain `{}` in place of the formatter, to measure the cost of the formatter alone.

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
    let f = "new Intl.DateTimeFormat('en-US', { timeZone: 'UTC', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' })";
    match case {
        "make" => {
            let k = n / 20;
            (format!("var o = {{ timeZone: 'UTC', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' }}; var a = []; for (var i = 0; i < {k}; i++) a.push(new Intl.DateTimeFormat('en-US', o)); a.length"), k as f64)
        }
        "format" => (format!("var f = {f}; var x = 0; for (var i = 0; i < {n}; i++) x += f.format(i * 60000).length; x"), n as f64 * 5.0),
        "parts" => {
            let k = n / 10;
            (format!("var f = {f}; var x = 0; for (var i = 0; i < {k}; i++) x += f.formatToParts(i * 60000).length; x"), k as f64 * 3.0)
        }
        "locale" => (
            format!("var o = {{ hour: '2-digit', minute: '2-digit', hourCycle: 'h23', timeZone: 'Asia/Kolkata' }}; var x = 0; for (var i = 0; i < {n}; i++) x += new Date(i * 60000).toLocaleTimeString('en-US', o).length; x"),
            n as f64 * 5.0,
        ),
        "plain" => (format!("var x = 0; for (var i = 0; i < {n}; i++) x += ('' + (10 + i % 10) + ':' + (10 + i % 6)).length; x"), n as f64 * 5.0),
        _ => die(format!("unknown case {case}")),
    }
}

fn run_case(case: String, scale: usize) {
    let n = 10_000 * scale;
    let (mut src, want) = program(&case, n);
    if std::env::var_os("INTL_EMPTY").is_some() {
        src = src.replace("new Intl.DateTimeFormat('en-US', o)", "{}");
    }
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
        die("usage: intl_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    std::thread::Builder::new().stack_size(16 << 20).spawn(move || run_case(name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
