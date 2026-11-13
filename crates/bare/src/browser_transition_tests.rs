//! Tests of `browser_motion`: `transitionrun/start/end/cancel` and `animationstart/iteration/end/cancel`, timed by the virtual clock.
//! A style change is seen by the next rendering step, so an event is checked with a margin of one frame (`FRAME`).

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

const FRAME: f64 = 17.0;

fn page(css: &str, body: &str) -> Browser {
    let html = format!("<!doctype html><html><head><style>{css}</style></head><body>{body}</body></html>");
    let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(Error::Limit(k)) => format!("limit:{k:?}"),
        Err(e) => format!("other:{e:?}"),
    }
}

/// Make `log`, listen for every motion event on `#d`, and read the style once (the "before-change" style of the page).
const LOG: &str = "var log = [], d = document.getElementById('d'); \
    ['transitionrun','transitionstart','transitionend','transitioncancel','animationstart','animationiteration','animationend','animationcancel'].forEach(function (t) { \
        d.addEventListener(t, function (e) { log.push(t + ':' + (e.propertyName || e.animationName) + ':' + e.elapsedTime + ':' + e.bubbles + e.cancelable + ':' + JSON.stringify(e.pseudoElement)) }) }); \
    d.getBoundingClientRect()";

fn log(b: &mut Browser) -> String {
    js(b, "var s = log.join(' '); log.length = 0; s")
}

fn run(b: &mut Browser, ms: f64) {
    b.advance_time(ms, 1000).unwrap();
}

#[test]
fn inline_transition_runs_starts_and_ends() {
    let mut b = page("", "<div id=d style='opacity:1;transition:opacity 100ms linear'></div>");
    js(&mut b, LOG);
    js(&mut b, "d.style.opacity = '0'");
    run(&mut b, FRAME);
    assert_eq!(log(&mut b), "transitionrun:opacity:0:truefalse:\"\" transitionstart:opacity:0:truefalse:\"\"");
    run(&mut b, 100.0);
    assert_eq!(log(&mut b), "transitionend:opacity:0.1:truefalse:\"\"");
    run(&mut b, 1000.0);
    assert_eq!(log(&mut b), "");
}

#[test]
fn stylesheet_transition_waits_for_its_delay() {
    let mut b = page(".a{transition:transform 200ms 50ms} .b{transform:translateX(10px)}", "<div id=d class=a></div>");
    js(&mut b, LOG);
    js(&mut b, "d.className = 'a b'");
    run(&mut b, 49.0);
    assert_eq!(log(&mut b), "transitionrun:transform:0:truefalse:\"\"");
    run(&mut b, 50.0 - 49.0 + FRAME);
    assert_eq!(log(&mut b), "transitionstart:transform:0:truefalse:\"\"");
    run(&mut b, 200.0 - FRAME - 5.0);
    assert_eq!(log(&mut b), "");
    run(&mut b, FRAME + 10.0);
    assert_eq!(log(&mut b), "transitionend:transform:0.2:truefalse:\"\"");
}

#[test]
fn zero_duration_and_unlisted_properties_fire_nothing() {
    let mut b = page("", "<div id=d style='opacity:1;transition:opacity 0s'></div>");
    js(&mut b, LOG);
    js(&mut b, "d.style.opacity = '0'");
    run(&mut b, 500.0);
    assert_eq!(log(&mut b), "");
    let mut b = page("", "<div id=d style='opacity:1;color:red;transition:opacity 100ms'></div>");
    js(&mut b, LOG);
    js(&mut b, "d.style.color = 'blue'");
    run(&mut b, 500.0);
    assert_eq!(log(&mut b), "");
}

#[test]
fn all_covers_every_property() {
    let mut b = page("", "<div id=d style='color:red;transition:all 100ms'></div>");
    js(&mut b, LOG);
    js(&mut b, "d.style.color = 'blue'");
    run(&mut b, 300.0);
    assert_eq!(log(&mut b), "transitionrun:color:0:truefalse:\"\" transitionstart:color:0:truefalse:\"\" transitionend:color:0.1:truefalse:\"\"");
}

#[test]
fn removing_the_element_cancels_the_transition() {
    let mut b = page("", "<div id=d style='opacity:1;transition:opacity 200ms'></div>");
    js(&mut b, LOG);
    js(&mut b, "d.style.opacity = '0'");
    run(&mut b, 40.0);
    js(&mut b, "d.remove()");
    run(&mut b, 1000.0);
    assert_eq!(log(&mut b), "transitionrun:opacity:0:truefalse:\"\" transitionstart:opacity:0:truefalse:\"\" transitioncancel:opacity:0:truefalse:\"\"");
}

#[test]
fn display_none_cancels_the_transition() {
    let mut b = page("", "<div id=d style='opacity:1;transition:opacity 200ms'></div>");
    js(&mut b, LOG);
    js(&mut b, "d.style.opacity = '0'");
    run(&mut b, 40.0);
    js(&mut b, "d.style.display = 'none'");
    run(&mut b, 1000.0);
    assert!(log(&mut b).ends_with("transitioncancel:opacity:0:truefalse:\"\""));
}

#[test]
fn removing_the_transition_property_cancels_it() {
    let mut b = page("", "<div id=d style='opacity:1;transition:opacity 200ms'></div>");
    js(&mut b, LOG);
    js(&mut b, "d.style.opacity = '0'");
    run(&mut b, 40.0);
    js(&mut b, "d.style.transition = 'none'");
    run(&mut b, 1000.0);
    assert!(log(&mut b).ends_with("transitioncancel:opacity:0:truefalse:\"\""));
}

#[test]
fn keyframes_animation_starts_iterates_and_ends() {
    let mut b = page("@keyframes k{from{opacity:0}to{opacity:1}} .k{animation:k 100ms 2}", "<div id=d></div>");
    js(&mut b, LOG);
    js(&mut b, "d.className = 'k'");
    run(&mut b, FRAME);
    assert_eq!(log(&mut b), "animationstart:k:0:truefalse:\"\"");
    run(&mut b, 100.0);
    assert_eq!(log(&mut b), "animationiteration:k:0.1:truefalse:\"\"");
    run(&mut b, 100.0);
    assert_eq!(log(&mut b), "animationend:k:0.2:truefalse:\"\"");
    run(&mut b, 1000.0);
    assert_eq!(log(&mut b), "");
}

#[test]
fn animation_without_keyframes_fires_nothing() {
    let mut b = page(".k{animation:nope 100ms}", "<div id=d></div>");
    js(&mut b, LOG);
    js(&mut b, "d.className = 'k'");
    run(&mut b, 500.0);
    assert_eq!(log(&mut b), "");
}

#[test]
fn removing_the_element_cancels_the_animation() {
    let mut b = page("@keyframes k{from{opacity:0}to{opacity:1}} .k{animation:k 200ms}", "<div id=d></div>");
    js(&mut b, LOG);
    js(&mut b, "d.className = 'k'");
    run(&mut b, 50.0);
    js(&mut b, "d.remove()");
    run(&mut b, 1000.0);
    assert_eq!(log(&mut b), "animationstart:k:0:truefalse:\"\" animationcancel:k:0:truefalse:\"\"");
}

#[test]
fn infinite_animation_never_ends_and_does_not_block_settling() {
    let mut b = page("@keyframes k{from{opacity:0}to{opacity:1}} .k{animation:k 1s infinite}", "<div id=d></div>");
    js(&mut b, LOG);
    js(&mut b, "d.className = 'k'");
    run(&mut b, 3500.0);
    let l = log(&mut b);
    assert!(l.starts_with("animationstart:k:0:truefalse:\"\" animationiteration:k:1:truefalse:\"\""), "{l}");
    assert!(!l.contains("animationend"), "{l}");
    // Nothing that the settle rule waits for stays queued.
    assert!(!b.blocks_settle(1e12));
}

#[test]
fn finished_transition_queues_nothing() {
    let mut b = page("", "<div id=d style='opacity:1;transition:opacity 100ms'></div>");
    js(&mut b, LOG);
    js(&mut b, "d.style.opacity = '0'");
    run(&mut b, 300.0);
    assert!(!b.blocks_settle(1e12));
}

#[test]
fn style_object_knows_transition_and_animation() {
    let mut b = page("", "<div id=d></div>");
    assert_eq!(js(&mut b, "['transition','animation','transitionProperty','animationName','animationIterationCount'].map(function (k) { return k in document.body.style }).join()"), "true,true,true,true,true");
    assert_eq!(js(&mut b, "var d = document.getElementById('d'); d.style.transition = 'opacity 1s'; d.style.transition"), "opacity 1s");
    assert_eq!(js(&mut b, "d.style.transitionDuration + '|' + d.style.transitionProperty"), "1s|opacity");
    assert_eq!(js(&mut b, "getComputedStyle(d).transitionDuration + '|' + getComputedStyle(d).animationName"), "1s|none");
}
