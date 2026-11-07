//! Load test for `JSON.stringify`. One case at one scale per process, so the peak memory belongs to that run.
//!
//!   stringify_load CASE SCALE    CASE: shallow | deep | callback | escape | gc | dag     SCALE: 1, 10, 100
//!
//! Each case builds data in JavaScript (timed apart), then times one or more `JSON.stringify` calls from the host and
//! compares the full result with the same string built in Rust (the checksum). A wrong result exits with status 1.
//! `dag` is hostile: a shared acyclic graph that doubles per level. It must end in a distinct `Stringify` limit.
//! It prints build and stringify wall time, CPU time, peak resident memory, steps, heap allocations and what `collect` frees.
//! `tools/js/stringify_load.sh` runs the matrix.

use bare_js::eval::*;
use bare_js::heap::{Get, Heap, HeapLimits};
use bare_js::parse::{parse_script, Limits};
use bare_js::value::Value;
use std::time::Instant;
#[path = "common/exec.rs"]
mod exec;

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

/// Naive `QuoteJSONString` on UTF-16 units, written apart from the engine code.
fn quote(u: &[u16]) -> Vec<u16> {
    let mut o = vec![b'"' as u16];
    let mut i = 0;
    while i < u.len() {
        let c = u[i];
        let pair = (0xD800..0xDC00).contains(&c) && i + 1 < u.len() && (0xDC00..0xE000).contains(&u[i + 1]);
        match c {
            _ if pair => {
                o.push(c);
                o.push(u[i + 1]);
                i += 1;
            }
            0x22 => o.extend("\\\"".encode_utf16()),
            0x5c => o.extend("\\\\".encode_utf16()),
            0x0a => o.extend("\\n".encode_utf16()),
            0..=0x1f | 0xD800..=0xDFFF => o.extend(format!("\\u{c:04x}").encode_utf16()),
            _ => o.push(c),
        }
        i += 1;
    }
    o.push(b'"' as u16);
    o
}

struct Case {
    setup: String,
    /// Source of `ser()`: the function whose result is compared.
    ser: String,
    input: Vec<u16>,
    want: Option<Vec<u16>>,
    reps: usize,
}

fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn build(case: &str, scale: usize) -> Case {
    let mk = |setup: &str, ser: &str, want: Option<String>, reps| Case { setup: setup.into(), ser: ser.into(), input: Vec::new(), want: want.map(|w| u(&w)), reps };
    match case {
        "shallow" => {
            let n = 1000 * scale;
            let want = format!("[{}]", (0..n).map(|i| format!("{{\"id\":{i},\"name\":\"n{i}\",\"tags\":[\"a\",\"b\"],\"ok\":true,\"v\":{}.5,\"o\":{{\"k\":null}}}}", i % 10)).collect::<Vec<_>>().join(","));
            mk(&format!("var data = []; for (var i = 0; i < {n}; i++) data.push({{ id: i, name: 'n' + i, tags: ['a', 'b'], ok: true, v: (i % 10) + 0.5, skip: undefined, o: {{ k: null }} }});"), "return JSON.stringify(data);", Some(want), 1)
        }
        "deep" => {
            let d = 10 * scale;
            let reps = 20;
            let mut want = String::new();
            for k in 0..d {
                want += &format!("{{\n{}\"n\": ", " ".repeat(k + 1));
            }
            want += "[]";
            for k in (0..d).rev() {
                want += &format!("\n{}}}", " ".repeat(k));
            }
            mk(&format!("var data = []; for (var i = 0; i < {d}; i++) data = {{ n: data }};"), "return JSON.stringify(data, null, 1);", Some(want), reps)
        }
        "callback" => {
            let n = 500 * scale;
            let want = format!("[{}]", (0..n).map(|i| format!("{{\"id\":{},\"k\":\"{i}\"}}", i + 1)).collect::<Vec<_>>().join(","));
            mk(&format!("var P = {{ toJSON: function (k) {{ return {{ id: this.id, k: k }}; }} }}; var data = []; for (var i = 0; i < {n}; i++) {{ var o = Object.create(P); o.id = i; data.push(o); }}"), "return JSON.stringify(data, function (k, v) { return typeof v === 'number' ? v + 1 : v; });", Some(want), 1)
        }
        "escape" => {
            let n = 20000 * scale;
            let unit: Vec<u16> = [u("a\"b\n\\"), vec![1, 0xD83D, 0xDE00, 0xD800, 0xE9, 0xDC00, b'z' as u16]].concat();
            let input: Vec<u16> = unit.iter().cycle().take(n).copied().collect();
            let want = quote(&input);
            Case { setup: String::new(), ser: "return JSON.stringify(INPUT);".into(), input, want: Some(want), reps: 1 }
        }
        "gc" => mk("function round(k) { var o = { k: k, a: [1, 2, { b: 'x' + k }], toJSON: undefined }; var s = JSON.stringify(o, function (key, v) { return v; }, 2); return s.length; }", "", None, 5 * scale),
        "dag" => mk("var a = []; for (var i = 0; i < 60; i++) a = [a, a];", "return JSON.stringify(a);", None, 1),
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
    let src = format!("{} function ser() {{ {} }}", c.setup, c.ser);
    let prog = parse_script(&src, Limits::default()).expect("parse");
    let mut it = Interp::new(&prog, heap, EvalLimits { max_steps: 20_000_000_000, ..Default::default() }).expect("setup");
    exec::set(&mut it);
    if !c.input.is_empty() {
        let s = it.heap.intern_vec(c.input.clone()).expect("input");
        let k = it.heap.key_from_str("INPUT").unwrap();
        let g = it.global_object();
        it.heap.set(g, k, Value::String(s)).unwrap();
    }
    let (t0, cpu0) = (Instant::now(), cpu_ms());
    it.run().expect("build run");
    let build_ms = t0.elapsed().as_secs_f64() * 1e3;
    let steps_build = it.stats().steps;
    let (t1, cpu1) = (Instant::now(), cpu_ms());
    let (mut freed, mut peak_live, mut note) = (0usize, 0usize, String::new());
    let mut ok = true;
    if case == "gc" {
        let f = global(&mut it, "round");
        let live0 = it.heap.stats().live_objects;
        let mut total = 0f64;
        for r in 0..c.reps {
            let Ok(Value::Number(n)) = it.call(f, Value::Undefined, &[Value::Number(r as f64)]) else { panic!("round") };
            total += n;
            peak_live = peak_live.max(it.heap.stats().live_objects);
            freed += it.collect(&[]).unwrap().objects;
        }
        let live1 = it.heap.stats().live_objects;
        ok = live1 <= live0 + 2 && total > 0.0;
        note = format!(" live_before={live0} live_after={live1}");
    } else {
        let f = global(&mut it, "ser");
        for _ in 0..c.reps {
            match it.call(f, Value::Undefined, &[]) {
                Ok(v) => {
                    let Value::String(s) = v else { panic!("not a string") };
                    let got = it.heap.str_units(s).unwrap();
                    ok = ok && c.want.as_deref() == Some(got);
                    note = format!(" out_units={}", got.len());
                }
                Err(Error::Limit(LimitKind::Stringify(k))) if case == "dag" => note = format!(" limit={k:?}"),
                Err(e) => {
                    eprintln!("FAILED call: {e:?}");
                    std::process::exit(1);
                }
            }
            if case == "dag" {
                break;
            }
        }
        if case == "dag" {
            ok = note.contains("limit=");
        }
    }
    let ser_ms = t1.elapsed().as_secs_f64() * 1e3;
    let cpu = cpu_ms() - cpu1;
    if !ok {
        eprintln!("WRONG RESULT case={case} scale={scale}{note}");
        std::process::exit(1);
    }
    let hs = it.heap.stats();
    println!(
        "case={case} scale={scale} reps={} build_ms={build_ms:.1} build_cpu_ms={} stringify_ms={ser_ms:.1} stringify_cpu_ms={cpu} rss_peak_mib={:.1} steps_build={steps_build} steps_total={} obj_allocs={} str_allocs={} live_obj={} live_str={} heap_mib={:.1} freed={freed} peak_live={peak_live}{note} OK",
        c.reps,
        cpu1 - cpu0,
        proc_kib("VmHWM:") as f64 / 1024.0,
        it.stats().steps,
        hs.total_object_allocs,
        hs.total_string_allocs,
        hs.live_objects,
        hs.live_strings,
        hs.bytes as f64 / 1048576.0,
    );
}
