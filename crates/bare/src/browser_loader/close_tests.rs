//! A page that loads from servers that close connections: after each reply (`Connection: close`), HTTP/1.0 with a body that ends
//! at the close, a close with no header (found on reuse), a cap of 2 connections, and a slow server that mixes these. Each case
//! checks what the page ran and read (script order, the import cascade), that each URL reached the server once, and that the
//! page ends. The server is `crates/net/tests/common/quirky.rs`.

#[path = "../../../net/tests/common/quirky.rs"]
mod quirky;

use crate::browser::*;
use crate::browser_fetch::FetchLimits;
use crate::browser_loader::*;
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_net::http::Client;
use quirky::{Config, Limit, Server, Style, start};
use std::sync::atomic::Ordering;
use std::time::Duration;

/// A page that hangs fails after this long. A run takes well under a second.
const HANG: Duration = Duration::from_secs(30);

/// Style sheets in the order of the cascade (imports before the sheet that imports them): the tree of `root.css` and then two links.
const ORDER: [&str; 10] = ["a1", "a2", "a", "b1", "b2", "b", "root", "c0", "c1", "c2"];
/// `@import` lines of a sheet.
fn imports(name: &str) -> &'static [&'static str] {
    match name {
        "root" => &["a", "b"],
        "a" => &["a1", "a2"],
        "b" => &["b1", "b2"],
        _ => &[],
    }
}

/// The text of a sheet. Element `pI` is `rtl` from the sheet at place I of `ORDER` and `ltr` from the sheet at place I + 1, so it is
/// `ltr` only when both loaded and the later one is later in the cascade.
fn sheet(name: &str) -> String {
    let at = ORDER.iter().position(|n| *n == name).unwrap();
    let mut css: String = imports(name).iter().map(|i| format!("@import '/{i}.css';")).collect();
    if at + 1 < ORDER.len() {
        css += &format!("#p{at}{{direction:rtl}}");
    }
    if at > 0 {
        css += &format!("#p{}{{direction:ltr}}", at - 1);
    }
    css
}

const PARSED: usize = 6;
const DYNAMIC: usize = 4;

fn pages(method: &str, path: &str) -> Option<(&'static str, Vec<u8>)> {
    let _ = method;
    if path == "/page.html" {
        let probes: String = (0..ORDER.len() - 1).map(|i| format!("<p id=p{i}></p>")).collect();
        let parsed: String = (0..PARSED).map(|i| format!("<script src=/s{i}.js></script>")).collect();
        let html = format!(
            "<head><link rel=stylesheet href=/root.css><link rel=stylesheet href=/c0.css><link rel=stylesheet href=/c1.css><link rel=stylesheet href=/c2.css>\
             <script>var log = []</script></head><body>{probes}{parsed}\
             <script>for (var i = 0; i < {DYNAMIC}; i++) {{ var s = document.createElement('script'); s.src = '/d' + i + '.js'; document.head.appendChild(s) }}</script>"
        );
        return Some(("text/html", html.into_bytes()));
    }
    let name = path.trim_start_matches('/');
    if let Some(n) = name.strip_suffix(".js") {
        return Some(("application/javascript", format!("log.push('{n}')").into_bytes()));
    }
    let name = name.strip_suffix(".css")?;
    ORDER.contains(&name).then(|| ("text/css", sheet(name).into_bytes()))
}

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().stack_size(64 << 20).spawn(move || drop(tx.send(f()))).unwrap();
    rx.recv_timeout(HANG).expect("the page hangs")
}

/// Load the page. Gives what the page ran and read, and the result.
fn load(server: &Server) -> (String, String, PageResult) {
    let url = server.url("/page.html");
    big(move || {
        let html = {
            let (_, body) = pages("GET", "/page.html").unwrap();
            String::from_utf8(body).unwrap()
        };
        let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).expect("browser");
        b.use_virtual_clock();
        let limits = PageLimits { quiet: Duration::from_millis(100), ..Default::default() };
        let r = b.run_page(PageConfig { url, client: Client::new(), limits, fetch: Some((Client::new(), FetchLimits::default())) });
        let v = b.run_script("log.join('|')").unwrap();
        let log = b.it.display(v);
        let ids: Vec<String> = (0..ORDER.len() - 1).map(|i| format!("'p{i}'")).collect();
        let v = b.run_script(&format!("[{}].map(function (i) {{ return getComputedStyle(document.getElementById(i)).direction }}).join()", ids.join(","))).unwrap();
        (log, b.it.display(v), r)
    })
}

/// Everything that a page needs: 10 sheets, 6 parsed scripts and 4 dynamic ones, each once.
fn check(server: &Server, (log, cascade, r): (String, String, PageResult), what: &str) {
    let scripts: Vec<String> = (0..PARSED).map(|i| format!("s{i}")).chain((0..DYNAMIC).map(|i| format!("d{i}"))).collect();
    assert_eq!(log, scripts.join("|"), "{what}: script order, {r:?}");
    assert_eq!(cascade, vec!["ltr"; ORDER.len() - 1].join(","), "{what}: import cascade, {r:?}");
    assert_eq!((r.stop, r.load_errors, r.css.failed), (Stop::Settled, 0, 0), "{what}: {:?}", r.diagnostics);
    assert_eq!((r.css.loaded, r.scripts_run), (ORDER.len(), 1 + PARSED + 1 + DYNAMIC), "{what}");
    // The document is given to the browser, so the server never sees it.
    let mut want: Vec<String> = ORDER.iter().map(|n| format!("/{n}.css")).chain(scripts.iter().map(|n| format!("/{n}.js"))).collect();
    let mut got: Vec<String> = server.requests().iter().map(|r| r.trim_start_matches("GET ").to_string()).collect();
    want.sort();
    got.sort();
    assert_eq!(got, want, "{what}: requests (a repeat is a duplicate, a missing one is not fetched)");
}

/// 0 ms: no worker (the reply is quick). 30 ms: workers. 100 ms: the slow server of the parallel tests.
fn all_speeds(style: fn(usize) -> Style, speeds: &[u64], what: &str) {
    for ms in speeds {
        let server = start(Config::new(style, Duration::from_millis(*ms)), pages);
        check(&server, load(&server), &format!("{what} at {ms} ms"));
    }
}

fn close(_: usize) -> Style {
    Style::CloseHeader
}
fn http10(_: usize) -> Style {
    Style::Http10
}
fn silent(_: usize) -> Style {
    Style::SilentClose
}
fn mixed(n: usize) -> Style {
    [Style::KeepAlive, Style::CloseHeader, Style::KeepAlive, Style::SilentClose, Style::KeepAlive, Style::Http10][n % 6]
}

#[test]
fn a_server_that_sends_connection_close_after_each_reply() {
    all_speeds(close, &[0, 30], "connection close");
}

#[test]
fn an_http10_server_with_a_body_that_ends_at_the_close() {
    all_speeds(http10, &[0, 30], "http/1.0");
}

#[test]
fn a_server_that_closes_an_idle_connection_with_no_header() {
    all_speeds(silent, &[0, 30], "silent close");
}

#[test]
fn a_slow_server_that_mixes_closes_with_keep_alive() {
    all_speeds(mixed, &[100], "mixed closes");
}

#[test]
fn a_server_that_serves_two_connections_at_once() {
    for ms in [0, 30] {
        let mut cfg = Config::new(|_| Style::KeepAlive, Duration::from_millis(ms));
        cfg.limit = Some(Limit::Wait(2));
        let server = start(cfg, pages);
        check(&server, load(&server), &format!("cap of 2 at {ms} ms"));
        assert!(server.peak.load(Ordering::SeqCst) <= 2);
    }
}

#[test]
fn a_cap_of_2_with_closes_after_each_reply() {
    let mut cfg = Config::new(close, Duration::from_millis(30));
    cfg.limit = Some(Limit::Wait(2));
    let server = start(cfg, pages);
    check(&server, load(&server), "cap of 2 with close");
}

#[test]
fn a_post_is_never_sent_twice_when_the_server_closes_after_each_reply() {
    let server = start(Config::new(silent, Duration::from_millis(30)), pages);
    let url = server.url("/page.html");
    let (log, _) = big(move || {
        let html = "<body><script>var log = [];
            fetch('/s0.js').then(function () { return fetch('/p0', { method: 'POST', body: 'x' }) }).then(function (r) { log.push('p0:' + r.status) }, function () { log.push('p0:err') });
            for (var i = 1; i < 5; i++) (function (i) { fetch('/s' + i + '.js').then(function () { log.push('g' + i) }) })(i);
        </script>";
        let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).unwrap();
        b.use_virtual_clock();
        let limits = PageLimits { quiet: Duration::from_millis(100), ..Default::default() };
        let r = b.run_page(PageConfig { url, client: Client::new(), limits, fetch: Some((Client::new(), FetchLimits::default())) });
        let v = b.run_script("log.join('|')").unwrap();
        (b.it.display(v), r)
    });
    // The POST went once, or failed without a second send. The GETs all arrived.
    assert!(server.hits("POST", "/p0") <= 1, "{:?}", server.requests());
    assert!(log.starts_with("g") || log.contains("p0:"), "{log}");
    for i in 0..5 {
        assert_eq!(server.hits("GET", &format!("/s{i}.js")), 1, "{:?}", server.requests());
    }
    assert_eq!(log.matches('g').count(), 4, "{log}");
}

#[test]
fn a_keep_alive_server_is_the_baseline() {
    all_speeds(|_| Style::KeepAlive, &[0, 30], "keep-alive");
}

/// The first file is fetched on the page thread, before any reply shows that the server is slow. It must not be fetched again by the
/// downloads that start after it.
fn once_each(html: &'static str, files: &[&str]) {
    let server = start(Config::new(|_| Style::KeepAlive, Duration::from_millis(30)), |_, path| {
        Some(if path.ends_with(".css") { ("text/css", b"p{}".to_vec()) } else { ("application/javascript", b"1".to_vec()) })
    });
    let url = server.url("/page.html");
    big(move || {
        let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).unwrap();
        b.use_virtual_clock();
        let limits = PageLimits { quiet: Duration::from_millis(100), ..Default::default() };
        b.run_page(PageConfig { url, client: Client::new(), limits, fetch: None });
    });
    for f in files {
        assert_eq!(server.hits("GET", f), 1, "{f}: {:?}", server.requests());
    }
    assert_eq!(server.total(), files.len(), "{:?}", server.requests());
}

#[test]
fn the_first_style_sheet_is_fetched_once() {
    once_each("<link rel=stylesheet href=/c0.css><link rel=stylesheet href=/c1.css><link rel=stylesheet href=/c2.css>", &["/c0.css", "/c1.css", "/c2.css"]);
}

#[test]
fn the_first_script_is_fetched_once() {
    once_each("<script src=/s0.js></script><script src=/s1.js></script><script src=/s2.js></script>", &["/s0.js", "/s1.js", "/s2.js"]);
}
