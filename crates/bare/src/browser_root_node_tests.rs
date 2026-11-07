//! Tests of `Node.prototype.getRootNode(options)`. Expectations come from the DOM Standard (the root is the topmost
//! ancestor, or the node itself when it has none; `composed` changes nothing here because there are no shadow roots) and
//! from the WebIDL rules for brand checks and dictionary conversion. Written by hand: Node has no DOM, so it is not an oracle.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const PAGE: &str = "<!doctype html><html><head><title>t</title></head><body><div id=a><p id=p>hi</p><!--c--><span id=s>one</span></div><svg><g id=g></g></svg><math><mi id=m></mi></math></body></html>";

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
        Err(e) => format!("other:{e:?}"),
    }
}

fn lim(name: &'static str) -> String {
    format!("limit:{:?}", LimitKind::Host(name))
}

/// `<body>` with `n` nested `<div>`s and an `<i id=deep>` inside the innermost one.
fn deep_page(n: usize) -> String {
    format!("<!doctype html><body>{}<i id=deep></i>{}</body>", "<div>".repeat(n), "</div>".repeat(n))
}

#[test]
fn method_shape_is_a_plain_method_on_node() {
    let mut b = browser();
    assert_eq!(js(&mut b, "typeof Node.prototype.getRootNode + ',' + Node.prototype.getRootNode.length + ',' + Node.prototype.getRootNode.name"), "function,0,getRootNode");
    assert_eq!(js(&mut b, "var d = Object.getOwnPropertyDescriptor(Node.prototype, 'getRootNode'); [d.writable, d.enumerable, d.configurable, 'get' in d].join()"), "true,false,true,false");
    assert_eq!(js(&mut b, "[document, document.doctype, document.documentElement, document.getElementById('p').firstChild, document.getElementById('a').childNodes[1], document.createTextNode('')].map(function (n) { return typeof n.getRootNode; }).join()"), "function,function,function,function,function,function");
    assert_eq!(js(&mut b, "typeof window.getRootNode + ',' + typeof Element.prototype.getRootNode"), "undefined,function");
}

#[test]
fn react_root_probe_finds_the_document_and_detached_roots() {
    let mut b = browser();
    assert_eq!(
        js(&mut b, "function cf(e) { return typeof e.getRootNode === 'function' ? e.getRootNode() : e.ownerDocument; } var dv = document.createElement('div'); [cf(document) === document, cf(document.getElementById('p')) === document, cf(dv) === dv].join()"),
        "true,true,true"
    );
}

#[test]
fn attached_nodes_have_the_document_as_root() {
    let mut b = browser();
    assert_eq!(
        js(&mut b, "var p = document.getElementById('p'); [document.getRootNode() === document, document.documentElement.getRootNode() === document, document.doctype.getRootNode() === document, p.getRootNode() === document, p.firstChild.getRootNode() === document, document.getElementById('a').childNodes[1].getRootNode() === document, document.getElementById('g').getRootNode() === document, document.getElementById('m').getRootNode() === document].join()"),
        "true,true,true,true,true,true,true,true"
    );
}

#[test]
fn detached_trees_have_their_top_ancestor_and_detached_leaves_are_their_own_root() {
    let mut b = browser();
    assert_eq!(
        js(&mut b, "var t = document.createTextNode('x'), c = document.createComment('c'), top = document.createElement('div'), mid = top.appendChild(document.createElement('section')), leaf = mid.appendChild(document.createElement('b')); [leaf.getRootNode() === top, mid.getRootNode() === top, top.getRootNode() === top, t.getRootNode() === t, c.getRootNode() === c, top.getRootNode() !== document].join()"),
        "true,true,true,true,true,true"
    );
}

#[test]
fn reparenting_moves_the_root_in_and_out_of_the_document() {
    let mut b = browser();
    assert_eq!(
        js(&mut b, "var x = document.getElementById('p'), holder = document.createElement('div'); var before = x.getRootNode() === document; holder.appendChild(x); var moved = x.getRootNode() === holder; document.body.appendChild(x); var back = x.getRootNode() === document; var gone = document.body.removeChild(x) === x && x.getRootNode() === x; [before, moved, back, gone].join()"),
        "true,true,true,true"
    );
}

#[test]
fn clones_are_detached_roots_and_their_descendants_share_the_clone_root() {
    let mut b = browser();
    assert_eq!(
        js(&mut b, "var c = document.getElementById('a').cloneNode(true); [c.getRootNode() === c, c.firstChild.getRootNode() === c, c.lastChild.getRootNode() === c, document.getElementById('p').cloneNode(true).getRootNode() !== document].join()"),
        "true,true,true,true"
    );
}

#[test]
fn foreign_namespaces_follow_the_same_walk() {
    let mut b = browser();
    assert_eq!(
        js(&mut b, "var sv = document.createElementNS('http://www.w3.org/2000/svg', 'rect'), mi = document.createElementNS('http://www.w3.org/1998/Math/MathML', 'mi'); var g = document.getElementById('g'); [g.getRootNode() === document, sv.getRootNode() === sv, mi.getRootNode() === mi, g.appendChild(sv) === sv && sv.getRootNode() === document, document.getElementById('m').getRootNode() === document].join()"),
        "true,true,true,true,true"
    );
}

#[test]
fn options_are_a_dictionary_with_undefined_and_null_empty() {
    let mut b = browser();
    assert_eq!(
        js(&mut b, "var p = document.getElementById('p'); [p.getRootNode(), p.getRootNode(undefined), p.getRootNode(null), p.getRootNode({}), p.getRootNode({ composed: true }), p.getRootNode({ composed: false }), p.getRootNode(function () {}), p.getRootNode([])].every(function (r) { return r === document; })"),
        "true"
    );
}

#[test]
fn primitive_options_are_a_type_error() {
    let mut b = browser();
    assert_eq!(
        js(&mut b, "var p = document.getElementById('p'), bad = 0; [0, 1, '', 'x', true, false, NaN, Symbol('s')].forEach(function (v) { try { p.getRootNode(v); } catch (e) { if (e instanceof TypeError) bad++; } }); [typeof p.getRootNode, bad].join()"),
        "function,8"
    );
}

#[test]
fn composed_getter_runs_once_and_before_the_walk() {
    let mut b = browser();
    assert_eq!(
        js(&mut b, "var p = document.getElementById('p'), calls = 0, holder = document.createElement('div'); var r = p.getRootNode({ get composed() { calls++; holder.appendChild(p); return true; } }); [calls, r === holder].join()"),
        "1,true"
    );
}

#[test]
fn a_throwing_composed_getter_stops_the_call_with_its_error() {
    let mut b = browser();
    assert_eq!(
        js(&mut b, "var p = document.getElementById('p'), out; try { p.getRootNode({ get composed() { throw new Error('g'); } }); out = 'no'; } catch (e) { out = e.name + ':' + e.message; } out"),
        "Error:g"
    );
}

#[test]
fn composed_getter_from_another_script_sees_the_tree_it_changed() {
    let mut b = browser();
    js(&mut b, "var holder = document.createElement('div'), p = document.getElementById('p'), calls = 0; var opts = { get composed() { calls++; holder.appendChild(p); return false; } };");
    assert_eq!(js(&mut b, "[p.getRootNode(opts) === holder, calls].join()"), "true,1");
}

#[test]
fn wrong_receivers_are_type_errors_before_the_options_are_read() {
    let mut b = browser();
    assert_eq!(
        js(&mut b, "var get = Node.prototype.getRootNode, bad = 0, reads = 0, o = { get composed() { reads++; return true; } }; [{}, window, null, undefined, Object.create(Node.prototype), Element.prototype, document.getElementById('p').constructor.prototype].forEach(function (t) { try { get.call(t, o); } catch (e) { if (e instanceof TypeError) bad++; } }); [typeof get, bad, reads].join()"),
        "function,7,0"
    );
}

#[test]
fn the_depth_cap_is_a_limit_and_no_script_runs_after_it() {
    let mut b = browser_with(&deep_page(8), BrowserLimits { max_tree_depth: 10, ..Default::default() }, 5_000_000);
    assert_eq!(js(&mut b, "document.getElementById('deep').getRootNode(); globalThis.hit = 1; 'after'"), lim("dom-depth"));
    assert_eq!(js(&mut b, "typeof hit"), "undefined");
}

#[test]
fn the_depth_cap_admits_a_walk_of_exactly_the_cap() {
    // 7 divs, then body, html and the document: 10 ancestors, which is the cap.
    let mut b = browser_with(&deep_page(7), BrowserLimits { max_tree_depth: 10, ..Default::default() }, 5_000_000);
    assert_eq!(js(&mut b, "document.getElementById('deep').getRootNode() === document"), "true");
}

#[test]
fn the_step_budget_stops_the_walk_and_no_script_runs_after_it() {
    big(|| {
        let src = "var x = document.getElementById('deep'); x.getRootNode(); globalThis.hit = 1; 'after'";
        // Calibrate: the lookup alone costs `lookup` steps. The budget covers the lookup and 100 steps more,
        // and the walk to the document (over 300 steps) does not fit in what is left.
        let mut probe = browser_with(&deep_page(300), BrowserLimits::default(), 5_000_000);
        let before = probe.it.stats().steps;
        js(&mut probe, "var x = document.getElementById('deep'); 0");
        let lookup = probe.it.stats().steps - before;
        let mut b = browser_with(&deep_page(300), BrowserLimits::default(), lookup + 100);
        assert_eq!(js(&mut b, src), format!("limit:{:?}", LimitKind::Steps));
        assert_eq!(js(&mut b, "typeof hit"), "undefined");
    });
}

#[test]
fn the_walked_root_keeps_one_stable_wrapper_while_js_holds_it() {
    let mut b = browser();
    assert_eq!(js(&mut b, "var t = document.createElement('div'), k = t.appendChild(document.createElement('i')), keep = k.getRootNode(); keep === t"), "true");
    b.collect(&[]).unwrap();
    assert_eq!(js(&mut b, "[k.getRootNode() === keep, keep === t].join()"), "true,true");
}

#[test]
fn a_detached_root_is_pruned_when_js_drops_it() {
    let mut b = browser();
    js(&mut b, "var t = document.createElement('div'), k = t.appendChild(document.createElement('i')), keep = k.getRootNode();");
    let held = b.stats().wrappers;
    js(&mut b, "t = null; k = null; keep = null;");
    b.collect(&[]).unwrap();
    assert!(b.stats().wrappers < held, "held {held}, after collect {}", b.stats().wrappers);
    assert_eq!(js(&mut b, "var t2 = document.createElement('div'); t2.getRootNode() === t2"), "true");
}

#[test]
fn repeated_walks_return_the_same_root_at_1_10_and_100_calls() {
    big(|| {
        let mut b = browser_with(&deep_page(200), BrowserLimits::default(), 5_000_000);
        for n in [1, 10, 100] {
            let src = format!("var x = document.getElementById('deep'), ok = 0; for (var i = 0; i < {n}; i++) {{ if (x.getRootNode() === document) ok++; }} ok");
            assert_eq!(js(&mut b, &src), n.to_string());
        }
    });
}

#[test]
fn attributes_are_their_own_root_and_convert_options() {
    let mut b = browser();
    assert_eq!(js(&mut b, "var e = document.getElementById('p'); e.setAttribute('x', 'v'); var a = e.getAttributeNode('x'), reads = 0; var opts = {get composed() { reads++; e.removeAttributeNode(a); return true; }}; [a.getRootNode() === a, a.getRootNode(opts) === a, a.getRootNode() === a, reads, a.ownerElement === null].join()"), "true,true,true,1,true");
}

#[test]
fn the_depth_cap_and_the_step_budget_stop_the_position_walk() {
    let mut b = browser_with(&deep_page(8), BrowserLimits { max_tree_depth: 10, ..Default::default() }, 5_000_000);
    assert_eq!(js(&mut b, "document.getElementById('deep').compareDocumentPosition(document.body); globalThis.hit = 1; 'after'"), lim("dom-depth"));
    assert_eq!(js(&mut b, "typeof hit"), "undefined");
    big(|| {
        let src = "var x = document.getElementById('deep'); x.compareDocumentPosition(document.body); globalThis.hit = 1; 'after'";
        let mut probe = browser_with(&deep_page(300), BrowserLimits::default(), 5_000_000);
        let before = probe.it.stats().steps;
        js(&mut probe, "var x = document.getElementById('deep'); 0");
        let lookup = probe.it.stats().steps - before;
        let mut b = browser_with(&deep_page(300), BrowserLimits::default(), lookup + 100);
        assert_eq!(js(&mut b, src), format!("limit:{:?}", LimitKind::Steps));
        assert_eq!(js(&mut b, "typeof hit"), "undefined");
    });
}

#[test]
fn a_deep_tree_is_compared_for_equality_without_the_native_stack() {
    big(|| {
        let mut b = browser_with(&deep_page(2000), BrowserLimits { max_tree_depth: 100_000, ..Default::default() }, 50_000_000);
        assert_eq!(js(&mut b, "var d = document.body; d.isEqualNode(d.cloneNode(true))"), "true");
    });
}
