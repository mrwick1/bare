//! Tests of `browser_resize`: `ResizeObserver` over the native cascade and layout, delivered by the rendering step of the scheduler.
//! Expected sizes come from the CSS box model and the monospace font of the engine (see `browser_geometry_tests.rs`), not from layout.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind};
use crate::browser_resize::ResizeLimits;
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

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

/// Run one frame (the rendering step is due 16 ms after a change) and give the log, then clear it.
fn frame(b: &mut Browser) -> String {
    b.advance_time(20.0, 100).unwrap();
    js(b, "var s = log.join(' | '); log.length = 0; s")
}

const LOG: &str = "var log = []; function show(es) { for (var e of es) { var b = e.borderBoxSize[0], c = e.contentBoxSize[0], r = e.contentRect; \
    log.push(e.target.id + ' b=' + b.inlineSize + 'x' + b.blockSize + ' c=' + c.inlineSize + 'x' + c.blockSize + ' r=' + r.x + ',' + r.y + ',' + r.width + ',' + r.height) } }";

#[test]
fn interfaces_and_argument_checks() {
    let mut b = page("", "<div id=t></div>");
    assert_eq!(js(&mut b, "typeof ResizeObserver + typeof ResizeObserverEntry + typeof ResizeObserverSize"), "functionfunctionfunction");
    assert_eq!(js(&mut b, "ResizeObserver()"), "throw:TypeError:Failed to construct: please use the 'new' operator");
    assert_eq!(js(&mut b, "new ResizeObserver(1)"), "throw:TypeError:Failed to construct 'ResizeObserver': parameter 1 is not of type 'Function'.");
    assert_eq!(js(&mut b, "new ResizeObserverEntry()"), "throw:TypeError:Illegal constructor");
    assert_eq!(js(&mut b, "var o = new ResizeObserver(function () {}); o.observe(document)"), "throw:TypeError:Failed to execute 'observe' on 'ResizeObserver': parameter 1 is not of type 'Element'.");
    assert_eq!(js(&mut b, "o.observe(document.getElementById('t'), { box: 'margin-box' })"), "throw:TypeError:Failed to execute 'observe' on 'ResizeObserver': The provided value 'margin-box' is not a valid enum value of type ResizeObserverBoxOptions.");
    assert_eq!(js(&mut b, "o.observe(document.getElementById('t'), { box: 'device-pixel-content-box' })"), "unsupported:ResizeObserver device-pixel-content-box (the device pixel ratio is not modelled)");
    assert_eq!(js(&mut b, "ResizeObserver.prototype.observe.call({}, document.body)"), "throw:TypeError:Illegal invocation");
}

#[test]
fn the_first_frame_reports_every_box_then_only_changes() {
    // content-box sizing: the border box is 100 + 2*7 + 2*2 = 118 wide and 40 + 2*5 + 2*2 = 54 high.
    let mut b = page("#t { width: 100px; height: 40px; padding: 5px 7px; border: 2px solid black }", "<div id=t></div><div id=h style='display: none'></div>");
    js(&mut b, &format!("{LOG} var o = new ResizeObserver(show); o.observe(document.getElementById('t')); o.observe(document.getElementById('h'))"));
    // nothing is delivered before the rendering step
    assert_eq!(js(&mut b, "log.length"), "0");
    // the first size is always reported (the last reported size starts at -1 x -1), a box-less element as 0 x 0
    assert_eq!(frame(&mut b), "t b=118x54 c=100x40 r=7,5,100,40 | h b=0x0 c=0x0 r=0,0,0,0");
    assert_eq!(frame(&mut b), "");
    // a change is reported once
    js(&mut b, "document.getElementById('t').style.width = '50px'");
    assert_eq!(frame(&mut b), "t b=68x54 c=50x40 r=7,5,50,40");
    assert_eq!(frame(&mut b), "");
    // a change that keeps the observed box is not reported: border-box observation, padding moves into the content
    js(&mut b, "o.unobserve(document.getElementById('t')); o.observe(document.getElementById('t'), { box: 'border-box' })");
    assert_eq!(frame(&mut b), "t b=68x54 c=50x40 r=7,5,50,40");
    js(&mut b, "var t = document.getElementById('t'); t.style.width = '54px'; t.style.paddingLeft = '5px'; t.style.paddingRight = '5px'");
    assert_eq!(frame(&mut b), "");
    // unobserve and disconnect stop the reports
    js(&mut b, "o.disconnect(); t.style.width = '10px'");
    assert_eq!(frame(&mut b), "");
}

#[test]
fn an_idle_page_queues_no_frames() {
    let mut b = page("#t { width: 10px; height: 10px }", "<div id=t></div>");
    js(&mut b, &format!("{LOG} new ResizeObserver(show).observe(document.getElementById('t'))"));
    assert_eq!(frame(&mut b), "t b=10x10 c=10x10 r=0,0,10,10");
    let p = b.advance_time(1000.0, 100).unwrap();
    assert_eq!((p.ran, p.next_due), (0, None));
}

#[test]
fn the_radix_use_size_shape_reads_the_border_box() {
    let mut b = page("#t { width: 30px; height: 12px; border: 1px solid black }", "<div id=t></div>");
    let src = "var size; var e = document.getElementById('t'); var ro = new ResizeObserver(function (t) { if (!Array.isArray(t) || !t.length) return; var i = t[0], r, o; \
        if ('borderBoxSize' in i) { var x = i.borderBoxSize, y = Array.isArray(x) ? x[0] : x; r = y.inlineSize; o = y.blockSize } else { r = e.offsetWidth; o = e.offsetHeight } size = r + 'x' + o }); \
        ro.observe(e, { box: 'border-box' })";
    js(&mut b, src);
    b.advance_time(20.0, 100).unwrap();
    assert_eq!(js(&mut b, "size"), "32x14");
}

#[test]
fn deeper_changes_run_in_the_same_frame_and_a_loop_is_reported() {
    let mut b = page("#a { width: 100px } #c { width: 10px; height: 10px }", "<div id=a><div id=c></div></div>");
    // the callback for `a` (depth 3) resizes `c` (depth 4): `c` is delivered in the same frame
    js(&mut b, "var log = []; var a = document.getElementById('a'), c = document.getElementById('c'); \
        new ResizeObserver(function (es) { log.push('a'); c.style.width = '20px' }).observe(a); \
        new ResizeObserver(function (es) { log.push('c' + es[0].contentRect.width) }).observe(c)");
    assert_eq!(frame(&mut b), "a | c10 | c20");
    assert!(b.take_reports().is_empty());
    // a callback that resizes its own target cannot be delivered again in this frame: the loop error, then the next frame
    js(&mut b, "var n = 0; new ResizeObserver(function (es) { log.push('self' + es[0].contentRect.width); if (n++ < 1) c.style.width = (30 + n) + 'px' }).observe(c)");
    // only the new observer is active (c is unchanged for the others)
    assert_eq!(frame(&mut b), "self20");
    let r = b.take_reports();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].message, "Error: ResizeObserver loop completed with undelivered notifications.");
    assert_eq!(frame(&mut b), "c31 | self31");
}

#[test]
fn a_throwing_callback_is_reported_and_others_still_run() {
    let mut b = page("#t { width: 10px; height: 10px }", "<div id=t></div>");
    js(&mut b, "var log = []; var t = document.getElementById('t'); new ResizeObserver(function () { throw new Error('boom') }).observe(t); \
        new ResizeObserver(function () { log.push('ran') }).observe(t)");
    assert_eq!(frame(&mut b), "ran");
    let r = b.take_reports();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].message.lines().next().unwrap(), "Error: boom");
    assert!(r[0].message.contains("\n    at "), "{r:?}");
}

#[test]
fn an_observer_lives_while_it_observes() {
    let mut b = page("#t { width: 10px; height: 10px }", "<div id=t></div>");
    // no script reference keeps the observer: its observation does
    js(&mut b, "var log = []; new ResizeObserver(function (es, o) { log.push(o instanceof ResizeObserver) }).observe(document.getElementById('t'))");
    b.collect(&[]).unwrap();
    assert_eq!(frame(&mut b), "true");
}

#[test]
fn observing_again_replaces_and_the_caps_hold() {
    let mut b = page("", "<div id=t></div><div id=u></div>");
    b.set_resize_limits(ResizeLimits { max_observers: 2, max_observations: 2 });
    js(&mut b, "var t = document.getElementById('t'), u = document.getElementById('u'); var o = new ResizeObserver(function () {})");
    // the same target again replaces its observation: it takes no new room
    assert_eq!(js(&mut b, "o.observe(t); o.observe(t, { box: 'border-box' }); o.observe(u)"), "undefined");
    assert_eq!(js(&mut b, "new ResizeObserver(function () {}).observe(t)"), format!("limit:{:?}", LimitKind::Host("resize-observations")));
    assert_eq!(js(&mut b, "new ResizeObserver(function () {})"), format!("limit:{:?}", LimitKind::Host("resize-observers")));
    // disconnect gives back the observations; an observer counts until the collector frees it
    assert_eq!(js(&mut b, "o.disconnect(); new ResizeObserver(function () {}).observe(t)"), format!("limit:{:?}", LimitKind::Host("resize-observers")));
    assert_eq!(js(&mut b, "o.observe(t); o.observe(u); 'ok'"), "ok");
}
