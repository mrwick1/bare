//! Parse-only load rows for the pre-parser (S2.2). One mode and one case per process:
//!
//!   parse_load MODE CASE SCALE      MODE: full | lazy | fns      CASE: stmts | chain | bodies | file PATH
//!
//! - `full`: `parse_script`, one tree of the whole text (the old parser's work, as ported).
//! - `lazy`: `parse_script_lazy`, the pre-parse: the top-level tree and the facts of every function.
//! - `fns`: `lazy`, then `parse_function` of every function (the work of compiling each function once).
//!
//! The sources of `stmts`, `chain` and `bodies` are the ones of `crates/js/examples/parse_load.rs` (same text for the same scale), so the
//! rows compare with the old parser. Instructions come from `perf stat` around the whole process (`tools/js/instr.sh`); this
//! program prints the allocations, the peak of live heap bytes during the parse and the VmHWM. A parse that fails exits with 1.

use bare_js2::facts::{self, Facts};
use bare_js2::parse::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            ALLOCS.fetch_add(1, Relaxed);
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

fn stmts_src(n: usize) -> String {
    let mut s = String::with_capacity(n * 16);
    for i in 0..n {
        s.push_str(&format!("var v{i} = {i} + 1;\n"));
    }
    s
}

fn chain_src(t: usize) -> String {
    let mut s = String::from("t = v0");
    for i in 1..t {
        s.push_str(&format!(" + v{i}"));
    }
    s.push_str(";\no");
    s.push_str(&".p".repeat(t));
    s.push_str(";\nf");
    s.push_str(&"()".repeat(t));
    s.push_str(";\n");
    s
}

const UNIT: &str = "\
function f_I(a, b = 1) { var s = 0; for (var i = 0; i < a; i++) { if (i % 2) s += i; else s -= b; } return s + I; }
class C_I extends B { x = 1; static y = I; #z = 2; constructor(p) { super(); this.p = p; } get g() { return this.#z; } m(q) { return q ? q * 2 : -q; } }
";

fn bodies_src(n: usize) -> String {
    let mut s = String::with_capacity(n * UNIT.len());
    for i in 0..n {
        s.push_str(&UNIT.replace('I', &i.to_string()));
    }
    s
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (Some(mode), Some(case)) = (a.get(1), a.get(2)) else { die("usage: parse_load MODE CASE SCALE [PATH]".into()) };
    let scale: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or_else(|| die("scale".into()));
    let src = match case.as_str() {
        "stmts" => stmts_src(2000 * scale),
        "chain" => chain_src(10_000 * scale),
        "bodies" => bodies_src(200 * scale),
        "file" => std::fs::read_to_string(a.get(4).unwrap_or_else(|| die("path".into()))).unwrap_or_else(|e| die(e.to_string())),
        _ => die(format!("unknown case {case}")),
    };
    let limits = Limits { max_list: u32::MAX, ..Limits::default() };
    let live0 = LIVE.load(Relaxed);
    PEAK.store(live0, Relaxed);
    let a0 = ALLOCS.load(Relaxed);
    let mut facts = Facts::new();
    let (nodes, skipped);
    match mode.as_str() {
        "full" => {
            let p = parse_script(&src, limits).unwrap_or_else(|e| die(format!("{e}")));
            (nodes, skipped) = (p.stats().nodes(), 0);
        }
        "lazy" | "fns" => {
            let p = parse_script_lazy(&src, limits, &mut facts).unwrap_or_else(|e| die(format!("{e}")));
            let mut n = p.stats().nodes();
            skipped = p.stats().skipped;
            drop(p);
            if mode == "fns" {
                let before = (facts.preparsed, facts.fns.len());
                // The tree of one function lives until the next one starts, as in the compiler.
                for id in 0..facts.fns.len() as u32 {
                    if facts.get(id).flags & facts::INLINE == 0 {
                        let (t, _) = parse_function(&src, &mut facts, id, limits).unwrap_or_else(|e| die(format!("function {id}: {e}")));
                        n += t.stats().nodes();
                    }
                }
                if (facts.preparsed, facts.fns.len()) != before {
                    die("a full parse of a function pre-parsed again".into());
                }
            }
            nodes = n;
        }
        _ => die(format!("unknown mode {mode}")),
    }
    println!(
        "mode={mode} case={case} scale={scale} src_bytes={} nodes={nodes} skipped={skipped} fns={} preparsed={} caps={} allocs={} peak_live_bytes={} rss_kib={}",
        src.len(), facts.fns.len(), facts.preparsed, facts.caps.len(), ALLOCS.load(Relaxed) - a0, PEAK.load(Relaxed) - live0, status_kib("VmHWM:")
    );
}
