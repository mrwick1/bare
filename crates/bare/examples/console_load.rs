//! Load test for `window.console` and its bounded sink. One case at one scale per process.
//!
//!   console_load CASE SCALE      CASE: flood | retain | format | patch      SCALE: 1, 10, 100
//!
//! flood: SCALE*3000 calls into a 1024-record sink; the Rust side checks the drop count and the kept records.
//! retain: SCALE*500 records of fresh objects kept under collections; each one is read back after.
//! format: SCALE*300 `%d %s %f` lines checked against text built in Rust.
//! patch: SCALE*1000 calls through a patched, bound and applied console; the saved method, a replaced console and
//! a deleted console all keep recording.
//! The checks use sums and strings computed in Rust, not by the script. Nothing logged is ever printed. Throwaway
//! synthetic data only. No network. `tools/js/console_load.sh` runs the matrix under caps.

use bare::browser::*;
use bare::browser_console::*;
use bare_html::tree;
use bare_js::eval::*;
use bare_js::heap::HeapLimits;
use bare_js::value::Value;
use std::time::Instant;

fn die(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1)
}

fn status_kib(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok()).unwrap_or(0)
}

/// CPU time of this process in milliseconds (user + system), from /proc/self/stat (100 ticks per second).
fn cpu_ms() -> u64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let rest = s.rsplit_once(')').map_or("", |x| x.1);
    let f: Vec<&str> = rest.split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    (t(11) + t(12)) * 10
}

fn run(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(e) => die(format!("script error {e:?}")),
    }
}

fn collect(b: &mut Browser, extra: &[Value]) {
    if let Err(e) = b.collect(extra) {
        die(format!("collect {e:?}"));
    }
}

fn expect(what: &str, got: String, want: String) {
    if got != want {
        die(format!("{what}: {got} != {want}"));
    }
}

fn num(v: Value) -> f64 {
    match v {
        Value::Number(n) => n,
        _ => die("not a number".into()),
    }
}

fn flood(b: &mut Browser, scale: usize) {
    let n = scale * 3000;
    run(b, "var m = [console.log, console.warn, console.error]; function f(i) { m[i % 3](i, i * 2); }");
    for k in (0..n).step_by(1000) {
        run(b, &format!("for (var i = {k}; i < {}; i++) f(i);", k + 1000));
    }
    let s = b.console_stats();
    expect("stats", format!("{s:?}"), format!("{:?}", ConsoleStats { records: 1024, values: 2048, calls: n as u64, dropped: (n - 1024) as u64 }));
    let recs = b.take_console_records();
    let mut sum = 0.0;
    for (j, r) in recs.iter().enumerate() {
        let i = n - 1024 + j;
        let lvl = [ConsoleLevel::Log, ConsoleLevel::Warn, ConsoleLevel::Error][i % 3];
        if r.seq != i as u64 || r.level != lvl || r.args.len() != 2 || num(r.args[1]) != (i * 2) as f64 {
            die(format!("record {j}"));
        }
        sum += num(r.args[0]);
    }
    expect("sum", sum.to_string(), (n - 1024..n).sum::<usize>().to_string());
}

fn retain(b: &mut Browser, scale: usize) {
    let n = scale * 500;
    b.set_console_limits(ConsoleLimits { max_records: n, ..Default::default() });
    for k in (0..n).step_by(500) {
        run(b, &format!("for (var i = {k}; i < {}; i++) console.log({{ k: i }}, [i], 'row' + i);", k + 500));
        collect(b, &[]);
    }
    expect("kept", b.console_stats().records.to_string(), n.to_string());
    let recs = b.take_console_records();
    let mut sum = 0usize;
    for (i, r) in recs.iter().enumerate() {
        let k: usize = b.it.get_str(r.args[0], "k").parse().unwrap_or_else(|_| die("k".into()));
        if k != i || b.it.get_str(r.args[1], "0") != i.to_string() {
            die(format!("row {i}"));
        }
        sum += k;
    }
    expect("sum", sum.to_string(), (0..n).sum::<usize>().to_string());
    let extra: Vec<Value> = recs.iter().take(10).map(|r| r.args[0]).collect();
    collect(b, &extra);
    expect("held", b.it.get_str(extra[9], "k"), "9".into());
    expect("freed", b.it.get_str(recs[n - 1].args[0], "k"), "?".into());
}

fn format(b: &mut Browser, scale: usize) {
    let n = scale * 300;
    for k in (0..n).step_by(300) {
        run(b, &format!("for (var i = {k}; i < {}; i++) console.log('r%d:%s|%f', i, 'x' + i, i / 4);", (k + 300).min(n)));
        for (j, r) in b.take_console_records().iter().enumerate() {
            let i = k + j;
            let got = b.format_console_record(r).unwrap_or_else(|e| die(format!("format {e:?}")));
            if got != format!("r{i}:x{i}|{}", i as f64 / 4.0) {
                die(format!("line {i}"));
            }
        }
    }
}

fn patch(b: &mut Browser, scale: usize) {
    let n = scale * 1000;
    run(b, "var orig = console.error, c = 0, d = 0, e2 = 0, saved = console.warn;
console.error = function () { c++; return orig.apply(console, arguments); };
var bound = console.log.bind(console, 'p'); var other = { log: function () { d++; } };
function g(i) { console.error('a', i); bound(i); saved.call(null, i); if (i % 10 === 0) { var keep = window.console; window.console = other; console.log(i); window.console = keep; }
  if (i % 100 === 0) { var k = window.console; delete window.console; try { console.log(i); } catch (x) { e2++; } window.console = k; } }");
    let mut seen = 0;
    for k in (0..n).step_by(1000) {
        run(b, &format!("for (var i = {k}; i < {}; i++) g(i);", k + 1000));
        seen += b.take_console_records().len();
    }
    let s = b.console_stats();
    expect("calls", s.calls.to_string(), (3 * n).to_string());
    expect("dropped", s.dropped.to_string(), ((3 * n).saturating_sub(seen)).to_string());
    expect("c", run(b, "c"), n.to_string());
    expect("d", run(b, "d"), n.div_ceil(10).to_string());
    expect("e2", run(b, "e2"), n.div_ceil(100).to_string());
}

fn exec(case: &str, scale: usize) {
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 400_000_000, ..Default::default() }, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser {e:?}")));
    let (t0, c0) = (Instant::now(), cpu_ms());
    match case {
        "flood" => flood(&mut b, scale),
        "retain" => retain(&mut b, scale),
        "format" => format(&mut b, scale),
        "patch" => patch(&mut b, scale),
        _ => die("case".into()),
    }
    b.take_console_records();
    collect(&mut b, &[]);
    let s = b.console_stats();
    if (s.records, s.values) != (0, 0) {
        die(format!("left over: {s:?}"));
    }
    println!("case={case} scale={scale} ok wall_ms={} cpu_ms={} peak_rss_kib={} calls={} dropped={}", t0.elapsed().as_millis(), cpu_ms() - c0, status_kib("VmHWM:"), s.calls, s.dropped);
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: console_load flood|retain|format|patch 1|10|100".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    let stack = if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 };
    std::thread::Builder::new().stack_size(stack).spawn(move || exec(&name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
