//! Tests of `browser_walker`: `TreeWalker`, `NodeFilter`, `document.createTreeWalker`. The Chrome cases first (46 of them), then the cost of a page that
//! does not use them, the bound and the garbage collector.

use crate::browser::*;
use crate::browser_topic_tests::topic_test;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;

topic_test!(chrome_cases, "walker", 46);

fn page(body: &str) -> Browser {
    Browser::new(tree::parse(&format!("<!doctype html><body>{body}</body>")), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(e) => format!("{e:?}"),
    }
}

#[test]
fn the_globals_are_lazy_and_work_when_read() {
    let mut b = page("<div id=r><i></i></div>");
    assert_eq!(js(&mut b, "var d = Object.getOwnPropertyDescriptor(window, 'TreeWalker'); typeof d.get"), "function");
    assert_eq!(js(&mut b, "typeof TreeWalker + typeof NodeFilter + (Object.getOwnPropertyDescriptor(window, 'TreeWalker').value === TreeWalker)"), "functionfunctiontrue");
    assert_eq!(js(&mut b, "window.NodeFilter = 3; NodeFilter"), "3");
}

#[test]
fn the_walker_bound_is_a_limit() {
    let mut b = page("");
    let r = b.run_script("for (var i = 0; i < 70000; i++) document.createTreeWalker(document.body)");
    assert!(matches!(r, Err(Error::Limit(_))), "{r:?}");
}

#[test]
fn a_filter_stays_alive_through_the_walker() {
    let mut b = page("<div id=r><i id=a></i></div>");
    js(&mut b, "var w = document.createTreeWalker(document.getElementById('r'), 1, (function () { var tag = 'a'; return function (n) { return n.id === tag ? 1 : 3 } })())");
    b.collect(&[]).unwrap();
    assert_eq!(js(&mut b, "w.nextNode().id"), "a");
}
