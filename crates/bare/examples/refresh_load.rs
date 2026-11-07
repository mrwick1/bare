//! Load test for the frame after a small change in the window (`Page::handle`, `Page::paint`). One scale per process.
//!
//!   refresh_load LINES [ROUNDS]     LINES: lines of text in the page (1000, 10000, 100000)     ROUNDS: rounds of each change (default 50)
//!
//! The page is a form (a text field, a checkbox, a link, a button whose listener writes a status line) over LINES `<div>` lines, in a
//! 800 by 600 window, with `a:hover` and `input:focus` rules. Each change is timed from the window event (or the script) to the painted
//! frame. It prints the p50 and the max of each change, its allocations and bytes, and the layouts that it ran, then the RSS and the
//! CPU of 3 s of idle (must be 0 ticks). `crates/bare/src/page_tests.rs` has the checks. The stack of the VM thread is 64 MiB.

use bare::browser::*;
use bare::page::{Page, Source};
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_window::{App, Event, Mods};
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

const NO: Mods = Mods { shift: false, ctrl: false, alt: false, meta: false };

fn main() {
    let lines: usize = std::env::args().nth(1).and_then(|v| v.parse().ok()).unwrap_or(10_000);
    let rounds: usize = std::env::args().nth(2).and_then(|v| v.parse().ok()).unwrap_or(50);
    std::thread::Builder::new().stack_size(64 << 20).spawn(move || run(lines, rounds)).unwrap().join().unwrap();
}

fn click(p: &mut Page, x: f64, y: f64) {
    p.handle(Event::PointerMove { x, y, buttons: 0, mods: NO });
    for pressed in [true, false] {
        p.handle(Event::PointerButton { x, y, button: 0, buttons: pressed as u16, pressed, mods: NO });
    }
}

fn run(lines: usize, rounds: usize) {
    let mut html = String::from(
        "<!doctype html><style>body{margin:8px;font:16px/20px sans-serif}.b{border-left:3px solid #36c;padding-left:5px}a:hover{color:#d00}\
         input:focus{border-color:#36c}#form{height:100px}</style><div id=form><input id=i> <input id=c type=checkbox> <a id=l href='#x'>link</a> \
         <button id=go>go</button><div id=st>status</div></div>",
    );
    for i in 0..lines {
        if i % (lines / 20).max(1) == 0 {
            html.push_str("<div class=b>");
        }
        html.push_str(&format!("<div>line {i}: the quick brown fox jumps over the lazy dog</div>"));
        if i % (lines / 20).max(1) == (lines / 20).max(1) - 1 {
            html.push_str("</div>");
        }
    }
    let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    b.set_geometry_limits(bare::browser_geometry::GeometryLimits { max_nodes: 4 << 20, max_text: 256 << 20, max_fragments: 16 << 20, ..Default::default() });
    b.set_computed_limits(bare::browser_computed::ComputedLimits { max_nodes: 4 << 20, max_work: u64::MAX / 4, ..Default::default() });
    b.set_viewport(800.0, 600.0).expect("viewport");
    b.set_document_url("http://localhost/long.html").expect("url");
    b.run_script("var n = 0; document.getElementById('go').addEventListener('click', () => { document.getElementById('st').textContent = 'clicked ' + (++n); }); document.body.addEventListener('click', () => {}); 0").expect("script");
    drop(html);
    let mut p = Page::new(Source::Browser(Box::new(b)), (800, 600));
    let t = Instant::now();
    p.paint();
    println!("lines {lines}: first paint {:.1} ms, RSS {} KiB", t.elapsed().as_secs_f64() * 1e3, status_kib("VmRSS:"));
    // Where the controls are: the first line of the form.
    let at = |p: &mut Page, id: &str| -> (f64, f64) {
        let b = p.browser().unwrap();
        let v = b.run_script(&format!("var r = document.getElementById('{id}').getBoundingClientRect(); (r.left + r.width / 2) + ',' + (r.top + r.height / 2)")).unwrap();
        let s = b.it.display(v);
        let (x, y) = s.split_once(',').unwrap();
        (x.parse().unwrap(), y.parse().unwrap())
    };
    let (field, check, link, go) = (at(&mut p, "i"), at(&mut p, "c"), at(&mut p, "l"), at(&mut p, "go"));
    p.touched();
    p.paint();
    let mut sizes = [(800, 600), (780, 600)].into_iter().cycle();
    type Step<'a> = (&'a str, Box<dyn FnMut(&mut Page, usize) + 'a>);
    let steps: Vec<Step> = vec![
        ("click on a line (a listener that changes nothing)", Box::new(|p: &mut Page, _| click(p, 300.0, 300.0))),
        ("click in the text field (focus, caret)", Box::new(move |p: &mut Page, _| click(p, field.0, field.1))),
        ("a letter typed in the field", Box::new(|p: &mut Page, _| {
            for pressed in [true, false] {
                p.handle(Event::Key { key: "a".into(), code: "KeyA", pressed, repeat: false, text: pressed.then(|| "a".into()), mods: NO });
            }
        })),
        ("checkbox click", Box::new(move |p: &mut Page, _| click(p, check.0, check.1))),
        ("pointer onto and off the link (a:hover)", Box::new(move |p: &mut Page, i| {
            let y = if i % 2 == 0 { link.1 } else { 300.0 };
            p.handle(Event::PointerMove { x: link.0, y, buttons: 0, mods: NO });
        })),
        ("button click: the listener writes the status text", Box::new(move |p: &mut Page, _| click(p, go.0, go.1))),
        ("resize by 20 px", Box::new(move |p: &mut Page, _| {
            let (w, h) = sizes.next().unwrap();
            p.resize(w, h);
        })),
    ];
    for (name, mut f) in steps {
        let mut times = Vec::new();
        let (l0, c0) = (p.renders, p.restyles);
        let (a0, by0) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed));
        for i in 0..rounds {
            let t = Instant::now();
            f(&mut p, i);
            p.paint();
            times.push(t.elapsed().as_secs_f64() * 1e3);
        }
        let (a1, by1) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed));
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "  {name}: p50 {:.2} ms, max {:.2} ms; {:.0} allocs and {:.0} KiB a change; layouts {:.1} and cascades {:.1} a change",
            times[times.len() / 2],
            times[times.len() - 1],
            (a1 - a0) as f64 / rounds as f64,
            (by1 - by0) as f64 / rounds as f64 / 1024.0,
            (p.renders - l0) as f64 / rounds as f64,
            (p.restyles - c0) as f64 / rounds as f64
        );
    }
    println!("  RSS after the changes {} KiB, peak {} KiB", status_kib("VmRSS:"), status_kib("VmHWM:"));
    let c = cpu_ms();
    std::thread::sleep(Duration::from_secs(3));
    println!("  idle CPU in 3 s: {} ms", cpu_ms() - c);
}
