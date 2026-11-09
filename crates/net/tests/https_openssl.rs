//! HTTPS through `bare_net::http::Client` with real certificate validation, against a loopback
//! `openssl s_server`. The trust anchor is a synthetic root made for each test, in RAM (see
//! tools/crypto/openssl_harness.rs). `s_server` serves one connection at a time, so a client that
//! opens a second connection while it holds the first would hang: the keep-alive tests rely on that.
//! These tests skip (and say so) when `openssl` is not installed.

#[path = "../../../tools/crypto/openssl_harness.rs"]
mod harness;

use bare_net::http::{Client, Request};
use bare_tls::trust_store;
use bare_url::Url;
use harness::{Pki, Server, openssl_available, watch_client};
use std::io::Read;
use std::thread::{JoinHandle, sleep};
use std::time::Duration;

macro_rules! need_openssl {
    () => {
        if !openssl_available() {
            eprintln!("skipped: openssl is not installed");
            return;
        }
    };
}

fn setup() -> (Pki, Server, Client) {
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    let srv = Server::start(&pki, "good", &[], true);
    let client = Client::with_roots(trust_store::decode_pem(&pki.root_pem()).unwrap());
    (pki, srv, client)
}

fn url(srv: &Server, path: &str) -> Url {
    Url::parse(&format!("https://localhost:{}{path}", srv.port)).unwrap()
}

const HEAD_END: &[u8] = b"\r\n\r\n";

/// Run a request on a thread: the test drives the server meanwhile. The client comes back for the next request.
fn spawn<R: std::fmt::Debug + Send + 'static>(mut client: Client, f: impl FnOnce(&mut Client) -> R + Send + 'static) -> JoinHandle<(Client, R)> {
    let note = watch_client();
    std::thread::spawn(move || {
        let r = f(&mut client);
        *note.lock().unwrap() = Some(harness::client_result(&r));
        (client, r)
    })
}

/// Send the reply in pieces, each its own write (so its own TLS record), with a pause between.
fn dribble(srv: &mut Server, pieces: &[&str]) {
    for p in pieces {
        srv.send(p.as_bytes());
        sleep(Duration::from_millis(25));
    }
}

fn ok(body: &str) -> String {
    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len())
}

#[test]
fn get_reads_a_buffered_body_from_split_records() {
    need_openssl!();
    let (_pki, mut srv, client) = setup();
    let u = url(&srv, "/page");
    let t = spawn(client, move |c| c.get(&u).map(|r| (r.status, r.body.clone())).map_err(|e| e.to_string()));
    let req = String::from_utf8(srv.expect(HEAD_END)).unwrap();
    assert!(req.starts_with("GET /page HTTP/1.1\r\nHost: localhost:"), "{req}");
    assert!(!req.contains("Connection:"), "{req}");
    dribble(&mut srv, &["HTTP/1.1 200 OK\r\nContent-Le", "ngth: 9\r\n\r\nhello ", "tl", "s"]);
    assert_eq!(t.join().unwrap().1, Ok((200, b"hello tls".to_vec())));
}

#[test]
fn get_stream_reads_a_chunked_body_from_split_records() {
    need_openssl!();
    let (_pki, mut srv, client) = setup();
    let u = url(&srv, "/stream");
    let t = spawn(client, move |c| {
        let mut s = c.get_stream(&u).map_err(|e| e.to_string())?;
        let mut text = String::new();
        s.body.read_to_string(&mut text).map_err(|e| e.to_string())?;
        Ok::<_, String>((s.response.status, text))
    });
    srv.expect(HEAD_END);
    dribble(&mut srv, &["HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel", "lo\r\n4\r\n tls\r", "\n0\r\n\r\n"]);
    assert_eq!(t.join().unwrap().1, Ok((200, "hello tls".to_string())));
}

#[test]
fn a_body_that_ends_at_close_needs_close_notify() {
    need_openssl!();
    // `-WWW` answers HTTP/1.0 with no length and then closes the right way (close_notify): the body is complete.
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    std::fs::write(pki.dir.join("f.txt"), b"until close").unwrap();
    let srv = Server::start(&pki, "good", &["-WWW"], false);
    let mut client = Client::with_roots(trust_store::decode_pem(&pki.root_pem()).unwrap());
    assert_eq!(client.get(&url(&srv, "/f.txt")).unwrap().body, b"until close");
    // A close with no close_notify is a cut body, not an end.
    let (_pki, mut srv, client) = setup();
    let u = url(&srv, "/eof");
    let t = spawn(client, move |c| c.get(&u).map(|r| r.body.clone()).map_err(|e| e.to_string()));
    srv.expect(HEAD_END);
    srv.send(b"HTTP/1.0 200 OK\r\n\r\nuntil clo");
    sleep(Duration::from_millis(100));
    srv.reset();
    let e = t.join().unwrap().1.unwrap_err();
    assert!(e.contains("record") || e.contains("end"), "{e}");
}

#[test]
fn a_redirect_with_no_length_that_closes_without_close_notify_is_followed() {
    need_openssl!();
    // craigslist.org answers "/" this way: 302, no length, then a bare close. The body of a redirect is not wanted.
    // The target is a second server: after a reset, the client could dial before the new `s_server` listens.
    let (pki, mut srv, client) = setup();
    let mut target = Server::start(&pki, "good", &[], true);
    let u = url(&srv, "/");
    let t = spawn(client, move |c| c.get(&u).map(|r| (r.status, r.body.clone())).map_err(|e| e.to_string()));
    srv.expect(HEAD_END);
    srv.send(format!("HTTP/1.1 302 Found\r\nLocation: https://localhost:{}/next\r\n\r\n", target.port).as_bytes());
    sleep(Duration::from_millis(100));
    srv.reset();
    let head = String::from_utf8(target.expect(HEAD_END)).unwrap();
    assert!(head.starts_with("GET /next HTTP/1.1"), "{head}");
    target.send(ok("moved").as_bytes());
    assert_eq!(t.join().unwrap().1, Ok((200, b"moved".to_vec())));
}

#[test]
fn send_once_gives_back_a_redirect_with_no_length_that_closes_without_close_notify() {
    need_openssl!();
    let (_pki, mut srv, client) = setup();
    let u = url(&srv, "/");
    let t = spawn(client, move |c| c.send_once(&Request::get(&u), 1 << 20).map(|r| (r.status, r.header("location").map(str::to_string))).map_err(|e| e.to_string()));
    srv.expect(HEAD_END);
    srv.send(b"HTTP/1.1 302 Found\r\nLocation: /next\r\n\r\n");
    sleep(Duration::from_millis(100));
    srv.reset();
    assert_eq!(t.join().unwrap().1, Ok((302, Some("/next".to_string()))));
}

#[test]
fn post_sends_the_exact_body_over_tls_for_a_small_and_a_big_body() {
    need_openssl!();
    let (_pki, mut srv, mut client) = setup();
    let big: Vec<u8> = (0..200_000u32).map(|i| ((i * 7) >> 3) as u8).collect();
    for body in [b"pw=hunter2&x=1".to_vec(), big] {
        let u = url(&srv, "/login");
        let want = body.clone();
        let t = spawn(client, move |c| {
            let headers = [("Content-Type", "application/x-www-form-urlencoded"), ("Authorization", "Bearer tok")];
            c.post(&u, &headers, &body).map(|r| (r.status, r.body.clone())).map_err(|e| e.to_string())
        });
        let head = String::from_utf8(srv.expect(HEAD_END)).unwrap();
        assert!(head.starts_with("POST /login HTTP/1.1\r\n"), "{head}");
        assert!(head.contains(&format!("Content-Length: {}\r\n", want.len())), "{head}");
        assert!(head.contains("Authorization: Bearer tok\r\n"), "{head}");
        assert_eq!(srv.take(want.len()), want);
        srv.send(ok("done").as_bytes());
        let (c, r) = t.join().unwrap();
        assert_eq!(r, Ok((200, b"done".to_vec())));
        client = c;
    }
}

#[test]
fn post_can_stream_its_reply() {
    need_openssl!();
    let (_pki, mut srv, client) = setup();
    let u = url(&srv, "/go");
    let t = spawn(client, move |c| {
        let mut s = c.send_stream(&Request::post(&u, &[], b"{}")).map_err(|e| e.to_string())?;
        let mut text = String::new();
        s.body.read_to_string(&mut text).map_err(|e| e.to_string())?;
        Ok::<_, String>(text)
    });
    srv.expect(HEAD_END);
    assert_eq!(srv.take(2), b"{}");
    dribble(&mut srv, &["HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nstr", "eamed"]);
    assert_eq!(t.join().unwrap().1, Ok("streamed".to_string()));
}

#[test]
fn keep_alive_reuses_one_tls_connection_for_get_and_post() {
    need_openssl!();
    // `s_server` takes one connection at a time: a second connection would never be served and the request would fail.
    let (_pki, mut srv, mut client) = setup();
    for i in 0..6 {
        let u = url(&srv, &format!("/n{i}"));
        let post = i % 2 == 1;
        let t = spawn(client, move |c| if post { c.post(&u, &[], b"x=1") } else { c.get(&u) }.map(|r| r.body.clone()).map_err(|e| e.to_string()));
        let head = String::from_utf8(srv.expect(HEAD_END)).unwrap();
        assert!(head.starts_with(&format!("{} /n{i} HTTP/1.1", if post { "POST" } else { "GET" })), "{head}");
        if post {
            srv.take(3);
        }
        // Part of the next reply goes in the same write as the end of this one: the leftover must stay in order.
        srv.send(ok(&format!("r{i}")).as_bytes());
        let (c, r) = t.join().unwrap();
        assert_eq!(r, Ok(format!("r{i}").into_bytes()));
        client = c;
    }
}

#[test]
fn a_get_reconnects_when_the_server_closed_an_idle_tls_connection() {
    need_openssl!();
    let (_pki, mut srv, mut client) = setup();
    for i in 0..3 {
        let u = url(&srv, "/again");
        let t = spawn(client, move |c| c.get(&u).map(|r| r.body.clone()).map_err(|e| e.to_string()));
        srv.expect(HEAD_END);
        srv.send(ok("fresh").as_bytes());
        let (c, r) = t.join().unwrap();
        assert_eq!(r, Ok(b"fresh".to_vec()), "round {i}");
        client = c;
        srv.reset();
        sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_post_never_reuses_a_closed_connection_and_is_never_sent_twice() {
    need_openssl!();
    let (_pki, mut srv, mut client) = setup();
    // 1. A first reply, then the server closes the idle connection. The POST must open a new one (not guess).
    let u = url(&srv, "/a");
    let t = spawn(client, move |c| c.get(&u).map(|r| r.status).map_err(|e| e.to_string()));
    srv.expect(HEAD_END);
    srv.send(ok("a").as_bytes());
    client = t.join().unwrap().0;
    srv.reset();
    sleep(Duration::from_millis(100));
    let u = url(&srv, "/b");
    let t = spawn(client, move |c| c.post(&u, &[], b"once").map(|r| r.status).map_err(|e| e.to_string()));
    let head = String::from_utf8(srv.expect(HEAD_END)).unwrap();
    assert!(head.starts_with("POST /b "), "{head}");
    assert_eq!(srv.take(4), b"once");
    // 2. The server takes the POST and closes without a reply. The client must report an error and not send it again.
    srv.reset();
    let (client, r) = t.join().unwrap();
    assert!(r.is_err(), "{r:?}");
    let more = srv.collect(600);
    assert!(more.is_empty(), "the POST was sent again: {:?}", String::from_utf8_lossy(&more));
    drop(client);
}

#[test]
fn a_redirect_is_followed_over_tls_and_a_cross_origin_one_drops_secret_headers() {
    need_openssl!();
    let (pki, mut a, client) = setup();
    let mut b = Server::start(&pki, "good", &[], true);
    let (ua, ub) = (url(&a, "/start"), url(&b, "/landing"));
    let target = ub.to_string();
    let t = spawn(client, move |c| {
        let headers = [("Authorization", "Bearer secret-token"), ("X-Keep", "yes")];
        c.send(&Request::post(&ua, &headers, b"data")).map(|r| (r.status, r.body.clone())).map_err(|e| e.to_string())
    });
    let first = String::from_utf8(a.expect(HEAD_END)).unwrap();
    assert!(first.contains("Authorization: Bearer secret-token"), "{first}");
    assert_eq!(a.take(4), b"data");
    a.send(format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: {target}\r\nContent-Length: 0\r\n\r\n").as_bytes());
    let second = String::from_utf8(b.expect(HEAD_END)).unwrap();
    assert!(second.starts_with("POST /landing HTTP/1.1"), "{second}");
    assert!(second.contains("X-Keep: yes") && !second.to_lowercase().contains("authorization"), "{second}");
    assert_eq!(b.take(4), b"data");
    b.send(ok("landed").as_bytes());
    assert_eq!(t.join().unwrap().1, Ok((200, b"landed".to_vec())));
}

#[test]
fn a_same_origin_302_turns_post_into_get_over_the_same_connection() {
    need_openssl!();
    let (_pki, mut srv, client) = setup();
    let u = url(&srv, "/form");
    let t = spawn(client, move |c| c.post(&u, &[], b"a=b").map(|r| r.body.clone()).map_err(|e| e.to_string()));
    srv.expect(HEAD_END);
    srv.take(3);
    srv.send(b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\n\r\n");
    let next = String::from_utf8(srv.expect(HEAD_END)).unwrap();
    assert!(next.starts_with("GET /next HTTP/1.1"), "{next}");
    srv.send(ok("next page").as_bytes());
    assert_eq!(t.join().unwrap().1, Ok(b"next page".to_vec()));
}

/// What the untrusted server got. A handshake that fails sends no application data, so there is none.
fn nothing_sent(srv: &mut Server) {
    let got = srv.collect(300);
    assert!(got.is_empty(), "application data reached an untrusted peer: {:?}", String::from_utf8_lossy(&got));
}

#[test]
fn an_untrusted_or_wrong_name_peer_gets_no_request_and_no_body() {
    need_openssl!();
    let (_pki, mut srv, _) = setup();
    let other = Pki::new();
    let secret = [("Authorization", "Bearer s3cret")];
    // A root that did not sign the leaf.
    let mut wrong_root = Client::with_roots(trust_store::decode_pem(&other.root_pem()).unwrap());
    let e = wrong_root.post(&url(&srv, "/login"), &secret, b"pw=hunter2").err().unwrap().to_string();
    assert!(e.contains("NoIssuer"), "{e}");
    assert!(!e.contains("s3cret") && !e.contains("hunter2"), "{e}");
    nothing_sent(&mut srv);
    assert!(wrong_root.get(&url(&srv, "/")).is_err());
    assert!(wrong_root.get_stream(&url(&srv, "/")).is_err());
    nothing_sent(&mut srv);
    // The OS bundle does not hold the synthetic root.
    let e = Client::new().post(&url(&srv, "/login"), &secret, b"pw=hunter2").err().unwrap().to_string();
    assert!(e.contains("NoIssuer"), "{e}");
    nothing_sent(&mut srv);
    // A right root and a wrong host name.
    let pki2 = Pki::new();
    pki2.leaf("good", "localhost", None);
    let mut srv2 = Server::start(&pki2, "good", &[], true);
    let mut c = Client::with_roots(trust_store::decode_pem(&pki2.root_pem()).unwrap());
    let wrong = Url::parse(&format!("https://wrong.localhost:{}/login", srv2.port)).unwrap();
    let e = c.post(&wrong, &secret, b"pw=hunter2").err().unwrap().to_string();
    assert!(e.contains("Host"), "{e}");
    nothing_sent(&mut srv2);
}

#[test]
fn a_redirect_from_a_trusted_server_to_an_untrusted_one_sends_nothing_there() {
    need_openssl!();
    let (_pki, mut a, client) = setup();
    let other = Pki::new();
    other.leaf("good", "localhost", None);
    let mut evil = Server::start(&other, "good", &[], true);
    let ua = url(&a, "/start");
    let target = url(&evil, "/steal").to_string();
    let t = spawn(client, move |c| c.post(&ua, &[("Cookie", "sid=1")], b"pw=hunter2").map(|r| r.status).map_err(|e| e.to_string()));
    a.expect(HEAD_END);
    a.take(10);
    a.send(format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: {target}\r\nContent-Length: 0\r\n\r\n").as_bytes());
    let r = t.join().unwrap().1;
    assert!(r.unwrap_err().contains("NoIssuer"));
    nothing_sent(&mut evil);
}

#[test]
fn a_timeout_names_the_caller_and_the_state_of_the_server_and_client() {
    need_openssl!();
    let (_pki, mut srv, client) = setup();
    let u = url(&srv, "/quiet");
    // The client asks for a path and the server never replies: the client returns an error after its own time limit,
    // so it is still running when the short wait ends.
    let t = spawn(client, move |c| c.get(&u).map(|r| r.status).map_err(|e| e.to_string()));
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        srv.expect_within(b"never in a request", Duration::from_millis(300));
    })).unwrap_err();
    let message = panic.downcast_ref::<String>().unwrap();
    for field in ["https_openssl.rs:", "s_server running", "stderr", "client still running (or panicked)"] {
        assert!(message.contains(field), "missing {field}: {message}");
    }
    drop(srv);
    assert!(t.join().unwrap().1.is_err());
}

#[test]
fn a_port_that_another_process_listens_on_is_not_a_started_server() {
    need_openssl!();
    let pki = Pki::new();
    pki.leaf("good", "localhost", None);
    let other = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = other.local_addr().unwrap().port();
    let e = Server::try_start(&pki, "good", &[], true, port).err().expect("the probe reached the other listener, not s_server");
    assert!(e.contains("Address already in use"), "{e}");
}

#[test]
fn client_diagnostics_stop_formatting_at_the_limit() {
    struct Long(std::cell::Cell<usize>);
    impl std::fmt::Debug for Long {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            for _ in 0..100_000 {
                self.0.set(self.0.get() + 1);
                f.write_str("x")?;
            }
            Ok(())
        }
    }
    let value = Long(std::cell::Cell::new(0));
    let text: String = harness::client_result(&value);
    assert_eq!(text.len(), 300);
    assert!(value.0.get() <= 301, "formatted {} characters", value.0.get());
}
