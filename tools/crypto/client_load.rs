//! Standalone interop and load driver for crates/tls/src/client.rs. No crates.
//!
//! The parent crate owns lib.rs, so this file declares the modules itself.
//!
//! Build (add `--cfg 'feature="force-portable"'` for the portable SHA-256 and AES paths):
//!   rustc --edition=2024 -C opt-level=3 tools/crypto/client_load.rs -o <out>
//! Unit tests of client.rs (they need this root to see all modules):
//!   rustc --edition=2024 --test tools/crypto/client_load.rs -o <t> && <t>
//! Modes:
//!   <out> mem <scale: 1|10|100>   no network. Replays a scripted server (client_fixture.rs).
//!         handshakes: 20*scale replays of one fixed flight; then 16 MiB*scale of application
//!         data in (the server side encrypts on the fly, its time and allocations are
//!         subtracted), and the same out to a sink.
//!   <out> interop <handshakes> <mib>   a loopback `openssl s_server` is the oracle. It gets a
//!         synthetic P-256 key made in /dev/shm (mode 0700) that is deleted when the tool exits.
//!         Runs: GET with tickets, <mib> MiB download, echo, and wrong-certificate and
//!         no-ALPN-h2 negative checks. The real clock and OS random are used.
//! The oracle is not linked into bare. No live network is used.

#[allow(dead_code)]
#[path = "../../crates/tls/src/aes_gcm.rs"]
mod aes_gcm;
#[allow(dead_code)]
#[path = "../../crates/tls/src/client.rs"]
mod client;
#[allow(dead_code)]
#[path = "client_fixture.rs"]
mod client_fixture;
#[allow(dead_code)]
#[path = "../../crates/tls/src/der.rs"]
mod der;
#[allow(dead_code)]
#[path = "../../crates/tls/src/ecc.rs"]
mod ecc;
#[allow(dead_code)]
#[path = "../../crates/tls/src/hello.rs"]
mod hello;
#[allow(dead_code)]
#[path = "../../crates/tls/src/hkdf.rs"]
mod hkdf;
#[allow(dead_code)]
#[path = "../../crates/tls/src/hmac.rs"]
mod hmac;
#[allow(dead_code)]
#[path = "../../crates/tls/src/key_schedule.rs"]
mod key_schedule;
#[allow(dead_code)]
#[path = "../../crates/tls/src/random.rs"]
mod random;
#[allow(dead_code)]
#[path = "../../crates/tls/src/record.rs"]
mod record;
#[allow(dead_code)]
#[path = "../../crates/tls/src/sha256.rs"]
mod sha256;
#[cfg(all(target_arch = "x86_64", not(feature = "force-portable")))]
#[path = "../../crates/tls/src/sha256_x86.rs"]
mod sha256_x86;
#[allow(dead_code)]
#[path = "../../crates/tls/src/sha384.rs"]
mod sha384;
#[allow(dead_code)]
#[path = "../../crates/tls/src/x25519.rs"]
mod x25519;
#[allow(dead_code)]
#[path = "../../crates/tls/src/hostname.rs"]
mod hostname;
#[allow(dead_code)]
#[path = "../../crates/tls/src/trust_store.rs"]
mod trust_store;
#[allow(dead_code)]
#[path = "../../crates/tls/src/verify.rs"]
mod verify;
#[allow(dead_code)]
#[path = "../../crates/tls/src/x509.rs"]
mod x509;

use client::{ChainVerifier, ClientStream};
use client_fixture as fx;
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

// ---- measurement ----

struct Counting;
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
/// Set while the scripted server works, so its allocations are not counted.
static PAUSED: AtomicBool = AtomicBool::new(false);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if !PAUSED.load(Relaxed) {
            ALLOCS.fetch_add(1, Relaxed);
            BYTES.fetch_add(l.size() as u64, Relaxed);
        }
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if !PAUSED.load(Relaxed) {
            ALLOCS.fetch_add(1, Relaxed);
            BYTES.fetch_add(n as u64, Relaxed);
        }
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static A: Counting = Counting;

unsafe extern "C" {
    fn clock_gettime(clk: i32, ts: *mut [i64; 2]) -> i32;
}

/// CPU seconds of this process (all threads), with nanosecond resolution.
fn cpu_secs() -> f64 {
    let mut ts = [0i64; 2];
    // SAFETY: CLOCK_PROCESS_CPUTIME_ID is 2 on Linux; `ts` is a valid timespec.
    unsafe { clock_gettime(2, &mut ts) };
    ts[0] as f64 + ts[1] as f64 * 1e-9
}

fn peak_rss_kib() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    let l = s.lines().find(|l| l.starts_with("VmHWM:")).unwrap();
    l.split_whitespace().nth(1).unwrap().parse().unwrap()
}

struct Meter {
    wall: Instant,
    cpu: f64,
    allocs: u64,
    bytes: u64,
}
impl Meter {
    fn start() -> Meter {
        Meter { wall: Instant::now(), cpu: cpu_secs(), allocs: ALLOCS.load(Relaxed), bytes: BYTES.load(Relaxed) }
    }
    /// `exclude` is wall and CPU time spent in the scripted server, taken out of the result.
    fn report(&self, label: &str, units: f64, unit: &str, exclude: f64) {
        let wall = self.wall.elapsed().as_secs_f64() - exclude;
        let cpu = cpu_secs() - self.cpu - exclude;
        let allocs = ALLOCS.load(Relaxed) - self.allocs;
        let bytes = BYTES.load(Relaxed) - self.bytes;
        println!(
            "{label}: wall {wall:.3}s cpu {cpu:.3}s | {:.1} {unit}/s | {:.1} {unit}/cpu-s | allocs {allocs} ({:.2}/unit) alloc bytes {bytes} | peak RSS {} KiB",
            units / wall,
            units / cpu.max(1e-9),
            allocs as f64 / units,
            peak_rss_kib()
        );
    }
}

// ---- verifier: the exact leaf, for one host. Not an allow-all. ----

struct Pin {
    leaf: Vec<u8>,
    host: &'static str,
}
impl ChainVerifier for Pin {
    fn verify_chain(&self, chain: &[&[u8]], host: &str, now: i64) -> Result<(), String> {
        let cert = x509::parse(chain[0]).map_err(|_| "parse".to_string())?;
        if chain.len() != 1 || chain[0] != self.leaf || host != self.host || !cert.valid_at(now) {
            return Err("not the pinned leaf".into());
        }
        Ok(())
    }
}

// ---- mem mode ----

/// A transport that makes the server's bytes on demand: the handshake flight, then
/// encrypted application records, then close_notify. Time spent here is measured.
struct Feed {
    srv: fx::Srv,
    buf: Vec<u8>,
    pos: usize,
    left: usize,
    chunk: usize,
    closed: bool,
    spent: f64,
    cpu: f64,
    sink: u64,
    data: Vec<u8>,
}

impl Read for Feed {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos == self.buf.len() {
            let (t, c) = (Instant::now(), cpu_secs());
            PAUSED.store(true, Relaxed);
            self.buf.clear();
            self.pos = 0;
            while self.buf.len() < 256 * 1024 && self.left > 0 {
                let n = self.left.min(self.chunk);
                self.srv.app(&self.data[..n]);
                self.left -= n;
                self.buf.extend_from_slice(&self.srv.take());
            }
            if self.left == 0 && !self.closed {
                self.srv.close_notify();
                self.buf.extend_from_slice(&self.srv.take());
                self.closed = true;
            }
            PAUSED.store(false, Relaxed);
            self.spent += t.elapsed().as_secs_f64();
            self.cpu += cpu_secs() - c;
            if self.buf.is_empty() {
                return Ok(0);
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}
impl Write for Feed {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.sink += b.len() as u64;
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn mem(scale: usize) {
    let verifier = Pin { leaf: fx::LEAF.to_vec(), host: fx::HOST };
    let rnd = fx::client_random();
    // Handshakes: replay one flight. The flight is made once, outside the timer.
    let mut s = fx::Srv::new();
    s.flight(&fx::ee_alpn(), fx::CV_ALPN);
    let wire = s.take();
    let n = 20 * scale;
    let m = Meter::start();
    for _ in 0..n {
        let c = client::connect_inner(fx::Script::new(wire.clone(), usize::MAX), fx::HOST, &verifier, fx::NOW, &rnd).unwrap();
        std::hint::black_box(&c);
    }
    m.report(&format!("mem handshakes x{n} (incl. one {}-byte wire clone each)", wire.len()), n as f64, "hs", 0.0);

    // Application data in: the server encrypts on the fly.
    let total = 16 * 1024 * 1024 * scale;
    let mut srv = fx::Srv::new();
    srv.flight(&fx::ee_alpn(), fx::CV_ALPN);
    let first = srv.take();
    let feed = Feed {
        srv,
        buf: first,
        pos: 0,
        left: total,
        chunk: 16384,
        closed: false,
        spent: 0.0,
        cpu: 0.0,
        sink: 0,
        data: (0..16384u32).map(|i| (i * 31 >> 3) as u8).collect(),
    };
    let mut c = client::connect_inner(feed, fx::HOST, &verifier, fx::NOW, &rnd).unwrap();
    let mut buf = vec![0u8; 64 * 1024];
    let m = Meter::start();
    let mut got = 0usize;
    let mut sum = 0u64;
    loop {
        let n = c.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        got += n;
        sum = sum.wrapping_add(buf[..n].iter().step_by(4099).map(|&b| b as u64).sum::<u64>());
    }
    assert_eq!(got, total);
    let spent = { let f = c.get_ref(); (f.spent, f.cpu) };
    let wall_total = m.wall.elapsed().as_secs_f64();
    let cpu_total = cpu_secs() - m.cpu;
    println!(
        "mem recv {} MiB: client wall {:.3}s cpu {:.3}s | {:.0} MiB/s wall | allocs {} | server-side (excluded) wall {:.3}s | checksum {sum}",
        total >> 20,
        wall_total - spent.0,
        cpu_total - spent.1,
        (total >> 20) as f64 / (wall_total - spent.0),
        ALLOCS.load(Relaxed) - m.allocs,
        spent.0
    );

    // Application data out: records are sealed and counted by a sink.
    let mut s2 = fx::Srv::new();
    s2.flight(&fx::ee_alpn(), fx::CV_ALPN);
    let feed = Feed { srv: s2, buf: Vec::new(), pos: 0, left: 0, chunk: 1, closed: true, spent: 0.0, cpu: 0.0, sink: 0, data: Vec::new() };
    let mut script = s2_wire(feed);
    let data = vec![0x5au8; 64 * 1024];
    let m = Meter::start();
    for _ in 0..total / data.len() {
        script.write_all(&data).unwrap();
    }
    let sent = script.get_ref().sink;
    m.report(&format!("mem send {} MiB (wire bytes {sent})", total >> 20), (total >> 20) as f64, "MiB", 0.0);
}

fn s2_wire(mut feed: Feed) -> ClientStream<Feed> {
    let verifier = Pin { leaf: fx::LEAF.to_vec(), host: fx::HOST };
    feed.buf = feed.srv.take();
    client::connect_inner(feed, fx::HOST, &verifier, fx::NOW, &fx::client_random()).unwrap()
}

// ---- interop mode ----

/// A directory in /dev/shm (RAM) with the synthetic key. Removed on drop.
struct Ram(PathBuf);
impl Drop for Ram {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn openssl(args: &[&str], dir: &PathBuf) {
    let st = Command::new("openssl").args(args).current_dir(dir).stdout(Stdio::null()).stderr(Stdio::null()).status().expect("openssl");
    assert!(st.success(), "openssl {args:?}");
}

fn make_cert(dir: &PathBuf, name: &str, san: &str) -> Vec<u8> {
    let cnf = format!("[req]\ndistinguished_name=dn\nprompt=no\nx509_extensions=v3\n[dn]\nCN={name}\n[v3]\nsubjectAltName={san}\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\n");
    std::fs::write(dir.join(format!("{name}.cnf")), cnf).unwrap();
    let (key, crt, der) = (format!("{name}.key"), format!("{name}.pem"), format!("{name}.der"));
    openssl(&["ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", &key], dir);
    openssl(&["req", "-new", "-x509", "-key", &key, "-config", &format!("{name}.cnf"), "-sha256", "-days", "2", "-out", &crt], dir);
    openssl(&["x509", "-in", &crt, "-outform", "DER", "-out", &der], dir);
    std::fs::read(dir.join(der)).unwrap()
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_server(dir: &PathBuf, cert: &str, extra: &[&str]) -> (Server, u16) {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let child = Command::new("openssl")
        .args(["s_server", "-accept", &format!("127.0.0.1:{port}"), "-cert", &format!("{cert}.pem"), "-key", &format!("{cert}.key")])
        .args(["-tls1_3", "-ciphersuites", "TLS_AES_128_GCM_SHA256", "-groups", "X25519", "-sigalgs", "ECDSA+SHA256", "-quiet"])
        .args(extra)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("openssl s_server");
    let server = Server(child);
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return (server, port);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("s_server did not start");
}

fn dial(port: u16) -> TcpStream {
    let t = TcpStream::connect(("127.0.0.1", port)).unwrap();
    t.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    t.set_write_timeout(Some(Duration::from_secs(20))).unwrap();
    t.set_nodelay(true).unwrap();
    t
}

fn interop(handshakes: usize, mib: usize) {
    let dir = PathBuf::from(format!("/dev/shm/bare-client-load-{}", std::process::id()));
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
    }
    let _ram = Ram(dir.clone());
    let leaf = make_cert(&dir, "good", "DNS:localhost");
    let other = make_cert(&dir, "other", "DNS:localhost");
    let pin = Pin { leaf: leaf.clone(), host: "localhost" };
    {
        let mut f = std::fs::File::create(dir.join("big.bin")).unwrap();
        let block: Vec<u8> = (0..1024 * 1024u32).map(|i| (i * 7 >> 5) as u8).collect();
        for _ in 0..mib {
            f.write_all(&block).unwrap();
        }
    }

    let (_srv, port) = start_server(&dir, "good", &["-WWW"]);
    // 1. Handshakes with a small download each (OpenSSL sends tickets after the handshake).
    let m = Meter::start();
    for i in 0..handshakes {
        let mut c = client::connect_with(dial(port), "localhost", &pin).unwrap_or_else(|e| panic!("handshake {i}: {e}"));
        assert!(!c.alpn_http11(), "no ALPN offered back");
        c.write_all(b"GET /good.pem HTTP/1.0\r\n\r\n").unwrap();
        let mut body = Vec::new();
        c.read_to_end(&mut body).unwrap();
        assert!(body.windows(10).any(|w| w == b"BEGIN CERT"), "body of {} bytes", body.len());
        c.shutdown().unwrap();
    }
    m.report(&format!("interop {handshakes} handshakes + small GET (wall includes openssl)"), handshakes as f64, "conn", 0.0);

    // 2. Bulk download.
    let mut c = client::connect_with(dial(port), "localhost", &pin).unwrap();
    c.write_all(b"GET /big.bin HTTP/1.0\r\n\r\n").unwrap();
    let m = Meter::start();
    let mut buf = vec![0u8; 64 * 1024];
    let (mut total, mut head) = (0usize, Vec::new());
    loop {
        let n = c.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        total += n;
        if head.len() < 4096 {
            head.extend_from_slice(&buf[..n.min(4096)]);
        }
    }
    let hdr = head.windows(4).position(|w| w == b"\r\n\r\n").expect("http header") + 4;
    assert_eq!(total - hdr, mib * 1024 * 1024, "all bytes arrived before close_notify");
    m.report(&format!("interop download {mib} MiB (wall includes openssl)"), mib as f64, "MiB", 0.0);
    c.shutdown().unwrap();
    drop(c);

    // 3. Upload: write a large request; the server answers 404 to the junk but reads all of it.
    let mut c = client::connect_with(dial(port), "localhost", &pin).unwrap();
    c.write_all(b"GET /nothing HTTP/1.0\r\n").unwrap();
    c.write_all(&vec![b'x'; 1 << 20]).unwrap();
    c.write_all(b"\r\n\r\n").unwrap();
    let mut body = Vec::new();
    c.read_to_end(&mut body).unwrap();
    println!("interop upload 1 MiB + answer {} bytes: ok", body.len());
    c.shutdown().unwrap();
    drop(c);

    // 4. A different certificate (same names, other key) is refused by the verifier before any Finished.
    let (_srv2, port2) = start_server(&dir, "other", &["-WWW"]);
    match client::connect_with(dial(port2), "localhost", &pin) {
        Err(client::Error::Certificate(m)) => println!("interop wrong certificate: refused ({m})"),
        Err(e) => panic!("wrong error {e}"),
        Ok(_) => panic!("wrong certificate accepted"),
    }
    // 5. Wrong host name: refused.
    match client::connect_with(dial(port), "localhost.", &pin) {
        Err(e) => println!("interop trailing-dot host: refused ({e})"),
        Ok(_) => panic!("accepted"),
    }
    // 6. The server prefers h2 and we offered only http/1.1: OpenSSL must not pick h2.
    let (_srv3, port3) = start_server(&dir, "good", &["-WWW", "-alpn", "h2"]);
    match client::connect_with(dial(port3), "localhost", &pin) {
        Err(e) => println!("interop server alpn h2 only: refused ({e})"),
        Ok(c) => println!("interop server alpn h2 only: connected, alpn_http11={}", c.alpn_http11()),
    }
    // 7. Server vs us with a close and no close_notify is a truncation, not EOF: kill the server mid-body.
    let (srv4, port4) = start_server(&dir, "good", &["-WWW"]);
    let mut c = client::connect_with(dial(port4), "localhost", &pin).unwrap();
    c.write_all(b"GET /big.bin HTTP/1.0\r\n\r\n").unwrap();
    let mut first = [0u8; 4096];
    c.read_exact(&mut first).unwrap();
    drop(srv4); // SIGKILL: the TCP connection closes without close_notify
    let mut sink = Vec::new();
    match c.read_to_end(&mut sink) {
        Err(e) => println!("interop kill mid-stream: {:?} after {} bytes", e.kind(), sink.len()),
        Ok(_) => panic!("truncation read as EOF"),
    }
}

/// Where a handshake's CPU goes: the pieces that run once per connection.
fn parts() {
    let n = 2000;
    let rnd = fx::client_random();
    let scalar: [u8; 32] = rnd[..32].try_into().unwrap();
    let peer = x25519::public_key(&[7; 32]);
    let t = Instant::now();
    for _ in 0..n {
        std::hint::black_box(x25519::public_key(&scalar));
        std::hint::black_box(x25519::shared_secret(&scalar, &peer).unwrap());
    }
    println!("x25519 public_key + shared_secret: {:.1} us", t.elapsed().as_secs_f64() / n as f64 * 1e6);
    let cert = x509::parse(fx::LEAF).unwrap();
    let x509::PublicKey::Ec { point, .. } = cert.public_key else { panic!() };
    let es = x509::EcdsaSig::parse(fx::CV_ALPN, x509::Curve::P256).unwrap();
    let mut rs = [0u8; 64];
    rs[32 - es.r.len()..32].copy_from_slice(es.r);
    rs[64 - es.s.len()..].copy_from_slice(es.s);
    // The signature is over another digest: verification fails, but runs the same arithmetic.
    let t = Instant::now();
    for _ in 0..n {
        let _ = std::hint::black_box(ecc::verify(ecc::Curve::P256, point, &[1u8; 32], &rs));
    }
    println!("P-256 verify (full arithmetic): {:.1} us", t.elapsed().as_secs_f64() / n as f64 * 1e6);
    let t = Instant::now();
    for _ in 0..n {
        std::hint::black_box(x509::parse(fx::LEAF).unwrap());
    }
    println!("x509::parse leaf: {:.2} us", t.elapsed().as_secs_f64() / n as f64 * 1e6);
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("mem") => mem(a.get(2).and_then(|s| s.parse().ok()).unwrap_or(1)),
        Some("parts") => parts(),
        Some("interop") => interop(a.get(2).and_then(|s| s.parse().ok()).unwrap_or(20), a.get(3).and_then(|s| s.parse().ok()).unwrap_or(16)),
        _ => eprintln!("usage: client_load mem <1|10|100> | interop <handshakes> <mib>"),
    }
}
