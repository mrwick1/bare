//! Fragments: the result of layout. A fragment is one rectangle on the page: the box of an element, a piece of text in
//! one line, or a list marker. Paint (module 5) draws them in order.
//!
//! Chrome (LayoutNG) writes the same kind of result: an immutable "fragment tree", where each fragment holds its size
//! and the place of its children relative to itself. Firefox writes the place and size into the frame (the box) itself.
//!
//! We keep all fragments in one `Vec`, in tree order (a parent, then its children), so paint reads them front to back
//! with no pointers. `end` is the place after the last fragment inside this one, so a whole subtree can be skipped
//! (for example, a box that is outside the window). While layout runs, a place is relative to the parent fragment:
//! a block can move after its children are done (margin collapsing moves it), and its children move with it for free.
//! At the end, `absolute` turns all places into page coordinates, in one pass.

use bare_html::dom::NodeId;

/// A rectangle in CSS pixels. `x` and `y` are its top-left corner.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// What a fragment shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// The border box of an element: a block, an inline box (one piece for each line it is on), or an atomic inline.
    Box,
    /// A piece of a text node on one line: the bytes `start..end` of the node's text. Paint reads them from the DOM, and
    /// shows a run of collapsible spaces as one space (`text::shown`).
    Text { start: u32, end: u32 },
    /// The marker of a list item (`•`, `3.`). `number` is the place of the item in its list. `boxes::marker_text`
    /// gives the text.
    Marker { number: i32 },
    /// The static position of a box with `position: absolute` or `fixed`: where its margin box would start with
    /// `position: static`. Zero size. Layout uses it to place the box, and takes it out at the end.
    Static,
}

/// One rectangle of the result.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fragment {
    pub rect: Rect,
    /// The element, or the text node. For a marker, the list item.
    pub node: NodeId,
    pub kind: Kind,
    /// The place after the last fragment inside this one. A fragment with nothing inside has `end` = its place + 1.
    pub end: u32,
}

/// The result of layout.
pub struct Layout {
    /// All fragments, in tree order, with places on the page.
    pub fragments: Vec<Fragment>,
    /// The size of the page: the width of the window, and the bottom of the root element's margin box. The page scrolls
    /// if `height` is more than the window.
    pub width: f32,
    pub height: f32,
    /// The scrollbars of the `overflow: auto` boxes that needed them: (right, bottom), by `NodeId::index`. `scroll.rs` reads it.
    pub bars: std::collections::HashMap<u32, (bool, bool)>,
}

/// Turn the places of all fragments from "relative to the parent fragment" into page coordinates.
pub fn absolute(fragments: &mut [Fragment]) {
    // The open fragments: (the place after their last child, their corner on the page).
    let mut open: Vec<(u32, f32, f32)> = Vec::new();
    for (i, f) in fragments.iter_mut().enumerate() {
        while open.last().is_some_and(|&(end, _, _)| end as usize <= i) {
            open.pop();
        }
        let (dx, dy) = open.last().map_or((0.0, 0.0), |&(_, x, y)| (x, y));
        f.rect.x += dx;
        f.rect.y += dy;
        if f.end as usize > i + 1 {
            open.push((f.end, f.rect.x, f.rect.y));
        }
    }
}

/// Take out the `Static` fragments. They have nothing inside, so each `end` goes down by the number of them before it.
pub fn drop_statics(fragments: &mut Vec<Fragment>) {
    // `before[i]`: how many `Static` fragments come before the place `i`.
    let mut before = Vec::with_capacity(fragments.len() + 1);
    let mut n = 0u32;
    for f in fragments.iter() {
        before.push(n);
        n += (f.kind == Kind::Static) as u32;
    }
    before.push(n);
    if n == 0 {
        return;
    }
    fragments.retain(|f| f.kind != Kind::Static);
    for f in fragments.iter_mut() {
        f.end -= before[f.end as usize];
    }
}

/// Move the fragments from `from` to the end by `dx`, `dy`. Only the top ones move: the ones inside them are relative.
pub fn shift(fragments: &mut [Fragment], from: usize, dx: f32, dy: f32) {
    let mut i = from;
    while i < fragments.len() {
        fragments[i].rect.x += dx;
        fragments[i].rect.y += dy;
        i = fragments[i].end as usize;
    }
}
