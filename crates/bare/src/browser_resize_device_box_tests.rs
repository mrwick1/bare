//! Tests of `box: 'device-pixel-content-box'` and `devicePixelContentBoxSize` in `browser_resize`.
//! Resize Observer, "calculate box size": for `device-pixel-content-box`, "set computedSize.inlineSize to target's content area
//! inline length, in integral device pixels". bare's `devicePixelRatio` is 1. The size snaps the content box edges to whole pixels,
//! as Chrome does: `round(right) - round(left)`. The body margin puts the content box at x = 8.

use crate::browser::*;
use crate::eng_new::Peek;
use crate::eng_new::NewFault as Error;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

fn page(css: &str, body: &str) -> Browser {
    let html = format!("<!doctype html><html><head><style>{css}</style></head><body>{body}</body></html>");
    let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if crate::eng_new::is_object(v) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u),
        Err(e) => format!("other:{e:?}"),
    }
}

fn frame(b: &mut Browser) -> String {
    b.advance_time(20.0, 100).unwrap();
    js(b, "var s = log.join(' | '); log.length = 0; s")
}

const LOG: &str = "var log = []; function show(es) { for (var e of es) { var c = e.contentBoxSize[0], d = e.devicePixelContentBoxSize[0]; \
    log.push(e.target.id + ' c=' + c.inlineSize + 'x' + c.blockSize + ' d=' + d.inlineSize + 'x' + d.blockSize) } }";

#[test]
fn device_pixel_content_box_reports_and_tracks_changes() {
    let mut b = page("#t { width: 100px; height: 40px; padding: 5px; border: 2px solid black }", "<div id=t></div>");
    js(&mut b, &format!("{LOG} var t = document.getElementById('t'); var o = new ResizeObserver(show); o.observe(t, {{ box: 'device-pixel-content-box' }})"));
    assert_eq!(frame(&mut b), "t c=100x40 d=100x40");
    assert_eq!(frame(&mut b), "");
    js(&mut b, "t.style.width = '60px'");
    assert_eq!(frame(&mut b), "t c=60x40 d=60x40");
    // content-box observation of the same target keeps working, and its entry has the device size too
    js(&mut b, "o.observe(t, { box: 'content-box' })");
    assert_eq!(frame(&mut b), "t c=60x40 d=60x40");
    js(&mut b, "t.style.height = '30px'");
    assert_eq!(frame(&mut b), "t c=60x30 d=60x30");
}

#[test]
fn device_pixel_content_box_snaps_fractional_edges() {
    // a: left 8, right 108.4 -> 100. b: left 8.3 -> 8, right 108.7 -> 109 -> 101 (rounding the length alone gives 100).
    let mut b = page("div { height: 10.6px } #a { width: 100.4px } #b { width: 100.4px; margin-left: 0.3px }", "<div id=a></div><div id=b></div>");
    js(&mut b, &format!("{LOG} var o = new ResizeObserver(show); for (var id of ['a', 'b']) o.observe(document.getElementById(id), {{ box: 'device-pixel-content-box' }})"));
    let out = frame(&mut b);
    assert!(out.contains("a c=100.4") && out.contains("d=100x"), "{out}");
    assert!(out.contains("b c=100.4") && out.contains("d=101x"), "{out}");
    // a size change below a device pixel changes no device size: no report
    js(&mut b, "document.getElementById('a').style.width = '100.3px'");
    assert_eq!(frame(&mut b), "");
}
