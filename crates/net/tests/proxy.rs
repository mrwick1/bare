//! Each route runs in a fresh process: BARE_PROXY is immutable after first use.
//! The harness keeps its keys in /dev/shm, so these tests run on Linux only.
#![cfg(target_os = "linux")]
#[path = "../../../tools/crypto/openssl_harness.rs"]
mod harness;
use bare_net::{cause, http::Client};
use bare_url::Url;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::thread;
use std::time::Duration;

fn head(s: &mut TcpStream) -> String {
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut b = Vec::new();
    while !b.ends_with(b"\r\n\r\n") {
        let mut c = [0];
        s.read_exact(&mut c).unwrap();
        b.push(c[0]);
    }
    String::from_utf8(b).unwrap()
}
fn listener() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").unwrap()
}
fn child(mode: &str, proxy: Option<&str>, target: &str, root: Option<&str>) {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args(["--exact", "proxy_child", "--nocapture"])
        .env("PROXY_CASE", mode)
        .env("PROXY_TARGET", target)
        .env_remove("BARE_PROXY")
        .env("BARE_NET_H1", "1");
    if let Some(p) = proxy {
        c.env("BARE_PROXY", p);
    }
    if let Some(r) = root {
        c.env("PROXY_ROOT", r);
    }
    assert!(c.status().unwrap().success(), "case {mode}");
}
#[test]
fn proxy_child() {
    let Ok(mode) = std::env::var("PROXY_CASE") else { return };
    let u = Url::parse(&std::env::var("PROXY_TARGET").unwrap()).unwrap();
    let mut c = if let Ok(root) = std::env::var("PROXY_ROOT") {
        Client::with_roots(bare_tls::trust_store::decode_pem(root.as_bytes()).unwrap())
    } else {
        Client::new()
    };
    if mode == "ok" || mode == "reuse" {
        for _ in 0..if mode == "reuse" { 2 } else { 1 } {
            let r = c.get(&u).unwrap();
            assert_eq!(r.body, b"proxy");
        }
    } else {
        let e = c.get(&u).err().expect("must fail");
        let text = cause::describe(&*e);
        assert!(text.contains(&mode), "{text}");
        assert!(!text.contains("secret"), "credential leaked");
    }
}
#[test]
fn proxy_routes() {
    for status in [407, 403] {
        let l = listener();
        let p = format!("http://user:secret@{}", l.local_addr().unwrap());
        let t = thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let h = head(&mut s);
            assert!(h.starts_with("CONNECT no-local-dns.invalid:443 HTTP/1.1\r\nHost: no-local-dns.invalid:443\r\n"));
            assert!(h.contains("Proxy-Authorization: Basic dXNlcjpzZWNyZXQ=\r\n"));
            write!(s, "HTTP/1.1 {status} Refused\r\n\r\n").unwrap();
        });
        child(&format!("proxy: {status}"), Some(&p), "https://no-local-dns.invalid/", None);
        t.join().unwrap();
    }
    for p in [
        "",
        "socks5://user:secret@host:80",
        "http://user:secret@host",
        "http://host:0",
        "http://host:80/path",
        "http://host:99999",
        "http://host:80\r\nx: y",
    ] {
        child("proxy: invalid BARE_PROXY", Some(p), "https://no-local-dns.invalid/", None);
    }
    let l = listener();
    let p = format!("http://{}", l.local_addr().unwrap());
    let t = thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        head(&mut s);
        let _ = s.write_all(&[b'x'; 256 * 1024 + 1]);
    });
    child("proxy: the headers", Some(&p), "https://no-local-dns.invalid/", None);
    t.join().unwrap();

    // Plain requests use absolute form. A second request reuses this route.
    let l = listener();
    let p = format!("http://user:secret@{}", l.local_addr().unwrap());
    let t = thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        for _ in 0..2 {
            let h = head(&mut s);
            assert!(h.starts_with("GET http://no-local-dns.invalid/page?q=1 HTTP/1.1\r\n"), "{h}");
            assert!(h.contains("Proxy-Authorization: Basic dXNlcjpzZWNyZXQ=\r\n"));
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nproxy").unwrap();
        }
    });
    child("reuse", Some(&p), "http://no-local-dns.invalid/page?q=1", None);
    t.join().unwrap();

    // Direct and loopback routes keep origin form, with no proxy credentials.
    for proxy in [None, Some("http://user:secret@127.0.0.1:1")] {
        let l = listener();
        let target = format!("http://{}/", l.local_addr().unwrap());
        let t = thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let h = head(&mut s);
            assert!(h.starts_with("GET / HTTP/1.1\r\n"));
            assert!(!h.to_lowercase().contains("proxy-authorization"));
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nproxy").unwrap();
        });
        child("ok", proxy, &target, None);
        t.join().unwrap();
    }
}
#[test]
fn connect_relays_verified_tls() {
    assert!(harness::openssl_available());
    let pki = harness::Pki::new();
    pki.leaf("good", "remote.invalid", None);
    for mode in ["ok", "tls:"] {
        let mut srv = harness::Server::start(&pki, "good", &[], true);
        let l = listener();
        let proxy = format!("http://user:secret@{}", l.local_addr().unwrap());
        let port = srv.port;
        let t = thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let h = head(&mut s);
            assert!(h.contains("Proxy-Authorization: Basic dXNlcjpzZWNyZXQ=\r\n"));
            let mut upstream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(b"HTTP/1.1 200 Tunnel\r\n\r\n").unwrap();
            let (mut a, mut b) = (s.try_clone().unwrap(), upstream.try_clone().unwrap());
            let relay = thread::spawn(move || {
                let _ = std::io::copy(&mut a, &mut b);
                let _ = b.shutdown(std::net::Shutdown::Write);
            });
            let _ = std::io::copy(&mut upstream, &mut s);
            let _ = s.shutdown(std::net::Shutdown::Write);
            relay.join().unwrap();
        });
        let root = String::from_utf8(pki.root_pem()).unwrap();
        let target = if mode == "ok" { "https://remote.invalid/" } else { "https://wrong.invalid/" };
        thread::scope(|scope| {
            let c = scope.spawn(|| child(mode, Some(&proxy), target, Some(&root)));
            if mode == "ok" {
                let h = String::from_utf8(srv.expect(b"\r\n\r\n")).unwrap();
                assert!(h.starts_with("GET / HTTP/1.1\r\n"));
                assert!(!h.to_lowercase().contains("proxy-authorization"));
                srv.send(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nproxy");
            }
            c.join().unwrap();
        });
        drop(srv);
        t.join().unwrap();
    }
}
