//! The `network/h2_ttfb` trace record: the wait from the end of the request writes to the first frame of the reply. The fixture runs in a
//! child process, because the trace owner is one per process. It talks to a loopback `openssl s_server -alpn h2` that this test drives
//! frame by frame, as in `h2_edge.rs`. The test skips (and says so) when `openssl` is not installed. Linux only, like the harness.
#![cfg(target_os = "linux")]

#[path = "../../../tools/crypto/openssl_harness.rs"]
mod harness;
#[allow(dead_code)]
#[path = "../src/hpack.rs"]
mod hpack;

use bare_net::http::{Client, Request};
use bare_observe::{Ids, process};
use bare_tls::trust_store;
use bare_url::Url;
use harness::{Pki, Server, openssl_available};
use std::time::Duration;

const HEADERS: u8 = 1;
const SETTINGS: u8 = 4;
const WINDOW_UPDATE: u8 = 8;
const END_STREAM: u8 = 1;
const END_HEADERS: u8 = 4;
const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
const WAIT_MS: u64 = 150;

fn frame(ty: u8, flags: u8, id: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = (payload.len() as u32).to_be_bytes()[1..].to_vec();
    out.extend_from_slice(&[ty, flags]);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn read_frame(srv: &mut Server) -> (u8, u8, u32, Vec<u8>) {
    let h = srv.take(9);
    let len = u32::from_be_bytes([0, h[0], h[1], h[2]]) as usize;
    let payload = srv.take(len);
    (h[3], h[4], u32::from_be_bytes([h[5], h[6], h[7], h[8]]), payload)
}

/// Runs in the child process: one GET, and the peer waits `WAIT_MS` ms between the request and the reply.
#[test]
fn fixture() {
    if std::env::var_os("H2_OBSERVE_FIXTURE").is_none() {
        return;
    }
    let owner = process::Process::from_env(false).unwrap();
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    let mut srv = Server::start(&pki, "good", &["-alpn", "h2"], true);
    let client = Client::with_roots(trust_store::decode_pem(&pki.root_pem()).unwrap());
    let url = Url::parse(&format!("https://localhost:{}/slow", srv.port)).unwrap();
    let t = std::thread::spawn(move || {
        let _scope = process::enter(Ids { session: Some(5), ..Ids::default() });
        let mut client = client;
        // `send_stream` gives the request an id; `get` does not.
        let r = client.send_stream(&Request::get(&url)).map(|s| s.response.status).map_err(|e| e.to_string());
        (client, r)
    });
    srv.expect(PREFACE);
    let (ty, _, id, _) = read_frame(&mut srv);
    assert_eq!((ty, id), (SETTINGS, 0));
    srv.send(&frame(SETTINGS, 0, 0, &[]));
    let id = loop {
        let (ty, _, id, _) = read_frame(&mut srv);
        match ty {
            HEADERS => break id,
            SETTINGS | WINDOW_UPDATE => {}
            other => panic!("unexpected frame type {other}"),
        }
    };
    std::thread::sleep(Duration::from_millis(WAIT_MS));
    srv.send(&frame(HEADERS, END_HEADERS | END_STREAM, id, &[0x88]));
    let (client, r) = t.join().unwrap();
    assert_eq!(r, Ok(200));
    drop(client);
    owner.shutdown();
}

fn run(mode: &str, path: &std::path::Path) {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "fixture", "--nocapture", "--test-threads=1"])
        .env("H2_OBSERVE_FIXTURE", "1")
        .env("BARE_OBSERVE", mode)
        .env("BARE_OBSERVE_FILE", path)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
}

#[test]
fn h2_ttfb_is_the_wait_for_the_first_frame() {
    if !openssl_available() {
        eprintln!("skipped: openssl is not installed");
        return;
    }
    let path = std::env::temp_dir().join(format!("bare-h2-observe-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&path);
    run("trace", &path);
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    let records: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let find = |event: &str| -> Vec<&serde_json::Value> { records.iter().filter(|r| r["category"] == "network" && r["event"] == event).collect() };
    let ttfb = find("h2_ttfb");
    assert_eq!(ttfb.len(), 1, "records: {text}");
    assert_eq!(ttfb[0]["session_id"], 5);
    assert!(ttfb[0]["request_id"].is_u64());
    let wait = ttfb[0]["fields"]["duration_ns"].as_u64().unwrap();
    let headers = find("h2_response_headers");
    assert_eq!(headers.len(), 1);
    let whole = headers[0]["fields"]["duration_ns"].as_u64().unwrap();
    assert!(wait >= 140_000_000, "wait {wait}");
    assert!(wait <= whole, "wait {wait} whole {whole}");

    run("off", &path);
    assert!(!path.exists());
}
