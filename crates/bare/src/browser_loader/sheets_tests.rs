//! Tests of the style sheet loading of the driver (`sheets.rs`): real pages, a real realm, a loopback server with
//! throwaway synthetic CSS. The expected values are written by hand from the CSS cascade rules (later sheet wins a tie,
//! an unlayered rule beats a layered one, a rule of a layer beats the rules of its sub-layers). `direction` is the property
//! that the tests read: it is inherited, native, and the browser sheet sets it from the `dir` attribute.

use crate::browser::*;
use crate::browser_fetch::FetchLimits;
use crate::browser_loader::*;
use crate::browser_loader::Input;
use crate::browser_session::{Session, SessionConfig, SessionStop};
use crate::resources::Limits;
use crate::resources::test_server::{Reply, Server, Shape, serve};
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_net::http::Client;
use std::collections::HashMap;
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// A server over a table of `path -> reply`. Another path is a 404 text page.
type Route = (&'static str, fn() -> Reply);

fn site(routes: Vec<Route>) -> Server {
    let routes: HashMap<&'static str, fn() -> Reply> = routes.into_iter().collect();
    serve(move |path| match routes.get(path) {
        Some(f) => f(),
        None => Reply::new(404, Some("text/plain"), "no"),
    })
}

/// A server over CSS texts: each path is a 200 `text/css` reply.
fn css_site(files: &[(&'static str, &str)]) -> Server {
    let files: HashMap<&'static str, String> = files.iter().map(|(p, t)| (*p, t.to_string())).collect();
    serve(move |path| match files.get(path.split('?').next().unwrap()) {
        Some(t) => Reply::css(t.as_str()),
        None => Reply::new(404, Some("text/plain"), "no"),
    })
}

fn limits() -> PageLimits {
    PageLimits { quiet: Duration::from_millis(100), ..Default::default() }
}

fn browser(html: &str, srv: &Server, page: &str) -> (Browser, String) {
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    (b, srv.url(page))
}

/// Run `html` served at `/page.html` and give the log of the page, the result and the browser.
fn run_at(html: &str, srv: &Server, page: &str, lim: PageLimits) -> (String, PageResult, Browser) {
    let (mut b, url) = browser(html, srv, page);
    let r = b.run_page(PageConfig { url, client: Client::new(), limits: lim, fetch: Some((Client::new(), FetchLimits::default())) });
    let v = b.run_script("typeof log === 'undefined' ? 'no-log' : log.join('|')").unwrap();
    let out = b.it.display(v);
    (out, r, b)
}

fn run(html: &str, srv: &Server) -> (String, PageResult, Browser) {
    run_at(html, srv, "/page.html", limits())
}

/// The order in which the server saw the paths (queries removed).
fn order(srv: &Server) -> Vec<String> {
    srv.heads.lock().unwrap().iter().map(|h| h.split_whitespace().nth(1).unwrap().to_string()).collect()
}

const LOG: &str = "var log = [];";
fn dir(id: &str) -> String {
    format!("log.push('{id}=' + getComputedStyle(document.getElementById('{id}')).direction);")
}

#[test]
fn linked_css_is_in_place_before_the_first_script_reads_it() {
    big(|| {
        let srv = css_site(&[("/a.css", "#p { direction: rtl }")]);
        let html = format!("<head><link rel=stylesheet href=a.css></head><body><p id=p>x</p><script>{LOG}{}</script>", dir("p"));
        let (log, r, _) = run(&html, &srv);
        assert_eq!(log, "p=rtl");
        assert_eq!(r.stop, Stop::Settled);
        assert_eq!((r.css.fetches, r.css.loaded, r.css.failed, r.css.supplied), (1, 1, 0, 1));
        assert_eq!((r.scripts_run, r.script_throws, r.load_errors), (1, 0, 0));
        assert_eq!(r.fetches, 1);
    });
}

#[test]
fn ua_embedded_linked_and_inline_sheets_cascade_in_document_order() {
    big(|| {
        // a: only the browser sheet (`dir=rtl`). b: embedded ltr, then linked rtl (later wins). c: linked rtl, then embedded ltr.
        let srv = css_site(&[("/a.css", "#b, #c { direction: rtl }")]);
        let html = format!(
            "<style>#b {{ direction: ltr }}</style><link rel=stylesheet href=a.css><style>#c {{ direction: ltr }}</style>\
             <p id=a dir=rtl></p><p id=b></p><p id=c></p><script>{LOG}{}{}{}</script>",
            dir("a"), dir("b"), dir("c")
        );
        let (log, r, _) = run(&html, &srv);
        assert_eq!(log, "a=rtl|b=rtl|c=ltr");
        assert_eq!(r.stop, Stop::Settled);
    });
}

#[test]
fn relative_urls_nested_imports_layers_and_fetch_order() {
    big(|| {
        // /dir/css/a.css -> layer(x) /dir/css/b.css -> layer(y) /dir/c.css. A rule of layer x beats layer x.y. A rule that is not in a layer beats both.
        let srv = css_site(&[
            ("/dir/css/a.css", "@import url(b.css) layer(x); #q { direction: ltr } #r { direction: rtl }"),
            ("/dir/css/b.css", "@import \"../c.css\" layer(y); #p { direction: rtl }"),
            ("/dir/c.css", "#p { direction: ltr } #q { direction: rtl }"),
        ]);
        let html = format!(
            "<link rel=stylesheet href=css/a.css><p id=p></p><p id=q></p><p id=r></p><script>{LOG}{}{}{}</script>",
            dir("p"), dir("q"), dir("r")
        );
        let (log, r, _) = run_at(&html, &srv, "/dir/page.html", limits());
        assert_eq!(log, "p=rtl|q=ltr|r=rtl");
        assert_eq!(order(&srv), ["/dir/css/a.css", "/dir/css/b.css", "/dir/c.css"]);
        assert_eq!((r.css.fetches, r.css.supplied), (3, 1));
        assert_eq!(r.stop, Stop::Settled);
    });
}

#[test]
fn inline_style_import_uses_the_document_base_and_comes_before_the_text() {
    big(|| {
        let srv = css_site(&[("/base/i.css", "#p { direction: rtl } #q { direction: rtl }"), ("/base/s.css", "#q { direction: ltr }")]);
        let html = format!(
            "<head><base href=/base/><style>@import 'i.css'; #p {{ direction: ltr }}</style><link rel=stylesheet href=s.css></head>\
             <p id=p></p><p id=q></p><script>{LOG}{}{}</script>",
            dir("p"), dir("q")
        );
        let (log, _, _) = run(&html, &srv);
        // p: the text of the style comes after its import. q: the link comes after the import.
        assert_eq!(log, "p=ltr|q=ltr");
        assert_eq!(order(&srv), ["/base/i.css", "/base/s.css"]);
    });
}

#[test]
fn one_url_is_one_fetch_but_each_reference_has_its_place() {
    big(|| {
        // The same sheet, written two ways, is linked before and after an embedded rule. The text is fetched once.
        let srv = css_site(&[("/a.css", "#p { direction: rtl }")]);
        let html = format!(
            "<link rel=stylesheet href=a.css><style>#p {{ direction: ltr }}</style><p id=p></p><script>{LOG}{}</script>\
             <link rel=stylesheet href=./a.css>",
            dir("p")
        );
        let (log, r, b) = run(&html, &srv);
        // The second link is in the document when the script runs (a pre-parsed DOM), so it comes after the style: rtl.
        assert_eq!(log, "p=rtl");
        assert_eq!(srv.hits("/a.css"), 1);
        assert_eq!((r.css.fetches, r.css.supplied), (1, 2));
        assert_eq!(b.computed_stats().external_sheets, 2);
    });
}

#[test]
fn an_import_cycle_is_cut_and_counted() {
    big(|| {
        let srv = css_site(&[("/a.css", "@import 'b.css'; #p { direction: rtl }"), ("/b.css", "@import 'a.css'; #p { direction: ltr }")]);
        let html = format!("<link rel=stylesheet href=a.css><p id=p></p><script>{LOG}{}</script>", dir("p"));
        let (log, r, _) = run(&html, &srv);
        assert_eq!(log, "p=rtl");
        assert_eq!((srv.hits("/a.css"), srv.hits("/b.css")), (1, 1));
        assert_eq!(r.css.cycles, 1);
        assert_eq!(r.stop, Stop::Settled);
    });
}

#[test]
fn an_import_chain_that_is_too_deep_stops_the_page_before_any_script() {
    big(|| {
        let files: Vec<(&'static str, String)> = (0..12).map(|i| (Box::leak(format!("/d{i}.css").into_boxed_str()) as &'static str, format!("@import 'd{}.css';", i + 1))).collect();
        let files: Vec<(&'static str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
        let srv = css_site(&files);
        let html = format!("<link rel=stylesheet href=d0.css><p id=p></p><script>{LOG}{}</script>", dir("p"));
        let (log, r, _) = run(&html, &srv);
        assert_eq!(r.stop, Stop::Fatal(Fatal::Limit("css-import-depth".into())));
        assert_eq!(r.scripts_run, 0);
        assert_eq!(log, "no-log");
        assert!(srv.total() <= 9, "no fetch past the depth: {}", srv.total());
    });
}

#[test]
fn a_failed_sheet_has_no_rules_but_is_counted_and_gets_an_error_event() {
    big(|| {
        let srv = site(vec![
            ("/gone.css", || Reply::new(404, Some("text/css"), "#p { direction: rtl }")),
            ("/plain.css", || Reply::new(200, Some("text/plain"), "#p { direction: rtl }")),
            ("/none.css", || Reply::new(200, None, "#p { direction: rtl }")),
            ("/gz.css", || Reply::new(200, Some("text/css"), "#p { direction: rtl }").header("Content-Encoding", "zstd")),
            ("/cut.css", || Reply::css("#p { direction: rtl }").shaped(Shape::Truncated(50))),
            ("/blank.css", || Reply::css("  /* nothing */\n")),
        ]);
        let html = format!(
            "<link rel=stylesheet href=gone.css><link rel=stylesheet href=plain.css><link rel=stylesheet href=none.css>\
             <link rel=stylesheet href=gz.css><link rel=stylesheet href=cut.css><link rel=stylesheet href=blank.css>\
             <link rel=stylesheet href='http://127.0.0.1:1/closed.css'><p id=p></p>\
             <script>{LOG} {}</script>",
            dir("p")
        );
        let (log, r, _) = run(&html, &srv);
        // Real failures give no rules (the default direction): never a rule of a body that was refused.
        assert_eq!(log, "p=ltr");
        assert_eq!(r.stop, Stop::Settled);
        assert_eq!((r.css.fetches, r.css.loaded, r.css.blank, r.css.failed), (7, 1, 1, 6));
        assert_eq!((r.load_errors, r.script_throws), (0, 0), "a style sheet failure is not a script failure");
        for what in ["status 404", "content type", "content encoding", "cut short", "network error"] {
            assert!(r.diagnostics.iter().any(|d| d.contains("style sheet failed") && d.contains(what)), "{what}: {:?}", r.diagnostics);
        }
        assert!(r.diagnostics.iter().all(|d| !d.contains("JavaScript")), "{:?}", r.diagnostics);
    });
}

#[test]
fn link_events_report_how_each_sheet_ended() {
    big(|| {
        let srv = site(vec![("/ok.css", || Reply::css("p { direction: rtl }")), ("/no.css", || Reply::new(500, Some("text/css"), ""))]);
        // A script adds the links and their handlers; the driver fires the events at its next boundary (a task of its own).
        let html = "<script>var log = [];\
            function add(href) { var l = document.createElement('link'); l.rel = 'stylesheet'; l.setAttribute('href', href);\
              l.addEventListener('load', function () { log.push('load:' + href) }); l.addEventListener('error', function () { log.push('error:' + href) }); document.head.appendChild(l); log.push('added:' + href) }\
            add('ok.css'); add('no.css');</script>";
        let (log, r, _) = run(html, &srv);
        assert_eq!(log, "added:ok.css|added:no.css|load:ok.css|error:no.css");
        assert_eq!((r.css.loaded, r.css.failed), (1, 1));
        assert_eq!(r.stop, Stop::Settled);
    });
}

#[test]
fn a_charset_and_the_size_of_the_text_are_applied_like_a_script() {
    big(|| {
        // 0xE9 is `é` in latin-1 and not valid UTF-8; it sits in a comment, so only the decode is tested.
        let srv = site(vec![("/l1.css", || Reply::new(200, Some("text/css; charset=iso-8859-1"), b"/* caf\xe9 */ #p { direction: rtl }".to_vec()))]);
        let html = format!("<link rel=stylesheet href=l1.css><p id=p></p><script>{LOG}{}</script>", dir("p"));
        let (log, r, _) = run(&html, &srv);
        assert_eq!(log, "p=rtl");
        assert_eq!(r.css.bytes, "/* café */ #p { direction: rtl }".len());
    });
}

#[test]
fn a_spent_budget_stops_the_page_before_later_scripts_and_requests() {
    big(|| {
        // `a.css` imports `b.css`: discovery sees one URL, the driver needs two downloads.
        let srv = css_site(&[("/a.css", "@import 'b.css'; p{}"), ("/b.css", "p{}"), ("/s.js", "")]);
        let html = "<link rel=stylesheet href=a.css><script>var log = ['ran']</script><script>fetch('/s.js')</script>";
        // Fetch budget: one download only.
        let lim = PageLimits { resources: Limits { max_fetches: 1, ..Limits::default() }, ..limits() };
        let (log, r, _) = run_at(html, &srv, "/page.html", lim);
        assert_eq!(r.stop, Stop::Fatal(Fatal::Limit("css-fetches".into())));
        assert_eq!((log.as_str(), r.scripts_run), ("no-log", 0));
        assert_eq!((srv.hits("/b.css"), srv.hits("/s.js")), (0, 0));
        // Total bytes: the second sheet does not fit.
        let lim = PageLimits { resources: Limits { max_total_bytes: 22, ..Limits::default() }, ..limits() };
        let (_, r, _) = run_at(html, &srv, "/page.html", lim);
        assert_eq!(r.stop, Stop::Fatal(Fatal::Limit("css-total-bytes".into())));
        assert_eq!(r.scripts_run, 0);
        // One body.
        let lim = PageLimits { resources: Limits { max_resource_bytes: 2, ..Limits::default() }, ..limits() };
        let (_, r, mut b) = run_at(html, &srv, "/page.html", lim);
        assert_eq!(r.stop, Stop::Fatal(Fatal::Limit("css-resource-bytes".into())));
        // The stop is sticky: a second run does no I/O and no JS.
        let before = srv.total();
        let again = b.run_page(PageConfig { url: srv.url("/page.html"), client: Client::new(), limits: limits(), fetch: None });
        assert_eq!(again.stop, r.stop);
        assert_eq!(srv.total(), before);
    });
}

#[test]
fn a_page_on_https_never_asks_for_an_http_sheet() {
    big(|| {
        let srv = css_site(&[("/a.css", "#p { direction: rtl }")]);
        let html = format!(
            "<link rel=stylesheet href='{}'><p id=p></p><script>{LOG}{}</script>",
            srv.url("/a.css"),
            dir("p")
        );
        let (mut b, _) = browser(&html, &srv, "/page.html");
        let url = format!("https://127.0.0.1:{}/page.html", srv.port);
        let r = b.run_page(PageConfig { url, client: Client::new(), limits: limits(), fetch: None });
        assert_eq!(srv.total(), 0, "no request went out");
        assert_eq!((r.css.failed, r.css.fetches), (1, 0), "refused before a request: no fetch is counted");
        assert!(r.diagnostics.iter().any(|d| d.contains("insecure")), "{:?}", r.diagnostics);
        let v = b.run_script("log.join('|')").unwrap();
        assert_eq!(b.it.display(v), "p=ltr");
    });
}

#[test]
fn a_sheet_that_a_script_adds_is_loaded_at_the_next_boundary_and_not_inside_the_read() {
    big(|| {
        let srv = css_site(&[("/dyn.css", "#p { direction: rtl }")]);
        let html = format!(
            "<p id=p></p><script>{LOG}\
               var l = document.createElement('link'); l.rel = 'stylesheet'; l.setAttribute('href', 'dyn.css'); document.head.appendChild(l);\
               setTimeout(function () {{ {} }}, 0);</script>",
            dir("p")
        );
        let (log, r, _) = run(&html, &srv);
        assert_eq!(log, "p=rtl");
        assert_eq!(srv.hits("/dyn.css"), 1);
        assert_eq!(r.stop, Stop::Settled);
        // In the same task there is no I/O, and the sheet has no rules yet (the next test). A link with a `media` text may be a link
        // that a script just turned on, which Chrome had loaded: that read still stops the page.
        let html = format!(
            "<p id=p></p><script>{LOG}var l = document.createElement('link'); l.rel = 'stylesheet'; l.setAttribute('media', 'all'); l.setAttribute('href', 'dyn.css'); document.head.appendChild(l); {}</script>",
            dir("p")
        );
        let srv = css_site(&[("/dyn.css", "#p { direction: rtl }")]);
        let (_, r, _) = run(&html, &srv);
        assert!(matches!(r.stop, Stop::Fatal(Fatal::Unsupported(w)) if w.contains("external stylesheet")), "{:?}", r.stop);
        assert_eq!(srv.hits("/dyn.css"), 0);
    });
}

/// Chrome (B1): a link whose `href` a script changed keeps the sheet that it had until the new one has loaded. A new link beside
/// it has no sheet yet, and the sheet that is in place still counts (J1).
#[test]
fn a_link_keeps_its_old_sheet_while_the_new_href_loads() {
    big(|| {
        let srv = css_site(&[("/one.css", "#p { direction: rtl }"), ("/two.css", "#q { direction: rtl }")]);
        let html = format!(
            "<link id=l rel=stylesheet href=one.css><p id=p></p><p id=q></p><script>{LOG}\
               document.getElementById('l').setAttribute('href', 'two.css'); {} {}\
               setTimeout(function () {{ {} {} }}, 0);</script>",
            dir("p"), dir("q"), dir("p"), dir("q")
        );
        let (log, r, _) = run(&html, &srv);
        assert_eq!(log, "p=rtl|q=ltr|p=ltr|q=rtl", "{:?}", r.stop);
        assert_eq!((r.stop, srv.hits("/one.css"), srv.hits("/two.css")), (Stop::Settled, 1, 1));
        // A second link is new: only its own rules wait.
        let html = format!(
            "<link rel=stylesheet href=one.css><p id=p></p><p id=q></p><script>{LOG}\
               var l = document.createElement('link'); l.rel = 'stylesheet'; l.setAttribute('href', 'two.css'); document.head.appendChild(l); {} {}\
               setTimeout(function () {{ {} {} }}, 0);</script>",
            dir("p"), dir("q"), dir("p"), dir("q")
        );
        let (log, r, _) = run(&html, &srv);
        assert_eq!(log, "p=rtl|q=ltr|p=rtl|q=rtl", "{:?}", r.stop);
    });
}

/// Chrome (D1, D2): a style preload that has loaded is in the document cache. A stylesheet link for the same `href`, or the preload
/// link with `rel` changed to `stylesheet` (the way React does it), has the sheet in the same task. Before the preload loads it has not.
#[test]
fn a_loaded_style_preload_gives_its_sheet_to_a_stylesheet_link_at_once() {
    big(|| {
        let srv = css_site(&[("/pre.css", "#p { direction: rtl }")]);
        let html = format!(
            "<p id=p></p><script>{LOG}\
               var k = document.createElement('link'); k.rel = 'preload'; k.as = 'style'; k.setAttribute('href', 'pre.css');\
               k.addEventListener('load', function () {{\
                 log.push('preload'); {}\
                 var l = document.createElement('link'); l.rel = 'stylesheet'; l.setAttribute('href', 'pre.css'); document.head.appendChild(l); {}\
               }});\
               document.head.appendChild(k); {}</script>",
            dir("p"), dir("p"), dir("p")
        );
        let (log, r, _) = run(&html, &srv);
        assert_eq!(log, "p=ltr|preload|p=ltr|p=rtl", "{:?}", r.stop);
        assert_eq!((r.stop, srv.hits("/pre.css")), (Stop::Settled, 1));
        // The same link, with `rel` changed in its own `load`.
        let html = format!(
            "<p id=p></p><script>{LOG}\
               var k = document.createElement('link'); k.rel = 'preload'; k.as = 'style'; k.setAttribute('href', 'pre.css');\
               k.addEventListener('load', function () {{ if (k.rel == 'preload') {{ k.rel = 'stylesheet'; {} }} }});\
               document.head.appendChild(k);</script>",
            dir("p")
        );
        let (log, r, _) = run(&html, &srv);
        assert_eq!(log, "p=rtl", "{:?}", r.stop);
        assert_eq!((r.stop, srv.hits("/pre.css")), (Stop::Settled, 2));
    });
}

/// Chrome (A2, I1): a link that fails has no rules and an `error`. An inline `<style>` that a script adds works at once, and only
/// its `@import` waits.
#[test]
fn a_failed_new_link_and_a_new_style_with_an_import_read_without_the_pending_part() {
    big(|| {
        let srv = css_site(&[("/imp.css", "#p { direction: rtl }")]);
        let html = format!(
            "<p id=p></p><p id=q></p><script>{LOG}\
               var l = document.createElement('link'); l.rel = 'stylesheet'; l.setAttribute('href', 'missing.css');\
               l.addEventListener('error', function () {{ log.push('error'); }}); document.head.appendChild(l); {}\
               var s = document.createElement('style'); s.textContent = '@import url(imp.css); #q {{ direction: rtl }}'; document.head.appendChild(s); {} {}\
               setTimeout(function () {{ {} {} }}, 0);</script>",
            dir("p"), dir("p"), dir("q"), dir("p"), dir("q")
        );
        let (log, r, _) = run(&html, &srv);
        assert_eq!(log, "p=ltr|p=ltr|q=rtl|error|p=rtl|q=rtl", "{:?}", r.stop);
        assert_eq!((r.stop, r.css.failed, srv.hits("/imp.css")), (Stop::Settled, 1, 1));
    });
}

/// A link that was off because of its `media` and that a script turns on may have had its sheet in Chrome: it is not read as empty.
#[test]
fn a_media_that_turns_a_link_on_in_the_same_task_stops_the_page() {
    big(|| {
        let srv = css_site(&[("/print.css", "#p { direction: rtl }")]);
        let html = format!(
            "<link id=l rel=stylesheet href=print.css media=print><p id=p></p><script>{LOG}{}\
               document.getElementById('l').setAttribute('media', 'all'); {}</script>",
            dir("p"), dir("p")
        );
        let (log, r, _) = run(&html, &srv);
        assert_eq!(log, "p=ltr");
        assert!(matches!(r.stop, Stop::Fatal(Fatal::Unsupported(w)) if w.contains("external stylesheet")), "{:?}", r.stop);
    });
}

/// The pattern of the staging login, written like React 19 and the Next.js App Router do it (names are those of their code): on a
/// client-side navigation React preloads the route sheet (`preload`, `as=style`), then, in its commit, puts in a
/// `<link rel=stylesheet data-precedence>` and `InnerScrollAndFocusHandler.handlePotentialScroll` reads `getComputedStyle(node).position`
/// of the first node of the new segment (`shouldSkipElement`: a `sticky` or `fixed` node is skipped). That read is in the task of the
/// insert. Chrome has no rules of the new sheet there, so the node is not skipped. After the sheet loads, it is skipped.
#[test]
fn a_react_like_route_commit_reads_position_in_the_task_that_inserts_the_route_sheet() {
    big(|| {
        let srv = css_site(&[("/route.css", "#first { position: sticky }"), ("/other.css", "#first { position: fixed }")]);
        let html = format!(
            "<main id=first></main><section id=second></section><script>{LOG}\
               function shouldSkipElement(e) {{ return ['sticky', 'fixed'].includes(getComputedStyle(e).position); }}\
               function handlePotentialScroll() {{ var r = document.getElementById('first'); while (shouldSkipElement(r)) {{ r = r.nextElementSibling; }} log.push('focus ' + r.id); }}\
               function insertStylesheet(href) {{ var l = document.createElement('link'); l.rel = 'stylesheet'; l.setAttribute('href', href); l.setAttribute('data-precedence', 'next'); document.head.appendChild(l); return l; }}\
               function preload(href) {{ var l = document.createElement('link'); l.rel = 'preload'; l.as = 'style'; l.setAttribute('href', href); document.head.appendChild(l); return l; }}\
               var p = preload('route.css'); p.addEventListener('load', function () {{ log.push('preload loaded'); }});\
               /* A commit that does not wait for the sheet (a sync update). */\
               var o = insertStylesheet('other.css'); handlePotentialScroll(); o.addEventListener('load', function () {{ log.push('other loaded'); handlePotentialScroll(); }});\
               /* A transition: the commit waits for the load of the route sheet, then reads. */\
               setTimeout(function () {{ var r = insertStylesheet('route.css'); r.addEventListener('load', function () {{ log.push('route loaded'); handlePotentialScroll(); }}); }}, 10);</script>"
        );
        let (log, r, _) = run(&html, &srv);
        assert_eq!(log, "focus first|preload loaded|other loaded|focus second|route loaded|focus second", "{:?}", r.stop);
        assert_eq!((r.stop, r.script_throws, srv.hits("/route.css"), srv.hits("/other.css")), (Stop::Settled, 0, 1, 1));
    });
}

#[test]
fn media_href_and_base_changes_use_the_right_sheet() {
    big(|| {
        let srv = css_site(&[
            ("/a/one.css", "#p { direction: rtl }"),
            ("/a/two.css", "#p { direction: ltr } #q { direction: rtl }"),
            ("/a/print.css", "#r { direction: rtl }"),
            ("/b/one.css", "#p { direction: ltr } #r { direction: rtl }"),
        ]);
        let html = format!(
            "<base id=base href=/a/><link id=l rel=stylesheet href=one.css><link id=m rel=stylesheet href=print.css media=print>\
             <p id=p></p><p id=q></p><p id=r></p><script>{LOG}\
              function at(n, f) {{ setTimeout(f, n * 50) }}\
              function set(id, name, v) {{ document.getElementById(id).setAttribute(name, v) }}\
              at(1, function () {{ {} {} {} set('l', 'href', 'two.css') }});\
              at(2, function () {{ {} {} {} set('m', 'media', 'all') }});\
              at(3, function () {{ {} set('l', 'href', 'one.css'); set('base', 'href', '/b/') }});\
              at(4, function () {{ {} {} {} }});\
             </script>",
            dir("p"), dir("q"), dir("r"), dir("p"), dir("q"), dir("r"), dir("r"), dir("p"), dir("q"), dir("r")
        );
        let (log, r, _) = run(&html, &srv);
        // 1: /a/one.css; the print sheet is off. 2: /a/two.css. 3: media=all turned print.css on: r is rtl.
        // 4: base /b/: one.css is /b/one.css (p ltr, r rtl); /b/print.css is a 404, so it has no rules.
        assert_eq!(log, "p=rtl|q=ltr|r=ltr|p=ltr|q=rtl|r=ltr|r=rtl|p=ltr|q=ltr|r=rtl", "{r:?}");
        let hits = ["/a/one.css", "/a/two.css", "/a/print.css", "/b/one.css", "/b/print.css"].map(|p| srv.hits(p));
        assert_eq!(hits, [1, 1, 1, 1, 1]);
        assert_eq!((r.css.failed, r.stop), (1, Stop::Settled));
    });
}

#[test]
fn a_node_removed_before_the_pump_is_not_fetched_and_a_navigation_stops_the_io() {
    big(|| {
        let srv = css_site(&[("/gone.css", "p{}"), ("/nav.css", "p{}")]);
        let html = "<script>\
            var l = document.createElement('link'); l.rel = 'stylesheet'; l.href = 'gone.css'; document.head.appendChild(l); l.remove();\
            var n = document.createElement('link'); n.rel = 'stylesheet'; n.href = 'nav.css'; document.head.appendChild(n); location.assign('/next.html');</script>";
        let (_, r, _) = run(html, &srv);
        assert!(matches!(r.stop, Stop::Navigate(_)), "{:?}", r.stop);
        assert_eq!(srv.hits("/gone.css") + srv.hits("/nav.css"), 0);
        assert_eq!(r.css.fetches, 0);
    });
}

#[test]
fn a_session_gives_each_document_a_fresh_realm_and_a_resume_keeps_the_budget() {
    big(|| {
        let srv = serve(|path| match path {
            "/p1.html" => Reply::new(200, Some("text/html"), "<link rel=stylesheet href=a.css><p id=p></p><button id=go>go</button>\
                <script>var log=[]; log.push(getComputedStyle(document.getElementById('p')).direction);\
                document.getElementById('go').addEventListener('click', function () { var l=document.createElement('link'); l.rel='stylesheet'; l.setAttribute('href','r.css'); document.head.appendChild(l) });\
                setTimeout(function(){ location.assign('/p2.html') }, 5000)</script>"),
            "/p2.html" => Reply::new(200, Some("text/html"), "<link rel=stylesheet href=a.css><p id=p></p><script>var log=[getComputedStyle(document.getElementById('p')).direction]</script>"),
            "/a.css" => Reply::css("#p { direction: rtl }"),
            "/r.css" => Reply::css("#p { direction: ltr }"),
            _ => Reply::new(404, Some("text/plain"), "no"),
        });
        let mut cfg = SessionConfig::new(&srv.url("/p1.html"));
        cfg.virtual_clock = true;
        cfg.page.quiet = Duration::from_millis(100);
        cfg.page.resources.max_fetches = 2;
        let mut s = Session::new(cfg);
        // The first document settles (its navigation timer is far away): one document so far.
        let r = s.run();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(r.pages.len(), 1);
        assert_eq!((r.pages[0].css.fetches, r.pages[0].fetches), (1, 1));
        let first = s.browser().unwrap().run_script("log.join('|')").unwrap();
        let first = s.browser().unwrap().it.display(first);
        assert_eq!(first, "rtl");
        // A click adds a sheet: it is fetched at the boundary, and the fetch counter goes on from 1 to 2.
        let r = s.resume(&[Input::Click("#go")]);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!((r.pages[0].css.fetches, r.pages[0].fetches), (2, 2));
        // A second click adds the same URL: it is in the cache of the document, so the budget of 2 is not touched.
        let r = s.resume(&[Input::Click("#go")]);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!((r.pages[0].css.fetches, r.pages[0].fetches), (2, 2));
        assert_eq!(srv.hits("/r.css"), 1);
    });
}

#[test]
fn a_redirected_sheet_is_checked_at_its_final_answer() {
    big(|| {
        let srv = serve(|path| match path {
            "/go.css" => Reply::new(302, None, "").header("Location", "/real.css"),
            "/bad.css" => Reply::new(302, None, "").header("Location", "/html.css"),
            "/real.css" => Reply::css("#p { direction: rtl }"),
            "/html.css" => Reply::new(200, Some("text/html"), "#p { direction: rtl }"),
            _ => Reply::new(404, Some("text/plain"), "no"),
        });
        let html = format!("<link rel=stylesheet href=go.css><link rel=stylesheet href=bad.css><p id=p></p><script>{LOG}{}</script>", dir("p"));
        let (log, r, _) = run(&html, &srv);
        // The first sheet is the body of its redirect; the second ends on a page that is not CSS, so it is a failure with no rules.
        assert_eq!(log, "p=rtl");
        assert_eq!((r.css.loaded, r.css.failed), (1, 1));
        assert!(r.diagnostics.iter().any(|d| d.contains("bad.css") && d.contains("content type")), "{:?}", r.diagnostics);
    });
}

#[test]
fn a_navigation_makes_a_fresh_cache_and_fresh_counters() {
    big(|| {
        let srv = serve(|path| match path {
            "/p1.html" => Reply::new(200, Some("text/html"), "<link rel=stylesheet href=a.css><script>location.assign('/p2.html')</script>"),
            "/p2.html" => Reply::new(200, Some("text/html"), "<link rel=stylesheet href=a.css><p id=p></p><script>var log=[getComputedStyle(document.getElementById('p')).direction]</script>"),
            "/a.css" => Reply::css("#p { direction: rtl }"),
            _ => Reply::new(404, Some("text/plain"), "no"),
        });
        let mut cfg = SessionConfig::new(&srv.url("/p1.html"));
        cfg.virtual_clock = true;
        cfg.page.resources.max_fetches = 1;
        let mut s = Session::new(cfg);
        let r = s.run();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(r.pages.len(), 2);
        // The first document never reached its scripts' next boundary with a task left: its sheet was fetched before the script ran.
        assert_eq!((r.pages[0].css.fetches, r.pages[1].css.fetches, r.pages[1].fetches), (1, 1, 1));
        assert_eq!(srv.hits("/a.css"), 2, "a cache is per document");
        let v = s.browser().unwrap().run_script("log.join('|')").unwrap();
        assert_eq!(s.browser().unwrap().it.display(v), "rtl");
    });
}

#[test]
fn a_resume_cannot_reset_the_fetch_budget() {
    big(|| {
        let srv = serve(|path| match path {
            "/p.html" => Reply::new(200, Some("text/html"), "<link rel=stylesheet href=a.css><button id=go>go</button>\
                <script>document.getElementById('go').addEventListener('click', function () { var l=document.createElement('link'); l.rel='stylesheet'; l.setAttribute('href','b.css'); document.head.appendChild(l) })</script>"),
            "/a.css" | "/b.css" => Reply::css("p{}"),
            _ => Reply::new(404, Some("text/plain"), "no"),
        });
        let mut cfg = SessionConfig::new(&srv.url("/p.html"));
        cfg.virtual_clock = true;
        cfg.page.resources.max_fetches = 1;
        let mut s = Session::new(cfg);
        assert_eq!(s.run().stop, SessionStop::Page(Stop::Settled));
        let r = s.resume(&[Input::Click("#go")]);
        assert_eq!(r.stop, SessionStop::Page(Stop::Fatal(Fatal::Limit("css-fetches".into()))), "{r:?}");
        assert_eq!(srv.hits("/b.css"), 0);
        // The session is ended for good: no request after the stop.
        let total = srv.total();
        assert_eq!(s.resume(&[]).stop, r.stop);
        assert_eq!(srv.total(), total);
    });
}

#[test]
fn imports_use_the_redirected_stylesheet_url_as_their_base() {
    big(|| {
        let srv = serve(|path| match path {
            "/go.css" => Reply::new(302, None, "").header("Location", "/nested/real.css"),
            "/nested/real.css" => Reply::css("@import 'child.css';"),
            "/nested/child.css" => Reply::css("#p { direction: rtl }"),
            _ => Reply::new(404, Some("text/plain"), "no"),
        });
        let html = format!("<link rel=stylesheet href=go.css><p id=p></p><script>{LOG}{}</script>", dir("p"));
        let (log, _, _) = run(&html, &srv);
        assert_eq!(log, "p=rtl");
        assert_eq!(srv.hits("/child.css"), 0);
        assert_eq!(srv.hits("/nested/child.css"), 1);
    });
}

#[test]
fn stylesheet_discovery_limits_keep_scripts_and_skip_excess_sources() {
    big(|| {
        let srv = site(vec![]);
        let (mut b, url) = browser("<link rel=stylesheet href=a.css><script>globalThis.ran=1</script>", &srv, "/page.html");
        b.set_computed_limits(crate::browser_computed::ComputedLimits { max_sources: 0, ..Default::default() });
        let r=b.run_page(PageConfig {url, client: Client::new(), limits: limits(), fetch: None});
        assert_eq!(r.stop, Stop::Settled);
        assert_eq!(r.scripts_run, 1);
        let value = b.run_script("globalThis.ran").unwrap();
        assert_eq!(b.it.display(value), "1");
        assert_eq!(srv.hits("/a.css"), 0);

        let (mut b, url) = browser("<style>p{direction:rtl}</style><link rel=stylesheet href=a.css><p id=p></p><script>globalThis.ran=getComputedStyle(document.getElementById('p')).direction</script>", &srv, "/page.html");
        b.set_computed_limits(crate::browser_computed::ComputedLimits { max_sources: 1, ..Default::default() });
        let r = b.run_page(PageConfig { url, client: Client::new(), limits: limits(), fetch: None });
        assert_eq!(r.stop, Stop::Settled);
        assert_eq!(r.scripts_run, 1);
        let value = b.run_script("globalThis.ran").unwrap();
        assert_eq!(b.it.display(value), "rtl");
        assert_eq!(srv.hits("/a.css"), 0);
    });
}

#[test]
fn cached_sheet_notifies_new_active_links_but_not_inactive_links() {
    big(|| {
        let srv = site(vec![("/a.css", || Reply::css("p {direction:rtl}"))]);
        let html = "<script>var log=[]; function link(id,media){var l=document.createElement('link');l.setAttribute('rel','stylesheet');l.setAttribute('href','a.css');l.setAttribute('media',media);l.addEventListener('load',function(){log.push(id+':'+l.getAttribute('media'))});document.head.appendChild(l);return l} var a=link('a','all'), b=link('b','not all'); setTimeout(function(){b.setAttribute('media','all')},0);</script>";
        let (log,_,_) = run(html,&srv);
        assert_eq!(log,"a:all|b:all");
        assert_eq!(srv.hits("/a.css"),1);
    });
}

#[test]
fn a_url_in_a_sheet_is_made_absolute_against_the_sheet() {
    use super::sheets::absolute_urls;
    let base = bare_url::Url::parse("https://a.test/w/load.php?x=1").unwrap();
    let abs = |t: &str| absolute_urls(t, &base).into_owned();
    assert_eq!(abs("a{background-image:url(/w/i.svg?8)}"), "a{background-image:url(\"https://a.test/w/i.svg?8\")}");
    assert_eq!(abs("a{mask:URL( 'i/b.png' ) no-repeat}b{x:url(\"../c.png\")}"), "a{mask:url(\"https://a.test/w/i/b.png\") no-repeat}b{x:url(\"https://a.test/c.png\")}");
    // Left as they are: data, a fragment, an absolute address, an escape, a url( with no end, and text with no url.
    for same in ["a{b:url(data:image/svg+xml,%3csvg/%3e)}", "a{b:url(\"DATA:x,y\")}", "a{b:url(#x)}", "a{b:url(https://c.test/d.png)}", "a{b:url(a\\ b)}", "a{b:url(x", "a{b:red}", "url(", ""] {
        assert_eq!(abs(same), same);
    }
    // The text is borrowed when nothing changes.
    assert!(matches!(absolute_urls("a{b:url(#x)}", &base), std::borrow::Cow::Borrowed(_)));
    // The address of an `@import` is joined by `flatten`: it stays, with any case and white space before `url(`. The next `url(` is made absolute.
    assert_eq!(abs("@import url(/s.css) layer(x);a{b:url(/i.png)}"), "@import url(/s.css) layer(x);a{b:url(\"https://a.test/i.png\")}");
    assert_eq!(abs("@IMPORT\n url('s.css');"), "@IMPORT\n url('s.css');");
}
