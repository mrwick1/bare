//! Tests of `browser_walker`: `TreeWalker`, `NodeFilter`, `document.createTreeWalker`. The Chrome cases first (46 of them), then the cost of a page that
//! does not use them, the bound and the garbage collector.

use crate::browser::*;
use crate::browser_topic_tests::topic_test;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;

topic_test!(chrome_cases, "walker", 46);
topic_test!(iterator_cases, "iterator", 54);

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

#[test]
fn the_iterator_global_is_lazy_and_the_watcher_follows_the_iterators() {
    let mut b = page("<div id=r><i id=a></i></div>");
    assert_eq!(js(&mut b, "typeof Object.getOwnPropertyDescriptor(window, 'NodeIterator').get"), "function");
    assert!(!b.dom.doc.borrow().has_watcher());
    js(&mut b, "document.createTreeWalker(document.body)");
    assert!(!b.dom.doc.borrow().has_watcher(), "a TreeWalker needs no watcher");
    js(&mut b, "var w = document.createNodeIterator(document.body, 1, function (n) { return n.id === 'a' ? 1 : 3 });");
    assert!(b.dom.doc.borrow().has_watcher());
    b.collect(&[]).unwrap();
    assert_eq!(js(&mut b, "w.nextNode().id"), "a");
    js(&mut b, "w = null;");
    b.collect(&[]).unwrap();
    assert!(!b.dom.doc.borrow().has_watcher());
}

#[test]
fn a_walker_and_an_iterator_do_not_take_each_others_methods() {
    let mut b = page("<div id=r></div>");
    assert_eq!(js(&mut b, "var w = document.createTreeWalker(document.body), n = document.createNodeIterator(document.body); [(function () { try { TreeWalker.prototype.nextNode.call(n) } catch (e) { return e.message } })(), (function () { try { NodeIterator.prototype.nextNode.call(w) } catch (e) { return e.message } })()].join()"), "Illegal invocation,Illegal invocation");
}

#[test]
fn the_iterator_bound_is_a_limit() {
    let mut b = page("");
    let r = b.run_script("for (var i = 0; i < 70000; i++) document.createNodeIterator(document.body)");
    assert!(matches!(r, Err(Error::Limit(_))), "{r:?}");
}

#[test]
fn a_filter_that_calls_its_own_iterator_throws() {
    // Known deviation: the standard and bare throw InvalidStateError; Chrome does not check, and a filter that always recurses never ends there.
    let mut b = page("<div id=r></div>");
    assert_eq!(js(&mut b, "var n = document.createNodeIterator(document.body, 1, function () { try { n.nextNode() } catch (e) { return e.name === 'InvalidStateError' ? 1 : 2 } return 1 }); n.nextNode() !== null"), "true");
    assert_eq!(js(&mut b, "var m = document.createNodeIterator(document.body, 1, function () { m.nextNode() }); try { m.nextNode() } catch (e) { e.name }"), "InvalidStateError");
}

#[test]
fn an_iterator_whose_reference_node_left_the_tree_goes_back_to_null() {
    // Chrome keeps the reference node when no node follows the removed one in the root, so the iterator is on a detached node.
    let mut b = page("<div id=r><p id=a>A</p><p id=b>B</p></div>");
    js(&mut b, "var n = document.createNodeIterator(document.getElementById('r'), 1); n.nextNode(); n.nextNode(); n.nextNode(); n.previousNode(); document.getElementById('b').remove();");
    assert_eq!(js(&mut b, "n.referenceNode.id + String(n.pointerBeforeReferenceNode) + String(n.previousNode())"), "btruenull");
}
