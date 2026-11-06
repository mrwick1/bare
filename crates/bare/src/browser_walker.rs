//! `NodeFilter`, `TreeWalker`, `NodeIterator`, `document.createTreeWalker` and `document.createNodeIterator`. Source: DOM Standard,
//! "Interface TreeWalker", "Interface NodeIterator", "Interface NodeFilter" and "traversal" (the algorithms are written as the standard writes
//! them), then compared with headless Chrome (`tools/fixtures/ui/walker-cases.txt`, `iterator-cases.txt`).
//!
//! What is built
//! - `document.createTreeWalker(root, whatToShow = 0xFFFFFFFF, filter = null)`. `root` is any node; `whatToShow` is `ToUint32`; `filter` is `null`,
//!   a function or an object with `acceptNode`. A value that is not an object gives `TypeError` (Chrome's text).
//! - `TreeWalker`: `root`, `whatToShow`, `filter` (read-only), `currentNode` (get and set; the setter takes any node), `parentNode()`,
//!   `firstChild()`, `lastChild()`, `previousSibling()`, `nextSibling()`, `previousNode()`, `nextNode()`.
//! - `NodeFilter`: the interface object with the three `FILTER_*` and the `SHOW_*` constants, read-only, not constructible.
//! - A filter is called with the node, as `this` the filter object (an object filter) or `undefined` (a function). `acceptNode` is read at each
//!   call. The result is `ToNumber` taken modulo 2^16 (`unsigned short`): 1 accepts, 2 rejects, anything else skips. A throw goes out of the
//!   method and leaves `currentNode` as it was. A call from inside a filter into the same walker is `InvalidStateError`
//!   ("Filter function can't be recursive"); the flag is cleared when the filter returns or throws. `whatToShow` is tested before the filter,
//!   and the filter is not called for a type that is not shown (that is `FILTER_SKIP`).
//! - The tree is the live light tree: a filter may change it, and a walk from a removed node goes on in the detached tree.
//!
//! - `NodeIterator`: `root`, `referenceNode`, `pointerBeforeReferenceNode`, `whatToShow`, `filter` (read-only prototype getters), `nextNode()`,
//!   `previousNode()` and `detach()` (no effect). It shares the table and the filter steps of the walker (`Walker::iter`). `FILTER_REJECT` and
//!   `FILTER_SKIP` are the same for it. The reference and the pointer change when the walk ends, so a throw in a filter leaves them as they were.
//!   The "pre-removing steps": the document watcher (`Dom::sync_watcher`) tells each iterator of a node about to leave the tree, and an iterator
//!   whose reference is in that node moves its reference out of it. As in Chrome, a node that is not inside the root of the iterator does nothing
//!   (the standard would move the reference out of the root when the root is inside the removed node). A page that makes no iterator pays nothing
//!   at a change.
//!
//! Deviations (on purpose)
//! - `nextNode()` from a detached node with no later node gives `null` (the standard's text would test the same node again, which Chrome does not).
//! - A walker is a plain object with a table entry here, so `root`, `whatToShow` and `filter` are prototype getters like Chrome's.
//!
//! Bound (`Limit(Host(name))`): `tree-walkers` (live walkers, set free by the collector).

use crate::browser::*;
use crate::browser_lazy::lazy_global;
use bare_html::dom::{Change, Document, NodeData, NodeId};
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

type V = R<Value>;

const MAX_WALKERS: usize = 1 << 16;
const ACCEPT: u16 = 1;
const REJECT: u16 = 2;
const SKIP: u16 = 3;

struct Walker {
    root: NodeId,
    what: u32,
    /// `Value::Null` or an object.
    filter: Value,
    cur: NodeId,
    /// A filter of this walker is running.
    active: bool,
    /// A `NodeIterator`: `cur` is its reference node.
    iter: bool,
    /// A `NodeIterator`: the pointer is before the reference node.
    before: bool,
    /// A `NodeIterator` that runs a filter: the node it looks at, and whether the pointer is before it. Removals move it too.
    cand: Option<(NodeId, bool)>,
}

#[derive(Default)]
pub(crate) struct State {
    /// The prototype and the two constructors, once made.
    made: Option<(ObjId, ObjId, ObjId)>,
    /// The prototype and the constructor of `NodeIterator`, once made.
    made_iter: Option<(ObjId, ObjId)>,
    walkers: HashMap<ObjId, Walker>,
    /// How many of `walkers` are iterators.
    iters: usize,
    /// "Replace all" is running: its iterators were told already.
    told: bool,
}

#[derive(Default)]
pub(crate) struct Walk {
    st: OnceCell<Rc<RefCell<State>>>,
}

impl Walk {
    /// The state, when a `NodeIterator` is alive (for the watcher).
    pub(crate) fn live(&self) -> Option<Rc<RefCell<State>>> {
        self.st.get().filter(|s| s.borrow().iters > 0).cloned()
    }
}

fn state(d: &Dom) -> &Rc<RefCell<State>> {
    d.walk.st.get_or_init(Default::default)
}

const SHOW: [(&str, u32); 13] = [
    ("SHOW_ALL", 0xFFFF_FFFF),
    ("SHOW_ELEMENT", 0x1),
    ("SHOW_ATTRIBUTE", 0x2),
    ("SHOW_TEXT", 0x4),
    ("SHOW_CDATA_SECTION", 0x8),
    ("SHOW_ENTITY_REFERENCE", 0x10),
    ("SHOW_ENTITY", 0x20),
    ("SHOW_PROCESSING_INSTRUCTION", 0x40),
    ("SHOW_COMMENT", 0x80),
    ("SHOW_DOCUMENT", 0x100),
    ("SHOW_DOCUMENT_TYPE", 0x200),
    ("SHOW_DOCUMENT_FRAGMENT", 0x400),
    ("SHOW_NOTATION", 0x800),
];

/// The `SHOW_*` bit of a node: bit `nodeType - 1`.
fn bit(d: &Dom, n: NodeId) -> u32 {
    match d.data(n) {
        NodeData::Element { .. } => 0x1,
        NodeData::Text(_) => 0x4,
        NodeData::Comment(_) => 0x80,
        NodeData::Document => 0x100,
        NodeData::Doctype(_) => 0x200,
        NodeData::ShadowRoot => 0x400,
    }
}

fn walker_of(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<ObjId> {
    match this {
        Value::Object(o) if state(d).borrow().walkers.get(&o).is_some_and(|w| !w.iter) => Ok(o),
        _ => Err(it.type_error("Illegal invocation")),
    }
}

fn iter_of(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<ObjId> {
    match this {
        Value::Object(o) if state(d).borrow().walkers.get(&o).is_some_and(|w| w.iter) => Ok(o),
        _ => Err(it.type_error("Illegal invocation")),
    }
}

/// The filter steps: `SKIP` for a type that is not shown, `ACCEPT` with no filter, else the call. `method` names the caller in errors.
fn filter(d: &Dom, it: &mut Interp<'_>, w: ObjId, method: &str, n: NodeId) -> R<u16> {
    it.charge(1)?;
    let (active, what, f) = {
        let s = state(d).borrow();
        let k = &s.walkers[&w];
        (k.active, k.what, k.filter)
    };
    let iface = if state(d).borrow().walkers[&w].iter { "NodeIterator" } else { "TreeWalker" };
    if active {
        return Err(d.dom_err(it, "InvalidStateError", &format!("Failed to execute '{method}' on '{iface}': Filter function can't be recursive")));
    }
    if what & bit(d, n) == 0 {
        return Ok(SKIP);
    }
    if matches!(f, Value::Null) {
        return Ok(ACCEPT);
    }
    state(d).borrow_mut().walkers.get_mut(&w).expect("live").active = true;
    let r = (|| -> R<u16> {
        let node = d.wrap(it, n)?;
        let (func, this) = if it.is_callable_value(f) {
            (f, Value::Undefined)
        } else {
            let k = d.key(it, "acceptNode")?;
            let g = it.get_value(f, k)?;
            if !it.is_callable_value(g) {
                return Err(it.type_error(&format!("Failed to execute '{method}' on '{iface}': Failed to execute 'acceptNode' on 'NodeFilter': The provided callback is not callable.")));
            }
            (g, f)
        };
        let v = it.call(func, this, &[node])?;
        let x = it.to_js_number(v)?;
        Ok(if x.is_finite() { x.trunc().rem_euclid(65536.0) as u16 } else { 0 })
    })();
    state(d).borrow_mut().walkers.get_mut(&w).expect("live").active = false;
    r
}

fn set_current(d: &Dom, it: &mut Interp<'_>, w: ObjId, n: NodeId) -> V {
    state(d).borrow_mut().walkers.get_mut(&w).expect("live").cur = n;
    d.wrap(it, n)
}

fn walker_fields(d: &Dom, w: ObjId) -> (NodeId, NodeId) {
    let s = state(d).borrow();
    (s.walkers[&w].root, s.walkers[&w].cur)
}

fn parent_node(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let w = walker_of(d, it, this)?;
    let (root, mut node) = walker_fields(d, w);
    while node != root {
        let Some(p) = d.doc.borrow().parent(node) else { break };
        node = p;
        if filter(d, it, w, "parentNode", node)? == ACCEPT {
            return set_current(d, it, w, node);
        }
    }
    Ok(Value::Null)
}

/// "Traverse children": `first` takes the first child and goes right, else the last child and goes left.
fn children(d: &Dom, it: &mut Interp<'_>, this: Value, first: bool) -> V {
    let method = if first { "firstChild" } else { "lastChild" };
    let w = walker_of(d, it, this)?;
    let (root, cur) = walker_fields(d, w);
    let pick = |n: NodeId| {
        let doc = d.doc.borrow();
        if first { doc.children(n).next() } else { doc.last_child(n) }
    };
    let mut node = pick(cur);
    'outer: while let Some(n) = node {
        let r = filter(d, it, w, method, n)?;
        if r == ACCEPT {
            return set_current(d, it, w, n);
        }
        if r == SKIP
            && let Some(c) = pick(n)
        {
            node = Some(c);
            continue;
        }
        let mut at = n;
        loop {
            let (sib, parent) = {
                let doc = d.doc.borrow();
                (if first { doc.next_sibling(at) } else { doc.prev_sibling(at) }, doc.parent(at))
            };
            if let Some(s) = sib {
                node = Some(s);
                continue 'outer;
            }
            match parent {
                Some(p) if p != root && p != cur => at = p,
                _ => return Ok(Value::Null),
            }
        }
    }
    Ok(Value::Null)
}

/// "Traverse siblings": `next` goes right, else left.
fn siblings(d: &Dom, it: &mut Interp<'_>, this: Value, next: bool) -> V {
    let method = if next { "nextSibling" } else { "previousSibling" };
    let w = walker_of(d, it, this)?;
    let (root, mut node) = walker_fields(d, w);
    if node == root {
        return Ok(Value::Null);
    }
    let step = |n: NodeId| {
        let doc = d.doc.borrow();
        if next { doc.next_sibling(n) } else { doc.prev_sibling(n) }
    };
    loop {
        let mut sibling = step(node);
        while let Some(s) = sibling {
            node = s;
            let r = filter(d, it, w, method, node)?;
            if r == ACCEPT {
                return set_current(d, it, w, node);
            }
            sibling = {
                let doc = d.doc.borrow();
                if next { doc.children(node).next() } else { doc.last_child(node) }
            };
            if r == REJECT || sibling.is_none() {
                sibling = step(node);
            }
        }
        let Some(p) = d.doc.borrow().parent(node) else { return Ok(Value::Null) };
        node = p;
        if node == root || filter(d, it, w, method, node)? == ACCEPT {
            return Ok(Value::Null);
        }
    }
}

fn previous_node(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let w = walker_of(d, it, this)?;
    let (root, mut node) = walker_fields(d, w);
    while node != root {
        let mut sibling = d.doc.borrow().prev_sibling(node);
        while let Some(s) = sibling {
            node = s;
            let mut r = filter(d, it, w, "previousNode", node)?;
            while r != REJECT {
                let Some(last) = d.doc.borrow().last_child(node) else { break };
                node = last;
                r = filter(d, it, w, "previousNode", node)?;
            }
            if r == ACCEPT {
                return set_current(d, it, w, node);
            }
            sibling = d.doc.borrow().prev_sibling(node);
        }
        let Some(p) = d.doc.borrow().parent(node) else { return Ok(Value::Null) };
        node = p;
        if filter(d, it, w, "previousNode", node)? == ACCEPT {
            return set_current(d, it, w, node);
        }
    }
    Ok(Value::Null)
}

fn next_node(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let w = walker_of(d, it, this)?;
    let (root, mut node) = walker_fields(d, w);
    let mut r = ACCEPT;
    loop {
        while r != REJECT {
            let Some(c) = d.doc.borrow().children(node).next() else { break };
            node = c;
            r = filter(d, it, w, "nextNode", node)?;
            if r == ACCEPT {
                return set_current(d, it, w, node);
            }
        }
        let mut up = node;
        let found = loop {
            if up == root {
                return Ok(Value::Null);
            }
            let (sib, parent) = {
                let doc = d.doc.borrow();
                (doc.next_sibling(up), doc.parent(up))
            };
            if let Some(s) = sib {
                break s;
            }
            match parent {
                Some(p) => up = p,
                None => return Ok(Value::Null),
            }
        };
        node = found;
        r = filter(d, it, w, "nextNode", node)?;
        if r == ACCEPT {
            return set_current(d, it, w, node);
        }
    }
}

fn first_child(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    children(d, it, this, true)
}
fn last_child(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    children(d, it, this, false)
}
fn next_sibling(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    siblings(d, it, this, true)
}
fn previous_sibling(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    siblings(d, it, this, false)
}

fn root_get(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let w = walker_of(d, it, this)?;
    d.wrap(it, walker_fields(d, w).0)
}

fn what_get(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let w = walker_of(d, it, this)?;
    Ok(Value::Number(state(d).borrow().walkers[&w].what as f64))
}

fn filter_get(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let w = walker_of(d, it, this)?;
    Ok(state(d).borrow().walkers[&w].filter)
}

fn current_get(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let w = walker_of(d, it, this)?;
    d.wrap(it, walker_fields(d, w).1)
}

fn current_set(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let w = walker_of(d, it, this)?;
    let Some(n) = a.first().and_then(|&v| d.node_of(v)) else {
        return Err(it.type_error("Failed to set the 'currentNode' property on 'TreeWalker': Failed to convert value to 'Node'."));
    };
    state(d).borrow_mut().walkers.get_mut(&w).expect("live").cur = n;
    Ok(Value::Undefined)
}

fn illegal_walker(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Err(it.type_error("Failed to construct 'TreeWalker': Illegal constructor"))
}

fn illegal_filter(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Err(it.type_error("Failed to construct 'NodeFilter': Illegal constructor"))
}

fn ensure(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    if state(d).borrow().made.is_some() {
        return Ok(());
    }
    let op = it.object_prototype();
    let proto = d.obj(it, op)?;
    let (kc, kp) = (d.key(it, "constructor")?, d.key(it, "prototype")?);
    let ctor = d.add_fn(it, illegal_walker, "TreeWalker", 0, true)?;
    it.define_data(ctor, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(ctor), true, false, true)?;
    crate::browser_lazy::tag(d, it, proto, "TreeWalker")?;
    d.def_get(it, proto, "root", root_get, None)?;
    d.def_get(it, proto, "whatToShow", what_get, None)?;
    d.def_get(it, proto, "filter", filter_get, None)?;
    d.def_get(it, proto, "currentNode", current_get, Some(current_set))?;
    for (name, h) in [("parentNode", parent_node as H), ("firstChild", first_child), ("lastChild", last_child), ("previousSibling", previous_sibling), ("nextSibling", next_sibling), ("previousNode", previous_node), ("nextNode", next_node)] {
        d.def_method(it, proto, name, 0, h)?;
    }
    // `NodeFilter` is a callback interface: an interface object with constants, and no prototype.
    let nf = d.add_fn(it, illegal_filter, "NodeFilter", 0, true)?;
    for (name, v) in [("FILTER_ACCEPT", ACCEPT as u32), ("FILTER_REJECT", REJECT as u32), ("FILTER_SKIP", SKIP as u32)].into_iter().chain(SHOW) {
        let k = d.key(it, name)?;
        it.define_data(nf, k, Value::Number(v as f64), false, true, false)?;
    }
    state(d).borrow_mut().made = Some((proto, ctor, nf));
    Ok(())
}

lazy_global!(lazy_walker, "TreeWalker", ensure, |d| state(d).borrow().made.expect("made").1);
lazy_global!(lazy_filter, "NodeFilter", ensure, |d| state(d).borrow().made.expect("made").2);

/// `createTreeWalker` and `createNodeIterator`: the same three arguments.
fn create(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], iter: bool) -> V {
    let who = if iter { "createNodeIterator" } else { "createTreeWalker" };
    d.nd(it, this)?;
    let Some(&r) = a.first() else {
        return Err(it.type_error(&format!("Failed to execute '{who}' on 'Document': 1 argument required, but only 0 present.")));
    };
    let Some(root) = d.node_of(r) else {
        return Err(it.type_error(&format!("Failed to execute '{who}' on 'Document': parameter 1 is not of type 'Node'.")));
    };
    let what = match a.get(1).copied().unwrap_or(Value::Undefined) {
        Value::Undefined => 0xFFFF_FFFF,
        v => bare_js::value::to_uint32(it.to_js_number(v)?),
    };
    let filter = match a.get(2).copied().unwrap_or(Value::Undefined) {
        Value::Undefined | Value::Null => Value::Null,
        v @ Value::Object(_) => v,
        _ => return Err(it.type_error(&format!("Failed to execute '{who}' on 'Document': parameter 3 is not of type 'Object'."))),
    };
    if iter { ensure_iter(d, it)? } else { ensure(d, it)? }
    if state(d).borrow().walkers.len() >= MAX_WALKERS {
        return Err(d.lim("tree-walkers"));
    }
    let proto = {
        let s = state(d).borrow();
        if iter { s.made_iter.expect("made").0 } else { s.made.expect("made").0 }
    };
    let w = d.obj(it, proto)?;
    let first = {
        let mut s = state(d).borrow_mut();
        s.walkers.insert(w, Walker { root, what, filter, cur: root, active: false, iter, before: true, cand: None });
        s.iters += iter as usize;
        iter && s.iters == 1
    };
    if first {
        d.sync_watcher();
    }
    Ok(Value::Object(w))
}

fn create_tree_walker(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    create(d, it, this, a, false)
}

fn create_node_iterator(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    create(d, it, this, a, true)
}

// ------------------------------------------------------------ NodeIterator

/// The last node of the tree of `n` in tree order: `n` itself, or its last descendant.
fn last_inclusive(doc: &Document, mut n: NodeId) -> NodeId {
    while let Some(c) = doc.last_child(n) {
        n = c;
    }
    n
}

/// The first node after `n` in tree order that is not in `n`, inside `root`.
fn after_subtree(doc: &Document, root: NodeId, mut n: NodeId) -> Option<NodeId> {
    while n != root {
        if let Some(s) = doc.next_sibling(n) {
            return Some(s);
        }
        n = doc.parent(n)?;
    }
    None
}

/// The node after `n` in the tree of `root`, in tree order.
fn following(doc: &Document, root: NodeId, n: NodeId) -> Option<NodeId> {
    doc.children(n).next().or_else(|| after_subtree(doc, root, n))
}

/// The node before `n` in the tree of `root`, in tree order.
fn preceding(doc: &Document, root: NodeId, n: NodeId) -> Option<NodeId> {
    if n == root {
        return None;
    }
    // A node that left the tree (a reference node that a removal did not move) has no parent: nothing precedes it.
    doc.prev_sibling(n).map_or_else(|| doc.parent(n), |p| Some(last_inclusive(doc, p)))
}

impl Walker {
    /// The "pre-removing steps" of the standard, for the node `gone`, which is about to leave `parent`.
    /// As in Chrome, a pointer before the reference node stays when no node follows `gone` in the root.
    fn removing(&mut self, doc: &Document, gone: NodeId, parent: NodeId) {
        let mut up = Some(parent);
        while up != Some(self.root) {
            let Some(p) = up else { return };
            up = doc.parent(p);
        }
        let root = self.root;
        let one = |cur: &mut NodeId, before: &mut bool| {
            let mut c = Some(*cur);
            while c != Some(gone) {
                let Some(n) = c else { return };
                c = doc.parent(n);
            }
            if *before {
                let Some(n) = after_subtree(doc, root, gone) else { return };
                *cur = n;
                return;
            }
            *cur = doc.prev_sibling(gone).map_or(parent, |p| last_inclusive(doc, p));
        };
        one(&mut self.cur, &mut self.before);
        if let Some((c, b)) = &mut self.cand {
            one(c, b);
        }
    }
}

impl State {
    /// Tell the iterators of a node that is about to leave the tree.
    pub(crate) fn on_change(&mut self, doc: &Document, ch: Change) {
        if let (Change::Unlink { parent, node, .. }, false) = (ch, self.told) {
            for w in self.walkers.values_mut().filter(|w| w.iter) {
                w.removing(doc, node, parent);
            }
        }
    }
}

impl Dom {
    /// As in Chrome, "replace all" tells the iterators about every child of `parent`, with the tree whole, before it removes the first
    /// (the standard removes one by one). The per-node removals are muted until `iter_told_end`.
    pub(crate) fn iter_told_all(&self, doc: &Document, parent: NodeId) {
        let Some(st) = self.walk.live() else { return };
        let mut s = st.borrow_mut();
        for c in doc.children(parent) {
            for w in s.walkers.values_mut().filter(|w| w.iter) {
                w.removing(doc, c, parent);
            }
        }
        s.told = true;
    }

    pub(crate) fn iter_told_end(&self) {
        if let Some(st) = self.walk.st.get() {
            st.borrow_mut().told = false;
        }
    }
}

/// "Traverse": `next` goes on in tree order, else back.
fn traverse(d: &Dom, it: &mut Interp<'_>, this: Value, next: bool) -> V {
    let method = if next { "nextNode" } else { "previousNode" };
    let w = iter_of(d, it, this)?;
    let (root, mut node, mut before) = {
        let s = state(d).borrow();
        let k = &s.walkers[&w];
        (k.root, k.cur, k.before)
    };
    let mut found = None;
    let res = loop {
        if before == next {
            before = !next;
        } else {
            let step = {
                let doc = d.doc.borrow();
                if next { following(&doc, root, node) } else { preceding(&doc, root, node) }
            };
            let Some(n) = step else { break Ok(Value::Null) };
            node = n;
            before = !next;
        }
        // A removal during the filter moves the candidate. As in Chrome, the reference node becomes the moved candidate, while the
        // result stays the node that was found.
        state(d).borrow_mut().walkers.get_mut(&w).expect("live").cand = Some((node, before));
        let r = filter(d, it, w, method, node);
        let cand = state(d).borrow().walkers[&w].cand;
        match r {
            Ok(ACCEPT) => {
                found = cand;
                break d.wrap(it, node);
            }
            Ok(_) => (node, before) = cand.expect("set"),
            Err(e) => break Err(e),
        }
    };
    let mut s = state(d).borrow_mut();
    let k = s.walkers.get_mut(&w).expect("live");
    k.cand = None;
    if let Some((n, b)) = found {
        (k.cur, k.before) = (n, b);
    }
    res
}

fn next_iter(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    traverse(d, it, this, true)
}

fn previous_iter(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    traverse(d, it, this, false)
}

fn detach_iter(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    iter_of(d, it, this)?;
    Ok(Value::Undefined)
}

fn iter_get(d: &Dom, it: &mut Interp<'_>, this: Value, f: fn(&Walker) -> Value) -> V {
    let w = iter_of(d, it, this)?;
    let v = f(&state(d).borrow().walkers[&w]);
    Ok(v)
}

fn iter_root(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let w = iter_of(d, it, this)?;
    let n = state(d).borrow().walkers[&w].root;
    d.wrap(it, n)
}

fn iter_reference(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let w = iter_of(d, it, this)?;
    let n = state(d).borrow().walkers[&w].cur;
    d.wrap(it, n)
}

fn iter_pointer(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    iter_get(d, it, this, |k| Value::Bool(k.before))
}

fn iter_what(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    iter_get(d, it, this, |k| Value::Number(k.what as f64))
}

fn iter_filter(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    iter_get(d, it, this, |k| k.filter)
}

fn illegal_iterator(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Err(it.type_error("Failed to construct 'NodeIterator': Illegal constructor"))
}

fn ensure_iter(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    if state(d).borrow().made_iter.is_some() {
        return Ok(());
    }
    let op = it.object_prototype();
    let proto = d.obj(it, op)?;
    let (kc, kp) = (d.key(it, "constructor")?, d.key(it, "prototype")?);
    let ctor = d.add_fn(it, illegal_iterator, "NodeIterator", 0, true)?;
    it.define_data(ctor, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(ctor), true, false, true)?;
    crate::browser_lazy::tag(d, it, proto, "NodeIterator")?;
    let gets: [(&str, H); 5] = [("root", iter_root), ("referenceNode", iter_reference), ("pointerBeforeReferenceNode", iter_pointer), ("whatToShow", iter_what), ("filter", iter_filter)];
    for (name, h) in gets {
        d.def_get(it, proto, name, h, None)?;
    }
    for (name, h) in [("nextNode", next_iter as H), ("previousNode", previous_iter), ("detach", detach_iter)] {
        d.def_method(it, proto, name, 0, h)?;
    }
    state(d).borrow_mut().made_iter = Some((proto, ctor));
    Ok(())
}

lazy_global!(lazy_iterator, "NodeIterator", ensure_iter, |d| state(d).borrow().made_iter.expect("made").1);

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    d.def_method(it, d.p().document, "createTreeWalker", 1, create_tree_walker)?;
    d.def_method(it, d.p().document, "createNodeIterator", 1, create_node_iterator)?;
    crate::browser_lazy::install(d, it, &[("TreeWalker", lazy_walker as H), ("NodeFilter", lazy_filter), ("NodeIterator", lazy_iterator)])
}

impl Dom {
    pub(crate) fn walk_roots(&self, out: &mut Vec<Value>) {
        if let Some(st) = self.walk.st.get() {
            let s = st.borrow();
            let (a, b) = s.made_iter.unzip();
            out.extend(s.made.into_iter().flat_map(|(p, c, n)| [p, c, n]).chain(a).chain(b).map(Value::Object));
        }
    }

    pub(crate) fn walk_trace(&self, owner: ObjId, out: &mut Vec<Value>) {
        if let Some(st) = self.walk.st.get()
            && let Some(w) = st.borrow().walkers.get(&owner)
        {
            out.push(w.filter);
        }
    }

    pub(crate) fn walk_prune(&self, live: &dyn Fn(ObjId) -> bool) {
        if let Some(st) = self.walk.st.get() {
            let mut s = st.borrow_mut();
            let had = s.iters;
            s.walkers.retain(|&o, _| live(o));
            s.iters = s.walkers.values().filter(|w| w.iter).count();
            let gone = had > 0 && s.iters == 0;
            drop(s);
            if gone {
                self.sync_watcher();
            }
        }
    }
}
