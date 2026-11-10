//! `fetch` over TLS through the real `bare_net` client, against a loopback `openssl s_server` with a synthetic root made
//! for each test in RAM (see tools/crypto/openssl_harness.rs). These tests skip (and say so) when `openssl` is not installed.
//! The trust anchor is a private root passed to the client. There is no way to turn certificate checks off.
//! The harness keeps its keys in /dev/shm, so these tests run on Linux only.
#![cfg(target_os = "linux")]

#[path = "../../../tools/crypto/openssl_harness.rs"]
mod harness;

use bare::browser::*;
use bare::browser_fetch::*;
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_net::http::Client;
use bare_tls::trust_store;
use harness::{Pki, Server, openssl_available};
use std::thread::JoinHandle;

macro_rules! need_openssl {
    () => {
        if !openssl_available() {
            eprintln!("skipped: openssl is not installed");
            return;
        }
    };
}

const HEAD_END: &[u8] = b"\r\n\r\n";

/// Run a browser on its own thread (big stack): the test drives the server meanwhile. It gives `out.join('|')`.
fn spawn(root: Vec<u8>, port: u16, script: String) -> JoinHandle<String> {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 20_000_000, ..Default::default() }, HeapLimits::default()).unwrap();
            let client = Client::with_roots(trust_store::decode_pem(&root).unwrap());
            b.configure_fetch(FetchConfig { base_url: Some(format!("https://localhost:{port}/app")), client, limits: FetchLimits::default() }).unwrap();
            b.run_script(&script).unwrap_or_else(|e| panic!("script: {e:?}"));
            b.drain().unwrap();
            while b.pending_fetches() > 0 {
                b.run_network_tasks(100).unwrap();
            }
            let v = b.run_script("out.join('|')").unwrap();
            let text = b.it.display(v);
            text + "#" + &b.take_fetch_diagnostics().join(";")
        })
        .unwrap()
}

fn ok(body: &str) -> String {
    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len())
}

fn nothing_sent(srv: &mut Server) {
    let got = srv.collect(300);
    assert!(got.is_empty(), "application data reached an untrusted peer: {:?}", String::from_utf8_lossy(&got));
}

const FAILED: &str = "TypeError:Failed to fetch";
const LOGIN: &str = "var out = []; fetch('/api/auth/login', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({u:'synthetic'})}).then(function (r) { out.push(r.status, r.url.slice(0, 17)); return r.json(); })
  .then(function (j) { out.push(j.access_token); return fetch('/api/auth/me', {headers:{Authorization:'Bearer ' + j.access_token}}); })
  .then(function (r) { return r.json(); }).then(function (j) { out.push(j.email); }, function (e) { out.push(e.name + ':' + e.message); });";

#[test]
fn login_style_post_then_get_over_verified_tls_on_one_connection() {
    need_openssl!();
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    let mut srv = Server::start(&pki, "good", &[], true);
    let t = spawn(String::from_utf8(pki.root_pem()).unwrap().into_bytes(), srv.port, LOGIN.to_string());
    let head = String::from_utf8(srv.expect(HEAD_END)).unwrap();
    assert!(head.starts_with("POST /api/auth/login HTTP/1.1\r\n"), "{head}");
    assert!(head.contains(&format!("Host: localhost:{}\r\n", srv.port)) && head.contains("content-type: application/json\r\n"), "{head}");
    let n: usize = head.lines().find_map(|l| l.strip_prefix("Content-Length: ")).unwrap().parse().unwrap();
    assert_eq!(srv.take(n), br#"{"u":"synthetic"}"#);
    srv.send(ok(r#"{"access_token":"synthetic-token"}"#).as_bytes());
    // The next request arrives on the same TLS connection (s_server serves one connection at a time).
    let head = String::from_utf8(srv.expect(HEAD_END)).unwrap();
    assert!(head.starts_with("GET /api/auth/me HTTP/1.1\r\n") && head.contains("authorization: Bearer synthetic-token\r\n"), "{head}");
    assert!(!head.to_lowercase().contains("cookie") && !head.to_lowercase().contains("origin:"), "{head}");
    srv.send(ok(r#"{"email":"a@example.test"}"#).as_bytes());
    let got = t.join().unwrap();
    assert_eq!(got, "200|https://localhost|synthetic-token|a@example.test#");
}

#[test]
fn an_untrusted_root_gets_no_request_and_the_promise_rejects() {
    need_openssl!();
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    let other = Pki::new();
    let mut srv = Server::start(&pki, "good", &[], true);
    let t = spawn(other.root_pem(), srv.port, LOGIN.to_string());
    nothing_sent(&mut srv);
    let got = t.join().unwrap();
    assert!(got.starts_with(FAILED) && got.contains("failed: network error (https://localhost:"), "{got}");
    assert!(!got.contains("synthetic"));
}

#[test]
fn a_wrong_host_name_gets_no_request() {
    need_openssl!();
    let pki = Pki::new();
    pki.leaf("bad", "other.example", None);
    let mut srv = Server::start(&pki, "bad", &[], true);
    let t = spawn(pki.root_pem(), srv.port, LOGIN.to_string());
    nothing_sent(&mut srv);
    assert!(t.join().unwrap().starts_with(FAILED));
}

#[test]
fn https_to_http_and_cross_origin_redirects_send_nothing_further() {
    need_openssl!();
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    // A plain listener that must never get a connection.
    let plain = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    plain.set_nonblocking(true).unwrap();
    let pp = plain.local_addr().unwrap().port();
    let mut a = Server::start(&pki, "good", &[], true);
    let t = spawn(pki.root_pem(), a.port, "var out = []; fetch('/x', {method:'POST', body:'pw=hunter2', headers:{Authorization:'Bearer tok'}}).then(function () { out.push('ok'); }, function (e) { out.push(e.name + ':' + e.message); });".to_string());
    let head = String::from_utf8(a.expect(HEAD_END)).unwrap();
    assert!(head.starts_with("POST /x"), "{head}");
    a.take(10);
    a.send(format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: http://localhost:{pp}/steal\r\nContent-Length: 0\r\n\r\n").as_bytes());
    let got = t.join().unwrap();
    assert!(got.starts_with(FAILED) && got.contains("https to http redirect"), "{got}");
    assert!(!got.contains("steal") && !got.contains("hunter2"), "{got}");
    assert!(plain.accept().is_err(), "a plain socket was opened");
    // Another https origin, trusted by the same root: still a different origin.
    let mut a = Server::start(&pki, "good", &[], true);
    let mut b = Server::start(&pki, "good", &[], true);
    let t = spawn(pki.root_pem(), a.port, "var out = []; fetch('/x', {method:'POST', body:'pw=hunter2', headers:{Authorization:'Bearer tok'}}).then(function () { out.push('ok'); }, function (e) { out.push(e.name + ':' + e.message); });".to_string());
    a.expect(HEAD_END);
    a.take(10);
    a.send(format!("HTTP/1.1 308 Permanent Redirect\r\nLocation: https://localhost:{}/steal\r\nContent-Length: 0\r\n\r\n", b.port).as_bytes());
    // CORS: the redirect is followed, as in Chrome (a simple POST). `Authorization` does not leave the origin, and the reply has no
    // allow header, so the page gets an error.
    let head = String::from_utf8(b.expect(HEAD_END)).unwrap().to_ascii_lowercase();
    assert!(head.starts_with("post /steal") && head.contains("origin: https://localhost:") && !head.contains("authorization") && !head.contains("cookie"), "{head}");
    b.take(10);
    b.send(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
    let got = t.join().unwrap();
    assert!(got.starts_with(FAILED) && got.contains("blocked by CORS") && !got.contains("steal") && !got.contains("hunter2"), "{got}");
}
