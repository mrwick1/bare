//! Standalone load driver for crates/tls/src/ecc.rs. No crates.
//!
//! Build: rustc --edition=2024 -C opt-level=3 tools/crypto/ecc_verify_load.rs -o <out>
//! Run:   <out> <cases.txt> <scale: 1|10|100> [base_ops=20000]
//!
//! `cases.txt` comes from data/crypto/ecdsa/oracle_gen.py: each line has a curve, key,
//! digest, signature and the answer given by OpenSSL. This tool runs `base_ops * scale`
//! verifications, taking the cases in a loop, and fails at the first answer that differs
//! from OpenSSL. It prints verifications per second for each curve, CPU time, peak RSS
//! (from /proc) and the number of heap allocations made inside the timed loop (expected: 0).

#[allow(dead_code)]
#[path = "../../crates/tls/src/ecc.rs"]
mod ecc;

use ecc::{Curve, verify};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

struct Counting;
static ALLOCS: AtomicU64 = AtomicU64::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static A: Counting = Counting;

fn cpu_secs() -> f64 {
    // /proc/self/stat fields 14 and 15 (utime, stime) in clock ticks; 100 Hz on Linux.
    let s = std::fs::read_to_string("/proc/self/stat").unwrap();
    let f: Vec<&str> = s[s.rfind(')').unwrap() + 2..].split(' ').collect();
    (f[11].parse::<f64>().unwrap() + f[12].parse::<f64>().unwrap()) / 100.0
}

fn peak_rss_kib() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    let l = s.lines().find(|l| l.starts_with("VmHWM:")).unwrap();
    l.split_whitespace().nth(1).unwrap().parse().unwrap()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

struct Case {
    curve: Curve,
    key: Vec<u8>,
    digest: Vec<u8>,
    sig: Vec<u8>,
    ok: bool,
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if !(3..=4).contains(&a.len()) {
        eprintln!("usage: {} <cases.txt> <scale 1|10|100> [base_ops]", a[0]);
        std::process::exit(2);
    }
    let scale: u64 = a[2].parse().expect("scale must be 1, 10 or 100");
    assert!(matches!(scale, 1 | 10 | 100), "scale must be 1, 10 or 100");
    let base: u64 = a.get(3).map_or(20_000, |s| s.parse().expect("base_ops must be a positive integer"));
    assert!(base > 0, "base_ops must be positive");
    let total = base.checked_mul(scale).expect("workload size overflow");

    let text = std::fs::read_to_string(&a[1]).expect("cannot read cases file");
    let cases: Vec<Case> = text
        .lines()
        .map(|l| {
            let f: Vec<&str> = l.split(' ').collect();
            let curve = match f[0] {
                "p256" => Curve::P256,
                "p384" => Curve::P384,
                c => panic!("curve {c}"),
            };
            Case { curve, key: unhex(f[1]), digest: unhex(f[2]), sig: unhex(f[3]), ok: f[4] == "ok" }
        })
        .collect();
    assert!(!cases.is_empty());

    let mut t = [Duration::ZERO; 2];
    let mut n = [0u64; 2];
    let (mut accepted, mut rejected) = (0u64, 0u64);
    let cpu0 = cpu_secs();
    let allocs0 = ALLOCS.load(Relaxed);
    let wall0 = Instant::now();
    for i in 0..total {
        let c = &cases[(i % cases.len() as u64) as usize];
        let t0 = Instant::now();
        let got = verify(c.curve, &c.key, &c.digest, &c.sig);
        let dt = t0.elapsed();
        assert_eq!(got.is_ok(), c.ok, "case {} differs from OpenSSL: {got:?}", i % cases.len() as u64);
        if c.ok {
            // Time only accepted signatures: they always run the full multiplication.
            // A rejected case may stop early (for example an off-curve key).
            let k = (c.curve == Curve::P384) as usize;
            t[k] += dt;
            n[k] += 1;
            accepted += 1
        } else {
            rejected += 1
        }
    }
    let allocs = ALLOCS.load(Relaxed) - allocs0;
    let wall = wall0.elapsed().as_secs_f64();
    let cpu = cpu_secs() - cpu0;
    println!("scale={scale}x verifications={total} cases={} accepted={accepted} rejected={rejected} (all equal to OpenSSL)", cases.len());
    for (k, name) in ["p256", "p384"].iter().enumerate() {
        if n[k] > 0 {
            println!("{name}: {} accepted verifications, {:.1} us each, {:.0}/s", n[k], t[k].as_secs_f64() * 1e6 / n[k] as f64, n[k] as f64 / t[k].as_secs_f64());
        }
    }
    println!("wall {wall:.2}s | cpu {cpu:.2}s | peak_rss {} KiB | allocs_in_loop {allocs}", peak_rss_kib());
}
