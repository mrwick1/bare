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
//! one `Vec`. Module 10 counts the memory for each tab.
//!
//! Comments compare Rust with JavaScript, because you know JS.

/// The number of a node. It is the place of the node in the `Vec` of its `Document`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// `Copy` means that passing a `NodeId` around copies it, like a number.
pub struct NodeId(usize);
// JS: like a number, but the type stops us from mixing it up with other numbers.
// The `usize` has no `pub`, so other crates cannot make a `NodeId` of their own.

/// What a node is.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeData {
    /// The top of the tree. It has no parent. It holds the doctype and the `html` element.
    Document,
    /// `<!DOCTYPE html>`. The name is in lowercase.
    Doctype(String),
    /// An element like `<p class="a">`. The name is in lowercase.
    Element { name: String, attrs: Vec<(String, String)> },
    /// A piece of text.
    Text(String),
    /// `<!-- text -->`.
    Comment(String),
}

struct Node {
    // No `pub`: only this file can change a node. This keeps parent and children in step.
    parent: Option<NodeId>,
    // `None` for the document at the top. JS: `parentNode` is `null` there.
    children: Vec<NodeId>,
    data: NodeData,
}

/// A whole tree. It owns all of its nodes.
pub struct Document {
    nodes: Vec<Node>,
    // The arena. `NodeId(0)` is the `Document` node, and every other node comes after it.
}

impl Document {
    /// A new tree with only the top node.
    pub fn new() -> Document {
        Document { nodes: vec![Node { parent: None, children: Vec::new(), data: NodeData::Document }] }
    }

    /// The top node.
    pub fn root(&self) -> NodeId {
        NodeId(0)
    }

    /// What this node is.
    pub fn data(&self, id: NodeId) -> &NodeData {
        &self.nodes[id.0].data
        // JS: `nodes[id].data`. This crashes if `id` comes from another document. See the top of this file.
    }

    /// The parent of this node. `None` for the top node.
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.0].parent
    }

    /// The children of this node, in order.
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id.0].children
        // `&[NodeId]` is a view on the list. The caller can read it but not change it.
    }

    /// Add a new node as the last child of `parent`. Gives the new node.
    pub fn append(&mut self, parent: NodeId, data: NodeData) -> NodeId {
        self.insert(parent, None, data)
    }

    /// Add a new node as a child of `parent`, just before `before`. With `None`, add it at the end.
    /// Gives the new node. The tree builder needs this to put text before a table.
    pub fn insert(&mut self, parent: NodeId, before: Option<NodeId>, data: NodeData) -> NodeId {
        let id = NodeId(self.nodes.len());
        // The new node goes at the end of the `Vec`, so its number is the length now.
        self.nodes.push(Node { parent: Some(parent), children: Vec::new(), data });
        let children = &mut self.nodes[parent.0].children;
        let at = before.and_then(|b| children.iter().position(|&c| c == b)).unwrap_or(children.len());
        // The place of `before` in the list of children, or the end. JS: `indexOf`, then `splice`.
        children.insert(at, id);
        id
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
        let children = &self.nodes[parent.0].children;
        let at = before.and_then(|b| children.iter().position(|&c| c == b)).unwrap_or(children.len());
        let previous = at.checked_sub(1).map(|i| children[i]);
        // The child in front of that place, if there is one. Copying the `NodeId` ends the borrow of `children`.
        if let Some(previous) = previous
            && let NodeData::Text(existing) = &mut self.nodes[previous.0].data
        {
            existing.push_str(text);
            return;
        }
        self.insert(parent, before, NodeData::Text(text.to_string()));
    }

    /// The tree as text, one line per node, in the format of the html5lib tests.
    /// Each level is indented 2 more spaces. Attributes come after their element, sorted by name.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for &child in self.children(self.root()) {
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
                out.push_str(&format!("| {indent}<{name}>\n"));
                let mut sorted: Vec<_> = attrs.iter().collect();
                sorted.sort();
                // Sorted, so that two trees with the same attributes in another order look the same.
                for (key, value) in sorted {
                    out.push_str(&format!("| {indent}  {key}=\"{value}\"\n"));
                }
            }
        }
        for &child in self.children(id) {
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

    fn element(name: &str, attrs: &[(&str, &str)]) -> NodeData {
        NodeData::Element { name: name.to_string(), attrs: attrs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect() }
    }

    #[test]
    fn a_new_document_has_only_the_root() {
        let doc = Document::new();
        assert_eq!(doc.data(doc.root()), &NodeData::Document);
        assert_eq!(doc.parent(doc.root()), None);
        assert!(doc.children(doc.root()).is_empty());
        assert_eq!(doc.dump(), "");
    }

    #[test]
    fn links_parents_and_children() {
        let mut doc = Document::new();
        let html = doc.append(doc.root(), element("html", &[]));
        let head = doc.append(html, element("head", &[]));
        let body = doc.append(html, element("body", &[]));
        assert_eq!(doc.children(html), [head, body]);
        // Children keep the order in which we added them.
        assert_eq!(doc.parent(head), Some(html));
        assert_eq!(doc.parent(html), Some(doc.root()));
        assert_eq!(doc.parent(body), Some(html));
    }

    #[test]
    fn joins_text_next_to_text() {
        let mut doc = Document::new();
        let p = doc.append(doc.root(), element("p", &[]));
        doc.append_text(p, "a");
        doc.append_text(p, "b");
        assert_eq!(doc.children(p).len(), 1);
        assert_eq!(doc.data(doc.children(p)[0]), &NodeData::Text("ab".to_string()));
        doc.append(p, NodeData::Comment("c".to_string()));
        doc.append_text(p, "d");
        assert_eq!(doc.children(p).len(), 3);
        // A comment sits between "ab" and "d", so "d" is a new text node.
    }

    #[test]
    fn dumps_a_tree() {
        let mut doc = Document::new();
        doc.append(doc.root(), NodeData::Doctype("html".to_string()));
        let html = doc.append(doc.root(), element("html", &[]));
        let body = doc.append(html, element("body", &[("id", "x"), ("class", "a b")]));
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
        let body = doc.append(doc.root(), element("body", &[]));
        let table = doc.append(body, element("table", &[]));
        let div = doc.insert(body, Some(table), element("div", &[]));
        assert_eq!(doc.children(body), [div, table]);
        // The new node is before the table, not after it.
        assert_eq!(doc.parent(div), Some(body));
        doc.insert_text(body, Some(table), "a");
        doc.insert_text(body, Some(table), "b");
        assert_eq!(doc.children(body).len(), 3);
        assert_eq!(doc.data(doc.children(body)[1]), &NodeData::Text("ab".to_string()));
        // The two pieces of text joined, and sit between the div and the table.
    }
}
