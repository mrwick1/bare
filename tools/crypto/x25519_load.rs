//! Standalone load and oracle driver for crates/tls/src/x25519.rs. No crates.
//!
//! Build: rustc --edition=2024 -C opt-level=3 tools/crypto/x25519_load.rs -o <out>
//! Run:   <out> load <scale: 1|10|100> [base_iters=2000]
//!        <out> random <scale: 1|10|100> [base_calls=100000]
//!        <out> oracle [pairs=20000]
//!
//! load: base_iters * scale iterations. Each iteration is a full key exchange: two public
//! keys and two shared secrets (4 X25519 calls), and the two secrets must match. The next
//! iteration's scalars come from this iteration's secret, so nothing can be skipped.
//! Prints wall time, CPU time, exchanges per second, peak RSS (from /proc) and the heap
//! allocations made inside the timed loop.
//!
//! oracle: runs the same random scalars and u-values (some noncanonical or with the top
//! bit set) through python3 `cryptography` (OpenSSL) and compares. Where OpenSSL refuses
//! (all-zero output) we must return all zeros. The test oracle is not linked into bare.

#[allow(dead_code)]
#[path = "../../crates/tls/src/x25519.rs"]
mod x25519;
#[allow(dead_code)]
#[path = "../../crates/tls/src/random.rs"]
mod random;

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;
use x25519::*;

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

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// xorshift64*: a fixed-seed generator for repeatable test inputs. Not for secrets.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn bytes(&mut self) -> [u8; 32] {
        let mut b = [0u8; 32];
        for c in b.chunks_mut(8) {
            c.copy_from_slice(&self.next().to_le_bytes());
        }
        b
    }
}

fn load(scale: u64, base: u64) {
    let iters = base.checked_mul(scale).filter(|n| n.checked_mul(4).is_some()).expect("workload size overflow");
    let mut a = [7u8; 32];
    let mut b = [9u8; 32];
    let cpu0 = cpu_secs(); // allocates, so read the counter after it
    let (alloc0, t0) = (ALLOCS.load(Relaxed), Instant::now());
    for _ in 0..iters {
        let (pa, pb) = (public_key(&a), public_key(&b));
        let s1 = shared_secret(&a, &pb).expect("all zero");
        let s2 = shared_secret(&b, &pa).expect("all zero");
        assert_eq!(s1, s2, "shared secrets differ");
        a = s1;
        b = pa;
    }
    let wall = t0.elapsed().as_secs_f64();
    let allocs = ALLOCS.load(Relaxed) - alloc0; // before cpu_secs(), which allocates
    let cpu = cpu_secs() - cpu0;
    println!("final state {}", hex(&a[..4])); // 4 bytes of a chained value: shows the work was not skipped
    println!(
        "scale {scale}x: {iters} exchanges ({} x25519 calls) wall {wall:.3}s cpu {cpu:.2}s  {:.0} exchanges/s  {:.2} us/x25519  peak RSS {} KiB  allocs in loop {allocs}",
        iters * 4,
        iters as f64 / wall,
        wall * 1e6 / (iters * 4) as f64,
        peak_rss_kib()
    );
}

fn random_load(scale: u64, base: u64) {
    let calls = base.checked_mul(scale).expect("workload size overflow");
    let mut buf = [0u8; 32];
    let cpu0 = cpu_secs();
    let (alloc0, start) = (ALLOCS.load(Relaxed), Instant::now());
    for _ in 0..calls {
        random::fill(&mut buf).expect("OS random source failed");
        std::hint::black_box(&buf);
    }
    let wall = start.elapsed().as_secs_f64();
    let allocs = ALLOCS.load(Relaxed) - alloc0;
    let cpu = cpu_secs() - cpu0;
    println!("random scale={scale} calls={calls} bytes_per_call=32 wall={wall:.3}s cpu={cpu:.2}s peak_rss_kib={} loop_allocations={allocs}", peak_rss_kib());
}

const PY: &str = r#"
import sys
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey, X25519PublicKey
for line in sys.stdin:
    k, u = line.split()
    try:
        out = X25519PrivateKey.from_private_bytes(bytes.fromhex(k)).exchange(X25519PublicKey.from_public_bytes(bytes.fromhex(u))).hex()
    except Exception:
        out = "ERR"
    print(out, flush=True)
"#;

fn oracle(n: u64) {
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let mut cases = Vec::new();
    for i in 0..n {
        let k = rng.bytes();
        let mut u = rng.bytes();
        match i % 4 {
            1 => u[31] |= 0x80, // top bit set
            2 => {
                // p + small, or 2^255 - 1 - small: noncanonical but below 2^255
                u = [0xff; 32];
                u[31] = 0x7f;
                u[0] = 0xed + (rng.next() % 19) as u8;
            }
            3 => u[31] = 0xff - (rng.next() % 2) as u8, // the largest values
            _ => {}
        }
        cases.push((k, u));
    }
    let mut child = Command::new("python3").args(["-I", "-c", PY]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().expect("python3");
    let input: String = cases.iter().map(|(k, u)| format!("{} {}\n", hex(k), hex(u))).collect();
    // Write on a thread: the child's output pipe must be drained while we write.
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()).unwrap());
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap();
    assert!(out.status.success(), "oracle script failed");
    let text = String::from_utf8(out.stdout).unwrap();
    let (mut same, mut rejected) = (0u64, 0u64);
    for ((k, u), line) in cases.iter().zip(text.lines()) {
        let ours = x25519(k, u);
        if line == "ERR" {
            assert_eq!(ours, [0; 32], "oracle refused but we gave {}", hex(&ours));
            rejected += 1;
        } else {
            assert_eq!(hex(&ours), line, "mismatch for k={} u={}", hex(k), hex(u));
            same += 1;
        }
    }
    assert_eq!(same + rejected, n, "oracle returned too few lines");
    println!("oracle: {same} outputs identical, {rejected} refused by oracle and all-zero here, 0 differences");
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("load") if (3..=4).contains(&a.len()) => {
            let scale: u64 = a[2].parse().expect("scale must be 1, 10 or 100");
            assert!(matches!(scale, 1 | 10 | 100), "scale must be 1, 10 or 100");
            let base = a.get(3).map_or(2000, |s| s.parse().expect("base_iters must be a positive integer"));
            assert!(base > 0, "base_iters must be positive");
            load(scale, base);
        }
        Some("random") if (3..=4).contains(&a.len()) => {
            let scale: u64 = a[2].parse().expect("scale must be 1, 10 or 100");
            assert!(matches!(scale, 1 | 10 | 100), "scale must be 1, 10 or 100");
            let base = a.get(3).map_or(100000, |s| s.parse().expect("base_calls must be positive"));
            assert!(base > 0, "base_calls must be positive");
            random_load(scale, base);
        }
        Some("oracle") if a.len() <= 3 => oracle(a.get(2).map_or(20000, |s| s.parse().expect("pairs"))),
        _ => {
            eprintln!("usage: {} load <1|10|100> [base_iters] | random <1|10|100> [base_calls] | oracle [pairs]", a[0]);
            std::process::exit(2);
        }
    }
}
