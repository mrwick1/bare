//! Load test for shared-realm multi-script execution. One case at one scale per process.
//!
//!   scripts_load CASE SCALE      CASE: many | few | cross | host | reject      SCALE: 1, 10, 100
//!
//! many: SCALE*200 small scripts, each calls a function of the one before. few: five big scripts of
//! SCALE*100 functions each. cross: two scripts that call each other in a loop of a third script. host: the host
//! calls a closure of the first script SCALE*20000 times with other scripts run in between. reject: a full
//! realm refuses SCALE*5000 scripts (count limit) and SCALE*1000 syntax errors, and must not grow.
//! Each case checks its result against the same computation in Rust. Prints time, CPU, peak RSS, allocations and
//! what the realm holds. `tools/js/scripts_load.sh` runs the matrix under caps.

use bare_js::eval::*;
use bare_js::heap::Heap;
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


fn run(it: &mut Interp, src: &str) -> String {
    match it.run_script(src) {
        Ok(v) => it.display(v),
        Err(e) => format!("ERR {e:?}"),
    }
}

fn expect(what: &str, got: String, want: String) {
    if got != want {
        die(format!("{what}: {got} != {want}"));
    }
}

fn exec(name: &str, scale: usize) {
    let mut limits = EvalLimits { max_steps: u64::MAX / 2, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let n_many = 200 * scale;
    let m_few = 100 * scale;
    if name == "many" {
        limits.max_scripts = n_many + 4;
    }
    if name == "reject" {
        limits.max_scripts = 64;
    }
    let mut it = Interp::realm(Heap::default(), limits).unwrap_or_else(|e| die(format!("realm: {e:?}")));
    let (a0, b0, cpu0) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed), cpu_ms());
    PEAK.store(LIVE.load(Relaxed), Relaxed);
    let t = Instant::now();
    let mut extra = String::new();
    match name {
        "many" => {
            run(&mut it, "var acc = 0;");
            for k in 1..=n_many {
                let s = format!("function f{k}(x) {{ return x + {k}; }} acc = f{k}(acc);");
                if run(&mut it, &s) != (k * (k + 1) / 2).to_string() {
                    die(format!("many: script {k}"));
                }
            }
            expect("many", run(&mut it, "acc + f1(0)"), (n_many * (n_many + 1) / 2 + 1).to_string());
        }
        "few" => {
            let mut want = 1u64;
            for s in 0..5 {
                let mut src = String::new();
                for i in 0..m_few {
                    src += &format!("function g{s}_{i}(x) {{ return (x * 3 + {i}) % 10007; }}\n");
                }
                if run(&mut it, &src) != "undefined" {
                    die("few: define".into());
                }
            }
            let mut driver = String::from("var x = 1;\n");
            for s in 0..5 {
                for i in 0..m_few {
                    driver += &format!("x = g{s}_{i}(x);\n");
                    want = (want * 3 + i as u64) % 10007;
                }
            }
            driver += "x";
            expect("few", run(&mut it, &driver), want.to_string());
        }
        "cross" => {
            run(&mut it, "function ping(n, x) { return n == 0 ? x : pong(n - 1, (x * 7 + 1) % 65521); }");
            run(&mut it, "function pong(n, x) { return n == 0 ? x : ping(n - 1, (x + 3) % 65521); }");
            let n = 2000 * scale;
            let mut x = 1u64;
            for _ in 0..n {
                for k in 0..10 {
                    x = if k % 2 == 0 { (x * 7 + 1) % 65521 } else { (x + 3) % 65521 };
                }
            }
            expect("cross", run(&mut it, &format!("var x = 1; for (var i = 0; i < {n}; i++) x = ping(10, x); x")), x.to_string());
        }
        "host" => {
            run(&mut it, "var c = 0; function inc(d) { c += d; return c; }");
            let f = it.run_script("inc").unwrap_or_else(|e| die(format!("{e:?}")));
            let n = 20000 * scale;
            let mut want = 0u64;
            for i in 0..n {
                let d = (i % 7) as u64;
                want += d;
                let r = it.call(f, Value::Undefined, &[Value::Number(d as f64)]).unwrap_or_else(|e| die(format!("call: {e:?}")));
                if it.display(r) != want.to_string() {
                    die("host: running total".into());
                }
                if i % 1000 == 0 {
                    expect("host mid", run(&mut it, &format!("var k{i} = c; k{i}")), want.to_string());
                }
            }
            expect("host", run(&mut it, "c"), want.to_string());
        }
        "reject" => {
            while it.stats().scripts < 64 {
                run(&mut it, "var keep = 1;");
            }
            let before = it.stats();
            for _ in 0..5000 * scale {
                expect("reject count", run(&mut it, "var z = 1;"), "ERR Limit(Scripts)".into());
            }
            let mut it2 = Interp::realm(Heap::default(), EvalLimits { max_steps: u64::MAX / 2, ..Default::default() }).unwrap();
            for i in 0..1000 * scale {
                if !run(&mut it2, &format!("var q{i} = ;")).starts_with("ERR Throw") {
                    die("reject syntax".into());
                }
            }
            let (s1, s2) = (before, it.stats());
            if (s1.scripts, s1.script_source, s1.script_nodes) != (s2.scripts, s2.script_source, s2.script_nodes) || it2.stats().scripts != 1 {
                die("reject: realm grew".into());
            }
            expect("reject keep", run(&mut it2, "typeof q0"), "undefined".into());
        }
        _ => die(format!("unknown case {name}")),
    }
    let run_ms = t.elapsed().as_secs_f64() * 1e3;
    let cpu = cpu_ms() - cpu0;
    let (allocs, abytes, peak_live) = (ALLOCS.load(Relaxed) - a0, BYTES.load(Relaxed) - b0, PEAK.load(Relaxed));
    let st = it.stats();
    let t3 = Instant::now();
    let freed = it.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
    let gc_ms = t3.elapsed().as_secs_f64() * 1e3;
    if name == "few" || name == "many" {
        extra = format!(" after-gc-check={}", run(&mut it, if name == "few" { "g0_0(1)" } else { "f1(1)" }));
    }
    println!(
        "{name:7} x{scale:<3} checksum=ok run={run_ms:9.1}ms cpu={cpu}ms peak_rss={:.1}MiB peak_live={:.1}MiB live_after={:.1}MiB allocs={allocs} alloc={:.1}MiB | scripts={} src={:.2}MiB nodes={} funcs={} | collect={gc_ms:.1}ms freed_objs={}{extra}",
        status_kib("VmHWM:") as f64 / 1024.0,
        peak_live as f64 / 1048576.0,
        LIVE.load(Relaxed) as f64 / 1048576.0,
        abytes as f64 / 1048576.0,
        st.scripts,
        st.script_source as f64 / 1048576.0,
        st.script_nodes,
        st.functions,
        freed.objects,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: scripts_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    // 16 MiB: a JS call level needs a few KiB of native stack and the parser recurses.
    std::thread::Builder::new().stack_size(16 << 20).spawn(move || exec(&name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
