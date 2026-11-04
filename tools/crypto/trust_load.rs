//! Standalone load and check driver for crates/tls/src/{hostname,trust_store}.rs. No crates.
//!
//! Build: rustc --edition=2024 -C opt-level=3 tools/crypto/trust_load.rs -o <out>
//! Run:   <out> pem <scale: 1|10|100> [base_iters=200]   decode the OS bundle base*scale times
//!        <out> host <scale: 1|10|100> [base_calls=1000000]   hostname checks, 4 and 1000 SANs
//!        <out> limits                                    size and count limits on big input
//!        <out> check [isrg-root-x2.der]                  load the OS store; find ISRG Root X2
//!
//! Prints wall time, CPU time, throughput, peak RSS (from /proc) and heap allocations made
//! inside the timed loop. Run each case in a fresh process under `ulimit -v` and `timeout`.

#[allow(dead_code)]
#[path = "../../crates/tls/src/hostname.rs"]
mod hostname;
#[allow(dead_code)]
#[path = "../../crates/tls/src/sha256.rs"]
mod sha256;
#[cfg(target_arch = "x86_64")]
#[allow(dead_code)]
#[path = "../../crates/tls/src/sha256_x86.rs"]
mod sha256_x86;
#[allow(dead_code)]
#[path = "../../crates/tls/src/trust_store.rs"]
mod trust_store;

use hostname::SanName;
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

fn pem_load(scale: u64, base: u64) {
    let iters = base.checked_mul(scale).expect("workload size overflow");
    let data = std::fs::read(trust_store::SYSTEM_PATHS[0]).expect("read OS bundle");
    let cpu0 = cpu_secs();
    let (alloc0, t0) = (ALLOCS.load(Relaxed), Instant::now());
    let (mut certs, mut bytes) = (0usize, 0usize);
    for _ in 0..iters {
        let s = trust_store::decode_pem(std::hint::black_box(&data)).expect("decode");
        certs += s.len();
        bytes += s.iter().map(<[u8]>::len).sum::<usize>();
    }
    let wall = t0.elapsed().as_secs_f64();
    let allocs = ALLOCS.load(Relaxed) - alloc0;
    let cpu = cpu_secs() - cpu0;
    let mb = (data.len() as u64 * iters) as f64 / 1e6;
    println!(
        "pem scale={scale}x iters={iters} in={:.0}MB certs={certs} der={:.0}MB wall={wall:.3}s cpu={cpu:.2}s {:.0} MB/s {:.1} us/bundle peak_rss_kib={} loop_allocations={allocs}",
        mb, bytes as f64 / 1e6, mb / wall, wall * 1e6 / iters as f64, peak_rss_kib()
    );
}

fn host_load(scale: u64, base: u64) {
    let calls = base.checked_mul(scale).expect("workload size overflow");
    // 4 SANs, match is the last one (staging shape), and 1000 SANs, no match (worst case).
    let small: Vec<Vec<u8>> = ["a.example", "b.example", "staging.8vx.in", "staging.app.8vox.ai"].iter().map(|s| s.as_bytes().to_vec()).collect();
    let big: Vec<Vec<u8>> = (0..1000).map(|i| format!("host{i}.big.example.com").into_bytes()).collect();
    for (tag, sans, host, want) in [("4san", &small, "Staging.App.8vox.ai", true), ("1000san-miss", &big, "staging.app.8vox.ai", false)] {
        let cpu0 = cpu_secs();
        let (alloc0, t0) = (ALLOCS.load(Relaxed), Instant::now());
        let mut hits = 0u64;
        for _ in 0..calls {
            let names = sans.iter().map(|s| SanName::Dns(s));
            hits += hostname::verify(std::hint::black_box(host), names).is_ok() as u64;
        }
        let wall = t0.elapsed().as_secs_f64();
        let allocs = ALLOCS.load(Relaxed) - alloc0;
        let cpu = cpu_secs() - cpu0;
        assert_eq!(hits, if want { calls } else { 0 });
        println!(
            "host {tag} scale={scale}x calls={calls} wall={wall:.3}s cpu={cpu:.2}s {:.1} ns/call peak_rss_kib={} loop_allocations={allocs}",
            wall * 1e9 / calls as f64, peak_rss_kib()
        );
    }
}

fn limits() {
    let cert = "-----BEGIN CERTIFICATE-----\nMAMCAQU=\n-----END CERTIFICATE-----\n";
    let timed = |name: &str, input: &[u8]| {
        let (a0, t0) = (ALLOCS.load(Relaxed), Instant::now());
        let r = trust_store::decode_pem(input);
        let (w, a) = (t0.elapsed().as_secs_f64(), ALLOCS.load(Relaxed) - a0);
        println!("{name}: {:?} wall={w:.4}s allocs={a} peak_rss_kib={}", r.as_ref().map(|s| s.len()).map_err(|e| e.to_string()), peak_rss_kib());
    };
    timed("1024 certs (max)", cert.repeat(1024).as_bytes());
    timed("1025 certs", cert.repeat(1025).as_bytes());
    timed("4 MiB of comments (max, no certs)", &vec![b'#'; trust_store::MAX_FILE_BYTES]);
    timed("4 MiB + 1", &vec![b'#'; trust_store::MAX_FILE_BYTES + 1]);
    // One block whose body never ends: stops at the per-certificate cap, not at the file cap.
    let mut one = b"-----BEGIN CERTIFICATE-----\n".to_vec();
    one.extend("A".repeat(64).repeat(1).as_bytes());
    one.push(b'\n');
    let body = "A".repeat(64) + "\n";
    let mut big = b"-----BEGIN CERTIFICATE-----\n".to_vec();
    for _ in 0..(trust_store::MAX_FILE_BYTES / 65 - 2) {
        big.extend(body.as_bytes());
    }
    timed("one 4 MiB block", &big);
}

fn check(fixture: Option<String>) {
    let (store, path) = trust_store::load_system().expect("load system store");
    let all: Vec<u8> = store.iter().flatten().copied().collect();
    println!("path={path} roots={} sha256(concatenated DER)={}", store.len(), hex(&sha256::sha256(&all)));
    // sha256 of ISRG Root X2 DER, from `openssl x509 -fingerprint -sha256` on this machine.
    const X2: &str = "69729b8e15a86efc177a57afb7171dfc64add28c2fca8cf1507e34453ccb1470";
    let found: Vec<usize> = (0..store.len()).filter(|&i| hex(&sha256::sha256(store.get(i).unwrap())) == X2).collect();
    println!("ISRG Root X2 (sha256 {X2}) at index {found:?}");
    assert_eq!(found.len(), 1, "ISRG Root X2 missing or duplicated");
    if let Some(f) = fixture {
        let want = std::fs::read(&f).expect("read fixture");
        println!("fixture {f}: sha256 {} identical={}", hex(&sha256::sha256(&want)), want == store.get(found[0]).unwrap());
        assert_eq!(want, store.get(found[0]).unwrap());
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let n = |i: usize, d: u64| a.get(i).map_or(d, |s| s.parse().expect("number"));
    let cmd = a.get(1).map(String::as_str);
    let scale = if matches!(cmd, Some("pem" | "host")) { n(2, 1) } else { 1 };
    assert!(matches!(scale, 1 | 10 | 100), "scale must be 1, 10 or 100");
    match cmd {
        Some("pem") => pem_load(scale, n(3, 200)),
        Some("host") => host_load(scale, n(3, 1_000_000)),
        Some("limits") => limits(),
        Some("check") => check(a.get(2).cloned()),
        _ => eprintln!("usage: pem|host <1|10|100> [base] | limits | check [fixture.der]"),
    }
}
