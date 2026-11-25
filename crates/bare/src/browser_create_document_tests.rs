//! `document.implementation.createDocument(namespace, qualifiedName, doctype)`. DOMPurify falls back to it when `DOMParser` fails.
use crate::browser::*;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

fn run(src: &str) -> String {
    let mut b = Browser::new(tree::parse("<!doctype html><p>x</p>"), BrowserLimits::default(), EvalLimits { max_steps: 2_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    let r = b.run_script(src).expect(src);
    b.it.display(r)
}

const HTML: &str = "http://www.w3.org/1999/xhtml";
const SVG: &str = "http://www.w3.org/2000/svg";

#[test]
fn html_template_takes_inner_html() {
    let src = format!("try {{ var d = document.implementation.createDocument('{HTML}', 'template', null); d.documentElement.innerHTML = '<p>a</p>'; d.documentElement.innerHTML + '|' + d.documentElement.content.textContent }} catch (e) {{ e.name + ': ' + e.message }}");
    assert_eq!(run(&src), "<p>a</p>|a");
}

#[test]
fn svg_root_keeps_its_namespace() {
    let src = format!("try {{ var e = document.implementation.createDocument('{SVG}', 'svg', null).documentElement; e.namespaceURI + '|' + e.localName }} catch (e) {{ e.name + ': ' + e.message }}");
    assert_eq!(run(&src), format!("{SVG}|svg"));
}

#[test]
fn empty_name_gives_no_root() {
    let src = format!("try {{ var d = document.implementation.createDocument('{HTML}', ''); [d.documentElement, d.childNodes.length].join('|') }} catch (e) {{ e.name + ': ' + e.message }}");
    assert_eq!(run(&src), "|0");
}

#[test]
fn bad_names_throw_the_dom_errors() {
    let src = "function f(n, q) { try { document.implementation.createDocument(n, q, null); return 'ok'; } catch (e) { return e.name; } } [f('x', '1x'), f(null, 'a:b')].join('|')";
    assert_eq!(run(src), "InvalidCharacterError|NamespaceError");
}

#[test]
fn the_document_is_separate() {
    let src = format!("var d = document.implementation.createDocument('{HTML}', 'template', null); \
        [typeof d.implementation, d === document, d.documentElement.ownerDocument === d].join('|')");
    assert_eq!(run(&src), "object|false|true");
}

#[test]
fn one_argument_is_a_type_error() {
    assert_eq!(run("try { document.implementation.createDocument('x'); 'ok' } catch (e) { e.name }"), "TypeError");
}
