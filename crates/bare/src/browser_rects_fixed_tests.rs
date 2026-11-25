//! The box of a `position: fixed` element with percentage sizes. Its containing block is the viewport (CSS 2.2 §10.1), so
//! `width: 100%` and `height: 100%` resolve against the viewport (§10.2, §10.5), also inside a parent of height 0 or an inline.

use crate::browser::*;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

fn run(html: &str, src: &str) -> String {
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).expect("browser");
    b.set_viewport(1280.0, 800.0).expect("viewport");
    let v = b.run_script(src).unwrap_or_else(|e| panic!("{src}: {e:?}"));
    b.it.display(v)
}

const VIEWER: &str = "<div id=f style='position:fixed;top:0;left:0;width:100%;height:100%;display:flex;flex-direction:column;overflow:hidden;box-sizing:border-box;z-index:500'>\
<div id=a style='display:flex;height:60px;flex:0 0 auto'>a</div><div id=b style='display:flex;height:calc(100% - 60px);flex:1 0 auto'>b</div></div>";

const READ: &str = "['f','a','b'].map(function(id){var r=document.getElementById(id).getBoundingClientRect();return r.x+','+r.y+','+r.width+'x'+r.height}).join(' ')";

const WANT: &str = "0,0,1280x800 0,0,1280x60 0,60,1280x740";

fn page(inner: &str) -> String {
    format!("<!doctype html><body style='margin:0'>{inner}")
}

/// Hypothesis (c): a fixed box inside a static parent of height 0.
#[test]
fn a_fixed_box_in_a_parent_of_height_0_fills_the_viewport() {
    assert_eq!(run(&page(&format!("<div><div>{VIEWER}</div></div>")), READ), WANT);
}

/// Hypothesis (b): a fixed box inside an inline span that holds blocks (block-in-inline, §9.2.1.1).
#[test]
fn a_fixed_box_in_a_block_in_inline_span_fills_the_viewport() {
    assert_eq!(run(&page(&format!("<div style='position:relative'><span><div style='position:absolute;width:100%;height:100%'><div><div>{VIEWER}</div></div></div></span></div>")), READ), WANT);
}

/// The ancestor chain of the live page, from `#app` down.
#[test]
fn a_fixed_box_in_the_live_ancestor_chain_fills_the_viewport() {
    let chain = format!(
        "<div><div><div id=app style='position:absolute;top:0;left:0;width:100%;height:100%'><div style='display:flex;height:100%'>\
<div style='position:relative;width:100%;height:100%'><span><div style='position:absolute;top:0;left:0;width:100%;height:100%'><div><div>{VIEWER}</div></div></div></span></div></div></div></div></div>"
    );
    assert_eq!(run(&page(&chain), READ), WANT);
}

/// The cause: the window lays out its absolute boxes after its walk for the `fixed` boxes, so a fixed box inside one was lost.
#[test]
fn a_fixed_box_in_an_absolute_box_of_the_window_fills_the_viewport() {
    assert_eq!(run(&page(&format!("<div style='position:absolute;width:100%;height:100%'>{VIEWER}</div>")), READ), WANT);
}

/// A fixed box two absolute boxes down: the inner one is laid out with the outer one, at the end of the list.
#[test]
fn a_fixed_box_in_nested_absolute_boxes_fills_the_viewport() {
    let inner = format!("<div style='position:absolute;width:100%;height:100%'><div style='position:absolute;width:50%;height:50%'>{VIEWER}</div></div>");
    assert_eq!(run(&page(&inner), READ), WANT);
}

/// A fixed box inside a fixed box: the inner one is found when the outer one is laid out.
#[test]
fn a_fixed_box_in_a_fixed_box_fills_the_viewport() {
    let outer = format!("<div style='position:fixed;top:10px;left:10px;width:50px;height:50px'>{VIEWER}</div>");
    assert_eq!(run(&page(&outer), READ), WANT);
}

/// The paint has the fixed box too, not only the rectangles: a red fixed box inside an absolute box covers the window.
#[test]
fn a_fixed_box_in_an_absolute_box_is_painted() {
    use crate::page::{Page, Source};
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            let html = "<!doctype html><body style='margin:0'><div style='position:absolute;width:100%;height:100%'>\
<div style='position:fixed;top:0;left:0;width:100%;height:100%;background:#f00'></div></div>";
            let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).expect("browser");
            b.set_viewport(200.0, 150.0).expect("viewport");
            let mut p = Page::new(Source::Browser(Box::new(b.into())), (200, 150));
            let px = p.paint();
            assert_eq!((px[10 * 200 + 10], px[140 * 200 + 190]), (0xFFFF_0000, 0xFFFF_0000));
        })
        .unwrap()
        .join()
        .unwrap();
}
