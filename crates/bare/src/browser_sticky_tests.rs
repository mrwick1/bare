//! Tests of `position: sticky` (`bare_layout::sticky`, read through `getBoundingClientRect`, `offsetTop` and `IntersectionObserver`). The Chrome cases
//! first (52 of them, `tools/fixtures/ui/sticky-cases.txt`), then what Chrome cannot show here.

use crate::browser_topic_tests::topic_test;

topic_test!(chrome_cases, "sticky", 52);

use crate::browser::*;
use crate::page::{Page, Source};
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;

fn browser(html: &str, w: f32, h: f32) -> Browser {
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    b.set_viewport(w, h).expect("viewport");
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    let v = b.run_script(src).unwrap_or_else(|e| panic!("{src}: {e:?}"));
    b.it.display(v)
}

/// The window drawn at three scroll offsets has the pixels of headless Chrome (`tools/ui/chrome_sticky_pixels.mjs`): the sticky box is drawn at
/// the stuck place, over the page, and a sticky box inside a box is cut at the end of it.
#[test]
fn the_window_draws_the_stuck_boxes_as_chrome_does() {
    const HTML: &str = include_str!("../../../tools/fixtures/ui/sticky-pixels.html");
    const CHROME: &str = include_str!("../../../tools/fixtures/ui/chrome-sticky-pixels.txt");
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            let mut p = Page::new(Source::Browser(Box::new(browser(HTML, 200.0, 150.0))), (200, 150));
            p.whole = 0;
            let mut shots = 0;
            let mut y = 0;
            for line in CHROME.lines() {
                if let Some(at) = line.strip_prefix('@') {
                    y = at.parse::<u32>().unwrap();
                    p.browser().unwrap().run_script(&format!("scrollTo(0, {y})")).unwrap();
                    shots += 1;
                    continue;
                }
                let (rows, runs) = line.split_once(' ').unwrap();
                let (r0, r1) = rows.split_once('-').unwrap();
                let px = p.paint().to_vec();
                for row in r0.parse::<usize>().unwrap()..=r1.parse::<usize>().unwrap() {
                    for run in runs.split(' ') {
                        let (xs, color) = run.split_once('=').unwrap();
                        let (x0, x1) = xs.split_once('-').unwrap();
                        let want = 0xFF00_0000 | u32::from_str_radix(color, 16).unwrap();
                        for x in x0.parse::<usize>().unwrap()..=x1.parse::<usize>().unwrap() {
                            assert_eq!(px[row * 200 + x], want, "scroll {y}, pixel {x},{row}: chrome {want:08x}");
                        }
                    }
                }
            }
            assert_eq!(shots, 4);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn a_sticky_box_inside_an_inline_box_that_is_not_atomic_is_unsupported() {
    let mut b = browser("<!doctype html><body style='margin:0'><span id=a style='position:sticky;top:0'>x</span>", 400.0, 300.0);
    assert!(matches!(b.run_script("document.getElementById('a').getBoundingClientRect().top"), Err(Error::Unsupported(_))));
}

/// A wheel scroll draws the list again and never lays out again, with sticky boxes too (they are one more offset at draw time).
#[test]
fn a_wheel_scroll_moves_the_sticky_box_and_does_not_lay_out_again() {
    use bare_window::{Event, Mods};
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            let html = "<!doctype html><style>body{margin:0}#a{position:sticky;top:10px;height:20px;background:#f00}#u{height:2000px}</style><div style='height:100px'></div><div id=a></div><div id=u></div>";
            let mut p = Page::new(Source::Browser(Box::new(browser(html, 200.0, 150.0))), (200, 150));
            p.whole = 0;
            assert_eq!(p.paint()[110 * 200 + 50], 0xFFFF_0000, "at its normal place");
            let layouts = p.browser().unwrap().geometry_stats().layouts;
            for _ in 0..3 {
                p.handle(Event::Wheel { x: 100.0, y: 100.0, dx: 0.0, dy: 100.0, mods: Mods { shift: false, ctrl: false, alt: false, meta: false } });
                p.paint();
            }
            let px = p.paint();
            assert_eq!(px[15 * 200 + 50], 0xFFFF_0000, "stuck at 10..30");
            assert_eq!(px[110 * 200 + 50], 0xFFFF_FFFF);
            assert_eq!(p.browser().unwrap().geometry_stats().layouts, layouts, "a scroll only draws again");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// A still picture (`bare --paint`) draws a sticky box at its place for the offset 0: `top: 50px` pushes a box that is at 20 down to 50.
#[test]
fn a_still_picture_draws_the_sticky_box_pushed_by_its_inset() {
    let mut doc = tree::parse("<!doctype html><style>body{margin:0}#a{position:sticky;top:50px;height:20px;background:#f00}</style><div style='height:20px'></div><div id=a></div><div style='height:900px'></div>");
    let viewport = (200.0, 150.0);
    let mut rules = bare_css::cascade::RuleSet::new(viewport);
    rules.add(&mut doc, bare_css::cascade::UA_CSS, bare_css::cascade::Origin::UserAgent);
    for source in bare_css::cascade::sources(&doc, viewport) {
        if let bare_css::cascade::Source::Inline(text) = source {
            rules.add(&mut doc, &text, bare_css::cascade::Origin::Author);
        }
    }
    let styles = rules.compute(&doc);
    let layout = bare_layout::layout(&doc, &styles, viewport);
    let c = bare_paint::capture(&doc, &styles, &layout, 200, 150);
    assert_eq!(c.pixels[25 * 200 + 5], 0xFFFF_FFFF, "not at its normal place (20..40)");
    assert_eq!(c.pixels[55 * 200 + 5], 0xFFFF_0000, "at 50..70");
}

/// A sticky box moves with the offsets of the moment: after a scroll in a script the next read is new, and a scroll back gives the first answer.
#[test]
fn a_read_after_a_scroll_gives_the_new_place_and_a_scroll_back_the_old_one() {
    let mut b = browser("<!doctype html><body style='margin:0;overflow:hidden'><div style='height:100px'></div><div id=a style='position:sticky;top:20px;height:30px;width:50px'><div id=k style='height:10px'></div></div><div style='height:2000px'></div>", 400.0, 300.0);
    let r = |b: &mut Browser, y: u32| js(b, &format!("scrollTo(0, {y}); var r = document.getElementById('a').getBoundingClientRect(), k = document.getElementById('k').getBoundingClientRect(); r.top + ' ' + k.top + ' ' + document.getElementById('a').offsetTop"));
    assert_eq!(r(&mut b, 0), "100 100 100");
    assert_eq!(r(&mut b, 50), "50 50 100");
    assert_eq!(r(&mut b, 200), "20 20 220");
    assert_eq!(r(&mut b, 0), "100 100 100");
}
