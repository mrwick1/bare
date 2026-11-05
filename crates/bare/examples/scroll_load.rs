//! Load test for scrolling a long page through the window boundary (`Page::handle`, `Page::paint`). One scale per process.
//!
//!   scroll_load LINES [STEPS]     LINES: lines of text in the page (10000, 100000)     STEPS: wheel steps (default 2000)
//!
//! The page is LINES `<div>` lines in 20 blocks, in a 800 by 600 window. It is built, painted once (cascade, layout, display
//! list, raster), then wheeled down and up STEPS times with a paint after each step. Prints the time of the first paint, the
//! p50 and p99 of a step (event and paint), allocations and bytes for a step, layouts counted by the browser (must not grow),
//! RSS, and the CPU of 3 s of idle after the last step (must be 0 ticks). `crates/bare/src/browser_scroll_tests.rs` has the
//! checks. The stack of the VM thread is 64 MiB.

use bare::browser::*;
use bare::page::{Page, Source};
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_window::{Event, Mods};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(l.size(), Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(new.saturating_sub(l.size()), Relaxed);
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn status_kib(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok()).unwrap_or(0)
}

/// CPU time of this process in milliseconds (user + system), from /proc/self/stat (100 ticks per second).
fn cpu_ms() -> u64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let f: Vec<&str> = s.rsplit_once(')').map_or("", |x| x.1).split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    (t(11) + t(12)) * 10
}

fn main() {
    let lines: usize = std::env::args().nth(1).and_then(|v| v.parse().ok()).unwrap_or(10_000);
    let steps: usize = std::env::args().nth(2).and_then(|v| v.parse().ok()).unwrap_or(2000);
    std::thread::Builder::new().stack_size(64 << 20).spawn(move || run(lines, steps)).unwrap().join().unwrap();
}

fn run(lines: usize, steps: usize) {
    let mut html = String::from("<!doctype html><style>body{margin:8px;font:16px/20px sans-serif} .b{border-left:3px solid #36c;padding-left:5px}</style>");
    for i in 0..lines {
        if i % (lines / 20).max(1) == 0 {
            html.push_str("<div class=b>");
        }
        html.push_str(&format!("<div>line {i}: the quick brown fox jumps over the lazy dog</div>"));
        if i % (lines / 20).max(1) == (lines / 20).max(1) - 1 {
            html.push_str("</div>");
        }
    }
    let t = Instant::now();
    let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    // A long page is more than the caps that guard a script's reads allow: lift the two that the first layout and cascade meet.
    b.set_geometry_limits(bare::browser_geometry::GeometryLimits { max_nodes: 4 << 20, max_text: 256 << 20, max_fragments: 16 << 20, ..Default::default() });
    b.set_computed_limits(bare::browser_computed::ComputedLimits { max_nodes: 4 << 20, ..Default::default() });
    b.set_viewport(800.0, 600.0).expect("viewport");
    b.set_document_url("http://localhost/long.html").expect("url");
    drop(html);
    let mut p = Page::new(Source::Browser(Box::new(b)), (800, 600));
    let parse = t.elapsed();
    let (a0, by0) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed));
    let t = Instant::now();
    p.paint();
    let first = t.elapsed();
    let (a1, by1) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed));
    let rss1 = status_kib("VmRSS:");
    let no = Mods { shift: false, ctrl: false, alt: false, meta: false };
    let mut times: Vec<f64> = Vec::with_capacity(steps);
    let layouts0 = p.browser().unwrap().geometry_stats().layouts;
    let (a2, by2) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed));
    for i in 0..steps {
        // 100 steps down, 100 steps up (a step is 53 px, one tick of a wheel).
        let dy = if (i / 100) % 2 == 0 { 53.0 } else { -53.0 };
        let t = Instant::now();
        p.handle(Event::Wheel { x: 400.0, y: 300.0, dx: 0.0, dy, mods: no });
        p.paint();
        times.push(t.elapsed().as_secs_f64() * 1e6);
    }
    let (a3, by3) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed));
    let layouts1 = p.browser().unwrap().geometry_stats().layouts;
    let y = p.browser().unwrap().scroll().y;
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |f: f64| times[((times.len() - 1) as f64 * f) as usize];
    println!("lines {lines}: parse and realm {:.1} ms, first paint {:.1} ms ({} allocs, {} KiB allocated), RSS after it {} KiB", parse.as_secs_f64() * 1e3, first.as_secs_f64() * 1e3, a1 - a0, (by1 - by0) / 1024, rss1);
    println!("  {steps} wheel steps with a paint: p50 {:.0} us, p99 {:.0} us, max {:.0} us; {:.1} allocs and {:.0} bytes a step; layouts {} -> {}; scrollY at the end {y}", q(0.5), q(0.99), times[times.len() - 1], (a3 - a2) as f64 / steps as f64, (by3 - by2) as f64 / steps as f64, layouts0, layouts1);
    println!("  RSS after the steps {} KiB, peak {} KiB", status_kib("VmRSS:"), status_kib("VmHWM:"));
    // The keys: Down and Page Down with their releases, and a paint.
    let mut keys: Vec<f64> = Vec::new();
    let lk = p.browser().unwrap().geometry_stats().layouts;
    for i in 0..200 {
        let name = if i % 2 == 0 { "ArrowDown" } else { "PageDown" };
        let t = Instant::now();
        for pressed in [true, false] {
            p.handle(Event::Key { key: name.into(), code: "ArrowDown", pressed, repeat: false, text: None, mods: no });
        }
        p.paint();
        keys.push(t.elapsed().as_secs_f64() * 1e6);
    }
    keys.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let lk2 = p.browser().unwrap().geometry_stats().layouts;
    println!("  a scroll key (press, release, paint): p50 {:.0} us, p99 {:.0} us; layouts {lk} -> {lk2}", keys[100], keys[197]);
    // A click far down the page: the hit test walks the fragments once (a pointer move does too).
    let mut clicks: Vec<f64> = Vec::new();
    p.handle(Event::Wheel { x: 400.0, y: 300.0, dx: 0.0, dy: 5300.0, mods: no });
    for _ in 0..100 {
        let t = Instant::now();
        p.handle(Event::PointerMove { x: 100.0, y: 200.0, buttons: 0, mods: no });
        for pressed in [true, false] {
            p.handle(Event::PointerButton { x: 100.0, y: 200.0, button: 0, buttons: pressed as u16, pressed, mods: no });
        }
        clicks.push(t.elapsed().as_secs_f64() * 1e6);
    }
    clicks.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let sy = p.browser().unwrap().scroll().y;
    let ls = p.browser().unwrap().geometry_stats().layouts;
    println!("  a click (move, press, release) at scrollY {sy}: p50 {:.0} us, p99 {:.0} us; layouts after the clicks {ls}", clicks[50], clicks[98]);
    let c = cpu_ms();
    std::thread::sleep(Duration::from_secs(3));
    println!("  idle CPU in 3 s: {} ms", cpu_ms() - c);
}
