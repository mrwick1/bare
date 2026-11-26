//! What the shell draws over the page: the status line, the hint labels, the box of the current find match, the `-- INSERT --` mark and
//! the list of pages. The page viewport does not shrink: this paints over the frame of the page, after it.
//!
//! The text is drawn with the same call as the fields of `page.rs` (`bare_paint::text`), in Noto Sans, on a solid background.
//!
//! Gaps: the text has no wrapping and no scroll: a long command line is cut at the right edge of the window (the caret can leave the
//! window then). A page list longer than the window is cut at the bottom.

use bare_layout::font::{self, Face, Metrics};
use bare_paint::canvas::{Canvas, IRect};
use bare_paint::glyphs::Glyphs;

const SIZE: f32 = 14.0;
const HINT_SIZE: f32 = 12.0;
const BAR_BG: u32 = 0xFF22_2222;
const BAR_FG: u32 = 0xFFF2_F2F2;
const HINT_BG: u32 = 0xFFFF_E066;
const HINT_FG: u32 = 0xFF1A_1A1A;
const FIND_EDGE: u32 = 0xFFFF_8800;

/// One label: the part still to type, and the top left of the element in window pixels.
pub struct Label {
    pub text: String,
    pub x: f32,
    pub y: f32,
}

/// Everything that can be on screen over the page.
#[derive(Default)]
pub struct Overlay {
    /// The line at the bottom: its text and the caret (a char index; `None`: no caret).
    pub line: Option<(String, Option<usize>)>,
    pub labels: Vec<Label>,
    pub find: Option<[f32; 4]>,
    pub insert: bool,
    pub pages: Vec<String>,
}

impl Overlay {
    pub fn is_empty(&self) -> bool {
        self.line.is_none() && self.labels.is_empty() && self.find.is_none() && !self.insert && self.pages.is_empty()
    }
}

fn width(m: &Metrics, text: &str) -> f32 {
    m.px(font::width(m.face, text))
}

fn put(canvas: &mut Canvas, glyphs: &mut Glyphs, clip: IRect, x: f32, top: i32, m: &Metrics, color: u32, text: &str) {
    bare_paint::text(canvas, glyphs, clip, x, top + m.ascent as i32, m.size, color, m.face, (0.0, 0.0), text);
}

fn outline(canvas: &mut Canvas, r: IRect, t: i32, color: u32) {
    for e in [IRect { x1: r.x1, y1: r.y0 + t, ..r }, IRect { y0: r.y1 - t, ..r }, IRect { x1: r.x0 + t, ..r }, IRect { x0: r.x1 - t, ..r }] {
        canvas.fill(e.intersect(r), color);
    }
}

pub fn draw(canvas: &mut Canvas, glyphs: &mut Glyphs, o: &Overlay) {
    let (w, h) = (canvas.width as i32, canvas.height as i32);
    let all = IRect { x0: 0, y0: 0, x1: w, y1: h };
    let m = Metrics::new(SIZE, Face::Sans);
    let row = m.normal_line_height().ceil() as i32 + 4;
    if let Some(f) = o.find {
        let r = IRect { x0: f[0].floor() as i32 - 1, y0: f[1].floor() as i32 - 1, x1: (f[0] + f[2]).ceil() as i32 + 1, y1: (f[1] + f[3]).ceil() as i32 + 1 };
        if r.intersect(all) == r {
            outline(canvas, r, 2, FIND_EDGE);
        }
    }
    let hm = Metrics::new(HINT_SIZE, Face::SansBold);
    for l in &o.labels {
        let (bw, bh) = (width(&hm, &l.text).ceil() as i32 + 6, hm.normal_line_height().ceil() as i32 + 2);
        let (x, y) = ((l.x as i32).clamp(0, (w - bw).max(0)), (l.y as i32).clamp(0, (h - bh).max(0)));
        let r = IRect { x0: x, y0: y, x1: x + bw, y1: y + bh };
        canvas.fill(r.intersect(all), HINT_BG);
        outline(canvas, r.intersect(all), 1, HINT_FG);
        put(canvas, glyphs, r.intersect(all), x as f32 + 3.0, y + 1, &hm, HINT_FG, &l.text);
    }
    if !o.pages.is_empty() {
        let pw = o.pages.iter().map(|p| width(&m, p)).fold(0.0, f32::max).ceil() as i32 + 16;
        let ph = row * o.pages.len() as i32 + 8;
        let r = IRect { x0: 0, y0: 0, x1: pw.min(w), y1: ph.min(h) };
        canvas.fill(r, BAR_BG);
        for (i, p) in o.pages.iter().enumerate() {
            put(canvas, glyphs, r, 8.0, 6 + row * i as i32, &m, BAR_FG, p);
        }
    }
    if o.insert && o.line.is_none() {
        let t = "-- INSERT --";
        let bw = width(&m, t).ceil() as i32 + 12;
        let r = IRect { x0: (w - bw).max(0), y0: (h - row).max(0), x1: w, y1: h };
        canvas.fill(r, BAR_BG);
        put(canvas, glyphs, r, r.x0 as f32 + 6.0, r.y0 + 2, &m, BAR_FG, t);
    }
    if let Some((text, caret)) = &o.line {
        let r = IRect { x0: 0, y0: (h - row).max(0), x1: w, y1: h };
        canvas.fill(r, BAR_BG);
        put(canvas, glyphs, r, 6.0, r.y0 + 2, &m, BAR_FG, text);
        if let Some(c) = caret {
            let prefix: String = text.chars().take(*c).collect();
            let x = 6 + width(&m, &prefix).round() as i32;
            canvas.fill(IRect { x0: x, y0: r.y0 + 2, x1: x + 1, y1: r.y1 - 2 }.intersect(r), BAR_FG);
        }
    }
}
