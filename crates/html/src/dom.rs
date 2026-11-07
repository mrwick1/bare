//! The DOM: the tree of nodes that a page becomes.
//!
//! `<p>Hi <b>you</b></p>` is a tree. `p` has two children: the text "Hi " and the element `b`.
//! `b` has one child: the text "you". CSS, layout and JavaScript all work on this tree.
//!
//! How do we store a tree in Rust? There are three options:
//! 1. Each node owns its children (`Vec<Node>` inside `Node`). Simple, but a child cannot point
//!    to its parent. The DOM needs that: `element.parentNode` in JS, and CSS rules like `div > p`.
//! 2. `Rc<RefCell<Node>>`, with a parent pointer in each child. It works, but the borrow rules are
//!    checked while the program runs, so a mistake is a crash. A parent and a child that point at
//!    each other are never freed (a leak), unless we use weak pointers too.
//! 3. An arena: one `Vec` holds all the nodes. A node refers to another by its place in the `Vec`,
//!    a plain number. This is our choice. Numbers are cheap to copy. There is no leak, and no
//!    run-time borrow checks. To delete a whole tree, drop the `Vec`.
//!    The cost: a `NodeId` has no meaning without its `Document`. Use it with another document and
//!    you get the wrong node, or a crash. Servo uses a different design (garbage collected nodes),
//!    because JavaScript can hold on to nodes.
//!
//! One more benefit: a tab owns one `Document`, so the memory of a tab's DOM is the size of
//! a few `Vec`s. Module 10 counts the memory for each tab.
//!
//! The nodes are small, because every byte counts when a page has 100,000 elements:
//! - A `NodeId` is 4 bytes (a `u32`), not 8. It is never zero, so `Option<NodeId>` is 4 bytes too.
//! - A node does not own a `Vec` of children. It holds its first and last child, and each child
//!   holds the next and the previous child. This is a linked list in the arena. It saves a heap
//!   allocation for each node. Adding at the end, or in front of a known node, is O(1). The
//!   cost: to reach child number 5, you walk the list. CSS and layout walk all the children
//!   in order, so for us this is no cost. Chrome stores children the same way.
//! - Names of elements and attributes (`div`, `class`) are stored once, in a table of the
//!   `Document`. A node holds an `Atom`: the number of its name in the table. A page has
//!   100,000 `div` and the word is in memory once. Comparing two names is comparing two numbers.
//! - Texts, attribute values, comments and doctypes are not `String`s. They all live in one big
//!   `String` of the `Document`, and a node holds a `Span`: where its text starts, and how long it is.
//!   A `String` for each small text costs 24 bytes for the header, and a heap allocation of at least
//!   32 bytes for a text of one letter. A `Span` is 8 bytes. All the attributes of all the elements
//!   also live in one `Vec`, and an element holds the range of its own. So a node needs no
//!   allocation of its own at all. The cost: a text cannot be changed in the middle. When we add
//!   to a text node that is not the last text in the buffer, we copy it to the end, and the old
//!   bytes are wasted. For a page that is read once, this almost never happens. JavaScript will
//!   change the DOM later, and then we may need to clean up the buffer.
//!
//! Comments compare Rust with JavaScript, because you know JS.

use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::num::NonZeroU32;

/// The number of a node. It is the place of the node in the `Vec` of its `Document`, plus one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
// `Copy` means that passing a `NodeId` around copies it, like a number.
pub struct NodeId(NonZeroU32);
// JS: like a number, but the type stops us from mixing it up with other numbers.
// `NonZeroU32` is a number that is never 0. Rust uses the free "0" to mean `None`, so an
// `Option<NodeId>` needs no extra space. The inner number has no `pub`, so other crates cannot
// make a `NodeId` of their own.

impl NodeId {
    /// The place in the `Vec` of the document, from 0. A `Vec` with one item for each node, like the style table, uses it.
    pub fn index(self) -> usize {
        self.0.get() as usize - 1
    }

    fn from_index(index: usize) -> NodeId {
        NodeId(NonZeroU32::new(u32::try_from(index + 1).expect("more than 4 billion nodes")).unwrap())
    }
}

/// The number of a name in the name table of a `Document`. Two atoms of one document are equal
/// when the names are equal. Atoms of different documents mean nothing together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Atom(u32);

impl Atom {
    /// The place of the name in the name table. Style code uses it to make hashes.
    pub fn index(self) -> u32 {
        self.0
    }

    /// The atom of `id`. Every document has it from the start, so the style code can find an `id`
    /// attribute with one number compare and no table lookup.
    pub const ID: Atom = Atom(0);
    /// The atom of `class`. Same reason as `ID`.
    pub const CLASS: Atom = Atom(1);
}

/// A piece of text in the text buffer of a `Document`: where it starts, and how long it is, in bytes.
/// Use `Document::text` to read it. A span of one document means nothing in another document.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    start: u32,
    len: u32,
    // 32 bits are enough for 4 GB of text in one page.
}

impl Span {
    fn end(self) -> usize {
        self.start as usize + self.len as usize
    }
}

/// One attribute of an element: `class="a b"`. Use `Document::text` to read the value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Attr {
    pub name: Atom,
    pub value: Span,
}

/// The attributes of one element: a range in the attribute list of the `Document`.
/// Use `Document::attrs` to read them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttrList {
    start: u32,
    len: u32,
}

/// A namespace that the document knows. An element is in `Html`, `Svg` or `MathMl`. An attribute is in no namespace,
/// or in `Xml`, `Xmlns` or `Xlink`. These are the only ones that the parser makes and that scripts may ask for. A
/// namespace outside this list is refused by the host with a clear error; it is never stored as a lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ns {
    Html,
    Svg,
    MathMl,
    Xml,
    Xmlns,
    Xlink,
}

impl Ns {
    /// The namespace URI, as `namespaceURI` gives it.
    pub fn uri(self) -> &'static str {
        match self {
            Ns::Html => "http://www.w3.org/1999/xhtml",
            Ns::Svg => "http://www.w3.org/2000/svg",
            Ns::MathMl => "http://www.w3.org/1998/Math/MathML",
            Ns::Xml => "http://www.w3.org/XML/1998/namespace",
            Ns::Xmlns => "http://www.w3.org/2000/xmlns/",
            Ns::Xlink => "http://www.w3.org/1999/xlink",
        }
    }

    /// The namespace of a URI, if it is one of ours.
    pub fn from_uri(uri: &str) -> Option<Ns> {
        [Ns::Html, Ns::Svg, Ns::MathMl, Ns::Xml, Ns::Xmlns, Ns::Xlink].into_iter().find(|n| n.uri() == uri)
    }
}

/// The namespace identity of an element that is not in the HTML namespace. See `Document::element_ns`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Foreign {
    ns: Ns,
    prefix: Option<Atom>,
}

/// What a node is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NodeData {
    // `Copy`: every variant is small, and holds no `String`. Copying a `NodeData` copies 16 bytes.
    /// The top of the tree. It has no parent. It holds the doctype and the `html` element.
    Document,
    /// `<!DOCTYPE html>`. The name is in lowercase. Use `Document::text` to read it.
    Doctype(Span),
    /// An element like `<p class="a">`. The name is in lowercase for HTML. For an SVG or MathML element it is the local
    /// name with its own case (`foreignObject`). Use `Document::atom_str` to read the name and `Document::attrs` to read
    /// the attributes. The namespace is not here: see `Document::element_ns`.
    Element { name: Atom, attrs: AttrList },
    /// A piece of text. Use `Document::text` to read it.
    Text(Span),
    /// `<!-- text -->`. Use `Document::text` to read it.
    Comment(Span),
    /// The top of a shadow tree (DOM Standard, ShadowRoot), or a plain `DocumentFragment` (the contents of a `template`, `Document::create_fragment`: no host).
    /// It has no parent link and is never a child: the nodes of its tree have it as their top. A shadow root has its host and mode in sparse tables
    /// (`Document::shadow_host`), so a page without shadow roots pays nothing. The tree builder makes one for each `template` that has children.
    ShadowRoot,
}

struct Node {
    // No `pub`: only this file can change a node. This keeps the links in step.
    parent: Option<NodeId>,
    // `None` for the document at the top. JS: `parentNode` is `null` there.
    first_child: Option<NodeId>,
    last_child: Option<NodeId>,
    prev: Option<NodeId>,
    next: Option<NodeId>,
    // The sibling before and after this node. JS: `previousSibling` and `nextSibling`.
    data: NodeData,
}

/// A whole tree. It owns all of its nodes.
pub struct Document {
    nodes: Vec<Node>,
    // The arena. The first node is the `Document` node, and every other node comes after it.
    names: Vec<Box<str>>,
    // The name table: the text for each atom.
    atoms: HashMap<Box<str>, Atom, BuildHasherDefault<Fnv>>,
    // The way back: from a name to its atom. A page has few different names, so the second copy is small.
    attrs: Vec<Attr>,
    // The attributes of all elements. Each element has a run of them, one after the other.
    text: String,
    // All the text of the page: texts, attribute values, comments and doctypes. See `Span`.
    class_sets: RefCell<HashMap<usize, HashSet<Box<str>, BuildHasherDefault<Fnv>>, BuildHasherDefault<Fnv>>>,
    // The classes of the elements that have a long `class` attribute, as a set. See `has_class`.
    // The attributes of an element never change, so this needs no clean-up.
    places: [OnceCell<Vec<(u32, u32)>>; 2],
    // The place of each element among its sibling elements, from the start and from the end. Made the
    // first time that `sibling_place` asks, and dropped when the tree changes. `[0]` counts all
    // elements, `[1]` counts the elements with the same name. See `sibling_place`.
    foreign: HashMap<u32, Foreign, BuildHasherDefault<Fnv>>,
    // The SVG and MathML elements, by `NodeId::index`. An element that is not here is in the HTML namespace, so a page
    // without SVG pays nothing. The name atom of a foreign element is its local name, shared with the HTML atoms, so a
    // type selector `rect` matches an SVG `rect`. The namespace stays with the node when it moves, and `clone_element` copies it.
    ns_atoms: HashMap<(Ns, Box<str>), Atom, BuildHasherDefault<Fnv>>,
    // The attributes that are in a namespace (`xlink:href`). Their atom is for the pair (namespace, qualified name), and the
    // name table holds the qualified name. The same qualified name with no namespace is another atom (see `intern`).
    atom_ns: HashMap<u32, Ns, BuildHasherDefault<Fnv>>,
    // The way back: from the atom of a namespaced attribute to its namespace.
    watched: HashSet<(u32, u32), BuildHasherDefault<Fnv>>,
    // The attributes (element index, atom) that a host holds an `Attr` object for. See `watch_attr`.
    removed: Vec<RemovedAttr>,
    // The watched attributes that were removed since the host last took the list. See `take_removed_attrs`.
    revision: u64,
    // Moves at every change that can alter a tree query: a node joins or leaves the tree, an attribute is set or removed, an
    // element gets a namespace. See `revision`.
    text_revision: u64,
    // Moves at every edit of the text of an existing node. See `text_revision`.
    shadow_of: HashMap<u32, NodeId, BuildHasherDefault<Fnv>>,
    // The shadow root of each shadow host, by `NodeId::index` of the host. The root is not a child of the host: the light links of the host never show it.
    shadow_host: HashMap<u32, (NodeId, bool), BuildHasherDefault<Fnv>>,
    // The way back: the host of each shadow root, and whether its mode is open. A host that is not in `shadow_of` has no root.
    template_content: HashMap<u32, NodeId, BuildHasherDefault<Fnv>>,
    // The contents of each HTML `template` that has any, by `NodeId::index` of the template: a fragment node that is not a child of the template.
    watcher: Option<Watcher>,
    // The host function that is told of each change (see `set_watcher`). A page with no watcher pays one `Option` check at a change.
}

/// A change of the tree that `Document::set_watcher` reports, before the document answers a later read. `prev` and `next` are the
/// siblings of `node` at that moment (for a node that left, the siblings it had).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Change {
    /// `node` was put in the children of `parent`.
    Link { parent: NodeId, node: NodeId, prev: Option<NodeId>, next: Option<NodeId> },
    /// `node` is about to leave the children of `parent`.
    Unlink { parent: NodeId, node: NodeId, prev: Option<NodeId>, next: Option<NodeId> },
    /// The attribute `name` of `el` is set or removed. `old` is the value before, `None` when there was none. The watcher runs
    /// while the change is in progress: it must not read the attribute, only use `old`.
    Attr { el: NodeId, name: Atom, old: Option<Span> },
    /// The text of `node` changes. `old` is the value it had.
    Text { node: NodeId, old: Span },
}

/// A host function for `Document::set_watcher`. It gets the document as it is at the call and must not run script.
pub type Watcher = Box<dyn FnMut(&Document, Change)>;

/// A watched attribute that was removed: the element, the name and the value it had. The value stays readable
/// with `Document::text`, because the text buffer never frees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RemovedAttr {
    pub el: NodeId,
    pub name: Atom,
    pub value: Span,
}

/// A fast hash for short names (FNV-1a). The standard one (SipHash) is slower, and it protects against
/// a server that picks keys to make the table slow. Here the "server" is the page, and it can only
/// slow down its own tab.
pub struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
}

/// The children of a node, in order. JS: like walking `firstChild`, then `nextSibling`.
pub struct Children<'a> {
    doc: &'a Document,
    next: Option<NodeId>,
}

impl Iterator for Children<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let id = self.next?;
        self.next = self.doc.nodes[id.index()].next;
        Some(id)
    }
}

impl Document {
    /// A new tree with only the top node.
    pub fn new() -> Document {
        let mut doc = Document {
            nodes: vec![Node { parent: None, first_child: None, last_child: None, prev: None, next: None, data: NodeData::Document }],
            names: Vec::new(),
            atoms: HashMap::default(),
            attrs: Vec::new(),
            text: String::new(),
            places: Default::default(),
            class_sets: Default::default(),
            foreign: HashMap::default(),
            ns_atoms: HashMap::default(),
            atom_ns: HashMap::default(),
            watched: HashSet::default(),
            removed: Vec::new(),
            revision: 0,
            text_revision: 0,
            shadow_of: HashMap::default(),
            shadow_host: HashMap::default(),
            template_content: HashMap::default(),
            watcher: None,
        };
        // The order matters: it must match `Atom::ID` and `Atom::CLASS`.
        doc.intern("id");
        doc.intern("class");
        doc
    }

    /// A number that moves at every public change that can alter the answer of a tree query: a node is linked into or
    /// detached from the tree (the parser, `insert_*`, `move_to`, `move_children`, `detach`), an attribute is set or removed
    /// (any name, any value), or an element gets a namespace. A host that caches the nodes of a query keeps the number from
    /// when it built the list, and builds it again when the number is not the same. Changes that cannot alter a query leave it
    /// alone: a new node with no parent, a text edit, a new name, a new span. It only grows, and it is not a count of changes.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Move `revision` for a change that the host keeps outside the tree, so a host cache of a query that reads it (the selected
    /// options of a select) is built again.
    pub fn touch(&mut self) {
        self.revision += 1;
    }

    /// A number that moves at every edit of the text of a node that exists (`set_text`, text that joins a text node). With
    /// `revision`, a host can tell that nothing that layout reads has changed. It only grows.
    pub fn text_revision(&self) -> u64 {
        self.text_revision
    }

    /// The top node.
    pub fn root(&self) -> NodeId {
        NodeId::from_index(0)
    }

    /// What this node is.
    pub fn data(&self, id: NodeId) -> &NodeData {
        &self.nodes[id.index()].data
        // JS: `nodes[id].data`. This crashes if `id` comes from another document. See the top of this file.
    }

    /// The parent of this node. `None` for the top node.
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].parent
    }

    /// The shadow root of the host `host`, if it has one (open or closed).
    #[inline]
    pub fn shadow_root(&self, host: NodeId) -> Option<NodeId> {
        // Most pages have no shadow root: skip the hash.
        if self.shadow_of.is_empty() {
            return None;
        }
        self.shadow_of.get(&(host.index() as u32)).copied()
    }

    /// The host of the shadow root `root`. `None` for any other node.
    pub fn shadow_host(&self, root: NodeId) -> Option<NodeId> {
        self.shadow_host.get(&(root.index() as u32)).map(|&(h, _)| h)
    }

    /// Is the shadow root `root` in open mode? `false` for a closed root and for any other node.
    pub fn shadow_is_open(&self, root: NodeId) -> bool {
        self.shadow_host.get(&(root.index() as u32)).is_some_and(|&(_, open)| open)
    }

    /// A new fragment node: the top of a tree with no host (JS: `document.createDocumentFragment()`).
    pub fn create_fragment(&mut self) -> NodeId {
        self.new_node(NodeData::ShadowRoot)
    }

    /// True when some `template` of this document has contents.
    #[inline(never)]
    pub fn has_template_content(&self) -> bool {
        !self.template_content.is_empty()
    }

    /// The contents of the HTML `template` `tpl`, if it has any yet. A page with no template contents pays one `is_empty` check.
    #[inline]
    pub fn template_content(&self, tpl: NodeId) -> Option<NodeId> {
        if self.template_content.is_empty() {
            return None;
        }
        self.template_content.get(&(tpl.index() as u32)).copied()
    }

    /// The contents of the HTML `template` `tpl`; an empty fragment is made if it has none. The caller checked that `tpl` is an HTML `template`.
    pub fn make_template_content(&mut self, tpl: NodeId) -> NodeId {
        if let Some(c) = self.template_content(tpl) {
            return c;
        }
        let c = self.create_fragment();
        self.template_content.insert(tpl.index() as u32, c);
        c
    }

    /// The parent of this node in the shadow-including tree: its parent, or the host when this node is a shadow root. JS: the step of `getRootNode({composed: true})`.
    pub fn parent_or_host(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].parent.or_else(|| self.shadow_host(id))
    }

    /// Give the element `host` a new, empty shadow root. The caller checked that `host` can be a host and has no root yet. The root is not in the light
    /// tree of the host. Gives the root.
    pub fn attach_shadow(&mut self, host: NodeId, open: bool) -> NodeId {
        debug_assert!(matches!(self.nodes[host.index()].data, NodeData::Element { .. }) && !self.shadow_of.contains_key(&(host.index() as u32)));
        let root = self.new_node(NodeData::ShadowRoot);
        self.shadow_of.insert(host.index() as u32, root);
        self.shadow_host.insert(root.index() as u32, (host, open));
        root
    }

    /// The parent of this node in the flat tree, the tree that is rendered (Blink `FlatTreeTraversal::Parent`). A child of a shadow root has
    /// the host as its parent. A light child of a host has no parent: slot assignment is not built, so no slot takes it, and it is not
    /// rendered. A page with no shadow root pays one `is_empty` check.
    #[inline(never)]
    pub fn flat_parent(&self, id: NodeId) -> Option<NodeId> {
        let parent = self.parent(id)?;
        if self.shadow_of.is_empty() {
            return Some(parent);
        }
        if let Some(host) = self.shadow_host(parent) {
            return Some(host);
        }
        if self.shadow_root(parent).is_some() { None } else { Some(parent) }
    }

    /// The children of this node in the flat tree: the children of its shadow root if it is a host, else its own children. Siblings are the
    /// same in both trees, so `next_sibling` walks on from any of them.
    #[inline]
    pub fn flat_children(&self, id: NodeId) -> Children<'_> {
        if self.shadow_of.is_empty() {
            return self.children(id);
        }
        self.children(self.shadow_root(id).unwrap_or(id))
    }

    /// The children of this node, in order.
    pub fn children(&self, id: NodeId) -> Children<'_> {
        Children { doc: self, next: self.nodes[id.index()].first_child }
    }

    /// Does the element `el` have `class` in its `class` attribute? A short attribute is read word by word.
    /// A long one (128 bytes or more) becomes a set the first time,
    /// so that 5,000 rules on one element do not read the 5,000 words 5,000 times.
    #[inline]
    pub fn has_class(&self, el: NodeId, class: &str) -> bool {
        let Some(value) = self.attr(el, Atom::CLASS) else { return false };
        if value.len() < 128 {
            return value.split_ascii_whitespace().any(|c| c == class);
        }
        self.class_sets.borrow_mut().entry(el.index()).or_insert_with(|| value.split_ascii_whitespace().map(Box::from).collect()).contains(class)
    }

    /// The place of the element `el` among its sibling elements: 1 for the first. It counts from the end with
    /// `from_end`, and counts only the elements with the same name with `of_type`. This is what `:nth-child`
    /// and `:nth-of-type` ask. All the places are made in one pass the first time, so each answer is a look-up.
    /// (Counting the siblings for each element is slow for a parent with 50,000 children.)
    pub fn sibling_place(&self, el: NodeId, of_type: bool, from_end: bool) -> u32 {
        let places = self.places[of_type as usize].get_or_init(|| self.make_places(of_type));
        let (start, end) = places[el.index()];
        if from_end { end } else { start }
    }

    fn make_places(&self, of_type: bool) -> Vec<(u32, u32)> {
        let mut places = vec![(0, 0); self.nodes.len()];
        let mut seen: HashMap<Atom, u32, BuildHasherDefault<Fnv>> = HashMap::default();
        for parent in 0..self.nodes.len() {
            if self.nodes[parent].first_child.is_none() {
                continue;
            }
            let parent = NodeId::from_index(parent);
            seen.clear();
            let mut total = 0;
            for child in self.children(parent) {
                if let NodeData::Element { name, .. } = self.nodes[child.index()].data {
                    let count = if of_type { seen.entry(name).or_insert(0) } else { &mut total };
                    *count += 1;
                    places[child.index()].0 = *count;
                }
            }
            // The place from the end is the count of the group, less the place from the start, plus one.
            for child in self.children(parent) {
                if let NodeData::Element { name, .. } = self.nodes[child.index()].data {
                    let count = if of_type { seen[&name] } else { total };
                    places[child.index()].1 = count - places[child.index()].0 + 1;
                }
            }
        }
        places
    }

    /// The number of different names (tags and attributes) in the name table. It only grows.
    pub fn name_count(&self) -> usize {
        self.names.len()
    }

    /// The number of nodes, with the top node.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Always false: a document has its top node.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The last child of this node. JS: `lastChild`.
    pub fn last_child(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].last_child
    }

    /// The atom for a name, if the document has it. It does not add the name.
    pub fn find_atom(&self, name: &str) -> Option<Atom> {
        self.atoms.get(name).copied()
    }

    /// The sibling before this node. JS: `previousSibling`.
    pub fn prev_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].prev
    }

    /// The sibling after this node. JS: `nextSibling`.
    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].next
    }

    /// The value of an attribute of an element. `None` if there is none, or if this is not an element.
    /// With a repeated name, the first one wins (the tree builder never stores a repeat).
    pub fn attr(&self, id: NodeId, name: Atom) -> Option<&str> {
        let NodeData::Element { attrs, .. } = self.nodes[id.index()].data else { return None };
        self.attrs(attrs).iter().find(|a| a.name == name).map(|a| self.text(a.value))
    }

    /// The text of a `Span`.
    pub fn text(&self, span: Span) -> &str {
        &self.text[span.start as usize..span.end()]
    }

    /// The attributes of an element, in the order of the page.
    pub fn attrs(&self, list: AttrList) -> &[Attr] {
        // An empty list can start after the end of the list of all attributes (a removal shortened it). It has nothing to read.
        if list.len == 0 {
            return &[];
        }
        &self.attrs[list.start as usize..(list.start + list.len) as usize]
    }

    /// The atom for a name. A name that is new goes in the table.
    pub fn intern(&mut self, name: &str) -> Atom {
        if let Some(&atom) = self.atoms.get(name) {
            return atom;
        }
        let atom = Atom(u32::try_from(self.names.len()).expect("more than 4 billion names"));
        self.names.push(name.into());
        self.atoms.insert(name.into(), atom);
        atom
    }

    /// The text of a name.
    pub fn atom_str(&self, atom: Atom) -> &str {
        &self.names[atom.0 as usize]
    }

    /// Add a new node as a child of `parent`, just before `before`. With `None`, add it at the end.
    /// `before` must be a child of `parent`. If it is not, the node goes at the end.
    /// Gives the new node. The tree builder needs this to put text before a table.
    fn insert(&mut self, parent: NodeId, before: Option<NodeId>, data: NodeData) -> NodeId {
        let id = NodeId::from_index(self.nodes.len());
        // The new node goes at the end of the `Vec`, so its number is the length now.
        self.nodes.push(Node { parent: None, first_child: None, last_child: None, prev: None, next: None, data });
        self.link(id, parent, before);
        id
    }

    /// Put `node`, which has no parent, in the list of children of `parent`, just before `before` (or at the end).
    fn link(&mut self, node: NodeId, parent: NodeId, before: Option<NodeId>) {
        self.places = Default::default();
        self.revision += 1;
        let before = before.filter(|&b| b != node && self.parent(b) == Some(parent));
        let prev = self.child_before(parent, before);
        let n = &mut self.nodes[node.index()];
        (n.parent, n.prev, n.next) = (Some(parent), prev, before);
        if self.watcher.is_some() {
            self.notify(Change::Link { parent, node, prev, next: before });
        }
        // Fix the links of the neighbours. JS: like inserting into a doubly linked list.
        match prev {
            Some(p) => self.nodes[p.index()].next = Some(node),
            None => self.nodes[parent.index()].first_child = Some(node),
        }
        match before {
            Some(b) => self.nodes[b.index()].prev = Some(node),
            None => self.nodes[parent.index()].last_child = Some(node),
        }
    }

    /// Take `node` out of its parent. Its children go with it. Does nothing for a node that has no parent.
    /// JS: like `node.remove()`.
    pub fn detach(&mut self, node: NodeId) {
        let Node { parent, prev, next, .. } = self.nodes[node.index()];
        let Some(parent) = parent else { return };
        if self.watcher.is_some() {
            self.notify(Change::Unlink { parent, node, prev, next });
        }
        self.places = Default::default();
        self.revision += 1;
        match prev {
            Some(p) => self.nodes[p.index()].next = next,
            None => self.nodes[parent.index()].first_child = next,
        }
        match next {
            Some(n) => self.nodes[n.index()].prev = prev,
            None => self.nodes[parent.index()].last_child = prev,
        }
        let n = &mut self.nodes[node.index()];
        (n.parent, n.prev, n.next) = (None, None, None);
    }

    /// Move `node` (with its children) to be a child of `parent`, just before `before`, or at the end for `None`.
    /// `parent` must not be `node` or inside it. The tree builder never does that. JS: like `parent.insertBefore(node, before)`.
    pub fn move_to(&mut self, node: NodeId, parent: NodeId, before: Option<NodeId>) {
        self.detach(node);
        self.link(node, parent, before);
    }

    /// Move all the children of `from` to the end of `to`, in the same order. JS: `to.append(...from.childNodes)`.
    pub fn move_children(&mut self, from: NodeId, to: NodeId) {
        while let Some(child) = self.nodes[from.index()].first_child {
            self.move_to(child, to, None);
        }
    }

    /// A new element with the same name and attributes as `node`, with no parent and no children. JS: `node.cloneNode(false)`.
    /// The values of the attributes are shared with `node`, not copied.
    pub fn clone_element(&mut self, node: NodeId) -> NodeId {
        let NodeData::Element { name, attrs } = self.nodes[node.index()].data else { panic!("clone_element: not an element") };
        let start = self.attrs.len();
        if attrs.len != 0 {
            self.attrs.extend_from_within(attrs.start as usize..(attrs.start + attrs.len) as usize);
        }
        let id = NodeId::from_index(self.nodes.len());
        let data = NodeData::Element { name, attrs: AttrList { start: to_u32(start), len: attrs.len } };
        self.places = Default::default();
        self.nodes.push(Node { parent: None, first_child: None, last_child: None, prev: None, next: None, data });
        if let Some(&f) = self.foreign.get(&(node.index() as u32)) {
            self.foreign.insert(id.index() as u32, f);
        }
        id
    }

    /// Add an element, with its name and attributes as text. The names go in the name table, and
    /// the values in the text buffer. `before` works as in `insert`; `None` adds it at the end.
    pub fn insert_element<K: AsRef<str>, V: AsRef<str>>(&mut self, parent: NodeId, before: Option<NodeId>, name: &str, attrs: &[(K, V)]) -> NodeId {
        let name = self.intern(name);
        let start = self.attrs.len();
        for (key, value) in attrs {
            let attr = Attr { name: self.intern(key.as_ref()), value: self.push_text(value.as_ref()) };
            self.attrs.push(attr);
        }
        let attrs = AttrList { start: to_u32(start), len: to_u32(attrs.len()) };
        self.insert(parent, before, NodeData::Element { name, attrs })
    }

    /// A new element with no parent and no attributes. JS: `document.createElement(name)`. The caller
    /// lowercases and checks the name. Put it in the tree with `move_to`.
    pub fn create_element(&mut self, name: &str) -> NodeId {
        let name = self.intern(name);
        let attrs = AttrList { start: to_u32(self.attrs.len()), len: 0 };
        self.new_node(NodeData::Element { name, attrs })
    }

    /// A new element with no parent and no attributes in the SVG or MathML namespace. JS: `document.createElementNS(ns, prefix + ':' + local)`.
    /// `local` keeps its case. The caller checked the names and the namespace rules. With `Ns::Html` it is `create_element`,
    /// and `prefix` must be `None`.
    pub fn create_element_ns(&mut self, ns: Ns, prefix: Option<&str>, local: &str) -> NodeId {
        let id = self.create_element(local);
        self.set_foreign(id, ns, prefix);
        id
    }

    fn set_foreign(&mut self, id: NodeId, ns: Ns, prefix: Option<&str>) {
        debug_assert!(matches!(ns, Ns::Html | Ns::Svg | Ns::MathMl));
        if ns != Ns::Html {
            self.revision += 1;
            let prefix = prefix.map(|p| self.intern(p));
            self.foreign.insert(id.index() as u32, Foreign { ns, prefix });
        }
    }

    /// Like `insert_element`, for an element in `ns` (the parser makes SVG and MathML this way). Each attribute has
    /// its namespace, if it has one; its name is the qualified name (`xlink:href`).
    pub fn insert_element_ns<K: AsRef<str>, V: AsRef<str>>(&mut self, parent: NodeId, before: Option<NodeId>, ns: Ns, name: &str, attrs: &[(Option<Ns>, K, V)]) -> NodeId {
        let name = self.intern(name);
        let start = self.attrs.len();
        for (attr_ns, key, value) in attrs {
            let attr = Attr { name: self.intern_ns(*attr_ns, key.as_ref()), value: self.push_text(value.as_ref()) };
            self.attrs.push(attr);
        }
        let attrs = AttrList { start: to_u32(start), len: to_u32(attrs.len()) };
        let id = self.insert(parent, before, NodeData::Element { name, attrs });
        self.set_foreign(id, ns, None);
        id
    }

    /// The namespace of an element. `Ns::Html` for an element made by the HTML rules, and for a node that is not an element.
    #[inline]
    pub fn element_ns(&self, el: NodeId) -> Ns {
        if self.foreign.is_empty() {
            return Ns::Html;
        }
        self.foreign.get(&(el.index() as u32)).map_or(Ns::Html, |f| f.ns)
    }

    /// Does the document have any SVG or MathML element? The tree builder asks, so that a page with none pays one compare.
    #[inline]
    pub fn has_foreign(&self) -> bool {
        !self.foreign.is_empty()
    }

    /// The name of an element (the local name, with its case), or an empty text for another node.
    pub fn element_name(&self, el: NodeId) -> &str {
        match self.nodes[el.index()].data {
            NodeData::Element { name, .. } => self.atom_str(name),
            _ => "",
        }
    }

    /// Is this node an element that is not in the HTML namespace?
    #[inline]
    pub fn is_foreign(&self, el: NodeId) -> bool {
        !self.foreign.is_empty() && self.foreign.contains_key(&(el.index() as u32))
    }

    /// Is this an HTML element with this (lowercase) name? An SVG `script` is not an HTML `script`.
    pub fn is_html(&self, el: NodeId, name: &str) -> bool {
        matches!(self.nodes[el.index()].data, NodeData::Element { name: n, .. } if self.atom_str(n) == name) && !self.is_foreign(el)
    }

    /// The prefix of an element: `svg` for `svg:rect`. `None` if there is none. The local name is `atom_str` of the name.
    pub fn element_prefix(&self, el: NodeId) -> Option<&str> {
        self.foreign.get(&(el.index() as u32)).and_then(|f| f.prefix).map(|p| self.atom_str(p))
    }

    /// The atom for an attribute name in a namespace. With `None` it is `intern`. A namespaced name is a different atom from
    /// the same name with no namespace.
    pub fn intern_ns(&mut self, ns: Option<Ns>, qname: &str) -> Atom {
        let Some(ns) = ns else { return self.intern(qname) };
        if let Some(&atom) = self.ns_atoms.get(&(ns, Box::from(qname))) {
            return atom;
        }
        let atom = Atom(u32::try_from(self.names.len()).expect("more than 4 billion names"));
        self.names.push(qname.into());
        self.ns_atoms.insert((ns, qname.into()), atom);
        self.atom_ns.insert(atom.0, ns);
        atom
    }

    /// The atom for an attribute name in a namespace, if the document has it. It does not add the name.
    pub fn find_ns_atom(&self, ns: Option<Ns>, qname: &str) -> Option<Atom> {
        match ns {
            None => self.find_atom(qname),
            Some(ns) => self.ns_atoms.get(&(ns, Box::from(qname))).copied(),
        }
    }

    /// The namespace of an attribute. `None` for no namespace.
    pub fn attr_ns(&self, name: Atom) -> Option<Ns> {
        if self.atom_ns.is_empty() { None } else { self.atom_ns.get(&name.0).copied() }
    }

    /// The prefix of an attribute: `xlink` for `xlink:href`. Only a namespaced attribute has one. (A plain `setAttribute('a:b')`
    /// makes an attribute with no namespace, no prefix, and the local name `a:b`.)
    pub fn attr_prefix(&self, name: Atom) -> Option<&str> {
        self.attr_ns(name)?;
        self.atom_str(name).split_once(':').map(|(p, _)| p)
    }

    /// The local name of an attribute.
    pub fn attr_local(&self, name: Atom) -> &str {
        let q = self.atom_str(name);
        match self.attr_ns(name) {
            Some(_) => q.split_once(':').map_or(q, |(_, l)| l),
            None => q,
        }
    }

    /// The first attribute of `el` whose qualified name is `qname`, in any namespace. JS: `getAttribute(qname)`.
    pub fn find_attr_qname(&self, el: NodeId, qname: &str) -> Option<Atom> {
        let NodeData::Element { attrs, .. } = self.nodes[el.index()].data else { return None };
        if self.ns_atoms.is_empty() {
            // No attribute has a namespace: a plain atom compare finds it.
            let atom = self.find_atom(qname)?;
            return self.attrs(attrs).iter().any(|a| a.name == atom).then_some(atom);
        }
        self.attrs(attrs).iter().map(|a| a.name).find(|&n| self.atom_str(n) == qname)
    }

    /// The attribute of `el` with this namespace and local name. JS: `getAttributeNS(ns, local)`.
    pub fn find_attr_ns(&self, el: NodeId, ns: Option<Ns>, local: &str) -> Option<Atom> {
        let NodeData::Element { attrs, .. } = self.nodes[el.index()].data else { return None };
        self.attrs(attrs).iter().map(|a| a.name).find(|&n| self.attr_ns(n) == ns && self.attr_local(n) == local)
    }

    /// Copy the node `n` of `src` into this document, with no parent and no children. An element keeps its name, its namespace, its prefix, and
    /// each attribute keeps its namespace; the names go in this name table and the values in this text buffer. The caller checked the caps
    /// first (`new_names_in`, `text_bytes`, `attr_slots`). Gives `None` for the top node and for a shadow root, which cannot be copied.
    pub fn import_node(&mut self, src: &Document, n: NodeId) -> Option<NodeId> {
        Some(match *src.data(n) {
            NodeData::Document | NodeData::ShadowRoot => return None,
            NodeData::Text(sp) => self.create_text(src.text(sp)),
            NodeData::Comment(sp) => self.create_comment(src.text(sp)),
            NodeData::Doctype(sp) => self.create_doctype(src.text(sp)),
            NodeData::Element { name, attrs } => {
                let name = self.intern(src.atom_str(name));
                let start = self.attrs.len();
                for a in src.attrs(attrs) {
                    let attr = Attr { name: self.intern_ns(src.attr_ns(a.name), src.atom_str(a.name)), value: self.push_text(src.text(a.value)) };
                    self.attrs.push(attr);
                }
                let list = AttrList { start: to_u32(start), len: attrs.len };
                let id = self.new_node(NodeData::Element { name, attrs: list });
                if src.is_foreign(n) {
                    self.set_foreign(id, src.element_ns(n), src.element_prefix(n));
                }
                id
            }
        })
    }

    /// How many names of this document (tags and attribute names) the table of `target` does not have yet. A host checks it against its name cap
    /// before `import_node`. It counts every name of this table, also one that no node of the tree uses any more, so it is an upper bound.
    pub fn new_names_in(&self, target: &Document) -> usize {
        (0..self.names.len())
            .filter(|&i| match self.atom_ns.get(&(i as u32)) {
                Some(&ns) => target.find_ns_atom(Some(ns), &self.names[i]).is_none(),
                None => target.find_atom(&self.names[i]).is_none(),
            })
            .count()
    }

    /// A new text node with no parent. It does not join a neighbour (only the parser joins texts).
    pub fn create_text(&mut self, text: &str) -> NodeId {
        let span = self.push_text(text);
        self.new_node(NodeData::Text(span))
    }

    /// A new comment with no parent.
    pub fn create_comment(&mut self, text: &str) -> NodeId {
        let span = self.push_text(text);
        self.new_node(NodeData::Comment(span))
    }

    /// A new doctype with no parent.
    pub fn create_doctype(&mut self, name: &str) -> NodeId {
        let span = self.push_text(name);
        self.new_node(NodeData::Doctype(span))
    }

    fn new_node(&mut self, data: NodeData) -> NodeId {
        self.places = Default::default();
        // The sibling-place table has one entry for each node, so a new node makes it too short.
        let id = NodeId::from_index(self.nodes.len());
        self.nodes.push(Node { parent: None, first_child: None, last_child: None, prev: None, next: None, data });
        id
    }

    /// Set an attribute of an element: change the value, or add it at the end of the list. The caller
    /// lowercases the name. Does nothing for a node that is not an element. The old value stays in the text
    /// buffer (see `text_bytes`).
    pub fn set_attr(&mut self, el: NodeId, name: &str, value: &str) {
        if matches!(self.nodes[el.index()].data, NodeData::Element { .. }) {
            let name = self.intern(name);
            self.set_attr_atom(el, name, value);
        }
    }

    /// Like `set_attr`, for a name that is an atom already (see `intern_ns`).
    pub fn set_attr_atom(&mut self, el: NodeId, name: Atom, value: &str) {
        let NodeData::Element { name: tag, attrs } = self.nodes[el.index()].data else { return };
        let span = self.push_text(value);
        self.revision += 1;
        self.class_sets.get_mut().remove(&el.index());
        // An empty list can start after the end (see `attrs`): it begins again at the end.
        let (start, len) = if attrs.len == 0 { (self.attrs.len(), 0) } else { (attrs.start as usize, attrs.len as usize) };
        if let Some(a) = self.attrs[start..start + len].iter_mut().find(|a| a.name == name) {
            let old = std::mem::replace(&mut a.value, span);
            if self.watcher.is_some() {
                self.notify(Change::Attr { el, name, old: Some(old) });
            }
            return;
        }
        if self.watcher.is_some() {
            self.notify(Change::Attr { el, name, old: None });
        }
        let new_start = if start + len == self.attrs.len() {
            start
        } else {
            // Another element's attributes follow: copy ours to the end. The old slots are wasted.
            self.attrs.extend_from_within(start..start + len);
            self.attrs.len() - len
        };
        self.attrs.push(Attr { name, value: span });
        self.nodes[el.index()].data = NodeData::Element { name: tag, attrs: AttrList { start: to_u32(new_start), len: to_u32(len + 1) } };
    }

    /// Remove an attribute. Gives whether it was there.
    pub fn remove_attr(&mut self, el: NodeId, name: &str) -> bool {
        let Some(name) = self.find_atom(name) else { return false };
        self.remove_attr_atom(el, name)
    }

    /// Like `remove_attr`, for a name that is an atom already (see `intern_ns`).
    pub fn remove_attr_atom(&mut self, el: NodeId, name: Atom) -> bool {
        let NodeData::Element { name: tag, attrs } = self.nodes[el.index()].data else { return false };
        let (start, len) = (attrs.start as usize, attrs.len as usize);
        if len == 0 {
            return false;
        }
        let Some(at) = self.attrs[start..start + len].iter().position(|a| a.name == name) else { return false };
        self.revision += 1;
        self.class_sets.get_mut().remove(&el.index());
        let value = self.attrs[start + at].value;
        if self.watcher.is_some() {
            self.notify(Change::Attr { el, name, old: Some(value) });
        }
        // A watched attribute is logged once: it leaves the watch set here, so the log cannot grow without a new `watch_attr`.
        if !self.watched.is_empty() && self.watched.remove(&(el.index() as u32, name.0)) {
            self.removed.push(RemovedAttr { el, name, value });
        }
        self.attrs.copy_within(start + at + 1..start + len, start + at);
        if start + len == self.attrs.len() {
            self.attrs.pop();
        }
        self.nodes[el.index()].data = NodeData::Element { name: tag, attrs: AttrList { start: attrs.start, len: attrs.len - 1 } };
        true
    }

    /// Tell `watcher` of every change of the tree, attributes and text from now on (`None` stops it). One watcher at a time. A page that
    /// sets none pays one check at a change. `Link` is sent after the node is in place, the other three before the change is made, so
    /// the old value is still readable. The watcher must not change this document.
    pub fn set_watcher(&mut self, watcher: Option<Watcher>) {
        self.watcher = watcher;
    }

    /// Is a watcher set? (For tests of the host: a page that uses none must have none.)
    pub fn has_watcher(&self) -> bool {
        self.watcher.is_some()
    }

    fn notify(&mut self, change: Change) {
        if let Some(mut w) = self.watcher.take() {
            w(self, change);
            self.watcher = Some(w);
        }
    }

    /// Ask to be told when the attribute `name` of `el` is removed (by any path: `remove_attr`, `remove_attr_atom`). A host
    /// that keeps an object for one attribute calls this, so that the object can become detached with the old value.
    /// A removal is logged once. Call it again for the next object of an attribute that was added back.
    pub fn watch_attr(&mut self, el: NodeId, name: Atom) {
        self.watched.insert((el.index() as u32, name.0));
    }

    /// Stop watching. Does nothing for an attribute that is not watched.
    pub fn unwatch_attr(&mut self, el: NodeId, name: Atom) {
        self.watched.remove(&(el.index() as u32, name.0));
    }

    /// Is a removal waiting in the log? A cheap check for hot paths.
    pub fn has_removed_attrs(&self) -> bool {
        !self.removed.is_empty()
    }

    /// The removals of watched attributes since the last call, oldest first. The log is empty after it.
    pub fn take_removed_attrs(&mut self) -> Vec<RemovedAttr> {
        std::mem::take(&mut self.removed)
    }

    /// The value of an attribute as a span (`None` if the element has no such attribute). Unlike `attr`, it does not borrow the text.
    pub fn attr_span(&self, el: NodeId, name: Atom) -> Option<Span> {
        let NodeData::Element { attrs, .. } = self.nodes[el.index()].data else { return None };
        self.attrs(attrs).iter().find(|a| a.name == name).map(|a| a.value)
    }

    /// Put `text` in the text buffer and give its span, for a value that no element holds (a detached attribute).
    /// The caller checks its text cap first (see `text_bytes`).
    pub fn new_span(&mut self, text: &str) -> Span {
        self.push_text(text)
    }

    /// How many attributes the element has. 0 for a node that is not an element.
    pub fn attr_count(&self, el: NodeId) -> usize {
        match self.nodes[el.index()].data {
            NodeData::Element { attrs, .. } => attrs.len as usize,
            _ => 0,
        }
    }

    /// Change the text of a text node, a comment or a doctype. Does nothing for other nodes.
    /// The old bytes stay in the text buffer (see `text_bytes`).
    pub fn set_text(&mut self, node: NodeId, text: &str) {
        self.text_revision += 1;
        if self.watcher.is_some()
            && let NodeData::Text(old) | NodeData::Comment(old) | NodeData::Doctype(old) = self.nodes[node.index()].data
        {
            self.notify(Change::Text { node, old });
        }
        match self.nodes[node.index()].data {
            NodeData::Text(_) => self.nodes[node.index()].data = NodeData::Text(self.push_text(text)),
            NodeData::Comment(_) => self.nodes[node.index()].data = NodeData::Comment(self.push_text(text)),
            NodeData::Doctype(_) => self.nodes[node.index()].data = NodeData::Doctype(self.push_text(text)),
            _ => {}
        }
    }

    /// Bytes in the text buffer, with the bytes of old values. The buffer never shrinks, so a host that lets a
    /// script change text must check this before it grows.
    pub fn text_bytes(&self) -> usize {
        self.text.len()
    }

    /// Slots in the attribute list, with the slots that no element uses any more. Same reason as `text_bytes`.
    pub fn attr_slots(&self) -> usize {
        self.attrs.len()
    }

    /// Put a text at the end of the text buffer. Gives its `Span`.
    fn push_text(&mut self, text: &str) -> Span {
        let start = self.text.len();
        self.text.push_str(text);
        Span { start: to_u32(start), len: to_u32(text.len()) }
    }

    /// The child of `parent` in front of `before`, or the last child when `before` is `None`.
    fn child_before(&self, parent: NodeId, before: Option<NodeId>) -> Option<NodeId> {
        match before {
            Some(b) => self.nodes[b.index()].prev,
            None => self.nodes[parent.index()].last_child,
        }
    }

    /// Add a comment as the last child of `parent`.
    pub fn append_comment_to(&mut self, parent: NodeId, text: &str) {
        let span = self.push_text(text);
        self.insert(parent, None, NodeData::Comment(span));
    }

    /// Add a comment at the end of the top node. Comments before and after the `html` element go there.
    pub fn append_comment(&mut self, text: &str) {
        self.append_comment_to(self.root(), text);
    }

    /// Add the doctype at the end of the top node.
    pub fn append_doctype(&mut self, name: &str) {
        let span = self.push_text(name);
        self.insert(self.root(), None, NodeData::Doctype(span));
    }

    /// Add text as the last child of `parent`. If the last child is text already, add to it.
    /// The DOM never has two text nodes side by side: `"a"` then `"b"` is one node `"ab"`.
    pub fn append_text(&mut self, parent: NodeId, text: &str) {
        self.insert_text(parent, None, text);
    }

    /// Like `append_text`, but just before `before`. The text joins the child in front of that place,
    /// if it is text.
    pub fn insert_text(&mut self, parent: NodeId, before: Option<NodeId>, text: &str) {
        let before = before.filter(|&b| self.parent(b) == Some(parent));
        if let Some(previous) = self.child_before(parent, before)
            && let NodeData::Text(old) = self.nodes[previous.index()].data
        {
            // Join the two texts. If the old text is the last one in the buffer, the new text just follows
            // it. If not, we copy the old text to the end first. The old bytes are wasted.
            let start = if old.end() == self.text.len() {
                old.start as usize
            } else {
                let start = self.text.len();
                self.text.extend_from_within(old.start as usize..old.end());
                start
            };
            self.text.push_str(text);
            self.text_revision += 1;
            let joined = Span { start: to_u32(start), len: to_u32(self.text.len() - start) };
            self.nodes[previous.index()].data = NodeData::Text(joined);
            return;
        }
        let span = self.push_text(text);
        self.insert(parent, before, NodeData::Text(span));
    }

    /// Give back the spare room of the lists. Call it when the tree is complete.
    /// A `Vec` doubles when it is full, so after 400,000 pushes it can hold 524,288 nodes.
    pub fn shrink_to_fit(&mut self) {
        self.nodes.shrink_to_fit();
        self.names.shrink_to_fit();
        self.attrs.shrink_to_fit();
        self.text.shrink_to_fit();
    }

    /// The tree as text, one line per node, in the format of the html5lib tests.
    /// Each level is indented 2 more spaces. Attributes come after their element, sorted by name.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for child in self.children(self.root()) {
            self.dump_node(child, 0, &mut out);
        }
        out
    }

    fn dump_node(&self, id: NodeId, depth: usize, out: &mut String) {
        // `&mut String` means we borrow the string and may add to it. JS: pass an array and `push`.
        let indent = "  ".repeat(depth);
        match self.data(id) {
            NodeData::Document | NodeData::ShadowRoot => {}
            NodeData::Doctype(name) => out.push_str(&format!("| {indent}<!DOCTYPE {}>\n", self.text(*name))),
            NodeData::Comment(text) => out.push_str(&format!("| {indent}<!-- {} -->\n", self.text(*text))),
            NodeData::Text(text) => out.push_str(&format!("| {indent}\"{}\"\n", self.text(*text))),
            NodeData::Element { name, attrs } => {
                // The html5lib format: an SVG or MathML element is `<svg path>`, an attribute in a namespace is `xlink href`.
                match self.element_ns(id) {
                    Ns::Svg => out.push_str(&format!("| {indent}<svg {}>\n", self.atom_str(*name))),
                    Ns::MathMl => out.push_str(&format!("| {indent}<math {}>\n", self.atom_str(*name))),
                    _ => out.push_str(&format!("| {indent}<{}>\n", self.atom_str(*name))),
                }
                let mut sorted: Vec<_> = self
                    .attrs(*attrs)
                    .iter()
                    .map(|a| match self.attr_ns(a.name) {
                        None => (self.atom_str(a.name).to_string(), self.text(a.value)),
                        Some(ns) => (format!("{} {}", self.attr_prefix(a.name).unwrap_or(if ns == Ns::Xmlns { "xmlns" } else { "" }), self.attr_local(a.name)), self.text(a.value)),
                    })
                    .collect();
                sorted.sort();
                // Sorted, so that two trees with the same attributes in another order look the same.
                for (key, value) in sorted {
                    out.push_str(&format!("| {indent}  {key}=\"{value}\"\n"));
                }
            }
        }
        for child in self.children(id) {
            self.dump_node(child, depth + 1, out);
            // A function that calls itself. It walks down the tree: one call for each level.
        }
    }
}

/// A position in the lists of a `Document`, as a `u32`.
fn to_u32(n: usize) -> u32 {
    u32::try_from(n).expect("more than 4 GB of text, or 4 billion attributes")
}

impl Default for Document {
    fn default() -> Document {
        Document::new()
    }
}

#[cfg(test)]
// Only compiled for `cargo test`.
mod tests {
    use super::*;

    /// Add an element at the end of `parent`.
    fn element(doc: &mut Document, parent: NodeId, name: &str, attrs: &[(&str, &str)]) -> NodeId {
        let attrs: Vec<_> = attrs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        doc.insert_element(parent, None, name, &attrs)
    }

    fn kids(doc: &Document, id: NodeId) -> Vec<NodeId> {
        doc.children(id).collect()
    }

    /// The text of a text node.
    fn text_of(doc: &Document, id: NodeId) -> &str {
        let NodeData::Text(span) = doc.data(id) else { panic!("not text") };
        doc.text(*span)
    }

    #[test]
    fn detached_nodes_do_not_break_the_sibling_cache() {
        let mut doc = Document::new();
        let root = doc.root();
        let a = element(&mut doc, root, "a", &[]);
        assert_eq!(doc.sibling_place(a, false, false), 1); // the cache is built now
        let n = doc.create_element("p");
        assert_eq!(doc.sibling_place(n, false, false), 0); // a detached node is in no sibling list; it must not crash
        doc.move_to(n, root, None);
        assert_eq!(doc.sibling_place(n, false, false), 2);
    }

    #[test]
    fn an_empty_attribute_list_survives_the_removal_of_the_last_attribute_slot() {
        // `c` is made while `a` holds the last attribute slot. Removing it shortens the list below `c`'s start.
        let mut doc = Document::new();
        let a = doc.create_element("a");
        doc.set_attr(a, "x", "1");
        let c = doc.create_element("c");
        assert!(doc.remove_attr(a, "x"));
        assert_eq!(doc.attr_count(c), 0);
        assert_eq!(doc.attr(c, Atom::ID), None);
        assert!(!doc.remove_attr(c, "id"));
        doc.set_attr(c, "id", "k");
        assert_eq!(doc.attr(c, Atom::ID), Some("k"));
        assert_eq!(doc.attr_count(a), 0);
        // `clone_element` of an empty list that starts after the end
        let d = doc.create_element("d");
        doc.set_attr(a, "x", "1");
        let e = doc.create_element("e");
        doc.remove_attr(a, "x");
        let copy = doc.clone_element(e);
        assert_eq!((doc.attr_count(copy), doc.attr_count(d)), (0, 0));
    }

    #[test]
    fn a_watched_attribute_is_logged_once_with_its_value_and_by_every_removal_path() {
        let mut doc = Document::new();
        let a = doc.create_element("a");
        doc.set_attr(a, "x", "1");
        doc.set_attr(a, "y", "2");
        let (x, y) = (doc.find_atom("x").unwrap(), doc.find_atom("y").unwrap());
        doc.watch_attr(a, x);
        doc.watch_attr(a, y);
        assert!(!doc.has_removed_attrs());
        assert!(doc.remove_attr(a, "x"));
        doc.set_attr(a, "x", "again");
        assert!(doc.remove_attr(a, "x")); // not watched any more: not logged again
        assert!(doc.remove_attr_atom(a, y));
        let log = doc.take_removed_attrs();
        assert_eq!(log.iter().map(|r| (doc.atom_str(r.name), doc.text(r.value))).collect::<Vec<_>>(), [("x", "1"), ("y", "2")]);
        assert!(!doc.has_removed_attrs());
        // an unwatched attribute and a replaced value leave no log
        doc.set_attr(a, "z", "3");
        doc.set_attr(a, "z", "4");
        doc.remove_attr(a, "z");
        assert!(!doc.has_removed_attrs());
        // `unwatch_attr` stops the log
        doc.set_attr(a, "w", "5");
        let w = doc.find_atom("w").unwrap();
        doc.watch_attr(a, w);
        doc.unwatch_attr(a, w);
        doc.remove_attr(a, "w");
        assert!(!doc.has_removed_attrs());
        assert_eq!(doc.attr_span(a, w), None);
    }

    #[test]
    fn set_and_remove_attributes_keep_the_others() {
        let mut doc = Document::new();
        let root = doc.root();
        let a = element(&mut doc, root, "a", &[("x", "1"), ("y", "2")]);
        let b = element(&mut doc, root, "b", &[("z", "3")]); // `a` is not at the end of the list now
        doc.set_attr(a, "w", "4");
        doc.set_attr(a, "x", "10");
        let list = |doc: &Document, el| match doc.data(el) {
            NodeData::Element { attrs, .. } => doc.attrs(*attrs).iter().map(|t| format!("{}={}", doc.atom_str(t.name), doc.text(t.value))).collect::<Vec<_>>().join(","),
            _ => panic!(),
        };
        assert_eq!(list(&doc, a), "x=10,y=2,w=4");
        assert_eq!(list(&doc, b), "z=3");
        assert!(doc.remove_attr(a, "y"));
        assert!(!doc.remove_attr(a, "y"));
        assert_eq!(list(&doc, a), "x=10,w=4");
        doc.set_attr(b, "q", "5");
        assert_eq!(list(&doc, b), "z=3,q=5");
        assert_eq!(list(&doc, a), "x=10,w=4");
        assert_eq!(doc.attr_count(a), 2);
    }

    #[test]
    fn a_namespaced_attribute_is_not_the_plain_attribute_of_the_same_name() {
        let mut doc = Document::new();
        let root = doc.root();
        let el = doc.create_element_ns(Ns::Svg, Some("s"), "use");
        doc.move_to(el, root, None);
        doc.set_attr(el, "xlink:href", "plain");
        let plain = doc.find_atom("xlink:href").unwrap();
        let ns = doc.intern_ns(Some(Ns::Xlink), "xlink:href");
        assert_ne!(ns, plain);
        assert_eq!(doc.find_ns_atom(Some(Ns::Xlink), "xlink:href"), Some(ns));
        assert_eq!(doc.find_ns_atom(Some(Ns::Xml), "xlink:href"), None);
        doc.set_attr_atom(el, ns, "ns");
        assert_eq!(doc.attr_count(el), 2);
        // by qualified name the first one wins; by namespace and local name each is found
        assert_eq!(doc.find_attr_qname(el, "xlink:href"), Some(plain));
        assert_eq!(doc.find_attr_ns(el, Some(Ns::Xlink), "href"), Some(ns));
        assert_eq!(doc.find_attr_ns(el, None, "xlink:href"), Some(plain));
        assert_eq!(doc.find_attr_ns(el, None, "href"), None);
        assert_eq!((doc.attr_ns(ns), doc.attr_prefix(ns), doc.attr_local(ns)), (Some(Ns::Xlink), Some("xlink"), "href"));
        assert_eq!((doc.attr(el, ns), doc.attr(el, plain)), (Some("ns"), Some("plain")));
        assert_eq!((doc.attr_ns(plain), doc.attr_prefix(plain), doc.attr_local(plain)), (None, None, "xlink:href"));
        assert_eq!((doc.element_ns(el), doc.element_prefix(el), doc.element_name(el)), (Ns::Svg, Some("s"), "use"));
        // a clone keeps the element's identity and the attribute identities
        let c = doc.clone_element(el);
        assert_eq!((doc.element_ns(c), doc.element_prefix(c)), (Ns::Svg, Some("s")));
        assert_eq!(doc.find_attr_ns(c, Some(Ns::Xlink), "href"), Some(ns));
        assert!(doc.remove_attr_atom(el, ns));
        assert_eq!(doc.attr_count(el), 1);
        assert_eq!(doc.find_attr_ns(c, Some(Ns::Xlink), "href"), Some(ns));
        assert!(!doc.remove_attr_atom(el, ns));
    }

    #[test]
    fn changing_a_long_class_attribute_drops_the_cached_set() {
        let mut doc = Document::new();
        let root = doc.root();
        let long = format!("{} target", "filler ".repeat(30));
        let a = element(&mut doc, root, "a", &[("class", &long)]);
        assert!(doc.has_class(a, "target")); // builds the cached set
        doc.set_attr(a, "class", &"other ".repeat(30));
        assert!(!doc.has_class(a, "target"));
        assert!(doc.has_class(a, "other"));
        doc.remove_attr(a, "class");
        assert!(!doc.has_class(a, "other"));
    }

    #[test]
    fn text_nodes_can_be_made_detached_and_changed() {
        let mut doc = Document::new();
        let root = doc.root();
        let p = element(&mut doc, root, "p", &[]);
        let t1 = doc.create_text("ab");
        let t2 = doc.create_text("cd");
        doc.move_to(t1, p, None);
        doc.move_to(t2, p, None);
        assert_eq!(kids(&doc, p), [t1, t2]); // two text nodes stay two (only the parser merges)
        doc.set_text(t1, "xyz");
        assert_eq!(text_of(&doc, t1), "xyz");
        assert_eq!(text_of(&doc, t2), "cd");
        let c = doc.create_comment("hi");
        doc.set_text(c, "yo");
        assert!(matches!(doc.data(c), NodeData::Comment(s) if doc.text(*s) == "yo"));
        assert!(doc.text_bytes() >= 9);
    }

    #[test]
    fn nodes_are_small() {
        // A big page has hundreds of thousands of nodes. Each byte here is multiplied by that.
        assert_eq!(std::mem::size_of::<NodeId>(), 4);
        assert_eq!(std::mem::size_of::<Option<NodeId>>(), 4);
        assert!(std::mem::size_of::<NodeData>() <= 16, "node data is {} bytes", std::mem::size_of::<NodeData>());
        assert!(std::mem::size_of::<Node>() <= 36, "a node is {} bytes", std::mem::size_of::<Node>());
    }

    #[test]
    fn a_new_document_has_only_the_root() {
        let doc = Document::new();
        assert_eq!(doc.data(doc.root()), &NodeData::Document);
        assert_eq!(doc.parent(doc.root()), None);
        assert_eq!(doc.children(doc.root()).count(), 0);
        assert_eq!(doc.dump(), "");
    }

    #[test]
    fn links_parents_and_children() {
        let mut doc = Document::new();
        let root = doc.root();
        let html = element(&mut doc, root, "html", &[]);
        let head = element(&mut doc, html, "head", &[]);
        let body = element(&mut doc, html, "body", &[]);
        assert_eq!(kids(&doc, html), [head, body]);
        // Children keep the order in which we added them.
        assert_eq!(doc.parent(head), Some(html));
        assert_eq!(doc.parent(html), Some(doc.root()));
        assert_eq!(doc.parent(body), Some(html));
    }

    #[test]
    fn names_are_stored_once() {
        let mut doc = Document::new();
        let root = doc.root();
        let a = element(&mut doc, root, "div", &[("class", "x")]);
        let b = element(&mut doc, root, "div", &[("class", "y")]);
        let (&NodeData::Element { name: na, attrs: la }, &NodeData::Element { name: nb, attrs: lb }) = (doc.data(a), doc.data(b)) else {
            panic!("not elements");
        };
        let (aa, ab) = (doc.attrs(la), doc.attrs(lb));
        assert_eq!(na, nb);
        assert_eq!(aa[0].name, ab[0].name);
        assert_eq!(doc.atom_str(na), "div");
        assert_eq!(doc.atom_str(aa[0].name), "class");
        assert_eq!((doc.text(aa[0].value), doc.text(ab[0].value)), ("x", "y"));
        assert_eq!(doc.names.len(), 3);
        // "id" and "class" from the start, and "div". Not 4, although we made two elements.
        assert_eq!(doc.atom_str(Atom::ID), "id");
        assert_eq!(aa[0].name, Atom::CLASS);
        assert_eq!(doc.attr(a, Atom::CLASS), Some("x"));
        assert_eq!(doc.attr(a, Atom::ID), None);
    }

    #[test]
    fn joins_text_next_to_text() {
        let mut doc = Document::new();
        let root = doc.root();
        let p = element(&mut doc, root, "p", &[]);
        doc.append_text(p, "a");
        doc.append_text(p, "b");
        assert_eq!(doc.children(p).count(), 1);
        assert_eq!(text_of(&doc, doc.children(p).next().unwrap()), "ab");
        doc.append_comment_to(p, "c");
        doc.append_text(p, "d");
        assert_eq!(doc.children(p).count(), 3);
        // A comment sits between "ab" and "d", so "d" is a new text node.
    }

    #[test]
    fn dumps_a_tree() {
        let mut doc = Document::new();
        let root = doc.root();
        doc.append_doctype("html");
        let html = element(&mut doc, root, "html", &[]);
        let body = element(&mut doc, html, "body", &[("id", "x"), ("class", "a b")]);
        doc.append_text(body, "Hi");
        doc.append_comment_to(body, " c ");
        let expected = "\
| <!DOCTYPE html>
| <html>
|   <body>
|     class=\"a b\"
|     id=\"x\"
|     \"Hi\"
|     <!--  c  -->
";
        assert_eq!(doc.dump(), expected);
        // The attributes are sorted: "class" comes before "id", although we added "id" first.
    }

    #[test]
    fn inserts_before_a_node() {
        let mut doc = Document::new();
        let root = doc.root();
        let body = element(&mut doc, root, "body", &[]);
        let table = element(&mut doc, body, "table", &[]);
        let div = doc.insert_element(body, Some(table), "div", &[] as &[(&str, &str)]);
        assert_eq!(kids(&doc, body), [div, table]);
        // The new node is before the table, not after it.
        assert_eq!(doc.parent(div), Some(body));
        doc.insert_text(body, Some(table), "a");
        doc.insert_text(body, Some(table), "b");
        let list = kids(&doc, body);
        assert_eq!(list.len(), 3);
        assert_eq!(text_of(&doc, list[1]), "ab");
        // The two pieces of text joined, and sit between the div and the table.
        assert_eq!((list[0], list[2]), (div, table));
    }

    #[test]
    fn joins_text_that_is_not_the_last_in_the_buffer() {
        // "ab" and "cd" are two texts. Joining "x" to "ab" must copy it, because "cd" comes after it in the buffer.
        let mut doc = Document::new();
        let root = doc.root();
        let p = element(&mut doc, root, "p", &[]);
        let q = element(&mut doc, root, "q", &[]);
        doc.append_text(p, "ab");
        doc.append_text(q, "cd");
        doc.append_text(p, "x");
        assert_eq!(text_of(&doc, doc.children(p).next().unwrap()), "abx");
        assert_eq!(text_of(&doc, doc.children(q).next().unwrap()), "cd");
        // The other text did not change.
    }

    #[test]
    fn reads_attributes_comments_and_doctype() {
        let mut doc = Document::new();
        let root = doc.root();
        doc.append_doctype("html");
        let a = element(&mut doc, root, "a", &[("href", "/x"), ("id", ""), ("title", "é€")]);
        doc.append_comment_to(a, "hi");
        let NodeData::Element { attrs, .. } = *doc.data(a) else { panic!("not an element") };
        let list: Vec<_> = doc.attrs(attrs).iter().map(|x| (doc.atom_str(x.name), doc.text(x.value))).collect();
        assert_eq!(list, [("href", "/x"), ("id", ""), ("title", "é€")]);
        let kinds: Vec<_> = doc.children(root).map(|c| doc.data(c)).collect();
        assert!(matches!(kinds[0], NodeData::Doctype(span) if doc.text(*span) == "html"));
        assert!(matches!(doc.data(doc.children(a).next().unwrap()), NodeData::Comment(span) if doc.text(*span) == "hi"));
    }

    #[test]
    fn inserts_before_the_first_child() {
        let mut doc = Document::new();
        let root = doc.root();
        let a = element(&mut doc, root, "a", &[]);
        let b = doc.insert_element(root, Some(a), "b", &[] as &[(&str, &str)]);
        let c = doc.insert_element(root, Some(a), "c", &[] as &[(&str, &str)]);
        assert_eq!(kids(&doc, root), [b, c, a]);
        // `b` was the first child, and `c` goes after `b` and before `a`.
        assert_eq!(doc.children(root).count(), 3);
    }

    #[test]
    fn a_before_that_is_not_a_child_adds_at_the_end() {
        let mut doc = Document::new();
        let root = doc.root();
        let a = element(&mut doc, root, "a", &[]);
        let inner = element(&mut doc, a, "i", &[]);
        let b = doc.insert_element(root, Some(inner), "b", &[] as &[(&str, &str)]);
        // `inner` is a child of `a`, not of the root. We do not link into the wrong list.
        assert_eq!(kids(&doc, root), [a, b]);
        assert_eq!(kids(&doc, a), [inner]);
    }

    /// The names of the children of a node, as text: `["a", "b"]`.
    fn names(doc: &Document, id: NodeId) -> Vec<String> {
        doc.children(id)
            .map(|c| match doc.data(c) {
                NodeData::Element { name, .. } => doc.atom_str(*name).to_string(),
                _ => "#".to_string(),
            })
            .collect()
    }

    /// Check that every link of the tree agrees: the parent of each child, and both directions of the sibling links.
    fn check(doc: &Document, id: NodeId) {
        let list = kids(doc, id);
        assert_eq!(doc.last_child(id), list.last().copied());
        for (i, &c) in list.iter().enumerate() {
            assert_eq!(doc.parent(c), Some(id));
            assert_eq!(doc.prev_sibling(c), i.checked_sub(1).map(|j| list[j]));
            assert_eq!(doc.next_sibling(c), list.get(i + 1).copied());
            check(doc, c);
        }
    }

    #[test]
    fn move_and_detach() {
        let mut doc = Document::new();
        let root = doc.root();
        let (a, b, c) = (element(&mut doc, root, "a", &[]), element(&mut doc, root, "b", &[]), element(&mut doc, root, "c", &[]));
        let inner = element(&mut doc, a, "inner", &[]);
        // Move to the end, to the front, to the middle, and into another node.
        doc.move_to(a, root, None);
        assert_eq!(names(&doc, root), ["b", "c", "a"]);
        doc.move_to(a, root, Some(b));
        assert_eq!(names(&doc, root), ["a", "b", "c"]);
        doc.move_to(c, root, Some(b));
        assert_eq!(names(&doc, root), ["a", "c", "b"]);
        doc.move_to(b, a, None);
        assert_eq!(names(&doc, root), ["a", "c"]);
        assert_eq!(names(&doc, a), ["inner", "b"]);
        // A `before` that is not a child of the parent, or is the node itself, puts it at the end.
        doc.move_to(c, a, Some(root));
        assert_eq!(names(&doc, a), ["inner", "b", "c"]);
        doc.move_to(c, a, Some(c));
        assert_eq!(names(&doc, a), ["inner", "b", "c"]);
        check(&doc, root);
        // `detach` takes a node and its children out. Doing it twice, or for a node with no parent, is fine.
        doc.detach(a);
        doc.detach(a);
        assert_eq!(names(&doc, root), Vec::<String>::new());
        assert_eq!(doc.parent(a), None);
        assert_eq!(names(&doc, a), ["inner", "b", "c"]);
        let _ = inner;
        check(&doc, a);
    }

    #[test]
    fn move_children_keeps_the_order() {
        let mut doc = Document::new();
        let root = doc.root();
        let (from, to) = (element(&mut doc, root, "from", &[]), element(&mut doc, root, "to", &[]));
        for name in ["x", "y", "z"] {
            element(&mut doc, from, name, &[]);
        }
        element(&mut doc, to, "first", &[]);
        doc.append_text(from, "t");
        doc.move_children(from, to);
        assert_eq!(names(&doc, from), Vec::<String>::new());
        assert_eq!(names(&doc, to), ["first", "x", "y", "z", "#"]);
        check(&doc, root);
        // Nothing to move is fine.
        doc.move_children(from, to);
        check(&doc, root);
    }

    #[test]
    fn clone_has_the_attributes_and_no_children() {
        let mut doc = Document::new();
        let root = doc.root();
        let a = element(&mut doc, root, "a", &[("href", "x"), ("class", "y z")]);
        element(&mut doc, a, "child", &[]);
        let copy = doc.clone_element(a);
        assert_eq!(doc.parent(copy), None);
        assert_eq!(names(&doc, copy), Vec::<String>::new());
        let (NodeData::Element { name: n1, attrs: a1 }, NodeData::Element { name: n2, attrs: a2 }) = (*doc.data(a), *doc.data(copy)) else { panic!() };
        assert_eq!(n1, n2);
        let read = |list| doc.attrs(list).iter().map(|x| (doc.atom_str(x.name).to_string(), doc.text(x.value).to_string())).collect::<Vec<_>>();
        assert_eq!(read(a1), read(a2));
        assert_eq!(read(a2), [("href".to_string(), "x".to_string()), ("class".to_string(), "y z".to_string())]);
        // The copy can be put in the tree.
        doc.move_to(copy, root, None);
        assert_eq!(names(&doc, root), ["a", "a"]);
        check(&doc, root);
    }

    #[test]
    fn sibling_places_follow_the_tree() {
        let mut doc = crate::tree::parse("<ul><i></i><li></li><i></i><li></li></ul>");
        let (html, ul) = (doc.children(doc.root()).last().unwrap(), |doc: &Document, n| doc.children(n).last().unwrap());
        let ul = ul(&doc, ul(&doc, html));
        let kids: Vec<NodeId> = doc.children(ul).collect();
        let places = |doc: &Document, of_type, from_end| doc.children(ul).map(|c| doc.sibling_place(c, of_type, from_end)).collect::<Vec<_>>();
        assert_eq!(places(&doc, false, false), [1, 2, 3, 4]);
        assert_eq!(places(&doc, false, true), [4, 3, 2, 1]);
        assert_eq!(places(&doc, true, false), [1, 1, 2, 2]);
        assert_eq!(places(&doc, true, true), [2, 2, 1, 1]);
        // A change in the tree must drop the places that we made.
        doc.detach(kids[1]);
        assert_eq!(places(&doc, false, false), [1, 2, 3]);
        assert_eq!(places(&doc, true, true), [2, 1, 1]);
    }

    #[test]
    fn has_class_reads_short_and_long_lists() {
        let long: String = (0..100).map(|i| format!("c{i}")).collect::<Vec<_>>().join("  ");
        let doc = crate::tree::parse(&format!("<i class='a  b\tc'></i><i class='{long}'></i><i></i>"));
        let html = doc.children(doc.root()).last().unwrap();
        let body = doc.children(html).last().unwrap();
        let (short, big, none): (Vec<NodeId>, Vec<NodeId>, Vec<NodeId>) = {
            let kids: Vec<NodeId> = doc.children(body).collect();
            (vec![kids[0]], vec![kids[1]], vec![kids[2]])
        };
        assert!(doc.has_class(short[0], "b") && doc.has_class(short[0], "c") && !doc.has_class(short[0], "d"));
        // The long list is a set. Ask twice: the second time it comes from the set.
        for _ in 0..2 {
            assert!(doc.has_class(big[0], "c0") && doc.has_class(big[0], "c99"));
            assert!(!doc.has_class(big[0], "c100") && !doc.has_class(big[0], "c"));
        }
        assert!(!doc.has_class(none[0], "a"));
    }

    /// Every public change that can alter which nodes are in the tree, or what their attributes or namespaces are, moves
    /// `revision`. A host cache of a tree query (`getElementsByName`) is valid only while it is the same.
    #[test]
    fn revision_moves_on_every_tree_attribute_and_namespace_change() {
        let mut doc = Document::new();
        let root = doc.root();
        let r0 = doc.revision();
        let mut seen = vec![r0];
        let mut moved = |doc: &Document, what: &str| {
            let r = doc.revision();
            assert!(r != *seen.last().unwrap(), "{what} did not change the revision");
            seen.push(r);
        };
        let a = doc.insert_element(root, None, "div", &[("name", "x")]);
        moved(&doc, "insert_element");
        let t = doc.create_element("p");
        let c = doc.create_text("t");
        doc.move_to(t, a, None);
        moved(&doc, "move_to (link)");
        doc.move_to(t, root, Some(a));
        moved(&doc, "move_to (reparent)");
        doc.detach(t);
        moved(&doc, "detach");
        doc.move_children(root, a);
        moved(&doc, "move_children");
        let name = doc.intern("name");
        doc.set_attr_atom(a, name, "y");
        moved(&doc, "set_attr_atom (change)");
        doc.set_attr(a, "other", "1");
        moved(&doc, "set_attr (add)");
        assert!(doc.remove_attr(a, "other"));
        moved(&doc, "remove_attr");
        assert!(doc.remove_attr_atom(a, name));
        moved(&doc, "remove_attr_atom");
        let svg = doc.insert_element_ns::<&str, &str>(a, None, Ns::Svg, "g", &[]);
        moved(&doc, "insert_element_ns");
        doc.append_text(a, "more");
        moved(&doc, "append_text (new node)");
        doc.append_comment_to(a, "c");
        moved(&doc, "append_comment_to");
        doc.append_comment("c2");
        moved(&doc, "append_comment");
        doc.append_doctype("html");
        moved(&doc, "append_doctype");
        let ns = doc.create_element_ns(Ns::Svg, None, "rect");
        moved(&doc, "create_element_ns (set_foreign)");
        let _ = (svg, c, ns);
    }

    /// Changes that cannot alter any tree query leave the revision alone, so a cache is not rebuilt for nothing.
    #[test]
    fn revision_stays_for_changes_outside_the_tree() {
        let mut doc = Document::new();
        let root = doc.root();
        let a = doc.insert_element(root, None, "div", &[("name", "x")]);
        let r = doc.revision();
        let n = doc.create_element("i");
        let c = doc.clone_element(a);
        let t = doc.create_text("t");
        let _ = doc.create_comment("c");
        let _ = doc.create_doctype("d");
        doc.set_text(t, "u");
        let _ = doc.intern("other");
        let _ = doc.new_span("s");
        doc.detach(n);
        assert!(!doc.remove_attr(a, "absent"));
        let _ = c;
        assert_eq!(doc.revision(), r);
    }

    #[test]
    fn revision_follows_the_parser_and_import() {
        let doc = crate::tree::parse("<!doctype html><body><i name=a></i></body>");
        assert!(doc.revision() > 0);
        let mut other = Document::new();
        let r = other.revision();
        let i = doc.children(doc.children(doc.root()).last().unwrap()).last().unwrap();
        let body = doc.children(doc.children(doc.root()).last().unwrap()).last().unwrap();
        let imported = other.import_node(&doc, body).unwrap();
        let _ = (i, imported);
        // an imported node has no parent: not a tree change; linking it is.
        assert_eq!(other.revision(), r);
        let root = other.root();
        other.move_to(imported, root, None);
        assert!(other.revision() != r);
    }
}
