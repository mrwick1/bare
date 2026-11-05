//! Scrollbars, in the classic look of Chrome on Linux (no overlay): a 15 px track of `#fcfcfc`, an arrow button at each end,
//! and a thumb with round ends. The numbers come from pixels of headless Chrome 15x scroll positions (`docs/resources.md`,
//! "Scrolling"): the arrows are triangles of 5 rows and 9 px, `#8b8b8b`, and the thumb is `#8b8b8b`, 9 px thick, 3 px in from
//! the track edge and from the buttons. A bar draws nothing for the part of the track that a button or a thumb covers
//! twice. Gaps: no hover or pressed colors, no dragging, no button repeat.

use crate::canvas::{Canvas, IRect, Shape};

pub use bare_layout::block::SCROLLBAR as WIDTH;

const TRACK: u32 = 0xfffc_fcfc;
const THUMB: u32 = 0xff8b_8b8b;
const BUTTON: i32 = 15;
const INSET: i32 = 3;

/// The place of the thumb along a track that starts at `from` and is `len` long, in whole pixels: its start and its end.
/// `client` is what shows, `content` the whole, `offset` how far it is scrolled. A thumb is never shorter than 20 px.
pub fn thumb(len: i32, client: f32, content: f32, offset: f32) -> (i32, i32) {
    let inner = (len - 2 * BUTTON - 2 * INSET).max(0);
    let t = if content > 0.0 { (inner as f32 * client / content).round() as i32 } else { inner };
    let t = t.clamp(20.min(inner), inner);
    let max = content - client;
    let at = if max > 0.0 { ((inner - t) as f32 * (offset / max).clamp(0.0, 1.0)).floor() as i32 } else { 0 };
    let start = BUTTON + INSET + at;
    (start, start + t)
}

/// Where along a bar of `len` pixels `pos` is: 0 the up or left button, 1 the track before the thumb, 2 the thumb, 3 the track
/// after it, 4 the down or right button. For a click.
pub fn part(len: i32, client: f32, content: f32, offset: f32, pos: i32) -> u8 {
    let (a, b) = thumb(len, client, content, offset);
    match pos {
        p if p < BUTTON => 0,
        p if p >= len - BUTTON => 4,
        p if p < a => 1,
        p if p < b => 2,
        _ => 3,
    }
}

/// Draw one bar into `r` (15 px across). `vertical`: a bar on the right edge. `clip` cuts all of it.
pub fn draw(canvas: &mut Canvas, clip: IRect, r: IRect, vertical: bool, client: f32, content: f32, offset: f32) {
    // Work along the bar (`a`) and across it (`b`) and turn the two into x and y at the end.
    let (len, across0) = if vertical { (r.y1 - r.y0, r.x0) } else { (r.x1 - r.x0, r.y0) };
    let rect = |a0: i32, a1: i32, b0: i32, b1: i32| {
        let (a0, a1) = (a0 + if vertical { r.y0 } else { r.x0 }, a1 + if vertical { r.y0 } else { r.x0 });
        if vertical { IRect { x0: across0 + b0, y0: a0, x1: across0 + b1, y1: a1 } } else { IRect { x0: a0, y0: across0 + b0, x1: a1, y1: across0 + b1 } }
    };
    canvas.fill(rect(0, len, 0, BUTTON).intersect(clip), TRACK);
    if len >= 2 * BUTTON + 2 * INSET + 20 {
        // The arrows: five rows of 1, 3, 5, 7 and 9 px, centered 7 px from the edge of the track.
        for i in 0..5 {
            canvas.fill(rect(6 + i, 7 + i, 7 - i, 8 + i).intersect(clip), THUMB);
            canvas.fill(rect(len - BUTTON + 3 + i, len - BUTTON + 4 + i, 3 + i, 12 - i).intersect(clip), THUMB);
        }
        let (a, b) = thumb(len, client, content, offset);
        let shape = Shape::new(rect(a, b, INSET, BUTTON - INSET), [4.5; 8]);
        canvas.fill_shape(&shape, THUMB, clip);
    }
}
