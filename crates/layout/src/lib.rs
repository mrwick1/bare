//! Layout: find the place and the size of every box on the page, from the DOM and the computed styles.
//!
//! `layout` is the way in. It walks the DOM once, from the `html` element down (`block.rs`, `inline.rs`), and writes a
//! list of fragments (`fragment.rs`): one rectangle for each box and each piece of text on a line.

pub mod block;
pub mod boxes;
pub mod controls;
pub mod flex;
pub mod floats;
// `pub mod` makes the module visible to other crates: `bare_layout::boxes`.
pub mod font;
pub mod fragment;
pub mod grid;
pub mod inline;
pub mod intrinsic;
mod memo;
pub mod scroll;
pub mod table;
pub mod text;

use bare_css::cascade::Styles;
use bare_html::dom::{Atom, Document, Fnv, NodeData, NodeId};
use std::collections::HashMap;
use std::hash::BuildHasherDefault;
use block::{Pos, Space};
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
    /// The intrinsic widths of the boxes measured so far, by `NodeId::index` (`intrinsic.rs`).
    pub sizes: Fast<(f32, f32)>,
    /// How many intrinsic measures are running, one inside the other.
    pub measuring: u32,
    /// Empty lists of fragments, to use again for the next inline-block (`inline.rs`): no new allocation for each one.
    pub spare: Vec<Vec<Fragment>>,
    /// The floats of the block formatting context that is laid out now (`floats.rs`).
    pub floats: floats::Floats,
    /// The `overflow: auto` boxes that layout found to need scrollbars: (right, bottom), by `NodeId::index`.
    pub bars: Fast<(bool, bool)>,
    /// How many layouts run again inside one another because a box found that it needs a scrollbar (`block::sized`).
    pub again: u32,
    /// The offsets of the boxes with `position: relative`, by `NodeId::index`. They move at the end of layout.
    pub rel: Fast<(f32, f32)>,
    /// The page has a box with `position: fixed`.
    pub fixed: bool,
    /// The static positions that wait for their containing block. With none, no walk is needed.
    pub waiting: u32,
    /// How many percents met a base that is not known (`block::resolve`). A flex item whose layout met none does not
    /// depend on its height: to stretch it, layout changes only its height (`flex.rs`).
    pub unknown: std::cell::Cell<u32>,
    /// The work that layout did more than once, in fragments: those of a layout that a flex or grid container threw away to lay the
    /// item out again, those of a scrollbar pass that was thrown away, and those that a kept layout put back (`memo.rs`). A page that
    /// lays nothing out twice has 0. Layout stops going deeper when it passes `budget` (`layout_within`).
    pub work: u64,
    pub budget: u64,
    /// How many boxes so far could run their content again for a scrollbar (`block::sized`). A layout that adds none does not read `bars`.
    pub scrolls: u32,
    /// How many flex and grid items so far were laid out (`Cx::lay_item`). A box that lays out none inside is cheap to run again: it is not kept.
    pub(crate) items: u32,
    /// `lay_item` keeps and finds results once `work` is this much (`memo.rs`).
    pub(crate) memo_from: u64,
    /// How many `lay_item` calls are running. Only inside one is `fx` written, because only a result of `lay_item` is put back.
    pub(crate) open: u32,
    /// What layout wrote by node (`rel`, `bars`), in order, and the results kept for boxes that run again (`memo.rs`).
    pub(crate) fx: Vec<memo::Effect>,
    pub(crate) memo: memo::Memo,
}

/// A table by `NodeId::index`. The page picks the keys, and it can only slow down its own layout, so the fast hash of `Document` is enough.
pub type Fast<V> = HashMap<u32, V, BuildHasherDefault<Fnv>>;

/// Lay out a page in a window of `viewport` (width, height) CSS pixels.
pub fn layout(doc: &Document, styles: &Styles, viewport: (f32, f32)) -> Layout {
    layout_within(doc, styles, viewport, u64::MAX)
}

/// `layout`, but it stops going deeper once it has done `budget` units of work (`Cx::work`). The fragments are then
/// incomplete: the caller must look at `Layout::work` and throw the result away when it is more than `budget`. Layout
/// cannot be interrupted, so this is how a caller bounds the time of a page that it does not trust.
pub fn layout_within(doc: &Document, styles: &Styles, viewport: (f32, f32), budget: u64) -> Layout {
    let mut cx = Cx { tree: boxes::Tree::new(doc, styles), out: Vec::with_capacity(doc.len()), depth: 0, marker: None, number: 0, sizes: Fast::default(), measuring: 0, spare: Vec::new(), floats: Default::default(), bars: Fast::default(), again: 0, rel: Fast::default(), fixed: false, waiting: 0, unknown: Default::default(), work: 0, budget, scrolls: 0, items: 0, memo_from: memo::start().min(budget), open: 0, fx: Vec::new(), memo: Default::default() };
    // Most pages make about one fragment for each DOM node: one for each element and each text on one line, none for
    // the `head` and for the spaces between blocks. Room for that many at once saves the copies of a `Vec` that grows.
    let mut height = 0.0;
    if let Some(root) = cx.tree.root() {
        // The root element sits in the window (the "initial containing block"). It makes its own block formatting
        // context, so its margins stay apart from the margins inside it.
        let done = block::block(&mut cx, root, Space { width: viewport.0, height: Some(viewport.1) }, Pos::default());
        let root_box = cx.out[done.fragment].rect;
        cx.out[done.fragment].rect.y = done.margin_top;
        height = done.margin_top + done.height + done.margin_bottom;
        // The window is the containing block of the absolute boxes with no positioned block above them, and of the
        // `fixed` ones (the page is not scrolled). A positioned root took its own absolute boxes already.
        let window = fragment::Rect { x: -root_box.x, y: -done.margin_top, w: viewport.0, h: viewport.1 };
        let abs = cx.tree.position(root) == bare_css::values::Keyword::Static;
        if (abs || cx.fixed) && cx.waiting > 0 {
            block::out_of_flow(&mut cx, done.fragment, window, true, abs);
        }
        cx.out[done.fragment].end = cx.out.len() as u32;
    }
    fragment::drop_statics(&mut cx.out);
    // `position: relative` moves a block with all that is inside it. Inline boxes moved when their line was written.
    if !cx.rel.is_empty() {
        for f in &mut cx.out {
            if f.kind == fragment::Kind::Box
                && !matches!(cx.tree.kind(f.node), boxes::Kind::Inline)
                && let Some(&(dx, dy)) = cx.rel.get(&(f.node.index() as u32))
            {
                f.rect.x += dx;
                f.rect.y += dy;
            }
        }
    }
    fragment::absolute(&mut cx.out);
    Layout { fragments: cx.out, width: viewport.0, height, bars: cx.bars, work: cx.work }
}

/// The fragments as text, one on each line, for tests and for `bare --layout`: the name (with `#id` if the element has
/// one), then `x,y wxh`. Text fragments show their text.
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
            (fragment::Kind::Box, &NodeData::Element { name, .. }) => match doc.attr(f.node, Atom::ID) {
                Some(id) => format!("{}#{id}", doc.atom_str(name)),
                None => doc.atom_str(name).to_string(),
            },
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
