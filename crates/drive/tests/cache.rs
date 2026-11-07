//! The scripts of an app download once per driver run: a reload and a later `open` take a fresh copy from the HTTP cache, as Chrome
//! does in one tab. A loopback server counts the requests for each path.

use bare_drive::run::{Options, Redactor, run};
use bare_drive::steps::parse;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const INDEX: &str = r#"<!doctype html><html><body><p id="n">0</p><script src="/app.js"></script></body></html>"#;
const APP: &str = "document.getElementById('n').textContent = 'ran'";

fn serve(js: Arc<AtomicUsize>, conns: Arc<AtomicUsize>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            conns.fetch_add(1, Ordering::SeqCst);
            let js = js.clone();
            std::thread::spawn(move || connection(s, &js));
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn connection(mut s: TcpStream, js: &AtomicUsize) {
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
        let r = if head.starts_with("GET /app.js ") {
            js.fetch_add(1, Ordering::SeqCst);
            format!("HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nCache-Control: public, max-age=31536000, immutable\r\nContent-Length: {}\r\n\r\n{APP}", APP.len())
        } else {
            format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{INDEX}", INDEX.len())
        };
        if s.write_all(r.as_bytes()).is_err() {
            return;
        }
    }
}

#[test]
fn a_reload_and_a_second_open_take_the_script_from_the_cache() {
    let (js, conns) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let origin = serve(js.clone(), conns.clone());
    let rep = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let ran = "expect js \"document.getElementById('n').textContent === 'ran'\"\n";
            let src = format!("open ${{ORIGIN}}/\n{ran}reload\n{ran}open ${{ORIGIN}}/\n{ran}");
            let steps = parse(&src, &HashMap::from([("ORIGIN".to_string(), origin)])).unwrap();
            let opt = Options { size: (800, 600), timeout: Duration::from_secs(5), out: None, window: false, progress: false };
            run(&steps, &opt, &mut Redactor::default())
        })
        .unwrap()
        .join()
        .unwrap();
    assert!(rep.ok, "{:?}", rep.failure.as_ref().map(|f| &f.1.message));
    assert_eq!(js.load(Ordering::SeqCst), 1, "the script was downloaded once");
    // The second `open` reuses the connections of the first: no new connection for each page.
    assert!(conns.load(Ordering::SeqCst) <= 2, "connections: {}", conns.load(Ordering::SeqCst));
}
