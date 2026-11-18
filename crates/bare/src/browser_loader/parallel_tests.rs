//! Tests of parallel downloads (`Net` in `browser_loader.rs`). A loopback server waits 100 ms before each reply. N independent files
//! must take about 100 ms times ceil(N / 6), not N times 100 ms, and the order in which scripts run must not change.
//!
//! The tests do not measure time. A wall-clock bound fails on a loaded machine. The server holds a request until the others have come in
//! (`Meet`), so a loader that sends them one at a time waits for the deadline and the test fails, and a correct loader passes at any speed.

use crate::browser::*;
use crate::browser_fetch::FetchLimits;
use crate::browser_loader::*;
use crate::resources::test_server::{Reply, Server, serve};
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_net::http::Client;
use bare_net::parallel::MAX_WORKERS;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::time::{Duration, Instant};

const LATENCY: Duration = Duration::from_millis(100);

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// The longest a held request waits. Only a loader that does not do the work reaches it.
const DEADLINE: Duration = Duration::from_secs(30);

/// The requests that are in the server together. The first request waits `LATENCY` alone: the loader turns the workers on after a slow
/// reply. A later request waits until `need` requests were inside together, so the test does not depend on the speed of the machine.
#[derive(Default)]
struct Meet {
    arrived: AtomicUsize,
    inside: AtomicUsize,
    peak: AtomicUsize,
    gave_up: AtomicBool,
}

impl Meet {
    fn hold(&self, need: usize) {
        let first = self.arrived.fetch_add(1, SeqCst) == 0;
        let now = self.inside.fetch_add(1, SeqCst) + 1;
        self.peak.fetch_max(now, SeqCst);
        if first {
            std::thread::sleep(LATENCY);
        } else {
            self.until(|| self.peak.load(SeqCst) >= need);
        }
        self.inside.fetch_sub(1, SeqCst);
    }

    /// Wait for `ok`. After the first deadline nobody waits, so a failing test ends in one deadline.
    fn until(&self, ok: impl Fn() -> bool) {
        let end = Instant::now() + DEADLINE;
        while !ok() && !self.gave_up.load(SeqCst) {
            if Instant::now() > end {
                self.gave_up.store(true, SeqCst);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// No wait ran into the deadline, and `need` requests were in together.
    fn met(&self, need: usize) -> bool {
        !self.gave_up.load(SeqCst) && self.peak.load(SeqCst) >= need
    }
}

/// `site` where `Meet` holds the requests until 3 are in together.
fn together() -> (Server, Arc<Meet>) {
    let meet = Arc::new(Meet::default());
    let m = meet.clone();
    (site_with(move |_| m.hold(3)), meet)
}

/// `/s<N>.js` pushes N to `log` (and waits `delay(N)`), `/c<N>.css` is a rule, `/d<N>` is a JSON body. Each waits.
fn site(delay: fn(usize) -> Duration) -> Server {
    site_with(move |n| std::thread::sleep(delay(n)))
}

fn site_with(wait: impl Fn(usize) + Send + Sync + 'static) -> Server {
    serve(move |path| {
        let n: usize = path.trim_start_matches(|c: char| !c.is_ascii_digit()).split('.').next().unwrap().parse().unwrap_or(0);
        wait(n);
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

/// A site where request 1 is slow alone (it turns the workers on) and request 2 waits until `total` requests have come in.
fn slow_second(total: usize) -> (Server, Arc<Meet>) {
    let meet = Arc::new(Meet::default());
    let m = meet.clone();
    let srv = site_with(move |n| {
        m.arrived.fetch_add(1, SeqCst);
        match n {
            1 => std::thread::sleep(LATENCY),
            2 => m.until(|| m.arrived.load(SeqCst) >= total),
            _ => {}
        }
    });
    (srv, meet)
}

#[test]
fn twelve_parsed_scripts_download_together_and_run_in_order() {
    big(|| {
        let scripts: String = (0..12).map(|i| format!("<script src=/s{i}.js></script>")).collect();
        let (srv, meet) = together();
        let (log, r, _) = run(&format!("<body><script>var log = []</script>{scripts}"), &srv);
        assert_eq!(log, (0..12).map(|i| format!("s{i}")).collect::<Vec<_>>().join("|"));
        assert_eq!((r.scripts_run, r.load_errors, r.fetches), (13, 0, 12));
        // One at a time, no 3 requests are in together.
        assert!(meet.met(3), "peak {}", meet.peak.load(SeqCst));
    });
}

/// `/m<N>.js` is a module that imports `/m<N+1>.js` (the last of `last` imports nothing) and pushes `m<N>` to `log` after its import ran.
fn chain(last: usize, wait: impl Fn() + Send + Sync + 'static) -> Server {
    serve(move |path| {
        wait();
        let n: usize = path.trim_start_matches("/m").trim_end_matches(".js").parse().unwrap_or(0);
        let import = if n < last { format!("import './m{}.js';", n + 1) } else { String::new() };
        Reply::js(format!("{import} log.push('m{n}')"))
    })
}

#[test]
fn modulepreload_links_download_together_and_the_graph_runs_in_order() {
    big(|| {
        // m0 imports m1, m1 imports m2, ... m5. A loader that reads each import only after its module came asks for one file at a time.
        // The page names m1 to m5 in `modulepreload` links, so their downloads start together.
        let meet = Arc::new(Meet::default());
        let m = meet.clone();
        let srv = chain(5, move || m.hold(3));
        let links: String = (1..=5).map(|i| format!("<link rel=modulepreload href=/m{i}.js>")).collect();
        let (log, r, _) = run(&format!("<head><script>var log = []</script>{links}</head><body><script type=module src=/m0.js></script></body>"), &srv);
        assert_eq!(log, "m5|m4|m3|m2|m1|m0");
        assert_eq!((r.load_errors, r.fetches), (0, 6));
        assert!(meet.met(3), "peak {}", meet.peak.load(SeqCst));
    });
}

#[test]
fn a_modulepreload_link_that_no_import_uses_does_not_change_the_run() {
    big(|| {
        let srv = chain(1, || std::thread::sleep(LATENCY));
        // m7 is named and never imported. Its download holds nothing: the page loads and its modules run in the order of the imports.
        let (log, r, _) = run("<head><script>var log = []</script><link rel=modulepreload href=/m1.js><link rel=modulepreload href=/m7.js></head><body><script type=module src=/m0.js></script></body>", &srv);
        assert_eq!(log, "m1|m0");
        assert_eq!(r.load_errors, 0);
    });
}

#[test]
fn import_calls_of_one_checkpoint_download_together() {
    big(|| {
        // Six `import()` calls in one task: their files are asked for together, and the page does not wait for one reply at a time.
        let imports: String = (1..=6).map(|i| format!("import('/s{i}.js')")).collect::<Vec<_>>().join(", ");
        let (srv, meet) = together();
        let (log, r, _) = run(&format!("<body><script>var log = []; Promise.all([{imports}])</script>"), &srv);
        let mut got: Vec<&str> = log.split('|').collect();
        got.sort();
        assert_eq!(got, ["s1", "s2", "s3", "s4", "s5", "s6"]);
        assert_eq!((r.load_errors, r.fetches), (0, 6));
        // One at a time, no 3 requests are in together.
        assert!(meet.met(3), "peak {}", meet.peak.load(SeqCst));
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
        // The server holds s2 until all 26 requests have come in. A loader that starts the 24 after s2 only when s2 is taken never
        // gets there, and s2 waits for the deadline.
        let (srv, meet) = slow_second(26);
        let scripts: String = (1..=26).map(|i| format!("<script src=/s{i}.js></script>")).collect();
        let (log, r, _) = run(&format!("<body><script>var log = []</script>{scripts}"), &srv);
        assert_eq!(log, (1..=26).map(|i| format!("s{i}")).collect::<Vec<_>>().join("|"));
        assert_eq!(r.load_errors, 0);
        assert!(meet.met(0), "s2 waited for the others and they did not come");
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
        let (srv, meet) = together();
        let (_, r, _) = run(&format!("<head>{links}<script>var log = []</script></head><body><p id=p0>x</p>"), &srv);
        assert_eq!((r.css.loaded, r.css.failed), (6, 0));
        assert!(meet.met(3), "peak {}", meet.peak.load(SeqCst));
    });
}

#[test]
fn twelve_fetches_run_together_and_settle_in_order() {
    big(|| {
        let html = "<body><script>var log = [];
            for (var i = 0; i < 12; i++) (function (i) { fetch('/d' + i).then(function (r) { return r.json() }).then(function (j) { log.push('d' + j.n) }) })(i);
        </script>";
        let (srv, meet) = together();
        let (log, _, _) = run(html, &srv);
        assert_eq!(log, (0..12).map(|i| format!("d{i}")).collect::<Vec<_>>().join("|"));
        assert!(meet.met(3), "peak {}", meet.peak.load(SeqCst));
    });
}

#[test]
fn a_slow_fetch_does_not_hold_back_the_requests_queued_after_it() {
    big(|| {
        // d1 runs alone and shows that the server is slow. d2 takes 600 ms, the 24 after it 100 ms each: they all run while the page
        // waits for d2. Before, only the first 6 queued requests were started, so the rest began after d2 (about 1,100 ms).
        let (srv, meet) = slow_second(26);
        let html = "<body><script>var log = [];
            for (var i = 1; i <= 26; i++) (function (i) { fetch('/d' + i).then(function (r) { return r.json() }).then(function (j) { log.push('d' + j.n) }) })(i);
        </script>";
        let (log, _, _) = run(html, &srv);
        assert_eq!(log, (1..=26).map(|i| format!("d{i}")).collect::<Vec<_>>().join("|"));
        assert!(meet.met(0), "d2 waited for the others and they did not come");
    });
}

#[test]
fn dynamic_scripts_download_together_and_run_in_order() {
    big(|| {
        let html = "<body><script>var log = [];
            for (var i = 0; i < 12; i++) { var s = document.createElement('script'); s.src = '/s' + i + '.js'; document.head.appendChild(s) }
        </script>";
        let (srv, meet) = together();
        let (log, r, _) = run(html, &srv);
        assert_eq!(log, (0..12).map(|i| format!("s{i}")).collect::<Vec<_>>().join("|"));
        assert_eq!(r.load_errors, 0);
        assert!(meet.met(3), "peak {}", meet.peak.load(SeqCst));
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
        // `/d0` and the POST (`/d1`) take `LATENCY` each and must not overlap: no request starts before the one ahead of it ends. `/d2` to
        // `/d4` are held until 3 are in together.
        let events = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let meet = Arc::new(Meet::default());
        let (e, m) = (events.clone(), meet.clone());
        let srv = site_with(move |n| {
            e.lock().unwrap().push(format!("start {n}"));
            if n < 2 {
                std::thread::sleep(LATENCY);
            } else {
                m.arrived.fetch_add(1, SeqCst);
                m.until(|| m.arrived.load(SeqCst) >= 3);
            }
            e.lock().unwrap().push(format!("end {n}"));
        });
        let html = "<body><script>var log = [];
            fetch('/d0'); fetch('/d1', { method: 'POST', body: 'x' }); fetch('/d2'); fetch('/d3');
            fetch('/d4').then(function () { log.push('last') });
        </script>";
        let (log, _, _) = run(html, &srv);
        assert_eq!(log, "last");
        // The server sees them in the order of the script: the POST waits for /d0, and /d2 to /d4 wait for the POST.
        let seen: Vec<String> = srv.heads.lock().unwrap().iter().map(|h| h.split(' ').take(2).collect::<Vec<_>>().join(" ")).collect();
        assert_eq!(seen[..2], ["get /d0", "post /d1"], "{seen:?}");
        // /d0, the POST, then /d2 to /d4 together.
        let events = events.lock().unwrap();
        assert_eq!(events[..4], ["start 0", "end 0", "start 1", "end 1"], "{events:?}");
        assert!(meet.met(0) && meet.arrived.load(SeqCst) == 3, "/d2 to /d4 were not in together");
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
fn run_import_case(case: &ImportCase, delay: Duration) -> (String, PageResult, Arc<Meet>) {
    let files: std::collections::HashMap<String, String> = case.1.iter().cloned().collect();
    let meet = Arc::new(Meet::default());
    let m = meet.clone();
    let srv = serve(move |path| match files.get(path) {
        Some(t) => {
            // With `LATENCY`: the case is a tree, and its sheets of a level are held until 3 are in together. Else a plain delay.
            if delay == LATENCY {
                m.hold(3);
            } else {
                std::thread::sleep(delay);
            }
            Reply::css(t.as_str())
        }
        None => Reply::new(404, Some("text/plain"), "no"),
    });
    let mut b = Browser::new(tree::parse(&case.1[0].1), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    let limits = PageLimits { quiet: Duration::from_millis(100), ..Default::default() };
    let r = b.run_page(PageConfig { url: srv.url("/page.html"), client: Client::new(), limits, fetch: None });
    let ids = ids_js(&case.2);
    let v = b.run_script(&format!("{ids}.map(function (i) {{ return i + '=' + getComputedStyle(document.getElementById(i)).direction }}).join()")).unwrap();
    (b.it.display(v), r, meet)
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
        let (got, r, meet) = run_import_case(&case, LATENCY);
        assert_eq!(got, chrome_import_results()[0].1);
        // 19 sheets. By level: the root, 6 sheets, 12 sheets (two waves of 6 at most). One at a time, no 3 are in together.
        assert_eq!((r.css.loaded, r.css.failed, r.fetches), (19, 0, 19));
        assert!(meet.met(3), "peak {}", meet.peak.load(SeqCst));
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
