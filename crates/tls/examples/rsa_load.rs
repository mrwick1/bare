//! Cost of RSA signature verification. Release mode, one process:
//!
//!   cargo run --release -p bare-tls --example rsa_load [rounds]
//!
//! Uses the valid signatures of tools/fixtures/rsa/vectors.txt (openssl made them). For each
//! key size it times: the whole PKCS#1 v1.5 verify with a SHA-256 hash of a 1.2 KB
//! to-be-signed part (one certificate signature), the whole RSA-PSS verify of a TLS
//! CertificateVerify (hash of the 130-byte content, then the verify), and the
//! modulus set-up alone. A "chain" is three RSA-2048 certificate signatures. It counts heap
//! allocations in the timed loops (expected: 0). The last lines are `key=value`.

use bare_tls::{bigint::Modulus, rsa, sha256::sha256};
use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

struct Counting;
static ALLOCS: AtomicU64 = AtomicU64::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}
#[global_allocator]
static A: Counting = Counting;

const VECTORS: &str = include_str!("../../../tools/fixtures/rsa/vectors.txt");

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

/// Microseconds per call, best of 5 batches.
fn time(rounds: u32, mut f: impl FnMut()) -> f64 {
    let mut best = f64::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        for _ in 0..rounds {
            f();
        }
        best = best.min(t.elapsed().as_secs_f64() * 1e6 / rounds as f64);
    }
    best
}

fn main() {
    let rounds: u32 = std::env::args().nth(1).map_or(2000, |a| a.parse().unwrap());
    let lines: Vec<Vec<&str>> = VECTORS.lines().filter(|l| !l.starts_with('#')).map(|l| l.split(' ').collect()).collect();
    let tbs = vec![0x30u8; 1200];
    for name in ["rsa2048", "rsa3072", "rsa4096"] {
        let der = unhex(lines.iter().find(|f| f[0] == "key" && f[1] == name).unwrap()[2]);
        let key = rsa::PublicKey::from_pkcs1(&der).unwrap();
        let case = |mode: &str| lines.iter().find(|f| f[0] == "case" && f[1] == "ok" && f[2] == name && f[3] == mode).unwrap();
        let (c1, c2) = (case("pkcs1-sha256"), case("pss-sha256"));
        let (d1, s1, d2, s2) = (unhex(c1[4]), unhex(c1[5]), unhex(c2[4]), unhex(c2[5]));
        let content = [0x20u8; 130];
        let before = ALLOCS.load(Ordering::Relaxed);
        // The digests in the vectors are for other messages. The timed work is the same:
        // hash, then the verify. Its answer is not used, only that it runs the full path.
        let pkcs1 = time(rounds, || {
            let h = sha256(black_box(&tbs));
            let _ = black_box(key.verify_pkcs1(rsa::Hash::Sha256, &h, &s1));
        });
        let pss = time(rounds, || {
            let h = sha256(black_box(&content));
            let _ = black_box(key.verify_pss(rsa::Hash::Sha256, &h, &s2));
        });
        // Check the real answers once, outside the timing.
        assert!(key.verify_pkcs1(rsa::Hash::Sha256, &d1, &s1).is_ok() && key.verify_pss(rsa::Hash::Sha256, &d2, &s2).is_ok());
        let setup = time(rounds, || {
            black_box(Modulus::new(black_box(key.modulus())).is_some());
        });
        let allocs = ALLOCS.load(Ordering::Relaxed) - before;
        println!("{name}: pkcs1_sha256_us={pkcs1:.1} pss_cv_us={pss:.1} modulus_setup_us={setup:.1} allocs_in_timed_loops={allocs}");
        if name == "rsa2048" {
            println!("chain_3_rsa2048_certificate_signatures_us={:.1}", 3.0 * pkcs1);
        }
    }
}
