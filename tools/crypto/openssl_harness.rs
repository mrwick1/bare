//! Test harness: a synthetic CA and a loopback `openssl s_server` (TLS 1.3). Included with
//! `#[path]` by `crates/tls/tests/client_openssl.rs` and `crates/net/tests/https_openssl.rs`.
//!
//! All keys live in a private directory in /dev/shm (RAM, mode 0700). `Pki` deletes it on drop.
//! No key is in the repository. No test here uses the network beyond 127.0.0.1.
//! `openssl` is the independent TLS peer. It is not linked into bare.
#![allow(dead_code)]

use std::cell::RefCell;
use std::io::{Read, Write};
use std::net::TcpListener;
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::panic::Location;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

static COUNTER: AtomicUsize = AtomicUsize::new(0);
/// How much of `s_server`'s stderr a timeout report keeps. The reader goes on draining past this, so the pipe never fills.
const STDERR_KEEP: usize = 4096;

/// What the client thread of this test did. `watch_client` makes it; the client thread fills it when it returns.
pub type ClientNote = Arc<Mutex<Option<String>>>;
type Spawned = (Child, Option<ChildStdin>, Option<Receiver<Vec<u8>>>, Arc<Mutex<Vec<u8>>>);

thread_local! {
    // A test thread starts the client thread and then calls `expect`, so the note is per test thread.
    static CLIENT: RefCell<Option<ClientNote>> = const { RefCell::new(None) };
}

/// Make a note for the client thread that this test is about to start. Its text is in the report of a `Server` timeout.
pub fn watch_client() -> ClientNote {
    let note = ClientNote::default();
    CLIENT.with(|c| *c.borrow_mut() = Some(note.clone()));
    note
}

/// Stop the formatter once the diagnostic has 300 characters.
pub fn client_result(value: &impl std::fmt::Debug) -> String {
    use std::fmt::Write as _;
    struct Limited { text: String, left: usize }
    impl std::fmt::Write for Limited {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            for ch in text.chars() {
                if self.left == 0 { return Err(std::fmt::Error); }
                self.text.push(ch);
                self.left -= 1;
            }
            Ok(())
        }
    }
    let mut out = Limited { text: String::new(), left: 300 };
    let _ = write!(&mut out, "{value:?}");
    out.text
}

/// Linux test harness: match the listening socket inode to the child's open descriptors.
fn owns_listener(pid: u32, port: u16) -> std::io::Result<bool> {
    let table = std::fs::read_to_string(format!("/proc/{pid}/net/tcp"))?;
    let address = format!("{:08X}:{port:04X}", u32::from_ne_bytes([127, 0, 0, 1]));
    let inodes: Vec<&str> = table.lines().skip(1).filter_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        (fields.get(1) == Some(&address.as_str()) && fields.get(3) == Some(&"0A"))
            .then(|| fields.get(9).copied()).flatten()
    }).collect();
    for fd in std::fs::read_dir(format!("/proc/{pid}/fd"))? {
        if let Ok(target) = std::fs::read_link(fd?.path())
            && inodes.iter().any(|inode| target.as_os_str() == std::ffi::OsStr::new(&format!("socket:[{inode}]"))) {
            return Ok(true);
        }
    }
    Ok(false)
}

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
        Pki::with_root(&["ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", "root.key"])
    }

    /// Like `new`, with an RSA-2048 root (certificates signed with sha256WithRSAEncryption).
    pub fn new_rsa() -> Pki {
        Pki::with_root(&["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", "root.key"])
    }

    fn with_root(keygen: &[&str]) -> Pki {
        let dir = PathBuf::from(format!("/dev/shm/bare-https-test-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
        let mut b = std::fs::DirBuilder::new();
        // Only Linux has /dev/shm, and the tests that use this harness run on Linux only. The mode keeps this code building elsewhere.
        #[cfg(unix)]
        b.mode(0o700);
        b.create(&dir).unwrap();
        let pki = Pki { dir };
        pki.run(keygen);
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
        self.leaf_with(name, san, window, &["ecparam", "-name", "prime256v1", "-genkey", "-noout"], "sha256");
    }

    /// An RSA leaf of `bits` bits, signed by the root with the hash `md` (`sha256` or `sha384`).
    pub fn rsa_leaf(&self, name: &str, san: &str, bits: u32, md: &str) {
        let opt = format!("rsa_keygen_bits:{bits}");
        self.leaf_with(name, san, None, &["genpkey", "-algorithm", "RSA", "-pkeyopt", &opt], md);
    }

    fn leaf_with(&self, name: &str, san: &str, window: Option<(&str, &str)>, keygen: &[&str], md: &str) {
        let (key, csr, pem, ext) = (format!("{name}.key"), format!("{name}.csr"), format!("{name}.pem"), format!("{name}.ext"));
        std::fs::write(
            self.dir.join(&ext),
            format!("subjectAltName=DNS:{san}\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\n"),
        )
        .unwrap();
        let mut args = keygen.to_vec();
        args.extend(["-out", &key]);
        self.run(&args);
        self.run(&["req", "-new", "-key", &key, "-subj", &format!("/CN={name}"), "-out", &csr]);
        let md = format!("-{md}");
        let mut args = vec!["x509", "-req", "-in", &csr, "-CA", "root.pem", "-CAkey", "root.key", "-CAcreateserial", &md, "-extfile", &ext, "-out", &pem];
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
    err: Arc<Mutex<Vec<u8>>>,
    seen: Vec<u8>,
    pub port: u16,
    dir: PathBuf,
    leaf: String,
    extra: Vec<String>,
    scripted: bool,
    /// The end of `send_detached`: its writer holds `stdin` open until this drops.
    hold: Option<std::sync::mpsc::Sender<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    /// Start on a free port. The port is found by binding and releasing it, so another process can take it before
    /// `s_server` binds. That shows as a failed start, and we try again with a new port.
    pub fn start(pki: &Pki, leaf: &str, extra: &[&str], scripted: bool) -> Server {
        let mut last = String::new();
        for _ in 0..5 {
            let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            match Server::try_start(pki, leaf, extra, scripted, port) {
                Ok(server) => return server,
                Err(e) => last = e,
            }
        }
        panic!("{last}");
    }

    /// Start on this port. `Err` if `s_server` could not bind it: no other process may pass for it.
    pub fn try_start(pki: &Pki, leaf: &str, extra: &[&str], scripted: bool, port: u16) -> Result<Server, String> {
        let extra: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
        let (child, stdin, out, err) = Self::spawn(&pki.dir, leaf, &extra, scripted, port)?;
        Ok(Server { child, stdin, out, err, seen: Vec::new(), port, dir: pki.dir.clone(), leaf: leaf.to_string(), extra, scripted, hold: None })
    }

    fn spawn(dir: &PathBuf, leaf: &str, extra: &[String], scripted: bool, port: u16) -> Result<Spawned, String> {
        let mut child = Command::new("openssl")
            .args(["s_server", "-accept", &format!("127.0.0.1:{port}"), "-cert", &format!("{leaf}.pem"), "-key", &format!("{leaf}.key")])
            // TLS 1.3 only, unless the caller picks the version itself (`-tls1_2`, `-min_protocol`): s_server takes one protocol flag.
            .args(if extra.iter().any(|a| a.starts_with("-tls1_") || a == "-min_protocol") { &[][..] } else { &["-tls1_3"][..] })
            .args(["-ciphersuites", "TLS_AES_128_GCM_SHA256", "-groups", "X25519", "-sigalgs", "ECDSA+SHA256", "-quiet"])
            .args(extra)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(if scripted { Stdio::piped() } else { Stdio::null() })
            .stderr(Stdio::piped())
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
        let err = Arc::new(Mutex::new(Vec::new()));
        let stderr_reader = child.stderr.take().map(|mut e| {
            let keep = err.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 1024];
                loop {
                    let n = match e.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    };
                    let mut kept = keep.lock().unwrap();
                    let room = STDERR_KEEP.saturating_sub(kept.len()).min(n);
                    kept.extend_from_slice(&buf[..room]);
                }
            })
        });
        let mut reason = "did not start within 5 seconds".to_string();
        for _ in 0..100 {
            match child.try_wait() {
                Ok(Some(status)) => { reason = format!("exited at start: {status}"); break; }
                Err(e) => { reason = format!("status check failed: {e}"); break; }
                Ok(None) => {}
            }
            match owns_listener(child.id(), port) {
                Ok(true) => return Ok((child, stdin, out, err)),
                Ok(false) => {}
                Err(e) => { reason = format!("listener ownership check failed: {e}"); break; }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        if let Some(reader) = stderr_reader { let _ = reader.join(); }
        Err(format!("s_server on port {port} {reason}; stderr {:?}", String::from_utf8_lossy(&err.lock().unwrap())))
    }

    /// End the connection the hard way: kill the server and start a new one on the same port.
    /// The client sees a close with no `close_notify`. Whatever the old server had not yet read is lost.
    pub fn reset(&mut self) {
        // Kill first. `s_server` that sees the end of its stdin shuts the connection down with `close_notify`.
        let _ = self.child.kill();
        drop(self.stdin.take());
        let _ = self.child.wait();
        self.seen.clear();
        let (child, stdin, out, err) = Self::spawn(&self.dir, &self.leaf, &self.extra, self.scripted, self.port).unwrap_or_else(|e| panic!("{e}"));
        self.child = child;
        self.stdin = stdin;
        self.out = out;
        self.err = err;
    }

    /// Why a wait ran out: the child's status, its stderr (bounded), and what the client thread did.
    fn report(&mut self) -> String {
        let status = match self.child.try_wait() {
            Ok(Some(s)) => format!("exited: {s}"),
            Ok(None) => "running".to_string(),
            Err(e) => format!("unknown: {e}"),
        };
        let client = CLIENT.with(|c| c.borrow().as_ref().map(|n| n.lock().unwrap().clone()));
        let client = match client {
            None => "no client was started".to_string(),
            Some(None) => "still running (or panicked)".to_string(),
            Some(Some(r)) => format!("returned {r}"),
        };
        format!("port {} s_server {status}; stderr {:?}; client {client}", self.port, String::from_utf8_lossy(&self.err.lock().unwrap()))
    }

    /// Bytes from the client until `pat` is in them. Gives everything up to the end of `pat`.
    #[track_caller]
    pub fn expect(&mut self, pat: &[u8]) -> Vec<u8> {
        self.expect_within(pat, Duration::from_secs(10))
    }

    #[track_caller]
    pub fn expect_within(&mut self, pat: &[u8], limit: Duration) -> Vec<u8> {
        let at = Location::caller();
        let end = Instant::now() + limit;
        loop {
            if let Some(i) = self.seen.windows(pat.len()).position(|w| w == pat) {
                return self.seen.drain(..i + pat.len()).collect();
            }
            let left = end.saturating_duration_since(Instant::now());
            match self.out.as_ref().unwrap().recv_timeout(left) {
                Ok(b) => self.seen.extend_from_slice(&b),
                Err(e) => {
                    let report = self.report();
                    panic!("{at}: waiting for {:?}: {e:?}; have {:?}; {report}", String::from_utf8_lossy(pat), String::from_utf8_lossy(&self.seen))
                }
            }
        }
    }

    /// Exactly `n` bytes from the client.
    #[track_caller]
    pub fn take(&mut self, n: usize) -> Vec<u8> {
        let at = Location::caller();
        let end = Instant::now() + Duration::from_secs(20);
        while self.seen.len() < n {
            let left = end.saturating_duration_since(Instant::now());
            match self.out.as_ref().unwrap().recv_timeout(left) {
                Ok(b) => self.seen.extend_from_slice(&b),
                Err(e) => {
                    let report = self.report();
                    panic!("{at}: waiting for {n} bytes: {e:?}; have {}; {report}", self.seen.len())
                }
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

    /// Send bytes to the client from another thread, for more than `s_server` takes before it closes the connection: `send` would block. The
    /// writer ends when `s_server` is gone. It keeps `stdin` open: `s_server` that sees its end closes the connection. No later `send` is possible.
    pub fn send_detached(&mut self, bytes: Vec<u8>) {
        let mut s = self.stdin.take().unwrap();
        let (hold, wait) = channel::<()>();
        self.hold = Some(hold);
        std::thread::spawn(move || {
            let _ = s.write_all(&bytes).and_then(|_| s.flush());
            let _ = wait.recv();
        });
    }

    /// Send bytes to the client (one TLS record each time, at most).
    pub fn send(&mut self, bytes: &[u8]) {
        let s = self.stdin.as_mut().unwrap();
        s.write_all(bytes).unwrap();
        s.flush().unwrap();
    }
}
