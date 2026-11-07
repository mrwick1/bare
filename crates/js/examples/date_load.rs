//! Load test for `Date`. One case at one scale per process, so the peak memory belongs to that run.
//!
//!   date_load CASE SCALE    CASE: utc | iso | parse | gc | clock | cap | longstr     SCALE: 1, 10, 100
//!
//! Every expected value is computed here, with a different algorithm from `bare_js::date` (Julian day numbers instead of the
//! `days_from_civil` of the engine), and the result is checked in the process. A wrong result exits with status 1.
//! `cap` and `longstr` are hostile: they must end in a `Sc(Dates)` or `Sc(DateString)` limit, never in memory growth.
//! `tools/js/date_load.sh` runs the matrix.

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

/// Julian day number of a Gregorian date (Fliegel and Van Flandern). `m` is 1..=12.
fn jdn(y: i64, m: i64, d: i64) -> i64 {
    let a = (m - 14) / 12;
    (1461 * (y + 4800 + a)) / 4 + (367 * (m - 2 - 12 * a)) / 12 - (3 * ((y + 4900 + a) / 100)) / 4 + d - 32075
}

/// The inverse (Richards), for a Julian day number of 0 or more: `(year, month, day)`.
fn civil(j: i64) -> (i64, i64, i64) {
    let f = j + 1401 + (((4 * j + 274_277) / 146_097) * 3) / 4 - 38;
    let e = 4 * f + 3;
    let h = e % 1461 / 4 * 5 + 2;
    let d = h % 153 / 5 + 1;
    let m = (h / 153 + 2) % 12 + 1;
    (e / 1461 - 4716 + (14 - m) / 12, m, d)
}

const EPOCH_JDN: i64 = 2_440_588;
const MOD: i64 = 1_000_000_007;

fn utc_ms(i: i64) -> i64 {
    let (y, mo, d, h, mi, s, ms) = (1900 + (i * 7) % 400, (i * 5) % 12, 1 + (i * 3) % 28, (i * 11) % 24, (i * 13) % 60, (i * 17) % 60, (i * 19) % 1000);
    (jdn(y, mo + 1, d) - EPOCH_JDN) * 86_400_000 + h * 3_600_000 + mi * 60_000 + s * 1000 + ms
}

fn iso_t(i: i64) -> i64 {
    i * 1_234_567_891 - 50_000_000_000_000
}

fn iso_text(t: i64) -> String {
    let (day, tod) = (t.div_euclid(86_400_000), t.rem_euclid(86_400_000));
    let (y, m, d) = civil(day + EPOCH_JDN);
    let year = if (0..=9999).contains(&y) { format!("{y:04}") } else { format!("{}{:06}", if y < 0 { '-' } else { '+' }, y.abs()) };
    format!("{year}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z", tod / 3_600_000, tod / 60_000 % 60, tod / 1000 % 60, tod % 1000)
}

struct Case {
    body: &'static str,
    n: usize,
    /// The expected result of `run(n)`, as a number.
    want: Option<f64>,
}

fn build(case: &str, scale: usize) -> Case {
    let n = 20_000 * scale;
    match case {
        "utc" => {
            let mut s = 0i64;
            for i in 0..n as i64 {
                s = (s + utc_ms(i)) % MOD;
            }
            Case {
                body: "var s = 0; for (var i = 0; i < n; i++) { s = (s + Date.UTC(1900 + (i * 7) % 400, (i * 5) % 12, 1 + (i * 3) % 28, (i * 11) % 24, (i * 13) % 60, (i * 17) % 60, (i * 19) % 1000)) % 1000000007; } return s;",
                n,
                want: Some(s as f64),
            }
        }
        "iso" => {
            let mut h = 0i64;
            for i in 0..n as i64 {
                for c in iso_text(iso_t(i)).bytes() {
                    h = (h * 31 + c as i64) % MOD;
                }
            }
            Case {
                body: "var h = acc; for (var i = off; i < off + n; i++) { var s = new Date(i * 1234567891 - 50000000000000).toISOString(); for (var j = 0; j < s.length; j++) h = (h * 31 + s.charCodeAt(j)) % 1000000007; } return h;",
                n,
                want: Some(h as f64),
            }
        }
        "parse" => Case {
            body: "var ok = acc; for (var i = off; i < off + n; i++) { var t = i * 1234567891 - 50000000000000; if (Date.parse(new Date(t).toISOString()) === t) ok++; } return ok;",
            n,
            want: Some(n as f64),
        },
        "gc" => Case { body: "var a = []; for (var i = 0; i < 1000; i++) a.push(new Date(i)); return a[999].getTime() + a.length;", n: 5 * scale, want: Some(1999.0) },
        "clock" => Case { body: "return Date.now() * 2 + new Date().getTime() * 0;", n: n / 4, want: None },
        "cap" => Case { body: "var a = []; for (var i = 0; i < n; i++) a.push(new Date(i)); return a.length;", n: 10_000 * scale + 1, want: None },
        "longstr" => Case { body: "var s = '2020-01-01T00:00:00.'; for (var i = 0; i < 12; i++) s = s + s; return Date.parse(s);", n: 1, want: None },
        _ => {
            eprintln!("unknown case {case}");
            std::process::exit(2);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (case, scale): (String, usize) = (args[1].clone(), args[2].parse().unwrap());
    std::thread::Builder::new().stack_size(if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 }).spawn(move || run(&case, scale)).unwrap().join().unwrap();
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
    let src = format!("function run(n, off, acc) {{ {} }}", c.body);
    let prog = parse_script(&src, Limits::default()).expect("parse");
    let mut it = Interp::new(&prog, heap, EvalLimits { max_steps: 20_000_000_000, ..Default::default() }).expect("setup");
    exec::set(&mut it);
    it.run().expect("setup run");
    if case == "cap" {
        it.set_sc_limits(ScLimits { max_dates: 10_000 * scale, ..Default::default() });
    }
    let f = global(&mut it, "run");
    let (t1, cpu1) = (Instant::now(), cpu_ms());
    let (mut ok, mut note, mut freed) = (true, String::new(), 0usize);
    let live0 = it.heap.stats().live_objects;
    match case {
        "gc" => {
            for _ in 0..c.n {
                match it.call(f, Value::Undefined, &[Value::Number(0.0)]) {
                    Ok(Value::Number(v)) => ok = ok && Some(v) == c.want,
                    r => {
                        eprintln!("FAILED call: {r:?}");
                        std::process::exit(1);
                    }
                }
                freed += it.collect(&[]).unwrap().objects;
            }
            let live1 = it.heap.stats().live_objects;
            ok = ok && it.date_tracked() == 0 && live1 <= live0 + 2;
            note = format!(" live_before={live0} live_after={live1} dates_after={}", it.date_tracked());
        }
        "clock" => {
            let mut sum = 0f64;
            let mut want = 0f64;
            let mut backward = 0usize;
            let mut last = f64::NEG_INFINITY;
            for i in 0..c.n as i64 {
                // the wall clock moves forward and backward: a jump of up to 1e6 s either way
                let w = ((i * 2_654_435_761) % 2_000_003 - 1_000_000) as f64 * 1000.0 + 1.6e12;
                it.set_wall_clock(WallClock::Virtual(w)).unwrap();
                match it.call(f, Value::Undefined, &[Value::Number(0.0)]) {
                    Ok(Value::Number(v)) => sum += v,
                    r => {
                        eprintln!("FAILED call: {r:?}");
                        std::process::exit(1);
                    }
                }
                want += w * 2.0;
                backward += (w < last) as usize;
                last = w;
            }
            ok = sum == want && backward > c.n / 10;
            note = format!(" backward_steps={backward}");
        }
        "cap" | "longstr" => match it.call(f, Value::Undefined, &[Value::Number(c.n as f64)]) {
            Err(Error::Limit(LimitKind::Sc(k))) => {
                ok = (case == "cap" && matches!(k, ScLimit::Dates) && it.date_tracked() == 10_000 * scale) || (case == "longstr" && matches!(k, ScLimit::DateString));
                note = format!(" limit={k:?} dates={}", it.date_tracked());
            }
            r => {
                eprintln!("FAILED: expected a limit, got {r:?}");
                std::process::exit(1);
            }
        },
        // `iso` and `parse` make one Date per item and the collector cannot run during a call, so they work in batches
        // (one call makes at most `max_dates` Dates). The running value is passed from one batch to the next.
        _ => {
            let batch = if matches!(case, "iso" | "parse") { 50_000 } else { c.n.max(1) };
            let (mut off, mut acc) = (0usize, 0f64);
            while off < c.n {
                let b = batch.min(c.n - off);
                match it.call(f, Value::Undefined, &[Value::Number(b as f64), Value::Number(off as f64), Value::Number(acc)]) {
                    Ok(Value::Number(v)) => acc = v,
                    r => {
                        eprintln!("FAILED call: {r:?}");
                        std::process::exit(1);
                    }
                }
                freed += it.collect(&[]).unwrap().objects;
                off += b;
            }
            ok = Some(acc) == c.want;
        }
    }
    let ms = t1.elapsed().as_secs_f64() * 1e3;
    if !ok {
        eprintln!("WRONG RESULT case={case} scale={scale}{note}");
        std::process::exit(1);
    }
    let hs = it.heap.stats();
    println!(
        "case={case} scale={scale} checksum=ok n={} date_ms={ms:.1} date_cpu_ms={} rss_peak_mib={:.1} steps={} obj_allocs={} heap_mib={:.1} freed={freed}{note} OK",
        c.n,
        cpu_ms() - cpu1,
        proc_kib("VmHWM:") as f64 / 1024.0,
        it.stats().steps,
        hs.total_object_allocs,
        hs.bytes as f64 / 1048576.0,
    );
}
