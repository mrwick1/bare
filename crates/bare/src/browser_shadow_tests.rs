//! Tests of the shadow root tree foundation (`Element.attachShadow`, `ShadowRoot`, `DocumentFragment` brand). Expectations are written by
//! hand from the DOM Standard (shadow trees, host-including ancestors, `getRootNode`, `isConnected`, `attach a shadow root`) and the
//! HTML Standard (valid shadow host names). No browser and no Node DOM was used: nothing here is browser-verified. The `announcer`
//! test copies the SHAPE of the Next.js `AppRouterAnnouncer` code that was read in a saved bundle (read, never run).

use crate::browser::*;
use bare_html::dom::NodeData;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

const PAGE: &str = "<!doctype html><html><head></head><body><div id=a><p id=lp>light</p></div></body></html>";

fn browser_with(page: &str, lim: BrowserLimits, steps: u64) -> Browser {
    let eval = EvalLimits { max_steps: steps, ..Default::default() };
    Browser::new(tree::parse(page), lim, eval, HeapLimits::default()).expect("browser")
}

fn browser() -> Browser {
    browser_with(PAGE, BrowserLimits::default(), 5_000_000)
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Throw(v)) => format!("throw-value:{}", b.it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(_)) => "unsupported".to_string(),
        Err(e) => format!("other:{e:?}"),
    }
}

fn lim(name: &'static str) -> String {
    format!("limit:{:?}", LimitKind::Host(name))
}

// ------------------------------------------------------------------------------------------ announcer shape

#[test]
fn next_route_announcer_shape_works_end_to_end() {
    let mut b = browser();
    // The shape of AppRouterAnnouncer: create a custom-named host, attach an open root, append a child to the root, append the host to the body, read the
    // root again later through `host.shadowRoot.childNodes[0]`, and remove the host on cleanup. (`getElementsByName` is another feature.)
    let out = js(
        &mut b,
        "var u = 'next-route-announcer';
         var e = document.createElement(u); e.style.cssText = 'position:absolute';
         var t = document.createElement('div'); t.ariaLive = 'assertive'; t.id = '__next-route-announcer__'; t.role = 'alert';
         e.attachShadow({mode: 'open'}).appendChild(t);
         document.body.appendChild(e);
         var found = document.getElementsByTagName(u)[0], r = [];
         r.push(found === e);
         r.push(!!(found.shadowRoot && found.shadowRoot.childNodes[0]));
         r.push(found.shadowRoot.childNodes[0] === t);
         r.push(t.isConnected, t.parentNode === found.shadowRoot, found.childNodes.length);
         if (found.isConnected) document.body.removeChild(found);
         r.push(e.isConnected, t.isConnected, e.shadowRoot.childNodes[0] === t, document.getElementsByTagName(u).length);
         r.join()",
    );
    assert_eq!(out, "true,true,true,true,true,0,false,false,true,0");
}

// ------------------------------------------------------------------------------------------ separate trees

#[test]
fn light_and_shadow_trees_are_separate() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var h = document.createElement('div'); h.appendChild(document.createElement('b')).textContent = 'light';
         var root = h.attachShadow({mode: 'open'}); var s = root.appendChild(document.createElement('i')); s.textContent = 'shadow';
         [h.childNodes.length, h.children.length, h.firstChild.nodeName, h.lastChild.nodeName, h.textContent, h.innerHTML, h.hasChildNodes(),
          root.childNodes.length, root.firstChild === s, root.textContent, s.parentNode === root, s.parentElement, root.parentNode, root.parentElement,
          root.previousSibling, root.nextSibling, h.contains(s), root.contains(s), s.getRootNode() === root, h.firstChild.getRootNode() === h].join()",
    );
    assert_eq!(out, "1,1,B,B,light,<b>light</b>,true,1,true,shadow,true,,,,,,false,true,true,true");
}

#[test]
fn light_text_content_and_inner_html_writes_leave_the_shadow_root_alone() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var h = document.createElement('div'), root = h.attachShadow({mode: 'open'}); root.appendChild(document.createElement('i'));
         h.textContent = 'x'; h.innerHTML = '<p>y</p>'; var a = [h.shadowRoot === root, root.childNodes.length, h.innerHTML];
         h.appendChild(document.createElement('u')); h.removeChild(h.firstChild); a.push(root.childNodes.length, h.shadowRoot.firstChild.nodeName);
         a.join()",
    );
    assert_eq!(out, "true,1,<p>y</p>,1,I");
}

// ------------------------------------------------------------------------------------------ shape, brands, access

#[test]
fn interfaces_prototypes_and_illegal_constructors() {
    let mut b = browser();
    assert_eq!(
        js(
            &mut b,
            "[typeof DocumentFragment, typeof ShadowRoot, Object.getPrototypeOf(DocumentFragment.prototype) === Node.prototype,
              Object.getPrototypeOf(ShadowRoot.prototype) === DocumentFragment.prototype, ShadowRoot.prototype.constructor === ShadowRoot,
              Node.DOCUMENT_FRAGMENT_NODE, Node.prototype.DOCUMENT_FRAGMENT_NODE, typeof Element.prototype.attachShadow, Element.prototype.attachShadow.length,
              Element.prototype.attachShadow.name, 'shadowRoot' in Element.prototype, 'shadowRoot' in Document.prototype, 'attachShadow' in Document.prototype].join()"
        ),
        "function,function,true,true,true,11,11,function,1,attachShadow,true,false,false"
    );
    for src in ["new ShadowRoot()", "ShadowRoot()", "DocumentFragment()"] {
        let out = js(&mut b, src);
        assert!(out.starts_with("throw:TypeError:"), "{src}: {out}");
    }
}

#[test]
fn shadow_root_object_shape_open_mode() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var h = document.createElement('div'), root = h.attachShadow({mode: 'open'});
         [h.shadowRoot === root, root instanceof ShadowRoot, root instanceof DocumentFragment, root instanceof Node, root instanceof Element, root instanceof EventTarget,
          root.nodeType, root.nodeName, root.nodeValue, root.mode, root.host === h, root.ownerDocument === document, root.isConnected, root.getRootNode() === root,
          root.delegatesFocus, root.clonable, root.serializable, root.slotAssignment, typeof root.remove, typeof root.innerHTML, typeof root.getElementById,
          typeof root.querySelector, typeof root.children, root.childElementCount, root.firstElementChild, root.lastElementChild].join()",
    );
    assert_eq!(out, "true,true,true,true,false,true,11,#document-fragment,,open,true,true,false,true,false,false,false,named,undefined,string,function,function,object,0,,");
}

#[test]
fn closed_mode_hides_the_root_from_the_host_but_the_saved_root_works() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var h = document.createElement('p'), root = h.attachShadow({mode: 'closed'}); var c = root.appendChild(document.createElement('span'));
         [h.shadowRoot, root.mode, root.host === h, root instanceof ShadowRoot, root.firstChild === c, root.childNodes.length, root.querySelector('span') === c,
          c.getRootNode() === root, c.getRootNode({composed: true}) === h, c.parentNode === root, root.removeChild(c) === c, root.childNodes.length].join()",
    );
    assert_eq!(out, ",closed,true,true,true,1,true,true,true,true,true,0");
}

#[test]
fn brand_checks_reject_foreign_receivers() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var h = document.createElement('div'), root = h.attachShadow({mode: 'open'}), bad = 0, n = 0;
         var hostG = Object.getOwnPropertyDescriptor(ShadowRoot.prototype, 'host').get, modeG = Object.getOwnPropertyDescriptor(ShadowRoot.prototype, 'mode').get;
         var srG = Object.getOwnPropertyDescriptor(Element.prototype, 'shadowRoot').get;
         [[hostG, document], [hostG, h], [hostG, {}], [hostG, null], [modeG, document.body], [modeG, window], [srG, document], [srG, root], [srG, {}], [srG, window],
          [Element.prototype.attachShadow, document], [Element.prototype.attachShadow, root], [Element.prototype.attachShadow, {}], [Element.prototype.attachShadow, null]].forEach(function (c) {
           n++; try { c[0].call(c[1], {mode: 'open'}); } catch (e) { if (e instanceof TypeError) bad++; }
         });
         [n, bad].join()",
    );
    assert_eq!(out, "14,14");
}

#[test]
fn shadow_root_getter_is_null_without_a_root_and_for_foreign_elements() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var svg = document.createElementNS('http://www.w3.org/2000/svg', 'g');
         [document.body.shadowRoot, document.createElement('x-y').shadowRoot, svg.shadowRoot, document.createTextNode('t').shadowRoot].map(String).join()",
    );
    assert_eq!(out, "null,null,null,undefined");
}

// ------------------------------------------------------------------------------------------ options

#[test]
fn options_are_read_in_webidl_order_and_arguments_are_checked() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var log = [], h = document.createElement('div');
         h.attachShadow({get slotAssignment() { log.push('slotAssignment'); return 'named'; }, get serializable() { log.push('serializable'); return false; },
                         get mode() { log.push('mode'); return 'open'; }, get delegatesFocus() { log.push('delegatesFocus'); return false; },
                         get clonable() { log.push('clonable'); return false; }});
         log.join()",
    );
    assert_eq!(out, "clonable,delegatesFocus,mode,serializable,slotAssignment");
    for (src, ok) in [
        ("document.createElement('div').attachShadow()", false),
        ("document.createElement('div').attachShadow(undefined)", false),
        ("document.createElement('div').attachShadow(null)", false),
        ("document.createElement('div').attachShadow({})", false),
        ("document.createElement('div').attachShadow('open')", false),
        ("document.createElement('div').attachShadow({mode: 'OPEN'})", false),
        ("document.createElement('div').attachShadow({mode: undefined})", false),
        ("document.createElement('div').attachShadow({mode: 'open', slotAssignment: 'x'})", false),
        ("document.createElement('div').attachShadow({mode: 'open', slotAssignment: 'named'}).mode", true),
        ("document.createElement('div').attachShadow({mode: 'open', delegatesFocus: false, clonable: 0, serializable: ''}).mode", true),
        ("document.createElement('div').attachShadow({mode: {toString: function () { return 'closed'; }}}).mode", true),
    ] {
        let out = js(&mut b, src);
        assert_eq!(out.starts_with("throw:TypeError:"), !ok, "{src}: {out}");
    }
}

#[test]
fn a_failing_option_getter_or_enum_attaches_nothing() {
    let mut b = browser();
    let before = b.stats().nodes;
    let out = js(
        &mut b,
        "var h = document.createElement('div'), r = [];
         try { h.attachShadow({get mode() { throw new RangeError('boom'); }}); } catch (e) { r.push(e.name + ':' + e.message); }
         try { h.attachShadow({mode: 'nope'}); } catch (e) { r.push(e.name); }
         try { h.attachShadow({mode: 'open', get delegatesFocus() { throw 7; }}); } catch (e) { r.push(e); }
         r.push(h.shadowRoot); r.join()",
    );
    assert_eq!(out, "RangeError:boom,TypeError,7,");
    assert_eq!(b.stats().nodes, before + 1, "only the host element was made");
}

#[test]
fn option_getters_may_change_the_host_and_the_checks_run_after_them() {
    let mut b = browser();
    // A getter attaches the root itself: the outer call then finds a duplicate (the host is read after the options).
    let out = js(
        &mut b,
        "var h = document.createElement('div'), inner; var r = [];
         try { h.attachShadow({get mode() { inner = h.attachShadow({mode: 'open'}); return 'open'; }}); r.push('no'); } catch (e) { r.push(e.name); }
         r.push(h.shadowRoot === inner, inner instanceof ShadowRoot); r.join()",
    );
    assert_eq!(out, "NotSupportedError,true,true");
    // A getter that moves the host or fills it does not break the call.
    let out = js(
        &mut b,
        "var h2 = document.createElement('section'), holder = document.createElement('div');
         var root = h2.attachShadow({get mode() { holder.appendChild(h2); h2.appendChild(document.createElement('b')); return 'open'; }});
         [root.host === h2, h2.parentNode === holder, h2.childNodes.length, root.getRootNode() === root, root.getRootNode({composed: true}) === holder].join()",
    );
    assert_eq!(out, "true,true,1,true,true");
    // A `toString` of the mode string runs JS too, with no borrow held: it may build a lot of DOM.
    let out = js(
        &mut b,
        "var h3 = document.createElement('p'); var root3 = h3.attachShadow({mode: {toString: function () { for (var i = 0; i < 20; i++) h3.appendChild(document.createElement('i')); document.body.appendChild(h3); return 'open'; }}});
         [root3.mode, h3.childNodes.length, h3.isConnected, root3.isConnected].join()",
    );
    assert_eq!(out, "open,20,true,true");
}

#[test]
fn unsupported_option_values_are_refused_before_anything_changes() {
    let mut b = browser();
    for opts in ["{mode: 'open', slotAssignment: 'manual'}"] {
        let before = b.stats().nodes;
        let out = js(&mut b, &format!("var h = document.createElement('div'); try {{ h.attachShadow({opts}); 'attached' }} catch (e) {{ 'caught' }}"));
        // `Unsupported` is not a JS exception: `catch` cannot see it, and the run ends with it.
        assert_eq!(out, "unsupported", "{opts}");
        assert_eq!(b.stats().nodes, before + 1, "{opts}: only the host was made");
        assert_eq!(js(&mut b, "h.shadowRoot === null && !('x' in h)"), "true", "{opts}");
        assert_eq!(js(&mut b, "try { h.attachShadow({mode: 'open'}).mode } catch (e) { 'dup' }"), "open", "{opts}: a later valid call still works");
    }
}

#[test]
fn the_three_flags_are_stored_and_read_back() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var f = function (o) { var r = document.createElement('div').attachShadow(o); return [r.delegatesFocus, r.clonable, r.serializable].join('') };
         [f({mode: 'open'}), f({mode: 'open', delegatesFocus: true}), f({mode: 'open', clonable: 1}), f({mode: 'closed', serializable: true}), f({mode: 'open', delegatesFocus: true, clonable: true, serializable: true})].join()",
    );
    assert_eq!(out, "falsefalsefalse,truefalsefalse,falsetruefalse,falsefalsetrue,truetruetrue");
}

// ------------------------------------------------------------------------------------------ host eligibility

#[test]
fn valid_shadow_host_names() {
    let mut b = browser();
    for tag in ["article", "aside", "blockquote", "body", "div", "footer", "h1", "h2", "h3", "h4", "h5", "h6", "header", "main", "nav", "p", "section", "span", "x-foo", "my-element.v2", "a-b_c"] {
        let out = js(&mut b, &format!("document.createElement('{tag}').attachShadow({{mode: 'open'}}).nodeType"));
        assert_eq!(out, "11", "{tag}");
    }
}

#[test]
fn invalid_shadow_hosts_throw_not_supported_error_and_change_nothing() {
    let mut b = browser();
    for tag in ["a", "button", "input", "ul", "li", "table", "img", "iframe", "form", "script", "foo", "font-face", "annotation-xml", "color-profile", "missing-glyph", "h7", "details", "textarea", "select", "label", "link", "style", "svg"] {
        let before = b.stats().nodes;
        let out = js(&mut b, &format!("var h = document.createElement('{tag}'); try {{ h.attachShadow({{mode: 'open'}}); 'attached' }} catch (e) {{ e.name + ':' + (e instanceof DOMException) + ':' + e.code + ':' + h.shadowRoot }}"));
        assert!(out.starts_with("NotSupportedError:"), "{tag}: {out}");
        assert!(out.ends_with(":9:null"), "{tag}: {out}");
        assert_eq!(b.stats().nodes, before + 1, "{tag}");
    }
}

#[test]
fn host_namespace_is_checked_and_createelement_lowercases() {
    let mut b = browser();
    let html = "http://www.w3.org/1999/xhtml";
    // `createElementNS(html, 'DIV')` is `Unsupported` in `browser_namespace.rs` (the native document stores lowercase HTML names), so a non-lowercase HTML
    // host cannot be made from JS. The exact-name rule is tested on the native predicate below.
    let out = js(
        &mut b,
        &format!(
            "var r = [];
             function t(el) {{ try {{ el.attachShadow({{mode: 'open'}}); return 'ok'; }} catch (e) {{ return e.name; }} }}
             r.push(t(document.createElementNS('{html}', 'div')), t(document.createElementNS('{html}', 'x-foo')), t(document.createElementNS('http://www.w3.org/2000/svg', 'div')),
                    t(document.createElementNS('http://www.w3.org/2000/svg', 'x-foo')), t(document.createElementNS('http://www.w3.org/1998/Math/MathML', 'x-foo')),
                    t(document.createElementNS('http://www.w3.org/2000/svg', 'g')), t(document.createElement('DIV')), t(document.createElement('X-Foo')));
             r.join()"
        ),
    );
    assert_eq!(out, "ok,ok,NotSupportedError,NotSupportedError,NotSupportedError,NotSupportedError,ok,ok");
    assert_eq!(js(&mut b, &format!("document.createElementNS('{html}', 'DIV')")), "unsupported");
}

#[test]
fn custom_element_name_predicate_is_exact() {
    use crate::browser_shadow::valid_custom_name as ok;
    for good in ["a-b", "x-foo", "my-element.v2", "a-b_c", "x-\u{e9}", "a-\u{10000}", "a--", "a-.", "x-\u{b7}"] {
        assert!(ok(good), "{good}");
    }
    for bad in ["", "x", "-x", "1-x", "X-foo", "x-Foo", "x-F", "x foo", "x-y z", "a-\u{d7}", "a-\u{2028}", "font-face", "annotation-xml", "color-profile", "font-face-src", "font-face-uri", "font-face-format", "font-face-name", "missing-glyph", "\u{e9}-x", "a-\u{f0000}"] {
        assert!(!ok(bad), "{bad}");
    }
}

#[test]
fn a_second_root_on_one_host_is_a_not_supported_error_even_with_the_same_mode() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var h = document.createElement('div'), r1 = h.attachShadow({mode: 'open'}), c = r1.appendChild(document.createElement('i')), r = [];
         ['open', 'closed'].forEach(function (m) { try { h.attachShadow({mode: m}); r.push('no'); } catch (e) { r.push(e.name + ':' + e.code); } });
         r.push(h.shadowRoot === r1, r1.firstChild === c, r1.childNodes.length); r.join()",
    );
    assert_eq!(out, "NotSupportedError:9,NotSupportedError:9,true,true,1");
    let out = js(&mut b, "var h = document.createElement('div'), r1 = h.attachShadow({mode: 'closed'}), r = []; try { h.attachShadow({mode: 'open'}); } catch (e) { r.push(e.name); } r.push(h.shadowRoot, r1.mode); r.join()");
    assert_eq!(out, "NotSupportedError,,closed");
}

// ------------------------------------------------------------------------------------------ root mutation

#[test]
fn ordinary_node_methods_work_on_the_root() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var h = document.createElement('div'), root = h.attachShadow({mode: 'open'}), r = [];
         var a = document.createElement('a'), c = document.createElement('b'), d = document.createElement('i'), t = document.createTextNode('T'), cm = document.createComment('C');
         root.appendChild(a); root.insertBefore(c, a); root.append(d, 'S', cm); root.prepend(t);
         r.push(root.childNodes.length, root.children.length, root.childElementCount, root.firstChild === t, root.lastChild === cm, root.firstElementChild === c, root.lastElementChild === d);
         r.push(root.textContent, c.nextSibling === a, a.previousSibling === c, c.previousSibling === t, d.parentNode === root, root.hasChildNodes());
         var e = document.createElement('u'); r.push(root.replaceChild(e, a) === a, a.parentNode, e.parentNode === root, root.removeChild(c) === c, c.parentNode, root.contains(e), root.contains(c));
         root.textContent = 'only'; r.push(root.childNodes.length, root.firstChild.data, e.parentNode, root.textContent);
         root.textContent = ''; r.push(root.childNodes.length, root.hasChildNodes(), root.firstChild, root.lastChild);
         r.join()",
    );
    assert_eq!(out, "6,3,3,true,true,true,true,TS,true,true,true,true,true,true,,true,true,,true,false,1,only,,only,0,false,,");
}

#[test]
fn insertion_rules_for_the_root_as_parent_and_as_node() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var h = document.createElement('div'), root = h.attachShadow({mode: 'open'}), r = [], other = document.createElement('p');
         function t(f) { try { f(); return 'ok'; } catch (e) { return e.name; } }
         r.push(t(function () { root.appendChild(document); }), t(function () { root.appendChild(document.doctype); }), t(function () { root.appendChild(root); }),
                t(function () { root.appendChild(document.createTextNode('x')); }), t(function () { root.appendChild(document.createComment('x')); }),
                t(function () { root.appendChild({}); }), t(function () { root.insertBefore(document.createElement('q'), other); }),
                t(function () { root.removeChild(other); }), t(function () { root.replaceChild(document.createElement('q'), other); }));
         r.join()",
    );
    assert_eq!(out, "HierarchyRequestError,HierarchyRequestError,HierarchyRequestError,ok,ok,TypeError,NotFoundError,NotFoundError,NotFoundError");
}

#[test]
fn a_host_cannot_enter_its_own_shadow_tree_but_a_second_tree_is_fine() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var h = document.createElement('div'), root = h.attachShadow({mode: 'open'}), mid = root.appendChild(document.createElement('section')), leaf = mid.appendChild(document.createElement('b')), r = [];
         function t(f) { try { f(); return 'ok'; } catch (e) { return e.name; } }
         r.push(t(function () { leaf.appendChild(h); }), t(function () { mid.appendChild(h); }), t(function () { root.appendChild(h); }), t(function () { root.insertBefore(h, mid); }),
                t(function () { mid.append(h); }), t(function () { root.replaceChild(h, mid); }), h.parentNode, root.childNodes.length);
         var outer = document.createElement('div'); r.push(t(function () { outer.appendChild(h); }), h.parentNode === outer, root.isConnected);
         // a host inside the shadow tree of another host
         var h2 = document.createElement('div'), root2 = h2.attachShadow({mode: 'closed'});
         r.push(t(function () { root2.appendChild(h); }), h.parentNode === root2, h.shadowRoot === root);
         // the cycle in the other order
         r.push(t(function () { leaf.appendChild(h2); }), t(function () { h.appendChild(h2); }));
         r.join()",
    );
    assert_eq!(out, "HierarchyRequestError,HierarchyRequestError,HierarchyRequestError,HierarchyRequestError,HierarchyRequestError,HierarchyRequestError,,1,ok,true,false,ok,true,true,HierarchyRequestError,HierarchyRequestError");
}

#[test]
fn a_root_cannot_be_inserted_anywhere() {
    let mut b = browser();
    let before = {
        js(&mut b, "var h = document.createElement('div'), root = h.attachShadow({mode: 'open'}), kid = root.appendChild(document.createElement('i')), target = document.createElement('p');");
        b.stats().nodes
    };
    for src in ["target.appendChild(root)", "target.insertBefore(root, null)", "target.append(root)", "target.prepend(root)", "document.body.replaceChild(root, document.getElementById('a'))"] {
        assert_eq!(js(&mut b, src), "unsupported", "{src}");
    }
    assert_eq!(js(&mut b, "[root.firstChild === kid, kid.parentNode === root, target.childNodes.length, h.shadowRoot === root, document.getElementById('a') !== null].join()"), "true,true,0,true,true");
    assert_eq!(b.stats().nodes, before);
}

#[test]
fn nodes_move_between_the_light_tree_the_shadow_tree_and_back() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var h = document.createElement('div'), root = h.attachShadow({mode: 'open'}), p = document.getElementById('lp'), r = [];
         r.push(p.getRootNode() === document, p.isConnected); root.appendChild(p);
         r.push(p.parentNode === root, p.getRootNode() === root, p.isConnected, document.getElementById('a').childNodes.length, document.getElementById('lp'), p.ownerDocument === document);
         document.getElementById('a').appendChild(p);
         r.push(p.parentNode === document.getElementById('a'), p.getRootNode() === document, p.isConnected, root.childNodes.length, document.getElementById('lp') === p);
         r.join()",
    );
    assert_eq!(out, "true,true,true,true,false,0,,true,true,true,true,0,true");
}

// ------------------------------------------------------------------------------------------ native identity

#[test]
fn native_nodes_have_the_shadow_kind_and_sparse_host_links() {
    let mut b = browser();
    let out = js(&mut b, "var h = document.createElement('div'), root = h.attachShadow({mode: 'closed'}), kid = root.appendChild(document.createElement('i')), lk = h.appendChild(document.createElement('b')); 1");
    assert_eq!(out, "1");
    let (host, root, kid, light) = {
        let get = |b: &mut Browser, name: &str| {
            let v = b.run_script(name).unwrap();
            b.node(v).unwrap_or_else(|| panic!("{name} is a node"))
        };
        (get(&mut b, "h"), get(&mut b, "root"), get(&mut b, "kid"), get(&mut b, "lk"))
    };
    let doc = b.document();
    let doc = doc.borrow();
    assert_eq!(*doc.data(root), NodeData::ShadowRoot);
    assert_eq!(doc.parent(root), None, "the root is not a child of the host");
    assert_eq!(doc.parent(kid), Some(root));
    assert_eq!(doc.parent(light), Some(host));
    assert_eq!(doc.children(host).collect::<Vec<_>>(), vec![light], "the light links of the host exclude the root");
    assert_eq!(doc.children(root).collect::<Vec<_>>(), vec![kid]);
    assert_eq!(doc.shadow_root(host), Some(root));
    assert_eq!(doc.shadow_host(root), Some(host));
    assert_eq!(doc.shadow_host(host), None);
    assert!(!doc.shadow_is_open(root));
    assert_eq!(doc.shadow_root(light), None);
    // The same arena: the wrapper of the root is the wrapper of this node, and a new wrapper of it is the same object.
    drop(doc);
    let again = b.wrapper(root).unwrap();
    assert_eq!(b.node(again), Some(root));
    assert_eq!(js(&mut b, "document.getElementById('a').ownerDocument === root.ownerDocument && root.ownerDocument === document"), "true");
}

// ------------------------------------------------------------------------------------------ nested roots, composed, connected

#[test]
fn nested_roots_get_root_node_and_is_connected_through_hosts() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var h1 = document.createElement('div'), s1 = h1.attachShadow({mode: 'open'}), h2 = s1.appendChild(document.createElement('section')), s2 = h2.attachShadow({mode: 'closed'}),
             x = s2.appendChild(document.createElement('b')), t = x.appendChild(document.createTextNode('t')), r = [];
         function ro(n, c) { return n.getRootNode(c); }
         // detached: the composed root of everything is the top host
         r.push(ro(x) === s2, ro(x, {composed: true}) === h1, ro(t, {composed: true}) === h1, ro(s2) === s2, ro(s2, {composed: true}) === h1, ro(h2) === s1, ro(h2, {composed: true}) === h1,
                ro(s1) === s1, ro(s1, {composed: true}) === h1, ro(h1) === h1, ro(h1, {composed: true}) === h1, ro(x, {composed: false}) === s2, ro(x, undefined) === s2);
         r.push(x.isConnected, h2.isConnected, s1.isConnected, s2.isConnected, h1.isConnected);
         document.body.appendChild(h1);
         r.push(ro(x) === s2, ro(x, {composed: true}) === document, ro(s2, {composed: true}) === document, ro(h2, {composed: true}) === document, ro(s1, {composed: true}) === document, ro(h1) === document, ro(h2) === s1);
         r.push(x.isConnected, t.isConnected, h2.isConnected, s1.isConnected, s2.isConnected, h1.isConnected);
         document.body.removeChild(h1);
         r.push(x.isConnected, s2.isConnected, ro(x, {composed: true}) === h1);
         r.join()",
    );
    assert_eq!(
        out,
        "true,true,true,true,true,true,true,true,true,true,true,true,true,false,false,false,false,false,true,true,true,true,true,true,true,true,true,true,true,true,true,false,false,true"
    );
}

#[test]
fn composed_option_getter_runs_once_before_the_walk_and_sees_shadow_moves() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var h = document.createElement('div'), root = h.attachShadow({mode: 'open'}), x = root.appendChild(document.createElement('b')), calls = 0, other = document.createElement('div');
         var r1 = x.getRootNode({get composed() { calls++; other.appendChild(h); return true; }});
         [calls, r1 === other, x.getRootNode() === root].join()",
    );
    assert_eq!(out, "1,true,true");
}

// ------------------------------------------------------------------------------------------ selectors

#[test]
fn light_tree_queries_exclude_shadow_children_and_root_queries_exclude_light_ones() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var host = document.createElement('div'); host.id = 'host'; host.appendChild(document.createElement('p')).id = 'lightkid';
         var root = host.attachShadow({mode: 'open'}); var sp = root.appendChild(document.createElement('p')); sp.id = 'shadowkid'; sp.className = 'k'; var sub = sp.appendChild(document.createElement('em')); sub.id = 'sub';
         document.getElementById('a').appendChild(host);
         var r = [];
         r.push(document.querySelector('#shadowkid'), document.querySelectorAll('p').length, document.querySelector('#host p').id, document.getElementById('shadowkid'), document.querySelectorAll('.k').length,
                document.getElementsByTagName('em').length, document.getElementsByClassName('k').length, host.querySelector('#shadowkid'), host.querySelectorAll('*').length);
         r.push(root.querySelector('#shadowkid') === sp, root.querySelector('#lightkid'), root.querySelectorAll('*').length, root.querySelector('p em') === sub, root.querySelector('p > em') === sub,
                root.getElementById('sub') === sub, root.getElementById('lightkid'), root.getElementById(''), root.querySelector('p:nth-child(1)') === sp, root.querySelector('p:nth-of-type(2)'));
         r.push(root.querySelector('body p'), root.querySelector('#host p'), root.querySelector(':root'), sp.matches('body p'), sp.matches('p'), sp.matches('p:first-child'), sp.closest('#host'), sp.closest('p') === sp, sub.closest('p') === sp);
         r.push(document.querySelector('em'), document.querySelectorAll('#sub').length);
         r.join()",
    );
    assert_eq!(
        out,
        ",2,lightkid,,0,0,0,,1,true,,2,true,true,true,,,true,,,,,false,true,true,,true,true,,0"
    );
}

#[test]
fn bad_selectors_in_a_root_are_syntax_errors_and_host_pseudo_classes_never_match_in_script() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var h = document.createElement('div'), root = h.attachShadow({mode: 'open'}), r = [];
         ['p[', '', ':host', '::slotted(p)', ':host(div)'].forEach(function (s) { try { root.querySelector(s); r.push('ok'); } catch (e) { r.push(e.name); } });
         r.join()",
    );
    // `:host`, `::slotted` and `:host()` are good selectors in a script call; they match nothing (they are for style sheets of a shadow tree).
    assert_eq!(out, "SyntaxError,SyntaxError,ok,ok,ok");
    assert_eq!(js(&mut b, "root.querySelector(':host') + ',' + root.host.matches(':host') + ',' + root.host.matches('::slotted(p)')"), "null,false,false");
}

// ------------------------------------------------------------------------------------------ clone, serialize, parse

#[test]
fn cloning_a_host_does_not_clone_its_root() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var h = document.createElement('div'); h.appendChild(document.createElement('b')); var root = h.attachShadow({mode: 'open'}); root.appendChild(document.createElement('i'));
         var inner = h.firstChild, ih = inner.appendChild(document.createElement('section')); ih.attachShadow({mode: 'open'}).appendChild(document.createElement('u'));
         var shallow = h.cloneNode(false), deep = h.cloneNode(true);
         [shallow.shadowRoot, deep.shadowRoot, deep.childNodes.length, deep.firstChild.firstChild.shadowRoot, deep.firstChild.firstChild.nodeName, h.shadowRoot === root].map(String).join()",
    );
    assert_eq!(out, "null,null,1,null,SECTION,true");
}

#[test]
fn cloning_a_root_is_not_supported_error_and_makes_no_node() {
    let mut b = browser();
    js(&mut b, "var h = document.createElement('div'), root = h.attachShadow({mode: 'open'}); root.appendChild(document.createElement('i'));");
    let before = b.stats().nodes;
    let out = js(&mut b, "var r = []; [false, true, undefined].forEach(function (d) { try { root.cloneNode(d); r.push('no'); } catch (e) { r.push(e.name + ':' + e.code); } }); r.join()");
    assert_eq!(out, "NotSupportedError:9,NotSupportedError:9,NotSupportedError:9");
    assert_eq!(b.stats().nodes, before);
}

#[test]
fn serialization_of_the_host_and_the_page_never_sees_the_root() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var h = document.createElement('div'); h.id = 'h'; h.appendChild(document.createElement('b')).textContent = 'l'; var root = h.attachShadow({mode: 'open'}); root.appendChild(document.createElement('i')).textContent = 's';
         document.body.appendChild(h);
         [document.body.innerHTML, h.innerHTML, root.firstChild.innerHTML, (root.firstChild.innerHTML = '<u>z</u>', root.firstChild.innerHTML), root.textContent, h.textContent].join('|')",
    );
    assert_eq!(out, "<div id=\"a\"><p id=\"lp\">light</p></div><div id=\"h\"><b>l</b></div>|<b>l</b>|s|<u>z</u>|z|l");
    let doc = b.document();
    let dump = doc.borrow().dump();
    assert!(dump.contains("<div>") && dump.contains("<b>") && !dump.contains("<i>"), "{dump}");
}

#[test]
fn the_page_parser_and_import_node_never_make_the_new_kind() {
    let mut doc = tree::parse("<!doctype html><body><div><template>x</template><p>a</p></div></body>");
    let mut stack = vec![doc.root()];
    while let Some(n) = stack.pop() {
        assert!(!matches!(doc.data(n), NodeData::ShadowRoot));
        stack.extend(doc.children(n));
    }
    let host = doc.create_element("div");
    let root = doc.attach_shadow(host, true);
    let mut other = tree::parse("<body>");
    assert_eq!(other.import_node(&doc, root), None, "a root is not copied by import");
    assert_eq!(doc.dump().lines().filter(|l| l.contains("ShadowRoot")).count(), 0);
}

// ------------------------------------------------------------------------------------------ events

#[test]
fn events_that_are_not_composed_stop_at_the_root_and_never_reach_the_host() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var host = document.createElement('div'), root = host.attachShadow({mode: 'open'}), mid = root.appendChild(document.createElement('div')), leaf = mid.appendChild(document.createElement('b')), log = [];
         document.body.appendChild(host);
         window.addEventListener('x', function () { log.push('window'); }); document.addEventListener('x', function () { log.push('document'); }); host.addEventListener('x', function () { log.push('host'); });
         root.addEventListener('x', function (e) { log.push('root:' + e.target.nodeName + ':' + e.currentTarget.nodeType + ':' + e.composedPath().map(function (n) { return n.nodeName; }).join('>')); });
         mid.addEventListener('x', function () { log.push('mid'); });
         leaf.dispatchEvent(new Event('x', {bubbles: true}));
         root.dispatchEvent(new Event('x', {bubbles: true}));
         host.dispatchEvent(new Event('x', {bubbles: true}));
         log.join('|')",
    );
    assert_eq!(out, "mid|root:B:11:B>DIV>#document-fragment|root:#document-fragment:11:#document-fragment|host|document|window");
}

#[test]
fn composed_dispatch_that_would_cross_a_root_is_refused_before_any_listener_runs() {
    let mut b = browser();
    js(&mut b, "var host = document.createElement('div'), root = host.attachShadow({mode: 'open'}), leaf = root.appendChild(document.createElement('b')), log = []; document.body.appendChild(host);
         ['leaf', 'root', 'host', 'document'].forEach(function (n) { var t = {leaf: leaf, root: root, host: host, document: document}[n]; t.addEventListener('c', function () { log.push(n); }); });");
    for target in ["leaf", "root"] {
        assert_eq!(js(&mut b, &format!("{target}.dispatchEvent(new Event('c', {{bubbles: true, composed: true}}))")), "unsupported", "{target}");
        assert_eq!(js(&mut b, "log.length"), "0", "{target}");
    }
    // The event is not left dispatching, and an event that stays inside is fine; a composed event from the host (no root crossing) works.
    assert_eq!(js(&mut b, "var e = new Event('c', {composed: true}); host.dispatchEvent(e); [log.join(), e.eventPhase, e.composedPath().length].join()"), "host,0,0");
    assert_eq!(js(&mut b, "var e2 = new Event('c', {composed: true}); try { leaf.dispatchEvent(e2); } catch (x) {} 1"), "unsupported");
}

#[test]
fn driver_dispatch_into_a_shadow_tree_is_refused_because_its_events_are_composed() {
    let mut b = browser();
    js(&mut b, "var host = document.createElement('div'), root = host.attachShadow({mode: 'open'}), leaf = root.appendChild(document.createElement('button')); document.body.appendChild(host);");
    let v = b.run_script("leaf").unwrap();
    let leaf = b.node(v).unwrap();
    assert!(matches!(b.click(leaf), Err(Error::Unsupported(_))));
    assert_eq!(js(&mut b, "document.activeElement === document.body"), "true");
}

#[test]
fn elements_in_a_shadow_tree_are_not_focusable() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var host = document.createElement('div'), root = host.attachShadow({mode: 'open'}), btn = root.appendChild(document.createElement('button')), log = [];
         document.body.appendChild(host); btn.addEventListener('focus', function () { log.push('focus'); }); btn.focus();
         [document.activeElement === document.body, log.length].join()",
    );
    assert_eq!(out, "true,0");
}

#[test]
fn scripts_in_a_shadow_tree_never_run_but_light_ones_do() {
    use crate::browser_loader::{PageConfig, PageLimits};
    use bare_net::http::Client;
    // Inline scripts only: the page makes no request (`fetches` is checked). The shadow scripts have text and a `src`; none runs and none is fetched.
    let html = "<!doctype html><body><div id=h></div><script>var log = []; var h = document.getElementById('h'), root = h.attachShadow({mode: 'open'});
        var s = document.createElement('script'); s.textContent = 'log.push(\\'shadow\\')'; root.appendChild(s);
        var f = document.createElement('script'); f.src = '/never.js'; root.appendChild(f);
        var l = document.createElement('script'); l.textContent = 'log.push(\\'light\\')'; h.appendChild(l);
        var m = document.createElement('script'); m.textContent = 'log.push(\\'moved\\')'; root.appendChild(m); root.removeChild(m); h.appendChild(m);</script>";
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    let r = b.run_page(PageConfig { url: "http://127.0.0.1:9/page.html".into(), client: Client::new(), limits: PageLimits { quiet: std::time::Duration::from_millis(50), ..Default::default() }, fetch: None });
    assert_eq!(js(&mut b, "log.join()"), "light,moved", "{r:?}");
    assert_eq!((r.scripts_run, r.fetches, r.load_errors), (3, 0, 0), "{r:?}");
}

// ------------------------------------------------------------------------------------------ GC, detach, slot reuse

#[test]
fn detached_host_and_root_cycles_are_collected_while_the_arena_keeps_the_nodes() {
    let mut b = browser();
    b.collect(&[]).unwrap();
    let base = b.stats();
    js(&mut b, "for (var i = 0; i < 300; i++) { var h = document.createElement('div'); var r = h.attachShadow({mode: i % 2 ? 'open' : 'closed'}); r.appendChild(document.createElement('p')).appendChild(document.createTextNode('t')); } h = r = null; 1");
    let held = b.stats();
    assert!(held.wrappers >= base.wrappers + 300 * 4, "{held:?}");
    b.collect(&[]).unwrap();
    let after = b.stats();
    assert_eq!(after.wrappers, base.wrappers, "the cycles go: {after:?}");
    assert_eq!(after.nodes, base.nodes + 300 * 4, "the native arena never frees");
}

#[test]
fn a_held_root_or_host_keeps_the_other_side_and_its_expandos() {
    let mut b = browser();
    js(&mut b, "var root; (function () { var h = document.createElement('div'); h.marker = 'host-expando'; root = h.attachShadow({mode: 'closed'}); root.marker = 'root-expando'; root.appendChild(document.createElement('i')).marker = 'kid'; })(); 1");
    b.collect(&[]).unwrap();
    assert_eq!(js(&mut b, "[root.host.marker, root.marker, root.firstChild.marker, root.host.shadowRoot].join()"), "host-expando,root-expando,kid,");
    js(&mut b, "var held = document.createElement('p'); held.marker = 'H'; var hroot = held.attachShadow({mode: 'open'}); hroot.firstChildMarker = 'R'; root = null; 1");
    b.collect(&[]).unwrap();
    // Only the host is held by name; the root is reachable through `host.shadowRoot` and keeps its own expando.
    assert_eq!(js(&mut b, "hroot = null; held.shadowRoot.firstChildMarker + held.marker"), "RH");
    b.collect(&[]).unwrap();
    assert_eq!(js(&mut b, "held.shadowRoot.firstChildMarker"), "R");
}

#[test]
fn attached_hosts_keep_their_roots_alive_without_any_js_reference() {
    let mut b = browser();
    js(&mut b, "(function () { var h = document.createElement('div'); h.id = 'keep'; var r = h.attachShadow({mode: 'closed'}); r.marker = 'M'; r.appendChild(document.createElement('i')).id = 'kid'; r.firstChild.marker = 'K'; document.body.appendChild(h); window.chk = function () { return 1; }; })(); 1");
    b.collect(&[]).unwrap();
    b.collect(&[]).unwrap();
    // A closed root is not reachable by script from the host, but the native tree keeps its wrappers (and listeners) while the host is attached.
    let out = js(&mut b, "var k = document.getElementById('keep'); [k.shadowRoot, k.childNodes.length].join()");
    assert_eq!(out, ",0");
    let st = b.stats();
    assert!(st.wrappers >= 4, "{st:?}");
}

#[test]
fn slot_reuse_after_collection_gives_correct_brands_and_identity() {
    let mut b = browser();
    for round in 0..4 {
        js(&mut b, "for (var i = 0; i < 50; i++) { var h = document.createElement('div'); h.attachShadow({mode: 'open'}).appendChild(document.createElement('b')); var p = document.createElement('p'); } h = p = null; 1");
        b.collect(&[]).unwrap();
        let out = js(
            &mut b,
            "var h = document.createElement('section'), p = document.createElement('p'), r = h.attachShadow({mode: 'open'}), t = document.createTextNode('x'), rr = [];
             rr.push(r instanceof ShadowRoot, p instanceof ShadowRoot, t instanceof ShadowRoot, r.nodeType, p.nodeType, h.shadowRoot === r, p.shadowRoot, r.host === h, r.host === r.host, h.shadowRoot === h.shadowRoot);
             try { Object.getOwnPropertyDescriptor(ShadowRoot.prototype, 'mode').get.call(p); rr.push('no'); } catch (e) { rr.push(e.name); }
             rr.join()",
        );
        assert_eq!(out, "true,false,false,11,1,true,,true,true,true,TypeError", "round {round}");
    }
}

#[test]
fn a_listener_on_a_shadow_root_survives_while_the_root_is_reachable_and_dies_with_it() {
    let mut b = browser();
    js(&mut b, "var h = document.createElement('div'), root = h.attachShadow({mode: 'open'}), hit = 0; root.addEventListener('x', function () { hit++; }); document.body.appendChild(h); h = null;");
    b.collect(&[]).unwrap();
    assert_eq!(js(&mut b, "root.dispatchEvent(new Event('x')); hit"), "1");
    assert!(b.stats().listeners >= 1);
    js(&mut b, "document.body.removeChild(document.body.lastChild); root = null; 1");
    b.collect(&[]).unwrap();
    assert_eq!(b.stats().listeners, 0);
}

// ------------------------------------------------------------------------------------------ caps and bounds

#[test]
fn caps_are_checked_before_growth_and_no_catch_or_finally_sees_a_limit() {
    // nodes: the host fits, the root does not.
    let nodes = browser().stats().nodes;
    let lim_nodes = BrowserLimits { max_nodes: nodes + 1, ..Default::default() };
    let mut b = browser_with(PAGE, lim_nodes, 5_000_000);
    let out = js(&mut b, "var h = document.createElement('div'), s = 'start'; try { h.attachShadow({mode: 'open'}); s = 'attached'; } catch (e) { s = 'caught'; } finally { s += '+finally'; } s");
    assert_eq!(out, lim("dom-nodes"));
    assert_eq!(b.stats().nodes, nodes + 1);
    assert_eq!(js(&mut b, "h.shadowRoot"), "null");

    // wrappers: the call fails before the node is made.
    let w = browser().stats().wrappers;
    let mut b = browser_with(PAGE, BrowserLimits { max_wrappers: w + 1, ..Default::default() }, 5_000_000);
    js(&mut b, "var h = document.createElement('div');");
    let nodes = b.stats().nodes;
    let out = js(&mut b, "try { h.attachShadow({mode: 'open'}); 'ok' } catch (e) { 'caught' }");
    assert_eq!(out, lim("dom-wrappers"));
    assert_eq!(b.stats().nodes, nodes, "no node was made");
    assert_eq!(js(&mut b, "h.shadowRoot"), "null");
}

#[test]
fn depth_cap_counts_the_hops_through_hosts_and_is_checked_before_mutation() {
    let mut b = browser_with(PAGE, BrowserLimits { max_tree_depth: 6, ..Default::default() }, 5_000_000);
    let out = js(
        &mut b,
        "var h = document.createElement('div'), root = h.attachShadow({mode: 'open'}), n = root, d = 1, r = [];
         try { while (true) { n = n.appendChild(document.createElement('i')); d++; if (d > 20) break; } } catch (e) { r.push('caught'); }
         r.push(d); r.join()",
    );
    assert_eq!(out, lim("dom-depth"));
    // The boundary: a host with a 3-deep shadow tree (composed height 4) fits under a parent of depth 1 (1 + 1 + 4 = 6).
    let mut b = browser_with(PAGE, BrowserLimits { max_tree_depth: 6, ..Default::default() }, 5_000_000);
    let out = js(
        &mut b,
        "var inner = document.createElement('div'), s = inner.attachShadow({mode: 'open'}), a = s.appendChild(document.createElement('i')), c = a.appendChild(document.createElement('i')), c2 = c.appendChild(document.createElement('i'));
         var holder = document.createElement('div'), mid = holder.appendChild(document.createElement('div')); mid.appendChild(inner) === inner && inner.getRootNode({composed: true}) === holder",
    );
    assert_eq!(out, "true");
    // The state after the limit: nothing moved. (Run it on a fresh browser to read it.)
    let mut b = browser_with(PAGE, BrowserLimits { max_tree_depth: 6, ..Default::default() }, 5_000_000);
    js(&mut b, "var inner = document.createElement('div'), s = inner.attachShadow({mode: 'open'}), a = s.appendChild(document.createElement('i')), c = a.appendChild(document.createElement('i')), c2 = c.appendChild(document.createElement('i'));
                var holder = document.createElement('div'), m1 = holder.appendChild(document.createElement('div')), m2 = m1.appendChild(document.createElement('div')), m3 = m2.appendChild(document.createElement('div'));");
    assert_eq!(js(&mut b, "m3.appendChild(inner)"), lim("dom-depth"));
    assert_eq!(js(&mut b, "[inner.parentNode, m3.childNodes.length, s.firstChild === a].join()"), ",0,true");
}

#[test]
fn attach_shadow_depth_cap_is_checked_on_the_host() {
    let mut b = browser_with(PAGE, BrowserLimits { max_tree_depth: 4, ..Default::default() }, 5_000_000);
    // html(1) > body(2) > div#a(3) > host(4) fits the cap of 4. A root at depth 5 does not.
    let out = js(&mut b, "var h = document.getElementById('a').appendChild(document.createElement('div')); try { h.attachShadow({mode: 'open'}); 'ok' } catch (e) { 'caught' }");
    assert_eq!(out, lim("dom-depth"));
    assert_eq!(js(&mut b, "h.shadowRoot"), "null");
}

#[test]
fn walks_through_roots_charge_steps_and_stop_at_the_step_budget() {
    let mut ok = browser();
    let src = "var treeTop = document.createElement('div'), cur = treeTop;
         for (var i = 0; i < 100; i++) { var r = cur.attachShadow({mode: 'open'}); cur = r.appendChild(document.createElement('div')); }
         var s = 0; for (var j = 0; j < 100; j++) { s += cur.getRootNode({composed: true}) === treeTop ? 1 : 0; } s";
    assert_eq!(js(&mut ok, src), "100", "200 hops deep, walked through 100 hosts");
    let mut b = browser_with(PAGE, BrowserLimits::default(), 5_000);
    let out = js(
        &mut b,
        "var treeTop = document.createElement('div'), cur = treeTop;
         for (var i = 0; i < 100; i++) { var r = cur.attachShadow({mode: 'open'}); cur = r.appendChild(document.createElement('div')); }
         var s = 0; for (var j = 0; j < 100000; j++) { s += cur.getRootNode({composed: true}) === treeTop ? 1 : 0; } s",
    );
    assert!(out.starts_with("limit:"), "{out}");
}

#[test]
fn query_results_in_a_root_are_capped_and_traversal_is_charged() {
    let mut b = browser_with(PAGE, BrowserLimits { max_results: 5, ..Default::default() }, 5_000_000);
    let out = js(&mut b, "var h = document.createElement('div'), r = h.attachShadow({mode: 'open'}); for (var i = 0; i < 6; i++) r.appendChild(document.createElement('i')); try { r.querySelectorAll('i'); 'ok' } catch (e) { 'caught' }");
    assert_eq!(out, lim("dom-results"));
    assert_eq!(js(&mut b, "r.querySelector('i') !== null && r.children.length > 0"), lim("dom-results"));
}

// ------------------------------------------------------------------------------------------ neighbours keep working

#[test]
fn attr_and_namespace_behaviour_inside_a_shadow_tree_is_unchanged() {
    let mut b = browser();
    let out = js(
        &mut b,
        "var h = document.createElement('div'), root = h.attachShadow({mode: 'open'}), el = root.appendChild(document.createElement('span')), r = [];
         el.setAttribute('x', '1'); var a = el.getAttributeNode('x');
         r.push(a.ownerElement === el, a.getRootNode() === a, a.ownerDocument === document, el.getAttribute('x'), el.attributes.length);
         var svg = root.appendChild(document.createElementNS('http://www.w3.org/2000/svg', 'g')); svg.setAttributeNS('http://www.w3.org/1999/xlink', 'xlink:href', '#q');
         r.push(svg.namespaceURI, svg.getAttributeNS('http://www.w3.org/1999/xlink', 'href'), svg.getRootNode() === root, svg.isConnected, root.querySelector('g') === svg);
         r.join()",
    );
    assert_eq!(out, "true,true,true,1,1,http://www.w3.org/2000/svg,#q,true,false,true");
}

#[test]
fn existing_light_tree_fragment_behaviour_is_unchanged() {
    let mut b = browser();
    assert_eq!(js(&mut b, "document.getElementById('a').innerHTML = '<p>1</p><p>2</p>'; document.getElementById('a').innerHTML + '|' + document.body.getRootNode().nodeName + '|' + typeof document.createDocumentFragment"), "<p>1</p><p>2</p>|#document|function");
}

// ------------------------------------------------------------------------------------------ load model

/// A load run for separate processes (`BARE_SHADOW_N=<hosts> cargo test ... shadow_load_model`; skipped without the variable). Each batch builds hosts
/// (alternating open and closed roots, three shadow children each, every fifth shadow child is a nested host), then collects. A model computed in Rust
/// from `n` is checked against what the page reports, against the native arena size, and against the wrappers left after the last collection.
#[test]
fn shadow_load_model() {
    let Some(n) = std::env::var("BARE_SHADOW_N").ok().and_then(|v| v.parse::<usize>().ok()) else { return };
    let vm = if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 };
    let heap = HeapLimits { max_bytes: vm, ..Default::default() };
    let mut b = Browser::new(tree::parse(PAGE), BrowserLimits::default(), EvalLimits { max_steps: u64::MAX / 4, ..Default::default() }, heap).expect("browser");
    b.collect(&[]).unwrap();
    let base = b.stats();
    const BATCH: usize = 100;
    let (mut open_seen, mut text_len, mut kept) = (0usize, 0usize, 0usize);
    let mut done = 0;
    while done < n {
        let k = BATCH.min(n - done);
        let src = format!(
            "var r = 0, t = 0, o = 0; for (var i = {done}; i < {done} + {k}; i++) {{
               var h = document.createElement(i % 3 ? 'div' : 'x-card'), root = h.attachShadow({{mode: i % 2 ? 'open' : 'closed'}});
               for (var j = 0; j < 3; j++) {{ var s = root.appendChild(document.createElement('span')); s.textContent = 'v' + i + j;
                 if (j === 0 && i % 5 === 0) s.appendChild(document.createElement('p')).attachShadow({{mode: 'open'}}).appendChild(document.createTextNode('n')); }}
               if (i % 4 === 0) {{ document.getElementById('a').appendChild(h); r++; }}
               if (h.shadowRoot) {{ o++; t += h.shadowRoot.textContent.length; }}
             }} [r, o, t].join()"
        );
        let out = js(&mut b, &src);
        let v: Vec<usize> = out.split(',').map(|x| x.parse().unwrap_or_else(|_| panic!("{out}"))).collect();
        kept += v[0];
        open_seen += v[1];
        text_len += v[2];
        done += k;
        b.collect(&[]).unwrap();
    }
    // Model: i odd = open root. Text of a root: "v<i>0v<i>1v<i>2", and "n" for the nested root of the first child when i % 5 == 0 (it is not in the light text of the span).
    let model_open = (0..n).filter(|i| i % 2 == 1).count();
    let model_len: usize = (0..n).filter(|i| i % 2 == 1).map(|i| 3 * (1 + i.to_string().len() + 1)).sum();
    let model_kept = (0..n).filter(|i| i % 4 == 0).count();
    assert_eq!((open_seen, text_len, kept), (model_open, model_len, model_kept));
    // Per host: host, root, 3 spans, 3 texts; every fifth also p, nested root and its text node.
    let model_nodes = n * 8 + (0..n).filter(|i| i % 5 == 0).count() * 3;
    let st = b.stats();
    assert_eq!(st.nodes, base.nodes + model_nodes, "the arena keeps every node");
    // The attached hosts and everything in them stay wrapped (nothing else can reach them); the detached ones are gone.
    assert!(st.wrappers <= base.wrappers + model_kept * 16, "{st:?}");
    assert_eq!(js(&mut b, "document.querySelectorAll('span').length + document.getElementById('a').children.length"), (model_kept + 1).to_string());
    assert_eq!(js(&mut b, "document.getElementById('a').lastElementChild.shadowRoot === null || document.getElementById('a').lastElementChild.shadowRoot instanceof ShadowRoot"), "true");
}

#[test]
fn shadow_root_creation_counts_live_lists_before_native_mutation() {
    let w=browser().stats().wrappers;
    let mut b=browser_with(PAGE,BrowserLimits{max_wrappers:w+2,..Default::default()},5_000_000);
    js(&mut b,"var h=document.createElement('div'); var list=document.getElementsByName('x');");
    let before=b.stats().nodes;
    assert_eq!(js(&mut b,"h.attachShadow({mode:'open'})"),lim("dom-wrappers"));
    assert_eq!(b.stats().nodes,before);
    assert_eq!(js(&mut b,"h.shadowRoot"),"null");
}

#[test]
fn connected_shadow_computed_style_is_read() {
    let mut b=browser();
    js(&mut b,"var h=document.createElement('div');var r=h.attachShadow({mode:'open'});var x=document.createElement('i');r.appendChild(x);document.body.appendChild(h);");
    assert_eq!(js(&mut b,"getComputedStyle(x).display"),"inline");
}

// ------------------------------------------------------------------------------------------ style scopes and slots

/// A host `x-a` with light children `#l` (default slot) and `#n` (`slot=named`) and a shadow tree that has two slots.
const SLOTTED: &str = "<!doctype html><html><head><style>p { color: red } x-a { color: blue; display: block }</style></head>\
    <body><x-a id=h><p id=l>a</p><b id=n slot=named>b</b>text</x-a></body></html>";
const SHADOW: &str = "var r = h.attachShadow({ mode: 'open' }); r.innerHTML = '<style>p { color: green } :host { width: 77px } ::slotted(p) { color: pink }</style>\
    <p id=in>x</p><div id=w><slot id=s1></slot><slot id=s2 name=named><i id=fb>f</i></slot></div>'; 0";

#[test]
fn computed_style_in_a_shadow_tree_uses_its_own_scope() {
    let mut b = browser_with(SLOTTED, BrowserLimits::default(), 50_000_000);
    js(&mut b, "var h = document.getElementById('h'); 0");
    js(&mut b, SHADOW);
    let g = |b: &mut Browser, e: &str, p: &str| js(b, &format!("getComputedStyle({e}).{p}"));
    // the shadow rule wins in the shadow tree; the page rule `p { red }` does not reach it
    assert_eq!(g(&mut b, "r.getElementById('in')", "color"), "rgb(0, 128, 0)");
    // `:host` gives the host a width, and the page rule for `x-a` (blue) wins over the inherited colour
    assert_eq!(g(&mut b, "h", "width"), "77px");
    assert_eq!(g(&mut b, "h", "color"), "rgb(0, 0, 255)");
    // a slotted node: the shadow `::slotted(p)` rule loses to no page rule, because shadow-tree rules lose for a normal declaration
    assert_eq!(g(&mut b, "document.getElementById('l')", "color"), "rgb(255, 0, 0)");
    // the slot is a node of the cascade: the slotted `b` inherits from `#s2`, which inherits from `#w` and the host (blue)
    assert_eq!(g(&mut b, "document.getElementById('n')", "color"), "rgb(0, 0, 255)");
}

#[test]
fn slot_api() {
    let mut b = browser_with(SLOTTED, BrowserLimits::default(), 50_000_000);
    js(&mut b, "var h = document.getElementById('h'); 0");
    js(&mut b, SHADOW);
    let t = |b: &mut Browser, s: &str| js(b, s);
    assert_eq!(t(&mut b, "var s1 = r.getElementById('s1'), s2 = r.getElementById('s2'); [s1 instanceof HTMLSlotElement, s1.name, s2.name, Object.prototype.toString.call(s1)].join()"), "true,,named,[object HTMLSlotElement]");
    assert_eq!(t(&mut b, "s1.assignedNodes().map(n => n.nodeName).join()"), "P,#text");
    assert_eq!(t(&mut b, "s1.assignedElements().map(n => n.id).join()"), "l");
    assert_eq!(t(&mut b, "s2.assignedNodes().length + ',' + s2.assignedNodes({ flatten: true }).map(n => n.id).join()"), "1,n");
    assert_eq!(t(&mut b, "var l = document.getElementById('l'); [l.assignedSlot === s1, document.getElementById('n').assignedSlot === s2, String(h.assignedSlot), l.getRootNode() === document, s1.getRootNode() === r, s1.getRootNode({ composed: true }) === document].join()"), "true,true,null,true,true,true");
    // nothing is slotted: flatten gives the fallback content
    assert_eq!(t(&mut b, "document.getElementById('n').remove(); s2.assignedNodes().length + ',' + s2.assignedNodes({ flatten: true }).map(n => n.id).join() + ',' + s2.assignedElements({ flatten: true }).length"), "0,fb,1");
    assert_eq!(t(&mut b, "s1.assignedNodes({ flatten: true }).length"), "2");
    // a closed tree: `assignedSlot` is null for the page
    assert_eq!(t(&mut b, "var h2 = document.createElement('div'); h2.attachShadow({ mode: 'closed' }).innerHTML = '<slot></slot>'; var k = h2.appendChild(document.createElement('u')); document.body.appendChild(h2); k.assignedSlot"), "null");
}

// ------------------------------------------------------------------------------------------ constructable style sheets

const ADOPT: &str = "<!doctype html><html><head><style>p { color: red }</style></head><body><x-a id=h></x-a><p id=lp>light</p></body></html>";

#[test]
fn adopted_sheets_join_the_cascade_of_their_tree() {
    let mut b = browser_with(ADOPT, BrowserLimits::default(), 50_000_000);
    js(&mut b, "var h = document.getElementById('h'); var r = h.attachShadow({ mode: 'open' }); r.innerHTML = '<p id=hid>a</p><p id=col>b</p>'; var s = new CSSStyleSheet(); s.replaceSync('#hid { display: none } #col { color: blue }'); r.adoptedStyleSheets = [s]; 0");
    let g = |b: &mut Browser, e: &str, p: &str| js(b, &format!("getComputedStyle({e}).{p}"));
    assert_eq!(g(&mut b, "r.getElementById('hid')", "display"), "none");
    assert_eq!(g(&mut b, "r.getElementById('col')", "color"), "rgb(0, 0, 255)");
    // the page rule `p { red }` stays out of the shadow tree, and the adopted sheet stays out of the page
    assert_eq!(g(&mut b, "document.getElementById('lp')", "display"), "block");
    assert_eq!(g(&mut b, "document.getElementById('lp')", "color"), "rgb(255, 0, 0)");
    // a later replaceSync changes the cascade
    js(&mut b, "s.replaceSync('#hid { display: block }')");
    assert_eq!(g(&mut b, "r.getElementById('hid')", "display"), "block");
    assert_eq!(g(&mut b, "r.getElementById('col')", "color"), "rgb(0, 0, 0)", "no rule left, the colour is inherited");
    // dropping the sheet from the list removes its rules
    js(&mut b, "s.replaceSync('#hid { display: none }'); r.adoptedStyleSheets = []");
    assert_eq!(g(&mut b, "r.getElementById('hid')", "display"), "block");
}

#[test]
fn adopted_sheets_of_the_document_and_the_sheet_api() {
    let mut b = browser_with(ADOPT, BrowserLimits::default(), 50_000_000);
    let t = |b: &mut Browser, s: &str| js(b, s);
    assert_eq!(t(&mut b, "var s = new CSSStyleSheet(); [s instanceof CSSStyleSheet, typeof s.replace, typeof s.replaceSync, s.cssRules.length, s.ownerNode, s.href, document.adoptedStyleSheets.length].join()"), "true,function,function,0,,,0");
    assert_eq!(t(&mut b, "s.replaceSync('p { color: green } @media print { p { color: pink } }'); s.cssRules.length + ',' + s.cssRules[0].selectorText"), "2,p");
    assert_eq!(t(&mut b, "document.adoptedStyleSheets = [s]; document.adoptedStyleSheets[0] === s"), "true");
    // an adopted sheet comes after the `<style>` elements of the document, so it wins at equal specificity
    assert_eq!(t(&mut b, "getComputedStyle(document.getElementById('lp')).color"), "rgb(0, 128, 0)");
    assert_eq!(t(&mut b, "s.insertRule('p { color: navy }', 1); getComputedStyle(document.getElementById('lp')).color"), "rgb(0, 0, 128)");
    // `replace` gives a promise of the sheet, and the rules are in place when it settles
    assert_eq!(t(&mut b, "var q = s.replace('p { color: teal }'); [q instanceof Promise, getComputedStyle(document.getElementById('lp')).color].join()"), "true,rgb(0, 128, 128)");
    assert_eq!(t(&mut b, "q.then(function (v) { window.__same = v === s }); 0"), "0");
    assert_eq!(t(&mut b, "document.adoptedStyleSheets = []; getComputedStyle(document.getElementById('lp')).color"), "rgb(255, 0, 0)");
    // a sheet from `document.styleSheets` cannot be adopted, and a non-sheet is refused
    assert_eq!(t(&mut b, "document.adoptedStyleSheets = [{}]"), "throw:TypeError:Failed to set the 'adoptedStyleSheets' property: Failed to convert value to 'CSSStyleSheet'.");
    assert_eq!(t(&mut b, "document.adoptedStyleSheets = [document.styleSheets[0]]"), "throw:NotAllowedError:Failed to set the 'adoptedStyleSheets' property: Can't adopt non-constructed stylesheets.");
    assert_eq!(t(&mut b, "CSSStyleSheet()"), "throw:TypeError:Failed to construct: please use the 'new' operator");
}
