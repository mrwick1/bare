//! Load test for the Array/Object/Math methods. One case at one scale per process, so that the peak memory
//! belongs to that run. Same output format and caps as `eval_load`.
//!
//!   arrobj_load CASE SCALE    CASE: mapfilter | sort | queue | objects | arrays    SCALE: 1, 10, 100
//!
//! Each case builds a small program, runs it, and checks the result against the same computation done in Rust
//! (the checksum). A wrong checksum exits with status 1 and prints no timing. It prints parse, setup and run
//! time (wall), CPU time, peak resident memory, allocations of the run, steps, the environments and functions the
//! evaluator holds, and what an explicit `collect` frees. `tools/js/eval_load.sh` runs the matrix with caps.

use bare_js::eval::*;
use bare_js::heap::Heap;
use bare_js::parse::{parse_script, Limits};
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

/// (program source, expected result text). Each case computes the same value in Rust.
struct Case {
    src: String,
    expect: String,
}

fn case(name: &str, scale: usize) -> Case {
    match name {
        "mapfilter" => {
            let n = 2_000 * scale as u64;
            let (mut s, mut cnt) = (0u64, 0u64);
            for i in 0..n {
                let v = i * 3 + 1;
                if v % 2 == 0 {
                    s += v;
                    cnt += 1;
                }
            }
            Case {
                src: format!(
                    "var a = []; for (var i = 0; i < {n}; i++) a.push(i);
                     var r = a.map(function(x) {{ return x * 3 + 1; }}).filter(function(x) {{ return x % 2 === 0; }});
                     r.reduce(function(p, c) {{ return p + c; }}, 0) + ',' + r.length + ',' + (r.some(function(x) {{ return x < 0; }}) ? 1 : 0)"
                ),
                expect: format!("{s},{cnt},0"),
            }
        }
        "sort" => {
            let n = 2_000 * scale as u64;
            let mut v: Vec<u64> = (0..n).map(|i| (i * 7919) % 10007).collect();
            v.sort();
            let chk: u64 = v.iter().enumerate().map(|(i, x)| (i as u64 % 13) * x).sum::<u64>() % 1000003;
            let mut d: Vec<String> = (0..n).map(|i| ((i * 7919) % 10007).to_string()).collect();
            d.sort();
            let first = d[0].clone();
            Case {
                src: format!(
                    "var a = []; for (var i = 0; i < {n}; i++) a.push((i * 7919) % 10007);
                     var b = a.map(String); var c = a.slice();
                     c.sort(function(x, y) {{ return x - y; }});
                     var t = 0; for (var j = 0; j < c.length; j++) t += (j % 13) * c[j];
                     b.sort();
                     (t % 1000003) + ',' + b[0]"
                ),
                expect: format!("{chk},{first}"),
            }
        }
        "queue" => {
            let n = 1_000 * scale as u64;
            let mut q: std::collections::VecDeque<u64> = (0..n).collect();
            let mut s = 0u64;
            let mut i = 0u64;
            while let Some(x) = q.pop_front() {
                s += x;
                i += 1;
                if i.is_multiple_of(5) && i < n {
                    q.push_back(1);
                }
            }
            Case {
                src: format!(
                    "var q = []; for (var i = 0; i < {n}; i++) q.push(i);
                     var s = 0, k = 0; while (q.length) {{ s += q.shift(); k++; if (k % 5 === 0 && k < {n}) q.push(1); }} s"
                ),
                expect: s.to_string(),
            }
        }
        "objects" => {
            let n = 200 * scale as u64;
            let sum: u64 = (0..n).map(|i| i * 2).sum();
            Case {
                src: format!(
                    "var o = {{}}; for (var i = 0; i < {n}; i++) o['k' + i] = i;
                     var c = Object.assign({{}}, o, {{ extra: 1 }});
                     var e = Object.entries(c).filter(function(p) {{ return p[0] !== 'extra'; }}).map(function(p) {{ return [p[0], p[1] * 2]; }});
                     var f = Object.fromEntries(e);
                     var t = 0; Object.values(f).forEach(function(v) {{ t += v; }});
                     t + ',' + Object.keys(f).length"
                ),
                expect: format!("{sum},{n}"),
            }
        }
        "arrays" => {
            let n = 500 * scale as u64;
            let len = n - 10 + n / 2 + 3;
            Case {
                src: format!(
                    "var a = Array.from({{ length: {n} }}, function(_, i) {{ return i; }});
                     var b = a.slice(10).concat(a.slice(0, {n} / 2), [1, 2, 3]);
                     var h = 0; for (var i = 0; i < 50; i++) h += b.indexOf(a[i * 7 % {n}]) + (b.includes(-1) ? 1 : 0);
                     var sp = b.slice(); sp.splice(5, 20, 'x'); sp.splice(2, 0, 'y', 'z');
                     b.length + ',' + sp.length + ',' + Array.of(1, 2).length + ',' + Math.max.apply(null, [1, h > 0 ? 5 : 0])"
                ),
                expect: format!("{len},{},2,5", len - 20 + 1 + 2),
            }
        }
        _ => die(format!("unknown case {name}")),
    }
}

fn run_case(name: String, scale: usize) {
    let c = case(&name, scale);
    let t0 = Instant::now();
    let prog = parse_script(&c.src, Limits::default()).unwrap_or_else(|e| die(format!("parse: {e}")));
    let parse_ms = t0.elapsed().as_secs_f64() * 1e3;
    let t1 = Instant::now();
    let limits = EvalLimits { max_steps: u64::MAX / 2, max_call_depth: 100, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let mut it = Interp::new(&prog, Heap::default(), limits).unwrap_or_else(|e| die(format!("setup: {e:?}")));
    let setup_ms = t1.elapsed().as_secs_f64() * 1e3;
    let (a0, b0, cpu0) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed), cpu_ms());
    PEAK.store(LIVE.load(Relaxed), Relaxed);
    let t2 = Instant::now();
    let r = it.run().unwrap_or_else(|e| die(format!("run: {e:?}")));
    let run_ms = t2.elapsed().as_secs_f64() * 1e3;
    let cpu = cpu_ms() - cpu0;
    let (allocs, abytes, peak_live) = (ALLOCS.load(Relaxed) - a0, BYTES.load(Relaxed) - b0, PEAK.load(Relaxed));
    let got = it.display(r);
    if got != c.expect {
        die(format!("{name} x{scale}: checksum {got} != expected {}", c.expect));
    }
    let st = it.stats();
    let hs = it.heap.stats();
    let t3 = Instant::now();
    let freed = it.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
    let gc_ms = t3.elapsed().as_secs_f64() * 1e3;
    let st2 = it.stats();
    println!(
        "{name:9} x{scale:<3} checksum=ok parse={parse_ms:8.1}ms setup={setup_ms:6.2}ms run={run_ms:9.1}ms cpu={cpu}ms steps={} ({:.1} Msteps/s) peak_rss={:.1}MiB peak_live={:.1}MiB allocs={allocs} alloc={:.1}MiB | held: envs={} funcs={} heap_objs={} heap_bytes={:.1}MiB | collect={gc_ms:.1}ms freed_objs={} -> envs={} funcs={}",
        st.steps,
        st.steps as f64 / run_ms / 1e3,
        status_kib("VmHWM:") as f64 / 1024.0,
        peak_live as f64 / 1048576.0,
        abytes as f64 / 1048576.0,
        st.live_envs,
        st.functions,
        hs.live_objects,
        hs.bytes as f64 / 1048576.0,
        freed.objects,
        st2.live_envs,
        st2.functions,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: arrobj_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    // 16 MiB: a JS call level needs a few KiB of native stack and max_call_depth is 100.
    std::thread::Builder::new().stack_size(16 << 20).spawn(move || run_case(name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
