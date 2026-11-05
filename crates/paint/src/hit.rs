//! Hit testing: the topmost element at a point of the page.
//!
//! The answer must be the element that a person sees at that point, so this walk follows the order of painting in
//! `display.rs` (CSS 2.1 Appendix E). Inside one context the layers are, from the bottom: the box itself, the blocks in
//! tree order, the floats, the lines (inline boxes, text, markers and inline-blocks) and the positioned boxes. The walk
//! finds the candidate in the highest layer, and in a layer the one that comes last in tree order, which is what the
//! painter draws on top. A float, an inline-block, a positioned box and a box that clips are contexts: the walk goes into
//! them and a hit inside belongs to the layer of the context.
//!
//! The dependency: `layer_of` here and `Builder::layer` in `display.rs` must agree. When paint learns a new rule (S4 topic 3:
//! `z-index` and stacking contexts), this file needs the same rule. A test paints every box of a page in its own color
//! and checks that the hit test names the box whose color is on top at each sampled pixel, so a rule that only one of
//! them knows makes that test fail.
//!
//! What the answer is: an element. A text fragment gives the parent of its text node, and a list marker gives its item.
//! A point on the page that no box covers gives the root element (Chrome does the same: `html`). A box with
//! `visibility` other than `visible` is not hit, and the boxes inside it can be. A box that clips (`overflow` other than
//! `visible`) hides the parts of its children outside its padding box.
//!
//! The cost is one pass over the fragments (no allocation): the fragments are in tree order, a context that clips skips its
//! inside when the point is outside, and nothing else skips, because a box can paint outside its parent.
//!
//! Gaps: no `pointer-events`, no transforms, no scroll offsets (S4 topic 5 adds them: the caller subtracts them first),
//! no `clip-path` or border radius (a point in the rounded-off corner still hits).

use bare_css::cascade::{ComputedStyle, Styles};
use bare_css::values::{Keyword, Property, Value};
use bare_html::dom::{Document, NodeData, NodeId};
use bare_layout::boxes::{Kind, Tree};
use bare_layout::fragment::{Fragment, Kind as FragmentKind, Layout, Rect};

/// The layers of one context, from the bottom (the order of `display.rs`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Phase {
    Blocks,
    Floats,
    Lines,
    Positioned,
}

/// How a box is painted. The twin of `Layer` in `display.rs`.
enum Layer {
    Block,
    Inline,
    Atomic,
    Float,
    Positioned,
    Clipped,
}

/// Deeper than this the contexts stop, as in `display.rs`: the same boxes are not drawn, so they are not hit.
const MAX_DEPTH: usize = 400;

/// The topmost element at `(x, y)` in page coordinates. `None` when the page has no layout.
pub fn hit_test(doc: &Document, styles: &Styles, layout: &Layout, x: f32, y: f32) -> Option<NodeId> {
    let tree = Tree::new(doc, styles);
    let root = tree.root()?;
    let frags = &layout.fragments;
    if frags.is_empty() {
        return None;
    }
    let h = Hit { tree, doc, frags, x, y };
    Some(h.context(0, 0).unwrap_or(root))
}

struct Hit<'a> {
    tree: Tree<'a>,
    doc: &'a Document,
    frags: &'a [Fragment],
    x: f32,
    y: f32,
}

/// The best candidate so far: the layer, then the place in tree order.
type Best = Option<((Phase, usize), NodeId)>;

fn offer(best: &mut Best, key: (Phase, usize), node: Option<NodeId>) {
    if let Some(n) = node
        && best.is_none_or(|(k, _)| key >= k)
    {
        *best = Some((key, n));
    }
}

impl Hit<'_> {
    fn inside(&self, r: Rect) -> bool {
        self.x >= r.x && self.x < r.x + r.w && self.y >= r.y && self.y < r.y + r.h
    }

    fn visible(&self, style: &ComputedStyle) -> bool {
        style.get(Property::Visibility) == Value::Keyword(Keyword::Visible)
    }

    fn layer(&self, f: &Fragment, style: &ComputedStyle) -> Layer {
        let kind = self.tree.kind(f.node);
        match kind {
            Kind::OutOfFlow => return Layer::Positioned,
            Kind::Float => return Layer::Float,
            _ => {}
        }
        if kind != Kind::Inline && style.get(Property::Position) != Value::Keyword(Keyword::Static) {
            return Layer::Positioned;
        }
        match kind {
            Kind::Inline => Layer::Inline,
            Kind::Atomic => Layer::Atomic,
            _ if clips(style) => Layer::Clipped,
            _ => Layer::Block,
        }
    }

    /// The topmost element inside the context that starts at the fragment `i`, or `None`.
    fn context(&self, i: usize, depth: usize) -> Option<NodeId> {
        if depth >= MAX_DEPTH {
            return None;
        }
        let f = self.frags[i];
        let style = self.tree.style(f.node);
        let mut best: Best = None;
        if self.visible(style) && self.inside(f.rect) {
            offer(&mut best, (Phase::Blocks, i), Some(f.node));
        }
        // The inside of a box that clips is reached only through its padding box.
        if i > 0 && clips(style) && !self.inside(padding_box(&f, style)) {
            return best.map(|(_, n)| n);
        }
        let end = f.end as usize;
        let mut j = i + 1;
        while j < end {
            let g = self.frags[j];
            match g.kind {
                FragmentKind::Box => {
                    let style = self.tree.style(g.node);
                    match self.layer(&g, style) {
                        Layer::Block => {
                            if self.visible(style) && self.inside(g.rect) {
                                offer(&mut best, (Phase::Blocks, j), Some(g.node));
                            }
                            j += 1;
                        }
                        Layer::Inline => {
                            if self.visible(style) && self.inside(g.rect) {
                                offer(&mut best, (Phase::Lines, j), Some(g.node));
                            }
                            j += 1;
                        }
                        Layer::Atomic => {
                            offer(&mut best, (Phase::Lines, j), self.context(j, depth + 1));
                            j = g.end as usize;
                        }
                        Layer::Float => {
                            offer(&mut best, (Phase::Floats, j), self.context(j, depth + 1));
                            j = g.end as usize;
                        }
                        Layer::Positioned => {
                            offer(&mut best, (Phase::Positioned, j), self.context(j, depth + 1));
                            j = g.end as usize;
                        }
                        Layer::Clipped => {
                            offer(&mut best, (Phase::Blocks, j), self.context(j, depth + 1));
                            j = g.end as usize;
                        }
                    }
                }
                FragmentKind::Text { .. } => {
                    if self.inside(g.rect) && self.visible(self.tree.style(g.node)) {
                        // A text node is never the target of a pointer event: the element around it is.
                        offer(&mut best, (Phase::Lines, j), self.doc.parent(g.node));
                    }
                    j += 1;
                }
                FragmentKind::Marker { .. } => {
                    if self.inside(g.rect) && self.visible(self.tree.style(g.node)) {
                        offer(&mut best, (Phase::Lines, j), Some(g.node));
                    }
                    j += 1;
                }
                FragmentKind::Static => j += 1,
            }
        }
        best.map(|(_, n)| n)
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
}
