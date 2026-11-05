//! Paint: draw the fragments of layout into a pixel buffer, and write the buffer as a PNG.
//!
//! The way in is `Painter::paint` (or `capture` for one picture). It makes a display list from the fragments
//! (`display.rs`), then draws the list into a `Canvas` (`canvas.rs`): solid rectangles for backgrounds and borders, and
//! glyphs from Noto Sans Mono (`glyphs.rs`) for text, at the places and with the widths that layout used. The picture is
//! a plain `Vec<u32>`, so a window (S4 topic 2) only has to show it. `png.rs` is the capture for tests and for
//! `bare --paint`.
//!
//! The design, the measurements behind the crates, and the gaps are in `docs/resources.md`, "Paint and pixel capture".

pub mod canvas;
pub mod display;
pub mod glyphs;
pub mod hit;
pub mod png;

use bare_css::cascade::Styles;
use bare_html::dom::Document;
use bare_layout::font::{self, Units};
use bare_layout::fragment::Layout;
use canvas::{Canvas, IRect};
use display::{DisplayList, Item};
use glyphs::{Glyphs, SUBPIXELS};

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
}

impl Painter {
    pub fn new() -> Painter {
        Painter::default()
    }

    /// Paint a page into `canvas`. The whole canvas is drawn again: no part of the old picture stays.
    pub fn paint(&mut self, doc: &Document, styles: &Styles, layout: &Layout, canvas: &mut Canvas) {
        self.list.build(doc, styles, layout, (canvas.width, canvas.height));
        self.draw(canvas);
    }

    /// Draw the display list that `paint` made.
    pub fn draw(&mut self, canvas: &mut Canvas) {
        let Painter { glyphs, list, clips, layers, saved } = self;
        canvas.pixels.fill(list.background);
        clips.clear();
        layers.clear();
        saved.clear();
        clips.push(canvas.bounds());
        for &item in &list.items {
            let clip = *clips.last().expect("the first clip is the canvas");
            match item {
                Item::Rect { r, color } => canvas.fill(r.intersect(clip), color),
                Item::Fill { shape, color } => canvas.fill_shape(&list.shapes[shape as usize], color, clip),
                Item::Ring { shape, color } => canvas.fill_ring(&list.shapes[shape as usize], &list.shapes[shape as usize + 1], color, clip),
                Item::Stroke { at, n, width, color } => canvas.stroke(&list.points[at as usize..(at + n) as usize], width, color, clip),
                Item::PushLayer { r, alpha } => {
                    let r = r.intersect(clip).intersect(canvas.bounds());
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
                Item::PushClip(r) => clips.push(r.intersect(clip)),
                Item::PopClip => {
                    clips.pop();
                }
                Item::Text { x, baseline, size, color, at, len, bold } => {
                    text(canvas, glyphs, clip, x, baseline, size, color, bold, &list.text[at as usize..(at + len) as usize]);
                }
                Item::Later(_) => unreachable!("`Later` stays in the scratch lists"),
            }
        }
    }
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
