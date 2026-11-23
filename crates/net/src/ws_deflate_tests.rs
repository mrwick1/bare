//! Tests of `permessage-deflate` (RFC 7692) in the WebSocket client (`ws.rs`): the offer, the settings of the server, and compressed
//! messages of the server. The client sends its own messages without compression.

use crate::http::{Client, proxy_base64};
use crate::ws::{self, Message, Stop, TEXT};
use bare_url::Url;
use flate2::{Compress, Compression, FlushCompress};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::JoinHandle;

/// A server for one connection: it answers the handshake with `ext` as `Sec-WebSocket-Extensions` (none when empty), then runs
/// `then`. Gives the head of the request.
fn server(ext: &'static str, then: impl FnOnce(TcpStream) + Send + 'static) -> (u16, JoinHandle<String>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut b).unwrap();
            head.push(b[0]);
        }
        let head = String::from_utf8(head).unwrap();
        let key = head.lines().find_map(|l| l.strip_prefix("Sec-WebSocket-Key: ")).unwrap();
        let accept = proxy_base64(&bare_tls::sha1::sha1(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes()));
        let ext = if ext.is_empty() { String::new() } else { format!("Sec-WebSocket-Extensions: {ext}\r\n") };
        write!(s, "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n{ext}\r\n").unwrap();
        then(s);
        head
    });
    (port, h)
}

/// A frame of the server, not masked. `rsv1` marks a compressed message.
fn frame(fin: bool, rsv1: bool, op: u8, data: &[u8]) -> Vec<u8> {
    let mut out = vec![(u8::from(fin) << 7) | (u8::from(rsv1) << 6) | op];
    match data.len() {
        n @ 0..=125 => out.push(n as u8),
        n @ 126..=0xffff => {
            out.push(126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(data);
    out
}

/// Compress one message as RFC 7692 says: raw deflate, a sync flush, without its last 4 bytes (`00 00 ff ff`).
fn deflate(c: &mut Compress, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 64);
    c.compress_vec(data, &mut out, FlushCompress::Sync).unwrap();
    assert!(out.len() < out.capacity(), "room left: the flush is complete");
    assert!(out.ends_with(&[0, 0, 0xff, 0xff]));
    out.truncate(out.len() - 4);
    out
}

/// Read a frame of the client: gives its first byte and the unmasked data.
fn read_frame(s: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut h = [0u8; 2];
    s.read_exact(&mut h).unwrap();
    let len = match h[1] & 0x7f {
        126 => {
            let mut b = [0u8; 2];
            s.read_exact(&mut b).unwrap();
            u16::from_be_bytes(b) as usize
        }
        n => n as usize,
    };
    let mut key = [0u8; 4];
    s.read_exact(&mut key).unwrap();
    let mut data = vec![0u8; len];
    s.read_exact(&mut data).unwrap();
    for (i, b) in data.iter_mut().enumerate() {
        *b ^= key[i % 4];
    }
    (h[0], data)
}

fn open(port: u16, max: usize) -> Result<(ws::Reader, ws::Writer, String, String), String> {
    let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
    ws::connect(&Client::new(), &url, &[], None, max).map_err(|e| e.to_string())
}

#[test]
fn compressed_messages_with_a_shared_window_and_a_plain_one() {
    let (port, h) = server("permessage-deflate", |mut s| {
        let mut c = Compress::new(Compression::default(), false);
        let first = "hello hello hello websocket compression".repeat(20);
        s.write_all(&frame(true, true, TEXT, &deflate(&mut c, first.as_bytes()))).unwrap();
        // The second message points back into the first: the client keeps its window.
        let second = deflate(&mut c, first.as_bytes());
        s.write_all(&frame(false, true, TEXT, &second[..3])).unwrap();
        s.write_all(&frame(true, false, 0, &second[3..])).unwrap();
        s.write_all(&frame(true, false, 2, &[1, 2, 3])).unwrap();
        // The client sends without compression.
        assert_eq!(read_frame(&mut s), (0x81, b"plain".to_vec()));
    });
    let (mut r, mut w, _, ext) = open(port, 1 << 20).unwrap();
    assert_eq!(ext, "permessage-deflate");
    let first = "hello hello hello websocket compression".repeat(20);
    assert_eq!(r.recv().unwrap(), Message::Text(first.clone()));
    assert_eq!(r.recv().unwrap(), Message::Text(first));
    assert_eq!(r.recv().unwrap(), Message::Binary(vec![1, 2, 3]));
    w.send(TEXT, b"plain").unwrap();
    let head = h.join().unwrap();
    assert!(head.contains("Sec-WebSocket-Extensions: permessage-deflate; client_max_window_bits\r\n"), "{head}");
}

#[test]
fn no_context_takeover_of_the_server_resets_the_window() {
    let (port, h) = server("permessage-deflate; server_no_context_takeover; server_max_window_bits=10; client_max_window_bits=12", |mut s| {
        for _ in 0..2 {
            let mut c = Compress::new(Compression::default(), false);
            s.write_all(&frame(true, true, TEXT, &deflate(&mut c, b"abcabcabcabc"))).unwrap();
        }
    });
    let (mut r, _w, _, ext) = open(port, 1 << 20).unwrap();
    assert_eq!(ext, "permessage-deflate; server_no_context_takeover; server_max_window_bits=10; client_max_window_bits=12");
    assert_eq!(r.recv().unwrap(), Message::Text("abcabcabcabc".into()));
    assert_eq!(r.recv().unwrap(), Message::Text("abcabcabcabc".into()));
    h.join().unwrap();
}

/// The server sends `bad`; the reader stops with `code` and sends a close with it.
fn fails(ext: &'static str, bad: Vec<u8>, max: usize, code: u16) {
    let (port, h) = server(ext, move |mut s| {
        s.write_all(&bad).unwrap();
        let (b0, data) = read_frame(&mut s);
        assert_eq!((b0, data), (0x88, code.to_be_bytes().to_vec()));
    });
    let (mut r, _w, _, _) = open(port, max).unwrap();
    match r.recv() {
        Err(Stop::Protocol(c, _)) => assert_eq!(c, code),
        other => panic!("{other:?}"),
    }
    h.join().unwrap();
}

#[test]
fn bad_compressed_frames_fail() {
    let mut c = Compress::new(Compression::default(), false);
    let bomb = deflate(&mut c, &[b'a'; 5000]);
    // A message that inflates past the cap.
    fails("permessage-deflate", frame(true, true, TEXT, &bomb), 1000, 1009);
    // RSV1 without the extension, on a control frame, and on a continuation frame.
    fails("", frame(true, true, TEXT, b"x"), 1 << 20, 1002);
    fails("permessage-deflate", frame(true, true, 9, b"x"), 1 << 20, 1002);
    let mut cont = frame(false, false, TEXT, b"x");
    cont.extend(frame(true, true, 0, b"y"));
    fails("permessage-deflate", cont, 1 << 20, 1002);
    // Data that does not inflate.
    fails("permessage-deflate", frame(true, true, TEXT, &[0xff, 0xff, 0xff]), 1 << 20, 1002);
}

#[test]
fn an_extension_or_a_setting_that_was_not_offered_fails() {
    for ext in ["x-webkit-deflate-frame", "permessage-deflate; foo", "permessage-deflate; server_max_window_bits=7", "permessage-deflate, permessage-deflate", "permessage-deflate; server_no_context_takeover=1"] {
        let (port, _h) = server(ext, |_| {});
        let e = open(port, 1 << 20).err().unwrap_or_else(|| panic!("{ext} was taken"));
        assert!(e.contains("Sec-WebSocket-Extensions"), "{ext}: {e}");
    }
}
