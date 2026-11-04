//! Standalone harness for crates/js/src/regexp.rs (no cargo, no dependencies).
//!   rustc --edition 2024 -O tools/js/regexp_load.rs -o regexp_load          # build
//!   rustc --edition 2024 --test tools/js/regexp_load.rs -o regexp_test      # oracle fixture test
//!   regexp_load check CASES.tsv           compare every span with Node (see regexp_oracle.mjs)
//!   regexp_load canon-dump                print canon(0..=0xFFFF), one hex value per line
//!   regexp_load MODE SCALE [EXPECT_SUM]   one load run: MODE = short | near | caps | budget | compile
//! A load run prints one line: wall, CPU, peak RSS, allocation count, bytes, peak live bytes,
//! retained scratch bytes, checksum, match count, error count, steps. With EXPECT_SUM (the
//! per-pass checksum from regexp_load_expect.mjs) it fails on a different checksum.
#![allow(dead_code)]
#[path = "../../crates/js/src/regexp.rs"]
mod regexp;

use regexp::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(l.size(), Relaxed);
        let live = LIVE.fetch_add(l.size(), Relaxed) + l.size();
        PEAK.fetch_max(live, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(n, Relaxed);
        if n > l.size() {
            let live = LIVE.fetch_add(n - l.size(), Relaxed) + n - l.size();
            PEAK.fetch_max(live, Relaxed);
        } else {
            LIVE.fetch_sub(l.size() - n, Relaxed);
        }
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static A: Counting = Counting;

#[repr(C)]
struct Timespec {
    s: i64,
    ns: i64,
}
unsafe extern "C" {
    fn clock_gettime(clk: i32, ts: *mut Timespec) -> i32;
}
fn cpu_ms() -> f64 {
    let mut t = Timespec { s: 0, ns: 0 };
    unsafe { clock_gettime(2, &mut t) }; // CLOCK_PROCESS_CPUTIME_ID
    t.s as f64 * 1e3 + t.ns as f64 / 1e6
}
fn rss_kib() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let f = |k: &str| s.lines().find(|l| l.starts_with(k)).and_then(|l| l.split_whitespace().nth(1)?.parse().ok()).unwrap_or(0);
    f("VmHWM:")
}

fn u16s(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}
fn unhex(s: &str) -> Vec<u16> {
    (0..s.len() / 4).map(|i| u16::from_str_radix(&s[4 * i..4 * i + 4], 16).unwrap()).collect()
}

/// Run one oracle case. Returns Err(description) on a mismatch.
fn check_case(line: &str) -> Result<char, String> {
    let f: Vec<&str> = line.split('\t').collect();
    assert_eq!(f.len(), 6, "bad line: {line}");
    let (label, flags, pat, input, li, exp) = (f[0], f[1], unhex(f[2]), unhex(f[3]), f[4].parse::<usize>().unwrap(), f[5]);
    let re = Regex::new(&pat, flags);
    let bad = |got: String| Err(format!("{label} flags={flags:?} li={li}: expected {exp}, got {got}"));
    match (exp, re) {
        ("E", Err(CompileError::Syntax { .. } | CompileError::InvalidFlags(_))) => Ok('E'),
        ("E", other) => bad(format!("{other:?}")),
        ("U", Err(CompileError::Unsupported { .. } | CompileError::UnsupportedFlag(_))) => Ok('U'),
        ("U", other) => bad(format!("{other:?}")),
        ("L", Err(CompileError::Limit(_))) => Ok('L'),
        ("L", other) => bad(format!("{other:?}")),
        (_, Err(e)) => bad(format!("{e:?}")),
        (_, Ok(re)) => {
            let mut sc = Scratch::new();
            let got = match re.exec(&input, li, &Limits::default(), None, &mut sc) {
                Ok(None) => "N".to_string(),
                Ok(Some(m)) => m.spans.iter().map(|s| s.map_or("-".to_string(), |(a, b)| format!("{a},{b}"))).collect::<Vec<_>>().join(";"),
                Err(e) => format!("{e:?}"),
            };
            if got == exp { Ok(if got == "N" { 'N' } else { 'M' }) } else { bad(got) }
        }
    }
}

fn check_file(text: &str) -> (usize, usize) {
    let (mut n, mut fails) = (0, 0);
    let mut kinds = std::collections::BTreeMap::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        n += 1;
        match check_case(line) {
            Ok(k) => *kinds.entry(k).or_insert(0) += 1,
            Err(e) => {
                fails += 1;
                if fails <= 20 {
                    eprintln!("MISMATCH {e}");
                }
            }
        }
    }
    println!("cases={n} mismatches={fails} kinds(M=match N=nomatch E=syntax U=unsupported)={kinds:?}");
    (n, fails)
}

#[test]
fn oracle_fixture() {
    let (n, fails) = check_file(include_str!("regexp_cases.tsv"));
    assert!(n > 500);
    assert_eq!(fails, 0);
}

// ---- load workloads (the same data and checksum are in regexp_load_expect.mjs) ----

fn fold(h: &mut u32, v: u32) {
    *h = h.wrapping_mul(31).wrapping_add(v);
}
fn fold_match(h: &mut u32, m: &Match) {
    for s in &m.spans {
        match s {
            Some((a, b)) => {
                fold(h, *a as u32 + 1);
                fold(h, *b as u32 + 1)
            }
            None => fold(h, 0),
        }
    }
}

/// Iterate matches like `String.prototype.match` with `g`: lastIndex = end, +1 after an empty match.
fn run_all(re: &Regex, input: &[u16], lim: &Limits, sc: &mut Scratch, h: &mut u32, stat: &mut (u64, u64)) {
    let mut li = 0;
    loop {
        match re.exec(input, li, lim, None, sc) {
            Ok(Some(m)) => {
                stat.0 += 1;
                fold_match(h, &m);
                if !re.flags().global {
                    break;
                }
                let (a, b) = m.spans[0].unwrap();
                li = if a == b { b + 1 } else { b };
            }
            Ok(None) => {
                fold(h, 7);
                break;
            }
            Err(_) => {
                stat.1 += 1;
                break;
            }
        }
    }
}

type Work = Vec<(&'static str, &'static str, Vec<String>)>;

fn short_work() -> Work {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect();
    vec![
        ("^\\/journeys\\/(?!new\\/?$)[^/]+\\/?$", "", s(&["/journeys/abc/", "/journeys/new", "/journeys/new/", "/calls/12"])),
        ("\\/+", "g", s(&["a//b///c", "/x"])),
        ("([a-z0-9])([A-Z])", "g", s(&["fooBarBaz", "x1Y2Z"])),
        ("^(\\d+(\\.\\d+)?)?(xs|sm|md|lg|xl)$", "", s(&["1.5xl", "xl", "12px"])),
        ("Googlebot|Mediapartners-Google|Bingbot|Slurp|DuckDuckBot|yandex", "i", s(&["Mozilla/5.0 (compatible; GOOGLEBOT/2.1)", "Mozilla/5.0 Firefox"])),
        ("\\r\\n?", "g", s(&["a\r\nb\rc\nd"])),
        ("\\W", "g", s(&["a-b_c d"])),
        ("^\\/(agents|campaigns|journeys|clients)(\\/|$)", "", s(&["/journeys/9", "/clients", "/x"])),
    ]
}
fn caps_work() -> Work {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect();
    vec![
        ("^(\\w+)\\s+(\\d+)-(?:(a)|(b)|(c))\\s*(.*?)\\s*$", "", s(&["user 123-b  hello world  ", "x 9-c", "x 9-z"])),
        ("(?:(\\d+)|([a-z]+)|(-))+", "", s(&["12ab-34cd-56ef-78gh-90ij"])),
        ("(a)|(b)|(c)|(d)|(e)|(f)", "g", s(&["abcdefabcdef"])),
        ("^(?:(?:([a-z]+):)?\\/\\/([^/?#]*))?([^?#]*)(?:\\?([^#]*))?(?:#(.*))?$", "", s(&["https://example.com/a/b?x=1&y=2#frag", "/rel?q"])),
    ]
}
fn near_work() -> Work {
    vec![("(\\d+)px", "", vec!["1".repeat(1000)]), ("(?:a|b)*c", "", vec!["ab".repeat(300)])]
}

fn load(mode: &str, scale: u64, expect: Option<u32>) {
    let (wall, cpu0) = (std::time::Instant::now(), cpu_ms());
    let (a0, b0) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed));
    let lim = Limits { max_steps: 1 << 40, ..Limits::default() };
    let mut sc = Scratch::new();
    let (mut stat, mut errors, mut steps) = ((0u64, 0u64), 0u64, 0u64);
    let mut sum = 0u32;
    let passes;
    let mut pass_sum = None;
    match mode {
        "short" | "caps" | "near" => {
            let (work, n) = match mode {
                "short" => (short_work(), 20_000 * scale),
                "caps" => (caps_work(), 5_000 * scale),
                _ => (near_work(), scale),
            };
            let compiled: Vec<(Regex, Vec<Vec<u16>>)> =
                work.iter().map(|(p, f, ins)| (Regex::from_str(p, f).unwrap(), ins.iter().map(|i| u16s(i)).collect())).collect();
            passes = n;
            for pass in 0..n {
                let mut h = 0u32;
                for (re, ins) in &compiled {
                    for i in ins {
                        run_all(re, i, &lim, &mut sc, &mut h, &mut stat);
                        steps += sc.steps;
                    }
                }
                if pass == 0 {
                    pass_sum = Some(h);
                }
                assert_eq!(Some(h), pass_sum, "checksum changed between passes");
                sum = h;
            }
        }
        "budget" => {
            passes = 100 * scale;
            let step_re = Regex::from_str("(a*)*b", "").unwrap();
            let stack_re = Regex::from_str("(?:a|b)*c", "").unwrap();
            let a30 = vec![b'a' as u16; 30];
            let ab = vec![b'a' as u16; 200_000];
            let stop = AtomicBool::new(true);
            for _ in 0..passes {
                let l1 = Limits { max_steps: 200_000, ..Limits::default() };
                assert_eq!(step_re.exec(&a30, 0, &l1, None, &mut sc), Err(ExecError::StepLimit));
                steps += sc.steps;
                let l2 = Limits { max_stack_frames: 10_000, ..Limits { max_steps: 1 << 40, ..Limits::default() } };
                assert_eq!(stack_re.exec(&ab, 0, &l2, None, &mut sc), Err(ExecError::StackLimit));
                let l3 = Limits { max_input_units: 100_000, ..Limits::default() };
                assert_eq!(stack_re.exec(&ab, 0, &l3, None, &mut sc), Err(ExecError::InputTooLarge));
                let l4 = Limits { max_steps: 1 << 40, ..Limits::default() };
                assert_eq!(step_re.exec(&a30, 0, &l4, Some(&stop), &mut sc), Err(ExecError::Cancelled));
                errors += 4;
            }
        }
        "compile" => {
            passes = 200 * scale;
            let pats: Vec<(String, String)> = short_work().into_iter().chain(caps_work()).chain(near_work()).map(|(p, f, _)| (p.to_string(), f.to_string())).collect();
            let mut n = 0u32;
            for _ in 0..passes {
                for (p, f) in &pats {
                    n = n.wrapping_add(Regex::from_str(p, f).unwrap().program_len() as u32);
                }
            }
            sum = n / passes as u32;
            pass_sum = Some(sum);
        }
        _ => panic!("unknown mode {mode}"),
    }
    if let (Some(e), Some(g)) = (expect, pass_sum) {
        assert_eq!(e, g, "checksum differs from the Node expectation");
    }
    let w = wall.elapsed().as_secs_f64() * 1e3;
    println!(
        "mode={mode} scale={scale} passes={passes} wall_ms={w:.1} cpu_ms={:.1} rss_kib={} allocs={} alloc_bytes={} peak_live_bytes={} retained_bytes={} pass_checksum={} matches={} errors={} steps={steps}",
        cpu_ms() - cpu0,
        rss_kib(),
        ALLOCS.load(Relaxed) - a0,
        BYTES.load(Relaxed) - b0,
        PEAK.load(Relaxed),
        sc.retained_bytes(),
        pass_sum.unwrap_or(sum),
        stat.0,
        errors + stat.1,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("check") => {
            let (_, fails) = check_file(&std::fs::read_to_string(&a[2]).unwrap());
            std::process::exit(if fails == 0 { 0 } else { 1 });
        }
        Some("canon-dump") => {
            for c in 0..=0xFFFFu32 {
                println!("{:04x}", canon(c as u16));
            }
        }
        Some(mode) => load(mode, a.get(2).and_then(|s| s.parse().ok()).unwrap_or(1), a.get(3).and_then(|s| s.parse().ok())),
        None => eprintln!("usage: regexp_load check FILE | MODE SCALE [EXPECT]"),
    }
}
