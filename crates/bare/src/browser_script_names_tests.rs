//! Tests of the script name in stack frames. Chrome is the reference: a frame of an external script shows the script URL, a frame of an
//! inline script shows the document URL, a frame of a script that `importScripts` loads in a worker shows its URL, and a frame of
//! `eval` code shows `<anonymous>`.

use crate::browser_session::*;
use crate::js_limits::EvalLimits;
use crate::resources::test_server::{Reply, Server, serve};
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn server(routes: Vec<(&'static str, &'static str)>) -> Server {
    serve(move |path| match routes.iter().find(|r| r.0 == path.split('?').next().unwrap_or(path)) {
        Some((p, body)) if p.ends_with(".js") => Reply::new(200, Some("text/javascript"), body.to_string()),
        Some((_, body)) => Reply::new(200, Some("text/html; charset=utf-8"), body.to_string()),
        None => Reply::new(404, Some("text/plain"), "no"),
    })
}

/// Run the page `/` of `srv` and give its `log` item in `localStorage`, with the server URL replaced by `U`.
fn run(srv: &Server) -> String {
    let mut c = SessionConfig::new(&srv.url("/"));
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
    c.page.quiet = Duration::from_millis(100);
    let mut s = Session::new(c);
    let r = s.run();
    let log = s.storage().get_item(false, &srv.url(""), "log").unwrap_or_else(|| panic!("no log: {r:?}"));
    log.replace(srv.url("").trim_end_matches('/'), "U")
}

const LOG: &str = "function log(x){localStorage.setItem('log',(localStorage.getItem('log')||'')+x+'|')}";

#[test]
fn a_frame_of_an_external_script_shows_its_url() {
    big(|| {
        let srv = server(vec![
            ("/", Box::leak(format!("<script>{LOG}</script><script src='/a.js?v=1'></script>").into_boxed_str())),
            ("/a.js", "function f() {\n  return new Error('x').stack.split('\\n')[1].trim();\n}\nlog(f()); try { null.x } catch (e) { log(e.stack.split('\\n')[1].trim()) }"),
        ]);
        assert_eq!(run(&srv), "at f (U/a.js?v=1:2:10)|at U/a.js?v=1:4:17|");
    });
}

#[test]
fn a_frame_of_an_inline_script_shows_the_document_url_and_eval_shows_anonymous() {
    big(|| {
        let srv = server(vec![
            ("/", Box::leak(format!("<script>{LOG}function g(){{ return new Error('x').stack.split('\\n')[1].trim() }} log(g()); log(eval('new Error(1).stack.split(\"\\\\n\")[1].trim()'))</script>").into_boxed_str())),
        ]);
        let got = run(&srv);
        assert!(got.starts_with("at g (U/:1:"), "{got}");
        assert!(got.contains("|at <anonymous>:1:") || got.contains("|at eval (") , "{got}");
    });
}

#[test]
fn a_frame_of_an_imported_script_in_a_worker_shows_its_url() {
    big(|| {
        let srv = server(vec![
            ("/", Box::leak(format!("<script>{LOG}var w = new Worker('/w.js'); w.onmessage = function(e){{ log(e.data) }};</script>").into_boxed_str())),
            ("/w.js", "importScripts('/lib.js');\ntry { boom() } catch (e) { postMessage(e.stack.split('\\n').slice(1, 3).map(function(s){ return s.trim() }).join(' ; ')) }"),
            ("/lib.js", "function boom() {\n  return undefined.operation;\n}"),
        ]);
        let got = run(&srv);
        assert!(got.starts_with("at boom (U/lib.js:2:"), "{got}");
        assert!(got.contains(" ; at U/w.js:2:"), "{got}");
    });
}

#[test]
fn the_same_text_from_two_urls_keeps_each_url() {
    big(|| {
        // The code cache shares the compiled text; the name belongs to each load.
        const SAME: &str = "(function h() { log(new Error('x').stack.split('\\n')[1].trim()) })();";
        let srv = server(vec![
            ("/", Box::leak(format!("<script>{LOG}</script><script src='/a.js'></script><script src='/b.js'></script>").into_boxed_str())),
            ("/a.js", SAME),
            ("/b.js", SAME),
        ]);
        assert_eq!(run(&srv), "at h (U/a.js:1:21)|at h (U/b.js:1:21)|");
    });
}

#[test]
fn a_frame_of_a_module_script_shows_its_url() {
    big(|| {
        let srv = server(vec![
            ("/", Box::leak(format!("<script>{LOG}</script><script type='module' src='/m.js'></script>").into_boxed_str())),
            ("/m.js", "function k() { return new Error('x').stack.split('\\n')[1].trim() }\nlog(k());"),
        ]);
        assert_eq!(run(&srv), "at k (U/m.js:1:23)|");
    });
}
