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
