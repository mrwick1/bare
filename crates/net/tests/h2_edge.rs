//! Edge cases of the HTTP/2 client against a loopback `openssl s_server -alpn h2` that the test drives frame by frame, as in
//! `h2_openssl.rs`: padded frames, PRIORITY, interim replies, trailers, CONTINUATION, WINDOW_UPDATE and SETTINGS edge cases, GOAWAY with a
//! last stream id, PUSH_PROMISE, and the error code of each protocol fault. The trust anchor is a synthetic root made for each test.
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
const PRIORITY: u8 = 2;
const RST_STREAM: u8 = 3;
const SETTINGS: u8 = 4;
const PUSH_PROMISE: u8 = 5;
const PING: u8 = 6;
const GOAWAY: u8 = 7;
const WINDOW_UPDATE: u8 = 8;
const CONTINUATION: u8 = 9;
const END_STREAM: u8 = 1;
const ACK: u8 = 1;
const END_HEADERS: u8 = 4;
const PADDED: u8 = 8;
const PRIORITY_FLAG: u8 = 0x20;

const PROTOCOL_ERROR: u32 = 1;
const FLOW_CONTROL_ERROR: u32 = 3;
const FRAME_SIZE_ERROR: u32 = 6;
const COMPRESSION_ERROR: u32 = 9;

struct Peer {
    srv: Server,
    dec: hpack::Decoder,
}

struct Req {
    id: u32,
    headers: Vec<(String, String)>,
    end_stream: bool,
}

impl Req {
    fn get(&self, name: &str) -> &str {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str()).unwrap_or_else(|| panic!("no {name} in {:?}", self.headers))
    }
}

fn setup() -> (Pki, Peer, Client) {
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    let srv = Server::start(&pki, "good", &["-alpn", "h2"], true);
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

/// A padded frame: the pad length, the payload, and `pad` zero bytes.
fn padded(ty: u8, flags: u8, id: u32, pad: u8, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![pad];
    p.extend_from_slice(payload);
    p.extend(std::iter::repeat_n(0, pad as usize));
    frame(ty, flags | PADDED, id, &p)
}

/// A header block made by hand: literals without indexing, no Huffman (`:status 200` is the static index 8).
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

    /// The next frame of this type. Frames of other types that the client sends on its own are skipped.
    fn next(&mut self, want: u8) -> (u8, u32, Vec<u8>) {
        loop {
            let (ty, flags, id, p) = self.read_frame();
            if ty == want {
                return (flags, id, p);
            }
            assert!(matches!(ty, SETTINGS | WINDOW_UPDATE | RST_STREAM) || (ty == DATA && want != DATA), "waiting for type {want}, got type {ty} {p:?}");
        }
    }

    /// A new connection: the preface and the settings of the client, and ours. The table of the client is new too.
    fn open(&mut self, ours: &[(u16, u32)]) -> Vec<(u16, u32)> {
        self.dec = hpack::Decoder::new(4096);
        self.srv.expect(PREFACE);
        let (ty, _, id, p) = self.read_frame();
        assert_eq!((ty, id), (SETTINGS, 0));
        self.send(&settings(ours));
        p.chunks(6).map(|c| (u16::from_be_bytes([c[0], c[1]]), u32::from_be_bytes([c[2], c[3], c[4], c[5]]))).collect()
    }

    fn request(&mut self) -> Req {
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
        Req { id, headers, end_stream }
    }

    /// `HEADERS 200` with these extra fields.
    fn head(&mut self, id: u32, extra: &[(&str, &str)]) {
        let mut fields = vec![(":status", "200")];
        fields.extend_from_slice(extra);
        self.send(&frame(HEADERS, END_HEADERS, id, &block(&fields)));
    }

    /// Wait until the client has handled every frame that we sent: a PING comes back after them, in order.
    fn sync(&mut self) {
        self.send(&frame(PING, 0, 0, b"syncsync"));
        loop {
            let (ty, flags, _, p) = self.read_frame();
            if ty == PING && flags & ACK != 0 && p == b"syncsync" {
                return;
            }
        }
    }

    fn goaway_code(&mut self) -> u32 {
        let (_, id, p) = self.next(GOAWAY);
        assert_eq!(id, 0);
        u32::from_be_bytes(p[4..8].try_into().unwrap())
    }

    fn rst_code(&mut self, id: u32) -> u32 {
        let (_, sid, p) = self.next(RST_STREAM);
        assert_eq!(sid, id);
        u32::from_be_bytes(p[..4].try_into().unwrap())
    }
}

/// What the client sends in the next `ms` ms has no frame of this type.
#[track_caller]
fn none_of(peer: &mut Peer, ty: u8, ms: u64) {
    let seen = peer.srv.collect(ms);
    let mut at = 0;
    while at + 9 <= seen.len() {
        let len = u32::from_be_bytes([0, seen[at], seen[at + 1], seen[at + 2]]) as usize;
        assert!(seen[at + 3] != ty, "the client sent a frame of type {ty}");
        at += 9 + len;
    }
}

type Got = Result<(u16, Vec<u8>), String>;

fn get(url: Url) -> impl FnOnce(&mut Client) -> Got + Send + 'static {
    move |c| c.get(&url).map(|r| (r.status, r.body.clone())).map_err(|e| e.to_string())
}

fn post(url: Url, body: Vec<u8>) -> impl FnOnce(&mut Client) -> Result<u16, String> + Send + 'static {
    move |c| c.post(&url, &[], &body).map(|r| r.status).map_err(|e| e.to_string())
}

#[test]
fn padded_headers_and_data_are_read() {
    need_openssl!();
    let (_pki, mut peer, client) = setup();
    let t = spawn(client, get(url(&peer, "/pad")));
    peer.open(&[]);
    let r = peer.request();
    // HEADERS with padding and the priority fields: pad length, 5 bytes of priority, the block, the padding.
    let mut payload = vec![3, 0, 0, 0, 0, 15];
    payload.extend(block(&[(":status", "200"), ("content-type", "text/plain")]));
    payload.extend([0, 0, 0]);
    let mut wire = frame(HEADERS, END_HEADERS | PADDED | PRIORITY_FLAG, r.id, &payload);
    wire.extend(padded(DATA, 0, r.id, 10, b"hello "));
    wire.extend(padded(DATA, 0, r.id, 0, b"pad"));
    wire.extend(padded(DATA, END_STREAM, r.id, 255, b"ded"));
    peer.send(&wire);
    assert_eq!(t.join().unwrap().1, Ok((200, b"hello padded".to_vec())));
}

#[test]
fn padded_frames_with_no_data_do_not_stall_the_windows() {
    need_openssl!();
    let (_pki, mut peer, client) = setup();
    let t = spawn(client, get(url(&peer, "/pad-only")));
    peer.open(&[]);
    let r = peer.request();
    peer.head(r.id, &[]);
    // The server obeys the windows: 4 MiB for the connection (the client's first WINDOW_UPDATE came with the request), 1 MiB for the stream. A padded frame holds 255 bytes of padding
    // at most, so 5,000 frames with no data are 1.3 MB: more than the window of the stream. With no data to read, only the client's
    // count of the padding can give the window back.
    let (mut conn, mut stream) = (4i64 << 20, 1i64 << 20);
    for i in 0..5100 {
        let data: &[u8] = if i < 5000 { b"" } else { b"x" };
        let size = 256 + data.len() as i64;
        while conn < size || stream < size {
            let (ty, _, id, p) = peer.read_frame();
            if ty == WINDOW_UPDATE {
                let add = u32::from_be_bytes(p[..4].try_into().unwrap()) as i64;
                if id == 0 { conn += add } else { stream += add }
            }
        }
        conn -= size;
        stream -= size;
        peer.send(&padded(DATA, 0, r.id, 255, data));
    }
    peer.send(&frame(DATA, END_STREAM, r.id, b"!"));
    assert_eq!(t.join().unwrap().1, Ok((200, [vec![b'x'; 100], b"!".to_vec()].concat())));
}

#[test]
fn priority_frames_are_ignored_and_a_short_one_fails_only_its_stream() {
    need_openssl!();
    let (_pki, mut peer, client) = setup();
    let c2 = client.fork();
    let t = spawn(client, get(url(&peer, "/prio")));
    peer.open(&[]);
    let r = peer.request();
    // A PRIORITY frame for the open stream, for a stream that does not exist yet (legal: a tree node), and in the middle of the reply.
    let prio = [0, 0, 0, 0, 200];
    let mut wire = frame(PRIORITY, 0, r.id, &prio);
    wire.extend(frame(PRIORITY, 0, 99, &prio));
    wire.extend(frame(HEADERS, END_HEADERS | PRIORITY_FLAG, r.id, &[&[0, 0, 0, 0, 16][..], &block(&[(":status", "200")])].concat()));
    wire.extend(frame(PRIORITY, 0, r.id, &prio));
    wire.extend(frame(DATA, 0, r.id, b"ab"));
    wire.extend(frame(PRIORITY, 0, r.id, &prio));
    wire.extend(frame(DATA, END_STREAM, r.id, b"cd"));
    wire.extend(frame(PRIORITY, 0, r.id, &prio));
    peer.send(&wire);
    assert_eq!(t.join().unwrap().1, Ok((200, b"abcd".to_vec())));
    // A PRIORITY frame that is not 5 bytes long is an error of the stream (RFC 9113 section 6.3), not of the connection.
    let t = spawn(c2, post(url(&peer, "/short"), vec![1]));
    let r = peer.request();
    peer.send(&frame(PRIORITY, 0, r.id, &[0, 0, 0]));
    assert_eq!(peer.rst_code(r.id), FRAME_SIZE_ERROR);
    let (c2, got) = t.join().unwrap();
    assert!(got.is_err());
    // The session is still good.
    let t = spawn(c2, get(url(&peer, "/after")));
    let r = peer.request();
    assert_eq!(r.id, 5);
    // A short PRIORITY for a stream that is reset or unknown needs no answer.
    peer.send(&[frame(PRIORITY, 0, 3, &[0, 0]), frame(PRIORITY, 0, 55, &[1])].concat());
    peer.head(5, &[("content-length", "2")]);
    peer.send(&frame(DATA, END_STREAM, 5, b"ok"));
    assert_eq!(t.join().unwrap().1, Ok((200, b"ok".to_vec())));
}

#[test]
fn interim_replies_come_first_and_the_final_reply_is_the_answer() {
    need_openssl!();
    let (_pki, mut peer, client) = setup();
    let c2 = client.fork();
    let u = url(&peer, "/i");
    let t = spawn(client, move |c| c.get(&u).map(|r| (r.status, r.header("link").map(str::to_string), r.body.clone())).map_err(|e| e.to_string()));
    peer.open(&[]);
    let r = peer.request();
    // 100, then two 103 (with Link), then 200. The Link of a 103 is not a header of the answer.
    let mut wire = frame(HEADERS, END_HEADERS, r.id, &block(&[(":status", "100")]));
    wire.extend(frame(HEADERS, END_HEADERS, r.id, &block(&[(":status", "103"), ("link", "</a.css>; rel=preload")])));
    wire.extend(frame(HEADERS, END_HEADERS, r.id, &block(&[(":status", "103"), ("link", "</b.js>; rel=preload")])));
    peer.send(&wire);
    none_of(&mut peer, RST_STREAM, 200);
    peer.head(r.id, &[("content-length", "2")]);
    peer.send(&frame(DATA, END_STREAM, r.id, b"ok"));
    assert_eq!(t.join().unwrap().1, Ok((200, None, b"ok".to_vec())));
    // An interim reply that ends the stream, and DATA before the final reply, are errors of the stream.
    let u = url(&peer, "/j");
    let t = spawn(c2, post(u, vec![1]));
    let r = peer.request();
    peer.send(&frame(HEADERS, END_HEADERS | END_STREAM, r.id, &block(&[(":status", "100")])));
    assert_eq!(peer.rst_code(r.id), PROTOCOL_ERROR);
    let (c2, got) = t.join().unwrap();
    assert!(got.is_err());
    let t = spawn(c2, post(url(&peer, "/k"), vec![1]));
    let r = peer.request();
    peer.send(&[frame(HEADERS, END_HEADERS, r.id, &block(&[(":status", "100")])), frame(DATA, 0, r.id, b"x")].concat());
    assert_eq!(peer.rst_code(r.id), PROTOCOL_ERROR);
    assert!(t.join().unwrap().1.is_err());
}

#[test]
fn trailers_end_the_stream_and_are_not_headers() {
    need_openssl!();
    let (_pki, mut peer, client) = setup();
    let u = url(&peer, "/t");
    let t = spawn(client, move |c| c.get(&u).map(|r| (r.status, r.header("x-sum").map(str::to_string), r.body.clone())).map_err(|e| e.to_string()));
    peer.open(&[]);
    let r = peer.request();
    peer.head(r.id, &[("content-length", "4")]);
    peer.send(&frame(DATA, 0, r.id, b"abcd"));
    // The trailers come in HEADERS and CONTINUATION, with END_STREAM on the HEADERS frame.
    let trailer = block(&[("x-sum", "42")]);
    let mut wire = frame(HEADERS, END_STREAM, r.id, &trailer[..4]);
    wire.extend(frame(CONTINUATION, END_HEADERS, r.id, &trailer[4..]));
    peer.send(&wire);
    assert_eq!(t.join().unwrap().1, Ok((200, None, b"abcd".to_vec())));
}

#[test]
fn bad_trailers_fail_their_stream_only() {
    need_openssl!();
    let (_pki, mut peer, client) = setup();
    let mut c = client;
    let mut first = true;
    // (what follows the data, the error code of the stream)
    let cases: [(&str, Vec<u8>); 3] = [
        ("trailers that do not end the stream", frame(HEADERS, END_HEADERS, 0, &block(&[("x-sum", "42")]))),
        ("a pseudo header in the trailers", frame(HEADERS, END_HEADERS | END_STREAM, 0, &block(&[(":status", "200")]))),
        ("a second :status", frame(HEADERS, END_HEADERS | END_STREAM, 0, &block(&[(":path", "/")]))),
    ];
    for (name, trailer) in cases {
        let t = spawn(c, post(url(&peer, "/bad-trailer"), vec![1]));
        if first {
            peer.open(&[]);
            first = false;
        }
        let r = peer.request();
        peer.head(r.id, &[]);
        let mut trailer = trailer;
        trailer[5..9].copy_from_slice(&r.id.to_be_bytes());
        peer.send(&[frame(DATA, 0, r.id, b"abcd"), trailer].concat());
        assert_eq!(peer.rst_code(r.id), PROTOCOL_ERROR, "{name}");
        let (c2, got) = t.join().unwrap();
        assert!(got.is_err(), "{name}");
        c = c2;
    }
    // Trailers after a short body: the body is shorter than its Content-Length.
    let t = spawn(c, post(url(&peer, "/short-trailer"), vec![1]));
    let r = peer.request();
    peer.head(r.id, &[("content-length", "10")]);
    peer.send(&[frame(DATA, 0, r.id, b"abcd"), frame(HEADERS, END_HEADERS | END_STREAM, r.id, &block(&[("x-sum", "1")]))].concat());
    assert!(t.join().unwrap().1.is_err());
}

#[test]
fn a_header_block_in_continuation_frames_is_one_block() {
    need_openssl!();
    let (_pki, mut peer, client) = setup();
    let t = spawn(client, get(url(&peer, "/c")));
    peer.open(&[]);
    let r = peer.request();
    let b = block(&[(":status", "200"), ("x-a", "1"), ("x-b", &"b".repeat(100)), ("content-length", "3")]);
    // Three pieces. The first cut is inside a literal, and one CONTINUATION has no bytes.
    let mut wire = frame(HEADERS, 0, r.id, &b[..3]);
    wire.extend(frame(CONTINUATION, 0, r.id, &b[3..50]));
    wire.extend(frame(CONTINUATION, 0, r.id, &[]));
    wire.extend(frame(CONTINUATION, END_HEADERS, r.id, &b[50..]));
    wire.extend(frame(DATA, END_STREAM, r.id, b"xyz"));
    peer.send(&wire);
    assert_eq!(t.join().unwrap().1, Ok((200, b"xyz".to_vec())));
}

#[test]
fn window_update_of_zero_fails_the_stream_and_a_late_one_is_ignored() {
    need_openssl!();
    let (_pki, mut peer, client) = setup();
    let c2 = client.fork();
    let t = spawn(client, get(url(&peer, "/z")));
    peer.open(&[]);
    let r = peer.request();
    peer.send(&frame(WINDOW_UPDATE, 0, r.id, &0u32.to_be_bytes()));
    assert_eq!(peer.rst_code(r.id), PROTOCOL_ERROR);
    let (client, got) = t.join().unwrap();
    assert!(got.is_err());
    // Updates for a stream that is reset, for one that is done and one that is gone: no error, the session goes on.
    let t = spawn(c2, get(url(&peer, "/y")));
    let r2 = peer.request();
    let inc = 1000u32.to_be_bytes();
    peer.send(&[frame(WINDOW_UPDATE, 0, r.id, &inc), frame(WINDOW_UPDATE, 0, r2.id, &(0x8000_0000u32 | 5).to_be_bytes())].concat());
    peer.head(r2.id, &[("content-length", "1")]);
    peer.send(&frame(DATA, END_STREAM, r2.id, b"y"));
    assert_eq!(t.join().unwrap().1, Ok((200, b"y".to_vec())));
    peer.send(&frame(WINDOW_UPDATE, 0, r2.id, &inc));
    let t = spawn(client, get(url(&peer, "/x")));
    let r3 = peer.request();
    assert_eq!(r3.id, 5);
    peer.head(5, &[("content-length", "1")]);
    peer.send(&frame(DATA, END_STREAM, 5, b"x"));
    assert_eq!(t.join().unwrap().1, Ok((200, b"x".to_vec())));
}

#[test]
fn a_window_update_that_passes_2_pow_31_fails_the_stream_only() {
    need_openssl!();
    let (_pki, mut peer, client) = setup();
    let t = spawn(client, post(url(&peer, "/w"), vec![7; 100_000]));
    peer.open(&[]);
    let r = peer.request();
    // Two updates of 2^31-1: whatever the client has sent so far, the window of the stream passes 2^31-1.
    peer.send(&[frame(WINDOW_UPDATE, 0, r.id, &0x7fff_ffffu32.to_be_bytes()), frame(WINDOW_UPDATE, 0, r.id, &0x7fff_ffffu32.to_be_bytes())].concat());
    assert_eq!(peer.rst_code(r.id), FLOW_CONTROL_ERROR);
    assert!(t.join().unwrap().1.is_err());
}

#[test]
fn a_window_update_after_the_end_of_a_reply_does_not_cut_the_body() {
    need_openssl!();
    let (_pki, mut peer, client) = setup();
    let u = url(&peer, "/late");
    let t = spawn(client, move |c| {
        let mut s = c.get_stream(&u).map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(500));
        let mut all = Vec::new();
        std::io::Read::read_to_end(&mut s.body, &mut all).map_err(|e| e.to_string())?;
        Ok::<_, String>(all)
    });
    peer.open(&[]);
    let r = peer.request();
    peer.head(r.id, &[("content-length", "5")]);
    // The reply is complete and not read yet. Bad updates for its stream (0, and past 2^31) must not throw the body away.
    peer.send(&[frame(DATA, END_STREAM, r.id, b"fully"), frame(WINDOW_UPDATE, 0, r.id, &0u32.to_be_bytes()), frame(WINDOW_UPDATE, 0, r.id, &0x7fff_ffffu32.to_be_bytes())].concat());
    assert_eq!(t.join().unwrap().1, Ok(b"fully".to_vec()));
}

#[test]
fn a_stream_window_that_goes_negative_waits_until_it_is_positive() {
    need_openssl!();
    let (_pki, mut peer, client) = setup();
    let body: Vec<u8> = (0..100_000u32).map(|i| (i % 247) as u8).collect();
    let t = spawn(client, post(url(&peer, "/neg"), body.clone()));
    peer.open(&[]);
    let r = peer.request();
    peer.send(&frame(WINDOW_UPDATE, 0, 0, &1_000_000u32.to_be_bytes()));
    let mut got = read_data(&mut peer, 65_535);
    none_of(&mut peer, DATA, 200);
    // The initial window drops to 1,000: each open stream moves by the difference, to -64,535.
    peer.send(&settings(&[(4, 1000)]));
    none_of(&mut peer, DATA, 200);
    peer.send(&frame(WINDOW_UPDATE, 0, r.id, &64_000u32.to_be_bytes()));
    none_of(&mut peer, DATA, 200);
    // -535 + 1,535 = 1,000.
    peer.send(&frame(WINDOW_UPDATE, 0, r.id, &1_535u32.to_be_bytes()));
    got.extend(read_data(&mut peer, 1000));
    none_of(&mut peer, DATA, 200);
    peer.send(&settings(&[(4, 65_535)]));
    got.extend(read_data(&mut peer, 100_000 - 66_535));
    assert_eq!(got, body);
    peer.head(r.id, &[]);
    peer.send(&frame(DATA, END_STREAM, r.id, b""));
    assert_eq!(t.join().unwrap().1, Ok(200));
}

/// DATA bytes from the client, exactly `n` of them.
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

#[test]
fn a_lower_stream_limit_in_the_middle_holds_new_requests_until_streams_end() {
    need_openssl!();
    let (_pki, mut peer, client) = setup();
    let forks: Vec<Client> = (0..4).map(|_| client.fork()).collect();
    let mut forks = forks.into_iter();
    let mut ids = Vec::new();
    let mut threads = Vec::new();
    for i in 0..3 {
        threads.push(spawn(forks.next().unwrap(), get(url(&peer, &format!("/s{i}")))));
        if i == 0 {
            peer.open(&[(3, 10)]);
        }
        ids.push(peer.request().id);
    }
    // 3 streams are open. The limit drops to 1: the 4th request waits until the others end, not just until one ends.
    peer.send(&settings(&[(3, 1)]));
    peer.sync();
    let t4 = spawn(forks.next().unwrap(), get(url(&peer, "/s3")));
    none_of(&mut peer, HEADERS, 300);
    for (n, id) in ids.iter().enumerate() {
        peer.head(*id, &[("content-length", "1")]);
        peer.send(&frame(DATA, END_STREAM, *id, b"."));
        assert_eq!(threads.remove(0).join().unwrap().1, Ok((200, b".".to_vec())));
        if n < 2 {
            none_of(&mut peer, HEADERS, 200);
        }
    }
    let r4 = peer.request();
    assert_eq!(r4.get(":path"), "/s3");
    // A limit of 0 holds everything; a limit raised wakes the waiting request.
    peer.send(&settings(&[(3, 0)]));
    peer.sync();
    peer.head(r4.id, &[("content-length", "1")]);
    peer.send(&frame(DATA, END_STREAM, r4.id, b"."));
    let (client, got) = t4.join().unwrap();
    assert!(got.is_ok());
    let t5 = spawn(client, get(url(&peer, "/s4")));
    none_of(&mut peer, HEADERS, 300);
    peer.send(&settings(&[(3, 5)]));
    let r5 = peer.request();
    peer.head(r5.id, &[("content-length", "1")]);
    peer.send(&frame(DATA, END_STREAM, r5.id, b"."));
    assert!(t5.join().unwrap().1.is_ok());
}

#[test]
fn a_larger_frame_size_from_the_server_gives_larger_data_frames() {
    need_openssl!();
    let (_pki, mut peer, client) = setup();
    let t = spawn(client, post(url(&peer, "/frames"), vec![1; 200_000]));
    peer.open(&[(5, 100_000), (0x99, 7)]);
    let r = peer.request();
    peer.send(&frame(WINDOW_UPDATE, 0, 0, &1_000_000u32.to_be_bytes()));
    peer.send(&frame(WINDOW_UPDATE, 0, r.id, &1_000_000u32.to_be_bytes()));
    let mut sizes = Vec::new();
    let mut total = 0;
    while total < 200_000 {
        let (ty, _, _, p) = peer.read_frame();
        if ty == DATA {
            sizes.push(p.len());
            total += p.len();
        }
    }
    // The first frame is cut by the window (65,535); the next ones use the new size, up to 100,000.
    assert!(sizes.iter().all(|s| *s <= 100_000) && sizes.iter().any(|s| *s > 16384), "{sizes:?}");
    peer.head(r.id, &[]);
    peer.send(&frame(DATA, END_STREAM, r.id, b""));
    assert_eq!(t.join().unwrap().1, Ok(200));
}

#[test]
fn goaway_keeps_the_streams_up_to_its_last_id_and_moves_the_rest() {
    need_openssl!();
    let (_pki, mut peer, client) = setup();
    let forks: Vec<Client> = (0..2).map(|_| client.fork()).collect();
    let mut threads = vec![];
    let mut ids = vec![];
    for (i, c) in std::iter::once(client).chain(forks).enumerate() {
        threads.push(spawn(c, get(url(&peer, &format!("/g{i}")))));
        if i == 0 {
            peer.open(&[]);
        }
        ids.push(peer.request().id);
    }
    assert_eq!(ids, [1, 3, 5]);
    // Last stream 3, error code ENHANCE_YOUR_CALM and a debug text. A second GOAWAY with a higher id changes nothing.
    let mut goaway = vec![0, 0, 0, 3, 0, 0, 0, 11];
    goaway.extend(b"slow down");
    let mut wire = frame(GOAWAY, 0, 0, &goaway);
    wire.extend(frame(GOAWAY, 0, 0, &[0, 0, 0, 7, 0, 0, 0, 0]));
    peer.send(&wire);
    // Streams 1 and 3 finish on this connection.
    for id in [1, 3] {
        peer.head(id, &[("content-length", "1")]);
        peer.send(&frame(DATA, END_STREAM, id, b"."));
    }
    let t3 = threads.pop().unwrap();
    for t in threads {
        assert_eq!(t.join().unwrap().1, Ok((200, b".".to_vec())));
    }
    // Stream 5 runs again on a new connection (the old one closes when its streams are done).
    peer.open(&[]);
    let again = peer.request();
    assert_eq!((again.id, again.get(":path")), (1, "/g2"));
    peer.head(1, &[("content-length", "2")]);
    peer.send(&frame(DATA, END_STREAM, 1, b"ok"));
    assert_eq!(t3.join().unwrap().1, Ok((200, b"ok".to_vec())));
}

#[test]
fn push_promise_ends_the_session_and_the_next_request_opens_another() {
    need_openssl!();
    let (_pki, mut peer, client) = setup();
    let t = spawn(client, get(url(&peer, "/push")));
    peer.open(&[]);
    let r = peer.request();
    // A promise (stream 2, a header block) while push is off: a connection error, and the open request fails (it does not hang).
    peer.send(&frame(PUSH_PROMISE, END_HEADERS, r.id, &[&[0, 0, 0, 2][..], &block(&[(":method", "GET")])].concat()));
    assert_eq!(peer.goaway_code(), PROTOCOL_ERROR);
    let (client, got) = t.join().unwrap();
    assert!(got.is_err());
    let t = spawn(client, get(url(&peer, "/next")));
    peer.open(&[]);
    let r = peer.request();
    assert_eq!(r.id, 1);
    peer.head(1, &[("content-length", "2")]);
    peer.send(&frame(DATA, END_STREAM, 1, b"ok"));
    assert_eq!(t.join().unwrap().1, Ok((200, b"ok".to_vec())));
}

#[test]
fn a_stream_that_passes_its_window_is_reset_and_the_session_goes_on() {
    need_openssl!();
    let (_pki, mut peer, client) = setup();
    let u = url(&peer, "/flood");
    // The caller does not read at first, so the window is not given back.
    let t = spawn(client, move |c| {
        let mut s = c.get_stream(&u).map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(1500));
        std::io::Read::read_to_end(&mut s.body, &mut Vec::new()).map_err(|e| e.to_string())
    });
    peer.open(&[]);
    let r = peer.request();
    peer.head(r.id, &[]);
    // The stream window is 1 MiB: 64 frames of 16 KiB fill it. The 65th is a flow-control error of the stream.
    for _ in 0..65 {
        peer.send(&frame(DATA, 0, r.id, &[0u8; 16384]));
    }
    assert_eq!(peer.rst_code(r.id), FLOW_CONTROL_ERROR);
    let (client, got) = t.join().unwrap();
    assert!(got.is_err());
    let t = spawn(client, get(url(&peer, "/ok")));
    let r = peer.request();
    peer.head(r.id, &[("content-length", "2")]);
    peer.send(&frame(DATA, END_STREAM, r.id, b"ok"));
    assert_eq!(t.join().unwrap().1, Ok((200, b"ok".to_vec())));
}

/// A frame of 16,385 bytes: one over the limit.
fn too_big() -> Vec<u8> {
    frame(DATA, 0, 1, &vec![0u8; 16385])
}

#[test]
fn each_protocol_fault_ends_the_connection_with_its_code() {
    need_openssl!();
    let (_pki, mut peer, client) = setup();
    let inc = |n: u32| n.to_be_bytes().to_vec();
    let cases: Vec<(&str, Vec<u8>, u32)> = vec![
        ("DATA on stream 0", frame(DATA, 0, 0, b"x"), PROTOCOL_ERROR),
        ("DATA on an idle stream", frame(DATA, 0, 99, b"x"), PROTOCOL_ERROR),
        ("padding as long as the frame", frame(DATA, PADDED, 1, &[2, 0]), PROTOCOL_ERROR),
        ("a padded frame with no length", frame(DATA, PADDED, 1, &[]), FRAME_SIZE_ERROR),
        ("a frame over 16 KiB", too_big(), FRAME_SIZE_ERROR),
        ("HEADERS on an even stream", frame(HEADERS, END_HEADERS, 2, &block(&[(":status", "200")])), PROTOCOL_ERROR),
        ("HEADERS on an idle stream", frame(HEADERS, END_HEADERS, 99, &block(&[(":status", "200")])), PROTOCOL_ERROR),
        ("HEADERS too short for its priority", frame(HEADERS, END_HEADERS | PRIORITY_FLAG, 1, &[0, 0, 0]), FRAME_SIZE_ERROR),
        ("a bad HPACK index", frame(HEADERS, END_HEADERS, 1, &[0x80]), COMPRESSION_ERROR),
        ("CONTINUATION with no HEADERS", frame(CONTINUATION, END_HEADERS, 1, &[0x88]), PROTOCOL_ERROR),
        ("a frame inside a header block", [frame(HEADERS, 0, 1, &[0x88]), frame(PING, 0, 0, &[0; 8])].concat(), PROTOCOL_ERROR),
        ("CONTINUATION of another stream", [frame(HEADERS, 0, 1, &[0x88]), frame(CONTINUATION, END_HEADERS, 3, &[])].concat(), PROTOCOL_ERROR),
        ("PRIORITY on stream 0", frame(PRIORITY, 0, 0, &[0; 5]), PROTOCOL_ERROR),
        ("RST_STREAM on stream 0", frame(RST_STREAM, 0, 0, &[0; 4]), PROTOCOL_ERROR),
        ("RST_STREAM on an idle stream", frame(RST_STREAM, 0, 99, &[0; 4]), PROTOCOL_ERROR),
        ("RST_STREAM of 3 bytes", frame(RST_STREAM, 0, 1, &[0; 3]), FRAME_SIZE_ERROR),
        ("SETTINGS on a stream", frame(SETTINGS, 0, 1, &[]), PROTOCOL_ERROR),
        ("SETTINGS of 5 bytes", frame(SETTINGS, 0, 0, &[0; 5]), FRAME_SIZE_ERROR),
        ("SETTINGS ACK with a payload", frame(SETTINGS, ACK, 0, &[0; 6]), FRAME_SIZE_ERROR),
        ("ENABLE_PUSH 2", settings(&[(2, 2)]), PROTOCOL_ERROR),
        ("INITIAL_WINDOW_SIZE 2^31", settings(&[(4, 1 << 31)]), FLOW_CONTROL_ERROR),
        ("MAX_FRAME_SIZE 100", settings(&[(5, 100)]), PROTOCOL_ERROR),
        ("MAX_FRAME_SIZE 2^24", settings(&[(5, 1 << 24)]), PROTOCOL_ERROR),
        ("PING on a stream", frame(PING, 0, 1, &[0; 8]), PROTOCOL_ERROR),
        ("PING of 7 bytes", frame(PING, 0, 0, &[0; 7]), FRAME_SIZE_ERROR),
        ("GOAWAY on a stream", frame(GOAWAY, 0, 1, &[0; 8]), PROTOCOL_ERROR),
        ("GOAWAY of 4 bytes", frame(GOAWAY, 0, 0, &[0; 4]), FRAME_SIZE_ERROR),
        ("WINDOW_UPDATE of 0 on the connection", frame(WINDOW_UPDATE, 0, 0, &inc(0)), PROTOCOL_ERROR),
        ("WINDOW_UPDATE of 3 bytes", frame(WINDOW_UPDATE, 0, 1, &[0; 3]), FRAME_SIZE_ERROR),
        ("WINDOW_UPDATE on an idle stream", frame(WINDOW_UPDATE, 0, 99, &inc(1)), PROTOCOL_ERROR),
        ("WINDOW_UPDATE past 2^31 on the connection", frame(WINDOW_UPDATE, 0, 0, &inc(0x7fff_ffff)), FLOW_CONTROL_ERROR),
        ("a stream window past 2^31 by SETTINGS", [frame(WINDOW_UPDATE, 0, 1, &inc(0x7fff_0000)), settings(&[(4, 0x7fff_ffff)])].concat(), FLOW_CONTROL_ERROR),
        ("PUSH_PROMISE", frame(PUSH_PROMISE, END_HEADERS, 1, &[0, 0, 0, 2, 0x88]), PROTOCOL_ERROR),
    ];
    let mut client = client;
    for (i, (name, wire, code)) in cases.into_iter().enumerate() {
        // A body for the request, so that stream 1 has a window to move in the WINDOW_UPDATE cases.
        let t = spawn(client, post(url(&peer, &format!("/fault{i}")), vec![0; 100]));
        peer.open(&[]);
        let r = peer.request();
        assert_eq!(r.id, 1, "{name}");
        peer.send(&wire);
        assert_eq!(peer.goaway_code(), code, "{name}");
        let (c, got) = t.join().unwrap();
        assert!(got.is_err(), "{name}");
        client = c;
    }
}

#[test]
fn a_server_that_closes_after_a_complete_reply_and_a_head_request_work_over_one_session() {
    need_openssl!();
    let (_pki, mut peer, client) = setup();
    let c2 = client.fork();
    let u = url(&peer, "/h");
    let t = spawn(client, move |c| c.send(&Request { cookies: Default::default(), method: Method::Head, url: &u, headers: &[], body: &[] }).map(|r| (r.status, r.body.len(), r.header("content-length").map(str::to_string))).map_err(|e| e.to_string()));
    peer.open(&[]);
    let r = peer.request();
    assert!(r.end_stream);
    // A HEAD reply with a content-encoding and no body: nothing to decode.
    peer.send(&frame(HEADERS, END_HEADERS | END_STREAM, r.id, &block(&[(":status", "200"), ("content-encoding", "gzip"), ("content-length", "99")])));
    assert_eq!(t.join().unwrap().1, Ok((200, 0, Some("99".to_string()))));
    let t = spawn(c2, get(url(&peer, "/204")));
    let r = peer.request();
    peer.send(&frame(HEADERS, END_HEADERS | END_STREAM, r.id, &block(&[(":status", "204"), ("content-encoding", "gzip")])));
    assert_eq!(t.join().unwrap().1, Ok((204, vec![])));
}
