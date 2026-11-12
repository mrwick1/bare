//! A live page queues a `fetch` and then keeps its thread busy for 2 s in five script tasks. The request must reach the server at
//! once, not at the next network turn of the page. The reply may wait for a turn. The loopback server notes when each request came.

use bare::page::Page;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The fetch is queued in the first lines of the inline script. Then each task spins for 400 ms on the clock of the page.
const INDEX: &str = r#"<!doctype html><title>busy</title><p id="out"></p><script>
var got = '', left = 5
fetch('/data').then(function (r) { return r.text() }).then(function (t) { got = t; done() })
function spin() { var end = Date.now() + 400; while (Date.now() < end) {} }
function task() { spin(); left--; if (left > 0) setTimeout(task, 0); else done() }
function done() { if (left === 0 && got) document.getElementById('out').textContent = 'done ' + got }
task()
</script>"#;

/// When the server saw each request: the path, and when it began to read it or wrote the last byte of the reply.
type Seen = Arc<Mutex<Vec<(String, Instant, Instant)>>>;

fn serve() -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Seen = Arc::default();
    let s2 = seen.clone();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let seen = s2.clone();
            std::thread::spawn(move || connection(s, seen));
        }
    });
    (format!("http://127.0.0.1:{port}"), seen)
}

fn connection(mut s: TcpStream, seen: Seen) {
    s.set_nodelay(true).ok();
    let (mut buf, mut piece) = (Vec::new(), [0u8; 4096]);
    loop {
        let end = loop {
            if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
            match s.read(&mut piece) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&piece[..n]),
            }
        };
        let came = Instant::now();
        let head = String::from_utf8_lossy(&buf[..end]).to_string();
        buf.drain(..end);
        let path = head.split(' ').nth(1).unwrap_or("/").to_string();
        let (ty, body) = if path == "/" { ("text/html", INDEX) } else if path == "/data" { ("text/plain", "ok") } else { ("text/plain", "none") };
        let r = format!("HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\r\n{body}", body.len());
        let ok = s.write_all(r.as_bytes()).is_ok();
        seen.lock().unwrap().push((path, came, Instant::now()));
        if !ok {
            return;
        }
    }
}

#[test]
fn a_queued_fetch_reaches_the_server_while_scripts_keep_the_page_busy() {
    let (origin, seen) = serve();
    let (out, took) = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let mut p = Page::open(&format!("{origin}/"), (800, 600)).unwrap();
            p.quiet = true;
            let t = Instant::now();
            loop {
                assert!(t.elapsed() < Duration::from_secs(15), "time limit");
                let (ran, next) = p.tick();
                if !ran {
                    if let Some(next) = next {
                        std::thread::sleep(next.saturating_duration_since(Instant::now()).min(Duration::from_millis(20)));
                    }
                }
                let Some(b) = p.browser() else { continue };
                let Ok(v) = b.run_script("var o = document.getElementById('out'); o ? o.textContent : ''") else { continue };
                let s = b.it.display(v);
                if s.starts_with("done") {
                    return (s, t.elapsed());
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(out, "done ok");
    assert!(took >= Duration::from_secs(2), "the page ran its busy tasks: {took:?}");
    let seen = seen.lock().unwrap();
    let page = seen.iter().find(|(p, ..)| p == "/").expect("the page was served").2;
    let data = seen.iter().find(|(p, ..)| p == "/data").expect("the fetch reached the server").1;
    let wait = data.saturating_duration_since(page);
    eprintln!("the fetch came {wait:?} after the page was served");
    // The page is parsed and its script starts in a few ms; the request is sent as it is queued.
    assert!(wait < Duration::from_millis(100), "the fetch came {wait:?} after the page was served");
}
