//! Give the nodes of dead trees back to the arena.
//!
//! The arena never freed a node. A page that parses a document, reads it and drops it (`DOMParser`, a removed subtree that no script holds)
//! kept every node, attribute and text of it for the life of the page. `Document::reclaim` frees them:
//! - A tree is a node with no parent and everything below it. A tree is live when a node of it is in the list that the host gives (the
//!   nodes that a wrapper, a range, an observer or any other state of the host still names), or the top node is in it, or a shadow root
//!   or a template content that belongs to a live tree. All other trees are dead, and are freed whole.
//! - A freed node goes on a list of free slots. `alloc` reuses a slot before it grows the arena, so the arena is as long as the most nodes that were
//!   alive at one time, not as long as all the nodes that were ever made.
//! - When much of the text and attribute lists was used only by freed nodes, they are built again from the live nodes (one pass,
//!   exact size), and the memory goes back at once.
//!
//! The host must name every `NodeId` that it keeps. A `NodeId` is 4 bytes and has no generation, so one that outlives its node would read the
//! node that took the slot. `set_poison` makes such a mistake a panic in tests: a freed node is never reused and every link of it leaves the arena.

use super::*;

/// Make `Node` slots for a freed node look like nothing: an empty comment that no link names. The start of the span is a value that no text has.
const FREE: NodeData = NodeData::Comment(Span { start: u32::MAX, len: 0 });
/// Poison: the node that a freed node points at. No arena is this long.
const GONE: NodeId = NodeId(NonZeroU32::MAX);

/// Rebuild the text and attribute lists when at least this many bytes were used by freed nodes only, and they are at least a fourth of the lists.
/// The rebuild costs one pass over the slots and a copy of what is live, so a fourth keeps its cost under four times the bytes that it frees.
const REBUILD_BYTES: usize = 256 << 10;
const ATTR_BYTES: usize = std::mem::size_of::<Attr>();

/// A set of slot numbers, one bit each.
struct Keep(Vec<u64>);

impl Keep {
    /// Add `i`. Is it new?
    fn insert(&mut self, i: usize) -> bool {
        let w = &mut self.0[i / 64];
        let new = *w >> (i % 64) & 1 == 0;
        *w |= 1 << (i % 64);
        new
    }

    fn contains(&self, i: usize) -> bool {
        self.0[i / 64] >> (i % 64) & 1 != 0
    }
}

impl Document {
    /// A node slot for `data`: a freed one if there is one, else a new one at the end of the arena. The node has no links.
    /// Inline: the common case is one push, as before the free list existed. `reuse` is out of line: inlined into the tree builder it costs more.
    #[inline(always)]
    pub(super) fn alloc(&mut self, data: NodeData) -> NodeId {
        if self.free.is_empty() {
            let id = NodeId::from_index(self.nodes.len());
            self.nodes.push(Node { parent: None, first_child: None, last_child: None, prev: None, next: None, data });
            return id;
        }
        self.reuse(data)
    }

    #[inline(never)]
    fn reuse(&mut self, data: NodeData) -> NodeId {
        let node = Node { parent: None, first_child: None, last_child: None, prev: None, next: None, data };
        let Some(i) = (if self.poison { None } else { self.free.pop() }) else {
            let id = NodeId::from_index(self.nodes.len());
            self.nodes.push(node);
            return id;
        };
        let id = NodeId::from_index(i as usize);
        // `scan_sheets` read the nodes before `sheet_scanned` once: a style element in a reused slot there would never be seen.
        let sheet = (i as usize) < self.sheet_scanned.get() && matches!(data, NodeData::Element { name, .. } if self.is_sheet_name(name));
        self.nodes[i as usize] = node;
        if sheet {
            self.sheet_made.get_mut().push(id);
        }
        id
    }

    /// Has `reclaim` freed this node (and has no node taken the slot since)?
    pub fn is_free(&self, id: NodeId) -> bool {
        self.nodes[id.index()].data == FREE
    }

    /// Nodes in use: the length of the arena less the free slots. The caps of a host count these, not `len`.
    pub fn live_len(&self) -> usize {
        self.nodes.len() - self.free.len()
    }

    /// Bytes of the text list that a node uses or may use. `text_bytes` less the bytes that only freed nodes used.
    pub fn live_text_bytes(&self) -> usize {
        self.text.len() - self.dead_text
    }

    /// Slots of the attribute list that an element uses or may use. `attr_slots` less the slots that only freed elements used.
    pub fn live_attr_slots(&self) -> usize {
        self.attrs.len() - self.dead_attrs
    }

    /// Bytes that the live nodes, text and attributes of the arena use: the native memory that a collect and a reclaim can release.
    pub fn arena_bytes(&self) -> usize {
        self.live_len() * std::mem::size_of::<Node>() + self.live_text_bytes() + self.live_attr_slots() * ATTR_BYTES
    }

    /// Test mode: a freed node is never reused, and a link of it points outside the arena, so the first use of a stale `NodeId` panics.
    pub fn set_poison(&mut self, on: bool) {
        self.poison = on;
    }

    /// The top of the tree of `id`: climb the parents.
    fn tree_top(&self, mut id: NodeId) -> NodeId {
        while let Some(p) = self.nodes[id.index()].parent {
            id = p;
        }
        id
    }

    /// Free every tree that has no node in `live`. Gives the number of nodes freed.
    ///
    /// `live` is every node that the host still names. The top node, the hover, active and focus nodes, the elements that a host watches an
    /// attribute of, and the shadow roots and template contents of live trees are live too. A host that keeps a `Span` (a value that a
    /// removed attribute had, a detached `Attr`) gives it to `pins` when the lists are rebuilt: `pins` calls the function that it gets
    /// once for each `&mut Span`, and the span reads the same text afterwards. The host forgets what it keeps about a freed node: ask `is_free`.
    ///
    /// The tree of a freed node is not in the tree of the page, so the page does not change. But the styles and the layout of a host keep a table by
    /// node number, and a reused slot would answer with the old node, so a reclaim that frees a node moves `revision` and tells the readers of the
    /// damage log to start over.
    pub fn reclaim(&mut self, live: &[NodeId], pins: &mut dyn FnMut(&mut dyn FnMut(&mut Span))) -> usize {
        // One bit for each slot: the tops of the live trees. A bit costs less than a hash, and the scan below visits every slot anyway.
        let mut keep = Keep(vec![0; self.nodes.len().div_ceil(64)]);
        keep.insert(0);
        let user = [self.user.hover, self.user.active, self.user.focus];
        let watched = self.watched.iter().map(|&(el, _)| NodeId::from_index(el as usize)).collect::<Vec<_>>();
        let removed = self.removed.iter().map(|r| r.el).collect::<Vec<_>>();
        for id in live.iter().copied().chain(user.into_iter().flatten()).chain(watched).chain(removed) {
            keep.insert(self.tree_top(id).index());
        }
        // A host and its shadow root, and a template and its contents, live and die together.
        loop {
            let mut grew = false;
            let links = self.shadow_of.iter().map(|(&h, &r)| (NodeId::from_index(h as usize), r)).chain(self.template_content.iter().map(|(&t, &c)| (NodeId::from_index(t as usize), c)));
            for (a, b) in links.collect::<Vec<_>>() {
                let (a, b) = (self.tree_top(a).index(), self.tree_top(b).index());
                match (keep.contains(a), keep.contains(b)) {
                    (true, false) => grew |= keep.insert(b),
                    (false, true) => grew |= keep.insert(a),
                    _ => {}
                }
            }
            if !grew {
                break;
            }
        }
        let mut freed = 0;
        for i in 1..self.nodes.len() {
            let n = &self.nodes[i];
            if n.parent.is_none() && n.data != FREE && !keep.contains(i) {
                freed += self.free_tree(i);
            }
        }
        if freed == 0 {
            return 0;
        }
        let nodes = &self.nodes;
        let gone = |k: u32| nodes[k as usize].data == FREE;
        self.foreign.retain(|&k, _| !gone(k));
        self.class_sets.get_mut().retain(|&k, _| !gone(k as u32));
        self.template_content.retain(|&k, _| !gone(k));
        self.shadow_of.retain(|&k, _| !gone(k));
        self.shadow_host.retain(|&k, _| !gone(k));
        self.shadow_flags.retain(|&k, _| !gone(k));
        self.custom.retain(|&k, _| !gone(k));
        self.states.retain(|&k, _| !gone(k));
        self.ui.retain(|&k, _| !gone(k));
        self.images.retain(|&k, _| !gone(k));
        self.sheet_made.get_mut().retain(|n| !gone(n.index() as u32));
        self.places = Default::default();
        *self.slots.get_mut() = None;
        self.revision += 1;
        self.mark_full();
        if self.dead_text + self.dead_attrs * ATTR_BYTES >= REBUILD_BYTES && (self.dead_text + self.dead_attrs * ATTR_BYTES) * 4 >= self.text.len() + self.attrs.len() * ATTR_BYTES {
            self.rebuild_lists(pins);
        }
        freed
    }

    /// Free the tree that starts at slot `top`. Gives the number of nodes. A node goes after its children (post-order) and the walk reads the
    /// links of a node before it clears them, so it needs no stack.
    fn free_tree(&mut self, top: usize) -> usize {
        let link = if self.poison { Some(GONE) } else { None };
        let mut i = top;
        let mut count = 0;
        loop {
            while let Some(c) = self.nodes[i].first_child {
                i = c.index();
            }
            loop {
                let Node { parent, next, data, .. } = self.nodes[i];
                match data {
                    NodeData::Text(s) | NodeData::Comment(s) | NodeData::Doctype(s) => self.dead_text += s.len as usize,
                    NodeData::Element { attrs, .. } => {
                        self.dead_attrs += attrs.len as usize;
                        self.dead_text += self.attrs(attrs).iter().map(|a| a.value.len as usize).sum::<usize>();
                    }
                    NodeData::Document | NodeData::ShadowRoot => {}
                }
                self.nodes[i] = Node { parent: link, first_child: link, last_child: link, prev: link, next: link, data: FREE };
                self.free.push(i as u32);
                count += 1;
                if i == top {
                    return count;
                }
                match next {
                    Some(n) => {
                        i = n.index();
                        break;
                    }
                    None => i = parent.map_or(top, |p| p.index()),
                }
            }
        }
    }

    /// Build `text` and `attrs` again from the nodes that are alive. A node keeps its place: only the spans move.
    fn rebuild_lists(&mut self, pins: &mut dyn FnMut(&mut dyn FnMut(&mut Span))) {
        let (mut bytes, mut slots) = (0, 0);
        for n in self.nodes.iter().filter(|n| n.data != FREE) {
            match n.data {
                NodeData::Text(s) | NodeData::Comment(s) | NodeData::Doctype(s) => bytes += s.len as usize,
                NodeData::Element { attrs, .. } => {
                    slots += attrs.len as usize;
                    bytes += self.attrs(attrs).iter().map(|a| a.value.len as usize).sum::<usize>();
                }
                NodeData::Document | NodeData::ShadowRoot => {}
            }
        }
        let old = std::mem::take(&mut self.text);
        let mut text = String::with_capacity(bytes);
        let mut attrs: Vec<Attr> = Vec::with_capacity(slots);
        let copy = |text: &mut String, s: Span| {
            let start = text.len();
            text.push_str(&old[s.start as usize..s.end()]);
            Span { start: to_u32(start), len: s.len }
        };
        for i in 0..self.nodes.len() {
            if self.nodes[i].data == FREE {
                continue;
            }
            let data = match self.nodes[i].data {
                NodeData::Text(s) => NodeData::Text(copy(&mut text, s)),
                NodeData::Comment(s) => NodeData::Comment(copy(&mut text, s)),
                NodeData::Doctype(s) => NodeData::Doctype(copy(&mut text, s)),
                NodeData::Element { name, attrs: list } => {
                    let start = attrs.len();
                    if list.len != 0 {
                        for a in &self.attrs[list.start as usize..(list.start + list.len) as usize] {
                            attrs.push(Attr { name: a.name, value: copy(&mut text, a.value) });
                        }
                    }
                    NodeData::Element { name, attrs: AttrList { start: to_u32(start), len: list.len } }
                }
                d @ (NodeData::Document | NodeData::ShadowRoot) => d,
            };
            self.nodes[i].data = data;
        }
        // What the document keeps: the removed attributes that the host did not take. The damage log is empty: `reclaim` made the readers start over.
        for r in &mut self.removed {
            r.value = copy(&mut text, r.value);
        }
        pins(&mut |s: &mut Span| *s = copy(&mut text, *s));
        text.shrink_to_fit();
        self.text = text;
        self.attrs = attrs;
        self.dead_text = 0;
        self.dead_attrs = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_pins(_: &mut dyn FnMut(&mut Span)) {}

    /// A detached tree like a parsed document: a document node, `<html><body>` and `items` paragraphs with an attribute and a text each.
    fn tree(doc: &mut Document, items: usize, text: &str) -> (NodeId, NodeId) {
        let top = doc.create_document();
        let html = doc.insert_element(top, None, "html", &[("lang", "en")] as &[(&str, &str)]);
        let body = doc.insert_element(html, None, "body", &[] as &[(&str, &str)]);
        let mut last = body;
        for i in 0..items {
            last = doc.insert_element(body, None, "p", &[("id", format!("p{i}"))]);
            doc.append_text(last, text);
        }
        (top, last)
    }

    fn page(doc: &mut Document) -> NodeId {
        let root = doc.root();
        let html = doc.insert_element(root, None, "html", &[] as &[(&str, &str)]);
        let body = doc.insert_element(html, None, "body", &[("class", "page")]);
        let a = doc.insert_element(body, None, "p", &[("id", "keep")]);
        doc.append_text(a, "hello");
        a
    }

    #[test]
    fn a_dead_tree_gives_its_slots_to_new_nodes() {
        let mut doc = Document::new();
        let keep = page(&mut doc);
        let before = doc.len();
        for _ in 0..3 {
            tree(&mut doc, 100, "some text");
            assert_eq!(doc.live_len(), before + 203);
            assert_eq!(doc.reclaim(&[keep], &mut no_pins), 203, "every node of the dead tree is freed");
            assert_eq!(doc.live_len(), before);
            assert_eq!(doc.len(), before + 203, "the arena keeps its length");
        }
        // The arena grew for the first tree only: later trees take the free slots.
        assert_eq!(doc.len(), before + 203);
    }

    #[test]
    fn a_live_tree_and_the_top_node_stay() {
        let mut doc = Document::new();
        let keep = page(&mut doc);
        let (_, leaf) = tree(&mut doc, 5, "x");
        let (dead_top, _) = tree(&mut doc, 5, "y");
        let dump = doc.dump();
        let freed = doc.reclaim(&[keep, leaf], &mut no_pins);
        assert_eq!(freed, 1 + 2 + 5 * 2, "the third tree only");
        assert!(doc.is_free(dead_top) && !doc.is_free(leaf) && !doc.is_free(keep));
        assert_eq!(doc.dump(), dump, "the page is the same");
        assert_eq!(doc.text(match doc.data(doc.children(keep).next().unwrap()) {
            NodeData::Text(s) => *s,
            _ => unreachable!(),
        }), "hello");
        assert_eq!(doc.reclaim(&[keep, leaf], &mut no_pins), 0, "nothing is left to free");
    }

    #[test]
    fn a_reused_slot_starts_clean() {
        let mut doc = Document::new();
        let keep = page(&mut doc);
        let (top, _) = tree(&mut doc, 3, "t");
        let html = doc.children(top).next().unwrap();
        doc.set_custom_state(html, Some(7));
        doc.set_states(html, vec!["a".into()]);
        doc.set_ui(html, UI_CHECKED_DIRTY);
        doc.set_image(html, Some((1, 1, vec![0u32].into())));
        let svg = doc.create_element_ns(Ns::Svg, None, "rect");
        let svg2 = doc.create_element_ns(Ns::Svg, None, "rect");
        let long = doc.insert_element(top, None, "div", &[("class", "a b c d e f g h i j k l m n o p q r s t u v w x y z a1 b1 c1 d1 e1 f1")]);
        assert!(doc.has_class(long, "z"));
        doc.reclaim(&[keep, svg2], &mut no_pins);
        assert!(doc.is_free(svg) && !doc.is_free(svg2));
        assert!(doc.is_foreign(svg2));
        for _ in 0..40 {
            let e = doc.create_element("div");
            assert_eq!(doc.custom_state(e), None, "no custom state");
            assert!(doc.states(e).is_empty());
            assert_eq!(doc.ui(e), 0);
            assert!(doc.image(e).is_none());
            assert!(!doc.is_foreign(e), "an HTML element");
            assert!(!doc.has_class(e, "z"));
        }
        assert!(!doc.has_custom() && !doc.has_images());
    }

    #[test]
    fn the_text_and_attribute_lists_are_built_again_when_most_is_dead() {
        let mut doc = Document::new();
        let keep = page(&mut doc);
        let text = "0123456789".repeat(100);
        for _ in 0..4 {
            tree(&mut doc, 100, &text);
        }
        let (text0, attrs0) = (doc.text_bytes(), doc.attr_slots());
        assert!(text0 > 400_000);
        let dump = doc.dump();
        let freed = doc.reclaim(&[keep], &mut no_pins);
        assert!(freed > 400);
        assert!(doc.text_bytes() < 100, "the text list holds only the page: {}", doc.text_bytes());
        assert!(doc.attr_slots() < 10, "{} of {attrs0} slots", doc.attr_slots());
        assert_eq!((doc.live_text_bytes(), doc.live_attr_slots()), (doc.text_bytes(), doc.attr_slots()));
        assert_eq!(doc.dump(), dump, "the page reads the same");
        let p = doc.children(doc.children(doc.children(doc.root()).next().unwrap()).next().unwrap()).next().unwrap();
        assert_eq!(doc.attr(p, Atom::ID), Some("keep"));
        // A node that is made now reads right, and a freed tree that is made again reads right.
        let (top, last) = tree(&mut doc, 3, "again");
        assert_eq!(doc.attr(last, Atom::ID), Some("p2"));
        assert_eq!(doc.text(match doc.data(doc.children(last).next().unwrap()) { NodeData::Text(s) => *s, _ => unreachable!() }), "again");
        let _ = top;
    }

    #[test]
    fn a_small_amount_of_dead_text_is_not_worth_a_rebuild() {
        let mut doc = Document::new();
        let keep = page(&mut doc);
        tree(&mut doc, 10, "short");
        let before = doc.text_bytes();
        assert!(doc.reclaim(&[keep], &mut no_pins) > 0);
        assert_eq!(doc.text_bytes(), before, "no rebuild");
        assert!(doc.live_text_bytes() < before, "but the dead bytes are not counted");
    }

    #[test]
    fn a_span_that_the_host_holds_reads_the_same_after_a_rebuild() {
        let mut doc = Document::new();
        let keep = page(&mut doc);
        let text = "x".repeat(300_000);
        tree(&mut doc, 1, &text);
        let mut held = doc.new_span("detached attribute value");
        let mut other = doc.attr_span(keep, Atom::ID).unwrap();
        doc.reclaim(&[keep], &mut |remap: &mut dyn FnMut(&mut Span)| {
            remap(&mut held);
            remap(&mut other);
        });
        assert_eq!(doc.text(held), "detached attribute value");
        assert_eq!(doc.text(other), "keep");
        assert!(doc.text_bytes() < 100);
    }

    #[test]
    fn the_damage_log_and_the_removed_attributes_follow_a_rebuild() {
        let mut doc = Document::new();
        let keep = page(&mut doc);
        doc.track_damage(DAMAGE_FRAME);
        doc.set_attr(keep, "title", "first value");
        doc.set_attr(keep, "title", "second value");
        let title = doc.find_atom("title").unwrap();
        doc.watch_attr(keep, title);
        doc.remove_attr(keep, "title");
        tree(&mut doc, 1, &"y".repeat(300_000));
        let log = doc.peek_damage();
        assert!(doc.reclaim(&[keep], &mut no_pins) > 0);
        assert!(doc.text_bytes() < 100, "rebuilt");
        let removed = doc.take_removed_attrs();
        assert_eq!(removed.iter().map(|r| doc.text(r.value)).collect::<Vec<_>>(), ["second value"]);
        // The readers start over: a free node may be named in the log, and a reused slot has a stale style.
        assert_eq!(doc.take_damage(DAMAGE_FRAME), None);
        drop(log);
    }

    #[test]
    fn a_reclaim_that_frees_moves_the_revision_and_one_that_frees_nothing_does_not() {
        let mut doc = Document::new();
        let keep = page(&mut doc);
        tree(&mut doc, 2, "z");
        let rev = doc.revision();
        assert!(doc.reclaim(&[keep], &mut no_pins) > 0);
        assert!(doc.revision() > rev, "styles keyed by revision are made again");
        assert_eq!(doc.reclaim(&[keep], &mut no_pins), 0);
        let rev = doc.revision();
        doc.reclaim(&[keep], &mut no_pins);
        assert_eq!(doc.revision(), rev, "a reclaim that frees nothing changes nothing");
    }

    #[test]
    fn a_shadow_root_and_its_host_live_and_die_together() {
        let mut doc = Document::new();
        let keep = page(&mut doc);
        let (_, leaf) = tree(&mut doc, 2, "a");
        let root = doc.attach_shadow(leaf, true);
        let inner = doc.insert_element(root, None, "span", &[] as &[(&str, &str)]);
        // The wrapper of the shadow root alone keeps the host tree.
        assert_eq!(doc.reclaim(&[keep, root], &mut no_pins), 0);
        assert!(!doc.is_free(leaf) && !doc.is_free(inner));
        // The wrapper of the host alone keeps the shadow tree.
        assert_eq!(doc.reclaim(&[keep, leaf], &mut no_pins), 0);
        assert!(!doc.is_free(inner));
        assert_eq!(doc.shadow_root(leaf), Some(root));
        // Nothing holds them: both go, and the host forgets its root.
        let n = doc.reclaim(&[keep], &mut no_pins);
        assert!(n > 3 && doc.is_free(inner) && doc.is_free(root) && doc.is_free(leaf));
        assert!(!doc.has_shadow_roots());
    }

    #[test]
    fn the_contents_of_a_template_live_and_die_with_the_template() {
        let mut doc = Document::new();
        let keep = page(&mut doc);
        let tpl = doc.create_element("template");
        let content = doc.make_template_content(tpl);
        let li = doc.insert_element(content, None, "li", &[] as &[(&str, &str)]);
        assert_eq!(doc.reclaim(&[keep, li], &mut no_pins), 0, "a node of the contents keeps the template");
        assert_eq!(doc.template_content(tpl), Some(content));
        assert_eq!(doc.reclaim(&[keep, tpl], &mut no_pins), 0);
        assert_eq!(doc.reclaim(&[keep], &mut no_pins), 3);
        assert!(!doc.has_template_content());
    }

    #[test]
    fn the_user_state_and_a_watched_attribute_keep_their_tree() {
        let mut doc = Document::new();
        let keep = page(&mut doc);
        let (_, a) = tree(&mut doc, 1, "a");
        let (_, b) = tree(&mut doc, 1, "b");
        let (_, c) = tree(&mut doc, 1, "c");
        doc.set_hover(Some(a));
        doc.set_focus(Some(b), false);
        doc.watch_attr(c, Atom::ID);
        assert_eq!(doc.reclaim(&[keep], &mut no_pins), 0, "all three trees are named");
        doc.set_hover(None);
        doc.set_focus(None, false);
        doc.unwatch_attr(c, Atom::ID);
        assert_eq!(doc.reclaim(&[keep], &mut no_pins), 3 * 5);
    }

    #[test]
    fn the_sheet_list_stays_exact_when_a_freed_slot_becomes_a_style_element() {
        let mut doc = Document::new();
        let keep = page(&mut doc);
        let root = doc.root();
        let body = doc.parent(keep).unwrap();
        doc.insert_element(body, None, "style", &[] as &[(&str, &str)]);
        let top = doc.create_document();
        for _ in 0..4 {
            doc.insert_element(top, None, "link", &[("rel", "stylesheet")]);
        }
        assert_eq!(doc.sheet_nodes().len(), 1, "the scan has read every slot");
        doc.reclaim(&[keep], &mut no_pins);
        assert_eq!(doc.sheet_nodes().len(), 1);
        // New style elements land in the freed slots, which the scan read before.
        for _ in 0..4 {
            doc.insert_element(root, None, "style", &[] as &[(&str, &str)]);
        }
        assert_eq!(doc.sheet_nodes().len(), 5);
        let loose = doc.create_element("link");
        doc.move_to(loose, body, None);
        assert_eq!(doc.sheet_nodes().len(), 6);
    }

    #[test]
    #[should_panic]
    fn a_stale_node_panics_in_poison_mode() {
        let mut doc = Document::new();
        doc.set_poison(true);
        let keep = page(&mut doc);
        let (top, _) = tree(&mut doc, 2, "a");
        doc.reclaim(&[keep], &mut no_pins);
        assert!(doc.is_free(top));
        let _ = doc.children(top).count();
    }

    #[test]
    fn poison_mode_never_reuses_a_slot() {
        let mut doc = Document::new();
        doc.set_poison(true);
        let keep = page(&mut doc);
        tree(&mut doc, 2, "a");
        let n = doc.len();
        doc.reclaim(&[keep], &mut no_pins);
        let e = doc.create_element("div");
        assert_eq!(e.index(), n, "a new slot");
        assert_eq!(doc.live_len(), doc.len() - 7, "the poisoned slots are not live");
    }

    /// Random trees, random live sets, random edits: the page that is live reads the same before and after, and the arena does not grow past the
    /// most nodes that were alive at one time plus what a round makes.
    #[test]
    fn random_reclaims_keep_what_is_live() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let mut doc = Document::new();
        let keep = page(&mut doc);
        let mut held: Vec<(NodeId, String)> = Vec::new();
        let mut high = 0;
        for round in 0..60 {
            for _ in 0..next(4) {
                let (top, leaf) = tree(&mut doc, 1 + next(40), &"w".repeat(next(30_000)));
                if next(3) == 0 {
                    doc.set_attr(leaf, "title", &format!("t{round}"));
                    held.push((leaf, doc.dump_node_for_test(top)));
                }
            }
            if next(2) == 0 && !held.is_empty() {
                held.remove(next(held.len()));
            }
            let live: Vec<NodeId> = held.iter().map(|h| h.0).chain([keep]).collect();
            doc.reclaim(&live, &mut no_pins);
            for (leaf, dump) in &held {
                assert!(!doc.is_free(*leaf));
                let top = doc.tree_top(*leaf);
                assert_eq!(&doc.dump_node_for_test(top), dump, "round {round}");
            }
            high = high.max(doc.live_len());
            assert!(doc.len() <= high + 4 * 82, "round {round}: {} slots, {high} live at most", doc.len());
        }
    }

    impl Document {
        fn dump_node_for_test(&self, id: NodeId) -> String {
            let mut out = String::new();
            self.dump_node(id, 0, false, &mut out);
            out
        }
    }
}
