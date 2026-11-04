//! Standalone load driver for crates/tls/src/aes_gcm.rs. No crates.
//!
//! Build: rustc --edition=2024 -C opt-level=3 tools/crypto/aes_gcm_load.rs -o <out>
//! Run:   <out> <portable|auto> <scale: 1|10|100> [base_mib=64]
//!
//! Total data = base_mib * scale MiB, in 16 KiB records (the TLS 1.3 maximum plaintext)
//! with a 5-byte AAD (a TLS record header). Each record gets a new nonce from
//! `tls13_nonce(iv, seq)` with seq counting up from 0, so no (key, nonce) pair repeats.
//! Per record: seal, open, compare to the original. Every 4096th record also gets one
//! flipped byte, which must fail to open and leave the buffer unchanged.
//! Prints throughput, CPU time and peak RSS (from /proc) and the number of heap
//! allocations made inside the timed loop.

#[allow(dead_code)]
#[path = "../../crates/tls/src/aes_gcm.rs"]
mod aes_gcm;

use aes_gcm::*;
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

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 3 {
        eprintln!("usage: {} <portable|auto> <scale> [base_mib]", a[0]);
        std::process::exit(2);
    }
    let key = *b"bare-aes-load-k1";
    let iv = *b"load-test-iv";
    let g = if a[1] == "portable" { Aes128Gcm::new_portable(&key) } else { Aes128Gcm::new(&key) };
    let scale: u64 = a[2].parse().unwrap();
    let base_mib: u64 = a.get(3).map_or(64, |s| s.parse().unwrap());
    const REC: usize = 16384;
    let records = base_mib * scale * (1 << 20) / REC as u64;

    // Fixed template plaintext; each record is the template XOR the low bytes of seq.
    let mut template = vec![0u8; REC];
    for (i, b) in template.iter_mut().enumerate() {
        *b = (i as u32).wrapping_mul(2654435761) as u8;
    }
    let mut buf = vec![0u8; REC];
    let mut orig = vec![0u8; REC];
    let mut saved = vec![0u8; REC];
    let aad = [0x17, 0x03, 0x03, 0x40, 0x11];
    let (mut seal_t, mut open_t) = (Duration::ZERO, Duration::ZERO);
    let (mut neg_checked, mut sink) = (0u64, 0u8);

    let cpu0 = cpu_secs();
    let allocs0 = ALLOCS.load(Relaxed);
    let wall0 = Instant::now();
    for seq in 0..records {
        orig.copy_from_slice(&template);
        orig[..8].copy_from_slice(&seq.to_le_bytes());
        buf.copy_from_slice(&orig);
        let nonce = tls13_nonce(&iv, seq);
        let t0 = Instant::now();
        let tag = g.seal_in_place_detached(&nonce, &aad, &mut buf).unwrap();
        let t1 = Instant::now();
        if seq % 4096 == 0 {
            saved.copy_from_slice(&buf);
            let pos = (seq as usize * 7) % REC;
            buf[pos] ^= 1;
            assert_eq!(g.open_in_place_detached(&nonce, &aad, &mut buf, &tag), Err(Error::AuthFailed));
            buf[pos] ^= 1;
            assert_eq!(buf, saved, "buffer changed by failed open");
            neg_checked += 1;
        }
        let t2 = Instant::now();
        g.open_in_place_detached(&nonce, &aad, &mut buf, &tag).unwrap();
        let t3 = Instant::now();
        assert!(buf == orig, "roundtrip mismatch at seq {seq}");
        sink ^= buf[(seq as usize) % REC] ^ tag[0];
        seal_t += t1 - t0;
        open_t += t3 - t2;
    }
    let allocs = ALLOCS.load(Relaxed) - allocs0;
    let wall = wall0.elapsed().as_secs_f64();
    let cpu = cpu_secs() - cpu0;
    let mb = (records * REC as u64) as f64 / 1e6;
    println!(
        "backend={} scale={}x data={:.0}MB records={} nonces=0..{} negative_checks={}",
        if g.is_accelerated() { "accelerated" } else { "portable" },
        scale, mb, records, records, neg_checked
    );
    println!(
        "seal {:.1} MB/s | open {:.1} MB/s | wall {:.2}s | cpu {:.2}s | peak_rss {} KiB | allocs_in_loop {} | sink {}",
        mb / seal_t.as_secs_f64(), mb / open_t.as_secs_f64(), wall, cpu, peak_rss_kib(), allocs, sink
    );
}
