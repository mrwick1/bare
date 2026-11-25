//! Tests of `browser_select`: `select`, `option`, `optgroup` and the option lists. The Chrome cases first, then the lazy globals, the
//! collector, the bounds and the painter's view of the state.

use crate::eng_new::Peek;
use crate::browser::*;
use crate::browser_topic_tests::topic_test;
use bare_html::tree;
use crate::eng_new::NewFault as Error;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;

topic_test!(chrome_cases, "select", 151);

fn page(body: &str) -> Browser {
    Browser::new(tree::parse(&format!("<!doctype html><body>{body}</body>")), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u),
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
    b.collect(&[]).expect("gc");
    assert_eq!(js(&mut b, "s.options.length + ',' + s.options[4].text"), "5,o4");
}

#[test]
fn a_collected_options_list_frees_its_cache_entry_and_a_held_one_keeps_it() {
    let mut b = page("<select id=s><option>a<option>b</select>");
    js(&mut b, "var s = document.getElementById('s'), o = s.options, c = s.selectedOptions; (function () { s.options.length; s.selectedOptions.length; })();");
    b.collect(&[]).expect("gc");
    b.collect(&[]).expect("gc");
    assert_eq!(b.dom.sel_cached(), 2);
    assert_eq!(js(&mut b, "(s.options === o) + ',' + (s.selectedOptions === c) + ',' + o.length + ',' + c.length + ',' + o[1].text"), "true,true,2,1,b");
    // The script drops both: the collector frees them, and the next read makes new ones that answer for the select.
    js(&mut b, "o = null; c = null;");
    b.collect(&[]).expect("gc");
    assert_eq!(b.dom.sel_cached(), 0);
    assert_eq!(js(&mut b, "var n = s.options; (n === s.options) + ',' + n.length + ',' + n[0].text + ',' + s.selectedOptions[0].text"), "true,2,a,a");
    assert_eq!(b.dom.sel_cached(), 2);
}

#[test]
fn a_key_reused_after_a_collection_answers_for_the_new_collection_only() {
    let mut b = page("<select id=a><option>a1</select><select id=b><option>b1<option>b2</select>");
    js(&mut b, "var a = document.getElementById('a'), bb = document.getElementById('b'); (function () { a.options.length; })();");
    b.collect(&[]).expect("gc");
    assert_eq!(b.dom.sel_cached(), 0);
    let slots = b.dom.st.borrow().owners.slots.len();
    assert_eq!(js(&mut b, "var ob = bb.options; ob.length"), "2");
    assert_eq!(b.dom.st.borrow().owners.slots.len(), slots);
    assert_eq!(js(&mut b, "ob[1].text"), "b2");
    assert_eq!(js(&mut b, "var oa = a.options; (oa === ob) + ',' + oa.length + ',' + oa[0].text + ',' + ob.length"), "false,1,a1,2");
}

#[test]
fn the_select_interfaces_survive_a_collection_that_no_script_reaches() {
    let mut b = page("<select id=s><option>a</select><textarea id=t>x</textarea>");
    js(&mut b, "document.getElementById('s').options.length");
    for _ in 0..3 {
        b.collect(&[]).expect("gc");
    }
    assert_eq!(js(&mut b, "var s = document.getElementById('s'); [s instanceof HTMLSelectElement, s.options instanceof HTMLOptionsCollection, s.options[0] instanceof HTMLOptionElement, new Option('x') instanceof HTMLOptionElement, document.getElementById('t') instanceof HTMLTextAreaElement, s.selectedOptions instanceof HTMLCollection].join()"), "true,true,true,true,true,true");
}
