//! Tests of scrolling in the browser host (`browser_scroll.rs`, `page.rs`).
//!
//! The main test replays the steps of `tools/fixtures/ui/scroll-steps.txt` on `tools/fixtures/ui/scroll.html` and compares each result with the
//! one that headless Chrome gave (`tools/fixtures/ui/chrome-scroll.txt`, made by `tools/ui/chrome_scroll.mjs`): sizes, positions, rectangles,
//! the count of `scroll` events and the keys. Chrome is the oracle. The other tests use the window boundary (`Page::handle`).

use crate::browser::*;
use crate::page::{Page, Source};
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_window::{Event, Mods};

const FIXTURE: &str = include_str!("../../../tools/fixtures/ui/scroll.html");
const CHROME: &str = include_str!("../../../tools/fixtures/ui/chrome-scroll.txt");
const NO: Mods = Mods { shift: false, ctrl: false, alt: false, meta: false };

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn page_of(html: &str, w: u32, h: u32) -> Page {
    let script = html.split("<script>").nth(1).and_then(|s| s.split("</script>").next()).unwrap_or("");
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    b.set_viewport(w as f32, h as f32).expect("viewport");
    b.set_document_url("http://localhost/scroll.html").expect("url");
    if !script.is_empty() {
        b.run_script(script).expect("script");
    }
    Page::new(Source::Browser(Box::new(b)), (w, h))
}

fn eval(p: &mut Page, src: &str) -> String {
    let b = p.browser().expect("browser");
    let v = b.run_script(src).unwrap_or_else(|e| panic!("{src}: {e:?}"));
    b.it.display(v)
}

fn key(p: &mut Page, name: &str, shift: bool) {
    let (key, code): (String, &'static str) = match name {
        "Space" => (" ".into(), "Space"),
        n => (n.into(), Box::leak(n.to_string().into_boxed_str())),
    };
    for pressed in [true, false] {
        let text = (pressed && name == "Space").then(|| " ".to_string());
        p.handle(Event::Key { key: key.clone(), code, pressed, repeat: false, text, mods: Mods { shift, ..NO } });
    }
}

fn wheel(p: &mut Page, x: f64, y: f64, dy: f64) {
    p.handle(Event::Wheel { x, y, dx: 0.0, dy, mods: NO });
}

#[test]
fn scroll_matches_chrome() {
    big(|| {
        let mut p = page_of(FIXTURE, 400, 300);
        for line in CHROME.lines() {
            let (step, want) = line.split_once(" => ").expect("a result");
            let got = if let Some(js) = step.strip_prefix("JS ") {
                eval(&mut p, js)
            } else if step == "WAIT" {
                p.browser().unwrap().advance_time(300.0, 1000).expect("tasks");
                String::new()
            } else {
                let k = step.strip_prefix("KEY ").expect("a key");
                let (name, shift) = k.split_once('+').map_or((k, false), |(n, _)| (n, true));
                key(&mut p, name, shift);
                eval(&mut p, "scrollY")
            };
            assert_eq!(got, want, "step: {step}");
        }
    });
}

const LONG: &str = "<!doctype html><style>body{margin:0}#a{height:1000px}#f{position:fixed;left:0;top:0;width:50px;height:20px;background:#f00}\
#c{width:200px;height:100px;overflow:auto}#c div{height:500px}a{display:block;height:40px}</style>\
<div id=f></div><div id=c><div></div></div><div id=a></div><a id=l href=\"/next\">link</a><input id=i style=\"display:block\"><div style=\"height:900px\"></div>";

#[test]
fn the_wheel_scrolls_the_container_under_the_pointer_then_the_page() {
    big(|| {
        let mut p = page_of(LONG, 400, 300);
        wheel(&mut p, 50.0, 50.0, 60.0);
        assert_eq!(eval(&mut p, "document.getElementById('c').scrollTop + ',' + scrollY"), "60,0");
        // The container is at its end: the wheel goes on to the page.
        wheel(&mut p, 50.0, 50.0, 1000.0);
        wheel(&mut p, 50.0, 50.0, 100.0);
        assert_eq!(eval(&mut p, "document.getElementById('c').scrollTop + ',' + scrollY"), "400,100");
        // Over the page, not the container.
        wheel(&mut p, 300.0, 200.0, 40.0);
        assert_eq!(eval(&mut p, "scrollY"), "140");
        wheel(&mut p, 300.0, 200.0, -1000.0);
        assert_eq!(eval(&mut p, "scrollY"), "0");
    });
}

#[test]
fn a_click_below_the_fold_hits_what_is_there_after_the_scroll() {
    big(|| {
        let mut p = page_of(LONG, 400, 300);
        eval(&mut p, "var hits = []; document.addEventListener('click', function (e) { hits.push(e.target.id) }); 0");
        wheel(&mut p, 300.0, 200.0, 600.0);
        wheel(&mut p, 300.0, 200.0, 400.0);
        // The link is at 1100 on the page, below the 300 px window. After 1000 px the link is 100 px from the top.
        let y: f64 = eval(&mut p, "Math.round(document.getElementById('l').getBoundingClientRect().top + 5)").parse().unwrap();
        assert!((0.0..300.0).contains(&y), "the link is in the window: {y}");
        for pressed in [true, false] {
            p.handle(Event::PointerButton { x: 10.0, y, button: 0, buttons: pressed as u16, pressed, mods: NO });
        }
        assert_eq!(eval(&mut p, "hits.join()"), "l");
        // A fixed box stays at the top of the window.
        p.handle(Event::PointerMove { x: 10.0, y: 10.0, buttons: 0, mods: NO });
        for pressed in [true, false] {
            p.handle(Event::PointerButton { x: 10.0, y: 10.0, button: 0, buttons: pressed as u16, pressed, mods: NO });
        }
        assert_eq!(eval(&mut p, "hits.join()"), "l,f");
    });
}

#[test]
fn tab_scrolls_an_offscreen_field_into_view() {
    big(|| {
        let mut p = page_of(LONG, 400, 300);
        key(&mut p, "Tab", false); // the link
        key(&mut p, "Tab", false); // the field, far below the fold
        assert_eq!(eval(&mut p, "document.activeElement.id"), "i");
        let top: f64 = eval(&mut p, "document.getElementById('i').getBoundingClientRect().top").parse().unwrap();
        assert!((0.0..300.0).contains(&top), "the field is in the window: {top}");
    });
}

#[test]
fn a_scrolled_page_is_drawn_shifted_and_the_fixed_box_stays() {
    big(|| {
        let html = "<!doctype html><style>body{margin:0}#a{height:100px;background:#00f}#b{height:100px;background:#0f0}#c{height:600px}#f{position:fixed;left:0;top:0;width:20px;height:20px;background:#f00}</style><div id=a></div><div id=b></div><div id=c></div><div id=f></div>";
        let mut p = page_of(html, 200, 150);
        let w = 200usize;
        assert_eq!(p.paint()[10 * w + 100], 0xFF00_00FF);
        wheel(&mut p, 100.0, 100.0, 100.0);
        let px = p.paint();
        assert_eq!(px[10 * w + 100], 0xFF00_FF00, "the green box moved up");
        assert_eq!(px[10 * w + 10], 0xFFFF_0000, "the fixed box did not move");
        // The page bar: the track at the right edge is light, the thumb dark.
        assert_eq!(px[100 * w + w - 5], 0xFFFC_FCFC);
    });
}

#[test]
fn the_page_does_not_lay_out_again_for_a_scroll() {
    big(|| {
        let mut p = page_of(LONG, 400, 300);
        p.paint();
        let before = p.browser().unwrap().geometry_stats().layouts;
        for _ in 0..5 {
            wheel(&mut p, 300.0, 200.0, 50.0);
            p.paint();
        }
        assert_eq!(p.browser().unwrap().geometry_stats().layouts, before, "a scroll only draws again");
    });
}
