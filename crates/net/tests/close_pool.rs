//! The worker pool (`Client::spawn`) against servers that close connections in different ways. Each test checks the bodies in the
//! order asked, that each URL reached the server once, and that no worker hangs. The server is `common/quirky.rs`.

mod common;
use common::quirky::{Config, Limit, Style, start};

use bare_net::http::Client;
use bare_url::Url;
use std::time::Duration;

/// A test that hangs fails after this long. A run takes well under a second.
const HANG: Duration = Duration::from_secs(30);

fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || drop(tx.send(f())));
    rx.recv_timeout(HANG).expect("the test hangs")
}

fn url(port: u16, path: &str) -> Url {
    Url::parse(&format!("http://127.0.0.1:{port}{path}")).unwrap()
}

fn echo(_: &str, path: &str) -> Option<(&'static str, Vec<u8>)> {
    Some(("text/plain", format!("body of {path}").into_bytes()))
}

fn get(c: &mut Client, port: u16, path: &str) -> Result<String, String> {
    c.get(&url(port, path)).map(|r| String::from_utf8(r.body.clone()).unwrap()).map_err(|e| e.to_string())
}

fn keep(_: usize) -> Style {
    Style::KeepAlive
}
fn close(_: usize) -> Style {
    Style::CloseHeader
}
fn http10(_: usize) -> Style {
    Style::Http10
}
fn silent(_: usize) -> Style {
    Style::SilentClose
}
/// Every third reply closes, in three different ways; the rest keep the connection.
fn mixed(n: usize) -> Style {
    [Style::KeepAlive, Style::CloseHeader, Style::KeepAlive, Style::SilentClose, Style::KeepAlive, Style::Http10][n % 6]
}

/// 24 requests on the pool, in two groups so that the second group meets the connections that the first one left.
fn run_pool(cfg: Config) {
    let server = start(cfg, echo);
    let port = server.port;
    let (bodies, server) = within(move || {
        let client = Client::new();
        let mut bodies = Vec::new();
        for group in 0..2 {
            let tasks: Vec<_> = (0..12).map(|i| client.spawn(move |c| get(c, port, &format!("/g{group}/{i}")))).collect();
            bodies.extend(tasks.into_iter().map(|t| t.wait().expect("a worker lost its job")));
        }
        (bodies, server)
    });
    for (n, got) in bodies.iter().enumerate() {
        let path = format!("/g{}/{}", n / 12, n % 12);
        assert_eq!(got.as_deref(), Ok(format!("body of {path}").as_str()), "{path}");
        assert_eq!(server.hits("GET", &path), 1, "{path} reached the server {} times", server.hits("GET", &path));
    }
    assert_eq!(server.total(), 24, "{:?}", server.requests());
}

#[test]
fn connection_close_after_each_reply() {
    for ms in [0, 30] {
        run_pool(Config::new(close, Duration::from_millis(ms)));
    }
}

#[test]
fn http10_body_ends_at_the_close() {
    for ms in [0, 30] {
        run_pool(Config::new(http10, Duration::from_millis(ms)));
    }
}

#[test]
fn a_get_is_retried_when_the_server_closed_an_idle_connection() {
    for ms in [0, 30] {
        run_pool(Config::new(silent, Duration::from_millis(ms)));
    }
}

#[test]
fn a_slow_server_that_mixes_closes_with_keep_alive() {
    run_pool(Config::new(mixed, Duration::from_millis(100)));
}

#[test]
fn a_post_is_never_sent_twice_to_a_server_that_closes_idle_connections() {
    let server = start(Config::new(silent, Duration::from_millis(10)), echo);
    let port = server.port;
    let server = within(move || {
        let client = Client::new();
        for round in 0..30 {
            // A GET leaves a connection that the server closes. A POST that finds it must not be sent again after a failed write.
            let mut c = client.fork();
            get(&mut c, port, &format!("/get{round}")).unwrap();
            match c.post(&url(port, &format!("/post{round}")), &[], b"x=1") {
                Ok(r) => assert_eq!(r.body, format!("body of /post{round}").into_bytes()),
                Err(_) => {} // The server closed first and the POST went to a dead connection: an error, never a second send.
            }
            // The same on a worker.
            let task = client.spawn(move |c| c.post(&url(port, &format!("/wpost{round}")), &[], b"x=1").map(|_| ()).map_err(|e| e.to_string()));
            let _ = task.wait().expect("a worker lost its job");
        }
        server
    });
    for round in 0..30 {
        for path in [format!("/post{round}"), format!("/wpost{round}")] {
            assert!(server.hits("POST", &path) <= 1, "{path} reached the server {} times", server.hits("POST", &path));
        }
        assert_eq!(server.hits("GET", &format!("/get{round}")), 1);
    }
}

#[test]
fn a_server_that_serves_two_connections_at_once() {
    let mut cfg = Config::new(keep, Duration::from_millis(30));
    cfg.limit = Some(Limit::Wait(2));
    let server = start(cfg.clone(), echo);
    let port = server.port;
    let server = within(move || {
        let client = Client::new();
        let tasks: Vec<_> = (0..12).map(|i| client.spawn(move |c| get(c, port, &format!("/{i}")))).collect();
        for (i, t) in tasks.into_iter().enumerate() {
            assert_eq!(t.wait().expect("a worker lost its job"), Ok(format!("body of /{i}")));
        }
        server
    });
    assert!(server.peak.load(std::sync::atomic::Ordering::SeqCst) <= 2);
    assert_eq!(server.total(), 12);
    for i in 0..12 {
        assert_eq!(server.hits("GET", &format!("/{i}")), 1);
    }
}

/// A new connection that the server closes with no reply is an error for that request (the client does not guess, and a retry on a
/// fresh connection would hide a server that is down). It must end the request, not hang the pool, and never repeat a request.
#[test]
fn a_server_that_closes_the_extra_connections_at_once() {
    let mut cfg = Config::new(keep, Duration::from_millis(30));
    cfg.limit = Some(Limit::Refuse(2));
    let server = start(cfg, echo);
    let port = server.port;
    let (results, server) = within(move || {
        let client = Client::new();
        let tasks: Vec<_> = (0..12).map(|i| client.spawn(move |c| get(c, port, &format!("/{i}")))).collect();
        (tasks.into_iter().map(|t| t.wait().expect("a worker lost its job")).collect::<Vec<_>>(), server)
    });
    for (i, r) in results.iter().enumerate() {
        if let Ok(body) = r {
            assert_eq!(body, &format!("body of /{i}"));
        }
        assert!(server.hits("GET", &format!("/{i}")) <= 1, "/{i} reached the server twice");
    }
    assert!(results.iter().any(Result::is_ok), "no request got through: {results:?}");
}
