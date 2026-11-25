//! The rectangles of a culled inline box (`browser_rects.rs`, `Pass::culled`) with a `::before` node, after a change of the tree.

use crate::browser::*;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

fn run(html: &str, src: &str) -> String {
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).expect("browser");
    b.set_viewport(300.0, 200.0).expect("viewport");
    let v = b.run_script(src).unwrap_or_else(|e| panic!("{src}: {e:?}"));
    b.it.display(v)
}

/// The first read makes the `::before` node. A new element moves the styles of the read past that node (`RuleSet::restyle`): the node
/// keeps its style, so the next read of the rectangles has the box of the `::before` and does not panic.
#[test]
fn a_pseudo_node_of_a_culled_inline_keeps_its_style_after_a_change() {
    let html = "<!doctype html><style>#s::before{content:'xx'}</style><body style='margin:0;font:16px monospace'><span id=s>ab</span>";
    let src = "var s = document.getElementById('s'); var a = s.getBoundingClientRect().width; document.body.append(document.createElement('div')); var b = s.getBoundingClientRect().width; a + ' ' + b";
    let out = run(html, src);
    let (a, b) = out.split_once(' ').unwrap();
    assert_eq!(a, b, "{out}");
    assert!(a.parse::<f64>().unwrap() > 30.0, "{out}");
}
