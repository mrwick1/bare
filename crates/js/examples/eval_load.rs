//! Load test for the evaluator. One case at one scale per process, so that the peak memory belongs to that run.
//!
//!   eval_load CASE SCALE      CASE: loop | calls | fib | closures | objects | webpack | throws | concat    SCALE: 1, 10, 100
//!
//! Each case builds a small program, runs it, and checks the result against the same computation done in Rust
//! (the checksum). A wrong checksum exits with status 1 and prints no timing. It prints parse, setup and run
//! time (wall), CPU time, peak resident memory, allocations of the run, steps, the environments and functions the
//! evaluator holds, and what an explicit `collect` frees. `tools/js/eval_load.sh` runs the matrix with caps.

use bare_js::eval::*;
use bare_js::heap::{Get, Heap};
use bare_js::parse::{parse_script, Limits};
use bare_js::value::Value;
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

/// (program source, expected result text, optional follow-up: global function to call after `collect`, expected text)
struct Case {
    src: String,
    expect: String,
    after_gc: Option<(&'static str, String)>,
}

fn case(name: &str, scale: usize) -> Case {
    match name {
        "loop" => {
            let n = 200_000 * scale as u64;
            let mut s = 0u64;
            for i in 0..n {
                s = (s + i * 3) % 1000003;
            }
            Case { src: format!("var s = 0; for (var i = 0; i < {n}; i++) {{ s = (s + i * 3) % 1000003; }} s"), expect: s.to_string(), after_gc: None }
        }
        "calls" => {
            let n = 100_000 * scale as u64;
            let mut s = 1u64;
            for i in 0..n {
                s = (s * 31 + i) % 65521;
            }
            Case { src: format!("function f(a, b) {{ return (a * 31 + b) % 65521; }} var s = 1; for (var i = 0; i < {n}; i++) s = f(s, i); s"), expect: s.to_string(), after_gc: None }
        }
        "fib" => {
            let k = match scale { 1 => 18, 10 => 23, _ => 28 };
            fn fib(n: u64) -> u64 { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }
            Case { src: format!("function fib(n) {{ return n < 2 ? n : fib(n - 1) + fib(n - 2); }} fib({k})"), expect: fib(k).to_string(), after_gc: None }
        }
        "closures" => {
            let n = 5_000 * scale as u64;
            let mut st: Vec<u64> = (0..n).collect();
            let mut s = 0u64;
            for c in st.iter_mut() {
                *c = (*c * 7 + 1) % 10007;
                s += *c;
            }
            let next0 = (st[0] * 7 + 1) % 10007;
            Case {
                src: format!(
                    "function mk(n) {{ var c = n; return function() {{ c = (c * 7 + 1) % 10007; return c; }}; }}
                     var fs = []; for (var i = 0; i < {n}; i++) fs.push(mk(i));
                     var s = 0; for (var j = 0; j < {n}; j++) s += fs[j]();
                     function first() {{ return fs[0](); }} s"
                ),
                expect: s.to_string(),
                after_gc: Some(("first", next0.to_string())),
            }
        }
        "objects" => {
            let n = 20_000 * scale as u64;
            let mut slots = vec![0u64; 997];
            for i in 0..n {
                slots[(i % 997) as usize] = i;
            }
            let s1: u64 = slots.iter().sum();
            let s2: u64 = (0..n).map(|i| i % 13).sum();
            Case {
                src: format!(
                    "var o = {{}}; for (var i = 0; i < {n}; i++) o['k' + (i % 997)] = i;
                     var s = 0; for (var k in o) s += o[k];
                     var a = []; for (var j = 0; j < {n}; j++) a.push(j % 13);
                     var t = 0; a.forEach(function(v) {{ t += v; }}); s + ',' + t"
                ),
                expect: format!("{s1},{s2}"),
                after_gc: None,
            }
        }
        "webpack" => {
            let m = 50 * scale;
            let mut v = vec![0u64; m];
            v[0] = 1;
            for i in 1..m {
                v[i] = (v[i >> 1] + v[i >> 2] + i as u64) % 1000003;
            }
            let total: u64 = v.iter().sum();
            let mut src = String::from("var modules = {\n0: function(module, exports, require) { exports.v = 1; },\n");
            for i in 1..m {
                src += &format!("{i}: function(module, exports, require) {{ exports.v = (require({}).v + require({}).v + {i}) % 1000003; }},\n", i >> 1, i >> 2);
            }
            src += &format!(
                "}};
                 var cache = {{}};
                 function __webpack_require__(id) {{
                   if (cache[id]) return cache[id].exports;
                   var module = cache[id] = {{ exports: {{}} }};
                   modules[id].call(module.exports, module, module.exports, __webpack_require__);
                   return module.exports;
                 }}
                 var self = this, loaded = 0;
                 var queue = (self.chunks = self.chunks || []);
                 function onChunk(parent, data) {{ for (var i = 0; i < data[0].length; i++) loaded += data[0][i]; if (parent) parent(data); }}
                 queue.forEach(onChunk.bind(null, 0));
                 queue.push = onChunk.bind(null, queue.push.bind(queue));
                 for (var c = 0; c < {m}; c++) (self.chunks = self.chunks || []).push([[c % 5, 1], {{}}]);
                 var sum = 0; for (var id = 0; id < {m}; id++) sum += __webpack_require__(id).v;
                 sum + ',' + loaded + ',' + self.chunks.length"
            );
            let loaded: u64 = (0..m as u64).map(|c| c % 5 + 1).sum();
            Case { src, expect: format!("{total},{loaded},{m}"), after_gc: None }
        }
        "throws" => {
            let n = 20_000 * scale as u64;
            let mut c = 0u64;
            for i in 0..n {
                if i % 3 == 0 {
                    c += format!("e{i}").len() as u64;
                } else {
                    c += 1;
                }
                c += 1;
            }
            Case {
                src: format!("var c = 0; for (var i = 0; i < {n}; i++) {{ try {{ if (i % 3 == 0) throw new Error('e' + i); c += 1; }} catch (e) {{ c += e.message.length; }} finally {{ c++; }} }} c"),
                expect: c.to_string(),
                after_gc: None,
            }
        }
        "concat" => {
            // Each `+=` interns a new string and nothing is collected during the run: quadratic memory. Small on purpose.
            let n = 100 * scale;
            Case { src: format!("var s = ''; for (var i = 0; i < {n}; i++) s += 'ab'; s.length"), expect: (2 * n).to_string(), after_gc: None }
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
    if let Some((f, want)) = &c.after_gc {
        let k = it.heap.key_from_str(f).unwrap();
        let Get::Value(fv) = it.heap.get(it.global_object(), k).unwrap() else { die("accessor".into()) };
        let v = it.call(fv, Value::Undefined, &[]).unwrap_or_else(|e| die(format!("call after gc: {e:?}")));
        if it.display(v) != *want {
            die(format!("after collect {} != {want}", it.display(v)));
        }
    }
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
        die("usage: eval_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    // 16 MiB: a JS call level needs a few KiB of native stack and max_call_depth is 100.
    std::thread::Builder::new().stack_size(16 << 20).spawn(move || run_case(name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
