//! Tests of parallel downloads (`Net` in `browser_loader.rs`). A loopback server waits 100 ms before each reply. N independent files
//! must take about 100 ms times ceil(N / 6), not N times 100 ms, and the order in which scripts run must not change.

use crate::browser::*;
use crate::browser_fetch::FetchLimits;
use crate::browser_loader::*;
use crate::resources::test_server::{Reply, Server, serve};
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_net::http::Client;
use bare_net::parallel::MAX_WORKERS;
use std::time::{Duration, Instant};

const LATENCY: Duration = Duration::from_millis(100);

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// `/s<N>.js` pushes N to `log` (and waits `delay(N)`), `/c<N>.css` is a rule, `/d<N>` is a JSON body. Each waits.
fn site(delay: fn(usize) -> Duration) -> Server {
    serve(move |path| {
        let n: usize = path.trim_start_matches(|c: char| !c.is_ascii_digit()).split('.').next().unwrap().parse().unwrap_or(0);
        std::thread::sleep(delay(n));
        if path.ends_with(".js") {
            Reply::js(format!("log.push('s{n}')"))
        } else if path.ends_with(".css") {
            Reply::css(format!("#p{n} {{ color: red }}"))
        } else {
            Reply::new(200, Some("application/json"), format!("{{\"n\":{n}}}"))
        }
    })
}

fn run(html: &str, srv: &Server) -> (String, PageResult, Duration) {
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    let limits = PageLimits { quiet: Duration::from_millis(100), ..Default::default() };
    let start = Instant::now();
    let r = b.run_page(PageConfig { url: srv.url("/page.html"), client: Client::new(), limits, fetch: Some((Client::new(), FetchLimits::default())) });
    let took = start.elapsed();
    eprintln!("parallel test: page took {took:?}");
    let v = b.run_script("log.join('|')").unwrap();
    (b.it.display(v), r, took)
}

fn flat(_: usize) -> Duration {
    LATENCY
}

#[test]
fn twelve_parsed_scripts_download_together_and_run_in_order() {
    big(|| {
        let scripts: String = (0..12).map(|i| format!("<script src=/s{i}.js></script>")).collect();
        let (log, r, took) = run(&format!("<body><script>var log = []</script>{scripts}"), &site(flat));
        assert_eq!(log, (0..12).map(|i| format!("s{i}")).collect::<Vec<_>>().join("|"));
        assert_eq!((r.scripts_run, r.load_errors, r.fetches), (13, 0, 12));
        // Two waves of 6, and the first wave starts with the page. One at a time it is 1200 ms.
        assert!(took < LATENCY * 6, "took {took:?}");
    });
}

#[test]
fn a_slow_first_script_does_not_change_the_order() {
    big(|| {
        // Script 0 is the slowest and script 5 the fastest: they finish in the reverse order.
        let srv = site(|n| Duration::from_millis(300 - 50 * n as u64));
        let scripts: String = (0..6).map(|i| format!("<script src=/s{i}.js></script>")).collect();
        let (log, r, _) = run(&format!("<body><script>var log = []</script>{scripts}"), &srv);
        assert_eq!(log, "s0|s1|s2|s3|s4|s5");
        assert_eq!(r.load_errors, 0);
    });
}

#[test]
fn a_slow_script_does_not_hold_back_the_downloads_after_it() {
    big(|| {
        // s1 is the first request: it runs alone and shows that the server is slow. s2 takes 600 ms, the 24 after it 100 ms each.
        // While the page waits for s2, the 5 other workers download all of them. Before, a file that was done but not taken kept its
        // place in the window of 6: s8 started only after s2 was taken.
        let srv = site(|n| if n == 2 { LATENCY * 6 } else { LATENCY });
        let scripts: String = (1..=26).map(|i| format!("<script src=/s{i}.js></script>")).collect();
        let (log, r, took) = run(&format!("<body><script>var log = []</script>{scripts}"), &srv);
        assert_eq!(log, (1..=26).map(|i| format!("s{i}")).collect::<Vec<_>>().join("|"));
        assert_eq!(r.load_errors, 0);
        // s1 (100 ms), then s2 (600 ms): the rest is done by then. Before: 4 more waves after s2 (about 1,100 ms).
        assert!(took < LATENCY * 9, "took {took:?}");
    });
}

#[test]
fn a_page_that_ends_drops_the_downloads_that_have_not_begun() {
    big(|| {
        // s1 navigates. When it runs, s2 to s30 are queued for the workers: the 6 that run finish, and the other 23 never start, so
        // the next document of the session does not wait behind them.
        let srv = serve(|path| {
            std::thread::sleep(LATENCY);
            if path == "/s1.js" { Reply::js("location.assign('/next')") } else { Reply::js("1") }
        });
        let scripts: String = (1..=30).map(|i| format!("<script src=/s{i}.js></script>")).collect();
        let (_, r, _) = run(&format!("<body><script>var log = []</script>{scripts}"), &srv);
        assert!(matches!(r.stop, Stop::Navigate(_)), "{:?}", r.stop);
        std::thread::sleep(LATENCY * 8);
        assert!(srv.total() <= 1 + MAX_WORKERS, "{} requests", srv.total());
    });
}

#[test]
fn defer_and_async_keep_their_rules_when_files_arrive_out_of_order() {
    big(|| {
        let srv = site(|n| Duration::from_millis(250 - 40 * n as u64));
        let html = "<head><script>var log = []</script><script src=/s0.js></script><script defer src=/s1.js></script>\
            <script async src=/s2.js></script><script defer src=/s3.js></script><script src=/s4.js></script></head><body></body>";
        let (log, r, _) = run(html, &srv);
        // Blocking scripts first, in order. Then the deferred ones, in order. The async one runs after them: it is queued at the end.
        assert_eq!(log, "s0|s4|s1|s3|s2");
        assert_eq!(r.load_errors, 0);
    });
}

#[test]
fn a_failed_script_is_reported_at_its_turn() {
    big(|| {
        let srv = serve(|path| {
            std::thread::sleep(LATENCY);
            if path == "/s1.js" { Reply::new(404, Some("text/plain"), "no") } else { Reply::js(format!("log.push('{path}')")) }
        });
        let html = "<body><script>var log = []</script><script src=/s0.js></script><script src=/s1.js></script><script src=/s2.js></script>";
        let (log, r, _) = run(html, &srv);
        assert_eq!(log, "/s0.js|/s2.js");
        assert_eq!(r.load_errors, 1);
        assert!(r.diagnostics.iter().any(|d| d.contains("script failed") && d.contains("/s1.js")), "{:?}", r.diagnostics);
    });
}

#[test]
fn style_sheets_download_together() {
    big(|| {
        let links: String = (0..6).map(|i| format!("<link rel=stylesheet href=/c{i}.css>")).collect();
        let (_, r, took) = run(&format!("<head>{links}<script>var log = []</script></head><body><p id=p0>x</p>"), &site(flat));
        assert_eq!((r.css.loaded, r.css.failed), (6, 0));
        assert!(took < LATENCY * 4, "took {took:?}");
    });
}

#[test]
fn twelve_fetches_run_together_and_settle_in_order() {
    big(|| {
        let html = "<body><script>var log = [];
            for (var i = 0; i < 12; i++) (function (i) { fetch('/d' + i).then(function (r) { return r.json() }).then(function (j) { log.push('d' + j.n) }) })(i);
        </script>";
        let (log, _, took) = run(html, &site(flat));
        assert_eq!(log, (0..12).map(|i| format!("d{i}")).collect::<Vec<_>>().join("|"));
        assert!(took < LATENCY * 6, "took {took:?}");
    });
}

#[test]
fn a_slow_fetch_does_not_hold_back_the_requests_queued_after_it() {
    big(|| {
        // d1 runs alone and shows that the server is slow. d2 takes 600 ms, the 24 after it 100 ms each: they all run while the page
        // waits for d2. Before, only the first 6 queued requests were started, so the rest began after d2 (about 1,100 ms).
        let srv = site(|n| if n == 2 { LATENCY * 6 } else { LATENCY });
        let html = "<body><script>var log = [];
            for (var i = 1; i <= 26; i++) (function (i) { fetch('/d' + i).then(function (r) { return r.json() }).then(function (j) { log.push('d' + j.n) }) })(i);
        </script>";
        let (log, _, took) = run(html, &srv);
        assert_eq!(log, (1..=26).map(|i| format!("d{i}")).collect::<Vec<_>>().join("|"));
        assert!(took < LATENCY * 9, "took {took:?}");
    });
}

#[test]
fn dynamic_scripts_download_together_and_run_in_order() {
    big(|| {
        let html = "<body><script>var log = [];
            for (var i = 0; i < 12; i++) { var s = document.createElement('script'); s.src = '/s' + i + '.js'; document.head.appendChild(s) }
        </script>";
        let (log, r, took) = run(html, &site(flat));
        assert_eq!(log, (0..12).map(|i| format!("s{i}")).collect::<Vec<_>>().join("|"));
        assert_eq!(r.load_errors, 0);
        assert!(took < LATENCY * 6, "took {took:?}");
    });
}

#[test]
fn the_fetch_budget_still_counts_each_file_once() {
    big(|| {
        let srv = site(flat);
        let scripts: String = (0..6).map(|i| format!("<script src=/s{i}.js></script>")).collect();
        let mut b = Browser::new(tree::parse(&format!("<body><script>var log = []</script>{scripts}")), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).unwrap();
        b.use_virtual_clock();
        let mut limits = PageLimits { quiet: Duration::from_millis(100), ..Default::default() };
        limits.resources.max_fetches = 4;
        let r = b.run_page(PageConfig { url: srv.url("/page.html"), client: Client::new(), limits, fetch: None });
        // The page names 6 files and the budget is 4: the loader refuses the page as it did before, and sends nothing.
        assert!(srv.total() <= 4, "{} requests", srv.total());
        assert!(!matches!(r.stop, Stop::Settled) || r.fetches <= 4, "{r:?}");
    });
}

#[test]
fn a_request_that_changes_things_keeps_its_place_between_the_others() {
    big(|| {
        let srv = site(flat);
        let html = "<body><script>var log = [];
            fetch('/d0'); fetch('/d1', { method: 'POST', body: 'x' }); fetch('/d2'); fetch('/d3');
            fetch('/d4').then(function () { log.push('last') });
        </script>";
        let (log, _, took) = run(html, &srv);
        assert_eq!(log, "last");
        // The server sees them in the order of the script: the POST waits for /d0, and /d2 to /d4 wait for the POST.
        let seen: Vec<String> = srv.heads.lock().unwrap().iter().map(|h| h.split(' ').take(2).collect::<Vec<_>>().join(" ")).collect();
        assert_eq!(seen[..2], ["get /d0", "post /d1"], "{seen:?}");
        // /d0, the POST, then /d2 to /d4 together: 3 round trips.
        assert!(took >= LATENCY * 3 && took < LATENCY * 5, "took {took:?}");
    });
}

/// The `@import` cases of `tools/fixtures/ui/imports-cases.txt` (name, files with `/page.html`, ids to read) and what Chrome answered.
type ImportCase = (String, Vec<(String, String)>, Vec<String>);

fn import_cases() -> Vec<ImportCase> {
    let mut cases: Vec<ImportCase> = Vec::new();
    for line in include_str!("../../../../tools/fixtures/ui/imports-cases.txt").lines() {
        if let Some(name) = line.strip_prefix("=== ") {
            cases.push((name.trim().to_string(), Vec::new(), Vec::new()));
        } else if let Some(path) = line.strip_prefix("@file ") {
            cases.last_mut().unwrap().1.push((path.to_string(), String::new()));
        } else if let Some(ids) = line.strip_prefix("@read ") {
            cases.last_mut().unwrap().2 = ids.split(' ').map(str::to_string).collect();
        } else if let Some(file) = cases.last_mut().and_then(|c| c.1.last_mut()) {
            file.1 += line;
            file.1 += "\n";
        }
    }
    cases
}

/// Run one case with `delay` before each style sheet reply. Gives what the page reads, as the Chrome file does, and the time.
fn run_import_case(case: &ImportCase, delay: Duration) -> (String, PageResult, Duration) {
    let files: std::collections::HashMap<String, String> = case.1.iter().cloned().collect();
    let srv = serve(move |path| match files.get(path) {
        Some(t) => {
            std::thread::sleep(delay);
            Reply::css(t.as_str())
        }
        None => Reply::new(404, Some("text/plain"), "no"),
    });
    let mut b = Browser::new(tree::parse(&case.1[0].1), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    let limits = PageLimits { quiet: Duration::from_millis(100), ..Default::default() };
    let start = Instant::now();
    let r = b.run_page(PageConfig { url: srv.url("/page.html"), client: Client::new(), limits, fetch: None });
    let took = start.elapsed();
    let ids = ids_js(&case.2);
    let v = b.run_script(&format!("{ids}.map(function (i) {{ return i + '=' + getComputedStyle(document.getElementById(i)).direction }}).join()")).unwrap();
    (b.it.display(v), r, took)
}

fn ids_js(ids: &[String]) -> String {
    format!("[{}]", ids.iter().map(|i| format!("'{i}'")).collect::<Vec<_>>().join(","))
}

fn chrome_import_results() -> Vec<(String, String)> {
    include_str!("../../../../tools/fixtures/ui/imports-chrome.txt").lines().map(|l| l.split_once('\t').map(|(a, b)| (a.to_string(), b.to_string())).unwrap()).collect()
}

#[test]
fn import_cases_match_chrome_with_and_without_latency() {
    big(|| {
        let (cases, want) = (import_cases(), chrome_import_results());
        assert_eq!(cases.len(), want.len());
        for (case, (name, expected)) in cases.iter().zip(&want) {
            assert_eq!(&case.0, name);
            // 0 ms: serial path. 30 ms: workers (a reply of 10 ms or more turns them on).
            for ms in [0, 30] {
                let (got, r, _) = run_import_case(case, Duration::from_millis(ms));
                assert_eq!(&got, expected, "{name} at {ms} ms");
                assert_eq!(r.stop, Stop::Settled, "{name} at {ms} ms");
            }
        }
    });
}

#[test]
fn a_tree_of_imports_downloads_by_level_and_keeps_the_import_order() {
    big(|| {
        let case = import_cases().into_iter().next().unwrap();
        let (got, r, took) = run_import_case(&case, LATENCY);
        assert_eq!(got, chrome_import_results()[0].1);
        // 19 sheets: one at a time they take 1900 ms. By level: the root, 6 sheets, 12 sheets (two waves of 6 at most).
        assert_eq!((r.css.loaded, r.css.failed, r.fetches), (19, 0, 19));
        assert!(took < LATENCY * 7, "took {took:?}");
    });
}

#[test]
fn a_chain_past_the_depth_limit_still_stops_the_page_with_slow_replies() {
    big(|| {
        let srv = serve(|path| {
            std::thread::sleep(Duration::from_millis(30));
            match path.trim_start_matches("/d").trim_end_matches(".css").parse::<usize>() {
                Ok(i) => Reply::css(format!("@import 'd{}.css';", i + 1)),
                Err(_) => Reply::new(404, Some("text/plain"), "no"),
            }
        });
        let mut b = Browser::new(tree::parse("<link rel=stylesheet href=d0.css><script>var log = []</script>"), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).unwrap();
        b.use_virtual_clock();
        let r = b.run_page(PageConfig { url: srv.url("/page.html"), client: Client::new(), limits: PageLimits::default(), fetch: None });
        assert_eq!(r.stop, Stop::Fatal(Fatal::Limit("css-import-depth".into())));
        assert_eq!(r.scripts_run, 0);
        assert!(srv.total() <= 10, "{} requests", srv.total());
    });
}
