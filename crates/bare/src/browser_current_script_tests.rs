//! `document.currentScript` through the microtask checkpoint (HTML Standard, "execute the script element" and "clean up after
//! running script"): promise jobs that a classic script queued see the script element. A module script, a timer and a
//! later task see `null`.

use crate::browser::*;
use crate::browser_fetch::{FetchConfig, FetchLimits};
use crate::browser_loader::*;
use crate::resources::test_server::{serve, Reply};
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_net::http::Client;
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// Load `html` (with `/e.js`, `/m.js` and `/d.js` served) and give `log.join('|')`.
fn log_of(html: &str) -> String {
    let html = html.to_string();
    big(move || {
        let srv = serve(|path| match path {
            "/e.js" => Reply::new(200, Some("application/javascript"), "Promise.resolve().then(function () { log.push('ext:' + (document.currentScript && document.currentScript.id)) })"),
            "/m.js" => Reply::new(200, Some("application/javascript"), "Promise.resolve().then(function () { log.push('mod:' + document.currentScript) })"),
            "/d.js" => Reply::new(200, Some("application/javascript"), "Promise.resolve().then(function () { log.push('dyn:' + (document.currentScript && document.currentScript.id)) })"),
            _ => Reply::new(404, Some("text/plain"), "no"),
        });
        let page = format!("<script>var log = []</script>{html}");
        let mut b = Browser::new(tree::parse(&page), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).unwrap();
        b.use_virtual_clock();
        b.configure_fetch(FetchConfig { base_url: Some(srv.url("/page.html")), client: Client::new(), limits: FetchLimits::default() }).unwrap();
        let limits = PageLimits { quiet: Duration::from_millis(100), ..Default::default() };
        b.run_page(PageConfig { url: srv.url("/page.html"), client: Client::new(), limits, fetch: None });
        let v = b.run_script("log.join('|')").unwrap();
        b.it.display(v)
    })
}

#[test]
fn a_promise_job_of_an_inline_script_sees_the_script() {
    assert_eq!(log_of("<script id=a>Promise.resolve().then(function () { log.push('p:' + document.currentScript.id) })</script>"), "p:a");
}

#[test]
fn the_job_runs_before_the_next_script_and_a_throwing_script_keeps_it_too() {
    let html = "<script id=a>Promise.resolve().then(function () { log.push('a:' + document.currentScript.id) }); throw new Error('x')</script>\
                <script id=b>log.push('b:' + document.currentScript.id)</script>";
    assert_eq!(log_of(html), "a:a|b:b");
}

#[test]
fn a_promise_job_of_external_and_dynamic_scripts_sees_the_script() {
    let html = "<script id=e src='/e.js'></script>\
                <script id=h>var s = document.createElement('script'); s.id = 'd'; s.src = '/d.js'; document.body.appendChild(s);\
                var i = document.createElement('script'); i.id = 'i'; i.text = \"Promise.resolve().then(function () { log.push('inl:' + document.currentScript.id) })\"; document.body.appendChild(i)</script>\
                <script id=df defer src='/e.js'></script>";
    // A dynamic inline script runs inside `appendChild` in a browser, so its jobs run at the checkpoint of the host script `h`.
    assert_eq!(log_of(html), "ext:e|inl:h|ext:df|dyn:d");
}

#[test]
fn a_promise_job_of_an_async_script_sees_the_script() {
    assert_eq!(log_of("<script id=y async src='/e.js'></script>"), "ext:y");
}

#[test]
fn a_module_script_and_a_timer_see_null() {
    let html = "<script type=module id=m>Promise.resolve().then(function () { log.push('mod:' + document.currentScript) })</script>\
                <script type=module src='/m.js'></script>\
                <script id=t>setTimeout(function () { log.push('timer:' + document.currentScript) }, 0)</script>";
    assert_eq!(log_of(html), "mod:null|mod:null|timer:null");
}
