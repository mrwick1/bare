//! Test harness: a synthetic CA and a loopback `openssl s_server` (TLS 1.3). Included with
//! `#[path]` by `crates/tls/tests/client_openssl.rs` and `crates/net/tests/https_openssl.rs`.
//!
//! All keys live in a private directory in /dev/shm (RAM, mode 0700). `Pki` deletes it on drop.
//! No key is in the repository. No test here uses the network beyond 127.0.0.1.
//! `openssl` is the independent TLS peer. It is not linked into bare.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub fn openssl_available() -> bool {
    Command::new("openssl").arg("version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// A root key and root certificate, and the leaves it signed. Removed from RAM on drop.
pub struct Pki {
    pub dir: PathBuf,
}

impl Drop for Pki {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Pki {
    pub fn new() -> Pki {
        let dir = PathBuf::from(format!("/dev/shm/bare-https-test-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let pki = Pki { dir };
        pki.run(&["ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", "root.key"]);
        pki.run(&[
            "req", "-x509", "-new", "-key", "root.key", "-sha256", "-days", "2", "-subj", "/CN=Bare Test Root", "-addext",
            "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign", "-out", "root.pem",
        ]);
        pki
    }

    fn run(&self, args: &[&str]) {
        let out = Command::new("openssl").args(args).current_dir(&self.dir).output().expect("openssl");
        assert!(out.status.success(), "openssl {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// The root certificate in PEM: what a client trusts.
    pub fn root_pem(&self) -> Vec<u8> {
        std::fs::read(self.dir.join("root.pem")).unwrap()
    }

    /// A P-256 leaf `name` (files `name.pem`, `name.key`) for the DNS name `san`. `window` is
    /// `(notBefore, notAfter)` as `YYYYMMDDHHMMSSZ`. The default is valid for a day from now.
    pub fn leaf(&self, name: &str, san: &str, window: Option<(&str, &str)>) {
        let (key, csr, pem, ext) = (format!("{name}.key"), format!("{name}.csr"), format!("{name}.pem"), format!("{name}.ext"));
        std::fs::write(
            self.dir.join(&ext),
            format!("subjectAltName=DNS:{san}\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\n"),
        )
        .unwrap();
        self.run(&["ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", &key]);
        self.run(&["req", "-new", "-key", &key, "-subj", &format!("/CN={name}"), "-out", &csr]);
        let mut args = vec!["x509", "-req", "-in", &csr, "-CA", "root.pem", "-CAkey", "root.key", "-CAcreateserial", "-sha256", "-extfile", &ext, "-out", &pem];
        match window {
            Some((from, to)) => args.extend(["-not_before", from, "-not_after", to]),
            None => args.extend(["-days", "1"]),
        }
        self.run(&args);
    }
}

/// A loopback `openssl s_server`. It takes one connection at a time. Killed on drop.
/// `scripted`: what the client sends (application data) comes to `expect`, and `send` sends bytes
/// to the client. Not scripted: the server runs with the extra arguments (for `-WWW`) and the
/// files in the PKI directory.
pub struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    out: Option<Receiver<Vec<u8>>>,
    seen: Vec<u8>,
    pub port: u16,
    dir: PathBuf,
    leaf: String,
    extra: Vec<String>,
    scripted: bool,
}

impl Drop for Server {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    pub fn start(pki: &Pki, leaf: &str, extra: &[&str], scripted: bool) -> Server {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let extra: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
        let (child, stdin, out) = Self::spawn(&pki.dir, leaf, &extra, scripted, port);
        Server { child, stdin, out, seen: Vec::new(), port, dir: pki.dir.clone(), leaf: leaf.to_string(), extra, scripted }
    }

    fn spawn(dir: &PathBuf, leaf: &str, extra: &[String], scripted: bool, port: u16) -> (Child, Option<ChildStdin>, Option<Receiver<Vec<u8>>>) {
        let mut child = Command::new("openssl")
            .args(["s_server", "-accept", &format!("127.0.0.1:{port}"), "-cert", &format!("{leaf}.pem"), "-key", &format!("{leaf}.key")])
            .args(["-tls1_3", "-ciphersuites", "TLS_AES_128_GCM_SHA256", "-groups", "X25519", "-sigalgs", "ECDSA+SHA256", "-quiet"])
            .args(extra)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(if scripted { Stdio::piped() } else { Stdio::null() })
            .stderr(Stdio::null())
            .spawn()
            .expect("openssl s_server");
        let stdin = child.stdin.take();
        let out = child.stdout.take().map(|mut o| {
            let (tx, rx) = channel();
            std::thread::spawn(move || {
                let mut buf = [0u8; 16384];
                while let Ok(n) = o.read(&mut buf) {
                    if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            });
            rx
        });
        for _ in 0..100 {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return (child, stdin, out);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("s_server did not start");
    }

    /// End the connection the hard way: kill the server and start a new one on the same port.
    /// The client sees a close with no `close_notify`. Whatever the old server had not yet read is lost.
    pub fn reset(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.seen.clear();
        let (child, stdin, out) = Self::spawn(&self.dir, &self.leaf, &self.extra, self.scripted, self.port);
        self.child = child;
        self.stdin = stdin;
        self.out = out;
    }

    /// Bytes from the client until `pat` is in them. Gives everything up to the end of `pat`.
    pub fn expect(&mut self, pat: &[u8]) -> Vec<u8> {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(i) = self.seen.windows(pat.len()).position(|w| w == pat) {
                return self.seen.drain(..i + pat.len()).collect();
            }
            let left = end.saturating_duration_since(Instant::now());
            match self.out.as_ref().unwrap().recv_timeout(left) {
                Ok(b) => self.seen.extend_from_slice(&b),
                Err(e) => panic!("waiting for {:?}: {e:?}; have {:?}", String::from_utf8_lossy(pat), String::from_utf8_lossy(&self.seen)),
            }
        }
    }

    /// Exactly `n` bytes from the client.
    pub fn take(&mut self, n: usize) -> Vec<u8> {
        let end = Instant::now() + Duration::from_secs(20);
        while self.seen.len() < n {
            let left = end.saturating_duration_since(Instant::now());
            match self.out.as_ref().unwrap().recv_timeout(left) {
                Ok(b) => self.seen.extend_from_slice(&b),
                Err(e) => panic!("waiting for {n} bytes: {e:?}; have {}", self.seen.len()),
            }
        }
        self.seen.drain(..n).collect()
    }

    /// Everything the client sends in the next `ms` milliseconds (and what was held back).
    pub fn collect(&mut self, ms: u64) -> Vec<u8> {
        let end = Instant::now() + Duration::from_millis(ms);
        loop {
            match self.out.as_ref().unwrap().recv_timeout(end.saturating_duration_since(Instant::now())) {
                Ok(b) => self.seen.extend_from_slice(&b),
                Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => return std::mem::take(&mut self.seen),
            }
        }
    }

    /// Send bytes to the client (one TLS record each time, at most).
    pub fn send(&mut self, bytes: &[u8]) {
        let s = self.stdin.as_mut().unwrap();
        s.write_all(bytes).unwrap();
        s.flush().unwrap();
    }
}
