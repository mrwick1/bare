//! Tests of the navigation `fetch` event (`browser_session.rs` `sw_document`, step 8 of `docs/internal/design-service-workers.md`):
//! a navigation in the scope of a registration whose worker has a `fetch` listener goes to the worker first. `respondWith` gives the
//! document; no reply goes to the network. Chrome is the reference. Time is the virtual clock.

use crate::browser_session::*;
use crate::js_limits::EvalLimits;
use crate::resources::test_server::{Reply, Server, serve};
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const LOG: &str = "function log(x){localStorage.setItem('log',(localStorage.getItem('log')||'')+x+',')}";

/// `/` registers `/sw.js`, which claims it, and goes to `/next` when it is controlled.
fn server(sw: &'static str) -> Server {
    let page = format!("<script>{LOG} if (!navigator.serviceWorker.controller) {{ navigator.serviceWorker.oncontrollerchange = function(){{ location = '/next' }}; navigator.serviceWorker.register('/sw.js') }} else log('controlled /');</script>");
    let next = format!("<script>{LOG} log('network /next ' + !!navigator.serviceWorker.controller)</script>");
    serve(move |path| match path {
        "/" => Reply::new(200, Some("text/html"), page.clone()),
        "/next" => Reply::new(200, Some("text/html"), next.clone()),
        "/sw.js" => Reply::js(sw),
        _ => Reply::new(404, Some("text/plain"), "no"),
    })
}

fn session(srv: &Server) -> Session {
    let mut c = SessionConfig::new(&srv.url("/"));
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
    c.page.quiet = Duration::from_millis(100);
    Session::new(c)
}

fn log(s: &Session, srv: &Server) -> Option<String> {
    s.storage().get_item(false, &srv.url(""), "log")
}

/// A worker that answers each navigation with a page that logs what the worker saw.
const ANSWER: &str = "self.onactivate = function(e){ e.waitUntil(clients.claim()) }; self.onfetch = function(e){ var q = e.request; if (q.mode !== 'navigate') return; e.respondWith(new Response(\"<script>function log(x){localStorage.setItem('log',(localStorage.getItem('log')||'')+x+',')} log('worker ' + location.pathname + ' ' + !!navigator.serviceWorker.controller + ' \" + [q.method, q.mode, q.redirect, JSON.stringify(e.clientId), q.headers.get('accept').split(',')[0]].join('|') + \"')</script>\", {headers: {'content-type': 'text/html'}})) };";

#[test]
fn the_worker_answers_a_navigation() {
    big(|| {
        let srv = server(ANSWER);
        let mut s = session(&srv);
        let r = s.run();
        assert_eq!(log(&s, &srv).as_deref(), Some("worker /next true GET|navigate|manual|\"\"|text/html,"), "{r:?}");
    });
}

#[test]
fn no_reply_goes_to_the_network() {
    big(|| {
        let srv = server("self.onactivate = function(e){ e.waitUntil(clients.claim()) }; self.onfetch = function(e){};");
        let mut s = session(&srv);
        let r = s.run();
        assert_eq!(log(&s, &srv).as_deref(), Some("network /next true,"), "{r:?}");
    });
}

#[test]
fn a_rejected_reply_fails_the_navigation() {
    big(|| {
        let srv = server("self.onactivate = function(e){ e.waitUntil(clients.claim()) }; self.onfetch = function(e){ e.respondWith(Promise.reject(new Error('x'))) };");
        let mut s = session(&srv);
        let r = s.run();
        assert!(matches!(&r.stop, SessionStop::LoadFailed(w) if w == "network error"), "{r:?}");
        assert_eq!(log(&s, &srv), None);
    });
}

#[test]
fn the_first_load_of_a_later_run_goes_to_the_worker() {
    big(|| {
        let srv = server(ANSWER);
        let mut s = session(&srv);
        s.run();
        s.storage().set_item(false, &srv.url(""), "log", "").unwrap();
        let r = s.run();
        assert_eq!(log(&s, &srv).as_deref(), Some("worker / true GET|navigate|manual|\"\"|text/html,"), "{r:?}");
    });
}
