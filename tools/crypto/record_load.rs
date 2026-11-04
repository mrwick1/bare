//! Standalone test, load and oracle driver for crates/tls/src/record.rs. No crates.
//!
//! The parent crate owns lib.rs, so this file declares the modules itself:
//! `record.rs` finds `aes_gcm` through `crate::aes_gcm`, so both are declared at the root here.
//!
//! Unit tests (RFC 8448 vectors and the rest):
//!   rustc --edition=2024 --test tools/crypto/record_load.rs -o <t> && <t>
//!   add `--cfg 'feature="force-portable"'` to test the portable AES backend on any CPU.
//! Load and oracle:
//!   rustc --edition=2024 -C opt-level=3 tools/crypto/record_load.rs -o <out>
//!   <out> mem  <scale: 1|10|100> [base_mib=64] [record_bytes=16384]
//!   <out> sock <scale: 1|10|100> [base_mib=64] [record_bytes=16384]
//!   <out> oracle [records=3000]
//!
//! mem:  one RecordLayer over an in-memory pipe, keys installed for both directions
//!       (loopback: the same key and IV, so every record we seal we also open). Records go
//!       out and come back in batches of 8, so the read-ahead buffer sees several records
//!       per `read`. Every record's bytes are compared to the source. Prints throughput of
//!       the write path and read path separately, wall, CPU, peak RSS and the heap
//!       allocations inside the timed loop (must be 0).
//! sock: two RecordLayers over a Unix socket pair, a writer thread and a reader thread.
//!       Prints the number of `read`/`write` calls (syscalls) too.
//! Both first run negative checks: tampered records must fail, then the layer is dead.
//! oracle: python3 `cryptography` (OpenSSL AES-GCM) decrypts what we write and writes
//!       records (random lengths, random zero padding, all types) that we must read.
//!       The oracle is not linked into bare.

#[allow(dead_code)]
#[path = "../../crates/tls/src/aes_gcm.rs"]
mod aes_gcm;
#[allow(dead_code)]
#[path = "../../crates/tls/src/record.rs"]
mod record;
#[allow(dead_code)]
#[path = "../../crates/tls/src/sha256.rs"]
mod sha256;
#[cfg(all(target_arch = "x86_64", not(feature = "force-portable")))]
#[path = "../../crates/tls/src/sha256_x86.rs"]
mod sha256_x86;

use record::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{self, BufRead, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
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

static READS: AtomicU64 = AtomicU64::new(0);
static WRITES: AtomicU64 = AtomicU64::new(0);

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

const KEY: [u8; 16] = *b"bare-record-key1";
const IV: [u8; 12] = *b"record-ld-iv";

/// In-memory pipe: `write` appends, `read` drains. Capacity is reused.
struct Pipe {
    buf: Vec<u8>,
    rpos: usize,
}
impl Read for Pipe {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = out.len().min(self.buf.len() - self.rpos);
        out[..n].copy_from_slice(&self.buf[self.rpos..self.rpos + n]);
        self.rpos += n;
        if self.rpos == self.buf.len() {
            self.buf.clear();
            self.rpos = 0;
        }
        Ok(n)
    }
}
impl Write for Pipe {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Write-only sink the oracle can empty between records.
#[derive(Clone, Default)]
struct Shared(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);
impl Read for Shared {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }
}
impl Write for Shared {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A socket that counts system calls.
struct Counted(UnixStream);
impl Read for Counted {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        READS.fetch_add(1, Relaxed);
        self.0.read(b)
    }
}
impl Write for Counted {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        WRITES.fetch_add(1, Relaxed);
        self.0.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// Payload of record `seq`: a fixed pseudo-random template with the record number in the
/// first 8 bytes, so a record cannot be swapped for another unnoticed. Stamping 8 bytes is
/// cheap, so the timings are of the record layer and not of the test data.
fn template(rec: usize) -> Vec<u8> {
    (0..rec).map(|i| (i as u64).wrapping_mul(2654435761).wrapping_shr(7) as u8).collect()
}
fn stamp(buf: &mut [u8], seq: u64) {
    buf[..8].copy_from_slice(&seq.to_le_bytes());
}
fn matches_record(d: &[u8], tpl: &[u8], seq: u64) -> bool {
    d.len() == tpl.len() && d[..8] == seq.to_le_bytes() && d[8..] == tpl[8..]
}

/// Tampered records must fail; the layer must be dead afterwards. Returns the number of checks.
fn negative_checks() -> u64 {
    let mut n = 0;
    let mut src = RecordLayer::new(Pipe { buf: vec![], rpos: 0 });
    src.install_write_key(&KEY, &IV).unwrap();
    let data = vec![0x5au8; 3000];
    src.write_application_data(&data).unwrap();
    let wire = src.get_ref().buf.clone();
    for i in (0..wire.len()).step_by(37) {
        let mut bad = wire.clone();
        bad[i] ^= 0x10;
        bad.extend_from_slice(&[0; 32]);
        let mut l = RecordLayer::new(Pipe { buf: bad, rpos: 0 });
        l.install_read_key(&KEY, &IV).unwrap();
        assert!(l.read_record().is_err(), "tamper at {i} accepted");
        assert_eq!(l.read_record(), Err(Error::Failed));
        n += 1;
    }
    // Raw EOF without close_notify is an error, not a clean end.
    let mut l = RecordLayer::new(Pipe { buf: wire[..wire.len() - 1].to_vec(), rpos: 0 });
    l.install_read_key(&KEY, &IV).unwrap();
    assert_eq!(l.read_record(), Err(Error::Truncated));
    n + 1
}

fn parse_args(a: &[String]) -> (u64, usize, usize) {
    let scale: u64 = a[2].parse().expect("scale must be 1, 10 or 100");
    assert!(matches!(scale, 1 | 10 | 100), "scale must be 1, 10 or 100");
    let base_mib: u64 = a.get(3).map_or(64, |s| s.parse().expect("base_mib must be a positive integer"));
    let rec: usize = a.get(4).map_or(MAX_PLAINTEXT, |s| s.parse().expect("record_bytes"));
    assert!(base_mib > 0 && rec >= 8 && rec <= MAX_PLAINTEXT, "bad sizes");
    let bytes = base_mib.checked_mul(scale).and_then(|n| n.checked_mul(1 << 20)).expect("workload size overflow");
    (bytes / rec as u64, rec, base_mib as usize)
}

fn backend() -> &'static str {
    if aes_gcm::Aes128Gcm::new(&KEY).is_accelerated() { "accelerated" } else { "portable" }
}

fn run_mem(records: u64, rec: usize) {
    let neg = negative_checks();
    let mut l = RecordLayer::new(Pipe { buf: Vec::with_capacity(8 * MAX_WIRE), rpos: 0 });
    l.install_write_key(&KEY, &IV).unwrap();
    l.install_read_key(&KEY, &IV).unwrap();
    const BATCH: u64 = 8;
    let tpl = template(rec);
    let mut src = tpl.clone();
    let (mut wt, mut rt) = (Duration::ZERO, Duration::ZERO);
    let mut sink = 0u8;
    let cpu0 = cpu_secs();
    let allocs0 = ALLOCS.load(Relaxed);
    let wall0 = Instant::now();
    let mut seq = 0u64;
    while seq < records {
        let n = BATCH.min(records - seq);
        let t0 = Instant::now();
        for s in seq..seq + n {
            stamp(&mut src, s);
            l.write_application_data(&src).unwrap();
        }
        let t1 = Instant::now();
        for s in seq..seq + n {
            match l.read_record().unwrap() {
                Record::ApplicationData(d) => {
                    assert!(matches_record(d, &tpl, s), "record {s} differs");
                    sink ^= d[0];
                }
                o => panic!("{o:?}"),
            }
        }
        let t2 = Instant::now();
        wt += t1 - t0;
        rt += t2 - t1;
        seq += n;
    }
    let allocs = ALLOCS.load(Relaxed) - allocs0;
    let wall = wall0.elapsed().as_secs_f64();
    let cpu = cpu_secs() - cpu0;
    let mb = (records * rec as u64) as f64 / 1e6;
    println!("mem backend={} data={:.0}MB records={} record_bytes={} negative_checks={}", backend(), mb, records, rec, neg);
    println!(
        "write(seal) {:.1} MB/s | read(open+compare) {:.1} MB/s | wall {:.2}s | cpu {:.2}s | peak_rss {} KiB | allocs_in_loop {} | sink {}",
        mb / wt.as_secs_f64(), mb / rt.as_secs_f64(), wall, cpu, peak_rss_kib(), allocs, sink
    );
}

fn run_sock(records: u64, rec: usize) {
    let neg = negative_checks();
    let (a, b) = UnixStream::pair().unwrap();
    let cpu0 = cpu_secs();
    let allocs0 = ALLOCS.load(Relaxed);
    let wall0 = Instant::now();
    let writer = std::thread::spawn(move || {
        let mut l = RecordLayer::new(Counted(a));
        l.install_write_key(&KEY, &IV).unwrap();
        let mut src = template(rec);
        for s in 0..records {
            stamp(&mut src, s);
            l.write_application_data(&src).unwrap();
        }
        l.send_close_notify().unwrap();
    });
    let mut l = RecordLayer::new(Counted(b));
    l.install_read_key(&KEY, &IV).unwrap();
    let tpl = template(rec);
    let mut got = 0u64;
    loop {
        match l.read_record().unwrap() {
            Record::ApplicationData(d) => {
                assert!(matches_record(d, &tpl, got), "record {got} differs");
                got += 1;
            }
            Record::CloseNotify => break, // a clean end: this is the only way the loop ends
            o => panic!("{o:?}"),
        }
    }
    writer.join().unwrap();
    assert_eq!(got, records);
    let wall = wall0.elapsed().as_secs_f64();
    let cpu = cpu_secs() - cpu0;
    let allocs = ALLOCS.load(Relaxed) - allocs0;
    let mb = (records * rec as u64) as f64 / 1e6;
    println!("sock backend={} data={:.0}MB records={} record_bytes={} negative_checks={}", backend(), mb, records, rec, neg);
    println!(
        "{:.1} MB/s | wall {:.2}s | cpu {:.2}s | peak_rss {} KiB | allocs(total, both threads) {} | read_calls {} | write_calls {}",
        mb / wall, wall, cpu, peak_rss_kib(), allocs, READS.load(Relaxed), WRITES.load(Relaxed)
    );
}

// ---- oracle ----

const PY: &str = r#"
import sys, hashlib, random
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
KEY = b"bare-record-key1"; IV = b"record-ld-iv"
g = AESGCM(KEY)
def nonce(seq):
    return bytes(a ^ b for a, b in zip(IV, (0).to_bytes(4, "big") + seq.to_bytes(8, "big")))
enc_seq = 0
for line in sys.stdin:
    p = line.split()
    if p[0] == "ENC":
        # A record from Rust: check header fields, decrypt, strip padding, report type and hash.
        rec = bytes.fromhex(p[1])
        assert rec[0] == 23 and rec[1:3] == b"\x03\x03" and int.from_bytes(rec[3:5], "big") == len(rec) - 5
        pt = g.decrypt(nonce(enc_seq), rec[5:], rec[:5]); enc_seq += 1
        pt = pt.rstrip(b"\x00"); ty, content = pt[-1], pt[:-1]
        print(ty, len(content), hashlib.sha256(content).hexdigest(), flush=True)
    elif p[0] == "GEN":
        n, seed = int(p[1]), int(p[2]); r = random.Random(seed)
        for seq in range(n):
            ty = 21 if seq == n - 1 else r.choice([23, 23, 23, 22])
            if ty == 21: content = bytes([r.choice([1, 2]), 0])  # close_notify, either level byte
            else:
                ln = r.choice([0, 1, 15, 16, 17, 255, 256, 16383, 16384, r.randrange(0, 16385)])
                if ty == 22 and ln == 0: ln = 1
                content = r.randbytes(ln)
            pad = min(r.choice([0, 0, 1, 7, 100, r.randrange(0, 600)]), 16384 + 1 - len(content) - 1)
            inner = content + bytes([ty]) + bytes(pad)
            h = bytes([23, 3, 3]) + (len(inner) + 16).to_bytes(2, "big")
            rec = h + g.encrypt(nonce(seq), inner, h)
            print(rec.hex(), ty, len(content), hashlib.sha256(content).hexdigest(), flush=True)
"#;

fn sha256_hex(d: &[u8]) -> String {
    // The crate's own SHA-256 is already checked against NIST vectors; use it for the digest.
    sha256::sha256(d).iter().map(|b| format!("{b:02x}")).collect()
}

fn run_oracle(n: usize) {
    let mut py = Command::new("python3")
        .args(["-I", "-c", PY])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("python3 with `cryptography` is needed");
    let mut tx = py.stdin.take().unwrap();
    let mut rx = io::BufReader::new(py.stdout.take().unwrap());
    let mut line = String::new();

    // 1. Rust writes, Python decrypts. One record per call so each line is one record.
    let sink = Shared::default();
    let mut l = RecordLayer::new(sink.clone());
    l.install_write_key(&KEY, &IV).unwrap();
    let mut x = 0x1234_5678_9abc_def0u64;
    let mut sent = 0;
    for i in 0..n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let len = match i % 6 {
            0 => 1,
            1 => 16384,
            2 => 16383,
            _ => 1 + (x % 16384) as usize,
        };
        let content: Vec<u8> = (0..len).map(|j| (x >> (j % 56)) as u8 ^ j as u8).collect();
        let hs = i % 7 == 3;
        if hs { l.write_handshake(&content) } else { l.write_application_data(&content) }.unwrap();
        let wire = std::mem::take(&mut *sink.0.borrow_mut());
        writeln!(tx, "ENC {}", wire.iter().map(|b| format!("{b:02x}")).collect::<String>()).unwrap();
        line.clear();
        rx.read_line(&mut line).unwrap();
        let f: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(f[0], if hs { "22" } else { "23" }, "record {i} type");
        assert_eq!(f[1].parse::<usize>().unwrap(), len, "record {i} length");
        assert_eq!(f[2], sha256_hex(&content), "record {i} content");
        sent += 1;
    }
    l.send_close_notify().unwrap();
    let wire = std::mem::take(&mut *sink.0.borrow_mut());
    writeln!(tx, "ENC {}", wire.iter().map(|b| format!("{b:02x}")).collect::<String>()).unwrap();
    line.clear();
    rx.read_line(&mut line).unwrap();
    assert!(line.starts_with("21 2 "), "close_notify: {line}");

    // 2. Python writes (random lengths, zero padding, handshake/app/alert), Rust reads.
    writeln!(tx, "GEN {n} 7").unwrap();
    drop(tx);
    let mut all = vec![];
    let mut expect = vec![];
    for _ in 0..n {
        line.clear();
        rx.read_line(&mut line).unwrap();
        let f: Vec<&str> = line.split_whitespace().collect();
        all.extend((0..f[0].len() / 2).map(|i| u8::from_str_radix(&f[0][2 * i..2 * i + 2], 16).unwrap()));
        expect.push((f[1].to_string(), f[2].parse::<usize>().unwrap(), f[3].to_string()));
    }
    py.wait().unwrap();
    // Alerts stop the stream (close_notify) or kill it; the generator only makes (2,40) and
    // (1,0)/(2,0), so feed one stream per alert boundary: read until the first alert.
    let mut l = RecordLayer::new(Pipe { buf: all, rpos: 0 });
    l.install_read_key(&KEY, &IV).unwrap();
    let mut checked = 0;
    for (ty, len, hash) in &expect {
        match l.read_record() {
            Ok(Record::ApplicationData(d)) => assert!(ty == "23" && d.len() == *len && sha256_hex(d) == *hash),
            Ok(Record::Handshake(d)) => assert!(ty == "22" && d.len() == *len && sha256_hex(d) == *hash),
            Ok(Record::CloseNotify) => {
                assert_eq!(ty, "21");
                break; // after close_notify the layer ignores the rest by design
            }
            Err(Error::PeerAlert(40)) => {
                assert_eq!(ty, "21");
                break;
            }
            Err(e) => panic!("record {checked}: {e:?}"),
        }
        checked += 1;
    }
    println!("oracle ok: {sent} records Rust->OpenSSL (+close_notify); {checked} OpenSSL->Rust records before the first alert, of {n} generated");
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(|s| s.as_str()) {
        Some("mem") if a.len() >= 3 => {
            let (records, rec, _) = parse_args(&a);
            run_mem(records, rec)
        }
        Some("sock") if a.len() >= 3 => {
            let (records, rec, _) = parse_args(&a);
            run_sock(records, rec)
        }
        Some("oracle") => run_oracle(a.get(2).map_or(3000, |s| s.parse().unwrap())),
        _ => {
            eprintln!("usage: {} <mem|sock> <scale: 1|10|100> [base_mib] [record_bytes] | oracle [records]", a[0]);
            std::process::exit(2);
        }
    }
}
