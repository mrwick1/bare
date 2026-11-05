//! Tests of `<link rel=preload>` (`preload.rs`) and of the dynamic style sheet pattern of React 19 (a preload link, a commit
//! that waits for its `load`, then a style sheet link that waits for its own `load`). Real pages, a real realm and a
//! loopback server with synthetic bodies. The expected values come from the HTML Standard ("link type preload").

use crate::browser::*;
use crate::browser_fetch::FetchLimits;
use crate::browser_loader::*;
use crate::resources::test_server::{Reply, Server, serve};
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_net::http::Client;
use std::collections::HashMap;
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// `(path, content type, body)`: a 200 reply. Another path is a 404.
fn site(files: &[(&'static str, &'static str, &str)]) -> Server {
    let files: HashMap<&'static str, (&'static str, String)> = files.iter().map(|(p, t, b)| (*p, (*t, b.to_string()))).collect();
    serve(move |path| match files.get(path.split('?').next().unwrap()) {
        Some((t, b)) => Reply::new(200, Some(t), b.as_str()),
        None => Reply::new(404, Some("text/plain"), "no"),
    })
}

fn limits() -> PageLimits {
    PageLimits { quiet: Duration::from_millis(100), ..Default::default() }
}

fn run_with(html: &str, srv: &Server, lim: PageLimits) -> (String, PageResult, Browser) {
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    let r = b.run_page(PageConfig { url: srv.url("/page.html"), client: Client::new(), limits: lim, fetch: Some((Client::new(), FetchLimits::default())) });
    let v = b.run_script("typeof log === 'undefined' ? 'no-log' : log.join('|')").unwrap();
    let out = b.it.display(v);
    (out, r, b)
}

fn run(html: &str, srv: &Server) -> (String, PageResult, Browser) {
    run_with(html, srv, limits())
}

const CSS: &str = "text/css";

#[test]
fn react_pattern_preload_then_stylesheet_commit() {
    big(|| {
        let srv = site(&[("/a.css", CSS, "#p { direction: rtl }")]);
        // Like React 19: a task appends `<link rel=preload as=style>` and commits only after its `load`. The commit appends the
        // style sheet (with `data-precedence`) and finishes after the `load` of that link.
        let html = "<body><p id=p>x</p><script>var log = [];\
            setTimeout(function () {\
              var pre = document.createElement('link');\
              pre.rel = 'preload'; pre.as = 'style'; pre.href = '/a.css';\
              pre.addEventListener('load', function () {\
                log.push('preload-load');\
                var s = document.createElement('link');\
                s.rel = 'stylesheet'; s.href = '/a.css'; s.setAttribute('data-precedence', 'next');\
                s.addEventListener('load', function () {\
                  log.push('sheet-load');\
                  log.push('commit:' + getComputedStyle(document.getElementById('p')).direction);\
                });\
                s.addEventListener('error', function () { log.push('sheet-error') });\
                document.head.appendChild(s);\
              });\
              pre.addEventListener('error', function () { log.push('preload-error') });\
              document.head.appendChild(pre);\
              log.push('appended');\
            }, 0);</script>";
        let (log, r, _) = run(html, &srv);
        assert_eq!(log, "appended|preload-load|sheet-load|commit:rtl");
        assert_eq!(r.stop, Stop::Settled);
        assert_eq!(srv.hits("/a.css"), 1, "the style sheet link reuses the preload bytes");
        assert_eq!((r.fetches, r.css.fetches, r.css.failed), (1, 1, 0));
    });
}

#[test]
fn preload_errors_on_404_bad_type_and_each_kind() {
    big(|| {
        let srv = site(&[("/ok.js", "application/javascript", "1"), ("/html.css", "text/html", "x"), ("/f.woff2", "font/woff2", "abc")]);
        let html = "<script>var log = [];\
            function add(as, href) {\
              var l = document.createElement('link'); l.rel = 'preload'; l.as = as; l.href = href;\
              l.addEventListener('load', function () { log.push(as + ':' + href + ':load') });\
              l.addEventListener('error', function () { log.push(as + ':' + href + ':error') });\
              document.head.appendChild(l);\
            }\
            setTimeout(function () {\
              add('style', '/missing.css'); add('style', '/html.css'); add('script', '/missing.js'); add('script', '/ok.js');\
              add('font', '/f.woff2'); add('font', '/nofont.woff2'); add('image', '/nope.png'); add('fetch', '/f.woff2');\
            }, 0);</script>";
        let (log, r, _) = run(html, &srv);
        let mut got: Vec<&str> = log.split('|').collect();
        got.sort();
        let want = ["fetch:/f.woff2:load", "font:/f.woff2:load", "font:/nofont.woff2:error", "image:/nope.png:error", "script:/missing.js:error", "script:/ok.js:load", "style:/html.css:error", "style:/missing.css:error"];
        assert_eq!(got, want);
        assert_eq!(r.stop, Stop::Settled);
        assert_eq!(srv.hits("/f.woff2"), 1, "the same URL is fetched once");
        assert!(r.diagnostics.iter().any(|d| d.starts_with("preload failed:") && d.contains("/missing.js")), "{:?}", r.diagnostics);
        assert_eq!(r.preload.failed, 5);
    });
}

#[test]
fn detached_unknown_as_and_parsed_links_do_nothing() {
    big(|| {
        let srv = site(&[("/a.css", CSS, "x"), ("/b.css", CSS, "x"), ("/c.css", CSS, "x"), ("/d.css", CSS, "x")]);
        let html = "<link rel=preload as=style href=/d.css><script>var log = [];\
            function mk(href, as) {\
              var l = document.createElement('link'); l.rel = 'preload'; if (as) l.as = as; l.href = href;\
              l.addEventListener('load', function () { log.push('load:' + href) });\
              l.addEventListener('error', function () { log.push('error:' + href) });\
              return l;\
            }\
            setTimeout(function () {\
              mk('/a.css', 'style');\
              document.head.appendChild(mk('/b.css', 'bogus'));\
              document.head.appendChild(mk('/c.css', ''));\
              var gone = mk('/c.css', 'style'); document.head.appendChild(gone); document.head.removeChild(gone);\
              log.push('done');\
            }, 0);</script>";
        let (log, r, _) = run(html, &srv);
        assert_eq!(log, "done");
        assert_eq!(srv.total(), 0, "{:?}", srv.heads.lock().unwrap());
        assert_eq!((r.fetches, r.preload.fetches), (0, 0));
    });
}

#[test]
fn the_load_event_is_a_task_not_part_of_append_child() {
    big(|| {
        let srv = site(&[("/f.woff2", "font/woff2", "abc")]);
        let html = "<script>var log = [];\
            var l = document.createElement('link'); l.rel = 'preload'; l.as = 'font'; l.href = '/f.woff2';\
            l.addEventListener('load', function () { log.push('load') });\
            document.head.appendChild(l); log.push('after-append');\
            Promise.resolve().then(function () { log.push('microtask') });</script>";
        let (log, r, _) = run(html, &srv);
        assert_eq!(log, "after-append|microtask|load");
        assert_eq!(r.stop, Stop::Settled);
    });
}

#[test]
fn preloaded_script_bytes_are_used_by_the_script_with_the_same_url() {
    big(|| {
        let srv = site(&[("/chunk.js", "application/javascript", "log.push('chunk-ran');")]);
        let html = "<script>var log = [];\
            var l = document.createElement('link'); l.rel = 'preload'; l.as = 'script'; l.href = '/chunk.js';\
            l.addEventListener('load', function () {\
              log.push('preload-load');\
              var s = document.createElement('script'); s.src = '/chunk.js';\
              s.addEventListener('load', function () { log.push('script-load') });\
              document.head.appendChild(s);\
            });\
            document.head.appendChild(l);</script>";
        let (log, r, _) = run(html, &srv);
        assert_eq!(log, "preload-load|chunk-ran|script-load");
        assert_eq!(srv.hits("/chunk.js"), 1);
        assert_eq!(r.fetches, 1);
    });
}

#[test]
fn many_links_to_one_url_fetch_once_and_each_gets_load() {
    big(|| {
        let srv = site(&[("/a.css", CSS, "#p { direction: rtl }"), ("/f.woff2", "font/woff2", "abc")]);
        let html = "<script>var log = []; var n = 0;\
            for (var i = 0; i < 30; i++) {\
              var l = document.createElement('link'); l.rel = 'preload'; l.as = i % 2 ? 'style' : 'font'; l.href = i % 2 ? '/a.css' : '/f.woff2';\
              l.addEventListener('load', function () { n++ });\
              document.head.appendChild(l);\
            }\
            setTimeout(function () { log.push('n=' + n) }, 50);</script>";
        let (log, r, _) = run(html, &srv);
        assert_eq!(log, "n=30");
        assert_eq!((srv.hits("/a.css"), srv.hits("/f.woff2")), (1, 1));
        assert_eq!(r.fetches, 2);
    });
}

#[test]
fn a_parsed_style_sheet_link_after_a_preload_uses_the_cache() {
    big(|| {
        let srv = site(&[("/a.css", CSS, "#p { direction: rtl }")]);
        let html = "<p id=p></p><script>var log = [];\
            var l = document.createElement('link'); l.rel = 'preload'; l.as = 'style'; l.href = '/a.css';\
            l.addEventListener('load', function () {\
              var s = document.createElement('link'); s.rel = 'stylesheet'; s.href = '/a.css';\
              s.addEventListener('load', function () { log.push('sheet:' + getComputedStyle(document.getElementById('p')).direction) });\
              document.head.appendChild(s);\
            });\
            document.head.appendChild(l);</script>";
        let (log, _, _) = run(html, &srv);
        assert_eq!(log, "sheet:rtl");
        assert_eq!(srv.hits("/a.css"), 1);
    });
}

#[test]
fn a_dynamic_stylesheet_link_loads_applies_and_fires_load_or_error() {
    big(|| {
        let srv = site(&[("/a.css", CSS, "#p { direction: rtl }")]);
        let html = "<p id=p></p><script>var log = [];\
            function add(href, extra) {\
              var s = document.createElement('link'); s.rel = 'stylesheet'; s.setAttribute('href', href);\
              if (extra) s.setAttribute('data-precedence', extra);\
              s.addEventListener('load', function () { log.push('load:' + href) });\
              s.addEventListener('error', function () { log.push('error:' + href) });\
              document.head.appendChild(s);\
            }\
            setTimeout(function () { add('/a.css', 'next'); add('/missing.css'); }, 0);\
            setTimeout(function () { log.push('p=' + getComputedStyle(document.getElementById('p')).direction) }, 20);</script>";
        let (log, r, _) = run(html, &srv);
        assert_eq!(log, "load:/a.css|error:/missing.css|p=rtl");
        assert_eq!((r.css.loaded, r.css.failed, srv.hits("/a.css")), (1, 1, 1));
    });
}

#[test]
fn link_href_as_cross_origin_reflect_and_document_scripts_lists_scripts() {
    big(|| {
        let srv = site(&[]);
        let html = "<script id=s1>var log = [];</script><link id=l rel=preload href=a/b.css as=style crossorigin=anonymous><script id=s2>\
            var l = document.getElementById('l');\
            log.push(typeof l.href); log.push(l.href);\
            l.href = '/z.css'; log.push(l.getAttribute('href')); log.push(l.href);\
            log.push(l.as); l.as = 'script'; log.push(l.getAttribute('as'));\
            log.push(l.crossOrigin); l.crossOrigin = 'use-credentials'; log.push(l.getAttribute('crossorigin'));\
            var d = document.createElement('link'); log.push('[' + d.href + ']'); log.push('[' + d.as + ']'); log.push(String(d.crossOrigin));\
            var sc = document.scripts; log.push(sc.length + ':' + sc[0].id + ':' + sc[1].id);\
            document.body.appendChild(document.createElement('script')); log.push(document.scripts.length);</script>";
        let (log, _, _) = run(html, &srv);
        let base = srv.url("");
        assert_eq!(log, format!("string|{base}/a/b.css|/z.css|{base}/z.css|style|script|anonymous|use-credentials|[]|[]|null|2:s1:s2|3"));
    });
}

#[test]
fn an_as_that_is_set_later_starts_the_load_and_a_changed_href_loads_again() {
    big(|| {
        let srv = site(&[("/a.woff2", "font/woff2", "abc"), ("/b.woff2", "font/woff2", "abc")]);
        let html = "<script>var log = [];\
            var l = document.createElement('link'); l.rel = 'preload'; l.setAttribute('href', '/a.woff2');\
            l.addEventListener('load', function () { log.push('load:' + l.getAttribute('href')) });\
            document.head.appendChild(l);\
            setTimeout(function () { l.as = 'font' }, 10);\
            setTimeout(function () { l.setAttribute('href', '/b.woff2') }, 30);</script>";
        let (log, r, _) = run(html, &srv);
        assert_eq!(log, "load:/a.woff2|load:/b.woff2");
        assert_eq!((srv.hits("/a.woff2"), srv.hits("/b.woff2")), (1, 1));
        assert_eq!((r.preload.fetches, r.preload.loaded, r.preload.failed), (2, 2, 0));
    });
}

#[test]
fn the_fetch_budget_and_the_size_cap_give_error_events() {
    big(|| {
        let srv = site(&[("/big.woff2", "font/woff2", &"x".repeat(2000)), ("/a.woff2", "font/woff2", "a"), ("/b.woff2", "font/woff2", "b"), ("/c.woff2", "font/woff2", "c")]);
        let html = "<script>var log = [];\
            ['/big.woff2', '/a.woff2', '/b.woff2', '/c.woff2'].forEach(function (h) {\
              var l = document.createElement('link'); l.rel = 'preload'; l.as = 'font'; l.href = h;\
              l.addEventListener('load', function () { log.push('load:' + h) });\
              l.addEventListener('error', function () { log.push('error:' + h) });\
              document.head.appendChild(l);\
            });</script>";
        let mut lim = limits();
        lim.resources.max_fetches = 3;
        lim.resources.max_resource_bytes = 1000;
        let (log, r, _) = run_with(html, &srv, lim);
        assert_eq!(log, "error:/big.woff2|load:/a.woff2|load:/b.woff2|error:/c.woff2");
        assert_eq!((r.fetches, r.preload.loaded, r.preload.failed), (3, 2, 2));
        assert_eq!(srv.hits("/c.woff2"), 0);
    });
}
