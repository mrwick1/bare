//! HTTP/2 through `bare_net::http::Client`, against a loopback `openssl s_server -alpn h2` that the test drives frame by frame.
//! The trust anchor is a synthetic root made for each test (see tools/crypto/openssl_harness.rs). `s_server` serves one connection at a
//! time, so a client that opens a second connection while it holds the first would hang: that is how the tests see "one connection".
//! These tests skip (and say so) when `openssl` is not installed.

#[path = "../../../tools/crypto/openssl_harness.rs"]
mod harness;
#[allow(dead_code)]
#[path = "../src/hpack.rs"]
mod hpack;

use bare_net::http::{Client, Method, Request};
use bare_tls::trust_store;
use bare_url::Url;
use harness::{Pki, Server, openssl_available, watch_client};
use std::io::Read;
use std::thread::JoinHandle;
use std::time::Duration;

macro_rules! need_openssl {
    () => {
        if !openssl_available() {
            eprintln!("skipped: openssl is not installed");
            return;
        }
    };
}

const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
const DATA: u8 = 0;
const HEADERS: u8 = 1;
const RST_STREAM: u8 = 3;
const SETTINGS: u8 = 4;
const PUSH_PROMISE: u8 = 5;
const PING: u8 = 6;
const GOAWAY: u8 = 7;
const WINDOW_UPDATE: u8 = 8;
const CONTINUATION: u8 = 9;
const END_STREAM: u8 = 1;
const END_HEADERS: u8 = 4;
const ACK: u8 = 1;

/// The scripted server: the frames that the client sent, and the frames that the test sends.
struct Peer {
    srv: Server,
    dec: hpack::Decoder,
}

struct Request2 {
    id: u32,
    headers: Vec<(String, String)>,
    end_stream: bool,
}

impl Request2 {
    fn get(&self, name: &str) -> &str {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str()).unwrap_or_else(|| panic!("no {name} in {:?}", self.headers))
    }
}

fn setup(alpn: &str) -> (Pki, Peer, Client) {
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    let srv = Server::start(&pki, "good", &["-alpn", alpn], true);
    let client = Client::with_roots(trust_store::decode_pem(&pki.root_pem()).unwrap());
    (pki, Peer { srv, dec: hpack::Decoder::new(4096) }, client)
}

fn url(p: &Peer, path: &str) -> Url {
    Url::parse(&format!("https://localhost:{}{path}", p.srv.port)).unwrap()
}

fn spawn<R: std::fmt::Debug + Send + 'static>(mut client: Client, f: impl FnOnce(&mut Client) -> R + Send + 'static) -> JoinHandle<(Client, R)> {
    let note = watch_client();
    std::thread::spawn(move || {
        let r = f(&mut client);
        *note.lock().unwrap() = Some(harness::client_result(&r));
        (client, r)
    })
}

fn frame(ty: u8, flags: u8, id: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = (payload.len() as u32).to_be_bytes()[1..].to_vec();
    out.extend_from_slice(&[ty, flags]);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// A header block made by hand: every field a literal without indexing, no Huffman. Not the encoder of the crate.
fn block(fields: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (n, v) in fields {
        if *n == ":status" && *v == "200" {
            out.push(0x88);
            continue;
        }
        out.push(0);
        out.push(n.len() as u8);
        out.extend_from_slice(n.as_bytes());
        out.push(v.len() as u8);
        out.extend_from_slice(v.as_bytes());
    }
    out
}

fn settings(pairs: &[(u16, u32)]) -> Vec<u8> {
    let payload: Vec<u8> = pairs.iter().flat_map(|(i, v)| [i.to_be_bytes().to_vec(), v.to_be_bytes().to_vec()].concat()).collect();
    frame(SETTINGS, 0, 0, &payload)
}

impl Peer {
    fn send(&mut self, bytes: &[u8]) {
        self.srv.send(bytes);
    }

    fn read_frame(&mut self) -> (u8, u8, u32, Vec<u8>) {
        let h = self.srv.take(9);
        let len = u32::from_be_bytes([0, h[0], h[1], h[2]]) as usize;
        let payload = self.srv.take(len);
        (h[3], h[4], u32::from_be_bytes([h[5], h[6], h[7], h[8]]), payload)
    }

    /// The next frame of this type. Frames of other types that the client sends on its own (the ACK of our SETTINGS, window updates) are skipped.
    fn next(&mut self, want: u8) -> (u8, u32, Vec<u8>) {
        loop {
            let (ty, flags, id, p) = self.read_frame();
            if ty == want {
                return (flags, id, p);
            }
            assert!(matches!(ty, SETTINGS | WINDOW_UPDATE | RST_STREAM), "waiting for type {want}, got type {ty} {p:?}");
        }
    }

    /// Read the preface and the settings of the client, and send ours. Gives the settings of the client.
    fn open(&mut self, ours: &[(u16, u32)]) -> Vec<(u16, u32)> {
        // Bytes of an earlier connection (a frame sent just before it closed) may come first.
        self.srv.expect(PREFACE);
        let (ty, _, id, p) = self.read_frame();
        assert_eq!((ty, id), (SETTINGS, 0));
        self.send(&settings(ours));
        p.chunks(6).map(|c| (u16::from_be_bytes([c[0], c[1]]), u32::from_be_bytes([c[2], c[3], c[4], c[5]]))).collect()
    }

    /// The client has applied our SETTINGS: it sends the ACK only after it did. A request that starts earlier may use the old limits.
    #[track_caller]
    fn acked(&mut self) {
        let (flags, ..) = self.next(SETTINGS);
        assert_eq!(flags, ACK, "not the ACK of our SETTINGS");
    }

    /// The next request: its HEADERS (and CONTINUATION) decoded.
    fn request(&mut self) -> Request2 {
        let (flags, id, mut data) = self.next(HEADERS);
        let end_stream = flags & END_STREAM != 0;
        let mut end = flags & END_HEADERS != 0;
        while !end {
            let (ty, f, i, p) = self.read_frame();
            assert_eq!((ty, i), (CONTINUATION, id));
            data.extend_from_slice(&p);
            end = f & END_HEADERS != 0;
        }
        let mut headers = Vec::new();
        self.dec.decode(&data, |n, v| headers.push((String::from_utf8_lossy(n).into_owned(), String::from_utf8_lossy(v).into_owned()))).unwrap();
        Request2 { id, headers, end_stream }
    }

    fn reply(&mut self, id: u32, fields: &[(&str, &str)], body: &[u8]) {
        let flags = END_HEADERS | if body.is_empty() { END_STREAM } else { 0 };
        let mut wire = frame(HEADERS, flags, id, &block(fields));
        if !body.is_empty() {
            wire.extend(frame(DATA, END_STREAM, id, body));
        }
        self.send(&wire);
    }

    fn ok(&mut self, id: u32, body: &[u8]) {
        self.reply(id, &[(":status", "200"), ("content-type", "text/plain")], body);
    }
}

/// The frames in some bytes that the client sent: (type, stream, payload length).
fn frames_in(bytes: &[u8]) -> Vec<(u8, u32, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 9 <= bytes.len() {
        let len = u32::from_be_bytes([0, bytes[at], bytes[at + 1], bytes[at + 2]]) as usize;
        out.push((bytes[at + 3], u32::from_be_bytes([bytes[at + 5], bytes[at + 6], bytes[at + 7], bytes[at + 8]]), len));
        at += 9 + len;
    }
    out
}

/// What the client sends in the next `ms` ms has no frame of this type.
#[track_caller]
fn none_of(peer: &mut Peer, ty: u8, ms: u64) {
    let seen = peer.srv.collect(ms);
    assert!(frames_in(&seen).iter().all(|f| f.0 != ty), "the client sent a frame of type {ty}: {:?}", frames_in(&seen));
}

/// DATA bytes from the client, up to `n` of them. Frames of the other types are skipped.
fn read_data(peer: &mut Peer, n: usize) -> Vec<u8> {
    let mut got = Vec::new();
    while got.len() < n {
        let (ty, _, _, p) = peer.read_frame();
        if ty == DATA {
            got.extend_from_slice(&p);
        } else {
            assert!(matches!(ty, SETTINGS | WINDOW_UPDATE), "type {ty}");
        }
    }
    assert_eq!(got.len(), n, "the client sent past the window");
    got
}

type Got = Result<(u16, Vec<u8>), String>;

fn get(url: Url) -> impl FnOnce(&mut Client) -> Got + Send + 'static {
    move |c| c.get(&url).map(|r| (r.status, r.body.clone())).map_err(|e| e.to_string())
}

#[test]
fn a_get_over_h2_sends_pseudo_headers_and_reads_the_reply() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("h2,http/1.1");
    let t = spawn(client, get(url(&peer, "/page?q=1")));
    let ours = peer.open(&[]);
    // Push is off, and the windows are the ones of the design.
    assert!(ours.contains(&(2, 0)), "{ours:?}");
    assert!(ours.contains(&(4, 1 << 20)), "{ours:?}");
    assert!(ours.contains(&(6, 256 * 1024)), "{ours:?}");
    let req = peer.request();
    assert_eq!(req.id, 1);
    assert!(req.end_stream);
    assert_eq!(req.headers[..4].iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), [":method", ":scheme", ":authority", ":path"]);
    assert_eq!(req.get(":method"), "GET");
    assert_eq!(req.get(":scheme"), "https");
    assert_eq!(req.get(":authority"), format!("localhost:{}", peer.srv.port));
    assert_eq!(req.get(":path"), "/page?q=1");
    assert_eq!(req.get("user-agent"), "bare/0.1");
    // No `accept-encoding` to a loopback host, as over HTTP/1.1. The unit test of the headers checks the rest.
    assert!(req.headers.iter().all(|(n, _)| n != "accept-encoding"));
    assert!(req.headers.iter().all(|(n, _)| n != "host" && n != "connection" && n != "transfer-encoding"), "{:?}", req.headers);
    // The reply in two DATA frames, with a content-length.
    let mut wire = frame(HEADERS, END_HEADERS, 1, &block(&[(":status", "200"), ("content-type", "text/plain"), ("content-length", "8")]));
    wire.extend(frame(DATA, 0, 1, b"hello "));
    wire.extend(frame(DATA, END_STREAM, 1, b"h2"));
    peer.send(&wire);
    assert_eq!(t.join().unwrap().1, Ok((200, b"hello h2".to_vec())));
    // The client sent the ACK of our settings and a window update for the connection, and nothing else.
}

#[test]
fn requests_at_once_share_one_connection_and_answers_may_come_in_any_order() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("h2");
    let c2 = client.fork();
    let (u1, u2) = (url(&peer, "/one"), url(&peer, "/two"));
    // The first request opens the session. The second finds it, so `s_server` (one connection at a time) never has to take another.
    let t1 = spawn(client, get(u1));
    peer.open(&[(3, 10)]);
    let r1 = peer.request();
    assert_eq!((r1.id, r1.get(":path")), (1, "/one"));
    let t2 = spawn(c2, get(u2));
    let r2 = peer.request();
    assert_eq!((r2.id, r2.get(":path")), (3, "/two"));
    // The second answer comes first.
    peer.ok(3, b"second");
    assert_eq!(t2.join().unwrap().1, Ok((200, b"second".to_vec())));
    peer.ok(1, b"first");
    assert_eq!(t1.join().unwrap().1, Ok((200, b"first".to_vec())));
}

#[test]
fn a_request_over_the_stream_limit_waits_for_a_slot() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("h2");
    let c2 = client.fork();
    let (u1, u2) = (url(&peer, "/one"), url(&peer, "/two"));
    let t1 = spawn(client, get(u1));
    peer.open(&[(3, 1)]);
    let r1 = peer.request();
    // The second request must find the limit of 1. Without the ACK, the reader may not have applied our SETTINGS yet, and a client may
    // send up to its default limit until it has (RFC 9113, 6.5.2).
    peer.acked();
    let t2 = spawn(c2, get(u2));
    // One stream at a time: the second HEADERS is not sent while the first stream is open.
    none_of(&mut peer, HEADERS, 300);
    peer.ok(r1.id, b"one");
    assert_eq!(t1.join().unwrap().1, Ok((200, b"one".to_vec())));
    let r2 = peer.request();
    assert_eq!((r2.id, r2.get(":path")), (3, "/two"));
    peer.ok(3, b"two");
    assert_eq!(t2.join().unwrap().1, Ok((200, b"two".to_vec())));
}

#[test]
fn a_ping_gets_an_ack_with_the_same_bytes_and_unknown_frames_are_ignored() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("h2");
    let t = spawn(client, get(url(&peer, "/")));
    peer.open(&[]);
    let r = peer.request();
    peer.send(&[frame(0xfa, 0, 0, b"unknown"), frame(PING, 0, 0, b"12345678")].concat());
    let (flags, id, p) = peer.next(PING);
    assert_eq!((flags, id, p.as_slice()), (1, 0, &b"12345678"[..]));
    peer.ok(r.id, b"pong");
    assert_eq!(t.join().unwrap().1, Ok((200, b"pong".to_vec())));
}

#[test]
fn goaway_sends_the_streams_it_did_not_handle_to_a_new_connection() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("h2");
    let t = spawn(client, get(url(&peer, "/again")));
    peer.open(&[]);
    let r = peer.request();
    // GOAWAY with last stream 0: stream 1 was not handled, so the client runs it again, here on a new connection.
    let mut wire = settings(&[]);
    wire.extend(frame(GOAWAY, 0, 0, &[0, 0, 0, 0, 0, 0, 0, 0]));
    peer.send(&wire);
    assert_eq!(r.get(":path"), "/again");
    // The client closes this connection, and `s_server` takes the new one. A new connection has a new table.
    peer.dec = hpack::Decoder::new(4096);
    peer.open(&[]);
    let again = peer.request();
    assert_eq!((again.id, again.get(":path")), (1, "/again"));
    peer.ok(1, b"second try");
    assert_eq!(t.join().unwrap().1, Ok((200, b"second try".to_vec())));
}

#[test]
fn goaway_lets_the_streams_it_did_handle_finish() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("h2");
    let t = spawn(client, get(url(&peer, "/done")));
    peer.open(&[]);
    let r = peer.request();
    peer.send(&frame(GOAWAY, 0, 0, &[0, 0, 0, 1, 0, 0, 0, 0]));
    peer.ok(r.id, b"finished");
    assert_eq!(t.join().unwrap().1, Ok((200, b"finished".to_vec())));
}

#[test]
fn rst_stream_fails_that_request_and_the_session_goes_on() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("h2");
    let c2 = client.fork();
    let (u1, u2) = (url(&peer, "/reset"), url(&peer, "/fine"));
    let t1 = spawn(client, get(u1));
    peer.open(&[]);
    let r1 = peer.request();
    // INTERNAL_ERROR: the server may have started to work on it, so the client does not run it again.
    peer.send(&frame(RST_STREAM, 0, r1.id, &2u32.to_be_bytes()));
    let err = t1.join().unwrap().1.unwrap_err();
    assert!(err.contains("reset the stream (code 2)"), "{err}");
    let t2 = spawn(c2, get(u2));
    let r2 = peer.request();
    assert_eq!((r2.id, r2.get(":path")), (3, "/fine"));
    peer.ok(3, b"still here");
    assert_eq!(t2.join().unwrap().1, Ok((200, b"still here".to_vec())));
}

#[test]
fn a_refused_stream_runs_again_on_a_new_connection() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("h2");
    let t = spawn(client, get(url(&peer, "/refused")));
    peer.open(&[]);
    let r = peer.request();
    // REFUSED_STREAM, and then GOAWAY so that the client lets go of this connection (`s_server` takes one at a time).
    peer.send(&[frame(RST_STREAM, 0, r.id, &7u32.to_be_bytes()), frame(GOAWAY, 0, 0, &[0; 8])].concat());
    peer.dec = hpack::Decoder::new(4096);
    peer.open(&[]);
    let again = peer.request();
    assert_eq!(again.get(":path"), "/refused");
    peer.ok(again.id, b"accepted");
    assert_eq!(t.join().unwrap().1, Ok((200, b"accepted".to_vec())));
}

#[test]
fn an_upload_stops_at_the_window_of_the_server_and_goes_on_when_it_opens() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("h2");
    let u = url(&peer, "/upload");
    let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let sent = body.clone();
    let t = spawn(client, move |c| c.post(&u, &[("content-type", "application/octet-stream")], &sent).map(|r| r.status).map_err(|e| e.to_string()));
    peer.open(&[]); // the default windows: 65,535 bytes for the connection and for the stream
    let r = peer.request();
    assert_eq!(r.get(":method"), "POST");
    assert_eq!(r.get("content-length"), "200000");
    assert!(!r.end_stream);
    let mut got = Vec::new();
    let take_data = |peer: &mut Peer, got: &mut Vec<u8>| loop {
        let (ty, flags, _, p) = peer.read_frame();
        match ty {
            DATA => {
                got.extend_from_slice(&p);
                if flags & END_STREAM != 0 {
                    return true;
                }
            }
            _ => assert!(matches!(ty, SETTINGS | WINDOW_UPDATE)),
        }
        if got.len() >= 65_535 {
            return false;
        }
    };
    assert!(!take_data(&mut peer, &mut got));
    assert_eq!(got.len(), 65_535, "the client sends exactly the window and stops");
    none_of(&mut peer, DATA, 300);
    // Open the window of the connection only: the stream window is still empty, so it still waits.
    peer.send(&frame(WINDOW_UPDATE, 0, 0, &200_000u32.to_be_bytes()));
    none_of(&mut peer, DATA, 300);
    peer.send(&frame(WINDOW_UPDATE, 0, r.id, &200_000u32.to_be_bytes()));
    while !take_data(&mut peer, &mut got) {}
    assert_eq!(got, body);
    peer.reply(r.id, &[(":status", "200")], b"");
    assert_eq!(t.join().unwrap().1, Ok(200));
}

#[test]
fn a_new_initial_window_moves_the_open_streams() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("h2");
    let u = url(&peer, "/moves");
    let body: Vec<u8> = (0..100_000u32).map(|i| (i % 249) as u8).collect();
    let sent = body.clone();
    let t = spawn(client, move |c| c.post(&u, &[], &sent).map(|r| r.status).map_err(|e| e.to_string()));
    peer.open(&[]);
    let r = peer.request();
    // The connection has room for all of it. The stream has 65,535 bytes: that is the stop.
    peer.send(&frame(WINDOW_UPDATE, 0, 0, &1_000_000u32.to_be_bytes()));
    let mut got = read_data(&mut peer, 65_535);
    none_of(&mut peer, DATA, 300);
    // An initial window that is 20,000 bytes more gives each open stream 20,000 bytes more, by the difference (RFC 9113 section 6.9.2).
    peer.send(&settings(&[(4, 65_535 + 20_000)]));
    got.extend(read_data(&mut peer, 20_000));
    none_of(&mut peer, DATA, 300);
    peer.send(&settings(&[(4, 200_000)]));
    got.extend(read_data(&mut peer, 100_000 - 85_535));
    assert_eq!(got, body);
    peer.reply(r.id, &[(":status", "200")], b"");
    assert_eq!(t.join().unwrap().1, Ok(200));
}

#[test]
fn the_window_of_the_client_follows_what_the_caller_reads() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("h2");
    let u = url(&peer, "/big");
    let (go, wait) = std::sync::mpsc::channel::<()>();
    let t = spawn(client, move |c| {
        let mut s = c.get_stream(&u).map_err(|e| e.to_string())?;
        // The caller reads nothing until the test says so. A fixed sleep ends too soon on a loaded machine.
        let _ = wait.recv();
        let mut all = Vec::new();
        s.body.read_to_end(&mut all).map_err(|e| e.to_string())?;
        Ok::<_, String>(all.len())
    });
    peer.open(&[]);
    let r = peer.request();
    peer.send(&frame(HEADERS, END_HEADERS, r.id, &block(&[(":status", "200")])));
    // The server may send the stream window and no more. It sends the whole window in frames of 16 KiB.
    for _ in 0..64 {
        peer.send(&frame(DATA, 0, r.id, &[1u8; 16384]));
    }
    // Nothing was read yet, so no window update for this stream may come (the one for the connection came at the start).
    let seen = peer.srv.collect(300);
    let mut at = 0;
    while at + 9 <= seen.len() {
        let len = u32::from_be_bytes([0, seen[at], seen[at + 1], seen[at + 2]]) as usize;
        let (ty, id) = (seen[at + 3], u32::from_be_bytes([seen[at + 5], seen[at + 6], seen[at + 7], seen[at + 8]]));
        assert!(!(ty == WINDOW_UPDATE && id == r.id), "a window update for the stream before the caller read");
        at += 9 + len;
    }
    // The caller reads. Now the updates come, and the server sends the rest.
    go.send(()).unwrap();
    let mut credit = 0u64;
    let end = std::time::Instant::now() + Duration::from_secs(10);
    let mut sent = 64 * 16384u64;
    while sent < 1_500_000 && std::time::Instant::now() < end {
        let (ty, _, id, p) = peer.read_frame();
        if ty == WINDOW_UPDATE && id == r.id {
            credit += u32::from_be_bytes(p.try_into().unwrap()) as u64;
            while sent < (1 << 20) + credit && sent < 1_500_000 {
                peer.send(&frame(DATA, 0, r.id, &[1u8; 16384]));
                sent += 16384;
            }
        }
    }
    peer.send(&frame(DATA, END_STREAM, r.id, b""));
    assert_eq!(t.join().unwrap().1, Ok(sent as usize));
}

#[test]
fn headers_over_the_list_limit_fail_that_stream_and_keep_the_table_in_step() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("h2");
    let c2 = client.fork();
    let (u1, u2) = (url(&peer, "/huge"), url(&peer, "/after"));
    let t1 = spawn(client, get(u1));
    peer.open(&[]);
    let r1 = peer.request();
    // 80 headers with 4,000 `a` each: 323 KB in the list (the limit is 256 KiB), and 200 KB on the wire in Huffman code (the block limit
    // is 256 KiB). The block ends with an entry for the table.
    let mut b = vec![0x40, 3, b'x', b'-', b'a', 1, b'1']; // incremental: x-a: 1 (index 62)
    for i in 0..80 {
        let name = format!("x-h{i}");
        b.extend_from_slice(&[0, name.len() as u8]);
        b.extend_from_slice(name.as_bytes());
        // 'a' is the 5 bit code 00011: 8 of them are the 5 bytes 18 c6 31 8c 63. A string of 500 times these bytes.
        b.extend(hpack_len(2500, true));
        b.extend([0x18, 0xc6, 0x31, 0x8c, 0x63].repeat(500));
    }
    b.extend_from_slice(&[0x40, 3, b'x', b'-', b'z', 1, b'9']); // incremental: x-z: 9 (index 62, x-a moves to 63)
    let mut wire = frame(HEADERS, END_STREAM, r1.id, &[&[0x88][..], &b[..16_000]].concat());
    let mut rest = &b[16_000..];
    while !rest.is_empty() {
        let n = rest.len().min(16_000);
        let last = n == rest.len();
        wire.extend(frame(CONTINUATION, if last { END_HEADERS } else { 0 }, r1.id, &rest[..n]));
        rest = &rest[n..];
    }
    peer.send(&wire);
    let err = t1.join().unwrap().1.unwrap_err();
    assert!(err.contains("too long"), "{err}");
    // The client reset that stream, and the session works. The second reply uses index 62: it must be `x-z: 9`.
    let (_, id, p) = peer.next(RST_STREAM);
    assert_eq!((id, u32::from_be_bytes(p.try_into().unwrap())), (r1.id, 8));
    let t2 = spawn(c2, get(u2));
    let r2 = peer.request();
    let mut wire = frame(HEADERS, END_HEADERS, r2.id, &[0x88, 0xbe, 0xbf]);
    wire.extend(frame(DATA, END_STREAM, r2.id, b"after"));
    peer.send(&wire);
    assert_eq!(t2.join().unwrap().1, Ok((200, b"after".to_vec())));
}

/// The length of a string: 7 bit prefix, and the flag for Huffman code.
fn hpack_len(n: usize, huffman: bool) -> Vec<u8> {
    let flag = if huffman { 0x80 } else { 0 };
    assert!(n >= 127);
    let mut out = vec![flag | 127];
    let mut rest = n - 127;
    while rest >= 128 {
        out.push((rest & 127) as u8 | 128);
        rest >>= 7;
    }
    out.push(rest as u8);
    out
}

#[test]
fn a_header_block_over_the_block_limit_ends_the_connection() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("h2");
    let t = spawn(client, get(url(&peer, "/flood")));
    peer.open(&[]);
    let r = peer.request();
    // CONTINUATION frames with no end: the client must stop at its limit, not hold them all.
    let mut wire = frame(HEADERS, 0, r.id, &[0u8; 16_000]);
    for _ in 0..20 {
        wire.extend(frame(CONTINUATION, 0, r.id, &[0u8; 16_000]));
    }
    peer.send(&wire);
    let (_, id, p) = peer.next(GOAWAY);
    assert_eq!(id, 0);
    assert_eq!(u32::from_be_bytes(p[4..8].try_into().unwrap()), 11, "ENHANCE_YOUR_CALM");
    assert!(t.join().unwrap().1.is_err());
}

/// The client stops at its block limit while the server still sends. The server must get the `GOAWAY`: a client that closes a socket with
/// unread bytes sends a TCP reset, and the server drops what it has not read (the `GOAWAY`) when the reset comes. It is a race, so the
/// flood is far over the socket buffers: with no linger in the client, the reset comes while most of it is still on the way.
#[test]
fn a_connection_error_while_the_server_still_sends_still_delivers_goaway() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("h2");
    let t = spawn(client, get(url(&peer, "/flood")));
    peer.open(&[]);
    let r = peer.request();
    let mut wire = frame(HEADERS, 0, r.id, &[0u8; 16_000]);
    for _ in 0..100 {
        wire.extend(frame(CONTINUATION, 0, r.id, &[0u8; 16_000]));
    }
    // `s_server` stops reading its input when the client ends its side, so the send must not wait for it.
    peer.srv.send_detached(wire);
    let (_, id, p) = peer.next(GOAWAY);
    assert_eq!(id, 0);
    assert_eq!(u32::from_be_bytes(p[4..8].try_into().unwrap()), 11, "ENHANCE_YOUR_CALM");
    assert!(t.join().unwrap().1.is_err());
}

#[test]
fn push_promise_and_other_protocol_errors_end_the_connection_with_goaway() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("h2");
    let t = spawn(client, get(url(&peer, "/push")));
    peer.open(&[]);
    let r = peer.request();
    peer.send(&frame(PUSH_PROMISE, END_HEADERS, r.id, &[0, 0, 0, 2, 0x88]));
    let (_, _, p) = peer.next(GOAWAY);
    assert_eq!(u32::from_be_bytes(p[4..8].try_into().unwrap()), 1, "PROTOCOL_ERROR");
    assert!(t.join().unwrap().1.is_err());
}

#[test]
fn a_body_that_does_not_match_its_content_length_is_an_error() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("h2");
    let c2 = client.fork();
    let (u1, u2) = (url(&peer, "/short"), url(&peer, "/long"));
    let t1 = spawn(client, get(u1));
    peer.open(&[]);
    let r1 = peer.request();
    let mut wire = frame(HEADERS, END_HEADERS, r1.id, &block(&[(":status", "200"), ("content-length", "10")]));
    wire.extend(frame(DATA, END_STREAM, r1.id, b"12345"));
    peer.send(&wire);
    assert!(t1.join().unwrap().1.is_err());
    let t2 = spawn(c2, get(u2));
    let r2 = peer.request();
    let mut wire = frame(HEADERS, END_HEADERS, r2.id, &block(&[(":status", "200"), ("content-length", "3")]));
    wire.extend(frame(DATA, END_STREAM, r2.id, b"12345"));
    peer.send(&wire);
    assert!(t2.join().unwrap().1.is_err());
}

#[test]
fn a_reply_with_http1_headers_or_capitals_is_refused() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("h2");
    let t = spawn(client, get(url(&peer, "/bad")));
    peer.open(&[]);
    let r = peer.request();
    peer.reply(r.id, &[(":status", "200"), ("Content-Type", "text/plain")], b"x");
    let err = t.join().unwrap().1.unwrap_err();
    assert!(err.contains("not allowed"), "{err}");
}

#[test]
fn a_connection_that_closes_in_the_middle_of_a_body_is_a_cut_body() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("h2");
    let t = spawn(client, get(url(&peer, "/cut")));
    peer.open(&[]);
    let r = peer.request();
    let mut wire = frame(HEADERS, END_HEADERS, r.id, &block(&[(":status", "200")]));
    wire.extend(frame(DATA, 0, r.id, b"half"));
    peer.send(&wire);
    std::thread::sleep(Duration::from_millis(100));
    peer.srv.reset();
    let err = t.join().unwrap().1.unwrap_err();
    assert!(err.contains("ended") || err.contains("eof") || err.contains("closed"), "{err}");
}

#[test]
fn a_server_that_speaks_only_http11_gets_http11() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("http/1.1");
    let t = spawn(client, get(url(&peer, "/h1")));
    let req = String::from_utf8(peer.srv.expect(b"\r\n\r\n")).unwrap();
    assert!(req.starts_with("GET /h1 HTTP/1.1\r\nHost: localhost:"), "{req}");
    peer.send(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nh1");
    assert_eq!(t.join().unwrap().1, Ok((200, b"h1".to_vec())));
}

#[test]
fn a_head_reply_has_no_body_and_the_reply_to_a_post_carries_its_headers() {
    need_openssl!();
    let (_pki, mut peer, client) = setup("h2");
    let c2 = client.fork();
    let (u1, u2) = (url(&peer, "/head"), url(&peer, "/post"));
    let t1 = spawn(client, move |c| c.send(&Request { method: Method::Head, url: &u1, headers: &[], body: &[] }).map(|r| (r.status, r.header("content-length").map(str::to_string), r.body.len())).map_err(|e| e.to_string()));
    peer.open(&[]);
    let r1 = peer.request();
    assert_eq!((r1.get(":method"), r1.end_stream), ("HEAD", true));
    // A HEAD reply says how long the body would be, and has none.
    peer.reply(r1.id, &[(":status", "200"), ("content-length", "1234")], b"");
    assert_eq!(t1.join().unwrap().1, Ok((200, Some("1234".to_string()), 0)));
    let t2 = spawn(c2, move |c| c.post(&u2, &[("x-token", "secret")], b"{}").map(|r| (r.status, r.header("x-reply").map(str::to_string))).map_err(|e| e.to_string()));
    let r2 = peer.request();
    assert_eq!((r2.get(":method"), r2.get("content-length"), r2.get("x-token")), ("POST", "2", "secret"));
    let (_, id, p) = peer.next(DATA);
    assert_eq!((id, p.as_slice()), (r2.id, &b"{}"[..]));
    peer.reply(r2.id, &[(":status", "200"), ("x-reply", "yes")], b"");
    assert_eq!(t2.join().unwrap().1, Ok((200, Some("yes".to_string()))));
}
