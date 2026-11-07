//! Paint: draw the fragments of layout into a pixel buffer, and write the buffer as a PNG.
//!
//! The way in is `Painter::paint` (or `capture` for one picture). It makes a display list from the fragments
//! (`display.rs`), then draws the list into a `Canvas` (`canvas.rs`): solid rectangles for backgrounds and borders, and
//! glyphs from Noto Sans Mono (`glyphs.rs`) for text, at the places and with the widths that layout used. The picture is
//! a plain `Vec<u32>`, so a window (S4 topic 2) only has to show it. `png.rs` is the capture for tests and for
//! `bare --paint`.
//!
//! The design, the measurements behind the crates, and the gaps are in `docs/resources.md`, "Paint and pixel capture".

pub mod bars;
pub mod canvas;
pub mod display;
pub mod glyphs;
pub mod hit;
pub mod png;
pub mod svg;

use bare_css::cascade::Styles;
use bare_html::dom::Document;
use bare_layout::font::{self, Units};
use bare_html::dom::NodeId;
use bare_layout::fragment::Layout;
use bare_layout::scroll::Areas;
use std::collections::HashMap;
use canvas::{Canvas, IRect};
use display::{DisplayList, Item};
use glyphs::{Glyphs, SUBPIXELS};

/// How far the page and its scroll containers are scrolled, in CSS pixels (the offset of the top left corner of the content).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Scroll {
    pub x: f32,
    pub y: f32,
    pub nodes: HashMap<NodeId, (f32, f32)>,
}

impl Scroll {
    /// The offset of the scroll container `node`.
    pub fn of(&self, node: NodeId) -> (f32, f32) {
        self.nodes.get(&node).copied().unwrap_or((0.0, 0.0))
    }
}

/// What a scrolled frame needs besides the display list: the offsets, the scroll containers, and whether the page itself has
/// scrollbars (its content is narrower than the window by the vertical bar, and shorter by the horizontal one).
#[derive(Clone, Copy)]
pub struct View<'a> {
    pub scroll: &'a Scroll,
    pub areas: &'a Areas,
    pub bars: (bool, bool),
}

/// Draws pages. Keep one for many frames: the buffers and the glyph cache stay, so the next paint allocates nothing.
#[derive(Default)]
pub struct Painter {
    pub glyphs: Glyphs,
    pub list: DisplayList,
    /// The clips that are open now: the last one is the one that counts, and it is inside all the others.
    clips: Vec<IRect>,
    /// The layers that are open: the box, the alpha and where the kept backdrop starts in `saved`.
    layers: Vec<(IRect, u8, usize)>,
    saved: Vec<u32>,
    /// The translations that are open: what to go back to at `PopScroll` and `PopFixed`.
    offsets: Vec<(i32, i32)>,
}

impl Painter {
    pub fn new() -> Painter {
        Painter::default()
    }

    /// Paint a page into `canvas`. The whole canvas is drawn again: no part of the old picture stays.
    pub fn paint(&mut self, doc: &Document, styles: &Styles, layout: &Layout, canvas: &mut Canvas) {
        // A sticky box is moved even when the page is not scrolled (`top: 20px` pushes it down): that needs the scroll containers.
        if styles.each().any(|s| s.get(bare_css::values::Property::Position) == bare_css::values::Value::Keyword(bare_css::values::Keyword::Sticky)) {
            let areas = bare_layout::scroll::areas(doc, styles, layout);
            self.build(doc, styles, layout, &areas, (canvas.width, canvas.height));
            self.draw_view(canvas, Some(&View { scroll: &Scroll::default(), areas: &areas, bars: (false, false) }));
            return;
        }
        self.list.build(doc, styles, layout, (canvas.width, canvas.height));
        self.draw(canvas);
    }

    /// Build the display list of the whole page, for `draw_view`. Call it when the page, its style or its layout changed, not
    /// for a scroll: a scroll only calls `draw_view` again.
    pub fn build(&mut self, doc: &Document, styles: &Styles, layout: &Layout, areas: &Areas, window: (u32, u32)) {
        let view = (window.0.max(areas.page.0.ceil() as u32), window.1.max(areas.page.1.ceil() as u32));
        self.list.build_with(doc, styles, layout, view, Some(areas));
    }

    /// Draw the display list that `paint` made.
    pub fn draw(&mut self, canvas: &mut Canvas) {
        self.draw_view(canvas, None);
    }

    /// Draw the display list with the scroll offsets of `view`, and the scrollbars of the page.
    pub fn draw_view(&mut self, canvas: &mut Canvas, view: Option<&View>) {
        let Painter { glyphs, list, clips, layers, saved, offsets } = self;
        canvas.pixels.fill(list.background);
        clips.clear();
        layers.clear();
        saved.clear();
        offsets.clear();
        let bounds = canvas.bounds();
        clips.push(bounds);
        // The translation of what is drawn now: the scroll offsets of the page and of the containers around it.
        let mut off = view.map_or((0, 0), |v| (-(v.scroll.x.round() as i32), -(v.scroll.y.round() as i32)));
        for &item in &list.items {
            let clip = *clips.last().expect("the first clip is the canvas");
            let (tx, ty) = off;
            let at = |r: IRect| IRect { x0: r.x0 + tx, y0: r.y0 + ty, x1: r.x1 + tx, y1: r.y1 + ty };
            match item {
                Item::Rect { r, color } => canvas.fill(at(r).intersect(clip), color),
                Item::Fill { shape, color } => canvas.fill_shape(&moved(&list.shapes[shape as usize], off), color, clip),
                Item::Ring { shape, color } => canvas.fill_ring(&moved(&list.shapes[shape as usize], off), &moved(&list.shapes[shape as usize + 1], off), color, clip),
                Item::Stroke { at: from, n, width, color } => {
                    if off == (0, 0) {
                        canvas.stroke(&list.points[from as usize..(from + n) as usize], width, color, clip);
                    } else {
                        let pts: Vec<(f32, f32)> = list.points[from as usize..(from + n) as usize].iter().map(|&(x, y)| (x + tx as f32, y + ty as f32)).collect();
                        canvas.stroke(&pts, width, color, clip);
                    }
                }
                Item::Poly { at: from, n, color } => canvas.fill_poly_at(&list.points[from as usize..(from + n) as usize], off, color, clip),
                Item::PushLayer { r, alpha } => {
                    let r = at(r).intersect(clip).intersect(bounds);
                    layers.push((r, alpha, saved.len()));
                    if !r.is_empty() {
                        canvas.save(r, saved);
                    }
                }
                Item::PopLayer => {
                    let (r, alpha, from) = layers.pop().expect("a layer is open");
                    if !r.is_empty() {
                        canvas.restore(r, &saved[from..], alpha);
                    }
                    saved.truncate(from);
                }
                Item::PushClip(r) => clips.push(at(r).intersect(clip)),
                Item::PopClip => {
                    clips.pop();
                }
                Item::Text { x, baseline, size, color, at: from, len, bold } => {
                    text(canvas, glyphs, clip, x + tx as f32, baseline + ty, size, color, bold, &list.text[from as usize..(from + len) as usize]);
                }
                Item::PushScroll(node) => {
                    offsets.push(off);
                    if let Some(v) = view {
                        let (sx, sy) = v.scroll.of(node);
                        off = (off.0 - sx.round() as i32, off.1 - sy.round() as i32);
                    }
                }
                Item::PopScroll | Item::PopSticky => off = offsets.pop().expect("a scroll container or a sticky box is open"),
                Item::PushSticky(k) => {
                    offsets.push(off);
                    if let Some(v) = view {
                        let (own, _) = v.areas.stuck(k as usize, (v.scroll.x, v.scroll.y), &|n| v.scroll.of(n));
                        off = (off.0 + own.0.round() as i32, off.1 + own.1.round() as i32);
                    }
                }
                Item::PushFixed => {
                    // A fixed box is cut by the window only, and does not move.
                    offsets.push(off);
                    off = (0, 0);
                    clips.push(bounds);
                }
                Item::PopFixed => {
                    clips.pop();
                    off = offsets.pop().expect("a fixed box is open");
                }
                Item::Bars { node, r } => {
                    if let Some(a) = view.and_then(|v| v.areas.get(node).map(|a| (v, a))) {
                        let (v, a) = a;
                        let (client, (sx, sy)) = (a.client(), v.scroll.of(node));
                        let r = at(r);
                        // The corner, when both bars show.
                        if a.bars.0 && a.bars.1 {
                            canvas.fill(IRect { x0: r.x1 - bars::WIDTH as i32, y0: r.y1 - bars::WIDTH as i32, ..r }.intersect(clip), 0xfffc_fcfc);
                        }
                        if a.bars.0 {
                            let strip = IRect { x0: r.x1 - bars::WIDTH as i32, y1: r.y1 - if a.bars.1 { bars::WIDTH as i32 } else { 0 }, ..r };
                            bars::draw(canvas, clip, strip, true, client.1, a.content.1, sy);
                        }
                        if a.bars.1 {
                            let strip = IRect { y0: r.y1 - bars::WIDTH as i32, x1: r.x1 - if a.bars.0 { bars::WIDTH as i32 } else { 0 }, ..r };
                            bars::draw(canvas, clip, strip, false, client.0, a.content.0, sx);
                        }
                    }
                }
                Item::Later(_) => unreachable!("`Later` stays in the scratch lists"),
            }
        }
        if let Some(v) = view {
            let (w, h) = (canvas.width as i32, canvas.height as i32);
            let all = canvas.bounds();
            let bar = bars::WIDTH as i32;
            if v.bars.0 && v.bars.1 {
                canvas.fill(IRect { x0: w - bar, y0: h - bar, x1: w, y1: h }, 0xfffc_fcfc);
            }
            if v.bars.0 {
                let strip = IRect { x0: w - bar, y0: 0, x1: w, y1: h - if v.bars.1 { bar } else { 0 } };
                bars::draw(canvas, all, strip, true, (h - if v.bars.1 { bar } else { 0 }) as f32, v.areas.page.1.max((h - if v.bars.1 { bar } else { 0 }) as f32), v.scroll.y);
            }
            if v.bars.1 {
                let strip = IRect { x0: 0, y0: h - bar, x1: w - if v.bars.0 { bar } else { 0 }, y1: h };
                bars::draw(canvas, all, strip, false, (w - if v.bars.0 { bar } else { 0 }) as f32, v.areas.page.0.max((w - if v.bars.0 { bar } else { 0 }) as f32), v.scroll.x);
            }
        }
    }
}

/// `s` moved by `(dx, dy)` whole pixels.
fn moved(s: &canvas::Shape, (dx, dy): (i32, i32)) -> canvas::Shape {
    canvas::Shape { r: IRect { x0: s.r.x0 + dx, y0: s.r.y0 + dy, x1: s.r.x1 + dx, y1: s.r.y1 + dy }, rad: s.rad }
}

/// Draw one run of text. The letters are `font::width_of` apart, as layout placed them: whole units of a tenth of an
/// em, added up, so a line of 1,000 letters ends where layout says.
#[allow(clippy::too_many_arguments)]
pub fn text(canvas: &mut Canvas, glyphs: &mut Glyphs, clip: IRect, x: f32, baseline: i32, size: f32, color: u32, bold: bool, run: &str) {
    // Not even the tallest letter reaches the clip: skip the run.
    if (baseline as f32 + size * 0.4) < clip.y0 as f32 || (baseline as f32 - size * 1.2) > clip.y1 as f32 {
        return;
    }
    let px = size / 10.0;
    let mut units: Units = 0;
    for c in run.chars() {
        // A newline or a tab that was kept (`white-space: pre`) is not a letter, and has no width here.
        if c < ' ' {
            continue;
        }
        let at = x + units as f32 * px;
        units += font::width_of(c);
        if at > clip.x1 as f32 {
            break;
        }
        if c == ' ' {
            continue;
        }
        let (face, id) = glyphs.glyph(bold, c);
        if id == 0 {
            continue;
        }
        let whole = at.floor();
        let sub = ((at - whole) * SUBPIXELS) as u8;
        let e = glyphs.get(face, id, size, sub.min(SUBPIXELS as u8 - 1));
        if e.w > 0 {
            canvas.mask((whole as i32 + e.left as i32, baseline + e.top as i32), (e.w as usize, e.h as usize), glyphs.mask(e), color, clip);
        }
    }
}

/// Paint a page into a new canvas of `width` by `height` pixels, with a new `Painter`. For one picture: a test, a
/// capture. A window keeps its `Painter` and its `Canvas`.
pub fn capture(doc: &Document, styles: &Styles, layout: &Layout, width: u32, height: u32) -> Canvas {
    let mut canvas = Canvas::new(width, height);
    Painter::new().paint(doc, styles, layout, &mut canvas);
    canvas
}
