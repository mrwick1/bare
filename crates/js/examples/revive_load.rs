//! Load test for `JSON.parse` with a callable reviver. One case at one scale per process, so the peak memory belongs to
//! that run.
//!
//!   revive_load CASE SCALE    CASE: shallow | deep | holes | gc | cycle     SCALE: 1, 10, 100
//!
//! The JSON text and the expected result are built in Rust (apart from the engine). The text goes in as the global
//! `INPUT`. The reviver runs in JavaScript. A wrong result exits with status 1. `cycle` is hostile: the reviver
//! links a later element back to the root, so the walk never ends by itself. It must end in a `Json` limit.
//! `tools/js/revive_load.sh` runs the matrix.

use bare_js::eval::*;
use bare_js::heap::{Get, Heap, HeapLimits};
use bare_js::parse::{parse_script, Limits};
use bare_js::value::Value;
use std::time::Instant;

fn proc_kib(field: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status").ok().and_then(|s| s.lines().find(|l| l.starts_with(field)).and_then(|l| l.split_whitespace().nth(1)?.parse().ok())).unwrap_or(0)
}

fn cpu_ms() -> u64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let rest = s.rsplit(')').next().unwrap_or("");
    let f: Vec<&str> = rest.split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
    (t(11) + t(12)) * 10
}

struct Case {
    /// The JSON text.
    input: String,
    /// Source of `run(text)`.
    body: &'static str,
    want: Option<String>,
    reps: usize,
}

fn build(case: &str, scale: usize) -> Case {
    match case {
        // objects with numbers to change (+1), a key to delete, nested arrays and strings
        "shallow" => {
            let n = 1000 * scale;
            let input = format!("[{}]", (0..n).map(|i| format!("{{\"id\":{i},\"name\":\"n{i}\",\"skip\":1,\"tags\":[\"a\",{}],\"o\":{{\"k\":null}}}}", i % 10)).collect::<Vec<_>>().join(","));
            let want = format!("[{}]", (0..n).map(|i| format!("{{\"id\":{},\"name\":\"n{i}\",\"tags\":[\"a\",{}],\"o\":{{\"k\":null}}}}", i + 1, i % 10 + 1)).collect::<Vec<_>>().join(","));
            Case { input, body: "return JSON.stringify(JSON.parse(t, function (k, v) { if (k === 'skip') return undefined; return typeof v === 'number' ? v + 1 : v; }));", want: Some(want), reps: 1 }
        }
        // nesting well above the 512 evaluator frames: the walk is iterative
        "deep" => {
            let d = 10 * scale;
            let input = format!("{}0{}", "[".repeat(d), "]".repeat(d));
            let want = format!("{}1{}", "[".repeat(d), "]".repeat(d));
            Case { input, body: "return JSON.stringify(JSON.parse(t, function (k, v) { return typeof v === 'number' ? v + 1 : v; }));", want: Some(want), reps: 20 }
        }
        // odd elements are deleted: holes, serialized as null
        "holes" => {
            let n = 4000 * scale;
            let input = format!("[{}]", (0..n).map(|i| i.to_string()).collect::<Vec<_>>().join(","));
            let want = format!("[{}]", (0..n).map(|i| if i % 2 == 1 { "null".to_string() } else { (i * 2).to_string() }).collect::<Vec<_>>().join(","));
            Case { input, body: "return JSON.stringify(JSON.parse(t, function (k, v) { return typeof v === 'number' ? (v % 2 ? undefined : v * 2) : v; }));", want: Some(want), reps: 1 }
        }
        "gc" => Case { input: "{\"a\":[1,{\"b\":\"x\"}],\"c\":{\"d\":[2,3]}}".into(), body: "return JSON.parse(t, function (k, v) { return v; }).a.length;", want: None, reps: 5 * scale },
        "cycle" => Case { input: "[[0],[0]]".into(), body: "return JSON.parse(t, function (k, v) { if (k === '0' && Array.isArray(v)) this[1] = this; return v; });", want: None, reps: 1 },
        _ => {
            eprintln!("unknown case {case}");
            std::process::exit(2);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (case, scale): (String, usize) = (args[1].clone(), args[2].parse().unwrap());
    std::thread::Builder::new().stack_size(64 << 20).spawn(move || run(&case, scale)).unwrap().join().unwrap();
}

fn global(it: &mut Interp, name: &str) -> Value {
    let k = it.heap.key_from_str(name).unwrap();
    match it.heap.get(it.global_object(), k) {
        Ok(Get::Value(v)) => v,
        _ => panic!("global {name}"),
    }
}

fn run(case: &str, scale: usize) {
    let c = build(case, scale);
    let heap = Heap::new(HeapLimits { max_bytes: 700 << 20, ..Default::default() }).unwrap();
    let src = format!("function run(t) {{ {} }}", c.body);
    let prog = parse_script(&src, Limits::default()).expect("parse");
    let mut it = Interp::new(&prog, heap, EvalLimits { max_steps: 20_000_000_000, ..Default::default() }).expect("setup");
    it.run().expect("setup run");
    let text = it.heap.intern_vec(c.input.encode_utf16().collect()).expect("input");
    let f = global(&mut it, "run");
    let (t1, cpu1) = (Instant::now(), cpu_ms());
    let (mut freed, mut note, mut ok) = (0usize, String::new(), true);
    let live0 = it.heap.stats().live_objects;
    for _ in 0..c.reps {
        match it.call(f, Value::Undefined, &[Value::String(text)]) {
            Ok(v) if case == "gc" => {
                ok = ok && matches!(v, Value::Number(n) if n == 2.0);
                freed += it.collect(&[Value::String(text)]).unwrap().objects;
            }
            Ok(Value::String(s)) => {
                let got = it.heap.str_units(s).unwrap();
                ok = ok && c.want.as_deref().map(|w| w.encode_utf16().collect::<Vec<u16>>()).as_deref() == Some(got);
                note = format!(" out_units={}", got.len());
            }
            Err(Error::Limit(LimitKind::Json(k))) if case == "cycle" => note = format!(" limit={k:?}"),
            r => {
                eprintln!("FAILED call: {r:?}");
                std::process::exit(1);
            }
        }
    }
    if case == "cycle" {
        ok = note.contains("limit=");
    }
    if case == "gc" {
        let live1 = it.heap.stats().live_objects;
        ok = ok && live1 <= live0 + 2;
        note = format!(" live_before={live0} live_after={live1}");
    }
    let ms = t1.elapsed().as_secs_f64() * 1e3;
    if !ok {
        eprintln!("WRONG RESULT case={case} scale={scale}{note}");
        std::process::exit(1);
    }
    let hs = it.heap.stats();
    println!(
        "case={case} scale={scale} reps={} revive_ms={ms:.1} revive_cpu_ms={} rss_peak_mib={:.1} steps={} obj_allocs={} str_allocs={} heap_mib={:.1} freed={freed}{note} OK",
        c.reps,
        cpu_ms() - cpu1,
        proc_kib("VmHWM:") as f64 / 1024.0,
        it.stats().steps,
        hs.total_object_allocs,
        hs.total_string_allocs,
        hs.bytes as f64 / 1048576.0,
    );
}
