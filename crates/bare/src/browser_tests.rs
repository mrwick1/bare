//! Tests of the DOM/JS bridge. Real scripts run in a real realm over a real `Document`. Expectations are written by
//! hand from the DOM Standard (structural fixtures); nothing here was compared with Chrome. Each test runs on a big stack.

use crate::browser::*;
use bare_html::dom::NodeData;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const PAGE: &str = "<!doctype html><html><head><title>t</title></head><body><div id=a class='x y'><p>hi</p><!--c--><span id=s>one</span></div><input id=i value=v><form id=f></form></body></html>";

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

fn dump(b: &Browser) -> String {
    b.document().borrow().dump()
}

fn lim(name: &'static str) -> String {
    format!("limit:{:?}", LimitKind::Host(name))
}

#[test]
fn globals_and_identity() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "[window === self, window === globalThis, window.document === document, typeof document.createElement].join()"), "true,true,true,function");
        assert_eq!(js(&mut b, "document.body === document.body && document.body.parentNode === document.documentElement && document.documentElement.parentNode === document"), "true");
        assert_eq!(js(&mut b, "var a = document.getElementById('a'); a === document.querySelector('#a') && a === document.body.firstElementChild"), "true");
        assert_eq!(js(&mut b, "[a instanceof Node, a instanceof Element, a instanceof HTMLElement, a instanceof EventTarget, a instanceof Text, document instanceof Document, window instanceof EventTarget].join()"), "true,true,true,true,false,true,true");
        assert_eq!(js(&mut b, "[document.nodeType, a.nodeType, a.firstChild.nodeType, a.childNodes[1].firstChild === null, a.childNodes[1].nodeType].join()"), "9,1,1,true,8");
        assert_eq!(js(&mut b, "[a.nodeName, a.tagName, a.localName, a.firstChild.firstChild.nodeName, document.nodeName].join()"), "DIV,DIV,div,#text,#document");
        assert_eq!(js(&mut b, "[Node.ELEMENT_NODE, Node.TEXT_NODE, Event.AT_TARGET].join()"), "1,3,2");
        assert_eq!(js(&mut b, "new Node()"), "throw:TypeError:Node is not a constructor");
    });
}

#[test]
fn navigation_matches_the_native_tree() {
    big(|| {
        let mut b = browser();
        assert_eq!(
            js(&mut b, "var a = document.getElementById('a'); [a.parentNode.nodeName, a.previousSibling, a.nextSibling.id, a.childNodes.length, a.children.length, a.childElementCount, a.firstChild.nextSibling.nodeType, a.lastChild.id, a.lastElementChild.id, a.firstElementChild.nextElementSibling.id, a.firstElementChild.previousElementSibling].join()"),
            "BODY,,i,3,2,2,8,s,s,s,"
        );
        assert_eq!(js(&mut b, "[document.head.nodeName, document.body.nodeName, document.doctype.nodeName, document.documentElement.tagName, a.ownerDocument === document, document.ownerDocument, a.isConnected, a.parentElement.tagName].join()"), "HEAD,BODY,html,HTML,true,,true,BODY");
        assert_eq!(js(&mut b, "document.getElementById('s').textContent + '|' + a.textContent + '|' + a.childNodes[1].data + '|' + a.childNodes[1].nodeValue + '|' + document.textContent"), "one|hione|c|c|null");
        assert_eq!(js(&mut b, "[a.hasChildNodes(), document.getElementById('s').firstChild.length, a.contains(document.getElementById('s')), a.contains(a), a.contains(document.body), a.contains(null)].join()"), "true,3,true,true,false,false");
    });
}

#[test]
fn mutations_change_the_native_tree() {
    big(|| {
        let mut b = browser_with("<body><div id=a><p>1</p></div></body>", BrowserLimits::default());
        let want = |b: &Browser, tail: &str| assert_eq!(dump(b), format!("| <html>\n|   <head>\n|   <body>\n|     <div>\n|       id=\"a\"\n{tail}"));
        js(&mut b, "var a = document.getElementById('a'); var e = document.createElement('B'); e.textContent = 'x'; a.appendChild(e);");
        want(&b, "|       <p>\n|         \"1\"\n|       <b>\n|         \"x\"\n");
        assert_eq!(js(&mut b, "a.insertBefore(document.createTextNode('t'), a.firstChild); a.childNodes.length"), "3");
        want(&b, "|       \"t\"\n|       <p>\n|         \"1\"\n|       <b>\n|         \"x\"\n");
        // moving a node moves it, it does not copy it
        js(&mut b, "a.appendChild(a.firstChild)");
        want(&b, "|       <p>\n|         \"1\"\n|       <b>\n|         \"x\"\n|       \"t\"\n");
        assert_eq!(js(&mut b, "a.removeChild(a.lastChild).data + a.childNodes.length"), "t2");
        js(&mut b, "var q = document.createElement('q'); a.replaceChild(q, a.firstChild); a.insertBefore(q, q)");
        want(&b, "|       <q>\n|       <b>\n|         \"x\"\n");
        assert_eq!(js(&mut b, "a.firstChild.remove(); a.childNodes.length + ',' + q.parentNode"), "1,null");
        js(&mut b, "a.append('s1', document.createElement('i'), 's2'); a.prepend('p1', 'p2')");
        want(&b, "|       \"p1\"\n|       \"p2\"\n|       <b>\n|         \"x\"\n|       \"s1\"\n|       <i>\n|       \"s2\"\n");
        // textContent replaces all children with one text node
        js(&mut b, "a.textContent = 'only'");
        want(&b, "|       \"only\"\n");
        js(&mut b, "a.textContent = ''");
        want(&b, "");
        // identity survives a move; a removed node stays usable
        assert_eq!(js(&mut b, "var w = document.createElement('w'); a.appendChild(w); var k = w.parentNode === a; a.removeChild(w); k + ',' + w.parentNode + ',' + (a.firstChild === null)"), "true,null,true");
    });
}

#[test]
fn clone_node_copies_attributes_text_and_input_state() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var a = document.getElementById('a'); var c = a.cloneNode(true); var s = a.cloneNode(false);");
        assert_eq!(js(&mut b, "[c === a, c.id, c.className, c.childNodes.length, c.parentNode, s.childNodes.length, s.id, c.firstChild.firstChild.data, c.textContent].join()"), "false,a,x y,3,,0,a,hi,hione");
        // independent: changing the clone leaves the original
        js(&mut b, "c.setAttribute('id', 'z'); c.firstChild.firstChild.data = 'changed'");
        assert_eq!(js(&mut b, "a.id + ',' + a.textContent"), "a,hione");
        // dirty input value and checkedness are copied by cloneNode
        js(&mut b, "var i = document.getElementById('i'); i.value = 'typed'; i.checked = true; var ic = i.cloneNode(false);");
        assert_eq!(js(&mut b, "[ic.value, ic.checked, ic.defaultValue, i.getAttribute('value')].join()"), "typed,true,v,v");
        assert_eq!(js(&mut b, "document.cloneNode(true)"), "throw:NotSupportedError:Cloning a Document is not supported.");
    });
}

#[test]
fn attributes_and_selector_caches() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var a = document.getElementById('a'); [a.getAttribute('class'), a.getAttribute('ID'), a.getAttribute('nope'), a.hasAttribute('id'), a.getAttributeNames().join('+')].join()"), "x y,a,,true,id+class");
        js(&mut b, "a.setAttribute('Data-K', 'v1'); a.setAttribute('data-k', 'v2'); a.removeAttribute('id');");
        assert_eq!(js(&mut b, "[a.getAttribute('data-k'), a.id === '', a.getAttributeNames().join('+'), document.getElementById('a')].join()"), "v2,true,class+data-k,");
        // a selector sees the new class at once (cached class sets and sibling tables are invalidated)
        assert_eq!(js(&mut b, "a.className = 'q'; [document.querySelectorAll('.x').length, document.querySelectorAll('.q').length, a.matches('div.q'), a.matches('.x')].join()"), "0,1,true,false");
        let long = format!("{} target", "filler ".repeat(30));
        js(&mut b, &format!("a.className = '{long}'"));
        assert_eq!(js(&mut b, "document.querySelectorAll('.target').length"), "1");
        js(&mut b, "a.className = 'other'");
        assert_eq!(js(&mut b, "document.querySelectorAll('.target').length + ',' + document.querySelectorAll('.other').length"), "0,1");
        assert_eq!(js(&mut b, "a.setAttribute('a b', '1')"), "throw:InvalidCharacterError:'a b' is not a valid attribute name.");
        assert_eq!(js(&mut b, "document.createElement('1x')"), "throw:InvalidCharacterError:The tag name provided ('1x') is not a valid name.");
        // :nth-child after a structural change
        assert_eq!(js(&mut b, "var p = a.firstElementChild; var n = document.createElement('em'); a.insertBefore(n, p); [p.matches(':nth-child(2)'), n.matches(':first-child'), document.querySelectorAll('div > :nth-child(1)').length].join()"), "true,true,1");
    });
}

#[test]
fn selectors_lookups_and_errors() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "[document.querySelectorAll('div *').length, document.querySelector('p + span') === null, document.querySelector('p ~ span').id, document.getElementById('s').closest('div').id, document.getElementById('s').closest('section'), document.querySelector('#nope'), document.getElementById('')].join()"), "2,false,s,a,,,");
        assert_eq!(js(&mut b, "document.getElementById('a').querySelector('#a')"), "null"); // only descendants
        assert_eq!(js(&mut b, "document.querySelector('p[')"), "throw:SyntaxError:'p[' is not a valid selector");
        assert_eq!(js(&mut b, "document.querySelector('a b c d')"), lim("dom-selector-complexity"));
        assert_eq!(js(&mut b, &format!("document.querySelector('{}')", "a".repeat(2000))), lim("dom-selector"));
        // a detached subtree is searched too, and is not found from the document
        assert_eq!(js(&mut b, "var d = document.createElement('div'); d.innerText2 = 1; d.appendChild(document.createElement('b')); [d.querySelector('b').tagName, document.querySelector('b'), d.isConnected].join()"), "B,,false");
        // the cause of the error is a DOMException-like object that inherits from Error
        assert_eq!(js(&mut b, "try { document.querySelector('['); } catch (e) { [e instanceof Error, e.name, e.code, String(e)].join() }"), "true,SyntaxError,12,SyntaxError: '[' is not a valid selector");
    });
}

#[test]
fn hierarchy_errors_and_descriptors() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var a = document.getElementById('a'); var s = document.getElementById('s'); var p = a.firstElementChild;");
        assert_eq!(js(&mut b, "s.appendChild(a)"), "throw:HierarchyRequestError:The new child element contains the parent.");
        assert_eq!(js(&mut b, "a.appendChild(a)"), "throw:HierarchyRequestError:The new child element contains the parent.");
        assert_eq!(js(&mut b, "p.insertBefore(document.createElement('x'), s)"), "throw:NotFoundError:The node before which the new node is to be inserted is not a child of this node.");
        assert_eq!(js(&mut b, "a.removeChild(document.body)"), "throw:NotFoundError:The node to be removed is not a child of this node.");
        assert_eq!(js(&mut b, "a.appendChild({})"), "throw:TypeError:parameter 1 is not of type 'Node'");
        assert_eq!(js(&mut b, "document.appendChild(document.createElement('html'))"), "throw:HierarchyRequestError:This document cannot take that child here.");
        assert_eq!(js(&mut b, "document.appendChild(document.createTextNode('x'))"), "throw:HierarchyRequestError:This node type cannot be a child of this node.");
        assert_eq!(js(&mut b, "a.appendChild(document)"), "throw:HierarchyRequestError:A Document cannot be inserted.");
        assert_eq!(js(&mut b, "document.createTextNode('t').appendChild(document.createElement('x'))"), "throw:HierarchyRequestError:This node type does not support this method.");
        // nothing changed after the failures
        assert_eq!(js(&mut b, "a.childNodes.length + ',' + document.childNodes.length"), "3,2");
        // accessor properties: a getter with the wrong `this` is a TypeError, and the flags are as in the IDL
        assert_eq!(js(&mut b, "Object.getOwnPropertyDescriptor(Element.prototype, 'id').get.call({})"), "throw:TypeError:Illegal invocation");
        assert_eq!(js(&mut b, "var d = Object.getOwnPropertyDescriptor(Node.prototype, 'nodeType'); [typeof d.get, d.set, d.enumerable, d.configurable].join()"), "function,,true,true");
        assert_eq!(js(&mut b, "Element.prototype.getAttribute.call(document.createTextNode('x'), 'a')"), "throw:TypeError:Illegal invocation");
        assert_eq!(js(&mut b, "Object.defineProperty(Node, 'ELEMENT_NODE', { value: 7 })"), "throw:TypeError:Cannot redefine property: ELEMENT_NODE");
        // a read-only attribute: assignment is ignored in sloppy mode
        assert_eq!(js(&mut b, "a.nodeType = 5; a.nodeType"), "1");
        assert_eq!(js(&mut b, "Document()"), "throw:TypeError:Illegal constructor");
        assert_eq!(js(&mut b, "Event('x')"), "throw:TypeError:Failed to construct: please use the 'new' operator");
    });
}

#[test]
fn utf16_round_trips_and_lone_surrogates_are_gated() {
    big(|| {
        let mut b = browser();
        // a pair becomes one UTF-8 character and comes back as the same two code units
        assert_eq!(js(&mut b, "var t = document.createTextNode('a\\ud83d\\ude00b'); [t.length, t.data.length, t.data.charCodeAt(1) === 55357, t.data === 'a\\ud83d\\ude00b'].join()"), "4,4,true,true");
        let doc = b.document();
        let text = doc.borrow().dump();
        assert!(!text.contains('\u{fffd}'));
        js(&mut b, "document.body.appendChild(t)");
        assert!(b.document().borrow().dump().contains("\"a\u{1F600}b\""));
        assert_eq!(js(&mut b, "document.body.lastChild.data === 'a\\ud83d\\ude00b'"), "true");
        // a lone surrogate would be changed to U+FFFD by UTF-8: it is refused, not changed
        assert_eq!(js(&mut b, "document.createTextNode('\\ud83d')"), "unsupported:lone surrogate in a DOM string (the native document is UTF-8)");
        assert_eq!(js(&mut b, "document.body.setAttribute('x', 'a\\ude00')"), "unsupported:lone surrogate in a DOM string (the native document is UTF-8)");
        assert_eq!(js(&mut b, "document.body.textContent = '\\ud800'"), "unsupported:lone surrogate in a DOM string (the native document is UTF-8)");
        // the failed set changed nothing
        assert!(b.document().borrow().dump().contains("\"a\u{1F600}b\""));
    });
}

#[test]
fn create_element_ns_is_gated() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "document.createElementNS('http://www.w3.org/1999/xhtml', 'section').tagName"), "SECTION");
        assert_eq!(js(&mut b, "document.createElementNS('http://www.w3.org/2000/svg', 'svg')"), "unsupported:createElementNS outside the HTML namespace (the native document has no namespaces)");
        assert_eq!(js(&mut b, "document.createElementNS(null, 'x')"), "unsupported:createElementNS outside the HTML namespace (the native document has no namespaces)");
        assert_eq!(js(&mut b, "document.createElementNS('http://www.w3.org/1999/xhtml', 'x:y')"), "unsupported:createElementNS with a prefix or capital letters (names are lowercase in the native document)");
        assert_eq!(js(&mut b, "[typeof document.createDocumentFragment, typeof document.body.innerHTML, typeof document.body.classList, typeof document.body.namespaceURI, typeof document.getElementsByTagName].join()"), "undefined,undefined,object,undefined,function");
    });
}

fn log_setup(b: &mut Browser) {
    js(b, "var log = []; function L(tag) { return function (e) { log.push(tag + ':' + e.eventPhase + ':' + (e.currentTarget === window ? 'win' : e.currentTarget.nodeName)); } }");
}

#[test]
fn event_path_capture_target_bubble_and_window() {
    big(|| {
        let mut b = browser();
        log_setup(&mut b);
        js(&mut b, "var a = document.getElementById('a'); var s = document.getElementById('s');
            window.addEventListener('t', L('wc'), true); window.addEventListener('t', L('wb'));
            document.addEventListener('t', L('dc'), true); document.addEventListener('t', L('db'));
            a.addEventListener('t', L('ac'), true); a.addEventListener('t', L('ab'));
            s.addEventListener('t', L('sb')); s.addEventListener('t', L('sc'), true);");
        assert_eq!(js(&mut b, "log = []; s.dispatchEvent(new Event('t', { bubbles: true })); log.join(' ')"), "wc:1:win dc:1:#document ac:1:DIV sc:2:SPAN sb:2:SPAN ab:3:DIV db:3:#document wb:3:win");
        // no bubbling: capture and target only
        assert_eq!(js(&mut b, "log = []; s.dispatchEvent(new Event('t')); log.join(' ')"), "wc:1:win dc:1:#document ac:1:DIV sc:2:SPAN sb:2:SPAN");
        // a detached node has no document and no window in its path
        assert_eq!(js(&mut b, "log = []; var d = document.createElement('div'); d.addEventListener('t', L('d')); d.dispatchEvent(new Event('t', { bubbles: true })); log.join(' ')"), "d:2:DIV");
        // `load` does not reach window from the document
        assert_eq!(js(&mut b, "log = []; document.addEventListener('load', L('dl')); window.addEventListener('load', L('wl')); document.dispatchEvent(new Event('load', { bubbles: true })); log.join(' ')"), "dl:2:#document");
        // the event object after dispatch
        assert_eq!(js(&mut b, "var ev = new Event('t', { bubbles: true }); s.dispatchEvent(ev); [ev.target === s, ev.currentTarget, ev.eventPhase, ev.isTrusted, ev.bubbles, ev.cancelable, ev.defaultPrevented, ev.type].join()"), "true,,0,false,true,false,false,t");
        assert_eq!(js(&mut b, "var path; s.addEventListener('p', function (e) { path = e.composedPath().length }); s.dispatchEvent(new Event('p')); path + ',' + new Event('p').composedPath().length"), "6,0");
    });
}

#[test]
fn listener_options_removal_and_mutation_during_dispatch() {
    big(|| {
        let mut b = browser();
        log_setup(&mut b);
        js(&mut b, "var s = document.getElementById('s'); function f1() { log.push('f1') } function f2() { log.push('f2') } function f3() { log.push('f3') }");
        // once: runs one time. a duplicate is ignored. remove needs the same capture flag
        assert_eq!(js(&mut b, "s.addEventListener('x', f1, { once: true }); s.addEventListener('x', f2); s.addEventListener('x', f2); s.addEventListener('x', f3, true); s.removeEventListener('x', f3); s.dispatchEvent(new Event('x')); s.dispatchEvent(new Event('x')); log.join()"), "f3,f1,f2,f3,f2");
        // removal during dispatch: a removed listener that has not run is skipped. an added one waits for the next dispatch
        js(&mut b, "log = []; var t = new EventTarget(); function r1() { log.push('r1'); t.removeEventListener('y', r2); t.addEventListener('y', r3) } function r2() { log.push('r2') } function r3() { log.push('r3') } t.addEventListener('y', r1); t.addEventListener('y', r2);");
        assert_eq!(js(&mut b, "t.dispatchEvent(new Event('y')); log.join()"), "r1");
        assert_eq!(js(&mut b, "t.dispatchEvent(new Event('y')); log.join()"), "r1,r1,r3");
        // handleEvent objects get themselves as `this`
        assert_eq!(js(&mut b, "log = []; var o = { n: 'obj', handleEvent: function (e) { log.push(this.n + e.type) } }; t.addEventListener('h', o); t.dispatchEvent(new Event('h')); log.join()"), "objh");
        assert_eq!(js(&mut b, "t.addEventListener('bad', {}); t.dispatchEvent(new Event('bad'))"), "true");
        let r = b.take_reports();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].message, "TypeError: The event listener's handleEvent is not a function");
        assert_eq!(js(&mut b, "t.addEventListener('x', 5)"), "throw:TypeError:Failed to execute 'addEventListener': parameter 2 is not of type 'Object'.");
        assert_eq!(js(&mut b, "t.addEventListener('x', null)"), "undefined");
        assert_eq!(js(&mut b, "t.addEventListener('x', f1, { signal: {} })"), "unsupported:addEventListener option signal");
        // a bare call uses window, as in a browser
        assert_eq!(js(&mut b, "var hit = 0; addEventListener('bare', function () { hit++ }); dispatchEvent(new Event('bare')); hit"), "1");
    });
}

#[test]
fn stop_propagation_prevent_default_and_passive() {
    big(|| {
        let mut b = browser();
        log_setup(&mut b);
        js(&mut b, "var a = document.getElementById('a'); var s = document.getElementById('s');
            s.addEventListener('t', function (e) { log.push('s1'); e.stopImmediatePropagation() }); s.addEventListener('t', L('s2')); a.addEventListener('t', L('a'));");
        assert_eq!(js(&mut b, "s.dispatchEvent(new Event('t', { bubbles: true })); log.join()"), "s1");
        js(&mut b, "log = []; var s3 = document.createElement('i'); a.appendChild(s3); s3.addEventListener('u', function (e) { e.stopPropagation(); log.push('s3a') }); s3.addEventListener('u', function () { log.push('s3b') }); a.addEventListener('u', L('a2'));");
        // stopPropagation lets the rest of this node run, but not the next node
        assert_eq!(js(&mut b, "s3.dispatchEvent(new Event('u', { bubbles: true })); log.join()"), "s3a,s3b");
        // a capture listener that stops: the target never runs
        assert_eq!(js(&mut b, "log = []; a.addEventListener('v', function (e) { e.stopPropagation(); log.push('cap') }, true); s.addEventListener('v', L('tgt')); s.dispatchEvent(new Event('v', { bubbles: true })); log.join()"), "cap");
        // preventDefault: only when cancelable, and not in a passive listener
        js(&mut b, "s.addEventListener('c', function (e) { e.preventDefault() });");
        assert_eq!(js(&mut b, "var e1 = new Event('c', { cancelable: true }); var r1 = s.dispatchEvent(e1); var e2 = new Event('c'); var r2 = s.dispatchEvent(e2); [r1, e1.defaultPrevented, r2, e2.defaultPrevented].join()"), "false,true,true,false");
        js(&mut b, "s.addEventListener('pp', function (e) { e.preventDefault() }, { passive: true });");
        assert_eq!(js(&mut b, "var e3 = new Event('pp', { cancelable: true }); [s.dispatchEvent(e3), e3.defaultPrevented].join()"), "true,false");
        // the event can be dispatched again after it finished; defaultPrevented stays
        assert_eq!(js(&mut b, "[s.dispatchEvent(e1), e1.defaultPrevented].join()"), "false,true");
        // CustomEvent keeps its detail
        assert_eq!(js(&mut b, "var seen; s.addEventListener('cu', function (e) { seen = e.detail.k }); s.dispatchEvent(new CustomEvent('cu', { detail: { k: 42 } })); [seen, new CustomEvent('z').detail, new CustomEvent('z') instanceof Event, new Event('z') instanceof CustomEvent].join()"), "42,,true,false");
    });
}

#[test]
fn listener_exceptions_are_reported_and_dispatch_continues() {
    big(|| {
        let mut b = browser();
        log_setup(&mut b);
        js(&mut b, "var s = document.getElementById('s'); s.addEventListener('t', function () { throw new TypeError('boom') }); s.addEventListener('t', function () { throw 'plain' }); s.addEventListener('t', L('after')); document.addEventListener('t', L('doc'));");
        assert_eq!(js(&mut b, "s.dispatchEvent(new Event('t', { bubbles: true })); log.join()"), "after:2:SPAN,doc:3:#document");
        let r = b.take_reports();
        assert_eq!(r.iter().map(|r| format!("{}|{}|{}", r.event_type, r.target, r.message)).collect::<Vec<_>>(), ["t|span|TypeError: boom", "t|span|plain"]);
        assert!(b.take_reports().is_empty());
        // a Limit stops dispatch: later listeners do not run, and the error is the result
        js(&mut b, "log = []; s.addEventListener('lim', function () { for (;;) {} }); s.addEventListener('lim', L('never'));");
        assert_eq!(js(&mut b, "s.dispatchEvent(new Event('lim'))"), "limit:Steps");
        assert_eq!(js(&mut b, "log.join()"), "");
        // the realm still works, and the event of the failed dispatch is not stuck as dispatching
        assert_eq!(js(&mut b, "var ev = new Event('ok'); s.dispatchEvent(ev); s.dispatchEvent(ev)"), "true");
        // an Unsupported thing in a listener stops dispatch too
        js(&mut b, "s.addEventListener('uns', function () { document.createElementNS('x', 'y') }); s.addEventListener('uns', L('never2'));");
        assert_eq!(js(&mut b, "s.dispatchEvent(new Event('uns')); log.join()"), "unsupported:createElementNS outside the HTML namespace (the native document has no namespaces)");
        // reports are capped; the oldest is dropped and counted
        let mut small = browser_with(PAGE, BrowserLimits { max_reports: 3, ..Default::default() });
        js(&mut small, "var s = document.getElementById('s'); s.addEventListener('t', function () { throw 1 });");
        js(&mut small, "for (var i = 0; i < 10; i++) s.dispatchEvent(new Event('t'))");
        assert_eq!(small.stats().reports, 3);
        assert_eq!(small.stats().dropped_reports, 7);
    });
}

#[test]
fn reentrant_dispatch_and_the_call_guard() {
    big(|| {
        let mut b = browser();
        log_setup(&mut b);
        js(&mut b, "var s = document.getElementById('s'); var depth = 0;
            s.addEventListener('n', function (e) { log.push('n' + depth); if (depth < 3) { depth++; s.dispatchEvent(new Event('n')); } });");
        assert_eq!(js(&mut b, "s.dispatchEvent(new Event('n')); log.join()"), "n0,n1,n2,n3");
        // dispatching the event that is being dispatched is an InvalidStateError (reported: the listener threw)
        js(&mut b, "log = []; s.addEventListener('same', function (e) { try { s.dispatchEvent(e) } catch (x) { log.push(x.name) } });");
        assert_eq!(js(&mut b, "s.dispatchEvent(new Event('same')); log.join()"), "InvalidStateError");
        // unbounded recursion through dispatch ends with the 100-call guard, not a native stack overflow
        js(&mut b, "s.addEventListener('inf', function () { s.dispatchEvent(new Event('inf')) });");
        assert_eq!(js(&mut b, "s.dispatchEvent(new Event('inf'))"), "limit:CallDepth");
        assert_eq!(js(&mut b, "1 + 1"), "2");
        // a listener that mutates the tree while it is dispatched through
        js(&mut b, "log = []; var a = document.getElementById('a'); a.addEventListener('m', function () { a.removeChild(s); log.push('removed') }); s.addEventListener('m', L('sm')); a.addEventListener('m', L('am'), true);");
        assert_eq!(js(&mut b, "s.dispatchEvent(new Event('m', { bubbles: true })); log.join() + ',' + (s.parentNode === null)"), "am:1:DIV,sm:2:SPAN,removed,true");
    });
}

#[test]
fn callbacks_from_different_scripts_keep_their_own_code() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = []; var tag = 'A'; function regA(t) { t.addEventListener('e', function (e) { log.push(tag + e.type) }) }");
        js(&mut b, "var tag2 = 'B'; var el = document.getElementById('a'); regA(el); el.addEventListener('e', function () { log.push(tag2 + tag) });");
        js(&mut b, "function other() { return 'unrelated ' + 1 + 2 + 3 }");
        assert_eq!(js(&mut b, "el.dispatchEvent(new Event('e')); log.join()"), "Ae,BA");
        // and the driver (Rust) dispatches into the same callbacks
        let a = b.query("#a").unwrap().unwrap();
        let r = b.dispatch(a, "e", false, false).unwrap();
        assert!(r.not_canceled);
        assert_eq!(js(&mut b, "log.join()"), "Ae,BA,Ae,BA");
    });
}

#[test]
fn input_state_and_the_test_driver() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var i = document.getElementById('i'); var log = []; var f = document.getElementById('f');
            document.addEventListener('input', function (e) { log.push('input:' + e.target.value + ':' + e.bubbles + ':' + e.isTrusted + ':' + (e instanceof CustomEvent)) });
            i.addEventListener('change', function (e) { log.push('change:' + i.value) });
            document.addEventListener('click', function (e) { log.push('click:' + e.target.id + e.cancelable) });
            f.addEventListener('submit', function (e) { log.push('submit'); e.preventDefault() });");
        assert_eq!(js(&mut b, "[i.value, i.defaultValue, i.type, i.checked].join()"), "v,v,text,false");
        let i = b.query("#i").unwrap().unwrap();
        let r = b.type_text(i, "hello").unwrap();
        assert!(r.not_canceled && r.reports.is_empty());
        assert_eq!(b.input_value(i).as_deref(), Some("hello"));
        // the native value changed, the attribute did not
        assert_eq!(js(&mut b, "[i.value, i.getAttribute('value'), i.defaultValue].join()"), "hello,v,v");
        b.commit_change(i).unwrap();
        let c = b.query("#a").unwrap().unwrap();
        b.click(c).unwrap();
        let f = b.query("#f").unwrap().unwrap();
        let sub = b.submit(f).unwrap();
        assert!(!sub.not_canceled);
        assert_eq!(js(&mut b, "log.join(' ')"), "input:hello:true:false:false change:hello click:atrue submit");
        // JS sets the value without an event; defaultValue follows the attribute
        assert_eq!(js(&mut b, "log = []; i.value = 'js'; i.setAttribute('value', 'w'); [i.value, i.defaultValue, log.length].join()"), "js,w,0");
        // a value that was never set follows the attribute
        assert_eq!(js(&mut b, "var j = document.createElement('input'); j.setAttribute('value', '1'); var before = j.value; j.setAttribute('value', '2'); before + j.value"), "12");
        // checkbox: checkedness is native state, `value` has default "on"
        js(&mut b, "var cb = document.createElement('input'); cb.setAttribute('type', 'checkbox'); document.body.appendChild(cb); cb.id = 'cb'; document.addEventListener('change', function (e) { log.push('c:' + e.target.checked) });");
        assert_eq!(js(&mut b, "[cb.checked, cb.value, cb.type, cb.defaultChecked].join()"), "false,on,checkbox,false");
        let cb = b.query("#cb").unwrap().unwrap();
        b.set_checked(cb, true).unwrap();
        assert_eq!(js(&mut b, "[cb.checked, cb.defaultChecked, log.join()].join()"), "true,false,input:on:true:false:false,c:true");
        js(&mut b, "cb.defaultChecked = true; cb.checked = false");
        assert_eq!(js(&mut b, "[cb.checked, cb.defaultChecked, cb.hasAttribute('checked')].join()"), "false,true,true");
        // the driver refuses things that are not inputs and kinds it cannot type into
        let a = b.query("#a").unwrap().unwrap();
        assert!(matches!(b.type_text(a, "x"), Err(Error::Unsupported(_))));
        assert!(matches!(b.type_text(cb, "x"), Err(Error::Unsupported(_))));
        assert!(matches!(b.set_checked(i, true), Err(Error::Unsupported(_))));
        // no default actions: a click on a checkbox does not toggle it
        b.click(cb).unwrap();
        assert_eq!(js(&mut b, "cb.checked"), "false");
        // other elements have no value state
        assert_eq!(js(&mut b, "typeof document.body.value + typeof document.getElementById('a').checked"), "undefinedundefined");
        assert_eq!(js(&mut b, "Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').get.call(document.body)"), "throw:TypeError:Illegal invocation");
    });
}

#[test]
fn driver_runs_a_microtask_checkpoint_after_each_listener() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = []; var a = document.getElementById('a');
            a.addEventListener('t', function () { log.push('A'); Promise.resolve().then(function () { log.push('micro-A') }) });
            a.addEventListener('t', function () { log.push('B') });");
        let a = b.query("#a").unwrap().unwrap();
        b.dispatch(a, "t", false, false).unwrap();
        // from the host, the stack is empty after A: its microtask runs before B
        assert_eq!(js(&mut b, "log.join()"), "A,micro-A,B");
        // from JS, the stack is not empty: microtasks wait for the script to end
        assert_eq!(js(&mut b, "log = []; a.dispatchEvent(new Event('t')); log.join()"), "A,B");
        b.drain().unwrap();
        assert_eq!(js(&mut b, "log.join()"), "A,B,micro-A");
    });
}

fn gc(b: &mut Browser, extra: &[Value]) {
    b.collect(extra).expect("collect");
}

#[test]
fn collect_keeps_attached_wrappers_listeners_and_expandos() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = []; (function () { var s = document.getElementById('s'); s.expando = 'kept'; s.addEventListener('t', function () { log.push('still') }); })();");
        gc(&mut b, &[]);
        // the wrapper has no JS reference, but the node is attached: same wrapper, same expando, same listener
        assert_eq!(js(&mut b, "var s = document.getElementById('s'); s.expando + (s === document.querySelector('#s'))"), "kepttrue");
        assert_eq!(js(&mut b, "s.dispatchEvent(new Event('t')); log.join()"), "still");
        assert!(b.stats().listeners >= 1);
    });
}

#[test]
fn collect_prunes_detached_wrappers_and_keeps_rooted_ones() {
    big(|| {
        let mut b = browser();
        // many detached nodes with listeners and events; only the last is kept in a variable
        js(&mut b, "var keep; for (var i = 0; i < 300; i++) { var n = document.createElement('div'); n.addEventListener('t', function () {}); n.expando = i; keep = n; } var ev = new Event('x'); ev = null;");
        let before = b.stats();
        assert!(before.wrappers >= 300 && before.listeners >= 300 && before.events >= 1);
        gc(&mut b, &[]);
        let after = b.stats();
        assert!(after.wrappers < 20, "wrappers {}", after.wrappers);
        assert_eq!(after.listeners, 1, "only the kept node's listener lives");
        assert_eq!(after.events, 0);
        // the kept detached node is usable and has its expando
        assert_eq!(js(&mut b, "keep.expando + ',' + keep.tagName + ',' + keep.isConnected"), "299,DIV,false");
        // the native arena keeps its nodes (existing Document limit)
        assert!(after.nodes >= before.nodes);
    });
}

#[test]
fn collect_keeps_a_detached_tree_connected_and_does_not_alias_stale_slots() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var root = document.createElement('div'); var kid = root.appendChild(document.createElement('b')); kid.mark = 'kid'; root.mark = 'root'; var tail = root.appendChild(document.createElement('i')); tail.mark = 'tail'; tail.addEventListener('t', function (e) { window.hit = (window.hit || 0) + 1 }); kid = null; tail = null;");
        gc(&mut b, &[]);
        // only `root` is held by JS; the wrappers of its children (and their expandos, listener) are still there
        assert_eq!(js(&mut b, "[root.firstChild.mark, root.lastChild.mark, root.mark].join()"), "kid,tail,root");
        assert_eq!(js(&mut b, "root.lastChild.dispatchEvent(new Event('t')); window.hit"), "1");
        // hold only a child: the parent's wrapper (and its expando) stays through the parent link
        js(&mut b, "var only = root.firstChild; root = null;");
        gc(&mut b, &[]);
        assert_eq!(js(&mut b, "only.parentNode.mark + ',' + only.parentNode.lastChild.mark"), "root,tail");
        // drop everything, collect, make many new objects: no new wrapper sees an old expando
        js(&mut b, "only = null;");
        gc(&mut b, &[]);
        assert!(b.stats().wrappers < 12);
        js(&mut b, "var bag = []; for (var i = 0; i < 200; i++) bag.push(document.createElement('p')); var seen = 0; for (var i = 0; i < 200; i++) if (bag[i].mark !== undefined) seen++;");
        assert_eq!(js(&mut b, "seen"), "0");
        // a node held only by the host (extra roots)
        let qa = b.query("#a").unwrap().unwrap(); let w = b.wrapper(qa).unwrap();
        js(&mut b, "var held = document.createElement('u'); held.mark = 'h'; document.getElementById('a').appendChild(held); document.getElementById('a').removeChild(held); held = null;");
        let held = js(&mut b, "document.createElement('q')");
        assert_eq!(held, "object");
        let _ = w;
    });
}

#[test]
fn removed_attached_subtrees_become_collectable_and_reattached_ones_stay() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var a = document.getElementById('a'); var s = document.getElementById('s'); s.mark = 'm'; a.removeChild(s); s = null; a = null;");
        gc(&mut b, &[]);
        // `s` was detached and unreferenced: its wrapper is gone, the node is not in the document
        assert_eq!(js(&mut b, "document.getElementById('s')"), "null");
        // moving a rooted detached node into the document keeps it
        js(&mut b, "var n = document.createElement('section'); n.mark = 'sec'; n.addEventListener('t', function () { window.got = 1 }); document.body.appendChild(n); n = null;");
        gc(&mut b, &[]);
        assert_eq!(js(&mut b, "var sec = document.querySelector('section'); sec.mark + (sec.dispatchEvent(new Event('t')), window.got)"), "sec1");
    });
}

#[test]
fn collect_with_an_event_target_and_a_stale_event() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var t = new EventTarget(); var hits = 0; t.addEventListener('e', function () { hits++ }); var gone = new EventTarget(); gone.addEventListener('e', function () {}); gone = null;");
        let l0 = b.stats().listeners;
        gc(&mut b, &[]);
        assert_eq!(b.stats().listeners, l0 - 1);
        assert_eq!(js(&mut b, "t.dispatchEvent(new Event('e')); hits"), "1");
    });
}

#[test]
fn caps_return_distinct_limit_errors() {
    big(|| {
        let l = BrowserLimits { max_nodes: 80, max_wrappers: 70, max_listeners: 5, max_listeners_per_target: 3, max_events: 4, max_results: 6, max_text_bytes: 4096, max_tree_depth: 8, max_attr_slots: 60, max_names: 40, ..Default::default() };
        let mut b = browser_with(PAGE, l);
        // listeners
        js(&mut b, "var t = new EventTarget(); function f() {} function g() {} function h() {} function k() {}");
        assert_eq!(js(&mut b, "t.addEventListener('a', f); t.addEventListener('a', g); t.addEventListener('a', h); t.addEventListener('a', k)"), lim("dom-listeners-per-target"));
        assert_eq!(js(&mut b, "var u = new EventTarget(); u.addEventListener('a', f); u.addEventListener('a', g); u.addEventListener('b', f)"), lim("dom-listeners"));
        // events
        assert_eq!(js(&mut b, "var evs = []; for (var i = 0; i < 10; i++) evs.push(new Event('x'));"), lim("dom-events"));
        // depth
        assert_eq!(js(&mut b, "var d = document.createElement('div'); var top = d; for (var i = 0; i < 20; i++) { var c = document.createElement('div'); c.appendChild(top); top = c; } 0"), lim("dom-depth"));
        // results: a flat list longer than the cap
        let mut b2 = browser_with(&format!("<body>{}</body>", "<i></i>".repeat(10)), BrowserLimits { max_results: 6, ..Default::default() });
        assert_eq!(js(&mut b2, "document.querySelectorAll('i')"), lim("dom-results"));
        assert_eq!(js(&mut b2, "document.body.childNodes"), lim("dom-results"));
        assert_eq!(js(&mut b2, "document.querySelector('i').tagName"), "I"); // first match needs no list
        // nodes and text
        let mut b3 = browser_with(PAGE, BrowserLimits { max_nodes: 20, max_text_bytes: 400, ..Default::default() });
        assert_eq!(js(&mut b3, "for (var i = 0; i < 100; i++) document.createElement('p')"), lim("dom-nodes"));
        assert_eq!(js(&mut b3, "document.body.setAttribute('x', new Array(500).join('x'))"), lim("dom-text"));
        // wrappers: all of them are alive until a collect
        let mut b4 = browser_with(PAGE, BrowserLimits { max_wrappers: 40, ..Default::default() });
        assert_eq!(js(&mut b4, "for (var i = 0; i < 100; i++) document.createTextNode('x')"), lim("dom-wrappers"));
        gc(&mut b4, &[]);
        assert_eq!(js(&mut b4, "document.createTextNode('x').data"), "x");
        // string size
        let mut b5 = browser_with(PAGE, BrowserLimits { max_string_bytes: 100, ..Default::default() });
        assert_eq!(js(&mut b5, "document.createTextNode(new Array(200).join('x'))"), lim("dom-string"));
        // names: a selector or attribute may not grow the name table without bound
        let mut b6 = browser_with(PAGE, BrowserLimits { max_names: 30, ..Default::default() });
        assert_eq!(js(&mut b6, "for (var i = 0; i < 100; i++) document.body.setAttribute('a' + i, '1')"), lim("dom-names"));
    });
}

#[test]
fn caps_hold_after_failures_and_a_cap_cannot_be_caught() {
    big(|| {
        let mut b = browser_with(PAGE, BrowserLimits { max_listeners: 2, ..Default::default() });
        js(&mut b, "var t = new EventTarget(); function f() {} function g() {}");
        assert_eq!(js(&mut b, "t.addEventListener('a', f); t.addEventListener('b', f); try { t.addEventListener('c', f) } catch (e) { 'caught' }"), lim("dom-listeners"));
        assert_eq!(b.stats().listeners, 2);
        // a removed listener makes room again
        assert_eq!(js(&mut b, "t.removeEventListener('a', f); t.addEventListener('c', f); 'ok'"), "ok");
    });
}

#[test]
fn walks_consume_steps() {
    big(|| {
        let html = format!("<body>{}</body>", "<div><p><span></span></p></div>".repeat(300));
        let doc = tree::parse(&html);
        let mut b = Browser::new(doc, BrowserLimits::default(), EvalLimits { max_steps: 2000, ..Default::default() }, HeapLimits::default()).unwrap();
        // a document-wide lookup that has to visit about 900 elements, many times
        assert_eq!(js(&mut b, "for (var i = 0; i < 100; i++) document.querySelectorAll('span').length"), "limit:Steps");
        // with a bigger budget the same script finishes
        let doc = tree::parse(&html);
        let mut b = Browser::new(doc, BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).unwrap();
        assert_eq!(js(&mut b, "var n = 0; for (var i = 0; i < 100; i++) n += document.querySelectorAll('span').length; n"), "30000");
        // text content of a big subtree is charged too
        let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), EvalLimits { max_steps: 2000, ..Default::default() }, HeapLimits::default()).unwrap();
        assert_eq!(js(&mut b, "for (var i = 0; i < 100; i++) document.body.textContent"), "limit:Steps");
    });
}

#[test]
fn wrappers_survive_between_calls_and_a_second_host_is_refused() {
    big(|| {
        let mut b = browser();
        let a = b.query("#a").unwrap().unwrap();
        let w = b.wrapper(a).unwrap();
        assert_eq!(b.node(w), Some(a));
        assert!(matches!(b.document().borrow().data(a), NodeData::Element { .. }));
        let h = b.it.heap.limits();
        assert!(h.max_objects > 0);
    });
}

#[test]
fn clone_checks_attribute_and_input_capacity_before_allocating() {
    big(|| {
        let mut b = browser_with("<body><input id=i></body>", BrowserLimits::default());
        js(&mut b, "var input = document.getElementById('i'); input.value = 'abcdefgh';");
        let old = b.stats();
        // Set limits before creating a new browser with the same source.
        let mut a = browser_with("<body><input id=i></body>", BrowserLimits { max_attr_slots: old.attr_slots, ..Default::default() });
        let before = a.stats();
        assert_eq!(js(&mut a, "document.getElementById('i').cloneNode(true)"), lim("dom-attrs"));
        assert_eq!(a.stats().nodes, before.nodes);
        let mut c = browser_with("<body><input id=i></body>", BrowserLimits { max_text_bytes: 12, ..Default::default() });
        js(&mut c, "var input = document.getElementById('i'); input.value = 'abcdefgh';");
        let before = c.stats();
        assert_eq!(js(&mut c, "input.cloneNode(true)"), lim("dom-input-bytes"));
        assert_eq!(c.stats().nodes, before.nodes);
    });
}

#[test]
fn custom_event_with_undefined_detail_keeps_custom_prototype() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var e = new CustomEvent('x', {detail: undefined}); [e instanceof CustomEvent, e.detail === null].join()"), "true,true");
    });
}

#[test]
fn dom_string_limit_counts_utf8_bytes() {
    big(|| {
        let mut b = browser_with(PAGE, BrowserLimits { max_string_bytes: 100, ..Default::default() });
        let src = format!("document.createTextNode('{}')", "é".repeat(64));
        assert_eq!(js(&mut b, &src), lim("dom-string"));
    });
}

#[test]
fn zero_report_capacity_retains_no_report() {
    big(|| {
        let mut b = browser_with(PAGE, BrowserLimits { max_reports: 0, ..Default::default() });
        js(&mut b, "document.addEventListener('x', function(){throw 'message'}); document.dispatchEvent(new Event('x'));");
        assert_eq!(b.stats().reports, 0);
        assert_eq!(b.stats().dropped_reports, 1);
    });
}

#[test]
fn wide_sibling_selectors_consume_traversal_budget() {
    big(|| {
        let html = format!("<body>{}</body>", "<i></i>".repeat(400));
        let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), EvalLimits { max_steps: 100, ..Default::default() }, HeapLimits::default()).unwrap();
        assert_eq!(js(&mut b, "document.body.lastElementChild.matches('z ~ i')"), "limit:Steps");
    });
}

#[test]
fn replacing_document_element_allows_the_old_root_to_leave() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var old = document.documentElement; var fresh = document.createElement('html'); document.replaceChild(fresh, old) === old && document.documentElement === fresh"), "true");
    });
}

#[test]
fn reports_truncate_large_thrown_strings_before_copying() {
    big(|| {
        let mut b = browser();
        js(&mut b, "document.addEventListener('x', function(){throw new Array(10001).join('é')}); document.dispatchEvent(new Event('x'));");
        let r = b.take_reports();
        assert_eq!(r.len(), 1);
        assert!(r[0].message.len() <= 4096);
        assert!(r[0].message.is_char_boundary(r[0].message.len()));
    });
}

#[test]
fn browser_rejects_an_initial_document_over_its_caps() {
    big(|| {
        for (limits, name) in [
            (BrowserLimits { max_nodes: 1, ..Default::default() }, "dom-nodes"),
            (BrowserLimits { max_text_bytes: 1, ..Default::default() }, "dom-text"),
            (BrowserLimits { max_attr_slots: 0, ..Default::default() }, "dom-attrs"),
            (BrowserLimits { max_names: 0, ..Default::default() }, "dom-names"),
        ] {
            let r = Browser::new(tree::parse(PAGE), limits, EvalLimits::default(), HeapLimits::default());
            assert!(matches!(r, Err(Error::Limit(LimitKind::Host(k))) if k == name), "initial cap {name}");
        }
    });
}

#[test]
fn dom_listeners_keep_script_and_builtin_state_across_checkpoints() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = []; var n = document.getElementById('s');");
        js(&mut b, "function left(){Promise.resolve().then(function(){log.push(JSON.stringify({v:/left/.test('left')}));});} n.addEventListener('go', left);");
        js(&mut b, "function right(){Promise.resolve().then(function(){log.push(JSON.stringify({v:/right/.test('left')}));});} n.addEventListener('go', right);");
        b.collect(&[]).unwrap();
        let node = b.query("#s").unwrap().unwrap();
        b.dispatch(node, "go", false, false).unwrap();
        assert_eq!(js(&mut b, "log.join('|')"), "{\"v\":true}|{\"v\":false}");
    });
}

#[test]
fn dom_string_copies_share_the_execution_budget() {
    big(|| {
        let html = format!("<body><p>{}</p></body>", "x".repeat(8192));
        for src in ["document.body.firstChild.firstChild.data".to_string(), format!("document.createTextNode('{}')", "x".repeat(8192))] {
            let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), EvalLimits { max_steps: 100, ..Default::default() }, HeapLimits::default()).unwrap();
            assert!(matches!(b.run_script(&src), Err(Error::Limit(LimitKind::Steps))));
        }
    });
}

#[test]
fn host_collection_traces_byte_views_and_their_buffer_callbacks() {
    big(|| {
        let mut b = browser();
        js(&mut b, "(function(){var n = document.getElementById('s'); var label = 'kept'; n.bytes = new TextEncoder().encode(JSON.stringify({v:/left/.test('left')})); n.bytes.buffer.read = function(){return label;};})();");
        js(&mut b, "(function(){var n = document.getElementById('s'); n.addEventListener('decode', function(){Promise.resolve().then(function(){n.textContent = new TextDecoder().decode(n.bytes) + ':' + n.bytes.buffer.read();});});})();");
        b.collect(&[]).unwrap();
        let n = b.query("#s").unwrap().unwrap();
        b.dispatch(n, "decode", false, false).unwrap();
        assert_eq!(js(&mut b, "document.getElementById('s').textContent"), "{\"v\":true}:kept");
    });
}

// Classes and async methods as listeners. The instances are reachable only through the host's listener edges,
// two scripts use the same node shapes, and the driver (not JS) dispatches.
const LISTENER_CLASSES_A: &str = "var log = [];
    class Counter { #n = 0; #el; static last;
      constructor(el) { this.#el = el; el.addEventListener('go', (e) => { Counter.last = this.work(e); }); }
      async work(e) { await null; this.#n += 1; this.#el.textContent = JSON.stringify({ n: this.#n }); log.push(e.type + this.#n); return this.#n; } }";
const LISTENER_CLASSES_B: &str = "class Handler { #seen = []; static last;
      async handleEvent(e) { await Promise.resolve(); this.#seen.push(e.type); document.getElementById('i').setAttribute('data-h', this.#seen.join('+').replace(/go/g, 'G')); Handler.last = this.#seen.length; } }
    (function () { new Counter(document.getElementById('s')); document.getElementById('s').addEventListener('go', new Handler()); })();";

#[test]
fn class_and_async_listeners_survive_collect_and_settle_in_the_driver() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, LISTENER_CLASSES_A), "undefined");
        assert_eq!(js(&mut b, LISTENER_CLASSES_B), "undefined");
        // the instances have no JS reference: the listener closures (a host edge) hold them
        assert_eq!(b.it.private_slots(), 3);
        b.collect(&[]).unwrap();
        b.collect(&[]).unwrap();
        assert_eq!(b.it.private_slots(), 3);
        assert_eq!(b.stats().listeners, 2);
        let s = b.query("#s").unwrap().unwrap();
        for round in 1..=2 {
            b.dispatch(s, "go", false, false).unwrap();
            // the driver ran a checkpoint after each listener, so every continuation has finished
            assert_eq!(js(&mut b, "log.join()"), (1..=round).map(|r| format!("go{r}")).collect::<Vec<_>>().join(","));
            let want = format!("{{\"n\":{round}}}");
            assert_eq!(js(&mut b, "document.getElementById('s').textContent"), want);
            assert_eq!(js(&mut b, "document.getElementById('i').getAttribute('data-h')"), vec!["G"; round].join("+"));
            assert_eq!(js(&mut b, "Handler.last"), round.to_string());
            let last = b.run_script("Counter.last").unwrap();
            assert!(matches!(b.it.promise_status(last), Some(bare_js::eval::PromiseStatus::Fulfilled(Value::Number(n))) if n == round as f64));
            assert_eq!(b.it.async_stats().activations, 0);
            assert_eq!(b.it.pending_jobs(), 0);
            b.collect(&[]).unwrap();
            assert_eq!(b.it.private_slots(), 3);
        }
        // removing the listeners drops every instance, slot and activation
        assert_eq!(js(&mut b, "var s = document.getElementById('s'); var p = s.parentNode; p.removeChild(s); Counter.last = null; s = null; p = null; 0"), "0");
        b.collect(&[]).unwrap();
        assert_eq!(b.it.private_slots(), 0);
        assert_eq!(b.stats().listeners, 0);
        assert_eq!(b.it.async_stats().activations, 0);
    });
}

#[test]
fn a_suspended_listener_activation_keeps_its_class_state_through_collect() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = [], release; var gate = new Promise(function (r) { release = r; });
            class Box { #v = { a: [1, 2] }; async on(e) { await gate; log.push(this.#v.a.length + e.type); } }
            (function () { var x = new Box(); document.getElementById('s').addEventListener('t', function (e) { x.on(e); }); })();");
        let s = b.query("#s").unwrap().unwrap();
        b.dispatch(s, "t", false, false).unwrap();
        // the activation is suspended (gate is pending). Nothing but the pending promise reaches the instance.
        assert_eq!(b.it.async_stats().activations, 1);
        b.collect(&[]).unwrap();
        assert_eq!(b.it.async_stats().activations, 1);
        assert_eq!(b.it.private_slots(), 1);
        js(&mut b, "release(0)");
        b.drain().unwrap();
        assert_eq!(js(&mut b, "log.join()"), "2t");
        assert_eq!(b.it.async_stats().activations, 0);
    });
}

#[test]
fn subclassing_a_host_event_is_refused_instead_of_returning_the_wrong_brand() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "class MyEvent extends Event { get kind() { return 'my'; } } var made = 0; typeof MyEvent"), "function");
        assert_eq!(js(&mut b, "var e = new MyEvent('x'); made = 1; e"), "unsupported:subclassing a host constructor (new.target is not passed to the constructor)");
        assert_eq!(js(&mut b, "made + ',' + (new Event('x') instanceof Event) + ',' + typeof new CustomEvent('y').type"), "0,true,string");
        // a class that merely holds an event is fine
        assert_eq!(js(&mut b, "class H { #e = new Event('z'); get t() { return this.#e.type; } } new H().t"), "z");
    });
}
