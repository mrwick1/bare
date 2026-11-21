//! Node, Element, Document and `<input>` methods of the browser host. See `browser.rs` for the design and the caps.
//!
//! Rules every function here follows:
//! - Read JS arguments first (`string_arg` may run `toString`), then borrow the document. No JS runs under a borrow.
//! - Every walk charges `it.charge` for each node it visits, and every result is capped before it grows.

use crate::eng::{Eng, Fault};
use crate::browser::*;
use bare_css::matching::{matches, subject_matches};
use bare_css::selector::{Combinator, Compound, Selector, Simple};
use bare_html::dom::{Atom, Document, NodeData, NodeId};
use crate::browser_scoped::*;
use bare_js_api::{Args, Local, Type};
pub(crate) use crate::browser_dom_old::*;

/// The call that inserts, for the text of a refusal: the method and the interface that Chrome names ("" for the interface: the kind of the parent).
/// An empty method gives the reason alone.
#[derive(Clone, Copy)]
pub(crate) struct Op(pub &'static str, pub &'static str);

pub(crate) const NO_OP: Op = Op("", "");

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
pub(crate) fn next_composed(doc: &Document, root: NodeId, cur: NodeId, depth: usize) -> Option<(NodeId, usize)> {
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

/// Descendant and later-sibling combinators of `s`: each is a search with a place to go back to. `Dom::selectors` refuses more than 5. The
/// step charge of `match_work` is the real guard: it is made before a match and a chain that is too long ends at the budget.
/// A complex argument of `:not()` or `:is()` (`Simple::Complex`) runs inside the search, so its count adds: the most that one argument adds.
pub(crate) fn backtracking(s: &Selector) -> u32 {
    chain_backtracking(&s.compounds)
}

fn chain_backtracking(compounds: &[Compound]) -> u32 {
    let n = compounds.len();
    let own = compounds.iter().take(n.saturating_sub(1)).filter(|c| matches!(c.combinator, Combinator::Descendant | Combinator::Later)).count() as u32;
    let inner = compounds.iter().flat_map(|c| &c.simples).map(|x| match x {
        Simple::Complex(c) => c.args.iter().map(|a| chain_backtracking(a)).max().unwrap_or(0),
        _ => 0,
    });
    own + inner.max().unwrap_or(0)
}

/// Conservative bound for each combinator search, including skipped non-element siblings.
fn match_work(doc: &Document, depth: usize, sels: &[Selector]) -> u64 {
    sels.iter().fold(0u64, |total, s| total.saturating_add(chain_work(doc, depth, &s.compounds)))
}

/// The bound for one chain of compounds: the product of its searches, times the work of the complex arguments of `:not()` and `:is()`,
/// which run again on each element that the searches visit. (`Simple::Not` and `Simple::Is` have compounds only: no search.)
fn chain_work(doc: &Document, depth: usize, compounds: &[Compound]) -> u64 {
    let mut cost = 1u64;
    for c in compounds.iter().take(compounds.len().saturating_sub(1)) {
        let factor = match c.combinator {
            Combinator::Later | Combinator::Next => doc.len() as u64 + 1,
            Combinator::Descendant => depth as u64 + 1,
            _ => 1,
        };
        cost = cost.saturating_mul(factor);
    }
    let mut inner = 1u64;
    for x in compounds.iter().flat_map(|c| &c.simples) {
        if let Simple::Complex(c) = x {
            for a in &c.args {
                inner = inner.saturating_add(chain_work(doc, depth, a));
            }
        }
    }
    cost.saturating_mul(inner)
}

fn needs_places(s: &Selector) -> bool {
    fn simple(x: &Simple) -> bool {
        match x {
            Simple::Nth(_) => true,
            Simple::Not(v) | Simple::Is(v) => v.iter().flatten().any(simple),
            Simple::Complex(c) => c.args.iter().flatten().flat_map(|c| &c.simples).any(simple),
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

impl<E: Eng> Dom<E> {
    /// Room for `nodes` more nodes and `text` more bytes of text in the native arena.
    pub(crate) fn room(&self, doc: &Document, nodes: usize, text: usize) -> R<E, ()> {
        if doc.live_len() + nodes > self.lim.max_nodes {
            return Err(self.lim("dom-nodes"));
        }
        if doc.live_text_bytes().saturating_add(text) > self.lim.max_text_bytes {
            return Err(self.lim("dom-text"));
        }
        Ok(())
    }

    /// Room for a new attribute (name, slot, value).
    pub(crate) fn attr_room(&self, doc: &Document, el: NodeId, name: &str, value: &str) -> R<E, ()> {
        self.room(doc, 0, value.len())?;
        if doc.live_attr_slots() + doc.attr_count(el) + 1 > self.lim.max_attr_slots {
            return Err(self.lim("dom-attrs"));
        }
        if doc.find_atom(name).is_none() && doc.name_count() + 1 > self.lim.max_names {
            return Err(self.lim("dom-names"));
        }
        Ok(())
    }

    /// The number of steps up to the top of the shadow-including tree: a step is a parent link, or a root to its host.
    pub(crate) fn depth_of_s<'s>(&self, s: &mut Sc<'s, E>, n: NodeId) -> Res<'s, usize> {
        let doc = self.doc.borrow();
        let (mut d, mut cur) = (0, n);
        while let Some(p) = doc.parent_or_host(cur) {
            s.charge(1)?;
            d += 1;
            cur = p;
        }
        Ok(d)
    }

    /// How deep the subtree of `n` goes, counting the shadow trees of its hosts (a root is one level below its host).
    fn height_of_s<'s>(&self, s: &mut Sc<'s, E>, n: NodeId) -> Res<'s, usize> {
        let doc = self.doc.borrow();
        let (mut cur, mut d, mut max) = (n, 0, 0);
        while let Some((c, cd)) = next_composed(&doc, n, cur, d) {
            s.charge(1)?;
            (cur, d) = (c, cd);
            max = max.max(d);
        }
        Ok(max)
    }

    pub(crate) fn kids_s<'s>(&self, s: &mut Sc<'s, E>, n: NodeId, only_elements: bool) -> Res<'s, Vec<NodeId>> {
        let doc = self.doc.borrow();
        let mut v = Vec::new();
        for c in doc.children(n) {
            s.charge(1)?;
            if only_elements && !matches!(doc.data(c), NodeData::Element { .. }) {
                continue;
            }
            if v.len() >= self.lim.max_results {
                return Err(self.lim_s(s, "dom-results"));
            }
            v.push(c);
        }
        Ok(v)
    }

    fn first_el_s<'s>(&self, s: &mut Sc<'s, E>, n: NodeId, last: bool) -> Res<'s, Option<NodeId>> {
        let doc = self.doc.borrow();
        let mut cur = if last { doc.last_child(n) } else { doc.children(n).next() };
        while let Some(c) = cur {
            s.charge(1)?;
            if matches!(doc.data(c), NodeData::Element { .. }) {
                return Ok(Some(c));
            }
            cur = if last { doc.prev_sibling(c) } else { doc.next_sibling(c) };
        }
        Ok(None)
    }

    fn sibling_el_s<'s>(&self, s: &mut Sc<'s, E>, n: NodeId, prev: bool) -> Res<'s, Option<NodeId>> {
        let doc = self.doc.borrow();
        let mut cur = if prev { doc.prev_sibling(n) } else { doc.next_sibling(n) };
        while let Some(c) = cur {
            s.charge(1)?;
            if matches!(doc.data(c), NodeData::Element { .. }) {
                return Ok(Some(c));
            }
            cur = if prev { doc.prev_sibling(c) } else { doc.next_sibling(c) };
        }
        Ok(None)
    }

    /// The elements under `scope` (not `scope`) that match, in tree order. `all` false stops at the first.
    pub(crate) fn query_s<'s>(&self, s: &mut Sc<'s, E>, scope: NodeId, text: &str, all: bool) -> Res<'s, Vec<NodeId>> {
        let sels = self.selectors_at_s(s, text, scope)?;
        let base = self.depth_of_s(s, scope)?;
        let doc = self.doc.borrow();
        if sels.iter().any(needs_places) {
            // The first `:nth-child` after a change builds a table of the whole document.
            s.charge(doc.len() as u64)?;
        }
        let mut out = Vec::new();
        let (mut cur, mut d) = (scope, 0usize);
        while let Some((c, cd)) = next_in(&doc, scope, cur, d) {
            (cur, d) = (c, cd);
            s.charge(1)?;
            // The matcher stops at once when the last compound does not fit: only a selector that fits has to search up or back.
            let mut hit = false;
            for sel in sels.iter() {
                if !subject_matches(&doc, c, sel) {
                    continue;
                }
                s.charge(match_work(&doc, base + d, std::slice::from_ref(sel)))?;
                if matches(&doc, c, sel) {
                    hit = true;
                    break;
                }
            }
            if hit {
                if out.len() >= self.lim.max_results {
                    return Err(self.lim_s(s, "dom-results"));
                }
                out.push(c);
                if !all {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// The error of a refused insert. `op` puts the name of the call in front, as Chrome does: "Failed to execute 'append' on 'Document': ...".
    pub(crate) fn refuse_s<'s>(&self, s: &mut Sc<'s, E>, op: Op, parent: NodeId, name: &str, reason: &str) -> bare_js_api::Error<'s> {
        if op.0.is_empty() {
            return self.dom_err_s(s, name, reason);
        }
        let iface = match (op.1, self.data(parent)) {
            ("", NodeData::Document) => "Document",
            ("", NodeData::ShadowRoot) => "DocumentFragment",
            ("", _) => "Element",
            (i, _) => i,
        };
        self.dom_err_s(s, name, &format!("Failed to execute '{}' on '{iface}': {reason}", op.0))
    }

    /// The Document rules of Chrome (`Document::CanAcceptChild`) for `nodes` going in before `before` (None: last), in place of `old`: one element child, one
    /// doctype, the doctype before the element. A fragment counts as its children. Chrome judges in two steps, and so does this: each item alone against
    /// the children as they are, in order (the first refusal wins, so a text after a bad element is not reported); then, when there are several items,
    /// all of them together, one after the other. With several nodes in the list, the ones that are children now do not count: Chrome has moved them out.
    /// Deviation: Chrome has moved them out (and has emptied a fragment) even when it refuses. Here a refusal changes nothing.
    fn check_doc_s<'s>(&self, s: &mut Sc<'s, E>, parent: NodeId, nodes: &[NodeId], before: Option<NodeId>, old: Option<NodeId>, op: Op) -> Res<'s, ()> {
        let bad: Option<String> = {
            let doc = self.doc.borrow();
            if let ([n], Some(o)) = (nodes, old)
                && std::mem::discriminant(doc.data(*n)) == std::mem::discriminant(doc.data(o))
            {
                return Ok(());
            }
            let mut items = Vec::new();
            for &n in nodes {
                if matches!(doc.data(n), NodeData::ShadowRoot) {
                    if doc.shadow_host(n).is_some() {
                        return Err(unsupp("inserting a ShadowRoot or DocumentFragment as a node (fragment insertion)"));
                    }
                    s.charge(1 + doc.children(n).count() as u64)?;
                    items.extend(doc.children(n));
                } else {
                    items.push(n);
                }
            }
            // The children now: doctypes, elements, and whether one comes after the reference point. `old` is skipped.
            let mut scan = |old: Option<NodeId>| -> Res<'s, (u32, u32, bool, bool)> {
                let (mut docs, mut els, mut doc_after, mut el_after, mut seen) = (0u32, 0u32, false, false, false);
                for c in doc.children(parent) {
                    s.charge(1)?;
                    if Some(c) == old {
                        seen = true;
                        continue;
                    }
                    if nodes.len() > 1 && nodes.contains(&c) {
                        continue;
                    }
                    seen |= Some(c) == before;
                    match doc.data(c) {
                        NodeData::Doctype(_) => (docs, doc_after) = (docs + 1, seen),
                        NodeData::Element { .. } => (els, el_after) = (els + 1, seen),
                        _ => {}
                    }
                }
                Ok((docs, els, doc_after, el_after))
            };
            // The first step of `replaceWith` with several nodes does not know the node it replaces.
            let first = if op.0 == "replaceWith" && nodes.len() > 1 { None } else { old };
            let (docs1, els1, doc_after1, el_after1) = scan(first)?;
            let (docs, mut els, doc_after, el_after) = if first == old { (docs1, els1, doc_after1, el_after1) } else { scan(old)? };
            let mut docs = docs;
            // One item against the counts so far, which it changes.
            let step = |n: NodeId, docs: &mut u32, els: &mut u32, doc_after: bool, el_after: bool| -> Option<&'static str> {
                match doc.data(n) {
                    NodeData::Document => Some("Nodes of type '#document' may not be inserted inside nodes of type '#document'."),
                    NodeData::Text(_) => Some("Nodes of type '#text' may not be inserted inside nodes of type '#document'."),
                    NodeData::Doctype(_) => {
                        *docs += 1;
                        if *els > 0 && !el_after {
                            Some("Can't insert a doctype after the root element.")
                        } else if *docs > 1 {
                            Some("Only one doctype on document allowed.")
                        } else {
                            None
                        }
                    }
                    NodeData::Element { .. } => {
                        *els += 1;
                        if doc_after {
                            Some("Can't insert an element before a doctype.")
                        } else if *els > 1 {
                            Some("Only one element on document allowed.")
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            };
            // `appendChild(fragment)` and its kin judge a fragment as one node: a text in it is refused before any count. The other calls take the items one by one.
            let whole = op.1 == "Node" && matches!(nodes, [n] if matches!(doc.data(*n), NodeData::ShadowRoot));
            let mut bad = None;
            for &n in &items {
                s.charge(1)?;
                let (mut d, mut e) = (docs1, els1);
                if !whole || matches!(doc.data(n), NodeData::Text(_)) {
                    bad = step(n, &mut d, &mut e, doc_after1, el_after1);
                }
                if bad.is_some() {
                    break;
                }
            }
            if bad.is_none() && (items.len() > 1 || whole) {
                for &n in &items {
                    s.charge(1)?;
                    bad = step(n, &mut docs, &mut els, doc_after, el_after);
                    if bad.is_some() {
                        break;
                    }
                }
            }
            bad.map(String::from)
        };
        match bad {
            Some(r) => Err(self.refuse_s(s, op, parent, "HierarchyRequestError", &r)),
            None => Ok(()),
        }
    }

    /// The depth cap for `node` (and what it holds) as a child of `parent`.
    fn check_depth_s<'s>(&self, s: &mut Sc<'s, E>, parent: NodeId, node: NodeId) -> Res<'s, ()> {
        let height = if self.doc.borrow().children(node).next().is_some() || self.doc.borrow().shadow_root(node).is_some() { self.height_of_s(s, node)? } else { 0 };
        if self.depth_of_s(s, parent)? + 1 + height > self.lim.max_tree_depth {
            return Err(self.lim_s(s, "dom-depth"));
        }
        Ok(())
    }

    /// Check a node insertion (DOM Standard "ensure pre-insertion validity", the parts that apply to this tree). True: `node` is a plain fragment (`is_plain_fragment`).
    pub(crate) fn check_insert_s<'s>(&self, s: &mut Sc<'s, E>, parent: NodeId, node: NodeId, before: Option<NodeId>, replacing: Option<NodeId>, op: Op) -> Res<'s, bool> {
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
            return Err(self.refuse_s(s, op, parent, "HierarchyRequestError", "This node type does not support this method."));
        }
        if let Some(b) = before
            && pk == K::Doc
            && self.doc.borrow().parent(b) != Some(parent)
        {
            return Err(self.refuse_s(s, op, parent, "NotFoundError", "The node before which the new node is to be inserted is not a child of this node."));
        }
        if pk == K::Doc {
            self.check_doc_s(s, parent, &[node], before, replacing, op)?;
            self.check_depth_s(s, parent, node)?;
            return Ok(nk == K::Frag);
        }
        {
            // `node` must not be `parent` or one of its host-including ancestors (a root leads to its host).
            let doc = self.doc.borrow();
            let mut cur = Some(parent);
            while let Some(c) = cur {
                s.charge(1)?;
                if c == node {
                    drop(doc);
                    return Err(self.refuse_s(s, op, parent, "HierarchyRequestError", "The new child element contains the parent."));
                }
                cur = doc.parent_or_host(c);
            }
        }
        if nk == K::Doc {
            let name = if pk == K::Frag { "#document-fragment".to_string() } else { crate::browser_namespace::tag(&self.doc.borrow(), parent) };
            return Err(self.refuse_s(s, op, parent, "HierarchyRequestError", &format!("Nodes of type '#document' may not be inserted inside nodes of type '{name}'.")));
        }
        if let Some(b) = before
            && self.doc.borrow().parent(b) != Some(parent)
        {
            return Err(self.refuse_s(s, op, parent, "NotFoundError", "The node before which the new node is to be inserted is not a child of this node."));
        }
        if nk == K::Doctype {
            return Err(self.refuse_s(s, op, parent, "HierarchyRequestError", "This node type cannot be a child of this node."));
        }
        let frag = nk == K::Frag && self.is_plain_fragment(node);
        if nk == K::Frag && !frag {
            // The standard inserts the children of a fragment. Done by `place` for a fragment with no host. Else refuse, before anything changes.
            return Err(unsupp("inserting a ShadowRoot or DocumentFragment as a node (fragment insertion)"));
        }
        self.check_depth_s(s, parent, node)?;
        Ok(frag)
    }

    /// `append`, `prepend`, `before`, `after`, `replaceWith` and `replaceChild` with more than one node: the standard makes one fragment of the nodes and
    /// inserts it, so the changes are one group (one `MutationObserver` record) and the inserted scripts run after all nodes are in. `old` leaves in the same
    /// group. One plain node is `insert_checked`.
    fn insert_many_s<'s>(&self, s: &mut Sc<'s, E>, parent: NodeId, nodes: &[NodeId], before: Option<NodeId>, old: Option<NodeId>, op: Op) -> Res<'s, ()> {
        if let ([n], None) = (nodes, old) {
            return self.insert_checked_s(s, parent, *n, before, op);
        }
        self.place_s(s, parent, nodes, before, old, false, op)
    }

    /// Move `nodes` in, before `before`, as one change group; a plain fragment gives its children and stays empty. One node is checked first (`checked`: by
    /// the caller), so a refusal changes nothing. Several are checked as they go in: each one changes what the next may do. The Document judges all of them
    /// first (`check_doc`), as Chrome does.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn place_s<'s>(&self, s: &mut Sc<'s, E>, parent: NodeId, nodes: &[NodeId], before: Option<NodeId>, old: Option<NodeId>, checked: bool, op: Op) -> Res<'s, ()> {
        let one = nodes.len() == 1;
        let in_doc = matches!(self.data(parent), NodeData::Document);
        if in_doc && !checked {
            self.check_doc_s(s, parent, nodes, before, old, op)?;
            for &n in nodes {
                self.check_depth_s(s, parent, n)?;
            }
        } else if one && !checked {
            self.check_insert_s(s, parent, nodes[0], old.or(before), old, op)?;
        }
        let mut moved = Vec::new();
        self.mo_group(None, || {
            if let Some(o) = old {
                self.doc.borrow_mut().detach(o);
            }
            for &n in nodes {
                let frag = if one || in_doc { self.is_plain_fragment(n) } else { self.check_insert_s(s, parent, n, before, None, op)? };
                let kids: Vec<NodeId>;
                let items = if frag {
                    kids = self.doc.borrow().children(n).collect();
                    s.charge(kids.len() as u64)?;
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
            script_inserted_s(self, s, k)?;
        }
        Ok(())
    }

    /// A fragment that is not a shadow root: `DocumentFragment`, also the contents of a `template`.
    pub(crate) fn is_plain_fragment(&self, n: NodeId) -> bool {
        let doc = self.doc.borrow();
        matches!(doc.data(n), NodeData::ShadowRoot) && doc.shadow_host(n).is_none()
    }

    pub(crate) fn insert_checked_s<'s>(&self, s: &mut Sc<'s, E>, parent: NodeId, node: NodeId, before: Option<NodeId>, op: Op) -> Res<'s, ()> {
        let frag = self.check_insert_s(s, parent, node, before, None, op)?;
        self.insert_valid_s(s, parent, node, before, frag, op)
    }

    /// The insertion of `insert_checked`, for a node that `check_insert` accepted (`frag`: what it gave).
    pub(crate) fn insert_valid_s<'s>(&self, s: &mut Sc<'s, E>, parent: NodeId, node: NodeId, before: Option<NodeId>, frag: bool, op: Op) -> Res<'s, ()> {
        if frag {
            return self.place_s(s, parent, &[node], before, None, true, op);
        }
        let mut doc = self.doc.borrow_mut();
        let before = if before == Some(node) { doc.next_sibling(node) } else { before };
        doc.move_to(node, parent, before);
        drop(doc);
        script_inserted_s(self, s, node)
    }

    fn opt_node_s<'s>(&self, s: &mut Sc<'s, E>, a: Args<'s>, i: usize) -> Res<'s, Option<NodeId>> {
        let v = arg_s::<E>(s, a, i);
        if is_nullish::<E>(s, v) { Ok(None) } else { self.arg_node_s(s, a, i).map(Some) }
    }

    /// Nodes or strings (as for `append`) to nodes. A string becomes a new text node.
    fn nodes_from_s<'s>(&self, s: &mut Sc<'s, E>, a: Args<'s>) -> Res<'s, Vec<NodeId>> {
        let mut out = Vec::new();
        for v in a.iter() {
            s.charge(1)?;
            if let Some(n) = self.node_of_s(s, v) {
                out.push(n);
            } else {
                let text = self.string_arg_s(s, v)?;
                let mut doc = self.doc.borrow_mut();
                self.room_s(s, &doc, 1, text.len())?;
                out.push(doc.create_text(&text));
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

    /// The bytes that all dirty values hold stay under `max_text_bytes` when the value of `n` has `len` bytes.
    pub(crate) fn value_room(&self, n: NodeId, len: usize) -> R<E, ()> {
        let old = self.st.borrow().inputs.get(&n).and_then(|i| i.value.as_ref()).map_or(0, String::len);
        if self.st.borrow().input_bytes - old + len > self.lim.max_text_bytes {
            return Err(self.lim("dom-input-bytes"));
        }
        Ok(())
    }

    /// Store the dirty value of a text control. `reset_caret`: a changed value puts the caret at the end (HTML: setting `value`).
    pub(crate) fn write_value(&self, n: NodeId, v: String, reset_caret: bool) -> R<E, ()> {
        self.value_room(n, v.len())?;
        let mut st = self.st.borrow_mut();
        let old = st.inputs.get(&n).and_then(|i| i.value.as_ref()).map_or(0, String::len);
        st.input_bytes = st.input_bytes - old + v.len();
        let i = st.inputs.entry(n).or_default();
        i.value = Some(v);
        if reset_caret {
            i.sel = Some(crate::browser_selection::END);
        }
        drop(st);
        self.sync_ui(n);
        Ok(())
    }

    /// Give the document the state of the control `n` that selectors read (`Document::ui`).
    pub(crate) fn sync_ui(&self, n: NodeId) {
        use bare_html::dom::{UI_CHECKED, UI_CHECKED_DIRTY, UI_INDETERMINATE, UI_VALUE_DIRTY, UI_VALUE_SET};
        let bits = self.st.borrow().inputs.get(&n).map_or(0, |i| {
            i.checked.map_or(0, |c| UI_CHECKED_DIRTY | if c { UI_CHECKED } else { 0 })
                | i.value.as_ref().map_or(0, |v| UI_VALUE_DIRTY | if v.is_empty() { 0 } else { UI_VALUE_SET })
                | if i.indeterminate { UI_INDETERMINATE } else { 0 }
        });
        self.doc.borrow_mut().set_ui(n, bits);
    }

    /// The test driver sets native input state (no event). `text` for text-like inputs, `checked` for checkbox and radio.
    pub(crate) fn driver_input(&self, n: NodeId, text: Option<&str>, checked: Option<bool>) -> R<E, ()> {
        let kind = {
            let doc = self.doc.borrow();
            match *doc.data(n) {
                NodeData::Element { name, .. } if doc.atom_str(name) == "input" && !doc.is_foreign(n) => input_kind(&doc, n),
                _ => return Err(Fault::Unsupported("test driver: not an <input> element")),
            }
        };
        let is_check = matches!(kind, "checkbox" | "radio");
        if let Some(t) = text {
            if is_check || matches!(kind, "file" | "button" | "submit" | "reset" | "image") {
                return Err(Fault::Unsupported("test driver: type_text needs a text-like <input>"));
            }
            if t.len() > self.lim.max_string_bytes {
                return Err(self.lim("dom-string"));
            }
            self.put_input_value(None, n, t.to_string())?;
        }
        if let Some(c) = checked {
            if !is_check {
                return Err(Fault::Unsupported("test driver: set_checked needs a checkbox or radio"));
            }
            self.st.borrow_mut().inputs.entry(n).or_default().checked = Some(c);
            self.sync_ui(n);
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

pub(crate) fn node_type<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    Ok(s.number(match d.data(n) {
        NodeData::Element { .. } => 1.0,
        NodeData::Text(_) => 3.0,
        NodeData::Comment(_) => 8.0,
        NodeData::Document => 9.0,
        NodeData::Doctype(_) => 10.0,
        NodeData::ShadowRoot => 11.0,
    }))
}

/// `nodeName`.
pub(crate) fn name_of(doc: &Document, n: NodeId) -> String {
    match *doc.data(n) {
        NodeData::Element { .. } => crate::browser_namespace::tag(doc, n),
        NodeData::Text(_) => "#text".into(),
        NodeData::Comment(_) => "#comment".into(),
        NodeData::Document => "#document".into(),
        NodeData::ShadowRoot => "#document-fragment".into(),
        NodeData::Doctype(sp) => doc.text(sp).to_string(),
    }
}

pub(crate) fn node_name<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    let name = name_of(&d.doc.borrow(), n);
    d.js_str_s(s, &name)
}

pub(crate) fn node_value<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    if matches!(d.data(n), NodeData::Text(_) | NodeData::Comment(_)) { node_value_text(d, s, this, a) } else { Ok(s.null()) }
}

pub(crate) fn set_node_value<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    if matches!(d.data(n), NodeData::Text(_) | NodeData::Comment(_)) { set_node_value_text(d, s, this, a) } else { Ok(s.undefined()) }
}

/// `data` of a text node or comment.
pub(crate) fn node_value_text<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    let text = {
        let doc = d.doc.borrow();
        match *doc.data(n) {
            NodeData::Text(sp) | NodeData::Comment(sp) => doc.text(sp).to_string(),
            _ => return Err(s.type_error("Illegal invocation")),
        }
    };
    d.js_str_s(s, &text)
}

pub(crate) fn set_node_value_text<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    if !matches!(d.data(n), NodeData::Text(_) | NodeData::Comment(_)) {
        return Err(s.type_error("Illegal invocation"));
    }
    let v = arg_s::<E>(s, a, 0);
    let text = if s.type_of(v) == Type::Null { String::new() } else { d.string_arg_s(s, v)? };
    let mut doc = d.doc.borrow_mut();
    d.room_s(s, &doc, 0, text.len())?;
    d.range_set_data(&doc, n, &text);
    doc.set_text(n, &text);
    Ok(s.undefined())
}

pub(crate) fn data_length<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    let doc = d.doc.borrow();
    match *doc.data(n) {
        NodeData::Text(sp) | NodeData::Comment(sp) => {
            s.charge((doc.text(sp).len() / 32) as u64)?;
            Ok(s.number(doc.text(sp).encode_utf16().count() as f64))
        }
        _ => Err(s.type_error("Illegal invocation")),
    }
}

fn rel<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, f: impl FnOnce(&Document, NodeId) -> Option<NodeId>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    let r = f(&d.doc.borrow(), n);
    d.wrap_opt_s(s, r)
}

pub(crate) fn parent_node<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    rel(d, s, this, |doc, n| doc.parent(n))
}

pub(crate) fn parent_element<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    rel(d, s, this, |doc, n| doc.parent(n).filter(|&p| matches!(doc.data(p), NodeData::Element { .. })))
}

pub(crate) fn first_child<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    rel(d, s, this, |doc, n| doc.children(n).next())
}

pub(crate) fn last_child<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    rel(d, s, this, |doc, n| doc.last_child(n))
}

pub(crate) fn previous_sibling<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    rel(d, s, this, |doc, n| doc.prev_sibling(n))
}

pub(crate) fn next_sibling<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    rel(d, s, this, |doc, n| doc.next_sibling(n))
}

pub(crate) fn owner_document<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    if matches!(d.data(n), NodeData::Document) {
        return Ok(s.null());
    }
    if let Some(o) = crate::browser_frames::owner_of(d, n) {
        return d.wrap_s(s, o);
    }
    let root = if d.in_inert(n) { crate::browser_custom::template_document(d) } else { d.doc.borrow().root() };
    d.wrap_s(s, root)
}

pub(crate) fn is_connected<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    let doc = d.doc.borrow();
    let mut cur = n;
    let it = s.engine();
    // Shadow-including: a node of a shadow tree is connected when its host is.
    while let Some(p) = doc.parent_or_host(cur) {
        bare_js_api::Engine::charge(it, 1)?;
        cur = p;
    }
    Ok(s.boolean(cur == doc.root()))
}

/// `Node.getRootNode(options)`: the topmost ancestor, or the node itself when it has no parent. A shadow root is the root of its own tree; with
/// `composed` true the walk goes on from a shadow root to its host. The options are read before the walk (the getter runs once), so a
/// getter that moves the node is seen by the walk. More than `max_tree_depth` steps is a limit, not a wrong root.
pub(crate) fn get_root_node<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let attr = is_attr_node_s(d, s, this);
    let n = if attr { None } else { Some(d.nd_s(s, this)?) };
    let mut composed = false;
    let opts = arg_s::<E>(s, a, 0);
    // WebIDL dictionary: undefined and null are an empty dictionary; any other primitive is a TypeError.
    if is_object::<E>(s, opts) {
        let v = s.get_str(opts, "composed")?;
        composed = s.to_boolean(v);
    } else if !is_nullish::<E>(s, opts) {
        return Err(s.type_error("Failed to execute 'getRootNode' on 'Node': The provided value is not of type 'GetRootNodeOptions'."));
    }
    // Attr nodes have no parent, including when attached to an element.
    let Some(n) = n else { return Ok(this) };
    let root = {
        let doc = d.doc.borrow();
        let (mut cur, mut depth) = (n, 0);
        while let Some(p) = if composed { doc.parent_or_host(cur) } else { doc.parent(cur) } {
            s.charge(1)?;
            depth += 1;
            if depth > d.lim.max_tree_depth {
                return Err(d.lim_s(s, "dom-depth"));
            }
            cur = p;
        }
        cur
    };
    d.wrap_s(s, root)
}

pub(crate) fn child_nodes<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    let kids = d.kids_s(s, n, false)?;
    wrap_coll_s(d, s, &kids)
}

pub(crate) fn wrap_all_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, ids: &[NodeId]) -> Res<'s, Local<'s>> {
    let mut vals = Vec::with_capacity(ids.len());
    for &k in ids {
        vals.push(d.wrap_s(s, k)?);
    }
    s.new_array(&vals)
}

pub(crate) fn children<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    let kids = d.kids_s(s, n, true)?;
    wrap_coll_s(d, s, &kids)
}

pub(crate) fn first_element_child<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    let r = d.first_el_s(s, n, false)?;
    d.wrap_opt_s(s, r)
}

pub(crate) fn last_element_child<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    let r = d.first_el_s(s, n, true)?;
    d.wrap_opt_s(s, r)
}

pub(crate) fn previous_element_sibling<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    let r = d.sibling_el_s(s, n, true)?;
    d.wrap_opt_s(s, r)
}

pub(crate) fn next_element_sibling<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    let r = d.sibling_el_s(s, n, false)?;
    d.wrap_opt_s(s, r)
}

pub(crate) fn child_element_count<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    let doc = d.doc.borrow();
    let mut c = 0usize;
    let it = s.engine();
    for k in doc.children(n) {
        bare_js_api::Engine::charge(it, 1)?;
        c += matches!(doc.data(k), NodeData::Element { .. }) as usize;
    }
    Ok(s.number(c as f64))
}

pub(crate) fn has_child_nodes<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    let has = d.doc.borrow().children(n).next().is_some();
    Ok(s.boolean(has))
}

pub(crate) fn text_content_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    let out = {
        let doc = d.doc.borrow();
        match *doc.data(n) {
            NodeData::Document | NodeData::Doctype(_) => return Ok(s.null()),
            NodeData::Text(sp) | NodeData::Comment(sp) => doc.text(sp).to_string(),
            NodeData::Element { .. } | NodeData::ShadowRoot => {
                let mut out = String::new();
                let (mut cur, mut depth) = (n, 0);
                while let Some((c, cd)) = next_in(&doc, n, cur, depth) {
                    s.charge(1)?;
                    (cur, depth) = (c, cd);
                    if let NodeData::Text(sp) = *doc.data(c) {
                        let text = doc.text(sp);
                        if text.len() > d.lim.max_string_bytes.saturating_sub(out.len()) {
                            return Err(d.lim_s(s, "dom-string"));
                        }
                        out.push_str(text);
                    }
                }
                out
            }
        }
    };
    d.js_str_s(s, &out)
}

pub(crate) fn set_text_content_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    let v = arg_s::<E>(s, a, 0);
    let text = if is_nullish::<E>(s, v) { String::new() } else { d.string_arg_s(s, v)? };
    match d.data(n) {
        NodeData::Element { .. } | NodeData::ShadowRoot => {
            let r = d.mo_group(Some(n), || {
                let mut doc = d.doc.borrow_mut();
                d.room_s(s, &doc, !text.is_empty() as usize, text.len())?;
                // Charge before the tree changes: a step limit then leaves the old children, never half of them.
                s.charge(doc.children(n).count() as u64)?;
                d.iter_told_all(&doc, n);
                while let Some(c) = doc.children(n).next() {
                    doc.detach(c);
                }
                if !text.is_empty() {
                    let t = doc.create_text(&text);
                    doc.move_to(t, n, None);
                }
                Ok(())
            });
            d.iter_told_end();
            r?;
        }
        NodeData::Text(_) | NodeData::Comment(_) => {
            let mut doc = d.doc.borrow_mut();
            d.room_s(s, &doc, 0, text.len())?;
            d.range_set_data(&doc, n, &text);
            doc.set_text(n, &text);
        }
        NodeData::Document | NodeData::Doctype(_) => {}
    }
    Ok(s.undefined())
}

pub(crate) fn append_child_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let parent = d.nd_s(s, this)?;
    let node = d.arg_node_s(s, a, 0)?;
    d.insert_checked_s(s, parent, node, None, Op("appendChild", "Node"))?;
    Ok(arg_s::<E>(s, a, 0))
}

pub(crate) fn insert_before<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let parent = d.nd_s(s, this)?;
    let node = d.arg_node_s(s, a, 0)?;
    let before = d.opt_node_s(s, a, 1)?;
    d.insert_checked_s(s, parent, node, before, Op("insertBefore", "Node"))?;
    Ok(arg_s::<E>(s, a, 0))
}

pub(crate) fn remove_child<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let parent = d.nd_s(s, this)?;
    let child = d.arg_node_s(s, a, 0)?;
    if d.doc.borrow().parent(child) != Some(parent) {
        return Err(d.dom_err_s(s, "NotFoundError", "The node to be removed is not a child of this node."));
    }
    d.doc.borrow_mut().detach(child);
    Ok(arg_s::<E>(s, a, 0))
}

pub(crate) fn replace_child<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let parent = d.nd_s(s, this)?;
    let new = d.arg_node_s(s, a, 0)?;
    let old = d.arg_node_s(s, a, 1)?;
    if d.doc.borrow().parent(old) != Some(parent) {
        return Err(d.refuse_s(s, Op("replaceChild", "Node"), parent, "NotFoundError", "The node to be replaced is not a child of this node."));
    }
    if new == old {
        return Ok(arg_s::<E>(s, a, 1));
    }
    let before = d.doc.borrow().next_sibling(old);
    d.insert_many_s(s, parent, &[new], before, Some(old), Op("replaceChild", "Node"))?;
    Ok(arg_s::<E>(s, a, 1))
}

pub(crate) fn append<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let parent = d.nd_s(s, this)?;
    let nodes = d.nodes_from_s(s, a)?;
    d.insert_many_s(s, parent, &nodes, None, None, Op("append", ""))?;
    Ok(s.undefined())
}

pub(crate) fn prepend<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let parent = d.nd_s(s, this)?;
    let nodes = d.nodes_from_s(s, a)?;
    let first = {
        let doc = d.doc.borrow();
        let mut c = doc.children(parent).next();
        while let Some(k) = c.filter(|k| nodes.contains(k)) {
            s.charge(nodes.len() as u64)?;
            c = doc.next_sibling(k);
        }
        c
    };
    d.insert_many_s(s, parent, &nodes, first, None, Op("prepend", ""))?;
    Ok(s.undefined())
}

pub(crate) fn remove<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    d.doc.borrow_mut().detach(n);
    Ok(s.undefined())
}

/// `before`, `after` and `replaceWith` (`how` 0, 1, 2) of ChildNode. A node that is also a sibling in the list does not count as the viable sibling.
pub(crate) fn child_node<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>, how: u8) -> Res<'s, Local<'s>> {
    let me = d.nd_s(s, this)?;
    let nodes = d.nodes_from_s(s, a)?;
    let Some(parent) = d.doc.borrow().parent(me) else { return Ok(s.undefined()) };
    let mut old = None;
    let before = {
        let doc = d.doc.borrow();
        let skip = |s: &mut Sc<'s, E>, mut cur: Option<NodeId>| -> Res<'s, Option<NodeId>> {
            while let Some(c) = cur.filter(|c| nodes.contains(c)) {
                s.charge(nodes.len() as u64)?;
                cur = doc.next_sibling(c);
            }
            Ok(cur)
        };
        if how == 0 {
            // The node after the closest earlier sibling that stays, once the list has moved out.
            let mut prev = doc.prev_sibling(me);
            while let Some(c) = prev.filter(|c| nodes.contains(c)) {
                s.charge(nodes.len() as u64)?;
                prev = doc.prev_sibling(c);
            }
            skip(s, prev.map_or_else(|| doc.children(parent).next(), |p| doc.next_sibling(p)))?
        } else {
            if how == 2 && (!nodes.contains(&me) || nodes.len() == 1) {
                old = Some(me);
            }
            skip(s, doc.next_sibling(me))?
        }
    };
    let iface = match d.data(me) {
        NodeData::Element { .. } => "Element",
        NodeData::Doctype(_) => "DocumentType",
        _ => "CharacterData",
    };
    d.insert_many_s(s, parent, &nodes, before, old, Op(["before", "after", "replaceWith"][how as usize], iface))?;
    Ok(s.undefined())
}

/// `replaceChildren` of Element and DocumentFragment: every node is checked first, so a refusal changes nothing. Then all children go and the nodes
/// come in as one change group (one record; a live range and an iterator are told once).
pub(crate) fn replace_children<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let parent = d.nd_s(s, this)?;
    let nodes = d.nodes_from_s(s, a)?;
    let op = Op("replaceChildren", "");
    if matches!(d.data(parent), NodeData::Document) {
        d.check_doc_s(s, parent, &nodes, None, None, op)?;
        for &n in &nodes {
            d.check_depth_s(s, parent, n)?;
        }
    } else {
        for &n in &nodes {
            d.check_insert_s(s, parent, n, None, None, op)?;
        }
    }
    let mut moved = Vec::new();
    let r = d.mo_group(Some(parent), || {
        let mut doc = d.doc.borrow_mut();
        // Charge before the tree changes: a step limit then leaves the old children, never half of them.
        let new_kids: usize = nodes.iter().map(|&n| if matches!(doc.data(n), NodeData::ShadowRoot) { doc.children(n).count() } else { 1 }).sum();
        s.charge((doc.children(parent).count() + new_kids) as u64)?;
        d.iter_told_all(&doc, parent);
        // With more than one node the standard first puts them in a fragment, which takes each out of its parent, in argument order.
        if nodes.len() > 1 {
            for &n in &nodes {
                doc.detach(n);
            }
        }
        while let Some(c) = doc.children(parent).next() {
            doc.detach(c);
        }
        for &n in &nodes {
            let frag = matches!(doc.data(n), NodeData::ShadowRoot);
            let kids: Vec<NodeId> = if frag { doc.children(n).collect() } else { vec![n] };
            for k in kids {
                doc.move_to(k, parent, None);
                moved.push(k);
            }
        }
        Ok(())
    });
    d.iter_told_end();
    r?;
    for k in moved {
        script_inserted_s(d, s, k)?;
    }
    Ok(s.undefined())
}

/// The start of `insertAdjacentElement/Text/HTML`: the receiver and the position text, after the argument count check.
pub(crate) fn adjacent_start_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>, name: &str) -> Res<'s, (NodeId, String)> {
    let me = d.el_s(s, this)?;
    if a.len() < 2 {
        return Err(s.type_error(&format!("Failed to execute '{name}' on 'Element': 2 arguments required, but only {} present.", a.len())));
    }
    Ok((me, d.arg_str_s(s, a, 0)?))
}

/// `beforebegin`, `afterbegin`, `beforeend`, `afterend` (any case) as 0 to 3.
pub(crate) fn adjacent_pos_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, name: &str, pos: &str) -> Res<'s, u8> {
    match ["beforebegin", "afterbegin", "beforeend", "afterend"].iter().position(|p| pos.eq_ignore_ascii_case(p)) {
        Some(i) => Ok(i as u8),
        None => Err(d.dom_err_s(s, "SyntaxError", &format!("Failed to execute '{name}' on 'Element': The value provided ('{pos}') is not one of 'beforeBegin', 'afterBegin', 'beforeEnd', or 'afterEnd'."))),
    }
}

/// The parent and the node that a new node goes before, for position `pos` of `me`. None: the position is next to `me` and `me` has no parent.
pub(crate) fn adjacent_place<E: Eng>(d: &Dom<E>, me: NodeId, pos: u8) -> Option<(NodeId, Option<NodeId>)> {
    let doc = d.doc.borrow();
    match pos {
        0 => doc.parent(me).map(|p| (p, Some(me))),
        1 => Some((me, doc.children(me).next())),
        2 => Some((me, None)),
        _ => doc.parent(me).map(|p| (p, doc.next_sibling(me))),
    }
}

/// `insertAdjacentElement(where, element)`: the element, or null when there is no parent for `beforebegin` or `afterend`.
pub(crate) fn insert_adjacent_element<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    const NAME: &str = "insertAdjacentElement";
    let (me, ps) = adjacent_start_s(d, s, this, a, NAME)?;
    let a1 = arg_s::<E>(s, a, 1);
    let node = match d.node_of_s(s, a1).filter(|&n| d.is_element(n)) {
        Some(n) => n,
        None => return Err(s.type_error("Failed to execute 'insertAdjacentElement' on 'Element': parameter 2 is not of type 'Element'.")),
    };
    let pos = adjacent_pos_s(d, s, NAME, &ps)?;
    let Some((parent, before)) = adjacent_place(d, me, pos) else { return Ok(s.null()) };
    d.insert_checked_s(s, parent, node, before, Op(NAME, "Element"))?;
    Ok(a1)
}

/// `insertAdjacentText(where, data)`: a new text node, which does not join its neighbours.
pub(crate) fn insert_adjacent_text<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    const NAME: &str = "insertAdjacentText";
    let (me, ps) = adjacent_start_s(d, s, this, a, NAME)?;
    let text = d.arg_str_s(s, a, 1)?;
    let pos = adjacent_pos_s(d, s, NAME, &ps)?;
    let Some((parent, before)) = adjacent_place(d, me, pos) else { return Ok(s.undefined()) };
    let t = {
        let mut doc = d.doc.borrow_mut();
        d.room_s(s, &doc, 1, text.len())?;
        doc.create_text(&text)
    };
    d.insert_checked_s(s, parent, t, before, Op(NAME, "Element"))?;
    Ok(s.undefined())
}

/// `document.createDocumentFragment()`.
pub(crate) fn create_document_fragment<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    d.nd_s(s, this)?;
    new_fragment_s(d, s, None)
}

/// `new DocumentFragment()`.
pub(crate) fn fragment_ctor_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let default = d.obj_local(s, d.p().fragment);
    let proto = s.proto_from_new_target(default)?;
    new_fragment_s(d, s, Some(proto))
}

/// An empty fragment of the document. `proto`: the prototype of a subclass.
pub(crate) fn new_fragment_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, proto: Option<Local<'s>>) -> Res<'s, Local<'s>> {
    tag_fragments_s(d, s)?;
    let n = {
        let mut doc = d.doc.borrow_mut();
        d.room_s(s, &doc, 1, 0)?;
        doc.create_fragment()
    };
    let w = d.wrap_s(s, n)?;
    if let Some(p) = proto
        && !d.is_obj_s(s, p, d.p().fragment)
    {
        s.set_prototype_of(w, Some(p))?;
    }
    Ok(w)
}

pub(crate) fn clone_node<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    let arg0 = arg_s::<E>(s, a, 0);
    let deep = s.to_boolean(arg0);
    if matches!(d.data(n), NodeData::Document) {
        return Err(d.dom_err_s(s, "NotSupportedError", "Cloning a Document is not supported."));
    }
    // DOM Standard cloneNode(): a shadow root cannot be cloned. (A host is cloned without its root: `clonable` is always false here.)
    if matches!(d.data(n), NodeData::ShadowRoot) && !d.is_plain_fragment(n) {
        return Err(d.dom_err_s(s, "NotSupportedError", "ShadowRoot nodes are not clonable."));
    }
    let c = clone_tree_s(d, s, n, deep, false)?;
    d.wrap_s(s, c)
}

/// The clone of `n` (and of what it holds, when `deep`): a new tree, not linked. The Document and a shadow root are the caller's to refuse.
/// `import`: the clone belongs to the document even when `n` is in the inert tree of a template (`importNode`).
pub(crate) fn clone_tree_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, n: NodeId, deep: bool, import: bool) -> Res<'s, NodeId> {
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
                s.charge(1)?;
                (cur, depth) = (c, cd);
                count += 1;
                account(c);
                contents.extend(doc.template_content(c));
            }
            while let Some(f) = contents.pop() {
                count += 1;
                let (mut cur, mut depth) = (f, 0);
                while let Some((c, cd)) = next_in(&doc, f, cur, depth) {
                    s.charge(1)?;
                    (cur, depth) = (c, cd);
                    count += 1;
                    account(c);
                    contents.extend(doc.template_content(c));
                }
            }
        }
        d.room_s(s, &doc, count, bytes)?;
        if attrs > d.lim.max_attr_slots.saturating_sub(doc.live_attr_slots()) {
            return Err(d.lim_s(s, "dom-attrs"));
        }
        if input_bytes > d.lim.max_text_bytes.saturating_sub(st.input_bytes) {
            return Err(d.lim_s(s, "dom-input-bytes"));
        }
    }
    let root_clone = {
        let mut doc = d.doc.borrow_mut();
        let mut root_clone = None;
        // The clone of an element that has a definition is upgraded, unless the source is in the inert tree of a template.
        let ce = d.ce.on.get() && (import || !doc.in_template_content(n));
        let mut elements = Vec::new();
        let mut stack: Vec<(NodeId, Option<NodeId>)> = vec![(n, None)];
        let mut st = d.st.borrow_mut();
        while let Some((src, new_parent)) = stack.pop() {
            s.charge(1)?;
            let c = match *doc.data(src) {
                NodeData::Element { .. } => {
                    let c = doc.clone_element(src);
                    if ce {
                        elements.push((src, c));
                    }
                    c
                }
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
                let u = doc.ui(src);
                doc.set_ui(c, u);
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
        d.clone_upgrades(&doc, &elements);
        root_clone.expect("one clone")
    };
    Ok(root_clone)
}

// ------------------------------------------------------------------------------------------ Element

pub(crate) fn tag_name<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    let name = crate::browser_namespace::tag(&d.doc.borrow(), n);
    d.js_str_s(s, &name)
}

pub(crate) fn local_name<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    let name = {
        let doc = d.doc.borrow();
        let NodeData::Element { name, .. } = *doc.data(n) else { unreachable!() };
        doc.atom_str(name).to_string()
    };
    d.js_str_s(s, &name)
}

pub(crate) fn attr_set_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, n: NodeId, name: &str, value: &str) -> Res<'s, ()> {
    let mut doc = d.doc.borrow_mut();
    d.attr_room_s(s, &doc, n, name, value)?;
    s.charge(doc.attr_count(n) as u64 + 1)?;
    doc.set_attr(n, name, value);
    Ok(())
}

fn reflect_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, name: &str) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    let v = {
        let doc = d.doc.borrow();
        doc.find_atom(name).and_then(|a| doc.attr(n, a)).map(str::to_string)
    };
    d.js_str_s(s, v.as_deref().unwrap_or(""))
}

fn reflect_set<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>, name: &str) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    let text = d.arg_str_s(s, a, 0)?;
    attr_set_s(d, s, n, name, &text)?;
    Ok(s.undefined())
}

pub(crate) fn id<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    reflect_get(d, s, this, "id")
}

pub(crate) fn set_id<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    reflect_set(d, s, this, a, "id")
}

pub(crate) fn class_name<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    reflect_get(d, s, this, "class")
}

pub(crate) fn set_class_name<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    reflect_set(d, s, this, a, "class")
}

pub(crate) fn lang<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    reflect_get(d, s, this, "lang")
}

pub(crate) fn set_lang<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    reflect_set(d, s, this, a, "lang")
}

pub(crate) fn get_attribute_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    let arg0 = arg_s::<E>(s, a, 0);
    let name = d.string_arg_s(s, arg0)?;
    let v = {
        let doc = d.doc.borrow();
        let name = crate::browser_namespace::attr_key(&doc, n, &name);
        doc.find_attr_qname(n, &name).and_then(|a| doc.attr(n, a)).map(str::to_string)
    };
    match v {
        Some(v) => d.js_str_s(s, &v),
        None => Ok(s.null()),
    }
}

pub(crate) fn has_attribute_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    let arg0 = arg_s::<E>(s, a, 0);
    let name = d.string_arg_s(s, arg0)?;
    let has = {
        let doc = d.doc.borrow();
        doc.find_attr_qname(n, &crate::browser_namespace::attr_key(&doc, n, &name)).is_some()
    };
    Ok(s.boolean(has))
}

pub(crate) fn matches_selector_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    let arg0 = arg_s::<E>(s, a, 0);
    let text = d.string_arg_s(s, arg0)?;
    let sels = d.selectors_at_s(s, &text, n)?;
    let b = match_here(d, s, n, &sels)?;
    Ok(s.boolean(b))
}

pub(crate) fn set_attribute<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    let name = d.arg_str_s(s, a, 0)?;
    let value = d.arg_str_s(s, a, 1)?;
    let (name, existing) = {
        let doc = d.doc.borrow();
        let name = crate::browser_namespace::attr_key(&doc, n, &name);
        let existing = doc.find_attr_qname(n, &name);
        (name, existing)
    };
    if !valid_name(&name) {
        return Err(d.dom_err_s(s, "InvalidCharacterError", &format!("'{name}' is not a valid attribute name.")));
    }
    match existing {
        Some(at) => ns_put_s(d, s, n, Some(at), &name, &value)?,
        None => attr_set_s(d, s, n, &name, &value)?,
    }
    if name == "src" {
        script_src_changed_s(d, s, n)?;
        media_src_changed_s(d, s, n)?;
        crate::browser_img::src_changed(d, n);
    }
    Ok(s.undefined())
}

pub(crate) fn remove_attribute_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    let name = d.arg_str_s(s, a, 0)?;
    let mut doc = d.doc.borrow_mut();
    s.charge(doc.attr_count(n) as u64 + 1)?;
    let name = crate::browser_namespace::attr_key(&doc, n, &name);
    if let Some(at) = doc.find_attr_qname(n, &name) {
        doc.remove_attr_atom(n, at);
    }
    Ok(s.undefined())
}

/// `Element.toggleAttribute(name, force)`.
pub(crate) fn toggle_attribute_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    let name = d.arg_str_s(s, a, 0)?;
    let f = arg_s::<E>(s, a, 1);
    let force = if s.type_of(f) == Type::Undefined { None } else { Some(s.to_boolean(f)) };
    let (name, existing) = {
        let doc = d.doc.borrow();
        let name = crate::browser_namespace::attr_key(&doc, n, &name);
        let existing = doc.find_attr_qname(n, &name);
        (name, existing)
    };
    if !valid_name(&name) {
        return Err(d.dom_err_s(s, "InvalidCharacterError", &format!("'{name}' is not a valid attribute name.")));
    }
    match existing {
        Some(_) if force == Some(true) => Ok(s.boolean(true)),
        Some(at) => {
            let mut doc = d.doc.borrow_mut();
            s.charge(doc.attr_count(n) as u64 + 1)?;
            doc.remove_attr_atom(n, at);
            Ok(s.boolean(false))
        }
        None if force == Some(false) => Ok(s.boolean(false)),
        None => {
            attr_set_s(d, s, n, &name, "")?;
            if name == "src" {
                script_src_changed_s(d, s, n)?;
            }
            Ok(s.boolean(true))
        }
    }
}

pub(crate) fn get_attribute_names<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    let names: Vec<String> = {
        let doc = d.doc.borrow();
        let NodeData::Element { attrs, .. } = *doc.data(n) else { unreachable!() };
        s.charge(attrs_len(&doc, n) as u64)?;
        doc.attrs(attrs).iter().map(|t| doc.atom_str(t.name).to_string()).collect()
    };
    let mut vals = Vec::with_capacity(names.len());
    for name in names {
        vals.push(d.js_str_s(s, &name)?);
    }
    s.new_array(&vals)
}

fn attrs_len(doc: &Document, n: NodeId) -> usize {
    doc.attr_count(n)
}

pub(crate) fn query_selector<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    let text = d.arg_str_s(s, a, 0)?;
    let r = d.query_s(s, n, &text, false)?;
    d.wrap_opt_s(s, r.first().copied())
}

pub(crate) fn query_selector_all<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    let text = d.arg_str_s(s, a, 0)?;
    let r = d.query_s(s, n, &text, true)?;
    wrap_all_s(d, s, &r)
}

/// Does the selector match `n`? The cost charged is an upper bound of the matcher's work.
fn match_here<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, n: NodeId, sels: &[Selector]) -> Res<'s, bool> {
    let depth = d.depth_of_s(s, n)?;
    let doc = d.doc.borrow();
    s.charge(match_work(&doc, depth, sels))?;
    if sels.iter().any(needs_places) {
        s.charge(doc.len() as u64)?;
    }
    Ok(sels.iter().any(|sel| matches(&doc, n, sel)))
}

pub(crate) fn closest<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    let text = d.arg_str_s(s, a, 0)?;
    let sels = d.selectors_at_s(s, &text, n)?;
    let mut cur = Some(n);
    while let Some(c) = cur {
        if d.is_element(c) && match_here(d, s, c, &sels)? {
            return d.wrap_s(s, c);
        }
        cur = d.doc.borrow().parent(c);
    }
    Ok(s.null())
}

// ------------------------------------------------------------------------------------------ Document

fn new_name<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, String> {
    let name = d.string_arg_s(s, v)?;
    if !valid_name(&name) {
        return Err(d.dom_err_s(s, "InvalidCharacterError", &format!("The tag name provided ('{name}') is not a valid name.")));
    }
    Ok(lower(&name))
}

pub(crate) fn make_element_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, name: &str) -> Res<'s, Local<'s>> {
    let n = {
        let mut doc = d.doc.borrow_mut();
        d.room_s(s, &doc, 1, 0)?;
        if doc.find_atom(name).is_none() && doc.name_count() + 1 > d.lim.max_names {
            return Err(d.lim_s(s, "dom-names"));
        }
        doc.create_element(name)
    };
    d.wrap_s(s, n)
}

pub(crate) fn create_element_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    d.nd_s(s, this)?;
    let a0 = arg_s::<E>(s, a, 0);
    let name = new_name(d, s, a0)?;
    let v = if d.ce.on.get() || a.len() > 1 {
        let o = arg_s::<E>(s, a, 1);
        crate::browser_custom::create_s(d, s, &name, o)?
    } else {
        make_element_s(d, s, &name)?
    };
    crate::browser_frames::note_owner(d, s, this, v);
    Ok(v)
}

pub(crate) fn create_text_node<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    d.nd_s(s, this)?;
    let text = d.arg_str_s(s, a, 0)?;
    let n = {
        let mut doc = d.doc.borrow_mut();
        d.room_s(s, &doc, 1, text.len())?;
        doc.create_text(&text)
    };
    let v = d.wrap_s(s, n)?;
    crate::browser_frames::note_owner(d, s, this, v);
    Ok(v)
}

pub(crate) fn create_comment<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    d.nd_s(s, this)?;
    let text = d.arg_str_s(s, a, 0)?;
    let n = {
        let mut doc = d.doc.borrow_mut();
        d.room_s(s, &doc, 1, text.len())?;
        doc.create_comment(&text)
    };
    let v = d.wrap_s(s, n)?;
    crate::browser_frames::note_owner(d, s, this, v);
    Ok(v)
}

pub(crate) fn get_element_by_id<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let root = d.nd_s(s, this)?;
    let want = d.arg_str_s(s, a, 0)?;
    if want.is_empty() {
        return Ok(s.null());
    }
    let found = {
        let doc = d.doc.borrow();
        let (mut cur, mut depth, mut found) = (root, 0, None);
        while let Some((c, cd)) = next_in(&doc, root, cur, depth) {
            s.charge(1)?;
            (cur, depth) = (c, cd);
            if doc.attr(c, Atom::ID) == Some(want.as_str()) {
                found = Some(c);
                break;
            }
        }
        found
    };
    d.wrap_opt_s(s, found)
}

fn doc_child<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, pick: impl Fn(&Document, NodeId) -> bool) -> Res<'s, Local<'s>> {
    let root = d.nd_s(s, this)?;
    let r = {
        let doc = d.doc.borrow();
        doc.children(root).find(|&c| pick(&doc, c))
    };
    d.wrap_opt_s(s, r)
}

pub(crate) fn is_named(doc: &Document, n: NodeId, names: &[&str]) -> bool {
    matches!(*doc.data(n), NodeData::Element { name, .. } if names.contains(&doc.atom_str(name))) && !doc.is_foreign(n)
}

pub(crate) fn document_element_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    doc_child(d, s, this, |doc, c| matches!(doc.data(c), NodeData::Element { .. }))
}

fn html_child<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, names: &[&str]) -> Res<'s, Local<'s>> {
    let root = d.nd_s(s, this)?;
    let r = {
        let doc = d.doc.borrow();
        doc.children(root).find(|&c| matches!(doc.data(c), NodeData::Element { .. })).and_then(|html| doc.children(html).find(|&c| is_named(&doc, c, names)))
    };
    d.wrap_opt_s(s, r)
}

pub(crate) fn head<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    html_child(d, s, this, &["head"])
}

pub(crate) fn body_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    html_child(d, s, this, &["body", "frameset"])
}

/// The first HTML `title` element of the document, in tree order.
fn title_element<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, root: NodeId) -> Res<'s, Option<NodeId>> {
    let doc = d.doc.borrow();
    let (mut cur, mut depth) = (root, 0);
    while let Some((c, cd)) = next_in(&doc, root, cur, depth) {
        s.charge(1)?;
        (cur, depth) = (c, cd);
        if is_named(&doc, c, &["title"]) {
            return Ok(Some(c));
        }
    }
    Ok(None)
}

/// `document.title`: the text of the first `title` element with ASCII white space stripped and collapsed.
pub(crate) fn document_title<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let root = d.nd_s(s, this)?;
    let Some(t) = title_element(d, s, root)? else { return d.js_str_s(s, "") };
    let text: String = {
        let doc = d.doc.borrow();
        doc.children(t).filter_map(|c| if let NodeData::Text(sp) = *doc.data(c) { Some(doc.text(sp).to_string()) } else { None }).collect()
    };
    s.charge((text.len() / 32) as u64)?;
    let ws = |c: char| matches!(c, ' ' | '\t' | '\n' | '\x0c' | '\r');
    let out = text.split(ws).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    d.js_str_s(s, &out)
}

/// `document.title = v`: set the text of the first `title` element. Without one, a new `title` goes at the end of `head` (no `head`: nothing).
pub(crate) fn set_document_title<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let root = d.nd_s(s, this)?;
    let text = d.arg_str_s(s, a, 0)?;
    let existing = title_element(d, s, root)?;
    let target = match existing {
        Some(t) => Some(t),
        None => {
            let mut doc = d.doc.borrow_mut();
            let head = doc.children(root).find(|&c| matches!(doc.data(c), NodeData::Element { .. })).and_then(|html| doc.children(html).find(|&c| is_named(&doc, c, &["head"])));
            match head {
                Some(h) => {
                    d.room_s(s, &doc, 2, text.len())?;
                    let none: &[(&str, &str)] = &[];
                    Some(doc.insert_element(h, None, "title", none))
                }
                None => None,
            }
        }
    };
    if let Some(t) = target {
        let r = d.mo_group(Some(t), || {
            let mut doc = d.doc.borrow_mut();
            d.room_s(s, &doc, !text.is_empty() as usize, text.len())?;
            d.iter_told_all(&doc, t);
            while let Some(c) = doc.children(t).next() {
                doc.detach(c);
            }
            if !text.is_empty() {
                let node = doc.create_text(&text);
                doc.move_to(node, t, None);
            }
            Ok(())
        });
        d.iter_told_end();
        r?;
    }
    Ok(s.undefined())
}

pub(crate) fn doctype<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    doc_child(d, s, this, |doc, c| matches!(doc.data(c), NodeData::Doctype(_)))
}

pub(crate) fn default_view<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    if n != d.doc.borrow().root() {
        return Ok(match window_of_s(d, s, n) {
            Some(w) => w,
            None => s.null(),
        });
    }
    Ok(d.obj_local(s, *d.global.get().expect("installed")))
}

// ------------------------------------------------------------------------------------------ <input>

pub(crate) fn input_el_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, NodeId> {
    let n = d.el_s(s, this)?;
    let is_input = is_named(&d.doc.borrow(), n, &["input"]);
    if is_input { Ok(n) } else { Err(s.type_error("Illegal invocation")) }
}

pub(crate) fn input_value<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = input_el_s(d, s, this)?;
    let text = d.input_value_of(n).unwrap_or_default();
    d.js_str_s(s, &text)
}

pub(crate) fn set_input_value<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = input_el_s(d, s, this)?;
    let v = arg_s::<E>(s, a, 0);
    let text = if s.type_of(v) == Type::Null { String::new() } else { d.string_arg_s(s, v)? };
    let kind = input_kind(&d.doc.borrow(), n);
    match kind {
        // "default/on" mode: the attribute is the value.
        "checkbox" | "radio" | "submit" | "reset" | "button" | "image" | "hidden" => attr_set_s(d, s, n, "value", &text)?,
        "file" if !text.is_empty() => return Err(d.dom_err_s(s, "InvalidStateError", "This input element accepts a filename, which may only be programmatically set to the empty string.")),
        "file" => {}
        _ => {
            let multiple = kind == "email" && d.doc.borrow().find_atom("multiple").is_some_and(|a| d.doc.borrow().attr(n, a).is_some());
            let clean = match crate::browser_controls::sanitize(kind, multiple, &text) {
                std::borrow::Cow::Owned(o) => Some(o),
                std::borrow::Cow::Borrowed(b) if b.len() != text.len() => Some(b.to_string()),
                _ => None,
            };
            put_input_value_s(d, s, n, clean.unwrap_or(text))?
        }
    }
    Ok(s.undefined())
}

/// `input.files`: an array of the Files (read from disk now), `null` for another kind of input. It is an array, not a FileList.
pub(crate) fn input_files<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = input_el_s(d, s, this)?;
    if input_kind(&d.doc.borrow(), n) != "file" {
        return Ok(s.null());
    }
    let refs = d.st.borrow().inputs.get(&n).map(|i| i.files.clone()).unwrap_or_default();
    let mut files = Vec::new();
    for f in &refs {
        files.push(file_from_ref_s(d, s, f)?);
    }
    s.new_array(&files)
}

pub(crate) fn input_checked<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = input_el_s(d, s, this)?;
    if let Some(c) = d.st.borrow().inputs.get(&n).and_then(|i| i.checked) {
        return Ok(s.boolean(c));
    }
    let doc = d.doc.borrow();
    Ok(s.boolean(doc.find_atom("checked").and_then(|a| doc.attr(n, a)).is_some()))
}

pub(crate) fn set_input_checked<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = input_el_s(d, s, this)?;
    let a0 = arg_s::<E>(s, a, 0);
    let b = s.to_boolean(a0);
    // A radio that gets checked unchecks the others of its group.
    if b && input_kind(&d.doc.borrow(), n) == "radio" {
        for o in radio_group_s(d, s, n)? {
            d.st.borrow_mut().inputs.entry(o).or_default().checked = Some(false);
            d.sync_ui(o);
        }
    }
    d.st.borrow_mut().inputs.entry(n).or_default().checked = Some(b);
    d.sync_ui(n);
    Ok(s.undefined())
}

pub(crate) fn input_indeterminate<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = input_el_s(d, s, this)?;
    let b = d.st.borrow().inputs.get(&n).is_some_and(|i| i.indeterminate);
    Ok(s.boolean(b))
}

pub(crate) fn set_input_indeterminate<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = input_el_s(d, s, this)?;
    let a0 = arg_s::<E>(s, a, 0);
    let b = s.to_boolean(a0);
    d.st.borrow_mut().inputs.entry(n).or_default().indeterminate = b;
    d.sync_ui(n);
    Ok(s.undefined())
}

pub(crate) fn input_type<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = input_el_s(d, s, this)?;
    let k = input_kind(&d.doc.borrow(), n);
    d.js_str_s(s, k)
}

pub(crate) fn set_input_type<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    input_el_s(d, s, this)?;
    reflect_set(d, s, this, a, "type")
}

pub(crate) fn default_value<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    input_el_s(d, s, this)?;
    reflect_get(d, s, this, "value")
}

pub(crate) fn set_default_value<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = input_el_s(d, s, this)?;
    let text = d.arg_str_s(s, a, 0)?;
    set_value_attr_s(d, s, n, &text)?;
    Ok(s.undefined())
}

pub(crate) fn default_checked<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = input_el_s(d, s, this)?;
    let doc = d.doc.borrow();
    Ok(s.boolean(doc.find_atom("checked").and_then(|a| doc.attr(n, a)).is_some()))
}

pub(crate) fn set_default_checked<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = input_el_s(d, s, this)?;
    let a0 = arg_s::<E>(s, a, 0);
    if s.to_boolean(a0) {
        attr_set_s(d, s, n, "checked", "")?;
    } else {
        d.doc.borrow_mut().remove_attr(n, "checked");
    }
    Ok(s.undefined())
}

/// An HTML `link` in the HTML namespace. An SVG or MathML `link` is not one (`is_named` skips foreign elements).
pub(crate) fn link_el_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, NodeId> {
    let n = d.el_s(s, this)?;
    if is_named(&d.doc.borrow(), n, &["link"]) { Ok(n) } else { Err(s.type_error("Illegal invocation")) }
}

pub(crate) fn link_rel<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = link_el_s(d, s, this)?;
    let doc = d.doc.borrow();
    let value = doc.find_atom("rel").and_then(|a| doc.attr(n, a)).unwrap_or("");
    d.js_str_s(s, value)
}

pub(crate) fn set_link_rel<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    link_el_s(d, s, this)?;
    reflect_set(d, s, this, a, "rel")
}

/// The `as` values that the Fetch Standard names (its destinations). Any other text reads as the empty string.
pub(crate) const LINK_AS: [&str; 23] = [
    "fetch", "audio", "audioworklet", "document", "embed", "font", "frame", "iframe", "image", "json", "manifest", "object", "paintworklet",
    "report", "script", "serviceworker", "sharedworker", "style", "track", "video", "webidentity", "worker", "xslt",
];

/// A known `as` value is given in lower case. An unknown one is the empty string (an enumerated attribute with no default).
pub(crate) fn link_as<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = link_el_s(d, s, this)?;
    let doc = d.doc.borrow();
    let raw = doc.find_atom("as").and_then(|a| doc.attr(n, a)).unwrap_or("");
    let known = LINK_AS.iter().find(|k| k.eq_ignore_ascii_case(raw));
    d.js_str_s(s, known.copied().unwrap_or(""))
}

pub(crate) fn set_link_as<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    link_el_s(d, s, this)?;
    reflect_set(d, s, this, a, "as")
}

/// `null` when the attribute is absent, `use-credentials` when it says so (any case), else `anonymous`.
pub(crate) fn link_cross_origin<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = link_el_s(d, s, this)?;
    let doc = d.doc.borrow();
    let Some(raw) = doc.find_atom("crossorigin").and_then(|a| doc.attr(n, a)) else { return Ok(s.null()) };
    d.js_str_s(s, if raw.eq_ignore_ascii_case("use-credentials") { "use-credentials" } else { "anonymous" })
}

/// `null` removes the attribute. Any other value is converted to a string and written.
pub(crate) fn set_link_cross_origin<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = link_el_s(d, s, this)?;
    let a0 = arg_s::<E>(s, a, 0);
    if s.type_of(a0) == Type::Null {
        d.doc.borrow_mut().remove_attr(n, "crossorigin");
        return Ok(s.undefined());
    }
    reflect_set(d, s, this, a, "crossorigin")
}
