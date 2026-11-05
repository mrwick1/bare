//! The pixel buffer. `decisions.md` ("Platforms") says paint draws into a plain `Vec<u32>`, so a window port only has to
//! show the buffer. One pixel is `0xAARRGGBB`: the order that Wayland shared memory (`ARGB8888`) and Win32 expect on a
//! little-endian machine. Every pixel of a canvas is opaque: the page starts on a solid color.
//!
//! No part here allocates for each pixel, each rectangle or each glyph. A rectangle is a `fill` of whole rows of the
//! buffer, and a glyph is a loop over its mask.

/// A rectangle of whole pixels. `x1` and `y1` are the first pixel past the edge, so the width is `x1 - x0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl IRect {
    pub fn intersect(self, o: IRect) -> IRect {
        IRect { x0: self.x0.max(o.x0), y0: self.y0.max(o.y0), x1: self.x1.min(o.x1), y1: self.y1.min(o.y1) }
    }

    pub fn is_empty(self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }
}

pub struct Canvas {
    pub width: u32,
    pub height: u32,
    /// `width * height` pixels, row by row from the top.
    pub pixels: Vec<u32>,
}

/// `x / 255`, rounded, for `x` up to 255 * 255. It is exact: a test checks all the values.
#[inline]
fn div255(x: u32) -> u32 {
    ((x + 128) * 257) >> 16
}

/// `src` over `dst` for one channel, with `a` as the alpha of `src` (1 to 254).
#[inline]
fn mix(src: u32, dst: u32, a: u32) -> u32 {
    div255(src * a + dst * (255 - a))
}

/// `color` (`0xAARRGGBB`, not premultiplied) over the opaque pixel `d`.
#[inline]
pub fn blend(color: u32, d: u32, a: u32) -> u32 {
    let r = mix(color >> 16 & 255, d >> 16 & 255, a);
    let g = mix(color >> 8 & 255, d >> 8 & 255, a);
    let b = mix(color & 255, d & 255, a);
    0xff00_0000 | r << 16 | g << 8 | b
}

impl Canvas {
    /// A white canvas, the color of a page with no background.
    pub fn new(width: u32, height: u32) -> Canvas {
        Canvas { width, height, pixels: vec![0xffff_ffff; width as usize * height as usize] }
    }

    pub fn bounds(&self) -> IRect {
        IRect { x0: 0, y0: 0, x1: self.width as i32, y1: self.height as i32 }
    }

    /// Set the size and fill with `color`. The buffer is kept when the size does not change: no new allocation for a frame.
    pub fn reset(&mut self, width: u32, height: u32, color: u32) {
        self.width = width;
        self.height = height;
        self.pixels.clear();
        self.pixels.resize(width as usize * height as usize, color);
    }

    pub fn pixel(&self, x: u32, y: u32) -> u32 {
        self.pixels[y as usize * self.width as usize + x as usize]
    }

    /// Fill `r` (already inside the canvas) with `color`. A color with alpha below 255 mixes with what is there.
    pub fn fill(&mut self, r: IRect, color: u32) {
        let a = color >> 24;
        if r.is_empty() || a == 0 {
            return;
        }
        let w = self.width as usize;
        for y in r.y0..r.y1 {
            let row = &mut self.pixels[y as usize * w + r.x0 as usize..y as usize * w + r.x1 as usize];
            if a == 255 {
                row.fill(color);
            } else {
                for p in row {
                    *p = blend(color, *p, a);
                }
            }
        }
    }

    /// Draw a mask of `w` by `h` coverage bytes with its top-left corner at `(x, y)`, in `color`, inside `clip`.
    pub fn mask(&mut self, (x, y): (i32, i32), (w, h): (usize, usize), mask: &[u8], color: u32, clip: IRect) {
        let r = IRect { x0: x, y0: y, x1: x + w as i32, y1: y + h as i32 }.intersect(clip).intersect(self.bounds());
        if r.is_empty() {
            return;
        }
        let ca = color >> 24;
        let stride = self.width as usize;
        let skip = (r.x0 - x) as usize;
        let n = (r.x1 - r.x0) as usize;
        for row in r.y0..r.y1 {
            let m = &mask[(row - y) as usize * w + skip..][..n];
            let dst = &mut self.pixels[row as usize * stride + r.x0 as usize..][..n];
            for (p, &cov) in dst.iter_mut().zip(m) {
                if cov == 0 {
                    continue;
                }
                let a = if ca == 255 { cov as u32 } else { div255(cov as u32 * ca) };
                *p = if a == 255 { color | 0xff00_0000 } else if a == 0 { *p } else { blend(color, *p, a) };
            }
        }
    }

    /// FNV-1a over the pixels: a short name for a picture, for tests.
    pub fn hash(&self) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for &p in &self.pixels {
            h = (h ^ p as u64).wrapping_mul(0x100_0000_01b3);
        }
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn div255_is_exact() {
        for x in 0..=255 * 255u32 {
            assert_eq!(div255(x), (x as f64 / 255.0).round() as u32, "{x}");
        }
    }

    #[test]
    fn fill_opaque_and_translucent() {
        let mut c = Canvas::new(4, 2);
        c.fill(IRect { x0: 1, y0: 0, x1: 3, y1: 1 }, 0xff00_00ff);
        assert_eq!(c.pixel(0, 0), 0xffff_ffff);
        assert_eq!(c.pixel(1, 0), 0xff00_00ff);
        assert_eq!(c.pixel(2, 0), 0xff00_00ff);
        assert_eq!(c.pixel(3, 0), 0xffff_ffff);
        assert_eq!(c.pixel(1, 1), 0xffff_ffff);
        // Alpha 128 of black over white: 255 * 127 / 255 = 127.
        c.fill(IRect { x0: 0, y0: 1, x1: 1, y1: 2 }, 0x8000_0000);
        assert_eq!(c.pixel(0, 1), 0xff7f_7f7f);
    }

    #[test]
    fn mask_clips_and_blends() {
        let mut c = Canvas::new(3, 3);
        let m = [255, 128, 0, 255];
        // A 2 by 2 mask at (-1, 1): only its right column is inside the canvas on x, and both rows on y.
        c.mask((-1, 1), (2, 2), &m, 0xff00_0000, c.bounds());
        assert_eq!(c.pixel(0, 1), 0xff7f_7f7f);
        assert_eq!(c.pixel(0, 2), 0xff00_0000);
        assert_eq!(c.pixel(1, 1), 0xffff_ffff);
        // A clip rectangle limits the mask too.
        let mut c = Canvas::new(3, 3);
        c.mask((0, 0), (2, 2), &[255; 4], 0xff00_0000, IRect { x0: 1, y0: 0, x1: 3, y1: 1 });
        assert_eq!(c.pixel(0, 0), 0xffff_ffff);
        assert_eq!(c.pixel(1, 0), 0xff00_0000);
        assert_eq!(c.pixel(1, 1), 0xffff_ffff);
    }
}
