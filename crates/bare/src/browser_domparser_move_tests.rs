//! Moving the children of a parsed document's body into a fragment, as CKEditor's HTML data processor does (`_toDom`).
use crate::browser::*;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

fn browser() -> Browser {
    let mut b = Browser::new(tree::parse("<!doctype html><p>x</p>"), BrowserLimits::default(), EvalLimits { max_steps: 2_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src).expect(src);
    b.it.display(r)
}

#[test]
fn the_body_children_of_a_parsed_document_move_into_its_fragment() {
    let mut b = browser();
    let got = js(&mut b, "var d = new DOMParser().parseFromString('<body><p>a</p>b<i>c</i></body>', 'text/html'); \
        var f = d.createDocumentFragment(), kids = d.body.childNodes, n = 0; \
        while (kids.length > 0 && n < 100) { f.appendChild(kids[0]); n++; } \
        [n, kids.length, f.childNodes.length, f.textContent].join('|')");
    assert_eq!(got, "3|0|3|abc");
}

#[test]
fn child_nodes_and_children_are_live_and_the_same_object() {
    let mut b = browser();
    let got = js(&mut b, "var e = document.createElement('div'); e.innerHTML = '<p>a</p>b<i>c</i>'; \
        var k = e.childNodes, c = e.children; var l0 = [k.length, c.length]; e.firstChild.remove(); \
        [k === e.childNodes, c === e.children, l0.join(), k.length, c.length, k[0].nodeName, c[0].nodeName, k.item(1).nodeName, \
         Object.prototype.toString.call(k), Object.prototype.toString.call(c), k instanceof NodeList, c instanceof HTMLCollection, \
         Array.prototype.slice.call(k).length, [...k].length, typeof k.forEach].join('|')");
    assert_eq!(got, "true|true|3,2|2|1|#text|I|I|[object NodeList]|[object HTMLCollection]|true|true|2|2|function");
}
