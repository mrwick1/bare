//! Standalone test harness and load driver for crates/tls/src/{der,x509}.rs. No crates.
//!
//! Tests: rustc --edition=2024 --test tools/crypto/x509_load.rs -o <out> && <out>
//! Load:  rustc --edition=2024 -C opt-level=3 tools/crypto/x509_load.rs -o <out>
//!        <out> <scale: 1|10|100> [base_rounds=20000]
//!
//! Each round parses the five public fixtures from tools/fixtures/x509, then parses 5
//! randomly mutated copies (1-3 bytes changed in a reused buffer; no allocation). The total is
//! base_rounds * scale rounds. A checksum of parsed fields keeps the work from being optimized
//! away. Prints certificates/s, MB/s, CPU time, peak RSS (from /proc) and heap allocations
//! inside the loop. Run it under `timeout` and `ulimit -v`.
//! Parsing only: this measures the parser, not a certificate check.

#[allow(dead_code)]
#[path = "../../crates/tls/src/der.rs"]
mod der;
#[allow(dead_code)]
#[path = "../../crates/tls/src/bigint.rs"]
mod bigint;
#[allow(dead_code)]
#[path = "../../crates/tls/src/rsa.rs"]
mod rsa;
#[allow(dead_code)]
#[path = "../../crates/tls/src/sha256.rs"]
mod sha256;
#[cfg(all(target_arch = "x86_64", not(feature = "force-portable")))]
#[allow(dead_code)]
#[path = "../../crates/tls/src/sha256_x86.rs"]
mod sha256_x86;
#[allow(dead_code)]
#[path = "../../crates/tls/src/sha384.rs"]
mod sha384;
#[allow(dead_code)]
#[path = "../../crates/tls/src/x509.rs"]
mod x509;

use std::alloc::{GlobalAlloc, Layout, System};
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
    // utime + stime from /proc/self/stat (fields 14, 15), in clock ticks of 1/100 s.
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let rest = s.rsplit(')').next().unwrap_or("");
    let f: Vec<&str> = rest.split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
    (t(11) + t(12)) / 100.0
}

fn peak_rss_kib() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
        .unwrap_or(0)
}

fn touch(der: &[u8]) -> u64 {
    match x509::parse(der) {
        Ok(c) => {
            let mut h = c.serial.len() as u64 + c.not_after.0 as u64 + c.tbs.len() as u64;
            h += c.extensions.count as u64;
            if let Some(s) = c.extensions.san {
                h += s.names().count() as u64;
            }
            if let x509::PublicKey::Ec { point, .. } = c.public_key {
                h += point[1] as u64;
            }
            h + 1
        }
        Err(_) => 0,
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let scale: u64 = a.get(1).and_then(|s| s.parse().ok()).filter(|s| [1, 10, 100].contains(s)).unwrap_or_else(|| {
        eprintln!("usage: x509_load <1|10|100> [base_rounds=20000]");
        std::process::exit(2)
    });
    let base: u64 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(20_000);
    let fix: [&[u8]; 5] = [
        include_bytes!("../fixtures/x509/staging-0-leaf.der"),
        include_bytes!("../fixtures/x509/staging-1-le-ye2.der"),
        include_bytes!("../fixtures/x509/staging-2-isrg-root-ye.der"),
        include_bytes!("../fixtures/x509/staging-3-isrg-x2-crosssigned.der"),
        include_bytes!("../fixtures/x509/isrg-root-x2.der"),
    ];
    assert!(base > 0, "base rounds must be positive");
    let rounds = base.checked_mul(scale).filter(|r| r.checked_mul(10).is_some()).expect("workload size overflow");
    let mut buf = vec![0u8; 2048]; // reused mutation buffer
    let (mut rng, mut sum, mut ok, mut bad, mut bytes) = (0x9e3779b97f4a7c15u64, 0u64, 0u64, 0u64, 0u64);
    let c0 = cpu_secs();
    let (a0, b0) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed));
    let t0 = Instant::now();
    for _ in 0..rounds {
        for f in fix {
            sum = sum.wrapping_add(touch(f));
            bytes += f.len() as u64;
            let b = &mut buf[..f.len()];
            b.copy_from_slice(f);
            for _ in 0..1 + rng % 3 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                b[(rng % f.len() as u64) as usize] = (rng >> 32) as u8;
            }
            let r = touch(b);
            if r == 0 { bad += 1 } else { ok += 1 }
            sum = sum.wrapping_add(r);
            bytes += f.len() as u64;
        }
    }
    let (allocs, alloc_bytes) = (ALLOCS.load(Relaxed) - a0, BYTES.load(Relaxed) - b0);
    let (wall, cpu) = (t0.elapsed().as_secs_f64(), cpu_secs() - c0);
    let certs = rounds * 10;
    println!("rounds={rounds} certs={certs} ({} MiB) mutated_ok={ok} mutated_err={bad} checksum={sum:016x}", bytes >> 20);
    println!("wall={wall:.2}s cpu={cpu:.2}s  {:.0} certs/s  {:.0} MB/s", certs as f64 / wall, bytes as f64 / wall / 1e6);
    println!("allocs_in_loop={} bytes_in_loop={} peak_rss={} KiB", allocs, alloc_bytes, peak_rss_kib());
}
