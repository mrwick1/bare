//! The nodes of the `::before` and `::after` boxes (CSS Generated Content).
//!
//! Layout has no box tree: it walks the flat tree (`Document::flat_children`). So a `::before` or `::after` box is a node: an element named
//! `::before` or `::after` with one text node inside. The host makes them from the styles before a layout (`bare_css::generated::sync`).
//! - A pseudo node is no child of its element. The element keeps it in `pseudo_of`, like a shadow root in `shadow_of`. So `children`, the
//!   selectors, the serializer and the scripts never see it. `flat_children` gives the `::before` node first and the `::after` node last, and
//!   `flat_parent` of a pseudo node gives its element.
//! - A node that no style shows now stays for the next time (`hide_pseudo`), so a toggle makes no new node.
//! - An element and its pseudo nodes live and die together when the arena gives the nodes of dead trees back (`reclaim.rs`).
//! - A page with none pays one `is_empty` check in `flat_children`, and one in `flat_parent` of a node with no parent.

use super::*;

/// One pseudo node of an element.
#[derive(Clone, Copy, Debug)]
pub(super) struct Gen {
    node: NodeId,
    /// Does a style show it now?
    shown: bool,
}

/// The names of the pseudo nodes, by side.
const NAMES: [&str; 2] = ["::before", "::after"];

impl Document {
    /// Does any element have a pseudo node, shown or not?
    pub fn has_pseudo(&self) -> bool {
        !self.pseudo_of.is_empty()
    }

    /// The node at `index` (`NodeId::index`), for a table of the style code that is keyed by it.
    pub fn node_at(&self, index: usize) -> Option<NodeId> {
        (index < self.nodes.len()).then(|| NodeId::from_index(index))
    }

    /// The element and the side (`0` before, `1` after) of the pseudo node `n`.
    pub fn pseudo_host(&self, n: NodeId) -> Option<(NodeId, u8)> {
        if self.pseudo_hosts.is_empty() {
            return None;
        }
        self.pseudo_hosts.get(&(n.index() as u32)).copied()
    }

    /// `flat_parent` of a node with no parent: the element of a pseudo node.
    #[cold]
    #[inline(never)]
    pub(super) fn pseudo_parent(&self, n: NodeId) -> Option<NodeId> {
        self.pseudo_host(n).map(|h| h.0)
    }

    /// The pseudo node of `el` on side `which` that is shown now.
    pub fn pseudo(&self, el: NodeId, which: u8) -> Option<NodeId> {
        let g = (*self.pseudo_of.get(&(el.index() as u32))?.get(which as usize)?)?;
        g.shown.then_some(g.node)
    }

    /// The shown pseudo nodes: element and side, in no order.
    pub fn shown_pseudo(&self) -> Vec<(NodeId, u8)> {
        let mut out = Vec::new();
        for (&el, pair) in &self.pseudo_of {
            for (which, g) in pair.iter().enumerate() {
                if g.is_some_and(|g| g.shown) {
                    out.push((NodeId::from_index(el as usize), which as u8));
                }
            }
        }
        out
    }

    /// Show the pseudo node of `el` on side `which` with `text`, and give it. The first time makes it. The text changes only when it differs,
    /// with no change record: a pseudo node is not in the DOM.
    pub fn show_pseudo(&mut self, el: NodeId, which: u8, text: &str) -> NodeId {
        let key = el.index() as u32;
        if let Some(g) = self.pseudo_of.get_mut(&key).and_then(|p| p[which as usize].as_mut()) {
            g.shown = true;
            let node = g.node;
            let t = self.nodes[node.index()].first_child.expect("a pseudo node holds a text node");
            if let NodeData::Text(span) = self.nodes[t.index()].data
                && self.text(span) != text
            {
                self.nodes[t.index()].data = NodeData::Text(self.push_text(text));
            }
            return node;
        }
        let node = self.create_element(NAMES[which as usize]);
        let t = self.create_text(text);
        self.nodes[t.index()].parent = Some(node);
        self.nodes[node.index()].first_child = Some(t);
        self.nodes[node.index()].last_child = Some(t);
        self.pseudo_of.entry(key).or_default()[which as usize] = Some(Gen { node, shown: true });
        self.pseudo_hosts.insert(node.index() as u32, (el, which));
        node
    }

    /// Hide the pseudo node of `el` on side `which`. It stays for the next `show_pseudo`.
    pub fn hide_pseudo(&mut self, el: NodeId, which: u8) {
        if let Some(g) = self.pseudo_of.get_mut(&(el.index() as u32)).and_then(|p| p[which as usize].as_mut()) {
            g.shown = false;
        }
    }

    /// `flat_children` of an element with a shown pseudo node: the `::before` node first, the `::after` node last.
    #[cold]
    #[inline(never)]
    pub(super) fn add_pseudo(&self, el: NodeId, out: &mut FlatChildren<'_>) {
        let Some(pair) = self.pseudo_of.get(&(el.index() as u32)) else { return };
        let shown = |w: usize| pair[w].filter(|g| g.shown).map(|g| g.node);
        if let Some(before) = shown(0) {
            out.then = (out.next, out.step);
            out.step = Step::Then;
            out.next = Some(before);
        }
        out.after = shown(1);
    }

    /// `flat_next` with pseudo nodes: `next` is the next node without them. After a `::before` node come the children of its element, and the
    /// `::after` node comes after the last child.
    #[cold]
    #[inline(never)]
    pub(super) fn pseudo_next(&self, id: NodeId, next: Option<NodeId>) -> Option<NodeId> {
        let after = |el: NodeId| self.pseudo(el, 1).filter(|&a| a != id);
        match self.pseudo_host(id) {
            Some((el, 0)) => {
                let first = if self.shadow_of.is_empty() { self.nodes[el.index()].first_child } else { self.shadowed_first(el) };
                first.or_else(|| after(el))
            }
            Some(_) => None,
            None => next.or_else(|| self.flat_parent(id).and_then(after)),
        }
    }

    /// The links between each element and its pseudo nodes, for `reclaim`: they live and die together.
    pub(super) fn pseudo_links(&self) -> impl Iterator<Item = (NodeId, NodeId)> + '_ {
        self.pseudo_hosts.iter().map(|(&n, &(el, _))| (el, NodeId::from_index(n as usize)))
    }
}

/// Forget the pseudo nodes that `gone` names, and those of the elements that it names (`reclaim`).
pub(super) fn retain(of: &mut HashMap<u32, [Option<Gen>; 2], BuildHasherDefault<Fnv>>, hosts: &mut HashMap<u32, (NodeId, u8), BuildHasherDefault<Fnv>>, mut gone: impl FnMut(u32) -> bool) {
    hosts.retain(|&n, &mut (el, _)| !gone(n) && !gone(el.index() as u32));
    of.retain(|_, pair| {
        for g in pair.iter_mut() {
            if g.is_some_and(|g| !hosts.contains_key(&(g.node.index() as u32))) {
                *g = None;
            }
        }
        pair.iter().any(Option::is_some)
    });
}
