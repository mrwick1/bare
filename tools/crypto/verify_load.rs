//! Standalone test harness and load driver for crates/tls/src/verify.rs. No crates.
//!
//! Tests: rustc --edition=2024 --test tools/crypto/verify_load.rs -o <out> && <out>
//!        (add `--cfg 'feature="force-portable"'` for the portable SHA-256 path)
//! Load:  rustc --edition=2024 -C opt-level=3 tools/crypto/verify_load.rs -o <out>
//!        <out> <valid|invalid|cv> <scale: 1|10|100> [base=2000] [anchors=150] [first|last]
//!
//! valid:   verify the public staging chain against a store of `anchors` certificates. One is
//!          the real ISRG Root X2 (first or last in the store); the rest are copies of it with a
//!          changed subject, so they parse but never match.
//! invalid: each round mutates one random bit of one of the first three staging certificates, or
//!          changes the host or the time, and verifies. Every round must fail. Exit code 1 if any
//!          passes.
//! cv:      CertificateVerify rejection path (no staging private key is available).
//! Prints wall time, CPU time, rate, peak RSS (from /proc) and heap allocations inside the timed
//! loop. Run each case in a fresh process under `timeout` and `ulimit -v`.

#[allow(dead_code)]
#[path = "../../crates/tls/src/der.rs"]
mod der;
#[allow(dead_code)]
#[path = "../../crates/tls/src/ecc.rs"]
mod ecc;
#[allow(dead_code)]
#[path = "../../crates/tls/src/hostname.rs"]
mod hostname;
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
#[path = "../../crates/tls/src/trust_store.rs"]
mod trust_store;
#[allow(dead_code)]
#[path = "../../crates/tls/src/verify.rs"]
mod verify;
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
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let rest = s.rsplit(')').next().unwrap_or("");
    let f: Vec<&str> = rest.split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
    (t(11) + t(12)) / 100.0
}

fn peak_rss_kib() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix("VmHWM:")).and_then(|v| v.split_whitespace().next()?.parse().ok()).unwrap_or(0)
}

const STAGING: [&[u8]; 4] = [
    include_bytes!("../fixtures/x509/staging-0-leaf.der"),
    include_bytes!("../fixtures/x509/staging-1-le-ye2.der"),
    include_bytes!("../fixtures/x509/staging-2-isrg-root-ye.der"),
    include_bytes!("../fixtures/x509/staging-3-isrg-x2-crosssigned.der"),
];
const X2: &[u8] = include_bytes!("../fixtures/x509/isrg-root-x2.der");
const HOST: &str = "staging.app.8vox.ai";
const NOW: i64 = 1_795_000_000;

fn b64(d: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in d.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            s.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    s
}

/// `n` anchors. The real X2 is at the front or back. The others have a changed subject.
fn make_store(n: usize, last: bool) -> trust_store::TrustStore {
    let needle = b"ISRG Root X2";
    let at = X2.windows(needle.len()).rposition(|w| w == needle).unwrap() + 11; // the final '2'
    let mut pem = String::new();
    let mut add = |d: &[u8]| {
        pem += "-----BEGIN CERTIFICATE-----\n";
        for l in b64(d).as_bytes().chunks(64) {
            pem += std::str::from_utf8(l).unwrap();
            pem += "\n";
        }
        pem += "-----END CERTIFICATE-----\n";
    };
    for i in 0..n {
        if i == if last { n - 1 } else { 0 } {
            add(X2);
        } else {
            let mut d = X2.to_vec();
            d[at] = b'A' + (i % 26) as u8; // another subject; the SKI stays, so AKI never rules it out
            d[at - 1] = b'0' + (i / 26 % 10) as u8;
            add(&d);
        }
    }
    trust_store::decode_pem(pem.as_bytes()).unwrap()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let usage = || -> ! {
        eprintln!("usage: verify_load <valid|invalid|cv> <1|10|100> [base=2000] [anchors=150] [first|last]");
        std::process::exit(2)
    };
    let mode = a.get(1).map(|s| s.as_str()).filter(|m| ["valid", "invalid", "cv"].contains(m)).unwrap_or_else(|| usage());
    let scale: u64 = a.get(2).and_then(|s| s.parse().ok()).filter(|s| [1, 10, 100].contains(s)).unwrap_or_else(|| usage());
    let base: u64 = a.get(3).and_then(|s| s.parse().ok()).filter(|b| *b > 0).unwrap_or(2000);
    let anchors: usize = a.get(4).and_then(|s| s.parse().ok()).filter(|n| (1..=trust_store::MAX_CERTS).contains(n)).unwrap_or(150);
    let last = a.get(5).map(|s| s.as_str()) != Some("first");
    let rounds = base.checked_mul(scale).expect("workload size overflow");
    let store = make_store(anchors, last);

    // Reference answers before the timed loop.
    let p = verify::verify_server_chain(&STAGING, &store, HOST, NOW).expect("staging chain must verify");
    assert_eq!(p.path_certs(), 4);
    let th = sha256::sha256(b"load transcript");
    let bad_sig = [0x30, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01];

    let mut bufs: Vec<Vec<u8>> = STAGING.iter().map(|c| c.to_vec()).collect(); // reused
    let (mut rng, mut ok, mut bad, mut sum) = (0x9e3779b97f4a7c15u64, 0u64, 0u64, 0u64);
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let c0 = cpu_secs();
    let (a0, b0) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed));
    let t0 = Instant::now();
    for _ in 0..rounds {
        match mode {
            "valid" => match verify::verify_server_chain(&STAGING, &store, HOST, NOW) {
                Ok(p) => {
                    ok += 1;
                    sum += p.path_certs() as u64;
                }
                Err(_) => bad += 1,
            },
            "invalid" => {
                let r = next();
                let (mut host, mut now) = (HOST, NOW);
                let which = (r >> 8) as usize % 3;
                match r % 8 {
                    0 => host = "staging.app.8vox.com",
                    1 => now = 1_792_055_226,
                    2 => now = 1_799_831_227,
                    _ => {
                        let b = &mut bufs[which];
                        let bit = (r >> 16) as usize % (b.len() * 8);
                        b[bit / 8] ^= 1 << (bit % 8);
                    }
                }
                let chain = [&bufs[0][..], &bufs[1][..], &bufs[2][..], &bufs[3][..]];
                match verify::verify_server_chain(&chain, &store, host, now) {
                    Ok(_) => ok += 1,
                    Err(_) => bad += 1,
                }
                bufs[which].copy_from_slice(STAGING[which]);
            }
            _ => {
                // The staging leaf is P-256, but there is no signature by its key. Only the
                // rejection path is timed here: key check, hash, one scalar multiplication.
                match verify::verify_certificate_verify(&p, 0x0403, &th, &bad_sig) {
                    Ok(()) => ok += 1,
                    Err(_) => bad += 1,
                }
            }
        }
    }
    let (allocs, alloc_bytes) = (ALLOCS.load(Relaxed) - a0, BYTES.load(Relaxed) - b0);
    let (wall, cpu) = (t0.elapsed().as_secs_f64(), cpu_secs() - c0);
    println!(
        "mode={mode} scale={scale} rounds={rounds} anchors={anchors} pos={} accepted={ok} rejected={bad} checksum={sum}",
        if last { "last" } else { "first" }
    );
    println!(
        "wall={wall:.3}s cpu={cpu:.2}s rate={:.0}/s us_each={:.1} peak_rss={}KiB allocs_in_loop={allocs} bytes={alloc_bytes}",
        rounds as f64 / wall,
        wall * 1e6 / rounds as f64,
        peak_rss_kib()
    );
    if mode == "valid" && bad != 0 || (mode == "invalid" || mode == "cv") && ok != 0 {
        eprintln!("FAIL: unexpected result counts");
        std::process::exit(1);
    }
}
