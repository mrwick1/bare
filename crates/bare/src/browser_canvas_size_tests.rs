//! Tests of the size of a `<canvas>` (HTML Standard, "The canvas element"): `width` and `height` reflect their attributes (300 and 150
//! by default), and the box takes that size when CSS gives none. An author width keeps the ratio of the canvas.

use crate::browser::*;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

fn js(page: &str, src: &str) -> String {
    let mut b = Browser::new(tree::parse(page), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    let r = b.run_script(src).expect("script");
    b.it.display(r)
}

const SIZE: &str = "var f = id => { var r = document.getElementById(id).getBoundingClientRect(); return r.width + 'x' + r.height };";

#[test]
fn width_and_height_reflect_the_attributes() {
    let page = "<canvas id=a></canvas><canvas id=b width=50 height=x></canvas>";
    let src = "var a = document.getElementById('a'), b = document.getElementById('b'); var r = [a.width, a.height, b.width, b.height];
        a.width = 228; r.push(a.getAttribute('width'), a.width); r.join()";
    assert_eq!(js(page, src), "300,150,50,150,228,228");
}

#[test]
fn the_box_takes_the_size_of_the_canvas() {
    let page = "<body style='margin:0'><canvas id=a></canvas><canvas id=b width=50 height=40></canvas><canvas id=c width=50 height=40 style='width:100px'></canvas><div style='display:flex'><canvas id=d width=20 height=10></canvas></div>";
    assert_eq!(js(page, &format!("{SIZE} [f('a'), f('b'), f('c'), f('d')].join()")), "300x150,50x40,100x80,20x10");
}

#[test]
fn a_script_that_sets_the_width_resizes_the_box() {
    let page = "<body style='margin:0'><div id=p style='display:flex'></div>";
    let src = format!("{SIZE} var c = document.createElement('canvas'); c.id = 'c'; c.width = 228; c.height = 228; document.getElementById('p').appendChild(c); f('c')");
    assert_eq!(js(page, &src), "228x228");
}
