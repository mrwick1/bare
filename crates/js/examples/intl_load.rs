//! Load test for Intl.DateTimeFormat, Intl.NumberFormat, `toLocaleString` and `localeCompare`. One case at one scale per process, so the peak memory belongs to that run.
//!
//!   intl_load CASE SCALE    CASE: make | format | parts | locale | plain | nf_format | nf_locale | nf_locale_opts | cmp | cmp_opts
//!                           SCALE: 1, 10, 100
//!
//! Each case runs a small program and checks its result against the same computation done in Rust (the checksum).
//! `make` keeps n/20 formatters alive, so its memory is the cost per formatter. `format` calls one formatter n times
//! ("HH:MM", h23, UTC); `parts` calls `formatToParts` n/10 times (4 objects per call, and a run does not collect). `locale` calls `toLocaleTimeString` with options n times (a formatter per call, in
//! Asia/Kolkata). `plain` is the baseline: the same loop with a string concatenation of the same length. It prints wall
//! time, peak resident memory (from /proc), heap objects and steps. `tools/js/intl_load.sh` runs the matrix with caps.
//! `nf_format` calls one en-IN `Intl.NumberFormat` n times on `i * 1234.5`. `nf_locale` calls `toLocaleString('en-IN')` n times
//! on the same values. `nf_locale_opts` calls `toLocaleString('en-US', { style: 'currency', currency: 'USD' })` n times (the
//! options are read on every call). `cmp` calls `'é'.localeCompare(x)` n times, `cmp_opts` calls `localeCompare` with
//! `{ numeric: true, sensitivity: 'base' }` n times. The checksums are the lengths and the sums computed in Rust.
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

/// The length of the en-IN text of `i * 1234.5`: Indian grouping of the integer part, then `.5` for an odd `i`.
fn indian_len(i: u64) -> usize {
    let twice = i * 2469;
    let d = (twice / 2).to_string().len();
    d + if d > 3 { (d - 3).div_ceil(2) } else { 0 } + if twice % 2 == 1 { 2 } else { 0 }
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
        "nf_format" => (
            format!("var f = new Intl.NumberFormat('en-IN'); var x = 0; for (var i = 0; i < {n}; i++) x += f.format(i * 1234.5).length; x"),
            (0..n as u64).map(indian_len).sum::<usize>() as f64,
        ),
        "nf_locale" => (
            format!("var x = 0; for (var i = 0; i < {n}; i++) x += (i * 1234.5).toLocaleString('en-IN').length; x"),
            (0..n as u64).map(indian_len).sum::<usize>() as f64,
        ),
        "nf_locale_opts" => (
            format!("var o = {{ style: 'currency', currency: 'USD' }}; var x = 0; for (var i = 0; i < {n}; i++) x += (i % 1000).toLocaleString('en-US', o).length; x"),
            // `$`, the digits and `.00` (the values stay below 1000: no group separator)
            (0..n).map(|i| 4 + (i % 1000).to_string().len()).sum::<usize>() as f64,
        ),
        "cmp" => (format!("var l = ['e', 'f', 'é']; var x = 0; for (var i = 0; i < {n}; i++) x += 'é'.localeCompare(l[i % 3]); x"), n.div_ceil(3) as f64 - ((n + 1) / 3) as f64),
        "cmp_opts" => (format!("var o = {{ numeric: true, sensitivity: 'base' }}; var x = 0; for (var i = 0; i < {n}; i++) x += 'a10'.localeCompare('a9', 'en', o); x"), n as f64),
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
