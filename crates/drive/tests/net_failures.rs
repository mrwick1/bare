//! A network failure keeps its cause for diagnostics (S5 finding of 2026-11-07: a run in which each fetch failed with "network error" could not
//! show why). A loopback server that refuses, resets in the middle of a body, answers a TLS handshake with text, or goes silent: the JSON of
//! `bare-drive` gives the kind in the `network` record, in the `fetch` console note and in the failure message. The page still sees only
//! `TypeError: Failed to fetch`, as in Chrome.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

const PAGE: &str = "<!DOCTYPE html><title>t</title><p id=r>waiting</p><script>fetch('/data').then(function (r) { return r.text() }).then(function (t) { document.getElementById('r').textContent = 'got ' + t }, function (e) { document.getElementById('r').textContent = String(e) })</script>";

/// What a connection does. The first connection (index 0) is the page; the second is the fetch.
type Handler = fn(usize, TcpStream) -> bool;

/// Serve on a free port: `handler(n, stream)` for the n-th connection. The first one runs here, and when it gives `false`, the listener closes.
/// The others run on their own threads (their result does not matter).
fn serve(handler: Handler) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for (n, s) in listener.incoming().flatten().enumerate() {
            if n > 0 {
                std::thread::spawn(move || handler(n, s));
            } else if !handler(n, s) {
                break;
            }
        }
    });
    port
}

/// Read the request head.
fn head(s: &mut TcpStream) {
    let (mut seen, mut b) = (Vec::new(), [0u8; 1024]);
    while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
        match s.read(&mut b) {
            Ok(0) | Err(_) => return,
            Ok(n) => seen.extend_from_slice(&b[..n]),
        }
    }
}

fn page(s: &mut TcpStream) {
    head(s);
    write!(s, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{PAGE}", PAGE.len()).unwrap();
}

/// Run `open URL` and a wait for the text of `#r`. Gives the exit code and the JSON.
fn drive(name: &str, url: &str, until: &str, env: &[(&str, &str)]) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("bare-drive-net-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let steps = dir.join("steps");
    std::fs::write(&steps, format!("timeout 3000\nopen {url}\n{until}\n")).unwrap();
    let run = std::process::Command::new(env!("CARGO_BIN_EXE_bare-drive")).args(["--quiet", "--out"]).arg(dir.join("out")).arg(&steps).envs(env.iter().copied()).output().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
    (run.status.code().unwrap_or(-1), String::from_utf8_lossy(&run.stdout).to_string())
}

/// The fetch of `/data` fails; the page shows Chrome's text. The cause is in the record, in the console note and nowhere in the page.
fn fetch_fails(name: &str, handler: Handler, env: &[(&str, &str)], kind: &str) {
    let port = serve(handler);
    let (code, json) = drive(name, &format!("http://127.0.0.1:{port}/"), "expect css=\"#r\" text=\"TypeError: Failed to fetch\"", env);
    assert_eq!(code, 0, "{name}: {json}");
    let record = json.split("\"network\":[").nth(1).unwrap().split("\"url\":").find(|r| r.contains("/data\"")).unwrap_or_else(|| panic!("{name}: no record of /data: {json}"));
    assert!(record.contains(&format!("\"error\":\"{kind}: ")), "{name}: {record}");
    let note = json.split("\"level\":\"fetch\"").nth(1).unwrap_or_else(|| panic!("{name}: no fetch note: {json}")).split('}').next().unwrap();
    assert!(note.contains(&format!("failed: network error (http://127.0.0.1:{port}): {kind}: ")), "{name}: {note}");
    assert!(!note.contains("/data"), "{name}: a path in the note: {note}");
}

#[test]
fn a_refused_connection_shows_connect_refused() {
    // The page closes the listener, so the connection of the fetch is refused.
    fetch_fails("refused", |n, mut s| { page(&mut s); n > 0 }, &[], "connect refused");
}

#[test]
fn a_reset_in_the_middle_of_a_body_shows_reset() {
    fetch_fails(
        "reset",
        |n, mut s| {
            if n == 0 {
                page(&mut s);
                return true;
            }
            // Headers and 10 of 1,000 bytes, then a close with the request unread: the kernel sends a reset.
            std::thread::sleep(Duration::from_millis(150));
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n0123456789").unwrap();
            std::thread::sleep(Duration::from_millis(150));
            true
        },
        &[],
        "reset",
    );
}

#[test]
fn a_server_that_goes_silent_shows_read_timeout() {
    fetch_fails(
        "timeout",
        |n, mut s| {
            if n == 0 {
                page(&mut s);
            } else {
                head(&mut s);
                std::thread::sleep(Duration::from_secs(3));
            }
            true
        },
        &[("BARE_NET_READ_TIMEOUT_MS", "300")],
        "read timeout",
    );
}

#[test]
fn a_tls_handshake_that_gets_text_shows_tls() {
    // `https` to a server that does not speak TLS. The document fails to load: the failure message names the cause.
    let port = serve(|_, mut s| {
        let _ = s.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
        true
    });
    let (code, json) = drive("tls", &format!("https://127.0.0.1:{port}/"), "expect css=\"#r\" text=\"x\"", &[]);
    assert_eq!(code, 1, "{json}");
    let record = json.split("\"network\":[").nth(1).unwrap();
    assert!(record.contains("\"error\":\"tls: "), "{record}");
    let failure = json.split("\"failure\":").nth(1).unwrap();
    assert!(failure.contains("network failures: ") && failure.contains("tls: "), "{failure}");
}
