//! Node, Element, Document and `<input>` methods of the browser host. See `browser.rs` for the design and the caps.
//!
//! Rules every function here follows:
//! - Read JS arguments first (`string_arg` may run `toString`), then borrow the document. No JS runs under a borrow.
//! - Every walk charges `it.charge` for each node it visits, and every result is capped before it grows.

use crate::browser::*;
use bare_css::matching::matches;
use bare_css::selector::{Selector, Simple};
use bare_html::dom::{Atom, Document, NodeData, NodeId};
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value};

type V = R<Value>;

pub(crate) fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

/// The node after `cur` in tree order, inside the subtree of `root` (`root` itself is never given).
/// `depth` is the depth of `cur` below `root`; the result has the depth of the node it gives.
pub(crate) fn next_in(doc: &Document, root: NodeId, cur: NodeId, depth: usize) -> Option<(NodeId, usize)> {
    if let Some(c) = doc.children(cur).next() {
        return Some((c, depth + 1));
    }
    let (mut n, mut d) = (cur, depth);
    loop {
        if n == root {
            return None;
        }
        if let Some(s) = doc.next_sibling(n) {
            return Some((s, d));
        }
        n = doc.parent(n)?;
        d -= 1;
    }
}

/// Like `next_in`, but a host's shadow tree is walked too: its root comes right after the host (one level down), then the light children of the host.
/// This is for bounds only (the depth of a subtree): a query or `textContent` must use `next_in`, which never enters a shadow tree.
fn next_composed(doc: &Document, root: NodeId, cur: NodeId, depth: usize) -> Option<(NodeId, usize)> {
    if let Some(r) = doc.shadow_root(cur) {
        return Some((r, depth + 1));
    }
    if let Some(c) = doc.children(cur).next() {
        return Some((c, depth + 1));
    }
    let (mut n, mut d) = (cur, depth);
    loop {
        if n == root {
            return None;
        }
        if let Some(s) = doc.next_sibling(n) {
            return Some((s, d));
        }
        d -= 1;
        match doc.parent(n) {
            Some(p) => n = p,
            None => {
                // `n` is a shadow root that is done: go on with the light children of its host.
                let host = doc.shadow_host(n)?;
                if let Some(c) = doc.children(host).next() {
                    return Some((c, d + 1));
                }
                n = host;
            }
        }
    }
}

pub(crate) fn backtracking(s: &Selector) -> u32 {
    let n = s.compounds.len();
    s.compounds
        .iter()
        .take(n.saturating_sub(1))
        .filter(|c| matches!(c.combinator, bare_css::selector::Combinator::Descendant | bare_css::selector::Combinator::Later))
        .count() as u32
}

/// Conservative bound for each combinator search, including skipped non-element siblings.
fn match_work(doc: &Document, depth: usize, sels: &[Selector]) -> u64 {
    sels.iter().fold(0u64, |total, s| {
        let mut cost = 1u64;
        for c in s.compounds.iter().take(s.compounds.len().saturating_sub(1)) {
            let factor = match c.combinator {
                bare_css::selector::Combinator::Later | bare_css::selector::Combinator::Next => doc.len() as u64 + 1,
                bare_css::selector::Combinator::Descendant => depth as u64 + 1,
                _ => 1,
            };
            cost = cost.saturating_mul(factor);
        }
        total.saturating_add(cost)
    })
}

fn needs_places(s: &Selector) -> bool {
    fn simple(x: &Simple) -> bool {
        match x {
            Simple::Nth(_) => true,
            Simple::Not(v) | Simple::Is(v) => v.iter().flatten().any(simple),
            _ => false,
        }
    }
    s.compounds.iter().flat_map(|c| &c.simples).any(simple)
}

/// Lowercase ASCII, like `createElement` and `setAttribute` do in an HTML document.
fn lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// A name for an element or an attribute: not empty, no space, `/`, `>`, `=`, quotes, `<` or NUL, and not starting
/// with a digit, `-` or `.`.
pub(crate) fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '.')
        && !s.contains(|c: char| c.is_ascii_whitespace() || matches!(c, '/' | '>' | '=' | '"' | '\'' | '<' | '\0'))
}

impl Dom {
    pub(crate) fn el(&self, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
        let n = self.nd(it, this)?;
        if self.is_element(n) { Ok(n) } else { Err(it.type_error("Illegal invocation")) }
    }

    /// Room for `nodes` more nodes and `text` more bytes of text in the native arena.
    pub(crate) fn room(&self, doc: &Document, nodes: usize, text: usize) -> R<()> {
        if doc.len() + nodes > self.lim.max_nodes {
            return Err(self.lim("dom-nodes"));
        }
        if doc.text_bytes().saturating_add(text) > self.lim.max_text_bytes {
            return Err(self.lim("dom-text"));
        }
        Ok(())
    }

    /// Room for a new attribute (name, slot, value).
    pub(crate) fn attr_room(&self, doc: &Document, el: NodeId, name: &str, value: &str) -> R<()> {
        self.room(doc, 0, value.len())?;
        if doc.attr_slots() + doc.attr_count(el) + 1 > self.lim.max_attr_slots {
            return Err(self.lim("dom-attrs"));
        }
        if doc.find_atom(name).is_none() && doc.name_count() + 1 > self.lim.max_names {
            return Err(self.lim("dom-names"));
        }
        Ok(())
    }

    /// The number of steps up to the top of the shadow-including tree: a step is a parent link, or a root to its host.
    pub(crate) fn depth_of(&self, it: &mut Interp<'_>, n: NodeId) -> R<usize> {
        let doc = self.doc.borrow();
        let (mut d, mut cur) = (0, n);
        while let Some(p) = doc.parent_or_host(cur) {
            it.charge(1)?;
            d += 1;
            cur = p;
        }
        Ok(d)
    }

    /// How deep the subtree of `n` goes, counting the shadow trees of its hosts (a root is one level below its host).
    fn height_of(&self, it: &mut Interp<'_>, n: NodeId) -> R<usize> {
        let doc = self.doc.borrow();
        let (mut cur, mut d, mut max) = (n, 0, 0);
        while let Some((c, cd)) = next_composed(&doc, n, cur, d) {
            it.charge(1)?;
            (cur, d) = (c, cd);
            max = max.max(d);
        }
        Ok(max)
    }

    pub(crate) fn kids(&self, it: &mut Interp<'_>, n: NodeId, only_elements: bool) -> R<Vec<NodeId>> {
        let doc = self.doc.borrow();
        let mut v = Vec::new();
        for c in doc.children(n) {
            it.charge(1)?;
            if only_elements && !matches!(doc.data(c), NodeData::Element { .. }) {
                continue;
            }
            if v.len() >= self.lim.max_results {
                return Err(self.lim("dom-results"));
            }
            v.push(c);
        }
        Ok(v)
    }

    fn first_el(&self, it: &mut Interp<'_>, n: NodeId, last: bool) -> R<Option<NodeId>> {
        let doc = self.doc.borrow();
        let mut cur = if last { doc.last_child(n) } else { doc.children(n).next() };
        while let Some(c) = cur {
            it.charge(1)?;
            if matches!(doc.data(c), NodeData::Element { .. }) {
                return Ok(Some(c));
            }
            cur = if last { doc.prev_sibling(c) } else { doc.next_sibling(c) };
        }
        Ok(None)
    }

    fn sibling_el(&self, it: &mut Interp<'_>, n: NodeId, prev: bool) -> R<Option<NodeId>> {
        let doc = self.doc.borrow();
        let mut cur = if prev { doc.prev_sibling(n) } else { doc.next_sibling(n) };
        while let Some(c) = cur {
            it.charge(1)?;
            if matches!(doc.data(c), NodeData::Element { .. }) {
                return Ok(Some(c));
            }
            cur = if prev { doc.prev_sibling(c) } else { doc.next_sibling(c) };
        }
        Ok(None)
    }

    /// The elements under `scope` (not `scope`) that match, in tree order. `all` false stops at the first.
    pub(crate) fn query(&self, it: &mut Interp<'_>, scope: NodeId, text: &str, all: bool) -> R<Vec<NodeId>> {
        let sels = self.selectors(it, text)?;
        let base = self.depth_of(it, scope)?;
        let doc = self.doc.borrow();
        if sels.iter().any(needs_places) {
            // The first `:nth-child` after a change builds a table of the whole document.
            it.charge(doc.len() as u64)?;
        }
        let mut out = Vec::new();
        let (mut cur, mut d) = (scope, 0usize);
        while let Some((c, cd)) = next_in(&doc, scope, cur, d) {
            (cur, d) = (c, cd);
            it.charge(match_work(&doc, base + d, &sels))?;
            if matches!(doc.data(c), NodeData::Element { .. }) && sels.iter().any(|s| matches(&doc, c, s)) {
                if out.len() >= self.lim.max_results {
                    return Err(self.lim("dom-results"));
                }
                out.push(c);
                if !all {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Check a node insertion (DOM Standard "ensure pre-insertion validity", the parts that apply to this tree). True: `node` is a plain fragment (`is_plain_fragment`).
    fn check_insert(&self, it: &mut Interp<'_>, parent: NodeId, node: NodeId, before: Option<NodeId>, replacing: Option<NodeId>) -> R<bool> {
        #[derive(PartialEq, Clone, Copy)]
        enum K {
            Doc,
            El,
            Text,
            Comment,
            Doctype,
            Frag,
        }
        let kind = |n: NodeId| match self.data(n) {
            NodeData::Document => K::Doc,
            NodeData::ShadowRoot => K::Frag,
            NodeData::Element { .. } => K::El,
            NodeData::Text(_) => K::Text,
            NodeData::Comment(_) => K::Comment,
            NodeData::Doctype(_) => K::Doctype,
        };
        let (pk, nk) = (kind(parent), kind(node));
        if !matches!(pk, K::Doc | K::El | K::Frag) {
            return Err(self.dom_err(it, "HierarchyRequestError", "This node type does not support this method."));
        }
        if nk == K::Doc {
            return Err(self.dom_err(it, "HierarchyRequestError", "A Document cannot be inserted."));
        }
        {
            // `node` must not be `parent` or one of its host-including ancestors (a root leads to its host).
            let doc = self.doc.borrow();
            let mut cur = Some(parent);
            while let Some(c) = cur {
                it.charge(1)?;
                if c == node {
                    drop(doc);
                    return Err(self.dom_err(it, "HierarchyRequestError", "The new child element contains the parent."));
                }
                cur = doc.parent_or_host(c);
            }
        }
        if let Some(b) = before
            && self.doc.borrow().parent(b) != Some(parent)
        {
            return Err(self.dom_err(it, "NotFoundError", "The node before which the new node is to be inserted is not a child of this node."));
        }
        if (nk == K::Text && pk == K::Doc) || (nk == K::Doctype && pk != K::Doc) {
            return Err(self.dom_err(it, "HierarchyRequestError", "This node type cannot be a child of this node."));
        }
        if pk == K::Doc && matches!(nk, K::El | K::Doctype) {
            let mut bad = false;
            {
                let doc = self.doc.borrow();
                let mut at_or_after = false;
                for c in doc.children(parent) {
                    it.charge(1)?;
                    at_or_after |= Some(c) == before;
                    if c == node || Some(c) == replacing {
                        continue;
                    }
                    let ck = kind(c);
                    bad |= match nk {
                        K::El => ck == K::El || (ck == K::Doctype && at_or_after),
                        _ => ck == K::Doctype || (ck == K::El && !at_or_after),
                    };
                }
            }
            if bad {
                return Err(self.dom_err(it, "HierarchyRequestError", "This document cannot take that child here."));
            }
        }
        let frag = nk == K::Frag && self.is_plain_fragment(node);
        if nk == K::Frag && (pk == K::Doc || !frag) {
            // The standard inserts the children of a fragment. Done by `place` for a fragment with no host into an element or a fragment. Else refuse, before anything changes.
            return Err(Error::Unsupported(unsupported("inserting a ShadowRoot or DocumentFragment as a node (fragment insertion)")));
        }
        let height = if self.doc.borrow().children(node).next().is_some() || self.doc.borrow().shadow_root(node).is_some() { self.height_of(it, node)? } else { 0 };
        if self.depth_of(it, parent)? + 1 + height > self.lim.max_tree_depth {
            return Err(self.lim("dom-depth"));
        }
        Ok(frag)
    }

    /// `append`, `prepend`, `before`, `after`, `replaceWith` and `replaceChild` with more than one node: the standard makes one fragment of the nodes and
    /// inserts it, so the changes are one group (one `MutationObserver` record) and the inserted scripts run after all nodes are in. `old` leaves in the same
    /// group. One plain node is `insert_checked`.
    fn insert_many(&self, it: &mut Interp<'_>, parent: NodeId, nodes: &[NodeId], before: Option<NodeId>, old: Option<NodeId>) -> R<()> {
        if let ([n], None) = (nodes, old) {
            return self.insert_checked(it, parent, *n, before);
        }
        self.place(it, parent, nodes, before, old, false)
    }

    /// Move `nodes` in, before `before`, as one change group; a plain fragment gives its children and stays empty. One node is checked first (`checked`: by
    /// the caller), so a refusal changes nothing. Several are checked as they go in: each one changes what the next may do.
    fn place(&self, it: &mut Interp<'_>, parent: NodeId, nodes: &[NodeId], before: Option<NodeId>, old: Option<NodeId>, checked: bool) -> R<()> {
        let one = nodes.len() == 1;
        if one && !checked {
            self.check_insert(it, parent, nodes[0], old.or(before), old)?;
        }
        let mut moved = Vec::new();
        self.mo_group(None, || {
            if let Some(o) = old {
                self.doc.borrow_mut().detach(o);
            }
            for &n in nodes {
                let frag = if one { self.is_plain_fragment(n) } else { self.check_insert(it, parent, n, before, None)? };
                let kids: Vec<NodeId>;
                let items = if frag {
                    kids = self.doc.borrow().children(n).collect();
                    it.charge(kids.len() as u64)?;
                    &kids[..]
                } else {
                    std::slice::from_ref(&n)
                };
                for &k in items {
                    let mut doc = self.doc.borrow_mut();
                    let b = if before == Some(k) { doc.next_sibling(k) } else { before };
                    doc.move_to(k, parent, b);
                    moved.push(k);
                }
            }
            Ok(())
        })?;
        for k in moved {
            self.script_inserted(it, k)?;
        }
        Ok(())
    }

    /// A fragment that is not a shadow root: `DocumentFragment`, also the contents of a `template`.
    pub(crate) fn is_plain_fragment(&self, n: NodeId) -> bool {
        let doc = self.doc.borrow();
        matches!(doc.data(n), NodeData::ShadowRoot) && doc.shadow_host(n).is_none()
    }

    pub(crate) fn insert_checked(&self, it: &mut Interp<'_>, parent: NodeId, node: NodeId, before: Option<NodeId>) -> R<()> {
        if self.check_insert(it, parent, node, before, None)? {
            return self.place(it, parent, &[node], before, None, true);
        }
        let mut doc = self.doc.borrow_mut();
        let before = if before == Some(node) { doc.next_sibling(node) } else { before };
        doc.move_to(node, parent, before);
        drop(doc);
        self.script_inserted(it, node)
    }

    fn opt_node(&self, it: &mut Interp<'_>, a: &[Value], i: usize) -> R<Option<NodeId>> {
        match arg(a, i) {
            Value::Null | Value::Undefined => Ok(None),
            _ => self.arg_node(it, a, i).map(Some),
        }
    }

    /// Nodes or strings (as for `append`) to nodes. A string becomes a new text node.
    fn nodes_from(&self, it: &mut Interp<'_>, a: &[Value]) -> R<Vec<NodeId>> {
        let mut out = Vec::new();
        for &v in a {
            it.charge(1)?;
            if let Some(n) = self.node_of(v) {
                out.push(n);
            } else {
                let s = self.string_arg(it, v)?;
                let mut doc = self.doc.borrow_mut();
                self.room(&doc, 1, s.len())?;
                out.push(doc.create_text(&s));
            }
        }
        Ok(out)
    }

    /// The value of an `<input>` as JS reads it.
    pub(crate) fn input_value_of(&self, n: NodeId) -> Option<String> {
        self.with_input_value(n, str::to_string)
    }

    /// Run `f` on the value of an `<input>` as JS reads it, borrowed (no copy). `f` must not touch the DOM or its state.
    pub(crate) fn with_input_value<T>(&self, n: NodeId, f: impl FnOnce(&str) -> T) -> Option<T> {
        let doc = self.doc.borrow();
        let area = crate::browser_selection::text_control(&doc, n)?;
        let st = self.st.borrow();
        if let Some(v) = st.inputs.get(&n).and_then(|i| i.value.as_deref()) {
            return Some(f(v));
        }
        if area {
            return Some(f(&crate::browser_textarea::api_value(&crate::browser_textarea::default_text(&doc, n))));
        }
        let kind = input_kind(&doc, n);
        let Some(attr) = doc.find_atom("value").and_then(|a| doc.attr(n, a)) else { return Some(f(if matches!(kind, "checkbox" | "radio") { "on" } else { "" })) };
        let multiple = kind == "email" && doc.find_atom("multiple").is_some_and(|a| doc.attr(n, a).is_some());
        Some(f(&crate::browser_controls::sanitize(kind, multiple, attr)))
    }

    /// Set the dirty value. A value that differs puts the caret at the end (HTML: setting `value`), and when that
    /// moves the selection of a text control, schedules its `selectionchange`. `it` is the JS caller that pays for
    /// the scans; the test driver has none (see `Dom::selection_after_value`).
    pub(crate) fn put_input_value(&self, it: Option<&mut Interp<'_>>, n: NodeId, v: String) -> R<()> {
        let area = self.doc.borrow().is_html(n, "textarea");
        let changed = self.with_input_value(n, |old| old != v) != Some(false);
        let moved = if changed { self.selection_after_value(it, n, &v)? } else { false };
        let old = self.st.borrow().inputs.get(&n).and_then(|i| i.value.as_ref()).map_or(0, String::len);
        if self.st.borrow().input_bytes - old + v.len() > self.lim.max_text_bytes {
            return Err(self.lim("dom-input-bytes"));
        }
        self.selection_room(n, moved, false)?; // every cap is checked before the first write
        self.queue_selection_tasks(n, moved, false)?;
        {
            let mut st = self.st.borrow_mut();
            st.input_bytes = st.input_bytes - old + v.len();
            let i = st.inputs.entry(n).or_default();
            // A changed value puts the caret at the end (HTML: setting `value`). An input keeps that as `None`; a textarea stores it.
            let end = if changed && area { Some(crate::browser_selection::caret_end(&v, true)) } else { None };
            i.value = Some(v);
            if changed {
                i.sel = end;
            }
        }
        Ok(())
    }

    /// The test driver sets native input state (no event). `text` for text-like inputs, `checked` for checkbox and radio.
    pub(crate) fn driver_input(&self, n: NodeId, text: Option<&str>, checked: Option<bool>) -> R<()> {
        let kind = {
            let doc = self.doc.borrow();
            match *doc.data(n) {
                NodeData::Element { name, .. } if doc.atom_str(name) == "input" && !doc.is_foreign(n) => input_kind(&doc, n),
                _ => return Err(Error::Unsupported(unsupported("test driver: not an <input> element"))),
            }
        };
        let is_check = matches!(kind, "checkbox" | "radio");
        if let Some(t) = text {
            if is_check || matches!(kind, "file" | "button" | "submit" | "reset" | "image") {
                return Err(Error::Unsupported(unsupported("test driver: type_text needs a text-like <input>")));
            }
            if t.len() > self.lim.max_string_bytes {
                return Err(self.lim("dom-string"));
            }
            self.put_input_value(None, n, t.to_string())?;
        }
        if let Some(c) = checked {
            if !is_check {
                return Err(Error::Unsupported(unsupported("test driver: set_checked needs a checkbox or radio")));
            }
            self.st.borrow_mut().inputs.entry(n).or_default().checked = Some(c);
        }
        Ok(())
    }
}

pub(crate) fn input_kind(doc: &Document, n: NodeId) -> &'static str {
    let t = doc.find_atom("type").and_then(|a| doc.attr(n, a)).unwrap_or("");
    const KINDS: [&str; 22] = [
        "text", "search", "tel", "url", "email", "password", "date", "month", "week", "time", "datetime-local", "number", "range", "color", "checkbox", "radio", "file", "submit", "image", "reset",
        "button", "hidden",
    ];
    if t.is_empty() {
        return "text";
    }
    KINDS.iter().find(|k| k.len() == t.len() && k.eq_ignore_ascii_case(t)).copied().unwrap_or("text")
}

// ------------------------------------------------------------------------------------------ Node

pub(crate) fn node_type(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.nd(it, this)?;
    Ok(Value::Number(match d.data(n) {
        NodeData::Element { .. } => 1.0,
        NodeData::Text(_) => 3.0,
        NodeData::Comment(_) => 8.0,
        NodeData::Document => 9.0,
        NodeData::Doctype(_) => 10.0,
        NodeData::ShadowRoot => 11.0,
    }))
}

pub(crate) fn node_name(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.nd(it, this)?;
    let s = {
        let doc = d.doc.borrow();
        match *doc.data(n) {
            NodeData::Element { .. } => crate::browser_namespace::tag(&doc, n),
            NodeData::Text(_) => "#text".into(),
            NodeData::Comment(_) => "#comment".into(),
            NodeData::Document => "#document".into(),
            NodeData::ShadowRoot => "#document-fragment".into(),
            NodeData::Doctype(sp) => doc.text(sp).to_string(),
        }
    };
    d.js_str(it, &s)
}

pub(crate) fn node_value(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.nd(it, this)?;
    if matches!(d.data(n), NodeData::Text(_) | NodeData::Comment(_)) { node_value_text(d, it, this, a) } else { Ok(Value::Null) }
}

pub(crate) fn set_node_value(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.nd(it, this)?;
    if matches!(d.data(n), NodeData::Text(_) | NodeData::Comment(_)) { set_node_value_text(d, it, this, a) } else { Ok(Value::Undefined) }
}

/// `data` of a text node or comment.
pub(crate) fn node_value_text(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.nd(it, this)?;
    let s = {
        let doc = d.doc.borrow();
        match *doc.data(n) {
            NodeData::Text(sp) | NodeData::Comment(sp) => doc.text(sp).to_string(),
            _ => return Err(it.type_error("Illegal invocation")),
        }
    };
    d.js_str(it, &s)
}

pub(crate) fn set_node_value_text(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.nd(it, this)?;
    if !matches!(d.data(n), NodeData::Text(_) | NodeData::Comment(_)) {
        return Err(it.type_error("Illegal invocation"));
    }
    let v = arg(a, 0);
    let s = if matches!(v, Value::Null) { String::new() } else { d.string_arg(it, v)? };
    let mut doc = d.doc.borrow_mut();
    d.room(&doc, 0, s.len())?;
    d.range_set_data(&doc, n, &s);
    doc.set_text(n, &s);
    Ok(Value::Undefined)
}

pub(crate) fn data_length(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.nd(it, this)?;
    let doc = d.doc.borrow();
    match *doc.data(n) {
        NodeData::Text(sp) | NodeData::Comment(sp) => {
            it.charge((doc.text(sp).len() / 32) as u64)?;
            Ok(Value::Number(doc.text(sp).encode_utf16().count() as f64))
        }
        _ => Err(it.type_error("Illegal invocation")),
    }
}

fn rel(d: &Dom, it: &mut Interp<'_>, this: Value, f: impl FnOnce(&Document, NodeId) -> Option<NodeId>) -> V {
    let n = d.nd(it, this)?;
    let r = f(&d.doc.borrow(), n);
    d.wrap_opt(it, r)
}

pub(crate) fn parent_node(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    rel(d, it, this, |doc, n| doc.parent(n))
}

pub(crate) fn parent_element(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    rel(d, it, this, |doc, n| doc.parent(n).filter(|&p| matches!(doc.data(p), NodeData::Element { .. })))
}

pub(crate) fn first_child(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    rel(d, it, this, |doc, n| doc.children(n).next())
}

pub(crate) fn last_child(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    rel(d, it, this, |doc, n| doc.last_child(n))
}

pub(crate) fn previous_sibling(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    rel(d, it, this, |doc, n| doc.prev_sibling(n))
}

pub(crate) fn next_sibling(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    rel(d, it, this, |doc, n| doc.next_sibling(n))
}

pub(crate) fn owner_document(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.nd(it, this)?;
    if matches!(d.data(n), NodeData::Document) {
        return Ok(Value::Null);
    }
    let root = d.doc.borrow().root();
    d.wrap(it, root)
}

pub(crate) fn is_connected(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.nd(it, this)?;
    let doc = d.doc.borrow();
    let mut cur = n;
    // Shadow-including: a node of a shadow tree is connected when its host is.
    while let Some(p) = doc.parent_or_host(cur) {
        it.charge(1)?;
        cur = p;
    }
    Ok(Value::Bool(cur == doc.root()))
}

/// `Node.getRootNode(options)`: the topmost ancestor, or the node itself when it has no parent. A shadow root is the root of its own tree; with
/// `composed` true the walk goes on from a shadow root to its host. The options are read before the walk (the getter runs once), so a
/// getter that moves the node is seen by the walk. More than `max_tree_depth` steps is a limit, not a wrong root.
pub(crate) fn get_root_node(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let attr = matches!(this, Value::Object(o) if d.attr.s.borrow().attrs.contains_key(&o));
    let n = if attr { None } else { Some(d.nd(it, this)?) };
    let mut composed = false;
    match arg(a, 0) {
        // WebIDL dictionary: undefined and null are an empty dictionary; any other primitive is a TypeError.
        Value::Undefined | Value::Null => {}
        opts @ Value::Object(_) => {
            let k = d.key(it, "composed")?;
            let v = it.get_value(opts, k)?;
            composed = it.to_js_bool(v)?;
        }
        _ => return Err(it.type_error("Failed to execute 'getRootNode' on 'Node': parameter 1 is not of type 'GetRootNodeOptions'.")),
    }
    // Attr nodes have no parent, including when attached to an element.
    let Some(n) = n else { return Ok(this) };
    let root = {
        let doc = d.doc.borrow();
        let (mut cur, mut depth) = (n, 0);
        while let Some(p) = if composed { doc.parent_or_host(cur) } else { doc.parent(cur) } {
            it.charge(1)?;
            depth += 1;
            if depth > d.lim.max_tree_depth {
                return Err(d.lim("dom-depth"));
            }
            cur = p;
        }
        cur
    };
    d.wrap(it, root)
}

pub(crate) fn child_nodes(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.nd(it, this)?;
    let kids = d.kids(it, n, false)?;
    wrap_all(d, it, &kids)
}

pub(crate) fn wrap_all(d: &Dom, it: &mut Interp<'_>, ids: &[NodeId]) -> V {
    let mut vals = Vec::with_capacity(ids.len());
    for &k in ids {
        vals.push(d.wrap(it, k)?);
    }
    it.array_from(&vals)
}

pub(crate) fn children(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.nd(it, this)?;
    let kids = d.kids(it, n, true)?;
    wrap_all(d, it, &kids)
}

pub(crate) fn first_element_child(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.nd(it, this)?;
    let r = d.first_el(it, n, false)?;
    d.wrap_opt(it, r)
}

pub(crate) fn last_element_child(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.nd(it, this)?;
    let r = d.first_el(it, n, true)?;
    d.wrap_opt(it, r)
}

pub(crate) fn previous_element_sibling(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.el(it, this)?;
    let r = d.sibling_el(it, n, true)?;
    d.wrap_opt(it, r)
}

pub(crate) fn next_element_sibling(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.el(it, this)?;
    let r = d.sibling_el(it, n, false)?;
    d.wrap_opt(it, r)
}

pub(crate) fn child_element_count(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.nd(it, this)?;
    let doc = d.doc.borrow();
    let mut c = 0usize;
    for k in doc.children(n) {
        it.charge(1)?;
        c += matches!(doc.data(k), NodeData::Element { .. }) as usize;
    }
    Ok(Value::Number(c as f64))
}

pub(crate) fn has_child_nodes(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.nd(it, this)?;
    let has = d.doc.borrow().children(n).next().is_some();
    Ok(Value::Bool(has))
}

pub(crate) fn contains(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.nd(it, this)?;
    let Some(other) = d.opt_node(it, a, 0)? else { return Ok(Value::Bool(false)) };
    let doc = d.doc.borrow();
    let mut cur = Some(other);
    while let Some(c) = cur {
        it.charge(1)?;
        if c == n {
            return Ok(Value::Bool(true));
        }
        cur = doc.parent(c);
    }
    Ok(Value::Bool(false))
}

pub(crate) fn text_content(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.nd(it, this)?;
    let s = {
        let doc = d.doc.borrow();
        match *doc.data(n) {
            NodeData::Document | NodeData::Doctype(_) => return Ok(Value::Null),
            NodeData::Text(sp) | NodeData::Comment(sp) => doc.text(sp).to_string(),
            NodeData::Element { .. } | NodeData::ShadowRoot => {
                let mut s = String::new();
                let (mut cur, mut depth) = (n, 0);
                while let Some((c, cd)) = next_in(&doc, n, cur, depth) {
                    it.charge(1)?;
                    (cur, depth) = (c, cd);
                    if let NodeData::Text(sp) = *doc.data(c) {
                        let text = doc.text(sp);
                        if text.len() > d.lim.max_string_bytes.saturating_sub(s.len()) {
                            return Err(d.lim("dom-string"));
                        }
                        s.push_str(text);
                    }
                }
                s
            }
        }
    };
    d.js_str(it, &s)
}

pub(crate) fn set_text_content(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.nd(it, this)?;
    let v = arg(a, 0);
    let s = if matches!(v, Value::Null | Value::Undefined) { String::new() } else { d.string_arg(it, v)? };
    match d.data(n) {
        NodeData::Element { .. } | NodeData::ShadowRoot => {
            let r = d.mo_group(Some(n), || {
                let mut doc = d.doc.borrow_mut();
                d.room(&doc, !s.is_empty() as usize, s.len())?;
                d.iter_told_all(&doc, n);
                while let Some(c) = doc.children(n).next() {
                    it.charge(1)?;
                    doc.detach(c);
                }
                if !s.is_empty() {
                    let t = doc.create_text(&s);
                    doc.move_to(t, n, None);
                }
                Ok(())
            });
            d.iter_told_end();
            r?;
        }
        NodeData::Text(_) | NodeData::Comment(_) => {
            let mut doc = d.doc.borrow_mut();
            d.room(&doc, 0, s.len())?;
            d.range_set_data(&doc, n, &s);
            doc.set_text(n, &s);
        }
        NodeData::Document | NodeData::Doctype(_) => {}
    }
    Ok(Value::Undefined)
}

pub(crate) fn append_child(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let parent = d.nd(it, this)?;
    let node = d.arg_node(it, a, 0)?;
    d.insert_checked(it, parent, node, None)?;
    Ok(arg(a, 0))
}

pub(crate) fn insert_before(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let parent = d.nd(it, this)?;
    let node = d.arg_node(it, a, 0)?;
    let before = d.opt_node(it, a, 1)?;
    d.insert_checked(it, parent, node, before)?;
    Ok(arg(a, 0))
}

pub(crate) fn remove_child(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let parent = d.nd(it, this)?;
    let child = d.arg_node(it, a, 0)?;
    if d.doc.borrow().parent(child) != Some(parent) {
        return Err(d.dom_err(it, "NotFoundError", "The node to be removed is not a child of this node."));
    }
    d.doc.borrow_mut().detach(child);
    Ok(arg(a, 0))
}

pub(crate) fn replace_child(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let parent = d.nd(it, this)?;
    let new = d.arg_node(it, a, 0)?;
    let old = d.arg_node(it, a, 1)?;
    if d.doc.borrow().parent(old) != Some(parent) {
        return Err(d.dom_err(it, "NotFoundError", "The node to be replaced is not a child of this node."));
    }
    if new == old {
        return Ok(arg(a, 1));
    }
    let before = d.doc.borrow().next_sibling(old);
    d.insert_many(it, parent, &[new], before, Some(old))?;
    Ok(arg(a, 1))
}

pub(crate) fn append(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let parent = d.nd(it, this)?;
    let nodes = d.nodes_from(it, a)?;
    d.insert_many(it, parent, &nodes, None, None)?;
    Ok(Value::Undefined)
}

pub(crate) fn prepend(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let parent = d.nd(it, this)?;
    let nodes = d.nodes_from(it, a)?;
    let first = {
        let doc = d.doc.borrow();
        let mut c = doc.children(parent).next();
        while let Some(k) = c.filter(|k| nodes.contains(k)) {
            it.charge(nodes.len() as u64)?;
            c = doc.next_sibling(k);
        }
        c
    };
    d.insert_many(it, parent, &nodes, first, None)?;
    Ok(Value::Undefined)
}

pub(crate) fn remove(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.nd(it, this)?;
    d.doc.borrow_mut().detach(n);
    Ok(Value::Undefined)
}

/// `before`, `after` and `replaceWith` (`how` 0, 1, 2) of ChildNode. A node that is also a sibling in the list does not count as the viable sibling.
fn child_node(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], how: u8) -> V {
    let me = d.nd(it, this)?;
    let nodes = d.nodes_from(it, a)?;
    let Some(parent) = d.doc.borrow().parent(me) else { return Ok(Value::Undefined) };
    let mut old = None;
    let before = {
        let doc = d.doc.borrow();
        let skip = |it: &mut Interp<'_>, mut cur: Option<NodeId>| -> R<Option<NodeId>> {
            while let Some(c) = cur.filter(|c| nodes.contains(c)) {
                it.charge(nodes.len() as u64)?;
                cur = doc.next_sibling(c);
            }
            Ok(cur)
        };
        if how == 0 {
            // The node after the closest earlier sibling that stays, once the list has moved out.
            let mut prev = doc.prev_sibling(me);
            while let Some(c) = prev.filter(|c| nodes.contains(c)) {
                it.charge(nodes.len() as u64)?;
                prev = doc.prev_sibling(c);
            }
            skip(it, prev.map_or_else(|| doc.children(parent).next(), |p| doc.next_sibling(p)))?
        } else {
            if how == 2 && (!nodes.contains(&me) || nodes.len() == 1) {
                old = Some(me);
            }
            skip(it, doc.next_sibling(me))?
        }
    };
    d.insert_many(it, parent, &nodes, before, old)?;
    Ok(Value::Undefined)
}

/// `document.createDocumentFragment()`.
fn create_document_fragment(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    d.nd(it, this)?;
    new_fragment(d, it, None)
}

/// `new DocumentFragment()`.
pub(crate) fn fragment_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    let proto = it.host_proto_from_new_target(d.p().fragment)?;
    new_fragment(d, it, Some(proto))
}

/// An empty fragment of the document. `proto`: the prototype of a subclass.
fn new_fragment(d: &Dom, it: &mut Interp<'_>, proto: Option<ObjId>) -> V {
    crate::browser_elements::tag_fragments(d, it)?;
    let n = {
        let mut doc = d.doc.borrow_mut();
        d.room(&doc, 1, 0)?;
        doc.create_fragment()
    };
    let w = d.wrap(it, n)?;
    if let (Some(p), Value::Object(o)) = (proto.filter(|&p| p != d.p().fragment), w) {
        it.heap.set_prototype_of(o, Some(p)).map_err(|e| it.heap_error(e))?;
    }
    Ok(w)
}

/// `before`, `after`, `replaceWith`, `createDocumentFragment`: installed after every other global, so their function ids do not move a host function that came before.
/// Deviation, for the cost of a realm: `Element` and `CharacterData` share one function object for each ChildNode method (Chrome has two).
pub(crate) fn install_late(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    d.def_method(it, d.p().document, "createDocumentFragment", 0, create_document_fragment)?;
    let hs: [(&str, H); 3] = [("before", |d, it, t, a| child_node(d, it, t, a, 0)), ("after", |d, it, t, a| child_node(d, it, t, a, 1)), ("replaceWith", |d, it, t, a| child_node(d, it, t, a, 2))];
    for (name, h) in hs {
        let f = d.def_method(it, d.p().element, name, 0, h)?;
        let k = d.key(it, name)?;
        it.define_data(d.p().chardata, k, Value::Object(f), true, false, true)?;
    }
    Ok(())
}

pub(crate) fn clone_node(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.nd(it, this)?;
    let deep = it.to_js_bool(arg(a, 0))?;
    if matches!(d.data(n), NodeData::Document) {
        return Err(d.dom_err(it, "NotSupportedError", "Cloning a Document is not supported."));
    }
    // DOM Standard cloneNode(): a shadow root cannot be cloned. (A host is cloned without its root: `clonable` is always false here.)
    if matches!(d.data(n), NodeData::ShadowRoot) && !d.is_plain_fragment(n) {
        return Err(d.dom_err(it, "NotSupportedError", "ShadowRoot nodes are not clonable."));
    }
    // Count first, so a clone that does not fit makes no node at all.
    let (mut count, mut bytes, mut attrs, mut input_bytes) = (1usize, 0usize, 0usize, 0usize);
    {
        let doc = d.doc.borrow();
        let st = d.st.borrow();
        let mut account = |c: NodeId| {
            bytes += match *doc.data(c) {
                NodeData::Text(sp) | NodeData::Comment(sp) | NodeData::Doctype(sp) => doc.text(sp).len(),
                _ => 0,
            };
            attrs += doc.attr_count(c);
            input_bytes += st.inputs.get(&c).and_then(|i| i.value.as_ref()).map_or(0, String::len);
        };
        account(n);
        // The contents of the templates in the subtree are cloned with it.
        let mut contents: Vec<NodeId> = if deep { doc.template_content(n).into_iter().collect() } else { Vec::new() };
        if deep {
            let (mut cur, mut depth) = (n, 0);
            while let Some((c, cd)) = next_in(&doc, n, cur, depth) {
                it.charge(1)?;
                (cur, depth) = (c, cd);
                count += 1;
                account(c);
                contents.extend(doc.template_content(c));
            }
            while let Some(f) = contents.pop() {
                count += 1;
                let (mut cur, mut depth) = (f, 0);
                while let Some((c, cd)) = next_in(&doc, f, cur, depth) {
                    it.charge(1)?;
                    (cur, depth) = (c, cd);
                    count += 1;
                    account(c);
                    contents.extend(doc.template_content(c));
                }
            }
        }
        d.room(&doc, count, bytes)?;
        if attrs > d.lim.max_attr_slots.saturating_sub(doc.attr_slots()) {
            return Err(d.lim("dom-attrs"));
        }
        if input_bytes > d.lim.max_text_bytes.saturating_sub(st.input_bytes) {
            return Err(d.lim("dom-input-bytes"));
        }
    }
    let root_clone = {
        let mut doc = d.doc.borrow_mut();
        let mut root_clone = None;
        let mut stack: Vec<(NodeId, Option<NodeId>)> = vec![(n, None)];
        let mut st = d.st.borrow_mut();
        while let Some((src, new_parent)) = stack.pop() {
            it.charge(1)?;
            let c = match *doc.data(src) {
                NodeData::Element { .. } => doc.clone_element(src),
                NodeData::Text(sp) => {
                    let t = doc.text(sp).to_string();
                    doc.create_text(&t)
                }
                NodeData::Comment(sp) => {
                    let t = doc.text(sp).to_string();
                    doc.create_comment(&t)
                }
                NodeData::Doctype(sp) => {
                    let t = doc.text(sp).to_string();
                    doc.create_doctype(&t)
                }
                NodeData::ShadowRoot => doc.create_fragment(),
                NodeData::Document => unreachable!("checked above, and a root is never a child"),
            };
            d.copy_script_started(src, c);
            if let Some(mut i) = st.inputs.get(&src).cloned() {
                i.sel = None; // the cloning steps copy value and checkedness, not the selection
                i.sc_pending = false; // the clone has no task of its own
                st.input_bytes += i.value.as_ref().map_or(0, String::len);
                st.inputs.insert(c, i);
            }
            match new_parent {
                Some(p) => doc.move_to(c, p, None),
                None => root_clone = Some(c),
            }
            if deep {
                let kids: Vec<NodeId> = doc.children(src).collect();
                stack.extend(kids.into_iter().rev().map(|k| (k, Some(c))));
                if let Some(f) = doc.template_content(src) {
                    let copy = doc.make_template_content(c);
                    let kids: Vec<NodeId> = doc.children(f).collect();
                    stack.extend(kids.into_iter().rev().map(|k| (k, Some(copy))));
                }
            }
        }
        root_clone.expect("one clone")
    };
    d.wrap(it, root_clone)
}

// ------------------------------------------------------------------------------------------ Element

pub(crate) fn tag_name(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.el(it, this)?;
    let s = {
        crate::browser_namespace::tag(&d.doc.borrow(), n)
    };
    d.js_str(it, &s)
}

pub(crate) fn local_name(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.el(it, this)?;
    let s = {
        let doc = d.doc.borrow();
        let NodeData::Element { name, .. } = *doc.data(n) else { unreachable!() };
        doc.atom_str(name).to_string()
    };
    d.js_str(it, &s)
}

pub(crate) fn attr_get(d: &Dom, it: &mut Interp<'_>, n: NodeId, name: &str) -> V {
    let s = {
        let doc = d.doc.borrow();
        doc.find_atom(name).and_then(|a| doc.attr(n, a)).map(str::to_string)
    };
    match s {
        Some(s) => d.js_str(it, &s),
        None => Ok(Value::Null),
    }
}

pub(crate) fn attr_set(d: &Dom, it: &mut Interp<'_>, n: NodeId, name: &str, value: &str) -> R<()> {
    let mut doc = d.doc.borrow_mut();
    d.attr_room(&doc, n, name, value)?;
    it.charge(doc.attr_count(n) as u64 + 1)?;
    doc.set_attr(n, name, value);
    Ok(())
}

fn reflect_get(d: &Dom, it: &mut Interp<'_>, this: Value, name: &str) -> V {
    let n = d.el(it, this)?;
    match attr_get(d, it, n, name)? {
        Value::Null => d.js_str(it, ""),
        v => Ok(v),
    }
}

fn reflect_set(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], name: &str) -> V {
    let n = d.el(it, this)?;
    let s = d.string_arg(it, arg(a, 0))?;
    attr_set(d, it, n, name, &s)?;
    Ok(Value::Undefined)
}

pub(crate) fn id(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    reflect_get(d, it, this, "id")
}

pub(crate) fn set_id(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    reflect_set(d, it, this, a, "id")
}

pub(crate) fn class_name(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    reflect_get(d, it, this, "class")
}

pub(crate) fn set_class_name(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    reflect_set(d, it, this, a, "class")
}

pub(crate) fn get_attribute(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.el(it, this)?;
    let name = d.string_arg(it, arg(a, 0))?;
    let s = {
        let doc = d.doc.borrow();
        let name = crate::browser_namespace::attr_key(&doc, n, &name);
        doc.find_attr_qname(n, &name).and_then(|a| doc.attr(n, a)).map(str::to_string)
    };
    match s {
        Some(s) => d.js_str(it, &s),
        None => Ok(Value::Null),
    }
}

pub(crate) fn has_attribute(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.el(it, this)?;
    let name = d.string_arg(it, arg(a, 0))?;
    let doc = d.doc.borrow();
    Ok(Value::Bool(doc.find_attr_qname(n, &crate::browser_namespace::attr_key(&doc, n, &name)).is_some()))
}

pub(crate) fn set_attribute(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.el(it, this)?;
    let name = d.string_arg(it, arg(a, 0))?;
    let value = d.string_arg(it, arg(a, 1))?;
    let (name, existing) = {
        let doc = d.doc.borrow();
        let name = crate::browser_namespace::attr_key(&doc, n, &name);
        let existing = doc.find_attr_qname(n, &name);
        (name, existing)
    };
    if !valid_name(&name) {
        return Err(d.dom_err(it, "InvalidCharacterError", &format!("'{name}' is not a valid attribute name.")));
    }
    match existing {
        Some(at) => crate::browser_namespace::put(d, it, n, Some(at), None, &name, &value)?,
        None => attr_set(d, it, n, &name, &value)?,
    }
    if name == "src" {
        d.script_src_changed(it, n)?;
    }
    Ok(Value::Undefined)
}

pub(crate) fn remove_attribute(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.el(it, this)?;
    let name = d.string_arg(it, arg(a, 0))?;
    let mut doc = d.doc.borrow_mut();
    it.charge(doc.attr_count(n) as u64 + 1)?;
    let name = crate::browser_namespace::attr_key(&doc, n, &name);
    if let Some(at) = doc.find_attr_qname(n, &name) {
        doc.remove_attr_atom(n, at);
    }
    Ok(Value::Undefined)
}

pub(crate) fn get_attribute_names(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.el(it, this)?;
    let names: Vec<String> = {
        let doc = d.doc.borrow();
        let NodeData::Element { attrs, .. } = *doc.data(n) else { unreachable!() };
        it.charge(attrs_len(&doc, n) as u64)?;
        doc.attrs(attrs).iter().map(|t| doc.atom_str(t.name).to_string()).collect()
    };
    let mut vals = Vec::with_capacity(names.len());
    for s in names {
        vals.push(d.js_str(it, &s)?);
    }
    it.array_from(&vals)
}

fn attrs_len(doc: &Document, n: NodeId) -> usize {
    doc.attr_count(n)
}

pub(crate) fn query_selector(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.nd(it, this)?;
    let text = d.string_arg(it, arg(a, 0))?;
    let r = d.query(it, n, &text, false)?;
    d.wrap_opt(it, r.first().copied())
}

pub(crate) fn query_selector_all(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.nd(it, this)?;
    let text = d.string_arg(it, arg(a, 0))?;
    let r = d.query(it, n, &text, true)?;
    wrap_all(d, it, &r)
}

/// Does the selector match `n`? The cost charged is an upper bound of the matcher's work.
fn match_here(d: &Dom, it: &mut Interp<'_>, n: NodeId, sels: &[Selector]) -> R<bool> {
    let depth = d.depth_of(it, n)?;
    let doc = d.doc.borrow();
    it.charge(match_work(&doc, depth, sels))?;
    if sels.iter().any(needs_places) {
        it.charge(doc.len() as u64)?;
    }
    Ok(sels.iter().any(|s| matches(&doc, n, s)))
}

pub(crate) fn matches_selector(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.el(it, this)?;
    let text = d.string_arg(it, arg(a, 0))?;
    let sels = d.selectors(it, &text)?;
    Ok(Value::Bool(match_here(d, it, n, &sels)?))
}

pub(crate) fn closest(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.el(it, this)?;
    let text = d.string_arg(it, arg(a, 0))?;
    let sels = d.selectors(it, &text)?;
    let mut cur = Some(n);
    while let Some(c) = cur {
        if d.is_element(c) && match_here(d, it, c, &sels)? {
            return d.wrap(it, c);
        }
        cur = d.doc.borrow().parent(c);
    }
    Ok(Value::Null)
}

// ------------------------------------------------------------------------------------------ Document

fn new_name(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<String> {
    let name = d.string_arg(it, v)?;
    if !valid_name(&name) {
        return Err(d.dom_err(it, "InvalidCharacterError", &format!("The tag name provided ('{name}') is not a valid name.")));
    }
    Ok(lower(&name))
}

pub(crate) fn make_element(d: &Dom, it: &mut Interp<'_>, name: &str) -> V {
    let n = {
        let mut doc = d.doc.borrow_mut();
        d.room(&doc, 1, 0)?;
        if doc.find_atom(name).is_none() && doc.name_count() + 1 > d.lim.max_names {
            return Err(d.lim("dom-names"));
        }
        doc.create_element(name)
    };
    d.wrap(it, n)
}

pub(crate) fn create_element(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    d.nd(it, this)?;
    let name = new_name(d, it, arg(a, 0))?;
    make_element(d, it, &name)
}

pub(crate) fn create_text_node(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    d.nd(it, this)?;
    let s = d.string_arg(it, arg(a, 0))?;
    let n = {
        let mut doc = d.doc.borrow_mut();
        d.room(&doc, 1, s.len())?;
        doc.create_text(&s)
    };
    d.wrap(it, n)
}

pub(crate) fn create_comment(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    d.nd(it, this)?;
    let s = d.string_arg(it, arg(a, 0))?;
    let n = {
        let mut doc = d.doc.borrow_mut();
        d.room(&doc, 1, s.len())?;
        doc.create_comment(&s)
    };
    d.wrap(it, n)
}

pub(crate) fn get_element_by_id(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let root = d.nd(it, this)?;
    let want = d.string_arg(it, arg(a, 0))?;
    if want.is_empty() {
        return Ok(Value::Null);
    }
    let found = {
        let doc = d.doc.borrow();
        let (mut cur, mut depth, mut found) = (root, 0, None);
        while let Some((c, cd)) = next_in(&doc, root, cur, depth) {
            it.charge(1)?;
            (cur, depth) = (c, cd);
            if doc.attr(c, Atom::ID) == Some(want.as_str()) {
                found = Some(c);
                break;
            }
        }
        found
    };
    d.wrap_opt(it, found)
}

fn doc_child(d: &Dom, it: &mut Interp<'_>, this: Value, pick: impl Fn(&Document, NodeId) -> bool) -> V {
    let root = d.nd(it, this)?;
    let r = {
        let doc = d.doc.borrow();
        doc.children(root).find(|&c| pick(&doc, c))
    };
    d.wrap_opt(it, r)
}

fn is_named(doc: &Document, n: NodeId, names: &[&str]) -> bool {
    matches!(*doc.data(n), NodeData::Element { name, .. } if names.contains(&doc.atom_str(name))) && !doc.is_foreign(n)
}

pub(crate) fn document_element(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    doc_child(d, it, this, |doc, c| matches!(doc.data(c), NodeData::Element { .. }))
}

fn html_child(d: &Dom, it: &mut Interp<'_>, this: Value, names: &[&str]) -> V {
    let root = d.nd(it, this)?;
    let r = {
        let doc = d.doc.borrow();
        doc.children(root).find(|&c| matches!(doc.data(c), NodeData::Element { .. })).and_then(|html| doc.children(html).find(|&c| is_named(&doc, c, names)))
    };
    d.wrap_opt(it, r)
}

pub(crate) fn head(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    html_child(d, it, this, &["head"])
}

pub(crate) fn body(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    html_child(d, it, this, &["body", "frameset"])
}

pub(crate) fn doctype(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    doc_child(d, it, this, |doc, c| matches!(doc.data(c), NodeData::Doctype(_)))
}

pub(crate) fn default_view(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    d.nd(it, this)?;
    Ok(Value::Object(*d.global.get().expect("installed")))
}

// ------------------------------------------------------------------------------------------ <input>

pub(crate) fn input_el(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
    let n = d.el(it, this)?;
    let is_input = is_named(&d.doc.borrow(), n, &["input"]);
    if is_input { Ok(n) } else { Err(it.type_error("Illegal invocation")) }
}

pub(crate) fn input_value(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = input_el(d, it, this)?;
    let s = d.input_value_of(n).unwrap_or_default();
    d.js_str(it, &s)
}

pub(crate) fn set_input_value(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = input_el(d, it, this)?;
    let v = arg(a, 0);
    let s = if matches!(v, Value::Null) { String::new() } else { d.string_arg(it, v)? };
    let kind = input_kind(&d.doc.borrow(), n);
    match kind {
        // "default/on" mode: the attribute is the value.
        "checkbox" | "radio" | "submit" | "reset" | "button" | "image" | "hidden" => attr_set(d, it, n, "value", &s)?,
        "file" if !s.is_empty() => return Err(d.dom_err(it, "InvalidStateError", "This input element accepts a filename, which may only be programmatically set to the empty string.")),
        "file" => {}
        _ => {
            let multiple = kind == "email" && d.doc.borrow().find_atom("multiple").is_some_and(|a| d.doc.borrow().attr(n, a).is_some());
            let clean = match crate::browser_controls::sanitize(kind, multiple, &s) {
                std::borrow::Cow::Owned(o) => Some(o),
                std::borrow::Cow::Borrowed(b) if b.len() != s.len() => Some(b.to_string()),
                _ => None,
            };
            d.put_input_value(Some(it), n, clean.unwrap_or(s))?
        }
    }
    Ok(Value::Undefined)
}

pub(crate) fn input_checked(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = input_el(d, it, this)?;
    if let Some(c) = d.st.borrow().inputs.get(&n).and_then(|i| i.checked) {
        return Ok(Value::Bool(c));
    }
    let doc = d.doc.borrow();
    Ok(Value::Bool(doc.find_atom("checked").and_then(|a| doc.attr(n, a)).is_some()))
}

pub(crate) fn set_input_checked(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = input_el(d, it, this)?;
    let b = it.to_js_bool(arg(a, 0))?;
    // A radio that gets checked unchecks the others of its group.
    if b && input_kind(&d.doc.borrow(), n) == "radio" {
        for o in d.radio_group(it, n)? {
            d.st.borrow_mut().inputs.entry(o).or_default().checked = Some(false);
        }
    }
    d.st.borrow_mut().inputs.entry(n).or_default().checked = Some(b);
    Ok(Value::Undefined)
}

pub(crate) fn input_type(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = input_el(d, it, this)?;
    let k = input_kind(&d.doc.borrow(), n);
    d.js_str(it, k)
}

pub(crate) fn set_input_type(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    input_el(d, it, this)?;
    reflect_set(d, it, this, a, "type")
}

pub(crate) fn default_value(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    input_el(d, it, this)?;
    reflect_get(d, it, this, "value")
}

pub(crate) fn set_default_value(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = input_el(d, it, this)?;
    let s = d.string_arg(it, arg(a, 0))?;
    d.set_value_attr(it, n, &s)?;
    Ok(Value::Undefined)
}

pub(crate) fn default_checked(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = input_el(d, it, this)?;
    let doc = d.doc.borrow();
    Ok(Value::Bool(doc.find_atom("checked").and_then(|a| doc.attr(n, a)).is_some()))
}

pub(crate) fn set_default_checked(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = input_el(d, it, this)?;
    if it.to_js_bool(arg(a, 0))? {
        attr_set(d, it, n, "checked", "")?;
    } else {
        d.doc.borrow_mut().remove_attr(n, "checked");
    }
    Ok(Value::Undefined)
}

/// An HTML `link` in the HTML namespace. An SVG or MathML `link` is not one (`is_named` skips foreign elements).
pub(crate) fn link_el(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
    let n = d.el(it, this)?;
    if is_named(&d.doc.borrow(), n, &["link"]) { Ok(n) } else { Err(it.type_error("Illegal invocation")) }
}

pub(crate) fn link_rel(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = link_el(d, it, this)?;
    let doc = d.doc.borrow();
    let value = doc.find_atom("rel").and_then(|a| doc.attr(n, a)).unwrap_or("");
    d.js_str(it, value)
}

pub(crate) fn set_link_rel(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    link_el(d, it, this)?;
    reflect_set(d, it, this, a, "rel")
}

/// The `as` values that the Fetch Standard names (its destinations). Any other text reads as the empty string.
pub(crate) const LINK_AS: [&str; 23] = [
    "fetch", "audio", "audioworklet", "document", "embed", "font", "frame", "iframe", "image", "json", "manifest", "object", "paintworklet",
    "report", "script", "serviceworker", "sharedworker", "style", "track", "video", "webidentity", "worker", "xslt",
];

/// A known `as` value is given in lower case. An unknown one is the empty string (an enumerated attribute with no default).
pub(crate) fn link_as(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = link_el(d, it, this)?;
    let doc = d.doc.borrow();
    let raw = doc.find_atom("as").and_then(|a| doc.attr(n, a)).unwrap_or("");
    let known = LINK_AS.iter().find(|k| k.eq_ignore_ascii_case(raw));
    d.js_str(it, known.copied().unwrap_or(""))
}

pub(crate) fn set_link_as(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    link_el(d, it, this)?;
    reflect_set(d, it, this, a, "as")
}

/// `null` when the attribute is absent, `use-credentials` when it says so (any case), else `anonymous`.
pub(crate) fn link_cross_origin(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = link_el(d, it, this)?;
    let doc = d.doc.borrow();
    let Some(raw) = doc.find_atom("crossorigin").and_then(|a| doc.attr(n, a)) else { return Ok(Value::Null) };
    d.js_str(it, if raw.eq_ignore_ascii_case("use-credentials") { "use-credentials" } else { "anonymous" })
}

/// `null` removes the attribute. Any other value is converted to a string and written.
pub(crate) fn set_link_cross_origin(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = link_el(d, it, this)?;
    if matches!(arg(a, 0), Value::Null) {
        d.doc.borrow_mut().remove_attr(n, "crossorigin");
        return Ok(Value::Undefined);
    }
    reflect_set(d, it, this, a, "crossorigin")
}
