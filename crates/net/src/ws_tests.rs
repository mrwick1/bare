//! Tests of the WebSocket client (`ws.rs`) against a server on the loopback.

use crate::http::{Client, proxy_base64};
use crate::ws::{self, BINARY, Message, Stop, TEXT};
use bare_url::Url;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::JoinHandle;

/// A server for one connection. It reads the handshake, gives it to `reply` for the head of the answer, then runs `then`.
fn server(reply: fn(&str) -> String, then: impl FnOnce(TcpStream) + Send + 'static) -> (u16, JoinHandle<String>) {
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
        s.write_all(reply(&head).as_bytes()).unwrap();
        then(s);
        head
    });
    (port, h)
}

fn accept_of(head: &str) -> String {
    let key = head.lines().find_map(|l| l.strip_prefix("Sec-WebSocket-Key: ")).unwrap();
    proxy_base64(&bare_tls::sha1::sha1(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes()))
}

fn ok(head: &str) -> String {
    format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n", accept_of(head))
}

/// A frame of the server: not masked.
fn frame(fin: bool, op: u8, data: &[u8]) -> Vec<u8> {
    let mut out = vec![(u8::from(fin) << 7) | op];
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

/// Read a frame of the client. It must be final and masked. Gives the opcode and the unmasked data.
fn read_frame(s: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut h = [0u8; 2];
    s.read_exact(&mut h).unwrap();
    assert_eq!(h[0] & 0xf0, 0x80, "final, no RSV");
    assert_eq!(h[1] & 0x80, 0x80, "masked");
    let len = match h[1] & 0x7f {
        126 => {
            let mut b = [0u8; 2];
            s.read_exact(&mut b).unwrap();
            u16::from_be_bytes(b) as usize
        }
        127 => {
            let mut b = [0u8; 8];
            s.read_exact(&mut b).unwrap();
            u64::from_be_bytes(b) as usize
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
    (h[0] & 0x0f, data)
}

fn open(port: u16, protocols: &[&str], max: usize) -> Result<(ws::Reader, ws::Writer, String), String> {
    let url = Url::parse(&format!("http://127.0.0.1:{port}/chat?room=1")).unwrap();
    let protocols: Vec<String> = protocols.iter().map(|p| p.to_string()).collect();
    ws::connect(&Client::new(), &url, &protocols, Some("http://127.0.0.1:9"), max).map(|(r, w, p, _)| (r, w, p)).map_err(|e| e.to_string())
}

#[test]
fn echo_of_text_and_of_binary_in_all_three_length_forms() {
    let (port, h) = server(ok, |mut s| {
        for _ in 0..4 {
            let (op, data) = read_frame(&mut s);
            s.write_all(&frame(true, op, &data)).unwrap();
        }
    });
    let (mut r, mut w, protocol) = open(port, &[], 1 << 20).unwrap();
    assert_eq!(protocol, "");
    w.send(TEXT, "héllo".as_bytes()).unwrap();
    assert_eq!(r.recv().unwrap(), Message::Text("héllo".into()));
    for n in [125, 300, 70_000] {
        let data: Vec<u8> = (0..n).map(|i| i as u8).collect();
        w.send(BINARY, &data).unwrap();
        assert_eq!(r.recv().unwrap(), Message::Binary(data));
    }
    let head = h.join().unwrap();
    for line in ["GET /chat?room=1 HTTP/1.1", "Upgrade: websocket", "Connection: Upgrade", "Sec-WebSocket-Version: 13", "Origin: http://127.0.0.1:9"] {
        assert!(head.contains(line), "{line} in {head}");
    }
    assert!(!head.contains("Sec-WebSocket-Protocol"), "{head}");
}

#[test]
fn ping_fragments_and_a_close_of_the_server() {
    let (port, h) = server(ok, |mut s| {
        s.write_all(&frame(true, 9, b"p1")).unwrap();
        s.write_all(&frame(false, 1, b"ab")).unwrap();
        s.write_all(&frame(true, 9, b"p2")).unwrap();
        s.write_all(&frame(false, 0, b"c")).unwrap();
        s.write_all(&frame(true, 0, b"d")).unwrap();
        assert_eq!(read_frame(&mut s), (10, b"p1".to_vec()));
        assert_eq!(read_frame(&mut s), (10, b"p2".to_vec()));
        let mut body = 4000u16.to_be_bytes().to_vec();
        body.extend_from_slice(b"bye");
        s.write_all(&frame(true, 8, &body)).unwrap();
        assert_eq!(read_frame(&mut s), (8, 4000u16.to_be_bytes().to_vec()));
    });
    let (mut r, _w, _) = open(port, &[], 1 << 20).unwrap();
    assert_eq!(r.recv().unwrap(), Message::Text("abcd".into()));
    assert_eq!(r.recv().unwrap(), Message::Close(4000, "bye".into()));
    h.join().unwrap();
}

#[test]
fn a_close_of_the_client_waits_for_the_answer() {
    let (port, h) = server(ok, |mut s| {
        let (op, data) = read_frame(&mut s);
        assert_eq!((op, &data[..2]), (8, &1000u16.to_be_bytes()[..]));
        assert_eq!(&data[2..], b"done");
        s.write_all(&frame(true, 1, b"late")).unwrap();
        s.write_all(&frame(true, 8, &data[..2])).unwrap();
    });
    let (mut r, mut w, _) = open(port, &[], 1 << 20).unwrap();
    w.close(Some(1000), "done").unwrap();
    assert_eq!(r.recv().unwrap(), Message::Text("late".into()));
    assert_eq!(r.recv().unwrap(), Message::Close(1000, String::new()));
    h.join().unwrap();
}

#[test]
fn the_server_picks_an_offered_subprotocol() {
    let (port, h) = server(|head| ok(head).replace("\r\n\r\n", "\r\nSec-WebSocket-Protocol: b\r\n\r\n"), |_| {});
    assert_eq!(open(port, &["a", "b"], 1 << 20).unwrap().2, "b");
    assert!(h.join().unwrap().contains("Sec-WebSocket-Protocol: a, b\r\n"));
    let (port, _h) = server(|head| ok(head).replace("\r\n\r\n", "\r\nSec-WebSocket-Protocol: c\r\n\r\n"), |_| {});
    assert!(open(port, &["a"], 1 << 20).err().unwrap().contains("not one that was offered"));
}

#[test]
fn a_bad_handshake_fails() {
    let (port, _h) = server(|_| "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".into(), |_| {});
    assert!(open(port, &[], 1 << 20).err().unwrap().contains("Unexpected response code: 200"));
    let (port, _h) = server(|head| ok(head).replace(&accept_of(head), "AAAA"), |_| {});
    assert!(open(port, &[], 1 << 20).err().unwrap().contains("Sec-WebSocket-Accept"));
    let (port, _h) = server(|head| ok(head).replace("\r\n\r\n", "\r\nSec-WebSocket-Extensions: x-webkit-deflate-frame\r\n\r\n"), |_| {});
    assert!(open(port, &[], 1 << 20).err().unwrap().contains("extension"));
}

/// A bad frame of the server: the reader stops with `code` and sends a close with it.
fn protocol_error(bad: Vec<u8>, max: usize, code: u16) {
    let (port, h) = server(ok, move |mut s| {
        s.write_all(&bad).unwrap();
        let (op, data) = read_frame(&mut s);
        assert_eq!((op, data), (8, code.to_be_bytes().to_vec()));
    });
    let (mut r, _w, _) = open(port, &[], max).unwrap();
    match r.recv() {
        Err(Stop::Protocol(c, _)) => assert_eq!(c, code),
        other => panic!("{other:?}"),
    }
    h.join().unwrap();
}

#[test]
fn bad_frames_of_the_server_fail_with_their_code() {
    let mut masked = frame(true, 1, b"x");
    masked[1] |= 0x80;
    protocol_error(masked, 1 << 20, 1002);
    protocol_error(frame(true, 0x41, b"x"), 1 << 20, 1002);
    protocol_error(frame(true, 3, b"x"), 1 << 20, 1002);
    protocol_error(frame(false, 9, b"x"), 1 << 20, 1002);
    protocol_error(frame(true, 0, b"x"), 1 << 20, 1002);
    protocol_error(frame(true, 1, &[0xff, 0xfe]), 1 << 20, 1007);
    protocol_error(frame(true, 2, &[0; 200]), 100, 1009);
    protocol_error(frame(true, 8, &999u16.to_be_bytes()), 1 << 20, 1002);
}

#[test]
fn an_end_without_a_close_is_an_io_stop() {
    let (port, h) = server(ok, drop);
    let (mut r, _w, _) = open(port, &[], 1 << 20).unwrap();
    h.join().unwrap();
    assert!(matches!(r.recv(), Err(Stop::Io(_))));
}
