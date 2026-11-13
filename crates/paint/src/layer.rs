//! Images of a box: `background-image` and `mask-image`.
//!
//! The builder (`display.rs`) turns the style of a box into a `Layer`: the picture, the size of one tile, where the first tile starts and
//! whether it repeats along each axis. `Canvas::draw_layer` draws it. A layer with a `color` is a mask: the color shows through the alpha
//! of the picture. A layer with no color draws the picture.
//!
//! The picture is scaled to the size of the tile once. `Scaled` keeps the result by (picture, size), so a frame that draws the same icon
//! again scales nothing. Scaling averages the source pixels that a pixel covers (alpha weighted), so a large icon that is made small
//! keeps its edges soft. The cache holds at most `MAX_BYTES`; it is emptied when it is full.

use crate::canvas::{Canvas, IRect, blend};
use bare_css::cascade::Pictures;
use bare_css::values::{Keyword, Length, Property, Value};
use bare_html::dom::Bitmap;
use std::collections::HashMap;
use std::sync::Arc;

/// The largest tile, in pixels (4 bytes each when scaled). A larger one is not drawn.
pub const MAX_TILE: u64 = 4 << 20;
/// What the cache of scaled pictures holds, at most, in bytes. The largest tile fits twice.
const MAX_BYTES: usize = 32 << 20;
/// The number of pictures that the cache holds, at most.
const MAX_ENTRIES: usize = 256;

/// One image of a box.
#[derive(Debug, Clone)]
pub struct Layer {
    pub bitmap: Bitmap,
    /// The size of one tile, in pixels.
    pub tile: (u32, u32),
    /// Where the tile with index (0, 0) starts, in page pixels.
    pub origin: (i32, i32),
    /// Does the picture repeat along x, and along y?
    pub repeat: (bool, bool),
    /// 0: draw the picture. Else `0xAARRGGBB`: draw this color where the picture has alpha.
    pub color: u32,
}

/// The pictures that were scaled, by (the pixels of the picture, width, height).
#[derive(Default)]
pub struct Scaled {
    map: HashMap<(usize, u32, u32), (bare_html::dom::Pixels, Arc<[u32]>)>,
    bytes: usize,
    /// How many pictures were scaled (not found in `map`): a test reads it.
    pub made: usize,
}

impl Scaled {
    /// The pixels of `bitmap` at `w` by `h` (`0xAARRGGBB`, not premultiplied). The pictures of the same size are not copied.
    pub fn get(&mut self, bitmap: &Bitmap, w: u32, h: u32) -> Arc<[u32]> {
        if (w, h) == (bitmap.0, bitmap.1) {
            return bitmap.2.shared();
        }
        let key = (bitmap.2.addr(), w, h);
        if let Some((_, px)) = self.map.get(&key) {
            return px.clone();
        }
        let px: Arc<[u32]> = scale(&bitmap.2, bitmap.0, bitmap.1, w, h).into();
        if self.map.len() >= MAX_ENTRIES || self.bytes + px.len() * 4 > MAX_BYTES {
            self.map.clear();
            self.bytes = 0;
        }
        self.bytes += px.len() * 4;
        self.made += 1;
        // The map holds the source too: the pointer in the key stays the address of this picture and no other.
        self.map.insert(key, (bitmap.2.clone(), px.clone()));
        px
    }
}

/// `src` (`sw` by `sh`) at `dw` by `dh`: each pixel is the average of the source pixels that it covers, weighted by their alpha.
fn scale(src: &[u32], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u32> {
    let mut out = Vec::with_capacity(dw as usize * dh as usize);
    let span = |i: u32, d: u32, s: u32| {
        let from = (u64::from(i) * u64::from(s) / u64::from(d)) as u32;
        let to = ((u64::from(i) + 1) * u64::from(s)).div_ceil(u64::from(d)) as u32;
        (from.min(s - 1), to.clamp(from + 1, s))
    };
    for y in 0..dh {
        let (y0, y1) = span(y, dh, sh);
        for x in 0..dw {
            let (x0, x1) = span(x, dw, sw);
            let (mut a, mut r, mut g, mut b) = (0u64, 0u64, 0u64, 0u64);
            for sy in y0..y1 {
                for &p in &src[(sy * sw + x0) as usize..(sy * sw + x1) as usize] {
                    let pa = u64::from(p >> 24);
                    a += pa;
                    r += pa * u64::from(p >> 16 & 255);
                    g += pa * u64::from(p >> 8 & 255);
                    b += pa * u64::from(p & 255);
                }
            }
            let n = u64::from((y1 - y0) * (x1 - x0));
            out.push(if a == 0 { 0 } else { ((a / n) as u32) << 24 | ((r / a) as u32) << 16 | ((g / a) as u32) << 8 | (b / a) as u32 });
        }
    }
    out
}

/// The properties of a kind of layer.
#[derive(Clone, Copy)]
pub struct Props {
    pub size: [Property; 2],
    pub position: [Property; 2],
    pub repeat: Property,
}

pub const BACKGROUND: Props = Props {
    size: [Property::BackgroundSizeX, Property::BackgroundSizeY],
    position: [Property::BackgroundPositionX, Property::BackgroundPositionY],
    repeat: Property::BackgroundRepeat,
};

pub const MASK: Props = Props {
    size: [Property::MaskSizeX, Property::MaskSizeY],
    position: [Property::MaskPositionX, Property::MaskPositionY],
    repeat: Property::MaskRepeat,
};

/// The size of the tile, the start of the first tile and the repeat of a picture of `image` pixels in `area` (the positioning area), from
/// the style. `None` when the tile is empty or too large to draw.
pub fn geometry(set: &Pictures, p: Props, image: (u32, u32), area: IRect) -> Option<((u32, u32), (i32, i32), (bool, bool))> {
    let (iw, ih) = (image.0 as f32, image.1 as f32);
    let (aw, ah) = ((area.x1 - area.x0) as f32, (area.y1 - area.y0) as f32);
    if iw < 1.0 || ih < 1.0 {
        return None;
    }
    // A length against `base`; `None` for `auto`.
    let len = |v: Value, base: f32| match v {
        Value::Length(Length::Px(x)) => Some(x),
        Value::Length(Length::Percent(x)) => Some(x * base / 100.0),
        Value::Length(Length::Calc(i)) => {
            let (px, pct) = bare_css::cascade::picture_calc(i);
            Some(px + pct * base / 100.0)
        }
        _ => None,
    };
    let get = |p: Property| set[p as usize - Property::CORE];
    let (sx, sy) = (get(p.size[0]), get(p.size[1]));
    let (w, h) = match sx {
        Value::Keyword(k @ (Keyword::Cover | Keyword::Contain)) => {
            let (kx, ky) = (aw / iw, ah / ih);
            let s = if k == Keyword::Cover { kx.max(ky) } else { kx.min(ky) };
            (iw * s, ih * s)
        }
        _ => match (len(sx, aw), len(sy, ah)) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, w * ih / iw),
            (None, Some(h)) => (h * iw / ih, h),
            (None, None) => (iw, ih),
        },
    };
    let (tw, th) = (w.round(), h.round());
    if !(tw >= 1.0 && th >= 1.0) || (tw as u64) * (th as u64) > MAX_TILE || tw > 65536.0 || th > 65536.0 {
        return None;
    }
    let at = |v: Value, free: f32| len(v, free).unwrap_or(0.0).round() as i32;
    let origin = (area.x0 + at(get(p.position[0]), aw - tw), area.y0 + at(get(p.position[1]), ah - th));
    let repeat = match get(p.repeat) {
        Value::Keyword(Keyword::NoRepeat) => (false, false),
        Value::Keyword(Keyword::RepeatX) => (true, false),
        Value::Keyword(Keyword::RepeatY) => (false, true),
        _ => (true, true),
    };
    Some(((tw as u32, th as u32), origin, repeat))
}

impl Canvas<'_> {
    /// Draw `layer` inside `r` and `clip`, moved by `off`. Not inline: the loop that draws a display list is hot, and a page with no
    /// background image or mask never gets here.
    #[inline(never)]
    pub fn draw_layer(&mut self, r: IRect, clip: IRect, off: (i32, i32), l: &Layer, scaled: &mut Scaled) {
        let vis = r.intersect(clip).intersect(self.bounds());
        let (tw, th) = (l.tile.0 as i32, l.tile.1 as i32);
        let (ox, oy) = (l.origin.0 + off.0, l.origin.1 + off.1);
        // Without a repeat, the picture is the one tile.
        let xs = if l.repeat.0 { (vis.x0, vis.x1) } else { (vis.x0.max(ox), vis.x1.min(ox + tw)) };
        let ys = if l.repeat.1 { (vis.y0, vis.y1) } else { (vis.y0.max(oy), vis.y1.min(oy + th)) };
        if vis.is_empty() || xs.0 >= xs.1 || ys.0 >= ys.1 {
            return;
        }
        let px = scaled.get(&l.bitmap, l.tile.0, l.tile.1);
        if px.len() != tw as usize * th as usize {
            return;
        }
        let w = self.width as usize;
        let ca = l.color >> 24;
        // The tile column of the first pixel; it goes up by one for each pixel and wraps.
        let tx0 = (xs.0 - ox).rem_euclid(tw);
        for y in ys.0..ys.1 {
            let row = &px[((y - oy).rem_euclid(th) * tw) as usize..][..tw as usize];
            let dst = &mut self.pixels[y as usize * w + xs.0 as usize..y as usize * w + xs.1 as usize];
            let mut tx = tx0 as usize;
            for d in dst {
                let p = row[tx];
                tx += 1;
                if tx == tw as usize {
                    tx = 0;
                }
                let (src, a) = if l.color == 0 { (p, p >> 24) } else { (l.color, ((p >> 24) * ca + 127) / 255) };
                *d = match a {
                    255 => src | 0xff00_0000,
                    0 => *d,
                    a => blend(src, *d, a),
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitmap(w: u32, h: u32, px: u32) -> Bitmap {
        (w, h, vec![px; (w * h) as usize].into())
    }

    #[test]
    fn scaling_averages_and_a_cache_hit_scales_nothing() {
        // 2 by 1: one opaque red pixel and one clear pixel. At 1 by 1 the color stays red and the alpha is half.
        let b: Bitmap = (2, 1, vec![0xffff_0000, 0].into());
        assert_eq!(scale(&b.2, 2, 1, 1, 1), [0x7fff_0000]);
        assert_eq!(scale(&b.2, 2, 1, 4, 1), [0xffff_0000, 0xffff_0000, 0, 0]);
        let mut cache = Scaled::default();
        let first = cache.get(&b, 4, 1);
        let again = cache.get(&b, 4, 1);
        assert_eq!((cache.made, Arc::ptr_eq(&first, &again)), (1, true));
        // The natural size is the picture itself.
        assert!(Arc::ptr_eq(&cache.get(&b, 2, 1), &b.2.shared()));
        assert_eq!(cache.made, 1);
    }

    #[test]
    fn tiles_repeat_and_a_mask_shows_the_color() {
        let mut cache = Scaled::default();
        let mut c = Canvas::new(8, 4);
        let l = Layer { bitmap: bitmap(2, 2, 0xff00_ff00), tile: (2, 2), origin: (1, 1), repeat: (true, false), color: 0 };
        c.draw_layer(IRect { x0: 0, y0: 0, x1: 8, y1: 4 }, c.bounds(), (0, 0), &l, &mut cache);
        // Repeats along x in both directions from x = 1, and stays on rows 1 and 2.
        assert_eq!([c.pixel(0, 1), c.pixel(1, 1), c.pixel(7, 2), c.pixel(1, 0), c.pixel(1, 3)], [0xff00_ff00, 0xff00_ff00, 0xff00_ff00, 0xffff_ffff, 0xffff_ffff]);
        // A mask: the color where the picture has alpha, nothing where it has none.
        let mut c = Canvas::new(4, 1);
        let l = Layer { bitmap: (2, 1, vec![0xff00_0000, 0x0000_0000].into()), tile: (2, 1), origin: (0, 0), repeat: (false, false), color: 0xffff_0000 };
        c.draw_layer(IRect { x0: 0, y0: 0, x1: 4, y1: 1 }, c.bounds(), (0, 0), &l, &mut cache);
        assert_eq!([c.pixel(0, 0), c.pixel(1, 0), c.pixel(2, 0)], [0xffff_0000, 0xffff_ffff, 0xffff_ffff]);
    }
}
