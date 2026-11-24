//! Tests of dedicated workers that run (`browser_worker.rs`, `browser_child.rs`, step 1 of W1 in `docs/internal/design-workers.md`).
//! Each page loads over loopback HTTP from a throwaway server of this process. A path that ends in `.js` is served as JavaScript.
//! The page writes what it sees to `localStorage`, and the test reads it through the session storage. Time is the virtual clock.

use crate::browser_loader::*;
use crate::browser_session::*;
use crate::resources::test_server::{Reply, Server, serve};
use crate::js_limits::EvalLimits;
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn server(routes: Vec<(&'static str, &'static str)>) -> Server {
    serve(move |path| match routes.iter().find(|r| r.0 == path) {
        Some((p, body)) if p.ends_with(".js") => Reply::new(200, Some("text/javascript"), body.to_string()),
        Some((_, body)) => Reply::new(200, Some("text/html; charset=utf-8"), body.to_string()),
        None => Reply::new(404, Some("text/plain"), "no"),
    })
}

/// Run the page `/` of `srv` and give the value of `key` in its `localStorage`.
fn run(srv: &Server, key: &str) -> (Option<String>, SessionResult) {
    let mut c = SessionConfig::new(&srv.url("/"));
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
    c.page.quiet = Duration::from_millis(100);
    let mut s = Session::new(c);
    let r = s.run();
    (s.storage().get_item(false, &srv.url(""), key), r)
}

const LOG: &str = "function log(x){localStorage.setItem('log',(localStorage.getItem('log')||'')+x+',')}";

fn page(script: &str) -> &'static str {
    Box::leak(format!("<script>{LOG}{script}</script>").into_boxed_str())
}

#[test]
fn a_same_origin_worker_answers_a_message_with_no_origin_and_no_source() {
    big(|| {
        let srv = server(vec![
            ("/", page("var w = new Worker('/w.js'); w.onmessage = function(e){ log([e.data.n, JSON.stringify(e.origin), String(e.source)].join('|')) }; w.postMessage({n: 1});")),
            ("/w.js", "onmessage = function(e){ postMessage({n: e.data.n + 1}) };"),
        ]);
        let (log, r) = run(&srv, "log");
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(log.as_deref(), Some("2|\"\"|null,"));
        assert_eq!(srv.hits("/w.js"), 1);
    });
}

#[test]
fn a_data_url_worker_runs_and_posts_first() {
    big(|| {
        let srv = server(vec![("/", page("var w = new Worker('data:text/javascript,postMessage(\"hi\")'); w.addEventListener('message', function(e){ log(e.data) });"))]);
        let (log, r) = run(&srv, "log");
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(log.as_deref(), Some("hi,"), "{r:?}");
    });
}

#[test]
fn messages_keep_their_order_both_ways() {
    big(|| {
        let srv = server(vec![
            ("/", page("var w = new Worker('/w.js'); w.onmessage = function(e){ log(e.data) }; for (var i = 0; i < 3; i++) w.postMessage(i);")),
            ("/w.js", "self.addEventListener('message', function(e){ postMessage('a' + e.data); postMessage('b' + e.data) });"),
        ]);
        let (log, _) = run(&srv, "log");
        assert_eq!(log.as_deref(), Some("a0,b0,a1,b1,a2,b2,"));
    });
}

#[test]
fn a_worker_script_that_does_not_load_fires_error_and_the_page_settles() {
    big(|| {
        let srv = server(vec![("/", page("var w = new Worker('/missing.js'); w.onerror = function(e){ log('error:' + e.type) }; w.onmessage = function(){ log('message') };"))]);
        let (log, r) = run(&srv, "log");
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(log.as_deref(), Some("error:error,"));
    });
}

#[test]
fn terminate_stops_the_worker_and_its_timers() {
    big(|| {
        let srv = server(vec![
            ("/", page("var n = 0; var w = new Worker('/w.js'); w.onmessage = function(e){ log(e.data); if (++n == 2) w.terminate() };")),
            ("/w.js", "var i = 0; setInterval(function(){ postMessage(i++) }, 10);"),
        ]);
        let (log, r) = run(&srv, "log");
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(log.as_deref(), Some("0,1,"));
    });
}

#[test]
fn close_in_the_worker_delivers_what_it_posted_before_and_ends_it() {
    big(|| {
        let srv = server(vec![
            ("/", page("var w = new Worker('/w.js'); w.onmessage = function(e){ log(e.data) }; setTimeout(function(){ w.postMessage('late') }, 50);")),
            ("/w.js", "postMessage('first'); close(); onmessage = function(e){ postMessage('echo ' + e.data) };"),
        ]);
        let (log, r) = run(&srv, "log");
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(log.as_deref(), Some("first,"));
    });
}

#[test]
fn a_function_in_a_message_throws_data_clone_error_in_the_sender() {
    big(|| {
        let srv = server(vec![
            ("/", page("var w = new Worker('/w.js'); try { w.postMessage({f: function(){}}) } catch (e) { log(e.name) }")),
            ("/w.js", ""),
        ]);
        let (log, _) = run(&srv, "log");
        assert_eq!(log.as_deref(), Some("DataCloneError,"));
    });
}
