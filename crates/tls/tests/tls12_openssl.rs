//! The TLS client against a loopback `openssl s_server -tls1_2`, with the real path validator.
//! Same harness as client_openssl.rs: a synthetic root in RAM, no network beyond 127.0.0.1.
//! These tests skip (and say so) when `openssl` is not installed.

#[path = "../../../tools/crypto/openssl_harness.rs"]
mod harness;

use bare_tls::client::{self, ClientStream, Error};
use bare_tls::record;
use bare_tls::trust_store::{self, TrustStore};
use harness::{Pki, Server, openssl_available};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

macro_rules! need_openssl {
    () => {
        if !openssl_available() {
            eprintln!("skipped: openssl is not installed");
            return;
        }
    };
}

fn dial(port: u16) -> TcpStream {
    let t = TcpStream::connect(("127.0.0.1", port)).unwrap();
    t.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    t.set_write_timeout(Some(Duration::from_secs(20))).unwrap();
    t
}

fn roots(pki: &Pki) -> TrustStore {
    trust_store::decode_pem(&pki.root_pem()).unwrap()
}

/// `-WWW` serves the files of the PKI directory. `-tls1_2` after the harness's `-tls1_3` wins.
fn www12(pki: &Pki, leaf: &str, extra: &[&str]) -> Server {
    std::fs::write(pki.dir.join("hello.txt"), b"hello over tls 1.2").unwrap();
    let mut args = vec!["-WWW", "-tls1_2"];
    args.extend_from_slice(extra);
    Server::start(pki, leaf, &args, false)
}

fn get<T: Read + Write>(c: &mut ClientStream<T>) -> Vec<u8> {
    c.write_all(b"GET /hello.txt HTTP/1.0\r\n\r\n").unwrap();
    let mut body = Vec::new();
    c.read_to_end(&mut body).unwrap();
    body
}

fn connects(srv: &Server, pki: &Pki) {
    let mut c = client::connect(dial(srv.port), "localhost", &roots(pki)).unwrap();
    let body = get(&mut c);
    assert!(body.ends_with(b"hello over tls 1.2"), "{:?}", String::from_utf8_lossy(&body));
}

#[test]
fn ecdhe_ecdsa_aes128_gcm_with_a_p256_leaf() {
    need_openssl!();
    let pki = Pki::new();
    pki.leaf("ec", "localhost", None);
    let srv = www12(&pki, "ec", &["-cipher", "ECDHE-ECDSA-AES128-GCM-SHA256", "-sigalgs", "ECDSA+SHA256"]);
    connects(&srv, &pki);
}

#[test]
fn ecdhe_key_exchange_with_x25519_and_with_p256() {
    need_openssl!();
    let pki = Pki::new();
    pki.leaf("ec", "localhost", None);
    pki.rsa_leaf("rsa", "localhost", 2048, "sha256");
    for (leaf, cipher, sigalg) in [("ec", "ECDHE-ECDSA-AES128-GCM-SHA256", "ECDSA+SHA256"), ("rsa", "ECDHE-RSA-AES128-GCM-SHA256", "RSA-PSS+SHA256")] {
        for group in ["X25519", "P-256"] {
            // An ECDSA P-256 certificate needs P-256 in the group list too.
            let groups = if leaf == "ec" { format!("{group}:P-256") } else { group.to_string() };
            let srv = www12(&pki, leaf, &["-cipher", cipher, "-sigalgs", sigalg, "-groups", &groups]);
            connects(&srv, &pki);
        }
    }
}

#[test]
fn ecdhe_rsa_aes128_gcm_with_every_signature_scheme() {
    need_openssl!();
    let pki = Pki::new();
    pki.rsa_leaf("rsa", "localhost", 2048, "sha256");
    for sigalg in ["RSA-PSS+SHA256", "RSA-PSS+SHA384", "RSA+SHA256", "RSA+SHA384"] {
        let srv = www12(&pki, "rsa", &["-cipher", "ECDHE-RSA-AES128-GCM-SHA256", "-sigalgs", sigalg]);
        connects(&srv, &pki);
    }
}

#[test]
fn alpn_http11_and_an_alpn_the_client_did_not_offer() {
    need_openssl!();
    let pki = Pki::new();
    pki.leaf("ec", "localhost", None);
    let srv = www12(&pki, "ec", &["-cipher", "ECDHE-ECDSA-AES128-GCM-SHA256", "-sigalgs", "ECDSA+SHA256", "-alpn", "http/1.1"]);
    let c = client::connect(dial(srv.port), "localhost", &roots(&pki)).unwrap();
    assert!(c.alpn_http11() && !c.alpn_h2());
    // `connect` offers http/1.1 only. A server with only h2 sends no_application_protocol.
    let srv = www12(&pki, "ec", &["-cipher", "ECDHE-ECDSA-AES128-GCM-SHA256", "-sigalgs", "ECDSA+SHA256", "-alpn", "h2"]);
    assert!(client::connect(dial(srv.port), "localhost", &roots(&pki)).is_err());
}

#[test]
fn a_server_that_offers_both_versions_gets_tls13() {
    need_openssl!();
    let pki = Pki::new();
    pki.leaf("ec", "localhost", None);
    // Both versions on the server: it must pick 1.3, and `-www` reports the protocol of the session.
    let srv = Server::start(&pki, "ec", &["-www", "-min_protocol", "TLSv1.2", "-max_protocol", "TLSv1.3"], false);
    let mut c = client::connect(dial(srv.port), "localhost", &roots(&pki)).unwrap();
    c.write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
    let mut page = Vec::new();
    c.read_to_end(&mut page).unwrap();
    let page = String::from_utf8_lossy(&page);
    assert!(page.contains("TLSv1.3"), "{page}");
}

#[test]
fn certificate_checks_are_the_same_as_for_tls13() {
    need_openssl!();
    let (pki, other) = (Pki::new(), Pki::new());
    pki.leaf("ec", "localhost", None);
    pki.leaf("old", "localhost", Some(("20200101000000Z", "20200102000000Z")));
    let args = ["-cipher", "ECDHE-ECDSA-AES128-GCM-SHA256", "-sigalgs", "ECDSA+SHA256"];
    let srv = www12(&pki, "ec", &args);
    for (host, store, want) in [("wrong.localhost", &pki, "Host"), ("localhost", &other, "NoIssuer")] {
        match client::connect(dial(srv.port), host, &roots(store)) {
            Err(Error::Certificate(m)) => assert!(m.contains(want), "{host}: {m}"),
            Err(e) => panic!("{e}"),
            Ok(_) => panic!("connected"),
        }
    }
    let srv = www12(&pki, "old", &args);
    match client::connect(dial(srv.port), "localhost", &roots(&pki)) {
        Err(Error::Certificate(m)) => assert!(m.contains("Expired"), "{m}"),
        r => panic!("{:?}", r.err()),
    }
}

#[test]
fn what_the_client_does_not_support_fails_with_the_server_alert() {
    need_openssl!();
    let pki = Pki::new();
    pki.leaf("ec", "localhost", None);
    // AES-256 only (no 0xC02C offered), and a server that has only P-384 (the client offers X25519 and P-256).
    for extra in [
        ["-cipher", "ECDHE-ECDSA-AES256-GCM-SHA384", "-groups", "X25519"],
        ["-cipher", "ECDHE-ECDSA-AES128-GCM-SHA256", "-groups", "P-384"],
    ] {
        let mut args = vec!["-sigalgs", "ECDSA+SHA256"];
        args.extend(extra);
        let srv = www12(&pki, "ec", &args);
        match client::connect(dial(srv.port), "localhost", &roots(&pki)) {
            Err(Error::Record(record::Error::PeerAlert(a))) => assert_eq!(a, 40, "handshake_failure"),
            r => panic!("{:?}", r.err()),
        }
    }
}
