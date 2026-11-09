//! The drawing of an inline `<svg>` (SVG 2): `path`, `line`, `polyline`, `polygon`, `rect`, `circle`, `ellipse` and `g`, with
//! `fill` and `stroke` (colors, from the cascade) and `stroke-width`, `stroke-linecap`, `stroke-linejoin` and
//! `stroke-miterlimit` (attributes, taken from the shape or the nearest element above it). The `viewBox` maps to the box of the
//! element with `preserveAspectRatio` at its default (`xMidYMid meet`).
//!
//! A shape becomes polygons in pixels, and `Canvas::fill_poly` fills them with the non-zero rule. A fill is the outline of the shape. A
//! stroke is one quad for each segment, plus a join (a disc, a bevel or a miter) at each corner and a cap at each end, all with the same
//! winding, so that the non-zero rule makes their union.
//!
//! Not built: `fill-rule`, `stroke-dasharray`, `fill-opacity`, `stroke-opacity`, `opacity` on a shape, `transform`, `use`, `text`, `image`,
//! paint servers (`url(#a)`), `preserveAspectRatio` other than the default, `stroke-width` from a style sheet, a unit other than `px` or none.

use crate::display::argb;
use bare_css::cascade::{ComputedStyle, Styles};
use bare_css::values::{Property, Value};
use bare_html::dom::{Document, NodeData, NodeId};

/// Math used when flattening SVG shapes. The browser can supply its software math.
pub trait GeometryMath {
    fn sin(x:f32)->f32;
    fn cos(x:f32)->f32;
    fn sin_cos(x:f32)->(f32,f32);
    fn atan2(y:f32,x:f32)->f32;
    fn acos(x:f32)->f32;
    fn hypot(x:f32,y:f32)->f32;
}
struct NativeMath;
impl GeometryMath for NativeMath {
    fn sin(x:f32)->f32{x.sin()}
    fn cos(x:f32)->f32{x.cos()}
    fn sin_cos(x:f32)->(f32,f32){x.sin_cos()}
    fn atan2(y:f32,x:f32)->f32{y.atan2(x)}
    fn acos(x:f32)->f32{x.acos()}
    fn hypot(x:f32,y:f32)->f32{x.hypot(y)}
}

/// Where the user space of the `<svg>` is in pixels: `x * s + tx`, `y * s + ty`.
#[derive(Clone, Copy)]
struct Map {
    s: f32,
    tx: f32,
    ty: f32,
}

/// One polyline of a shape, in user units, and whether it is closed.
struct Sub {
    pts: Vec<(f32, f32)>,
    closed: bool,
}

/// What a shape is painted with: the `fill` and the `stroke` as `0xAARRGGBB`, or `None` for `none`.
type Paint = (Option<u32>, Option<u32>);

fn paint_of(style: &ComputedStyle) -> Paint {
    let one = |p| match style.get(p) {
        Value::Color(c) if c.a > 0 => Some(argb(c)),
        _ => None,
    };
    (one(Property::Fill), one(Property::Stroke))
}

fn attr<'a>(doc: &'a Document, node: NodeId, name: &str) -> Option<&'a str> {
    doc.find_atom(name).and_then(|a| doc.attr(node, a))
}

/// A number with an optional `px`.
fn num(v: &str) -> Option<f32> {
    let v = v.trim();
    v.strip_suffix("px").unwrap_or(v).trim_end().parse::<f32>().ok().filter(|n| n.is_finite())
}

fn len(doc: &Document, node: NodeId, name: &str) -> f32 {
    attr(doc, node, name).and_then(num).unwrap_or(0.0)
}

/// Draw the `<svg>` element `root` in the pixel box `(x, y, w, h)`: `out` gets each polygon list (contours apart by a NaN point) and its color.
pub fn draw(doc: &Document, styles: &Styles, root: NodeId, (x, y, w, h): (f32, f32, f32, f32), out: &mut dyn FnMut(&[(f32, f32)], u32)) {
    let view = attr(doc, root, "viewBox").and_then(|v| {
        let mut n = v.split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty()).map(|t| t.parse::<f32>().ok());
        let (a, b, c, d) = (n.next()??, n.next()??, n.next()??, n.next()??);
        (n.next().is_none() && c > 0.0 && d > 0.0).then_some((a, b, c, d))
    });
    let map = match view {
        Some((vx, vy, vw, vh)) => {
            let s = (w / vw).min(h / vh);
            Map { s, tx: x + (w - vw * s) / 2.0 - vx * s, ty: y + (h - vh * s) / 2.0 - vy * s }
        }
        None => Map { s: 1.0, tx: x, ty: y },
    };
    let paint = styles.style(root).map_or((None, None), paint_of);
    let mut buf = Vec::new();
    walk(doc, styles, root, root, paint, map, out, &mut buf);
}

#[allow(clippy::too_many_arguments)]
fn walk(doc: &Document, styles: &Styles, root: NodeId, node: NodeId, inherited: Paint, map: Map, out: &mut dyn FnMut(&[(f32, f32)], u32), buf: &mut Vec<(f32, f32)>) {
    for child in doc.children(node) {
        if !matches!(doc.data(child), NodeData::Element { .. }) {
            continue;
        }
        // A shape has a style (the rule of the UA sheet hides it, and what is inside a hidden box has none, so a `g` keeps its box).
        let paint = styles.style(child).map_or(inherited, paint_of);
        let name = doc.element_name(child);
        if name == "g" {
            walk(doc, styles, root, child, paint, map, out, buf);
            continue;
        }
        let subs = shape::<NativeMath>(doc, child, name, map.s);
        if subs.is_empty() {
            continue;
        }
        if let Some(color) = paint.0 {
            buf.clear();
            for sub in &subs {
                if sub.pts.len() > 1 {
                    contour(buf, sub.pts.iter().map(|&p| map.at(p)));
                }
            }
            if !buf.is_empty() {
                out(buf, color);
            }
        }
        if let Some(color) = paint.1 {
            let st = Stroke::of(doc, root, child, map.s);
            buf.clear();
            for sub in &subs {
                st.add(sub, map, buf);
            }
            if !buf.is_empty() {
                out(buf, color);
            }
        }
    }
}

impl Map {
    fn at(self, (x, y): (f32, f32)) -> (f32, f32) {
        (x * self.s + self.tx, y * self.s + self.ty)
    }
}

/// Add a contour to a polygon list.
fn contour(out: &mut Vec<(f32, f32)>, pts: impl Iterator<Item = (f32, f32)>) {
    if !out.is_empty() {
        out.push((f32::NAN, f32::NAN));
    }
    out.extend(pts);
}

/// The polylines of a shape, in user units. `s`: pixels for each unit, to choose how many segments a curve needs.
fn shape<M: GeometryMath>(doc: &Document, node: NodeId, name: &str, s: f32) -> Vec<Sub> {
    let l = |n| len(doc, node, n);
    let pair = |c: &[f32]| (c[0], c[1]);
    match name {
        "path" => attr(doc, node, "d").map_or_else(Vec::new, |d| path::<M>(d, s)),
        "line" => vec![Sub { pts: vec![(l("x1"), l("y1")), (l("x2"), l("y2"))], closed: false }],
        "polyline" | "polygon" => {
            let nums: Vec<f32> = attr(doc, node, "points").unwrap_or("").split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty()).map_while(|t| t.parse().ok()).collect();
            vec![Sub { pts: nums.chunks_exact(2).map(pair).collect(), closed: name == "polygon" }]
        }
        "rect" => {
            let (x, y, w, h) = (l("x"), l("y"), l("width"), l("height"));
            if w <= 0.0 || h <= 0.0 {
                return Vec::new();
            }
            let (rx, ry) = (attr(doc, node, "rx").and_then(num), attr(doc, node, "ry").and_then(num));
            let (rx, ry) = (rx.or(ry).unwrap_or(0.0).clamp(0.0, w / 2.0), ry.or(rx).unwrap_or(0.0).clamp(0.0, h / 2.0));
            let mut pts = vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)];
            if rx > 0.0 && ry > 0.0 {
                // Clockwise from the end of the top edge: a line, then a corner.
                pts = vec![(x + rx, y), (x + w - rx, y)];
                for (to, next) in [((x + w, y + ry), (x + w, y + h - ry)), ((x + w - rx, y + h), (x + rx, y + h)), ((x, y + h - ry), (x, y + ry)), ((x + rx, y), (x + rx, y))] {
                    let from = pts[pts.len() - 1];
                    arc::<M>(&mut pts, from, (rx, ry, 0.0, false, true), to, s);
                    pts.push(next);
                }
                pts.pop();
            }
            let sub = Sub { pts, closed: true };
            vec![sub]
        }
        "circle" | "ellipse" => {
            let (rx, ry) = if name == "circle" { (l("r"), l("r")) } else { (l("rx"), l("ry")) };
            if rx <= 0.0 || ry <= 0.0 {
                return Vec::new();
            }
            let (cx, cy) = (l("cx"), l("cy"));
            let n = ((rx.max(ry) * s * 6.0).ceil() as usize).clamp(16, 360);
            // The corners of a polygon are on the curve, so it holds less than the curve: a larger radius gives the same area.
            let k = fit::<M>(n);
            let pts = (0..n).map(|i| {
                let t = std::f32::consts::TAU * i as f32 / n as f32;
                (cx + rx * k * M::cos(t), cy + ry * k * M::sin(t))
            });
            vec![Sub { pts: pts.collect(), closed: true }]
        }
        _ => Vec::new(),
    }
}

/// Read numbers, flags and commands out of the text of a path.
struct Scan<'a> {
    s: &'a [u8],
    i: usize,
}

impl Scan<'_> {
    fn skip(&mut self) {
        while self.i < self.s.len() && (self.s[self.i].is_ascii_whitespace() || self.s[self.i] == b',') {
            self.i += 1;
        }
    }

    fn num(&mut self) -> Option<f32> {
        self.skip();
        let start = self.i;
        let at = |s: &Self, i: usize| s.s.get(i).copied().unwrap_or(0);
        if matches!(at(self, self.i), b'+' | b'-') {
            self.i += 1;
        }
        let digits = |s: &mut Self| {
            let from = s.i;
            while at(s, s.i).is_ascii_digit() {
                s.i += 1;
            }
            s.i > from
        };
        let int = digits(self);
        let mut frac = false;
        if at(self, self.i) == b'.' {
            self.i += 1;
            frac = digits(self);
        }
        if !int && !frac {
            self.i = start;
            return None;
        }
        if matches!(at(self, self.i), b'e' | b'E') {
            let save = self.i;
            self.i += 1;
            if matches!(at(self, self.i), b'+' | b'-') {
                self.i += 1;
            }
            if !digits(self) {
                self.i = save;
            }
        }
        std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()
    }

    /// An arc flag is one digit, with no separator needed after it.
    fn flag(&mut self) -> Option<bool> {
        self.skip();
        let f = match self.s.get(self.i)? {
            b'0' => false,
            b'1' => true,
            _ => return None,
        };
        self.i += 1;
        Some(f)
    }

    fn command(&mut self) -> Option<u8> {
        self.skip();
        let c = *self.s.get(self.i)?;
        c.is_ascii_alphabetic().then(|| {
            self.i += 1;
            c
        })
    }
}

/// The polylines of a path (`d`). A mistake ends the path: what came before it is drawn (SVG 2, "error handling").
fn path<M: GeometryMath>(d: &str, s: f32) -> Vec<Sub> {
    let mut sc = Scan { s: d.as_bytes(), i: 0 };
    let mut subs: Vec<Sub> = Vec::new();
    let (mut cur, mut start, mut ctl) = ((0.0f32, 0.0f32), (0.0f32, 0.0f32), (0.0f32, 0.0f32));
    // The command that goes on if numbers follow, and the kind (in lowercase) of the segment before.
    let (mut cmd, mut prev) = (0u8, 0u8);
    loop {
        sc.skip();
        if sc.i >= sc.s.len() {
            break;
        }
        let c = match (sc.command(), cmd) {
            (Some(c), _) => c,
            (None, b'M') => b'L',
            (None, b'm') => b'l',
            (None, 0 | b'z' | b'Z') => break,
            (None, c) => c,
        };
        if cmd == 0 && !matches!(c, b'M' | b'm') {
            break;
        }
        cmd = c;
        let kind = c.to_ascii_lowercase();
        let (ox, oy) = if c.is_ascii_lowercase() { cur } else { (0.0, 0.0) };
        let done = (|| {
            match kind {
                b'm' | b'l' => {
                    let p = (sc.num()? + ox, sc.num()? + oy);
                    if kind == b'm' {
                        subs.push(Sub { pts: vec![p], closed: false });
                        start = p;
                        cmd = if c == b'm' { b'l' } else { b'L' };
                    } else {
                        cur_sub(&mut subs, cur).pts.push(p);
                    }
                    cur = p;
                }
                b'h' | b'v' => {
                    let v = sc.num()?;
                    cur = if kind == b'h' { (v + ox, cur.1) } else { (cur.0, v + oy) };
                    cur_sub(&mut subs, cur).pts.push(cur);
                }
                b'c' | b's' => {
                    let c1 = if kind == b'c' { (sc.num()? + ox, sc.num()? + oy) } else if matches!(prev, b'c' | b's') { (2.0 * cur.0 - ctl.0, 2.0 * cur.1 - ctl.1) } else { cur };
                    let (c2, p) = ((sc.num()? + ox, sc.num()? + oy), (sc.num()? + ox, sc.num()? + oy));
                    cubic::<M>(&mut cur_sub(&mut subs, cur).pts, cur, c1, c2, p, s);
                    (ctl, cur) = (c2, p);
                }
                b'q' | b't' => {
                    let q = if kind == b'q' { (sc.num()? + ox, sc.num()? + oy) } else if matches!(prev, b'q' | b't') { (2.0 * cur.0 - ctl.0, 2.0 * cur.1 - ctl.1) } else { cur };
                    let p = (sc.num()? + ox, sc.num()? + oy);
                    quad::<M>(&mut cur_sub(&mut subs, cur).pts, cur, q, p, s);
                    (ctl, cur) = (q, p);
                }
                b'a' => {
                    let (rx, ry, rot) = (sc.num()?, sc.num()?, sc.num()?);
                    let (large, sweep) = (sc.flag()?, sc.flag()?);
                    let p = (sc.num()? + ox, sc.num()? + oy);
                    arc::<M>(&mut cur_sub(&mut subs, cur).pts, cur, (rx, ry, rot, large, sweep), p, s);
                    cur = p;
                }
                b'z' => {
                    if let Some(last) = subs.last_mut() {
                        last.closed = true;
                    }
                    cur = start;
                }
                _ => return None,
            }
            Some(())
        })();
        if done.is_none() {
            break;
        }
        prev = kind;
    }
    // A lone point (an `M` with nothing after it) draws nothing. A closed one (`M x y z`) draws a dot with a round or a square cap.
    subs.retain(|s| s.pts.len() > 1 || s.closed);
    subs
}

/// The polyline that a segment is added to: the open one at the end, or a new one at the current point (after a `Z`).
fn cur_sub(subs: &mut Vec<Sub>, cur: (f32, f32)) -> &mut Sub {
    if subs.last().is_none_or(|l| l.closed) {
        subs.push(Sub { pts: vec![cur], closed: false });
    }
    subs.last_mut().expect("one polyline")
}

fn cubic<M: GeometryMath>(out: &mut Vec<(f32, f32)>, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), p3: (f32, f32), s: f32) {
    let d = |a: (f32, f32), b: (f32, f32)| M::hypot(a.0-b.0,a.1-b.1);
    let n = ((d(p0, p1) + d(p1, p2) + d(p2, p3)) * s / 1.5).ceil().clamp(4.0, 96.0) as usize;
    for i in 1..=n {
        let t = i as f32 / n as f32;
        let u = 1.0 - t;
        let (a, b, c, e) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
        out.push((a * p0.0 + b * p1.0 + c * p2.0 + e * p3.0, a * p0.1 + b * p1.1 + c * p2.1 + e * p3.1));
    }
}

fn quad<M: GeometryMath>(out: &mut Vec<(f32, f32)>, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), s: f32) {
    let c1 = (p0.0 + 2.0 / 3.0 * (p1.0 - p0.0), p0.1 + 2.0 / 3.0 * (p1.1 - p0.1));
    let c2 = (p2.0 + 2.0 / 3.0 * (p1.0 - p2.0), p2.1 + 2.0 / 3.0 * (p1.1 - p2.1));
    cubic::<M>(out, p0, c1, c2, p2, s);
}

/// An elliptical arc from `from` to `to` (SVG 2, "implementation notes": endpoint to center). `(rx, ry, rotation in degrees, large, sweep)`.
fn arc<M: GeometryMath>(out: &mut Vec<(f32, f32)>, from: (f32, f32), (rx, ry, rot, large, sweep): (f32, f32, f32, bool, bool), to: (f32, f32), s: f32) {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if from == to {
        return;
    }
    if rx == 0.0 || ry == 0.0 {
        out.push(to);
        return;
    }
    let (sin, cos) = M::sin_cos(rot.to_radians());
    let (dx, dy) = ((from.0 - to.0) / 2.0, (from.1 - to.1) / 2.0);
    let (x1, y1) = (cos * dx + sin * dy, -sin * dx + cos * dy);
    let scale = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if scale > 1.0 {
        let k = scale.sqrt();
        (rx, ry) = (rx * k, ry * k);
    }
    let (rx2, ry2) = (rx * rx, ry * ry);
    let num = (rx2 * ry2 - rx2 * y1 * y1 - ry2 * x1 * x1).max(0.0);
    let coef = (num / (rx2 * y1 * y1 + ry2 * x1 * x1)).sqrt() * if large == sweep { -1.0 } else { 1.0 };
    let (cxp, cyp) = (coef * rx * y1 / ry, -coef * ry * x1 / rx);
    let (cx, cy) = (cos * cxp - sin * cyp + (from.0 + to.0) / 2.0, sin * cxp + cos * cyp + (from.1 + to.1) / 2.0);
    let angle = |ux: f32, uy: f32, vx: f32, vy: f32| M::atan2(ux * vy - uy * vx,ux * vx + uy * vy);
    let (ux, uy) = ((x1 - cxp) / rx, (y1 - cyp) / ry);
    let theta = angle(1.0, 0.0, ux, uy);
    let mut delta = angle(ux, uy, (-x1 - cxp) / rx, (-y1 - cyp) / ry);
    if !sweep && delta > 0.0 {
        delta -= std::f32::consts::TAU;
    } else if sweep && delta < 0.0 {
        delta += std::f32::consts::TAU;
    }
    // A step that keeps a segment within 0.05 px of the curve.
    let r = rx.max(ry) * s;
    let step = if r > 0.05 { 2.0 * M::acos(1.0 - 0.05 / r) } else { 1.0 };
    let n = ((delta.abs() / step.max(0.02)).ceil() as usize).clamp(2, 256);
    for i in 1..n {
        let t = theta + delta * i as f32 / n as f32;
        let (px, py) = (rx * M::cos(t), ry * M::sin(t));
        out.push((cos * px - sin * py + cx, sin * px + cos * py + cy));
    }
    out.push(to);
}

/// The stroke of a shape: its width in pixels, the end of a line, the corner of two lines and the limit of a miter.
struct Stroke {
    half: f32,
    cap: u8,
    join: u8,
    miter: f32,
}

impl Stroke {
    /// Read the attributes of the shape, or of the nearest element above it that has them, up to the `<svg>`.
    fn of(doc: &Document, root: NodeId, node: NodeId, s: f32) -> Stroke {
        let find = |name: &str| {
            let mut n = Some(node);
            while let Some(e) = n {
                if let Some(v) = attr(doc, e, name) {
                    return Some(v);
                }
                n = if e == root { None } else { doc.parent(e) };
            }
            None
        };
        let word = |name: &str, list: &[&str]| find(name).and_then(|v| list.iter().position(|w| v.trim() == *w)).unwrap_or(0) as u8;
        Stroke {
            half: find("stroke-width").and_then(num).filter(|w| *w >= 0.0).unwrap_or(1.0) * s / 2.0,
            cap: word("stroke-linecap", &["butt", "round", "square"]),
            join: word("stroke-linejoin", &["miter", "round", "bevel"]),
            miter: find("stroke-miterlimit").and_then(num).filter(|m| *m >= 1.0).unwrap_or(4.0),
        }
    }

    /// Add the polygons of one polyline to `out`.
    fn add(&self, sub: &Sub, map: Map, out: &mut Vec<(f32, f32)>) {
        let hw = self.half;
        if hw <= 0.0 {
            return;
        }
        let mut p: Vec<(f32, f32)> = Vec::with_capacity(sub.pts.len());
        for &q in &sub.pts {
            let q = map.at(q);
            if p.last().is_none_or(|l| (l.0 - q.0).hypot(l.1 - q.1) > 1e-4) {
                p.push(q);
            }
        }
        if sub.closed && p.len() > 1 && (p[0].0 - p[p.len() - 1].0).hypot(p[0].1 - p[p.len() - 1].1) <= 1e-4 {
            p.pop();
        }
        if p.len() == 1 {
            // A point: a disc for a round cap, a square for a square one.
            match self.cap {
                1 => disc(out, p[0], hw),
                2 => poly(out, &[(p[0].0 - hw, p[0].1 - hw), (p[0].0 + hw, p[0].1 - hw), (p[0].0 + hw, p[0].1 + hw), (p[0].0 - hw, p[0].1 + hw)]),
                _ => {}
            }
            return;
        }
        let n = p.len();
        let segments = if sub.closed { n } else { n - 1 };
        let dir = |i: usize| {
            let (a, b) = (p[i % n], p[(i + 1) % n]);
            let l = (b.0 - a.0).hypot(b.1 - a.1);
            ((b.0 - a.0) / l, (b.1 - a.1) / l)
        };
        for i in 0..segments {
            let (a, b, d) = (p[i % n], p[(i + 1) % n], dir(i));
            let m = (-d.1 * hw, d.0 * hw);
            poly(out, &[(a.0 + m.0, a.1 + m.1), (b.0 + m.0, b.1 + m.1), (b.0 - m.0, b.1 - m.1), (a.0 - m.0, a.1 - m.1)]);
            // The corner with the next segment.
            if i + 1 < segments || sub.closed {
                self.corner(out, b, d, dir(i + 1));
            }
        }
        if !sub.closed {
            for (end, d, sign) in [(p[0], dir(0), -1.0), (p[n - 1], dir(n - 2), 1.0)] {
                match self.cap {
                    1 => disc(out, end, hw),
                    2 => {
                        let (m, e) = ((-d.1 * hw, d.0 * hw), (d.0 * hw * sign, d.1 * hw * sign));
                        poly(out, &[(end.0 + m.0, end.1 + m.1), (end.0 + m.0 + e.0, end.1 + m.1 + e.1), (end.0 - m.0 + e.0, end.1 - m.1 + e.1), (end.0 - m.0, end.1 - m.1)]);
                    }
                    _ => {}
                }
            }
        }
    }

    /// The join at `v` between a line that goes in the direction `d0` and one that goes in `d1`.
    fn corner(&self, out: &mut Vec<(f32, f32)>, v: (f32, f32), d0: (f32, f32), d1: (f32, f32)) {
        let hw = self.half;
        let (cross, dot) = (d0.0 * d1.1 - d0.1 * d1.0, d0.0 * d1.0 + d0.1 * d1.1);
        if cross.abs() < 1e-4 && dot > 0.0 {
            return;
        }
        // The outer side of the turn.
        let side = if cross > 0.0 { -1.0 } else { 1.0 };
        let (n0, n1) = ((-d0.1 * hw * side, d0.0 * hw * side), (-d1.1 * hw * side, d1.0 * hw * side));
        let (a, b) = ((v.0 + n0.0, v.1 + n0.1), (v.0 + n1.0, v.1 + n1.1));
        match self.join {
            // A round join of a small turn (a piece of a curve) is a bevel: the difference is far under a pixel.
            1 if dot < 0.95 => disc(out, v, hw),
            0 if 1.0 / ((1.0 + dot) / 2.0).max(1e-6).sqrt() <= self.miter => {
                let k = 1.0 / (1.0 + dot);
                poly(out, &[v, a, (v.0 + (n0.0 + n1.0) * k, v.1 + (n0.1 + n1.1) * k), b]);
            }
            _ => poly(out, &[v, a, b]),
        }
    }
}

/// Add a polygon with the winding that every piece of a stroke has: the negative one. A reversed polygon is turned.
fn poly(out: &mut Vec<(f32, f32)>, pts: &[(f32, f32)]) {
    let area: f32 = pts.iter().zip(pts.iter().cycle().skip(1)).map(|(a, b)| a.0 * b.1 - b.0 * a.1).sum();
    if !out.is_empty() {
        out.push((f32::NAN, f32::NAN));
    }
    if area > 0.0 {
        out.extend(pts.iter().rev());
    } else {
        out.extend_from_slice(pts);
    }
}

fn disc(out: &mut Vec<(f32, f32)>, c: (f32, f32), r: f32) {
    let n = ((r * 6.0).ceil() as usize).clamp(12, 96);
    let r = r * fit::<NativeMath>(n);
    let pts: Vec<(f32, f32)> = (0..n).map(|i| {
        let t = std::f32::consts::TAU * i as f32 / n as f32;
        (c.0 + r * t.cos(), c.1 + r * t.sin())
    }).collect();
    poly(out, &pts);
}

/// The factor for the radius of a regular polygon of `n` corners that gives the area of the circle.
fn fit<M: GeometryMath>(n: usize) -> f32 {
    let n = n as f32;
    (std::f32::consts::PI / (n / 2.0 * M::sin(std::f32::consts::TAU / n))).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts(d: &str) -> Vec<(Vec<(f32, f32)>, bool)> {
        path::<NativeMath>(d, 1.0).into_iter().map(|s| (s.pts, s.closed)).collect()
    }

    #[test]
    fn lines_relative_and_implicit_commands() {
        assert_eq!(pts("M1 2 L3 4 h2 v-1 z"), vec![(vec![(1.0, 2.0), (3.0, 4.0), (5.0, 4.0), (5.0, 3.0)], true)]);
        // Numbers after `m` are `l`, relative to the point before. Commas, signs and dots cut a number.
        assert_eq!(pts("m1 1 2 2-1.5.5"), vec![(vec![(1.0, 1.0), (3.0, 3.0), (1.5, 3.5)], false)]);
        // A segment after `z` starts at the start of the closed polyline.
        let two = pts("M0 0 L4 0 L4 4 z l0 3");
        assert_eq!((two.len(), two[1].0.clone(), two[1].1), (2, vec![(0.0, 0.0), (0.0, 3.0)], false));
    }

    #[test]
    fn a_mistake_ends_the_path_and_keeps_what_is_before() {
        assert_eq!(pts("M0 0 L1 1 L2"), vec![(vec![(0.0, 0.0), (1.0, 1.0)], false)]);
        assert_eq!(pts("L1 1"), vec![]);
        assert_eq!(pts("M0 0 X1 1 L5 5"), vec![]);
        // A lone point draws nothing, and `M x y z` keeps its dot.
        assert_eq!(pts("M3 3"), vec![]);
        assert_eq!(pts("M3 3z"), vec![(vec![(3.0, 3.0)], true)]);
    }

    #[test]
    fn curves_and_arcs_end_on_their_end_point() {
        for (d, end) in [("M0 0 C4 0 8 4 8 8", (8.0, 8.0)), ("M0 0 S4 4 8 8", (8.0, 8.0)), ("M0 0 Q8 0 8 8", (8.0, 8.0)), ("M0 0 T8 8", (8.0, 8.0)), ("M0 8 A8 8 0 0 1 8 0", (8.0, 0.0)), ("M0 8a8 8 0 0 1 8-8", (8.0, 0.0)), ("M0 8 A8 8 0 00 8 0", (8.0, 0.0)), ("M0 8 A1 1 0 0 1 8 0", (8.0, 0.0))] {
            let p = pts(d);
            let last = *p[0].0.last().expect("a point");
            assert!((last.0 - end.0).abs() < 1e-3 && (last.1 - end.1).abs() < 1e-3, "{d}: {last:?}");
            assert!(p[0].0.len() > 3, "{d}: a curve has more than its end points");
        }
        // The half circle of radius 8 passes through (4, 8 - 8 * sin 45 deg) and stays on the circle.
        let arc = &pts("M0 8 A8 8 0 0 1 16 8")[0].0;
        assert!(arc.iter().all(|&(x, y)| ((x - 8.0).hypot(y - 8.0) - 8.0).abs() < 0.01), "on the circle");
        assert!(arc.iter().all(|&(_, y)| y <= 8.0 + 1e-3), "the sweep flag chooses the upper half in a y-down space");
    }

    /// The area of the polygons of a stroke, by filling them in a canvas.
    fn coverage(st: &Stroke, sub: &Sub) -> f32 {
        let mut out = Vec::new();
        st.add(sub, Map { s: 1.0, tx: 0.0, ty: 0.0 }, &mut out);
        let mut c = crate::canvas::Canvas::new(40, 40);
        c.pixels.fill(0xff00_0000);
        let bounds = c.bounds();
        c.fill_poly(&out, 0xffff_ffff, bounds);
        c.pixels.iter().map(|&p| (p & 0xff) as f32 / 255.0).sum()
    }

    #[test]
    fn a_stroke_is_the_union_of_its_pieces() {
        let line = Sub { pts: vec![(5.0, 10.0), (25.0, 10.0)], closed: false };
        let st = |cap, join| Stroke { half: 2.0, cap, join, miter: 4.0 };
        // 20 long and 4 wide. A square cap adds 2 at each end, a round one adds half a disc (6.28) at each end.
        assert!((coverage(&st(0, 0), &line) - 80.0).abs() < 0.5);
        assert!((coverage(&st(2, 0), &line) - 96.0).abs() < 0.5);
        assert!((coverage(&st(1, 0), &line) - (80.0 + 4.0 * std::f32::consts::PI)).abs() < 0.8);
        // Two arms of 80 that share 4 make a right angle. A miter join adds the corner square (4), a bevel join half of it (2).
        let corner = Sub { pts: vec![(5.0, 5.0), (25.0, 5.0), (25.0, 25.0)], closed: false };
        assert!((coverage(&st(0, 0), &corner) - 160.0).abs() < 0.5);
        assert!((coverage(&st(0, 2), &corner) - 158.0).abs() < 0.5);
        // A closed square of 20 with width 4: the ring 24 * 24 - 16 * 16.
        let square = Sub { pts: vec![(8.0, 8.0), (28.0, 8.0), (28.0, 28.0), (8.0, 28.0)], closed: true };
        assert!((coverage(&st(0, 0), &square) - (24.0 * 24.0 - 16.0 * 16.0)).abs() < 1.0);
        // Pieces that overlap are counted once.
        let back = Sub { pts: vec![(5.0, 10.0), (25.0, 10.0), (5.0, 10.0)], closed: false };
        assert!((coverage(&st(0, 0), &back) - 80.0).abs() < 1.0);
    }

    #[test]
    fn polygons_fill_with_the_non_zero_rule_and_exact_edges() {
        let mut c = crate::canvas::Canvas::new(20, 20);
        c.pixels.fill(0xff00_0000);
        let bounds = c.bounds();
        // A square from 2.5 to 7.5: the edge pixels are half covered. A second square on top of it changes nothing.
        let sq = |x0: f32, x1: f32| [(x0, 2.0), (x1, 2.0), (x1, 8.0), (x0, 8.0)];
        let mut pts = sq(2.5, 7.5).to_vec();
        pts.push((f32::NAN, f32::NAN));
        pts.extend(sq(3.0, 6.0));
        c.fill_poly(&pts, 0xffff_ffff, bounds);
        assert_eq!([c.pixel(2, 4), c.pixel(3, 4), c.pixel(7, 4), c.pixel(8, 4), c.pixel(5, 1), c.pixel(5, 8)], [0xff80_8080, 0xffff_ffff, 0xff80_8080, 0xff00_0000, 0xff00_0000, 0xff00_0000]);
    }
}

/// The text of an SVG file that `bitmap` reads, at most.
pub const MAX_FILE: usize = 512 << 10;
/// The sides of the picture that `bitmap` makes, at most. A larger one is refused.
const MAX_SIDE: f32 = 4096.0;

/// The size of the picture that an `<svg>` element gives: `width` and `height`, else the `viewBox`, else 300 by 150 (SVG 2, "sizing"). One
/// side with a `viewBox` gives the other by its ratio.
fn file_size(doc: &Document, root: NodeId) -> Option<(u32, u32)> {
    let view = attr(doc, root, "viewBox").and_then(|v| {
        let mut n = v.split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty()).map(|t| t.parse::<f32>().ok());
        let (_, _, c, d) = (n.next()??, n.next()??, n.next()??, n.next()??);
        (n.next().is_none() && c > 0.0 && d > 0.0).then_some((c, d))
    });
    let (w, h) = (attr(doc, root, "width").and_then(num), attr(doc, root, "height").and_then(num));
    let (w, h) = match (w, h, view) {
        (Some(w), Some(h), _) => (w, h),
        (Some(w), None, Some((c, d))) => (w, w * d / c),
        (None, Some(h), Some((c, d))) => (h * c / d, h),
        (None, None, Some(v)) => v,
        _ => (w.unwrap_or(300.0), h.unwrap_or(150.0)),
    };
    ((1.0..=MAX_SIDE).contains(&w) && (1.0..=MAX_SIDE).contains(&h)).then_some((w.round() as u32, h.round() as u32))
}

/// An SVG file as the pixels of an `<img>`: its width, its height and `0xAARRGGBB` pixels. The shapes are drawn by the painter of an inline
/// `<svg>` (see above), at the size of the file. The file is put in a page and painted twice, on white and on black: the difference of
/// the two gives the alpha of each pixel. `None` for a file that is too long, has no `<svg>` or has a side over `MAX_SIDE`.
pub fn bitmap(text: &str) -> Option<(u32, u32, Vec<u32>)> {
    use bare_css::cascade::{Origin, RuleSet, Source, UA_CSS};
    if text.len() > MAX_FILE {
        return None;
    }
    // The XML prolog and the doctype are not HTML: the page starts at the element.
    let svg = &text[text.find("<svg")?..];
    // One page for both pictures: parse, style, layout and display list are made once. Only the color under the shapes changes.
    let mut doc = bare_html::tree::parse(&format!("<body style=\"margin:0;font-size:0;line-height:0\">{svg}"));
    let size = {
        let svg = doc.find_atom("svg")?;
        let mut stack = vec![doc.root()];
        let root = loop {
            let n = stack.pop()?;
            if matches!(doc.data(n), NodeData::Element { name, .. } if *name == svg) {
                break n;
            }
            stack.extend(doc.children(n));
        };
        file_size(&doc, root)?
    };
    let viewport = (size.0 as f32, size.1 as f32);
    let mut rules = RuleSet::new(viewport);
    rules.add(&mut doc, UA_CSS, Origin::UserAgent);
    for source in bare_css::cascade::sources(&doc, viewport) {
        if let Source::Inline(text) = source {
            rules.add(&mut doc, &text, Origin::Author);
        }
    }
    let styles = rules.compute(&doc);
    let layout = bare_layout::layout(&doc, &styles, viewport);
    let mut painter = crate::Painter::new();
    let mut white = crate::canvas::Canvas::new(size.0, size.1);
    painter.paint(&doc, &styles, &layout, &mut white);
    // The page has no background of its own, so the list paints on white; the second pass puts black under the same list.
    let mut black = crate::canvas::Canvas::new(size.0, size.1);
    painter.list.background = 0xff00_0000;
    painter.draw(&mut black);
    let px = white
        .pixels
        .iter()
        .zip(black.pixels.iter())
        .map(|(&w, &b)| {
            let c = |p: u32, s: u32| p >> s & 255;
            let gap = (c(w, 16).saturating_sub(c(b, 16)) + c(w, 8).saturating_sub(c(b, 8)) + c(w, 0).saturating_sub(c(b, 0))) / 3;
            match 255 - gap.min(255) {
                0 => 0,
                a => (a << 24) | ((c(b, 16) * 255 / a).min(255) << 16) | ((c(b, 8) * 255 / a).min(255) << 8) | (c(b, 0) * 255 / a).min(255),
            }
        })
        .collect();
    Some((size.0, size.1, px))
}

#[cfg(test)]
mod file_tests {
    use super::bitmap;

    #[test]
    fn an_svg_file_becomes_pixels_with_alpha() {
        let svg = "<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"12\" viewBox=\"0 0 24 12\"><rect x=\"0\" y=\"0\" width=\"12\" height=\"12\" fill=\"#ff0000\"/></svg>";
        let (w, h, px) = bitmap(svg).unwrap();
        assert_eq!((w, h, px.len()), (24, 12, 288));
        assert_eq!(px[5 * 24 + 3], 0xffff_0000);
        // Outside the rectangle nothing is painted.
        assert_eq!(px[5 * 24 + 20] >> 24, 0);
    }

    #[test]
    fn the_size_comes_from_the_view_box_and_a_bad_file_gives_none() {
        assert_eq!(bitmap("<svg viewBox=\"0 0 20 10\"><rect width=\"20\" height=\"10\"/></svg>").map(|b| (b.0, b.1)), Some((20, 10)));
        assert!(bitmap("<svg width=\"5000\" height=\"5\"></svg>").is_none());
        assert!(bitmap("<p>no</p>").is_none());
        assert!(bitmap(&format!("<svg>{}</svg>", " ".repeat(super::MAX_FILE))).is_none());
    }
}

/// Flatten a shape with the same curve approximation used by painting.
pub fn geometry<M: GeometryMath>(doc: &Document, node: NodeId) -> Vec<Vec<(f32,f32)>> {
    shape::<M>(doc,node,doc.element_name(node),1.0).into_iter().map(|mut sub| {
        if sub.closed && !sub.pts.is_empty() {sub.pts.push(sub.pts[0]);}
        sub.pts
    }).collect()
}
