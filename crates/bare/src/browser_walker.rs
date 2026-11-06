//! `NodeFilter`, `TreeWalker` and `document.createTreeWalker`. Source: DOM Standard, "Interface TreeWalker", "Interface NodeFilter" and
//! "traversal" (the algorithms are written as the standard writes them), then compared with headless Chrome
//! (`tools/fixtures/ui/walker-cases.txt`).
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
//! Deviations (on purpose)
//! - `nextNode()` from a detached node with no later node gives `null` (the standard's text would test the same node again, which Chrome does not).
//! - A walker is a plain object with a table entry here, so `root`, `whatToShow` and `filter` are prototype getters like Chrome's. A `NodeIterator` and
//!   `document.createNodeIterator` are not built. A `DocumentFragment` root is not possible yet (`createDocumentFragment` does not exist); a shadow root is.
//!
//! Bound (`Limit(Host(name))`): `tree-walkers` (live walkers, set free by the collector).

use crate::browser::*;
use crate::browser_lazy::lazy_global;
use bare_html::dom::{NodeData, NodeId};
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;

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
}

#[derive(Default)]
struct State {
    /// The prototype and the two constructors, once made.
    made: Option<(ObjId, ObjId, ObjId)>,
    walkers: HashMap<ObjId, Walker>,
}

#[derive(Default)]
pub(crate) struct Walk {
    st: OnceCell<RefCell<State>>,
}

fn state(d: &Dom) -> &RefCell<State> {
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
        Value::Object(o) if state(d).borrow().walkers.contains_key(&o) => Ok(o),
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
    if active {
        return Err(d.dom_err(it, "InvalidStateError", &format!("Failed to execute '{method}' on 'TreeWalker': Filter function can't be recursive")));
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
                return Err(it.type_error(&format!("Failed to execute '{method}' on 'TreeWalker': Failed to execute 'acceptNode' on 'NodeFilter': The provided callback is not callable.")));
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

const CREATE: &str = "Failed to execute 'createTreeWalker' on 'Document'";

fn create_tree_walker(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    d.nd(it, this)?;
    let Some(&r) = a.first() else {
        return Err(it.type_error(&format!("{CREATE}: 1 argument required, but only 0 present.")));
    };
    let Some(root) = d.node_of(r) else {
        return Err(it.type_error(&format!("{CREATE}: parameter 1 is not of type 'Node'.")));
    };
    let what = match a.get(1).copied().unwrap_or(Value::Undefined) {
        Value::Undefined => 0xFFFF_FFFF,
        v => bare_js::value::to_uint32(it.to_js_number(v)?),
    };
    let filter = match a.get(2).copied().unwrap_or(Value::Undefined) {
        Value::Undefined | Value::Null => Value::Null,
        v @ Value::Object(_) => v,
        _ => return Err(it.type_error(&format!("{CREATE}: parameter 3 is not of type 'Object'."))),
    };
    ensure(d, it)?;
    if state(d).borrow().walkers.len() >= MAX_WALKERS {
        return Err(d.lim("tree-walkers"));
    }
    let proto = state(d).borrow().made.expect("made").0;
    let w = d.obj(it, proto)?;
    state(d).borrow_mut().walkers.insert(w, Walker { root, what, filter, cur: root, active: false });
    Ok(Value::Object(w))
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    d.def_method(it, d.p().document, "createTreeWalker", 1, create_tree_walker)?;
    crate::browser_lazy::install(d, it, &[("TreeWalker", lazy_walker as H), ("NodeFilter", lazy_filter)])
}

impl Dom {
    pub(crate) fn walk_roots(&self, out: &mut Vec<Value>) {
        if let Some(st) = self.walk.st.get()
            && let Some((p, c, n)) = st.borrow().made
        {
            out.extend([p, c, n].map(Value::Object));
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
            st.borrow_mut().walkers.retain(|&o, _| live(o));
        }
    }
}
