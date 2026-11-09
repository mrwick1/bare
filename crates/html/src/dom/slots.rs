//! Slots: the assignment of the light children of a shadow host to the `<slot>` elements of its shadow tree, and the two trees that
//! follow from it. DOM Standard "assign slottables", and Blink's `FlatTreeTraversal`.

use super::*;

/// The slot assignment of a document at one `revision`. Made the first time that a slot or the flat tree is read after a change, so a
/// page without shadow roots never makes it.
#[derive(Default)]
pub(super) struct Slots {
    rev: u64,
    /// Each `<slot>` of a shadow tree (also one that takes nothing) and the nodes that it takes, in order.
    assigned: HashMap<u32, Vec<NodeId>, BuildHasherDefault<Fnv>>,
    /// Each taken node: its slot and its place in the list of the slot.
    of: HashMap<u32, (NodeId, u32), BuildHasherDefault<Fnv>>,
}

impl Document {
    /// The slot assignment now. `revision` moves at every change of the tree and of an attribute, which are the inputs.
    fn slots(&self) -> std::cell::Ref<'_, Slots> {
        if self.slots.borrow().as_ref().is_none_or(|s| s.rev != self.revision) {
            let made = self.make_slots();
            *self.slots.borrow_mut() = Some(made);
        }
        std::cell::Ref::map(self.slots.borrow(), |s| s.as_ref().expect("made above"))
    }

    /// DOM Standard "assign slottables for a tree" for every shadow root: the first `<slot>` of a name, in tree order, takes the light
    /// children of the host (elements by their `slot` attribute, text by the empty name).
    fn make_slots(&self) -> Slots {
        let mut out = Slots { rev: self.revision, ..Slots::default() };
        let Some(slot) = self.find_atom("slot") else { return out };
        let name = self.find_atom("name");
        let mut first: HashMap<&str, NodeId> = HashMap::new();
        for (&host, &root) in &self.shadow_of {
            first.clear();
            let mut stack: Vec<NodeId> = self.children(root).collect();
            stack.reverse();
            while let Some(n) = stack.pop() {
                if matches!(self.nodes[n.index()].data, NodeData::Element { name: tag, .. } if tag == slot) && self.element_ns(n) == Ns::Html {
                    out.assigned.insert(n.index() as u32, Vec::new());
                    first.entry(name.and_then(|a| self.attr(n, a)).unwrap_or("")).or_insert(n);
                }
                let at = stack.len();
                stack.extend(self.children(n));
                stack[at..].reverse();
            }
            if first.is_empty() {
                continue;
            }
            let host = NodeId::from_index(host as usize);
            for c in self.children(host) {
                let wanted = match self.nodes[c.index()].data {
                    NodeData::Element { .. } => self.attr(c, slot).unwrap_or(""),
                    NodeData::Text(_) => "",
                    _ => continue,
                };
                if let Some(&s) = first.get(wanted) {
                    let list = out.assigned.get_mut(&(s.index() as u32)).expect("a slot of the tree");
                    out.of.insert(c.index() as u32, (s, list.len() as u32));
                    list.push(c);
                }
            }
        }
        out
    }

    /// The nodes that the slot `slot` takes, in order. Empty for a node that is not a slot of a shadow tree.
    pub fn assigned_nodes(&self, slot: NodeId) -> Vec<NodeId> {
        if self.shadow_of.is_empty() {
            return Vec::new();
        }
        self.slots().assigned.get(&(slot.index() as u32)).cloned().unwrap_or_default()
    }

    /// The slot that takes `node`: a light child of a shadow host (an element, or text) that a `<slot>` of the shadow tree takes.
    pub fn assigned_slot(&self, node: NodeId) -> Option<NodeId> {
        if self.shadow_of.is_empty() {
            return None;
        }
        self.slots().of.get(&(node.index() as u32)).map(|&(s, _)| s)
    }

    /// Does any shadow tree have a `<slot>`?
    pub fn has_slots(&self) -> bool {
        !self.shadow_of.is_empty() && !self.slots().assigned.is_empty()
    }

    /// Is `el` a `<slot>` element of a shadow tree?
    pub fn is_slot(&self, el: NodeId) -> bool {
        !self.shadow_of.is_empty() && self.slots().assigned.contains_key(&(el.index() as u32))
    }

    /// The top node of the tree of `n`: the document node, a shadow root, or the root of a detached tree.
    pub fn tree_root(&self, n: NodeId) -> NodeId {
        let mut top = n;
        while let Some(p) = self.nodes[top.index()].parent {
            top = p;
        }
        top
    }

    /// The parent of `id` in the flat tree (`flat_parent`), or in the tree of the cascade: with `style`, a slot is a node, the parent of the
    /// nodes that it takes. Else a slot makes no box and the nodes pass to its parent. `None` for a light child that no slot takes.
    #[inline(never)]
    pub(super) fn shadowed_parent(&self, id: NodeId, p: NodeId, style: bool) -> Option<NodeId> {
        if let Some(host) = self.shadow_host(p) {
            return Some(host);
        }
        if self.shadow_root(p).is_some() {
            let slot = self.assigned_slot(id)?;
            return if style { Some(slot) } else { self.shadowed_up(slot) };
        }
        if self.is_slot(p) {
            // The fallback shows only while the slot takes nothing.
            return if !self.assigned_nodes(p).is_empty() { None } else if style { Some(p) } else { self.shadowed_up(p) };
        }
        Some(p)
    }

    /// The flat parent of the slot `slot`.
    fn shadowed_up(&self, slot: NodeId) -> Option<NodeId> {
        let p = self.parent(slot)?;
        self.shadowed_parent(slot, p, false)
    }

    /// The parent of `id` in the tree of the cascade: the flat tree with the slots in it. A page without shadow roots pays one check.
    #[inline]
    pub fn style_parent(&self, id: NodeId) -> Option<NodeId> {
        let p = self.parent(id)?;
        if self.shadow_of.is_empty() { Some(p) } else { self.shadowed_parent(id, p, true) }
    }

    /// The first node of what the flat tree puts at the place of `c`: `c`, or for a slot what it takes (or its fallback), skipping the
    /// slots that give nothing.
    fn first_of(&self, c: NodeId) -> Option<NodeId> {
        if !self.is_slot(c) {
            return Some(c);
        }
        let list = self.assigned_nodes(c);
        let mut cur = if list.is_empty() { self.nodes[c.index()].first_child } else { Some(list[0]) };
        while let Some(x) = cur {
            if let Some(f) = self.first_of(x) {
                return Some(f);
            }
            cur = if list.is_empty() { self.next_sibling(x) } else { self.slots().of.get(&(x.index() as u32)).and_then(|&(_, i)| list.get(i as usize + 1)).copied() };
        }
        None
    }

    /// The candidate that follows `cur` in its sequence (the children of its parent, or the nodes of its slot), and the slot to go on
    /// after when the sequence ends.
    fn step(&self, cur: NodeId) -> (Option<NodeId>, Option<NodeId>) {
        let Some(p) = self.parent(cur) else { return (None, None) };
        if self.shadow_root(p).is_some() {
            return match self.slots().of.get(&(cur.index() as u32)) {
                Some(&(s, i)) => (self.slots().assigned.get(&(s.index() as u32)).and_then(|l| l.get(i as usize + 1)).copied(), Some(s)),
                None => (None, None),
            };
        }
        (self.next_sibling(cur), if self.is_slot(p) { Some(p) } else { None })
    }

    /// The next sibling of `id` in the flat tree (`flat_children` walks with it). A slot makes no box: the nodes that it takes are siblings
    /// of its own siblings. A page without shadow roots pays one check.
    #[inline]
    pub fn flat_next(&self, id: NodeId) -> Option<NodeId> {
        if self.shadow_of.is_empty() {
            return self.next_sibling(id);
        }
        self.shadowed_next(id)
    }

    #[inline(never)]
    pub(super) fn shadowed_next(&self, id: NodeId) -> Option<NodeId> {
        let mut cur = id;
        loop {
            let (cand, up) = self.step(cur);
            let mut cand = cand;
            while let Some(c) = cand {
                if let Some(f) = self.first_of(c) {
                    return Some(f);
                }
                cand = self.step(c).0;
            }
            cur = up?;
        }
    }

    /// The first child of `id` in the flat tree.
    #[inline(never)]
    pub(super) fn shadowed_first(&self, id: NodeId) -> Option<NodeId> {
        let mut cur = self.nodes[self.shadow_root(id).unwrap_or(id).index()].first_child;
        while let Some(x) = cur {
            if let Some(f) = self.first_of(x) {
                return Some(f);
            }
            cur = self.step(x).0;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::parse;

    fn by_id(doc: &Document, id: &str) -> NodeId {
        let want = doc.find_atom("id").expect("id");
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if doc.attr(n, want) == Some(id) {
                return n;
            }
            stack.extend(doc.children(n));
            stack.extend(doc.shadow_root(n).into_iter().flat_map(|r| doc.children(r)));
        }
        panic!("no #{id}")
    }

    fn ids(doc: &Document, nodes: impl IntoIterator<Item = NodeId>) -> Vec<String> {
        let id = doc.find_atom("id").unwrap();
        nodes.into_iter().map(|n| if matches!(doc.data(n), NodeData::Text(_)) { "#text".to_string() } else { doc.attr(n, id).unwrap_or("?").to_string() }).collect()
    }

    /// A host `#h` with the light children `light` and the shadow tree `shadow` (an HTML text; its elements are copied in).
    fn page(light: &str, shadow: &str) -> Document {
        let mut doc = parse(&format!("<body><div id=h>{light}</div></body>"));
        let host = by_id(&doc, "h");
        let root = doc.attach_shadow(host, true);
        let frag = parse(&format!("<body>{shadow}</body>"));
        let body = frag.children(frag.children(frag.root()).next().unwrap()).last().unwrap();
        fn copy(doc: &mut Document, frag: &Document, from: NodeId, to: NodeId) {
            for c in frag.children(from).collect::<Vec<_>>() {
                let n = doc.import_node(frag, c).unwrap();
                doc.move_to(n, to, None);
                copy(doc, frag, c, n);
            }
        }
        copy(&mut doc, &frag, body, root);
        doc
    }

    #[test]
    fn a_slot_takes_the_light_children_with_its_name() {
        let doc = page("<span id=a slot=x></span><b id=b></b>text<i id=c slot=x></i>", "<slot id=sx name=x></slot><slot id=sd></slot>");
        let (sx, sd) = (by_id(&doc, "sx"), by_id(&doc, "sd"));
        assert!(doc.is_slot(sx) && doc.is_slot(sd));
        assert_eq!(ids(&doc, doc.assigned_nodes(sx)), ["a", "c"]);
        // Text goes to the slot with no name. The first slot of a name takes the nodes: a second `<slot>` of that name gets none.
        assert_eq!(ids(&doc, doc.assigned_nodes(sd)), ["b", "#text"]);
        assert_eq!(doc.assigned_slot(by_id(&doc, "a")), Some(sx));
        assert_eq!(doc.assigned_slot(by_id(&doc, "b")), Some(sd));
    }

    #[test]
    fn the_first_slot_of_a_name_wins_and_a_light_slot_is_no_slot() {
        let doc = page("<b id=b></b>", "<slot id=s1></slot><div><slot id=s2></slot></div>");
        assert_eq!(ids(&doc, doc.assigned_nodes(by_id(&doc, "s1"))), ["b"]);
        assert!(doc.assigned_nodes(by_id(&doc, "s2")).is_empty());
        let plain = parse("<slot id=s></slot>");
        assert!(!plain.is_slot(by_id(&plain, "s")));
        assert_eq!(plain.tree_root(by_id(&plain, "s")), plain.root());
    }

    #[test]
    fn the_flat_tree_shows_slotted_nodes_in_place_of_the_slot() {
        let doc = page("<b id=b></b><span id=a slot=x></span>", "<p id=p1></p><slot id=sx name=x></slot><div id=w><slot id=sd></slot></div>");
        let host = by_id(&doc, "h");
        // The slot makes no box: the nodes that it takes are in its place. The flat order follows the shadow tree.
        assert_eq!(ids(&doc, doc.flat_children(host)), ["p1", "a", "w"]);
        assert_eq!(ids(&doc, doc.flat_children(by_id(&doc, "w"))), ["b"]);
        assert_eq!(doc.flat_parent(by_id(&doc, "a")), Some(host));
        assert_eq!(doc.flat_parent(by_id(&doc, "b")), Some(by_id(&doc, "w")));
        assert_eq!(doc.flat_parent(by_id(&doc, "p1")), Some(host));
        // Siblings walk through the slot.
        assert_eq!(doc.flat_next(by_id(&doc, "p1")), Some(by_id(&doc, "a")));
        assert_eq!(doc.flat_next(by_id(&doc, "a")), Some(by_id(&doc, "w")));
        assert_eq!(doc.flat_next(by_id(&doc, "w")), None);
        assert_eq!(doc.flat_next(by_id(&doc, "b")), None);
    }

    #[test]
    fn a_slot_with_nothing_takes_its_fallback_and_an_unslotted_child_is_not_in_the_flat_tree() {
        let doc = page("<b id=b slot=none></b>", "<slot id=s><i id=f1></i><u id=f2></u></slot><em id=e></em>");
        let host = by_id(&doc, "h");
        assert_eq!(ids(&doc, doc.flat_children(host)), ["f1", "f2", "e"]);
        assert_eq!(doc.flat_next(by_id(&doc, "f2")), Some(by_id(&doc, "e")));
        assert_eq!(doc.flat_parent(by_id(&doc, "f1")), Some(host));
        assert_eq!(doc.flat_parent(by_id(&doc, "b")), None);
        // Taking a node hides the fallback.
        let doc = page("<b id=b></b>", "<slot id=s><i id=f1></i></slot><em id=e></em>");
        assert_eq!(ids(&doc, doc.flat_children(by_id(&doc, "h"))), ["b", "e"]);
        assert_eq!(doc.flat_parent(by_id(&doc, "f1")), None);
    }

    #[test]
    fn an_empty_slot_is_skipped_and_slots_nest() {
        let doc = page("<b id=b></b>", "<slot id=empty name=none></slot><slot id=s></slot>");
        let host = by_id(&doc, "h");
        assert_eq!(ids(&doc, doc.flat_children(host)), ["b"]);
        assert_eq!(doc.flat_next(by_id(&doc, "b")), None);
        // A slot inside a slot (a slot that is the light child of an inner host): the nodes pass through both.
        let mut doc = parse("<body><div id=outer><b id=b></b></div></body>");
        let outer = by_id(&doc, "outer");
        let r1 = doc.attach_shadow(outer, true);
        let inner = doc.insert_element(r1, None, "section", &[("id", "inner")]);
        let s1 = doc.insert_element(inner, None, "slot", &[("id", "s1")]);
        let r2 = doc.attach_shadow(inner, true);
        let s2 = doc.insert_element(r2, None, "slot", &[("id", "s2")]);
        let after = doc.insert_element(r2, None, "hr", &[("id", "after")]);
        assert_eq!(doc.assigned_nodes(s1), [by_id(&doc, "b")]);
        assert_eq!(doc.assigned_nodes(s2), [s1]);
        assert_eq!(ids(&doc, doc.flat_children(inner)), ["b", "after"]);
        assert_eq!(doc.flat_parent(by_id(&doc, "b")), Some(inner));
        assert_eq!(doc.flat_next(by_id(&doc, "b")), Some(after));
    }

    #[test]
    fn the_assignment_follows_changes_of_the_tree_and_of_the_slot_attribute() {
        let mut doc = page("<b id=b></b>", "<slot id=sx name=x></slot><slot id=sd></slot>");
        let (sx, sd, b) = (by_id(&doc, "sx"), by_id(&doc, "sd"), by_id(&doc, "b"));
        assert_eq!(doc.assigned_slot(b), Some(sd));
        doc.set_attr(b, "slot", "x");
        assert_eq!(doc.assigned_slot(b), Some(sx));
        let host = by_id(&doc, "h");
        let extra = doc.insert_element(host, None, "i", &[("id", "i")]);
        assert_eq!(doc.assigned_nodes(sd), [extra]);
        doc.detach(extra);
        assert!(doc.assigned_nodes(sd).is_empty());
    }

    #[test]
    fn a_document_without_shadow_roots_keeps_the_plain_tree() {
        let doc = parse("<body><p id=a></p><p id=b></p></body>");
        let body = doc.children(doc.children(doc.root()).next().unwrap()).last().unwrap();
        assert_eq!(ids(&doc, doc.flat_children(body)), ["a", "b"]);
        assert_eq!(doc.flat_next(by_id(&doc, "a")), Some(by_id(&doc, "b")));
        assert_eq!(doc.flat_next(by_id(&doc, "b")), None);
        assert_eq!(doc.flat_parent(by_id(&doc, "a")), Some(body));
    }
}
