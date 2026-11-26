//! The raster of a canvas 2D context (`docs/internal/design-canvas-2d.md`, section 6).
//!
//! - A canvas records its draws in a [`Record`]. It rasters them only when something reads the pixels ([`Record::flush`]). A canvas
//!   that nobody reads never allocates pixels.
//! - Pixels are premultiplied, one `u32` each as `0xAARRGGBB`: RGBA8 premultiplied in the byte order of the page painter.
//! - A fill is a scanline rasterizer with an active edge list. Each pixel row has [`SUB`] sub-scanlines, or one sample line for each
//!   band when all its edges are vertical (exact coverage). Each line gives exact horizontal coverage, kept as a difference row, so a
//!   long span costs two writes. Nonzero and evenodd.
//! - An image is a fill of its destination with an image source, sampled at each pixel center through the inverse transform: nearest,
//!   or bilinear when smoothing is on. So transforms and coverage work the same for shapes and pictures.

use bare_html::dom::Pixels;
use std::sync::Arc;

/// Sub-scanlines per pixel row.
const SUB: usize = 16;

/// When the ops of a record hold more bytes than this or than the bitmap (`4 * w * h`), the next draw rasters them into the base. The
/// raster cost does not change: it comes earlier. So the ops of a canvas that draws a lot and is never read never hold more memory than
/// its pixels would.
const MIN_OP_BYTES: usize = 64 << 10;

/// Chrome's limits of a canvas: 32,767 pixels per side and 268,435,456 pixels in total (hypothesis on the exact numbers). A larger
/// canvas has no bitmap: it draws nothing and reads as blank.
pub const MAX_SIDE: u32 = 32_767;
pub const MAX_AREA: u64 = 268_435_456;

/// Is a canvas of `w` by `h` inside the limits?
pub fn fits(w: u32, h: u32) -> bool {
    w <= MAX_SIDE && h <= MAX_SIDE && w as u64 * h as u64 <= MAX_AREA
}

/// An affine transform `[a, b, c, d, e, f]`: `x' = a x + c y + e`, `y' = b x + d y + f` (the order of `setTransform`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine(pub [f64; 6]);

impl Affine {
    pub const IDENTITY: Affine = Affine([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + c * y + e, b * x + d * y + f)
    }

    /// `self` after `o`: the point goes through `o` first.
    pub fn then(&self, o: &Affine) -> Affine {
        let [a, b, c, d, e, f] = o.0;
        let [p, q, r, s, t, u] = self.0;
        Affine([p * a + r * b, q * a + s * b, p * c + r * d, q * c + s * d, p * e + r * f + t, q * e + s * f + u])
    }

    /// The inverse, or `None` when the transform is singular.
    pub fn invert(&self) -> Option<Affine> {
        let [a, b, c, d, e, f] = self.0;
        let det = a * d - b * c;
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        let (ia, ib, ic, id) = (d / det, -b / det, -c / det, a / det);
        Some(Affine([ia, ib, ic, id, -(ia * e + ic * f), -(ib * e + id * f)]))
    }
}

/// The pixels of a picture source.
#[derive(Clone)]
pub enum Px {
    /// A decoded picture: `0xAARRGGBB` not premultiplied (the bitmap of an `<img>`).
    Straight(Pixels),
    /// Premultiplied pixels (a canvas, an `ImageBitmap`).
    Premul(Arc<Vec<u32>>),
}

/// A picture to draw: its size and pixels.
#[derive(Clone)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub px: Px,
}

impl Image {
    /// The premultiplied pixel at index `i`.
    #[inline]
    fn at(px: &[u32], straight: bool, i: usize) -> u32 {
        if straight { premul(px[i]) } else { px[i] }
    }
}

/// What a fill paints with.
#[derive(Clone)]
pub enum Source {
    /// A premultiplied color.
    Solid(u32),
    /// A picture. `inv` maps a canvas point to a picture point. `clamp` is the source rectangle `(x0, y0, x1, y1)` in whole pixels: a
    /// sample never reads outside it.
    Image { img: Image, inv: Affine, clamp: (u32, u32, u32, u32), smooth: bool },
}

/// How a draw mixes with the pixels under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    SourceOver,
    /// `clearRect`: the covered pixels go to transparent black, in proportion to the coverage.
    Clear,
}

/// One recorded draw. It holds all that the raster needs.
#[derive(Clone)]
pub enum Draw {
    /// Fill the polygons `path` (canvas pixels, contours apart by a NaN point, each closed) with `src`, times `alpha` (0 to 1).
    Fill { path: Vec<(f32, f32)>, evenodd: bool, src: Source, alpha: f32, mode: Mode },
    /// `putImageData`: replace the pixels of the rectangle at `(x, y)` with `px` (premultiplied, `w * h`). No blend, no transform.
    Put { x: i32, y: i32, w: u32, h: u32, px: Vec<u32> },
}

impl Draw {
    fn bytes(&self) -> usize {
        std::mem::size_of::<Draw>()
            + match self {
                Draw::Fill { path, .. } => path.len() * 8,
                Draw::Put { px, .. } => px.len() * 4,
            }
    }

    /// Does this draw replace every pixel of a `w` by `h` canvas with something that does not depend on the old pixels?
    fn covers_all(&self, w: u32, h: u32) -> bool {
        let Draw::Fill { path, src, alpha, mode, .. } = self else {
            return matches!(self, Draw::Put { x: 0, y: 0, w: pw, h: ph, .. } if *pw == w && *ph == h);
        };
        let opaque = match (mode, src) {
            (Mode::Clear, _) => true,
            (Mode::SourceOver, Source::Solid(c)) => *alpha >= 1.0 && c >> 24 == 255,
            _ => false,
        };
        opaque && is_rect_over(path, w, h)
    }
}

/// Is `path` one axis-aligned rectangle that holds the whole `w` by `h` canvas?
fn is_rect_over(path: &[(f32, f32)], w: u32, h: u32) -> bool {
    if path.len() != 4 {
        return false;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(x, y) in path {
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    let on_box = path.iter().all(|&(x, y)| (x == x0 || x == x1) && (y == y0 || y == y1));
    on_box && x0 <= 0.0 && y0 <= 0.0 && x1 >= w as f32 && y1 >= h as f32
}

/// The draws of one canvas and the raster of the draws before them.
pub struct Record {
    pub w: u32,
    pub h: u32,
    ops: Vec<Draw>,
    bytes: usize,
    /// The pixels of the ops that were played. `None`: transparent black, no allocation.
    base: Option<Arc<Vec<u32>>>,
    /// How many times ops were played (tests and the cost rows check the lazy design with it).
    pub rasters: u64,
}

impl Record {
    pub fn new(w: u32, h: u32) -> Record {
        Record { w, h, ops: Vec::new(), bytes: 0, base: None, rasters: 0 }
    }

    /// A new size clears the bitmap (HTML: setting `width` or `height`).
    pub fn resize(&mut self, w: u32, h: u32) {
        *self = Record { rasters: self.rasters, ..Record::new(w, h) };
    }

    /// Is the bitmap blank with nothing recorded?
    pub fn is_blank(&self) -> bool {
        self.ops.is_empty() && self.base.is_none()
    }

    /// Record a draw. A draw that hides all old pixels drops the ops before it.
    pub fn push(&mut self, d: Draw) {
        if !fits(self.w, self.h) || self.w == 0 || self.h == 0 {
            return;
        }
        if d.covers_all(self.w, self.h) {
            self.ops.clear();
            self.bytes = 0;
            self.base = None;
            if matches!(d, Draw::Fill { mode: Mode::Clear, .. }) {
                return;
            }
        } else if self.bytes > MIN_OP_BYTES.max(self.w as usize * self.h as usize * 4) {
            self.flush();
        }
        self.bytes += d.bytes();
        self.ops.push(d);
    }

    /// Play the ops into the base and give it. `None`: the bitmap is blank (or too big to allocate, as Chrome gives a blank canvas).
    pub fn flush(&mut self) -> Option<&Arc<Vec<u32>>> {
        if !self.ops.is_empty() {
            let ops = std::mem::take(&mut self.ops);
            self.bytes = 0;
            let n = self.w as usize * self.h as usize;
            if self.base.is_none() {
                let mut v = Vec::new();
                if v.try_reserve_exact(n).is_err() {
                    return None;
                }
                v.resize(n, 0);
                self.base = Some(Arc::new(v));
            }
            let px = Arc::make_mut(self.base.as_mut().expect("made"));
            let mut sc = Scratch::default();
            for d in &ops {
                play(&mut sc, px, self.w, self.h, d);
            }
            self.rasters += 1;
        }
        self.base.as_ref()
    }

    /// The bitmap as a picture source (the premultiplied pixels, shared: a later draw on this canvas copies them first).
    pub fn image(&mut self) -> Image {
        let (w, h) = (self.w, self.h);
        let px = self.flush().cloned().unwrap_or_else(|| Arc::new(vec![0; w as usize * h as usize]));
        Image { w, h, px: Px::Premul(px) }
    }

    /// The bitmap as `0xAARRGGBB` not premultiplied, the input of paint.
    pub fn straight(&mut self) -> Vec<u32> {
        let n = self.w as usize * self.h as usize;
        match self.flush() {
            Some(px) => px.iter().map(|&p| unpremul(p)).collect(),
            None => vec![0; n],
        }
    }

    /// The RGBA bytes, not premultiplied, of the rectangle at `(x, y)` of `w` by `h`. Pixels outside the canvas are transparent black.
    pub fn rgba(&mut self, x: i64, y: i64, w: u32, h: u32) -> Vec<u8> {
        let (cw, ch) = (self.w as i64, self.h as i64);
        let mut out = vec![0u8; w as usize * h as usize * 4];
        let Some(px) = self.flush() else { return out };
        for row in 0..h as i64 {
            let sy = y + row;
            if !(0..ch).contains(&sy) {
                continue;
            }
            let (c0, c1) = ((x.max(0)).min(cw), (x + w as i64).clamp(0, cw));
            for sx in c0..c1 {
                let p = unpremul(px[(sy * cw + sx) as usize]);
                let o = ((row * w as i64 + sx - x) * 4) as usize;
                out[o..o + 4].copy_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8]);
            }
        }
        out
    }
}

/// A premultiplied pixel of a `0xAARRGGBB` pixel that is not.
#[inline]
pub fn premul(p: u32) -> u32 {
    let a = p >> 24;
    match a {
        255 => p,
        0 => 0,
        _ => a << 24 | mul255(p >> 16 & 255, a) << 16 | mul255(p >> 8 & 255, a) << 8 | mul255(p & 255, a),
    }
}

/// The `0xAARRGGBB` pixel, not premultiplied, of a premultiplied one.
#[inline]
pub fn unpremul(p: u32) -> u32 {
    let a = p >> 24;
    match a {
        255 => p,
        0 => 0,
        _ => {
            let ch = |c: u32| ((c * 255 + a / 2) / a).min(255);
            a << 24 | ch(p >> 16 & 255) << 16 | ch(p >> 8 & 255) << 8 | ch(p & 255)
        }
    }
}

/// `x * y / 255`, rounded.
#[inline]
fn mul255(x: u32, y: u32) -> u32 {
    let t = x * y + 128;
    (t + (t >> 8)) >> 8
}

/// Each channel of `p` times `k / 255`.
#[inline]
fn scale(p: u32, k: u32) -> u32 {
    if k == 255 {
        return p;
    }
    mul255(p >> 24, k) << 24 | mul255(p >> 16 & 255, k) << 16 | mul255(p >> 8 & 255, k) << 8 | mul255(p & 255, k)
}

/// Premultiplied `s` over `d`.
#[inline]
fn over(s: u32, d: u32) -> u32 {
    match s >> 24 {
        255 => s,
        0 => d,
        a => s + scale(d, 255 - a),
    }
}

fn play(sc: &mut Scratch, px: &mut [u32], w: u32, h: u32, d: &Draw) {
    match d {
        Draw::Fill { path, evenodd, src, alpha, mode } => fill(sc, px, w, h, path, *evenodd, src, *alpha, *mode),
        Draw::Put { x, y, w: pw, h: ph, px: from } => {
            for row in 0..*ph as i64 {
                let ty = *y as i64 + row;
                if !(0..h as i64).contains(&ty) {
                    continue;
                }
                let (c0, c1) = ((*x as i64).max(0), (*x as i64 + *pw as i64).min(w as i64));
                if c0 >= c1 {
                    continue;
                }
                let s = (row * *pw as i64 + c0 - *x as i64) as usize;
                let t = (ty * w as i64 + c0) as usize;
                let n = (c1 - c0) as usize;
                px[t..t + n].copy_from_slice(&from[s..s + n]);
            }
        }
    }
}

/// Fill `path` into `px` with `src`. See the module notes.
#[allow(clippy::too_many_arguments)]
fn fill(sc: &mut Scratch, px: &mut [u32], w: u32, h: u32, path: &[(f32, f32)], evenodd: bool, src: &Source, alpha: f32, mode: Mode) {
    let alpha = alpha.clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return;
    }
    // Picture pixels that are not premultiplied become so at each sample.
    let (img_px, straight): (&[u32], bool) = match src {
        Source::Image { img: Image { px: Px::Straight(p), .. }, .. } => (p, true),
        Source::Image { img: Image { px: Px::Premul(p), .. }, .. } => (p, false),
        Source::Solid(_) => (&[], false),
    };
    if let Source::Image { img, .. } = src
        && img_px.len() != img.w as usize * img.h as usize
    {
        return;
    }
    raster(sc, path, evenodd, w, h, |y, x0, cov| {
        let row = &mut px[y as usize * w as usize..(y as usize + 1) * w as usize];
        for (i, &c) in cov.iter().enumerate() {
            if c <= 0.0 {
                continue;
            }
            // Truncated, as Skia does: half coverage is 127.
            let k = ((c.min(1.0) * alpha) * 255.0 + 1e-3) as u32;
            if k == 0 {
                continue;
            }
            let x = x0 as usize + i;
            let d = &mut row[x];
            if mode == Mode::Clear {
                *d = scale(*d, 255 - k);
                continue;
            }
            let s = match src {
                Source::Solid(c) if k == 255 && c >> 24 == 255 => {
                    *d = *c;
                    continue;
                }
                Source::Solid(c) => *c,
                Source::Image { img, inv, clamp, smooth } => sample(img, img_px, straight, inv, *clamp, *smooth, x as f64 + 0.5, y as f64 + 0.5),
            };
            *d = over(scale(s, k), *d);
        }
    });
}

/// The premultiplied color of the picture at the canvas point `(x, y)`.
#[allow(clippy::too_many_arguments)]
#[inline]
fn sample(img: &Image, px: &[u32], straight: bool, inv: &Affine, (x0, y0, x1, y1): (u32, u32, u32, u32), smooth: bool, x: f64, y: f64) -> u32 {
    let (u, v) = inv.apply(x, y);
    let iw = img.w as usize;
    let cl = |t: i64, lo: u32, hi: u32| t.clamp(lo as i64, hi as i64 - 1) as usize;
    if !smooth {
        // A point on a pixel border takes the pixel before it, as Chrome does (a source row at exactly 1.0 gives row 0).
        const BIAS: f64 = 1.0 / 65536.0;
        return Image::at(px, straight, cl((v - BIAS).floor() as i64, y0, y1) * iw + cl((u - BIAS).floor() as i64, x0, x1));
    }
    let (u, v) = (u - 0.5, v - 0.5);
    let (fu, fv) = (u.floor(), v.floor());
    let (wx, wy) = (((u - fu) * 256.0) as u32, ((v - fv) * 256.0) as u32);
    let (ua, ub) = (cl(fu as i64, x0, x1), cl(fu as i64 + 1, x0, x1));
    let (va, vb) = (cl(fv as i64, y0, y1), cl(fv as i64 + 1, y0, y1));
    let p = [Image::at(px, straight, va * iw + ua), Image::at(px, straight, va * iw + ub), Image::at(px, straight, vb * iw + ua), Image::at(px, straight, vb * iw + ub)];
    if p[0] == p[1] && p[0] == p[2] && p[0] == p[3] {
        return p[0];
    }
    let k = [(256 - wx) * (256 - wy), wx * (256 - wy), (256 - wx) * wy, wx * wy];
    let mut out = 0;
    for sh in [0, 8, 16, 24] {
        let sum: u32 = (0..4).map(|i| (p[i] >> sh & 255) * k[i]).sum();
        out |= (sum >> 16).min(255) << sh;
    }
    out
}

/// One edge, top to bottom: the top point, the bottom y, the x step per y, and the direction (+1 down, -1 up).
#[derive(Clone, Copy)]
struct Edge {
    x0: f32,
    y0: f32,
    y1: f32,
    dxdy: f32,
    dir: i32,
}

/// The buffers of [`raster`], kept for all the draws of one flush: no allocation for each draw.
#[derive(Default)]
struct Scratch {
    edges: Vec<Edge>,
    active: Vec<Edge>,
    cross: Vec<(f32, i32)>,
    /// The sample lines of one pixel row: their y and their weight (the height they stand for).
    lines: Vec<(f32, f32)>,
    cov: Vec<f32>,
    delta: Vec<f32>,
}

/// Scan `path` and call `span(y, x0, coverage)` for each pixel row that it touches: the coverage (0 to 1) of the pixels from `x0`.
/// A row where every active edge is vertical needs no sub-scanlines: its coverage is exact from one sample line for each band between
/// the edge ends in the row (a rectangle, a `drawImage` with no rotation). Another row takes [`SUB`] sub-scanlines.
fn raster(sc: &mut Scratch, path: &[(f32, f32)], evenodd: bool, w: u32, h: u32, mut span: impl FnMut(u32, u32, &[f32])) {
    let Scratch { edges, active, cross, lines, cov, delta } = sc;
    edges.clear();
    active.clear();
    for c in path.split(|p| p.0.is_nan() || p.1.is_nan()) {
        for (i, &(ax, ay)) in c.iter().enumerate() {
            let (bx, by) = c[(i + 1) % c.len()];
            if ay == by || !(ax.is_finite() && ay.is_finite() && bx.is_finite() && by.is_finite()) {
                continue;
            }
            let (t, b, dir) = if ay < by { ((ax, ay), (bx, by), 1) } else { ((bx, by), (ax, ay), -1) };
            edges.push(Edge { x0: t.0, y0: t.1, y1: b.1, dxdy: (b.0 - t.0) / (b.1 - t.1), dir });
        }
    }
    if edges.is_empty() {
        return;
    }
    edges.sort_unstable_by(|a, b| a.y0.total_cmp(&b.y0));
    let top = edges[0].y0.floor().max(0.0) as i64;
    let bottom = edges.iter().fold(f32::MIN, |m, e| m.max(e.y1)).ceil().min(h as f32) as i64;
    let wf = w as f32;
    cov.clear();
    cov.resize(w as usize + 1, 0.0);
    delta.clear();
    delta.resize(w as usize + 2, 0.0);
    let mut next = 0;
    let step = 1.0 / SUB as f32;
    for y in top..bottom {
        let yf = y as f32;
        while next < edges.len() && edges[next].y0 < yf + 1.0 {
            active.push(edges[next]);
            next += 1;
        }
        active.retain(|e| e.y1 > yf);
        if active.is_empty() {
            continue;
        }
        lines.clear();
        if active.iter().all(|e| e.dxdy == 0.0) {
            // The band edges: the row ends and every edge end inside the row. Each band is sampled once at its middle.
            lines.push((yf, 0.0));
            lines.extend(active.iter().flat_map(|e| [e.y0, e.y1]).filter(|&v| v > yf && v < yf + 1.0).map(|v| (v, 0.0)));
            lines.push((yf + 1.0, 0.0));
            lines.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
            for i in 0..lines.len() - 1 {
                let (a, b) = (lines[i].0, lines[i + 1].0);
                lines[i] = ((a + b) / 2.0, b - a);
            }
            lines.pop();
        } else {
            lines.extend((0..SUB).map(|k| (yf + (k as f32 + 0.5) * step, step)));
        }
        let (mut lo, mut hi) = (usize::MAX, 0usize);
        for &(sy, wt) in lines.iter() {
            if wt <= 0.0 {
                continue;
            }
            cross.clear();
            cross.extend(active.iter().filter(|e| e.y0 <= sy && sy < e.y1).map(|e| (e.x0 + (sy - e.y0) * e.dxdy, e.dir)));
            if cross.len() < 2 {
                continue;
            }
            cross.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
            let mut wind = 0;
            for i in 0..cross.len() - 1 {
                wind += cross[i].1;
                let inside = if evenodd { wind & 1 != 0 } else { wind != 0 };
                if !inside {
                    continue;
                }
                let (a, b) = (cross[i].0.clamp(0.0, wf), cross[i + 1].0.clamp(0.0, wf));
                if b <= a {
                    continue;
                }
                let (ia, ib) = (a as usize, b as usize);
                lo = lo.min(ia);
                hi = hi.max(ib + 1);
                if ia == ib {
                    cov[ia] += (b - a) * wt;
                } else {
                    cov[ia] += (ia as f32 + 1.0 - a) * wt;
                    delta[ia + 1] += wt;
                    delta[ib] -= wt;
                    cov[ib] += (b - ib as f32) * wt;
                }
            }
        }
        if lo >= hi {
            continue;
        }
        let hi = hi.min(w as usize);
        let mut run = 0.0;
        for x in lo..hi {
            run += delta[x];
            delta[x] = 0.0;
            cov[x] += run;
        }
        delta[hi] = 0.0;
        delta[hi + 1] = 0.0;
        span(y as u32, lo as u32, &cov[lo..hi]);
        cov[lo..=hi].fill(0.0);
    }
}
