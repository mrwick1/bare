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
//! A box that makes a stacking context (the root, a positioned box or a flex item with `z-index`, any box with `opacity`
//! under 1) paints as one unit. The boxes with a `z-index` other than 0 that are inside it, at any depth that is not
//! another context, wait in `DisplayList::hoist`. The context paints them at the end: the negative ones first (after its own
//! background, before the blocks in it), the positive ones last, sorted by `z-index` and then by tree order. A flex or grid
//! item paints as an inline-block (step 4). A page with no `opacity` and no `z-index` skips all of this.
//!
//! `opacity` under 1 is a layer: `PushLayer` keeps the pixels behind the box, the box paints as usual, and `PopLayer`
//! mixes the result with the kept pixels. Over an opaque backdrop that is the same as drawing the box in a layer of its own and
//! mixing the layer in, and it needs no second canvas. The layer covers the border boxes and the text of the whole box.
//!
//! Gaps, on purpose: no scrollbars. `double`, `groove`, `ridge`, `inset` and `outset` borders draw as `solid`. A rounded
//! box draws its border in one color (with four colors the sides are square), `dashed` and `dotted` borders that are
//! rounded draw as `solid`, and `overflow` clips to the square padding box, also when the box is rounded. An inline box
//! cut over two lines draws the borders of all four sides on each piece. `z-index` of an inline box does nothing.

use crate::canvas::{IRect, Shape};
use bare_css::cascade::{ComputedStyle, Styles};
use bare_css::color::Color;
use bare_css::values::{Keyword, Length, Property, Value};
use bare_html::dom::{Document, NodeData, NodeId};
use bare_layout::boxes::{self, Kind, Tree};
use bare_layout::controls::{self, Control};
use bare_layout::fragment::{Fragment, Kind as FragmentKind, Layout};
use bare_layout::scroll::Areas;
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
    /// A rectangle with round corners: `DisplayList::shapes[shape]`.
    Fill { shape: u32, color: u32 },
    /// What is in `shapes[shape]` and not in `shapes[shape + 1]`: a round border.
    Ring { shape: u32, color: u32 },
    /// A line of `width` pixels through `n` points from `DisplayList::points[at]`. One point is a dot.
    Stroke { at: u32, n: u32, width: f32, color: u32 },
    /// Draw nothing outside `IRect` (and outside the clips before it) until the matching `PopClip`.
    PushClip(IRect),
    PopClip,
    /// Everything up to the matching `PopLayer` shows with `alpha` (0 to 255) of its strength, inside `IRect`.
    PushLayer { r: IRect, alpha: u8 },
    PopLayer,
    /// The items until `PopScroll` are the content of the scroll container `node`: they move with its scroll offset.
    /// The offset is read when the list is drawn, so a scroll does not build the list again.
    PushScroll(NodeId),
    PopScroll,
    /// The items until `PopFixed` belong to a `position: fixed` box: they stay where they are when the page scrolls, and no
    /// clip of a box around them cuts them.
    PushFixed,
    PopFixed,
    /// The scrollbars of the scroll container `node`, drawn in the padding box `r`. The offset is read at draw time.
    Bars { node: NodeId, r: IRect },
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
    /// The shapes of `Item::Fill` and `Item::Ring`.
    pub shapes: Vec<Shape>,
    /// The points of `Item::Stroke`.
    pub points: Vec<(f32, f32)>,
    /// Text fields that the host draws the text of: the field, and whether it has text now. Paint draws only the
    /// placeholder of an empty one, and nothing for the others. The host sets it before `build`.
    pub hosted: Vec<(NodeId, bool)>,
    // The scratch lists of `build`. They keep their memory from one frame to the next.
    lines: Vec<Item>,
    floats: Vec<u32>,
    positioned: Vec<u32>,
    hoist: Vec<Hoisted>,
    /// The scroll containers around the boxes that wait in `hoist`: (box, padding box). One run for each box.
    chain: Vec<(NodeId, IRect)>,
}

/// A box of a stacking context with a `z-index` other than 0, on its way up to the context. `clip` is what clipped it
/// where it is in the tree.
#[derive(Clone, Copy)]
struct Hoisted {
    z: i32,
    frag: u32,
    clip: IRect,
    /// Where the scroll containers around the box are in `DisplayList::chain`.
    chain: (u32, u32),
}

/// What the styles of the page need from paint: the checks are skipped when no style asks for them.
#[derive(Default, Clone, Copy)]
struct Page {
    /// `opacity` under 1 or `z-index` is used.
    stack: bool,
    /// A flex or grid container is used.
    flex: bool,
    /// The page has an `<input>`, `<select>`, `<textarea>` or `<button>`.
    controls: bool,
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
    /// A positioned box, or a box that makes a stacking context. `z`: the order in its context.
    Positioned(i32),
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
        self.build_with(doc, styles, layout, view, None);
    }

    /// Like `build`, for a page that scrolls. `areas` are its scroll containers (`bare_layout::scroll::areas`). The list holds
    /// the whole page, not only what `view` shows: `view` is the size of the page, and what a scroll container holds is
    /// not left out. The items of a scroll container sit between `PushScroll` and `PopScroll`, a fixed box between
    /// `PushFixed` and `PopFixed`, so that a scroll draws the same list again with another offset.
    pub fn build_with(&mut self, doc: &Document, styles: &Styles, layout: &Layout, view: (u32, u32), areas: Option<&Areas>) {
        self.items.clear();
        self.chain.clear();
        self.text.clear();
        self.lines.clear();
        self.floats.clear();
        self.positioned.clear();
        self.hoist.clear();
        self.shapes.clear();
        self.points.clear();
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
        let mut page = Page { controls: [tree.controls.input, tree.controls.select, tree.controls.textarea, tree.controls.button].iter().any(Option::is_some), ..Page::default() };
        for style in styles.each() {
            page.stack |= matches!(style.get(Property::Opacity), Value::Number(o) if o < 1.0) || matches!(style.get(Property::ZIndex), Value::Number(_));
            page.flex |= matches!(style.get(Property::Display), Value::Keyword(Keyword::Flex | Keyword::InlineFlex | Keyword::Grid | Keyword::InlineGrid));
        }
        let mut b = Builder { tree, doc, frags: &layout.fragments, list: self, view, propagated, depth: 0, page, clips: Vec::new(), areas, scrolls: Vec::new() };
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
    page: Page,
    /// The clips that are open now (the same as the ones of `Painter::draw`).
    clips: Vec<IRect>,
    /// The scroll containers of the page, if it scrolls.
    areas: Option<&'a Areas>,
    /// The scroll containers that are open now, the inner last, with their padding boxes.
    scrolls: Vec<(NodeId, IRect)>,
}

impl Builder<'_> {
    fn push(&mut self, line: bool, item: Item) {
        if line {
            self.list.lines.push(item);
        } else {
            self.list.items.push(item);
        }
    }

    /// Is this box an item of a flex or grid container?
    fn item(&self, node: NodeId) -> bool {
        self.page.flex && matches!(self.tree.kind(node), Kind::Block | Kind::ListItem) && self.tree.is_flex_or_grid(self.doc.flat_parent(node))
    }

    /// The place of a box in the order of its stacking context, if the box makes one: `z-index` (it counts for a positioned
    /// box and a flex item) and `opacity`. A box that is no context gives `None`.
    fn stack(&self, f: &Fragment, style: &ComputedStyle) -> Option<i32> {
        if !self.page.stack {
            return None;
        }
        let z = match style.get(Property::ZIndex) {
            Value::Number(n) if style.get(Property::Position) != Value::Keyword(Keyword::Static) || self.item(f.node) => Some(n as i32),
            _ => None,
        };
        match style.get(Property::Opacity) {
            Value::Number(o) if o < 1.0 => Some(z.unwrap_or(0)),
            _ => z,
        }
    }

    fn layer(&self, f: &Fragment, style: &ComputedStyle) -> Layer {
        let kind = self.tree.kind(f.node);
        match kind {
            Kind::OutOfFlow => return Layer::Positioned(self.stack(f, style).unwrap_or(0)),
            Kind::Float => return Layer::Float,
            _ => {}
        }
        // `relative` and `sticky` boxes paint over the flow. An inline box stays in its line (a gap).
        if kind != Kind::Inline {
            let stack = self.stack(f, style);
            if stack.is_some() || style.get(Property::Position) != Value::Keyword(Keyword::Static) {
                return Layer::Positioned(stack.unwrap_or(0));
            }
        }
        match kind {
            Kind::Inline => Layer::Inline,
            Kind::Atomic => Layer::Atomic,
            _ if self.item(f.node) => Layer::Atomic,
            _ if clips(style) => Layer::Clipped,
            _ => Layer::Block,
        }
    }

    /// The border boxes of the fragments `i..end`, together, in whole pixels: what a layer must hold.
    fn extent(&self, i: usize) -> IRect {
        let mut r = IRect { x0: i32::MAX, y0: i32::MAX, x1: i32::MIN, y1: i32::MIN };
        for f in &self.frags[i..self.frags[i].end as usize] {
            let (x0, y0, x1, y1) = (f.rect.x.floor() as i32, f.rect.y.floor() as i32, (f.rect.x + f.rect.w).ceil() as i32, (f.rect.y + f.rect.h).ceil() as i32);
            r = IRect { x0: r.x0.min(x0), y0: r.y0.min(y0), x1: r.x1.max(x1), y1: r.y1.max(y1) };
        }
        r
    }

    /// Paint the fragment `i` and everything in it, in the order of the module comment. A fixed box goes between
    /// `PushFixed` and `PopFixed`.
    fn context(&mut self, i: usize) {
        let f = self.frags[i];
        let fixed = self.areas.is_some() && i > 0 && self.tree.kind(f.node) != Kind::Inline && self.tree.position(f.node) == Keyword::Fixed;
        if fixed {
            self.list.items.push(Item::PushFixed);
        }
        self.context_in(i);
        if fixed {
            self.list.items.push(Item::PopFixed);
        }
    }

    fn context_in(&mut self, i: usize) {
        if self.depth >= MAX_DEPTH {
            return;
        }
        let f = self.frags[i];
        let style = self.tree.style(f.node);
        let sc = i == 0 || self.stack(&f, style).is_some();
        let mut layer = false;
        if self.page.stack && i > 0 {
            let alpha = match style.get(Property::Opacity) {
                Value::Number(o) if o < 1.0 => (o * 255.0 + 0.5) as u8,
                _ => 255,
            };
            if alpha == 0 {
                return;
            }
            if alpha < 255 {
                let r = self.extent(i).intersect(self.view);
                if r.is_empty() {
                    return;
                }
                self.list.items.push(Item::PushLayer { r, alpha });
                layer = true;
            }
        }
        let clip = if i > 0 && clips(style) { Some(padding_box(&f, style)) } else { None };
        self.decor(&f, style, false);
        // A box that clips, with its inside outside the canvas, has nothing more to show.
        let outer_view = self.view;
        if let Some(c) = clip {
            if c.intersect(self.view).is_empty() {
                if layer {
                    self.list.items.push(Item::PopLayer);
                }
                return;
            }
            self.list.items.push(Item::PushClip(c));
            self.clips.push(c);
            if self.areas.is_some() {
                // What is inside can be scrolled into view: nothing in it is left out for being outside the page.
                self.list.items.push(Item::PushScroll(f.node));
                self.scrolls.push((f.node, c));
                self.view = IRect { x0: i32::MIN / 2, y0: i32::MIN / 2, x1: i32::MAX / 2, y1: i32::MAX / 2 };
            }
        }
        // The scroll containers around this one: a box that waits in `hoist` below it goes out of them, and is wrapped in them again.
        let depth_here = self.scrolls.len();
        self.depth += 1;
        let hoist = self.list.hoist.len();
        let inside = self.list.items.len();
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
                        Layer::Positioned(z) if z != 0 => {
                            let clip = self.clips.last().copied().unwrap_or(self.view);
                            let at = self.list.chain.len() as u32;
                            self.list.chain.extend_from_slice(&self.scrolls);
                            self.list.hoist.push(Hoisted { z, frag: j as u32, clip, chain: (at, self.scrolls.len() as u32) });
                            j = g.end as usize;
                        }
                        Layer::Positioned(_) => {
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
        if sc {
            self.hoisted(hoist, inside, depth_here);
        }
        if clip.is_some() {
            if self.areas.is_some() {
                self.list.items.push(Item::PopScroll);
                self.scrolls.pop();
                self.view = outer_view;
                if self.areas.and_then(|a| a.get(f.node)).is_some_and(|a| a.bars != (false, false)) {
                    self.list.items.push(Item::Bars { node: f.node, r: clip.expect("checked") });
                }
            }
            self.list.items.push(Item::PopClip);
            self.clips.pop();
        }
        if layer {
            self.list.items.push(Item::PopLayer);
        }
        self.depth -= 1;
    }

    /// The end of a stacking context: paint the boxes that came up to it from `hoist`. The ones with a negative `z-index`
    /// go to the place `inside` (behind the blocks of the context), the others to the end.
    fn hoisted(&mut self, hoist: usize, inside: usize, depth_here: usize) {
        let end = self.list.hoist.len();
        if end == hoist {
            return;
        }
        self.list.hoist[hoist..].sort_by_key(|h| (h.z, h.frag));
        let negative = self.list.hoist[hoist..].partition_point(|h| h.z < 0) + hoist;
        let before = self.list.items.len();
        for k in hoist..end {
            if k == negative {
                // The negative ones are done: move them in front of what the context painted before.
                let moved = self.list.items.len() - before;
                self.list.items[inside..].rotate_right(moved);
            }
            let h = self.list.hoist[k];
            // The scroll containers that the box left on its way up to this context: open them again around it.
            let (at, n) = (h.chain.0 as usize, h.chain.1 as usize);
            let reopen = n.saturating_sub(depth_here);
            let saved = self.view;
            for e in 0..reopen {
                let (node, c) = self.list.chain[at + depth_here + e];
                self.list.items.push(Item::PushClip(c));
                self.list.items.push(Item::PushScroll(node));
                self.clips.push(c);
                self.view = IRect { x0: i32::MIN / 2, y0: i32::MIN / 2, x1: i32::MAX / 2, y1: i32::MAX / 2 };
            }
            let clipped = self.areas.is_none() && h.clip != self.view;
            if clipped {
                self.list.items.push(Item::PushClip(h.clip));
                self.clips.push(h.clip);
            }
            self.context(h.frag as usize);
            if clipped {
                self.list.items.push(Item::PopClip);
                self.clips.pop();
            }
            for _ in 0..reopen {
                self.list.items.push(Item::PopScroll);
                self.list.items.push(Item::PopClip);
                self.clips.pop();
            }
            self.view = saved;
        }
        if negative == end {
            let moved = self.list.items.len() - before;
            self.list.items[inside..].rotate_right(moved);
        }
        self.list.hoist.truncate(hoist);
    }

    /// The background and the borders of one box. `line`: it goes in the lines, not in the blocks.
    fn decor(&mut self, f: &Fragment, style: &ComputedStyle, line: bool) {
        if style.get(Property::Visibility) != Value::Keyword(Keyword::Visible) {
            return;
        }
        let b = border_box(f);
        // A box outside the canvas has nothing to draw: no shape, no dash, no text of its own.
        if b.intersect(self.view).is_empty() {
            return;
        }
        let control = if !line && self.page.controls { controls::of(&self.tree, f.node) } else { None };
        let native = control.filter(|&c| is_native(style, c));
        if let Some(c) = native {
            self.native(f, b, c);
        } else {
            let round = radii(style, b);
            let bg = argb(style.color(Property::BackgroundColor));
            if bg >> 24 != 0 && f.node != self.propagated {
                match round {
                    Some(s) => self.fill(line, s, bg),
                    None => self.rect(line, b, bg),
                }
            }
            self.borders(line, b, style, round);
        }
        if let Some(c) = control {
            self.control(f, style, c);
        }
    }

    fn borders(&mut self, line: bool, b: IRect, style: &ComputedStyle, round: Option<Shape>) {
        #[derive(Clone, Copy, PartialEq)]
        enum Dash {
            Solid,
            Dashed,
            Dotted,
        }
        let side = |w: Property, s: Property, c: Property| -> (i32, u32, Dash) {
            let w = style.px(w);
            match style.get(s) {
                Value::Keyword(Keyword::None | Keyword::Hidden) => (0, 0, Dash::Solid),
                _ if w <= 0.0 => (0, 0, Dash::Solid),
                // The computed width is a whole number already (`Styles` snaps it, as Chrome does).
                Value::Keyword(Keyword::Dashed) => (w as i32, argb(style.color(c)), Dash::Dashed),
                Value::Keyword(Keyword::Dotted) => (w as i32, argb(style.color(c)), Dash::Dotted),
                _ => (w as i32, argb(style.color(c)), Dash::Solid),
            }
        };
        let (top, tc, td) = side(Property::BorderTopWidth, Property::BorderTopStyle, Property::BorderTopColor);
        let (right, rc, rd) = side(Property::BorderRightWidth, Property::BorderRightStyle, Property::BorderRightColor);
        let (bottom, bc, bd) = side(Property::BorderBottomWidth, Property::BorderBottomStyle, Property::BorderBottomColor);
        let (left, lc, ld) = side(Property::BorderLeftWidth, Property::BorderLeftStyle, Property::BorderLeftColor);
        if top + right + bottom + left == 0 {
            return;
        }
        let (w, h) = (b.x1 - b.x0, b.y1 - b.y0);
        // The sides cannot be thicker than the box.
        let top = top.min(h);
        let bottom = bottom.min(h - top);
        let left = left.min(w);
        let right = right.min(w - left);
        if let Some(outer) = round
            && tc == rc
            && rc == bc
            && bc == lc
        {
            self.list.shapes.push(outer);
            self.list.shapes.push(outer.inset(top, right, bottom, left));
            if tc >> 24 != 0 {
                let shape = self.list.shapes.len() as u32 - 2;
                self.push(line, Item::Ring { shape, color: tc });
            } else {
                self.list.shapes.truncate(self.list.shapes.len() - 2);
            }
            return;
        }
        let (y0, y1) = (b.y0 + top, b.y1 - bottom);
        // Each side as a rectangle, or as dashes along it.
        let side = |this: &mut Self, r: IRect, horizontal: bool, width: i32, color: u32, dash: Dash| match dash {
            Dash::Solid => this.rect(line, r, color),
            _ => this.dashes(line, r, horizontal, width, color, dash == Dash::Dotted),
        };
        // A dashed side runs the whole length of the box, as in Chrome, and the sides next to it overlap in the corners.
        let across = |d: Dash, full: IRect, short: IRect| if d == Dash::Solid { short } else { full };
        if top > 0 {
            side(self, IRect { y1: b.y0 + top, ..b }, true, top, tc, td);
        }
        if bottom > 0 {
            side(self, IRect { y0: b.y1 - bottom, ..b }, true, bottom, bc, bd);
        }
        if left > 0 {
            side(self, across(ld, IRect { x1: b.x0 + left, ..b }, IRect { x1: b.x0 + left, y0, y1, ..b }), false, left, lc, ld);
        }
        if right > 0 {
            side(self, across(rd, IRect { x0: b.x1 - right, ..b }, IRect { x0: b.x1 - right, y0, y1, ..b }), false, right, rc, rd);
        }
    }

    /// A `dashed` or `dotted` side. The dashes are as Chrome sizes them (`StrokeData::SetupPaintDashPathEffect`): a dash is
    /// 3 times the width for a width under 3, and 2 times above (the gap starts as 2 and 1 times), a dot is the width and
    /// so is the gap. Then the gap grows or shrinks a little so that a whole number of dashes fills the side, from end to end.
    fn dashes(&mut self, line: bool, r: IRect, horizontal: bool, width: i32, color: u32, dotted: bool) {
        let len = if horizontal { r.x1 - r.x0 } else { r.y1 - r.y0 } as f32;
        let w = width as f32;
        let (dash, gap) = match () {
            _ if dotted => (w, w),
            _ if width >= 3 => (2.0 * w, w),
            _ => (3.0 * w, 2.0 * w),
        };
        // The number of dashes: the one before and the one after the size that fits, the gap nearer to the first wins.
        let low = ((len + gap) / (dash + gap)).floor().max(1.0);
        let gap_of = |n: f32| (len - n * dash) / (n - 1.0);
        let (n, gap) = if low < 2.0 || len <= dash {
            (1.0, 0.0)
        } else if gap_of(low + 1.0) <= 0.0 || (gap_of(low) - gap).abs() < (gap_of(low + 1.0) - gap).abs() {
            (low, gap_of(low))
        } else {
            (low + 1.0, gap_of(low + 1.0))
        };
        let (dash, round) = if n == 1.0 && len <= dash { (len, false) } else { (dash, dotted && width >= 4) };
        let (lo, hi) = if horizontal { (self.view.x0 as f32, self.view.x1 as f32) } else { (self.view.y0 as f32, self.view.y1 as f32) };
        let start = if horizontal { r.x0 } else { r.y0 } as f32;
        for k in 0..n as u32 {
            let at = start + k as f32 * (dash + gap);
            if at + dash < lo || at > hi {
                continue;
            }
            // Chrome puts a dash on the pixel that holds its start, and the end on the pixel that holds its end.
            let (a, z) = (at.floor() as i32, ((at + dash).floor() as i32).max(at.floor() as i32 + 1));
            if round {
                // The dot sits in the middle of the side.
                let (cx, cy) = if horizontal { (at + dash / 2.0, (r.y0 + r.y1) as f32 / 2.0) } else { ((r.x0 + r.x1) as f32 / 2.0, at + dash / 2.0) };
                let n = self.list.points.len() as u32;
                self.list.points.push((cx, cy));
                self.push(line, Item::Stroke { at: n, n: 1, width: w, color });
            } else if horizontal {
                self.rect(line, IRect { x0: a, x1: z, ..r }, color);
            } else {
                self.rect(line, IRect { y0: a, y1: z, ..r }, color);
            }
        }
    }

    fn fill(&mut self, line: bool, s: Shape, color: u32) {
        if color >> 24 != 0 && !s.r.intersect(self.view).is_empty() {
            let shape = self.list.shapes.len() as u32;
            self.list.shapes.push(s);
            self.push(line, Item::Fill { shape, color });
        }
    }

    /// A round border of `width` pixels around `s`, in `color`.
    fn ring(&mut self, s: Shape, width: i32, color: u32) {
        let shape = self.list.shapes.len() as u32;
        self.list.shapes.push(s);
        self.list.shapes.push(s.inset(width, width, width, width));
        self.list.items.push(Item::Ring { shape, color });
    }

    fn stroke(&mut self, points: &[(f32, f32)], width: f32, color: u32) {
        let at = self.list.points.len() as u32;
        self.list.points.extend_from_slice(points);
        self.list.items.push(Item::Stroke { at, n: points.len() as u32, width, color });
    }

    /// The look of Chrome's own form controls, for a control whose style is the one of the UA style sheet (`is_native`).
    fn native(&mut self, f: &Fragment, b: IRect, c: Control) {
        let a = &self.tree.controls;
        let checked = a.checked.is_some_and(|at| self.doc.attr(f.node, at).is_some());
        const GRAY: u32 = 0xff76_7676;
        const BLUE: u32 = 0xff00_75ff;
        let (x, y) = (b.x0 as f32, b.y0 as f32);
        match c {
            Control::Field | Control::TextArea | Control::Select => {
                // A 1 px border with the corner pixels left out: Chrome draws no more of the corner.
                let (m, e) = (IRect { x0: b.x0 + 1, x1: b.x1 - 1, ..b }, IRect { y0: b.y0 + 1, y1: b.y1 - 1, ..b });
                self.rect(false, m, 0xffff_ffff);
                self.rect(false, e, 0xffff_ffff);
                self.rect(false, IRect { y1: b.y0 + 1, ..m }, GRAY);
                self.rect(false, IRect { y0: b.y1 - 1, ..m }, GRAY);
                self.rect(false, IRect { x1: b.x0 + 1, ..e }, GRAY);
                self.rect(false, IRect { x0: b.x1 - 1, ..e }, GRAY);
                if c == Control::TextArea {
                    // The grip: two short slanted lines in the corner.
                    let (gx, gy) = (b.x1 as f32, b.y1 as f32);
                    self.stroke(&[(gx - 3.0, gy - 1.5), (gx - 1.5, gy - 3.0)], 1.0, 0xff76_7676);
                    self.stroke(&[(gx - 6.5, gy - 1.5), (gx - 1.5, gy - 6.5)], 1.0, 0xff76_7676);
                }
                if c == Control::Select {
                    // The arrow: a "v" 8.6 px wide at the right.
                    let (cx, cy) = (b.x1 as f32 - 7.8, (b.y0 + b.y1) as f32 / 2.0);
                    self.stroke(&[(cx - 4.3, cy - 2.1), (cx, cy + 2.3), (cx + 4.3, cy - 2.1)], 1.8, 0xff00_0000);
                }
            }
            Control::Label | Control::Button => {
                let s = Shape::new(b, [2.9; 8]);
                self.fill(false, s, 0xffef_efef);
                self.ring(s, 1, GRAY);
            }
            Control::Check { radio: false } => {
                let s = Shape::new(b, [2.9; 8]);
                if checked {
                    self.fill(false, Shape::new(b, [2.0; 8]), BLUE);
                    self.stroke(&[(x + 3.2, y + 5.9), (x + 5.7, y + 9.0), (x + 10.3, y + 3.3)], 1.9, 0xffff_ffff);
                } else {
                    self.fill(false, s, 0xffff_ffff);
                    self.ring(s, 1, GRAY);
                }
            }
            Control::Check { radio: true } => {
                let s = Shape::new(b, [(b.x1 - b.x0) as f32 / 2.0; 8]);
                if checked {
                    self.fill(false, s, BLUE);
                    self.fill(false, s.inset(1, 1, 1, 1), 0xffff_ffff);
                    self.stroke(&[((b.x0 + b.x1) as f32 / 2.0, (b.y0 + b.y1) as f32 / 2.0)], 6.0, BLUE);
                } else {
                    self.fill(false, s, 0xffff_ffff);
                    self.ring(s, 1, GRAY);
                }
            }
        }
    }

    /// The text of a control: the value of a field, its placeholder, the label of a button, the option of a select. It sits
    /// in the content box, one line, centered in the height, and the box clips it.
    fn control(&mut self, f: &Fragment, style: &ComputedStyle, c: Control) {
        let a = self.tree.controls;
        let get = |name: Option<_>| name.and_then(|at| self.doc.attr(f.node, at)).unwrap_or("");
        let m = Metrics::of(style);
        let (value, color): (String, u32) = match c {
            Control::Field => {
                let hosted = self.list.hosted.iter().find(|h| h.0 == f.node).map(|h| h.1);
                if hosted == Some(true) {
                    return;
                }
                let v = if hosted.is_some() { "" } else { get(a.value) };
                let is_password = a.kind.and_then(|k| self.doc.attr(f.node, k)).is_some_and(|k| k.eq_ignore_ascii_case("password"));
                if v.is_empty() {
                    (get(a.placeholder).to_string(), 0xff75_7575)
                } else if is_password {
                    ("\u{2022}".repeat(v.chars().count()), argb(style.color(Property::TextColor)))
                } else {
                    (v.to_string(), argb(style.color(Property::TextColor)))
                }
            }
            Control::Label => (controls::label(&self.tree, f.node).to_string(), argb(style.color(Property::TextColor))),
            Control::Select => (controls::selected_text(&self.tree, f.node), argb(style.color(Property::TextColor))),
            // The text of a textarea is its child, and layout places it. Only the placeholder is ours.
            Control::TextArea if self.doc.children(f.node).next().is_none() => (get(a.placeholder).to_string(), 0xff75_7575),
            _ => return,
        };
        if value.is_empty() || color >> 24 == 0 {
            return;
        }
        let px = |p| style.px(p);
        let (l, t) = (px(Property::BorderLeftWidth) + px(Property::PaddingLeft), px(Property::BorderTopWidth) + px(Property::PaddingTop));
        let w = f.rect.w - l - px(Property::BorderRightWidth) - px(Property::PaddingRight);
        let h = f.rect.h - t - px(Property::BorderBottomWidth) - px(Property::PaddingBottom);
        let content = IRect { x0: snap(f.rect.x + l), y0: snap(f.rect.y + t), x1: snap(f.rect.x + l + w), y1: snap(f.rect.y + t + h) };
        if content.intersect(self.view).is_empty() {
            return;
        }
        // The line is as wide as the letters: the label of a button is in the middle.
        let width = controls::text_width(&m, &value);
        let x = f.rect.x + l + if c == Control::Label { ((w - width) / 2.0).max(0.0) } else { 0.0 };
        let baseline = snap(f.rect.y + t + if c == Control::TextArea { m.ascent } else { controls::baseline(&self.tree, f.node, h) });
        let at = self.list.text.len() as u32;
        self.list.text.push_str(&value);
        let len = value.len() as u32;
        let bold = matches!(style.get(Property::FontWeight), Value::Number(w) if w >= 600.0);
        self.list.items.push(Item::PushClip(content));
        self.list.items.push(Item::Text { x, baseline, size: m.size, color, at, len, bold });
        self.list.items.push(Item::PopClip);
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

/// Has the control the look of the UA style sheet (border, background), so that Chrome draws it with its own theme? A style
/// of the page (`border`, `background`, `appearance: none`) turns the theme off, and the box is then the one of CSS.
fn is_native(style: &ComputedStyle, c: Control) -> bool {
    use Keyword as K;
    if style.get(Property::Appearance) == Value::Keyword(K::None) {
        return false;
    }
    let (bg, width, kind) = match c {
        Control::Field => (0xffff_ffff, 2.0, K::Inset),
        Control::Label | Control::Button => (0xffef_efef, 2.0, K::Outset),
        Control::TextArea | Control::Select => (0xffff_ffff, 1.0, K::Solid),
        Control::Check { .. } => (0, 0.0, K::None),
    };
    if argb(style.color(Property::BackgroundColor)) != bg {
        return false;
    }
    let sides = [
        (Property::BorderTopWidth, Property::BorderTopStyle, Property::BorderTopColor),
        (Property::BorderRightWidth, Property::BorderRightStyle, Property::BorderRightColor),
        (Property::BorderBottomWidth, Property::BorderBottomStyle, Property::BorderBottomColor),
        (Property::BorderLeftWidth, Property::BorderLeftStyle, Property::BorderLeftColor),
    ];
    sides.iter().all(|&(w, s, col)| {
        style.px(w) == width && (width == 0.0 || (style.get(s) == Value::Keyword(kind) && argb(style.color(col)) == 0xff76_7676))
    })
}

/// The shape of a box with `border-radius`, or `None` if all four corners are square.
fn radii(style: &ComputedStyle, b: IRect) -> Option<Shape> {
    let (w, h) = ((b.x1 - b.x0) as f32, (b.y1 - b.y0) as f32);
    let corner = |p| match style.get(p) {
        Value::Length(Length::Px(x)) if x > 0.0 => (x, x),
        Value::Length(Length::Percent(x)) if x > 0.0 => (x / 100.0 * w, x / 100.0 * h),
        _ => (0.0, 0.0),
    };
    let c = [Property::BorderTopLeftRadius, Property::BorderTopRightRadius, Property::BorderBottomRightRadius, Property::BorderBottomLeftRadius].map(corner);
    if c.iter().all(|&(x, y)| x == 0.0 && y == 0.0) {
        return None;
    }
    Some(Shape::new(b, [c[0].0, c[0].1, c[1].0, c[1].1, c[2].0, c[2].1, c[3].0, c[3].1]))
}
