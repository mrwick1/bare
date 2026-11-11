//! Tests of `XMLHttpRequest` and `ProgressEvent` (`browser_xhr.rs`). The cases of `tools/fixtures/ui/xhr-cases.txt` run against a
//! loopback server that answers like `tools/ui/chrome_xhr.mjs`. Each result must equal the line headless Chrome gave
//! (`tools/fixtures/ui/xhr-expected.txt`). The other tests pin the gates, the limits and the GC.

use crate::browser::*;
use crate::browser_fetch::*;
use crate::browser_fetch_tests::{Rec, Reply, Srv, reply, serve};
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;
use bare_net::http::Client;
use std::time::{Duration, Instant};

const PRELUDE: &str = include_str!("../../../tools/fixtures/ui/xhr-prelude.js");
const CASES: &str = include_str!("../../../tools/fixtures/ui/xhr-cases.txt");
const EXPECTED: &str = include_str!("../../../tools/fixtures/ui/xhr-expected.txt");

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn send(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Reply {
    reply(status, headers, body)
}

/// The endpoints of `chrome_xhr.mjs`.
pub fn server() -> Srv {
    serve(|r: &Rec| {
        let path = r.path.split('?').next().unwrap_or("");
        let hv = |n: &str| r.head.lines().find_map(|l| l.strip_prefix(&format!("{n}:"))).map(|v| v.trim().to_string()).unwrap_or_default();
        let text = [("Content-Type", "text/plain")];
        match path {
            "/page.html" => send(200, &[("Content-Type", "text/html")], b"<!doctype html><body></body>"),
            "/text" => send(200, &[("Content-Type", "text/plain; charset=utf-8"), ("X-Custom", "abc")], b"hello"),
            "/json" => send(200, &[("Content-Type", "application/json")], br#"{"a":1,"b":[true,null]}"#),
            "/badjson" => send(200, &[("Content-Type", "application/json")], b"{oops"),
            "/echo" => {
                let j = format!("{{\"m\":{:?},\"ct\":{:?},\"x\":{:?},\"b\":{:?}}}", r.method, hv("content-type"), hv("x-test"), String::from_utf8_lossy(&r.body));
                send(200, &text, j.as_bytes())
            }
            "/accept" => send(200, &text, hv("accept").as_bytes()),
            "/missing" => send(404, &text, b"nope"),
            "/reset" => Reply { raw: Vec::new(), close: true },
            "/slow" => {
                std::thread::sleep(Duration::from_millis(300));
                send(200, &text, b"late")
            }
            "/bin" => send(200, &[("Content-Type", "application/octet-stream")], &[0, 1, 2, 253, 254, 255]),
            "/headers" => Reply {
                raw: b"HTTP/1.1 200 X\r\nContent-Type: text/plain\r\nX-Dup: a\r\nX-Dup: b\r\nx-mixed-Case: v\r\nSet-Cookie: s=1\r\nContent-Length: 1\r\n\r\nh".to_vec(),
                close: false,
            },
            "/redir" => send(302, &[("Location", "/text")], b""),
            "/empty" => send(204, &[], b""),
            "/len" => send(200, &text, b"0123456789"),
            "/chunked" => Reply { raw: b"HTTP/1.1 200 X\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n3\r\ndef\r\n0\r\n\r\n".to_vec(), close: false },
            "/utf8" => send(200, &[("Content-Type", "text/plain; charset=utf-8")], "héllo 世界".as_bytes()),
            _ => send(404, &text, b"unknown"),
        }
    })
}

pub fn browser(port: u16) -> Browser {
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 20_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.configure_fetch(FetchConfig { base_url: Some(format!("http://127.0.0.1:{port}/page.html")), client: Client::new(), limits: FetchLimits::default() }).unwrap();
    b
}

pub fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

/// Pump timers and the network until the script sets `finished`.
pub fn settle(b: &mut Browser) {
    settle_for(b, Duration::from_secs(5));
}

/// `settle`, with a time limit.
pub fn settle_for(b: &mut Browser, limit: Duration) {
    let end = Instant::now() + limit;
    b.drain().unwrap();
    while Instant::now() < end && js(b, "finished") != "true" {
        b.run_scheduled_tasks(100).unwrap();
        b.run_network_tasks(100).unwrap();
        b.run_streams().unwrap();
        b.run_scheduled_tasks(100).unwrap();
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn cases() -> Vec<(String, String)> {
    let mut v: Vec<(String, Vec<&str>)> = Vec::new();
    for l in CASES.lines() {
        if let Some(n) = l.strip_prefix("=== ") {
            v.push((n.trim().to_string(), Vec::new()));
        } else if let Some(c) = v.last_mut() {
            c.1.push(l);
        }
    }
    v.into_iter().map(|(n, l)| (n, l.join("\n").trim_end_matches('\n').to_string())).collect()
}

#[test]
fn every_recorded_chrome_case_gives_the_same_result() {
    big(|| {
        let srv = server();
        let expected: std::collections::HashMap<&str, &str> = EXPECTED.lines().filter_map(|l| l.split_once('\t')).collect();
        let cs = cases();
        assert_eq!(cs.len(), expected.len());
        let mut bad = Vec::new();
        for (name, src) in &cs {
            let mut b = browser(srv.port);
            let r = js(&mut b, &format!("{PRELUDE}\n{src}"));
            let got = if r.starts_with("throw") || r.starts_with("unsupported") || r.starts_with("limit") || r.starts_with("other") {
                format!("SCRIPT:{r}")
            } else {
                settle(&mut b);
                js(&mut b, "out.join('|')")
            };
            let got = esc(&got);
            if got != expected[name.as_str()] {
                bad.push(format!("{name}\n  want {}\n  got  {got}", expected[name.as_str()]));
            }
        }
        assert!(bad.is_empty(), "{} of {} differ:\n{}", bad.len(), cs.len(), bad.join("\n"));
    });
}

#[test]
fn the_globals_are_lazy_and_a_realm_that_never_reads_them_makes_no_object() {
    big(|| {
        let mut b = browser(1);
        let before = b.stats().wrappers;
        assert_eq!(js(&mut b, "['XMLHttpRequest','XMLHttpRequestEventTarget','XMLHttpRequestUpload','ProgressEvent'].every(function (n) { return Object.getOwnPropertyNames(window).indexOf(n) >= 0; })"), "true");
        assert_eq!(b.stats().wrappers, before);
        assert_eq!(js(&mut b, "var x = new XMLHttpRequest(); [x instanceof EventTarget, x.upload instanceof XMLHttpRequestEventTarget, new ProgressEvent('p', {loaded: 3, total: 4, lengthComputable: true}).total, String(x)].join()"), "true,true,4,[object XMLHttpRequest]");
        assert_eq!(js(&mut b, "try { new XMLHttpRequestUpload(); 1 } catch (e) { e.name }"), "TypeError");
    });
}

#[test]
fn what_is_not_supported_is_refused_before_anything_changes() {
    big(|| {
        let srv = server();
        let mut b = browser(srv.port);
        js(&mut b, "var x = new XMLHttpRequest();");
        for (src, what) in [
            ("x.open('GET', '/text', false)", "synchronous XMLHttpRequest (async = false)"),
            ("x.open('GET', '/text', true, 'u')", "XMLHttpRequest.open with a user name or a password"),
            ("x.responseType = 'document'", "XMLHttpRequest responseType document"),
        ] {
            assert_eq!(js(&mut b, src), format!("unsupported:{what}"), "{src}");
        }
        assert_eq!(js(&mut b, "x.readyState"), "0");
        assert!(js(&mut b, "x.open('FOO', '/echo'); x.send('a')").starts_with("unsupported:"));
        assert!(js(&mut b, "x = new XMLHttpRequest(); x.open('POST', '/echo'); x.send(document)").starts_with("unsupported:"));
        assert_eq!(b.pending_fetches(), 0);
        assert!(srv.recs().is_empty());
    });
}

#[test]
fn a_late_reply_is_a_timeout_and_a_collected_request_leaves_nothing() {
    big(|| {
        let srv = server();
        let mut b = browser(srv.port);
        let base = b.stats();
        js(&mut b, &format!("{PRELUDE}\nvar x = new XMLHttpRequest(); rec(x); done(x); x.timeout = 50; x.open('GET', '/slow'); x.send();"));
        settle(&mut b);
        assert_eq!(js(&mut b, "out.join('|')"), "readystatechange:1|loadstart(0/0)|readystatechange:4|timeout(0/0)|loadend(0/0)");
        js(&mut b, "x = null; out = [];");
        b.run_scheduled_tasks(100).unwrap();
        b.collect(&[]).unwrap();
        let s = b.stats();
        assert_eq!((s.wrappers, s.events, s.listeners), (base.wrappers, base.events, base.listeners));
        assert_eq!(b.pending_fetches(), 0);
    });
}

#[test]
fn many_sequential_requests_return_the_object_count_to_the_baseline() {
    big(|| {
        let srv = server();
        let mut b = browser(srv.port);
        js(&mut b, "XMLHttpRequest; var n = 0;");
        b.collect(&[]).unwrap();
        let base = b.stats();
        for _ in 0..50 {
            js(&mut b, "var x = new XMLHttpRequest(); x.onload = function () { n++; }; x.open('GET', '/text'); x.send(); x = null;");
            while b.pending_fetches() > 0 {
                b.run_network_tasks(10).unwrap();
            }
            b.drain().unwrap();
        }
        assert_eq!(js(&mut b, "n"), "50");
        b.collect(&[]).unwrap();
        let s = b.stats();
        assert_eq!((s.wrappers, s.events, s.listeners), (base.wrappers, base.events, base.listeners));
    });
}

#[test]
fn a_page_that_leaves_aborts_a_queued_request() {
    big(|| {
        let srv = server();
        let mut b = browser(srv.port);
        js(&mut b, &format!("{PRELUDE}\nvar x = new XMLHttpRequest(); rec(x); x.open('GET', '/text'); x.send();"));
        assert_eq!(b.pending_fetches(), 1);
        b.abort_fetches().unwrap();
        assert_eq!(b.pending_fetches(), 0);
        assert_eq!(js(&mut b, "out.join('|')"), "readystatechange:1|loadstart(0/0)|readystatechange:4|abort(0/0)|loadend(0/0)");
        assert!(srv.recs().is_empty());
    });
}

#[test]
fn each_method_runs_and_the_forbidden_ones_are_a_security_error() {
    big(|| {
        let srv = server();
        let mut b = browser(srv.port);
        assert_eq!(js(&mut b, "var out = []; var done = 0; for (var m of ['PUT', 'PATCH', 'DELETE', 'OPTIONS', 'HEAD', 'put']) { var x = new XMLHttpRequest(); x.open(m, '/echo'); x.onload = (function (x, m) { return function () { out.push(m + ':' + x.status + ':' + x.responseText.length); }; })(x, m); x.send('abc'); }"), "undefined");
        let mut n = 0;
        while b.pending_fetches() > 0 && n < 1000 {
            b.run_network_tasks(100).unwrap();
            n += 1;
        }
        b.drain().unwrap();
        let wire: Vec<String> = srv.recs().iter().map(|q| format!("{} {}", q.method, String::from_utf8_lossy(&q.body))).collect();
        assert_eq!(wire, ["PUT abc", "PATCH abc", "DELETE abc", "OPTIONS abc", "HEAD ", "PUT abc"]);
        for m in ["CONNECT", "trace", "Track"] {
            assert_eq!(js(&mut b, &format!("x = new XMLHttpRequest(); x.open('{m}', '/echo')")), format!("throw:SecurityError:Failed to execute 'open' on 'XMLHttpRequest': '{m}' HTTP method is unsupported."));
        }
    });
}

/// A `blob:` URL of the Blob URL store is read by `XMLHttpRequest` with no network (W3C File API).
#[test]
fn an_xhr_reads_a_blob_url_without_the_network() {
    big(|| {
        let srv = server();
        let mut b = browser(srv.port);
        js(&mut b, "var out = [], finished = false; var u = URL.createObjectURL(new Blob(['h\\u00e9llo'], { type: 'text/plain;charset=utf-8' }));");
        assert!(js(&mut b, "u").starts_with("blob:"));
        js(&mut b, "var x = new XMLHttpRequest(); x.open('GET', u); x.onload = function () { out.push(x.status + '|' + x.responseText + '|' + x.getResponseHeader('content-type') + '|' + x.getResponseHeader('content-length')); finished = true; }; x.onerror = function () { out.push('error'); finished = true; }; x.send(); out.push('sent');");
        settle(&mut b);
        assert_eq!(js(&mut b, "out.join(',')"), "sent,200|h\u{e9}llo|text/plain;charset=utf-8|6");
        js(&mut b, "out.length = 0; finished = false; var y = new XMLHttpRequest(); y.open('GET', u); y.responseType = 'blob'; y.onload = function () { out.push(y.response.size + y.response.type); var z = new XMLHttpRequest(); z.open('GET', u); z.responseType = 'arraybuffer'; z.onload = function () { out.push(z.response.byteLength); finished = true; }; z.send(); }; y.send();");
        settle(&mut b);
        assert_eq!(js(&mut b, "out.join(',')"), "6text/plain,6");
        js(&mut b, "out.length = 0; finished = false; URL.revokeObjectURL(u); var w = new XMLHttpRequest(); w.open('GET', u); w.onerror = function () { out.push('error:' + w.status + w.readyState); finished = true; }; w.onload = function () { out.push('load'); finished = true; }; w.send();");
        settle(&mut b);
        assert_eq!(js(&mut b, "out.join(',')"), "error:04");
        // Only GET reads a Blob URL.
        js(&mut b, "out.length = 0; finished = false; var u3 = URL.createObjectURL(new Blob(['x'])); var q = new XMLHttpRequest(); q.open('POST', u3); q.onerror = function () { out.push('post-error'); finished = true; }; q.send('b');");
        settle(&mut b);
        assert_eq!(js(&mut b, "out.join(',')"), "post-error");
        assert_eq!(srv.recs().len(), 0, "no request reached the server");
    });
}
