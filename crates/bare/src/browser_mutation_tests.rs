//! Tests of `browser_mutation`: `MutationObserver`. The Chrome cases first (52 of them), then what Chrome cannot show: the cost of a page with no
//! observer, the bounds, the garbage collector and the watcher of the document.

use crate::browser::*;
use crate::browser_topic_tests::topic_test;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

topic_test!(chrome_cases, "mutation", 52);

fn page(body: &str) -> Browser {
    let mut b = Browser::new(tree::parse(&format!("<!doctype html><body>{body}</body>")), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Limit(k)) => format!("limit:{k:?}"),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn a_page_with_no_observer_has_no_watcher_and_no_microtask() {
    let mut b = page("<div id=a></div>");
    js(&mut b, "var a = document.getElementById('a'); a.appendChild(document.createElement('i')); a.setAttribute('x', '1'); a.textContent = 'q'");
    assert_eq!(b.it.pending_jobs(), 0);
    // The globals are accessors until the first read (the lazy install).
    assert_eq!(js(&mut b, "typeof Object.getOwnPropertyDescriptor(window, 'MutationObserver').get"), "function");
    assert_eq!(js(&mut b, "typeof MutationObserver"), "function");
    assert_eq!(js(&mut b, "typeof Object.getOwnPropertyDescriptor(window, 'MutationObserver').value"), "function");
    assert_eq!(js(&mut b, "window.MutationObserver = 5; MutationObserver"), "5");
}

#[test]
fn the_watcher_goes_when_the_last_registration_goes() {
    let mut b = page("<div id=a></div>");
    js(&mut b, "var a = document.getElementById('a'); var m = new MutationObserver(function () {}); m.observe(a, { attributes: true }); a.setAttribute('x', '1')");
    assert_eq!(b.it.pending_jobs(), 1);
    b.drain().unwrap();
    js(&mut b, "m.disconnect(); a.setAttribute('x', '2')");
    assert_eq!(b.it.pending_jobs(), 0);
    // It works again after that.
    assert_eq!(js(&mut b, "var n = 0; m = new MutationObserver(function (r) { n += r.length }); m.observe(a, { attributes: true }); a.setAttribute('x', '3'); 1"), "1");
    b.drain().unwrap();
    assert_eq!(js(&mut b, "n"), "1");
}

#[test]
fn one_microtask_for_many_changes() {
    let mut b = page("<div id=a></div>");
    js(&mut b, "var a = document.getElementById('a'); var calls = 0, seen = 0; var m = new MutationObserver(function (r) { calls++; seen += r.length }); m.observe(a, { attributes: true }); for (var i = 0; i < 100; i++) a.setAttribute('x', i)");
    assert_eq!(b.it.pending_jobs(), 1);
    b.drain().unwrap();
    assert_eq!(js(&mut b, "calls + ' ' + seen"), "1 100");
}

#[test]
fn the_record_count_is_bounded() {
    let mut b = page("<div id=a></div>");
    let r = js(&mut b, "var a = document.getElementById('a'); var m = new MutationObserver(function () {}); m.observe(a, { attributes: true }); for (var i = 0; i < 70000; i++) a.setAttribute('x', i); 1");
    assert_eq!(r, "limit:Host(\"mutation-records\")");
}

#[test]
fn an_observer_with_a_registration_survives_a_collection() {
    let mut b = page("<div id=a></div>");
    js(&mut b, "var a = document.getElementById('a'); var n = 0; new MutationObserver(function (r) { n += r.length }).observe(a, { attributes: true })");
    b.collect(&[]).unwrap();
    js(&mut b, "a.setAttribute('x', '1')");
    b.drain().unwrap();
    assert_eq!(js(&mut b, "n"), "1");
    // After a disconnect nothing keeps it.
    js(&mut b, "var m = new MutationObserver(function () {}); m.observe(a, { attributes: true }); m.disconnect(); m = null");
    b.collect(&[]).unwrap();
    assert_eq!(b.stats().wrappers > 0, true);
}
