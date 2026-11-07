//! `Element.getClientRects`, `Range.getClientRects` and `Range.getBoundingClientRect` (CSSOM View). The rectangles come from the
//! fragments of native layout (`bare_layout::fragment`), moved by the page and container offsets (`Geo::shift`, as `getBoundingClientRect`).
//!
//! What is built
//! - Element: one rectangle for each box fragment of the element (one for each line of an inline box with a background or a border).
//!   An element with no box (`display: none`, `display: contents`, not in the document) has an empty list.
//! - An inline box with nothing to paint (a "culled" inline: most `span`, `b` and `a`) has no fragment. Chrome gives it the rectangles
//!   of its content: one for each run of text and atomic boxes on a line, and the rectangles of each child element in between. An
//!   atomic box counts with the font box of the parent at the baseline (not its own border box). The same rule is here. An empty
//!   inline and a `br` have no fragment either; for those the layout runs once more with every inline box kept (`layout_uncull`).
//! - Range (`Range::GetBorderAndTextQuads` of Chrome): in tree order from the first node of the range to the last, each element that
//!   the range contains whole (and whose parent is not one of those) gives its rectangles, and each Text node gives the rectangles of
//!   its part of the range, one for each line. A part with no shown letter (collapsed spaces, an empty range) gives a rectangle of 0
//!   width when no other rectangle of that Text exists. A newline of a `pre` text gives a rectangle of 0 width at the end of its line.
//! - `getBoundingClientRect` of a range, and of an element with no box fragment, is the union of the rectangles that are not empty.
//!   With none that is not empty, the first one. With no rectangle, all zero.
//!
//! The list is a `DOMRectList` and the rectangles are `DOMRect` (`browser_geom.rs`).
//!
//! Gates and gaps (recorded, none is faked)
//! - An SVG or MathML element, or a range that holds one, is `Unsupported` (SVG has no layout here). So is a `col` or `colgroup` (layout makes
//!   no box for a column; a range that holds one gets no rectangle for it).
//! - `display: contents` is not a value of the CSS engine: the element is laid out as a block, so it has a rectangle (Chrome: none).
//! - A `pre-wrap` line that ends in spaces gives one rectangle for the text and one for the spaces in Chrome; here one for the text.
//! - A tab in `white-space: pre`, a `text-transform` that changes the number of letters, and the margin of an atomic box in a culled
//!   inline are not measured as Chrome does: the tab counts as one letter and the margin as 0.

use crate::browser::*;
use crate::browser_platform::V;
use crate::browser_range::bounds;
use bare_css::values::{Keyword, Property, Value as Cv};
use bare_html::dom::{Document, NodeData, NodeId};
use bare_js::eval::{Error, Interp};
use bare_js::value::Value;
use bare_layout::boxes::{Kind, Tree};
use bare_layout::font::{self, Metrics};
use bare_layout::fragment::{Fragment, Kind as Frag, Layout, Rect};
use bare_layout::text::WhiteSpace;
use bare_layout::Fast;
use bare_paint::Scroll;
use std::cell::OnceCell;
use std::collections::HashSet;

use crate::browser_scroll::Geo;

const ZERO: Rect = Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 };

/// The fragments by node, built when a read needs them.
struct Idx<'a> {
    layout: &'a Layout,
    by: OnceCell<Fast<Vec<u32>>>,
}

impl<'a> Idx<'a> {
    fn new(layout: &'a Layout) -> Idx<'a> {
        Idx { layout, by: OnceCell::new() }
    }

    /// The fragments of `n`, in layout order, with their places in the list.
    fn of(&self, n: NodeId) -> impl Iterator<Item = (usize, &'a Fragment)> + '_ {
        let by = self.by.get_or_init(|| {
            let mut m = Fast::<Vec<u32>>::default();
            for (i, f) in self.layout.fragments.iter().enumerate() {
                m.entry(f.node.index() as u32).or_default().push(i as u32);
            }
            m
        });
        by.get(&(n.index() as u32)).into_iter().flatten().map(move |&i| (i as usize, &self.layout.fragments[i as usize]))
    }

    fn boxes(&self, n: NodeId) -> impl Iterator<Item = &'a Fragment> + '_ {
        self.of(n).map(|(_, f)| f).filter(|f| f.kind == Frag::Box)
    }
}

/// Why a pass stops: it needs the layout that keeps every inline box, or it met an element that has no layout.
enum Need {
    Plain,
    Foreign,
}
type Rs = Result<Vec<Rect>, Need>;

struct Pass<'a> {
    g: &'a Geo<'a>,
    s: &'a Scroll,
    tree: Tree<'a>,
    main: Idx<'a>,
    plain: Option<Idx<'a>>,
}

fn same_line(a: &Rect, b: &Rect) -> bool {
    (a.y - b.y).abs() < 0.01 && (a.h - b.h).abs() < 0.01
}

/// Add a rectangle to the group of the line, or close the group and start a new one.
fn join(out: &mut Vec<Rect>, group: &mut Option<Rect>, r: Rect) {
    match group {
        Some(g) if same_line(g, &r) => {
            let x1 = (g.x + g.w).max(r.x + r.w);
            g.x = g.x.min(r.x);
            g.w = x1 - g.x;
        }
        _ => out.extend(group.replace(r)),
    }
}

impl<'a> Pass<'a> {
    fn at(&self, f: &Fragment, owner: NodeId) -> Rect {
        let (dx, dy) = self.g.shift(self.s, owner);
        Rect { x: f.rect.x + dx, y: f.rect.y + dy, ..f.rect }
    }

    fn doc(&self) -> &'a Document {
        self.g.doc
    }

    /// The rectangles of an element.
    fn element(&self, el: NodeId) -> Rs {
        if self.doc().is_foreign(el) {
            return Err(Need::Foreign);
        }
        let boxes: Vec<Rect> = self.main.boxes(el).map(|f| self.at(f, el)).collect();
        if !boxes.is_empty() {
            return Ok(boxes);
        }
        match self.tree.kind(el) {
            Kind::Break => self.leaf(el, true),
            Kind::Inline => match self.culled(el)? {
                v if v.is_empty() => self.leaf(el, false),
                v => Ok(v),
            },
            _ => Ok(Vec::new()),
        }
    }

    /// A `br` or an empty inline: from the layout that keeps every inline box.
    fn leaf(&self, el: NodeId, br: bool) -> Rs {
        let plain = self.plain.as_ref().ok_or(Need::Plain)?;
        if br {
            return Ok(plain.of(el).map(|(_, f)| self.at(f, el)).take(1).collect());
        }
        let boxes: Vec<Rect> = plain.boxes(el).map(|f| self.at(f, el)).collect();
        if !boxes.is_empty() {
            return Ok(boxes);
        }
        // No line was made for it (the block holds nothing else): a point at the start of the content of the block.
        let mut cur = self.doc().flat_parent(el);
        while let Some(a) = cur {
            if let Some(f) = self.main.boxes(a).next() {
                let st = self.g.styles.style(a);
                let px = |p| st.map_or(0.0, |s| s.px(p));
                let r = self.at(f, a);
                return Ok(vec![Rect { x: r.x + px(Property::BorderLeftWidth) + px(Property::PaddingLeft), y: r.y + px(Property::BorderTopWidth) + px(Property::PaddingTop), w: 0.0, h: 0.0 }]);
            }
            if self.g.styles.style(a).is_none_or(|s| s.get(Property::Display) == Cv::Keyword(Keyword::None)) {
                break;
            }
            cur = self.doc().flat_parent(a);
        }
        Ok(Vec::new())
    }

    /// An inline box that layout did not keep: the runs of text and atomic boxes of each line, and the rectangles of the child elements.
    fn culled(&self, el: NodeId) -> Rs {
        let (doc, tree) = (self.doc(), &self.tree);
        let m = Metrics::of(tree.style(el));
        let ascent = m.ascent;
        let (mut out, mut group) = (Vec::new(), None);
        for k in doc.flat_children(el) {
            match doc.data(k) {
                NodeData::Text(_) => {
                    let mut nl = self.newlines(k).into_iter().peekable();
                    for (_, f) in self.main.of(k).filter(|(_, f)| matches!(f.kind, Frag::Text { .. })) {
                        let Frag::Text { start, .. } = f.kind else { continue };
                        // A newline is a rectangle of its own, at the end of the line before it.
                        while let Some((_, r)) = nl.next_if(|&(i, _)| i < start as usize) {
                            out.extend(group.take());
                            out.push(r);
                        }
                        join(&mut out, &mut group, self.at(f, k));
                    }
                    for (_, r) in nl {
                        out.extend(group.take());
                        out.push(r);
                    }
                }
                NodeData::Element { .. } => match tree.kind(k) {
                    Kind::Atomic => {
                        if let Some((i, f)) = self.main.of(k).next() {
                            // The font box of this inline on the baseline of the atomic box: its last line, or its bottom edge.
                            let layout = self.main.layout;
                            let last = layout.fragments[i + 1..f.end as usize].iter().rev().find(|t| matches!(t.kind, Frag::Text { .. }));
                            let base = last.map_or(f.rect.y + f.rect.h, |t| t.rect.y + Metrics::of(tree.style(t.node)).ascent);
                            let r = self.at(f, k);
                            let (_, dy) = self.g.shift(self.s, k);
                            join(&mut out, &mut group, Rect { x: r.x, y: base + dy - ascent, w: r.w, h: m.ascent + m.descent });
                        }
                    }
                    Kind::Float | Kind::OutOfFlow => {}
                    _ => {
                        let child = Metrics::of(tree.style(k));
                        let rects = self.element(k)?;
                        if tree.kind(k) == Kind::Inline && self.main.boxes(k).next().is_none() && child != m {
                            // An inline with another font and nothing to paint is part of the line of its parent: the font box of the parent on its baseline.
                            for r in rects {
                                join(&mut out, &mut group, Rect { y: r.y + child.ascent - m.ascent, h: m.ascent + m.descent, ..r });
                            }
                        } else {
                            out.extend(group.take());
                            out.extend(rects);
                        }
                    }
                },
                _ => {}
            }
        }
        out.extend(group);
        Ok(out)
    }

    /// The newlines of a `pre` text: a rectangle of 0 width at the end of the line before each, with the byte of the newline.
    /// Layout makes no fragment for a newline.
    fn newlines(&self, t: NodeId) -> Vec<(usize, Rect)> {
        let doc = self.doc();
        let NodeData::Text(span) = *doc.data(t) else { return Vec::new() };
        let text = doc.text(span);
        let Some(style) = doc.flat_parent(t).and_then(|p| self.g.styles.style(p)) else { return Vec::new() };
        if !WhiteSpace::of(style).newlines || !text.contains('\n') {
            return Vec::new();
        }
        let m = Metrics::of(style);
        let frags: Vec<&Fragment> = self.main.of(t).map(|(_, f)| f).filter(|f| matches!(f.kind, Frag::Text { .. })).collect();
        let mut out = Vec::new();
        for (i, _) in text.match_indices('\n') {
            let before = frags.iter().rev().find_map(|f| match f.kind {
                Frag::Text { start, end } if end as usize <= i => Some((*f, start as usize, end as usize)),
                _ => None,
            });
            if let Some((f, s, e)) = before {
                let x = self.x_at(t, text, f, s, e, m);
                let r = self.at(f, t);
                out.push((i, Rect { x, w: 0.0, ..r }));
            }
        }
        out
    }

    /// The x of byte `o` of `text` on the line of `f`, whose text starts at byte `s`: the widths of the letters that are shown, a run
    /// of collapsing spaces being one letter.
    fn x_at(&self, t: NodeId, text: &str, f: &Fragment, s: usize, o: usize, m: Metrics) -> f32 {
        let style = self.doc().flat_parent(t).and_then(|p| self.g.styles.style(p));
        let collapse = style.is_some_and(|st| WhiteSpace::of(st).collapse);
        let spacing = style.map_or((0.0, 0.0), |s| font::spacing(true, s));
        let (mut units, mut prev, mut extra) = (0, false, 0.0);
        for c in text[s..o].chars() {
            let sp = is_space(c);
            if !(collapse && sp && prev) {
                let w = font::width_of(c);
                units += w;
                if w != 0 {
                    extra += spacing.0;
                }
                if c == ' ' || c == '\u{a0}' {
                    extra += spacing.1;
                }
            }
            prev = sp;
        }
        f.rect.x + self.g.shift(self.s, t).0 + m.px(units) + extra
    }

    /// The rectangles of a part of a Text node: bytes `a..b` of its text.
    fn text(&self, t: NodeId, a: usize, b: usize) -> Vec<Rect> {
        let doc = self.doc();
        let NodeData::Text(span) = *doc.data(t) else { return Vec::new() };
        let text = doc.text(span);
        let Some(parent) = doc.flat_parent(t) else { return Vec::new() };
        let Some(style) = self.g.styles.style(parent) else { return Vec::new() };
        let (ws, m) = (WhiteSpace::of(style), Metrics::of(style));
        let frags: Vec<&Fragment> = self.main.of(t).map(|(_, f)| f).filter(|f| matches!(f.kind, Frag::Text { .. })).collect();
        let shown = |from: usize, to: usize| text[from..to].char_indices().any(|(i, c)| !(ws.collapse && is_space(c) && text[..from + i].chars().next_back().is_some_and(is_space)));
        let x_at = |f: &Fragment, s: usize, o: usize| self.x_at(t, text, f, s, o, m);
        let piece = |f: &Fragment, x0: f32, x1: f32| {
            let r = self.at(f, t);
            Rect { x: x0, w: x1 - x0, ..r }
        };
        let (mut strict, mut touch) = (Vec::new(), Vec::new());
        for f in &frags {
            let Frag::Text { start, end } = f.kind else { continue };
            let (s, e) = (start as usize, end as usize);
            let (c0, c1) = (a.max(s), b.min(e));
            if c0 < c1 && shown(c0, c1) {
                strict.push((c0, piece(f, x_at(f, s, c0), x_at(f, s, c1))));
            } else if a <= e && b >= s {
                let c = a.clamp(s, e);
                let x = x_at(f, s, c);
                touch.push((c, piece(f, x, x)));
            }
        }
        strict.extend(self.newlines(t).into_iter().filter(|&(i, _)| i >= a && i < b));
        strict.sort_by_key(|p| p.0);
        let v = if strict.is_empty() { touch } else { strict };
        v.into_iter().map(|p| p.1).collect()
    }
}

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n')
}

/// Byte offset of UTF-16 offset `o` in `s`, rounded down to a letter, or the end.
fn byte_of(s: &str, o: u32) -> usize {
    let mut u = 0;
    for (i, c) in s.char_indices() {
        if u + c.len_utf16() as u32 > o {
            return i;
        }
        u += c.len_utf16() as u32;
    }
    s.len()
}

fn skip(doc: &Document, mut n: NodeId) -> Option<NodeId> {
    loop {
        if let Some(s) = doc.next_sibling(n) {
            return Some(s);
        }
        n = doc.parent(n)?;
    }
}

fn next(doc: &Document, n: NodeId) -> Option<NodeId> {
    doc.children(n).next().or_else(|| skip(doc, n))
}

fn data_node(doc: &Document, n: NodeId) -> bool {
    matches!(doc.data(n), NodeData::Text(_) | NodeData::Comment(_) | NodeData::Doctype(_))
}

/// The rectangles of a range (see the header).
fn range(p: &Pass, (sc, so, ec, eo): (NodeId, u32, NodeId, u32)) -> Rs {
    let doc = p.doc();
    let child = |n: NodeId, i: u32| doc.children(n).nth(i as usize);
    let first = if data_node(doc, sc) {
        Some(sc)
    } else if let Some(c) = child(sc, so) {
        Some(c)
    } else if so == 0 {
        Some(sc)
    } else {
        skip(doc, sc)
    };
    let stop = if data_node(doc, ec) { skip(doc, ec) } else { child(ec, eo).or_else(|| skip(doc, ec)) };
    // The elements that hold the end are partly in the range. So is the start container, when the range starts in an element with no child there.
    let mut partly = HashSet::from([sc]);
    let mut up = Some(ec);
    while let Some(n) = up {
        partly.insert(n);
        up = doc.parent(n);
    }
    let (mut out, mut taken) = (Vec::new(), HashSet::new());
    let mut cur = first;
    while let Some(n) = cur.filter(|&n| Some(n) != stop) {
        match doc.data(n) {
            NodeData::Element { .. } => {
                if !partly.contains(&n) && !doc.parent(n).is_some_and(|q| taken.contains(&q)) {
                    taken.insert(n);
                    out.extend(p.element(n)?);
                }
            }
            NodeData::Text(span) => {
                let text = doc.text(*span);
                let a = if n == sc { byte_of(text, so) } else { 0 };
                let b = if n == ec { byte_of(text, eo) } else { text.len() };
                out.extend(p.text(n, a, b));
            }
            _ => {}
        }
        cur = next(doc, n);
    }
    Ok(out)
}

/// The union of the rectangles that are not empty; with none, the first; with no rectangle, all zero.
fn union(rects: &[Rect]) -> Rect {
    let mut r: Option<Rect> = None;
    for &q in rects {
        r = match r {
            None => Some(q),
            Some(c) if q.w > 0.0 && q.h > 0.0 => Some(if c.w > 0.0 && c.h > 0.0 {
                let (x, y) = (c.x.min(q.x), c.y.min(q.y));
                Rect { x, y, w: (c.x + c.w).max(q.x + q.w) - x, h: (c.y + c.h).max(q.y + q.h) - y }
            } else {
                q
            }),
            c => c,
        };
    }
    r.unwrap_or(ZERO)
}

/// Run `f` on the layout as it is, and again on the layout that keeps every inline box when `f` asks for it.
fn with_pass(d: &Dom, it: &mut Interp<'_>, g: &Geo, s: &Scroll, f: impl Fn(&Pass) -> Rs) -> R<Vec<Rect>> {
    fn pass<'a>(g: &'a Geo<'a>, s: &'a Scroll, plain: Option<&'a Layout>) -> Pass<'a> {
        Pass { g, s, tree: Tree::new(g.doc, g.styles), main: Idx::new(g.layout), plain: plain.map(Idx::new) }
    }
    match f(&pass(g, s, None)) {
        Ok(v) => return Ok(v),
        Err(Need::Foreign) => return Err(gate(FOREIGN)),
        Err(Need::Plain) => {}
    }
    d.geometry.uncull.set(true);
    let plain = d.layout_at(it, g.styles, g.view());
    d.geometry.uncull.set(false);
    f(&pass(g, s, Some(&plain?))).or_else(|n| if matches!(n, Need::Foreign) { Err(gate(FOREIGN)) } else { Ok(Vec::new()) })
}

const FOREIGN: &str = "getClientRects of an SVG or MathML element (SVG has no layout)";

fn gate(what: &'static str) -> Error {
    Error::Unsupported(unsupported(what))
}

/// The rectangles of an element, in window coordinates.
pub(crate) fn element_rects(d: &Dom, it: &mut Interp<'_>, el: NodeId) -> R<Vec<Rect>> {
    if !d.in_document(it, el)? {
        return Ok(Vec::new());
    }
    {
        let doc = d.doc.borrow();
        if doc.is_foreign(el) {
            return Err(gate(FOREIGN));
        }
        if matches!(doc.element_name(el), "col" | "colgroup") {
            return Err(gate("getClientRects of a col or colgroup (layout makes no box for a column)"));
        }
    }
    d.with_geo_it(it, |g, s, it| {
        let one: Vec<Rect> = g
            .layout
            .fragments
            .iter()
            .filter(|f| f.node == el && f.kind == Frag::Box)
            .map(|f| {
                let (dx, dy) = g.shift(s, el);
                Rect { x: f.rect.x + dx, y: f.rect.y + dy, ..f.rect }
            })
            .collect();
        if !one.is_empty() {
            return Ok(one);
        }
        with_pass(d, it, g, s, |p| p.element(el))
    })?
}

fn client_rects(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let el = d.el(it, this)?;
    let r = element_rects(d, it, el)?;
    crate::browser_geom::rect_list(d, it, &r)
}

fn range_rects(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<Vec<Rect>> {
    let b = bounds(d, it, this)?;
    if !d.in_document(it, b.0)? || !d.in_document(it, b.2)? {
        return Ok(Vec::new());
    }
    {
        let doc = d.doc.borrow();
        let mut n = Some(b.0);
        while let Some(x) = n.filter(|&x| x != doc.root()) {
            if doc.is_foreign(x) {
                return Err(gate(FOREIGN));
            }
            n = doc.parent(x);
        }
    }
    d.with_geo_it(it, |g, s, it| with_pass(d, it, g, s, |p| range(p, b)))?
}

pub(crate) fn range_client_rects(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let r = range_rects(d, it, this)?;
    crate::browser_geom::rect_list(d, it, &r)
}

pub(crate) fn range_bounding_rect(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let r = range_rects(d, it, this)?;
    crate::browser_geom::layout_rect(d, it, union(&r))
}

/// `getBoundingClientRect` of an element that has no box fragment: the union of its rectangles.
pub(crate) fn element_bounding(d: &Dom, it: &mut Interp<'_>, el: NodeId) -> R<Rect> {
    Ok(union(&element_rects(d, it, el)?))
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    d.def_method(it, d.p().element, "getClientRects", 0, client_rects).map(|_| ())
}
