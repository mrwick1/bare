//! Tests of `browser_geom` and `browser_matrix`: the Geometry Interfaces. The Chrome cases first, then the cost and the lazy realm.
use crate::browser::*;
use crate::browser_topic_tests::topic_test;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;

topic_test!(chrome_cases, "geometry", 48);

fn page(body: &str) -> Browser {
    Browser::new(tree::parse(&format!("<!doctype html><body>{body}</body>")), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("{e:?}"),
    }
}

const NAMES: &str = "['DOMRectReadOnly', 'DOMRect', 'DOMRectList', 'DOMPointReadOnly', 'DOMPoint', 'DOMQuad', 'DOMMatrixReadOnly', 'DOMMatrix']";

#[test]
fn the_globals_are_lazy_in_three_groups_and_work_when_read() {
    let mut b = page("<p id=p>abc</p>");
    let kinds = format!("{NAMES}.map(function (n) {{ var d = Object.getOwnPropertyDescriptor(window, n); return typeof d.get + (d.enumerable ? 'e' : '') + (d.configurable ? 'c' : '') }}).join()");
    assert_eq!(js(&mut b, &kinds), ["functionc"; 8].join(","));
    // A read of one global makes its group and replaces that global only. A script can still replace a global before it is read.
    assert_eq!(js(&mut b, "typeof DOMRect"), "function");
    let after = format!("{NAMES}.map(function (n) {{ return typeof Object.getOwnPropertyDescriptor(window, n).get }}).join()");
    assert_eq!(js(&mut b, &after), "function,undefined,function,function,function,function,function,function");
    assert_eq!(js(&mut b, "window.DOMMatrix = 3; DOMMatrix"), "3");
    assert_eq!(js(&mut b, "typeof DOMMatrixReadOnly"), "function");
    assert_eq!(js(&mut b, "var r = document.getElementById('p').getBoundingClientRect(); [r instanceof DOMRect, r instanceof DOMRectReadOnly, document.getElementById('p').getClientRects() instanceof DOMRectList, Array.isArray(document.body.getClientRects())].join()"), "true,true,true,false");
}

#[test]
fn a_layout_read_makes_the_objects_but_resolves_no_global() {
    let mut b = page("<p id=p>abc</p>");
    js(&mut b, "document.getElementById('p').getBoundingClientRect()");
    assert_eq!(js(&mut b, &format!("{NAMES}.map(function (n) {{ return typeof Object.getOwnPropertyDescriptor(window, n).get }}).join()")), "function,function,function,function,function,function,function,function");
    assert_eq!(js(&mut b, "typeof DOMMatrix"), "function");
}

#[test]
fn many_rectangles_points_and_matrices_are_collected_and_a_quad_keeps_its_points() {
    let mut b = page("<p id=p>abc</p>");
    js(&mut b, "var q = new DOMQuad({ x: 1, y: 2 }, { x: 3, y: 4 }, { x: 5, y: 6 }, { x: 7, y: 8 }); var p = document.getElementById('p'); var s = 0;");
    assert_eq!(js(&mut b, "for (var i = 0; i < 4000; i++) { var r = p.getBoundingClientRect(); s += r.width + new DOMPoint(i).x + new DOMMatrix([1, 0, 0, 1, i, 0]).translate(1, 2).e + p.getClientRects().length; } s > 0"), "true");
    let before = b.dom.geom.slots.borrow().len();
    b.collect(&[]).expect("collect");
    let after = b.dom.geom.slots.borrow().len();
    assert!(before > after && after < 100, "slots before {before}, after {after}");
    assert_eq!(js(&mut b, "[q.p1.x, q.p2.y, q.p3.x, q.p4.y, q.getBounds().width].join()"), "1,4,5,8,6");
}

#[test]
fn a_subclass_and_the_brand_checks_work_on_the_new_objects() {
    let mut b = page("");
    assert_eq!(js(&mut b, "class R extends DOMRect { get area() { return this.width * this.height } }; var r = new R(0, 0, 3, 4); [r.area, r instanceof DOMRect, Object.prototype.toString.call(r), JSON.stringify(r)].join()"), "12,true,[object DOMRect],{\"x\":0,\"y\":0,\"width\":3,\"height\":4,\"top\":0,\"right\":3,\"bottom\":4,\"left\":0}");
    assert_eq!(js(&mut b, "try { DOMMatrix.prototype.translate.call({}) } catch (e) { e.name + ':' + e.message }"), "TypeError:Illegal invocation");
    assert_eq!(js(&mut b, "try { structuredClone(new DOMRect()) } catch (e) { e.name }"), "DataCloneError");
    assert_eq!(js(&mut b, "new DOMMatrix('rotate(90deg)').transformPoint({ x: 1 }).y"), "1");
}
