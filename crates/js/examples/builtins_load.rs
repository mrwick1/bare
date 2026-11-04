//! Load test for the JSON.parse, RegExp and String.prototype bindings. One case at one scale per process, so the
//! peak memory belongs to that run.
//!
//!   builtins_load CASE SCALE      CASE: json | scan | replace | split | strings | routes | zero | gc     SCALE: 1, 10, 100
//!
//! Each case builds its input in Rust, runs a small script, and compares the full result string with the same
//! computation done in Rust (the checksum). A wrong result exits with status 1 and prints no timing.
//! It prints parse and run wall time, CPU time, peak resident memory, steps, heap allocations and what `collect` frees.
//! There is no allocator hook (no unsafe): allocations are the heap's own counters. `tools/js/builtins_load.sh` runs the matrix.

use bare_js::eval::*;
use bare_js::heap::{Heap, HeapLimits};
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
    input: String,
    script: String,
    want: String,
}

fn lines(n: usize) -> String {
    (0..n).map(|i| format!("GET /p/{i}/x?q={} {} {}ms\n", i % 97, 200 + i % 5, i % 1000)).collect()
}

fn build(case: &str, scale: usize) -> Case {
    match case {
        "json" => {
            let n = 1000 * scale;
            let mut s = String::from("[");
            let (mut sum, mut tags) = (0u64, 0u64);
            for i in 0..n {
                if i > 0 {
                    s.push(',');
                }
                s += &format!("{{\"id\":{i},\"name\":\"n\\u00e9{i}\",\"tags\":[\"a\",\"b\"],\"ok\":true,\"v\":{}.5,\"o\":{{\"k\":null}}}}", i % 10);
                sum += i as u64 + format!("né{i}").chars().count() as u64 + 2;
                tags += 2;
            }
            s.push(']');
            Case { input: s, script: "var a = JSON.parse(INPUT), sum = 0, t = 0; for (var i = 0; i < a.length; i++) { var r = a[i]; sum += r.id + r.name.length + r.tags.length; t += r.tags.length + (r.ok ? 0 : 1) + (r.o.k === null ? 0 : 1); } a.length + ':' + sum + ':' + t".into(), want: format!("string:{n}:{sum}:{tags}") }
        }
        "scan" => {
            let n = 2000 * scale;
            let (mut sum, mut k) = (0u64, 0u64);
            for i in 0..n as u64 {
                sum += i + i % 1000;
                k += 1;
            }
            Case { input: lines(n), script: "var re = /GET \\/p\\/(\\d+)\\/x\\?q=(\\d+) (\\d+) (\\d+)ms/g, m, sum = 0, n = 0; while ((m = re.exec(INPUT)) !== null) { sum += Number(m[1]) + Number(m[4]); n++; } sum + ':' + n + ':' + INPUT.match(/\\d+ms/g).length".into(), want: format!("string:{sum}:{k}:{k}") }
        }
        "replace" => {
            let n = 2000 * scale;
            let input = lines(n);
            let a = input.len();
            // second result: every maximal digit run becomes one digit (its length)
            let mut b = 0usize;
            let mut run = false;
            for c in input.chars() {
                if c.is_ascii_digit() {
                    if !run {
                        b += 1;
                    }
                    run = true;
                } else {
                    b += 1;
                    run = false;
                }
            }
            Case { input, script: "INPUT.replace(/(\\d+)ms/g, '$1us').length + ':' + INPUT.replace(/\\d+/g, function (m) { return m.length; }).length".into(), want: format!("string:{a}:{b}") }
        }
        "split" => {
            let n = 2000 * scale;
            let input = lines(n);
            let a = input.split('\n').count();
            // `/ +/` splits on runs of spaces: pieces = separators + 1; the lines have 3 single spaces and a trailing newline
            let sep_runs = input.matches(' ').count();
            let c = input.split('/').count();
            Case { input, script: "INPUT.split('\\n').length + ':' + INPUT.split(/ +/).length + ':' + INPUT.split('/').length".into(), want: format!("string:{a}:{}:{c}", sep_runs + 1) }
        }
        "strings" => {
            let n = 10000 * scale;
            let mut sum = 0i64;
            for i in 0..n as i64 {
                let w = format!("Item {i} ");
                let t = w.trim().to_lowercase();
                sum += t.find("item").unwrap() as i64 + 2 + w.as_bytes()[1] as i64 + 1 + t.len() as i64 + (i % 3);
            }
            Case { input: String::new(), script: format!("var s = 0; for (var i = 0; i < {n}; i++) {{ var w = 'Item ' + i + ' '; var t = w.trim().toLowerCase(); s += t.indexOf('item') + w.slice(2, 4).length + w.charCodeAt(1) + (w.startsWith('It') ? 1 : 0) + t.concat('').length + (t.includes('m') ? i % 3 : 0); }} s"), want: format!("number:{sum}") }
        }
        "routes" => {
            let n = 10000 * scale;
            let (mut c, mut e) = (0u64, 0u64);
            for i in 0..n {
                if i % 4 == 1 || i % 4 == 2 {
                    c += 1;
                }
                e += 1000;
            }
            Case { input: String::new(), script: format!("var re = /^\\/journeys\\/(?!new\\/?$)[^/]+\\/?$/, c = 0, e = 0; for (var i = 0; i < {n}; i++) {{ var p = i % 4 === 0 ? '/journeys/new' : i % 4 === 1 ? '/journeys/' + i : i % 4 === 2 ? '/journeys/' + i + '/' : '/agents/' + i; if (re.test(p)) c++; if (new RegExp('^/(journeys|agents)').test(p)) e += 1000; }} c + ':' + e"), want: format!("string:{c}:{e}") }
        }
        "zero" => {
            let n = 10000 * scale;
            Case { input: "a".repeat(n), script: "INPUT.replace(/(?:)/g, '-').length + ':' + INPUT.match(/x*/g).length + ':' + INPUT.split(/(?:)/).length".into(), want: format!("string:{}:{}:{n}", 2 * n + 1, n + 1) }
        }
        "gc" => {
            // Driven by `main`: rounds of regexp and callback work with an explicit collect between rounds.
            let rounds = 5 * scale;
            Case { input: String::new(), script: String::new(), want: format!("{}", -300 * rounds as i64) }
        }
        _ => {
            eprintln!("unknown case {case}");
            std::process::exit(2);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (case, scale): (String, usize) = (args[1].clone(), args[2].parse().unwrap());
    let t = std::thread::Builder::new().stack_size(64 << 20).spawn(move || run(&case, scale));
    t.unwrap().join().unwrap();
}

fn run(case: &str, scale: usize) {
    let c = build(case, scale);
    let heap = Heap::new(HeapLimits { max_bytes: 700 << 20, ..Default::default() }).unwrap();
    let (t0, cpu0) = (Instant::now(), cpu_ms());
    let src = if case == "gc" { "function round(k) { var out = 0; for (var i = 0; i < 300; i++) { var r = new RegExp('(\\\\d+)x' + (i % 7), 'g'); var s = 'a1x' + (i % 7) + ' b22x' + (i % 7) + ' c' + k; out += s.replace(r, function (m, d) { return d.length; }).length - s.length + 3 + (s.match(r) || []).length - 2; var lit = /q(\\d)/; out += lit.test('q' + i) ? 1 : 0; } return out; }" } else { c.script.as_str() };
    let prog = parse_script(src, Limits::default()).expect("parse");
    let parse = t0.elapsed();
    let mut it = Interp::new(&prog, heap, EvalLimits { max_steps: 20_000_000_000, ..Default::default() }).expect("setup");
    if !c.input.is_empty() {
        let units: Vec<u16> = c.input.encode_utf16().collect();
        let s = it.heap.intern_vec(units).expect("input");
        let k = it.heap.key_from_str("INPUT").unwrap();
        let g = it.global_object();
        it.heap.set(g, k, Value::String(s)).unwrap();
    }
    let t1 = Instant::now();
    let (got, collected) = if case == "gc" {
        it.run().unwrap();
        let k = it.heap.key_from_str("round").unwrap();
        let f = match it.heap.get(it.global_object(), k) {
            Ok(bare_js::heap::Get::Value(v)) => v,
            _ => panic!("round"),
        };
        let mut total = 0i64;
        let (mut freed, mut peak_live) = (0usize, 0usize);
        for r in 0..(5 * scale) {
            let v = it.call(f, Value::Undefined, &[Value::Number(r as f64)]).unwrap();
            let Value::Number(n) = v else { panic!("not a number") };
            total += n as i64;
            peak_live = peak_live.max(it.heap.stats().live_objects);
            freed += it.collect(&[]).unwrap().objects;
            assert_eq!(it.regexp_objects(), 0, "dead regexps survived collect");
        }
        println!("  gc: freed {freed} objects, peak live objects {peak_live}, live after {}", it.heap.stats().live_objects);
        // Each iteration: replace shrinks "1xG" and "22xG" to one digit each (-5), then +3, +2 matches, -2, +1 for the literal: -1.
        (format!("{total}"), freed)
    } else {
        match it.run() {
            Ok(v) => (match v {
                Value::String(_) => format!("string:{}", it.display(v)),
                Value::Number(_) => format!("number:{}", it.display(v)),
                _ => it.display(v),
            }, 0),
            Err(e) => {
                eprintln!("FAILED run: {e:?}");
                std::process::exit(1);
            }
        }
    };
    let run = t1.elapsed();
    let cpu = cpu_ms() - cpu0;
    if got != c.want {
        eprintln!("WRONG RESULT case={case} scale={scale}\n got  {}\n want {}", &got[..got.len().min(200)], &c.want[..c.want.len().min(200)]);
        std::process::exit(1);
    }
    let hs = it.heap.stats();
    println!(
        "case={case} scale={scale} input_units={} parse_ms={:.1} run_ms={:.1} cpu_ms={cpu} rss_peak_mib={:.1} steps={} obj_allocs={} str_allocs={} live_obj={} live_str={} heap_mib={:.1} regexps={} freed={collected} OK",
        c.input.len(),
        parse.as_secs_f64() * 1e3,
        run.as_secs_f64() * 1e3,
        proc_kib("VmHWM:") as f64 / 1024.0,
        it.stats().steps,
        hs.total_object_allocs,
        hs.total_string_allocs,
        hs.live_objects,
        hs.live_strings,
        hs.bytes as f64 / 1048576.0,
        it.regexp_objects(),
    );
}
