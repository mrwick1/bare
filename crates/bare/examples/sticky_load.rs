//! Load test for `position: sticky` through the window boundary (`Page::handle`, `Page::paint`). One scale per process.
//!
//!   sticky_load N [STEPS]     N: sticky boxes in the page (1, 100, 1000)     STEPS: wheel steps (default 100)
//!
//! The page is N sections in 40,000 px, each with a `position: sticky; top: 0` header and a line of text, in an 800 by 600 window. It is built and
//! painted once (cascade, layout, areas, display list, raster), then wheeled down STEPS times of 53 px with a paint after each step. The last header
//! that the window shows is read with `getBoundingClientRect` and checked against the rule. Prints the time of the first paint, the total, p50 and
//! max of the steps, layouts counted by the browser (must not grow), and the RSS and the peak RSS. `tools/js/sticky_load.sh` runs the three scales.
//! The stack of the VM thread is 64 MiB.

use bare::browser::*;
use bare::page::{Page, Source};
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_window::{Event, Mods};
use std::time::Instant;

fn status_kib(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok()).unwrap_or(0)
}

fn main() {
    let n: usize = std::env::args().nth(1).and_then(|v| v.parse().ok()).unwrap_or(100);
    let steps: usize = std::env::args().nth(2).and_then(|v| v.parse().ok()).unwrap_or(100);
    std::thread::Builder::new().stack_size(64 << 20).spawn(move || run(n, steps)).unwrap().join().unwrap();
}

fn run(n: usize, steps: usize) {
    let section = (40_000 / n).max(40);
    let mut html = format!("<!doctype html><style>body{{margin:0;font:16px/20px sans-serif}} .s{{height:{section}px}} .h{{position:sticky;top:0;height:20px;background:#36c;color:#fff}}</style>");
    for i in 0..n {
        html.push_str(&format!("<div class=s><div class=h id=h{i}>header {i}</div>row {i}</div>"));
    }
    let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    b.set_geometry_limits(bare::browser_geometry::GeometryLimits { max_nodes: 4 << 20, max_text: 256 << 20, max_fragments: 16 << 20, ..Default::default() });
    b.set_computed_limits(bare::browser_computed::ComputedLimits { max_nodes: 4 << 20, ..Default::default() });
    b.set_viewport(800.0, 600.0).expect("viewport");
    b.set_document_url("http://localhost/sticky.html").expect("url");
    drop(html);
    let mut p = Page::new(Source::Browser(Box::new(b)), (800, 600));
    let t = Instant::now();
    p.paint();
    let first = t.elapsed();
    let layouts0 = p.browser().unwrap().geometry_stats().layouts;
    let no = Mods { shift: false, ctrl: false, alt: false, meta: false };
    let mut times: Vec<f64> = Vec::with_capacity(steps);
    let all = Instant::now();
    for _ in 0..steps {
        let t = Instant::now();
        p.handle(Event::Wheel { x: 400.0, y: 300.0, dx: 0.0, dy: 53.0, mods: no });
        p.paint();
        times.push(t.elapsed().as_secs_f64() * 1e6);
    }
    let total = all.elapsed();
    let layouts1 = p.browser().unwrap().geometry_stats().layouts;
    let y = p.browser().unwrap().scroll().y as usize;
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    // The header of the section at the top of the window: its page place is `y` or the start of its section, and at most 20 px above the end.
    let k = (y / section).min(n - 1);
    let got = p.browser().unwrap().run_script(&format!("document.getElementById('h{k}').getBoundingClientRect().top")).expect("read");
    let got = p.browser().unwrap().it.display(got);
    let want = (y.max(k * section).min((k + 1) * section - 20) as i64 - y as i64).to_string();
    println!("sticky {n}: first paint (layout, areas, list, raster) {:.1} ms; {steps} steps with a paint: total {:.1} ms, p50 {:.0} us, max {:.0} us; layouts {layouts0} -> {layouts1}", first.as_secs_f64() * 1e3, total.as_secs_f64() * 1e3, times[times.len() / 2], times[times.len() - 1]);
    println!("  scrollY {y}, header {k} top {got} (want {want}: {}); RSS {} KiB, peak {} KiB", if got == want { "ok" } else { "WRONG" }, status_kib("VmRSS:"), status_kib("VmHWM:"));
    assert_eq!(got, want);
    assert_eq!(layouts0, layouts1, "a scroll only draws again");
}
