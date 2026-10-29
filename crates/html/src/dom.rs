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
//!
//! Comments compare Rust with JavaScript, because you know JS.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::num::NonZeroU32;

/// The number of a node. It is the place of the node in the `Vec` of its `Document`, plus one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// `Copy` means that passing a `NodeId` around copies it, like a number.
pub struct NodeId(NonZeroU32);
// JS: like a number, but the type stops us from mixing it up with other numbers.
// `NonZeroU32` is a number that is never 0. Rust uses the free "0" to mean `None`, so an
// `Option<NodeId>` needs no extra space. The inner number has no `pub`, so other crates cannot
// make a `NodeId` of their own.

impl NodeId {
    /// The place in the `Vec`.
    fn index(self) -> usize {
        self.0.get() as usize - 1
    }

    fn from_index(index: usize) -> NodeId {
        NodeId(NonZeroU32::new(u32::try_from(index + 1).expect("more than 4 billion nodes")).unwrap())
    }
}

/// The number of a name in the name table of a `Document`. Two atoms of one document are equal
/// when the names are equal. Atoms of different documents mean nothing together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Atom(u32);

/// One attribute of an element: `class="a b"`.
#[derive(Debug, Clone, PartialEq)]
pub struct Attr {
    pub name: Atom,
    pub value: String,
}

/// What a node is.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeData {
    /// The top of the tree. It has no parent. It holds the doctype and the `html` element.
    Document,
    /// `<!DOCTYPE html>`. The name is in lowercase.
    Doctype(String),
    /// An element like `<p class="a">`. The name is in lowercase. Use `Document::atom_str` to read a name.
    Element { name: Atom, attrs: Vec<Attr> },
    /// A piece of text.
    Text(String),
    /// `<!-- text -->`.
    Comment(String),
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
}

/// A fast hash for short names (FNV-1a). The standard one (SipHash) is slower, and it protects against
/// a server that picks keys to make the table slow. Here the "server" is the page, and it can only
/// slow down its own tab.
struct Fnv(u64);

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
        Document {
            nodes: vec![Node { parent: None, first_child: None, last_child: None, prev: None, next: None, data: NodeData::Document }],
            names: Vec::new(),
            atoms: HashMap::default(),
        }
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

    /// The children of this node, in order.
    pub fn children(&self, id: NodeId) -> Children<'_> {
        Children { doc: self, next: self.nodes[id.index()].first_child }
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

    /// Add a new node as the last child of `parent`. Gives the new node.
    pub fn append(&mut self, parent: NodeId, data: NodeData) -> NodeId {
        self.insert(parent, None, data)
    }

    /// Add a new node as a child of `parent`, just before `before`. With `None`, add it at the end.
    /// `before` must be a child of `parent`. If it is not, the node goes at the end.
    /// Gives the new node. The tree builder needs this to put text before a table.
    pub fn insert(&mut self, parent: NodeId, before: Option<NodeId>, data: NodeData) -> NodeId {
        let before = before.filter(|&b| self.parent(b) == Some(parent));
        let prev = self.child_before(parent, before);
        let id = NodeId::from_index(self.nodes.len());
        // The new node goes at the end of the `Vec`, so its number is the length now.
        self.nodes.push(Node { parent: Some(parent), first_child: None, last_child: None, prev, next: before, data });
        // Fix the links of the neighbours. JS: like inserting into a doubly linked list.
        match prev {
            Some(p) => self.nodes[p.index()].next = Some(id),
            None => self.nodes[parent.index()].first_child = Some(id),
        }
        match before {
            Some(b) => self.nodes[b.index()].prev = Some(id),
            None => self.nodes[parent.index()].last_child = Some(id),
        }
        id
    }

    /// Add an element, with its name and attributes as text. The names go in the name table.
    pub fn insert_element(&mut self, parent: NodeId, before: Option<NodeId>, name: &str, attrs: &[(String, String)]) -> NodeId {
        let name = self.intern(name);
        let attrs = attrs.iter().map(|(key, value)| Attr { name: self.intern(key), value: value.clone() }).collect();
        self.insert(parent, before, NodeData::Element { name, attrs })
    }

    /// The child of `parent` in front of `before`, or the last child when `before` is `None`.
    fn child_before(&self, parent: NodeId, before: Option<NodeId>) -> Option<NodeId> {
        match before {
            Some(b) => self.nodes[b.index()].prev,
            None => self.nodes[parent.index()].last_child,
        }
    }

    /// Add a comment at the end of the top node. Comments before and after the `html` element go there.
    pub fn append_comment(&mut self, text: String) {
        self.append(self.root(), NodeData::Comment(text));
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
            && let NodeData::Text(existing) = &mut self.nodes[previous.index()].data
        {
            existing.push_str(text);
            return;
        }
        self.insert(parent, before, NodeData::Text(text.to_string()));
    }

    /// Give back the spare room of the lists. Call it when the tree is complete.
    /// A `Vec` doubles when it is full, so after 400,000 pushes it can hold 524,288 nodes.
    pub fn shrink_to_fit(&mut self) {
        self.nodes.shrink_to_fit();
        self.names.shrink_to_fit();
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
            NodeData::Document => {}
            NodeData::Doctype(name) => out.push_str(&format!("| {indent}<!DOCTYPE {name}>\n")),
            NodeData::Comment(text) => out.push_str(&format!("| {indent}<!-- {text} -->\n")),
            NodeData::Text(text) => out.push_str(&format!("| {indent}\"{text}\"\n")),
            NodeData::Element { name, attrs } => {
                out.push_str(&format!("| {indent}<{}>\n", self.atom_str(*name)));
                let mut sorted: Vec<_> = attrs.iter().map(|a| (self.atom_str(a.name), &a.value)).collect();
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

    #[test]
    fn nodes_are_small() {
        // A big page has hundreds of thousands of nodes. Each byte here is multiplied by that.
        assert_eq!(std::mem::size_of::<NodeId>(), 4);
        assert_eq!(std::mem::size_of::<Option<NodeId>>(), 4);
        assert!(std::mem::size_of::<Node>() <= 56, "a node is {} bytes", std::mem::size_of::<Node>());
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
        let (NodeData::Element { name: na, attrs: aa }, NodeData::Element { name: nb, attrs: ab }) = (doc.data(a), doc.data(b)) else {
            panic!("not elements");
        };
        assert_eq!(na, nb);
        assert_eq!(aa[0].name, ab[0].name);
        assert_eq!(doc.atom_str(*na), "div");
        assert_eq!(doc.atom_str(aa[0].name), "class");
        assert_eq!(doc.names.len(), 2);
        // Only "div" and "class", although we made two elements.
    }

    #[test]
    fn joins_text_next_to_text() {
        let mut doc = Document::new();
        let root = doc.root();
        let p = element(&mut doc, root, "p", &[]);
        doc.append_text(p, "a");
        doc.append_text(p, "b");
        assert_eq!(doc.children(p).count(), 1);
        let first = doc.children(p).next().unwrap();
        assert_eq!(doc.data(first), &NodeData::Text("ab".to_string()));
        doc.append(p, NodeData::Comment("c".to_string()));
        doc.append_text(p, "d");
        assert_eq!(doc.children(p).count(), 3);
        // A comment sits between "ab" and "d", so "d" is a new text node.
    }

    #[test]
    fn dumps_a_tree() {
        let mut doc = Document::new();
        let root = doc.root();
        doc.append(root, NodeData::Doctype("html".to_string()));
        let html = element(&mut doc, root, "html", &[]);
        let body = element(&mut doc, html, "body", &[("id", "x"), ("class", "a b")]);
        doc.append_text(body, "Hi");
        doc.append(body, NodeData::Comment(" c ".to_string()));
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
        let div = doc.insert_element(body, Some(table), "div", &[]);
        assert_eq!(kids(&doc, body), [div, table]);
        // The new node is before the table, not after it.
        assert_eq!(doc.parent(div), Some(body));
        doc.insert_text(body, Some(table), "a");
        doc.insert_text(body, Some(table), "b");
        let list = kids(&doc, body);
        assert_eq!(list.len(), 3);
        assert_eq!(doc.data(list[1]), &NodeData::Text("ab".to_string()));
        // The two pieces of text joined, and sit between the div and the table.
        assert_eq!((list[0], list[2]), (div, table));
    }

    #[test]
    fn inserts_before_the_first_child() {
        let mut doc = Document::new();
        let root = doc.root();
        let a = element(&mut doc, root, "a", &[]);
        let b = doc.insert_element(root, Some(a), "b", &[]);
        let c = doc.insert_element(root, Some(a), "c", &[]);
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
        let b = doc.insert_element(root, Some(inner), "b", &[]);
        // `inner` is a child of `a`, not of the root. We do not link into the wrong list.
        assert_eq!(kids(&doc, root), [a, b]);
        assert_eq!(kids(&doc, a), [inner]);
    }
}
