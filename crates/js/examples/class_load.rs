//! Load test for class execution. One case at one scale per process, so that the peak memory belongs to that run.
//!
//!   class_load CASE SCALE     CASE: construct | derived | private | accessors | super | churn    SCALE: 1, 10, 100
//!
//! Each case builds a small program, runs it, and checks the result against the same computation done in Rust
//! (the checksum). A wrong checksum exits with status 1 and prints no timing. It prints parse, setup and run
//! time (wall), CPU time, peak resident memory, allocations of the run, steps, the environments and functions the
//! evaluator holds, and what an explicit `collect` frees. `tools/js/class_load.sh` runs the matrix with caps.

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

struct Case {
    src: String,
    expect: String,
}

fn case(name: &str, scale: usize) -> Case {
    let n = 20_000 * scale as u64;
    let (src, expect) = match name {
        // Base class with a constructor and a public field.
        "construct" => {
            let s: u64 = (0..n).map(|i| i + 1 + i % 7).sum();
            (format!("class P {{ k = 1; constructor(a) {{ this.a = a; }} }} var s = 0; for (var i = 0; i < {n}; i++) {{ var p = new P(i); s += p.a + p.k + i % 7; }} s"), s)
        }
        // Three-level chain: explicit super(), implicit constructor, fields after super.
        "derived" => {
            let s: u64 = (0..n).map(|i| 3 * i + 1 + 2).sum();
            (format!("class A {{ constructor(a) {{ this.a = a; }} }} class B extends A {{ y = 1; }} class C extends B {{ constructor(a) {{ super(a); this.c = 2; }} }} var s = 0; for (var i = 0; i < {n}; i++) {{ var o = new C(i); s += o.a * 3 + o.y + o.c; }} s"), s)
        }
        // Private field and private method on every instance; brand checks on each access.
        "private" => {
            let s: u64 = (0..n).map(|i| (i % 10) * 2 + 1).sum();
            (format!("class Q {{ #v = 0; #bump() {{ return this.#v + 1; }} set(x) {{ this.#v = x; }} get() {{ return this.#bump(); }} }} var q = new Q(); var s = 0; for (var i = 0; i < {n}; i++) {{ q.set(i % 10 * 2); s += q.get(); }} s"), s)
        }
        // Public and private accessors, static method.
        "accessors" => {
            let s: u64 = (0..n).map(|i| i % 100 + 5).sum();
            (format!("class R {{ #x = 0; get #p() {{ return this.#x; }} set #p(v) {{ this.#x = v; }} get x() {{ return this.#p; }} set x(v) {{ this.#p = v; }} static id(v) {{ return v; }} }} var r = new R(); var s = 0; for (var i = 0; i < {n}; i++) {{ r.x = R.id(i % 100); s += r.x + 5; }} s"), s)
        }
        // super.m() and super property reads through two levels.
        "super" => {
            let s: u64 = (0..n).map(|i| i % 50 + 3).sum();
            (format!("class A {{ m(v) {{ return v + 1; }} }} class B extends A {{ m(v) {{ return super.m(v) + 1; }} }} class C extends B {{ m(v) {{ return super.m(v) + 1; }} }} var c = new C(); var s = 0; for (var i = 0; i < {n}; i++) s += c.m(i % 50); s"), s)
        }
        // Many dead instances with private slots: the slots must be freed by collect.
        "churn" => {
            let s: u64 = (0..n).map(|i| i % 13).sum();
            (format!("class Z {{ #a; #b = 2; constructor(a) {{ this.#a = a; }} get a() {{ return this.#a; }} }} var s = 0; for (var i = 0; i < {n}; i++) {{ s += new Z(i % 13).a; }} s"), s)
        }
        _ => die(format!("unknown case {name}")),
    };
    Case { src, expect: expect.to_string() }
}

fn run_case(name: String, scale: usize) {
    let c = case(&name, scale);
    let t0 = Instant::now();
    let prog = parse_script(&c.src, Limits::default()).unwrap_or_else(|e| die(format!("parse: {e}")));
    let parse_ms = t0.elapsed().as_secs_f64() * 1e3;
    let t1 = Instant::now();
    let limits = EvalLimits { max_steps: u64::MAX / 2, max_call_depth: 100, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let mut it = Interp::new(&prog, Heap::default(), limits).unwrap_or_else(|e| die(format!("setup: {e:?}")));
    it.set_private_slot_limit(1 << 23); // churn x100 holds 4M dead slots until the explicit collect; the default 2M stops it with Limit(PrivateSlots)
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
    let slots0 = it.private_slots();
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

    println!("slots_before_collect={} slots_after={} classes={}", slots0, it.private_slots(), it.live_classes());
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: class_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    // 16 MiB: a JS call level needs a few KiB of native stack and max_call_depth is 100.
    std::thread::Builder::new().stack_size(16 << 20).spawn(move || run_case(name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
