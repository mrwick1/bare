//! Standalone check, load and benchmark driver for the decimal reduction in crates/js/src/number.rs,
//! as used by lex.rs (`number_value`), value.rs (`string_to_number`) and json.rs. No crates.
//!
//! Build: rustc --edition=2024 -C opt-level=3 tools/js/decimal_load.rs -o <out>
//! Unit tests of the modules: rustc --edition=2024 --test tools/js/decimal_load.rs -o <t> && <t>
//!
//! <out> check <dir>                  every case in <dir>/manifest.tsv (written by decimal_oracle.mjs);
//!                                    each expected bit pattern comes from Node
//! <out> run <path> <workload> <1|10|100>
//!                                    one path (lex, str or json) and one workload in this process
//!                                    (sizes: scale x 1,000,000 digits; peak RSS <= 1 GiB)
//! <out> all                          every path, workload and scale in its own child process, each with
//!                                    `ulimit -v` (1 GiB address space) and `timeout 45`
//! <out> bench                        short conversions: ns and allocations per call, before/after
//!
//! Workloads: cancel (9...9.5e-N = 1), halfway (1 + 2^-53 plus zeros and a tail: 1 + 2^-52),
//! zeros (0.00...1e N = 0.1).

#[allow(dead_code)]
#[path = "../../crates/js/src/number.rs"]
mod number;
#[allow(dead_code)]
#[path = "../../crates/js/src/value.rs"]
mod value;
#[allow(dead_code)]
#[path = "../../crates/js/src/heap.rs"]
mod heap;
#[allow(dead_code)]
#[path = "../../crates/js/src/json.rs"]
mod json;
#[allow(dead_code)]
#[path = "../../crates/js/src/lex.rs"]
mod lex;

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

struct Counting;
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(l.size() as u64, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(n as u64, Relaxed);
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static A: Counting = Counting;

fn cpu_secs() -> f64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap();
    let f: Vec<&str> = s[s.rfind(')').unwrap() + 2..].split(' ').collect();
    (f[11].parse::<f64>().unwrap() + f[12].parse::<f64>().unwrap()) / 100.0
}

fn status_kib(field: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    let l = s.lines().find(|l| l.starts_with(field)).unwrap();
    l.split_whitespace().nth(1).unwrap().parse().unwrap()
}

/// The value of `src` read as a lexer number token (unsigned, one token, whole text).
fn lex_value(src: &str) -> Option<f64> {
    let mut l = lex::Lexer::new(src).ok()?;
    let t = l.next(lex::Goal::Div).ok()?;
    if t.end as usize != src.len() || !matches!(t.kind, lex::Kind::Number(lex::NumberBase::Decimal)) {
        return None;
    }
    lex::number_value(src, &t)
}

fn str_value(src: &str) -> f64 {
    let units: Vec<u16> = src.encode_utf16().collect();
    value::string_to_number(&units)
}

fn json_value(src: &str) -> Option<f64> {
    let units: Vec<u16> = src.encode_utf16().collect();
    let mut h = heap::Heap::default();
    match json::parse(&mut h, &units, &json::JsonOptions::default(), &json::JsonLimits::default()).ok()? {
        value::Value::Number(n) => Some(n),
        _ => None,
    }
}

fn convert(path: &str, src: &str) -> Option<f64> {
    match path {
        "lex" => lex_value(src),
        "str" => Some(str_value(src)),
        "json" => json_value(src),
        _ => panic!("unknown path {path}"),
    }
}

/// Every case in the manifest: the bit pattern must equal Node's, for the path the case names.
fn check(dir: &str) {
    let manifest = std::fs::read_to_string(format!("{dir}/manifest.tsv")).expect("run decimal_oracle.mjs first");
    let (mut total, mut bad) = (0usize, 0usize);
    for line in manifest.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        let want = u64::from_str_radix(f[0], 16).unwrap();
        let src = std::fs::read_to_string(format!("{dir}/{}", f[2])).unwrap();
        // A lex case is checked on the lexer path and on Number(string) (which rejects `_`).
        let paths: &[&str] = if f[1] == "lex" { &["lex"] } else if f[1] == "json" { &["json"] } else { &["str"] };
        for path in paths {
            total += 1;
            let got = convert(path, &src).map(f64::to_bits);
            if got != Some(want) {
                bad += 1;
                if bad <= 10 {
                    println!("MISMATCH {path} {} len={} got={got:x?} want={want:x}", f[2], src.len());
                }
            }
        }
    }
    println!("decimal check: {total} conversions, {bad} mismatches against Node");
    if bad > 0 {
        std::process::exit(1);
    }
}

fn workload(name: &str, n: usize) -> (String, u64) {
    let zeros = "0".repeat(n);
    match name {
        // 9...9.5 * 10^-n, about 1 (the cancelling exponent that made Rust's parser return infinity).
        "cancel" => (format!("{}.5e-{n}", "9".repeat(n)), 1.0f64.to_bits()),
        // 1 + 2^-53 (a tie), then n zeros and a 1 after it: the tail is above the tie.
        "halfway" => (format!("1.00000000000000011102230246251565404236316680908203125{zeros}1"), (1.0 + f64::EPSILON).to_bits()),
        "zeros" => (format!("0.{zeros}1e{n}"), 0.1f64.to_bits()),
        _ => panic!("unknown workload {name}"),
    }
}

fn run(path: &str, name: &str, scale: usize) {
    assert!(matches!(scale, 1 | 10 | 100), "scale must be 1, 10 or 100");
    let n = scale * 1_000_000;
    let (src, want) = workload(name, n);
    if path == "lex" && src.len() > lex::MAX_SOURCE_LEN {
        // The lexer's documented source limit applies before any number is read.
        assert!(lex::Lexer::new(&src).is_err());
        println!("path=lex workload={name} scale={scale}x units={} SourceTooLarge (lexer limit {} bytes, expected)", src.len(), lex::MAX_SOURCE_LEN);
        return;
    }
    let (rss0, cpu0, a0) = (status_kib("VmRSS:"), cpu_secs(), ALLOCS.load(Relaxed));
    let t = Instant::now();
    let got = black_box(convert(path, black_box(&src)));
    let wall = t.elapsed().as_secs_f64();
    let cpu = cpu_secs() - cpu0;
    let allocs = ALLOCS.load(Relaxed) - a0;
    let peak = status_kib("VmHWM:");
    assert!(peak <= 1 << 20, "peak RSS {peak} KiB exceeds 1 GiB");
    let ok = got.map(f64::to_bits) == Some(want);
    println!(
        "path={path} workload={name} scale={scale}x units={} wall={wall:.3}s cpu={cpu:.2}s allocs={allocs} peak_rss_kib={peak} rss_start_kib={rss0} result_bits={:x} {}",
        src.len(),
        got.map_or(0, f64::to_bits),
        if ok { "ok" } else { "WRONG" }
    );
    if !ok {
        std::process::exit(1);
    }
}

fn all() {
    let me = std::env::current_exe().unwrap();
    let mut failed = false;
    for path in ["lex", "str", "json"] {
        for w in ["cancel", "halfway", "zeros"] {
            for scale in [1, 10, 100] {
                let cmd = format!("ulimit -v 1048576; exec timeout 45 '{}' run {path} {w} {scale}", me.display());
                let o = std::process::Command::new("sh").args(["-c", &cmd]).output().unwrap();
                if o.status.success() {
                    print!("{}", String::from_utf8_lossy(&o.stdout));
                } else {
                    failed = true;
                    println!("path={path} workload={w} scale={scale}x FAILED status={:?}\n{}", o.status.code(), String::from_utf8_lossy(&o.stderr));
                }
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
}

/// Short decimal tokens, the common case: time only the conversion, after the lexer has run.
fn bench() {
    const N: usize = 2_000_000;
    let texts = ["12.5", "0.001", "3.14159", "1e-7", "123456789.123", "42", "6.02214076e23", "0.1"];
    let lexed: Vec<(String, lex::Token)> = texts
        .iter()
        .map(|s| {
            let mut l = lex::Lexer::new(s).unwrap();
            (s.to_string(), l.next(lex::Goal::Div).unwrap())
        })
        .collect();
    let units: Vec<Vec<u16>> = texts.iter().map(|s| s.encode_utf16().collect()).collect();
    let mut sum = 0.0f64;

    let (a, t) = (ALLOCS.load(Relaxed), Instant::now());
    for i in 0..N {
        let (s, tok) = &lexed[i % lexed.len()];
        sum += black_box(lex::number_value(black_box(s), tok)).unwrap();
    }
    let lex_ns = t.elapsed().as_secs_f64() * 1e9 / N as f64;
    let lex_allocs = (ALLOCS.load(Relaxed) - a) as f64 / N as f64;

    let (a, t) = (ALLOCS.load(Relaxed), Instant::now());
    for i in 0..N {
        sum += black_box(value::string_to_number(black_box(&units[i % units.len()])));
    }
    let str_ns = t.elapsed().as_secs_f64() * 1e9 / N as f64;
    let str_allocs = (ALLOCS.load(Relaxed) - a) as f64 / N as f64;

    println!("bench lex_number_value: {lex_ns:.1} ns/op allocs/op={lex_allocs:.3}");
    println!("bench string_to_number: {str_ns:.1} ns/op allocs/op={str_allocs:.3} (checksum {sum:.1})");
}

fn preflight() {
    let text = "9".repeat(1_000_000) + ".5e-1000000";
    let separated = "9_".repeat(1_000_000) + "9.5e-1000001";
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut heap = heap::Heap::default();
    for kind in ["lex", "str", "json", "separated"] {
        let before = BYTES.load(Relaxed);
        let value = match kind {
            "lex" => lex_value(&text).unwrap(),
            "str" => value::string_to_number(&units),
            "json" => match json::parse(&mut heap, &units, &json::JsonOptions::default(), &json::JsonLimits::default()).unwrap() { value::Value::Number(n) => n, _ => panic!() },
            _ => lex_value(&separated).unwrap(),
        };
        let bytes = BYTES.load(Relaxed) - before;
        assert_eq!(value, 1.0);
        assert!(bytes < 8192, "{kind} copied its long input: {bytes} allocated bytes");
        println!("{kind}: bounded conversion scratch, {bytes} allocated bytes");
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("check") if a.len() == 3 => check(&a[2]),
        Some("run") if a.len() == 5 => run(&a[2], &a[3], a[4].parse().unwrap()),
        Some("all") => all(),
        Some("bench") => bench(),
        Some("preflight") => preflight(),
        _ => {
            eprintln!("usage: {} check <dir> | run <lex|str|json> <cancel|halfway|zeros> <1|10|100> | all | bench", a[0]);
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_cases_agree() {
        for (path, name) in [("lex", "cancel"), ("str", "halfway"), ("json", "zeros")] {
            let (src, want) = workload(name, 1_500);
            assert_eq!(convert(path, &src).map(f64::to_bits), Some(want), "{path} {name}");
        }
    }
}
