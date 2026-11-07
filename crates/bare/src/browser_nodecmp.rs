//! `Node` comparison and namespace lookup: `compareDocumentPosition`, `contains`, `isSameNode`, `isEqualNode`, `lookupNamespaceURI`, `lookupPrefix`,
//! `isDefaultNamespace` and the `Node.DOCUMENT_POSITION_*` constants (DOM Standard, "Node").
//!
//! Every method takes a node or an `Attr` (an `Attr` is a `Node` that is not in the tree: it sits next to its owner element). Checked against
//! headless Chrome 155 (`tools/fixtures/ui/nodecmp-cases.txt`, answers in `chrome-nodecmp.txt`).
//!
//! `compareDocumentPosition` follows the Standard step by step, with the roles swapped as it does: `node1` is the argument, `node2` is `this`.
//! The tree is the plain one (no shadow-including step), so a shadow tree, the contents of a `template` and a fragment are trees of their own and are
//! `DISCONNECTED` from the page. Two nodes in different trees give `DISCONNECTED | IMPLEMENTATION_SPECIFIC` and `PRECEDING` or `FOLLOWING`. Chrome
//! picks the side by the addresses of the two nodes. Here the side comes from the node numbers (the order of creation), and an `Attr` comes after
//! every node: `a.compareDocumentPosition(b)` is `FOLLOWING` when `b` was made after `a`, and the answer is the same every time. Chrome gives the
//! same for two elements. For nodes of different kinds (text, comment, fragment, document) the side in Chrome depends on its allocator: no page can
//! rely on it, and this host does not copy it.
//!
//! `lookupNamespaceURI`, `lookupPrefix` and `isDefaultNamespace` read the namespace of an element from the host (HTML, SVG, MathML) and the `xmlns`
//! attributes from the attribute list. `isEqualNode` does not compare the contents of a `template` (the Standard compares children only).
//!
//! Deviations: `Node.ATTRIBUTE_NODE`, `CDATA_SECTION_NODE` and the other legacy node constants are not defined. A doctype has no public or system
//! id here, so two doctypes are equal when their names are.
//!
//! Costs: a walk charges one step for each node it visits. `compareDocumentPosition` walks the two parent chains (the depth cap applies) and, for
//! two siblings' branches, the children of their common parent up to the first of the two. `isEqualNode` is iterative: its stack is on the heap,
//! so a deep tree is no limit.

use crate::browser::*;
use crate::browser_attr as at;
use bare_html::dom::{Atom, NodeData, NodeId, Ns};
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};

type V = R<Value>;

const DISCONNECTED: u16 = 1;
const PRECEDING: u16 = 2;
const FOLLOWING: u16 = 4;
const CONTAINS: u16 = 8;
const CONTAINED_BY: u16 = 16;
const IMPL: u16 = 32;

/// What a `Node` method can be called on or with: a tree node or an `Attr`.
#[derive(Clone, Copy, PartialEq)]
enum Ep {
    N(NodeId),
    A(ObjId),
}

impl Dom {
    fn ep(&self, v: Value) -> Option<Ep> {
        self.node_of(v).map(Ep::N).or_else(|| at::probe(self, v).map(Ep::A))
    }

    fn this_ep(&self, it: &mut Interp<'_>, this: Value) -> R<Ep> {
        self.ep(this).ok_or_else(|| it.type_error("Illegal invocation"))
    }

    /// Argument 0 as a node or `Attr`. A missing argument is a TypeError; `null` and `undefined` are `None` when `nullable`.
    fn other_ep(&self, it: &mut Interp<'_>, a: &[Value], name: &str, nullable: bool) -> R<Option<Ep>> {
        if a.is_empty() {
            return Err(it.type_error(&format!("Failed to execute '{name}' on 'Node': 1 argument required, but only 0 present.")));
        }
        if nullable && matches!(a[0], Value::Null | Value::Undefined) {
            return Ok(None);
        }
        match self.ep(a[0]) {
            Some(e) => Ok(Some(e)),
            None => Err(it.type_error(&format!("Failed to execute '{name}' on 'Node': parameter 1 is not of type 'Node'."))),
        }
    }

    /// An optional string argument (`DOMString?`): `null`, `undefined` and the empty string are `None`.
    fn opt_string(&self, it: &mut Interp<'_>, a: &[Value], name: &str) -> R<Option<String>> {
        if a.is_empty() {
            return Err(it.type_error(&format!("Failed to execute '{name}' on 'Node': 1 argument required, but only 0 present.")));
        }
        crate::browser_namespace::ns_arg(self, it, a[0])
    }

    /// The nodes from the root of the tree down to `n`.
    fn path_to_root(&self, it: &mut Interp<'_>, n: NodeId) -> R<Vec<NodeId>> {
        let doc = self.doc.borrow();
        let mut up = vec![n];
        let mut cur = n;
        while let Some(p) = doc.parent(cur) {
            it.charge(1)?;
            if up.len() > self.lim.max_tree_depth {
                return Err(self.lim("dom-depth"));
            }
            up.push(p);
            cur = p;
        }
        up.reverse();
        Ok(up)
    }

    /// The element of `e` and, for an `Attr`, its name.
    fn split(&self, e: Ep) -> (Option<NodeId>, Option<Atom>) {
        match e {
            Ep::N(n) => (Some(n), None),
            Ep::A(o) => {
                let (atom, owner, _) = at::state(self, o);
                (owner, Some(atom))
            }
        }
    }
}

/// A number that puts any two nodes in one fixed order: the nodes by creation, then the attributes.
fn key(e: Ep) -> (u8, usize) {
    match e {
        Ep::N(n) => (0, n.index()),
        Ep::A(o) => (1, o.slot()),
    }
}

/// `Node.prototype.compareDocumentPosition(other)`.
pub(crate) fn compare_document_position(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let this = d.this_ep(it, this)?;
    let other = d.other_ep(it, a, "compareDocumentPosition", false)?.expect("not nullable");
    if this == other {
        return Ok(Value::Number(0.0));
    }
    Ok(Value::Number(f64::from(position(d, it, this, other)?)))
}

#[inline(never)]
fn position(d: &Dom, it: &mut Interp<'_>, this: Ep, other: Ep) -> R<u16> {
    let ((n1, a1), (n2, a2)) = (d.split(other), d.split(this));
    // Two attributes of one element: the order of the attribute list.
    if let (Some(el), Some(x), Some(y), true) = (n1, a1, a2, n1 == n2) {
        let doc = d.doc.borrow();
        if let NodeData::Element { attrs, .. } = *doc.data(el) {
            for at in doc.attrs(attrs) {
                it.charge(1)?;
                if at.name == x {
                    return Ok(IMPL | PRECEDING);
                }
                if at.name == y {
                    return Ok(IMPL | FOLLOWING);
                }
            }
        }
    }
    let (c1, c2) = match (n1, n2) {
        (Some(x), Some(y)) => (d.path_to_root(it, x)?, d.path_to_root(it, y)?),
        _ => (Vec::new(), Vec::new()),
    };
    if c1.is_empty() || c1[0] != c2[0] {
        return Ok(DISCONNECTED | IMPL | if key(other) > key(this) { FOLLOWING } else { PRECEDING });
    }
    let k = c1.iter().zip(&c2).take_while(|(x, y)| x == y).count();
    let (ancestor, descendant, same) = (k == c1.len() && c1.len() < c2.len(), k == c2.len() && c2.len() < c1.len(), c1.len() == k && c2.len() == k);
    if (ancestor && a1.is_none()) || (same && a2.is_some()) {
        return Ok(CONTAINS | PRECEDING);
    }
    if (descendant && a2.is_none()) || (same && a1.is_some()) {
        return Ok(CONTAINED_BY | FOLLOWING);
    }
    let before = if ancestor {
        true
    } else if descendant {
        false
    } else {
        // The branches split under c1[k - 1]: the first of the two children in the child list is first in tree order.
        let doc = d.doc.borrow();
        let mut first = true;
        for c in doc.children(c1[k - 1]) {
            it.charge(1)?;
            if c == c1[k] {
                break;
            }
            if c == c2[k] {
                first = false;
                break;
            }
        }
        first
    };
    Ok(if before { PRECEDING } else { FOLLOWING })
}

/// `Node.prototype.contains(other)`: `other` is `this` or one of its descendants. An `Attr` is not a descendant of anything.
pub(crate) fn contains(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let this = d.this_ep(it, this)?;
    let Some(other) = d.other_ep(it, a, "contains", true)? else { return Ok(Value::Bool(false)) };
    let (Ep::N(n), Ep::N(o)) = (this, other) else { return Ok(Value::Bool(this == other)) };
    let doc = d.doc.borrow();
    let mut cur = Some(o);
    while let Some(c) = cur {
        it.charge(1)?;
        if c == n {
            return Ok(Value::Bool(true));
        }
        cur = doc.parent(c);
    }
    Ok(Value::Bool(false))
}

/// `Node.prototype.isSameNode(other)`.
pub(crate) fn is_same_node(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let this = d.this_ep(it, this)?;
    let other = d.other_ep(it, a, "isSameNode", true)?;
    Ok(Value::Bool(other == Some(this)))
}

/// `Node.prototype.isEqualNode(other)`.
pub(crate) fn is_equal_node(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let this = d.this_ep(it, this)?;
    let Some(other) = d.other_ep(it, a, "isEqualNode", true)? else { return Ok(Value::Bool(false)) };
    let eq = match (this, other) {
        (Ep::A(x), Ep::A(y)) => {
            let doc = d.doc.borrow();
            let ((p, _, _), (q, _, _)) = (at::state(d, x), at::state(d, y));
            doc.attr_ns(p) == doc.attr_ns(q) && doc.attr_local(p) == doc.attr_local(q) && at::value_of(d, x) == at::value_of(d, y)
        }
        (Ep::N(x), Ep::N(y)) => trees_equal(d, it, x, y)?,
        _ => false,
    };
    Ok(Value::Bool(eq))
}

/// The node-by-node test of the Standard, over an explicit stack.
#[inline(never)]
fn trees_equal(d: &Dom, it: &mut Interp<'_>, x: NodeId, y: NodeId) -> R<bool> {
    let doc = d.doc.borrow();
    let mut stack = vec![(x, y)];
    while let Some((p, q)) = stack.pop() {
        it.charge(1)?;
        let same = match (*doc.data(p), *doc.data(q)) {
            (NodeData::Document, NodeData::Document) | (NodeData::ShadowRoot, NodeData::ShadowRoot) => true,
            (NodeData::Doctype(s), NodeData::Doctype(t)) | (NodeData::Text(s), NodeData::Text(t)) | (NodeData::Comment(s), NodeData::Comment(t)) => doc.text(s) == doc.text(t),
            (NodeData::Element { name: n1, attrs: l1 }, NodeData::Element { name: n2, attrs: l2 }) => {
                let (l1, l2) = (doc.attrs(l1), doc.attrs(l2));
                n1 == n2
                    && doc.element_ns(p) == doc.element_ns(q)
                    && doc.element_prefix(p) == doc.element_prefix(q)
                    && l1.len() == l2.len()
                    && l1.iter().all(|x| l2.iter().any(|y| doc.attr_ns(x.name) == doc.attr_ns(y.name) && doc.attr_local(x.name) == doc.attr_local(y.name) && doc.text(x.value) == doc.text(y.value)))
            }
            _ => false,
        };
        if !same {
            return Ok(false);
        }
        let (mut i, mut j) = (doc.children(p), doc.children(q));
        loop {
            match (i.next(), j.next()) {
                (None, None) => break,
                (Some(c), Some(e)) => stack.push((c, e)),
                _ => return Ok(false),
            }
        }
    }
    Ok(true)
}

/// The element where a lookup starts: the owner of an `Attr`, the document element of a `Document`, the parent element of a node that is below
/// an element. `None` for a doctype, a fragment, a shadow root and a node with no element above it.
fn start(d: &Dom, e: Ep) -> Option<NodeId> {
    let doc = d.doc.borrow();
    let n = match e {
        Ep::A(o) => return at::state(d, o).1,
        Ep::N(n) => n,
    };
    match *doc.data(n) {
        NodeData::Element { .. } => Some(n),
        NodeData::Document => doc.children(n).find(|&c| matches!(doc.data(c), NodeData::Element { .. })),
        NodeData::Doctype(_) | NodeData::ShadowRoot => None,
        NodeData::Text(_) | NodeData::Comment(_) => doc.parent(n).filter(|&p| matches!(doc.data(p), NodeData::Element { .. })),
    }
}

fn parent_element(d: &Dom, n: NodeId) -> Option<NodeId> {
    let doc = d.doc.borrow();
    doc.parent(n).filter(|&p| matches!(doc.data(p), NodeData::Element { .. }))
}

/// "Locate a namespace" for `prefix` (`None` is the default namespace), from `el` up.
fn locate_namespace(d: &Dom, it: &mut Interp<'_>, el: NodeId, prefix: Option<&str>) -> R<Option<String>> {
    if prefix == Some("xml") {
        return Ok(Some(Ns::Xml.uri().to_string()));
    }
    if prefix == Some("xmlns") {
        return Ok(Some(Ns::Xmlns.uri().to_string()));
    }
    let mut cur = Some(el);
    while let Some(e) = cur {
        it.charge(1)?;
        let doc = d.doc.borrow();
        if doc.element_prefix(e) == prefix {
            return Ok(Some(doc.element_ns(e).uri().to_string()));
        }
        if let NodeData::Element { attrs, .. } = *doc.data(e) {
            for x in doc.attrs(attrs) {
                if doc.attr_ns(x.name) != Some(Ns::Xmlns) {
                    continue;
                }
                let hit = match prefix {
                    Some(p) => doc.attr_prefix(x.name) == Some("xmlns") && doc.attr_local(x.name) == p,
                    None => doc.attr_prefix(x.name).is_none() && doc.attr_local(x.name) == "xmlns",
                };
                if hit {
                    let v = doc.text(x.value);
                    return Ok(Some(v.to_string()).filter(|v| !v.is_empty()));
                }
            }
        }
        drop(doc);
        cur = parent_element(d, e);
    }
    Ok(None)
}

/// "Locate a namespace prefix" for `ns`, from `el` up.
fn locate_prefix(d: &Dom, it: &mut Interp<'_>, el: NodeId, ns: &str) -> R<Option<String>> {
    let mut cur = Some(el);
    while let Some(e) = cur {
        it.charge(1)?;
        let doc = d.doc.borrow();
        if let (true, Some(p)) = (doc.element_ns(e).uri() == ns, doc.element_prefix(e)) {
            return Ok(Some(p.to_string()));
        }
        if let NodeData::Element { attrs, .. } = *doc.data(e) {
            for x in doc.attrs(attrs) {
                if doc.attr_ns(x.name) == Some(Ns::Xmlns) && doc.attr_prefix(x.name) == Some("xmlns") && doc.text(x.value) == ns {
                    return Ok(Some(doc.attr_local(x.name).to_string()));
                }
            }
        }
        drop(doc);
        cur = parent_element(d, e);
    }
    Ok(None)
}

fn opt_js(d: &Dom, it: &mut Interp<'_>, s: Option<String>) -> V {
    match s {
        Some(s) => d.js_str(it, &s),
        None => Ok(Value::Null),
    }
}

/// `Node.prototype.lookupNamespaceURI(prefix)`.
pub(crate) fn lookup_namespace_uri(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let this = d.this_ep(it, this)?;
    let prefix = d.opt_string(it, a, "lookupNamespaceURI")?;
    let found = match start(d, this) {
        Some(el) => locate_namespace(d, it, el, prefix.as_deref())?,
        None => None,
    };
    opt_js(d, it, found)
}

/// `Node.prototype.lookupPrefix(namespace)`.
pub(crate) fn lookup_prefix(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let this = d.this_ep(it, this)?;
    let ns = d.opt_string(it, a, "lookupPrefix")?;
    let found = match (ns, start(d, this)) {
        (Some(ns), Some(el)) => locate_prefix(d, it, el, &ns)?,
        _ => None,
    };
    opt_js(d, it, found)
}

/// `Node.prototype.isDefaultNamespace(namespace)`.
pub(crate) fn is_default_namespace(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let this = d.this_ep(it, this)?;
    let ns = d.opt_string(it, a, "isDefaultNamespace")?;
    let default = match start(d, this) {
        Some(el) => locate_namespace(d, it, el, None)?,
        None => None,
    };
    Ok(Value::Bool(default == ns))
}
