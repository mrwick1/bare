//! Bounded local HTTPS load for `bare_net::http::Client` with real certificate validation.
//!
//!   cargo run --release -p bare-net --example https_load -- <mode> <scale: 1|10|100>
//!
//! Modes (each asserts status, body bytes and checksums; run each in its own process):
//!   hs    `20*scale` GETs, each on a new connection (an `openssl s_server -WWW` closes after each reply).
//!   ka    `200*scale` GETs and `100*scale` 1 KiB POSTs on ONE connection (a Python `ssl` server), plus redirects.
//!   big   download `16*scale` MiB with `get_stream` (Content-Length), and the same chunked.
//!   post  `scale` POSTs of 8 MiB from one slice; the server answers with the length and byte sum.
//! The synthetic CA and keys live in /dev/shm (mode 0700) and are deleted on exit. The only peer is
//! 127.0.0.1. Wall time includes the server, which runs in another process. CPU is this process only.

#[path = "../../../tools/crypto/openssl_harness.rs"]
mod harness;

use bare_net::http::{Client, Request};
use bare_tls::trust_store;
use bare_url::Url;
use harness::{Pki, Server};
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

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

unsafe extern "C" {
    fn clock_gettime(clk: i32, ts: *mut [i64; 2]) -> i32;
}
fn cpu_secs() -> f64 {
    let mut ts = [0i64; 2];
    // SAFETY: CLOCK_PROCESS_CPUTIME_ID is 2 on Linux; `ts` is a valid timespec.
    unsafe { clock_gettime(2, &mut ts) };
    ts[0] as f64 + ts[1] as f64 * 1e-9
}
fn status_kib(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    s.lines().find(|l| l.starts_with(key)).unwrap().split_whitespace().nth(1).unwrap().parse().unwrap()
}

struct Meter(Instant, f64, u64, u64);
impl Meter {
    fn start() -> Meter {
        Meter(Instant::now(), cpu_secs(), ALLOCS.load(Relaxed), BYTES.load(Relaxed))
    }
    fn report(&self, label: &str, units: f64, unit: &str) {
        let wall = self.0.elapsed().as_secs_f64();
        let cpu = cpu_secs() - self.1;
        let allocs = ALLOCS.load(Relaxed) - self.2;
        println!(
            "{label}: wall {wall:.3}s cpu {cpu:.3}s | {:.1} {unit}/s wall | {:.1} {unit}/cpu-s | allocs {allocs} ({:.1}/{unit}) alloc bytes {} | peak RSS {} KiB",
            units / wall,
            units / cpu.max(1e-9),
            allocs as f64 / units,
            BYTES.load(Relaxed) - self.3,
            status_kib("VmHWM:")
        );
    }
}

struct Py(Child);
impl Drop for Py {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn python_server(pki: &Pki) -> (Py, u16) {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    // The script runs from the key directory, so the keys are relative paths and never on a command line with a path.
    std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/https_keepalive_server.py"), pki.dir.join("server.py")).unwrap();
    let py = Py(Command::new("python3")
        .args(["-I", "server.py", "good.pem", "good.key", &port.to_string()])
        .current_dir(&pki.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("python3"));
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return (py, port);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("python server did not start");
}

fn pattern_block() -> Vec<u8> {
    (0..64 * 1024usize).map(|i| ((i * 7) >> 3) as u8).collect()
}

fn url(port: u16, path: &str) -> Url {
    Url::parse(&format!("https://localhost:{port}{path}")).unwrap()
}

fn byte_sum(data: &[u8]) -> u64 {
    data.iter().map(|&b| b as u64).sum()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mode = a.get(1).map(String::as_str).unwrap_or("");
    let scale: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    if !harness::openssl_available() {
        eprintln!("openssl is not installed");
        std::process::exit(2);
    }
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    let roots = trust_store::decode_pem(&pki.root_pem()).unwrap();
    let mut client = Client::with_roots(roots);
    match mode {
        "hs" => {
            std::fs::write(pki.dir.join("small.txt"), b"ok").unwrap();
            let srv = Server::start(&pki, "good", &["-WWW"], false);
            let n = 20 * scale;
            let m = Meter::start();
            for i in 0..n {
                let r = client.get(&url(srv.port, "/small.txt")).unwrap_or_else(|e| panic!("request {i}: {e}"));
                assert!(r.status == 200 && r.body == b"ok", "request {i}: {} {:?}", r.status, r.body);
            }
            m.report(&format!("hs {n} GETs, one new TLS connection each (server: openssl -WWW)"), n as f64, "req");
        }
        "ka" => {
            let (_py, port) = python_server(&pki);
            let n = 200 * scale;
            let m = Meter::start();
            for i in 0..n {
                let r = client.get(&url(port, "/small")).unwrap_or_else(|e| panic!("GET {i}: {e}"));
                assert!(r.status == 200 && r.body == b"ok");
            }
            m.report(&format!("ka {n} GETs on one reused connection"), n as f64, "req");
            let body: Vec<u8> = (0..1024u32).map(|i| (i * 13) as u8).collect();
            let want = format!("1024 {}", byte_sum(&body));
            let n = 100 * scale;
            let m = Meter::start();
            for i in 0..n {
                let r = client.post(&url(port, "/sum"), &[("Content-Type", "application/octet-stream")], &body).unwrap_or_else(|e| panic!("POST {i}: {e}"));
                assert!(r.status == 200 && r.body == want.as_bytes(), "POST {i}: {:?}", String::from_utf8_lossy(&r.body));
            }
            m.report(&format!("ka {n} POSTs of 1 KiB on one reused connection"), n as f64, "req");
            let m = Meter::start();
            for i in 0..n {
                let r = client.get(&url(port, "/redirect")).unwrap_or_else(|e| panic!("redirect {i}: {e}"));
                assert!(r.status == 200 && r.body == b"ok");
            }
            m.report(&format!("ka {n} GETs through a 302"), n as f64, "req");
        }
        "big" => {
            // One transfer is at most 64 MiB: `Client` refuses a Content-Length over 128 MiB (MAX_REPLY).
            let (_py, port) = python_server(&pki);
            let block = pattern_block();
            let total_mib = 16 * scale;
            for (label, route) in [("Content-Length", "big"), ("chunked", "chunked")] {
                let m = Meter::start();
                let mut left = total_mib;
                while left > 0 {
                    let mib = left.min(64);
                    left -= mib;
                    let mut s = client.get_stream(&url(port, &format!("/{route}/{mib}"))).unwrap();
                    assert_eq!(s.response.status, 200);
                    let mut buf = vec![0u8; 64 * 1024];
                    let (mut total, mut off) = (0usize, 0usize);
                    loop {
                        let n = s.body.read(&mut buf).unwrap();
                        if n == 0 {
                            break;
                        }
                        // Check every byte against the pattern (it repeats every 64 KiB).
                        for (i, b) in buf[..n].iter().enumerate() {
                            if *b != block[(off + i) % block.len()] {
                                panic!("byte {} differs", total + i);
                            }
                        }
                        off = (off + n) % block.len();
                        total += n;
                    }
                    assert_eq!(total, mib << 20, "{label}");
                }
                m.report(&format!("big {total_mib} MiB streamed ({label}, transfers of at most 64 MiB)"), total_mib as f64, "MiB");
            }
        }
        "post" => {
            // `scale` POSTs of 8 MiB from one slice: the tool holds 8 MiB, not the whole total.
            let (_py, port) = python_server(&pki);
            let body: Vec<u8> = (0..8usize << 20).map(|i| ((i * 31) >> 5) as u8).collect();
            let want = format!("{} {}", body.len(), byte_sum(&body) & 0xFFFF_FFFF);
            let u = url(port, "/sum");
            let m = Meter::start();
            for i in 0..scale {
                let r = client.send(&Request::post(&u, &[], &body)).unwrap_or_else(|e| panic!("POST {i}: {e}"));
                assert!(r.status == 200 && r.body == want.as_bytes(), "{:?} want {want}", String::from_utf8_lossy(&r.body));
            }
            m.report(&format!("post {} MiB as {scale} POSTs of 8 MiB from a borrowed slice", 8 * scale), 8.0 * scale as f64, "MiB");
        }
        _ => eprintln!("usage: https_load <hs|ka|big|post> <1|10|100>"),
    }
}
