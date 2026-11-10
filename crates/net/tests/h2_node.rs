//! HTTP/2 through `bare_net::http::Client`, against Node's `http2` (nghttp2). It is an independent peer: it checks our HPACK encoder
//! (nghttp2 decodes it), our HPACK decoder (nghttp2 encodes for it), flow control with real windows, and multiplexing on one connection.
//! The trust anchor is a synthetic root made for each test. These tests skip (and say so) when `node` or `openssl` is missing.
//! The harness keeps its keys in /dev/shm, so these tests run on Linux only.
#![cfg(target_os = "linux")]

#[path = "../../../tools/crypto/openssl_harness.rs"]
mod harness;

use bare_net::http::Client;
use bare_tls::trust_store;
use bare_url::Url;
use harness::{Pki, openssl_available};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

struct Node {
    child: Child,
    port: u16,
    _pki: Pki,
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(delay_ms: u32) -> Option<(Node, Client)> {
    if !openssl_available() || Command::new("node").arg("--version").stdout(Stdio::null()).status().is_err() {
        eprintln!("skipped: node or openssl is not installed");
        return None;
    }
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/net/h2_node_server.mjs");
    let mut child = Command::new("node")
        .args([script, &port.to_string(), pki.dir.join("good.pem").to_str().unwrap(), pki.dir.join("good.key").to_str().unwrap(), &delay_ms.to_string()])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.as_mut().unwrap()).read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "listening");
    let client = Client::with_roots(trust_store::decode_pem(&pki.root_pem()).unwrap());
    Some((Node { child, port, _pki: pki }, client))
}

fn url(n: &Node, path: &str) -> Url {
    Url::parse(&format!("https://localhost:{}{path}", n.port)).unwrap()
}

fn text(r: &bare_net::http::Response) -> String {
    String::from_utf8(r.body.clone()).unwrap()
}

#[test]
fn our_headers_reach_nghttp2_intact_and_the_reply_comes_back_over_h2() {
    let Some((node, mut client)) = start(0) else { return };
    let r = client
        .post(
            &url(&node, "/echo"),
            &[("Authorization", "Bearer s3cr3t"), ("X-Long", &"v".repeat(3000)), ("X-Accent", "caf\u{e9}"), ("Accept", "text/html"), ("Content-Type", "text/plain")],
            b"hello",
        )
        .unwrap();
    assert_eq!(r.status, 200);
    let body = text(&r);
    for want in ["\"httpVersion\":\"2.0\"", "\"method\":\"POST\"", "\"authorization\":\"Bearer s3cr3t\"", "\"accept\":\"text/html\"", "\"content-type\":\"text/plain\"", "\"bytes\":5"] {
        assert!(body.contains(want), "{want} not in {body}");
    }
    assert!(body.contains(&format!("\"x-long\":\"{}\"", "v".repeat(3000))));
    assert!(body.contains("\"x-accent\":\"caf"));
}

#[test]
fn our_decoder_reads_what_nghttp2_encodes_across_a_filling_dynamic_table() {
    let Some((node, mut client)) = start(0) else { return };
    for i in 0..6 {
        // A new query each time: the reply cache does not answer, and the dynamic table of the server fills and is reused.
        let r = client.get(&url(&node, &format!("/headers?n={i}"))).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(text(&r), "ok");
        assert_eq!(r.header("content-type"), Some("text/plain"));
        assert_eq!(r.header("allow"), Some(format!("value-of-allow-{}", "x".repeat(5)).as_str()));
        assert_eq!(r.header("www-authenticate"), Some(format!("value-of-www-authenticate-{}", "x".repeat(16)).as_str()));
        let cookies: Vec<_> = r.headers.iter().filter(|(n, _)| n == "set-cookie").map(|(_, v)| v.as_str()).collect();
        assert_eq!(cookies, ["a=1; Path=/", "b=2; Secure"]);
    }
    assert!(text(&client.get(&url(&node, "/conns")).unwrap()).contains("\"conns\":1"));
}

#[test]
fn a_3_mib_download_and_a_1_mib_upload_pass_the_flow_control_windows() {
    let Some((node, mut client)) = start(0) else { return };
    let n = 3 << 20;
    let r = client.get(&url(&node, &format!("/bytes?n={n}"))).unwrap();
    assert_eq!(r.body.len(), n);
    assert!(r.body.iter().enumerate().all(|(i, b)| *b == (i % 251) as u8));
    let up: Vec<u8> = (0..1 << 20).map(|i| (i * 7 % 256) as u8).collect();
    let r = client.post(&url(&node, "/upload"), &[], &up).unwrap();
    let body = text(&r);
    assert!(body.contains(&format!("\"bytes\":{}", up.len())), "{body}");
    let want = Command::new("sha256sum").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    use std::io::Write;
    want.stdin.as_ref().unwrap().write_all(&up).unwrap();
    let out = want.wait_with_output().unwrap();
    assert!(body.contains(&String::from_utf8_lossy(&out.stdout)[..64]), "{body}");
}

#[test]
fn eight_workers_share_one_connection() {
    let Some((node, client)) = start(60) else { return };
    let start = std::time::Instant::now();
    // The worker pool (6 threads, 2 connecting at once) is what the page loader uses.
    let tasks: Vec<_> = (0..8)
        .map(|i| {
            let u = url(&node, &format!("/asset/{i}.js"));
            client.spawn(move |c| c.get(&u).unwrap().status)
        })
        .collect();
    for t in tasks {
        assert_eq!(t.wait(), Some(200));
    }
    let took = start.elapsed();
    let mut c = client.fork();
    let conns = text(&c.get(&url(&node, "/conns")).unwrap());
    // The first dial may race: the gate lets 2 connections open at once. The rest use a session. Eight serial replies take 480 ms.
    assert!(conns.contains("\"conns\":1") || conns.contains("\"conns\":2"), "{conns}");
    assert!(took < std::time::Duration::from_millis(400), "{took:?}");
}
