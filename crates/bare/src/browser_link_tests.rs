//! Tests of the `HTMLLinkElement` brand and the `rel` reflection. Expectations come from the HTML Standard (the `link` element,
//! `rel` as a reflected DOMString), the WebIDL rules for ToString and brand checks, and the DOM Standard (clone, namespaces),
//! written by hand. Node has no DOM, so it is not an oracle. Nothing here starts a stylesheet load: `rel` is only a value.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

const SVG: &str = "http://www.w3.org/2000/svg";
const MATH: &str = "http://www.w3.org/1998/Math/MathML";
const XHTML: &str = "http://www.w3.org/1999/xhtml";

const PAGE: &str = "<!doctype html><html><head><link id=s rel='Preload  Stylesheet'><link id=a></head><body><div id=d rel=x></div><a id=k rel=y>x</a><svg><link id=sv rel=z></svg></body></html>";

fn browser_with(page: &str, limits: BrowserLimits) -> Browser {
    Browser::new(tree::parse(page), limits, EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn browser(page: &str) -> Browser {
    browser_with(page, BrowserLimits::default())
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Throw(v)) => format!("throw-value:{}", b.it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(e) => format!("other:{e:?}"),
    }
}

fn lim(name: &'static str) -> String {
    format!("limit:{:?}", LimitKind::Host(name))
}

#[test]
fn interface_and_prototype_shape() {
    let mut b = browser(PAGE);
    assert_eq!(js(&mut b, "typeof HTMLLinkElement + ',' + HTMLLinkElement.name + ',' + HTMLLinkElement.length + ',' + (window.HTMLLinkElement === HTMLLinkElement)"), "function,HTMLLinkElement,0,true");
    assert_eq!(js(&mut b, "Object.getPrototypeOf(HTMLLinkElement.prototype) === HTMLElement.prototype && HTMLLinkElement.prototype.constructor === HTMLLinkElement"), "true");
    assert_eq!(js(&mut b, "Object.getPrototypeOf(document.getElementById('a')) === HTMLLinkElement.prototype"), "true");
    js(&mut b, "document.getElementById('a').sheet"); // The member is defined on its first read.
    assert_eq!(js(&mut b, "Object.getOwnPropertyNames(HTMLLinkElement.prototype).sort().join()"), "as,constructor,crossOrigin,href,rel,relList,sheet");
    // like every host interface object here: not a constructor, and no instance can be made
    assert!(js(&mut b, "new HTMLLinkElement()").starts_with("throw:TypeError"));
    assert!(js(&mut b, "HTMLLinkElement()").starts_with("throw:TypeError"));
}

#[test]
fn rel_descriptor_is_a_plain_accessor() {
    let mut b = browser(PAGE);
    assert_eq!(js(&mut b, "var d = Object.getOwnPropertyDescriptor(HTMLLinkElement.prototype, 'rel'); [typeof d.get, typeof d.set, d.enumerable, d.configurable, 'value' in d, d.get.name, d.set.name, d.get.length, d.set.length].join()"), "function,function,true,true,false,get rel,set rel,0,1");
    assert_eq!(js(&mut b, "('rel' in HTMLElement.prototype) + ',' + ('rel' in HTMLInputElement.prototype) + ',' + ('rel' in Element.prototype)"), "false,false,false");
}

#[test]
fn link_nodes_have_the_brand_and_other_nodes_do_not() {
    let mut b = browser(PAGE);
    js(&mut b, &format!("var a = document.getElementById('a'), k = document.getElementById('k'), d = document.getElementById('d'), sv = document.getElementById('sv'), sl = document.createElementNS('{SVG}', 'link'), ml = document.createElementNS('{MATH}', 'link');"));
    let brands = "function (e) { return [e instanceof HTMLLinkElement, e instanceof HTMLElement, e instanceof SVGElement, e instanceof Element].join('') }";
    // [HTMLLinkElement, HTMLElement, SVGElement, Element]: parsed, created (any case), and namespace-HTML created nodes
    assert_eq!(js(&mut b, &format!("[a, document.createElement('link'), document.createElement('LINK'), document.createElementNS('{XHTML}', 'link')].map({brands}).join()")), "truetruefalsetrue,truetruefalsetrue,truetruefalsetrue,truetruefalsetrue");
    assert_eq!(js(&mut b, &format!("[k, d, sv, sl, ml].map({brands}).join()")), "falsetruefalsetrue,falsetruefalsetrue,falsefalsetruetrue,falsefalsetruetrue,falsefalsefalsetrue");
    assert_eq!(js(&mut b, "[document, document.createTextNode('x'), window].map(function (e) { return e instanceof HTMLLinkElement }).join()"), "false,false,false");
}

#[test]
fn rel_reads_a_string_for_absent_present_and_removed() {
    let mut b = browser(PAGE);
    // React's hydration cleanup reads `n.rel.toLowerCase()` on a link: an absent `rel` must still be a string
    assert_eq!(js(&mut b, "var n = document.createElement('link'); typeof n.rel.toLowerCase() + n.rel.toLowerCase().length"), "string0");
    assert_eq!(js(&mut b, "typeof document.getElementById('a').rel + ',' + document.getElementById('a').rel.length"), "string,0");
    assert_eq!(js(&mut b, "document.getElementById('a').hasAttribute('rel')"), "false");
    // present: the value as written (case and spaces kept)
    assert_eq!(js(&mut b, "document.getElementById('s').rel"), "Preload  Stylesheet");
    assert_eq!(js(&mut b, "document.getElementById('s').rel.toLowerCase()"), "preload  stylesheet");
    // removed: back to the empty string
    assert_eq!(js(&mut b, "var s = document.getElementById('s'); s.removeAttribute('rel'); s.rel.length + ',' + s.hasAttribute('rel')"), "0,false");
    // an empty string is a present attribute
    assert_eq!(js(&mut b, "var a = document.getElementById('a'); a.rel = ''; a.hasAttribute('rel') + ',' + a.rel.length"), "true,0");
}

#[test]
fn rel_setter_uses_tostring_and_writes_the_native_attribute() {
    let mut b = browser(PAGE);
    js(&mut b, "var a = document.getElementById('a');");
    assert_eq!(js(&mut b, "a.rel = 'Icon'; a.getAttribute('rel')"), "Icon");
    assert_eq!(js(&mut b, "a.setAttribute('rel', 'Viaattr'); a.rel"), "Viaattr");
    assert_eq!(js(&mut b, "a.setAttributeNS(null, 'rel', 'Vians'); a.rel"), "Vians");
    assert_eq!(js(&mut b, "a.rel = 7; a.rel + ',' + typeof a.rel"), "7,string");
    assert_eq!(js(&mut b, "a.rel = null; a.getAttribute('rel')"), "null");
    assert_eq!(js(&mut b, "a.rel = undefined; a.rel"), "undefined");
    // ordinary ToString: toString first, not valueOf
    assert_eq!(js(&mut b, "a.rel = { toString: function () { return 'Preload' }, valueOf: function () { return 'V' } }; a.getAttribute('rel')"), "Preload");
    assert_eq!(js(&mut b, "a.rel = { valueOf: function () { return 'V' } }; a.getAttribute('rel')"), "[object Object]");
}

#[test]
fn rel_setter_tostring_throws_and_reentrancy_are_safe() {
    let mut b = browser(PAGE);
    js(&mut b, "var a = document.getElementById('a'); a.rel = 'Keep';");
    // a throw in ToString ends the call and writes nothing
    assert_eq!(js(&mut b, "a.rel = { toString: function () { throw new RangeError('boom') } }"), "throw:RangeError:boom");
    assert_eq!(js(&mut b, "a.getAttribute('rel')"), "Keep");
    assert!(js(&mut b, "a.rel = Object.create(null)").starts_with("throw:TypeError"));
    assert_eq!(js(&mut b, "a.getAttribute('rel')"), "Keep");
    // ToString may write the attribute: the setter's own write comes last
    assert_eq!(js(&mut b, "a.rel = { toString: function () { a.setAttribute('rel', 'inner'); return 'outer' } }; a.getAttribute('rel')"), "outer");
    // ToString may read the getter: no document borrow is held while JS runs
    assert_eq!(js(&mut b, "a.rel = { toString: function () { return 'x' + a.rel } }; a.rel"), "xouter");
    // ToString may detach the node: the write still lands on it
    assert_eq!(js(&mut b, "a.rel = { toString: function () { a.remove(); return 'gone' } }; a.getAttribute('rel') + ',' + a.isConnected"), "gone,false");
    assert_eq!(js(&mut b, "document.body.appendChild(a); a.rel = { toString: function () { return 'ok' + (document.querySelector('#a') === a) + (document.querySelectorAll('link').length > 0) } }; a.rel"), "oktruetrue");
}

#[test]
fn rel_clone_move_and_gc_keep_the_native_value() {
    let mut b = browser(PAGE);
    assert_eq!(js(&mut b, "var s = document.getElementById('s'), c = s.cloneNode(true); [c instanceof HTMLLinkElement, c.rel, c === s].join()"), "true,Preload  Stylesheet,false");
    assert_eq!(js(&mut b, "c.rel = 'Copy'; s.rel + '|' + c.rel"), "Preload  Stylesheet|Copy");
    assert_eq!(js(&mut b, "document.body.appendChild(s); s.rel = 'Moved'; [document.getElementById('s') === s, s.parentNode === document.body, s.rel].join()"), "true,true,Moved");
    // a detached wrapper kept by a global survives a collection, with its brand, prototype and value
    assert_eq!(js(&mut b, "var k = document.createElement('link'); k.rel = 'Kept'; window.keep = k; 'ok'"), "ok");
    b.collect(&[]).expect("collect");
    assert_eq!(js(&mut b, "[keep.rel, keep instanceof HTMLLinkElement, Object.getPrototypeOf(keep) === HTMLLinkElement.prototype, keep === keep].join()"), "Kept,true,true,true");
    assert_eq!(js(&mut b, "keep.rel = 'Again'; 0"), "0");
    b.collect(&[]).expect("collect");
    assert_eq!(js(&mut b, "keep.rel + ',' + window.keep.getAttribute('rel')"), "Again,Again");
    // unreferenced detached links are pruned; attached ones keep their wrapper
    assert_eq!(js(&mut b, "for (var i = 0; i < 200; i++) document.createElement('link').rel = 'tmp' + i; document.getElementById('a') === document.getElementById('a')"), "true");
    b.collect(&[]).expect("collect");
    assert_eq!(js(&mut b, "document.getElementById('a').rel.length + ',' + document.createElement('link').rel.length"), "0,0");
}

#[test]
fn wrong_receivers_and_foreign_namespaces_are_illegal() {
    let mut b = browser(PAGE);
    js(&mut b, &format!("var g = Object.getOwnPropertyDescriptor(HTMLLinkElement.prototype, 'rel'), k = document.getElementById('k'), d = document.getElementById('d'), sv = document.getElementById('sv'), m = document.createElementNS('{MATH}', 'link'), t = document.createTextNode('x');"));
    for recv in ["document.body", "document", "k", "d", "t", "sv", "m", "{}", "HTMLLinkElement.prototype", "window", "null", "undefined"] {
        assert_eq!(js(&mut b, &format!("g.get.call({recv})")), "throw:TypeError:Illegal invocation", "get on {recv}");
        assert_eq!(js(&mut b, &format!("g.set.call({recv}, 'changed')")), "throw:TypeError:Illegal invocation", "set on {recv}");
    }
    // the refused writes changed nothing
    assert_eq!(js(&mut b, "k.getAttribute('rel') + ',' + d.getAttribute('rel') + ',' + sv.getAttribute('rel')"), "y,x,z");
    assert_eq!(js(&mut b, "[typeof k.rel, typeof d.rel, typeof sv.rel].join()"), "undefined,undefined,undefined");
    assert_eq!(js(&mut b, "HTMLLinkElement.prototype.rel"), "throw:TypeError:Illegal invocation");
    // an SVG `link` is an ordinary wrapper: a JS property, not the attribute
    assert_eq!(js(&mut b, &format!("var sl = document.createElementNS('{SVG}', 'link'); sl.rel = 'x'; sl.hasAttribute('rel') + ',' + sl.rel")), "false,x");
}

#[test]
fn rel_caps_are_checked_before_the_write_and_on_read() {
    // a value over `max_string_bytes` is refused before any write (the attribute keeps its old value)
    let mut b = browser_with(PAGE, BrowserLimits { max_string_bytes: 100, ..Default::default() });
    assert_eq!(js(&mut b, "var a = document.getElementById('a'), v = ''; for (var i = 0; i < 50; i++) v += 'x'; a.rel = v; a.rel.length"), "50");
    assert_eq!(js(&mut b, "var w = ''; for (var i = 0; i < 200; i++) w += 'y'; a.rel = w; 0"), lim("dom-string"));
    assert_eq!(js(&mut b, "a.getAttribute('rel').length"), "50");

    // reading a long value is refused too: the same string cap as every other string out of the document
    let page = format!("<body><link id=big rel='{}'></body>", "z".repeat(200));
    let mut b = browser_with(&page, BrowserLimits { max_string_bytes: 100, ..Default::default() });
    assert_eq!(js(&mut b, "document.getElementById('big').rel"), lim("dom-string"));
    assert_eq!(js(&mut b, "document.getElementById('big').hasAttribute('rel')"), "true");

    // a new attribute needs an attribute slot: none left, nothing is written (this page has no attribute or text)
    let plain = "<!doctype html><body><p></p></body>";
    let mut b = browser_with(plain, BrowserLimits { max_attr_slots: 0, ..Default::default() });
    assert_eq!(js(&mut b, "var n = document.createElement('link'); n.rel = 'x'; 0"), lim("dom-attrs"));
    assert_eq!(js(&mut b, "n.hasAttribute('rel') + ',' + n.rel.length"), "false,0");

    // the value bytes count against the native text cap, before the write: the cap is the page's own text, so one more byte is refused
    let base = browser(plain).document().borrow().text_bytes();
    let mut b = browser_with(plain, BrowserLimits { max_text_bytes: base, ..Default::default() });
    assert_eq!(js(&mut b, "var n = document.createElement('link'); n.rel = 'x'; 0"), lim("dom-text"));
    assert_eq!(js(&mut b, "n.hasAttribute('rel')"), "false");
}


#[test]
fn rel_reflects_only_the_null_namespace_attribute() {
    let mut b = browser(PAGE);
    assert_eq!(js(&mut b, "var x = document.createElement('link'); x.setAttributeNS('http://www.w3.org/1999/xlink','rel','foreign'); x.rel"), "");
    assert_eq!(js(&mut b, "x.rel = 'own'; [x.rel, x.getAttributeNS(null,'rel'), x.getAttributeNS('http://www.w3.org/1999/xlink','rel')].join()"), "own,own,foreign");
}
