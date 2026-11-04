//! Standalone test, load and interop driver for key_schedule.rs and hello.rs. No crates.
//! The parent owns lib.rs, so this file includes the modules by path at its crate root.
//!
//! Unit tests (module tests run too):
//!   rustc --edition=2024 --test tools/crypto/hello_load.rs -o <out> && <out>
//!   add `-C opt-level=3` for release; add `--cfg 'feature="force-portable"'` for portable SHA-256.
//! Load build: rustc --edition=2024 -C opt-level=3 tools/crypto/hello_load.rs -o <out>
//! Run:   <out> load <scale: 1|10|100> [base_iters=1000]
//!        <out> interop <port>      (127.0.0.1 only; needs a local `openssl s_server -tls1_3`)
//!
//! load: each iteration is the CPU work of one client handshake without a socket: X25519 key
//! generation, ClientHello encode, ServerHello parse, shared secret, transcript hashing of
//! 3 messages (about 4 KiB), handshake and application secrets, both Finished values, key and
//! IV for 4 secrets, and one key update. The next iteration's inputs depend on this one's
//! output, so nothing can be skipped. Prints wall, CPU, iterations per second, peak RSS and
//! the heap allocations made inside the timed loop.
//!
//! interop: connects to a local OpenSSL server (the oracle, never linked into bare). Sends our
//! ClientHello, parses the ServerHello, decrypts every handshake record with our derived keys,
//! checks the server Finished with our verify_data, sends our Finished, then exchanges one
//! HTTP request under the application keys. A wrong secret anywhere makes a tag fail.

#[allow(dead_code)]
#[path = "../../crates/tls/src/sha256.rs"]
mod sha256;
#[cfg(all(target_arch = "x86_64", not(feature = "force-portable")))]
#[allow(dead_code)]
#[path = "../../crates/tls/src/sha256_x86.rs"]
mod sha256_x86;
#[allow(dead_code)]
#[path = "../../crates/tls/src/hmac.rs"]
mod hmac;
#[allow(dead_code)]
#[path = "../../crates/tls/src/hkdf.rs"]
mod hkdf;
#[allow(dead_code)]
#[path = "../../crates/tls/src/x25519.rs"]
mod x25519;
#[allow(dead_code)]
#[path = "../../crates/tls/src/random.rs"]
mod random;
#[allow(dead_code)]
#[path = "../../crates/tls/src/aes_gcm.rs"]
mod aes_gcm;
#[allow(dead_code)]
#[path = "../../crates/tls/src/key_schedule.rs"]
mod key_schedule;
#[allow(dead_code)]
#[path = "../../crates/tls/src/hello.rs"]
mod hello;

use hello::*;
use key_schedule::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Read, Write};
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

const SH_MAX: usize = 4 + 2 + 32 + 1 + 32 + 2 + 1 + 2 + 6 + 40;

/// A ServerHello for the load test, built by hand: same shape as a real one. No heap.
fn server_hello(sid: &[u8; 32], public: &[u8; 32], out: &mut [u8; SH_MAX]) -> usize {
    let mut n = 0;
    let mut put = |d: &[u8]| {
        out[n..n + d.len()].copy_from_slice(d);
        n += d.len();
    };
    put(&[2, 0, 0, 0, 3, 3]);
    put(&[0x5a; 32]);
    put(&[32]);
    put(sid);
    put(&[0x13, 1, 0]);
    put(&[0, 46, 0, 0x33, 0, 36, 0, 0x1d, 0, 32]);
    put(public);
    put(&[0, 0x2b, 0, 2, 3, 4]);
    out[3] = (n - 4) as u8;
    n
}

fn load(scale: u64, base: u64) {
    let iters = base.checked_mul(scale).expect("workload size overflow");
    let server_secret = [0x42u8; 32];
    let server_pub = x25519::public_key(&server_secret);
    let mut sid = [0u8; 32];
    let mut client_secret = [7u8; 32];
    let mut sh_buf = [0u8; SH_MAX];
    let mut ch = [0u8; MAX_CLIENT_HELLO_LEN];
    let fake_cert = [0xc3u8; 2400]; // stands for EncryptedExtensions + Certificate + CertificateVerify
    let mut acc = 0u8;
    let cpu0 = cpu_secs(); // allocates, so read the counter after it
    let (alloc0, t0) = (ALLOCS.load(Relaxed), Instant::now());
    for i in 0..iters {
        sid[..8].copy_from_slice(&i.to_le_bytes());
        let sh_len = server_hello(&sid, &server_pub, &mut sh_buf); // this iteration's session ID
        let public = x25519::public_key(&client_secret);
        let n = encode_client_hello(
            &ClientHelloParams { random: &[(i & 0xff) as u8; 32], session_id: &sid, x25519_public: &public, server_name: b"staging.app.8vox.ai" },
            &mut ch,
        )
        .unwrap();
        let sh = parse_server_hello(&sh_buf[..sh_len], &sid).unwrap();
        let shared = x25519::shared_secret(&client_secret, &sh.x25519_public).unwrap();
        let mut t = Transcript::new();
        t.update(&ch[..n]);
        t.update(&sh_buf[..sh_len]);
        let hello_hash = t.hash();
        let hs = HandshakeSecret::from_ecdhe(&shared);
        let c_hs = hs.client_traffic(&hello_hash);
        let s_hs = hs.server_traffic(&hello_hash);
        let ks = s_hs.traffic_keys();
        let kc = c_hs.traffic_keys();
        t.update(&fake_cert);
        let sf = s_hs.finished_verify_data(&t.hash());
        assert!(s_hs.verify_finished(&t.hash(), &sf));
        t.update(&[FINISHED, 0, 0, 32]);
        t.update(&sf);
        let ap_hash = t.hash();
        let cf = c_hs.finished_verify_data(&ap_hash);
        let master = hs.into_master();
        let c_ap = master.client_traffic(&ap_hash).update();
        let s_ap = master.server_traffic(&ap_hash);
        let ka = c_ap.traffic_keys();
        let kb = s_ap.traffic_keys();
        acc ^= ks.key[0] ^ kc.iv[1] ^ cf[2] ^ ka.key[3] ^ kb.iv[4];
        client_secret = shared; // chain: the next iteration depends on this one
    }
    let wall = t0.elapsed().as_secs_f64();
    let allocs = ALLOCS.load(Relaxed) - alloc0; // before cpu_secs(), which allocates
    let cpu = cpu_secs() - cpu0;
    println!("final {acc:02x} {:02x}{:02x}", client_secret[0], client_secret[1]);
    println!(
        "scale {scale}x: {iters} handshake-CPU iterations wall {wall:.3}s cpu {cpu:.2}s  {:.0} iter/s  {:.1} us/iter  peak RSS {} KiB  allocs in loop {allocs}",
        iters as f64 / wall,
        wall * 1e6 / iters as f64,
        peak_rss_kib()
    );
}

// ---- interop with a local OpenSSL server -------------------------------------------------

fn read_record(s: &mut std::net::TcpStream) -> (u8, [u8; 5], Vec<u8>) {
    let mut h = [0u8; 5];
    s.read_exact(&mut h).expect("read record header");
    let len = u16::from_be_bytes([h[3], h[4]]) as usize;
    assert!(len <= 16384 + 256, "record too long");
    let mut b = vec![0u8; len];
    s.read_exact(&mut b).expect("read record body");
    (h[0], h, b)
}

fn seal(keys: &TrafficKeys, seq: u64, inner_type: u8, data: &[u8]) -> Vec<u8> {
    let g = aes_gcm::Aes128Gcm::new(&keys.key);
    let mut body = data.to_vec();
    body.push(inner_type);
    let len = body.len() + 16;
    let hdr = [23, 3, 3, (len >> 8) as u8, len as u8];
    let tag = g.seal_in_place_detached(&aes_gcm::tls13_nonce(&keys.iv, seq), &hdr, &mut body).unwrap();
    let mut out = hdr.to_vec();
    out.extend(body);
    out.extend(tag);
    out
}

fn open(keys: &TrafficKeys, seq: u64, hdr: &[u8; 5], body: &[u8]) -> (u8, Vec<u8>) {
    let g = aes_gcm::Aes128Gcm::new(&keys.key);
    let (ct, tag) = body.split_at(body.len() - 16);
    let mut pt = ct.to_vec();
    g.open_in_place_detached(&aes_gcm::tls13_nonce(&keys.iv, seq), hdr, &mut pt, tag.try_into().unwrap()).expect("record tag failed: wrong keys");
    while pt.last() == Some(&0) {
        pt.pop();
    }
    let t = pt.pop().expect("empty inner plaintext");
    (t, pt)
}

fn interop(port: u16) {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect loopback");
    let mut secret = [0u8; 32];
    random::fill(&mut secret).unwrap();
    let public = x25519::public_key(&secret);
    let mut sid = [0u8; 32];
    random::fill(&mut sid).unwrap();
    let mut rnd = [0u8; 32];
    random::fill(&mut rnd).unwrap();
    let mut ch = [0u8; MAX_CLIENT_HELLO_LEN];
    let n = encode_client_hello(&ClientHelloParams { random: &rnd, session_id: &sid, x25519_public: &public, server_name: b"localhost" }, &mut ch).unwrap();
    let mut rec = vec![22, 3, 1, (n >> 8) as u8, n as u8];
    rec.extend_from_slice(&ch[..n]);
    s.write_all(&rec).unwrap();

    let (t, _, body) = read_record(&mut s);
    assert_eq!(t, 22, "expected a handshake record");
    let (sh_msg, rest) = next_message(&body, 1024).unwrap();
    assert!(rest.is_empty());
    let sh = parse_server_hello(sh_msg, &sid).unwrap();
    println!("ServerHello accepted ({} bytes)", sh_msg.len());
    let shared = x25519::shared_secret(&secret, &sh.x25519_public).unwrap();
    let mut tr = Transcript::new();
    tr.update(&ch[..n]);
    tr.update(sh_msg);
    let hs = HandshakeSecret::from_ecdhe(&shared);
    let h = tr.hash();
    let c_hs = hs.client_traffic(&h);
    let s_hs = hs.server_traffic(&h);
    let (sk, ck) = (s_hs.traffic_keys(), c_hs.traffic_keys());

    let (mut seq, mut got_finished, mut names) = (0u64, false, Vec::new());
    let mut pending: Vec<u8> = Vec::new();
    while !got_finished {
        let (t, hdr, body) = read_record(&mut s);
        if t == 20 {
            continue; // middlebox-compatibility ChangeCipherSpec
        }
        assert_eq!(t, 23);
        let (inner, pt) = open(&sk, seq, &hdr, &body);
        seq += 1;
        assert_eq!(inner, 22, "expected handshake inside");
        pending.extend(pt);
        while let Ok((m, _)) = next_message(&pending, 1 << 20) {
            let (ty, len) = (m[0], m.len());
            names.push(ty);
            if ty == FINISHED {
                assert!(s_hs.verify_finished(&tr.hash(), &m[4..]), "server Finished does not verify");
                got_finished = true;
            }
            tr.update(m);
            pending.drain(..len);
            if got_finished {
                break;
            }
        }
    }
    println!("server handshake messages {names:?}; server Finished verified");
    let ap_hash = tr.hash();
    let mut fin = [FINISHED, 0, 0, 32].to_vec();
    fin.extend_from_slice(&c_hs.finished_verify_data(&ap_hash));
    s.write_all(&seal(&ck, 0, 22, &fin)).unwrap();
    let master = hs.into_master();
    let (ca, sa) = (master.client_traffic(&ap_hash).traffic_keys(), master.server_traffic(&ap_hash).traffic_keys());
    s.write_all(&seal(&ca, 0, 23, b"GET / HTTP/1.0\r\n\r\n")).unwrap();
    let mut seq = 0;
    let mut text = Vec::new();
    loop {
        let (t, hdr, body) = read_record(&mut s);
        assert_eq!(t, 23);
        // Post-handshake NewSessionTicket records may come first; both are under the server application keys.
        let (inner, pt) = open(&sa, seq, &hdr, &body);
        seq += 1;
        if inner == 23 {
            text.extend(pt);
            break;
        }
    }
    println!("application data under our keys: {:?}", String::from_utf8_lossy(&text[..text.len().min(40)]));
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("load") if (3..=4).contains(&a.len()) => {
            let scale: u64 = a[2].parse().expect("scale must be 1, 10 or 100");
            assert!(matches!(scale, 1 | 10 | 100), "scale must be 1, 10 or 100");
            let base = a.get(3).map_or(1000, |s| s.parse().expect("base_iters must be a positive integer"));
            assert!(base > 0, "base_iters must be positive");
            load(scale, base);
        }
        Some("interop") if a.len() == 3 => interop(a[2].parse().expect("port")),
        _ => {
            eprintln!("usage: {} load <1|10|100> [base_iters] | interop <port>", a[0]);
            std::process::exit(2);
        }
    }
}
