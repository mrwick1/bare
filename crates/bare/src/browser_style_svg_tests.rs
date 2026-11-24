//! Tests of the SVG stroke dash properties on `style` (`stroke-dasharray`, `stroke-dashoffset`): React sets them with
//! `el.style.strokeDasharray = ...`, and the painter reads them from the `style` attribute (`bare_paint::svg`).

use crate::browser::*;
use bare_html::tree;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;

fn js(b: &mut Browser, src: &str) -> String {
    let v = b.run_script(src).expect("script");
    b.it.display(v)
}

#[test]
fn stroke_dash_properties_write_the_style_attribute() {
    let mut b = Browser::new(tree::parse("<!doctype html><svg><circle id=c r=40></circle></svg>"), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    assert_eq!(js(&mut b, "var c = document.getElementById('c'), s = c.style; ['strokeDasharray' in s, 'stroke-dashoffset' in s].join()"), "true,true");
    assert_eq!(js(&mut b, "s.strokeDasharray = '251.3'; s.strokeDashoffset = '188.5'; c.getAttribute('style')"), "stroke-dasharray: 251.3; stroke-dashoffset: 188.5;");
    assert_eq!(js(&mut b, "[s.strokeDasharray, s.getPropertyValue('stroke-dashoffset'), s.length].join()"), "251.3,188.5,2");
    assert_eq!(js(&mut b, "s.strokeDasharray = '10px, 5'; s['stroke-dasharray']"), "10px, 5");
    // A value that is not a dash list is ignored, as for any property.
    assert_eq!(js(&mut b, "s.strokeDasharray = '-1'; s.strokeDashoffset = 'x'; c.getAttribute('style')"), "stroke-dasharray: 10px, 5; stroke-dashoffset: 188.5;");
    assert_eq!(js(&mut b, "s.strokeDasharray = 'none'; s.removeProperty('stroke-dashoffset'); c.getAttribute('style')"), "stroke-dasharray: none;");
    assert_eq!(js(&mut b, "c.setAttribute('style', 'stroke-dashoffset: 3'); s.strokeDashoffset"), "3");
    // Not built: the cascade does not compute them, so the computed value is empty (Chrome gives `3px`).
    assert_eq!(js(&mut b, "JSON.stringify(getComputedStyle(c).strokeDashoffset)"), "\"\"");
}
