//! Tests of the inert `HTMLIFrameElement` brand. Expectations are written by hand from the HTML Standard (interface list) and
//! the DOM Standard (clone, namespaces). Nothing here was compared with Chrome, and Node has no DOM. The `su` function is a
//! reduced copy, made by reading the cached staging bundle (chunk fd9d); the bundle is not run.

use crate::browser::*;
use crate::browser_fetch::{FetchConfig, FetchLimits};
use crate::browser_loader::*;
use crate::resources::test_server::{Reply, serve};
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;
use bare_net::http::Client;
use std::time::Duration;

const SVG: &str = "http://www.w3.org/2000/svg";
const MATH: &str = "http://www.w3.org/1998/Math/MathML";

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser(html: &str) -> Browser {
    Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    }
}

const PAGE: &str = "<!doctype html><body><iframe id=p src='/child.html'></iframe><iframe id=q>raw <b>text</b></iframe><div id=d></div><input id=i></body>";

#[test]
fn iframe_nodes_have_the_iframe_brand() {
    big(|| {
        let mut b = browser(PAGE);
        assert_eq!(js(&mut b, "typeof HTMLIFrameElement + ',' + (window.HTMLIFrameElement === HTMLIFrameElement)"), "function,true");
        // parsed, created (any case) and namespace-HTML created nodes
        js(&mut b, "var c = document.createElement('iFrame'), n = document.createElementNS('http://www.w3.org/1999/xhtml', 'iframe'), p = document.getElementById('p');");
        assert_eq!(js(&mut b, "[p, c, n].map(function (e) { return [e instanceof HTMLIFrameElement, e instanceof HTMLElement, e instanceof Element, e instanceof Node, e instanceof SVGElement].join('') }).join()"), "truetruetruetruefalse,truetruetruetruefalse,truetruetruetruefalse");
        assert_eq!(js(&mut b, "[Object.getPrototypeOf(HTMLIFrameElement.prototype) === HTMLElement.prototype, HTMLIFrameElement.prototype.constructor === HTMLIFrameElement, Object.getPrototypeOf(c) === HTMLIFrameElement.prototype].join()"), "true,true,true");
        assert_eq!(js(&mut b, "HTMLIFrameElement.name + ',' + HTMLIFrameElement.length"), "HTMLIFrameElement,0");
    });
}

#[test]
fn other_elements_and_foreign_namespaces_are_not_iframes() {
    big(|| {
        let mut b = browser(PAGE);
        js(&mut b, &format!("var d = document.getElementById('d'), i = document.getElementById('i'), s = document.createElementNS('{SVG}', 'iframe'), m = document.createElementNS('{MATH}', 'iframe'), t = document.createTextNode('x');"));
        assert_eq!(js(&mut b, "[d, i, s, m, t, document, window].map(function (e) { return e instanceof HTMLIFrameElement }).join()"), "false,false,false,false,false,false,false");
        assert_eq!(js(&mut b, "[s instanceof SVGElement, s instanceof HTMLElement, m instanceof HTMLElement, m instanceof Element].join()"), "true,false,false,true");
        assert_eq!(js(&mut b, "[d, i].map(function (e) { return Object.getPrototypeOf(e) === HTMLIFrameElement.prototype }).join()"), "false,false");
        assert_eq!(js(&mut b, "[document.createElement('input') instanceof HTMLInputElement, document.createElement('input') instanceof HTMLIFrameElement].join()"), "true,false");
        // parser: an iframe inside svg is an SVG element (not raw text, not an iframe)
        let mut b = browser("<body><svg><iframe id=f></iframe></svg>");
        assert_eq!(js(&mut b, "var f = document.getElementById('f'); [f.namespaceURI, f instanceof HTMLIFrameElement, f instanceof SVGElement].join()"), format!("{SVG},false,true"));
    });
}

#[test]
fn the_constructor_is_illegal_and_methods_are_branded() {
    big(|| {
        let mut b = browser(PAGE);
        assert_eq!(js(&mut b, "new HTMLIFrameElement()"), "throw:TypeError:Failed to construct 'HTMLIFrameElement': Illegal constructor");
        assert_eq!(js(&mut b, "HTMLIFrameElement()"), "throw:TypeError:Failed to construct 'HTMLIFrameElement': Please use the 'new' operator, this DOM object constructor cannot be called as a function.");
        // a borrowed HTMLElement method works on an iframe and still rejects an SVG receiver
        js(&mut b, &format!("var c = document.createElement('iframe'), s = document.createElementNS('{SVG}', 'iframe'); var focus = HTMLElement.prototype.focus;"));
        assert_eq!(js(&mut b, "focus.call(c); 'ok'"), "ok");
        assert_eq!(js(&mut b, "focus.call(s)"), "throw:TypeError:Illegal invocation");
        // there is no iframe-only method or property: an iframe does not borrow them from a input
        assert_eq!(js(&mut b, "var vget = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').get; try { vget.call(c); 'no' } catch (e) { e.constructor === TypeError ? 'TypeError' : 'other' }"), "TypeError");
    });
}

#[test]
fn clones_moves_and_collection_keep_the_brand() {
    big(|| {
        let mut b = browser(PAGE);
        js(&mut b, "var p = document.getElementById('p'); window.w1 = p;");
        assert_eq!(js(&mut b, "var c = p.cloneNode(true); [c !== p, c instanceof HTMLIFrameElement, c.getAttribute('src'), c.isConnected].join()"), "true,true,/child.html,false");
        assert_eq!(js(&mut b, "var sh = p.cloneNode(false); sh instanceof HTMLIFrameElement"), "true");
        // move to another parent and back
        js(&mut b, "document.getElementById('d').appendChild(p); document.body.appendChild(p);");
        assert_eq!(js(&mut b, "[p instanceof HTMLIFrameElement, p.parentNode === document.body, document.getElementById('p') === p].join()"), "true,true,true");
        // a second script sees the same wrapper
        assert_eq!(js(&mut b, "document.getElementById('p') === w1 && w1 instanceof HTMLIFrameElement"), "true");
        // drop every reference of the iframes that were only wrapped: the wrapper is made again with the brand after a collection
        js(&mut b, "c = sh = null; document.getElementById('q'); ");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "var q = document.getElementById('q'); [q instanceof HTMLIFrameElement, q.tagName, q.localName, q.textContent].join()"), "true,IFRAME,iframe,raw <b>text</b>");
        // a detached created iframe that lost all its references comes back from native identity only if attached: attach, drop, collect, read
        js(&mut b, "document.body.appendChild(document.createElement('iframe'));");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "document.body.lastChild instanceof HTMLIFrameElement"), "true");
        // the reduced iframe element in a document built by the parser has no children element
        assert_eq!(js(&mut b, "q.children.length + ',' + q.childNodes.length"), "0,1");
    });
}

#[test]
fn nested_browsing_contexts_are_inert() {
    big(|| {
        let mut b = browser(PAGE);
        // The named window is inert. Child documents and navigation remain absent.
        assert_eq!(js(&mut b, "var f = document.getElementById('p'); [typeof f.contentWindow, typeof f.contentDocument, typeof f.src, typeof f.srcdoc, typeof f.sandbox, typeof f.loading, typeof f.getSVGDocument, 'contentWindow' in HTMLIFrameElement.prototype].join()"), "object,object,string,string,undefined,string,undefined,true");
        assert_eq!(js(&mut b, "Object.getOwnPropertyNames(HTMLIFrameElement.prototype).join()"), "constructor,contentWindow,contentDocument,name,src,srcdoc,allow,width,height,loading,referrerPolicy");
        assert_eq!(js(&mut b, "typeof window.frames + ',' + typeof window.parent + ',' + typeof window.frameElement"), "object,object,object");
        // attributes stay plain attributes: no reflection, no side effect
        assert_eq!(js(&mut b, "f.setAttribute('src', '/other.html'); f.setAttribute('srcdoc', '<p>x</p>'); [f.getAttribute('src'), f.getAttribute('srcdoc'), f.hasAttribute('name')].join()"), "/other.html,<p>x</p>,false");
    });
}

#[test]
fn no_child_document_is_ever_fetched() {
    big(|| {
        let srv = serve(|_| Reply::new(404, Some("text/plain"), "no"));
        let html = "<head><script>var log = []</script></head><body><iframe id=p src='/child.html'></iframe><script>\
            var a = document.createElement('iframe'); a.setAttribute('src', '/dyn.html'); document.body.appendChild(a); a.src2 = 1;\
            var b2 = document.createElement('iframe'); b2.setAttribute('src', '/dyn2.html'); b2.setAttribute('srcdoc', '<script src=/inner.js><\\/script>'); document.body.appendChild(b2);\
            a.setAttribute('src', '/dyn3.html'); a.cloneNode(true); window.ok = a instanceof HTMLIFrameElement;</script></body>";
        let mut b = browser(html);
        b.configure_fetch(FetchConfig { base_url: Some(srv.url("/page.html")), client: Client::new(), limits: FetchLimits::default() }).unwrap();
        let r = b.run_page(PageConfig { url: srv.url("/page.html"), client: Client::new(), limits: PageLimits { quiet: Duration::from_millis(100), ..Default::default() }, fetch: None });
        assert_eq!(js(&mut b, "String(window.ok)"), "true", );
        assert_eq!((r.script_throws, r.scripts_run), (0, 2));
        for p in ["/child.html", "/dyn.html", "/dyn2.html", "/dyn3.html", "/inner.js"] {
            assert_eq!(srv.hits(p), 0, "{p} was fetched");
        }
        assert_eq!(r.load_errors, 0);
    });
}

#[test]
fn reduced_react_su_walks_focus_without_a_nested_context() {
    big(|| {
        // `su` and `e6` (activeElement of a document), as in chunk fd9d at byte 135303; e6 is simplified to document.activeElement.
        let mut b = browser(PAGE);
        js(&mut b, "function e6(d) { d = d || document; try { return d.activeElement || d.body } catch (e) { return d.body } }
            function su() { for (var e = window, t = e6(); t instanceof e.HTMLIFrameElement;) { try { var n = 'string' == typeof t.contentWindow.location.href } catch (e) { n = !1 } if (n) e = t.contentWindow; else break; t = e6(e.document) } return t }");
        // body focused: the loop does not start (before the fix `window.HTMLIFrameElement` was undefined and this threw)
        assert_eq!(js(&mut b, "su() === document.body"), "true");
        // An iframe is active: its inert window has no location, so the walk ends at the iframe.
        js(&mut b, "var f = document.getElementById('p'); f.setAttribute('tabindex', '0'); f.focus();");
        assert_eq!(js(&mut b, "var r = su(); [r instanceof HTMLIFrameElement, r === document.activeElement].join()"), "true,true");
        // a trivial createRoot-like render in the same realm no longer throws on the instanceof check
        assert_eq!(js(&mut b, "var out; try { out = (document.getElementById('d') instanceof window.HTMLIFrameElement) ? 'y' : 'n' } catch (e) { out = 'throw:' + e.message } out"), "n");
    });
}

#[test]
fn named_iframes_follow_the_document() {
    big(|| {
        let mut b = browser("<body><iframe name='__tcfapiLocator' id='locator'></iframe></body>");
        assert_eq!(js(&mut b, "var f = document.getElementById('locator'); [typeof f.contentWindow === 'object', frames === window, frames.length, window.__tcfapiLocator === f.contentWindow, frames[0] === f.contentWindow, window.locator === f.contentWindow, document.locator === f, document.getElementsByName('__tcfapiLocator')[0] === f].join()"), "true,true,1,true,true,true,true,true");
        assert_eq!(js(&mut b, "f.name = 'other'; typeof window.__tcfapiLocator + ',' + (frames.other === f.contentWindow)"), "undefined,true");
        assert_eq!(js(&mut b, "f.remove(); [frames.length, typeof frames.other, typeof window.locator].join()"), "0,undefined,undefined");
        assert_eq!(js(&mut b, "var g = document.createElement('iframe'); g.name = 'newFrame'; document.body.appendChild(g); frames.newFrame === g.contentWindow && frames[0] === g.contentWindow"), "true");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "frames.newFrame === g.contentWindow && g.contentWindow.frameElement === g"), "true");
        assert_eq!(js(&mut b, "var h = document.createElement('iframe'); h.name = 'newFrame'; document.body.insertBefore(h, g); frames[0] === h.contentWindow && frames.newFrame === h.contentWindow"), "true");
        assert_eq!(js(&mut b, "h.remove(); frames.newFrame === g.contentWindow && h.contentWindow === null"), "true");
    });
}

#[test]
fn frame_lookup_in_large_documents_retries_host_reads() {
    big(|| {
        let mut b = browser(&format!("<body>{}<iframe name='child' id='f'></iframe></body>", "<div></div>".repeat(50)));
        assert_eq!(js(&mut b, "var c = frames.child; [c === frames[0], c === document.getElementById('f').contentWindow, typeof window.missing].join()"), "true,true,undefined");
    });
}

#[test]
fn blank_frame_window_is_an_event_target_with_a_document() {
    big(|| {
        let mut b = browser("<!doctype html><body><p>x</p></body>");
        let r = js(&mut b, "var f = document.createElement('iframe'); f.style.cssText = 'position:absolute;width:9em;height:9em;top:-99em';
            document.body.insertBefore(f, document.body.firstChild);
            var w = f.contentWindow; w.document.open(); w.document.close();
            var hits = 0, fn = function (e) { hits += 1; };
            w.addEventListener('resize', fn, false);
            w.dispatchEvent(new CustomEvent('resize'));
            var once = hits;
            w.removeEventListener('resize', fn, false);
            w.dispatchEvent(new CustomEvent('resize'));
            [once, hits, typeof w.addEventListener, f.contentDocument === w.document, f.contentDocument === f.contentDocument,
             !!f.contentDocument.body, !!f.contentDocument.head, !!f.contentDocument.documentElement, f.contentDocument !== document,
             w.document.write('<b id=q>hi</b>') === undefined, f.contentDocument.getElementById('q').textContent, document.getElementById('q')].join()");
        assert_eq!(r, "1,1,function,true,true,true,true,true,true,true,hi,");
    });
}

#[test]
fn frame_with_another_src_has_a_window_but_no_document() {
    big(|| {
        let mut b = browser("<!doctype html><body><iframe id=a src='/x.html'></iframe><iframe id=c src='about:blank'></iframe><iframe id=d srcdoc='<p>'></iframe></body>");
        assert_eq!(js(&mut b, "var e = (id) => document.getElementById(id); typeof e('a').contentWindow.addEventListener + ',' + e('a').contentDocument + ',' + (e('c').contentDocument !== null) + ',' + e('d').contentDocument"), "function,null,true,null");
        assert_eq!(js(&mut b, "var g = document.createElement('iframe'); g.contentDocument + ',' + g.contentWindow"), "null,null");
    });
}

#[test]
fn blank_frame_document_survives_a_collection() {
    big(|| {
        let mut b = browser("<!doctype html><body></body>");
        js(&mut b, "var f = document.body.appendChild(document.createElement('iframe')); f.contentDocument.x = 7; f.contentWindow.y = 8; f = null;");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "var g = document.body.lastChild; g.contentDocument.x + ',' + g.contentWindow.y + ',' + (g.contentDocument === g.contentWindow.document)"), "7,8,true");
    });
}

#[test]
fn blank_frame_document_knows_its_window_and_owns_its_nodes() {
    big(|| {
        let mut b = browser("<!doctype html><body><p>x</p></body>");
        b.set_document_url("https://example.test/mail").expect("url");
        js(&mut b, "var f = document.createElement('iframe'); document.body.appendChild(f); var d = f.contentDocument, w = f.contentWindow;");
        // defaultView
        assert_eq!(js(&mut b, "[d.defaultView === w, document.defaultView === window, document.implementation.createHTMLDocument('').defaultView].join()"), "true,true,");
        // ownerDocument
        assert_eq!(js(&mut b, "[d.createElement('div').ownerDocument === d, d.body.ownerDocument === d, d.createTextNode('t').ownerDocument === d, d.createComment('c').ownerDocument === d, document.createElement('p').ownerDocument === document, d.ownerDocument].join()"), "true,true,true,true,true,");
        assert_eq!(js(&mut b, "var e = d.createElement('div'); d.body.appendChild(e); e.ownerDocument === d && d.body.firstChild === e"), "true");
        // activeElement
        assert_eq!(js(&mut b, "d.activeElement === d.body"), "true");
        // URL and location
        assert_eq!(js(&mut b, "[d.URL, d.documentURI, typeof w.location, w.location.href, w.location.protocol, w.location.toString(), w.location instanceof Location, document.URL !== 'about:blank'].join()"), "about:blank,about:blank,object,about:blank,about:,about:blank,true,true");
        // globals
        assert_eq!(js(&mut b, "[w.Object === Object, w.Array === Array, w.Function === Function, w.Element === Element, typeof w.setTimeout, typeof w.clearTimeout, typeof w.getComputedStyle, typeof w.postMessage, typeof w.focus, typeof w.navigator, typeof w.history, typeof w.localStorage, typeof w.innerWidth, typeof w.getSelection].join()"), "true,true,true,true,function,function,function,function,function,object,object,object,number,function");
        assert_eq!(js(&mut b, "var t = 0; w.setTimeout(function () { t = 1; }, 0); typeof w.getComputedStyle(d.body) + ',' + (w.document === d) + ',' + (w.window === w) + ',' + (w.parent === window)"), "object,true,true,true");
    });
}
