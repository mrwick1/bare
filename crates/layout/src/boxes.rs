//! Boxes: which box each node of the DOM makes. This is the "box tree" (Chrome: the layout tree, Firefox: the frame tree).
//!
//! The DOM and the boxes are not the same tree:
//! - `display: none` makes no box, and nothing inside it does. `head`, `script` and `style` are `display: none`.
//!   (`visibility: hidden` still makes a box: it takes space, it is only not painted.)
//! - A block that holds blocks and text, like `<div>text<p>para</p>more</div>`, gets a box with no element for each
//!   run of text: an "anonymous block box". CSS needs this so that a block holds either only blocks or only lines.
//!   The `div` above holds three blocks: anonymous ("text"), `p`, anonymous ("more").
//! - Spaces and newlines between two blocks make no box. `<div>\n  <p>a</p>\n  <p>b</p>\n</div>` has the text
//!   "\n  " three times, and it would make three empty anonymous blocks. Most pages are full of this text.
//! - A list item gets one more box: the marker (`•` or `1.`).
//! - A `<span>` that runs over two lines makes two pieces. That happens in layout (`inline.rs`), not here.
//!
//! How real browsers keep the box tree:
//! - Chrome: a `LayoutObject` for each box, a C++ object on the heap with pointers to its parent and siblings. Layout
//!   (LayoutNG) reads it and writes a second tree, the immutable "fragment tree", with the sizes and places.
//! - Firefox: an `nsIFrame` for each box, with the sizes and places in the same object. Text runs are cached on it.
//! - Servo (layout 2020) and Ladybird: a box tree built in a first pass, then a fragment tree.
//!
//! All of them store the box tree. It costs memory: Chrome uses about 1.6 KB for each element for style and layout
//! (`docs/resources.md`), and the layout objects are most of it.
//!
//! Our choice: we do not store a box tree at all. The rules above are functions that layout calls while it walks the
//! DOM: `kind` says what box a node makes, and `children` gives the child boxes of a block, with the anonymous blocks
//! as runs of DOM siblings (`Child::Lines(first, last)`). Layout writes only its result, the fragments.
//! - Gain: no memory and no time for a second tree. A box tree of 200,000 boxes at 20 bytes each would be 4 MB, and a
//!   pass over the whole DOM to build it.
//! - Cost: the rules run again on each layout. They are cheap (a style lookup and a few compares for each node), and
//!   `children` reads each child twice. When the page changes (JavaScript, module 9), browsers keep the box tree to
//!   update only a part of it. We can keep the results of layout for each node in a side table instead, like `Styles`
//!   keeps a style for each node.
//!
//! Comments compare Rust with JavaScript, because you know JS.

use bare_css::cascade::{ComputedStyle, Styles};
use bare_css::values::{Keyword, Property, Value};
use bare_html::dom::{Atom, Document, NodeData, NodeId};

/// What box a node makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// No box: `display: none`, a comment, a doctype.
    None,
    /// A block: it holds blocks, or lines. `display: block`, and for now also `table`, `flex`, `grid` and the parts of
    /// a table (their own layout is not built, see `docs/progress.md`).
    Block,
    /// A block with a marker in front: `display: list-item` (`li`).
    ListItem,
    /// An inline box: its content flows in the lines of the block around it (`span`, `a`, `b`).
    Inline,
    /// An inline box that is one unit on the line, with a block inside: `inline-block`, `button`, `img`.
    /// It is never cut across two lines. CSS calls it an "atomic inline".
    Atomic,
    /// `float: left` or `right`: out of the normal flow, at the side of the block (`floats.rs`). Inside it is a block
    /// with its own block formatting context, like an inline-block.
    Float,
    /// `position: absolute` or `fixed`: out of the flow, placed in its containing block (`block::out_of_flow`). It
    /// leaves a "static position" in the flow, where it would be with `position: static`. Inside it is a block with its
    /// own block formatting context.
    OutOfFlow,
    /// A text node.
    Text,
    /// `<br>`: the line ends here.
    Break,
}

impl Kind {
    /// Does it sit in a line (an inline-level box), and not in a stack of blocks?
    pub fn is_inline(self) -> bool {
        matches!(self, Kind::Inline | Kind::Atomic | Kind::Text | Kind::Break | Kind::Float | Kind::OutOfFlow)
        // A float counts here: it goes with the inline content next to it, so text around it stays in one run of lines.
    }

    /// Is it a block in a stack of blocks (a block-level box)?
    pub fn is_block(self) -> bool {
        matches!(self, Kind::Block | Kind::ListItem)
    }
}

/// What layout needs to make boxes: the DOM, the styles, and the names that it tests. Make it once for each layout.
/// `Copy`: it holds only references and numbers, so a copy is cheap.
#[derive(Clone, Copy)]
pub struct Tree<'a> {
    pub doc: &'a Document,
    pub styles: &'a Styles,
    // A `&'a` is a reference that must not outlive `'a`: the `Tree` cannot live longer than the DOM and the styles.
    // JS has no such rule: the garbage collector keeps the objects alive.
    br: Option<Atom>,
    ol: Option<Atom>,
    start: Option<Atom>,
    value: Option<Atom>,
    body: Option<Atom>,
    // The atoms of these names, if the page has them. A page with no `<br>` has no atom for `br`.
}

impl<'a> Tree<'a> {
    pub fn new(doc: &'a Document, styles: &'a Styles) -> Tree<'a> {
        Tree { doc, styles, br: doc.find_atom("br"), ol: doc.find_atom("ol"), start: doc.find_atom("start"), value: doc.find_atom("value"), body: doc.find_atom("body") }
    }

    /// The `html` element: the root of the boxes. `None` if it is not shown.
    pub fn root(&self) -> Option<NodeId> {
        let html = self.doc.children(self.doc.root()).find(|&n| matches!(self.doc.data(n), NodeData::Element { .. }))?;
        (self.kind(html) != Kind::None).then_some(html)
    }

    /// The style of a box: of the element, or of the parent element for a text node. An anonymous block has the style of
    /// its parent block too (it inherits, and it has no margin, border or padding: layout knows that).
    pub fn style(&self, node: NodeId) -> &'a ComputedStyle {
        let element = match self.doc.data(node) {
            NodeData::Element { .. } => node,
            _ => self.doc.parent(node).expect("a text node has a parent"),
        };
        self.styles.style(element).expect("layout only asks for the style of a box")
    }

    /// What box `node` makes.
    pub fn kind(&self, node: NodeId) -> Kind {
        match *self.doc.data(node) {
            NodeData::Text(_) => Kind::Text,
            // The parent of a text node is shown, or layout would not be here.
            NodeData::Element { name, .. } => {
                let Some(style) = self.styles.style(node) else { return Kind::None };
                let Value::Keyword(display) = style.get(Property::Display) else { return Kind::Block };
                match display {
                    Keyword::None => Kind::None,
                    _ if self.doc.parent(node) == Some(self.doc.root()) => Kind::Block,
                    _ if matches!(style.get(Property::Position), Value::Keyword(Keyword::Absolute | Keyword::Fixed)) => Kind::OutOfFlow,
                    // A child of a flex or grid container is an item: a block, whatever its `display` and `float`
                    // ("blockified", CSS Display 3).
                    Keyword::ListItem if self.is_flex_or_grid(self.doc.parent(node)) => Kind::ListItem,
                    _ if self.is_flex_or_grid(self.doc.parent(node)) => Kind::Block,
                    // The root element is always a block, whatever its `display` (CSS Display 3: "blockify").
                    // `float` does nothing on a box that is out of the flow (CSS 2.1, 9.7).
                    _ if matches!(style.get(Property::Float), Value::Keyword(Keyword::Left | Keyword::Right)) => Kind::Float,
                    // A float is a block, whatever its `display`: a floated `<span>` too.
                    Keyword::Inline if Some(name) == self.br => Kind::Break,
                    Keyword::Inline => Kind::Inline,
                    Keyword::ListItem => Kind::ListItem,
                    Keyword::InlineBlock | Keyword::InlineFlex | Keyword::InlineGrid => Kind::Atomic,
                    _ => Kind::Block,
                    // `block`, `table`, `flex`, `grid` and the table parts. Their layout is a block for now.
                }
            }
            _ => Kind::None,
        }
    }

    /// Is this text all spaces that CSS throws away? Only spaces, tabs and newlines, in a block with `white-space`
    /// `normal` or `nowrap` (`pre` and the others keep them). `\r` and `\x0c` are already gone: the HTML parser turns them into `\n`.
    pub fn is_collapsible_space(&self, text: NodeId) -> bool {
        let NodeData::Text(span) = *self.doc.data(text) else { return false };
        let collapses = matches!(self.style(text).get(Property::WhiteSpace), Value::Keyword(Keyword::Normal | Keyword::Nowrap));
        collapses && self.doc.text(span).bytes().all(|b| matches!(b, b' ' | b'\t' | b'\n'))
    }

    /// The child boxes of a block, in order. See `Children`.
    pub fn children(&self, block: NodeId) -> Children<'_, 'a> {
        // A block holds lines if no child is a block (the common case for `p`, `h1`, `li`), else it holds blocks and
        // the runs of inline content between them become anonymous blocks. This needs one look at all the children first.
        let has_blocks = self.doc.children(block).any(|c| self.kind(c).is_block());
        Children { tree: self, next: self.doc.children(block).next(), has_blocks }
    }

    /// The `overflow-x` and `overflow-y` of a block, as layout uses them. The root element gives its values to the window,
    /// and so does the first `body` if the root has `visible` (CSS Overflow 3, 3.3). Those two are then `visible`
    /// themselves: `body { overflow: hidden }` stops the page from scrolling, and the body is not a scroll container.
    pub fn overflow(&self, node: NodeId) -> (Keyword, Keyword) {
        let get = |n| {
            let s = self.style(n);
            let k = |p| match s.get(p) {
                Value::Keyword(k) => k,
                _ => Keyword::Visible,
            };
            (k(Property::OverflowX), k(Property::OverflowY))
        };
        let visible = (Keyword::Visible, Keyword::Visible);
        let Some(parent) = self.doc.parent(node) else { return visible };
        if parent == self.doc.root() {
            return visible;
        }
        let is_body = |n| matches!(*self.doc.data(n), NodeData::Element { name, .. } if Some(name) == self.body);
        if is_body(node) && self.doc.parent(parent) == Some(self.doc.root()) && get(parent) == visible && self.doc.children(parent).find(|&c| is_body(c)) == Some(node) {
            return visible;
        }
        get(node)
    }

    /// Is this element a flex or a grid container? Its children are then items, not blocks or inlines.
    pub fn is_flex_or_grid(&self, node: Option<NodeId>) -> bool {
        let Some(node) = node else { return false };
        let Some(style) = self.styles.style(node) else { return false };
        matches!(style.get(Property::Display), Value::Keyword(Keyword::Flex | Keyword::InlineFlex | Keyword::Grid | Keyword::InlineGrid))
    }

    /// The `position` of a box.
    pub fn position(&self, node: NodeId) -> Keyword {
        match self.style(node).get(Property::Position) {
            Value::Keyword(k) => k,
            _ => Keyword::Static,
        }
    }

    /// Is this block a scroll container: does it hide or scroll what does not fit? `clip` hides it too, but it is not one.
    pub fn scrolls(&self, node: NodeId) -> bool {
        matches!(self.overflow(node).0, Keyword::Hidden | Keyword::Scroll | Keyword::Auto)
    }

    /// The number of a list item in its list, for its marker. Call it for the list items of one parent in order, with one
    /// `Counter` for that parent.
    pub fn list_number(&self, counter: &mut Counter, item: NodeId) -> i32 {
        // `<li value=7>` sets the number, and the next items go on from it (HTML standard, "ordinal value").
        let set = self.value.and_then(|a| self.doc.attr(item, a)).and_then(|v| v.trim().parse::<i32>().ok());
        counter.next = match set {
            Some(v) => v,
            None => counter.next,
        };
        let n = counter.next;
        counter.next = n.saturating_add(1);
        n
    }

    /// A counter for the list items of `parent`. `<ol start=5>` starts at 5, other lists at 1.
    pub fn counter(&self, parent: NodeId) -> Counter {
        let is_ol = matches!(*self.doc.data(parent), NodeData::Element { name, .. } if Some(name) == self.ol);
        let start = self.start.filter(|_| is_ol).and_then(|a| self.doc.attr(parent, a)).and_then(|v| v.trim().parse::<i32>().ok());
        Counter { next: start.unwrap_or(1) }
    }
}

/// The next number for a list item. See `Tree::counter`. A counter for each parent, and no walk back over the siblings
/// for each item: a list of 100,000 items costs one step for each item, not 100,000.
pub struct Counter {
    next: i32,
}

/// The text of a list marker, with the space after it, as Chrome draws it: `• `, `◦ `, `▪ `, `3. `. `None` for
/// `list-style-type: none`.
pub fn marker_text(style: &ComputedStyle, number: i32) -> Option<String> {
    match style.get(Property::ListStyleType) {
        Value::Keyword(Keyword::Disc) => Some("\u{2022} ".to_string()),
        Value::Keyword(Keyword::Circle) => Some("\u{25e6} ".to_string()),
        Value::Keyword(Keyword::Square) => Some("\u{25aa} ".to_string()),
        Value::Keyword(Keyword::Decimal) => Some(format!("{number}. ")),
        _ => None,
    }
}

/// One child box of a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Child {
    /// A block-level child element: `Kind::Block` or `Kind::ListItem`.
    Block(NodeId),
    /// Inline content: the DOM siblings from the first to the last, both included. If the block also has block
    /// children, this is an anonymous block. Layout makes lines of it (an "inline formatting context").
    Lines(NodeId, NodeId),
}

/// The child boxes of a block. JS: like a generator function that yields `Block` or `Lines` items.
/// Children that make no box are skipped, and so is a run of inline content that is only spaces, between blocks.
pub struct Children<'t, 'a> {
    tree: &'t Tree<'a>,
    next: Option<NodeId>,
    has_blocks: bool,
}

impl Iterator for Children<'_, '_> {
    type Item = Child;

    fn next(&mut self) -> Option<Child> {
        let tree = self.tree;
        loop {
            let node = self.next?;
            // `?` on an `Option`: stop with `None` when there is no next child. JS: `if (node == null) return null`.
            self.next = tree.doc.next_sibling(node);
            let kind = tree.kind(node);
            if kind.is_block() {
                return Some(Child::Block(node));
            }
            if !kind.is_inline() {
                continue;
            }
            // A run of inline content: go on until the next block, or the end.
            let (first, mut last) = (node, node);
            let mut only_space = kind == Kind::Text && tree.is_collapsible_space(node);
            while let Some(n) = self.next {
                let k = tree.kind(n);
                if k.is_block() {
                    break;
                }
                self.next = tree.doc.next_sibling(n);
                if k.is_inline() {
                    last = n;
                    only_space = only_space && k == Kind::Text && tree.is_collapsible_space(n);
                }
            }
            if self.has_blocks && only_space {
                continue;
            }
            return Some(Child::Lines(first, last));
        }
    }
}

/// The box tree as text, one box on each line, for tests. Anonymous blocks are `(lines)`.
pub fn dump(tree: &Tree) -> String {
    /// One thing to print: a child of a block, or a node inside a run of lines.
    enum Item {
        Child(Child),
        Inline(NodeId),
    }
    let mut out = String::new();
    let Some(root) = tree.root() else { return out };
    let mut stack = vec![(Item::Child(Child::Block(root)), 0)];
    // A loop with a stack, and not a function that calls itself: a deep tree cannot overflow the call stack.
    // Children go on the stack in reverse, so that the first child comes out first.
    let blocks = |node: NodeId, depth: usize, stack: &mut Vec<(Item, usize)>| {
        let kids: Vec<_> = tree.children(node).collect();
        stack.extend(kids.into_iter().rev().map(|c| (Item::Child(c), depth + 1)));
    };
    while let Some((item, depth)) = stack.pop() {
        let indent = "  ".repeat(depth);
        match item {
            Item::Child(Child::Block(node)) => {
                let label = if tree.kind(node) == Kind::ListItem { "list-item" } else { "block" };
                out += &format!("{indent}{label} {}\n", name(tree, node));
                blocks(node, depth, &mut stack);
            }
            Item::Child(Child::Lines(first, last)) => {
                out += &format!("{indent}(lines)\n");
                let mut run = vec![first];
                while *run.last().unwrap() != last {
                    run.push(tree.doc.next_sibling(*run.last().unwrap()).unwrap());
                }
                stack.extend(run.into_iter().rev().map(|n| (Item::Inline(n), depth + 1)));
            }
            Item::Inline(node) => match tree.kind(node) {
                Kind::Text => out += &format!("{indent}text {:?}\n", text(tree, node)),
                Kind::Break => out += &format!("{indent}break\n"),
                Kind::Inline => {
                    out += &format!("{indent}inline {}\n", name(tree, node));
                    let kids: Vec<_> = tree.doc.children(node).collect();
                    stack.extend(kids.into_iter().rev().map(|c| (Item::Inline(c), depth + 1)));
                }
                Kind::Atomic | Kind::Float | Kind::OutOfFlow => {
                    let label = match tree.kind(node) {
                        Kind::Float => "float",
                        Kind::OutOfFlow => "out of flow",
                        _ => "atomic",
                    };
                    out += &format!("{indent}{label} {}\n", name(tree, node));
                    blocks(node, depth, &mut stack);
                }
                Kind::Block | Kind::ListItem => {
                    // A block inside an inline box (`<span><div>x</div></span>`). See `inline.rs`.
                    out += &format!("{indent}block-in-inline {}\n", name(tree, node));
                    blocks(node, depth, &mut stack);
                }
                Kind::None => {}
            },
        }
    }
    out
}

fn name(tree: &Tree, node: NodeId) -> String {
    match *tree.doc.data(node) {
        NodeData::Element { name, .. } => tree.doc.atom_str(name).to_string(),
        _ => String::new(),
    }
}

fn text(tree: &Tree, node: NodeId) -> String {
    match *tree.doc.data(node) {
        NodeData::Text(span) => tree.doc.text(span).to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bare_css::cascade::{Origin, RuleSet, UA_CSS};
    use bare_html::tree::parse;

    /// The box tree of a page with the browser style sheet and one author sheet.
    fn boxes(html: &str, css: &str) -> String {
        let mut doc = parse(html);
        let mut rules = RuleSet::new((800.0, 600.0));
        rules.add(&mut doc, UA_CSS, Origin::UserAgent);
        rules.add(&mut doc, css, Origin::Author);
        let styles = rules.compute(&doc);
        dump(&Tree::new(&doc, &styles))
    }

    #[test]
    fn head_and_display_none_make_no_box() {
        let out = boxes("<title>t</title><p>a</p><div style='display:none'><p>b</p></div>", "");
        assert_eq!(out, "block html\n  block body\n    block p\n      (lines)\n        text \"a\"\n");
    }

    #[test]
    fn the_children_of_a_flex_or_grid_container_are_blocks() {
        // A span, an inline-block and a float become items. The text between them is an anonymous item. An absolute
        // box stays out of the flow, and a list item stays a list item.
        let out = boxes("<div style='display:flex'><span>a</span>b<i style='display:inline-block'>c</i><b style='float:left'>d</b><u style='position:absolute'>e</u><li>f</li></div>", "");
        let inside: Vec<_> = out.lines().skip(3).filter(|l| l.starts_with("      ") && !l.starts_with("        ")).collect();
        assert_eq!(inside, ["      block span", "      (lines)", "      block i", "      block b", "      (lines)", "      list-item li"], "{out}");
        let out = boxes("<div style='display:grid'><span>a</span></div>", "");
        assert!(out.contains("block span"), "{out}");
    }

    #[test]
    fn text_between_blocks_gets_an_anonymous_block() {
        let out = boxes("<div>one<p>two</p>three <b>four</b></div>", "");
        assert_eq!(
            out,
            "block html\n  block body\n    block div\n      (lines)\n        text \"one\"\n      block p\n        (lines)\n          text \"two\"\n      (lines)\n        text \"three \"\n        inline b\n          text \"four\"\n"
        );
    }

    #[test]
    fn spaces_between_blocks_make_no_box() {
        let out = boxes("<div>\n  <p>a</p>\n  <p>b</p>\n</div>", "");
        assert_eq!(out, "block html\n  block body\n    block div\n      block p\n        (lines)\n          text \"a\"\n      block p\n        (lines)\n          text \"b\"\n");
        // With `white-space: pre` the spaces stay, so they make anonymous blocks.
        let pre = boxes("<div>\n<p>a</p></div>", "div { white-space: pre }");
        assert!(pre.contains("(lines)\n        text \"\\n\""), "{pre}");
        // Spaces in a block with no block children stay: they may be the space between two words.
        let line = boxes("<p><b>a</b> <i>b</i></p>", "");
        assert!(line.contains("text \" \""), "{line}");
    }

    #[test]
    fn display_values_map_to_kinds() {
        let out = boxes(
            "<span id=a>x</span><br><img><li>y</li><table><tr><td>z</td></tr></table>",
            "#a { display: inline-block }",
        );
        assert_eq!(
            out,
            "block html\n  block body\n    (lines)\n      atomic span\n        (lines)\n          text \"x\"\n      break\n      atomic img\n    list-item li\n      (lines)\n        text \"y\"\n    block table\n      block tbody\n        block tr\n          block td\n            (lines)\n              text \"z\"\n"
        );
    }

    #[test]
    fn a_block_inside_an_inline() {
        let out = boxes("<span>a<div>b</div>c</span>", "");
        assert_eq!(
            out,
            "block html\n  block body\n    (lines)\n      inline span\n        text \"a\"\n        block-in-inline div\n          (lines)\n            text \"b\"\n        text \"c\"\n"
        );
    }

    #[test]
    fn the_root_is_a_block() {
        let out = boxes("x", "html { display: inline }");
        assert!(out.starts_with("block html\n"), "{out}");
        let none = boxes("x", "html { display: none }");
        assert_eq!(none, "");
    }

    #[test]
    fn list_numbers() {
        let mut doc = parse("<ol start=5><li>a<li value=10>b<li>c</ol><ul><li>d</ul>");
        let mut rules = RuleSet::new((800.0, 600.0));
        rules.add(&mut doc, UA_CSS, Origin::UserAgent);
        let styles = rules.compute(&doc);
        let tree = Tree::new(&doc, &styles);
        let body = tree.children(tree.root().unwrap()).find_map(|c| match c {
            Child::Block(b) => Some(b),
            _ => None,
        });
        let lists: Vec<NodeId> = doc.children(body.unwrap()).collect();
        let numbers = |list: NodeId| {
            let mut counter = tree.counter(list);
            doc.children(list).map(|li| tree.list_number(&mut counter, li)).collect::<Vec<_>>()
        };
        assert_eq!(numbers(lists[0]), [5, 10, 11]);
        assert_eq!(numbers(lists[1]), [1]);
        let li = doc.children(lists[0]).next().unwrap();
        assert_eq!(marker_text(tree.style(li), 5).as_deref(), Some("5. "));
        let li = doc.children(lists[1]).next().unwrap();
        assert_eq!(marker_text(tree.style(li), 1).as_deref(), Some("\u{2022} "));
    }

    #[test]
    fn a_deep_tree_does_not_overflow_the_stack() {
        let html = "<div>".repeat(50_000);
        let out = boxes(&html, "");
        assert_eq!(out.lines().count(), 50_002);
    }
}
