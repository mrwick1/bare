//! Layout: find the place and the size of every box on the page, from the DOM and the computed styles.
//!
//! `layout` is the way in. It walks the DOM once, from the `html` element down (`block.rs`, `inline.rs`), and writes a
//! list of fragments (`fragment.rs`): one rectangle for each box and each piece of text on a line.

pub mod block;
pub mod boxes;
// `pub mod` makes the module visible to other crates: `bare_layout::boxes`.
pub mod font;
pub mod fragment;
pub mod inline;
pub mod text;

use bare_css::cascade::Styles;
use bare_html::dom::{Document, NodeData, NodeId};
use block::Space;
use fragment::{Fragment, Layout};

/// What all of layout shares while it runs.
pub struct Cx<'a> {
    pub tree: boxes::Tree<'a>,
    /// The fragments so far.
    pub out: Vec<Fragment>,
    /// How many blocks deep the walk is now. See `block::MAX_DEPTH`.
    pub depth: usize,
    /// A list item whose marker is not placed yet. The first line inside it takes it.
    pub marker: Option<NodeId>,
    /// The number of the last list item, for its marker.
    pub number: i32,
}

/// Lay out a page in a window of `viewport` (width, height) CSS pixels.
pub fn layout(doc: &Document, styles: &Styles, viewport: (f32, f32)) -> Layout {
    let mut cx = Cx { tree: boxes::Tree::new(doc, styles), out: Vec::with_capacity(doc.len()), depth: 0, marker: None, number: 0 };
    // Most pages make about one fragment for each DOM node: one for each element and each text on one line, none for
    // the `head` and for the spaces between blocks. Room for that many at once saves the copies of a `Vec` that grows.
    let mut height = 0.0;
    if let Some(root) = cx.tree.root() {
        // The root element sits in the window (the "initial containing block"). It makes its own block formatting
        // context, so its margins stay apart from the margins inside it.
        let done = block::block(&mut cx, root, Space { width: viewport.0, height: Some(viewport.1) });
        cx.out[done.fragment].rect.y = done.margin_top;
        height = done.margin_top + done.height + done.margin_bottom;
    }
    fragment::absolute(&mut cx.out);
    Layout { fragments: cx.out, width: viewport.0, height }
}

/// The fragments as text, one on each line, for tests and for `bare --layout`: the name, then `x,y wxh`. Text fragments
/// show their text.
pub fn dump(doc: &Document, layout: &Layout) -> String {
    let mut out = String::new();
    let mut ends: Vec<u32> = Vec::new();
    for (i, f) in layout.fragments.iter().enumerate() {
        while ends.last().is_some_and(|&e| e as usize <= i) {
            ends.pop();
        }
        out += &"  ".repeat(ends.len());
        let r = f.rect;
        let what = match (f.kind, doc.data(f.node)) {
            (fragment::Kind::Box, &NodeData::Element { name, .. }) => doc.atom_str(name).to_string(),
            (fragment::Kind::Text { start, end }, &NodeData::Text(span)) => format!("{:?}", &doc.text(span)[start as usize..end as usize]),
            (fragment::Kind::Marker { number }, _) => format!("marker {number}"),
            _ => String::from("?"),
        };
        out += &format!("{what} {},{} {}x{}\n", r.x, r.y, r.w, r.h);
        if f.end as usize > i + 1 {
            ends.push(f.end);
        }
    }
    out
}
