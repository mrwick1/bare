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
