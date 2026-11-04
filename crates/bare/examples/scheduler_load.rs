//! Load test for the scheduler (timers, microtasks, MessageChannel). One case at one scale per process, on a virtual
//! clock: nothing sleeps.
//!
//!   scheduler_load CASE SCALE      CASE: timers | messages | mixed      SCALE: 1, 10, 100
//!
//! timers: SCALE*1000 timers with 101 different delays and ids, every 5th cleared at once, plus an interval per batch
//! that clears itself after 3 runs. messages: SCALE*1000 numbers and strings through a fresh MessageChannel per batch.
//! mixed: SCALE*1000 timers that each queue a microtask, a Promise job and a message whose handler queues a microtask.
//! Batches of 500 run through `advance_to` with at most 256 tasks per pump and a `collect` after each pump. Each case
//! checks sums, the firing order (a rolling hash) and counters against the same computation in Rust, then checks
//! that the queues and the port and event metadata are empty after the last collect. Prints time, CPU, peak RSS,
//! allocations and the realm counters. `tools/js/scheduler_load.sh` runs the matrix under caps.

use bare::browser::*;
use bare_html::tree;
use bare_js::eval::*;
use bare_js::heap::HeapLimits;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            ALLOCS.fetch_add(1, Relaxed);
            BYTES.fetch_add(l.size(), Relaxed);
            let live = LIVE.fetch_add(l.size(), Relaxed) + l.size();
            PEAK.fetch_max(live, Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            ALLOCS.fetch_add(1, Relaxed);
            if new > l.size() {
                BYTES.fetch_add(new - l.size(), Relaxed);
                let live = LIVE.fetch_add(new - l.size(), Relaxed) + new - l.size();
                PEAK.fetch_max(live, Relaxed);
            } else {
                LIVE.fetch_sub(l.size() - new, Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

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

fn expect(what: &str, got: String, want: String) {
    if got != want {
        die(format!("{what}: {got} != {want}"));
    }
}

const BATCH: usize = 500;
const PUMP: usize = 256;

fn exec(name: &str, scale: usize) {
    let eval = EvalLimits { max_steps: u64::MAX / 2, max_scripts: 1 << 14, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    b.use_virtual_clock();
    let (a0, b0, cpu0) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed), cpu_ms());
    PEAK.store(LIVE.load(Relaxed), Relaxed);
    let t = Instant::now();
    let (mut gc_ms, mut freed_objs, mut pumps, mut tasks, mut max_queue) = (0.0, 0usize, 0usize, 0usize, 0usize);
    let n = 1000 * scale;
    let mut settle = |b: &mut Browser, span: f64| {
        let target = b.now() + span;
        loop {
            let s = b.sched_stats();
            max_queue = max_queue.max(s.tasks).max(s.queued_messages);
            let p = b.advance_to(target, PUMP).unwrap_or_else(|e| die(format!("pump: {e:?}")));
            pumps += 1;
            tasks += p.ran;
            let t = Instant::now();
            freed_objs += b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}"))).objects;
            gc_ms += t.elapsed().as_secs_f64() * 1e3;
            if !p.capped {
                break;
            }
        }
    };
    let (mut sum, mut cnt, mut h) = (0u64, 0u64, 0u64);
    let chain = |h: &mut u64, v: u64| *h = (*h * 31 + v) % 1000003;
    match name {
        "timers" => {
            run(&mut b, "var sum = 0, cnt = 0, h = 0, ic = 0;
                function mk() { return function (a) { sum += a; cnt++; h = (h * 31 + a) % 1000003 } }
                function ivl() { var i = 0; var id = setInterval(function () { i++; ic++; if (i === 3) clearInterval(id) }, 50) }
                function batch(base, count) { ivl(); for (var k = base; k < base + count; k++) { var id = setTimeout(mk(), (k * 7) % 101, k); if (k % 5 === 0) clearTimeout(id) } }");
            for base in (0..n).step_by(BATCH) {
                run(&mut b, &format!("batch({base}, {BATCH})"));
                let mut keep: Vec<usize> = (base..base + BATCH).filter(|k| k % 5 != 0).collect();
                keep.sort_by_key(|&k| ((k * 7) % 101, k));
                for k in keep {
                    sum += k as u64;
                    cnt += 1;
                    chain(&mut h, k as u64);
                }
                settle(&mut b, 200.0);
            }
            let ic = (n / BATCH * 3) as u64;
            expect("timers", run(&mut b, "[sum, cnt, h, ic].join()"), format!("{sum},{cnt},{h},{ic}"));
        }
        "messages" => {
            run(&mut b, "var sum = 0, cnt = 0, h = 0, c;
                function batch(base, count) {
                  c = new MessageChannel();
                  c.port1.onmessage = function (e) { var d = e.data; var v = typeof d === 'number' ? d : d.length; sum += v; cnt++; h = (h * 31 + v) % 1000003 };
                  for (var k = base; k < base + count; k++) c.port2.postMessage(k % 2 === 0 ? k : 'p' + k);
                }");
            for base in (0..n).step_by(BATCH) {
                run(&mut b, &format!("batch({base}, {BATCH})"));
                for k in base..base + BATCH {
                    let v = if k % 2 == 0 { k as u64 } else { format!("p{k}").len() as u64 };
                    sum += v;
                    cnt += 1;
                    chain(&mut h, v);
                }
                settle(&mut b, 1.0);
            }
            expect("messages", run(&mut b, "[sum, cnt, h].join()"), format!("{sum},{cnt},{h}"));
        }
        "mixed" => {
            run(&mut b, "var t = 0, m = 0, q = 0, p = 0;
                function batch(base, count) {
                  var c = new MessageChannel();
                  c.port1.onmessage = function (e) { m++; queueMicrotask(function () { q++ }) };
                  for (var k = base; k < base + count; k++) (function (k) {
                    setTimeout(function () { t++; queueMicrotask(function () { q++ }); Promise.resolve(k).then(function (v) { p += v }); c.port2.postMessage(k) }, k % 13)
                  })(k);
                }");
            for base in (0..n).step_by(BATCH) {
                run(&mut b, &format!("batch({base}, {BATCH})"));
                sum += (base..base + BATCH).map(|k| k as u64).sum::<u64>();
                settle(&mut b, 20.0);
            }
            let n = n as u64;
            expect("mixed", run(&mut b, "[t, m, q, p].join()"), format!("{n},{n},{},{sum}", 2 * n));
        }
        _ => die(format!("unknown case {name}")),
    }
    // the last collect left nothing behind
    b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
    let s = b.sched_stats();
    let st = b.stats();
    let live_ports = if name == "messages" { 2 } else { 0 };
    if (s.timers, s.tasks, s.queued_messages, s.queued_message_bytes, s.message_events, s.ports, st.events) != (0, 0, 0, 0, 0, live_ports, 0) {
        die(format!("queues or metadata not empty: {s:?} events={}", st.events));
    }
    let run_ms = t.elapsed().as_secs_f64() * 1e3;
    let cpu = cpu_ms() - cpu0;
    let (allocs, abytes, peak_live) = (ALLOCS.load(Relaxed) - a0, BYTES.load(Relaxed) - b0, PEAK.load(Relaxed));
    println!(
        "{name:8} x{scale:<3} checksum=ok run={run_ms:9.1}ms cpu={cpu}ms peak_rss={:.1}MiB peak_live={:.1}MiB live_after={:.1}MiB allocs={allocs} alloc={:.1}MiB | tasks={tasks} pumps={pumps} max_queue={max_queue} after: timers={} tasks={} queued={} ports={} events={} wrappers={} | gc={gc_ms:.1}ms freed_objs={freed_objs}",
        status_kib("VmHWM:") as f64 / 1024.0,
        peak_live as f64 / 1048576.0,
        LIVE.load(Relaxed) as f64 / 1048576.0,
        abytes as f64 / 1048576.0,
        s.timers,
        s.tasks,
        s.queued_messages,
        s.ports,
        st.events,
        st.wrappers,
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        die("usage: scheduler_load CASE SCALE".into());
    }
    let (name, scale) = (args[1].clone(), args[2].parse::<usize>().unwrap_or_else(|_| die("bad scale".into())));
    // the evaluator needs a big stack: 8 MiB in release is the stated floor, use 64 MiB
    let t = std::thread::Builder::new().stack_size(64 << 20).spawn(move || exec(&name, scale)).unwrap();
    t.join().unwrap_or_else(|_| die("panic".into()));
}
