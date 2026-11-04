//! Load harness for `resources.rs` (S1 topic 6). It starts a loopback server in this process, builds a page that names
//! resources on it, then times `discover` and `load` and checks every count. Run in release mode, one case for each process:
//!
//!   cargo run --release -p bare --example resources_load -- <case> <scale> [runs]
//!
//! Cases (scale is 1, 10 or 100):
//!   small      50 x scale scripts of 2 KiB, all different URLs
//!   large      one script of scale MiB, with `Content-Length`
//!   largechunk the same, with `Transfer-Encoding: chunked` (16 KiB chunks): the buffer grows by doubling
//!   dups       1000 x scale script and link elements, 10 different URLs
//!
//! Limits are raised to the case, so the harness measures cost and not the default caps (the tests check the caps).
//! Any wrong count or any failed download exits with 1: an error is never a timing. The last line is `key=value`.
//! Heap is the peak of live bytes during `load`, over the live bytes before it. The server runs in this process,
//! so its own buffers are in that number. RSS is `VmHWM` of the whole process.

#![allow(dead_code)]

#[path = "../src/resources.rs"]
mod resources;
#[path = "../src/test_server.rs"]
mod test_server;

use bare_html::tree::parse;
use bare_url::Url;
use resources::{Kind, Limits, Source, discover, load};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};
use test_server::{Payload, Reply, Shape, serve};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let now = LIVE.fetch_add(l.size(), Relaxed) + l.size();
        PEAK.fetch_max(now, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let now = LIVE.fetch_add(new, Relaxed) + new - l.size();
        LIVE.fetch_sub(l.size(), Relaxed);
        PEAK.fetch_max(now, Relaxed);
        unsafe { System.realloc(p, l, new) }
    }
}
#[global_allocator]
static A: Counting = Counting;

fn status_kib(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok()).unwrap_or(0)
}

fn fail(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(case), Some(scale)) = (args.first(), args.get(1).and_then(|s| s.parse::<usize>().ok())) else {
        eprintln!("usage: resources_load small|large|largechunk|dups <scale> [runs]");
        std::process::exit(2)
    };
    let runs: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3);

    // The page, the server and the expected counts.
    let (html, expect_items, expect_targets, expect_bytes, limits, handler): (String, usize, usize, usize, Limits, Box<dyn Fn(&str) -> Reply + Send + Sync>) = match case.as_str() {
        "small" => {
            let n = 50 * scale;
            let html: String = (0..n).map(|i| format!("<script src=/s/{i}.js async></script>")).collect();
            let limits = Limits { max_references: n + 1, max_fetches: n, max_total_bytes: n * 2048, ..Limits::default() };
            (html, n, n, n * 2048, limits, Box::new(|_| Reply::js(vec![b'x'; 2048])))
        }
        "large" | "largechunk" => {
            let size = scale << 20;
            let chunked = case == "largechunk";
            let limits = Limits { max_resource_bytes: size, max_total_bytes: size, ..Limits::default() };
            let handler = move |_: &str| {
                let shape = if chunked { Shape::Chunked(16 * 1024) } else { Shape::Length };
                Reply { status: 200, headers: vec![("Content-Type".into(), "text/javascript".into())], body: Payload::Filler(size), shape }
            };
            ("<script src=/big.js></script>".to_string(), 1, 1, size, limits, Box::new(handler))
        }
        "dups" => {
            let n = 1000 * scale;
            let html: String = (0..n).map(|i| if i % 2 == 0 { format!("<script src=/d/{}.js defer></script>", i % 10) } else { format!("<link rel=stylesheet href=/d/{}.css>", i % 10) }).collect();
            let limits = Limits { max_references: n + 1, ..Limits::default() };
            (html, n, 10, 5 * 100 + 5 * 100, limits, Box::new(|p: &str| if p.ends_with(".css") { Reply::css(vec![b'c'; 100]) } else { Reply::js(vec![b'j'; 100]) }))
        }
        _ => fail(format!("unknown case {case}")),
    };

    let server = serve(handler);
    let page = Url::parse(&format!("http://127.0.0.1:{}/index.html", server.port)).unwrap();

    let (mut parse_ms, mut discover_ms, mut load_ms) = (Vec::new(), Vec::new(), Vec::new());
    let mut heap = 0;
    for run in 0..runs {
        let t = Instant::now();
        let doc = parse(&html);
        parse_ms.push(ms(t.elapsed()));

        let t = Instant::now();
        let d = discover(&doc, &page, &limits).unwrap_or_else(|e| fail(format!("discover: {e}")));
        discover_ms.push(ms(t.elapsed()));
        let externals = d.items.iter().filter(|i| matches!(i.kind, Kind::Script { source: Source::External(_), .. } | Kind::Stylesheet { .. })).count();
        if d.items.len() != expect_items || d.targets.len() != expect_targets || externals != expect_items {
            fail(format!("discovery: {} items, {} targets, {externals} external", d.items.len(), d.targets.len()));
        }

        let before = LIVE.load(Relaxed);
        PEAK.store(before, Relaxed);
        let hits_before = server.total();
        let t = Instant::now();
        let loaded = load(&d.targets, &limits);
        load_ms.push(ms(t.elapsed()));
        heap = heap.max(PEAK.load(Relaxed) - before);

        if let Some(e) = loaded.results.iter().find_map(|r| r.as_ref().err()) {
            fail(format!("download failed: {e}"));
        }
        let hits = server.total() - hits_before;
        if loaded.retained != expect_bytes || hits != expect_targets {
            fail(format!("{} bytes kept (want {expect_bytes}), {hits} requests (want {expect_targets})", loaded.retained));
        }
        println!("run {run}: parse {:.2} ms, discover {:.2} ms, load {:.2} ms", parse_ms[run], discover_ms[run], load_ms[run]);
    }
    let med = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    println!(
        "case={case} scale={scale} items={expect_items} fetches={expect_targets} bytes={expect_bytes} parse_ms={:.2} discover_ms={:.2} load_ms={:.2} load_heap_peak_kib={} rss_hwm_kib={} ok=true",
        med(&mut parse_ms),
        med(&mut discover_ms),
        med(&mut load_ms),
        heap / 1024,
        status_kib("VmHWM:")
    );
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}
