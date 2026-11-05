//! The display list: the fragments of layout, as a flat list of things to draw, in the order of painting.
//!
//! How real browsers do it: Chrome (cc/Blink) records a `PaintOpBuffer` for each layer, then the compositor rasterises
//! it in tiles, on the GPU or the CPU. Firefox builds an `nsDisplayList` and turns it into WebRender commands. Both keep
//! the list so that a scroll or an animation can draw again with no new layout.
//!
//! Ours is the same idea with fewer parts: a `Vec<Item>` of rectangles, text runs and clips. The text of all the runs
//! is in one `String`. A frame reuses both buffers, so a repeat paint does not allocate.
//!
//! The order is CSS 2.1, Appendix E, for each "context". A context is the root, or a box that paints as one unit:
//!
//! 1. the background and the borders of the box itself,
//! 2. the backgrounds and borders of the blocks inside, in tree order (not the ones in a float, an inline-block,
//!    a positioned box or a box that clips),
//! 3. the floats, each one a context,
//! 4. the lines: backgrounds and borders of inline boxes, text and markers, in tree order, with the inline-blocks as contexts,
//! 5. the positioned boxes, each one a context, in tree order.
//!
//! Gaps, on purpose (S4 topic 3 adds them when the target needs them): no `z-index` (the property is not parsed), so a
//! positioned box paints in tree order over the normal flow. A positioned box makes a context in step 5, but its own
//! positioned children paint inside it and not in the context above. A flex or grid item paints as a block, not as an
//! inline-block. A box with `overflow` other than `visible` paints as one unit in the place of a block (step 2). No
//! scrollbars. Borders are solid only (`dashed`, `dotted`, `double` and the 3D styles draw as `solid`), with square
//! corners: where two sides meet, the top and the bottom side own the corner pixels. No radius (the property is not parsed).
//! An inline box cut over two lines draws the borders of all four sides on each piece.

use crate::canvas::IRect;
use bare_css::cascade::{ComputedStyle, Styles};
use bare_css::color::Color;
use bare_css::values::{Keyword, Property, Value};
use bare_html::dom::{Document, NodeData, NodeId};
use bare_layout::boxes::{self, Kind, Tree};
use bare_layout::fragment::{Fragment, Kind as FragmentKind, Layout};
use bare_layout::font::Metrics;
use bare_layout::text::{self, Transform, WhiteSpace};

/// One thing to draw.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Item {
    /// A rectangle of whole pixels. `color` is `0xAARRGGBB`.
    Rect { r: IRect, color: u32 },
    /// A run of text: `len` bytes at `at` in `DisplayList::text`. `x` is the left edge of the first letter, in pixels, and
    /// `baseline` is a whole pixel row.
    Text { x: f32, baseline: i32, size: f32, color: u32, at: u32, len: u32, bold: bool },
    /// Draw nothing outside `IRect` (and outside the clips before it) until the matching `PopClip`.
    PushClip(IRect),
    PopClip,
    /// A box that waits for its turn in the order of painting. Only in the scratch lists, never in `items`.
    Later(u32),
}

#[derive(Default)]
pub struct DisplayList {
    pub items: Vec<Item>,
    /// The letters of all `Item::Text`, one run after the other.
    pub text: String,
    /// The color of the page behind everything: the canvas.
    pub background: u32,
    // The scratch lists of `build`. They keep their memory from one frame to the next.
    lines: Vec<Item>,
    floats: Vec<u32>,
    positioned: Vec<u32>,
}

/// `Color` as `0xAARRGGBB`.
pub fn argb(c: Color) -> u32 {
    (c.a as u32) << 24 | (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32
}

/// What each part of the order of painting does with a box.
enum Layer {
    Block,
    Inline,
    /// A context in the place of the lines: an inline-block.
    Atomic,
    Float,
    Positioned,
    /// A context in the place of a block: a box that clips.
    Clipped,
}

/// Deeper than this, the contexts inside one another stop: the boxes below are not drawn. Layout stops there too
/// (`bare_layout::block::MAX_DEPTH`). It keeps the recursion of `Builder::context` small.
const MAX_DEPTH: usize = 400;

impl DisplayList {
    pub fn new() -> DisplayList {
        DisplayList::default()
    }

    /// Make the list for a page: `view` is the size of the canvas in pixels. What is outside it makes no item.
    pub fn build(&mut self, doc: &Document, styles: &Styles, layout: &Layout, view: (u32, u32)) {
        self.items.clear();
        self.text.clear();
        self.lines.clear();
        self.floats.clear();
        self.positioned.clear();
        self.background = 0xffff_ffff;
        let tree = Tree::new(doc, styles);
        let Some(root) = tree.root() else { return };
        if layout.fragments.is_empty() {
            return;
        }
        // The background of the root, or of `body` if the root has none, is the background of the canvas, and the
        // element does not paint it again (CSS Backgrounds 3, 2.11.2).
        let root_bg = tree.style(root).color(Property::BackgroundColor);
        let mut propagated = root;
        let mut canvas = root_bg;
        if root_bg.a == 0
            && let Some(body) = doc.children(root).find(|&n| matches!(doc.data(n), NodeData::Element { .. }) && doc.element_name(n) == "body")
            && let Some(style) = styles.style(body)
        {
            canvas = style.color(Property::BackgroundColor);
            propagated = body;
        }
        if canvas.a > 0 {
            // A translucent canvas color mixes with white. With alpha 255 the mix is the color itself.
            self.background = crate::canvas::blend(argb(canvas), 0xffff_ffff, canvas.a as u32);
        }
        let view = IRect { x0: 0, y0: 0, x1: view.0 as i32, y1: view.1 as i32 };
        let mut b = Builder { tree, doc, frags: &layout.fragments, list: self, view, propagated, depth: 0 };
        b.context(0);
    }
}

/// A coordinate in CSS pixels as a whole pixel: the edge goes to the nearest pixel, as Chrome snaps boxes.
fn snap(v: f32) -> i32 {
    // Saturating: a huge coordinate (`as` clamps) does not wrap.
    v.round() as i32
}

struct Builder<'a> {
    tree: Tree<'a>,
    doc: &'a Document,
    frags: &'a [Fragment],
    list: &'a mut DisplayList,
    view: IRect,
    /// The element whose background is the canvas.
    propagated: NodeId,
    depth: usize,
}

impl Builder<'_> {
    fn push(&mut self, line: bool, item: Item) {
        if line {
            self.list.lines.push(item);
        } else {
            self.list.items.push(item);
        }
    }

    fn layer(&self, f: &Fragment, style: &ComputedStyle) -> Layer {
        let kind = self.tree.kind(f.node);
        match kind {
            Kind::OutOfFlow => return Layer::Positioned,
            Kind::Float => return Layer::Float,
            _ => {}
        }
        // `relative` and `sticky` boxes paint over the flow. An inline box stays in its line (a gap).
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

    /// Paint the fragment `i` and everything in it, in the order of the module comment.
    fn context(&mut self, i: usize) {
        if self.depth >= MAX_DEPTH {
            return;
        }
        let f = self.frags[i];
        let style = self.tree.style(f.node);
        let clip = if i > 0 && clips(style) { Some(padding_box(&f, style)) } else { None };
        self.decor(&f, style, false);
        // A box that clips, with its inside outside the canvas, has nothing more to show.
        if let Some(c) = clip {
            if c.intersect(self.view).is_empty() {
                return;
            }
            self.list.items.push(Item::PushClip(c));
        }
        self.depth += 1;
        let (lines, floats, positioned) = (self.list.lines.len(), self.list.floats.len(), self.list.positioned.len());
        let end = f.end as usize;
        let mut j = i + 1;
        while j < end {
            let g = self.frags[j];
            match g.kind {
                FragmentKind::Box => {
                    let style = self.tree.style(g.node);
                    match self.layer(&g, style) {
                        Layer::Block => {
                            self.decor(&g, style, false);
                            j += 1;
                        }
                        Layer::Inline => {
                            self.decor(&g, style, true);
                            j += 1;
                        }
                        Layer::Atomic => {
                            self.list.lines.push(Item::Later(j as u32));
                            j = g.end as usize;
                        }
                        Layer::Float => {
                            self.list.floats.push(j as u32);
                            j = g.end as usize;
                        }
                        Layer::Positioned => {
                            self.list.positioned.push(j as u32);
                            j = g.end as usize;
                        }
                        Layer::Clipped => {
                            self.context(j);
                            j = g.end as usize;
                        }
                    }
                }
                FragmentKind::Text { start, end } => {
                    self.text(&g, start as usize, end as usize);
                    j += 1;
                }
                FragmentKind::Marker { number } => {
                    self.marker(&g, number);
                    j += 1;
                }
                FragmentKind::Static => j += 1,
            }
        }
        // Each deferred box is a context. A context inside pushes above these entries and takes them off again.
        let end = self.list.floats.len();
        for k in floats..end {
            self.context(self.list.floats[k] as usize);
        }
        self.list.floats.truncate(floats);
        let end = self.list.lines.len();
        for k in lines..end {
            match self.list.lines[k] {
                Item::Later(j) => self.context(j as usize),
                item => self.list.items.push(item),
            }
        }
        self.list.lines.truncate(lines);
        let end = self.list.positioned.len();
        for k in positioned..end {
            self.context(self.list.positioned[k] as usize);
        }
        self.list.positioned.truncate(positioned);
        if clip.is_some() {
            self.list.items.push(Item::PopClip);
        }
        self.depth -= 1;
    }

    /// The background and the borders of one box. `line`: it goes in the lines, not in the blocks.
    fn decor(&mut self, f: &Fragment, style: &ComputedStyle, line: bool) {
        if style.get(Property::Visibility) != Value::Keyword(Keyword::Visible) {
            return;
        }
        let b = border_box(f);
        if b.is_empty() {
            return;
        }
        let bg = argb(style.color(Property::BackgroundColor));
        if bg >> 24 != 0 && f.node != self.propagated {
            self.rect(line, b, bg);
        }
        let side = |w: Property, s: Property, c: Property| -> (i32, u32) {
            let w = style.px(w);
            match style.get(s) {
                Value::Keyword(Keyword::None | Keyword::Hidden) => (0, 0),
                _ if w <= 0.0 => (0, 0),
                // Chrome: a width under 1 px is 1 px, and others go down to whole pixels.
                _ => ((w.floor() as i32).max(1), argb(style.color(c))),
            }
        };
        let (top, tc) = side(Property::BorderTopWidth, Property::BorderTopStyle, Property::BorderTopColor);
        let (right, rc) = side(Property::BorderRightWidth, Property::BorderRightStyle, Property::BorderRightColor);
        let (bottom, bc) = side(Property::BorderBottomWidth, Property::BorderBottomStyle, Property::BorderBottomColor);
        let (left, lc) = side(Property::BorderLeftWidth, Property::BorderLeftStyle, Property::BorderLeftColor);
        if top + right + bottom + left == 0 {
            return;
        }
        let (w, h) = (b.x1 - b.x0, b.y1 - b.y0);
        // The sides cannot be thicker than the box.
        let top = top.min(h);
        let bottom = bottom.min(h - top);
        let left = left.min(w);
        let right = right.min(w - left);
        let (y0, y1) = (b.y0 + top, b.y1 - bottom);
        if top > 0 {
            self.rect(line, IRect { y1: b.y0 + top, ..b }, tc);
        }
        if bottom > 0 {
            self.rect(line, IRect { y0: b.y1 - bottom, ..b }, bc);
        }
        if left > 0 {
            self.rect(line, IRect { x1: b.x0 + left, y0, y1, ..b }, lc);
        }
        if right > 0 {
            self.rect(line, IRect { x0: b.x1 - right, y0, y1, ..b }, rc);
        }
    }

    fn rect(&mut self, line: bool, r: IRect, color: u32) {
        if color >> 24 != 0 && !r.intersect(self.view).is_empty() {
            self.push(line, Item::Rect { r, color });
        }
    }

    /// A run of text in the lines. The text is what layout measured: the spaces collapsed and the case changed, as
    /// `text::shown` does.
    fn text(&mut self, f: &Fragment, start: usize, end: usize) {
        let style = self.tree.style(f.node);
        if style.get(Property::Visibility) != Value::Keyword(Keyword::Visible) {
            return;
        }
        let NodeData::Text(span) = *self.doc.data(f.node) else { return };
        let full = self.doc.text(span);
        let raw = &full[start..end];
        let ws = WhiteSpace::of(style);
        let transform = Transform::of(style);
        let m = Metrics::of(style);
        let at = self.list.text.len();
        if transform == Transform::None && !(ws.collapse && needs_collapse(raw)) {
            // The common case: the text goes in as it is.
            self.list.text.push_str(raw);
        } else {
            let word_start = full[..start].chars().next_back().is_none_or(char::is_whitespace);
            self.list.text.push_str(&text::shown(raw, ws, transform, word_start));
        }
        self.run(f, style, m, at);
    }

    fn marker(&mut self, f: &Fragment, number: i32) {
        let style = self.tree.style(f.node);
        if style.get(Property::Visibility) != Value::Keyword(Keyword::Visible) {
            return;
        }
        let Some(text) = boxes::marker_text(style, number) else { return };
        let at = self.list.text.len();
        self.list.text.push_str(&text);
        self.run(f, style, Metrics::of(style), at);
    }

    /// The item for the text that was just added at `at`. It drops the text again if nothing of it is on the canvas.
    fn run(&mut self, f: &Fragment, style: &ComputedStyle, m: Metrics, at: usize) {
        let color = argb(style.color(Property::TextColor));
        let len = self.list.text.len() - at;
        let baseline = snap(f.rect.y + m.ascent);
        let (x0, x1) = (f.rect.x, f.rect.x + f.rect.w);
        let outside = baseline as f32 - m.ascent >= self.view.y1 as f32
            || (baseline as f32 + m.descent) <= 0.0
            || x0 >= self.view.x1 as f32
            || x1 <= 0.0
            || color >> 24 == 0
            || len == 0;
        if outside {
            self.list.text.truncate(at);
            return;
        }
        let bold = matches!(style.get(Property::FontWeight), Value::Number(w) if w >= 600.0);
        self.list.lines.push(Item::Text { x: x0, baseline, size: m.size, color, at: at as u32, len: len as u32, bold });
        // Lines, drawn after the text, with the font's numbers (Noto Sans Mono: the underline is 0.1 em below the
        // baseline and 0.05 em thick, the strike-through is 0.322 em above it).
        let thick = snap(m.size * 0.05).max(1);
        let (a, b) = (snap(x0), snap(x1));
        let line = match style.get(Property::TextDecoration) {
            Value::Keyword(Keyword::Underline) => Some(baseline + snap(m.size * 0.1)),
            Value::Keyword(Keyword::LineThrough) => Some(baseline - snap(m.size * 0.322) - thick / 2),
            Value::Keyword(Keyword::Overline) => Some(baseline - snap(m.ascent)),
            _ => None,
        };
        if let Some(y) = line {
            self.rect(true, IRect { x0: a, y0: y, x1: b, y1: y + thick }, color);
        }
    }
}

/// Does the text need `text::shown` for its spaces? A tab or a newline, or two spaces in a row.
fn needs_collapse(s: &str) -> bool {
    let b = s.as_bytes();
    b.iter().any(|&c| c == b'\t' || c == b'\n') || b.windows(2).any(|w| w == b"  ")
}

fn clips(style: &ComputedStyle) -> bool {
    let hidden = |p| !matches!(style.get(p), Value::Keyword(Keyword::Visible));
    hidden(Property::OverflowX) || hidden(Property::OverflowY)
}

fn border_box(f: &Fragment) -> IRect {
    IRect { x0: snap(f.rect.x), y0: snap(f.rect.y), x1: snap(f.rect.x + f.rect.w), y1: snap(f.rect.y + f.rect.h) }
}

/// The box inside the borders: what `overflow` clips to.
fn padding_box(f: &Fragment, style: &ComputedStyle) -> IRect {
    let b = border_box(f);
    let w = |p| style.px(p).max(0.0).floor() as i32;
    IRect { x0: b.x0 + w(Property::BorderLeftWidth), y0: b.y0 + w(Property::BorderTopWidth), x1: b.x1 - w(Property::BorderRightWidth), y1: b.y1 - w(Property::BorderBottomWidth) }
}
