//! The TLS client with the real path validator against a loopback `openssl s_server`.
//! The trust anchor is a synthetic root made for each test, in RAM. See tools/crypto/openssl_harness.rs.
//! These tests skip (and say so) when `openssl` is not installed.

#[path = "../../../tools/crypto/openssl_harness.rs"]
mod harness;

use bare_tls::client::{self, ClientStream, Error};
use bare_tls::trust_store::{self, TrustStore};
use harness::{Pki, Server, openssl_available};
use std::io::{self, Read, Write};
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

/// A server that serves the files of the PKI directory (HTTP/1.0, closes at the end).
fn www(pki: &Pki, leaf: &str) -> Server {
    std::fs::write(pki.dir.join("hello.txt"), b"hello over verified tls").unwrap();
    Server::start(pki, leaf, &["-WWW"], false)
}

fn get<T: Read + Write>(c: &mut ClientStream<T>) -> Vec<u8> {
    c.write_all(b"GET /hello.txt HTTP/1.0\r\n\r\n").unwrap();
    let mut body = Vec::new();
    c.read_to_end(&mut body).unwrap();
    body
}

/// Gives at most `n` bytes for each read: records and handshake messages arrive in pieces.
struct Trickle(TcpStream, usize);
impl Read for Trickle {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = out.len().min(self.1);
        self.0.read(&mut out[..n])
    }
}
impl Write for Trickle {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.write(&b[..b.len().min(self.1)])
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

fn cert_error<T>(r: Result<ClientStream<T>, Error>) -> String
where
    T: Read + Write,
{
    match r {
        Err(Error::Certificate(m)) => m,
        Err(e) => panic!("wrong error: {e}"),
        Ok(_) => panic!("the handshake succeeded"),
    }
}

#[test]
fn a_chain_to_the_trusted_root_connects_and_reads() {
    need_openssl!();
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    let srv = www(&pki, "good");
    let mut c = client::connect(dial(srv.port), "localhost", &roots(&pki)).unwrap();
    let body = get(&mut c);
    assert!(body.ends_with(b"hello over verified tls"), "{:?}", String::from_utf8_lossy(&body));
}

#[test]
fn split_reads_still_verify_and_read() {
    need_openssl!();
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    let srv = www(&pki, "good");
    for n in [1, 2, 7, 100] {
        let mut c = client::connect(Trickle(dial(srv.port), n), "localhost", &roots(&pki)).unwrap();
        assert!(get(&mut c).ends_with(b"hello over verified tls"), "read size {n}");
    }
}

#[test]
fn a_root_that_did_not_sign_the_leaf_is_refused() {
    need_openssl!();
    let (server_pki, other) = (Pki::new(), Pki::new());
    server_pki.leaf("good", "localhost", None);
    let srv = www(&server_pki, "good");
    let why = cert_error(client::connect(dial(srv.port), "localhost", &roots(&other)));
    assert!(why.contains("NoIssuer"), "{why}");
}

#[test]
fn a_root_that_the_peer_sends_is_not_trusted() {
    need_openssl!();
    let (server_pki, other) = (Pki::new(), Pki::new());
    server_pki.leaf("good", "localhost", None);
    // The server sends its own root after the leaf. The client trusts only `other`.
    let srv = Server::start(&server_pki, "good", &["-WWW", "-cert_chain", "root.pem"], false);
    let why = cert_error(client::connect(dial(srv.port), "localhost", &roots(&other)));
    assert!(why.contains("NoIssuer"), "{why}");
}

#[test]
fn the_wrong_host_name_is_refused() {
    need_openssl!();
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    let srv = www(&pki, "good");
    for host in ["wrong.localhost", "localhost.example", "xlocalhost"] {
        let why = cert_error(client::connect(dial(srv.port), host, &roots(&pki)));
        assert!(why.contains("Host"), "{host}: {why}");
    }
}

#[test]
fn an_expired_or_not_yet_valid_leaf_is_refused() {
    need_openssl!();
    let pki = Pki::new();
    pki.leaf("old", "localhost", Some(("20200101000000Z", "20200102000000Z")));
    pki.leaf("future", "localhost", Some(("20990101000000Z", "20990102000000Z")));
    for (leaf, want) in [("old", "Expired"), ("future", "NotYetValid")] {
        let srv = www(&pki, leaf);
        let why = cert_error(client::connect(dial(srv.port), "localhost", &roots(&pki)));
        assert!(why.contains(want), "{leaf}: {why}");
    }
}

#[test]
fn an_empty_trust_set_trusts_nothing() {
    need_openssl!();
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    let srv = www(&pki, "good");
    // `decode_pem` refuses an empty bundle: there is no way to build an allow-all store.
    assert!(trust_store::decode_pem(b"").is_err());
    let other = Pki::new();
    let why = cert_error(client::connect(dial(srv.port), "localhost", &roots(&other)));
    assert!(why.contains("NoIssuer"), "{why}");
}

#[test]
fn http11_alpn_is_negotiated_and_a_server_with_no_common_alpn_aborts() {
    need_openssl!();
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    let srv = Server::start(&pki, "good", &["-WWW", "-alpn", "http/1.1"], false);
    assert!(client::connect(dial(srv.port), "localhost", &roots(&pki)).unwrap().alpn_http11());
    // The server only knows h2 and we offer only http/1.1: OpenSSL aborts with no_application_protocol (120).
    // A server that selects a protocol we did not offer is rejected too: see `Error::Alpn` in the unit tests.
    let srv = Server::start(&pki, "good", &["-WWW", "-alpn", "h2"], false);
    match client::connect(dial(srv.port), "localhost", &roots(&pki)) {
        Err(Error::Record(e)) => assert!(e.to_string().contains("120"), "{e}"),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("connected to a server with no common ALPN"),
    }
}

#[test]
fn h2_is_negotiated_when_offered_and_the_server_picks_it() {
    need_openssl!();
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    // The server knows both and prefers h2 (the -alpn list is in order of preference).
    let srv = Server::start(&pki, "good", &["-WWW", "-alpn", "h2,http/1.1"], false);
    let c = client::connect_h2(dial(srv.port), "localhost", &roots(&pki)).unwrap();
    assert!(c.alpn_h2() && !c.alpn_http11());
    drop(c); // `s_server` serves one connection at a time
    // The same server with a client that does not offer h2 gets http/1.1.
    let c = client::connect(dial(srv.port), "localhost", &roots(&pki)).unwrap();
    assert!(c.alpn_http11() && !c.alpn_h2());
    // A server that only knows http/1.1: the client that offers h2 falls back to it.
    let srv = Server::start(&pki, "good", &["-WWW", "-alpn", "http/1.1"], false);
    let c = client::connect_h2(dial(srv.port), "localhost", &roots(&pki)).unwrap();
    assert!(c.alpn_http11() && !c.alpn_h2());
    // A server with no ALPN at all: neither flag, and the caller speaks HTTP/1.1.
    let srv = Server::start(&pki, "good", &["-WWW"], false);
    let c = client::connect_h2(dial(srv.port), "localhost", &roots(&pki)).unwrap();
    assert!(!c.alpn_http11() && !c.alpn_h2());
}

/// A server with an RSA leaf that signs CertificateVerify with `sigalg`. Returns the HTTP body.
fn rsa_server(pki: &Pki, leaf: &str, sigalg: &str) -> Server {
    std::fs::write(pki.dir.join("hello.txt"), b"hello over verified tls").unwrap();
    Server::start(pki, leaf, &["-WWW", "-sigalgs", sigalg], false)
}

#[test]
fn an_rsa_leaf_connects_with_rsa_pss() {
    need_openssl!();
    // (root is RSA, leaf bits, hash the root signs with, CertificateVerify scheme the server uses)
    for (rsa_root, bits, md, sigalg) in [
        (false, 2048, "sha256", "rsa_pss_rsae_sha256"),
        (false, 3072, "sha256", "rsa_pss_rsae_sha384"),
        (true, 2048, "sha256", "rsa_pss_rsae_sha256"),
        (true, 2048, "sha384", "rsa_pss_rsae_sha384"),
        (false, 4096, "sha256", "rsa_pss_rsae_sha256"),
    ] {
        let pki = if rsa_root { Pki::new_rsa() } else { Pki::new() };
        pki.rsa_leaf("rsa", "localhost", bits, md);
        let srv = rsa_server(&pki, "rsa", sigalg);
        let body = get(&mut client::connect(dial(srv.port), "localhost", &roots(&pki)).unwrap());
        assert!(body.ends_with(b"hello over verified tls"), "{rsa_root} {bits} {md} {sigalg}: {:?}", String::from_utf8_lossy(&body));
    }
}

#[test]
fn an_rsa_chain_with_the_wrong_root_is_refused() {
    need_openssl!();
    let (pki, other) = (Pki::new_rsa(), Pki::new_rsa());
    pki.rsa_leaf("rsa", "localhost", 2048, "sha256");
    let srv = rsa_server(&pki, "rsa", "rsa_pss_rsae_sha256");
    let why = cert_error(client::connect(dial(srv.port), "localhost", &roots(&other)));
    assert!(why.contains("NoIssuer"), "{why}");
}
