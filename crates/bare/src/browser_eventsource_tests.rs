//! Tests of `EventSource` (`shims/eventsource.js`) against a loopback server that streams `text/event-stream`.

use crate::browser_fetch_tests::{Reply, reply, serve};
use crate::browser_xhr_tests::{browser, settle};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

fn stream(body: &str) -> Reply {
    reply(200, &[("Content-Type", "text/event-stream; charset=utf-8")], body.as_bytes())
}

fn run(src: &str) -> String {
    let n = AtomicUsize::new(0);
    let srv = serve(move |r| match r.path.as_str() {
        "/es" => stream("retry: 20\n: comment\ndata: one\n\nid: 7\nevent: ping\ndata: a\ndata: b\n\ndata: dropped"),
        "/again" => {
            let last = r.header("last-event-id").pop().unwrap_or_default();
            match n.fetch_add(1, SeqCst) {
                0 => stream("retry: 10\nid: 5\ndata: first\n\n"),
                _ => stream(&format!("data: second after {last}\n\n")),
            }
        }
        "/html" => reply(200, &[("Content-Type", "text/html")], b"data: x\n\n"),
        _ => reply(404, &[], b""),
    });
    let mut b = browser(srv.port);
    b.set_document_url(&format!("http://127.0.0.1:{}/page.html", srv.port)).unwrap();
    let r = crate::browser_xhr_tests::js(&mut b, &format!("var out = [], finished = false; {src}"));
    assert!(!["throw", "other", "unsupp", "limit"].iter().any(|p| r.starts_with(p)), "{r}");
    settle(&mut b);
    crate::browser_xhr_tests::js(&mut b, "out.join('|')")
}

#[test]
fn it_exists() {
    assert_eq!(run("out.push(typeof EventSource, EventSource.CLOSED); finished = true"), "function|2");
}

#[test]
fn message_named_event_and_last_event_id() {
    let got = run("var es = new EventSource('/es'); out.push(es.readyState);
        es.onopen = function () { out.push('open', es.readyState); };
        es.onmessage = function (e) { out.push('m:' + e.data + ':' + e.lastEventId + ':' + e.origin.slice(0, 7)); };
        es.addEventListener('ping', function (e) { out.push('p:' + e.data.replace('\\n', '+') + ':' + e.lastEventId); es.close(); finished = true; });");
    assert_eq!(got, "0|open|1|m:one::http://|p:a+b:7");
}

#[test]
fn reconnects_with_last_event_id() {
    let got = run("var es = new EventSource('/again');
        es.onmessage = function (e) { out.push(e.data); if (e.data !== 'first') { es.close(); finished = true; } };
        es.onerror = function () { out.push('err' + es.readyState); };");
    assert_eq!(got, "first|err0|second after 5");
}

#[test]
fn a_wrong_content_type_fails_and_closes() {
    let got = run("var es = new EventSource('/html'); es.onopen = function () { out.push('open'); };
        es.onerror = function () { out.push('err' + es.readyState); finished = true; };");
    assert_eq!(got, "err2");
}

#[test]
fn a_bad_url_throws_syntax_error() {
    assert_eq!(run("try { new EventSource('http://'); } catch (e) { out.push(e.name); } finished = true"), "SyntaxError");
}

/// A loopback server whose `/live` reply is a chunked `text/event-stream` that sends `parts` with a pause before each, then stays open
/// for `hold`. `/x` answers a POST with `done`. Gives the port.
fn live_server(parts: &'static [&'static str], pause: std::time::Duration, hold: std::time::Duration) -> u16 {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let (mut head, mut b) = (Vec::new(), [0u8; 1]);
                while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
                    head.push(b[0]);
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let len = head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
                let _ = c.read_exact(&mut vec![0; len]);
                if head.starts_with("POST /x ") {
                    let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndone");
                    return;
                }
                if !head.starts_with("GET /live ") {
                    let _ = c.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    return;
                }
                let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n");
                for p in parts {
                    std::thread::sleep(pause);
                    if c.write_all(format!("{:x}\r\n{p}\r\n", p.len()).as_bytes()).and_then(|_| c.flush()).is_err() {
                        return;
                    }
                }
                std::thread::sleep(hold);
            });
        }
    });
    port
}

/// Run `src` against `live_server`, pump the page until `finished` or `limit`, and give `out` and the time it took.
fn run_live(parts: &'static [&'static str], src: &str, limit: std::time::Duration) -> (String, std::time::Duration) {
    let port = live_server(parts, std::time::Duration::from_millis(50), std::time::Duration::from_secs(10));
    let mut b = browser(port);
    b.set_document_url(&format!("http://127.0.0.1:{port}/page.html")).unwrap();
    let r = crate::browser_xhr_tests::js(&mut b, &format!("var out = [], finished = false; {src}"));
    assert!(!["throw", "other", "unsupp", "limit"].iter().any(|p| r.starts_with(p)), "{r}");
    let start = std::time::Instant::now();
    crate::browser_xhr_tests::settle_for(&mut b, limit);
    (crate::browser_xhr_tests::js(&mut b, "out.join('|')"), start.elapsed())
}

#[test]
fn events_arrive_while_the_stream_is_open() {
    let (got, took) = run_live(&["data: one\n\n", "data: two\n\n"], "var es = new EventSource('/live');
        es.onopen = function () { out.push('open'); };
        es.onmessage = function (e) { out.push(e.data); if (e.data === 'two') { es.close(); finished = true; } };", std::time::Duration::from_secs(5));
    assert_eq!(got, "open|one|two");
    assert!(took < std::time::Duration::from_secs(3), "took {took:?}: the events waited for the end of the stream");
}

#[test]
fn an_event_split_over_chunks_fires_once() {
    let (got, _) = run_live(&["data: hel", "lo\r", "\n\r\n", "data: end\n\n"], "var es = new EventSource('/live');
        es.onmessage = function (e) { out.push(e.data); if (e.data === 'end') { es.close(); finished = true; } };", std::time::Duration::from_secs(5));
    assert_eq!(got, "hello|end");
}

#[test]
fn close_stops_the_events() {
    let (got, _) = run_live(&["data: one\n\n", "data: two\n\n", "data: three\n\n"], "var es = new EventSource('/live');
        es.onmessage = function (e) { out.push(e.data, es.readyState); es.close(); };", std::time::Duration::from_millis(600));
    assert_eq!(got, "one|1");
}

#[test]
fn a_post_completes_while_a_stream_is_open() {
    let (got, _) = run_live(&["data: one\n\n"], "var es = new EventSource('/live');
        es.onmessage = function (e) { out.push(e.data); fetch('/x', { method: 'POST', body: '{}' }).then(function (r) { return r.text(); }).then(function (t) { out.push(t); finished = true; }); };", std::time::Duration::from_secs(5));
    assert_eq!(got, "one|done");
}
