//! Record and replay of replies (`BARE_NET_RECORD`, `BARE_NET_REPLAY`; here a test sets the tape with `Client::use_tape`): a reply that was
//! recorded from a loopback server comes back with no request to it, a request that was not recorded is a clear error, and a cut body and a
//! failed request replay as they were.

use bare_net::http::{Client, Request};
use bare_net::replay::Tape;
use bare_url::Url;
use flate2::{Compression, write::GzEncoder};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bare-net-replay-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn gzip(text: &str) -> Vec<u8> {
    let mut e = GzEncoder::new(Vec::new(), Compression::fast());
    e.write_all(text.as_bytes()).unwrap();
    e.finish().unwrap()
}

/// A server that answers `/gz` with a gzip body and a `Set-Cookie`, `/old` with a redirect to `/gz`, `/cut` with a body that stops short, and
/// anything else with the text of the body of the request. `count` counts the requests.
fn serve(count: Arc<AtomicUsize>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { return };
            count.fetch_add(1, SeqCst);
            let (mut head, mut piece) = (Vec::new(), [0u8; 4096]);
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut piece) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.extend_from_slice(&piece[..n]),
                }
            }
            let text = String::from_utf8_lossy(&head).to_string();
            let path = text.split(' ').nth(1).unwrap_or("").to_string();
            let body_at = head.windows(4).position(|w| w == b"\r\n\r\n").map_or(head.len(), |i| i + 4);
            let reply: Vec<u8> = match path.as_str() {
                "/gz" => {
                    let z = gzip("hello from the server");
                    [format!("HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nSet-Cookie: a=1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", z.len()).into_bytes(), z].concat()
                }
                "/old" => b"HTTP/1.1 301 Moved Permanently\r\nLocation: /gz\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                "/cut" => b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n0123456789".to_vec(),
                _ => {
                    let body = &head[body_at..];
                    [format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes(), body.to_vec()].concat()
                }
            };
            let _ = s.write_all(&reply);
        }
    });
    port
}

fn url(port: u16, path: &str) -> Url {
    Url::parse(&format!("http://127.0.0.1:{port}{path}")).unwrap()
}

fn client(tape: &Arc<Tape>) -> Client {
    let mut c = Client::new();
    c.use_tape(tape.clone());
    c
}

#[test]
fn a_recorded_reply_replays_with_no_request() {
    let (dir, count) = (tmp("same"), Arc::new(AtomicUsize::new(0)));
    let port = serve(count.clone());
    let record = Tape::record(&dir);
    let mut live = client(&record);
    // A redirect, a gzip body with a cookie, and a POST.
    let a = live.get(&url(port, "/old")).unwrap();
    let b = live.send(&Request::post(&url(port, "/echo"), &[("Content-Type", "text/plain")], b"a=1")).unwrap();
    assert_eq!((a.status, a.body.as_slice()), (200, b"hello from the server".as_slice()));
    assert_eq!(b.body, b"a=1");
    let live_requests = count.load(SeqCst);
    assert_eq!(live_requests, 3);

    let mut again = client(&Tape::replay(&dir));
    let (a2, b2) = (again.get(&url(port, "/old")).unwrap(), again.send(&Request::post(&url(port, "/echo"), &[("Content-Type", "text/plain")], b"a=1")).unwrap());
    assert_eq!(count.load(SeqCst), live_requests, "a replay must not reach the server");
    assert_eq!((a2.status, &a2.headers, &a2.body), (a.status, &a.headers, &a.body));
    assert_eq!((&b2.headers, &b2.body), (&b.headers, &b.body));
    // The cookie of the recorded reply is in the jar of the replaying client.
    assert!(!again.jar().lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_request_that_was_not_recorded_is_a_clear_error() {
    let (dir, count) = (tmp("miss"), Arc::new(AtomicUsize::new(0)));
    let port = serve(count.clone());
    let mut live = client(&Tape::record(&dir));
    live.get(&url(port, "/gz")).unwrap();
    let mut replay = client(&Tape::replay(&dir));
    let e = replay.get(&url(port, "/other")).err().unwrap().to_string();
    assert_eq!(e, format!("replay: no recorded reply for GET http://127.0.0.1:{port}/other"));
    // The same URL with another body is another request.
    let e = replay.send(&Request::post(&url(port, "/gz"), &[], b"x")).err().unwrap().to_string();
    assert!(e.starts_with("replay: no recorded reply for POST "), "{e}");
    assert_eq!(count.load(SeqCst), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_cut_body_and_a_failed_request_replay_as_they_were() {
    let (dir, count) = (tmp("cut"), Arc::new(AtomicUsize::new(0)));
    let port = serve(count);
    let dead = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut live = client(&Tape::record(&dir));
    let mut body = live.get_stream(&url(port, "/cut")).unwrap().body;
    let (mut got, live_error) = (Vec::new(), body.read_to_end(&mut Vec::new()).err());
    drop(body);
    let live_refused = live.get(&url(dead, "/")).err().unwrap().to_string();
    assert!(live_error.is_some());

    let mut replay = client(&Tape::replay(&dir));
    let mut body = replay.get_stream(&url(port, "/cut")).unwrap().body;
    let error = loop {
        let mut piece = [0u8; 64];
        match body.read(&mut piece) {
            Ok(n) => got.extend_from_slice(&piece[..n]),
            Err(e) => break e,
        }
    };
    assert_eq!(got, b"0123456789");
    // The server closed the connection early: the live read said `UnexpectedEof`, and so does the replay.
    assert_eq!(error.kind(), live_error.unwrap().kind());
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    assert_eq!(replay.get(&url(dead, "/")).err().unwrap().to_string(), live_refused);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_fill_goes_to_the_network_only_for_what_is_missing() {
    let (dir, count) = (tmp("fill"), Arc::new(AtomicUsize::new(0)));
    let port = serve(count.clone());
    client(&Tape::record(&dir)).get(&url(port, "/gz")).unwrap();
    assert_eq!(count.load(SeqCst), 1);
    let mut fill = client(&Tape::fill(&dir));
    fill.get(&url(port, "/gz")).unwrap();
    assert_eq!(count.load(SeqCst), 1, "a recorded reply needs no request");
    let echo = fill.send(&Request::post(&url(port, "/echo"), &[], b"q=1")).unwrap();
    assert_eq!((echo.body.as_slice(), count.load(SeqCst)), (b"q=1".as_slice(), 2));
    // The reply that the fill got is in the directory now: a replay needs no server.
    let again = client(&Tape::replay(&dir)).send(&Request::post(&url(port, "/echo"), &[], b"q=1")).unwrap();
    assert_eq!((again.body.as_slice(), count.load(SeqCst)), (b"q=1".as_slice(), 2));
    let _ = std::fs::remove_dir_all(&dir);
}
