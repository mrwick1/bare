//! Tests of `importScripts` in a worker page and of the console of a worker (`browser_worker.rs`, step 2 of W1 in
//! `docs/internal/design-workers.md`). Each page loads over loopback HTTP from a throwaway server of this process. A path that ends in
//! `.js` is served as JavaScript, `/html.js` as HTML. The worker posts what it sees; the page writes it to `localStorage`.

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
        Some((p, body)) if p.ends_with(".js") && *p != "/html.js" => Reply::new(200, Some("text/javascript"), body.to_string()),
        Some((_, body)) => Reply::new(200, Some("text/html; charset=utf-8"), body.to_string()),
        None => Reply::new(404, Some("text/plain"), "no"),
    })
}

/// The page `/` makes the worker `w` and logs each message from it.
const PAGE: &str = "<script>var w = new Worker('/w/w.js'); w.onmessage = function(e){ localStorage.setItem('log', (localStorage.getItem('log')||'') + e.data + ',') };</script>";

fn run(srv: &Server) -> (Session, Option<String>, SessionResult) {
    let mut c = SessionConfig::new(&srv.url("/"));
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
    c.page.quiet = Duration::from_millis(100);
    let mut s = Session::new(c);
    let r = s.run();
    let log = s.storage().get_item(false, &srv.url(""), "log");
    (s, log, r)
}

#[test]
fn import_scripts_runs_each_url_in_order_against_the_worker_url() {
    big(|| {
        let srv = server(vec![
            ("/", PAGE),
            ("/w/w.js", "var order = []; importScripts('a.js', '/w/b.js'); postMessage(order.join('') + add(1, 2) + K);"),
            ("/w/a.js", "order.push('a'); function add(x, y){ return x + y } const K = 'k';"),
            ("/w/b.js", "order.push('b');"),
        ]);
        let (mut s, log, r) = run(&srv);
        let rep: Vec<String> = s.browser().unwrap().take_reports().into_iter().map(|r| r.message).collect();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(log.as_deref(), Some("ab3k,"), "{rep:?} {:?}", r.pages[0].diagnostics);
        assert_eq!((srv.hits("/w/a.js"), srv.hits("/w/b.js")), (1, 1));
    });
}

#[test]
fn a_script_that_does_not_load_is_a_network_error_and_the_later_urls_do_not_load() {
    big(|| {
        let srv = server(vec![
            ("/", PAGE),
            ("/w/w.js", "for (const u of ['missing.js', '/html.js']) { try { importScripts(u, 'b.js') } catch (e) { postMessage(e.name) } }"),
            ("/html.js", "var x = 1;"),
            ("/w/b.js", "postMessage('b ran')"),
        ]);
        let (_, log, _) = run(&srv);
        assert_eq!(log.as_deref(), Some("NetworkError,NetworkError,"));
        assert_eq!(srv.hits("/w/b.js"), 0);
    });
}

#[test]
fn a_bad_url_is_a_syntax_error_before_any_download() {
    big(|| {
        let srv = server(vec![
            ("/", PAGE),
            ("/w/w.js", "try { importScripts('a.js', 'http://[') } catch (e) { postMessage(e.name) }"),
            ("/w/a.js", "postMessage('a ran')"),
        ]);
        let (_, log, _) = run(&srv);
        assert_eq!(log.as_deref(), Some("SyntaxError,"));
        assert_eq!(srv.hits("/w/a.js"), 0);
    });
}

#[test]
fn a_data_url_imports_and_a_throw_reaches_the_caller() {
    big(|| {
        let srv = server(vec![
            ("/", PAGE),
            ("/w/w.js", "importScripts('data:text/javascript,var fromData %3D 7'); try { importScripts('t.js') } catch (e) { postMessage(fromData + ':' + e.message) }"),
            ("/w/t.js", "throw new Error('inside')"),
        ]);
        let (_, log, _) = run(&srv);
        assert_eq!(log.as_deref(), Some("7:inside,"));
    });
}

#[test]
fn the_console_and_an_uncaught_error_of_a_worker_reach_the_page_with_the_worker_url() {
    big(|| {
        let srv = server(vec![("/", PAGE), ("/w/w.js", "console.log('hello from worker'); throw new Error('uncaught in worker')")]);
        let (mut s, _, r) = run(&srv);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        let b = s.browser().unwrap();
        let lines: Vec<String> = b.take_console_records().iter().map(|r| b.format_console_record(r).unwrap()).collect();
        let label = srv.url("/w/w.js");
        assert!(lines.contains(&format!("{label}: hello from worker")), "{lines:?}");
        let reports: Vec<String> = b.take_reports().into_iter().map(|r| r.message).collect();
        assert!(reports.iter().any(|m| m.starts_with(&label) && m.contains("uncaught in worker")), "{reports:?}");
    });
}
