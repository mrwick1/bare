//! Tests of `browser_frame`: `requestAnimationFrame` and the rendering step. The Chrome cases first, then what Chrome cannot show
//! (the frame grid of the virtual clock, the bounds, the interaction with `ResizeObserver`).

use crate::browser::*;
use crate::browser_topic_tests::topic_test;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

topic_test!(chrome_cases, "raf", 21);

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
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn a_frame_is_the_next_step_of_the_grid() {
    let mut b = page("");
    // The clock reads 1000 ms. The grid is 1000/60 ms, so the next frame is at 1000 + 1000/60.
    js(&mut b, "var t = []; requestAnimationFrame(function (x) { t.push(x) })");
    b.advance_time(16.0, 100).unwrap();
    assert_eq!(js(&mut b, "t.length"), "0");
    b.advance_time(1.0, 100).unwrap();
    assert_eq!(js(&mut b, "t.length + ' ' + (Math.abs(t[0] - 1016.6666666666667) < 1e-6)"), "1 true");
    // Nothing is queued: the clock may move and no frame runs.
    b.advance_time(500.0, 100).unwrap();
    assert_eq!(js(&mut b, "t.length"), "1");
    assert_eq!(b.next_scheduled_due(), None);
}

#[test]
fn a_frame_loop_runs_once_per_step_of_virtual_time() {
    let mut b = page("");
    js(&mut b, "var n = 0; function f() { n++; requestAnimationFrame(f) } requestAnimationFrame(f)");
    b.advance_time(1010.0, 1000).unwrap();
    assert_eq!(js(&mut b, "n"), "60");
}

#[test]
fn the_driver_that_does_not_move_the_clock_runs_no_frame() {
    let mut b = page("");
    js(&mut b, "var n = 0; requestAnimationFrame(function () { n++ })");
    b.run_scheduled_tasks(100).unwrap();
    assert_eq!(js(&mut b, "n"), "0");
}

#[test]
fn a_cancelled_callback_leaves_no_root() {
    let mut b = page("");
    js(&mut b, "var id = requestAnimationFrame(function () {}); cancelAnimationFrame(id)");
    b.advance_time(20.0, 100).unwrap();
    assert_eq!(js(&mut b, "typeof id"), "number");
}

#[test]
fn the_callback_count_is_bounded() {
    let mut b = page("");
    // A limit is never catchable: the script ends at the 65,537th call.
    assert_eq!(js(&mut b, "var f = function () {}, i = 0; try { for (; i < 70000; i++) requestAnimationFrame(f) } catch (e) {} i"), "limit:Host(\"animation-frame-callbacks\")");
    assert_eq!(js(&mut b, "i"), "65536");
}

#[test]
fn a_frame_runs_before_the_resize_observer_of_the_same_frame() {
    let mut b = page("<div id=t style='width:10px;height:10px'></div>");
    js(&mut b, "var log = []; var o = new ResizeObserver(function () { log.push('RO') }); o.observe(document.getElementById('t')); requestAnimationFrame(function () { log.push('raf'); document.getElementById('t').style.width = '20px' })");
    b.advance_time(20.0, 100).unwrap();
    assert_eq!(js(&mut b, "log.join(' ')"), "raf RO");
}
