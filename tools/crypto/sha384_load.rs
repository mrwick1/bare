//! Standalone load driver for crates/tls/src/sha384.rs. No crates.
//!
//! Build: rustc --edition=2024 -C opt-level=3 tools/crypto/sha384_load.rs -o <out>
//! Run:   <out> <scale: 1|10|100> [base_mib=64]
//!
//! Total data = base_mib * scale MiB, fed as chunks of 16421 bytes (not a multiple of 128,
//! so the partial-block path runs on nearly every call). Chunk i is a fixed template with
//! its first 8 bytes set to i (little-endian). The last chunk is cut to fit the total.
//! Prints the digest, so the same stream can be hashed in Python hashlib and compared.
//! Also prints throughput, CPU time, peak RSS (from /proc) and heap allocations in the loop.

#[allow(dead_code)]
#[path = "../../crates/tls/src/sha384.rs"]
mod sha384;

use sha384::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

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

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if !(2..=3).contains(&a.len()) {
        eprintln!("usage: {} <scale: 1|10|100> [base_mib]", a[0]);
        std::process::exit(2);
    }
    let scale: u64 = a[1].parse().expect("scale must be 1, 10 or 100");
    assert!(matches!(scale, 1 | 10 | 100), "scale must be 1, 10 or 100");
    let base_mib: u64 = a.get(2).map_or(64, |s| s.parse().expect("base_mib must be a positive integer"));
    assert!(base_mib > 0, "base_mib must be positive");
    const CHUNK: usize = 16421;
    let total = base_mib.checked_mul(scale).and_then(|n| n.checked_mul(1 << 20)).expect("workload size overflow");

    let mut buf = vec![0u8; CHUNK];
    for (i, b) in buf.iter_mut().enumerate() {
        *b = (i as u32).wrapping_mul(2654435761) as u8;
    }
    let mut h = Sha384::new();
    let (mut done, mut i) = (0u64, 0u64);
    let cpu0 = cpu_secs();
    let allocs0 = ALLOCS.load(Relaxed);
    let t0 = Instant::now();
    while done < total {
        buf[..8].copy_from_slice(&i.to_le_bytes());
        let n = (total - done).min(CHUNK as u64) as usize;
        h.update(&buf[..n]);
        done += n as u64;
        i += 1;
    }
    let digest = h.finalize();
    let wall = t0.elapsed().as_secs_f64();
    let allocs = ALLOCS.load(Relaxed) - allocs0;
    let cpu = cpu_secs() - cpu0;
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    println!("scale={scale}x bytes={total} chunks={i} digest={hex}");
    println!(
        "{:.1} MB/s | wall {:.2}s | cpu {:.2}s | peak_rss {} KiB | allocs_in_loop {}",
        total as f64 / 1e6 / wall, wall, cpu, peak_rss_kib(), allocs
    );
}
