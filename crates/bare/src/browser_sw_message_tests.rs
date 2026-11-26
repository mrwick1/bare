//! Tests of the messages between a page and its service worker (`browser_sw.rs`, step 4 of
//! `docs/internal/design-service-workers.md`): `ServiceWorker.postMessage`, `ExtendableMessageEvent`, `Client.postMessage`,
//! `clients.matchAll`, `clients.get`, a `MessagePort` in the transfer list, and a start on a message. Time is the virtual clock.

use crate::browser_session::*;
use crate::js_limits::EvalLimits;
use crate::resources::test_server::{Reply, Server, serve};
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const LOG: &str = "function log(x){localStorage.setItem('log',(localStorage.getItem('log')||'')+x+',')}";

fn server(page: &str, sw: &'static str) -> Server {
    let page = format!("<script>{LOG}{page}</script>");
    serve(move |path| match path {
        "/" => Reply::new(200, Some("text/html"), page.clone()),
        "/sw.js" => Reply::js(sw),
        _ => Reply::new(404, Some("text/plain"), "no"),
    })
}

fn run(srv: &Server, quiet: Duration) -> (Option<String>, SessionResult) {
    let mut c = SessionConfig::new(&srv.url("/"));
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
    c.page.quiet = quiet;
    let mut s = Session::new(c);
    let r = s.run();
    (s.storage().get_item(false, &srv.url(""), "log"), r)
}

/// The page registers `/sw.js`, waits for `ready`, and then runs `then` with `w`, the active worker.
fn when_ready(then: &str) -> String {
    format!("navigator.serviceWorker.register('/sw.js'); navigator.serviceWorker.ready.then(function(r){{ var w = r.active; {then} }});")
}

#[test]
fn a_message_goes_to_the_worker_and_the_reply_comes_back_with_its_source() {
    big(|| {
        let srv = server(
            &when_ready("var c = navigator.serviceWorker; c.onmessage = function(e){ log([e.data.n, e.data.type, e.data.frame, e.data.same, e.source === w, e.origin === location.origin, e.constructor.name].join('|')) }; w.postMessage({n: 1});"),
            "self.onmessage = function(e){ e.source.postMessage({n: e.data.n + 1, type: e.source.type, frame: e.source.frameType, same: e.origin === new URL(e.source.url).origin && e instanceof ExtendableMessageEvent && e.source instanceof WindowClient}) };",
        );
        let (log, r) = run(&srv, Duration::from_millis(100));
        assert_eq!(log.as_deref(), Some("2|window|top-level|true|true|true|MessageEvent,"), "{r:?}");
    });
}

#[test]
fn a_message_port_crosses_to_the_worker() {
    big(|| {
        let srv = server(
            &when_ready("var ch = new MessageChannel(); ch.port1.onmessage = function(e){ log('port:' + e.data) }; w.postMessage('hi', [ch.port2]);"),
            "self.addEventListener('message', function(e){ e.ports[0].postMessage(e.data + '!') });",
        );
        let (log, r) = run(&srv, Duration::from_millis(100));
        assert_eq!(log.as_deref(), Some("port:hi!,"), "{r:?}");
    });
}

#[test]
fn match_all_and_get_find_the_page() {
    big(|| {
        let srv = server(
            &when_ready("navigator.serviceWorker.onmessage = function(e){ log(e.data) }; w.postMessage('go');"),
            "self.onactivate = function(e){ e.waitUntil(clients.claim()) }; self.onmessage = function(e){ e.waitUntil(clients.matchAll().then(function(l){ return clients.get(e.source.id).then(function(c){ l[0].postMessage([l.length, l[0].url === e.source.url, c.id === e.source.id, l[0].visibilityState].join('|')) }) })) };",
        );
        let (log, r) = run(&srv, Duration::from_millis(100));
        assert_eq!(log.as_deref(), Some("1|true|true|visible,"), "{r:?}");
    });
}

#[test]
fn a_message_starts_a_stopped_worker() {
    big(|| {
        // The worker stops after 30 s with no event. The message at 31 s starts it again; its script runs again first.
        let srv = server(
            &when_ready("navigator.serviceWorker.onmessage = function(e){ log(e.data) }; w.postMessage('a'); setTimeout(function(){ w.postMessage('b') }, 31000);"),
            "var starts = (self.starts || 0) + 1; self.onmessage = function(e){ e.source.postMessage(e.data + starts) };",
        );
        let (log, r) = run(&srv, Duration::from_secs(40));
        assert_eq!(log.as_deref(), Some("a1,b1,"), "{r:?}");
    });
}

#[test]
fn post_message_needs_an_argument() {
    big(|| {
        let srv = server(&when_ready("try { w.postMessage() } catch (e) { log(e.name + ': ' + e.message) }"), "");
        let (log, r) = run(&srv, Duration::from_millis(100));
        assert_eq!(log.as_deref(), Some("TypeError: Failed to execute 'postMessage' on 'ServiceWorker': 1 argument required, but only 0 present.,"), "{r:?}");
    });
}
