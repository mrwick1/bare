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

/// A rectangle with round corners: the box `r` and the radius of each corner, `[tl.x, tl.y, tr.x, tr.y, br.x, br.y, bl.x,
/// bl.y]`. A corner is a quarter of an ellipse. The radii must fit: no two on one side add up to more than the side
/// (`Shape::new` does that, as CSS Backgrounds 3, 5.5).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shape {
    pub r: IRect,
    pub rad: [f32; 8],
}

impl Shape {
    /// A shape with the radii cut so that they fit in `r`.
    pub fn new(r: IRect, mut rad: [f32; 8]) -> Shape {
        let (w, h) = ((r.x1 - r.x0) as f32, (r.y1 - r.y0) as f32);
        // The side of each pair: (the two radii on the top, along x), the right (along y), the bottom, the left.
        let sums = [(0, 2, w), (3, 5, h), (6, 4, w), (1, 7, h)];
        let mut f = 1.0f32;
        for (a, b, side) in sums {
            let sum = rad[a] + rad[b];
            if sum > side {
                f = f.min(side / sum);
            }
        }
        if f < 1.0 {
            rad.iter_mut().for_each(|v| *v *= f);
        }
        Shape { r, rad }
    }

    pub fn is_square(&self) -> bool {
        self.rad.iter().all(|&v| v <= 0.0)
    }

    /// The shape inside the borders `t`, `r`, `b`, `l`: the box shrinks, and a radius loses the width of the border next to it.
    pub fn inset(&self, t: i32, r: i32, b: i32, l: i32) -> Shape {
        let q = &self.rad;
        let (lw, rw, tw, bw) = (l as f32, r as f32, t as f32, b as f32);
        let rad = [(q[0] - lw).max(0.0), (q[1] - tw).max(0.0), (q[2] - rw).max(0.0), (q[3] - tw).max(0.0), (q[4] - rw).max(0.0), (q[5] - bw).max(0.0), (q[6] - lw).max(0.0), (q[7] - bw).max(0.0)];
        Shape { r: IRect { x0: self.r.x0 + l, y0: self.r.y0 + t, x1: self.r.x1 - r, y1: self.r.y1 - b }, rad }
    }

    /// How much of the pixel `(x, y)` is inside, 0 to 1. The edge of a corner is anti-aliased with the distance of the
    /// pixel center to the curve (exact for a circle, close for an ellipse).
    fn cov(&self, x: i32, y: i32) -> f32 {
        let r = self.r;
        if x < r.x0 || x >= r.x1 || y < r.y0 || y >= r.y1 {
            return 0.0;
        }
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        // (rx, ry, corner x, corner y, direction x, direction y)
        let q = &self.rad;
        let corners = [
            (q[0], q[1], r.x0 as f32 + q[0], r.y0 as f32 + q[1], -1.0, -1.0),
            (q[2], q[3], r.x1 as f32 - q[2], r.y0 as f32 + q[3], 1.0, -1.0),
            (q[4], q[5], r.x1 as f32 - q[4], r.y1 as f32 - q[5], 1.0, 1.0),
            (q[6], q[7], r.x0 as f32 + q[6], r.y1 as f32 - q[7], -1.0, 1.0),
        ];
        for (rx, ry, cx, cy, sx, sy) in corners {
            let (dx, dy) = ((px - cx) * sx, (py - cy) * sy);
            if rx > 0.0 && ry > 0.0 && dx > 0.0 && dy > 0.0 {
                let n = (dx / rx).hypot(dy / ry);
                return ((1.0 - n) * rx.min(ry) + 0.5).clamp(0.0, 1.0);
            }
        }
        1.0
    }

    /// The pixels left and right of row `y` where the coverage may be less than 1: the corner boxes that the row is in.
    fn zone(&self, y: i32) -> (i32, i32) {
        let (r, q) = (self.r, &self.rad);
        let (top, bottom) = ((y - r.y0) as f32 + 0.5, (r.y1 - y) as f32 - 0.5);
        let l = if top < q[1] { q[0] } else if bottom < q[7] { q[6] } else { 0.0 };
        let rr = if top < q[3] { q[2] } else if bottom < q[5] { q[4] } else { 0.0 };
        (l.ceil() as i32, rr.ceil() as i32)
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

    /// Mix `color` into one pixel with `cov` (0 to 1) of its alpha.
    #[inline]
    fn plot(&mut self, x: i32, y: i32, color: u32, cov: f32) {
        let a = ((color >> 24) as f32 * cov + 0.5) as u32;
        if a == 0 {
            return;
        }
        let p = &mut self.pixels[y as usize * self.width as usize + x as usize];
        *p = if a >= 255 { color | 0xff00_0000 } else { blend(color, *p, a) };
    }

    /// Fill a shape with round corners with `color`, inside `clip`. The rows between the corners are plain `fill`s.
    pub fn fill_shape(&mut self, s: &Shape, color: u32, clip: IRect) {
        let vis = s.r.intersect(clip).intersect(self.bounds());
        if vis.is_empty() || color >> 24 == 0 {
            return;
        }
        for y in vis.y0..vis.y1 {
            let (l, r) = s.zone(y);
            let (xl, xr) = ((s.r.x0 + l).min(s.r.x1), (s.r.x1 - r).max(s.r.x0));
            self.fill(IRect { x0: xl.max(vis.x0), y0: y, x1: xr.min(vis.x1), y1: y + 1 }, color);
            for x in (vis.x0..xl.min(vis.x1)).chain(xr.max(vis.x0)..vis.x1) {
                self.plot(x, y, color, s.cov(x, y));
            }
        }
    }

    /// Fill what is in `outer` and not in `inner`: a border with round corners. Only the pixels that can be partly
    /// covered are looked at: the strips of the border and the corners. The rest is a plain fill, or nothing.
    pub fn fill_ring(&mut self, outer: &Shape, inner: &Shape, color: u32, clip: IRect) {
        let vis = outer.r.intersect(clip).intersect(self.bounds());
        if vis.is_empty() || color >> 24 == 0 {
            return;
        }
        let hollow = inner.r.is_empty();
        for y in vis.y0..vis.y1 {
            let (ol, or) = outer.zone(y);
            let in_row = !hollow && y >= inner.r.y0 && y < inner.r.y1;
            // Left and right of the middle that is evaluated pixel by pixel: the border strip, and the corner boxes.
            let (mut l, mut r) = (ol, or);
            if in_row {
                let (il, ir) = inner.zone(y);
                l = l.max(inner.r.x0 - outer.r.x0 + il);
                r = r.max(outer.r.x1 - inner.r.x1 + ir);
            }
            let (xl, xr) = ((outer.r.x0 + l).min(outer.r.x1), (outer.r.x1 - r).max(outer.r.x0));
            if xl >= xr {
                for x in vis.x0..vis.x1 {
                    self.plot(x, y, color, outer.cov(x, y) - if in_row { inner.cov(x, y) } else { 0.0 });
                }
                continue;
            }
            if !in_row {
                self.fill(IRect { x0: xl.max(vis.x0), y0: y, x1: xr.min(vis.x1), y1: y + 1 }, color);
            }
            for x in (vis.x0..xl.min(vis.x1)).chain(xr.max(vis.x0)..vis.x1) {
                self.plot(x, y, color, (outer.cov(x, y) - if in_row { inner.cov(x, y) } else { 0.0 }).max(0.0));
            }
        }
    }

    /// A line of `width` pixels through the points, with round joins and caps, anti-aliased: the check mark and the arrow
    /// of a control. Only the pixels near the line are looked at.
    pub fn stroke(&mut self, points: &[(f32, f32)], width: f32, color: u32, clip: IRect) {
        let Some(&(fx, fy)) = points.first() else { return };
        let (mut x0, mut y0, mut x1, mut y1) = (fx, fy, fx, fy);
        for &(x, y) in points {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        let pad = width / 2.0 + 1.0;
        let r = IRect { x0: (x0 - pad).floor() as i32, y0: (y0 - pad).floor() as i32, x1: (x1 + pad).ceil() as i32, y1: (y1 + pad).ceil() as i32 };
        let vis = r.intersect(clip).intersect(self.bounds());
        for y in vis.y0..vis.y1 {
            for x in vis.x0..vis.x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                // One point is a disc.
                let mut d = if points.len() == 1 { (px - fx).hypot(py - fy) } else { f32::MAX };
                for w in points.windows(2) {
                    let ((ax, ay), (bx, by)) = (w[0], w[1]);
                    let (vx, vy) = (bx - ax, by - ay);
                    let t = (((px - ax) * vx + (py - ay) * vy) / (vx * vx + vy * vy).max(1e-6)).clamp(0.0, 1.0);
                    d = d.min((px - (ax + t * vx)).hypot(py - (ay + t * vy)));
                }
                self.plot(x, y, color, (width / 2.0 + 0.5 - d).clamp(0.0, 1.0));
            }
        }
    }

    /// `fill_poly` of the points moved by `(tx, ty)`. Not inline: the loop that draws a display list is hot, and a page with no `<svg>` never gets here.
    #[inline(never)]
    pub fn fill_poly_at(&mut self, pts: &[(f32, f32)], (tx, ty): (i32, i32), color: u32, clip: IRect) {
        let pts: Vec<(f32, f32)> = pts.iter().map(|&(x, y)| (x + tx as f32, y + ty as f32)).collect();
        self.fill_poly(&pts, color, clip);
    }

    /// Fill polygons, anti-aliased, with the non-zero rule. `pts` has the contours apart by a NaN point; each is closed. Each row is
    /// looked at on `SUB` lines: a line gives exact horizontal coverage, so the coverage of a pixel is exact to 1/`SUB` high.
    pub fn fill_poly(&mut self, pts: &[(f32, f32)], color: u32, clip: IRect) {
        const SUB: usize = 8;
        // The edges (top to bottom) with their direction, and the box of the polygons.
        let mut edges: Vec<(f32, f32, f32, f32, i32)> = Vec::new();
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for c in pts.split(|p| p.0.is_nan()) {
            for (i, &(ax, ay)) in c.iter().enumerate() {
                let (bx, by) = c[(i + 1) % c.len()];
                (x0, y0, x1, y1) = (x0.min(ax), y0.min(ay), x1.max(ax), y1.max(ay));
                if ay != by {
                    edges.push(if ay < by { (ax, ay, bx, by, 1) } else { (bx, by, ax, ay, -1) });
                }
            }
        }
        let vis = IRect { x0: x0.floor() as i32, y0: y0.floor() as i32, x1: x1.ceil() as i32, y1: y1.ceil() as i32 }.intersect(clip).intersect(self.bounds());
        if vis.is_empty() || color >> 24 == 0 {
            return;
        }
        let mut cov = vec![0.0f32; (vis.x1 - vis.x0) as usize];
        let (mut cross, mut near): (Vec<(f32, i32)>, Vec<&(f32, f32, f32, f32, i32)>) = (Vec::new(), Vec::new());
        for y in vis.y0..vis.y1 {
            near.clear();
            near.extend(edges.iter().filter(|e| e.1 < (y + 1) as f32 && e.3 > y as f32));
            if near.is_empty() {
                continue;
            }
            cov.fill(0.0);
            for k in 0..SUB {
                let sy = y as f32 + (k as f32 + 0.5) / SUB as f32;
                cross.clear();
                cross.extend(near.iter().filter(|e| e.1 <= sy && sy < e.3).map(|e| (e.0 + (sy - e.1) * (e.2 - e.0) / (e.3 - e.1), e.4)));
                cross.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut wind = 0;
                for w in 0..cross.len() {
                    wind += cross[w].1;
                    if wind == 0 || w + 1 == cross.len() {
                        continue;
                    }
                    // The span from this crossing to the next is inside.
                    let (a, b) = ((cross[w].0 - vis.x0 as f32).clamp(0.0, cov.len() as f32), (cross[w + 1].0 - vis.x0 as f32).clamp(0.0, cov.len() as f32));
                    let (ia, ib) = (a as usize, b as usize);
                    if ia == ib {
                        let last = cov.len() - 1;
                        cov[ia.min(last)] += (b - a) / SUB as f32;
                    } else {
                        cov[ia] += ((ia + 1) as f32 - a) / SUB as f32;
                        for c in &mut cov[ia + 1..ib] {
                            *c += 1.0 / SUB as f32;
                        }
                        if ib < cov.len() {
                            cov[ib] += (b - ib as f32) / SUB as f32;
                        }
                    }
                }
            }
            for (i, &c) in cov.iter().enumerate() {
                if c > 0.0 {
                    self.plot(vis.x0 + i as i32, y, color, c.min(1.0));
                }
            }
        }
    }

    /// Add the pixels of `r` (already inside the canvas) to `into`, row by row: the backdrop of a layer.
    pub fn save(&self, r: IRect, into: &mut Vec<u32>) {
        let w = self.width as usize;
        for y in r.y0..r.y1 {
            into.extend_from_slice(&self.pixels[y as usize * w + r.x0 as usize..y as usize * w + r.x1 as usize]);
        }
    }

    /// The end of a layer: each pixel of `r` is the pixel now, with `alpha` of its strength, over the pixel that `save` kept.
    pub fn restore(&mut self, r: IRect, saved: &[u32], alpha: u8) {
        let w = self.width as usize;
        let n = (r.x1 - r.x0) as usize;
        for (k, y) in (r.y0..r.y1).enumerate() {
            let row = &mut self.pixels[y as usize * w + r.x0 as usize..][..n];
            for (p, &s) in row.iter_mut().zip(&saved[k * n..][..n]) {
                if *p != s {
                    *p = blend(*p, s, alpha as u32);
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

    #[test]
    fn a_square_shape_is_a_fill_and_a_round_one_is_symmetric() {
        let r = IRect { x0: 1, y0: 1, x1: 11, y1: 9 };
        let (mut a, mut b) = (Canvas::new(12, 10), Canvas::new(12, 10));
        a.fill(r, 0xff12_3456);
        b.fill_shape(&Shape::new(r, [0.0; 8]), 0xff12_3456, b.bounds());
        assert_eq!(a.pixels, b.pixels);
        // A round shape: the four corners are the same, the middle is solid, and the corner pixel is not.
        let mut c = Canvas::new(12, 10);
        c.fill_shape(&Shape::new(r, [4.0; 8]), 0xff00_0000, c.bounds());
        assert_eq!(c.pixel(1, 1), 0xffff_ffff);
        assert_eq!(c.pixel(10, 1), 0xffff_ffff);
        assert_eq!(c.pixel(1, 8), 0xffff_ffff);
        assert_eq!(c.pixel(10, 8), 0xffff_ffff);
        assert_eq!(c.pixel(5, 1), 0xff00_0000);
        assert!(c.pixel(1, 4) & 255 < 0x20, "the edge of the arc is almost solid");
        for (x, y) in [(2, 1), (3, 2), (1, 3)] {
            assert_eq!(c.pixel(x, y), c.pixel(11 - x, y), "left and right");
            assert_eq!(c.pixel(x, y), c.pixel(x, 9 - y), "top and bottom");
        }
    }

    #[test]
    fn radii_are_cut_to_fit_the_box() {
        // Two radii of 100 on a side of 40: both go down to 20 (CSS Backgrounds 3, 5.5).
        let s = Shape::new(IRect { x0: 0, y0: 0, x1: 40, y1: 20 }, [100.0; 8]);
        assert!(s.rad.iter().all(|&v| v == 10.0), "{:?}", s.rad);
        assert!(Shape::new(IRect { x0: 0, y0: 0, x1: 5, y1: 5 }, [0.0; 8]).is_square());
        // The border takes from the radius: an inner shape of a 4 px border has a radius 4 less, and not below 0.
        let inner = Shape::new(IRect { x0: 0, y0: 0, x1: 40, y1: 40 }, [6.0; 8]).inset(4, 4, 4, 4);
        assert_eq!((inner.rad[0], inner.r.x0, inner.r.x1), (2.0, 4, 36));
        assert_eq!(Shape::new(IRect { x0: 0, y0: 0, x1: 40, y1: 40 }, [3.0; 8]).inset(4, 4, 4, 4).rad[0], 0.0);
    }

    #[test]
    fn a_ring_is_the_outer_shape_without_the_inner() {
        let outer = Shape::new(IRect { x0: 0, y0: 0, x1: 20, y1: 20 }, [8.0; 8]);
        let mut c = Canvas::new(20, 20);
        c.fill_ring(&outer, &outer.inset(2, 2, 2, 2), 0xff00_0000, c.bounds());
        assert_eq!(c.pixel(10, 0), 0xff00_0000);
        assert_eq!(c.pixel(10, 1), 0xff00_0000);
        assert_eq!(c.pixel(10, 2), 0xffff_ffff, "the inside is not touched");
        assert_eq!(c.pixel(0, 10), 0xff00_0000);
        assert_eq!(c.pixel(10, 10), 0xffff_ffff);
        assert_eq!(c.pixel(0, 0), 0xffff_ffff);
        // The same as the difference of two fills, for every pixel (the shortcut for the strips must not change the result).
        let (mut a, mut b) = (Canvas::new(20, 20), Canvas::new(20, 20));
        a.fill_shape(&outer, 0xff00_0000, a.bounds());
        a.fill_shape(&outer.inset(2, 2, 2, 2), 0xffff_ffff, a.bounds());
        b.fill_ring(&outer, &outer.inset(2, 2, 2, 2), 0xff00_0000, b.bounds());
        let worst = a.pixels.iter().zip(&b.pixels).map(|(&p, &q)| ((p & 255) as i32 - (q & 255) as i32).abs()).max().unwrap();
        assert!(worst <= 40, "{worst}");
    }

    #[test]
    fn a_stroke_covers_the_line_and_a_point_is_a_dot() {
        let mut c = Canvas::new(20, 10);
        c.stroke(&[(2.0, 5.0), (18.0, 5.0)], 2.0, 0xff00_0000, c.bounds());
        assert_eq!(c.pixel(10, 4), 0xff00_0000);
        assert_eq!(c.pixel(10, 5), 0xff00_0000);
        assert_eq!(c.pixel(10, 2), 0xffff_ffff);
        let mut d = Canvas::new(10, 10);
        d.stroke(&[(5.0, 5.0)], 6.0, 0xff00_0000, d.bounds());
        assert_eq!(d.pixel(5, 5), 0xff00_0000);
        assert_eq!(d.pixel(0, 0), 0xffff_ffff);
    }

    #[test]
    fn a_layer_mixes_what_was_drawn_with_what_was_behind() {
        let mut c = Canvas::new(4, 2);
        let r = c.bounds();
        let mut kept = Vec::new();
        c.save(r, &mut kept);
        c.fill(r, 0xff00_0000);
        c.restore(r, &kept, 128);
        // 128 of 255 of black over white: 255 * 127 / 255 = 127.
        assert!(c.pixels.iter().all(|&p| p == 0xff7f_7f7f));
        // A pixel that did not change stays as it is, with no rounding.
        let mut d = Canvas::new(2, 1);
        d.pixels[0] = 0xff12_3456;
        let mut kept = Vec::new();
        d.save(d.bounds(), &mut kept);
        d.restore(d.bounds(), &kept, 77);
        assert_eq!(d.pixels[0], 0xff12_3456);
    }
}
