//! Tests of `Document.getElementsByName` and the live `NodeList`. Expectations are by reading, not from a Node DOM oracle:
//! HTML Standard "getElementsByName" (live `NodeList`, HTML elements with a `name` attribute identical to the argument, tree
//! order, one required `DOMString`), DOM Standard "NodeList" (`length`, `item`, `iterable<Node>`), and WebIDL (an indexed
//! getter without a setter; an interface with a value iterator and an indexed getter gets the `Array.prototype` iteration
//! methods). Those expectations are manual and labelled as such.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const PAGE: &str = "<!doctype html><html><head><meta name=viewport content=a><title>t</title></head><body>\
<div id=d1 name=x></div><form id=f name=x><input id=i1 name=x><input id=i2 name=y></form>\
<p id=p1 NAME=x></p><span id=s1 name=X></span><a id=a1 name=\"\"></a><b id=b1></b>\
<svg id=sv name=x><foreignObject><div id=fo name=x></div></foreignObject></svg><math id=mm name=x></math>\
</body></html>";

fn browser_with(html: &str, lim: BrowserLimits, steps: u64) -> Browser {
    Browser::new(tree::parse(html), lim, EvalLimits { max_steps: steps, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn browser() -> Browser {
    browser_with(PAGE, BrowserLimits::default(), 5_000_000)
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

fn ok(b: &mut Browser, src: &str, want: &str) {
    assert_eq!(js(b, src), want, "{src}");
}

const IDS: &str = "function ids(l) { var r = []; for (var i = 0; i < l.length; i++) r.push(l[i].id); return r.join(); } ";

/// The failure that was seen in the cached bundle: Next's `AppRouterAnnouncer` (reduced, no shadow DOM).
#[test]
fn announcer_lookup_with_optional_chain_works_and_finds_the_element_later() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var t = document.getElementsByName('next-route-announcer')[0]; String(t && t.shadowRoot && t.shadowRoot.childNodes[0])", "undefined");
        ok(&mut b, "var t2 = document.getElementsByName('next-route-announcer')[0]; String(t2?.shadowRoot?.childNodes[0])", "undefined");
        ok(&mut b, "var a = document.createElement('next-route-announcer'); a.setAttribute('name', 'next-route-announcer'); document.body.appendChild(a); document.getElementsByName('next-route-announcer')[0] === a", "true");
        ok(&mut b, "typeof document.getElementsByName", "function");
    });
}

/// HTML Standard, by reading: HTML elements, `name` value identical (case-sensitive), tree order, attached only.
#[test]
fn membership_order_case_and_namespace() {
    big(|| {
        let mut b = browser();
        ok(&mut b, &format!("{IDS} ids(document.getElementsByName('x'))"), "d1,f,i1,p1,fo");
        ok(&mut b, &format!("{IDS} ids(document.getElementsByName('X'))"), "s1");
        ok(&mut b, &format!("{IDS} ids(document.getElementsByName('y'))"), "i2");
        ok(&mut b, &format!("{IDS} ids(document.getElementsByName('viewport'))"), "");
        ok(&mut b, "document.getElementsByName('viewport').length + ':' + document.getElementsByName('viewport')[0].tagName", "1:META");
        // The empty string matches only an empty `name`; an element with no `name` never matches.
        ok(&mut b, &format!("{IDS} ids(document.getElementsByName(''))"), "a1");
        ok(&mut b, "document.getElementsByName('nothing').length", "0");
        // An SVG or MathML element is not an HTML element, even with a `name` attribute; an HTML element inside foreignObject is.
        ok(&mut b, "var s = document.createElementNS('http://www.w3.org/2000/svg', 'g'); s.setAttribute('name', 'sv2'); document.body.appendChild(s); document.getElementsByName('sv2').length", "0");
        ok(&mut b, "var m = document.createElementNS('http://www.w3.org/1998/Math/MathML', 'mi'); m.setAttribute('name', 'sv3'); document.body.appendChild(m); document.getElementsByName('sv3').length", "0");
        // A namespaced attribute `name` is another attribute.
        ok(&mut b, "var q = document.createElement('div'); q.setAttributeNS('http://www.w3.org/XML/1998/namespace', 'xml:name', 'ns1'); document.body.appendChild(q); document.getElementsByName('ns1').length", "0");
        // Detached elements are not in the document.
        ok(&mut b, "var det = document.createElement('div'); det.setAttribute('name', 'det'); document.getElementsByName('det').length", "0");
    });
}

#[test]
fn argument_conversion_and_required_argument() {
    big(|| {
        let mut b = browser_with("<!doctype html><body><i id=n1 name=1></i><i id=nn name=null></i><i id=nu name=undefined></i><i id=o name=custom></i></body>", BrowserLimits::default(), 5_000_000);
        ok(&mut b, &format!("{IDS} [ids(document.getElementsByName(1)), ids(document.getElementsByName(null)), ids(document.getElementsByName(undefined))].join('|')"), "n1|nn|nu");
        ok(&mut b, &format!("{IDS} ids(document.getElementsByName({{ toString: function () {{ return 'custom'; }} }}))"), "o");
        ok(&mut b, "try { document.getElementsByName() } catch (e) { e.name }", "TypeError");
        ok(&mut b, "try { document.getElementsByName(Symbol()) } catch (e) { e.name }", "TypeError");
        ok(&mut b, "try { document.getElementsByName({ toString: function () { throw new RangeError('boom') } }) } catch (e) { e.name }", "RangeError");
        ok(&mut b, "try { document.getElementsByName('\\ud800') } catch (e) { 'threw' }", "unsupported:lone surrogate in a DOM string (the native document is UTF-8)");
        // Only a Document is `this`.
        ok(&mut b, "try { document.getElementsByName.call({}, 'x') } catch (e) { e.name }", "TypeError");
        ok(&mut b, "try { document.getElementsByName.call(document.body, 'x') } catch (e) { e.name }", "TypeError");
        ok(&mut b, "document.getElementsByName.length", "1");
    });
}

#[test]
fn a_held_list_is_live() {
    big(|| {
        let mut b = browser();
        ok(&mut b, &format!("{IDS} var L = document.getElementsByName('x'); ids(L)"), "d1,f,i1,p1,fo");
        // append at the end, insertBefore at the start (tree order, not insertion order)
        ok(&mut b, "var n1 = document.createElement('u'); n1.id = 'n1'; n1.name = 'x'; n1.setAttribute('name', 'x'); document.body.appendChild(n1); ids(L)", "d1,f,i1,p1,fo,n1");
        ok(&mut b, "var n0 = document.createElement('u'); n0.id = 'n0'; n0.setAttribute('name', 'x'); document.body.insertBefore(n0, document.body.firstChild); ids(L)", "n0,d1,f,i1,p1,fo,n1");
        ok(&mut b, "n0.remove(); ids(L) + '|' + L.length", "d1,f,i1,p1,fo,n1|6");
        // remove a parent: its descendants leave with it
        ok(&mut b, "var form = document.getElementById('f'); form.remove(); ids(L)", "d1,p1,fo,n1");
        // reparent changes the order
        ok(&mut b, "document.body.insertBefore(form, document.getElementById('p1')); ids(L)", "d1,f,i1,p1,fo,n1");
        ok(&mut b, "document.body.appendChild(document.getElementById('d1')); ids(L)", "f,i1,p1,fo,n1,d1");
        // rename and removal of the attribute
        ok(&mut b, "document.getElementById('p1').setAttribute('name', 'z'); ids(L)", "f,i1,fo,n1,d1");
        ok(&mut b, "document.getElementById('p1').setAttribute('name', 'x'); ids(L)", "f,i1,p1,fo,n1,d1");
        ok(&mut b, "document.getElementById('p1').removeAttribute('name'); ids(L)", "f,i1,fo,n1,d1");
        ok(&mut b, "document.getElementById('s1').setAttribute('name', 'x'); ids(L)", "f,i1,s1,fo,n1,d1");
        // attribute node paths
        ok(&mut b, "var an = document.getElementById('s1').getAttributeNode('name'); document.getElementById('s1').removeAttributeNode(an); ids(L)", "f,i1,fo,n1,d1");
        ok(&mut b, "document.getElementById('s1').setAttributeNode(an); ids(L)", "f,i1,s1,fo,n1,d1");
        ok(&mut b, "an.value = 'q'; ids(L)", "f,i1,fo,n1,d1");
        // other lists see the same document and are independent
        ok(&mut b, "var Q = document.getElementsByName('q'); ids(Q) + '|' + (Q === L) + '|' + (document.getElementsByName('q') === Q)", "s1|false|false");
        // an element moved out of the document and in again
        ok(&mut b, "var tmp = document.createElement('div'); tmp.appendChild(document.getElementById('fo')); ids(L)", "f,i1,n1,d1");
        ok(&mut b, "document.body.appendChild(tmp); ids(L)", "f,i1,n1,d1,fo");
        // same wrapper identity while it is in the list
        ok(&mut b, "L[0] === L[0] && L[0] === document.getElementById('f')", "true");
    });
}

#[test]
fn a_held_list_sees_changes_made_to_the_native_document() {
    big(|| {
        let mut b = browser();
        ok(&mut b, &format!("{IDS} var L = document.getElementsByName('x'); ids(L)"), "d1,f,i1,p1,fo");
        let doc = b.document();
        let (name, d1, p1, fo) = {
            let d = doc.borrow();
            let find = |id: &str| {
                let mut st = vec![d.root()];
                while let Some(n) = st.pop() {
                    if d.attr(n, bare_html::dom::Atom::ID) == Some(id) {
                        return n;
                    }
                    st.extend(d.children(n));
                }
                panic!("no #{id}")
            };
            (d.find_atom("name").expect("name atom"), find("d1"), find("p1"), find("fo"))
        };
        doc.borrow_mut().detach(d1);
        ok(&mut b, "ids(L)", "f,i1,p1,fo");
        let body = doc.borrow().parent(p1).unwrap();
        doc.borrow_mut().move_to(d1, body, None);
        ok(&mut b, "ids(L)", "f,i1,p1,fo,d1");
        doc.borrow_mut().set_attr_atom(p1, name, "other");
        ok(&mut b, "ids(L)", "f,i1,fo,d1");
        doc.borrow_mut().set_attr(p1, "name", "x");
        ok(&mut b, "ids(L)", "f,i1,p1,fo,d1");
        assert!(doc.borrow_mut().remove_attr_atom(fo, name));
        ok(&mut b, "ids(L)", "f,i1,p1,d1");
        let e = doc.borrow_mut().insert_element(body, None, "em", &[("name", "x"), ("id", "ne")]);
        let _ = e;
        ok(&mut b, "ids(L)", "f,i1,p1,d1,ne");
        // the length read and an index read after a native change agree
        doc.borrow_mut().detach(d1);
        ok(&mut b, "L.length + ':' + (L[4] === undefined) + ':' + L.item(4) + ':' + (3 in L) + ':' + (4 in L)", "4:true:null:true:false");
    });
}

#[test]
fn empty_list_and_item() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var E = document.getElementsByName('no-such'); [E.length, E[0], E.item(0), E.item(-1), 0 in E, E['0'], E.item(1e10)].map(String).join()", "0,undefined,null,null,false,undefined,null");
        ok(&mut b, "var L = document.getElementsByName('y'); [L.item(0).id, L.item(4294967296).id, String(L.item(1)), L.item(NaN).id, L.item('0').id, L.item(0.9).id, L.item(Infinity).id].join()", "i2,i2,null,i2,i2,i2,i2");
        ok(&mut b, "try { L.item() } catch (e) { e.name }", "TypeError");
        ok(&mut b, "L.item.length + ':' + typeof L.item", "1:function");
        ok(&mut b, "var o = { valueOf: function () { document.body.appendChild(document.getElementById('i2').cloneNode(true)); return 1; } }; String(document.getElementsByName('y').item(o) && 1)", "1");
    });
}

#[test]
fn indexed_properties_are_read_only_and_length_is_an_accessor() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var L = document.getElementsByName('y'); L.length = 7; L[0] = 1; L[5] = 1; delete L[0]; [L.length, L[0].id, L[5]].join()", "1,i2,");
        ok(&mut b, "'use strict'; try { L.length = 9 } catch (e) { e.name }", "TypeError");
        ok(&mut b, "'use strict'; try { L[0] = 9 } catch (e) { e.name }", "TypeError");
        ok(&mut b, "'use strict'; try { L[3] = 9 } catch (e) { e.name }", "TypeError");
        ok(&mut b, "'use strict'; try { delete L[0] } catch (e) { e.name }", "TypeError");
        ok(&mut b, "var d = Object.getOwnPropertyDescriptor(L, '0'); [d.writable, d.enumerable, d.configurable, d.value === L[0]].join()", "false,true,true,true");
        ok(&mut b, "var d2 = Object.getOwnPropertyDescriptor(NodeList.prototype, 'length'); [typeof d2.get, d2.set, d2.enumerable, d2.configurable].join()", "function,,true,true");
        ok(&mut b, "Object.keys(L).join() + '|' + Object.getOwnPropertyNames(L).join()", "0|0");
        // `for in` also lists the enumerable prototype members (WebIDL operations and `length` are enumerable); the index comes first.
        ok(&mut b, "var seen = []; for (var k in L) seen.push(k); seen[0] + ':' + seen.indexOf('length') + ':' + (seen.indexOf('forEach') > 0)", "0:1:true");
        ok(&mut b, "Object.getPrototypeOf(L) === NodeList.prototype && L instanceof NodeList", "true");
        ok(&mut b, "document.getElementsByName('x').constructor === NodeList && NodeList.name + NodeList.length", "NodeList0");
    });
}

#[test]
fn brand_checks_and_illegal_constructor() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "try { new NodeList() } catch (e) { e.name + ':' + e.message }", "TypeError:Illegal constructor");
        ok(&mut b, "try { NodeList() } catch (e) { e.name }", "TypeError");
        ok(&mut b, "var g = Object.getOwnPropertyDescriptor(NodeList.prototype, 'length').get; try { g.call({}) } catch (e) { e.name + ':' + e.message }", "TypeError:Illegal invocation");
        ok(&mut b, "try { g.call(document.body.attributes) } catch (e) { e.name }", "TypeError");
        ok(&mut b, "try { NodeList.prototype.length } catch (e) { e.name }", "TypeError");
        ok(&mut b, "try { NodeList.prototype.item.call({}, 0) } catch (e) { e.name + ':' + e.message }", "TypeError:Illegal invocation");
        ok(&mut b, "try { NodeList.prototype.item.call(document.body.attributes, 0) } catch (e) { e.name }", "TypeError");
        ok(&mut b, "try { NodeList.prototype.item.call(document.getElementsByName('y')) } catch (e) { e.name }", "TypeError");
        // A plain object with the prototype of a list is not a list.
        ok(&mut b, "try { Object.create(NodeList.prototype).length } catch (e) { e.name }", "TypeError");
        ok(&mut b, "try { Object.create(document.getElementsByName('y')).length } catch (e) { e.name }", "TypeError");
    });
}

/// WebIDL, by reading: a value iterator on an interface with an indexed getter uses the `Array.prototype` methods.
#[test]
fn iteration_methods_are_the_array_ones_and_follow_the_live_list() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "[NodeList.prototype.forEach === Array.prototype.forEach, NodeList.prototype.entries === Array.prototype.entries, NodeList.prototype.keys === Array.prototype.keys, NodeList.prototype.values === Array.prototype.values, NodeList.prototype[Symbol.iterator] === Array.prototype.values].join()", "true,true,true,true,true");
        ok(&mut b, &format!("{IDS} var L = document.getElementsByName('x'); var s = []; L.forEach(function (e, i, l) {{ s.push(i + e.id + (l === L)); }}); s.join()"), "0d1true,1ftrue,2i1true,3p1true,4fotrue");
        ok(&mut b, "var t = []; for (var e of L) t.push(e.id); t.join()", "d1,f,i1,p1,fo");
        ok(&mut b, "var u = []; for (var [i, e] of L.entries()) u.push(i + e.id); for (var k of L.keys()) u.push(k); u.join()", "0d1,1f,2i1,3p1,4fo,0,1,2,3,4");
        ok(&mut b, "Array.from(L).length + ':' + Array.prototype.slice.call(L, 1, 3).map(function (e) { return e.id }).join() + ':' + [...L].length", "5:f,i1:5");
        // for-of over a live list: an element added during iteration is visited (the iterator reads the length each step);
        // an element removed ahead of the cursor is skipped. Array iterator semantics (ECMAScript), by reading.
        ok(&mut b, "var V = document.getElementsByName('y'); var seen = []; for (var e of V) { seen.push(e.id); if (seen.length === 1) { var a = document.createElement('i'); a.id = 'late'; a.setAttribute('name', 'y'); document.body.appendChild(a); } if (seen.length > 5) break; } seen.join()", "i2,late");
        ok(&mut b, "var W = document.getElementsByName('y'); var seen2 = []; for (var e of W) { seen2.push(e.id); if (seen2.length === 1) document.getElementById('late').remove(); } seen2.join()", "i2");
        // forEach reads the length once, then checks each index: an addition is not visited, a removal ahead is skipped.
        ok(&mut b, "var F = document.getElementsByName('y'); var seen3 = []; F.forEach(function (e) { seen3.push(e.id); if (seen3.length === 1) { var z = document.createElement('i'); z.id = 'z'; z.setAttribute('name', 'y'); document.body.appendChild(z); } }); seen3.join() + '|' + F.length", "i2|2");
        ok(&mut b, "var G = document.getElementsByName('y'); var seen4 = []; G.forEach(function (e) { seen4.push(e.id); document.getElementById('z').remove(); }); seen4.join() + '|' + G.length", "i2|1");
        // a callback that throws leaves the list usable
        ok(&mut b, "try { G.forEach(function () { throw new RangeError('x') }) } catch (e) { e.name + G.length }", "RangeError1");
        // forEach with a non-callable is the Array.prototype TypeError
        ok(&mut b, "try { G.forEach(1) } catch (e) { e.name }", "TypeError");
        // recursion through the callback is stopped by the depth guard, not by a crash
        let r = js(&mut b, "var H = document.getElementsByName('y'); function rec() { H.forEach(rec); } rec();");
        assert!(r.starts_with("limit:") || r.starts_with("throw:RangeError"), "{r}");
    });
}

#[test]
fn collector_keeps_a_held_list_and_prunes_dropped_ones() {
    big(|| {
        let mut b = browser();
        ok(&mut b, &format!("{IDS} var keep = document.getElementsByName('x'); keep.mark = 1; ids(keep)"), "d1,f,i1,p1,fo");
        for _ in 0..3 {
            b.collect(&[]).expect("collect");
        }
        ok(&mut b, "keep.mark + ':' + ids(keep) + ':' + keep.length", "1:d1,f,i1,p1,fo:5");
        // many dropped lists, then a collection, then new lists with other names: no aliasing of a reused slot
        ok(&mut b, "for (var i = 0; i < 100; i++) { document.getElementsByName('y').length; } 1", "1");
        b.collect(&[]).expect("collect");
        ok(&mut b, "var N = document.getElementsByName('X'); var Y = document.getElementsByName('y'); [N.length, N[0].id, Y.length, Y[0].id, keep.length].join()", "1,s1,1,i2,5");
        // a held wrapper of an element that left the list stays valid
        ok(&mut b, "var d1 = keep[0]; d1.remove(); b2 = keep.length; [b2, d1.getAttribute('name'), keep[0].id].join()", "4,x,f");
        b.collect(&[]).expect("collect");
        ok(&mut b, "document.body.appendChild(d1); [keep.length, keep[keep.length - 1] === d1].join()", "5,true");
    });
}

#[test]
fn two_browsers_do_not_share_lists() {
    big(|| {
        let mut a = browser();
        let mut b = browser_with("<!doctype html><body><p id=only name=x></p></body>", BrowserLimits::default(), 5_000_000);
        ok(&mut a, &format!("{IDS} var L = document.getElementsByName('x'); ids(L)"), "d1,f,i1,p1,fo");
        ok(&mut b, &format!("{IDS} var L = document.getElementsByName('x'); ids(L)"), "only");
        ok(&mut a, "document.getElementById('d1').remove(); ids(L)", "f,i1,p1,fo");
        ok(&mut b, "ids(L) + L.length", "only1");
        let (wa, wb) = (std::rc::Rc::downgrade(&a.dom), std::rc::Rc::downgrade(&b.dom));
        drop(a);
        assert!(wa.upgrade().is_none() && wb.upgrade().is_some());
        ok(&mut b, "document.body.appendChild(document.createElement('br')); L.length", "1");
        drop(b);
        assert!(wb.upgrade().is_none());
    });
}

#[test]
fn a_borrowed_document_stops_the_script_instead_of_an_empty_list() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var L = document.getElementsByName('x'); L.length", "5");
        let doc = b.document();
        let _guard = doc.borrow_mut();
        let r = js(&mut b, "L[0]");
        assert!(r.starts_with("other:") || r.starts_with("unsupported:") || r.starts_with("throw"), "{r}");
        assert_ne!(r, "undefined");
    });
}

#[test]
fn limits_fire_before_the_scan_and_cannot_be_caught() {
    big(|| {
        // result cap: a list with more members than `max_results` is a limit, never a short list
        let lim = BrowserLimits { max_results: 8, ..Default::default() };
        let mut b = browser_with("<!doctype html><body></body>", lim, 5_000_000);
        ok(&mut b, "for (var i = 0; i < 8; i++) { var e = document.createElement('i'); e.setAttribute('name', 'm'); document.body.appendChild(e); } document.getElementsByName('m').length", "8");
        ok(&mut b, "var e = document.createElement('i'); e.setAttribute('name', 'm'); document.body.appendChild(e); 1", "1");
        assert_eq!(js(&mut b, "var fin = 0, caught = 0; try { document.getElementsByName('m').length } catch (e) { caught = 1 } finally { fin = 1 }"), "limit:Host(\"dom-results\")");
        ok(&mut b, "[caught, fin].join()", "0,0");
        assert_eq!(js(&mut b, "document.getElementsByName('m')[0]"), "limit:Host(\"dom-results\")");
        // a name that matches nothing is still fine
        ok(&mut b, "document.getElementsByName('zz').length", "0");
    });
}

#[test]
fn step_budget_is_charged_for_the_scan_and_stops_it() {
    big(|| {
        let mut html = String::from("<!doctype html><body>");
        for i in 0..2000 {
            html.push_str(&format!("<i name=n{}></i>", i % 7));
        }
        let mut b = browser_with(&html, BrowserLimits::default(), 30_000);
        // a rebuild is charged for the nodes it walks, so a loop of rebuilds hits the budget
        let r = js(&mut b, "var L = document.getElementsByName('n1'); var t = 0; for (var i = 0; i < 100; i++) { document.body.appendChild(document.createElement('b')); t += L.length; }");
        assert!(r.starts_with("limit:Steps"), "{r}");
        // no catch or finally runs after the limit
        let mut b = browser_with(&html, BrowserLimits::default(), 30_000);
        assert!(js(&mut b, "var flag = 0, fin = 0; var L = document.getElementsByName('n1'); try { for (var i = 0; i < 100; i++) { document.body.appendChild(document.createElement('b')); L.length; } } catch (e) { flag = 1 } finally { fin = 1 }").starts_with("limit:Steps"));
    });
}

#[test]
fn an_unchanged_document_is_not_scanned_again() {
    big(|| {
        let mut html = String::from("<!doctype html><body>");
        for i in 0..2000 {
            html.push_str(&format!("<i name=n{}></i>", i % 7));
        }
        // 2000 nodes: one scan costs ~2000 steps; 5000 reads of a fresh list stay far below 5000 * 2000.
        let mut b = browser_with(&html, BrowserLimits::default(), 400_000);
        ok(&mut b, "var L = document.getElementsByName('n1'); var t = 0; for (var i = 0; i < 5000; i++) { t += L.length + (L[0] ? 1 : 0); } t", &(5000 * (286 + 1)).to_string());
    });
}

#[test]
fn pending_promise_job_that_hits_a_limit_is_not_replayed() {
    big(|| {
        let lim = BrowserLimits { max_results: 2, ..Default::default() };
        let mut b = browser_with("<!doctype html><body><i name=m></i><i name=m></i><i name=m></i></body>", lim, 5_000_000);
        ok(&mut b, "var runs = 0; var L = document.getElementsByName('m'); Promise.resolve().then(function () { runs++; return L.length; }).catch(function () { runs += 100; }); 1", "1");
        let r = b.drain();
        assert!(matches!(r, Err(Error::Limit(_))), "{r:?}");
        // The engine keeps a failed checkpoint failed (`failed_job`): the same error again, and the job does not run twice.
        let again = b.drain();
        assert!(matches!(again, Err(Error::Limit(_))), "{again:?}");
        ok(&mut b, "runs", "1");
    });
}

// ---- part 2: tests that use the stats API and the heap callbacks directly (they need `nodelist_stats`) ----

use bare_js::heap::{HeapError, IndexedSource, Slot};

fn list_obj(b: &Browser, v: &str) -> bare_js::value::ObjId {
    let _ = v;
    let t = b.dom.nl.s.borrow();
    *t.lists.keys().next().expect("one list")
}

#[test]
fn heap_callbacks_never_scan_and_answer_needs_host_when_stale() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var L = document.getElementsByName('x'); 1", "1");
        let o = list_obj(&b, "L");
        // never built: stale. Both callbacks ask for the host and the document is not scanned (no step is charged: no Interp).
        assert_eq!(b.dom.indexed_len(o), Err(HeapError::NeedsHost { owner: o, index: 0 }));
        assert_eq!(b.dom.indexed_item(o, 3), Err(HeapError::NeedsHost { owner: o, index: 3 }));
        ok(&mut b, "L.length", "5");
        assert_eq!(b.dom.indexed_len(o), Ok(5));
        assert_eq!(b.dom.indexed_item(o, 7), Ok(Slot::Absent));
        // index 0 has no wrapper until it is read: `Missing`, then `Ready`
        assert!(matches!(b.dom.indexed_item(o, 0), Ok(Slot::Missing) | Ok(Slot::Ready(_))));
        ok(&mut b, "L[0].id", "d1");
        assert!(matches!(b.dom.indexed_item(o, 0), Ok(Slot::Ready(_))));
        // a native change makes it stale again, with no JS running
        let doc = b.document();
        let n = doc.borrow_mut().create_element("u");
        let body = {
            let d = doc.borrow();
            d.children(d.children(d.root()).last().unwrap()).last().unwrap()
        };
        doc.borrow_mut().move_to(n, body, None);
        assert_eq!(b.dom.indexed_len(o), Err(HeapError::NeedsHost { owner: o, index: 0 }));
        // a borrowed document is an error, never a short answer
        let g = doc.borrow_mut();
        assert_eq!(b.dom.indexed_len(o), Err(HeapError::HostUnavailable));
        assert_eq!(b.dom.indexed_item(o, 0), Err(HeapError::HostUnavailable));
        drop(g);
        // an object that is not a list is not answered by this module
        assert!(crate::browser_nodelist::len(&b.dom, o).is_some());
    });
}

#[test]
fn stale_list_works_with_has_property_keys_and_for_in_without_a_prior_length_read() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var L = document.getElementsByName('y'); [0 in L, 1 in L, Object.keys(L).join(), JSON.stringify(Object.keys(L))].join('|')", "true|false|0|[\"0\"]");
        ok(&mut b, "document.body.appendChild(Object.assign(document.createElement('i'), {})); var a = document.createElement('i'); a.setAttribute('name', 'y'); document.body.appendChild(a); [1 in L, Object.keys(L).join()].join('|')", "true|0,1");
        ok(&mut b, "document.body.removeChild(a); var ks = []; for (var k in L) { if (/^\\d+$/.test(k)) ks.push(k); } ks.join()", "0");
        ok(&mut b, "a.parentNode === null && Object.getOwnPropertyNames(L).join()", "0");
        ok(&mut b, "document.body.appendChild(a); String(Object.entries(L).length) + [...L].length + Array.from(L, function (e) { return e.id; }).join('')", "22i2");
    });
}

#[test]
fn json_of_a_fresh_list_works_and_a_stale_one_is_a_visible_error_never_wrong() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var L = document.getElementsByName('y'); L.length; typeof JSON.stringify(L)", "string");
        ok(&mut b, "document.body.appendChild(document.createElement('hr')); L.length; typeof JSON.stringify(L)", "string");
        let r = js(&mut b, "var a = document.createElement('i'); a.setAttribute('name', 'y'); document.body.appendChild(a); JSON.stringify(L)");
        // Not an empty or short result: either a valid string or an explicit error.
        assert!(r.starts_with("other:") || r.starts_with("unsupported:") || r.starts_with("throw") || r.starts_with('{') || r.starts_with('"'), "{r}");
    });
}

#[test]
fn tables_are_pruned_and_slots_are_not_aliased() {
    big(|| {
        let mut b = browser();
        ok(&mut b, "var keep = document.getElementsByName('x'); keep.length; var junk = []; for (var i = 0; i < 50; i++) junk.push(document.getElementsByName(i % 2 ? 'y' : 'X')); junk.forEach(function (l) { l.length; }); 1", "1");
        let before = b.nodelist_stats();
        assert_eq!(before.lists, 51, "{before:?}");
        assert!(before.cached_ids >= 5 + 25 + 25 && before.name_bytes >= 51, "{before:?}");
        ok(&mut b, "junk = null; 1", "1");
        b.collect(&[]).expect("collect");
        let after = b.nodelist_stats();
        assert_eq!((after.lists, after.cached_ids, after.name_bytes), (1, 5, 1), "{after:?}");
        // freed object slots are reused by new lists of other names: they start with their own name and no cached ids
        ok(&mut b, "var N = []; for (var i = 0; i < 60; i++) N.push(document.getElementsByName(i % 2 ? 'nothing' : 'x')); N.length", "60");
        ok(&mut b, "[N[0].length, N[1].length, N[58].length, N[59].length, keep.length].join()", "5,0,5,0,5");
        let s = b.nodelist_stats();
        assert_eq!(s.lists, 61, "{s:?}");
        ok(&mut b, "N = null; keep = null; 1", "1");
        b.collect(&[]).expect("collect");
        assert_eq!(b.nodelist_stats().lists, 0);
    });
}

#[test]
fn list_objects_name_bytes_and_cached_ids_are_capped_before_growth() {
    big(|| {
        let lim = BrowserLimits { max_wrappers: 40, ..Default::default() };
        let mut b = browser_with(PAGE, lim, 5_000_000);
        let r = js(&mut b, "var all = []; for (var i = 0; i < 100; i++) all.push(document.getElementsByName('x')); all.length");
        assert_eq!(r, "limit:Host(\"dom-wrappers\")", "{r}");
        assert!(b.nodelist_stats().lists <= 40);
        // the name-bytes cap
        let mut b = browser();
        let r = js(&mut b, "var big = 'n'; for (var j = 0; j < 19; j++) big += big; big = big.slice(0, 400000); var keep = []; for (var i = 0; i < 4; i++) keep.push(document.getElementsByName(big)); keep.length");
        assert_eq!(r, "limit:Host(\"dom-nodelist-names\")", "{r}");
        assert_eq!(b.nodelist_stats().lists, 2);
        // the cached-ids cap (4 * max_results), checked before the vector grows
        let lim = BrowserLimits { max_results: 4, ..Default::default() };
        let mut b = browser_with("<!doctype html><body><i name=m></i><i name=m></i><i name=m></i></body>", lim, 5_000_000);
        let r = js(&mut b, "var ls = []; for (var i = 0; i < 10; i++) { var l = document.getElementsByName('m'); ls.push(l); l.length; } ls.length");
        assert_eq!(r, "limit:Host(\"dom-nodelist-nodes\")", "{r}");
        // 5 lists of 3 fit under the cap of 16; the sixth fails after its first push and keeps no ids.
        assert_eq!(b.nodelist_stats().cached_ids, 15);
    });
}

#[test]
fn a_limit_during_a_rebuild_leaves_the_old_cache_and_the_script_stopped() {
    big(|| {
        let lim = BrowserLimits { max_results: 3, ..Default::default() };
        let mut b = browser_with("<!doctype html><body><i name=m></i><i name=m></i></body>", lim, 5_000_000);
        ok(&mut b, "var L = document.getElementsByName('m'); L.length", "2");
        ok(&mut b, "for (var i = 0; i < 2; i++) { var e = document.createElement('i'); e.setAttribute('name', 'm'); document.body.appendChild(e); } 1", "1");
        assert_eq!(js(&mut b, "L.length"), "limit:Host(\"dom-results\")");
        assert_eq!(b.nodelist_stats().cached_ids, 2);
        ok(&mut b, "document.body.lastChild.remove(); L.length", "3");
    });
}

#[test]
fn object_to_string_and_structured_clone_treat_a_list_as_a_host_object() {
    big(|| {
        let mut b = browser();
        b.set_document_url("https://example.test/p").unwrap();
        ok(&mut b, "Object.prototype.toString.call(document.getElementsByName('x'))", "[object NodeList]");
        ok(&mut b, "try { history.pushState(document.getElementsByName('x'), ''); 'cloned' } catch (e) { e.name }", "DataCloneError");
        ok(&mut b, "try { history.pushState({ a: document.getElementsByName('x') }, ''); 'cloned' } catch (e) { e.name }", "DataCloneError");
    });
}
