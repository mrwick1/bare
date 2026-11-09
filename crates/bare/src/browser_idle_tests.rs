//! Tests of `browser_idle`, `checkVisibility` and `draggable`. The Chrome cases first, then what Chrome cannot show (the virtual clock,
//! the deadline, the timeout task, the gates, the bounds).

use crate::browser::*;
use crate::browser_topic_tests::topic_test;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

topic_test!(chrome_idle_cases, "idle", 30);
topic_test!(chrome_visibility_cases, "visibility", 28);
topic_test!(chrome_draggable_cases, "draggable", 13);

fn page(body: &str) -> Browser {
    let mut b = Browser::new(tree::parse(&format!("<!doctype html><body>{body}</body>")), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Limit(k)) => format!("limit:{k:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn a_page_that_never_asks_queues_nothing() {
    let mut b = page("");
    js(&mut b, "var x = 1");
    assert_eq!(b.next_scheduled_due(), None);
    b.advance_time(500.0, 100).unwrap();
    assert_eq!(b.next_scheduled_due(), None);
}

#[test]
fn the_period_is_bounded_by_the_next_timer_and_by_50_ms() {
    let mut b = page("");
    js(&mut b, "var r = []; requestIdleCallback(function (d) { r.push(d.timeRemaining()) }); setTimeout(function () {}, 7);");
    b.advance_time(1.0, 100).unwrap();
    assert_eq!(js(&mut b, "r.join()"), "7");
    js(&mut b, "r = []; requestIdleCallback(function (d) { r.push(d.timeRemaining()) });");
    b.advance_time(1.0, 100).unwrap();
    assert_eq!(js(&mut b, "r.join()"), "6");
    js(&mut b, "r = []; requestIdleCallback(function (d) { r.push(d.timeRemaining()) });");
    b.advance_time(20.0, 100).unwrap();
    js(&mut b, "r = []; requestIdleCallback(function (d) { r.push(d.timeRemaining()) });");
    b.advance_time(1.0, 100).unwrap();
    // The timer is gone: the period is 50 ms.
    assert_eq!(js(&mut b, "r.join()"), "50");
}

#[test]
fn a_callback_that_asks_again_waits_one_period() {
    let mut b = page("");
    js(&mut b, "var t = []; function f() { t.push(performance.now()); requestIdleCallback(f) } requestIdleCallback(f);");
    b.advance_time(120.0, 1000).unwrap();
    assert_eq!(js(&mut b, "t.join()"), "1000,1050,1100");
}

#[test]
fn a_frame_loop_does_not_starve_idle_callbacks() {
    let mut b = page("");
    js(&mut b, "var log = []; function f() { log.push('f'); requestAnimationFrame(f) } requestAnimationFrame(f); requestIdleCallback(function (d) { log.push('i' + (d.timeRemaining() < 17)) });");
    b.advance_time(40.0, 1000).unwrap();
    assert_eq!(js(&mut b, "log.join(' ')"), "f itrue f");
}

#[test]
fn a_timeout_runs_the_callback_before_a_late_period() {
    let mut b = page("");
    // The period waits for the frame (16.7 ms). The timeout is 5 ms.
    js(&mut b, "var r = []; requestAnimationFrame(function () { r.push('raf') }); requestIdleCallback(function (d) { r.push('idle:' + d.didTimeout + ':' + d.timeRemaining()) }, { timeout: 5 });");
    b.advance_time(30.0, 100).unwrap();
    assert_eq!(js(&mut b, "r.join(' ')"), "idle:true:0 raf");
}

#[test]
fn a_callback_that_ran_leaves_no_timeout_task() {
    let mut b = page("");
    js(&mut b, "requestIdleCallback(function () {}, { timeout: 100000 });");
    b.advance_time(1.0, 100).unwrap();
    assert_eq!(b.next_scheduled_due(), None);
    js(&mut b, "cancelIdleCallback(requestIdleCallback(function () {}, { timeout: 100000 }));");
    b.advance_time(1.0, 100).unwrap();
    assert_eq!(b.next_scheduled_due(), None);
}

#[test]
fn a_throw_is_reported_and_the_rest_runs() {
    let mut b = page("");
    js(&mut b, "var n = 0; requestIdleCallback(function () { throw new Error('x') }); requestIdleCallback(function () { n++ });");
    b.advance_time(1.0, 100).unwrap();
    assert_eq!(js(&mut b, "n"), "1");
    assert_eq!(b.take_reports().len(), 1);
}

#[test]
fn a_deadline_object_outside_its_callback_still_answers() {
    let mut b = page("");
    js(&mut b, "var d; requestIdleCallback(function (x) { d = x });");
    b.advance_time(1.0, 100).unwrap();
    assert_eq!(js(&mut b, "typeof d.timeRemaining() + ' ' + d.didTimeout"), "number false");
    b.collect(&[]).unwrap();
    assert_eq!(js(&mut b, "d.didTimeout"), "false");
}

#[test]
fn the_callback_count_is_bounded() {
    let mut b = page("");
    assert_eq!(js(&mut b, "var f = function () {}, i = 0; try { for (; i < 70000; i++) requestIdleCallback(f) } catch (e) {} i"), "limit:Host(\"idle-callbacks\")");
    assert_eq!(js(&mut b, "i"), "65536");
}

#[test]
fn check_visibility_gates_what_the_engine_does_not_model() {
    let mut b = page("<style>.x { content-visibility: auto }</style><div id=a>a</div><div id=h style='display:none'></div>");
    assert_eq!(js(&mut b, "document.getElementById('a').checkVisibility()"), "true");
    // A box that is certainly absent is false without a guess.
    assert_eq!(js(&mut b, "document.getElementById('h').checkVisibility()"), "false");
    let mut b = page("<div id=a style='display: contents'>a</div>");
    assert!(js(&mut b, "document.getElementById('a').checkVisibility()").starts_with("unsupported:"));
    let mut b = page("<svg id=s></svg><select><option id=o></option></select>");
    assert!(js(&mut b, "document.getElementById('s').checkVisibility()").starts_with("unsupported:"));
    assert!(js(&mut b, "document.getElementById('o').checkVisibility()").starts_with("unsupported:"));
}

#[test]
fn check_visibility_ignores_auto_unless_requested() {
    let mut b = page("<div style='content-visibility: auto'><div id=a>visible</div></div>");
    assert_eq!(js(&mut b, "document.getElementById('a').checkVisibility()"), "true");
    assert!(js(&mut b, "document.getElementById('a').checkVisibility({contentVisibilityAuto: true})").starts_with("unsupported:"));
}
