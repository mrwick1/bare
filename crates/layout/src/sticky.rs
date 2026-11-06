//! `position: sticky` (CSS Positioned Layout 3, 3.4): a box that stays in view while its scroll container scrolls, and never leaves
//! its containing block.
//!
//! Layout puts a sticky box in the flow, where `static` would put it. The offset that keeps it in view depends on the scroll offsets, so
//! it is not part of layout: `scroll::areas` records the constraints once for each layout (`Sticky`), and `Areas::stuck` gives the offset for
//! the offsets of one moment. Paint, hit testing and the script geometry add it (as they add a scroll offset). A scroll never lays out again.
//!
//! The rules (all checked against headless Chrome, `tools/fixtures/ui/sticky-cases.txt`):
//! - The box sticks in its nearest scroll container (or in the window). The constraining rectangle is the padding box of the container
//!   without its scrollbars and without its padding, moved by the scroll offset. A percent inset is of its width (`left`, `right`) or height.
//! - The border box must stay `top` below the top edge, and `bottom` above the bottom edge (the same across). `auto` is no constraint.
//!   The bottom edge is done first, then the top edge: when both cannot hold, the top wins.
//! - The containing block is the content box of the nearest block-level ancestor (of the table, for a table part). When that is the scroll
//!   container itself, it is the whole scrollable overflow. The margin box of the sticky box stays inside it. The offset never moves the box
//!   against its inset (a box already past the edge of its containing block stays at its normal place).
//! - A sticky box inside a sticky box moves with it: the inner box is constrained in the place that the outer one has now.
//! - Everything inside a sticky box moves with it. A sticky box is a stacking context (`bare-paint`).
//!
//! Gates (`Areas::gate`): a sticky inline box that is not atomic (it has one piece for each line).
//!
//! Not modelled: `scroll-padding`, a percent padding or margin of the box and of its containing block (they count as 0, as in `scroll.rs`), the
//! wider edge of a collapsed table border (the table border counts, not the cells), `writing-mode`, transforms (the engine has none).

use crate::boxes::{Kind as BoxKind, Tree};
use crate::fragment::{Kind, Layout, Rect};
use crate::scroll::Areas;
use bare_css::cascade::ComputedStyle;
use bare_css::values::{Keyword, Length, Property, Value};
use bare_html::dom::NodeId;

/// No parent and no scroll container.
pub const NONE: u32 = u32::MAX;

/// One sticky box and what limits it. All rectangles are in page coordinates, with no scroll offset and no sticky offset.
#[derive(Debug, Clone, Copy)]
pub struct Sticky {
    pub node: NodeId,
    /// The place of its box in `Layout::fragments`.
    pub frag: u32,
    /// The nearest sticky box above it, in `Areas::sticky` (`NONE`: no one).
    pub parent: u32,
    /// The scroll container that it sticks in, in `Areas::list` (`NONE`: the window).
    pub scroller: u32,
    /// The constraining rectangle: the scrollport without the padding of the container.
    pub view: Rect,
    /// The border box.
    pub rect: Rect,
    /// The content box of the containing block.
    pub cb: Rect,
    /// The margins of the box: top, right, bottom, left.
    pub margin: [f32; 4],
    /// The insets: top, right, bottom, left (`None`: `auto`). A percent is resolved already.
    pub inset: [Option<f32>; 4],
}

/// How far the box moves along one axis. `v0..v1`: the constraining rectangle. `b0..b1`: the border box. `m0`, `m1`: its margins.
/// `c0..c1`: the content box of its containing block. `i0`, `i1`: the insets at the start and at the end.
fn axis(v: (f32, f32), b: (f32, f32), m: (f32, f32), c: (f32, f32), i: (Option<f32>, Option<f32>)) -> f32 {
    let mut d = 0.0;
    if let Some(end) = i.1 {
        // Up, until the box is `end` above the bottom edge, but not above the top of its containing block.
        d = (v.1 - end - b.1).min(0.0).max(((c.0 + m.0) - b.0).min(0.0));
    }
    if let Some(start) = i.0 {
        // Down, until the box is `start` below the top edge, but not below the bottom of its containing block, and never up.
        let up = (v.0 + start - (b.0 + d)).max(0.0);
        d += up.min(((c.1 - m.1) - (b.1 + d)).max(0.0));
    }
    d
}

impl Sticky {
    /// The offset of this box, for the offset `a` of the sticky boxes above it in the same scroll container, and the scroll offset `s` of
    /// its container.
    fn offset(&self, a: (f32, f32), s: (f32, f32)) -> (f32, f32) {
        let m = &self.margin;
        let (r, c, v) = (self.rect, self.cb, self.view);
        let (ax, ay) = a;
        let x = axis((v.x + s.0, v.x + s.0 + v.w), (r.x + ax, r.x + ax + r.w), (m[3], m[1]), (c.x + ax, c.x + ax + c.w), (self.inset[3], self.inset[1]));
        let y = axis((v.y + s.1, v.y + s.1 + v.h), (r.y + ay, r.y + ay + r.h), (m[0], m[2]), (c.y + ay, c.y + ay + c.h), (self.inset[0], self.inset[2]));
        (x, y)
    }
}

impl Areas {
    /// The place in `sticky` of the box of `node`, if it is a sticky box.
    pub fn sticky_of(&self, node: NodeId) -> Option<usize> {
        self.sticky_index.get(&(node.index() as u32)).map(|&k| k as usize)
    }

    /// The offsets of the sticky box `k`, for these scroll offsets: `page` and `of` (the offset of a scroll container). Gives its own
    /// offset (what paint adds around it) and the offset of the box with all the sticky boxes above it (what it moves from its normal place).
    /// One step for each sticky box above it.
    pub fn stuck(&self, k: usize, page: (f32, f32), of: &impl Fn(NodeId) -> (f32, f32)) -> ((f32, f32), (f32, f32)) {
        let (_, own, total) = self.solve(k, page, of);
        (own, total)
    }

    /// (the offset of this box and of the boxes above it in its scroll container, its own offset, its offset with all the boxes above it).
    fn solve(&self, k: usize, page: (f32, f32), of: &impl Fn(NodeId) -> (f32, f32)) -> ((f32, f32), (f32, f32), (f32, f32)) {
        let st = &self.sticky[k];
        let (above, above_total) = match st.parent {
            NONE => ((0.0, 0.0), (0.0, 0.0)),
            p => {
                let (inside, _, total) = self.solve(p as usize, page, of);
                // A sticky box in another scroll container moves the container, and the scrollport with it: it does not change the
                // place of this box in the container.
                (if self.sticky[p as usize].scroller == st.scroller { inside } else { (0.0, 0.0) }, total)
            }
        };
        let s = match st.scroller {
            NONE => page,
            i => of(self.list[i as usize].node),
        };
        let s = (s.0.round(), s.1.round());
        let own = st.offset(above, s);
        ((above.0 + own.0, above.1 + own.1), own, (above_total.0 + own.0, above_total.1 + own.1))
    }
}

fn inset(style: &ComputedStyle, p: Property, of: f32, tree: &Tree) -> Option<f32> {
    match style.get(p) {
        Value::Length(Length::Px(x)) => Some(x),
        Value::Length(Length::Percent(pc)) => Some(of * pc / 100.0),
        Value::Length(Length::Calc(i)) => {
            let (px, pc) = tree.styles.calc(i);
            Some(px + of * pc / 100.0)
        }
        _ => None,
    }
}

/// The content box of the box of the fragment `f`: its border box without its borders, padding and scrollbars. A table part uses the borders that
/// layout used for the table: half of its own border (the collapsed model has no padding).
fn content_box(areas: &Areas, tree: &Tree, f: &crate::fragment::Fragment) -> Rect {
    let style = tree.style(f.node);
    let w = |p| style.px(p).max(0.0);
    let table = matches!(style.get(Property::Display), Value::Keyword(Keyword::Table | Keyword::InlineTable));
    let collapsed = table && style.get(Property::BorderCollapse) == Value::Keyword(Keyword::Collapse);
    let edge = |border, padding| if collapsed { w(border) / 2.0 } else { w(border) + w(padding) };
    let (l, t, r, b) = (edge(Property::BorderLeftWidth, Property::PaddingLeft), edge(Property::BorderTopWidth, Property::PaddingTop), edge(Property::BorderRightWidth, Property::PaddingRight), edge(Property::BorderBottomWidth, Property::PaddingBottom));
    let mut out = Rect { x: f.rect.x + l, y: f.rect.y + t, w: (f.rect.w - l - r).max(0.0), h: (f.rect.h - t - b).max(0.0) };
    // A scroll container: its content is the whole scrollable overflow (it starts at the padding box and holds the padding at the end).
    if let Some(a) = areas.get(f.node) {
        out.w = a.content.0 - (l - w(Property::BorderLeftWidth)) - (r - w(Property::BorderRightWidth));
        out.h = a.content.1 - (t - w(Property::BorderTopWidth)) - (b - w(Property::BorderBottomWidth));
        out.w = out.w.max(0.0);
        out.h = out.h.max(0.0);
    }
    out
}

/// Fill the constraints of the sticky boxes that `scroll::areas` found: one more pass over the fragments up to the last one. A page with no
/// sticky box never comes here.
pub(crate) fn finish(areas: &mut Areas, tree: &Tree, layout: &Layout) {
    let frags = &layout.fragments;
    let last = areas.sticky.last().expect("a sticky box").frag as usize;
    // The block-level boxes that are open: (their end, their place).
    let mut open: Vec<(u32, u32)> = Vec::new();
    let mut k = 0;
    for (i, f) in frags.iter().enumerate().take(last + 1) {
        while open.last().is_some_and(|o| o.0 as usize <= i) {
            open.pop();
        }
        if areas.sticky[k].frag as usize == i {
            let style = tree.style(f.node);
            let display = style.get(Property::Display);
            let table_part = matches!(display, Value::Keyword(Keyword::TableCell | Keyword::TableRow | Keyword::TableRowGroup | Keyword::TableHeaderGroup | Keyword::TableFooterGroup));
            // The containing block of a table part is the table (and a loose part has the box around it).
            let cb = table_part.then(|| open.iter().rev().find(|o| matches!(tree.style(frags[o.1 as usize].node).get(Property::Display), Value::Keyword(Keyword::Table | Keyword::InlineTable)))).flatten().or(open.last());
            let cb = cb.map_or(Rect { x: 0.0, y: 0.0, w: layout.width, h: layout.height }, |o| content_box(areas, tree, &frags[o.1 as usize]));
            let view = match areas.sticky[k].scroller {
                NONE => Rect { x: 0.0, y: 0.0, w: layout.view.0, h: layout.view.1 },
                s => {
                    let a = &areas.list[s as usize];
                    let sty = tree.style(a.node);
                    let w = |p| sty.px(p).max(0.0);
                    let (l, t, r, b) = (w(Property::PaddingLeft), w(Property::PaddingTop), w(Property::PaddingRight), w(Property::PaddingBottom));
                    let c = a.client();
                    Rect { x: a.pad.x + l, y: a.pad.y + t, w: (c.0 - l - r).max(0.0), h: (c.1 - t - b).max(0.0) }
                }
            };
            let m = |p| style.px(p);
            let s = &mut areas.sticky[k];
            s.view = view;
            s.rect = f.rect;
            s.cb = cb;
            s.margin = [m(Property::MarginTop), m(Property::MarginRight), m(Property::MarginBottom), m(Property::MarginLeft)];
            s.inset = [inset(style, Property::Top, view.h, tree), inset(style, Property::Right, view.w, tree), inset(style, Property::Bottom, view.h, tree), inset(style, Property::Left, view.w, tree)];
            k += 1;
            if k == areas.sticky.len() {
                return;
            }
        }
        if f.kind == Kind::Box && f.end as usize > i + 1 && tree.kind(f.node) != BoxKind::Inline {
            open.push((f.end, i as u32));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::axis;

    #[test]
    fn a_top_inset_pushes_the_box_down_to_it_and_no_further_than_its_containing_block() {
        // The window is 0..300. The box is 100..150 and the containing block 0..1000.
        let go = |scrolled: f32| axis((scrolled, scrolled + 300.0), (100.0, 150.0), (0.0, 0.0), (0.0, 1000.0), (Some(0.0), None));
        assert_eq!(go(0.0), 0.0, "in view: no move");
        assert_eq!(go(120.0), 20.0, "the top edge passed it");
        assert_eq!(axis((960.0, 1260.0), (100.0, 150.0), (0.0, 0.0), (0.0, 1000.0), (Some(0.0), None)), 850.0, "it stops at the end of its containing block");
    }

    #[test]
    fn the_margin_box_stays_in_the_containing_block_and_a_box_past_the_edge_never_moves_up() {
        assert_eq!(axis((500.0, 800.0), (100.0, 150.0), (0.0, 30.0), (0.0, 200.0), (Some(0.0), None)), 20.0, "the margin of 30 is inside: 150 + 30 + 20 = 200");
        assert_eq!(axis((500.0, 800.0), (100.0, 150.0), (0.0, 80.0), (0.0, 200.0), (Some(0.0), None)), 0.0);
    }

    #[test]
    fn a_bottom_inset_pulls_the_box_up_and_the_top_inset_wins_when_both_cannot_hold() {
        assert_eq!(axis((0.0, 300.0), (500.0, 540.0), (0.0, 0.0), (0.0, 2000.0), (None, Some(25.0))), -265.0);
        assert_eq!(axis((0.0, 300.0), (100.0, 380.0), (0.0, 0.0), (0.0, 2000.0), (Some(10.0), Some(20.0))), -90.0, "bottom: 280 - 380 = -100, then top: back +10");
    }
}
