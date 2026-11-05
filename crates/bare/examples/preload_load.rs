//! Load test for `<link rel=preload>` loading of the page driver (`src/browser_loader/preload.rs`) over a loopback HTTP server
//! inside this process. One case at one scale per process, on a virtual clock (nothing sleeps).
//!
//!   preload_load CASE SCALE      CASE: font | style | script | dups      SCALE: 1, 10, 100 (10, 100, 1000 links)
//!
//! font: SCALE*10 `<link rel=preload as=font>` added by one script, all different URLs (2 KiB bodies, opaque: dropped).
//! style: the same with `as=style` and 1 KiB sheets; each `load` handler then adds `<link rel=stylesheet>` for the same URL.
//! script: the same with `as=script`; each `load` handler adds a `<script src>` for the same URL, which runs the preloaded text.
//! dups: SCALE*10 font links to 10 different URLs.
//! Every case counts the events in JS and checks the server counts: one fetch for each URL, none for the stylesheets and
//! scripts that follow a preload. Prints time, CPU and peak RSS. Only throwaway synthetic data. No external network.

#[path = "../src/test_server.rs"]
mod test_server;

use bare::browser::*;
use bare::browser_computed::ComputedLimits;
use bare::{PageConfig, PageLimits, Stop};
use bare_html::tree;
use bare_js::eval::*;
use bare_js::heap::HeapLimits;
use bare_net::http::Client;
use std::time::{Duration, Instant};
use test_server::{Reply, serve};

fn die(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1)
}

fn status_kib(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok()).unwrap_or(0)
}

fn cpu_ms() -> u64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let rest = s.rsplit_once(')').map_or("", |x| x.1);
    let f: Vec<&str> = rest.split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    (t(11) + t(12)) * 10
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(case), Some(scale)) = (args.first(), args.get(1).and_then(|s| s.parse::<usize>().ok())) else {
        eprintln!("usage: preload_load font|style|script|dups <scale>");
        std::process::exit(2)
    };
    let n = 10 * scale;
    let urls = match case.as_str() {
        "font" | "style" | "script" => n,
        "dups" => 10,
        _ => die(format!("unknown case {case}")),
    };
    // One script adds all links. `as` and the URL come from the case; a `load` handler adds the follow-up element.
    let (as_, ext, follow) = match case.as_str() {
        "style" => ("style", "css", "var s = document.createElement('link'); s.rel = 'stylesheet'; s.href = h; document.head.appendChild(s);"),
        "script" => ("script", "js", "var s = document.createElement('script'); s.src = h; document.head.appendChild(s);"),
        _ => ("font", "woff2", ""),
    };
    let html = format!(
        "<script>var loads = 0, errors = 0, ran = 0;\
         for (var i = 0; i < {n}; i++) {{\
           (function (h) {{\
             var l = document.createElement('link'); l.rel = 'preload'; l.as = '{as_}'; l.href = h;\
             l.addEventListener('load', function () {{ loads++; {follow} }});\
             l.addEventListener('error', function () {{ errors++ }});\
             document.head.appendChild(l);\
           }})('/r' + (i % {urls}) + '.{ext}');\
         }}</script>"
    );
    let server = serve(move |path| {
        let ok = path.starts_with("/r") && path.contains('.');
        match (ok, path.rsplit('.').next()) {
            (true, Some("css")) => Reply::css("a { direction: rtl }".repeat(50)),
            (true, Some("js")) => Reply::js("ran++;"),
            (true, _) => Reply::new(200, Some("font/woff2"), vec![b'f'; 2048]),
            _ => Reply::new(404, None, ""),
        }
    });
    let eval = EvalLimits { max_steps: u64::MAX / 2, ..Default::default() };
    let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    b.use_virtual_clock();
    // The default cap of the computed module (256 sheets) is below the 1000 sheets of the biggest `style` case.
    b.set_computed_limits(ComputedLimits { max_external: 2048, max_sources: 2048, ..ComputedLimits::default() });
    let url = server.url("/page.html");
    let (cpu0, t) = (cpu_ms(), Instant::now());
    let limits = PageLimits {
        max_tasks: 10_000_000,
        max_rounds: 10_000_000,
        max_time: Duration::from_secs(40),
        quiet: Duration::from_millis(50),
        max_pending_scripts: 4096,
        resources: bare::resources::Limits { max_fetches: 4096, max_references: 1 << 16, ..Default::default() },
        ..Default::default()
    };
    let r = b.run_page(PageConfig { url, client: Client::new(), limits, fetch: None });
    let run_ms = t.elapsed().as_secs_f64() * 1e3;
    if r.stop != Stop::Settled || r.script_throws != 0 || r.css.failed != 0 || r.preload.failed != 0 {
        die(format!("{r:?}"));
    }
    let got = {
        let v = b.run_script("loads + ':' + errors + ':' + ran").unwrap_or_else(|e| die(format!("read: {e:?}")));
        b.it.display(v)
    };
    let want = format!("{n}:0:{}", if case == "script" { n } else { 0 });
    if got != want {
        die(format!("events differ: got {got} want {want}"));
    }
    if r.fetches != urls || r.preload.fetches != urls || server.total() != urls || r.preload.loaded != n {
        die(format!("one fetch for each URL: wanted {urls}: preload={:?} fetches={} server={}", r.preload, r.fetches, server.total()));
    }
    println!(
        "{case:7} x{scale:<3} links={n} urls={urls} events=ok run={run_ms:8.1}ms cpu={}ms peak_rss={:.1}MiB | preload={:?} css={:?} tasks={} rounds={}",
        cpu_ms() - cpu0,
        status_kib("VmHWM:") as f64 / 1024.0,
        r.preload,
        r.css,
        r.tasks,
        r.rounds
    );
}
