//! Scroll containers: where they are, how big their content is, and how far they can scroll.
//!
//! Layout makes fragments at the places they have with no scroll offset. This pass reads the fragments once and finds,
//! for each box that clips (`overflow` other than `visible` on an axis) and for the page itself, the size of its
//! scrollable overflow: the union of the border boxes of everything inside it, and its own padding box. Scrolling later
//! changes only an offset. It never lays out again. Paint (`bare-paint`) and hit testing add the offset, and the
//! window and the script API clamp it with the numbers found here.
//!
//! Chrome gives the same answer from its `LayoutBox::ScrollableOverflowRect`. Differences are listed in
//! `docs/resources.md`, "Scrolling".
//!
//! Rules:
//! - A `position: fixed` box and everything in it is left out: it does not scroll with the page and it does not make the
//!   page longer.
//! - What a scroll container holds does not count for the boxes around it (it clips). Its own border box does.
//! - The scrollbars come from layout (`Layout::bars`, which it found when it laid the box out): `overflow: scroll`
//!   always has them, `auto` only when the content did not fit.

use crate::block::SCROLLBAR;
use crate::boxes::Tree;
use crate::fragment::{Kind, Layout, Rect};
use bare_css::cascade::Styles;
use bare_css::values::{Keyword, Property};
use bare_html::dom::{Document, NodeData, NodeId};
use std::collections::HashMap;

/// One scroll container.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub node: NodeId,
    /// The place of its box in `Layout::fragments`.
    pub frag: u32,
    /// The padding box, in page coordinates.
    pub pad: Rect,
    /// A scrollbar on the right (vertical) and at the bottom (horizontal).
    pub bars: (bool, bool),
    /// The size of the scrollable overflow: `scrollWidth` and `scrollHeight`.
    pub content: (f32, f32),
}

impl Area {
    /// The size that the content sees: the padding box without the scrollbars (`clientWidth` and `clientHeight`).
    pub fn client(&self) -> (f32, f32) {
        ((self.pad.w - if self.bars.0 { SCROLLBAR } else { 0.0 }).max(0.0), (self.pad.h - if self.bars.1 { SCROLLBAR } else { 0.0 }).max(0.0))
    }

    /// The most that it can scroll along each axis.
    pub fn max(&self) -> (f32, f32) {
        let c = self.client();
        ((self.content.0 - c.0).max(0.0), (self.content.1 - c.1).max(0.0))
    }
}

/// All the scroll containers of a layout, and the size of the page content.
#[derive(Debug)]
pub struct Areas {
    pub list: Vec<Area>,
    index: HashMap<u32, u32>,
    /// The scrollable overflow of the page: the width of the layout and the bottom of the content, or more if something reaches further.
    /// It can be less than the window.
    pub page: (f32, f32),
    /// What the root element or the first `body` gives to the window: `overflow-x` and `overflow-y`.
    pub page_overflow: (Keyword, Keyword),
}

impl Areas {
    pub fn get(&self, node: NodeId) -> Option<&Area> {
        self.index.get(&(node.index() as u32)).map(|&i| &self.list[i as usize])
    }
}

/// The `overflow-x` and `overflow-y` that the window gets (CSS Overflow 3, 3.3): the root element's, or the first
/// `body`'s if the root has `visible`.
pub fn page_overflow(doc: &Document, styles: &Styles) -> (Keyword, Keyword) {
    let Some(root) = doc.children(doc.root()).find(|&n| matches!(doc.data(n), NodeData::Element { .. })) else { return (Keyword::Visible, Keyword::Visible) };
    let get = |n| {
        let Some(s) = styles.style(n) else { return (Keyword::Visible, Keyword::Visible) };
        let k = |p| match s.get(p) {
            bare_css::values::Value::Keyword(k) => k,
            _ => Keyword::Visible,
        };
        (k(Property::OverflowX), k(Property::OverflowY))
    };
    let r = get(root);
    if r != (Keyword::Visible, Keyword::Visible) {
        return r;
    }
    let body = doc.children(root).find(|&c| matches!(doc.data(c), NodeData::Element { .. }) && doc.element_name(c) == "body");
    body.map_or(r, get)
}

/// Find the scroll containers of `layout`, and the size of its page. One pass over the fragments.
pub fn areas(doc: &Document, styles: &Styles, layout: &Layout) -> Areas {
    let tree = Tree::new(doc, styles);
    let frags = &layout.fragments;
    let mut out = Areas { list: Vec::new(), index: HashMap::new(), page: (layout.width, layout.height), page_overflow: page_overflow(doc, styles) };
    // The open containers: where they end, their place in `out.list`, the corner that their content reaches, and the padding
    // at the right and at the bottom. The end padding counts in the overflow, as in Chrome.
    type Open = (u32, usize, f32, f32, f32, f32);
    let mut open: Vec<Open> = Vec::new();
    let finish = |out: &mut Areas, (_, k, r, b, pr, pb): Open| {
        let a = &mut out.list[k];
        let c = a.client();
        a.content = ((r + pr - a.pad.x).max(c.0), (b + pb - a.pad.y).max(c.1));
    };
    let mut i = 0;
    while i < frags.len() {
        while open.last().is_some_and(|o| o.0 as usize <= i) {
            let o = open.pop().expect("checked");
            finish(&mut out, o);
        }
        let f = frags[i];
        let (right, bottom) = (f.rect.x + f.rect.w, f.rect.y + f.rect.h);
        if f.kind == Kind::Box && i > 0 && tree.position(f.node) == Keyword::Fixed && tree.kind(f.node) != crate::boxes::Kind::Inline {
            i = f.end as usize;
            continue;
        }
        match open.last_mut() {
            Some(top) => {
                top.2 = top.2.max(right);
                top.3 = top.3.max(bottom);
            }
            None => {
                out.page.0 = out.page.0.max(right);
                out.page.1 = out.page.1.max(bottom);
            }
        }
        if f.kind == Kind::Box && i > 0 {
            let (ox, oy) = tree.overflow(f.node);
            let clips = |k| !matches!(k, Keyword::Visible);
            if clips(ox) || clips(oy) {
                let style = tree.style(f.node);
                let w = |p| style.px(p).max(0.0);
                let (l, t, r, b) = (w(Property::BorderLeftWidth), w(Property::BorderTopWidth), w(Property::BorderRightWidth), w(Property::BorderBottomWidth));
                let pad = Rect { x: f.rect.x + l, y: f.rect.y + t, w: (f.rect.w - l - r).max(0.0), h: (f.rect.h - t - b).max(0.0) };
                let auto = |k, at| match k {
                    Keyword::Scroll => true,
                    Keyword::Auto => at,
                    _ => false,
                };
                let found = layout.bars.get(&(f.node.index() as u32)).copied().unwrap_or((false, false));
                let bars = (auto(oy, found.0), auto(ox, found.1));
                let k = out.list.len();
                out.index.insert(f.node.index() as u32, k as u32);
                out.list.push(Area { node: f.node, frag: i as u32, pad, bars, content: (pad.w, pad.h) });
                open.push((f.end, k, pad.x, pad.y, w(Property::PaddingRight), w(Property::PaddingBottom)));
            }
        }
        i += 1;
    }
    while let Some(o) = open.pop() {
        finish(&mut out, o);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bare_css::cascade::{Origin, RuleSet, Source, UA_CSS};

    fn run(html: &str, vp: (f32, f32)) -> (Document, Areas) {
        let mut doc = bare_html::tree::parse(html);
        let mut rules = RuleSet::new(vp);
        rules.add(&mut doc, UA_CSS, Origin::UserAgent);
        for source in bare_css::cascade::sources(&doc, vp) {
            if let Source::Inline(text) = source {
                rules.add(&mut doc, &text, Origin::Author);
            }
        }
        let styles = rules.compute(&doc);
        let layout = crate::layout(&doc, &styles, vp);
        let a = areas(&doc, &styles, &layout);
        (doc, a)
    }

    fn by_id<'a>(doc: &Document, a: &'a Areas, id: &str) -> &'a Area {
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if doc.attr(n, bare_html::dom::Atom::ID) == Some(id) {
                return a.get(n).expect("a scroll container");
            }
            stack.extend(doc.children(n));
        }
        panic!("no element with id {id}")
    }

    #[test]
    fn a_box_that_scrolls_knows_its_content_and_its_bar() {
        let (doc, a) = run("<style>body{margin:0} #s{overflow:auto;width:100px;height:50px} #c{height:200px;width:40px}</style><div id=s><div id=c></div></div>", (300.0, 300.0));
        let s = by_id(&doc, &a, "s");
        assert_eq!(s.bars, (true, false), "the content is taller: a bar on the right");
        assert_eq!(s.content, (85.0, 200.0), "the client width is 100 - 15");
        assert_eq!(s.max(), (0.0, 150.0));
        assert_eq!(s.client(), (85.0, 50.0));
    }

    #[test]
    fn hidden_scrolls_with_no_bar_and_scroll_always_has_one() {
        let (doc, a) = run("<style>body{margin:0} div{width:100px;height:50px} #h{overflow:hidden} #x{overflow:scroll} p{margin:0;height:80px;width:300px}</style><div id=h><p></p></div><div id=x></div>", (400.0, 400.0));
        let h = by_id(&doc, &a, "h");
        assert_eq!((h.bars, h.content), ((false, false), (300.0, 80.0)));
        let x = by_id(&doc, &a, "x");
        assert_eq!(x.bars, (true, true));
        assert_eq!(x.max(), (0.0, 0.0));
    }

    #[test]
    fn the_page_is_as_tall_as_its_content_and_fixed_boxes_do_not_count() {
        let (_, a) = run("<style>body{margin:0} #t{height:900px} #f{position:fixed;top:2000px;height:50px;width:50px}</style><div id=t></div><div id=f></div>", (300.0, 300.0));
        assert_eq!(a.page, (300.0, 900.0));
        let (_, b) = run("<style>body{margin:0} pre{margin:0}</style><pre>0123456789012345678901234567890123456789012345678901234567890123456789</pre>", (300.0, 300.0));
        assert_eq!(b.page.0, 70.0 * 9.6_f32.max(9.0), "a long line is wider than the window: the page scrolls sideways");
    }

    #[test]
    fn what_a_container_holds_does_not_count_for_the_page() {
        let (_, a) = run("<style>body{margin:0} #s{overflow:hidden;height:40px} #c{height:5000px}</style><div id=s><div id=c></div></div>", (300.0, 300.0));
        assert_eq!(a.page.1, 40.0, "the box is 40 px tall: its 5000 px inside are clipped");
    }

    #[test]
    fn the_window_gets_the_overflow_of_the_root_or_the_body() {
        let (_, a) = run("<style>body{overflow:hidden}</style><p>x", (300.0, 300.0));
        assert_eq!(a.page_overflow, (Keyword::Hidden, Keyword::Hidden));
        let (_, a) = run("<style>html{overflow-y:scroll}</style><p>x", (300.0, 300.0));
        assert_eq!(a.page_overflow.1, Keyword::Scroll);
    }
}
