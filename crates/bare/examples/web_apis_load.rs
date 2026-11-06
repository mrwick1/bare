//! Load test for `AbortController`/`AbortSignal`, `structuredClone`, `btoa`/`atob`, `Intl.Segmenter` and `String.prototype.matchAll`.
//! One case at one scale per process.
//!
//!   web_apis_load CASE SCALE      CASE: abort | clone | base64 | segmenter | matchall      SCALE: 1, 10, 100
//!
//! Each case runs SCALE*1000 rounds, in scripts of 100 rounds with a VM collect between them, and checks its sum against the same
//! computation in Rust. abort: a controller, a listener, `AbortSignal.any`, an `{signal}` listener and `abort(reason)`.
//! clone: a cyclic graph with Map, Set, Date, RegExp, typed array and Error, and one refused function. base64: `btoa` and `atob`
//! round trips of 1 to 300 bytes, and one invalid input. segmenter: word and grapheme segmentation, `containing`. matchall:
//! `matchAll` over digit runs and capture groups. After the last collect the live object count must be back to the baseline
//! (nothing leaks). Prints time, CPU, peak RSS and allocations. `tools/js/web_apis_load.sh` runs the matrix under caps.

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

const CHUNK: usize = 100;

fn digits(n: usize) -> usize {
    n.to_string().len()
}

/// (script that defines `chunk(start, count)`, the Rust sum of round `i`).
fn program(name: &str) -> (&'static str, fn(usize) -> usize) {
    match name {
        "abort" => (
            "function chunk(start, count) { var s = 0;
               for (var i = start; i < start + count; i++) {
                 var c = new AbortController(), d = new AbortController(), n = 0, fired = 0;
                 c.signal.addEventListener('abort', function () { n++; });
                 var any = AbortSignal.any([c.signal, d.signal]);
                 var t = new EventTarget(); t.addEventListener('x', function () { fired++; }, { signal: c.signal });
                 c.abort('r');
                 t.dispatchEvent(new Event('x'));
                 s += n + (c.signal.aborted ? 1 : 0) + (any.aborted ? 1 : 0) + any.reason.length + fired + (AbortSignal.abort().reason.name === 'AbortError' ? 1 : 0);
               }
               return s; }",
            |_| 5,
        ),
        "clone" => (
            "function chunk(start, count) { var s = 0;
               for (var i = start; i < start + count; i++) {
                 var o = { i: i, a: [1, 2, 3], m: new Map([[1, 'a'], [2, 'b']]), s: new Set([i, i + 1]), d: new Date(i), r: /x+/g, u: new Uint8Array([i & 255, 2]), e: new RangeError('m' + i) };
                 o.self = o;
                 var c = structuredClone(o);
                 s += (c.self === c ? 1 : 0) + (c !== o ? 1 : 0) + c.i + c.a.length + c.m.size + c.s.size + c.d.getTime() + c.r.flags.length + c.u[0] + c.e.message.length + (c.e instanceof RangeError ? 1 : 0);
                 try { structuredClone(function () {}); } catch (e) { s += e.name === 'DataCloneError' ? 1 : 0; }
               }
               return s; }",
            |i| 1 + 1 + i + 3 + 2 + 2 + i + 1 + (i & 255) + 1 + digits(i) + 1 + 1,
        ),
        "base64" => (
            "function chunk(start, count) { var s = 0;
               for (var i = start; i < start + count; i++) {
                 var len = 1 + (i % 300), str = '';
                 for (var j = 0; j < len; j++) str += String.fromCharCode((i + j) & 255);
                 var b = btoa(str);
                 s += atob(b) === str ? b.length : -1e9;
                 try { atob('a*b'); } catch (e) { s += e.name === 'InvalidCharacterError' ? 1 : 0; }
               }
               return s; }",
            |i| 4 * (1 + (i % 300)).div_ceil(3) + 1,
        ),
        "segmenter" => (
            "function chunk(start, count) { var s = 0;
               var w = new Intl.Segmenter('en', { granularity: 'word' }), g = new Intl.Segmenter();
               for (var i = start; i < start + count; i++) {
                 var segs = w.segment('Hello, world ' + i + '! ok'), n = 0, wl = 0;
                 for (var x of segs) { n++; if (x.isWordLike) wl++; }
                 s += n + wl + Array.from(g.segment('e\\u0301x\\uD83D\\uDC4D')).length + segs.containing(0).segment.length;
               }
               return s; }",
            |_| 9 + 4 + 3 + 5,
        ),
        "matchall" => (
            "function chunk(start, count) { var s = 0;
               for (var i = start; i < start + count; i++) {
                 var all = [...('a1b22c333-' + i).matchAll(/\\d+/g)], idx = 0;
                 for (var m of all) idx += m.index;
                 s += all.length + idx + all[all.length - 1][0].length + Array.from('k1=v1;k2=v2'.matchAll(/(\\w+)=(\\w+)/g)).length;
               }
               return s; }",
            |i| 4 + 20 + digits(i) + 2,
        ),
        _ => die(format!("unknown case {name}")),
    }
}

fn exec(name: &str, scale: usize) {
    let eval = EvalLimits { max_steps: u64::MAX / 2, max_scripts: 1 << 14, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    b.set_document_url("https://h.test:8443/start").unwrap_or_else(|e| die(e));
    let (prog, round) = program(name);
    run(&mut b, prog);
    // one round first: the lazy globals and `Intl` are made here, so they are not counted as growth
    run(&mut b, "chunk(0, 1)");
    b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
    let base_live = b.it.heap.stats().live_objects;
    let (a0, b0, cpu0) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed), cpu_ms());
    PEAK.store(LIVE.load(Relaxed), Relaxed);
    let t = Instant::now();
    let (mut gc_ms, mut freed_objs) = (0.0, 0);
    let n = 1000 * scale;
    let (mut got, mut want) = (0usize, 0usize);
    for start in (0..n).step_by(CHUNK) {
        let v: f64 = run(&mut b, &format!("chunk({start}, {CHUNK})")).parse().unwrap_or_else(|_| die("chunk result".into()));
        got = got.wrapping_add(v as i64 as usize);
        for i in start..start + CHUNK {
            want += round(i);
        }
        let t = Instant::now();
        freed_objs += b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}"))).objects;
        gc_ms += t.elapsed().as_secs_f64() * 1e3;
    }
    expect(name, got.to_string(), want.to_string());
    let live = b.it.heap.stats().live_objects;
    if live > base_live + 8 {
        die(format!("{name}: {live} live objects after the last collect, {base_live} at the start"));
    }
    let run_ms = t.elapsed().as_secs_f64() * 1e3;
    let cpu = cpu_ms() - cpu0;
    let (allocs, abytes, peak_live) = (ALLOCS.load(Relaxed) - a0, BYTES.load(Relaxed) - b0, PEAK.load(Relaxed));
    println!(
        "{name:9} x{scale:<3} checksum=ok run={run_ms:9.1}ms cpu={cpu}ms peak_rss={:.1}MiB peak_live={:.1}MiB live_after={:.1}MiB allocs={allocs} alloc={:.1}MiB | gc={gc_ms:.1}ms freed_objs={freed_objs} live_objs={live}/{base_live}",
        status_kib("VmHWM:") as f64 / 1024.0,
        peak_live as f64 / 1048576.0,
        LIVE.load(Relaxed) as f64 / 1048576.0,
        abytes as f64 / 1048576.0,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: web_apis_load abort|clone|base64|segmenter|matchall 1|10|100".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    std::thread::Builder::new().stack_size(256 << 20).spawn(move || exec(&name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
