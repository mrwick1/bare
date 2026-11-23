//! A script from a tracker host (`bare_net::blocklist`) is refused before any request, and the page still loads. A tape holds a reply
//! for the script: if the request were made, the script would run.

use crate::browser::*;
use crate::browser_fetch::{FetchConfig, FetchLimits};
use crate::browser_loader::*;
use crate::resources::test_server::{Reply, serve};
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_net::http::Client;
use bare_net::replay::{Entry, Tape};
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// Load a page with one external script at `src`, whose reply is on the tape. Gives the log and the result.
fn run(src: &str, dir: &str) -> (String, PageResult) {
    let dir = std::env::temp_dir().join(dir);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let tape = Tape::fill(&dir);
    let label = Tape::label("GET", src.split_once("://").map(|(s, r)| format!("{s}://{}", r.replacen('/', ":80/", 1))).unwrap().as_str(), b"");
    let headers = vec![("content-type".to_string(), "application/javascript".to_string())];
    tape.slot(label).save(&Entry { status: 200, headers, complete: true, body: b"window.tracked = 1;".to_vec(), ..Entry::default() });
    let srv = serve(|_| Reply::new(404, Some("text/plain"), "no"));
    let html = format!("<script>var log = [];</script><script src='{src}'></script><script>log.push(typeof tracked)</script>");
    let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    let mut client = Client::new();
    client.use_tape(tape);
    b.configure_fetch(FetchConfig { base_url: Some(srv.url("/page.html")), client: client.fork(), limits: FetchLimits::default() }).unwrap();
    let limits = PageLimits { quiet: Duration::from_millis(100), ..Default::default() };
    let r = b.run_page(PageConfig { url: srv.url("/page.html"), client, limits, fetch: None });
    let v = b.run_script("log.join('|')").unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (b.it.display(v), r)
}

#[test]
fn a_tracker_script_is_blocked_and_the_page_still_loads() {
    big(|| {
        let (log, r) = run("http://www.googletagmanager.com/gtm.js", "bare-trackers-blocked");
        assert_eq!(log, "undefined");
        assert_eq!(r.stop, Stop::Settled);
        assert!(r.diagnostics.iter().any(|d| d.contains("blocked: tracker host")), "{:?}", r.diagnostics);
        assert!(!r.diagnostics.iter().any(|d| d.contains("network error")), "{:?}", r.diagnostics);
    });
}

#[test]
fn a_script_from_another_host_still_runs() {
    big(|| {
        let (log, r) = run("http://cdn.example.test/gtm.js", "bare-trackers-control");
        assert_eq!(log, "number");
        assert_eq!(r.stop, Stop::Settled);
    });
}
