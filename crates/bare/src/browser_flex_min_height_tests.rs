//! Tests of a column flex container whose height comes only from `min-height`: its `flex: 1` items grow to fill it, as in Chrome.
//! T3 Code puts its sidebar groups at the bottom this way (`min-h-full`, `flex-1` and `mt-auto`).

use crate::eng_new::Peek;
use crate::browser::*;
use bare_html::tree;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;

fn heights(html: &str) -> String {
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    let v = b.run_script("['a', 'b', 'c'].map(i => document.getElementById(i)).filter(e => e).map(e => e.offsetHeight + ':' + e.offsetTop).join()").expect("script");
    b.it.display(v)
}

#[test]
fn flex_items_grow_into_the_min_height() {
    let html = "<!doctype html><body style='margin:0'><div id=a style='display:flex;flex-direction:column;min-height:200px'><div id=b style='flex:1'>x</div><div id=c style='height:20px'></div></div>";
    assert_eq!(heights(html), "200:0,180:0,20:180");
}

#[test]
fn a_percent_min_height_of_a_sized_parent_counts_too() {
    let html = "<!doctype html><body style='margin:0'><div style='height:300px;overflow:scroll;scrollbar-width:none'><div id=a style='display:flex;flex-direction:column;min-height:100%'><div id=b style='flex:1;display:flex;flex-direction:column'><div style='height:10px'></div><div id=c style='height:20px;margin-top:auto'></div></div></div></div>";
    assert_eq!(heights(html), "300:0,300:0,20:280");
}
