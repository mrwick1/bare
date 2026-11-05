//! Tests of namespace identity: HTML, SVG and MathML elements, namespaced attributes, and parser-made SVG.
//! Expectations are written by hand from the DOM Standard and the HTML parsing rules. Nothing here was compared with
//! Chrome, and Node has no DOM. React shapes are copied from the cached staging bundle by reading (not by running it).

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

const SVG: &str = "http://www.w3.org/2000/svg";
const HTML: &str = "http://www.w3.org/1999/xhtml";
const MATH: &str = "http://www.w3.org/1998/Math/MathML";
const XLINK: &str = "http://www.w3.org/1999/xlink";
const XML: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS: &str = "http://www.w3.org/2000/xmlns/";

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const PAGE: &str = "<!doctype html><html><head><title>t</title></head><body><div id=a></div></body></html>";

fn browser_with(html: &str, lim: BrowserLimits) -> Browser {
    let doc = tree::parse(html);
    Browser::new(doc, lim, EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn browser() -> Browser {
    browser_with(PAGE, BrowserLimits::default())
}

fn show(b: &mut Browser, r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Throw(v)) => format!("throw-value:{}", b.it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src);
    show(b, r)
}

fn err_name(b: &mut Browser, src: &str) -> String {
    let r = js(b, src);
    r.split(':').take(2).collect::<Vec<_>>().join(":")
}

#[test]
fn created_elements_carry_their_namespace() {
    big(|| {
        let mut b = browser();
        js(&mut b, &format!("var s = document.createElementNS('{SVG}', 'svg'), h = document.createElement('DiV'), m = document.createElementNS('{MATH}', 'math'), x = document.createElementNS('{HTML}', 'section');"));
        assert_eq!(js(&mut b, "[s.namespaceURI, s.localName, s.prefix, s.tagName, s.nodeName, s.nodeType].join('|')"), format!("{SVG}|svg||svg|svg|1"));
        assert_eq!(js(&mut b, "s.prefix === null && h.prefix === null && m.prefix === null"), "true");
        assert_eq!(js(&mut b, "[h.namespaceURI, h.localName, h.tagName, h.nodeName].join('|')"), format!("{HTML}|div|DIV|DIV"));
        assert_eq!(js(&mut b, "[x.namespaceURI, x.localName, x.tagName].join('|')"), format!("{HTML}|section|SECTION"));
        assert_eq!(js(&mut b, "[m.namespaceURI, m.localName, m.tagName].join('|')"), format!("{MATH}|math|math"));
        // the document's own elements are HTML
        assert_eq!(js(&mut b, "[document.documentElement.namespaceURI, document.body.namespaceURI].join('|')"), format!("{HTML}|{HTML}"));
        assert_eq!(js(&mut b, "typeof document.namespaceURI + ',' + typeof document.createTextNode('x').namespaceURI"), "undefined,undefined");
    });
}

#[test]
fn svg_names_keep_their_case_and_html_names_do_not() {
    big(|| {
        let mut b = browser();
        js(&mut b, &format!("var f = document.createElementNS('{SVG}', 'foreignObject'), g = document.createElementNS('{SVG}', 'linearGradient'), q = document.createElementNS('{SVG}', 'svg:rect');"));
        assert_eq!(js(&mut b, "[f.localName, f.tagName, f.nodeName, g.localName, g.tagName].join()"), "foreignObject,foreignObject,foreignObject,linearGradient,linearGradient");
        assert_eq!(js(&mut b, "[q.prefix, q.localName, q.tagName, q.nodeName].join()"), "svg,rect,svg:rect,svg:rect");
        // an HTML element is lowercased by createElement and its tagName is uppercase
        assert_eq!(js(&mut b, "var h = document.createElement('foreignObject'); [h.localName, h.tagName].join()"), "foreignobject,FOREIGNOBJECT");
        // case is part of identity: the two names are different elements for CSS and getElementsByTagName
        js(&mut b, "document.body.appendChild(f); document.body.appendChild(h); document.body.appendChild(q);");
        assert_eq!(js(&mut b, "[document.getElementsByTagName('foreignObject').length, document.getElementsByTagName('foreignobject').length, document.getElementsByTagName('RECT').length, document.getElementsByTagName('svg:rect').length, document.getElementsByTagName('FOREIGNOBJECT').length].join()"), "2,1,0,1,1");
    });
}

#[test]
fn wrapper_brands_follow_the_namespace_not_the_name() {
    big(|| {
        let mut b = browser();
        js(&mut b, &format!("var s = document.createElementNS('{SVG}', 'svg'), si = document.createElementNS('{SVG}', 'input'), ss = document.createElementNS('{SVG}', 'script'), h = document.createElement('div'), hi = document.createElement('input'), m = document.createElementNS('{MATH}', 'math');"));
        assert_eq!(js(&mut b, "[s instanceof SVGElement, s instanceof Element, s instanceof Node, s instanceof HTMLElement].join()"), "true,true,true,false");
        assert_eq!(js(&mut b, "[h instanceof SVGElement, h instanceof HTMLElement].join()"), "false,true");
        // an SVG element named `input` is not an <input>, and an SVG `script` is not an HTML script
        assert_eq!(js(&mut b, "[si instanceof HTMLInputElement, si instanceof SVGElement, typeof si.value, hi instanceof HTMLInputElement].join()"), "false,true,undefined,true");
        assert_eq!(js(&mut b, "[ss instanceof HTMLElement, ss instanceof SVGElement].join()"), "false,true");
        assert_eq!(js(&mut b, "[m instanceof Element, m instanceof HTMLElement, m instanceof SVGElement].join()"), "true,false,false");
        assert_eq!(js(&mut b, "Object.getPrototypeOf(SVGElement.prototype) === Element.prototype"), "true");
        assert_eq!(js(&mut b, "new SVGElement()"), "throw:TypeError:SVGElement is not a constructor");
    });
}

#[test]
fn qualified_name_and_namespace_errors_make_nothing() {
    big(|| {
        let mut b = browser();
        let before = b.stats();
        let names = b.document().borrow().name_count();
        for (call, want) in [
            (format!("document.createElementNS('{SVG}', '1a')"), "throw:InvalidCharacterError"),
            (format!("document.createElementNS('{SVG}', 'a b')"), "throw:InvalidCharacterError"),
            (format!("document.createElementNS('{SVG}', '')"), "throw:InvalidCharacterError"),
            (format!("document.createElementNS('{SVG}', ':a')"), "throw:InvalidCharacterError"),
            (format!("document.createElementNS('{SVG}', 'a:')"), "throw:InvalidCharacterError"),
            (format!("document.createElementNS('{SVG}', 'a:b:c')"), "throw:InvalidCharacterError"),
            ("document.createElementNS(null, 'a:b')".to_string(), "throw:NamespaceError"),
            (format!("document.createElementNS('{SVG}', 'xml:a')"), "throw:NamespaceError"),
            (format!("document.createElementNS('{SVG}', 'xmlns:a')"), "throw:NamespaceError"),
            (format!("document.createElementNS('{SVG}', 'xmlns')"), "throw:NamespaceError"),
            (format!("document.createElementNS('{XMLNS}', 'a')"), "throw:NamespaceError"),
        ] {
            assert_eq!(err_name(&mut b, &call), want, "{call}");
        }
        assert_eq!(b.stats().nodes, before.nodes);
        assert_eq!(b.document().borrow().name_count(), names);
    });
}

#[test]
fn namespaces_outside_the_scope_are_refused_by_name() {
    big(|| {
        let mut b = browser();
        let before = b.stats();
        let names = b.document().borrow().name_count();
        let other = "unsupported:createElementNS with a namespace other than HTML, SVG or MathML";
        assert_eq!(js(&mut b, "document.createElementNS('http://example.com/x', 'a')"), other);
        assert_eq!(js(&mut b, "document.createElementNS('http://example.com/x', 'p:a')"), other);
        assert_eq!(js(&mut b, &format!("document.createElementNS('{XLINK}', 'a')")), other);
        assert_eq!(js(&mut b, "document.createElementNS(null, 'x')"), "unsupported:createElementNS with a null namespace");
        assert_eq!(js(&mut b, "document.createElementNS('', 'x')"), "unsupported:createElementNS with a null namespace");
        assert_eq!(js(&mut b, "document.createElementNS(undefined, 'x')"), "unsupported:createElementNS with a null namespace");
        // the HTML namespace keeps its lowercase-only gate: no prefix, no capital letters
        assert_eq!(js(&mut b, &format!("document.createElementNS('{HTML}', 'x:y')")), "unsupported:createElementNS in the HTML namespace with a prefix or capital letters (names are lowercase in the native document)");
        assert_eq!(js(&mut b, &format!("document.createElementNS('{HTML}', 'Div')")), "unsupported:createElementNS in the HTML namespace with a prefix or capital letters (names are lowercase in the native document)");
        assert_eq!(b.stats().nodes, before.nodes);
        assert_eq!(b.document().borrow().name_count(), names);
        // attributes
        js(&mut b, &format!("var s = document.createElementNS('{SVG}', 'svg');"));
        let names = b.document().borrow().name_count();
        let attr = "unsupported:setAttributeNS with a namespace other than none, XML, XMLNS or XLink";
        assert_eq!(js(&mut b, "s.setAttributeNS('http://example.com/x', 'p:a', 'v')"), attr);
        assert_eq!(js(&mut b, &format!("s.setAttributeNS('{SVG}', 'a', 'v')")), attr);
        assert_eq!(js(&mut b, "s.getAttributeNames().length"), "0");
        assert_eq!(b.document().borrow().name_count(), names);
        // reading a namespace that no attribute can have is just "not there"
        assert_eq!(js(&mut b, "[s.getAttributeNS('http://example.com/x', 'a'), s.hasAttributeNS('http://example.com/x', 'a')].join()"), ",false");
    });
}

#[test]
fn attributes_follow_the_case_rules_of_the_element() {
    big(|| {
        let mut b = browser();
        js(&mut b, &format!("var s = document.createElementNS('{SVG}', 'svg'), h = document.createElement('div'); s.setAttribute('viewBox', '0 0 1 1'); h.setAttribute('VIEWBOX', 'x'); s.setAttribute('preserveAspectRatio', 'none');"));
        assert_eq!(js(&mut b, "s.getAttributeNames().join()"), "viewBox,preserveAspectRatio");
        assert_eq!(js(&mut b, "[s.getAttribute('viewBox'), s.getAttribute('viewbox'), s.hasAttribute('viewBox'), s.hasAttribute('VIEWBOX')].join()"), "0 0 1 1,,true,false");
        assert_eq!(js(&mut b, "s.getAttribute('viewbox') === null"), "true");
        assert_eq!(js(&mut b, "h.getAttributeNames().join() + '|' + h.getAttribute('ViewBox') + '|' + h.hasAttribute('VIEWBOX')"), "viewbox|x|true");
        // a second setAttribute with the same case changes the value, with another case it adds a name on an SVG element
        js(&mut b, "s.setAttribute('viewBox', '1 1 1 1'); s.setAttribute('viewbox', 'lower');");
        assert_eq!(js(&mut b, "s.getAttributeNames().join() + '|' + s.getAttribute('viewBox') + '|' + s.getAttribute('viewbox')"), "viewBox,preserveAspectRatio,viewbox|1 1 1 1|lower");
        js(&mut b, "s.removeAttribute('viewBox');");
        assert_eq!(js(&mut b, "s.getAttributeNames().join()"), "preserveAspectRatio,viewbox");
        js(&mut b, "s.removeAttribute('VIEWBOX');");
        assert_eq!(js(&mut b, "s.getAttributeNames().join()"), "preserveAspectRatio,viewbox");
        // the invalid name of setAttribute is still refused before any write
        assert_eq!(err_name(&mut b, "s.setAttribute('a b', 'v')"), "throw:InvalidCharacterError");
        assert_eq!(js(&mut b, "s.getAttributeNames().length"), "2");
    });
}

#[test]
fn namespaced_attributes_have_their_own_identity() {
    big(|| {
        let mut b = browser();
        js(&mut b, &format!("var s = document.createElementNS('{SVG}', 'use'); s.setAttributeNS('{XLINK}', 'xlink:href', '#a'); s.setAttributeNS('{XML}', 'xml:space', 'preserve'); s.setAttributeNS('{XMLNS}', 'xmlns:xlink', '{XLINK}'); s.setAttributeNS(null, 'fill', 'red'); s.setAttributeNS('{XMLNS}', 'xmlns', '{SVG}');"));
        assert_eq!(js(&mut b, "s.getAttributeNames().join()"), "xlink:href,xml:space,xmlns:xlink,fill,xmlns");
        assert_eq!(js(&mut b, &format!("[s.getAttributeNS('{XLINK}', 'href'), s.getAttributeNS(null, 'href'), s.getAttributeNS('{XML}', 'space'), s.getAttributeNS('{XMLNS}', 'xlink'), s.getAttributeNS('{XMLNS}', 'xmlns'), s.getAttributeNS(null, 'fill'), s.getAttributeNS('', 'fill')].join('|')")), format!("#a||preserve|{XLINK}|{SVG}|red|red"));
        assert_eq!(js(&mut b, "s.getAttribute('xlink:href') + ',' + s.getAttribute('xmlns:xlink').length + ',' + s.getAttribute('fill')"), format!("#a,{},red", XLINK.len()));
        assert_eq!(js(&mut b, &format!("[s.hasAttributeNS('{XLINK}', 'href'), s.hasAttributeNS(null, 'href'), s.hasAttributeNS(null, 'fill'), s.hasAttribute('xml:space')].join()")), "true,false,true,true");
        // setting by the qualified name changes the namespaced attribute that has that name
        js(&mut b, "s.setAttribute('xlink:href', '#b');");
        assert_eq!(js(&mut b, &format!("s.getAttributeNS('{XLINK}', 'href') + ',' + s.getAttributeNames().length")), "#b,5");
        // the same namespace and local name changes the value and keeps the old prefix
        js(&mut b, &format!("s.setAttributeNS('{XLINK}', 'x:href', '#c');"));
        assert_eq!(js(&mut b, "[s.getAttributeNames()[0], s.getAttribute('xlink:href'), s.getAttribute('x:href'), s.getAttributeNames().length].join()"), "xlink:href,#c,,5");
        js(&mut b, &format!("s.removeAttributeNS('{XLINK}', 'href'); s.removeAttributeNS(null, 'fill'); s.removeAttributeNS(null, 'nothing');"));
        assert_eq!(js(&mut b, "s.getAttributeNames().join()"), "xml:space,xmlns:xlink,xmlns");
        // a removal by qualified name removes a namespaced attribute too, the one that comes first
        js(&mut b, "s.removeAttribute('xml:space');");
        assert_eq!(js(&mut b, "s.getAttributeNames().join()"), "xmlns:xlink,xmlns");
        // a null-namespace attribute named like a prefixed one is a different attribute from the namespaced one
        js(&mut b, &format!("var u = document.createElementNS('{SVG}', 'use'); u.setAttribute('xlink:href', 'plain'); u.setAttributeNS('{XLINK}', 'xlink:href', 'ns');"));
        assert_eq!(js(&mut b, &format!("[u.getAttributeNames().join(), u.getAttribute('xlink:href'), u.getAttributeNS('{XLINK}', 'href'), u.getAttributeNS(null, 'xlink:href')].join('|')")), "xlink:href,xlink:href|plain|ns|plain");
        // on an HTML element the NS functions do not change the case
        js(&mut b, "var h = document.createElement('div'); h.setAttributeNS(null, 'DATA-X', '1');");
        assert_eq!(js(&mut b, "[h.getAttributeNames().join(), h.getAttribute('data-x'), h.getAttributeNS(null, 'DATA-X')].join('|')"), "DATA-X||1");
    });
}

#[test]
fn attribute_namespace_errors_change_nothing() {
    big(|| {
        let mut b = browser();
        js(&mut b, &format!("var s = document.createElementNS('{SVG}', 'svg'); s.setAttribute('keep', '1');"));
        let slots = b.stats().attr_slots;
        let names = b.document().borrow().name_count();
        for (call, want) in [
            ("s.setAttributeNS(null, 'a:b', 'v')".to_string(), "throw:NamespaceError"),
            (format!("s.setAttributeNS('{XLINK}', 'xml:lang', 'v')"), "throw:NamespaceError"),
            (format!("s.setAttributeNS('{XLINK}', 'xmlns:foo', 'v')"), "throw:NamespaceError"),
            (format!("s.setAttributeNS('{XLINK}', 'xmlns', 'v')"), "throw:NamespaceError"),
            (format!("s.setAttributeNS('{XMLNS}', 'a:b', 'v')"), "throw:NamespaceError"),
            (format!("s.setAttributeNS('{XMLNS}', 'a', 'v')"), "throw:NamespaceError"),
            (format!("s.setAttributeNS('{XLINK}', 'xlink:', 'v')"), "throw:InvalidCharacterError"),
            (format!("s.setAttributeNS('{XLINK}', ':href', 'v')"), "throw:InvalidCharacterError"),
            (format!("s.setAttributeNS('{XLINK}', 'a:b:c', 'v')"), "throw:InvalidCharacterError"),
            (format!("s.setAttributeNS('{XLINK}', '1a', 'v')"), "throw:InvalidCharacterError"),
            (format!("s.setAttributeNS('{XLINK}', 'a b', 'v')"), "throw:InvalidCharacterError"),
            (format!("s.setAttributeNS('{XLINK}', '', 'v')"), "throw:InvalidCharacterError"),
        ] {
            assert_eq!(err_name(&mut b, &call), want, "{call}");
        }
        assert_eq!(js(&mut b, "s.getAttributeNames().join()"), "keep");
        assert_eq!(b.stats().attr_slots, slots);
        assert_eq!(b.document().borrow().name_count(), names);
        // a value that cannot be stored changes nothing either (a lone surrogate)
        assert_eq!(js(&mut b, &format!("s.setAttributeNS('{XLINK}', 'xlink:href', '\\ud800')")), "unsupported:lone surrogate in a DOM string (the native document is UTF-8)");
        assert_eq!(js(&mut b, "s.getAttributeNames().join()"), "keep");
        // the XML names of valid-looking qualified names are the ones the existing rules accept
        assert_eq!(js(&mut b, &format!("s.setAttributeNS('{XLINK}', 'xlink:href', 1); s.getAttributeNS('{XLINK}', 'href')")), "1");
    });
}

#[test]
fn clone_and_move_keep_the_namespace_of_every_node() {
    big(|| {
        let mut b = browser();
        js(&mut b, &format!(
            "var root = document.createElementNS('{SVG}', 'svg'), g = document.createElementNS('{SVG}', 'svg:g'), fo = document.createElementNS('{SVG}', 'foreignObject'), d = document.createElement('div');
             root.setAttributeNS('{XMLNS}', 'xmlns:xlink', '{XLINK}'); g.setAttributeNS('{XLINK}', 'xlink:href', '#x'); g.setAttribute('viewBox', '0 0 1 1');
             root.appendChild(g); root.appendChild(fo); fo.appendChild(d); document.body.appendChild(root);"));
        let probe = "function probe(n) { var out = []; (function walk(e) { out.push([e.namespaceURI, e.prefix, e.localName, e.tagName, e.getAttributeNames().join('+')].join('/')); for (var c = e.firstElementChild; c; c = c.nextElementSibling) walk(c); })(n); return out.join(';'); }";
        js(&mut b, probe);
        let want = format!("{SVG}//svg/svg/xmlns:xlink;{SVG}/svg/g/svg:g/xlink:href+viewBox;{SVG}//foreignObject/foreignObject/;{HTML}//div/DIV/");
        assert_eq!(js(&mut b, "probe(root)"), want);
        // a deep clone is detached and has the same identity for each node, and the attribute namespaces
        assert_eq!(js(&mut b, "var c = root.cloneNode(true); [c !== root, c.isConnected, probe(c) === probe(root)].join()"), "true,false,true");
        assert_eq!(js(&mut b, &format!("c.firstElementChild.getAttributeNS('{XLINK}', 'href') + ',' + c.getAttributeNS('{XMLNS}', 'xlink')")), format!("#x,{XLINK}"));
        // a shallow clone keeps the element's own identity and attributes
        assert_eq!(js(&mut b, "var sc = g.cloneNode(false); [sc.namespaceURI, sc.prefix, sc.localName, sc.childNodes.length, sc.getAttributeNames().join()].join('|')"), format!("{SVG}|svg|g|0|xlink:href,viewBox"));
        // moving between parents (HTML to SVG to HTML) changes nothing about identity
        js(&mut b, "document.getElementById('a').appendChild(g); fo.appendChild(g); root.appendChild(g); document.body.appendChild(g);");
        assert_eq!(js(&mut b, &format!("[g.namespaceURI, g.prefix, g.localName, g.parentNode === document.body, g.getAttributeNS('{XLINK}', 'href')].join('|')")), format!("{SVG}|svg|g|true|#x"));
        // an HTML element made inside an SVG parent keeps the HTML namespace; an SVG element in an HTML parent keeps SVG
        assert_eq!(js(&mut b, "[d.namespaceURI, fo.parentNode === root, root.parentNode === document.body].join()"), format!("{HTML},true,true"));
        // a clone is another native node: the original is not changed by changing the clone
        js(&mut b, "c.firstElementChild.setAttributeNS(null, 'extra', '1');");
        assert_eq!(js(&mut b, "g.hasAttribute('extra')"), "false");
        // selectors see the local name; case rules are the document's (lowercase names match)
        assert_eq!(js(&mut b, "[root.matches('svg'), g.matches('g'), document.querySelectorAll('svg').length, document.body.querySelector('g') === g].join()"), "true,true,1,true");
    });
}

#[test]
fn reduced_react_host_context_and_creation_shapes() {
    big(|| {
        let mut b = browser();
        // copied by reading from the cached bundle (functions s2, s3, the root context V, and the createElementNS branch)
        js(&mut b, &format!(
            "function s2(e){{switch(e){{case '{SVG}':return 1;case '{MATH}':return 2;default:return 0}}}}
             function s3(e,t){{if(0===e)switch(t){{case 'svg':return 1;case 'math':return 2;default:return 0}}return 1===e&&'foreignObject'===t?0:e}}
             function rootCtx(t){{var e=t.nodeType;switch(e){{case 9:case 11:t=(t=t.documentElement)&&(t=t.namespaceURI)?s2(t):0;break;default:if(t=(e=8===e?t.parentNode:t).tagName,e=e.namespaceURI)t=s3(e=s2(e),t);else switch(t){{case 'svg':t=1;break;case 'math':t=2;break;default:t=0}}}}return t}}
             function create(ctx,n,doc){{var e;switch(ctx){{case 1:e=doc.createElementNS('{SVG}',n);break;case 2:e=doc.createElementNS('{MATH}',n);break;default:switch(n){{case 'svg':e=doc.createElementNS('{SVG}',n);break;case 'math':e=doc.createElementNS('{MATH}',n);break;default:e=doc.createElement(n)}}}}return e}}"));
        assert_eq!(js(&mut b, "rootCtx(document)"), "0");
        assert_eq!(js(&mut b, "rootCtx(document.body)"), "0");
        // div -> svg -> path -> foreignObject -> div -> math
        js(&mut b, "var d = create(0, 'div', document), s = create(rootCtx(d), 'svg', document); d.appendChild(s);");
        assert_eq!(js(&mut b, "[rootCtx(s), s.namespaceURI, s instanceof SVGElement].join()"), format!("1,{SVG},true"));
        js(&mut b, "var p = create(rootCtx(s), 'path', document), fo = create(rootCtx(s), 'foreignObject', document); s.appendChild(p); s.appendChild(fo);");
        assert_eq!(js(&mut b, "[rootCtx(p), p.namespaceURI, rootCtx(fo), fo.tagName].join()"), format!("1,{SVG},0,foreignObject"));
        js(&mut b, "var inner = create(rootCtx(fo), 'div', document); fo.appendChild(inner); var m = create(rootCtx(inner), 'math', document); inner.appendChild(m);");
        assert_eq!(js(&mut b, "[inner.namespaceURI, rootCtx(inner), m.namespaceURI, rootCtx(m)].join()"), format!("{HTML},0,{MATH},2"));
        // a comment node asks its parent
        assert_eq!(js(&mut b, "var c = document.createComment('x'); s.appendChild(c); rootCtx(c)"), "1");
        // attribute shapes of the bundle: the helper eY and xlink:href
        js(&mut b, &format!("function eY(e,t,n,r){{if(null===r)e.removeAttribute(n);else{{switch(typeof r){{case 'undefined':case 'function':case 'symbol':case 'boolean':e.removeAttribute(n);return}}e.setAttributeNS(t,n,''+r)}}}}
            eY(p, null, 'stroke-width', 2); eY(p, '{XLINK}', 'xlink:actuate', 'onLoad'); p.setAttributeNS('{XLINK}', 'xlink:href', '#g'); eY(p, '{XLINK}', 'xlink:actuate', null); p.setAttribute('xmlns:xlink', '{XLINK}');"));
        assert_eq!(js(&mut b, &format!("p.getAttributeNames().join() + '|' + p.getAttribute('stroke-width') + '|' + p.getAttributeNS('{XLINK}', 'href')")), "stroke-width,xlink:href,xmlns:xlink|2|#g");
        assert_eq!(js(&mut b, &format!("[p.getAttributeNS(null, 'xmlns:xlink'), p.getAttributeNS('{XMLNS}', 'xlink')].join()")), format!("{XLINK},"));
        // the title lookup and stylesheet filter of the bundle read namespaceURI of existing elements
        assert_eq!(js(&mut b, &format!("Array.prototype.filter.call(document.getElementsByTagName('path'), function (a) {{ return '{SVG}' !== a.namespaceURI }}).length")), "0");
    });
}

const SVG_PAGE: &str = "<!doctype html><html><body><p id=before>x</p><svg id=sv viewBox='0 0 10 10' xmlns='http://www.w3.org/2000/svg' xmlns:xlink='http://www.w3.org/1999/xlink'>\
<linearGradient id=g><stop offset=0></stop></linearGradient><foreignObject id=fo width=5><div id=d>t</div></foreignObject>\
<path id=pa d='M0 0' xlink:href='#g' xml:lang='en'/><rect id=rc class=a></rect><title id=ti>cap</title><a id=sa href='x'><circle id=ci /></a></svg><p id=after>y</p>\
<math id=ma><mi id=mi>x</mi></math></body></html>";

#[test]
fn the_parser_makes_svg_and_mathml_with_their_namespace() {
    big(|| {
        let mut b = browser_with(SVG_PAGE, BrowserLimits::default());
        let id = |name: &str| format!("document.getElementById('{name}')");
        let row = |name: &str| format!("var e = {}; [e.namespaceURI, e.localName, e.tagName].join('|')", id(name));
        assert_eq!(js(&mut b, &row("sv")), format!("{SVG}|svg|svg"));
        assert_eq!(js(&mut b, &row("g")), format!("{SVG}|linearGradient|linearGradient"));
        assert_eq!(js(&mut b, &row("fo")), format!("{SVG}|foreignObject|foreignObject"));
        assert_eq!(js(&mut b, &row("d")), format!("{HTML}|div|DIV"));
        assert_eq!(js(&mut b, &row("pa")), format!("{SVG}|path|path"));
        assert_eq!(js(&mut b, &row("ma")), format!("{MATH}|math|math"));
        assert_eq!(js(&mut b, &row("mi")), format!("{MATH}|mi|mi"));
        assert_eq!(js(&mut b, &row("ci")), format!("{SVG}|circle|circle"));
        assert_eq!(js(&mut b, &row("sa")), format!("{SVG}|a|a"));
        assert_eq!(js(&mut b, &row("before")), format!("{HTML}|p|P"));
        // brands
        assert_eq!(js(&mut b, &format!("[{0} instanceof SVGElement, {1} instanceof HTMLElement, {1} instanceof SVGElement, {2} instanceof HTMLElement, {2} instanceof Element].join()", id("pa"), id("d"), id("ma"))), "true,true,false,false,true");
        // attribute case and namespaces
        assert_eq!(js(&mut b, &format!("var s = {}; [s.getAttribute('viewBox'), s.getAttribute('viewbox'), s.getAttributeNames().join()].join('|')", id("sv"))), "0 0 10 10||id,viewBox,xmlns,xmlns:xlink");
        assert_eq!(js(&mut b, &format!("var p = {}; [p.getAttributeNS('{XLINK}', 'href'), p.getAttributeNS('{XML}', 'lang'), p.getAttributeNS(null, 'd'), p.getAttributeNames().join()].join('|')", id("pa"))), "#g|en|M0 0|id,d,xlink:href,xml:lang");
        assert_eq!(js(&mut b, &format!("[s.getAttributeNS('{XMLNS}', 'xmlns'), s.getAttributeNS('{XMLNS}', 'xlink').length, s.getAttributeNS(null, 'xmlns')].join('|')")), format!("{SVG}|{}|", XLINK.len()));
        // self-closing svg elements do not swallow the next one, and `p` after the svg is a sibling of it
        assert_eq!(js(&mut b, "[document.getElementById('pa').nextElementSibling.id, document.getElementById('after').previousElementSibling.id, document.getElementById('after').parentNode === document.body].join()"), "rc,sv,true");
        // type selectors match the local name; class and id work
        assert_eq!(js(&mut b, "[document.querySelectorAll('svg').length, document.querySelector('svg > rect').id, document.querySelector('rect.a').id, document.querySelectorAll('svg *').length].join()"), "1,rc,rc,9");
        assert_eq!(js(&mut b, "[document.getElementsByTagName('foreignObject').length, document.getElementsByTagName('foreignobject').length, document.getElementsByTagName('path').length, document.getElementsByTagName('a').length].join()"), "1,0,1,1");
        // an SVG element that was never wrapped before a collection is wrapped again with the right brand
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, &format!("var r = document.getElementById('rc'); [r instanceof SVGElement, r.namespaceURI === '{SVG}', r.parentNode === document.getElementById('sv')].join()")), "true,true,true");
    });
}

const FOREIGN_CONTEXT_PAGE: &str = "<!doctype html><html><body><svg id=sv><title id=ti><b id=tb>x</b></title><style id=st>a &amp; b<g id=sg /></style>\
<text id=tx><![CDATA[<c>&amp;]]></text><foreignObject id=fo><style id=fs>&amp;<b></style><textarea id=ta>&amp;<b></textarea><p id=fp><![CDATA[q]]></p></foreignObject></svg>\
<b id=out><svg><foreignObject id=f2>x</b>y</foreignObject></svg></body></html>";

#[test]
fn the_parser_picks_raw_text_and_cdata_by_namespace() {
    big(|| {
        let mut b = browser_with(FOREIGN_CONTEXT_PAGE, BrowserLimits::default());
        let row = |id: &str| format!("var e = document.getElementById('{id}'); [e.namespaceURI, e.localName, e.childNodes.length, e.firstChild.nodeType, e.firstChild.data].join('|')");
        // SVG title is an integration point: HTML markup inside. SVG style holds text and an SVG element.
        assert_eq!(js(&mut b, "var tb = document.getElementById('tb'); [tb.namespaceURI, tb.parentNode.id, tb.firstChild.data].join('|')"), format!("{HTML}|ti|x"));
        assert_eq!(js(&mut b, &row("st")), format!("{SVG}|style|2|3|a & b"));
        assert_eq!(js(&mut b, "var g = document.getElementById('sg'); [g.namespaceURI, g.parentNode.id].join('|')"), format!("{SVG}|st"));
        // CDATA in SVG is text, not decoded; the same marker in an HTML element is a comment.
        assert_eq!(js(&mut b, &row("tx")), format!("{SVG}|text|1|3|<c>&amp;"));
        assert_eq!(js(&mut b, "var p = document.getElementById('fp'); [p.firstChild.nodeType, p.firstChild.data].join('|')"), "8|[CDATA[q]]");
        // The HTML style and textarea in the foreignObject keep raw text and RCDATA.
        assert_eq!(js(&mut b, &row("fs")), format!("{HTML}|style|1|3|&amp;<b>"));
        assert_eq!(js(&mut b, &row("ta")), format!("{HTML}|textarea|1|3|&<b>"));
        // `</b>` does not close past the foreignObject: `y` stays inside it.
        assert_eq!(js(&mut b, "var f = document.getElementById('f2'); [f.textContent, f.parentNode.localName, document.getElementById('out').lastElementChild.localName].join('|')"), "xy|svg|svg");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "document.getElementById('tx').firstChild.data"), "<c>&amp;");
    });
}

#[test]
fn weak_values_and_collection_with_svg_wrappers() {
    big(|| {
        let mut b = browser();
        let first = js(&mut b, &format!("var wm = new WeakMap(), ws = new WeakSet(); for (var i = 0; i < 50; i++) {{ var e = document.createElementNS('{SVG}', 'g'); e.setAttributeNS('{XLINK}', 'xlink:href', '#' + i); wm.set(e, i); ws.add(e); }} var keep = document.createElementNS('{SVG}', 'svg'); keep.setAttributeNS(null, 'viewBox', '0 0 1 1'); wm.set(keep, 'kept'); ws.add(keep); e = null; 1"));
        assert_eq!(first, "1");
        let before = b.stats();
        assert!(before.wrappers >= 51, "wrappers {}", before.wrappers);
        b.collect(&[]).unwrap();
        let after = b.stats();
        assert!(after.wrappers < 20, "wrappers {}", after.wrappers);
        // the kept detached element is intact and its weak entries live
        assert_eq!(js(&mut b, "[keep.namespaceURI, keep instanceof SVGElement, keep.getAttribute('viewBox'), wm.get(keep), ws.has(keep)].join('|')"), format!("{SVG}|true|0 0 1 1|kept|true"));
        // new elements made after the collection do not inherit the entries of the dropped ones (no stale slot is reused)
        assert_eq!(js(&mut b, &format!("var hits = 0; for (var i = 0; i < 100; i++) {{ var n = document.createElementNS('{SVG}', 'g'); if (wm.has(n) || ws.has(n)) hits++; }} hits")), "0");
        // the native arena keeps its nodes and namespace identities (documented retention); a collection never frees them
        assert!(after.nodes >= before.nodes);
        // native identity is separate from the wrapper: attach, drop every reference, collect, read again
        js(&mut b, "document.body.appendChild(keep); keep = null; n = null;");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "var k = document.body.lastChild; [k.namespaceURI, k instanceof SVGElement, k.tagName, k.getAttribute('viewBox')].join('|')"), format!("{SVG}|true|svg|0 0 1 1"));
    });
}

#[test]
fn wrappers_and_listeners_cross_scripts() {
    big(|| {
        let mut b = browser();
        js(&mut b, &format!("var s = document.createElementNS('{SVG}', 'svg'); document.body.appendChild(s); window.seen = []; s.addEventListener('t', function (e) {{ seen.push(e.target.namespaceURI + ':' + e.target.tagName + ':' + (this === s)); }});"));
        // a second script in the same realm sees the same wrapper and the same namespace
        assert_eq!(js(&mut b, &format!("var t = document.body.lastChild; [t === s, t.namespaceURI === '{SVG}', t instanceof SVGElement].join()")), "true,true,true");
        js(&mut b, "t.dispatchEvent(new Event('t'));");
        assert_eq!(js(&mut b, "seen.join()"), format!("{SVG}:svg:true"));
    });
}

#[test]
fn native_caps_apply_to_namespaced_names_and_attributes() {
    big(|| {
        // names: the table holds the local name and the prefix; a refusal creates no node and no name
        let base = browser().document().borrow().name_count();
        let mut small = browser_with(PAGE, BrowserLimits { max_names: base + 2, ..Default::default() });
        let lim_names = format!("limit:{:?}", LimitKind::Host("dom-names"));
        assert_eq!(js(&mut small, &format!("document.createElementNS('{SVG}', 'p1:a1') && 1")), "1");
        let (nodes, names) = (small.stats().nodes, small.document().borrow().name_count());
        assert_eq!(js(&mut small, &format!("document.createElementNS('{SVG}', 'p2:a2')")), lim_names);
        assert_eq!(js(&mut small, &format!("document.createElementNS('{SVG}', 'a3')")), lim_names);
        assert_eq!((small.stats().nodes, small.document().borrow().name_count()), (nodes, names));
        // an existing name needs no room
        assert_eq!(js(&mut small, &format!("document.createElementNS('{SVG}', 'p1:a1') && 1")), "1");
        // a namespaced attribute needs a name of its own
        let base = browser().document().borrow().name_count();
        let mut small = browser_with(PAGE, BrowserLimits { max_names: base, ..Default::default() });
        assert_eq!(js(&mut small, &format!("document.body.setAttributeNS('{XLINK}', 'xlink:href', 'v')")), lim_names);
        assert_eq!(small.document().borrow().name_count(), base);
        // nodes
        let mut tiny = browser_with(PAGE, BrowserLimits { max_nodes: 8, ..Default::default() });
        let n = tiny.stats().nodes;
        assert_eq!(js(&mut tiny, &format!("var r = 0; for (var i = 0; i < 20; i++) {{ document.createElementNS('{SVG}', 'g'); r++ }} r")), format!("limit:{:?}", LimitKind::Host("dom-nodes")));
        assert_eq!(tiny.stats().nodes, 8);
        assert!(n <= 8);
        // attribute slots and text
        let slots = browser().stats().attr_slots;
        let mut few = browser_with(PAGE, BrowserLimits { max_attr_slots: slots + 4, ..Default::default() });
        assert_eq!(js(&mut few, &format!("var s = document.createElementNS('{SVG}', 'svg'); s.setAttributeNS('{XLINK}', 'xlink:href', 'a'); s.setAttributeNS('{XLINK}', 'xlink:href', 'b'); 1")), "1");
        let before = few.stats().attr_slots;
        let r = js(&mut few, "s.setAttributeNS(null, 'x', '1'); s.setAttributeNS(null, 'y', '2'); s.setAttributeNS(null, 'z', '3')");
        assert_eq!(r, format!("limit:{:?}", LimitKind::Host("dom-attrs")));
        assert!(few.stats().attr_slots <= before + 1);
        let mut text = browser_with(PAGE, BrowserLimits { max_text_bytes: 2000, ..Default::default() });
        assert_eq!(js(&mut text, &format!("var big = 'x'; for (var i = 0; i < 13; i++) big += big; document.body.setAttributeNS('{XLINK}', 'xlink:href', big)")), format!("limit:{:?}", LimitKind::Host("dom-text")));
        // a deep clone is counted before it is made (nodes)
        let mut clone = browser_with(PAGE, BrowserLimits { max_nodes: 30, ..Default::default() });
        js(&mut clone, &format!("var r = document.createElementNS('{SVG}', 'svg'); for (var i = 0; i < 10; i++) r.appendChild(document.createElementNS('{SVG}', 'g'));"));
        let n = clone.stats().nodes;
        assert_eq!(js(&mut clone, "var a = r.cloneNode(true); var b2 = r.cloneNode(true); 1"), format!("limit:{:?}", LimitKind::Host("dom-nodes")));
        assert!(clone.stats().nodes <= 30);
        assert!(clone.stats().nodes >= n + 11);
    });
}

#[test]
fn svg_scripts_and_links_are_not_html_resources() {
    use bare_url::Url;
    let doc = tree::parse("<body><svg><script src=a.js></script><link rel=stylesheet href=a.css><base href='http://evil/'></svg><script src=b.js></script>");
    let page = Url::parse("http://h/p").unwrap();
    let found = crate::resources::discover(&doc, &page, &Default::default()).unwrap();
    assert_eq!(found.items.len(), 1, "{:?}", found.items);
    assert_eq!(found.base, "http://h/p");
}

#[test]
fn html_only_style_and_focus_use_native_namespace_identity() {
    big(|| {
        let mut b = browser();
        let src = format!("var foreign = document.createElementNS('{SVG}', 'rect'); foreign.setAttribute('tabindex', '0'); document.body.appendChild(foreign); var styleGetter = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'style').get; var rejected = 0; try {{ styleGetter.call(foreign); }} catch(e) {{ if(e instanceof TypeError) rejected++; }} try {{ HTMLElement.prototype.focus.call(foreign); }} catch(e) {{ if(e instanceof TypeError) rejected++; }} try {{ HTMLElement.prototype.blur.call(foreign); }} catch(e) {{ if(e instanceof TypeError) rejected++; }} rejected");
        assert_eq!(js(&mut b, &src), "3");
        let foreign = b.run_script("foreign").unwrap();
        let node = b.node(foreign).unwrap();
        assert!(!b.focus(node).unwrap().focused);
        assert_eq!(js(&mut b, "var htmlSvg = document.createElement('svg'); typeof htmlSvg.style"), "object");
    });
}

#[test]
fn native_selection_does_not_accept_foreign_input_names() {
    big(|| {
        let mut b = browser();
        let v = b.run_script("document.createElementNS('http://www.w3.org/2000/svg','input')").unwrap();
        let n = b.node(v).unwrap();
        assert_eq!(b.input_selection(n), None);
    });
}
