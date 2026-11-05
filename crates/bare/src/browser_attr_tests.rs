//! Tests of `Element.attributes` (`NamedNodeMap`) and `Attr`. Expectations come from the DOM Standard, by reading.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

const SVG: &str = "http://www.w3.org/2000/svg";
const XLINK: &str = "http://www.w3.org/1999/xlink";
const XML: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS: &str = "http://www.w3.org/2000/xmlns/";

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const PAGE: &str = "<!doctype html><html lang=en class=c><head><title>t</title></head><body><div id=a class=\"x y\" data-k=v></div><p></p></body></html>";

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


fn by_id(doc: &bare_html::dom::Document, id: &str) -> bare_html::dom::NodeId {
    let mut cur = doc.root();
    let mut stack = vec![cur];
    while let Some(n) = stack.pop() {
        cur = n;
        if doc.attr(n, bare_html::dom::Atom::ID) == Some(id) {
            return cur;
        }
        stack.extend(doc.children(n));
    }
    panic!("no #{id}")
}

fn ok(b: &mut Browser, src: &str, want: impl AsRef<str>) {
    assert_eq!(js(b, src), want.as_ref(), "{src}");
}

/// The failure that was seen in bare: React clears the attributes of `<html>` with `while (map.length) el.removeAttributeNode(map[0])`.
#[test]
fn react_host_singleton_cleanup_loop_empties_the_element() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var el = document.documentElement, map = el.attributes, n = 0; while (map.length) { el.removeAttributeNode(map[0]); n++; if (n > 100) break; } [n, map.length, el.hasAttributes(), el.getAttribute('lang') === null].join('|')", "2|0|false|true");
    });
}

#[test]
fn react_loop_runs_repeatedly_over_many_elements_and_attributes() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var total = 0; for (var r = 0; r < 20; r++) { var e = document.createElement('div'); for (var i = 0; i < 50; i++) e.setAttribute('a' + i, String(i)); var m = e.attributes; if (m.length !== 50) throw new Error('len ' + m.length); var guard = 0; while (m.length) { e.removeAttributeNode(m[0]); total++; if (++guard > 100) throw new Error('loop'); } if (e.hasAttributes()) throw new Error('left'); } total", "1000");
        // The other cleanup shape: remove by the name of the first entry, and by the last entry.
        ok(&mut b, "var e2 = document.createElement('p'); e2.setAttribute('x', '1'); e2.setAttribute('y', '2'); e2.setAttribute('z', '3'); var m2 = e2.attributes; while (m2.length) e2.removeAttribute(m2[m2.length - 1].name); [m2.length, e2.attributes === m2].join()", "0,true");
    });
}

#[test]
fn map_and_attr_have_identity() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var el = document.getElementById('a'), map = el.attributes; [el.attributes === el.attributes, map[0] === map[0], map[0] === map.item(0), map[0] === el.getAttributeNode('id'), map[0] === map.getNamedItem('id'), map[1] === el.getAttributeNode('class'), map[0] !== map[1], map[0].ownerElement === el, map[0].ownerDocument === document].join()", "true,true,true,true,true,true,true,true,true");
        ok(&mut b, "[el.attributes === document.getElementById('a').attributes, el.attributes === document.body.attributes].join()", "true,false");
        ok(&mut b, "[map[0] instanceof Attr, map instanceof NamedNodeMap, map[0] instanceof Node, Object.prototype.toString.call(map)].join()", "true,true,true,[object Object]");
        ok(&mut b, "[typeof Attr, typeof NamedNodeMap, Attr.name, NamedNodeMap.name].join()", "function,function,Attr,NamedNodeMap");
        ok(&mut b, "try { new Attr() } catch (e) { e.name }", "TypeError");
        ok(&mut b, "try { new NamedNodeMap() } catch (e) { e.name }", "TypeError");
    });
}

#[test]
fn map_follows_every_native_and_script_mutation_path() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var el = document.getElementById('a'), map = el.attributes; function names() { return Array.prototype.map.call(map, function (a) { return a.name; }).join() + '#' + map.length; }; names()", "id,class,data-k#3");
        // setAttribute (new and existing), removeAttribute
        ok(&mut b, "el.setAttribute('title', 't'); names()", "id,class,data-k,title#4");
        ok(&mut b, "el.setAttribute('class', 'z'); names()", "id,class,data-k,title#4");
        ok(&mut b, "el.removeAttribute('class'); names()", "id,data-k,title#3");
        // the NS methods
        ok(&mut b, &format!("el.setAttributeNS('{XLINK}', 'xlink:href', '#u'); names()"), "id,data-k,title,xlink:href#4");
        ok(&mut b, &format!("el.removeAttributeNS('{XLINK}', 'href'); names()"), "id,data-k,title#3");
        // id and className setters, classList, style
        ok(&mut b, "el.className = 'q'; names()", "id,data-k,title,class#4");
        ok(&mut b, "el.classList.remove('q'); names()", "id,data-k,title,class#4");
        ok(&mut b, "el.removeAttribute('class'); el.classList.add('w'); names() + ':' + el.getAttribute('class')", "id,data-k,title,class#4:w");
        ok(&mut b, "el.removeAttribute('class'); el.style.color = 'red'; names()", "id,data-k,title,style#4");
        ok(&mut b, "el.style.color = ''; el.removeAttribute('style'); names()", "id,data-k,title#3");
        // the same Attr object follows a value change
        ok(&mut b, "var t = map.getNamedItem('title'); el.setAttribute('title', 'new'); [t.value, map.getNamedItem('title') === t, t.ownerElement === el].join()", "new,true,true");
        ok(&mut b, "t.value = 'viaattr'; el.getAttribute('title')", "viaattr");
        // the native document changes without any script call
        {
            let doc = b.document();
            let el = by_id(&doc.borrow(), "a");
            doc.borrow_mut().set_attr(el, "native", "1");
        }
        ok(&mut b, "names()", "id,data-k,title,native#4");
        {
            let doc = b.document();
            let el = by_id(&doc.borrow(), "a");
            doc.borrow_mut().remove_attr(el, "title");
        }
        ok(&mut b, "[names(), t.ownerElement, t.value, map.getNamedItem('title')].join('|')", "id,data-k,native#3||viaattr|");
    });
}

#[test]
fn attr_fields_come_from_the_native_name_and_namespace() {
    big(|| {
        let mut b = browser();
        ok(&mut b, &format!("var s = document.createElementNS('{SVG}', 'svg'); s.setAttributeNS('{XLINK}', 'xlink:href', '#a'); s.setAttributeNS('{XML}', 'xml:lang', 'en'); s.setAttributeNS('{XMLNS}', 'xmlns:xlink', '{XLINK}'); s.setAttributeNS('{XMLNS}', 'xmlns', '{SVG}'); s.setAttribute('viewBox', '0 0 1 1'); s.setAttribute('a:b', 'c'); var m = s.attributes; Array.prototype.map.call(m, function (a) {{ return [a.name, a.localName, a.prefix, a.namespaceURI, a.value].join('~'); }}).join('\\n')"),
            format!("xlink:href~href~xlink~{XLINK}~#a\nxml:lang~lang~xml~{XML}~en\nxmlns:xlink~xlink~xmlns~{XMLNS}~{XLINK}\nxmlns~xmlns~~{XMLNS}~{SVG}\nviewBox~viewBox~~~0 0 1 1\na:b~a:b~~~c"));
        ok(&mut b, "var a = s.getAttributeNode('viewBox'); [a.nodeType, a.nodeName, a.nodeValue, a.textContent, a.specified, a.isConnected, a.parentNode, a.firstChild, a.childNodes.length, a.hasChildNodes()].join()", "2,viewBox,0 0 1 1,0 0 1 1,true,false,,,0,false");
        ok(&mut b, "a.nodeValue = 'x'; var x1 = s.getAttribute('viewBox'); a.textContent = null; [x1, s.getAttribute('viewBox') === '', a.value === ''].join()", "x,true,true");
        ok(&mut b, "a.value = undefined; s.getAttribute('viewBox')", "undefined");
        // An HTML element keeps names lowercase and is case-insensitive for lookup; an SVG element is exact.
        ok(&mut b, "var h = document.createElement('div'); h.setAttribute('DATA-X', '1'); [h.attributes[0].name, h.attributes.getNamedItem('Data-X') !== null, h.getAttributeNode('DATA-X') === h.attributes[0], s.attributes.getNamedItem('VIEWBOX'), s.getAttributeNode('viewbox')].join()", "data-x,true,true,,");
        ok(&mut b, &format!("[s.getAttributeNodeNS('{XLINK}', 'href').name, s.getAttributeNodeNS(null, 'href'), s.getAttributeNodeNS('{XLINK}', 'xlink:href'), s.getAttributeNodeNS('urn:other', 'href'), s.attributes.getNamedItemNS('{XML}', 'lang').value, s.attributes.getNamedItemNS(null, 'viewBox').value].join()"), "xlink:href,,,,en,undefined");
    });
}

#[test]
fn namespaced_set_changes_the_value_and_keeps_the_prefix() {
    big(|| {
        let mut b = browser();
        ok(&mut b, &format!("var s = document.createElementNS('{SVG}', 'svg'); s.setAttributeNS('{XLINK}', 'xlink:href', '#1'); var a = s.attributes[0]; s.setAttributeNS('{XLINK}', 'other:href', '#2'); [s.attributes.length, s.attributes[0] === a, a.name, a.prefix, a.value, s.getAttributeNS('{XLINK}', 'href')].join()"), "1,true,xlink:href,xlink,#2,#2");
        // `getAttribute` finds the first attribute with this qualified name, in any namespace: it is the same Attr.
        ok(&mut b, "s.getAttributeNode('xlink:href') === a", "true");
    });
}

#[test]
fn removal_gives_the_exact_attr_detached_and_readding_never_aliases() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var el = document.getElementById('a'), map = el.attributes, a = map.getNamedItem('class'); var r = el.removeAttributeNode(a); [r === a, a.ownerElement, a.value, a.name, map.length, el.hasAttribute('class'), map.getNamedItem('class')].join('|')", "true||x y|class|2|false|");
        ok(&mut b, "el.setAttribute('class', 'again'); var n = el.getAttributeNode('class'); [n === a, n.value, a.value, a.ownerElement, n.ownerElement === el].join()", "false,again,x y,,true");
        // removeAttribute (not the node method) detaches the Attr the same way, with the value it had
        ok(&mut b, "var b1 = el.getAttributeNode('data-k'); el.removeAttribute('data-k'); el.setAttribute('data-k', 'two'); [b1.value, b1.ownerElement, el.getAttributeNode('data-k') === b1, el.getAttribute('data-k')].join()", "v,,false,two");
        // remove, add, remove again, with no read between: the first Attr keeps the first value
        ok(&mut b, "var c1 = el.getAttributeNode('data-k'); el.removeAttribute('data-k'); el.setAttribute('data-k', 'three'); el.removeAttribute('data-k'); [c1.value, c1.ownerElement, el.hasAttribute('data-k')].join()", "two,,false");
        // the NS remove, style and classList paths detach too
        ok(&mut b, &format!("el.setAttributeNS('{XLINK}', 'xlink:href', 'h'); var d1 = el.getAttributeNode('xlink:href'); el.removeAttributeNS('{XLINK}', 'href'); [d1.value, d1.ownerElement, d1.namespaceURI, d1.prefix].join()"), format!("h,,{XLINK},xlink"));
        ok(&mut b, "el.style.color = 'red'; var e1 = el.getAttributeNode('style'); el.removeAttribute('style'); [e1.value, e1.ownerElement].join()", "color: red;,");
        // a detached Attr can be written, and it is a value of its own
        ok(&mut b, "a.value = 'detached'; [a.value, a.nodeValue, el.getAttribute('class')].join()", "detached,detached,again");
        // removeNamedItem
        ok(&mut b, "var rm = map.removeNamedItem('id'); [rm.name, rm.value, rm.ownerElement, map.length, el.id].join('|')", "id|a||1|");
        ok(&mut b, "try { map.removeNamedItem('id') } catch (e) { e.name }", "NotFoundError");
        ok(&mut b, &format!("el.setAttributeNS('{XLINK}', 'xlink:role', 'r'); var rn = map.removeNamedItemNS('{XLINK}', 'role'); [rn.name, rn.value, rn.namespaceURI].join()"), format!("xlink:role,r,{XLINK}"));
        ok(&mut b, &format!("try {{ map.removeNamedItemNS('{XLINK}', 'role') }} catch (e) {{ e.name }}"), "NotFoundError");
        ok(&mut b, "try { map.removeNamedItemNS('urn:other', 'role') } catch (e) { e.name }", "NotFoundError");
        ok(&mut b, "try { el.removeAttributeNode(a) } catch (e) { e.name }", "NotFoundError");
        ok(&mut b, "var other = document.createElement('i'); other.setAttribute('k', 'v'); try { el.removeAttributeNode(other.getAttributeNode('k')) } catch (e) { e.name }", "NotFoundError");
        ok(&mut b, "try { el.removeAttributeNode({}) } catch (e) { e.name + ':' + e.message }", "TypeError:parameter 1 is not of type 'Attr'");
    });
}

#[test]
fn set_attribute_node_attaches_the_same_object_or_fails_loudly() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var el = document.getElementById('a'), at = document.createAttribute('Data-New'); [at.name, at.localName, at.value, at.ownerElement, at.namespaceURI, at.prefix].join('|')", "data-new|data-new||||");
        ok(&mut b, "at.value = 'v1'; var prev = el.setAttributeNode(at); [prev, at.ownerElement === el, el.getAttribute('data-new'), el.getAttributeNode('data-new') === at, el.attributes.length, el.attributes[3] === at].join()", ",true,v1,true,4,true");
        ok(&mut b, "el.setAttributeNode(at) === at && el.attributes.length", "4");
        // replace: the old Attr is returned, detached with its old value; the new one is attached in the same place
        ok(&mut b, "var old = el.getAttributeNode('id'); var nw = document.createAttribute('id'); nw.value = 'zz'; var ret = el.setAttributeNode(nw); [ret === old, old.value, old.ownerElement, nw.ownerElement === el, el.id, el.attributes[0] === nw, el.attributes.length].join()", "true,a,,true,zz,true,4");
        ok(&mut b, "old.ownerElement === null && el.getElementById === undefined && document.getElementById('a') === null && document.getElementById('zz') === el", "true");
        // in use by another element
        ok(&mut b, "var p = document.createElement('p'); try { p.setAttributeNode(at) } catch (e) { e.name + ':' + e.code }", "InUseAttributeError:10");
        // a removed Attr can go to another element
        ok(&mut b, "var rem = el.removeAttributeNode(at), prevp = p.setAttributeNode(rem); [prevp, p.getAttribute('data-new'), rem.ownerElement === p, el.hasAttribute('data-new')].join()", ",v1,true,false");
        // setNamedItem is the same operation
        ok(&mut b, "var q = document.createElement('q'), at2 = document.createAttribute('k'); at2.value = '9'; [q.attributes.setNamedItem(at2), q.getAttribute('k'), q.attributes.setNamedItemNS(document.createAttribute('j')) === null, q.attributes.length].join()", ",9,true,2");
        // namespaced creation and attach
        ok(&mut b, &format!("var sv = document.createElementNS('{SVG}', 'svg'), na = document.createAttributeNS('{XLINK}', 'xlink:href'); na.value = '#t'; sv.setAttributeNodeNS(na); [sv.getAttributeNS('{XLINK}', 'href'), na.prefix, na.localName, na.namespaceURI, sv.attributes[0] === na].join()"), format!("#t,xlink,href,{XLINK},true"));
        // a different prefix for an existing namespace and local name is not faked
        let r = js(&mut b, &format!("var na3 = document.createAttributeNS('{XLINK}', 'x:href'); sv.setAttributeNode(na3)"));
        assert!(r.starts_with("unsupported:setAttributeNode of an attribute with the namespace and local name"), "{r}");
        ok(&mut b, "[sv.attributes.length, sv.attributes[0] === na, na.ownerElement === sv, na3.ownerElement].join()", "1,true,true,");
        // errors
        ok(&mut b, "try { document.createAttribute('a b') } catch (e) { e.name }", "InvalidCharacterError");
        ok(&mut b, "try { document.createAttributeNS(null, 'x:y') } catch (e) { e.name }", "NamespaceError");
        let r = js(&mut b, "document.createAttributeNS('urn:other', 'x:y')");
        assert!(r.starts_with("unsupported:createAttributeNS with a namespace other than none"), "{r}");
        ok(&mut b, "try { el.setAttributeNode({}) } catch (e) { e.name + ':' + e.message }", "TypeError:parameter 1 is not of type 'Attr'");
    });
}

#[test]
fn indexed_access_descriptors_keys_and_length_are_truthful() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var el = document.getElementById('a'), map = el.attributes; [map.length, 0 in map, 2 in map, 3 in map, map[3], map[-1], map['1'] === map[1], map.hasOwnProperty(0), map.hasOwnProperty(3), map.hasOwnProperty('length'), 'length' in map, 'item' in map].join()", "3,true,true,false,,,true,true,false,false,true,true");
        ok(&mut b, "Object.keys(map).join() + '|' + Reflect.ownKeys(map).join() + '|' + Object.getOwnPropertyNames(map).join()", "0,1,2|0,1,2|0,1,2");
        ok(&mut b, "var d = Object.getOwnPropertyDescriptor(map, 1); [d.writable, d.enumerable, d.configurable, d.value === map[1], Object.getOwnPropertyDescriptor(map, 3), Object.getOwnPropertyDescriptor(map, 'length')].join()", "false,true,true,true,,");
        ok(&mut b, "var out = []; for (var k in map) if (!isNaN(k)) out.push(k); out.join()", "0,1,2");
        ok(&mut b, "map[0] = 1; map[7] = 1; map.x = 5; [map[0] === el.getAttributeNode('id'), 7 in map, map.x, map.length, Object.keys(map).join()].join()", "true,false,5,3,0,1,2,x");
        ok(&mut b, "'use strict'; try { map[0] = 1 } catch (e) { e.name }", "TypeError");
        ok(&mut b, "'use strict'; try { map[9] = 1 } catch (e) { e.name }", "TypeError");
        ok(&mut b, "'use strict'; try { delete map[0] } catch (e) { e.name }", "TypeError");
        ok(&mut b, "'use strict'; try { map.length = 9 } catch (e) { e.name }", "TypeError");
        ok(&mut b, "try { Object.defineProperty(map, 0, { value: 1 }) } catch (e) { e.name }", "TypeError");
        // length is a prototype accessor and cannot be set; it reads the native count
        ok(&mut b, "var ld = Object.getOwnPropertyDescriptor(NamedNodeMap.prototype, 'length'); [typeof ld.get, ld.set, ld.enumerable, ld.configurable].join()", "function,,true,true");
        ok(&mut b, "el.setAttribute('n1', 'v'); el.setAttribute('n2', 'v'); [map.length, 4 in map, map[4].name, Object.keys(map).length].join()", "5,true,n2,6"); // the sixth key is the expando `x` set above
        // Array methods, spread, for-of and Array.from read the live map
        ok(&mut b, "[Array.from(map).map(function (a) { return a.name; }).join(), [].concat.apply([], [Array.prototype.slice.call(map).map(function (a) { return a.value; })]).join()].join('|')", "id,class,data-k,n1,n2|a,x y,v,v,v");
        ok(&mut b, "var names = []; for (var a of map) names.push(a.name); names.join() + '|' + [...map].length", "id,class,data-k,n1,n2|5");
        ok(&mut b, "var seen = []; Array.prototype.forEach.call(map, function (a, i) { seen.push(i + a.name); if (i === 0) el.removeAttribute('n2'); }); seen.join()", "0id,1class,2data-k,3n1");
        ok(&mut b, "map[Symbol.iterator] === Array.prototype[Symbol.iterator]", "true");
        ok(&mut b, "map.item(0) === map[0] && map.item(99) === null && map.item(-1) === null && map.item(4294967296) === map[0]", "true");
        ok(&mut b, "JSON.stringify(Object.keys(map))", "[\"0\",\"1\",\"2\",\"3\",\"x\"]");
    });
}

#[test]
fn wrong_receivers_are_illegal_invocations() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var f = Object.getOwnPropertyDescriptor(Attr.prototype, 'name').get; try { f.call({}) } catch (e) { e.name + ':' + e.message }", "TypeError:Illegal invocation");
        ok(&mut b, "try { NamedNodeMap.prototype.item.call({}, 0) } catch (e) { e.name + ':' + e.message }", "TypeError:Illegal invocation");
        ok(&mut b, "try { Object.getOwnPropertyDescriptor(NamedNodeMap.prototype, 'length').get.call(document.body) } catch (e) { e.name }", "TypeError");
        ok(&mut b, "try { Object.getOwnPropertyDescriptor(Element.prototype, 'attributes').get.call({}) } catch (e) { e.name }", "TypeError");
        // `attributes` and `getAttributeNode` are on Element, not on Document or Text
        ok(&mut b, "[typeof document.attributes, typeof document.createTextNode('x').attributes, typeof document.getAttributeNode].join()", "undefined,undefined,undefined");
        // A node method on an Attr is an honest error, not a wrong answer
        ok(&mut b, "var at = document.body.attributes.length === 0 ? document.createAttribute('x') : document.body.attributes[0]; try { at.cloneNode() } catch (e) { e.name }", "TypeError");
    });
}

#[test]
fn text_is_utf8_and_a_lone_surrogate_is_refused() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var el = document.createElement('div'); el.setAttribute('data-é', '日本😀'); var a = el.attributes[0]; [a.name, a.value, a.value.length, el.attributes.getNamedItem('data-é') === a].join()", "data-é,日本😀,4,true");
        ok(&mut b, "a.value = 'ü'; el.getAttribute('data-é')", "ü");
        let r = js(&mut b, "a.value = '\\ud800'");
        assert!(r.starts_with("unsupported:lone surrogate"), "{r}");
        ok(&mut b, "a.value", "ü");
        let r = js(&mut b, "document.createAttribute('x\\ud800')");
        assert!(r.starts_with("unsupported:lone surrogate"), "{r}");
        let r = js(&mut b, "el.attributes.getNamedItem('\\udc00')");
        assert!(r.starts_with("unsupported:lone surrogate"), "{r}");
        // a detached attribute value with a pair round trips
        ok(&mut b, "var d = document.createAttribute('d'); d.value = '\\ud83d\\ude00'; d.value.length + ':' + d.value.charCodeAt(0)", "2:55357");
    });
}

#[test]
fn script_state_survives_scripts_promises_and_timers() {
    big(|| {
        let mut b = browser();
        b.use_virtual_clock_at(1000.0).unwrap();
        ok(&mut b, "var el = document.getElementById('a'), map = el.attributes, held = map[1]; Promise.resolve().then(function () { el.removeAttribute('class'); window.afterMicro = map.length + ':' + held.ownerElement; }); setTimeout(function () { el.setAttribute('class', 'late'); window.afterTimer = map.length + ':' + (map.getNamedItem('class') === held) + ':' + held.value; }, 10); map.length", "3");
        b.drain().expect("drain");
        ok(&mut b, "afterMicro", "2:null");
        b.advance_time(20.0, 1000).expect("advance");
        // The second script sees both changes through the object that the first script made.
        ok(&mut b, "afterTimer + '|' + map.length + '|' + held.value + '|' + held.ownerElement", "3:false:x y|3|x y|null");
        ok(&mut b, "map[2].name + ':' + map[2].value", "class:late");
    });
}

#[test]
fn collector_follows_edges_and_prunes_dead_wrappers() {
    big(|| {
        let mut b = browser();
        // The map keeps a detached element alive, and its Attrs are traced from it (SameObject while held).
        ok(&mut b, "var keep = (function () { var e = document.createElement('div'); e.setAttribute('k', 'v'); e.setAttribute('j', 'w'); return e.attributes; })(); keep[0].expando = 7; 1", "1");
        b.collect(&[]).expect("collect");
        ok(&mut b, "[keep.length, keep[0].ownerElement.tagName, keep[0].expando, keep[1].value, keep[0].ownerElement.attributes === keep].join()", "2,DIV,7,w,true");
        // An Attr alone keeps its element, and a detached Attr keeps nothing but its value.
        ok(&mut b, "var at = (function () { var e = document.createElement('p'); e.setAttribute('z', 'zz'); return e.getAttributeNode('z'); })(); var gone = (function () { var e = document.createElement('p'); e.setAttribute('y', 'yy'); return e.removeAttributeNode(e.getAttributeNode('y')); })(); 1", "1");
        b.collect(&[]).expect("collect");
        ok(&mut b, "[at.ownerElement.getAttribute('z'), gone.ownerElement, gone.value, gone.name].join()", "zz,,yy,y");
        let n_before = {
            let t = b.dom.attr.s.borrow();
            (t.attrs.len(), t.maps.len(), t.by_el.len())
        };
        // Drop everything: after a collection the tables hold nothing of the dead objects, and the document watches no attribute of them.
        ok(&mut b, "keep = at = gone = null; 1", "1");
        b.collect(&[]).expect("collect");
        let n_after = {
            let t = b.dom.attr.s.borrow();
            (t.attrs.len(), t.maps.len(), t.by_el.len())
        };
        assert!(n_before.0 >= 4 && n_before.1 >= 1, "{n_before:?}");
        assert_eq!(n_after, (0, 0, 0), "{n_after:?}");
        // Slot reuse: many new objects after the collection never see a dead wrapper's state.
        ok(&mut b, "var acc = []; for (var i = 0; i < 200; i++) { var e = document.createElement('div'); e.setAttribute('i', String(i)); acc.push(e.attributes[0].value + e.attributes.length); } acc[0] + ':' + acc[199]", "01:1991");
        ok(&mut b, "e = null; 1", "1");
        b.collect(&[]).expect("collect");
        ok(&mut b, "acc.length", "200");
        let (attrs, maps, by_el) = {
            let t = b.dom.attr.s.borrow();
            (t.attrs.len(), t.maps.len(), t.by_el.len())
        };
        assert_eq!((attrs, maps, by_el), (0, 0, 0));
    });
}

#[test]
fn an_element_with_a_held_attr_keeps_identity_across_collections() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var el = document.getElementById('a'); var held = el.getAttributeNode('id'); held.tag = 'T'; 1", "1");
        for _ in 0..3 {
            b.collect(&[]).expect("collect");
        }
        ok(&mut b, "[el.getAttributeNode('id') === held, held.tag, el.attributes[0] === held].join()", "true,T,true");
        // The element wrapper of an attached element holds its Attr even when the script dropped it: the expando stays.
        ok(&mut b, "el.getAttributeNode('class').mark = 1; 1", "1");
        b.collect(&[]).expect("collect");
        ok(&mut b, "el.getAttributeNode('class').mark", "1");
    });
}

#[test]
fn caps_fail_before_any_change() {
    big(|| {
        // Wrappers: the cap counts nodes, Attrs and maps together.
        let lim = BrowserLimits { max_wrappers: 40, ..Default::default() };
        let mut b = browser_with(PAGE, lim);
        ok(&mut b, "var el = document.createElement('div'); for (var i = 0; i < 30; i++) el.setAttribute('a' + i, 'v'); var m = el.attributes; var got = 0; var err = ''; try { for (var i = 0; i < 30; i++) { m[i]; got++; } } catch (e) { err = String(e); } got", "30");
        // 30 attributes + the map are under the cap... one more element's worth is not.
        let r = js(&mut b, "var f = document.createElement('div'); for (var i = 0; i < 10; i++) f.setAttribute('b' + i, 'v'); var m2 = f.attributes; for (var i = 0; i < 10; i++) m2[i]; 1");
        assert_eq!(r, "limit:Host(\"dom-wrappers\")", "{r}");
        // The state is whole after the refusal: native attributes, the map length and the earlier Attrs are intact.
        ok(&mut b, "[f.attributes.length, m.length, m[0].name, m[29].name, m[0] === el.getAttributeNode('a0')].join()", "10,30,a0,a29,true");
        // A removal after the refusal still detaches the Attr that exists.
        ok(&mut b, "var h = m[5]; el.removeAttribute('a5'); [h.ownerElement, h.value, m.length, m[5].name].join()", ",v,29,a6");

        // The text cap: a detached value that does not fit is refused and leaves the old value.
        let lim = BrowserLimits { max_text_bytes: 4096, ..Default::default() };
        let mut b = browser_with(PAGE, lim);
        ok(&mut b, "var at = document.createAttribute('t'); at.value = 'small'; var big = new Array(8000).join('x'); 1", "1");
        let r = js(&mut b, "at.value = big");
        assert_eq!(r, "limit:Host(\"dom-text\")", "{r}");
        ok(&mut b, "at.value", "small");

        // The name cap: createAttribute with a new name is refused when the table is full.
        let n_names = tree::parse(PAGE).name_count();
        let lim = BrowserLimits { max_names: n_names + 1, ..Default::default() };
        let mut b = browser_with(PAGE, lim);
        ok(&mut b, "document.createAttribute('one').name", "one");
        let r = js(&mut b, "document.createAttribute('two')");
        assert_eq!(r, "limit:Host(\"dom-names\")", "{r}");
        ok(&mut b, "document.createAttribute('one').name", "one");

        // The attribute-slot cap on the path of `setAttributeNode`.
        let doc = tree::parse(PAGE);
        let slots = doc.attr_slots();
        let lim = BrowserLimits { max_attr_slots: slots + 2, ..Default::default() };
        let mut b = browser_with(PAGE, lim);
        let r = js(&mut b, "var e = document.createElement('div'); var x1 = document.createAttribute('x1'), x2 = document.createAttribute('x2'), x3 = document.createAttribute('x3'); e.setAttributeNode(x1); e.setAttributeNode(x2); e.setAttributeNode(x3)");
        assert_eq!(r, "limit:Host(\"dom-attrs\")", "{r}");
        // (the native check counts the element's own attributes twice, so the second one is refused already)
        ok(&mut b, "[e.attributes.length, x2.ownerElement, x1.ownerElement === e, e.hasAttribute('x2')].join()", "1,,true,false");
    });
}

#[test]
fn steps_are_charged_and_limits_hold_for_large_maps() {
    big(|| {
        // A tight step budget stops a long attribute scan with a limit, not a hang.
        let doc = tree::parse(PAGE);
        let mut b = Browser::new(doc, BrowserLimits::default(), EvalLimits { max_steps: 20_000, ..Default::default() }, HeapLimits::default()).expect("browser");
        let r = js(&mut b, "var e = document.createElement('div'); for (var i = 0; i < 400; i++) e.setAttribute('k' + i, 'v'); var m = e.attributes; var n = 0; for (var r = 0; r < 100000; r++) { n += m.getNamedItem('k399').value.length; }");
        assert!(r.starts_with("limit:Steps"), "{r}");
    });
}
