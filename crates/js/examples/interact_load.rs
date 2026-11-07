//! Load test for classes together with async methods. One case at one scale per process, so the peak
//! memory belongs to that run.
//!
//!   interact_load CASE SCALE    CASE: asyncpriv | fanpriv | supercalls | suspended | churn    SCALE: 1, 10, 100
//!
//! Each case builds a small program, runs it, drains the microtask queue and checks the result against the same
//! computation done in Rust (the checksum). `suspended` runs `collect` while every activation (and its class
//! instance) is still suspended. A wrong checksum exits with status 1. It prints wall time per phase, CPU time and
//! peak resident memory of the process (from /proc), the metadata that is still retained after the work
//! (private slots, classes, activations, tracked functions) and what an explicit `collect` frees.
//! `tools/js/interact_load.sh` runs the matrix with caps.

use bare_js::eval::*;
use bare_js::heap::Heap;
use bare_js::parse::{parse_script, Limits};
use bare_js::value::Value;
use std::time::Instant;
#[path = "common/exec.rs"]
mod exec;

fn die(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1)
}

fn proc_kib(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(key)).and_then(|r| r.split_whitespace().next()).and_then(|n| n.parse().ok()).unwrap_or(0)
}

fn cpu_ms() -> f64 {
    // utime + stime of the whole process, in clock ticks of 10 ms (Linux, USER_HZ 100)
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let f: Vec<&str> = s.rsplit_once(')').map_or("", |x| x.1).split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|n| n.parse::<f64>().ok()).unwrap_or(0.0);
    (t(11) + t(12)) * 10.0
}

/// (source, expected checksum). The last expression is a reader. Cases with one activation per item use n/2 items,
/// so the default cap of 65536 activations holds at 100x.
fn program(case: &str, n: usize) -> (String, f64) {
    let reader = "(function () { return out[0]; })";
    let m = n / 2;
    let tri = |k: usize| (k * (k - 1) / 2) as f64;
    match case {
        "asyncpriv" => (format!("var out = [0]; class Acc {{ #t = 0; async add(k) {{ this.#t += await k; out[0] = this.#t; }} }} async function f() {{ var a = new Acc(); for (var i = 0; i < {n}; i++) await a.add(i); }} f();{reader}"), tri(n)),
        "fanpriv" => (format!("var out = [0]; class W {{ #v; constructor(v) {{ this.#v = v; }} async go() {{ var a = await this.#v; this.#v = a + (await 1); out[0] += this.#v; }} }} for (var i = 0; i < {m}; i++) new W(i).go();{reader}"), tri(m) + m as f64),
        "supercalls" => (format!("var out = [0]; class A {{ v(x) {{ return x + 1; }} }} class B extends A {{ async v(x) {{ return super.v(await x); }} }} async function f() {{ var b = new B(); for (var i = 0; i < {n}; i++) out[0] += await b.v(i); }} f();{reader}"), tri(n) + n as f64),
        "suspended" => (format!("var out = [0]; var res; var gate = new Promise(function (r) {{ res = r; }}); class S {{ #v = [2]; async run() {{ await gate; out[0] += this.#v[0]; }} }} for (var i = 0; i < {m}; i++) new S().run(); res(0);{reader}"), 2.0 * m as f64),
        "churn" => (format!("var out = [0]; class N {{ #peer; #k; constructor(k) {{ this.#k = k; this.#peer = this; }} async m() {{ await 0; out[0] += this.#peer.#k; }} }} for (var i = 0; i < {m}; i++) new N(i).m();{reader}"), tri(m)),
        _ => die(format!("unknown case {case}")),
    }
}

fn run_case(case: String, scale: usize) {
    let n = 1000 * scale;
    let (src, want) = program(&case, n);
    let t0 = Instant::now();
    let prog = parse_script(&src, Limits::default()).unwrap_or_else(|e| die(format!("parse {e}")));
    let t_parse = t0.elapsed();
    let t1 = Instant::now();
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 2_000_000_000, ..Default::default() }).unwrap_or_else(|e| die(format!("setup {e:?}")));
    exec::set(&mut it);
    let t_setup = t1.elapsed();
    let t2 = Instant::now();
    let reader = it.run().unwrap_or_else(|e| die(format!("run {e:?}")));
    let t_run = t2.elapsed();
    let (queued, act, tracked) = (it.pending_jobs(), it.async_stats(), it.stats());
    let (slots_mid, classes_mid) = (it.private_slots(), it.live_classes());
    let t_gc = Instant::now();
    let mid = it.collect(&[reader]).unwrap_or_else(|e| die(format!("mid collect {e:?}")));
    let t_gc = t_gc.elapsed();
    let (slots_after_mid, act_after_mid) = (it.private_slots(), it.async_stats().activations);
    let t3 = Instant::now();
    let jobs = it.drain_microtasks().unwrap_or_else(|e| die(format!("drain {e:?}")));
    let t_drain = t3.elapsed();
    let got = match it.call(reader, Value::Undefined, &[]) {
        Ok(Value::Number(x)) => x,
        other => die(format!("reader {other:?}")),
    };
    if got != want {
        die(format!("{case} x{scale}: checksum {got} != {want}"));
    }
    let (slots_before, before) = (it.private_slots(), it.heap.stats());
    let t4 = Instant::now();
    let freed = it.collect(&[reader]).unwrap_or_else(|e| die(format!("collect {e:?}")));
    let t_collect = t4.elapsed();
    let after = it.heap.stats();
    let a = it.async_stats();
    let cpu = cpu_ms();
    println!(
        "{case:10} x{scale:<3} n={n:<7} jobs={jobs:<7} queued={queued:<7} run={:>7.1}ms drain={:>7.1}ms collect={:>6.1}ms | cpu(whole process)={cpu:>7.1}ms peakRSS={}MiB | suspended_before={} slots_before_midgc={slots_mid} classes={classes_mid} mid_gc={:.1}ms mid_freed={} slots_after_midgc={slots_after_mid} suspended_after_midgc={act_after_mid} | after_drain: slots={slots_before} live_objs={} -> after_collect: freed={} live_objs={} slots={} classes={} suspended={} frames={} aslots={} functions={} steps={} setup={:.1}ms parse={:.1}ms",
        t_run.as_secs_f64() * 1e3,
        t_drain.as_secs_f64() * 1e3,
        t_collect.as_secs_f64() * 1e3,
        proc_kib("VmHWM:") / 1024,
        act.activations,
        t_gc.as_secs_f64() * 1e3,
        mid.objects,
        before.live_objects,
        freed.objects,
        after.live_objects,
        it.private_slots(),
        it.live_classes(),
        a.activations,
        a.frames,
        a.slots,
        it.stats().functions,
        it.stats().steps,
        t_setup.as_secs_f64() * 1e3,
        t_parse.as_secs_f64() * 1e3,
    );
    let _ = tracked;
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: interact_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    // release 8 MiB, debug 64 MiB (the large-thread stack requirement)
    let stack = if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 };
    std::thread::Builder::new().stack_size(stack).spawn(move || run_case(name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
