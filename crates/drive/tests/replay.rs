//! A journey that was recorded (`BARE_NET_RECORD`) passes again on replay (`BARE_NET_REPLAY`) with no request to the server, and a page that
//! the recording does not hold fails with a clear error. The binary `bare-drive` runs each pass, as the benchmark runner does.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;

const INDEX: &str = r#"<!doctype html><html><body><p id="n">0</p><script src="/app.js"></script></body></html>"#;
const APP: &str = "fetch('/data').then(r => r.text()).then(t => { document.getElementById('n').textContent = 'data:' + t })";

/// A server that counts its connections. Gives its origin.
fn serve(hits: Arc<AtomicUsize>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            hits.fetch_add(1, SeqCst);
            std::thread::spawn(move || connection(s));
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn connection(mut s: TcpStream) {
    let (mut seen, mut piece) = (Vec::new(), [0u8; 4096]);
    loop {
        let end = loop {
            if let Some(at) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
            match s.read(&mut piece) {
                Ok(0) | Err(_) => return,
                Ok(n) => seen.extend_from_slice(&piece[..n]),
            }
        };
        let head = String::from_utf8_lossy(&seen[..end]).to_string();
        seen.drain(..end);
        let (status, kind, body) = match head.split(' ').nth(1).unwrap_or("") {
            "/" => (200, "text/html", INDEX),
            "/app.js" => (200, "text/javascript", APP),
            "/data" => (200, "text/plain", "7"),
            _ => (404, "text/plain", "no"),
        };
        let reply = format!("HTTP/1.1 {status} X\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\r\n{body}", body.len());
        if s.write_all(reply.as_bytes()).is_err() {
            return;
        }
    }
}

/// Run a step file as `bare-drive`, with one tape variable set. Gives the exit code and stderr.
fn drive(dir: &PathBuf, steps: &str, base: &str, tape: (&str, &PathBuf)) -> (i32, String) {
    let file = dir.join("journey.steps");
    std::fs::write(&file, steps).unwrap();
    let run = std::process::Command::new(env!("CARGO_BIN_EXE_bare-drive"))
        .args(["--quiet", "--var", &format!("BASE={base}"), "--out", dir.join("out").to_str().unwrap()])
        .arg(&file)
        .env(tape.0, tape.1)
        .env("BARE_SEED", "1")
        .output()
        .unwrap();
    (run.status.code().unwrap_or(-1), String::from_utf8_lossy(&run.stderr).to_string())
}

#[test]
fn a_recorded_journey_passes_on_replay_with_no_request_and_a_missing_page_is_an_error() {
    let dir = std::env::temp_dir().join(format!("bare-drive-replay-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (hits, tape) = (Arc::new(AtomicUsize::new(0)), dir.join("tape"));
    let base = serve(hits.clone());
    let steps = "timeout 20000\nopen ${BASE}/\nexpect js \"document.getElementById('n').textContent === 'data:7'\"\n";

    let (code, err) = drive(&dir, steps, &base, ("BARE_NET_RECORD", &tape));
    assert_eq!(code, 0, "the live run: {err}");
    let live = hits.load(SeqCst);
    assert!(live > 0);

    let (code, err) = drive(&dir, steps, &base, ("BARE_NET_REPLAY", &tape));
    assert_eq!(code, 0, "the replay: {err}");
    assert_eq!(hits.load(SeqCst), live, "a replay must not reach the server");
    assert!(!err.contains("no recorded reply"), "{err}");

    let other = "timeout 3000\nopen ${BASE}/missing\nexpect js \"document.getElementById('n') !== null\"\n";
    let (code, err) = drive(&dir, other, &base, ("BARE_NET_REPLAY", &tape));
    assert_ne!(code, 0, "{err}");
    assert!(err.contains(&format!("no recorded reply for GET {base}/missing")), "{err}");
    assert_eq!(hits.load(SeqCst), live, "a miss must not reach the server");
    let _ = std::fs::remove_dir_all(&dir);
}
