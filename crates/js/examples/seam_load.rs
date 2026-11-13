//! Load test for the seams between Array, Object, String, RegExp and Symbol/Map/Set (species, spreadable, iterators, hooks). One case at one scale per process, so that the peak memory
//! belongs to that run. Same output format and caps as `eval_load`.
//!
//!   seam_load CASE SCALE    CASE: from_set | from_map_entries | concat_flag | iter_methods | forof_break | string_hooks | species_map    SCALE: 1, 10, 100
//!
//! Each case builds a small program, runs it, and checks the result against the same computation done in Rust
//! (the checksum). A wrong checksum exits with status 1 and prints no timing. It prints parse, setup and run
//! time (wall), CPU time, peak resident memory, allocations of the run, steps, the environments and functions the
//! evaluator holds, and what an explicit `collect` frees. `tools/js/seam_load.sh` runs the matrix with caps.

use bare_js::eval::*;
use bare_js::heap::Heap;
use bare_js::parse::{parse_script, Limits};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;
#[path = "common/exec.rs"]
mod exec;

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
    let sc = scale as u64;
    match name {
        "from_set" => {
            let n = 2_000 * sc;
            Case {
                src: format!("var s = new Set(); for (var i = 0; i < {n}; i++) s.add(i); var a = Array.from(s, function (x) {{ return x * 2; }}); a.reduce(function (p, c) {{ return p + c; }}, 0) + ',' + a.length"),
                expect: format!("{},{n}", n * (n - 1)),
            }
        }
        "from_map_entries" => {
            let n = 1_000 * sc;
            Case {
                src: format!("var m = new Map(); for (var i = 0; i < {n}; i++) m.set('k' + i, i); var o = Object.fromEntries(m); Object.keys(o).length + ',' + Object.values(o).reduce(function (p, c) {{ return p + c; }}, 0)"),
                expect: format!("{n},{}", n * (n - 1) / 2),
            }
        }
        "concat_flag" => {
            let n = 500 * sc;
            Case {
                src: format!("var o = {{ length: 3, 0: 1, 1: 2, 2: 3 }}; o[Symbol.isConcatSpreadable] = true; var t = 0; for (var i = 0; i < {n}; i++) t += [0].concat(o, [4]).length; t"),
                expect: (5 * n).to_string(),
            }
        }
        "iter_methods" => {
            let n = 2_000 * sc;
            let tri = n * (n - 1) / 2;
            Case {
                src: format!("var a = Array.from({{ length: {n} }}, function (_, i) {{ return i; }}); var s = 0; for (var [i, v] of a.entries()) s += i + v; for (var k of a.keys()) s += k; s += Array.from(a.values()).length; s"),
                expect: (3 * tri + n).to_string(),
            }
        }
        "forof_break" => {
            let n = 1_000 * sc;
            Case {
                src: format!("var arr = [1, 2, 3]; var c = 0; for (var i = 0; i < {n}; i++) {{ for (var x of arr) {{ c += x; break; }} }} c"),
                expect: n.to_string(),
            }
        }
        "string_hooks" => {
            let n = 1_000 * sc;
            Case {
                src: format!("var t = 0; for (var i = 0; i < {n}; i++) {{ t += 'a,b,c'.split(',').length; t += 'abc'.replace('b', 'xx').length; t += 'abc'.match(/b/).index; t += 'x-y-z'.replace(/-/g, '++').length; }} t"),
                expect: (15 * n).to_string(),
            }
        }
        "species_map" => {
            let n = 500 * sc;
            Case {
                src: format!("function inc(x) {{ return x + 1; }} function big(x) {{ return x > 2; }} var a = [1, 2, 3]; var t = 0; for (var i = 0; i < {n}; i++) t += a.map(inc).filter(big).length; t"),
                expect: (2 * n).to_string(),
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
    exec::set(&mut it);
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
        "{name:16} x{scale:<3} checksum=ok parse={parse_ms:8.1}ms setup={setup_ms:6.2}ms run={run_ms:9.1}ms cpu={cpu}ms steps={} ({:.1} Msteps/s) peak_rss={:.1}MiB peak_live={:.1}MiB allocs={allocs} alloc={:.1}MiB | held: envs={} funcs={} heap_objs={} heap_bytes={:.1}MiB | collect={gc_ms:.1}ms freed_objs={} -> envs={} funcs={}",
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
        die("usage: seam_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    // 16 MiB: a JS call level needs a few KiB of native stack and max_call_depth is 100.
    std::thread::Builder::new().stack_size(16 << 20).spawn(move || run_case(name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
