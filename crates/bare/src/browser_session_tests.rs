//! Tests of the document session (`browser_session.rs`) and of the navigation stop of the loader. Every document is
//! loaded over loopback HTTP from a throwaway server of this process. Scripts do all the work (`location.assign`, timers,
//! `fetch`, storage): no test calls a host handler. Each test runs on a big stack. Expectations come from the HTML
//! Standard where the session follows it, and from the module comment where it does not.

use crate::browser::*;
use crate::browser_loader::*;
use crate::browser_session::*;
use crate::resources::test_server::{Reply, Server, serve};
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_net::http::Client;
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

type Route = (&'static str, Reply);

fn html(body: &str) -> Reply {
    Reply::new(200, Some("text/html; charset=utf-8"), body)
}

fn js(body: &str) -> Reply {
    Reply::new(200, Some("application/javascript"), body)
}

/// A server that answers the exact path (lowercase, query included). The route list is built again for each request.
fn server(routes: impl Fn() -> Vec<Route> + Send + Sync + 'static) -> Server {
    serve(move |path| routes().into_iter().find(|r| r.0 == path).map(|r| r.1).unwrap_or_else(|| Reply::new(404, Some("text/plain"), "no")))
}

fn cfg(url: String) -> SessionConfig {
    let mut c = SessionConfig::new(&url);
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
    c.page.quiet = Duration::from_millis(100);
    c
}

fn get(s: &Session, srv: &Server, key: &str) -> Option<String> {
    s.storage().get_item(false, &srv.url(""), key)
}

#[test]
fn initial_inline_and_dynamic_scripts_run_in_the_first_document() {
    big(|| {
        let srv = server(|| {
            vec![
                (
                    "/",
                    html(
                        "<script>localStorage.setItem('a', 'inline'); var s = document.createElement('script'); s.src = '/dyn.js';
                         s.onload = function () { localStorage.setItem('c', 'onload') }; document.head.appendChild(s);
                         var t = document.createElement('script'); t.text = 'localStorage.setItem(\"d\", location.pathname)';
                         document.body.appendChild(t); localStorage.setItem('url', location.href)</script>",
                    ),
                ),
                ("/dyn.js", js("localStorage.setItem('b', 'dynamic:' + (document.currentScript !== null))")),
            ]
        });
        let mut s = Session::new(cfg(srv.url("/")));
        let r = s.run();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!((r.pages.len(), r.pages[0].script_throws, r.pages[0].load_errors), (1, 0, 0));
        for (k, v) in [("a", "inline"), ("b", "dynamic:true"), ("c", "onload"), ("d", "/")] {
            assert_eq!(get(&s, &srv, k).as_deref(), Some(v), "{k}");
        }
        // the realm knew the real URL before the first script ran
        assert_eq!(get(&s, &srv, "url"), Some(srv.url("/")));
        assert_eq!(r.urls[0], format!("http://127.0.0.1:{}/", srv.port));
    });
}

#[test]
fn async_class_fetch_history_and_timer_work_together_on_one_origin() {
    big(|| {
        let srv = server(|| {
            vec![
                (
                    "/app/",
                    html(
                        "<script>
                        class Api { #n = 0; async get(p) { this.#n++; const r = await fetch(p); return (await r.text()) + ':' + this.#n } }
                        const api = new Api();
                        (async () => {
                          localStorage.setItem('r1', await api.get('data'));
                          history.pushState({ s: 1 }, '', '/app/deep/page');
                          localStorage.setItem('loc', location.pathname);
                          localStorage.setItem('r2', await api.get('data'));
                          await new Promise(function (ok) { setTimeout(ok, 30) });
                          localStorage.setItem('timer', 'done:' + JSON.stringify(history.state));
                        })().catch(function (e) { localStorage.setItem('err', String(e)) });
                        </script>",
                    ),
                ),
                ("/app/data", Reply::new(200, Some("text/plain"), "one")),
                ("/app/deep/data", Reply::new(200, Some("text/plain"), "two")),
            ]
        });
        let mut s = Session::new(cfg(srv.url("/app/")));
        let r = s.run();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "err"), None);
        assert_eq!(get(&s, &srv, "r1").as_deref(), Some("one:1"));
        assert_eq!(get(&s, &srv, "loc").as_deref(), Some("/app/deep/page"));
        // a relative fetch follows the URL that pushState set, at once
        assert_eq!(get(&s, &srv, "r2").as_deref(), Some("two:2"));
        assert_eq!(srv.hits("/app/deep/data"), 1);
        assert_eq!(get(&s, &srv, "timer").as_deref(), Some("done:{\"s\":1}"));
        assert_eq!(r.pages.len(), 1);
    });
}

#[test]
fn location_assign_makes_a_fresh_realm_with_shared_storage_and_no_old_callbacks() {
    big(|| {
        let srv = server(|| {
            vec![
                (
                    "/a",
                    html(
                        "<script>window.marker = 'old'; localStorage.setItem('k', 'v1'); sessionStorage.setItem('s', 'sess');
                         setTimeout(function () { localStorage.setItem('old-timer', '1') }, 0);
                         fetch('/slow').then(function () { localStorage.setItem('old-fetch', '1') });
                         window.addEventListener('load', function () { localStorage.setItem('old-load', '1') });
                         Promise.resolve().then(function () { localStorage.setItem('same-task-microtask', '1') });
                         location.assign('/b?token=secretq#frag')</script>
                         <script>localStorage.setItem('second-script', '1')</script>",
                    ),
                ),
                (
                    "/b?token=secretq",
                    html(
                        "<script>localStorage.setItem('b-sees', [typeof window.marker, localStorage.getItem('k'), sessionStorage.getItem('s'),
                           location.pathname, location.search, location.hash].join('|'))</script>",
                    ),
                ),
                ("/slow", Reply::new(200, Some("text/plain"), "x")),
            ]
        });
        let mut s = Session::new(cfg(srv.url("/a")));
        let r = s.run();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(r.pages.len(), 2);
        assert_eq!(get(&s, &srv, "b-sees").as_deref(), Some("undefined|v1|sess|/b|?token=secretq|#frag"));
        assert_eq!(get(&s, &srv, "same-task-microtask").as_deref(), Some("1"));
        for k in ["old-timer", "old-fetch", "old-load", "second-script"] {
            assert_eq!(get(&s, &srv, k), None, "{k} must not run after the navigation");
        }
        assert_eq!(srv.hits("/slow"), 0, "a queued fetch of the old document never starts");
        // no query, fragment or token in any result
        let dump = format!("{r:?}");
        assert!(!dump.contains("secretq") && !dump.contains("frag"), "{dump}");
        assert_eq!(r.urls, vec![format!("http://127.0.0.1:{}/a", srv.port), format!("http://127.0.0.1:{}/b", srv.port)]);
    });
}

#[test]
fn reload_loads_the_same_url_again_and_the_document_limit_stops_a_loop() {
    big(|| {
        let srv = server(|| {
            vec![
                ("/r", html("<script>var n = +(localStorage.getItem('n') || 0) + 1; localStorage.setItem('n', String(n)); if (n < 3) location.reload()</script>")),
                ("/loop", html("<script>location.reload()</script>")),
            ]
        });
        let mut s = Session::new(cfg(srv.url("/r")));
        let r = s.run();
        assert_eq!((r.stop.clone(), r.pages.len(), srv.hits("/r")), (SessionStop::Page(Stop::Settled), 3, 3), "{r:?}");
        assert_eq!(get(&s, &srv, "n").as_deref(), Some("3"));

        let mut c = cfg(srv.url("/loop"));
        c.limits.max_documents = 4;
        let mut s = Session::new(c);
        let r = s.run();
        assert_eq!((r.stop, r.pages.len(), srv.hits("/loop")), (SessionStop::DocumentLimit, 4, 4));
        assert!(matches!(&r.pages[3].stop, Stop::Navigate(_)));
    });
}

#[test]
fn redirects_are_followed_within_the_cap_and_unsafe_ones_stop_before_any_request() {
    big(|| {
        let srv = server(|| {
            vec![
                ("/go", Reply::new(302, None, "").header("Location", "/final?x=1")),
                ("/final?x=1", html("<script>localStorage.setItem('at', location.pathname + location.search)</script>")),
                ("/loop", Reply::new(302, None, "").header("Location", "/loop")),
                ("/ftp", Reply::new(302, None, "").header("Location", "ftp://127.0.0.1/x")),
                ("/js", Reply::new(302, None, "").header("Location", "javascript:alert(1)")),
                ("/nolocation", Reply::new(302, None, "")),
            ]
        });
        let mut s = Session::new(cfg(srv.url("/go")));
        let r = s.run();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "at").as_deref(), Some("/final?x=1"));
        assert_eq!(r.urls[0], format!("http://127.0.0.1:{}/final", srv.port));

        let mut c = cfg(srv.url("/loop"));
        c.limits.max_redirects = 3;
        let r = Session::new(c).run();
        assert_eq!(r.stop, SessionStop::LoadFailed("too many redirects".into()));
        assert_eq!(srv.hits("/loop"), 4, "the first request and three redirects");

        for p in ["/ftp", "/js", "/nolocation"] {
            let r = Session::new(cfg(srv.url(p))).run();
            assert!(matches!(r.stop, SessionStop::LoadFailed(_)), "{p}: {r:?}");
            assert_eq!(srv.hits(p), 1);
        }
        assert_eq!(srv.total(), 2 + 4 + 3);
    });
}

#[test]
fn a_redirect_from_https_to_http_is_not_allowed() {
    let u = |s: &str| bare_url::Url::parse(s).unwrap();
    assert!(!redirect_allowed(&u("https://a.test/x"), &u("http://a.test/x")));
    assert!(redirect_allowed(&u("http://a.test/x"), &u("https://a.test/x")));
    assert!(redirect_allowed(&u("https://a.test/x"), &u("https://b.test/x")));
}

#[test]
fn a_bad_status_type_encoding_or_size_ends_the_session_with_a_fixed_label() {
    big(|| {
        let srv = server(|| {
            vec![
                ("/404", Reply::new(404, Some("text/html"), "<script>localStorage.setItem('ran','1')</script>")),
                                ("/notype", Reply::new(200, None, "<p>")),
                ("/gz", html("x").header("Content-Encoding", "zstd")),
                ("/big", html(&"a".repeat(5000))),
            ]
        });
        for (p, why) in [("/404", "status 404"), ("/notype", "the content type is not HTML"), ("/gz", "the content encoding is not supported")] {
            let r = Session::new(cfg(srv.url(p))).run();
            assert_eq!(r.stop, SessionStop::LoadFailed(why.into()), "{p}");
            assert!(r.pages.is_empty());
        }
        let mut c = cfg(srv.url("/big"));
        c.limits.max_document_bytes = 1000;
        assert!(matches!(Session::new(c).run().stop, SessionStop::LoadFailed(_)));
        // a failed navigation leaves the diagnostics free of the query
        let r = Session::new(cfg(srv.url("/404?token=secretq"))).run();
        assert!(!format!("{r:?}").contains("secretq"));
        let mut s = Session::new(cfg(srv.url("/404")));
        s.run();
        assert_eq!(get(&s, &srv, "ran"), None);
    });
}

#[test]
fn a_text_or_image_document_becomes_a_pre_or_an_img_and_other_types_are_downloads() {
    let doc = |ct: &str, body: &[u8]| non_html_document(Some(ct), "http://h.test/a?x=1&y=2", body);
    assert_eq!(doc("application/json", b"{\"a\": \"<b>\"}").unwrap(), "<pre style=\"white-space:pre-wrap;word-wrap:break-word\">{\"a\": \"&lt;b&gt;\"}</pre>");
    for t in ["text/plain; charset=utf-8", "text/xml", "application/xml", "application/ld+json", "text/css", "application/javascript"] {
        assert!(doc(t, b"x & y").unwrap().contains("x &amp; y"), "{t}");
    }
    assert_eq!(doc("image/png", b"\x89PNG").unwrap(), "<img src=\"http://h.test/a?x=1&amp;y=2\">");
    assert_eq!(doc("image/svg+xml", b"<svg/>").unwrap(), "<img src=\"http://h.test/a?x=1&amp;y=2\">");
    for t in ["application/pdf", "application/octet-stream", "video/mp4"] {
        assert_eq!(doc(t, b"%PDF").unwrap_err(), "the content is a download", "{t}");
    }
    assert_eq!(non_html_document(None, "http://h.test/", b"<p>").unwrap_err(), "the content type is not HTML");
}

#[test]
fn a_json_response_loads_as_a_text_document() {
    big(|| {
        let srv = server(|| vec![("/json", Reply::new(200, Some("application/json"), "{}")), ("/pdf", Reply::new(200, Some("application/pdf"), "%PDF"))]);
        let r = Session::new(cfg(srv.url("/json"))).run();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(r.pages.len(), 1);
        let r = Session::new(cfg(srv.url("/pdf"))).run();
        assert_eq!(r.stop, SessionStop::LoadFailed("the content is a download".into()));
    });
}

#[test]
fn a_fatal_error_in_the_same_task_wins_over_a_navigation_and_nothing_loads_after_it() {
    big(|| {
        let srv = server(|| {
            vec![
                ("/f", html("<script>localStorage.setItem('before', '1'); location.assign('/b'); console.log.apply(null, new Array(100))</script>")),
                ("/m", html("<script>location.assign('/b'); Promise.resolve().then(function () { console.log.apply(null, new Array(100)) })</script>")),
                ("/b", html("<script>localStorage.setItem('b-ran', '1')</script>")),
            ]
        });
        for p in ["/f", "/m"] {
            let mut s = Session::new(cfg(srv.url(p)));
            let r = s.run();
            assert!(matches!(r.stop, SessionStop::Page(Stop::Fatal(Fatal::Limit(_)))), "{p}: {r:?}");
            assert_eq!(r.pages.len(), 1);
            assert_eq!(get(&s, &srv, "b-ran"), None);
            // a second run of the same realm gives the same stop and runs nothing (the sticky fatal state)
            let again = s.browser().unwrap().run_page(PageConfig { url: srv.url(p), client: Client::new(), limits: PageLimits::default(), fetch: None });
            assert_eq!(again.stop, r.pages[0].stop);
        }
        assert_eq!(srv.hits("/b"), 0, "no I/O after a fatal error");
    });
}

#[test]
fn another_origin_gets_its_own_storage_and_the_session_keeps_each_origin() {
    big(|| {
        let other = server(|| vec![("/c", html("<script>localStorage.setItem('seen', String(localStorage.getItem('secret'))); localStorage.setItem('own', 'B')</script>"))]);
        let target = other.url("/c");
        let srv = server(move || vec![("/a", html(&format!("<script>localStorage.setItem('secret', 'A-only'); location.href = '{target}'</script>")))]);
        let mut s = Session::new(cfg(srv.url("/a")));
        let r = s.run();
        assert_eq!((r.stop.clone(), r.pages.len()), (SessionStop::Page(Stop::Settled), 2), "{r:?}");
        assert_eq!(get(&s, &srv, "secret").as_deref(), Some("A-only"));
        assert_eq!(s.storage().get_item(false, &other.url(""), "seen").as_deref(), Some("null"));
        assert_eq!(s.storage().get_item(false, &other.url(""), "own").as_deref(), Some("B"));
        assert_eq!(s.storage().get_item(false, &other.url(""), "secret"), None);
        assert_eq!(s.storage().origin_count(false), 2);
    });
}

#[test]
fn session_wide_budgets_end_a_spinning_page_and_the_run_collects_garbage() {
    big(|| {
        let srv = server(|| {
            vec![
                ("/spin", html("<script>setInterval(function () { localStorage.setItem('t', String(Date.now() >= 0)) }, 0)</script>")),
                ("/alloc", html("<script>for (var i = 0; i < 20; i++) { var big = []; for (var j = 0; j < 2000; j++) big.push({ j: j, s: 'x' + j }); } location.reload()</script>")),
            ]
        });
        let mut c = cfg(srv.url("/spin"));
        c.limits.max_tasks = 500;
        let r = Session::new(c).run();
        assert!(matches!(r.stop, SessionStop::Page(Stop::TaskBudget) | SessionStop::TaskBudget), "{r:?}");
        assert!(r.tasks <= 500 + 8, "{}", r.tasks);

        let mut c = cfg(srv.url("/spin"));
        c.limits.max_time = Duration::MAX;
        c.page.max_time = Duration::MAX;
        c.page.max_tasks = 300;
        let r = Session::new(c).run();
        assert_eq!(r.stop, SessionStop::Page(Stop::TaskBudget), "Duration::MAX must not panic: {r:?}");

        // 6 documents, each builds garbage and reloads: each old heap is dropped before the next load
        let mut c = cfg(srv.url("/alloc"));
        c.limits.max_documents = 6;
        c.eval.max_steps = 50_000_000;
        let r = Session::new(c).run();
        assert_eq!((r.stop, r.pages.len(), srv.hits("/alloc")), (SessionStop::DocumentLimit, 6, 6));
    });
}

// ---- the loader itself: URL setup and the navigation stop (no session)

fn bare_browser(html: &str) -> Browser {
    Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).unwrap()
}

#[test]
fn run_page_sets_the_document_url_and_fetch_origin_before_the_first_script() {
    big(|| {
        let srv = server(|| vec![("/p/x", Reply::new(200, Some("text/plain"), "ok"))]);
        let mut b = bare_browser("<script>var out = []; out.push(location.href); fetch('x').then(function (r) { return r.text() }).then(function (t) { out.push(t) })</script>");
        b.use_virtual_clock();
        let page = srv.url("/p/y");
        let r = b.run_page(PageConfig { url: page.clone(), client: Client::new(), limits: PageLimits::default(), fetch: Some((Client::new(), Default::default())) });
        assert_eq!(r.stop, Stop::Settled, "{r:?}");
        let v = b.run_script("out.join('|')").unwrap();
        assert_eq!(b.it.display(v), format!("{page}|ok"));
        // a document URL of another origin than the page is refused before anything runs
        let mut b = bare_browser("<script>throw 1</script>");
        b.set_document_url("http://other.test/").unwrap();
        let r = b.run_page(PageConfig { url: page, client: Client::new(), limits: PageLimits::default(), fetch: None });
        assert!(matches!(r.stop, Stop::Fatal(Fatal::Setup(_))) && r.scripts_run == 0, "{r:?}");
    });
}

#[test]
fn run_page_stops_with_navigate_and_the_debug_output_has_no_query() {
    big(|| {
        let srv = server(Vec::new);
        let mut b = bare_browser("<script>setTimeout(function () { throw new Error('old timer') }, 0); location.assign('/next?token=secretq#f')</script><script>throw 1</script>");
        b.use_virtual_clock();
        let r = b.run_page(PageConfig { url: srv.url("/cur"), client: Client::new(), limits: PageLimits::default(), fetch: None });
        let Stop::Navigate(n) = &r.stop else { panic!("{r:?}") };
        assert_eq!(n.0.url, srv.url("/next?token=secretq#f"));
        assert_eq!((r.scripts_run, r.script_throws), (1, 0), "the second script and the old timer do not run");
        assert!(!format!("{r:?}").contains("secretq"));
        assert!(format!("{:?}", r.stop).contains("/next"));
        assert_eq!(b.take_reports().len(), 0);
    });
}

#[test]
fn same_document_popstate_are_pumped_by_the_driver() {
    big(|| {
        let srv = server(Vec::new);
        let mut b = bare_browser(
            "<script>var out = []; addEventListener('popstate', function (e) { out.push('pop:' + JSON.stringify(e.state)) });
             history.pushState({ a: 1 }, '', '/one'); history.pushState({ a: 2 }, '', '/two'); history.back()</script>",
        );
        b.use_virtual_clock();
        let r = b.run_page(PageConfig { url: srv.url("/zero"), client: Client::new(), limits: PageLimits::default(), fetch: None });
        assert_eq!(r.stop, Stop::Settled, "{r:?}");
        let v = b.run_script("out.join('|')").unwrap();
        assert_eq!(b.it.display(v), "pop:{\"a\":1}");
    });
}

#[test]
fn a_page_time_limit_is_not_replaced_by_the_session_limit() {
    big(|| {
        let srv = server(|| vec![("/", html("<script>localStorage.setItem('ran','yes')</script>"))]);
        let mut c = cfg(srv.url("/"));
        c.page.max_time = Duration::ZERO;
        let mut s = Session::new(c);
        let r = s.run();
        assert_eq!(r.stop, SessionStop::Page(Stop::TimeBudget));
        assert_eq!(get(&s, &srv, "ran"), None);
    });
}

#[test]
fn document_fragments_are_not_silently_truncated() {
    big(|| {
        let srv = server(|| vec![("/", html("<script>localStorage.setItem('length',String(location.hash.length))</script>"))]);
        let mut s = Session::new(cfg(format!("{}#{}", srv.url("/"), "x".repeat(3000))));
        let r = s.run();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled));
        assert_eq!(get(&s, &srv, "length").as_deref(), Some("3001"));
    });
}

#[test]
fn navigation_finishes_its_checkpoint_without_running_pending_scripts() {
    big(|| {
        let srv = server(|| vec![("/",html("<script>Promise.resolve().then(function(){localStorage.setItem('checkpoint','yes')});var s=document.createElement('script');s.text='localStorage.setItem(\"pending\",\"bad\")';document.head.appendChild(s);location.assign('/next')</script>")),("/next",html("<body>next</body>"))]);
        let mut s = Session::new(cfg(srv.url("/")));
        let r = s.run();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled));
        assert_eq!(get(&s, &srv, "checkpoint").as_deref(), Some("yes"));
        assert_eq!(get(&s, &srv, "pending"), None);
    });
}

#[test]
fn configured_session_viewport_reaches_media_queries() {
    big(|| {
        let srv = server(|| vec![("/",html("<script>localStorage.setItem('narrow',String(matchMedia('(max-width: 500px)').matches))</script>"))]);
        let mut c = cfg(srv.url("/"));
        c.viewport = Some((300.0,800.0));
        let mut s = Session::new(c);
        assert_eq!(s.run().stop, SessionStop::Page(Stop::Settled));
        assert_eq!(get(&s, &srv, "narrow").as_deref(), Some("true"));
    });
}

#[test]
fn xhtml_is_shown_as_text_and_not_executed() {
    big(|| {
        let srv = server(|| vec![("/",Reply::new(200,Some("application/xhtml+xml"),"<html><body><script>localStorage.setItem('xml','ran')</script></body></html>"))]);
        let mut s = Session::new(cfg(srv.url("/")));
        assert_eq!(s.run().stop, SessionStop::Page(Stop::Settled));
        assert_eq!(get(&s, &srv, "xml"), None);
    });
}

/// A finite fiber-style walk: two mutually recursive functions over a chain of 40 nodes (81 JS calls). The default
/// `SessionConfig` must finish it. The `try` in `a6` cannot catch a limit, so a refusal would stop the page.
const FIBER_WALK: &str = r#"<script>
function a5(t) { for (t = t.child; t !== null;) a6(t), t = t.sibling; }
function a6(t) { switch (t.tag) { case 0: case 1: try { a5(t) } catch (e) {} break; default: a5(t) } }
var root = { tag: 3, child: null, sibling: null }, tail = root;
for (var i = 0; i < 40; i++) { var n = { tag: 0, child: null, sibling: null }; tail.child = n; tail = n; }
a5(root);
localStorage.setItem('walk', 'done');
</script>"#;

#[test]
fn a_finite_forty_level_walk_completes_in_the_default_session() {
    big(|| {
        let srv = server(|| vec![("/", html(FIBER_WALK))]);
        let mut c = SessionConfig::new(&srv.url("/"));
        c.virtual_clock = true;
        c.page.quiet = Duration::from_millis(100);
        let mut s = Session::new(c);
        let r = s.run();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "walk").as_deref(), Some("done"));
    });
}

#[test]
fn a_react_commit_walk_2000_levels_deep_runs_under_the_default_session_limits() {
    // The shape of React 19's commit phase: two functions that call each other once for each fiber level, with a `switch`, an
    // `if`, a `for` loop and a `try`. The live staging app stopped with `Limit(TraversalDepth)` at 96 levels. 2,000 levels are
    // 4,000 nested calls. The session keeps `SessionConfig::new`'s limits (only the step budget is raised).
    big(|| {
        let page = "<script>
            var MASK = 3;
            function commitMutationEffectsOnFiber(fiber, root) {
              switch (fiber.tag) {
                case 0: case 11: recursivelyTraverseMutationEffects(root, fiber); root.count = root.count + 1; break;
                case 5: try { recursivelyTraverseMutationEffects(root, fiber); } catch (e) { root.err = e; } root.count = root.count + 1; break;
                default: recursivelyTraverseMutationEffects(root, fiber);
              }
            }
            function recursivelyTraverseMutationEffects(root, parentFiber) {
              if (parentFiber.subtreeFlags & MASK) {
                for (var child = parentFiber.child; child !== null; child = child.sibling) commitMutationEffectsOnFiber(child, root);
              }
            }
            var tags = [0, 5, 3, 11], root = { tag: 3, subtreeFlags: 3, child: null, sibling: null, count: 0 }, tail = root;
            for (var i = 0; i < 2000; i++) {
              var n = { tag: tags[i % 4], subtreeFlags: 3, child: null, sibling: null };
              n.sibling = { tag: 0, subtreeFlags: 0, child: null, sibling: null };
              tail.child = n; tail = n;
            }
            commitMutationEffectsOnFiber(root, root);
            localStorage.setItem('count', String(root.count));
            </script>";
        let srv = server(move || vec![("/", html(page))]);
        let mut c = SessionConfig::new(&srv.url("/"));
        c.virtual_clock = true;
        c.page.quiet = Duration::from_millis(100);
        c.eval.max_steps = 50_000_000;
        let mut s = Session::new(c);
        let r = s.run();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!((r.pages[0].script_throws, r.pages[0].load_errors), (0, 0));
        assert_eq!(get(&s, &srv, "count").as_deref(), Some("3500"));
    });
}

#[test]
fn the_documents_and_the_inputs_of_a_session_share_its_connections() {
    big(|| {
        // Chrome keeps its sockets through a navigation and a click. A new pool for each document or input cost a TCP and a TLS
        // handshake each time on staging (one round trip each), and the first scripts after it ran one at a time.
        let srv = server(|| {
            vec![
                ("/r", html("<button id=b>b</button><script>var n = +(localStorage.getItem('n') || 0) + 1; localStorage.setItem('n', String(n));
                    if (n < 3) location.reload();
                    document.getElementById('b').addEventListener('click', function () { var s = document.createElement('script'); s.src = '/s.js'; document.body.appendChild(s) })</script>")),
                ("/s.js", js("localStorage.setItem('s', 'ran')")),
            ]
        });
        let mut s = Session::new(cfg(srv.url("/r")));
        let r = s.run();
        assert_eq!((r.stop, r.pages.len()), (SessionStop::Page(Stop::Settled), 3));
        assert_eq!(s.resume(&[crate::browser_loader::Input::Click("#b")]).stop, SessionStop::Page(Stop::Settled));
        assert_eq!(get(&s, &srv, "s").as_deref(), Some("ran"));
        assert_eq!((srv.total(), srv.connections()), (4, 1));
    });
}
