//! Tests of `browser_select`: `select`, `option`, `optgroup` and the option lists. The Chrome cases first, then the lazy globals, the
//! collector, the bounds and the painter's view of the state.

use crate::browser::*;
use crate::browser_topic_tests::topic_test;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;

topic_test!(chrome_cases, "select", 151);

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

#[test]
fn the_globals_are_lazy_and_work_when_read() {
    let mut b = page("<p>x</p>");
    assert_eq!(js(&mut b, "['HTMLSelectElement', 'HTMLOptionElement', 'HTMLOptGroupElement', 'HTMLOptionsCollection', 'HTMLCollection', 'Option'].map(function (n) { return typeof Object.getOwnPropertyDescriptor(window, n).get }).join()"), "function,function,function,function,function,function");
    assert_eq!(js(&mut b, "[typeof HTMLSelectElement, typeof Option, Option.prototype === HTMLOptionElement.prototype, Object.getPrototypeOf(HTMLOptionsCollection.prototype) === HTMLCollection.prototype].join()"), "function,function,true,true");
    assert_eq!(js(&mut b, "window.HTMLCollection = 3; HTMLCollection"), "3");
}

#[test]
fn the_radix_native_setter_snippet_works_for_all_three_controls() {
    let mut b = page("<select id=s><option value=a>A<option value=b>B</select><input id=i><input id=c type=checkbox><textarea id=t>x</textarea>");
    let snip = "var el = document.getElementById('s'), v = 'b', n = 0; el.addEventListener('change', function () { n++ }); let t = Object.getOwnPropertyDescriptor(window.HTMLSelectElement.prototype, \"value\").set; if (t) { t.call(el, v); el.dispatchEvent(new Event(\"change\", {bubbles: true})); } [typeof t, el.value, el.selectedIndex, n].join()";
    assert_eq!(js(&mut b, snip), "function,b,1,1");
    let rest = "var d = function (C, k) { return Object.getOwnPropertyDescriptor(window[C].prototype, k) }; var i = document.getElementById('i'), c = document.getElementById('c'), ta = document.getElementById('t'); d('HTMLInputElement', 'value').set.call(i, 'v'); d('HTMLInputElement', 'checked').set.call(c, true); var r = [d('HTMLTextAreaElement', 'value').get.call(ta)]; d('HTMLTextAreaElement', 'value').set.call(ta, 'w'); r.push(i.value, c.checked, ta.value); r.join()";
    assert_eq!(js(&mut b, rest), "x,v,true,w");
    assert_eq!(js(&mut b, "try { Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').get.call(document.body) } catch (e) { e.name }"), "TypeError");
}

#[test]
fn a_page_without_selects_makes_no_interface() {
    let mut b = page("<p>x</p><input>");
    js(&mut b, "document.createElement('div'); document.createElement('input')");
    assert!(!b.dom.sel_made());
    js(&mut b, "document.createElement('select')");
    assert!(b.dom.sel_made());
}

#[test]
fn the_painter_reads_the_selected_option() {
    let mut b = page("<select id=s><option>a</option><option>b</option></select>");
    let node = |b: &mut Browser, src: &str| {
        let v = b.run_script(src).expect("script");
        b.dom.node_of(v)
    };
    let s = node(&mut b, "document.getElementById('s')").expect("select");
    let (a, c) = (node(&mut b, "s = document.getElementById('s'); s.options[0]"), node(&mut b, "s.options[1]"));
    assert_eq!(b.selected_option(s), a);
    js(&mut b, "s.selectedIndex = 1");
    assert_eq!(b.selected_option(s), c);
    js(&mut b, "s.value = 'none'");
    assert_eq!(b.selected_option(s), None);
}

#[test]
fn bounds_and_the_collector_keep_the_lists_right() {
    let mut b = page("<select id=s></select>");
    js(&mut b, "var s = document.getElementById('s'); for (var i = 0; i < 50; i++) s.add(new Option('o' + i)); s.options.length = 5");
    assert_eq!(js(&mut b, "s.options.length + ',' + s.selectedOptions.length + ',' + (s.options === s.options)"), "5,1,true");
    b.it.collect(&[]).expect("gc");
    assert_eq!(js(&mut b, "s.options.length + ',' + s.options[4].text"), "5,o4");
    let mut small = Browser::new(tree::parse("<body><select id=s></select>"), BrowserLimits { max_results: 4, ..Default::default() }, EvalLimits::default(), HeapLimits::default()).expect("browser");
    assert!(js(&mut small, "var s = document.getElementById('s'); s.options.length = 9; s.options.length").contains("dom-results"));
}
