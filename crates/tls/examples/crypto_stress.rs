//! Load test for the crypto core. Run in release mode, one case per process:
//!
//!   cargo run --release -p bare-tls --example crypto_stress -- <case> <size> [chunk]
//!
//! Cases:
//!   sha   <bytes> [chunk]   stream <bytes> of zeros through SHA-256, `chunk` bytes per update
//!   hmac  <bytes> [chunk]   the same through HMAC-SHA256 (a 100-byte key)
//!   hkdf  <count>           <count> times HKDF-Expand of the maximum 8160 bytes
//!   small <count> [len]     <count> separate HMACs of <len> bytes (default 64): per-call cost
//!
//! The program reads input from one reused buffer, so memory does not grow with <bytes>.
//! It counts heap allocations inside the timed loop with a wrapper around the system
//! allocator. The hash path must show 0. The last line is machine-readable `key=value`.

use bare_tls::{hkdf, hmac, sha256};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

// A `#[global_allocator]` replaces Rust's heap for the whole program. Ours forwards every
// call to the system allocator and bumps two counters first. (Like wrapping `malloc` in C.)
struct Counting;
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(l.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}
#[global_allocator]
static A: Counting = Counting;

// Peak resident memory of this process, from the kernel (Linux). 0 elsewhere.
fn peak_rss_kib() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .unwrap_or(0)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = || -> ! {
        eprintln!("usage: crypto_stress sha|hmac <bytes> [chunk] | hkdf <count> | small <count> [len]");
        std::process::exit(2)
    };
    let case = args.first().map(String::as_str).unwrap_or_else(|| usage());
    let n: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or_else(|| usage());
    let arg3: Option<usize> = args.get(2).and_then(|s| s.parse().ok());

    let chunk = arg3.unwrap_or(1 << 20).max(1);
    let buf = vec![0u8; if case == "sha" || case == "hmac" { chunk } else { 0 }];
    let key = [0x42u8; 100];
    let a0 = ALLOCS.load(Ordering::Relaxed);
    let b0 = BYTES.load(Ordering::Relaxed);
    let t = Instant::now();
    let (digest, work): ([u8; 32], u64) = match case {
        "sha" => {
            let mut h = sha256::Sha256::new();
            let mut left = n;
            while left > 0 {
                let k = left.min(chunk as u64) as usize;
                h.update(&buf[..k]);
                left -= k as u64;
            }
            (h.finalize(), n)
        }
        "hmac" => {
            let mut h = hmac::Hmac::new(&key);
            let mut left = n;
            while left > 0 {
                let k = left.min(chunk as u64) as usize;
                h.update(&buf[..k]);
                left -= k as u64;
            }
            (h.finalize(), n)
        }
        "hkdf" => {
            let prk = hkdf::extract(b"salt", b"ikm");
            let mut out = [0u8; hkdf::MAX_OUTPUT_LEN];
            let mut x = 0u8;
            for i in 0..n {
                let info = i.to_le_bytes();
                hkdf::expand(&prk, &info, &mut out).unwrap();
                x ^= out[hkdf::MAX_OUTPUT_LEN - 1]; // keep the work from being optimised away
            }
            ([x; 32], n * hkdf::MAX_OUTPUT_LEN as u64)
        }
        "small" => {
            let len = arg3.unwrap_or(64);
            let msg = vec![0x37u8; len];
            let mut acc = [0u8; 32];
            for i in 0..n {
                let mut h = hmac::Hmac::new(&acc[..(i % 33) as usize]);
                h.update(&msg);
                acc = h.finalize();
            }
            (acc, n * len as u64)
        }
        _ => usage(),
    };
    let secs = t.elapsed().as_secs_f64();
    let allocs = ALLOCS.load(Ordering::Relaxed) - a0;
    let bytes = BYTES.load(Ordering::Relaxed) - b0;
    let rss = peak_rss_kib();
    println!("digest={}", hex(&digest));
    println!(
        "case={case} n={n} work_bytes={work} secs={secs:.3} mb_per_s={:.1} ops_per_s={:.0} allocs_in_loop={allocs} alloc_bytes={bytes} peak_rss_kib={rss}",
        work as f64 / secs / 1e6,
        n as f64 / secs
    );
}
