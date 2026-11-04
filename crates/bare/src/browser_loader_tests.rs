//! Tests of the page loader and driver (`browser_loader.rs`). Real scripts run in a real realm. The network is a loopback
//! HTTP server of this process (`resources::test_server`, throwaway synthetic bodies). Expectations are written by hand
//! from the HTML Standard where the loader follows it, and from the module comment where it deviates (pre-parsed DOM).
//! Each test runs on a big stack.

use crate::browser::*;
use crate::browser_fetch::{FetchConfig, FetchLimits};
use crate::browser_loader::*;
use crate::resources::test_server::{Reply, Server, serve};
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_net::http::Client;
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// `(path, status, content type, body, extra headers)`
type Route = (&'static str, u16, Option<&'static str>, String, Vec<(&'static str, &'static str)>);

fn js(path: &'static str, body: &str) -> Route {
    (path, 200, Some("application/javascript"), body.to_string(), vec![])
}

fn server(routes: Vec<Route>) -> Server {
    serve(move |path| match routes.iter().find(|r| r.0 == path) {
        Some((_, status, ct, body, headers)) => {
            let mut r = Reply::new(*status, *ct, body.as_bytes());
            for (n, v) in headers {
                r = r.header(n, v);
            }
            r
        }
        None => Reply::new(404, Some("text/plain"), "no"),
    })
}

const PRE: &str = "<script>var log = []; function L(x) { log.push(x + ':' + document.readyState) }</script>";

fn make(html: &str, srv: &Server, steps: u64, virtual_clock: bool) -> Browser {
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: steps, ..Default::default() }, HeapLimits::default()).expect("browser");
    if virtual_clock {
        b.use_virtual_clock();
    }
    b.configure_fetch(FetchConfig { base_url: Some(srv.url("/page.html")), client: Client::new(), limits: FetchLimits::default() }).unwrap();
    b
}

fn load(b: &mut Browser, srv: &Server, limits: PageLimits) -> PageResult {
    b.run_page(PageConfig { url: srv.url("/page.html"), client: Client::new(), limits, fetch: None })
}

fn quick() -> PageLimits {
    PageLimits { quiet: Duration::from_millis(100), ..Default::default() }
}

/// Run a page and give `log.join('|')`, the result and the server.
fn page(html: &str, routes: Vec<Route>, limits: PageLimits) -> (String, PageResult, Server) {
    let srv = server(routes);
    let mut b = make(html, &srv, 5_000_000, true);
    let r = load(&mut b, &srv, limits);
    let v = b.run_script("typeof log === 'undefined' ? 'no-log' : log.join('|')").unwrap();
    (b.it.display(v), r, srv)
}

fn eval(b: &mut Browser, src: &str) -> String {
    let v = b.run_script(src).unwrap();
    b.it.display(v)
}

#[test]
fn blocking_defer_async_order_with_lifecycle() {
    big(|| {
        let html = format!(
            "<head>{PRE}<script>
              document.addEventListener('DOMContentLoaded', function () {{ L('dcl') }});
              window.addEventListener('load', function () {{ L('load') }});
              document.addEventListener('readystatechange', function () {{ L('rsc') }});
              L('i1')</script>
              <script async src='/as.js'></script><script defer src='/d1.js'></script><script src='/b.js'></script>
              <script defer src='/d2.js'></script></head><body><script>L('i2')</script></body>"
        );
        let routes = vec![js("/as.js", "L('as')"), js("/d1.js", "L('d1')"), js("/d2.js", "L('d2')"), js("/b.js", "L('b')")];
        let (log, r, _) = page(&html, routes, quick());
        assert_eq!(
            log,
            "i1:loading|b:loading|i2:loading|rsc:interactive|d1:interactive|d2:interactive|dcl:interactive|as:interactive|rsc:complete|load:complete"
        );
        assert_eq!(r.stop, Stop::Settled, "{r:?}");
        assert_eq!((r.ready_state, r.scripts_run, r.script_throws, r.load_errors), ("complete", 7, 0, 0));
    });
}

#[test]
fn scripts_share_one_realm_and_a_repeated_element_runs_again_with_one_download() {
    big(|| {
        let html = format!(
            "{PRE}<script src='/a.js'></script><script src='/a.js'></script>
             <script>let lex = 'L'; var keep = window.cb; log.push(inc() + lex + keep() + ':' + window.n)</script>"
        );
        let routes = vec![js("/a.js", "window.n = (window.n || 0) + 1; var shared = 40 + window.n; function inc() { return ++shared } window.cb = function () { return shared }")];
        let (log, r, srv) = page(&html, routes, quick());
        // after two runs shared = 42; inc() makes 43
        assert_eq!(log, "43L43:2");
        assert_eq!(srv.hits("/a.js"), 1);
        assert_eq!(r.scripts_run, 4);
        assert_eq!(r.fetches, 1);
    });
}

#[test]
fn current_script_is_the_element_and_is_cleared_even_when_the_script_throws() {
    big(|| {
        let html = format!(
            "{PRE}<script id=s1>log.push('inline:' + document.currentScript.id)</script>
             <script id=t>Promise.resolve().then(function () {{ log.push('after-throw:' + document.currentScript) }}); throw new TypeError('secret-xyz')</script>
             <script src='/cs.js'></script>
             <script>setTimeout(function () {{ log.push('timer:' + document.currentScript) }}, 0);
               Promise.resolve().then(function () {{ log.push('promise:' + document.currentScript) }})</script>"
        );
        let routes = vec![js("/cs.js", "log.push('ext:' + document.currentScript.getAttribute('src'))")];
        let (log, r, _) = page(&html, routes, quick());
        assert_eq!(log, "inline:s1|after-throw:null|ext:/cs.js|promise:null|timer:null");
        assert_eq!((r.script_throws, r.stop.clone()), (1, Stop::Settled));
        let all = r.diagnostics.join("\n");
        assert!(all.contains("TypeError"), "{all}");
        assert!(!all.contains("secret"), "{all}");
    });
}

#[test]
fn failed_resources_never_run_and_get_an_error_event() {
    big(|| {
        let html = format!(
            "{PRE}<script>function w(id) {{ document.getElementById(id).addEventListener('error', function () {{ log.push('err:' + id) }}) }}
               w('bad'); w('nf'); w('gz'); w('big'); document.getElementById('ok').addEventListener('load', function () {{ log.push('load:ok') }})</script>
             <script id=bad src='/bad.js'></script><script id=nf src='/nf.js'></script><script id=gz src='/gz.js'></script>
             <script id=big src='/big.js'></script><script id=ok src='/ok.js'></script>
             <script>log.push(typeof pwned + typeof pwned2 + typeof pwned3 + typeof pwned4)</script>"
        );
        let routes = vec![
            ("/bad.js", 200, Some("text/html"), "window.pwned = 1".into(), vec![]),
            ("/nf.js", 404, Some("application/javascript"), "window.pwned2 = 1".into(), vec![]),
            ("/gz.js", 200, Some("application/javascript"), "window.pwned3 = 1".into(), vec![("Content-Encoding", "gzip")]),
            js("/big.js", &format!("window.pwned4 = 1; /*{}*/", "x".repeat(200))),
            js("/ok.js", "log.push('ok')"),
        ];
        let limits = PageLimits { resources: crate::resources::Limits { max_resource_bytes: 100, ..Default::default() }, ..quick() };
        let (log, r, _) = page(&html, routes, limits);
        assert_eq!(log, "err:bad|err:nf|err:gz|err:big|ok|load:ok|undefinedundefinedundefinedundefined");
        assert_eq!((r.load_errors, r.stop), (4, Stop::Settled));
        let all = r.diagnostics.join("\n");
        assert!(!all.contains("pwned"), "{all}");
    });
}

#[test]
fn dynamic_scripts_run_once_in_order_with_load_and_error_events() {
    big(|| {
        let html = format!(
            "{PRE}<script>
              var s = document.createElement('script'); s.src = '/dyn.js';
              s.onload = function () {{ log.push('onload') }}; s.addEventListener('load', function () {{ log.push('listener-load') }});
              document.head.appendChild(s); log.push('after-append');
              var e = document.createElement('script'); e.src = '/missing.js'; e.onerror = function () {{ log.push('onerror') }}; document.body.appendChild(e);
              var inl = document.createElement('script'); inl.textContent = 'log.push(\"dyn-inline:\" + (document.currentScript === inl))';
              document.body.appendChild(inl); log.push('after-inline-append');
              s.remove(); document.head.appendChild(s);
              var late = document.createElement('script'); document.body.appendChild(late); late.setAttribute('src', '/late.js');
              var bare = document.createElement('script'); bare.src = '/never.js';</script>"
        );
        let routes = vec![js("/dyn.js", "log.push('dyn:' + document.currentScript.getAttribute('src')); window.runs = (window.runs || 0) + 1"), js("/late.js", "log.push('late')")];
        let srv = server(routes);
        let mut b = make(&html, &srv, 5_000_000, true);
        let r = load(&mut b, &srv, quick());
        assert_eq!(eval(&mut b, "log.join('|')"), "after-append|after-inline-append|dyn-inline:true|dyn:/dyn.js|onload|listener-load|onerror|late");
        assert_eq!(eval(&mut b, "window.runs"), "1");
        assert_eq!(srv.hits("/dyn.js"), 1);
        assert_eq!(srv.hits("/never.js"), 0, "a script that was never inserted is not fetched");
        assert_eq!((r.stop.clone(), r.load_errors), (Stop::Settled, 1));
        // the IDL surface
        assert_eq!(eval(&mut b, "var q = document.createElement('script'); q.async = true; q.defer = false; q.type = 'text/javascript'; [typeof HTMLScriptElement, q instanceof HTMLScriptElement, q.async, q.defer, q.type, q.getAttribute('async'), q.src === ''].join()"), "function,true,true,false,text/javascript,,true");
        assert_eq!(eval(&mut b, "q.src = '/x.js'; q.src"), srv.url("/x.js"));
    });
}

#[test]
fn a_dynamic_inline_script_from_a_timer_runs_right_after_the_timer() {
    big(|| {
        let html = format!(
            "{PRE}<script>setTimeout(function () {{ var s = document.createElement('script'); s.textContent = 'log.push(\"from-timer-script\")';
               document.body.appendChild(s); log.push('timer-end'); Promise.resolve().then(function () {{ log.push('micro') }}) }}, 0)</script>"
        );
        let (log, r, _) = page(&html, vec![], quick());
        // the timer task checkpoint runs inside the pump, before the driver runs the queued inline script (documented deviation)
        assert_eq!(log, "timer-end|micro|from-timer-script");
        assert_eq!(r.stop, Stop::Settled);
    });
}

#[test]
fn network_promises_and_timers_get_their_checkpoints() {
    big(|| {
        let html = format!(
            "{PRE}<script>
              fetch('/data').then(function (r) {{ return r.text() }}).then(function (t) {{ log.push('f:' + t);
                setTimeout(function () {{ log.push('timer'); Promise.resolve().then(function () {{ log.push('micro-after-timer') }}) }}, 5) }});
              Promise.resolve().then(function () {{ log.push('micro1') }}); log.push('script1')</script>
             <script>log.push('script2')</script>"
        );
        let routes = vec![("/data", 200, Some("text/plain"), "hello".into(), vec![])];
        let (log, r, srv) = page(&html, routes, quick());
        assert_eq!(log, "script1|micro1|script2|f:hello|timer|micro-after-timer");
        assert_eq!((r.stop, srv.hits("/data")), (Stop::Settled, 1));
    });
}

#[test]
fn the_real_clock_waits_for_a_near_timer_and_stops_at_a_far_one() {
    big(|| {
        let srv = server(vec![]);
        let html = format!("{PRE}<script>setTimeout(function () {{ log.push('near') }}, 20); setTimeout(function () {{ log.push('far') }}, 60000)</script>");
        let mut b = make(&html, &srv, 5_000_000, false);
        let t = std::time::Instant::now();
        let r = load(&mut b, &srv, quick());
        assert_eq!(r.stop, Stop::Settled);
        assert_eq!(eval(&mut b, "log.join('|')"), "near");
        assert!(t.elapsed() >= Duration::from_millis(20) && t.elapsed() < Duration::from_secs(5));
    });
}

#[test]
fn budgets_stop_the_run_and_a_timer_loop_cannot_starve_the_network() {
    big(|| {
        let html = format!("{PRE}<script>var done = 0; function f() {{ setTimeout(f, 0) }} f(); fetch('/data').then(function () {{ done = 1 }}); var n = 0</script>");
        let routes = vec![("/data", 200, Some("text/plain"), "x".into(), vec![])];
        let srv = server(routes);
        let mut b = make(&html, &srv, 5_000_000, true);
        let r = load(&mut b, &srv, PageLimits { max_tasks: 500, ..quick() });
        assert_eq!(r.stop, Stop::TaskBudget, "{r:?}");
        assert!(r.tasks <= 500 && r.tasks >= 400, "{}", r.tasks);
        assert_eq!(eval(&mut b, "done"), "1");

        let mut b = make(&html, &srv, 5_000_000, true);
        let r = load(&mut b, &srv, PageLimits { max_rounds: 5, ..quick() });
        assert_eq!((r.stop, r.rounds), (Stop::RoundBudget, 5));

        let mut b = make(&html, &srv, 5_000_000, true);
        let r = load(&mut b, &srv, PageLimits { max_time: Duration::ZERO, ..quick() });
        assert_eq!((r.stop, r.scripts_run), (Stop::TimeBudget, 0));
        assert_eq!(eval(&mut b, "typeof log"), "undefined");
    });
}

#[test]
fn resource_caps_are_checked_before_scripts_run() {
    big(|| {
        // total cap: a.js is kept for its second element while b.js loads, so b.js does not fit
        let html = format!("{PRE}<script src='/a.js'></script><script id=b src='/b.js'></script><script src='/a.js'></script>");
        let body = format!("log.push('x'); /*{}*/", "y".repeat(40));
        let routes = vec![js("/a.js", &body), js("/b.js", &body.replace("'x'", "'b'"))];
        let limits = PageLimits { resources: crate::resources::Limits { max_total_bytes: 100, ..Default::default() }, ..quick() };
        let (log, r, _) = page(&html, routes.clone(), limits);
        assert_eq!(log, "x|x");
        assert_eq!(r.load_errors, 1, "{r:?}");

        // distinct URL cap and reference cap end the setup: nothing runs, nothing is fetched
        for resources in [crate::resources::Limits { max_fetches: 1, ..Default::default() }, crate::resources::Limits { max_references: 2, ..Default::default() }] {
            let (log, r, srv) = page(&html, routes.clone(), PageLimits { resources, ..quick() });
            assert_eq!(log, "no-log");
            assert!(matches!(r.stop, Stop::Fatal(Fatal::Setup(_))), "{r:?}");
            assert_eq!((srv.total(), r.scripts_run), (0, 0));
        }

        // dynamic fetch budget: the second dynamic script gets an error event
        let html = format!("{PRE}<script>for (var i = 0; i < 2; i++) {{ var s = document.createElement('script'); s.src = '/a.js?' + i; s.onerror = function () {{ log.push('err') }}; document.body.appendChild(s) }}</script>");
        let routes = vec![js("/a.js?0", "log.push('a0')"), js("/a.js?1", "log.push('a1')")];
        let (log, r, _) = page(&html, routes, PageLimits { resources: crate::resources::Limits { max_fetches: 1, ..Default::default() }, ..quick() });
        assert_eq!(log, "a0|err");
        assert_eq!(r.load_errors, 1);

        // pending dynamic scripts
        let html = format!("{PRE}<script>for (var i = 0; i < 3; i++) {{ var s = document.createElement('script'); s.textContent = 'log.push(1)'; document.body.appendChild(s) }}</script>");
        let (_, r, _) = page(&html, vec![], PageLimits { max_pending_scripts: 2, ..quick() });
        assert!(matches!(&r.stop, Stop::Fatal(Fatal::Limit(l)) if l.contains("pending-scripts")), "{r:?}");
        let (_, r, _) = page(&html, vec![], PageLimits { max_pending_script_bytes: 20, ..quick() });
        assert!(matches!(&r.stop, Stop::Fatal(Fatal::Limit(l)) if l.contains("pending-script-bytes")), "{r:?}");
    });
}

#[test]
fn a_pending_dynamic_script_survives_collections() {
    big(|| {
        let html = format!(
            "{PRE}<script>
              var g1 = document.createElement('script'); g1.src = '/g1.js'; document.head.appendChild(g1);
              var s = document.createElement('script'); s.src = '/g2.js'; s.onload = function () {{ log.push('gc-load') }};
              document.head.appendChild(s); s.remove(); s = null;
              for (var i = 0; i < 100; i++) {{ document.createElement('div') }}</script>"
        );
        let routes = vec![js("/g1.js", "log.push('g1')"), js("/g2.js", "log.push('g2')")];
        let (log, r, _) = page(&html, routes, PageLimits { batch: 1, collect_every: 1, ..quick() });
        assert_eq!(log, "g1|g2|gc-load");
        assert_eq!(r.stop, Stop::Settled);
    });
}

#[test]
fn a_module_script_is_an_explicit_fatal_and_nothing_runs_after_it() {
    big(|| {
        let html = format!("{PRE}<script>log.push('a')</script><script type=module src='/m.js'></script><script defer src='/after.js'></script>");
        let routes = vec![js("/m.js", "log.push('m')"), js("/after.js", "log.push('after')")];
        let srv = server(routes);
        let mut b = make(&html, &srv, 5_000_000, true);
        let r = load(&mut b, &srv, quick());
        assert_eq!(r.stop, Stop::Fatal(Fatal::Unsupported("module script")));
        assert_eq!(eval(&mut b, "log.join('|')"), "a");
        assert_eq!(srv.total(), 0);
        // a second call returns the same stop and does nothing
        let again = load(&mut b, &srv, quick());
        assert_eq!(again.stop, r.stop);
        assert_eq!((again.scripts_run, srv.total()), (0, 0));
    });
}

#[test]
fn a_limit_or_a_failed_checkpoint_stops_all_js_and_io() {
    big(|| {
        let cases = [
            // a hard limit inside a script
            "fetch('/x'); log.push('s1'); for (;;) {}",
            // a job that hits a limit: the checkpoint after the script fails
            "fetch('/x'); log.push('s1'); Promise.resolve().then(function () { for (;;) {} })",
        ];
        for body in cases {
            let html = format!("{PRE}<script>setTimeout(function () {{ log.push('timer') }}, 0)</script><script>{body}</script><script src='/later.js'></script><script>log.push('s3')</script>");
            let routes = vec![js("/later.js", "log.push('later')"), ("/x", 200, Some("text/plain"), "x".into(), vec![])];
            let srv = server(routes);
            let mut b = make(&html, &srv, 100_000, true);
            let r = load(&mut b, &srv, quick());
            assert!(matches!(&r.stop, Stop::Fatal(Fatal::Limit(l)) if l == "Steps"), "{r:?}");
            assert_eq!(srv.total(), 0, "no I/O after the failure: {body}");
            assert_eq!(r.fetches, 0);
            // the VM is poisoned or stopped: the realm is not asked to run more by the driver
            assert_eq!(load(&mut b, &srv, quick()).stop, r.stop);
            assert_eq!(srv.total(), 0);
            assert!(r.diagnostics.iter().any(|d| d.starts_with("fatal in ")), "{r:?}");
        }
    });
}

#[test]
fn syntax_errors_throw_and_loading_goes_on() {
    big(|| {
        let html = format!("{PRE}<script>this is not javascript</script><script>log.push('next')</script>");
        let (log, r, _) = page(&html, vec![], quick());
        assert_eq!(log, "next");
        assert_eq!((r.script_throws, r.stop.clone()), (1, Stop::Settled));
        assert!(r.diagnostics.iter().any(|d| d.contains("SyntaxError")), "{r:?}");
    });
}
