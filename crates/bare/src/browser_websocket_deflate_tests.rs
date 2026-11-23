//! `permessage-deflate` as the page sees it (`shims/websocket.js`, `bare_net::ws`): `extensions` names what the server took, and a
//! compressed message arrives inflated.

use crate::browser_xhr_tests::{browser, js, settle};
use std::io::{Read, Write};
use std::net::TcpListener;

fn b64(input: &[u8]) -> String {
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

#[test]
fn a_compressed_message_arrives_inflated_and_extensions_names_the_deflate() {
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
        let accept = b64(&bare_tls::sha1::sha1(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes()));
        write!(s, "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\nSec-WebSocket-Extensions: permessage-deflate; server_no_context_takeover\r\n\r\n").unwrap();
        // "Hello", compressed: the example of RFC 7692, section 7.2.3.1.
        s.write_all(&[0xc1, 0x07, 0xf2, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00]).unwrap();
        let _ = s.read(&mut [0u8; 64]);
    });
    let mut b = browser(port);
    b.set_document_url(&format!("http://127.0.0.1:{port}/page.html")).unwrap();
    js(&mut b, "var out = [], finished = false; var ws = new WebSocket('/');
        ws.onopen = function () { out.push(ws.extensions); };
        ws.onmessage = function (e) { out.push(e.data); ws.close(); finished = true; };");
    settle(&mut b);
    assert_eq!(js(&mut b, "out.join('|')"), "permessage-deflate; server_no_context_takeover|Hello");
    drop(b);
    h.join().unwrap();
}
