//! Tests of `WebSocket` (`shims/websocket.js`): the connection always fails, as against a server that refuses it.

use crate::browser_fetch_tests::{reply, serve};
use crate::browser_xhr_tests::{browser, js, settle};

fn run(src: &str) -> String {
    let srv = serve(|_| reply(404, &[], b""));
    let mut b = browser(srv.port);
    b.set_document_url(&format!("http://127.0.0.1:{}/page.html", srv.port)).unwrap();
    let r = js(&mut b, &format!("var out = [], finished = false; {src}"));
    assert!(!["throw", "other", "unsupp", "limit"].iter().any(|p| r.starts_with(p)), "{r}");
    settle(&mut b);
    js(&mut b, "out.join('|')").replace(&srv.port.to_string(), "P")
}

#[test]
fn it_exists() {
    assert_eq!(run("out.push(typeof WebSocket, WebSocket.CLOSED, typeof CloseEvent); finished = true"), "function|3|function");
}

#[test]
fn a_connection_fails_with_error_then_close() {
    let got = run("var ws = new WebSocket('/ws'); out.push(ws.url, ws.readyState);
        ws.onerror = function (e) { out.push(e.type, ws.readyState); };
        ws.addEventListener('close', function (e) { out.push(e.type, e instanceof CloseEvent, e.code, e.wasClean, ws.readyState); finished = true; });");
    assert_eq!(got, "ws://127.0.0.1:P/ws|0|error|3|close|true|1006|false|3");
}

#[test]
fn bad_arguments_throw_as_in_chrome() {
    let got = run("['ws://a/#x', 'ftp://a/', 'http://'].forEach(function (u) { try { new WebSocket(u); } catch (e) { out.push(e.name); } });
        var ws = new WebSocket('wss://a/');
        try { ws.send('x'); } catch (e) { out.push(e.name); }
        try { ws.close(999); } catch (e) { out.push(e.name); }
        ws.close(); out.push(ws.readyState); finished = true");
    assert_eq!(got, "SyntaxError|SyntaxError|SyntaxError|InvalidStateError|InvalidAccessError|2");
}
