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
//! The walk calls itself for each level of the tree. A tree deeper than `MAX_DEPTH` stops there: what is deeper is not
//! laid out. Firefox does the same (`MAX_REFLOW_DEPTH`). Chrome has no limit, and a very deep tree crashes its tab.
//!
//! Comments compare Rust with JavaScript, because you know JS.

use crate::boxes::{Child, Kind};
use crate::fragment::{self, Fragment, Rect};
use crate::{Cx, inline};
use bare_css::cascade::ComputedStyle;
use bare_css::values::{Keyword, Length, Property, Value};
use bare_html::dom::NodeId;

/// How deep layout goes. Each level uses some call stack (about 1 KB in a debug build), and the main thread has 8 MB.
pub const MAX_DEPTH: usize = 400;

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

/// A length in pixels. A percent is of `base`. `None` for `auto` and `none`, and for a percent of an unknown base.
pub fn resolve(cx: &Cx, v: Value, base: Option<f32>) -> Option<f32> {
    match v {
        Value::Length(Length::Px(x)) => Some(x),
        Value::Length(Length::Percent(p)) => base.map(|b| b * p / 100.0),
        Value::Length(Length::Calc(i)) => {
            let (px, pct) = cx.tree.styles.calc(i);
            match base {
                Some(b) => Some(px + b * pct / 100.0),
                None if pct == 0.0 => Some(px),
                None => None,
            }
        }
        _ => None,
    }
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
    let b = |p| style.px(p);
    let border = Sides { top: b(Property::BorderTopWidth), right: b(Property::BorderRightWidth), bottom: b(Property::BorderBottomWidth), left: b(Property::BorderLeftWidth) };
    let p = |p| resolve(cx, style.get(p), Some(cb_width)).unwrap_or(0.0).max(0.0);
    let padding = Sides { top: p(Property::PaddingTop), right: p(Property::PaddingRight), bottom: p(Property::PaddingBottom), left: p(Property::PaddingLeft) };
    (border, padding)
}

/// Does this block make its own block formatting context? Then the margins of its children do not collapse with its own.
pub fn own_context(cx: &Cx, node: NodeId) -> bool {
    if cx.tree.kind(node) == Kind::Atomic || Some(node) == cx.tree.root() {
        return true;
    }
    matches!(
        cx.tree.style(node).get(Property::Display),
        Value::Keyword(Keyword::Table | Keyword::TableCell | Keyword::TableCaption | Keyword::Flex | Keyword::Grid)
    )
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
}

/// Lay out a block-level box in `space`. Its fragment is at x = its left margin, y = 0: the parent moves it down.
pub fn block(cx: &mut Cx, node: NodeId, space: Space) -> Done {
    let style = cx.tree.style(node);
    let (border, padding) = border_padding(cx, style, space.width);
    let bp = Sides {
        top: border.top + padding.top,
        right: border.right + padding.right,
        bottom: border.bottom + padding.bottom,
        left: border.left + padding.left,
    };
    let (width, margin_left, _) = widths(cx, style, space.width, bp.horizontal());
    let margin = |p| resolve(cx, style.get(p), Some(space.width)).unwrap_or(0.0);
    let (margin_top, margin_bottom) = (margin(Property::MarginTop), margin(Property::MarginBottom));

    let fragment = cx.out.len();
    cx.out.push(Fragment { rect: Rect { x: margin_left, y: 0.0, w: width + bp.horizontal(), h: 0.0 }, node, kind: crate::fragment::Kind::Box, end: 0 });
    // The height comes after the children: `h` is filled in below.

    let border_box = style.get(Property::BoxSizing) == Value::Keyword(Keyword::BorderBox);
    let content = |v: f32| if border_box { (v - bp.vertical()).max(0.0) } else { v };
    let height = resolve(cx, style.get(Property::Height), space.height).map(content);
    let min = resolve(cx, style.get(Property::MinHeight), space.height).map_or(0.0, content);
    let max = resolve(cx, style.get(Property::MaxHeight), space.height).map(content);
    let clamp = |h: f32| max.map_or(h, |m| h.min(m)).max(min);

    let own = own_context(cx, node);
    let top_open = !own && bp.top == 0.0;
    let bottom_open = !own && bp.bottom == 0.0 && height.is_none();
    if cx.tree.kind(node) == Kind::ListItem {
        cx.marker = Some(node);
        // The marker goes on the first line inside the item (`inline.rs`). If no line takes it, it goes at the top.
    }
    let inner = Space { width, height: height.map(clamp) };
    let kids = if cx.depth < MAX_DEPTH {
        cx.depth += 1;
        let kids = children(cx, node, inner, top_open, bp.top, bp.left);
        cx.depth -= 1;
        kids
    } else {
        Kids { y: 0.0, top: Strut::default(), bottom: Strut::default(), open: top_open }
    };
    if cx.marker == Some(node) {
        cx.marker = None;
        inline::lone_marker(cx, node, bp.left, bp.top);
    }

    let mut done = Done { fragment, margin_top, margin_bottom, ..Done::default() };
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
    let content_height = clamp(height.unwrap_or(content_height));
    done.through = kids.open && bottom_open && content_height == 0.0 && bp.vertical() == 0.0;
    if kids.open && !done.through {
        // The margins inside reached the top, but the block has a height: they stay at the top, and the bottom is closed.
        done.bottom = Strut::default();
    }
    done.height = content_height + bp.vertical();
    let end = cx.out.len() as u32;
    let f = &mut cx.out[fragment];
    f.rect.h = done.height;
    f.end = end;
    done
}

/// The content width and the left and right margins of a block (CSS 2.1, 10.3.3, with `box-sizing`, `min-width` and
/// `max-width`). `bp`: the borders and padding of both sides.
pub fn widths(cx: &Cx, style: &ComputedStyle, cb: f32, bp: f32) -> (f32, f32, f32) {
    let border_box = style.get(Property::BoxSizing) == Value::Keyword(Keyword::BorderBox);
    let content = |v: f32| if border_box { (v - bp).max(0.0) } else { v };
    let ml = resolve(cx, style.get(Property::MarginLeft), Some(cb));
    let mr = resolve(cx, style.get(Property::MarginRight), Some(cb));
    let solve = |width: Option<f32>| -> (f32, f32, f32) {
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
    };
    let (mut w, mut l, mut r) = solve(resolve(cx, style.get(Property::Width), Some(cb)).map(content));
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

/// The result of the children of a block.
struct Kids {
    /// The bottom of the last child (its border box), from the top of the content box.
    y: f32,
    /// The margins that came out through the top. Only if `top_open`.
    top: Strut,
    /// The margins after the last child, not used yet.
    bottom: Strut,
    /// Nothing stopped the margins at the top: no child with a border, padding, a line or a height.
    open: bool,
}

/// Lay out the children of a block, one under the other. `top_open`: the margins of the first child may go out through the
/// top. The fragments of the children are placed relative to the parent's border box: `dx`, `dy` are the parent's
/// border and padding.
fn children(cx: &mut Cx, node: NodeId, space: Space, top_open: bool, dy: f32, dx: f32) -> Kids {
    let mut kids = Kids { y: 0.0, top: Strut::default(), bottom: Strut::default(), open: top_open };
    // `bottom` is the strut that is open now: the margins since the last child that has content.
    let tree = cx.tree;
    // A copy of the `Tree`, so that the loop over the children does not hold `cx`, and the loop body can change it.
    let mut counter = tree.counter(node);
    for child in tree.children(node) {
        match child {
            Child::Block(b) => {
                if cx.tree.kind(b) == Kind::ListItem {
                    cx.number = cx.tree.list_number(&mut counter, b);
                }
                let done = block(cx, b, space);
                kids.bottom.add(done.margin_top);
                kids.bottom.merge(done.top);
                if done.through {
                    kids.bottom.add(done.margin_bottom);
                    let y = if kids.open { 0.0 } else { kids.y + kids.bottom.sum() };
                    cx.out[done.fragment].rect.y = y + dy;
                    cx.out[done.fragment].rect.x += dx;
                    continue;
                }
                let y = place(&mut kids);
                cx.out[done.fragment].rect.y = y + dy;
                cx.out[done.fragment].rect.x += dx;
                kids.y = y + done.height;
                kids.bottom = Strut::default();
                kids.bottom.add(done.margin_bottom);
                kids.bottom.merge(done.bottom);
            }
            Child::Lines(first, last) => {
                let from = cx.out.len();
                let lines = inline::lines(cx, node, first, last, space);
                if !lines.any {
                    continue;
                    // Only empty inline boxes: no line, and the margins go on through.
                }
                let y = place(&mut kids);
                fragment::shift(&mut cx.out, from, dx, y + dy);
                kids.y = y + lines.height;
            }
        }
    }
    kids
}

/// A child with content comes: the open margins are resolved, and the child goes under them. The new top of the child.
fn place(kids: &mut Kids) -> f32 {
    if kids.open {
        // The first content: the margins before it go out through the top of the parent.
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
mod tests {
    use crate::fragment::Rect;
    use bare_css::cascade::{Origin, RuleSet, UA_CSS};
    use bare_html::dom::{Atom, Document, NodeId};
    use bare_html::tree::parse;

    /// The layout of a page in a window of 800 by 600, as text.
    fn dump(html: &str, css: &str) -> String {
        let (doc, layout) = run(html, css);
        crate::dump(&doc, &layout)
    }

    fn run(html: &str, css: &str) -> (Document, crate::fragment::Layout) {
        let mut doc = parse(html);
        let mut rules = RuleSet::new((800.0, 600.0));
        rules.add(&mut doc, UA_CSS, Origin::UserAgent);
        rules.add(&mut doc, css, Origin::Author);
        let styles = rules.compute(&doc);
        let layout = crate::layout(&doc, &styles, (800.0, 600.0));
        (doc, layout)
    }

    /// The border box of the element with this `id`.
    fn rect(html: &str, css: &str, id: &str) -> Rect {
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
            "html 0,0 800x92\n  body 8,16 784x60\n    p 8,16 784x22\n    p 8,54 784x22\n"
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
    fn a_very_deep_tree_stops_at_the_limit() {
        let (_, layout) = run(&"<div>".repeat(50_000), "");
        assert!(layout.fragments.len() <= super::MAX_DEPTH + 3, "{}", layout.fragments.len());
    }
}
