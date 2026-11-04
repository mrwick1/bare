//! Load test for the script-made default `ReadableStream`. One case at one scale per process.
//!
//!   streams_load CASE SCALE      CASE: flow | many | pending | churn      SCALE: 1, 10, 100
//!
//! flow: one stream with a `pull` source that enqueues SCALE*5000 TextEncoder rows (highWaterMark 4) and a reader loop; the
//! Rust side checks the count, the bytes and that the queue never passed the mark by more than one chunk.
//! many: SCALE*300 short streams (3 chunks and close each), each read to the end.
//! pending: SCALE*3 rounds of 1000 pending reads on one stream, a collection while they wait, then 1000 enqueues.
//! churn: SCALE*300 streams that are cancelled, released and re-locked, or errored while reads are pending.
//! A collection runs between batches. Prints time, CPU, peak RSS and what the stream layer holds. Only throwaway
//! synthetic data. No network. `tools/js/streams_load.sh` runs the matrix under caps.

use bare::browser::*;
use bare_html::tree;
use bare_js::eval::*;
use bare_js::heap::HeapLimits;
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
        Err(e) => die(format!("script error {e:?} in {src}")),
    }
}

fn settle(b: &mut Browser) {
    if let Err(e) = b.drain() {
        die(format!("drain {e:?}"));
    }
}

fn collect(b: &mut Browser) {
    if let Err(e) = b.collect(&[]) {
        die(format!("collect {e:?}"));
    }
}

fn expect(what: &str, got: String, want: String) {
    if got != want {
        die(format!("{what}: {got} != {want}"));
    }
}

fn flow(b: &mut Browser, scale: usize) {
    let n = scale * 5000;
    run(b, &format!(r#"var enc = new TextEncoder(), N = {n}, produced = 0, consumed = 0, bytes = 0, first = 0, maxQ = 0, finished = false, left = 0;
var rs = new ReadableStream({{ pull(c) {{
  if (produced >= N) {{ c.close(); return; }}
  c.enqueue(enc.encode('row' + produced + ':' + 'abcdefghijklmnopqrstuvwxyz0123456789')); produced++;
  maxQ = Math.max(maxQ, 4 - c.desiredSize); }} }}, {{ highWaterMark: 4 }});
var reader = rs.getReader();
function batch(n) {{ left = n; (function step() {{ if (left-- <= 0) return; return reader.read().then(function (x) {{
  if (x.done) {{ finished = true; return; }} consumed++; bytes += x.value.length; first += x.value[3] - 48; return step(); }}); }})(); }}"#));
    while run(b, "finished") != "true" {
        run(b, "batch(1000)");
        settle(b);
        collect(b);
    }
    let want: usize = (0..n).map(|i| 3 + i.to_string().len() + 1 + 36).sum();
    expect("consumed", run(b, "consumed"), n.to_string());
    expect("bytes", run(b, "bytes"), want.to_string());
    expect("produced", run(b, "produced"), n.to_string());
    let q: usize = run(b, "maxQ").parse().unwrap();
    if q > 5 {
        die(format!("queue passed the mark: {q}"));
    }
}

fn many(b: &mut Browser, scale: usize) {
    let total = scale * 300;
    run(b, "var made = 0, got = 0, sum = 0, enc = new TextEncoder();
function one(k) { var rs = new ReadableStream({ start(c) { for (var i = 0; i < 3; i++) c.enqueue(enc.encode('k' + k + 'c' + i)); c.close(); } }, { highWaterMark: 3 });
  var r = rs.getReader(); made++;
  (function step() { return r.read().then(function (x) { if (x.done) return; got++; sum += x.value.length; return step(); }); })(); }");
    let mut k = 0;
    while k < total {
        let end = (k + 100).min(total);
        run(b, &format!("for (var k = {k}; k < {end}; k++) one(k);"));
        settle(b);
        collect(b);
        k = end;
    }
    let want: usize = (0..total).map(|k| 3 * (1 + k.to_string().len() + 2)).sum();
    expect("made", run(b, "made"), total.to_string());
    expect("got", run(b, "got"), (total * 3).to_string());
    expect("sum", run(b, "sum"), want.to_string());
}

fn pending(b: &mut Browser, scale: usize) {
    let rounds = scale * 3;
    run(b, "var ctl, total = 0, order = true, last = -1, seen = 0;
function round() { var rs = new ReadableStream({ start(c) { ctl = c; } }, { highWaterMark: 0 }); var r = rs.getReader(); last = -1;
  for (var i = 0; i < 1000; i++) r.read().then(function (x) { if (x.value.i !== last + 1) order = false; last = x.value.i; seen++; total += x.value.i; }); }");
    for _ in 0..rounds {
        run(b, "round()");
        if b.stream_stats().pending_reads != 1000 {
            die(format!("pending reads {}", b.stream_stats().pending_reads));
        }
        // Only the controller is held. The stream, reader and 1000 promises with their callbacks must live.
        collect(b);
        collect(b);
        expect("pending after collect", b.stream_stats().pending_reads.to_string(), "1000".into());
        run(b, "for (var i = 0; i < 1000; i++) ctl.enqueue({ i: i }); ctl.close();");
        settle(b);
        collect(b);
    }
    expect("seen", run(b, "seen"), (rounds * 1000).to_string());
    expect("total", run(b, "total"), (rounds * 499_500).to_string());
    expect("order", run(b, "order"), "true".into());
}

fn churn(b: &mut Browser, scale: usize) {
    let total = scale * 300;
    run(b, "var cancelled = 0, released = 0, errored = 0, srcCancels = 0, k3 = 0;
function mk3(i) { return new ReadableStream({ start(c) { c.enqueue(i); c.enqueue(i + 1); }, cancel() { srcCancels++; } }, { highWaterMark: 4 }); }
function churn(i) {
  var rs = mk3(i), r = rs.getReader();
  if (i % 3 === 0) { r.read().then(function () { return r.cancel('x'); }).then(function () { cancelled++; }); }
  else if (i % 3 === 1) { var p = r.read(); p.then(function () { r.releaseLock(); var r2 = rs.getReader(); return r2.read(); }).then(function (x) { if (x.value === i + 1) released++; }); }
  else { var pend = new ReadableStream({ start(c) { k3 = c; } }).getReader(); pend.read().catch(function (e) { if (e === 'E') errored++; }); k3.error('E'); }
}");
    let mut k = 0;
    while k < total {
        let end = (k + 100).min(total);
        run(b, &format!("for (var k = {k}; k < {end}; k++) churn(k);"));
        settle(b);
        collect(b);
        k = end;
    }
    let (c, r, e) = ((0..total).filter(|i| i % 3 == 0).count(), (0..total).filter(|i| i % 3 == 1).count(), (0..total).filter(|i| i % 3 == 2).count());
    expect("cancelled", run(b, "cancelled"), c.to_string());
    expect("released", run(b, "released"), r.to_string());
    expect("errored", run(b, "errored"), e.to_string());
    expect("srcCancels", run(b, "srcCancels"), c.to_string());
}

fn exec(case: &str, scale: usize) {
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 200_000_000, ..Default::default() }, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser {e:?}")));
    let (t0, c0) = (Instant::now(), cpu_ms());
    match case {
        "flow" => flow(&mut b, scale),
        "many" => many(&mut b, scale),
        "pending" => pending(&mut b, scale),
        "churn" => churn(&mut b, scale),
        _ => die("case".into()),
    }
    settle(&mut b);
    collect(&mut b);
    let s = b.stream_stats();
    if (s.pending_reads, s.queued_chunks, s.queued_bytes) != (0, 0, 0) {
        die(format!("left over: {s:?}"));
    }
    println!("case={case} scale={scale} ok wall_ms={} cpu_ms={} peak_rss_kib={} streams_left={} readers_left={} callbacks={}", t0.elapsed().as_millis(), cpu_ms() - c0, status_kib("VmHWM:"), s.streams, s.readers, s.callbacks);
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: streams_load flow|many|pending|churn 1|10|100".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    let stack = if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 };
    std::thread::Builder::new().stack_size(stack).spawn(move || exec(&name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
