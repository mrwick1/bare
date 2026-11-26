//! Tests of the `fetch` event (`browser_sw.rs`, `sw_hub.rs`, step 7 of `docs/internal/design-service-workers.md`): a `fetch()` of a
//! controlled page goes to its service worker; `respondWith` gives the response, no reply goes to the network, a rejected reply
//! fails the fetch. A worker with no `fetch` listener gets no event. Chrome is the reference. Time is the virtual clock.

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
        "/data" => Reply::new(200, Some("text/plain"), "from the network"),
        _ => Reply::new(404, Some("text/plain"), "no"),
    })
}

fn run(srv: &Server) -> (Option<String>, SessionResult) {
    let mut c = SessionConfig::new(&srv.url("/"));
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
    c.page.quiet = Duration::from_millis(100);
    let mut s = Session::new(c);
    let r = s.run();
    (s.storage().get_item(false, &srv.url(""), "log"), r)
}

/// The page registers `/sw.js` (which claims it in `activate`) and runs `then` when it is controlled.
fn controlled(then: &str) -> String {
    format!("navigator.serviceWorker.oncontrollerchange = function(){{ {then} }}; navigator.serviceWorker.register('/sw.js');")
}

const CLAIM: &str = "self.onactivate = function(e){ e.waitUntil(clients.claim()) };";

fn text(log: &str) -> String {
    format!("function show(p){{ return p.then(function(r){{ return r.text().then(function(t){{ return [t, r.status, r.statusText, r.headers.get('x-a'), r.url === new URL('/data', location).href].join('|') }}) }}, function(x){{ return x.name + ': ' + x.message }}) }} {log}")
}

#[test]
fn respond_with_gives_the_response_to_the_page() {
    big(|| {
        let sw: &'static str = Box::leak(format!("{CLAIM} self.onfetch = function(e){{ if (e.request.url.endsWith('/data')) e.respondWith(new Response('from the worker', {{status: 201, statusText: 'Made', headers: {{'x-a': '1'}}}})) }};").into_boxed_str());
        let srv = server(&controlled(&text("show(fetch('/data')).then(log);")), sw);
        let (log, r) = run(&srv);
        assert_eq!(log.as_deref(), Some("from the worker|201|Made|1|true,"), "{r:?}");
    });
}

#[test]
fn no_reply_goes_to_the_network_and_a_worker_fetch_is_not_intercepted() {
    big(|| {
        let sw: &'static str = Box::leak(format!("{CLAIM} self.onfetch = function(e){{ if (e.request.url.endsWith('/net')) e.respondWith(fetch('/data')) }};").into_boxed_str());
        let srv = server(&controlled(&text("show(fetch('/data')).then(log).then(function(){ return show(fetch('/net')) }).then(log);")), sw);
        let (log, r) = run(&srv);
        assert_eq!(log.as_deref(), Some("from the network|200|||true,from the network|200|||true,"), "{r:?}");
    });
}

#[test]
fn the_request_crosses_with_its_method_headers_body_and_client() {
    big(|| {
        let sw: &'static str = Box::leak(format!("{CLAIM} self.onfetch = function(e){{ var q = e.request; e.respondWith(Promise.all([q.text(), clients.get(e.clientId)]).then(function(l){{ return new Response([q.method, q.headers.get('x-q'), l[0], l[1] && l[1].url === new URL('/', location).href, e.resultingClientId, e instanceof FetchEvent].join('|')) }})) }};").into_boxed_str());
        let srv = server(&controlled("fetch('/data', {method: 'POST', headers: {'x-q': 'v'}, body: 'abc'}).then(function(r){ return r.text() }).then(log);"), sw);
        let (log, r) = run(&srv);
        assert_eq!(log.as_deref(), Some("POST|v|abc|true||true,"), "{r:?}");
    });
}

#[test]
fn a_rejected_or_wrong_reply_fails_the_fetch() {
    big(|| {
        let sw: &'static str = Box::leak(format!("{CLAIM} self.onfetch = function(e){{ e.respondWith(e.request.url.endsWith('/a') ? Promise.reject(new Error('x')) : 'not a response') }};").into_boxed_str());
        let srv = server(&controlled(&text("show(fetch('/a')).then(log).then(function(){ return show(fetch('/b')) }).then(log);")), sw);
        let (log, r) = run(&srv);
        assert_eq!(log.as_deref(), Some("TypeError: Failed to fetch,TypeError: Failed to fetch,"), "{r:?}");
    });
}

#[test]
fn respond_with_throws_as_chrome_does() {
    big(|| {
        let sw: &'static str = Box::leak(format!("{CLAIM} self.onfetch = function(e){{ var out = []; e.respondWith(new Promise(function(ok){{ setTimeout(function(){{ try {{ e.respondWith(new Response('x')) }} catch (x) {{ out.push(x.name + ': ' + x.message) }} ok(new Response(out.join('|'))) }}, 0) }})); try {{ e.respondWith(new Response('y')) }} catch (x) {{ out.push(x.name + ': ' + x.message) }} }};").into_boxed_str());
        let srv = server(&controlled("fetch('/data').then(function(r){ return r.text() }).then(log);"), sw);
        let (log, r) = run(&srv);
        assert_eq!(
            log.as_deref(),
            Some("InvalidStateError: Failed to execute 'respondWith' on 'FetchEvent': The event has already been responded to.|InvalidStateError: Failed to execute 'respondWith' on 'FetchEvent': The event has already been responded to.,"),
            "{r:?}"
        );
    });
}

#[test]
fn a_worker_with_no_fetch_listener_gets_no_fetch_event() {
    big(|| {
        let srv = server(
            &controlled("fetch('/data').then(function(r){ return r.text() }).then(function(t){ log(t); navigator.serviceWorker.onmessage = function(e){ log(e.data) }; navigator.serviceWorker.controller.postMessage(0) });"),
            "self.onactivate = function(e){ e.waitUntil(clients.claim()) }; var n = 0, d = self.dispatchEvent; self.dispatchEvent = function(e){ if (e.type === 'fetch') n++; return d.call(self, e) }; self.onmessage = function(e){ e.source.postMessage('fetch events: ' + n) };",
        );
        let (log, r) = run(&srv);
        assert_eq!(log.as_deref(), Some("from the network,fetch events: 0,"), "{r:?}");
    });
}

#[test]
fn a_late_respond_with_throws_and_the_fetch_goes_to_the_network() {
    big(|| {
        let sw: &'static str = Box::leak(format!("{CLAIM} self.onfetch = function(e){{ e.waitUntil(new Promise(function(ok){{ setTimeout(function(){{ try {{ e.respondWith(new Response('z')) }} catch (x) {{ clients.get(e.clientId).then(function(c){{ c.postMessage(x.name + ': ' + x.message) }}) }} ok() }}, 0) }})) }};").into_boxed_str());
        let srv = server(&controlled("navigator.serviceWorker.onmessage = function(e){ log(e.data) }; fetch('/data').then(function(r){ return r.text() }).then(log);"), sw);
        let (log, r) = run(&srv);
        let log = log.unwrap_or_default();
        assert!(log.contains("from the network,"), "{log} {r:?}");
        assert!(log.contains("InvalidStateError: Failed to execute 'respondWith' on 'FetchEvent': The event handler is already finished.,"), "{log} {r:?}");
    });
}
