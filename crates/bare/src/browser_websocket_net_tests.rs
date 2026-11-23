//! Tests of `WebSocket` against a real server on the loopback (`browser_websocket.rs`, `shims/websocket.js`).

use crate::browser_xhr_tests::{browser, js, settle};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::JoinHandle;

/// A server for one connection: it answers the handshake (with the first offered subprotocol), then runs `then`. Gives the head of
/// the request.
fn server(then: impl FnOnce(&mut TcpStream) + Send + 'static) -> (u16, JoinHandle<String>) {
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
        let accept = bare_net_b64(&bare_tls::sha1::sha1(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes()));
        let proto = head.lines().find_map(|l| l.strip_prefix("Sec-WebSocket-Protocol: ")).map(|p| format!("Sec-WebSocket-Protocol: {}\r\n", p.split(',').next().unwrap())).unwrap_or_default();
        write!(s, "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n{proto}\r\n").unwrap();
        then(&mut s);
        head
    });
    (port, h)
}

fn bare_net_b64(input: &[u8]) -> String {
    const D: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in input.chunks(3) {
        let n = ((c[0] as u32) << 16) | ((*c.get(1).unwrap_or(&0) as u32) << 8) | *c.get(2).unwrap_or(&0) as u32;
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            out.push(if i <= c.len() { D[((n >> shift) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// A frame of the server (short, not masked).
fn frame(op: u8, data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x80 | op, data.len() as u8];
    out.extend_from_slice(data);
    out
}

/// A frame of the client: unmask it.
fn read_frame(s: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut h = [0u8; 2];
    s.read_exact(&mut h).unwrap();
    assert_eq!(h[1] & 0x80, 0x80, "masked");
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
    (h[0] & 0x0f, data)
}

/// Echo each message; answer a close with the same body.
fn echo(s: &mut TcpStream) {
    loop {
        let (op, data) = read_frame(s);
        s.write_all(&frame(op, &data)).unwrap();
        if op == 8 {
            return;
        }
    }
}

fn run(port: u16, src: &str) -> String {
    let mut b = browser(port);
    b.set_document_url(&format!("http://127.0.0.1:{port}/page.html")).unwrap();
    let r = js(&mut b, &format!("var out = [], finished = false; {src}"));
    assert!(!["throw", "other", "unsupp", "limit"].iter().any(|p| r.starts_with(p)), "{r}");
    settle(&mut b);
    js(&mut b, "out.join('|')").replace(&port.to_string(), "P")
}

#[test]
fn echo_of_text_bytes_and_a_blob_in_order_then_a_clean_close() {
    let (port, h) = server(echo);
    let got = run(port, "var ws = new WebSocket('/echo?x=1', ['chat', 'v2']), n = 0; ws.binaryType = 'arraybuffer';
        out.push(ws.readyState);
        ws.onopen = function () {
            out.push('open', ws.readyState, ws.protocol);
            ws.send('hi'); ws.send(new Uint8Array([9, 1, 2, 3]).subarray(1)); ws.send(new Blob(['blob'])); ws.send('after');
        };
        ws.onmessage = function (e) {
            out.push(e.origin, typeof e.data === 'string' ? e.data : Array.from(new Uint8Array(e.data)).join('.'));
            if (++n === 4) { ws.close(1000, 'bye'); out.push(ws.readyState); }
        };
        ws.onclose = function (e) { out.push('close', e.code, e.reason, e.wasClean, ws.readyState, ws.bufferedAmount); finished = true; };");
    assert_eq!(got, "0|open|1|chat|ws://127.0.0.1:P|hi|ws://127.0.0.1:P|1.2.3|ws://127.0.0.1:P|98.108.111.98|ws://127.0.0.1:P|after|2|close|1000|bye|true|3|0");
    let head = h.join().unwrap();
    for line in ["GET /echo?x=1 HTTP/1.1", &format!("Origin: http://127.0.0.1:{port}"), "Sec-WebSocket-Protocol: chat, v2"] {
        assert!(head.contains(line), "{line} in {head}");
    }
}

#[test]
fn a_message_of_the_server_is_a_blob_by_default_and_its_close_ends_the_socket() {
    let (port, h) = server(|s| {
        s.write_all(&frame(2, b"abc")).unwrap();
        let mut body = 4001u16.to_be_bytes().to_vec();
        body.extend_from_slice(b"gone");
        s.write_all(&frame(8, &body)).unwrap();
        assert_eq!(read_frame(s), (8, 4001u16.to_be_bytes().to_vec()));
    });
    let got = run(port, "var ws = new WebSocket('/');
        ws.onmessage = function (e) { out.push(e.data instanceof Blob, e.data.size); };
        ws.onerror = function () { out.push('error'); };
        ws.onclose = function (e) { out.push('close', e.code, e.reason, e.wasClean); finished = true; };");
    assert_eq!(got, "true|3|close|4001|gone|true");
    h.join().unwrap();
}

#[test]
fn a_protocol_error_of_the_server_fires_error_then_close_1006() {
    let (port, h) = server(|s| {
        s.write_all(&frame(1, &[0xff])).unwrap();
        assert_eq!(read_frame(s), (8, 1007u16.to_be_bytes().to_vec()));
    });
    let got = run(port, "var ws = new WebSocket('/');
        ws.onmessage = function () { out.push('message'); };
        ws.onerror = function () { out.push('error', ws.readyState); };
        ws.onclose = function (e) { out.push('close', e.code, e.wasClean); finished = true; };");
    assert_eq!(got, "error|3|close|1006|false");
    h.join().unwrap();
}

#[test]
fn a_close_before_the_open_fails_the_socket() {
    let got = run(54321, "var ws = new WebSocket('ws://127.0.0.1:54321/'); ws.close(); out.push(ws.readyState);
        ws.onerror = function () { out.push('error'); };
        ws.onclose = function (e) { out.push('close', e.code, e.wasClean); finished = true; };");
    assert_eq!(got, "2|error|close|1006|false");
}

#[test]
fn bad_subprotocols_and_mixed_content_throw() {
    let got = run(54321, "[['a b'], ['a', 'a'], 'x,y'].forEach(function (p) { try { new WebSocket('ws://127.0.0.1:54321/', p); } catch (e) { out.push(e.name); } });
        try { new WebSocket('/', []).close(4000, 'x'.repeat(124)); } catch (e) { out.push(e.name); }
        finished = true");
    assert_eq!(got, "SyntaxError|SyntaxError|SyntaxError|SyntaxError");
}

#[test]
fn a_page_that_goes_away_closes_its_socket_with_1001() {
    let (port, h) = server(|s| {
        s.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        assert_eq!(read_frame(s), (8, 1001u16.to_be_bytes().to_vec()));
    });
    let mut b = browser(port);
    b.set_document_url(&format!("http://127.0.0.1:{port}/page.html")).unwrap();
    js(&mut b, "var finished = false, ws = new WebSocket('/'); ws.onopen = function () { finished = true; };");
    settle(&mut b);
    assert_eq!(js(&mut b, "ws.readyState"), "1");
    drop(b);
    h.join().unwrap();
}
