//! Hit testing: the topmost element at a point of the page.
//!
//! The answer must be the element that a person sees at that point, so this walk is the walk of `display.rs` (CSS 2.1 Appendix E)
//! with a point in place of the pixels: it "paints" the page in the same order, and the last element whose box holds the point is
//! the top one. Inside one context the layers are, from the bottom: the box itself, the boxes with a negative `z-index`, the blocks
//! in tree order, the floats, the lines (inline boxes, text, markers and inline-blocks; a flex or grid item counts as an inline-block),
//! the positioned boxes with `z-index` 0 or `auto`, and the boxes with a positive `z-index`. A float, an inline-block, a positioned
//! box and a box that clips are contexts: the walk goes into them. A box that makes a stacking context (the root, a positioned box or
//! a flex item with a `z-index`, a sticky box, any box with `opacity` under 1) takes the boxes with a `z-index` other than 0 that are
//! inside it, at any depth that is not another stacking context, and puts them in `z-index` order (then tree order), as `hoisted` of
//! `display.rs` does.
//!
//! The dependency: `layer`, `stack` and `item` here and `Builder::layer`, `Builder::stack` and `Builder::item` in `display.rs` must
//! agree. When paint learns a new rule, this file needs the same rule. A test paints every box of a page in its own color and checks
//! that the hit test names the box whose color is on top at each sampled pixel, so a rule that only one of them knows makes that test fail.
//!
//! What the answer is: an element. A text fragment gives the parent of its text node, and a list marker gives its item. A point on the
//! page that no box covers gives the root element (Chrome does the same: `html`). The root is always the last of the list, also when
//! its `pointer-events` is `none` (Chrome does the same). A box is hit when its `visibility` is `visible` and its `pointer-events` is
//! not `none` (`pointer-events` is inherited, a child with `auto` or `all` is hit again). The value words of SVG (`visiblePainted` and
//! the like) act as `auto`, as they do for an HTML element. A box with `opacity: 0` is hit. A box with `visibility` other than `visible`
//! or `pointer-events: none` is not hit, and the boxes inside it can be. A box that clips (`overflow` other than `visible`) hides the
//! parts of its children outside its padding box.
//!
//! The cost is one pass over the fragments: a context that clips skips its inside when the point is outside (a fixed box inside it
//! is still looked at, see below), and nothing else skips, because a box can paint outside its parent. The lists of the deferred
//! boxes are local to each context and stay empty (no allocation) for most contexts.
//!
//! Scrolling (S4 topic 5): `hit_test_scrolled` takes the scroll offsets. The point is in window coordinates. The page offset
//! is added to it for the boxes of the page, and the offset of a scroll container is added when the walk goes into it, so a
//! click inside a scrolled box or below the fold hits what is drawn there. A `position: fixed` box is hit with the window
//! point, as `Painter::draw_view` draws it, and no clip of a box around it cuts it: when the point is outside a clipping box, the walk
//! still goes into the boxes that `Areas::fixed` lists inside it, and only those can be hit. A point on a scrollbar of a container hits the
//! container, not what is behind.
//!
//! Gaps: no transforms (the engine has none), no `clip-path` or border radius (a point in the rounded-off corner still hits), no
//! `pointer-events` words of SVG that differ from `auto`.

use bare_css::cascade::{ComputedStyle, Styles};
use bare_css::values::{Keyword, Property, Value};
use bare_html::dom::{Document, NodeData, NodeId};
use bare_layout::boxes::{Kind, Tree};
use bare_layout::fragment::{Fragment, Kind as FragmentKind, Layout, Rect};
use bare_layout::scroll::Areas;

use crate::Scroll;

/// How a box is painted. The twin of `Layer` in `display.rs`.
enum Layer {
    Block,
    Inline,
    Atomic,
    Float,
    /// A positioned box, or a box that makes a stacking context. The number is the `z-index` (0 for `auto`).
    Positioned(i32),
    Clipped,
}

/// Deeper than this the contexts stop, as in `display.rs`: the same boxes are not drawn, so they are not hit.
const MAX_DEPTH: usize = 400;

type Point = (f32, f32);

/// The topmost element at `(x, y)` in page coordinates. `None` when the page has no layout.
pub fn hit_test(doc: &Document, styles: &Styles, layout: &Layout, x: f32, y: f32) -> Option<NodeId> {
    hit(doc, styles, layout, None, (x, y), (x, y)).first().copied()
}

/// The topmost element at the window point `(x, y)` of a page that is scrolled by `scroll`.
pub fn hit_test_scrolled(doc: &Document, styles: &Styles, layout: &Layout, areas: &Areas, scroll: &Scroll, x: f32, y: f32) -> Option<NodeId> {
    hits_scrolled(doc, styles, layout, areas, scroll, x, y).first().copied()
}

/// All the elements at the window point `(x, y)`, the topmost first and the root last (`document.elementsFromPoint`, before the
/// retargeting of shadow trees). Empty when the page has no layout.
pub fn hits_scrolled(doc: &Document, styles: &Styles, layout: &Layout, areas: &Areas, scroll: &Scroll, x: f32, y: f32) -> Vec<NodeId> {
    hit(doc, styles, layout, Some((areas, scroll)), (x + scroll.x.round(), y + scroll.y.round()), (x, y))
}

fn hit(doc: &Document, styles: &Styles, layout: &Layout, scrolled: Option<(&Areas, &Scroll)>, page: Point, window: Point) -> Vec<NodeId> {
    let tree = Tree::new(doc, styles);
    let Some(root) = tree.root() else { return Vec::new() };
    let frags = &layout.fragments;
    if frags.is_empty() {
        return Vec::new();
    }
    // Most pages have no `z-index`, `opacity` or flex container: one pass over the styles (as `display.rs` makes) spares the checks of each box.
    let (mut stacks, mut flex) = (false, false);
    for style in styles.each() {
        stacks |= matches!(style.get(Property::Opacity), Value::Number(o) if o < 1.0) || matches!(style.get(Property::ZIndex), Value::Number(_)) || style.get(Property::Position) == Value::Keyword(Keyword::Sticky);
        flex |= matches!(style.get(Property::Display), Value::Keyword(Keyword::Flex | Keyword::InlineFlex | Keyword::Grid | Keyword::InlineGrid));
        if stacks && flex {
            break;
        }
    }
    let h = Hit { tree, doc, frags, scrolled, window, stacks, flex };
    // From the bottom to the top, as painted.
    let mut painted = Vec::with_capacity(8);
    h.context(0, 0, page, true, &mut painted, &mut Vec::new());
    let mut top_first: Vec<NodeId> = Vec::with_capacity(painted.len() + 1);
    for &n in painted.iter().rev() {
        if !top_first.contains(&n) {
            top_first.push(n);
        }
    }
    if top_first.last() != Some(&root) {
        top_first.retain(|&n| n != root);
        top_first.push(root);
    }
    top_first
}

struct Hit<'a> {
    tree: Tree<'a>,
    doc: &'a Document,
    frags: &'a [Fragment],
    scrolled: Option<(&'a Areas, &'a Scroll)>,
    /// The point in window coordinates: where a fixed box is hit.
    window: Point,
    /// Some box has `opacity` under 1, a `z-index` or `position: sticky`.
    stacks: bool,
    /// Some box is a flex or grid container.
    flex: bool,
}

/// A box of a stacking context with a `z-index` other than 0, on its way up to the context: where it is, and its point.
struct Waiting {
    z: i32,
    frag: usize,
    at: Point,
    live: bool,
}

/// What the lines layer holds, in tree order.
enum Line {
    Node(NodeId),
    Context(usize, Point, bool),
}

impl Hit<'_> {
    fn inside(&self, p: Point, r: Rect) -> bool {
        p.0 >= r.x && p.0 < r.x + r.w && p.1 >= r.y && p.1 < r.y + r.h
    }

    /// Can a pointer hit a box with this style?
    fn hittable(&self, style: &ComputedStyle) -> bool {
        style.get(Property::Visibility) == Value::Keyword(Keyword::Visible) && style.get(Property::PointerEvents) != Value::Keyword(Keyword::None)
    }

    /// Is this box an item of a flex or grid container? It paints as an inline-block.
    fn item(&self, node: NodeId, kind: Kind) -> bool {
        if !self.flex || !matches!(kind, Kind::Block | Kind::ListItem) {
            return false;
        }
        self.tree.is_flex_or_grid(self.doc.flat_parent(node))
    }

    /// The place of a box in the order of its stacking context, if the box makes one. The twin of `Builder::stack`.
    fn stack(&self, f: &Fragment, style: &ComputedStyle, kind: Kind, position: Value) -> Option<i32> {
        if !self.stacks {
            return None;
        }
        let z = match style.get(Property::ZIndex) {
            Value::Number(n) if !matches!(position, Value::Keyword(Keyword::Static)) || self.item(f.node, kind) => Some(n as i32),
            _ if matches!(position, Value::Keyword(Keyword::Sticky)) => Some(0),
            _ => None,
        };
        match style.get(Property::Opacity) {
            Value::Number(o) if o < 1.0 => Some(z.unwrap_or(0)),
            _ => z,
        }
    }

    /// The twin of `Builder::layer`.
    fn layer(&self, f: &Fragment, style: &ComputedStyle) -> Layer {
        let kind = self.tree.kind(f.node);
        match kind {
            Kind::OutOfFlow => return Layer::Positioned(self.stack(f, style, kind, style.get(Property::Position)).unwrap_or(0)),
            Kind::Float => return Layer::Float,
            _ => {}
        }
        if kind != Kind::Inline {
            let position = style.get(Property::Position);
            let stack = self.stack(f, style, kind, position);
            if stack.is_some() || !matches!(position, Value::Keyword(Keyword::Static)) {
                return Layer::Positioned(stack.unwrap_or(0));
            }
        }
        match kind {
            Kind::Inline => Layer::Inline,
            Kind::Atomic => Layer::Atomic,
            _ if self.item(f.node, kind) => Layer::Atomic,
            _ if clips(style) => Layer::Clipped,
            _ => Layer::Block,
        }
    }

    /// Is a fixed box (that no other fixed box holds) among the fragments `i + 1..end`?
    fn fixed_inside(&self, i: usize, end: usize) -> bool {
        self.scrolled.is_some_and(|(areas, _)| {
            let k = areas.fixed.partition_point(|&f| f as usize <= i);
            areas.fixed.get(k).is_some_and(|&f| (f as usize) < end)
        })
    }

    /// Put the elements under the point `p` inside the context that starts at the fragment `i` on `out`, from the bottom to the top.
    /// `live` is false while the point is outside a clipping box around: then only a fixed box can be hit. `hoist` is the list of
    /// the stacking context around: the boxes with a `z-index` wait there.
    fn context(&self, i: usize, depth: usize, p: Point, live: bool, out: &mut Vec<NodeId>, hoist: &mut Vec<Waiting>) {
        if depth >= MAX_DEPTH {
            return;
        }
        let f = self.frags[i];
        let style = self.tree.style(f.node);
        let sc = i == 0 || self.stack(&f, style, self.tree.kind(f.node), style.get(Property::Position)).is_some();
        let mut own = Vec::new();
        let hoist = if sc { &mut own } else { hoist };
        let mut live = live;
        let mut p = p;
        if live && self.inside(p, f.rect) && self.hittable(style) {
            out.push(f.node);
        }
        if i > 0 && clips(style) {
            // The inside of a box that clips is reached only through its padding box.
            if live && !self.inside(p, padding_box(&f, style)) {
                live = false;
            }
            // A point on one of its scrollbars is the container's.
            if live && let Some((areas, _)) = self.scrolled && let Some(a) = areas.get(f.node) {
                let bar = bare_layout::block::SCROLLBAR;
                if (a.bars.0 && p.0 >= a.pad.x + a.pad.w - bar) || (a.bars.1 && p.1 >= a.pad.y + a.pad.h - bar) {
                    return;
                }
            }
            if !live && !self.fixed_inside(i, f.end as usize) {
                return;
            }
            // Inside a scroll container the point moves with its offset.
            if let Some((_, scroll)) = self.scrolled {
                let (sx, sy) = scroll.of(f.node);
                p = (p.0 + sx.round(), p.1 + sy.round());
            }
        }
        let inside = out.len();
        let (mut floats, mut lines, mut positioned): (Vec<(usize, Point, bool)>, Vec<Line>, Vec<(usize, Point, bool)>) = (Vec::new(), Vec::new(), Vec::new());
        let end = f.end as usize;
        let mut j = i + 1;
        while j < end {
            let g = self.frags[j];
            match g.kind {
                FragmentKind::Box => {
                    let style = self.tree.style(g.node);
                    match self.layer(&g, style) {
                        Layer::Block => {
                            if live && self.inside(p, g.rect) && self.hittable(style) {
                                out.push(g.node);
                            }
                            j += 1;
                        }
                        Layer::Inline => {
                            if live && self.inside(p, g.rect) && self.hittable(style) {
                                lines.push(Line::Node(g.node));
                            }
                            j += 1;
                        }
                        Layer::Atomic => {
                            if live || self.fixed_inside(j, g.end as usize) {
                                lines.push(Line::Context(j, p, live));
                            }
                            j = g.end as usize;
                        }
                        Layer::Float => {
                            if live || self.fixed_inside(j, g.end as usize) {
                                floats.push((j, p, live));
                            }
                            j = g.end as usize;
                        }
                        Layer::Positioned(z) => {
                            let kind = self.tree.kind(g.node);
                            let fixed = self.scrolled.is_some() && kind != Kind::Inline && self.tree.position(g.node) == Keyword::Fixed;
                            let mut at = if fixed { self.window } else { p };
                            // A sticky box is drawn moved: the point is in its place when it is moved back.
                            if let Some((areas, scroll)) = self.scrolled.filter(|(a, _)| !a.sticky.is_empty())
                                && kind != Kind::Inline
                                && let Some(k) = areas.sticky_of(g.node)
                            {
                                let (own, _) = areas.stuck(k, (scroll.x, scroll.y), &|n| scroll.of(n));
                                at = (at.0 - own.0.round(), at.1 - own.1.round());
                            }
                            let live = live || fixed;
                            if live || self.fixed_inside(j, g.end as usize) {
                                if z != 0 {
                                    hoist.push(Waiting { z, frag: j, at, live });
                                } else {
                                    positioned.push((j, at, live));
                                }
                            }
                            j = g.end as usize;
                        }
                        Layer::Clipped => {
                            if live || self.fixed_inside(j, g.end as usize) {
                                self.context(j, depth + 1, p, live, out, hoist);
                            }
                            j = g.end as usize;
                        }
                    }
                }
                FragmentKind::Text { .. } => {
                    // A text node is never the target of a pointer event: the element around it is.
                    if live && self.inside(p, g.rect) && self.hittable(self.tree.style(g.node)) && let Some(e) = self.doc.parent(g.node) {
                        lines.push(Line::Node(e));
                    }
                    j += 1;
                }
                FragmentKind::Marker { .. } => {
                    if live && self.inside(p, g.rect) && self.hittable(self.tree.style(g.node)) {
                        lines.push(Line::Node(g.node));
                    }
                    j += 1;
                }
                FragmentKind::Static => j += 1,
            }
        }
        for (j, at, live) in floats {
            self.context(j, depth + 1, at, live, out, hoist);
        }
        for line in lines {
            match line {
                Line::Node(n) => out.push(n),
                Line::Context(j, at, live) => self.context(j, depth + 1, at, live, out, hoist),
            }
        }
        for (j, at, live) in positioned {
            self.context(j, depth + 1, at, live, out, hoist);
        }
        if sc && !own.is_empty() {
            // The negative ones go behind the blocks of the context, the others over everything it painted.
            own.sort_by_key(|w| (w.z, w.frag));
            let mut behind = Vec::new();
            for w in &own {
                let to = if w.z < 0 { &mut behind } else { &mut *out };
                self.context(w.frag, depth + 1, w.at, w.live, to, &mut Vec::new());
            }
            out.splice(inside..inside, behind);
        }
    }
}

fn clips(style: &ComputedStyle) -> bool {
    let hidden = |p| !matches!(style.get(p), Value::Keyword(Keyword::Visible));
    hidden(Property::OverflowX) || hidden(Property::OverflowY)
}

fn padding_box(f: &Fragment, style: &ComputedStyle) -> Rect {
    let w = |p| style.px(p).max(0.0);
    let (l, t, r, b) = (w(Property::BorderLeftWidth), w(Property::BorderTopWidth), w(Property::BorderRightWidth), w(Property::BorderBottomWidth));
    Rect { x: f.rect.x + l, y: f.rect.y + t, w: (f.rect.w - l - r).max(0.0), h: (f.rect.h - t - b).max(0.0) }
}

/// The nearest element at or above `n` (a text node gives its parent). Used by callers that start from a node.
pub fn element_of(doc: &Document, n: NodeId) -> Option<NodeId> {
    let mut cur = Some(n);
    while let Some(c) = cur {
        if matches!(doc.data(c), NodeData::Element { .. }) {
            return Some(c);
        }
        cur = doc.parent(c);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use bare_css::cascade::{Origin, RuleSet, Source, UA_CSS};
    use bare_html::tree;
    use crate::Scroll;

    const W: u32 = 300;
    const H: u32 = 400;

    struct Page {
        doc: Document,
        styles: Styles,
        layout: Layout,
    }

    fn page(html: &str) -> Page {
        let mut doc = tree::parse(html);
        let viewport = (W as f32, H as f32);
        let mut rules = RuleSet::new(viewport);
        rules.add(&mut doc, UA_CSS, Origin::UserAgent);
        for source in bare_css::cascade::sources(&doc, viewport) {
            if let Source::Inline(text) = source {
                rules.add(&mut doc, &text, Origin::Author);
            }
        }
        let styles = rules.compute(&doc);
        let layout = bare_layout::layout(&doc, &styles, viewport);
        Page { doc, styles, layout }
    }

    impl Page {
        fn at(&self, x: f32, y: f32) -> String {
            let n = hit_test(&self.doc, &self.styles, &self.layout, x, y).expect("a layout");
            self.name(n)
        }

        fn at_scrolled(&self, scroll: &Scroll, x: f32, y: f32) -> String {
            let areas = bare_layout::scroll::areas(&self.doc, &self.styles, &self.layout);
            let n = hit_test_scrolled(&self.doc, &self.styles, &self.layout, &areas, scroll, x, y).expect("a layout");
            self.name(n)
        }

        fn id(&self, id: &str) -> NodeId {
            let mut stack = vec![self.doc.root()];
            while let Some(n) = stack.pop() {
                if self.doc.attr(n, bare_html::dom::Atom::ID) == Some(id) {
                    return n;
                }
                stack.extend(self.doc.children(n));
            }
            panic!("no #{id}")
        }

        fn name(&self, n: NodeId) -> String {
            match self.doc.data(n) {
                NodeData::Element { .. } => {
                    let id = self.doc.find_atom("id").and_then(|a| self.doc.attr(n, a)).unwrap_or("");
                    if id.is_empty() { self.doc.element_name(n).to_string() } else { id.to_string() }
                }
                _ => "?".to_string(),
            }
        }
    }

    #[test]
    fn blocks_and_the_root() {
        let p = page("<style>body{margin:0} #a{height:50px;background:red} #b{height:30px;margin-top:20px}</style><div id=a></div><div id=b></div>");
        assert_eq!(p.at(10.0, 10.0), "a");
        assert_eq!(p.at(10.0, 60.0), "body", "the gap is the margin of b: the body is there");
        assert_eq!(p.at(10.0, 80.0), "b", "a block with no background is hit too");
        assert_eq!(p.at(10.0, 150.0), "html", "no box: the root, as in Chrome");
        assert_eq!(p.at(-5.0, 10.0), "html");
    }

    #[test]
    fn text_gives_its_element_and_a_nested_box_beats_its_parent() {
        let p = page("<style>body{margin:0} #o{width:200px;padding:10px}</style><div id=o>hello <b id=i>world</b></div>");
        // The line starts at y 10. "hello " is 6 letters of 9.6 px from x 10; "world" starts at x 67.6.
        assert_eq!(p.at(20.0, 15.0), "o");
        assert_eq!(p.at(80.0, 15.0), "i", "the text of a <b> is the <b>");
        assert_eq!(p.at(150.0, 15.0), "o", "the empty rest of the line is the block");
        assert_eq!(p.at(5.0, 15.0), "o", "the padding of the block belongs to it");
    }

    #[test]
    fn lines_paint_over_blocks_even_when_the_block_comes_later() {
        // The inline <span> is in the lines layer: it paints over a block box that is later in tree order.
        let p = page("<style>body{margin:0} .blk{position:relative;height:0} #p{height:40px;margin-top:-20px;background:#ccc} #s{background:yellow}</style><div><span id=s>over</span></div><div id=p></div>");
        // The span is on the first line (0..~18 px). The block #p covers y -20..20 and is later in tree order, but blocks
        // paint under lines, so the text wins where both are.
        assert_eq!(p.at(5.0, 8.0), "s");
        assert_eq!(p.at(100.0, 10.0), "p", "where the span is not, the block is");
    }

    #[test]
    fn absolute_boxes_are_on_top_and_later_ones_beat_earlier_ones() {
        let p = page(
            "<style>body{margin:0} div{position:absolute;width:100px;height:100px} #a{left:10px;top:10px;background:red} #b{left:60px;top:60px;background:blue} #c{left:0;top:0;width:300px;height:5px;position:static}</style>\
             <div id=a></div><div id=b></div><p id=c></p>",
        );
        assert_eq!(p.at(20.0, 20.0), "a");
        assert_eq!(p.at(80.0, 80.0), "b", "b is later in tree order: on top where they overlap");
        assert_eq!(p.at(105.0, 105.0), "b");
    }

    #[test]
    fn floats_are_over_blocks_and_a_clipping_box_hides_what_sticks_out() {
        let p = page(
            "<style>body{margin:0} #f{float:left;width:50px;height:50px} #blk{height:100px} #clip{overflow:hidden;width:60px;height:60px;margin-left:100px} #big{width:200px;height:200px}</style>\
             <div id=f></div><div id=blk></div><div id=clip><div id=big></div></div>",
        );
        assert_eq!(p.at(10.0, 10.0), "f");
        assert_eq!(p.at(10.0, 70.0), "blk");
        // The clip box is at y 100..160, x 100..160. Its child is 200 by 200: the part outside is not hit.
        assert_eq!(p.at(120.0, 120.0), "big");
        assert_eq!(p.at(180.0, 120.0), "body", "outside the clip, the big child does not count: the body is behind it");
        assert_eq!(p.at(120.0, 180.0), "html", "below the body: the root");
    }

    #[test]
    fn hidden_boxes_are_not_hit_but_what_is_inside_can_be() {
        let p = page("<style>body{margin:0} #h{visibility:hidden;width:100px;height:40px} #v{visibility:visible;width:30px;height:30px}</style><div id=h><div id=v></div></div>");
        assert_eq!(p.at(10.0, 10.0), "v");
        assert_eq!(p.at(60.0, 10.0), "body", "the hidden box itself is not hit: the body is behind it");
    }

    #[test]
    fn inline_blocks_are_contexts() {
        let p = page("<style>body{margin:0} #ib{display:inline-block;width:50px;height:30px;background:red} #in{width:20px;height:20px}</style><div>x<span id=ib><div id=in></div></span></div>");
        assert_eq!(p.at(9.6 + 5.0, 5.0), "in");
        assert_eq!(p.at(9.6 + 30.0, 5.0), "ib");
    }

    #[test]
    fn list_markers_give_the_item() {
        let p = page("<style>body{margin:0} ul{margin:0;padding-left:40px}</style><ul><li id=li>one</li></ul>");
        assert_eq!(p.at(50.0, 8.0), "li");
    }

    /// The hit test and the painter must agree on what is on top. Every box has its own color. At each sampled pixel
    /// that holds the color of a box, the hit test must name that box.
    #[test]
    fn hit_test_names_the_box_that_the_painter_draws_on_top() {
        let names = ["b1", "f1", "b2", "ab1", "ab2", "clip", "inner", "ib", "sp", "rel", "late"];
        let html = "<style>body{margin:0}</style>\
            <div id=b1 style='height:60px;background:#010000'></div>\
            <div id=f1 style='float:left;width:80px;height:80px;background:#020000'></div>\
            <div id=b2 style='height:70px;background:#030000;margin-top:-30px'></div>\
            <div id=ab1 style='position:absolute;left:30px;top:20px;width:90px;height:90px;background:#040000'></div>\
            <div id=ab2 style='position:absolute;left:70px;top:60px;width:90px;height:90px;background:#050000'></div>\
            <div id=clip style='overflow:hidden;width:50px;height:50px;background:#060000;margin-left:150px'><div id=inner style='width:100px;height:20px;background:#070000'></div></div>\
            <div style='margin-top:80px'><span id=ib style='display:inline-block;width:40px;height:40px;background:#080000'></span><span id=sp style='background:#090000;color:transparent'>text</span></div>\
            <div id=rel style='position:relative;left:150px;top:-30px;width:100px;height:30px;background:#0a0000'></div>\
            <div id=late style='height:20px;background:#0b0000'></div>";
        let p = page(html);
        let canvas = crate::capture(&p.doc, &p.styles, &p.layout, W, H);
        let mut checked = std::collections::HashSet::new();
        for y in (0..H).step_by(2) {
            for x in (0..W).step_by(2) {
                let c = canvas.pixel(x, y) & 0x00ff_ffff;
                let k = (c >> 16) as usize;
                if c & 0xffff != 0 || k == 0 || k > names.len() {
                    continue;
                }
                let want = names[k - 1];
                // The center of the pixel is where the point is.
                let got = p.at(x as f32 + 0.5, y as f32 + 0.5);
                assert_eq!(got, want, "pixel ({x}, {y}) is painted as {want}");
                checked.insert(want);
            }
        }
        // Every box shows somewhere, so every layer was tested at least once.
        for n in names {
            assert!(checked.contains(n), "{n} has a pixel on top");
        }
    }

    /// The same check for the rules of stacking: `z-index` (negative, positive, in a context that holds its children), `opacity`, flex items with a
    /// `z-index`, a float over a positioned box without `z-index`, and a fixed box. Every pixel that holds the pure color of a box is named by the hit test.
    #[test]
    fn hit_test_follows_z_index_opacity_and_flex_like_the_painter() {
        let names = ["base", "neg", "hi", "lo", "ctx", "deep", "fl", "ab", "fa", "fb", "fc", "fix", "ctxneg", "under"];
        let html = "<style>body{margin:0} div{box-sizing:border-box}</style>\
            <div id=base style='height:120px;width:50px;background:#010000'></div>\
            <div id=neg style='position:absolute;left:10px;top:10px;width:100px;height:100px;z-index:-1;background:#020000'></div>\
            <div id=hi style='position:absolute;left:60px;top:20px;width:90px;height:60px;z-index:5;background:#030000'></div>\
            <div id=lo style='position:absolute;left:100px;top:40px;width:90px;height:60px;z-index:2;background:#040000'></div>\
            <div id=ctx style='position:relative;z-index:1;left:20px;top:-60px;width:120px;height:80px;background:#050000'><div id=deep style='position:absolute;left:30px;top:30px;width:200px;height:20px;z-index:99;background:#060000'></div><div id=ctxneg style='position:absolute;left:0;top:0;width:20px;height:20px;z-index:-1;background:#0d0000'></div></div>\
            <div id=fl style='float:left;width:50px;height:50px;background:#070000'></div>\
            <div id=ab style='position:absolute;left:0;top:130px;width:60px;height:60px;background:#080000'></div>\
            <div style='display:flex;position:absolute;left:100px;top:130px;width:200px;height:60px'><div id=fa style='width:80px;height:60px;margin-right:-30px;z-index:3;background:#090000'></div><div id=fb style='width:80px;height:60px;background:#0a0000'></div><div id=fc style='width:80px;height:60px;position:relative;z-index:1;background:#0b0000'></div></div>\
            <div id=under style='position:absolute;left:210px;top:260px;width:60px;height:60px;z-index:2;background:#0e0000'></div>\
            <div id=op style='position:absolute;left:200px;top:250px;width:90px;height:60px;opacity:0.9;background:#100000;z-index:4'></div>\
            <div id=fix style='position:fixed;left:250px;top:200px;width:40px;height:40px;background:#0c0000'></div>";
        let p = page(html);
        let canvas = crate::capture(&p.doc, &p.styles, &p.layout, W, H);
        let mut checked = std::collections::HashSet::new();
        for y in (0..H).step_by(2) {
            for x in (0..W).step_by(2) {
                let c = canvas.pixel(x, y) & 0x00ff_ffff;
                let k = (c >> 16) as usize;
                if c & 0xffff != 0 || k == 0 || k > names.len() {
                    continue;
                }
                let want = names[k - 1];
                let node = hit_test(&p.doc, &p.styles, &p.layout, x as f32 + 0.5, y as f32 + 0.5).expect("a layout");
                let got = p.name(node);
                // A box with no background (`body`, `html`) is hit over a box with a negative `z-index` that shows through it.
                if got != want && p.styles.style(node).expect("style").color(Property::BackgroundColor).a == 0 && want == "neg" {
                    checked.insert(want);
                    continue;
                }
                assert_eq!(got, want, "pixel ({x}, {y}) is painted as {want}");
                checked.insert(want);
            }
        }
        for n in names {
            assert!(checked.contains(n), "{n} has a pixel on top");
        }
        assert_eq!(p.at(230.0, 280.0), "op", "a box with opacity is a context with z-index 4 over `under`, and its pixels are mixed: they are not checked above");
    }

    #[test]
    fn z_index_orders_the_list_and_a_context_keeps_its_children_inside() {
        let p = page(
            "<style>body{margin:0} div{width:100px;height:100px} .a{position:absolute;left:0;top:0} #n{z-index:-1} #p{z-index:3} #q{z-index:1} #c{position:relative;z-index:2} #k{position:absolute;left:0;top:0;z-index:99}</style>\
             <div class=a id=p></div><div class=a id=q></div><div class=a id=n></div><div id=c><div id=k></div></div>",
        );
        let names = |x, y| {
            let areas = bare_layout::scroll::areas(&p.doc, &p.styles, &p.layout);
            let all = hits_scrolled(&p.doc, &p.styles, &p.layout, &areas, &Scroll::default(), x, y);
            all.iter().map(|&n| p.name(n)).collect::<Vec<_>>().join(">")
        };
        // #k has z-index 99 but its context #c has 2: it stays under #p (3), over #q (1).
        assert_eq!(names(10.0, 10.0), "p>k>c>q>body>n>html");
        assert_eq!(names(150.0, 150.0), "html");
    }

    #[test]
    fn pointer_events_none_skips_a_box_and_auto_brings_a_child_back() {
        let p = page(
            "<style>body{margin:0} div{width:100px;height:100px} #a{position:absolute;left:0;top:0} #b{position:absolute;left:0;top:0;pointer-events:none} #c{pointer-events:auto;position:absolute;left:0;top:0;width:30px;height:30px} #d{position:absolute;left:150px;top:0;pointer-events:none}</style>\
             <div id=a></div><div id=b><div id=c></div></div><div id=d><div id=e style='width:20px;height:20px'></div></div>",
        );
        assert_eq!(p.at(10.0, 10.0), "c", "the child of a box with none is hit when it says auto");
        assert_eq!(p.at(50.0, 50.0), "a", "b is skipped");
        assert_eq!(p.at(160.0, 10.0), "html", "d and its child e inherit none: the root is left");
        let off = page("<style>html{pointer-events:none} body{margin:0} #a{width:50px;height:50px}</style><div id=a></div>");
        assert_eq!(off.at(10.0, 10.0), "html", "the root is the last answer, also when it says none");
        assert_eq!(off.at(300.0, 300.0), "html");
    }

    #[test]
    fn a_fixed_box_is_not_cut_by_a_clip_around_it_and_the_rest_of_that_clip_is() {
        let p = page("<style>body{margin:0;overflow:hidden;height:50px} #c{position:relative;overflow:hidden;width:100px;height:60px} #f{position:fixed;left:200px;top:100px;width:100px;height:100px} #g{position:absolute;left:0;top:300px;width:50px;height:50px}</style><div id=c><div id=f></div><div id=g></div></div>");
        let s = Scroll::default();
        assert_eq!(p.at_scrolled(&s, 250.0, 150.0), "f", "outside the clip box c and outside body: the fixed box is still hit");
        assert_eq!(p.at_scrolled(&s, 25.0, 320.0), "html", "g is cut by c and body");
        assert_eq!(p.at_scrolled(&Scroll { y: 0.0, ..Scroll::default() }, 10.0, 10.0), "c");
    }

    #[test]
    fn a_click_below_the_fold_hits_what_is_drawn_there() {
        let p = page("<style>body{margin:0} div{height:100px} </style><div id=a></div><div id=b></div><div id=c></div><div id=d></div><div id=e></div>");
        let s = Scroll { y: 250.0, ..Scroll::default() };
        // The window shows the page from y 250: the point (10, 10) is the page point (10, 260), in #c.
        assert_eq!(p.at_scrolled(&s, 10.0, 10.0), "c");
        assert_eq!(p.at_scrolled(&s, 10.0, 160.0), "e", "410 is in the last box (400..500)");
        assert_eq!(p.at_scrolled(&Scroll::default(), 10.0, 160.0), "b");
    }

    #[test]
    fn a_click_in_a_scrolled_container_hits_the_scrolled_content_and_the_clip_still_cuts() {
        let p = page("<style>body{margin:0} #s{overflow:hidden;width:100px;height:50px;margin:20px} i{display:block;height:30px} </style><div id=s><i id=i1></i><i id=i2></i><i id=i3></i><i id=i4></i></div><p id=after>after");
        let mut s = Scroll::default();
        assert_eq!(p.at_scrolled(&s, 30.0, 25.0), "i1");
        s.nodes.insert(p.id("s"), (0.0, 60.0));
        assert_eq!(p.at_scrolled(&s, 30.0, 25.0), "i3", "scrolled by 60: the third item (60..90) is at the top of the box (y 20)");
        assert_eq!(p.at_scrolled(&s, 30.0, 60.0), "i4");
        assert_eq!(p.at_scrolled(&s, 30.0, 80.0), "body", "below the box (20..70) the clip hides the 4th item: the page is there");
        assert_eq!(p.at_scrolled(&s, 10.0, 95.0), "after");
    }

    #[test]
    fn a_fixed_box_is_hit_where_it_is_drawn_and_a_scrollbar_is_the_container() {
        let p = page("<style>body{margin:0} #t{height:900px} #f{position:fixed;left:10px;top:10px;width:40px;height:40px} #s{overflow:auto;width:100px;height:50px;margin-top:100px} #in{height:200px;width:100px}</style><div id=t></div><div id=f></div><div id=s><div id=in></div></div>");
        let s = Scroll { y: 500.0, ..Scroll::default() };
        assert_eq!(p.at_scrolled(&s, 20.0, 20.0), "f", "the fixed box does not move with the page");
        assert_eq!(p.at_scrolled(&s, 70.0, 20.0), "t");
        // The box #s is at page y 1000..1050 with a bar at its right: x 85..100.
        let s = Scroll { y: 960.0, ..Scroll::default() };
        assert_eq!(p.at_scrolled(&s, 50.0, 60.0), "in");
        assert_eq!(p.at_scrolled(&s, 90.0, 60.0), "s", "on the scrollbar: the container");
    }

    #[test]
    fn a_sticky_box_is_hit_where_it_is_drawn() {
        let p = page("<style>body{margin:0} #t{height:100px} #s{position:sticky;top:10px;height:30px;width:100px} #u{height:900px}</style><div id=t></div><div id=s><i id=k></i></div><div id=u></div>");
        // At the top of the page the box is at 100..130: its normal place.
        assert_eq!(p.at_scrolled(&Scroll::default(), 10.0, 110.0), "s");
        // Scrolled by 200 it sticks at 10..40 in the window: the point (10, 20) is in it, and (10, 110) is the page again.
        let s = Scroll { y: 200.0, ..Scroll::default() };
        assert_eq!(p.at_scrolled(&s, 10.0, 20.0), "s");
        assert_eq!(p.at_scrolled(&s, 10.0, 110.0), "u");
    }
}
