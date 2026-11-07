//! Block layout: the width, the height and the place of each block, one under the other (CSS 2.1, sections 8.3 and 10).
//!
//! Widths go down the tree: a block is as wide as its parent's content box, minus its own margins, borders and padding,
//! unless `width` says otherwise. Heights go up the tree: a block is as tall as its children, unless `height` says
//! otherwise. So one walk does both: a block gets its width, lays out its children one under the other, and then
//! knows its height.
//!
//! The width (CSS 2.1, 10.3.3), with `ml` and `mr` for the margins and `bp` for the borders and padding of both sides:
//!
//! ```text
//! ml + bp + width + mr = width of the parent's content box
//! width: auto              width takes what is left (auto margins are 0)
//! margin-left/right: auto  the margin takes what is left. Both auto: they split it, and the block is centered
//! nothing is auto          too many numbers: margin-right is ignored and takes what is left
//! ```
//!
//! Margin collapsing (CSS 2.1, 8.3.1): when two vertical margins touch, with no border, padding or text between them,
//! they become one margin: the biggest one (with negative margins: the biggest positive one plus the most negative one).
//! - Two blocks one under the other: the bottom margin of the first and the top margin of the second.
//! - A parent and its first child, if the parent has no top border or padding: the child's top margin goes out through
//!   the top of the parent. `<body><p>` with no other style: the `p` margin (16px) and the `body` margin (8px) become
//!   one margin of 16px above the `body`. The same at the bottom, for a parent with `height: auto`.
//! - An empty block: its own top and bottom margins touch, and the margins around it go through it.
//! - Not through a block that makes its own "block formatting context": the root element, an inline-block, a table
//!   cell, a flex or grid container. Their margins stay apart from the margins of their children.
//!
//! How the real engines do it: Chrome (LayoutNG) passes a "margin strut" from one block to the next: the biggest
//! positive and the most negative margin seen so far, and resolves it when it meets a border, padding or a line.
//! A block whose place depends on margins that are not resolved yet waits ("BFC block offset" in LayoutNG). Firefox
//! passes an `nsCollapsingMargin` the same way. We pass a `Strut` too. To avoid the wait, all places are relative to the
//! parent until the end (`fragment.rs`), so a child is placed at 0 and its parent moves later.
//!
//! Floats (`floats.rs`) need more: the place of a line in its block formatting context (BFC) while it is laid out, to
//! know which floats are next to it. So each block also gets a `Pos`: where it starts in the BFC, and the margins above
//! it that are still open. LayoutNG passes the same two things ("BFC offset" and "margin strut"). A place in the BFC
//! is final once a border, padding or a line resolves the margins above it.
//!
//! The walk calls itself for each level of the tree. A tree deeper than `MAX_DEPTH` stops there: what is deeper is not
//! laid out. Firefox does the same (`MAX_REFLOW_DEPTH`). Chrome has no limit, and a very deep tree crashes its tab.
//!
//! Comments compare Rust with JavaScript, because you know JS.

use crate::boxes::{Child, Kind};
use crate::fragment::{self, Fragment, Rect};
use crate::{Cx, inline, intrinsic};
use bare_css::cascade::ComputedStyle;
use bare_css::values::{Keyword, Length, Property, Value};
use bare_html::dom::NodeId;
use std::collections::HashMap;

/// How deep layout goes. Each level uses some call stack (about 1 KB in a debug build), and the main thread has 8 MB.
pub const MAX_DEPTH: usize = 400;

/// The biggest length that layout uses, in pixels: a bigger one is cut down to it. `f32` turns a length like
/// `2e45px` into infinity, and infinity minus infinity is NaN, which breaks every place after it. Chrome keeps lengths
/// in a `LayoutUnit` (a whole number of 1/64 pixels in 32 bits), so its biggest length is this same number.
pub const MAX_PX: f32 = 33_554_431.0;

/// Margins that touch and collapse into one: the biggest positive one and the most negative one.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Strut {
    positive: f32,
    negative: f32,
}

impl Strut {
    pub fn add(&mut self, margin: f32) {
        self.positive = self.positive.max(margin);
        self.negative = self.negative.min(margin);
    }

    pub fn merge(&mut self, other: Strut) {
        self.add(other.positive);
        self.add(other.negative);
    }

    /// This strut with the margins of `other` added.
    pub fn with(mut self, other: Strut) -> Strut {
        self.merge(other);
        self
    }

    /// The one margin that they make.
    pub fn sum(self) -> f32 {
        self.positive + self.negative
    }
}

/// The space that a block is laid out in: the content box of its parent (the "containing block").
#[derive(Debug, Clone, Copy)]
pub struct Space {
    pub width: f32,
    /// The height, if it is known before the children are laid out (`height: 300px`). `None` for `height: auto`:
    /// then `height: 50%` on a child means `auto` too.
    pub height: Option<f32>,
}

/// Where a block starts in its block formatting context (BFC), for the floats. `x` is the left edge of the content box
/// of its containing block. `y` is where its margin box starts if no margin is above it, and `strut` holds the margins
/// above it that are not resolved yet: its border box starts at `y + strut.sum()`, with its own top margin in the strut.
/// The coordinates are from the top-left of the border box of the BFC root.
#[derive(Debug, Clone, Copy, Default)]
pub struct Pos {
    pub x: f32,
    pub y: f32,
    pub strut: Strut,
}

/// A length in pixels. A percent is of `base`. `None` for `auto` and `none`, and for a percent of an unknown base.
pub fn resolve(cx: &Cx, v: Value, base: Option<f32>) -> Option<f32> {
    let px = match v {
        Value::Length(Length::Px(x)) => Some(x),
        Value::Length(Length::Percent(p)) => {
            if base.is_none() {
                cx.unknown.set(cx.unknown.get() + 1);
            }
            base.map(|b| b * p / 100.0)
        }
        Value::Length(Length::Calc(i)) => {
            let (px, pct) = cx.tree.styles.calc(i);
            match base {
                Some(b) => Some(px + b * pct / 100.0),
                None if pct == 0.0 => Some(px),
                None => {
                    cx.unknown.set(cx.unknown.get() + 1);
                    None
                }
            }
        }
        _ => None,
    };
    px.map(|px| px.clamp(-MAX_PX, MAX_PX))
}

/// The four sides of something: margins, borders or padding.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sides {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Sides {
    pub fn horizontal(&self) -> f32 {
        self.left + self.right
    }
    pub fn vertical(&self) -> f32 {
        self.top + self.bottom
    }
}

/// The borders and the padding of a box. Percent padding is of the width of the containing block, on all four sides.
pub fn border_padding(cx: &Cx, style: &ComputedStyle, cb_width: f32) -> (Sides, Sides) {
    let b = |p| style.px(p).min(MAX_PX);
    let border = Sides { top: b(Property::BorderTopWidth), right: b(Property::BorderRightWidth), bottom: b(Property::BorderBottomWidth), left: b(Property::BorderLeftWidth) };
    let p = |p| resolve(cx, style.get(p), Some(cb_width)).unwrap_or(0.0).max(0.0);
    let padding = Sides { top: p(Property::PaddingTop), right: p(Property::PaddingRight), bottom: p(Property::PaddingBottom), left: p(Property::PaddingLeft) };
    (border, padding)
}

/// Does this block make its own block formatting context? Then the margins of its children do not collapse with its own.
pub fn own_context(cx: &Cx, node: NodeId) -> bool {
    if matches!(cx.tree.kind(node), Kind::Atomic | Kind::Float | Kind::OutOfFlow) || Some(node) == cx.tree.root() || cx.tree.is_flex_or_grid(cx.tree.doc.flat_parent(node)) {
        return true;
    }
    cx.tree.scrolls(node)
        || matches!(
        cx.tree.style(node).get(Property::Display),
        Value::Keyword(Keyword::Table | Keyword::TableCell | Keyword::TableCaption | Keyword::Flex | Keyword::Grid | Keyword::FlowRoot)
    )
}

/// What lays out the content of a block container: its formatting context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    /// Blocks and lines: `children` here, and `inline.rs`.
    Flow,
    /// `display: flex` and `inline-flex` (`flex.rs`).
    Flex,
    /// `display: grid` and `inline-grid` (`grid.rs`).
    Grid,
    /// `display: table` (`table.rs`).
    Table,
}

pub fn context(cx: &Cx, node: NodeId) -> Context {
    match cx.tree.style(node).get(Property::Display) {
        Value::Keyword(Keyword::Flex | Keyword::InlineFlex) => Context::Flex,
        Value::Keyword(Keyword::Grid | Keyword::InlineGrid) => Context::Grid,
        Value::Keyword(Keyword::Table | Keyword::InlineTable) => Context::Table,
        _ => Context::Flow,
    }
}

/// What the parent needs from a child block.
#[derive(Debug, Clone, Copy, Default)]
pub struct Done {
    /// The place of the child's fragment, to put it at its place later.
    pub fragment: usize,
    /// The height of the border box.
    pub height: f32,
    /// The child's own top and bottom margins.
    pub margin_top: f32,
    pub margin_bottom: f32,
    /// The margins of the children that came out through the top of this block. If `through`, all the margins inside.
    pub top: Strut,
    /// The margins of the last children that came out through the bottom.
    pub bottom: Strut,
    /// The block is empty, and its top and bottom margins collapse together (and with the margins around it).
    pub through: bool,
    /// The left and right margins.
    pub margin_left: f32,
    pub margin_right: f32,
    /// The baseline of the last line inside, from the top of the border box. An inline-block sits on it.
    pub baseline: Option<f32>,
    /// The height of the content box from the content alone, before `height`, `min-height` and `max-height`. A flex
    /// item needs it for its automatic minimum size.
    pub content: f32,
}

/// Lay out a block-level box in `space`, at `pos` in its BFC. Its fragment is at x = its left margin, y = 0: the parent
/// moves it down.
pub fn block(cx: &mut Cx, node: NodeId, space: Space, pos: Pos) -> Done {
    sized(cx, node, space, None, None, pos)
}

/// The sides that a box with `clear` must go below: (left, right).
pub fn clear_sides(style: &ComputedStyle) -> (bool, bool) {
    match style.get(Property::Clear) {
        Value::Keyword(Keyword::Left) => (true, false),
        Value::Keyword(Keyword::Right) => (false, true),
        Value::Keyword(Keyword::Both) => (true, true),
        _ => (false, false),
    }
}

/// Lay out a block. `forced`: the width of the content box and the two margins, when the caller already knows them
/// (an inline-block that shrinks to fit its content, `inline.rs`). `tall`: the height of the content box, when the caller
/// knows it (an absolutely positioned box with `top` and `bottom`).
pub fn sized(cx: &mut Cx, node: NodeId, space: Space, forced: Option<(f32, f32, f32)>, tall: Option<f32>, pos: Pos) -> Done {
    sized_with(cx, node, space, forced, tall, pos, None)
}

/// `sized` for a table and its cells. `sides`: the borders and the padding of a table cell, if the table sets them (a
/// collapsed border is half of an edge). A table finds its own in its `Plan`. For both, `height` is a minimum that
/// `table.rs` takes care of, so here it is `auto`.
pub(crate) fn sized_with(cx: &mut Cx, node: NodeId, space: Space, forced: Option<(f32, f32, f32)>, tall: Option<f32>, pos: Pos, sides: Option<(Sides, Sides)>) -> Done {
    let style = cx.tree.style(node);
    // A form control that has no size of its own in the style sheet: the size of its content when `auto` (`controls.rs`).
    let control = crate::controls::of(&cx.tree, node);
    let own = control.and_then(|c| crate::controls::size(&cx.tree, node, c));
    let plan = (context(cx, node) == Context::Table).then(|| crate::table::Plan::new(cx, node, space.width));
    let (border, padding) = match (&plan, sides) {
        (Some(p), _) => p.sides(),
        (None, Some(s)) => s,
        (None, None) => border_padding(cx, style, space.width),
    };
    let bp = Sides {
        top: border.top + padding.top,
        right: border.right + padding.right,
        bottom: border.bottom + padding.bottom,
        left: border.left + padding.left,
    };
    let (width, margin_left, margin_right) = match &plan {
        Some(p) => p.widths(cx, forced, space.width, bp.horizontal()),
        None => forced.unwrap_or_else(|| widths_or(cx, style, space.width, bp.horizontal(), own.map(|s| s.0))),
    };
    let margin = |p| resolve(cx, style.get(p), Some(space.width)).unwrap_or(0.0);
    let (margin_top, margin_bottom) = (margin(Property::MarginTop), margin(Property::MarginBottom));

    let fragment = cx.out.len();
    cx.out.push(Fragment { rect: Rect { x: margin_left, y: 0.0, w: width + bp.horizontal(), h: 0.0 }, node, kind: crate::fragment::Kind::Box, end: 0 });
    // The height comes after the children: `h` is filled in below.

    let border_box = style.get(Property::BoxSizing) == Value::Keyword(Keyword::BorderBox);
    let content = |v: f32| if border_box { (v - bp.vertical()).max(0.0) } else { v };
    let height = tall.or_else(|| if plan.is_some() || sides.is_some() { None } else { resolve(cx, style.get(Property::Height), space.height).map(content) }).or(own.map(|s| s.1));
    let min = resolve(cx, style.get(Property::MinHeight), space.height).map_or(0.0, content);
    let max = resolve(cx, style.get(Property::MaxHeight), space.height).map(content);
    let clamp = |h: f32| max.map_or(h, |m| h.min(m)).max(min);

    let own = own_context(cx, node);
    let top_open = !own && bp.top == 0.0;
    let bottom_open = !own && bp.bottom == 0.0 && height.is_none();
    let (ox, oy) = cx.tree.overflow(node);
    // The scrollbars: (right, bottom). They take room inside the content box. `scroll` always has one. `auto` has one
    // only if the content does not fit, so the layout below runs again when it does not. In a layout that runs again,
    // the boxes inside keep what they found the first time: else each level of `auto` boxes would double the work.
    let auto = (oy == Keyword::Auto, ox == Keyword::Auto);
    let mut bars = (oy == Keyword::Scroll, ox == Keyword::Scroll);
    // Chrome reserves the scrollbar of a `<textarea>` that scrolls, whether it is there or not: the text wraps at `cols`.
    let reserve = control == Some(crate::controls::Control::TextArea) && auto.0;
    bars.0 |= reserve;
    let mut retry = (auto.0 || auto.1) && !reserve;
    cx.scrolls += retry as u32;
    if retry && cx.again > 0 && let Some(&known) = cx.bars.get(&(node.index() as u32)) {
        bars = known;
        retry = false;
    }
    let (number, waiting, mut again) = (cx.number, cx.waiting, false);
    let mut width = width;
    let (kids, floats_bottom) = loop {
        let (gx, gy) = (if bars.0 { SCROLLBAR } else { 0.0 }, if bars.1 { SCROLLBAR } else { 0.0 });
        if cx.tree.kind(node) == Kind::ListItem {
            cx.marker = Some(node);
            // The marker goes on the first line inside the item (`inline.rs`). If no line takes it, it goes at the top.
        }
        let inner = Space { width: (width - gx).max(0.0), height: height.map(|h| (clamp(h) - gy).max(0.0)) };
        // Where the content box starts in the BFC. A new BFC has its own coordinates and its own floats: the floats of the
        // BFC around it wait in `outer` until it is done.
        let strut = pos.strut.with(Strut { positive: margin_top.max(0.0), negative: margin_top.min(0.0) });
        let x = pos.x + margin_left + bp.left;
        let here = if own {
            Pos { x: bp.left, y: bp.top, strut: Strut::default() }
        } else if top_open {
            Pos { x, y: pos.y, strut }
        } else {
            Pos { x, y: pos.y + strut.sum() + bp.top, strut: Strut::default() }
        };
        let outer = own.then(|| std::mem::take(&mut cx.floats));
        // An `auto` box with no scrollbar on the right yet: once the content goes below the room, it needs one, and the
        // rest of the content is laid out in the next pass. A log of 20,000 lines in a box of 400px is laid out once, not twice.
        let room = clamp(height.unwrap_or(f32::INFINITY)) - gy;
        let stop = if retry && auto.0 && !bars.0 { room } else { f32::INFINITY };
        let kids = if cx.depth < MAX_DEPTH {
            cx.depth += 1;
            // Flex, grid and table containers are their own BFC, so `top_open` is false for them.
            let kids = match context(cx, node) {
                Context::Flow => children(cx, node, inner, top_open, bp.top, bp.left, here, stop),
                Context::Flex => crate::flex::layout(cx, node, inner, bp),
                Context::Grid => crate::grid::layout(cx, node, inner, bp),
                Context::Table => crate::table::layout(cx, node, inner, bp, plan.as_ref().expect("a table has a plan")),
            };
            cx.depth -= 1;
            kids
        } else {
            Kids { y: 0.0, top: Strut::default(), bottom: Strut::default(), open: top_open, baseline: None, top_y: here.y }
        };
        // A BFC root grows to contain its floats (CSS 2.1, 10.6.7). Other blocks do not: a float sticks out of them.
        let floats_bottom = outer.and_then(|outer| std::mem::replace(&mut cx.floats, outer).bottom()).map(|b| b - bp.top);
        if retry {
            // A scroll container is its own BFC, so the content ends at the last margin.
            let used = floats_bottom.map_or(0.0f32, |b| b).max(kids.y + kids.bottom.sum());
            let stopped = kids.y > stop;
            // After a stop, the content on the right is not all there: the next pass looks at it.
            let wide = !stopped && auto.1 && right_edge(cx, fragment) > bp.left + inner.width + 0.01;
            let need = (bars.0 || (auto.0 && used > room + 0.01), bars.1 || wide);
            if !stopped {
                cx.set_bars(node, need);
            }
            if need != bars && cx.work <= cx.budget {
                // The pass is thrown away: that is work (`Cx::work`).
                cx.work += (cx.out.len() - fragment) as u64;
                // A box that shrinks to fit its content gets wider by the scrollbar, so the content keeps its room.
                if need.0 && !bars.0 && forced.is_some() && matches!(cx.tree.kind(node), Kind::Atomic | Kind::Float) && style.get(Property::Width) == Value::Keyword(Keyword::Auto) {
                    width += SCROLLBAR;
                }
                bars = need;
                cx.out.truncate(fragment + 1);
                cx.number = number;
                cx.waiting = waiting;
                if !again {
                    again = true;
                    cx.again += 1;
                }
                continue;
            }
        }
        break (kids, floats_bottom);
    };
    if again {
        cx.again -= 1;
    }
    cx.out[fragment].rect.w = width + bp.horizontal();
    if cx.marker == Some(node) {
        cx.marker = None;
        inline::lone_marker(cx, node, bp.left, bp.top);
    }
    let gy = if bars.1 { SCROLLBAR } else { 0.0 };

    let mut done = Done { fragment, margin_top, margin_bottom, margin_left, margin_right, ..Done::default() };
    done.baseline = kids.baseline.map(|b| b + bp.top);
    let content_height = if kids.open {
        // Nothing inside stopped the margins: all of them reach the top of this block.
        done.top = kids.bottom;
        0.0
    } else {
        done.top = kids.top;
        if bottom_open {
            done.bottom = kids.bottom;
            kids.y
        } else {
            kids.y + kids.bottom.sum()
        }
    };
    let content_height = floats_bottom.map_or(content_height, |b| content_height.max(b));
    done.content = content_height;
    // A scrollbar at the bottom is inside the height, if the box has one. Else the box grows by it.
    let natural = content_height;
    let content_height = clamp(height.unwrap_or(content_height + gy));
    // Chrome centers the content of a `<button>` in its height.
    if control == Some(crate::controls::Control::Button) && content_height > natural + gy {
        let dy = (content_height - gy - natural) / 2.0;
        let mut j = fragment + 1;
        while j < cx.out.len() {
            cx.out[j].rect.y += dy;
            j = cx.out[j].end as usize;
        }
        done.baseline = done.baseline.map(|b| b + dy);
    }
    // The text line of a field, a button made from an `<input>` or a select is not a box: its baseline is computed.
    if matches!(control, Some(crate::controls::Control::Field | crate::controls::Control::Label | crate::controls::Control::Select)) {
        done.baseline = Some(bp.top + crate::controls::baseline(&cx.tree, node, content_height));
    }
    // A checkbox or a radio sits on its bottom border edge; a button with no line in it, on the bottom of its content box.
    match control {
        Some(crate::controls::Control::Check { .. }) => done.baseline = Some(bp.top + content_height + bp.bottom),
        Some(crate::controls::Control::Button) if done.baseline.is_none() => done.baseline = Some(bp.top + content_height),
        _ => {}
    }
    done.through = kids.open && bottom_open && content_height == 0.0 && bp.vertical() == 0.0;
    if kids.open && !done.through {
        // The margins inside reached the top, but the block has a height: they stay at the top, and the bottom is closed.
        done.bottom = Strut::default();
    }
    done.height = content_height + bp.vertical();
    let position = cx.tree.position(node);
    if position == Keyword::Relative {
        let offset = relative_offset(cx, style, space);
        if offset != (0.0, 0.0) {
            cx.set_rel(node, offset);
        }
    }
    if position != Keyword::Static && cx.waiting > 0 {
        // The containing block of the absolute boxes inside: this padding box.
        // The scrollbars are not in it: absolute boxes measure from the client edge.
        let gx = if bars.0 { SCROLLBAR } else { 0.0 };
        let pad = Rect { x: border.left, y: border.top, w: width + padding.horizontal() - gx, h: done.height - border.vertical() - gy };
        out_of_flow(cx, fragment, pad, false, true);
    }
    let end = cx.out.len() as u32;
    let f = &mut cx.out[fragment];
    f.rect.h = done.height;
    f.end = end;
    done
}

/// How far `position: relative` moves a box from its place (CSS 2.1, 9.4.3). `left` wins over `right`, and `top` over
/// `bottom`. A percent is of the containing block, and `top: 50%` in a block with no fixed height is `auto`.
pub fn relative_offset(cx: &Cx, style: &ComputedStyle, cb: Space) -> (f32, f32) {
    let get = |p, base| resolve(cx, style.get(p), base);
    let pick = |a: Option<f32>, b: Option<f32>| a.or(b.map(|b| -b)).unwrap_or(0.0);
    (pick(get(Property::Left, Some(cb.width)), get(Property::Right, Some(cb.width))), pick(get(Property::Top, cb.height), get(Property::Bottom, cb.height)))
}

/// Lay out the boxes with `position: absolute` whose containing block is the fragment `at`, and add them inside it.
/// `pad`: the containing block, from the border box of `at` (its padding box, or the window for the root). `fixed`:
/// this is the window, which takes the `fixed` boxes. `abs`: take the absolute boxes with no other positioned block above them.
///
/// Chrome (LayoutNG) passes the out-of-flow boxes up the fragment tree as "candidates" until they reach their
/// containing block. Here the static positions are fragments in the flat list, and the containing block finds them
/// with one walk over its own fragments. A positioned block inside takes its own, so the walk skips it: each fragment
/// is walked once, for the nearest positioned block above it (the root walks into them when the page has `fixed` boxes).
pub fn out_of_flow(cx: &mut Cx, at: usize, pad: Rect, fixed: bool, abs: bool) {
    let mut found = Vec::new();
    // The first and the last fragment of each positioned inline box: an inline box can be a containing block too.
    let mut inlines: HashMap<u32, (Rect, Rect)> = HashMap::new();
    // The open fragments: (their end, their corner from the border box of `at`, a positioned block is open).
    let mut open: Vec<(u32, f32, f32, bool)> = Vec::new();
    let mut i = at + 1;
    while i < cx.out.len() {
        while open.last().is_some_and(|o| o.0 as usize <= i) {
            open.pop();
        }
        let f = cx.out[i];
        let (px, py, inside) = open.last().map_or((0.0, 0.0, false), |o| (o.1, o.2, o.3));
        let (x, y) = (px + f.rect.x, py + f.rect.y);
        match f.kind {
            crate::fragment::Kind::Static => {
                let is_fixed = cx.tree.position(f.node) == Keyword::Fixed;
                if if is_fixed { fixed } else { abs && !inside } {
                    found.push((f.node, x - pad.x, y - pad.y));
                }
            }
            crate::fragment::Kind::Box if cx.tree.kind(f.node) == Kind::Inline && cx.tree.position(f.node) != Keyword::Static => {
                let r = Rect { x, y, ..f.rect };
                inlines.entry(f.node.index() as u32).and_modify(|e| e.1 = r).or_insert((r, r));
            }
            crate::fragment::Kind::Box if f.end as usize > i + 1 => {
                let positioned = cx.tree.kind(f.node) != Kind::Inline && cx.tree.position(f.node) != Keyword::Static;
                if positioned && !(fixed && cx.fixed) {
                    i = f.end as usize;
                    continue;
                }
                open.push((f.end, x, y, inside || positioned));
            }
            _ => {}
        }
        i += 1;
    }
    let top = cx.out[at].node;
    for (node, sx, sy) in found {
        // The nearest positioned inline box above it, inside `at`. Its containing block goes from the top-left of the
        // padding box of its first fragment to the bottom-right of the padding box of its last one (CSS 2.1, 10.1).
        let mut cb = pad;
        let mut up = cx.tree.doc.flat_parent(node);
        while !inlines.is_empty()
            && let Some(n) = up
            && n != top
        {
            if let Some(&(first, last)) = inlines.get(&(n.index() as u32)) {
                let (border, _) = border_padding(cx, cx.tree.style(n), pad.w);
                let (x0, y0) = (first.x + border.left, first.y + border.top);
                cb = Rect { x: x0, y: y0, w: (last.x + last.w - border.right - x0).max(0.0), h: (last.y + last.h - border.bottom - y0).max(0.0) };
                break;
            }
            up = cx.tree.doc.flat_parent(n);
        }
        absolute(cx, node, sx + pad.x - cb.x, sy + pad.y - cb.y, cb);
    }
}

/// Lay out one absolutely positioned box in the containing block `cb`, with its static position `sx`, `sy` (both from
/// the corner of `cb`). Its fragment goes at the end of the list, placed from the border box around `cb`.
fn absolute(cx: &mut Cx, node: NodeId, sx: f32, sy: f32, cb: Rect) {
    cx.waiting = cx.waiting.saturating_sub(1);
    let style = cx.tree.style(node);
    let (border, padding) = border_padding(cx, style, cb.w);
    let (bp_x, bp_y) = (border.horizontal() + padding.horizontal(), border.vertical() + padding.vertical());
    let border_box = style.get(Property::BoxSizing) == Value::Keyword(Keyword::BorderBox);
    let get = |p, base: f32| resolve(cx, style.get(p), Some(base));
    let size = |p, base: f32, bp: f32| get(p, base).map(|v| if border_box { (v - bp).max(0.0) } else { v });
    let (lo, hi) = (size(Property::MinWidth, cb.w, bp_x).unwrap_or(0.0), size(Property::MaxWidth, cb.w, bp_x).unwrap_or(f32::INFINITY));
    // Percent margins are of the width of the containing block, on all four sides.
    let (mx, my) = ((get(Property::MarginLeft, cb.w), get(Property::MarginRight, cb.w)), (get(Property::MarginTop, cb.w), get(Property::MarginBottom, cb.w)));
    let (left, right) = (get(Property::Left, cb.w), get(Property::Right, cb.w));
    let (top, bottom) = (get(Property::Top, cb.h), get(Property::Bottom, cb.h));
    let width = size(Property::Width, cb.w, bp_x).map(|w| w.min(hi).max(lo));
    let height = size(Property::Height, cb.h, bp_y);
    let shrink = |cx: &mut Cx, room: f32| {
        let (min, max) = intrinsic::sizes(cx, node);
        max.min(min.max(room)).min(hi).max(lo)
    };
    let (x, w, ml, mr) = axis(cx, cb.w, left, right, width, mx, bp_x, sx, shrink);
    let w = w.min(hi).max(lo);

    // The height: known before the layout if it is fixed, or if `top` and `bottom` fix it. Else from the content.
    // `min-height` and `max-height` apply in `sized`.
    let fill = match (top, bottom, height) {
        (Some(t), Some(b), None) => Some((cb.h - t - b - my.0.unwrap_or(0.0) - my.1.unwrap_or(0.0) - bp_y).max(0.0)),
        _ => None,
    };
    let marker = cx.marker.take();
    let done = sized(cx, node, Space { width: cb.w, height: Some(cb.h) }, Some((w, ml, mr)), fill, Pos::default());
    cx.marker = marker;
    let content = done.height - bp_y;
    let (y, _, mt, _) = axis(cx, cb.h, top, bottom, Some(content), my, bp_y, sy, |_, h| h);
    let f = &mut cx.out[done.fragment];
    f.rect.x = cb.x + x + ml;
    f.rect.y = cb.y + y + mt;
}

/// One axis of an absolutely positioned box (CSS 2.1, 10.3.7 for the width, 10.6.4 for the height). `cb`: the size of
/// the containing block. `start`, `end`: `left` and `right` (or `top` and `bottom`), `None` for `auto`. `size`: the
/// content size, if it is not `auto`. `m`: the margins. `bp`: the borders and padding. `stat`: the static position.
/// `fit`: the size for `auto`, given the room. Gives the start of the margin box, the content size, and the margins.
#[allow(clippy::too_many_arguments)]
fn axis(cx: &mut Cx, cb: f32, start: Option<f32>, end: Option<f32>, size: Option<f32>, m: (Option<f32>, Option<f32>), bp: f32, stat: f32, fit: impl FnOnce(&mut Cx, f32) -> f32) -> (f32, f32, f32, f32) {
    let (m1, m2) = (m.0.unwrap_or(0.0), m.1.unwrap_or(0.0));
    match (start, end, size) {
        (Some(s), Some(e), Some(z)) => {
            // Nothing is `auto`: `auto` margins share what is left, else the end is ignored (left to right).
            let rest = cb - s - e - z - bp;
            match m {
                (None, None) if rest >= 0.0 => (s, z, rest / 2.0, rest / 2.0),
                (None, None) => (s, z, 0.0, rest),
                (None, Some(b)) => (s, z, rest - b, b),
                (Some(a), None) => (s, z, a, rest - a),
                (Some(a), Some(b)) => (s, z, a, b),
            }
        }
        // Both sides are fixed: the box fills the room between them.
        (Some(s), Some(e), None) => (s, (cb - s - e - m1 - m2 - bp).max(0.0), m1, m2),
        (None, None, z) => (stat, z.unwrap_or_else(|| fit(cx, cb - stat - m1 - m2 - bp)), m1, m2),
        (Some(s), None, z) => (s, z.unwrap_or_else(|| fit(cx, cb - s - m1 - m2 - bp)), m1, m2),
        (None, Some(e), z) => {
            let z = z.unwrap_or_else(|| fit(cx, cb - e - m1 - m2 - bp));
            (cb - e - m2 - z - bp - m1, z, m1, m2)
        }
    }
}

/// The width of a scrollbar: the classic ones of Chrome on Linux and Windows. The overlay scrollbars of macOS and phones take no room.
pub const SCROLLBAR: f32 = 15.0;

/// The right edge of the content inside the fragment `at`, from its left border edge. `at` is the last fragment that is
/// still open, so its content is all of `cx.out` after it. A box that hides or scrolls its content counts only for its own border box.
fn right_edge(cx: &Cx, at: usize) -> f32 {
    let mut right = 0.0f32;
    let mut open: Vec<(u32, f32)> = Vec::new();
    let mut i = at + 1;
    while i < cx.out.len() {
        while open.last().is_some_and(|&(end, _)| end as usize <= i) {
            open.pop();
        }
        let f = &cx.out[i];
        let x = open.last().map_or(0.0, |&(_, x)| x) + f.rect.x;
        right = right.max(x + f.rect.w);
        if f.kind == crate::fragment::Kind::Box && cx.tree.kind(f.node) != Kind::Inline && cx.tree.scrolls(f.node) {
            i = f.end as usize;
            continue;
        }
        if f.end as usize > i + 1 {
            open.push((f.end, x));
        }
        i += 1;
    }
    right
}

/// The content width and the margins of a block, from its margins (`None` for `auto`) and its width (`None` for `auto`).
/// `cb`: the width of the containing block. `bp`: the borders and padding of both sides.
pub(crate) fn solve_margins(ml: Option<f32>, mr: Option<f32>, cb: f32, bp: f32, width: Option<f32>) -> (f32, f32, f32) {
    match width {
        None => {
            let (l, r) = (ml.unwrap_or(0.0), mr.unwrap_or(0.0));
            ((cb - l - r - bp).max(0.0), l, r)
        }
        Some(w) => {
            let rest = cb - bp - w;
            match (ml, mr) {
                (None, None) if rest >= 0.0 => (w, rest / 2.0, rest / 2.0),
                (None, None) => (w, 0.0, rest),
                (None, Some(r)) if rest - r >= 0.0 => (w, rest - r, r),
                (None, Some(_)) => (w, 0.0, rest),
                (Some(l), _) => (w, l, rest - l),
                // Too many numbers: `margin-right` takes what is left (left to right text).
            }
        }
    }
}

/// The content width and the left and right margins of a block (CSS 2.1, 10.3.3, with `box-sizing`, `min-width` and
/// `max-width`). `bp`: the borders and padding of both sides.
pub fn widths(cx: &Cx, style: &ComputedStyle, cb: f32, bp: f32) -> (f32, f32, f32) {
    widths_or(cx, style, cb, bp, None)
}

/// `widths`, with `auto`: the width of a box that is not as wide as its container when `width` is `auto` (a form
/// control, `controls.rs`). `None` is the block rule: fill the container.
pub fn widths_or(cx: &Cx, style: &ComputedStyle, cb: f32, bp: f32, auto: Option<f32>) -> (f32, f32, f32) {
    let border_box = style.get(Property::BoxSizing) == Value::Keyword(Keyword::BorderBox);
    let content = |v: f32| if border_box { (v - bp).max(0.0) } else { v };
    let ml = resolve(cx, style.get(Property::MarginLeft), Some(cb));
    let mr = resolve(cx, style.get(Property::MarginRight), Some(cb));
    let solve = |width: Option<f32>| solve_margins(ml, mr, cb, bp, width);
    let (mut w, mut l, mut r) = solve(resolve(cx, style.get(Property::Width), Some(cb)).map(content).or(auto));
    if let Some(max) = resolve(cx, style.get(Property::MaxWidth), Some(cb)).map(content)
        && w > max
    {
        (w, l, r) = solve(Some(max));
    }
    if let Some(min) = resolve(cx, style.get(Property::MinWidth), Some(cb)).map(content)
        && w < min
    {
        (w, l, r) = solve(Some(min));
    }
    (w, l, r)
}

/// The result of the children of a block. A flex, grid or table layout gives one too: `y` is the height of its content,
/// `baseline` its first baseline, and the rest is empty (no margin goes out of a BFC).
pub(crate) struct Kids {
    /// The bottom of the last child (its border box), from the top of the content box.
    y: f32,
    /// The margins that came out through the top. Only if `top_open`.
    top: Strut,
    /// The margins after the last child, not used yet.
    bottom: Strut,
    /// Nothing stopped the margins at the top: no child with a border, padding, a line or a height.
    open: bool,
    /// The baseline of the last line, from the top of the content box.
    baseline: Option<f32>,
    /// Where the content box starts in the BFC, once the margins at the top are resolved (when `open` is false).
    top_y: f32,
}

impl Kids {
    /// The result of a layout that is its own BFC: the height of the content box, and its first baseline from its top.
    pub(crate) fn of(height: f32, baseline: Option<f32>) -> Kids {
        Kids { y: height, top: Strut::default(), bottom: Strut::default(), open: false, baseline, top_y: 0.0 }
    }
}

/// Lay out the children of a block, one under the other. `top_open`: the margins of the first child may go out through the
/// top. The fragments of the children are placed relative to the parent's border box: `dx`, `dy` are the parent's
/// border and padding. `here`: where the content box starts in the BFC (see `Pos`).
/// `stop`: stop after the child that ends below this, from the top of the content box (see `sized`).
#[allow(clippy::too_many_arguments)]
fn children(cx: &mut Cx, node: NodeId, space: Space, top_open: bool, dy: f32, dx: f32, here: Pos, stop: f32) -> Kids {
    let mut kids = Kids { y: 0.0, top: Strut::default(), bottom: Strut::default(), open: top_open, baseline: None, top_y: here.y };
    // `bottom` is the strut that is open now: the margins since the last child that has content.
    let tree = cx.tree;
    // A copy of the `Tree`, so that the loop over the children does not hold `cx`, and the loop body can change it.
    let mut counter = tree.counter(node);
    for child in tree.children(node) {
        if kids.y > stop {
            break;
        }
        match child {
            Child::Block(b) => {
                if cx.tree.kind(b) == Kind::ListItem {
                    cx.number = cx.tree.list_number(&mut counter, b);
                }
                let pos = next(&kids, here);
                let (forced, moved) = if cx.floats.is_empty() { (None, None) } else { avoid_floats(cx, b, space, pos) };
                if let Some(y) = moved {
                    // Clearance: the block goes down to `y`, below the floats. Its top margin no longer collapses with
                    // the margins above it: they are resolved here.
                    if kids.open {
                        kids.top_y = here.y + here.strut.with(kids.bottom).sum();
                        kids.open = false;
                        kids.top = kids.bottom;
                    }
                    kids.bottom = Strut::default();
                    let mt = resolve(cx, cx.tree.style(b).get(Property::MarginTop), Some(space.width)).unwrap_or(0.0);
                    let done = sized(cx, b, space, forced, None, Pos { x: here.x, y: y - mt, strut: Strut::default() });
                    let top = Strut { positive: mt.max(0.0), negative: mt.min(0.0) }.with(done.top);
                    let y = y - mt - kids.top_y + top.sum();
                    cx.out[done.fragment].rect.y = y + dy;
                    cx.out[done.fragment].rect.x += dx;
                    kids.y = y + done.height;
                    if let Some(b) = done.baseline {
                        kids.baseline = Some(y + b);
                    }
                    kids.bottom.add(done.margin_bottom);
                    kids.bottom.merge(done.bottom);
                    continue;
                }
                let done = sized(cx, b, space, forced, None, pos);
                kids.bottom.add(done.margin_top);
                kids.bottom.merge(done.top);
                if done.through {
                    kids.bottom.add(done.margin_bottom);
                    let y = if kids.open { 0.0 } else { kids.y + kids.bottom.sum() };
                    cx.out[done.fragment].rect.y = y + dy;
                    cx.out[done.fragment].rect.x += dx;
                    continue;
                }
                let y = place(&mut kids, here);
                cx.out[done.fragment].rect.y = y + dy;
                cx.out[done.fragment].rect.x += dx;
                kids.y = y + done.height;
                if let Some(b) = done.baseline {
                    kids.baseline = Some(y + b);
                }
                kids.bottom = Strut::default();
                kids.bottom.add(done.margin_bottom);
                kids.bottom.merge(done.bottom);
            }
            Child::Lines(first, last) => {
                let from = cx.out.len();
                let pos = next(&kids, here);
                let lines = inline::lines(cx, node, first, last, space.width, (here.x, pos.y + pos.strut.sum()));
                if !lines.any {
                    // Only empty inline boxes or floats: no line, and the margins go on through. The floats go where
                    // the first line would be.
                    let y = if kids.open { kids.y } else { kids.y + kids.bottom.sum() };
                    fragment::shift(&mut cx.out, from, dx, y + dy);
                    continue;
                }
                let y = place(&mut kids, here);
                fragment::shift(&mut cx.out, from, dx, y + dy);
                kids.y = y + lines.height;
                if let Some(b) = lines.baseline {
                    kids.baseline = Some(y + b);
                }
            }
        }
    }
    kids
}

/// Where the next child starts in the BFC: see `Pos`.
fn next(kids: &Kids, here: Pos) -> Pos {
    if kids.open {
        // Nothing resolved the margins yet: the parent's open margins and the children's ones are one strut.
        Pos { x: here.x, y: here.y, strut: here.strut.with(kids.bottom) }
    } else {
        Pos { x: here.x, y: kids.top_y + kids.y, strut: kids.bottom }
    }
}

/// A block next to floats, before it is laid out. `clear` moves it below them. A block that makes its own BFC (an
/// inline-block, a table, a flex container) may not overlap them (CSS 2.1, 9.5): it gets narrower to fit beside them,
/// or it goes down until it fits. Gives the width and margins to use (if they change), and the new top of its border
/// box in the BFC (if it moves down).
fn avoid_floats(cx: &mut Cx, node: NodeId, space: Space, pos: Pos) -> (Option<(f32, f32, f32)>, Option<f32>) {
    let style = cx.tree.style(node);
    let mt = resolve(cx, style.get(Property::MarginTop), Some(space.width)).unwrap_or(0.0);
    let top = pos.y + pos.strut.with(Strut { positive: mt.max(0.0), negative: mt.min(0.0) }).sum();
    let (left, right) = clear_sides(style);
    let mut y = cx.floats.clear(left, right).map_or(top, |c| top.max(c));
    let mut forced = None;
    if own_context(cx, node) {
        let (border, padding) = border_padding(cx, style, space.width);
        let bp = border.horizontal() + padding.horizontal();
        let (w, _, _) = widths(cx, style, space.width, bp);
        // The margins as they are written: `widths` gives a fixed width an over-constrained right margin that fills the line.
        let margin = |p| resolve(cx, style.get(p), Some(space.width)).unwrap_or(0.0);
        let (ml, mr) = (margin(Property::MarginLeft), margin(Property::MarginRight));
        let auto = style.get(Property::Width) == Value::Keyword(Keyword::Auto);
        let min = if auto { crate::intrinsic::sizes(cx, node).0 } else { w };
        loop {
            let (l, r) = cx.floats.band(y, 0.0, pos.x, pos.x + space.width);
            if (l, r) == (pos.x, pos.x + space.width) {
                break;
            }
            // The border box goes between the floats. Its margins may overlap them.
            let (l, r) = ((l - pos.x).max(ml), (r - pos.x).min(space.width - mr));
            let next = cx.floats.next_bottom(y, 0.0);
            if r - l - bp >= min - 0.01 || next.is_none() {
                let w = if auto { (r - l - bp).max(0.0) } else { w };
                forced = Some((w, l, space.width - l - w - bp));
                break;
            }
            y = next.unwrap_or(y);
        }
    }
    (forced, (y > top + 0.01).then_some(y))
}

/// A child with content comes: the open margins are resolved, and the child goes under them. The new top of the child.
fn place(kids: &mut Kids, here: Pos) -> f32 {
    if kids.open {
        // The first content: the margins before it go out through the top of the parent.
        kids.top_y = here.y + here.strut.with(kids.bottom).sum();
        kids.open = false;
        kids.top = kids.bottom;
        kids.bottom = Strut::default();
        return kids.y;
    }
    let y = kids.y + kids.bottom.sum();
    kids.bottom = Strut::default();
    y
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::fragment::Rect;
    use bare_css::cascade::{Origin, RuleSet, UA_CSS};
    use bare_html::dom::{Atom, Document, NodeId};
    use bare_html::tree::parse;

    /// The layout of a page in a window of 800 by 600, as text.
    pub fn dump(html: &str, css: &str) -> String {
        let (doc, layout) = run(html, css);
        crate::dump(&doc, &layout)
    }

    pub fn run(html: &str, css: &str) -> (Document, crate::fragment::Layout) {
        let mut doc = parse(html);
        let mut rules = RuleSet::new((800.0, 600.0));
        rules.add(&mut doc, UA_CSS, Origin::UserAgent);
        rules.add(&mut doc, css, Origin::Author);
        let styles = rules.compute(&doc);
        let layout = crate::layout(&doc, &styles, (800.0, 600.0));
        (doc, layout)
    }

    /// The border box of the element with this `id`.
    pub fn rect(html: &str, css: &str, id: &str) -> Rect {
        let (doc, layout) = run(html, css);
        let node = find(&doc, id);
        layout.fragments.iter().find(|f| f.node == node).unwrap_or_else(|| panic!("no fragment for {id}")).rect
    }

    fn find(doc: &Document, id: &str) -> NodeId {
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if doc.attr(n, Atom::ID) == Some(id) {
                return n;
            }
            stack.extend(doc.children(n));
        }
        panic!("no element {id}")
    }

    fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn a_block_fills_the_width() {
        assert_eq!(dump("<div style='height:50px'></div>", ""), "html 0,0 800x66\n  body 8,8 784x50\n    div 8,8 784x50\n");
    }

    #[test]
    fn margins_of_siblings_collapse() {
        // The first `p` margin (16) and the `body` margin (8) collapse into 16 above the body. 16 between the two `p`.
        assert_eq!(
            dump("<p>a</p><p>b</p>", ""),
            "html 0,0 800x92\n  body 8,16 784x60\n    p 8,16 784x22\n      \"a\" 8,16 9.6x22\n    p 8,54 784x22\n      \"b\" 8,54 9.6x22\n"
        );
    }

    #[test]
    fn a_border_stops_the_collapse() {
        let html = "<div id=d style='border-top:1px solid'><p id=p>a</p></div>";
        assert_eq!(rect(html, "", "d"), r(8.0, 8.0, 784.0, 1.0 + 16.0 + 22.0));
        // The bottom margin of the `p` goes out through the bottom of the `div`, which has no bottom border.
        assert_eq!(rect(html, "", "p"), r(8.0, 25.0, 784.0, 22.0));
        // With padding the same.
        assert_eq!(rect("<div style='padding-top:1px'><p id=p>a</p></div>", "", "p"), r(8.0, 25.0, 784.0, 22.0));
    }

    #[test]
    fn negative_margins() {
        // 30 and -10 make 20. -5 and -10 make -10.
        let html = "<div style='height:10px;margin-bottom:30px'></div><div id=b style='height:10px;margin-top:-10px'></div>";
        assert_eq!(rect(html, "", "b").y, 8.0 + 10.0 + 20.0);
        let html = "<div style='height:10px;margin-bottom:-5px'></div><div id=b style='height:10px;margin-top:-10px'></div>";
        assert_eq!(rect(html, "", "b").y, 8.0 + 10.0 - 10.0);
    }

    #[test]
    fn an_empty_block_lets_margins_through() {
        let html = "<div style='margin:20px 0'></div><div id=b style='margin-top:30px;height:10px'></div>";
        assert_eq!(rect(html, "", "b"), r(8.0, 30.0, 784.0, 10.0));
        let (_, layout) = run(html, "");
        assert_eq!(layout.height, 30.0 + 10.0 + 8.0);
        // An empty block with a height stops them.
        let html = "<div style='margin:20px 0;height:1px'></div><div id=b style='margin-top:30px;height:10px'></div>";
        assert_eq!(rect(html, "", "b").y, 20.0 + 1.0 + 30.0);
    }

    #[test]
    fn the_root_and_inline_blocks_keep_their_margins() {
        // `html` makes its own context: the `body` margin stays inside it.
        let (doc, layout) = run("<p>x</p>", "html { margin-top: 5px }");
        let _ = doc;
        assert_eq!(layout.fragments[0].rect.y, 5.0);
        assert_eq!(layout.fragments[1].rect.y, 5.0 + 16.0);
    }

    #[test]
    fn auto_margins_center() {
        assert_eq!(rect("<div id=d style='width:200px;margin:0 auto;height:1px'></div>", "", "d"), r(300.0, 8.0, 200.0, 1.0));
        assert_eq!(rect("<div id=d style='width:200px;margin-left:auto;height:1px'></div>", "", "d").x, 8.0 + 584.0);
        // Too wide: the auto margins are 0.
        assert_eq!(rect("<div id=d style='width:1000px;margin:0 auto;height:1px'></div>", "", "d").x, 8.0);
    }

    #[test]
    fn padding_border_and_box_sizing() {
        let css = "#d { width: 100px; padding: 10px; border: 5px solid }";
        assert_eq!(rect("<div id=d></div>", css, "d"), r(8.0, 8.0, 130.0, 30.0));
        let css = "#d { width: 100px; height: 50px; padding: 10px; border: 5px solid; box-sizing: border-box }";
        assert_eq!(rect("<div id=d></div>", css, "d"), r(8.0, 8.0, 100.0, 50.0));
        // Percent padding is of the parent's width, also on top.
        assert_eq!(rect("<div id=d style='padding-top:10%'></div>", "", "d").h, 78.4);
    }

    #[test]
    fn percent_and_calc_and_min_max() {
        assert_eq!(rect("<div id=d style='width:50%;height:1px'></div>", "", "d").w, 392.0);
        assert_eq!(rect("<div id=d style='width:calc(50% - 20px);height:1px'></div>", "", "d").w, 372.0);
        assert_eq!(rect("<div id=d style='max-width:100px;height:1px'></div>", "", "d").w, 100.0);
        assert_eq!(rect("<div id=d style='width:10px;min-width:100px;height:1px'></div>", "", "d").w, 100.0);
        // A percent height of a parent with `height: auto` is `auto`. Of a parent with a height, it works.
        assert_eq!(rect("<div id=d style='height:50%'></div>", "", "d").h, 0.0);
        assert_eq!(rect("<div style='height:200px'><div id=d style='height:50%'></div></div>", "", "d").h, 100.0);
        assert_eq!(rect("<div id=d style='height:10px;min-height:30px'></div>", "", "d").h, 30.0);
        assert_eq!(rect("<div id=d style='height:100px;max-height:30px'></div>", "", "d").h, 30.0);
        // `html { height: 100% }` is the height of the window.
        assert_eq!(run("", "html { height: 100% }").1.fragments[0].rect.h, 600.0);
    }

    #[test]
    fn list_markers_without_lines() {
        let out = dump("<ol start=3><li><div style='height:5px'></div></li></ol>", "");
        // The marker `3. ` is 3 letters of 9.6px, to the left of the content box of the `li`.
        assert!(out.contains("marker 3 19.2,16 28.8x22\n"), "{out}");
    }

    #[test]
    fn a_new_context_with_a_width_fits_beside_a_float() {
        // The float takes 100 of 300. A block of 150 that makes its own context fits in the 200 next to it.
        let html = "<div style='width:300px'><div style='float:left;width:100px;height:30px'></div><div id=b style='display:flow-root;width:150px;height:5px'></div></div>";
        assert_eq!(rect(html, "", "b"), r(108.0, 8.0, 150.0, 5.0));
    }

    #[test]
    fn overflow_hidden_makes_a_new_context_and_clip_does_not() {
        // The margin of the child stays inside a scroll container. Through `clip` it collapses with the margin of the body.
        let html = "<div id=d style='overflow:hidden'><p style='margin:30px 0;height:5px'></p></div>";
        assert_eq!(rect(html, "", "d"), r(8.0, 8.0, 784.0, 65.0));
        let html = "<div id=d style='overflow:clip'><p style='margin:30px 0;height:5px'></p></div>";
        assert_eq!(rect(html, "", "d"), r(8.0, 30.0, 784.0, 5.0));
        // Beside a float, like `display: flow-root`.
        let html = "<div style='float:left;width:100px;height:30px'></div><div id=d style='overflow:auto;height:5px'></div>";
        assert_eq!(rect(html, "", "d"), r(108.0, 8.0, 684.0, 5.0));
    }

    #[test]
    fn overflow_of_the_body_goes_to_the_window() {
        // So the body is not a scroll container: the margin inside collapses with its own 8px.
        let html = "<body style='overflow:hidden'><div id=d><p style='margin:30px 0;height:5px'></p></div>";
        assert_eq!(rect(html, "", "d"), r(8.0, 30.0, 784.0, 5.0));
        // Not if the root has its own overflow.
        let html = "<html style='overflow:auto'><body style='overflow:hidden'><div id=d><p style='margin:30px 0;height:5px'></p></div>";
        assert_eq!(rect(html, "", "d"), r(8.0, 38.0, 784.0, 5.0));
    }

    #[test]
    fn scrollbars_take_room_in_the_content_box() {
        let css = "body { line-height: 20px }";
        // `scroll`: both, always. The child is 15 narrower; the box keeps its size.
        let html = "<div style='overflow:scroll;width:200px;height:100px'><div id=c>x</div></div>";
        assert_eq!(rect(html, css, "c"), r(8.0, 8.0, 185.0, 20.0));
        // A scrollbar at the bottom makes a box with `height: auto` taller.
        let html = "<div id=d style='overflow-x:scroll;width:200px'>x</div>";
        assert_eq!(rect(html, css, "d"), r(8.0, 8.0, 200.0, 35.0));
        // `auto`: only on the side where the content does not fit.
        let html = "<div id=d style='overflow:auto;width:200px'><div id=c>x</div></div>";
        assert_eq!(rect(html, css, "d"), r(8.0, 8.0, 200.0, 20.0));
        let html = "<div style='overflow:auto;width:100px;max-height:30px'><div id=c>a<br>b<br>c</div></div>";
        assert_eq!(rect(html, css, "c"), r(8.0, 8.0, 85.0, 60.0));
        let html = "<div id=d style='overflow:auto;width:100px;white-space:nowrap'>xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx</div>";
        assert_eq!(rect(html, css, "d"), r(8.0, 8.0, 100.0, 35.0));
        // The padding at the end counts: 105 + 10 does not fit in 100 + 10.
        let html = "<div id=d style='overflow:auto;width:100px;padding-right:10px'><div style='width:105px;height:5px'></div></div>";
        assert_eq!(rect(html, css, "d"), r(8.0, 8.0, 110.0, 20.0));
        // A scroll container inside hides its own content.
        let html = "<div id=d style='overflow:auto;width:100px'><div style='overflow:hidden;height:5px'><div style='width:500px'></div></div></div>";
        assert_eq!(rect(html, css, "d"), r(8.0, 8.0, 100.0, 5.0));
    }

    #[test]
    fn a_box_that_shrinks_to_fit_gets_wider_by_its_scrollbar() {
        let css = "body { line-height: 20px }";
        let a = rect("<div id=d style='float:left'>abc</div>", css, "d").w;
        let b = rect("<div id=d style='float:left;overflow-y:scroll'>abc</div>", css, "d").w;
        assert_eq!(b, a + 15.0);
        let c = rect("<div id=d style='float:left;overflow-y:auto;height:20px'>abc<br>def</div>", css, "d").w;
        assert_eq!(c, a + 15.0);
        let c = rect("<span id=d style='display:inline-block;overflow-y:auto;height:20px'>abc<br>def</span>", css, "d").w;
        assert_eq!(c, a + 15.0);
    }

    #[test]
    fn nested_auto_boxes_that_overflow() {
        // Each needs a scrollbar. The inner one keeps its width of 185 in the second layout of the outer one.
        let html = "<div style='overflow:auto;height:50px;width:200px'><div id=c style='overflow:auto;height:100px'><div style='height:300px'></div></div></div>";
        assert_eq!(rect(html, "", "c"), r(8.0, 8.0, 185.0, 100.0));
        // 40 levels: each runs again once, and the ones inside keep what they found.
        let html = "<div style='overflow:auto;height:50px'>".repeat(40) + "<div style='height:300px'></div>";
        assert!(run(&html, "").1.fragments.len() == 43);
    }

    #[test]
    fn an_inline_block_that_scrolls_sits_on_its_bottom_edge() {
        let css = "body { line-height: 20px }";
        let html = "<div>a<span id=i style='display:inline-block;overflow:hidden;height:40px'>b</span></div>";
        let i = rect(html, css, "i");
        assert_eq!((i.y, i.h), (8.0, 40.0));
        let html = "<div id=d>a<span style='display:inline-block;overflow:hidden;height:40px'>b</span></div>";
        // The line is taller than 40: the descent of the strut goes below the inline-block.
        assert!(rect(html, css, "d").h > 40.0);
    }

    #[test]
    fn a_long_log_in_an_auto_box() {
        // The first pass stops at the first line below the room. The second pass lays out all of it, 15 narrower.
        let html = format!("<div style='overflow:auto;height:50px'>{}<p id=last>end</p></div>", "<p>line</p>".repeat(20));
        assert_eq!(rect(&html, "", "last").w, 784.0 - 15.0);
        // The list numbers start again with the second pass.
        let html = "<ol style='overflow:auto;height:20px'><li>a<li>b<li>c</ol>";
        let out = dump(html, "");
        assert!(out.contains("marker 3") && !out.contains("marker 4"), "{out}");
    }

    // A containing block at (8, 8) with a border of 2 and a padding of 5: its padding box is at (10, 10), 310 by 90.
    const CB: &str = "#c { position: relative; width: 300px; height: 80px; border: 2px solid; padding: 5px } .a { position: absolute }";

    fn in_cb(inner: &str) -> Rect {
        rect(&format!("<div id=c>{inner}</div>"), CB, "a")
    }

    #[test]
    fn relative_moves_a_block_and_what_is_inside() {
        let html = "<div id=d style='position:relative;top:5px;left:10px;width:200px'><p id=p style='margin:0'>x</p></div>";
        assert_eq!(rect(html, "", "d"), r(18.0, 13.0, 200.0, 22.0));
        assert_eq!(rect(html, "", "p"), r(18.0, 13.0, 200.0, 22.0));
        // `right` and `bottom` move the other way. `left` wins over `right`.
        let html = "<div id=d style='position:relative;bottom:5px;right:10px;left:1px'>x</div>";
        assert_eq!(rect(html, "", "d"), r(9.0, 3.0, 784.0, 22.0));
    }

    #[test]
    fn relative_moves_an_inline_box_and_its_text() {
        let html = "<p>ab <span id=s style='position:relative;top:-4px;left:3px'>cd</span></p>";
        let (_, layout) = run(html, "");
        let text = layout.fragments.iter().rev().find(|f| matches!(f.kind, crate::fragment::Kind::Text { .. })).unwrap().rect;
        // "ab " is 28.8 wide: the span and its text start at 8 + 28.8 + 3, and 4 higher than the line.
        let span = rect(html, "", "s");
        assert!((span.x - 39.8).abs() < 0.01 && span.y == 12.0, "{span:?}");
        assert_eq!((text.x, text.y), (span.x, span.y));
    }

    #[test]
    fn absolute_boxes_in_their_containing_block() {
        assert_eq!(in_cb("<div id=a class=a style='top:10px;left:20px;width:50px;height:20px'></div>"), r(30.0, 20.0, 50.0, 20.0));
        // `right` and `bottom`, and the width of the content ("corner", 6 letters).
        assert_eq!(in_cb("<div id=a class=a style='right:0;bottom:0'>corner</div>"), r(10.0 + 310.0 - 57.6, 78.0, 57.6, 22.0));
        // `left` and `right` fix the width, `top` and `bottom` the height.
        assert_eq!(in_cb("<div id=a class=a style='left:10px;right:30px;top:5px;bottom:10px'>x</div>"), r(20.0, 15.0, 270.0, 75.0));
        // All fixed, with `auto` margins: in the middle.
        assert_eq!(in_cb("<div id=a class=a style='left:0;right:0;top:0;bottom:0;width:100px;height:30px;margin:auto'></div>"), r(115.0, 40.0, 100.0, 30.0));
        // Percent of the padding box, and `box-sizing`.
        assert_eq!(in_cb("<div id=a class=a style='left:10%;top:50%;width:50%;height:50%;padding:4px;box-sizing:border-box'></div>"), r(41.0, 55.0, 155.0, 45.0));
        // `float` does nothing on it.
        assert_eq!(in_cb("<div id=a class=a style='right:10px;top:0;float:left'>corner</div>"), r(10.0 + 300.0 - 57.6, 10.0, 57.6, 22.0));
    }

    #[test]
    fn with_no_offsets_an_absolute_box_stays_at_its_static_position() {
        // A block: under the line before it, at the left of the content box (15, 15).
        assert_eq!(in_cb("before<div id=a class=a>x</div>"), r(15.0, 37.0, 9.6, 22.0));
        // An inline: where the line is, after "ab " (28.8).
        let a = in_cb("ab <span id=a class=a>x</span> cd");
        assert!((a.x - 43.8).abs() < 0.01 && (a.y, a.w, a.h) == (15.0, 9.6, 22.0), "{a:?}");
        // It takes no room: the text after it is where it would be without it.
        let html = "<div id=c><div class=a>x</div><p id=a style='margin:0'>y</p></div>";
        assert_eq!(rect(html, CB, "a"), r(15.0, 15.0, 300.0, 22.0));
    }

    #[test]
    fn the_window_is_the_containing_block_without_a_positioned_box() {
        let html = "<div style='margin:50px'><div id=a style='position:absolute;left:600px;top:20px;width:100px'></div></div>";
        assert_eq!(rect(html, "", "a"), r(600.0, 20.0, 100.0, 0.0));
        // `fixed` also inside a positioned box. The test window is 800 by 600.
        let html = "<div style='position:relative;margin:50px'><div id=a style='position:fixed;right:0;bottom:0;width:80px;height:30px'></div></div>";
        assert_eq!(rect(html, "", "a"), r(720.0, 570.0, 80.0, 30.0));
    }

    #[test]
    fn absolute_inside_absolute() {
        let html = "<div id=c><div class=a style='top:10px;left:10px;width:150px;height:60px'><div id=a class=a style='right:5px;bottom:5px;width:20px;height:20px'></div></div></div>";
        assert_eq!(rect(html, CB, "a"), r(145.0, 55.0, 20.0, 20.0));
    }

    #[test]
    fn bottom_zero_in_a_block_with_auto_height() {
        let html = "<div id=c style='height:auto'><div id=a class=a style='bottom:0;left:0'>x</div>a<br>b<br>c</div>";
        // The padding box is 66 + 10 tall.
        assert_eq!(rect(html, CB, "a"), r(10.0, 10.0 + 76.0 - 22.0, 9.6, 22.0));
    }

    #[test]
    fn a_static_position_follows_text_align() {
        // "mid line" is 8 letters, 76.8 wide: the line starts at 8 + (300 - 76.8) / 2, and the box after "mid ".
        let html = "<div style='text-align:center;width:300px'>mid <span id=a style='position:absolute'>abs</span> line</div>";
        assert_eq!(rect(html, "", "a").x, 8.0 + 111.6 + 38.4);
    }

    #[test]
    fn a_relative_inline_box_is_a_containing_block() {
        // The span starts after "text " (48 px) at y = 8. `left: 0; top: 0` is the corner of its padding box.
        let html = "<div style='width:300px'>text <span style='position:relative;padding-left:20px'>in <span id=a style='position:absolute;left:0;top:0'>x</span></span></div>";
        let a = rect(html, "", "a");
        assert_eq!((a.x, a.y), (8.0 + 48.0, 8.0));
    }

    #[test]
    fn a_very_deep_tree_stops_at_the_limit() {
        let (_, layout) = run(&"<div>".repeat(50_000), "");
        assert!(layout.fragments.len() <= super::MAX_DEPTH + 3, "{}", layout.fragments.len());
    }
}
