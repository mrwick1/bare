//! Edge cases of the HTTP/2 client against Node's `http2` (nghttp2) with `tools/net/h2_node_edge.mjs`: padded frames, PRIORITY, interim
//! replies, trailers, compressed bodies, CONTINUATION, a low `SETTINGS_MAX_CONCURRENT_STREAMS`, SETTINGS in the middle of a connection,
//! GOAWAY with a last stream id, and a server that closes idle sessions. The trust anchor is a synthetic root made for each test. These
//! tests skip (and say so) when `node` or `openssl` is missing.
//! The harness keeps its keys in /dev/shm, so these tests run on Linux only.
#![cfg(target_os = "linux")]

#[path = "../../../tools/crypto/openssl_harness.rs"]
mod harness;

use bare_net::http::{Client, Method, Request};
use bare_tls::trust_store;
use bare_url::Url;
use harness::{Pki, openssl_available};
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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

fn start(options: &str) -> Option<(Node, Client)> {
    if !openssl_available() || Command::new("node").arg("--version").stdout(Stdio::null()).status().is_err() {
        eprintln!("skipped: node or openssl is not installed");
        return None;
    }
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/net/h2_node_edge.mjs");
    let mut child = Command::new("node")
        .args([script, &port.to_string(), pki.dir.join("good.pem").to_str().unwrap(), pki.dir.join("good.key").to_str().unwrap(), options])
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

fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

fn text(n: usize) -> Vec<u8> {
    "the quick brown fox jumps over the lazy dog\n".repeat(n.div_ceil(44)).into_bytes()[..n].to_vec()
}

/// `(connections, streams, most streams open at once)` that the server counted.
fn counts(n: &Node, c: &mut Client) -> (u64, u64, u64) {
    let body = String::from_utf8(c.get(&url(n, "/conns")).unwrap().body.clone()).unwrap();
    let num = |k: &str| body.split(&format!("\"{k}\":")).nth(1).unwrap().split([',', '}']).next().unwrap().parse().unwrap();
    (num("conns"), num("streams"), num("peak"))
}

#[test]
fn padded_data_and_headers_are_read() {
    for padding in ["max", "aligned"] {
        let Some((node, mut client)) = start(&format!("{{\"padding\":\"{padding}\"}}")) else { return };
        for n in [0, 1, 1000, 16384, 100_000, 3 << 20] {
            let r = client.get(&url(&node, &format!("/plain?n={n}"))).unwrap();
            assert_eq!((r.status, r.body.len()), (200, n), "{padding} {n}");
            assert!(r.body == pattern(n), "{padding} {n}: bytes differ");
        }
        // One session served all of it.
        assert_eq!(counts(&node, &mut client).0, 1);
    }
}

#[test]
fn a_priority_frame_is_ignored() {
    let Some((node, mut client)) = start("{}") else { return };
    for i in 0..3 {
        let r = client.get(&url(&node, &format!("/priority?n={}", 1000 + i))).unwrap();
        assert_eq!((r.status, r.body == pattern(1000 + i)), (200, true));
    }
    assert_eq!(counts(&node, &mut client).0, 1);
}

#[test]
fn interim_replies_are_skipped_and_the_final_reply_is_returned() {
    let Some((node, mut client)) = start("{}") else { return };
    for kind in ["100", "103", "multi"] {
        let r = client.get(&url(&node, &format!("/interim?kind={kind}"))).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, &b"final"[..]), "{kind}");
        // The headers of a 103 (Link) are not the headers of the answer.
        assert_eq!(r.header("link"), None, "{kind}");
        assert_eq!(r.header("content-length"), Some("5"));
    }
    assert_eq!(counts(&node, &mut client).0, 1);
}

#[test]
fn trailers_end_the_stream_and_the_session_goes_on() {
    let Some((node, mut client)) = start("{}") else { return };
    for n in [0, 10, 50_000] {
        let r = client.get(&url(&node, &format!("/trailers?n={n}"))).unwrap();
        assert_eq!((r.status, r.body == pattern(n)), (200, true), "{n}");
        // A trailer is not a header of the answer.
        assert_eq!(r.header("x-sum"), None);
    }
    assert_eq!(counts(&node, &mut client).0, 1);
}

#[test]
fn gzip_br_and_deflate_bodies_are_decoded_over_h2() {
    let Some((node, mut client)) = start("{}") else { return };
    for coding in ["gzip", "br", "deflate"] {
        for n in [1, 5000, 3 << 20] {
            // With a content-length (the compressed size), and without (a stream of parts).
            for extra in ["", "&stream=1"] {
                let r = client.get(&url(&node, &format!("/{coding}?n={n}{extra}"))).unwrap();
                assert_eq!(r.status, 200, "{coding} {n}{extra}");
                assert!(r.body == text(n), "{coding} {n}{extra}: {} bytes", r.body.len());
            }
        }
    }
    assert_eq!(counts(&node, &mut client).0, 1);
}

#[test]
fn a_reply_with_content_encoding_and_no_body_is_not_a_decode_error() {
    let Some((node, mut client)) = start("{}") else { return };
    for status in [204, 304] {
        let r = client.get(&url(&node, &format!("/empty?status={status}"))).unwrap();
        assert_eq!((r.status, r.body.len()), (status, 0));
    }
    let head = url(&node, "/gzip?n=5000");
    let r = client.send(&Request { cookies: Default::default(), method: Method::Head, url: &head, headers: &[], body: &[] }).unwrap();
    assert_eq!((r.status, r.body.len(), r.header("content-encoding")), (200, 0, Some("gzip")));
}

#[test]
fn a_header_block_over_16_kib_arrives_in_continuation_frames() {
    let Some((node, mut client)) = start("{}") else { return };
    let r = client.get(&url(&node, "/hugehdr?n=40")).unwrap();
    assert_eq!((r.status, r.body.as_slice()), (200, &b"ok"[..]));
    let big: Vec<_> = r.headers.iter().filter(|(n, _)| n.starts_with("x-big-")).collect();
    assert_eq!(big.len(), 40);
    assert!(big.iter().all(|(_, v)| v.len() == 1000));
    // The decoder stays in step with the table of the server for the next block.
    let r = client.get(&url(&node, "/hugehdr?n=3")).unwrap();
    assert_eq!(r.headers.iter().filter(|(n, _)| n.starts_with("x-big-")).count(), 3);
    assert_eq!(counts(&node, &mut client).0, 1);
}

/// `n` requests of 20 ms each from the worker pool, on whatever connections the client opens.
fn many(node: &Node, client: &Client, n: usize) -> Vec<(u16, usize)> {
    let tasks: Vec<_> = (0..n)
        .map(|i| {
            let u = url(node, &format!("/slow?ms=20&n={}", 1000 + i));
            client.spawn(move |c| c.get(&u).map(|r| (r.status, r.body.len())).unwrap_or((0, 0)))
        })
        .collect();
    tasks.into_iter().map(|t| t.wait().unwrap()).collect()
}

#[test]
fn twenty_requests_pass_a_server_that_allows_one_or_two_streams() {
    for max in [1, 2] {
        let Some((node, client)) = start(&format!("{{\"maxStreams\":{max}}}")) else { return };
        let t = Instant::now();
        let got = many(&node, &client, 20);
        let took = t.elapsed();
        assert_eq!(got, (0..20).map(|i| (200, 1000 + i)).collect::<Vec<_>>(), "max {max}");
        let (conns, streams, peak) = counts(&node, &mut client.fork());
        // The limit holds on each connection. The connect gate lets 2 connections open at once, so 2 is allowed; 3 would mean that a
        // stream was refused and run again on a new connection.
        assert!(peak <= max * conns && (1..=2).contains(&conns), "max {max}: conns {conns} peak {peak}");
        assert_eq!(streams, 21, "max {max}");
        assert!(took < Duration::from_millis(20 * 20 * 3 / max as u64 + 1500), "max {max}: {took:?}");
    }
}

#[test]
fn settings_in_the_middle_of_a_connection_apply_to_the_next_requests() {
    let Some((node, mut client)) = start("{}") else { return };
    let workers = client.fork();
    // Down to one stream at a time, then back up.
    assert_eq!(client.get(&url(&node, "/settings?max=1")).unwrap().status, 200);
    assert!(many(&node, &workers, 8).iter().all(|r| r.0 == 200));
    assert_eq!(counts(&node, &mut client).2, 1);
    assert_eq!(client.get(&url(&node, "/settings?max=50")).unwrap().status, 200);
    assert!(many(&node, &workers, 8).iter().all(|r| r.0 == 200));
    // A small initial window: an upload of 300 KB goes through in small steps, and arrives whole.
    assert_eq!(client.get(&url(&node, "/settings?window=1000")).unwrap().status, 200);
    let up = pattern(300_000);
    let r = client.post(&url(&node, "/upload"), &[], &up).unwrap();
    assert!(String::from_utf8_lossy(&r.body).contains("\"bytes\":300000"), "{}", String::from_utf8_lossy(&r.body));
    // Back to the default window, and a frame size of 64 KiB: the upload goes in bigger frames.
    assert_eq!(client.get(&url(&node, "/settings?window=65535&frame=65536")).unwrap().status, 200);
    let r = client.post(&url(&node, "/upload"), &[], &up).unwrap();
    assert!(String::from_utf8_lossy(&r.body).contains("\"bytes\":300000"));
    // A reply that is bigger than the old window still comes whole.
    assert!(client.get(&url(&node, "/plain?n=2000000")).unwrap().body == pattern(2_000_000));
    assert_eq!(counts(&node, &mut client).0, 1);
}

#[test]
fn goaway_with_a_last_stream_id_runs_the_later_streams_on_a_new_connection() {
    let Some((node, mut client)) = start("{}") else { return };
    let workers = client.fork();
    // Three slow requests are open (streams 1, 3, 5). Then a fourth (7) makes the server say GOAWAY with last stream 1.
    let slow: Vec<_> = (0..3)
        .map(|i| {
            let u = url(&node, &format!("/slow?ms=400&n={}", 100 + i));
            workers.spawn(move |c| c.get(&u).map(|r| (r.status, r.body.len())).map_err(|e| e.to_string()))
        })
        .collect();
    std::thread::sleep(Duration::from_millis(150));
    let r = client.get(&url(&node, "/goaway?last=1")).unwrap();
    assert_eq!((r.status, r.body.as_slice()), (200, &b"ok"[..]));
    for (i, t) in slow.into_iter().enumerate() {
        assert_eq!(t.wait().unwrap(), Ok((200, 100 + i)), "request {i}");
    }
    // Later requests use a new connection.
    let (conns, ..) = counts(&node, &mut client);
    assert!(conns >= 2, "{conns}");
}

#[test]
fn a_goaway_for_every_stream_after_this_one_moves_the_session() {
    let Some((node, mut client)) = start("{}") else { return };
    let r = client.get(&url(&node, "/goaway?last=1")).unwrap();
    assert_eq!(r.status, 200);
    // The session is closed for new streams. The next request opens another connection and works.
    for i in 0..3 {
        assert_eq!(client.get(&url(&node, &format!("/plain?n={i}"))).unwrap().status, 200);
    }
    assert_eq!(counts(&node, &mut client).0, 2);
}

#[test]
fn a_server_that_closes_idle_sessions_does_not_break_the_next_request() {
    for kill in [false, true] {
        let Some((node, mut client)) = start(&format!("{{\"idleMs\":300,\"idleKill\":{kill}}}")) else { return };
        assert_eq!(client.get(&url(&node, "/plain?n=10")).unwrap().status, 200);
        std::thread::sleep(Duration::from_millis(900));
        assert_eq!(client.get(&url(&node, "/plain?n=11")).unwrap().body.len(), 11, "kill {kill}");
        // Requests near the moment of the close: a GET never fails. 25 tries, 5 ms apart, around the 300 ms mark.
        for i in 0..25u64 {
            std::thread::sleep(Duration::from_millis(250 + i * 5));
            let r = client.get(&url(&node, &format!("/plain?n={}", 20 + i)));
            assert_eq!(r.map(|r| r.body.len()).map_err(|e| e.to_string()), Ok(20 + i as usize), "kill {kill} try {i}");
        }
    }
}

#[test]
fn a_post_after_a_graceful_idle_close_runs_on_a_new_connection() {
    // A graceful close says GOAWAY with the last stream the server saw. A POST that comes after is unprocessed, and may run again.
    let Some((node, mut client)) = start("{\"idleMs\":300}") else { return };
    assert_eq!(client.get(&url(&node, "/plain?n=10")).unwrap().status, 200);
    for i in 0..30u64 {
        std::thread::sleep(Duration::from_millis(270 + i * 3));
        let r = client.post(&url(&node, "/upload"), &[], b"abc").map_err(|e| e.to_string());
        assert!(r.as_ref().is_ok_and(|r| String::from_utf8_lossy(&r.body).contains("\"bytes\":3")), "try {i}: {:?}", r.map(|r| r.status));
    }
}

#[test]
fn a_body_that_arrives_in_small_parts_over_a_long_time_is_whole() {
    let Some((node, mut client)) = start("{}") else { return };
    let u = url(&node, "/trickle?parts=30&gap=15");
    let mut s = client.get_stream(&u).unwrap();
    let mut all = Vec::new();
    s.body.read_to_end(&mut all).unwrap();
    assert_eq!(all.len(), 3000);
}
