//! Tests of `browser_intersection`: `IntersectionObserver`. The Chrome cases first (48 of them), then what Chrome cannot show here: the cost of a page
//! with no observer, the gates, a margin that reaches outside the window (Chrome ran in an iframe), the bounds and the garbage collector.

use crate::browser::*;
use crate::browser_topic_tests::topic_test;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;

topic_test!(chrome_cases, "intersection", 48);

fn page(body: &str) -> Browser {
    let html = format!("<!doctype html><html style='overflow:hidden'><body style='margin:0'>{body}</body></html>");
    let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b.set_viewport(400.0, 300.0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(e) => format!("{e:?}"),
    }
}

const BOX: &str = "<div id=a style='position:absolute;left:10px;top:310px;width:50px;height:50px'></div>";

#[test]
fn the_window_margin_reaches_outside_the_window() {
    // Spec: the root rectangle of the top-level window grows by `rootMargin`. 310..320 of the target is inside 300 + 20.
    let mut b = page(BOX);
    js(&mut b, "var out = []; new IntersectionObserver(function (es) { out.push(es[0].isIntersecting + ':' + es[0].intersectionRatio.toFixed(2) + ':' + es[0].intersectionRect.height + ':' + es[0].rootBounds.bottom) }, { rootMargin: '20px' }).observe(document.getElementById('a'))");
    b.advance_time(100.0, 10_000).unwrap();
    assert_eq!(js(&mut b, "out.join()"), "true:0.20:10:320");
}

#[test]
fn a_page_with_no_observer_queues_nothing() {
    let mut b = page(BOX);
    let p = b.advance_time(500.0, 10_000).unwrap();
    assert_eq!((p.ran, p.next_due), (0, None));
    assert_eq!(js(&mut b, "typeof IntersectionObserver"), "function");
}

#[test]
fn an_idle_page_settles_after_the_first_frame() {
    let mut b = page(BOX);
    js(&mut b, "var n = 0; new IntersectionObserver(function () { n++ }).observe(document.getElementById('a'))");
    b.advance_time(1000.0, 10_000).unwrap();
    assert_eq!(js(&mut b, "n"), "1");
    // No frame is wanted now: the scheduler has no task.
    let p = b.advance_time(1000.0, 100).unwrap();
    assert_eq!((p.ran, p.next_due), (0, None));
}

#[test]
fn a_scroll_moves_the_target_and_reports() {
    let mut b = page("<div id=c style='position:absolute;left:0;top:0;width:100px;height:100px;overflow:hidden'><div style='height:200px'></div><div id=a style='height:50px;width:50px'></div></div>");
    js(&mut b, "var out = []; new IntersectionObserver(function (es) { out.push(es[0].isIntersecting) }).observe(document.getElementById('a'))");
    b.advance_time(100.0, 10_000).unwrap();
    js(&mut b, "document.getElementById('c').scrollTop = 150");
    b.advance_time(100.0, 10_000).unwrap();
    assert_eq!(js(&mut b, "out.join()"), "false,true");
}

#[test]
fn gates_are_unsupported() {
    let mut b = page(BOX);
    assert!(matches!(b.run_script("new IntersectionObserver(function () {}, { trackVisibility: true, delay: 100 })"), Err(Error::Unsupported(_))));
    let mut b = page("<div id=a style='position:sticky;top:0;width:10px;height:10px'></div>");
    js(&mut b, "new IntersectionObserver(function () {}).observe(document.getElementById('a'))");
    assert!(matches!(b.advance_time(100.0, 10_000), Err(Error::Unsupported(_))));
}

#[test]
fn the_observation_bound_is_a_limit() {
    let mut b = page("");
    let r = b.run_script("var o = new IntersectionObserver(function () {}); for (var i = 0; i < 20000; i++) o.observe(document.createElement('i'))");
    assert!(matches!(r, Err(Error::Limit(_))), "{r:?}");
}

#[test]
fn an_observer_with_a_target_survives_collection() {
    let mut b = page(BOX);
    js(&mut b, "var out = []; (function () { var o = new IntersectionObserver(function (es) { out.push(es.length) }); o.observe(document.getElementById('a')) })()");
    b.collect(&[]).unwrap();
    b.advance_time(100.0, 10_000).unwrap();
    assert_eq!(js(&mut b, "out.join()"), "1");
}

#[test]
fn observers_report_in_the_order_of_creation() {
    // Chrome keeps its observers in an unordered set (two runs gave two orders), so only the standard orders this.
    let mut b = page(BOX.replace("310", "20").as_str());
    js(&mut b, "var out = []; var a = new IntersectionObserver(function () { out.push('a') }), c = new IntersectionObserver(function () { out.push('c') }); c.observe(document.getElementById('a')); a.observe(document.getElementById('a'))");
    b.advance_time(100.0, 10_000).unwrap();
    assert_eq!(js(&mut b, "out.join()"), "a,c");
}
