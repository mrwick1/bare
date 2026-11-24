//! HelloRetryRequest (RFC 8446 §4.1.4) against a loopback `openssl s_server`.
//! The server accepts only secp256r1. Our first key share is X25519, so the server asks again.
//! Microsoft servers (teams.microsoft.com, login.microsoftonline.com) do the same.
//! These tests skip (and say so) when `openssl` is not installed.
#![cfg(target_os = "linux")]

#[path = "../../../tools/crypto/openssl_harness.rs"]
mod harness;

use bare_tls::client;
use bare_tls::trust_store;
use harness::{Pki, Server, openssl_available};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

#[test]
fn a_server_that_wants_p256_gets_a_second_hello_and_the_page_reads() {
    if !openssl_available() {
        eprintln!("skipped: openssl is not installed");
        return;
    }
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    std::fs::write(pki.dir.join("hello.txt"), b"hello after a retry").unwrap();
    let srv = Server::start(&pki, "good", &["-WWW", "-groups", "P-256"], false);
    let t = TcpStream::connect(("127.0.0.1", srv.port)).unwrap();
    t.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let roots = trust_store::decode_pem(&pki.root_pem()).unwrap();
    let mut c = client::connect(t, "localhost", &roots).unwrap();
    c.write_all(b"GET /hello.txt HTTP/1.0\r\n\r\n").unwrap();
    let mut body = Vec::new();
    c.read_to_end(&mut body).unwrap();
    assert!(body.ends_with(b"hello after a retry"), "{:?}", String::from_utf8_lossy(&body));
}
