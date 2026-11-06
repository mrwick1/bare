//! Load test for geometry reads (`offsetWidth` over native layout, `browser_geometry.rs`).
//!
//!   geometry_load CASE SCALE      CASE: dashboard | stretch | scroll      SCALE: 1, 10, 100
//!
//! dashboard: the page of `tools/fixtures/ui/geometry-dashboard.html` (207 elements and texts, 9 levels of boxes that layout can lay out
//! more than once, 17 levels deep), with its `<main>` repeated SCALE times. stretch: SCALE copies of 40 flex rows inside one another, each
//! with a sibling that is taller the nearer it is to the outside, so every row is stretched and laid out again (2^40 layouts without the
//! map of `layout/src/memo.rs`). Both are read through `offsetWidth` with raised limits, so the numbers are of the work and not of a cap.
//! scroll: SCALE copies of 30 scroll containers that need a bar, each inside a stretched flex row (2^30 layouts, the map does not help).
//! It is read with the default limits, and the read must stop with `geometry-work`; the time that it took is printed.
//! Prints the cascade, layout and read times, the work of layout in units, the fragments, the peak RSS and the allocations.

use bare::browser::*;
use bare_css::cascade::{self, Origin, RuleSet};
use bare_html::tree;
use bare_js::eval::*;
use bare_js::heap::HeapLimits;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            ALLOCS.fetch_add(1, Relaxed);
            BYTES.fetch_add(l.size(), Relaxed);
            PEAK.fetch_max(LIVE.fetch_add(l.size(), Relaxed) + l.size(), Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            ALLOCS.fetch_add(1, Relaxed);
            if new > l.size() {
                BYTES.fetch_add(new - l.size(), Relaxed);
                PEAK.fetch_max(LIVE.fetch_add(new - l.size(), Relaxed) + new - l.size(), Relaxed);
            } else {
                LIVE.fetch_sub(l.size() - new, Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

fn die(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1)
}

fn status_kib(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok()).unwrap_or(0)
}

const DASHBOARD: &str = include_str!("../../../tools/fixtures/ui/geometry-dashboard.html");

fn page(name: &str, scale: usize) -> String {
    match name {
        "dashboard" => {
            let (a, rest) = DASHBOARD.split_once("<main>").unwrap_or_else(|| die("fixture".into()));
            let (main, z) = rest.split_once("</main>").unwrap_or_else(|| die("fixture".into()));
            let last = main.replacen("<div class='cell rel hid c1'>", "<div id=t class='cell rel hid c1'>", 1);
            format!("{a}{}<main>{last}</main>{z}", format!("<main>{main}</main>").repeat(scale - 1))
        }
        "stretch" => {
            let n = 40;
            let mut one = String::new();
            for _ in 0..n {
                one += "<div style='display:flex'>";
            }
            one += "<span>text</span>";
            for k in (0..n).rev() {
                one += &format!("<div style='height:{}px'></div></div>", 10 + 20 * (n - 1 - k));
            }
            format!("<body style='margin:0'>{}<div id=t>end</div>", one.repeat(scale))
        }
        "scroll" => {
            let n = 30;
            let mut one = String::new();
            for _ in 0..n {
                one += "<div style='display:flex'><div style='overflow:auto;max-height:300px'>";
            }
            one += "<span>text</span>";
            for k in (0..n).rev() {
                one += &format!("</div><div style='height:{}px'></div></div>", 10 + 20 * (n - 1 - k));
            }
            format!("<body style='margin:0'>{}<div id=t>end</div>", one.repeat(scale))
        }
        _ => die("usage: geometry_load dashboard|stretch|scroll 1|10|100".into()),
    }
}

fn exec(name: &str, scale: usize) {
    let html = page(name, scale);
    let viewport = (1280.0, 800.0);
    if name == "scroll" {
        // Only the bounded read: layout with no budget would take 2^30 layouts for each copy.
        let eval = EvalLimits { max_steps: u64::MAX / 2, ..Default::default() };
        let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
        let t = Instant::now();
        let r = b.run_script("document.getElementById('t').offsetWidth");
        println!("scroll x{scale}: read stopped after {:.1} ms with {r:?}; peak RSS {} MiB", t.elapsed().as_secs_f64() * 1e3, status_kib("VmHWM:") / 1024);
        return;
    }
    // The same page by hand, to time the cascade and layout alone.
    let mut doc = tree::parse(&html);
    let mut rules = RuleSet::new(viewport);
    rules.add(&mut doc, cascade::UA_CSS, Origin::UserAgent);
    for source in cascade::sources(&doc, viewport) {
        if let cascade::Source::Inline(t) = source {
            rules.add(&mut doc, &t, Origin::Author);
        }
    }
    let t = Instant::now();
    let styles = rules.compute(&doc);
    let cascade_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let direct = bare_layout::layout(&doc, &styles, viewport);
    let layout_ms = t.elapsed().as_secs_f64() * 1e3;
    drop((doc, styles));
    let eval = EvalLimits { max_steps: u64::MAX / 2, ..Default::default() };
    let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    b.set_geometry_limits(bare::browser_geometry::GeometryLimits { max_nodes: 4 << 20, max_text: 256 << 20, max_fragments: 16 << 20, max_depth: 96, max_work: 1 << 40 });
    let t = Instant::now();

    let r = b.run_script("document.getElementById('t').offsetWidth").unwrap_or_else(|e| die(format!("read: {e:?}")));
    let first_ms = t.elapsed().as_secs_f64() * 1e3;
    let width = b.it.display(r);
    let t = Instant::now();
    b.run_script("document.getElementById('t').offsetHeight").unwrap_or_else(|e| die(format!("read: {e:?}")));
    let second_ms = t.elapsed().as_secs_f64() * 1e3;
    let s = b.geometry_stats();
    println!(
        "{name} x{scale}: width {width}, fragments {}, work {} units (alone: {}), cascade {cascade_ms:.2} ms, layout {layout_ms:.2} ms, first read {first_ms:.2} ms, second read {second_ms:.2} ms, peak RSS {} MiB, peak heap {:.1} MiB, allocs {}",
        direct.fragments.len(),
        s.last_work,
        direct.work,
        status_kib("VmHWM:") / 1024,
        PEAK.load(Relaxed) as f64 / (1 << 20) as f64,
        ALLOCS.load(Relaxed)
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: geometry_load dashboard|stretch|scroll 1|10|100".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    std::thread::Builder::new().stack_size(64 << 20).spawn(move || exec(&name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
