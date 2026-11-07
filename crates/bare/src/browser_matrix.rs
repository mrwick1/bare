//! `DOMMatrixReadOnly` and `DOMMatrix` (Geometry Interfaces Level 1), as Chrome 155 has them (`tools/fixtures/ui/geometry-cases.txt`).
//! The rectangles and points are in `browser_geom.rs`; the objects and their table are the same.
//!
//! The matrix is 16 numbers in column-major order (`m11, m12, m13, m14, m21, ...`) and the flag `is2D`. A 2D matrix keeps the other ten numbers
//! at the identity, and a 2D product uses the six affine numbers only (so an infinity in a number that is not there cannot make a NaN).
//!
//! What Chrome does, and this file does the same
//! - Angles: `sin` and `cos` of an angle in degrees come from the quadrant and a fold at 45 degrees, so multiples of 90 are exact. A negative
//!   angle folds by `floor(d / 90)`. From 90 000 000 degrees the angle is cut to a turn by `fmod` and the plain `sin` and `cos` run. `skew` is `tan`.
//! - The string syntax (constructor and `setMatrixValue`) is a CSS `<transform-list>` whose lengths are absolute, as Chrome parses it: `px cm mm Q in pt pc`.
//!   A length in `em`, `%`, `vw` and the like is "Values must be resolvable at parse time". Lengths are 32-bit floats, cut to
//!   -33554430 .. 33554428. `scale` is a 32-bit float; `matrix()` keeps a double; a number past the float range is cut to it. The empty string and `none` are the identity.
//! - `DOMMatrixInit` is read in the order of Chrome (the 2D members, then `is2D`, then the 3D members) and checked as the standard says
//!   ("validate and fixup"), with Chrome's messages.
//!
//! Gaps: `calc()` and `var()` in the string are a syntax error (Chrome resolves `calc()` of absolute lengths). `perspective(none)` is not accepted.
//! The getters of `DOMMatrix.prototype` also accept a `DOMMatrixReadOnly` (the setters do not).

use crate::browser::*;
use crate::browser_geom::{Iface, Slot, finish, illegal, make, method, new_obj, num, point_init, point_object, put};
use crate::browser_lazy::{self, lazy_global};
use crate::browser_platform::{V, arg};
use bare_js::eval::Interp;
use bare_js::heap::Elem;
use bare_js::{atan2, cos, sin, tan};
use bare_js::value::{ObjId, Value, number_to_string};
use std::f64::consts::PI;

#[derive(Clone, Copy)]
pub(crate) struct Mat {
    pub m: [f64; 16],
    pub is2d: bool,
    /// A `DOMMatrix` (mutable).
    pub rw: bool,
}

const IDENT: [f64; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
/// a, b, c, d, e, f in the 16 numbers.
const AFFINE: [usize; 6] = [0, 1, 4, 5, 12, 13];

/// `sin` and `cos` of an angle in degrees. Chrome folds the angle into the first octant.
fn sin_cos(deg: f64) -> (f64, f64) {
    if deg.is_nan() || deg.is_infinite() {
        return (f64::NAN, f64::NAN);
    }
    if deg.abs() >= 90_000_000.0 {
        let r = (deg % 360.0) * PI / 180.0;
        return (sin(r), cos(r));
    }
    let q = (deg / 90.0).floor();
    let t = deg - 90.0 * q;
    let (mut c, mut s) = if t <= 45.0 { (cos(t * PI / 180.0), sin(t * PI / 180.0)) } else { (sin((90.0 - t) * PI / 180.0), cos((90.0 - t) * PI / 180.0)) };
    if t == 45.0 {
        s = c;
    }
    (c, s) = match (q.rem_euclid(4.0)) as u8 {
        0 => (c, s),
        1 => (-s, c),
        2 => (-c, -s),
        _ => (s, -c),
    };
    (s, c)
}

impl Mat {
    pub fn new(rw: bool) -> Mat {
        Mat { m: IDENT, is2d: true, rw }
    }

    fn nan(rw: bool) -> Mat {
        Mat { m: [f64::NAN; 16], is2d: false, rw }
    }

    fn affine(a: [f64; 6], rw: bool) -> Mat {
        let mut r = Mat::new(rw);
        for (i, v) in AFFINE.into_iter().zip(a) {
            r.m[i] = v;
        }
        r
    }

    fn is_identity(&self) -> bool {
        self.m == IDENT
    }

    /// `self * o`: `o` acts first.
    fn mul(&self, o: &Mat) -> Mat {
        let (a, b) = (&self.m, &o.m);
        if self.is2d && o.is2d {
            return Mat::affine(
                [
                    a[0] * b[0] + a[4] * b[1],
                    a[1] * b[0] + a[5] * b[1],
                    a[0] * b[4] + a[4] * b[5],
                    a[1] * b[4] + a[5] * b[5],
                    a[0] * b[12] + a[4] * b[13] + a[12],
                    a[1] * b[12] + a[5] * b[13] + a[13],
                ],
                self.rw,
            );
        }
        let mut r = [0.0; 16];
        for col in 0..4 {
            for row in 0..4 {
                r[col * 4 + row] = a[row] * b[col * 4] + a[4 + row] * b[col * 4 + 1] + a[8 + row] * b[col * 4 + 2] + a[12 + row] * b[col * 4 + 3];
            }
        }
        Mat { m: r, is2d: false, rw: self.rw }
    }

    fn translate(&mut self, x: f64, y: f64, z: f64) {
        if z != 0.0 {
            self.is2d = false;
        }
        let m = &mut self.m;
        if self.is2d {
            m[12] += x * m[0] + y * m[4];
            m[13] += x * m[1] + y * m[5];
        } else {
            for r in 0..4 {
                m[12 + r] += x * m[r] + y * m[4 + r] + z * m[8 + r];
            }
        }
    }

    fn scale(&mut self, x: f64, y: f64, z: f64) {
        for r in 0..4 {
            self.m[r] *= x;
            self.m[4 + r] *= y;
            self.m[8 + r] *= z;
        }
    }

    /// Scale about the point (ox, oy, oz); the point is the origin when it is 0.
    fn scale_about(&mut self, x: f64, y: f64, z: f64, o: [f64; 3]) {
        let moved = o != [0.0; 3];
        if moved {
            self.translate(o[0], o[1], o[2]);
        }
        self.scale(x, y, z);
        if moved {
            self.translate(-o[0], -o[1], -o[2]);
        }
    }

    fn skew_x(&mut self, deg: f64) {
        let t = tan(deg * PI / 180.0);
        for r in 0..4 {
            self.m[4 + r] += self.m[r] * t;
        }
    }

    fn skew_y(&mut self, deg: f64) {
        let t = tan(deg * PI / 180.0);
        for r in 0..4 {
            self.m[r] += self.m[4 + r] * t;
        }
    }

    /// Post-multiply a rotation by `deg` about the axis (x, y, z).
    fn rotate_axis(&mut self, x: f64, y: f64, z: f64, deg: f64) {
        let len = (x * x + y * y + z * z).sqrt();
        let flat = x == 0.0 && y == 0.0;
        if !flat {
            self.is2d = false;
        }
        if len == 0.0 {
            return;
        }
        let (x, y, z) = (x / len, y / len, z / len);
        let (s, c) = sin_cos(deg);
        let k = 1.0 - c;
        let mut r = Mat::new(self.rw);
        r.is2d = flat;
        r.m[0] = c + x * x * k;
        r.m[1] = z * s + x * y * k;
        r.m[2] = -y * s + x * z * k;
        r.m[4] = x * y * k - z * s;
        r.m[5] = c + y * y * k;
        r.m[6] = x * s + y * z * k;
        r.m[8] = x * z * k + y * s;
        r.m[9] = y * z * k - x * s;
        r.m[10] = c + z * z * k;
        let was = self.is2d;
        *self = self.mul(&r);
        self.is2d = was;
    }

    fn invert(&mut self) {
        let m = self.m;
        if self.is2d {
            let det = m[0] * m[5] - m[1] * m[4];
            if det == 0.0 || !det.is_finite() {
                *self = Mat::nan(self.rw);
                return;
            }
            *self = Mat::affine(
                [m[5] / det, -m[1] / det, -m[4] / det, m[0] / det, (m[4] * m[13] - m[5] * m[12]) / det, (m[1] * m[12] - m[0] * m[13]) / det],
                self.rw,
            );
            return;
        }
        let mut inv = [0.0; 16];
        inv[0] = m[5] * m[10] * m[15] - m[5] * m[11] * m[14] - m[9] * m[6] * m[15] + m[9] * m[7] * m[14] + m[13] * m[6] * m[11] - m[13] * m[7] * m[10];
        inv[4] = -m[4] * m[10] * m[15] + m[4] * m[11] * m[14] + m[8] * m[6] * m[15] - m[8] * m[7] * m[14] - m[12] * m[6] * m[11] + m[12] * m[7] * m[10];
        inv[8] = m[4] * m[9] * m[15] - m[4] * m[11] * m[13] - m[8] * m[5] * m[15] + m[8] * m[7] * m[13] + m[12] * m[5] * m[11] - m[12] * m[7] * m[9];
        inv[12] = -m[4] * m[9] * m[14] + m[4] * m[10] * m[13] + m[8] * m[5] * m[14] - m[8] * m[6] * m[13] - m[12] * m[5] * m[10] + m[12] * m[6] * m[9];
        inv[1] = -m[1] * m[10] * m[15] + m[1] * m[11] * m[14] + m[9] * m[2] * m[15] - m[9] * m[3] * m[14] - m[13] * m[2] * m[11] + m[13] * m[3] * m[10];
        inv[5] = m[0] * m[10] * m[15] - m[0] * m[11] * m[14] - m[8] * m[2] * m[15] + m[8] * m[3] * m[14] + m[12] * m[2] * m[11] - m[12] * m[3] * m[10];
        inv[9] = -m[0] * m[9] * m[15] + m[0] * m[11] * m[13] + m[8] * m[1] * m[15] - m[8] * m[3] * m[13] - m[12] * m[1] * m[11] + m[12] * m[3] * m[9];
        inv[13] = m[0] * m[9] * m[14] - m[0] * m[10] * m[13] - m[8] * m[1] * m[14] + m[8] * m[2] * m[13] + m[12] * m[1] * m[10] - m[12] * m[2] * m[9];
        inv[2] = m[1] * m[6] * m[15] - m[1] * m[7] * m[14] - m[5] * m[2] * m[15] + m[5] * m[3] * m[14] + m[13] * m[2] * m[7] - m[13] * m[3] * m[6];
        inv[6] = -m[0] * m[6] * m[15] + m[0] * m[7] * m[14] + m[4] * m[2] * m[15] - m[4] * m[3] * m[14] - m[12] * m[2] * m[7] + m[12] * m[3] * m[6];
        inv[10] = m[0] * m[5] * m[15] - m[0] * m[7] * m[13] - m[4] * m[1] * m[15] + m[4] * m[3] * m[13] + m[12] * m[1] * m[7] - m[12] * m[3] * m[5];
        inv[14] = -m[0] * m[5] * m[14] + m[0] * m[6] * m[13] + m[4] * m[1] * m[14] - m[4] * m[2] * m[13] - m[12] * m[1] * m[6] + m[12] * m[2] * m[5];
        inv[3] = -m[1] * m[6] * m[11] + m[1] * m[7] * m[10] + m[5] * m[2] * m[11] - m[5] * m[3] * m[10] - m[9] * m[2] * m[7] + m[9] * m[3] * m[6];
        inv[7] = m[0] * m[6] * m[11] - m[0] * m[7] * m[10] - m[4] * m[2] * m[11] + m[4] * m[3] * m[10] + m[8] * m[2] * m[7] - m[8] * m[3] * m[6];
        inv[11] = -m[0] * m[5] * m[11] + m[0] * m[7] * m[9] + m[4] * m[1] * m[11] - m[4] * m[3] * m[9] - m[8] * m[1] * m[7] + m[8] * m[3] * m[5];
        inv[15] = m[0] * m[5] * m[10] - m[0] * m[6] * m[9] - m[4] * m[1] * m[10] + m[4] * m[2] * m[9] + m[8] * m[1] * m[6] - m[8] * m[2] * m[5];
        let det = m[0] * inv[0] + m[1] * inv[4] + m[2] * inv[8] + m[3] * inv[12];
        if det == 0.0 || !det.is_finite() {
            *self = Mat::nan(self.rw);
            return;
        }
        for v in &mut inv {
            *v /= det;
        }
        self.m = inv;
    }

    /// The point (x, y, z, w) moved by the matrix.
    pub fn transform(&self, p: [f64; 4]) -> [f64; 4] {
        let (m, [x, y, z, w]) = (&self.m, p);
        if self.is2d {
            return [m[0] * x + m[4] * y + m[12] * w, m[1] * x + m[5] * y + m[13] * w, z, w];
        }
        [
            m[0] * x + m[4] * y + m[8] * z + m[12] * w,
            m[1] * x + m[5] * y + m[9] * z + m[13] * w,
            m[2] * x + m[6] * y + m[10] * z + m[14] * w,
            m[3] * x + m[7] * y + m[11] * z + m[15] * w,
        ]
    }

    fn finite(&self) -> bool {
        self.m.iter().all(|v| v.is_finite())
    }

    /// `rotate(rx, ry, rz)` of the standard: z, then y, then x.
    fn rotate(&mut self, rx: f64, ry: f64, rz: f64) {
        if rx != 0.0 || ry != 0.0 {
            self.is2d = false;
        }
        let was = self.is2d;
        if rz != 0.0 {
            self.rotate_axis(0.0, 0.0, 1.0, rz);
        }
        if ry != 0.0 {
            self.rotate_axis(0.0, 1.0, 0.0, ry);
        }
        if rx != 0.0 {
            self.rotate_axis(1.0, 0.0, 0.0, rx);
        }
        self.is2d = was;
    }
}

// ------------------------------------------------------------------------------------------ the string syntax

enum Fail {
    Syntax,
    Relative,
}

struct Parser<'a> {
    s: &'a [u8],
    p: usize,
    /// A length in `em`, `%` or the like was met.
    relative: bool,
}

#[derive(Clone, Copy)]
enum Unit {
    None,
    Len(f64),
    Pct,
    Rel,
    Angle(f64),
}

impl Parser<'_> {
    fn skip(&mut self) {
        loop {
            match self.s.get(self.p) {
                Some(b' ' | b'\t' | b'\n' | b'\r' | 0x0c) => self.p += 1,
                Some(b'/') if self.s.get(self.p + 1) == Some(&b'*') => {
                    self.p += 2;
                    while self.p < self.s.len() && !self.s[self.p..].starts_with(b"*/") {
                        self.p += 1;
                    }
                    self.p = (self.p + 2).min(self.s.len());
                }
                _ => return,
            }
        }
    }

    fn ident(&mut self) -> &str {
        let from = self.p;
        while self.s.get(self.p).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'-' || *c == b'_') {
            self.p += 1;
        }
        std::str::from_utf8(&self.s[from..self.p]).unwrap_or("")
    }

    /// A number and its unit.
    fn value(&mut self) -> Result<(f64, Unit), Fail> {
        let from = self.p;
        let at = |p: usize| self.s.get(p).copied();
        let mut p = self.p;
        if matches!(at(p), Some(b'+' | b'-')) {
            p += 1;
        }
        let digits = |mut p: usize| {
            while at(p).is_some_and(|c| c.is_ascii_digit()) {
                p += 1;
            }
            p
        };
        let int_end = digits(p);
        let mut end = int_end;
        let mut any = int_end > p;
        if at(end) == Some(b'.') && at(end + 1).is_some_and(|c| c.is_ascii_digit()) {
            end = digits(end + 1);
            any = true;
        }
        if !any {
            return Err(Fail::Syntax);
        }
        if matches!(at(end), Some(b'e' | b'E')) {
            let mut q = end + 1;
            if matches!(at(q), Some(b'+' | b'-')) {
                q += 1;
            }
            if at(q).is_some_and(|c| c.is_ascii_digit()) {
                end = digits(q);
            }
        }
        let text = std::str::from_utf8(&self.s[from..end]).map_err(|_| Fail::Syntax)?;
        let n: f64 = text.parse().map_err(|_| Fail::Syntax)?;
        self.p = end;
        let unit = if at(end) == Some(b'%') {
            self.p += 1;
            Unit::Pct
        } else {
            let u = self.ident().to_ascii_lowercase();
            match u.as_str() {
                "" => Unit::None,
                "px" => Unit::Len(1.0),
                "cm" => Unit::Len(96.0 / 2.54),
                "mm" => Unit::Len(96.0 / 25.4),
                "q" => Unit::Len(96.0 / 101.6),
                "in" => Unit::Len(96.0),
                "pt" => Unit::Len(96.0 / 72.0),
                "pc" => Unit::Len(16.0),
                "deg" => Unit::Angle(1.0),
                "grad" => Unit::Angle(0.9),
                "rad" => Unit::Angle(180.0 / PI),
                "turn" => Unit::Angle(360.0),
                "em" | "ex" | "ch" | "rem" | "cap" | "ic" | "lh" | "rlh" | "vw" | "vh" | "vi" | "vb" | "vmin" | "vmax" | "svw" | "svh" | "svi" | "svb" | "lvw" | "lvh" | "lvi" | "lvb" | "dvw" | "dvh" | "dvi" | "dvb" | "cqw" | "cqh" | "cqi"
                | "cqb" | "cqmin" | "cqmax" => Unit::Rel,
                _ => return Err(Fail::Syntax),
            }
        };
        Ok((n, unit))
    }

    /// The arguments of a function, up to `)` or the end of the text.
    fn args(&mut self) -> Result<Vec<(f64, Unit)>, Fail> {
        let mut out = Vec::new();
        loop {
            self.skip();
            out.push(self.value()?);
            self.skip();
            match self.s.get(self.p) {
                Some(b',') => self.p += 1,
                Some(b')') => {
                    self.p += 1;
                    return Ok(out);
                }
                None => return Ok(out),
                _ => return Err(Fail::Syntax),
            }
        }
    }

    fn len(&mut self, (n, u): (f64, Unit), pct: bool) -> Result<f64, Fail> {
        match u {
            Unit::None if n == 0.0 => Ok(0.0),
            Unit::Len(f) => Ok(((n * f) as f32 as f64).clamp(-33554430.0, 33554428.0)),
            Unit::Pct | Unit::Rel if pct || matches!(u, Unit::Rel) => {
                self.relative = true;
                Ok(0.0)
            }
            _ => Err(Fail::Syntax),
        }
    }

    fn angle((n, u): (f64, Unit)) -> Result<f64, Fail> {
        match u {
            Unit::None if n == 0.0 => Ok(0.0),
            Unit::Angle(f) => Ok(n.clamp(-(f32::MAX as f64), f32::MAX as f64) * f),
            _ => Err(Fail::Syntax),
        }
    }

    fn scalar((n, u): (f64, Unit)) -> Result<f64, Fail> {
        match u {
            Unit::None => Ok(n.clamp(-(f32::MAX as f64), f32::MAX as f64) as f32 as f64),
            _ => Err(Fail::Syntax),
        }
    }

    fn plain((n, u): (f64, Unit)) -> Result<f64, Fail> {
        match u {
            Unit::None => Ok(n.clamp(-(f32::MAX as f64), f32::MAX as f64)),
            _ => Err(Fail::Syntax),
        }
    }

    fn function(&mut self, acc: &mut Mat) -> Result<(), Fail> {
        let name = self.ident().to_ascii_lowercase();
        if self.s.get(self.p) != Some(&b'(') {
            return Err(Fail::Syntax);
        }
        self.p += 1;
        let a = self.args()?;
        let n = a.len();
        match (name.as_str(), n) {
            ("matrix", 6) => {
                let v = a.iter().map(|&x| Self::plain(x)).collect::<Result<Vec<_>, _>>()?;
                *acc = acc.mul(&Mat::affine([v[0], v[1], v[2], v[3], v[4], v[5]], acc.rw));
            }
            ("matrix3d", 16) => {
                let v = a.iter().map(|&x| Self::plain(x)).collect::<Result<Vec<_>, _>>()?;
                let mut m = Mat { m: [0.0; 16], is2d: false, rw: acc.rw };
                m.m.copy_from_slice(&v);
                *acc = acc.mul(&m);
                acc.is2d = false;
            }
            ("translate", 1 | 2) => {
                let x = self.len(a[0], true)?;
                let y = if n == 2 { self.len(a[1], true)? } else { 0.0 };
                acc.translate(x, y, 0.0);
            }
            ("translatex", 1) => {
                let x = self.len(a[0], true)?;
                acc.translate(x, 0.0, 0.0);
            }
            ("translatey", 1) => {
                let y = self.len(a[0], true)?;
                acc.translate(0.0, y, 0.0);
            }
            ("translatez", 1) => {
                let z = self.len(a[0], false)?;
                acc.translate(0.0, 0.0, z);
                acc.is2d = false;
            }
            ("translate3d", 3) => {
                let (x, y, z) = (self.len(a[0], true)?, self.len(a[1], true)?, self.len(a[2], false)?);
                acc.translate(x, y, z);
                acc.is2d = false;
            }
            ("scale", 1 | 2) => {
                let x = Self::scalar(a[0])?;
                let y = if n == 2 { Self::scalar(a[1])? } else { x };
                acc.scale(x, y, 1.0);
            }
            ("scalex", 1) => acc.scale(Self::scalar(a[0])?, 1.0, 1.0),
            ("scaley", 1) => acc.scale(1.0, Self::scalar(a[0])?, 1.0),
            ("scalez", 1) => {
                acc.scale(1.0, 1.0, Self::scalar(a[0])?);
                acc.is2d = false;
            }
            ("scale3d", 3) => {
                acc.scale(Self::scalar(a[0])?, Self::scalar(a[1])?, Self::scalar(a[2])?);
                acc.is2d = false;
            }
            ("rotate" | "rotatez", 1) => acc.rotate_axis(0.0, 0.0, 1.0, Self::angle(a[0])?),
            ("rotatex", 1) => acc.rotate_axis(1.0, 0.0, 0.0, Self::angle(a[0])?),
            ("rotatey", 1) => acc.rotate_axis(0.0, 1.0, 0.0, Self::angle(a[0])?),
            ("rotate3d", 4) => {
                let (x, y, z) = (Self::scalar(a[0])?, Self::scalar(a[1])?, Self::scalar(a[2])?);
                acc.rotate_axis(x, y, z, Self::angle(a[3])?);
                acc.is2d = false;
            }
            ("skew", 1 | 2) => {
                let tx = tan(Self::angle(a[0])? * PI / 180.0);
                let ty = if n == 2 { tan(Self::angle(a[1])? * PI / 180.0) } else { 0.0 };
                *acc = acc.mul(&Mat::affine([1.0, ty, tx, 1.0, 0.0, 0.0], acc.rw));
            }
            ("skewx", 1) => acc.skew_x(Self::angle(a[0])?),
            ("skewy", 1) => acc.skew_y(Self::angle(a[0])?),
            ("perspective", 1) => {
                if a[0].0 < 0.0 {
                    return Err(Fail::Syntax);
                }
                let p = self.len(a[0], false)?;
                let mut m = Mat::new(acc.rw);
                m.is2d = false;
                if p != 0.0 {
                    m.m[11] = -1.0 / p;
                }
                *acc = acc.mul(&m);
                acc.is2d = false;
            }
            _ => return Err(Fail::Syntax),
        }
        Ok(())
    }
}

/// The matrix of a CSS transform list.
fn parse(text: &str, rw: bool) -> Result<Mat, Fail> {
    let mut acc = Mat::new(rw);
    if text.is_empty() {
        return Ok(acc);
    }
    let mut p = Parser { s: text.as_bytes(), p: 0, relative: false };
    p.skip();
    if p.p >= p.s.len() {
        return Err(Fail::Syntax);
    }
    if p.s[p.p..].len() >= 4 && p.s[p.p..p.p + 4].eq_ignore_ascii_case(b"none") && !p.s.get(p.p + 4).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'-' || *c == b'(') {
        p.p += 4;
        p.skip();
        return if p.p >= p.s.len() { Ok(acc) } else { Err(Fail::Syntax) };
    }
    loop {
        p.function(&mut acc)?;
        p.skip();
        if p.p >= p.s.len() {
            break;
        }
    }
    if p.relative { Err(Fail::Relative) } else { Ok(acc) }
}

// ------------------------------------------------------------------------------------------ the objects

fn ensure(d: &Dom, it: &mut Interp<'_>) -> R<[Iface; 2]> {
    if let Some(m) = d.geom.m.get() {
        return Ok(*m);
    }
    make_all(d, it)
}

/// The names of the 22 number attributes, in order: a..f, then m11..m44.
const ATTRS: [&str; 22] = [
    "a", "b", "c", "d", "e", "f", "m11", "m12", "m13", "m14", "m21", "m22", "m23", "m24", "m31", "m32", "m33", "m34", "m41", "m42", "m43", "m44",
];

/// The place in the 16 numbers of attribute `k`.
const fn at(k: usize) -> usize {
    if k < 6 { AFFINE[k] } else { k - 6 }
}

macro_rules! handlers {
    ($f:ident; $($k:literal)*) => { [$($f::<$k>),*] };
}

#[cold]
#[inline(never)]
fn make_all(d: &Dom, it: &mut Interp<'_>) -> R<[Iface; 2]> {
    let base = it.object_prototype();
    let ro = make(d, it, base, "DOMMatrixReadOnly", ro_ctor, true)?;
    let rw = make(d, it, ro.proto, "DOMMatrix", rw_ctor, true)?;
    it.heap.set_prototype_of(rw.ctor, Some(ro.ctor)).map_err(|e| it.heap_error(e))?;
    method(d, it, ro.ctor, "fromFloat32Array", 1, from_array::<false, false>)?;
    method(d, it, ro.ctor, "fromFloat64Array", 1, from_array::<false, true>)?;
    method(d, it, ro.ctor, "fromMatrix", 0, from_matrix::<false>)?;
    method(d, it, rw.ctor, "fromFloat32Array", 1, from_array::<true, false>)?;
    method(d, it, rw.ctor, "fromFloat64Array", 1, from_array::<true, true>)?;
    method(d, it, rw.ctor, "fromMatrix", 0, from_matrix::<true>)?;
    let getters: [H; 22] = handlers!(get; 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21);
    let setters: [H; 22] = handlers!(set; 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21);
    for (k, name) in ATTRS.into_iter().enumerate() {
        d.def_get(it, ro.proto, name, getters[k], None)?;
    }
    d.def_get(it, ro.proto, "is2D", get_is_2d, None)?;
    d.def_get(it, ro.proto, "isIdentity", get_is_identity, None)?;
    let ops: [(&str, u32, H); 17] = [
        ("flipX", 0, flip_x),
        ("flipY", 0, flip_y),
        ("inverse", 0, op::<false, INVERT>),
        ("multiply", 0, op::<false, MULTIPLY>),
        ("rotate", 0, op::<false, ROTATE>),
        ("rotateAxisAngle", 0, op::<false, ROTATE_AXIS>),
        ("rotateFromVector", 0, op::<false, ROTATE_VECTOR>),
        ("scale", 0, op::<false, SCALE>),
        ("scale3d", 0, op::<false, SCALE3D>),
        ("scaleNonUniform", 0, scale_non_uniform),
        ("skewX", 0, op::<false, SKEW_X>),
        ("skewY", 0, op::<false, SKEW_Y>),
        ("toFloat32Array", 0, to_array::<false>),
        ("toFloat64Array", 0, to_array::<true>),
        ("toJSON", 0, to_json),
        ("transformPoint", 0, transform_point),
        ("translate", 0, op::<false, TRANSLATE>),
    ];
    for (n, len, h) in ops {
        method(d, it, ro.proto, n, len, h)?;
    }
    for (k, name) in ATTRS.into_iter().enumerate() {
        d.def_get(it, rw.proto, name, getters[k], Some(setters[k]))?;
    }
    let selves: [(&str, u32, H); 11] = [
        ("invertSelf", 0, op::<true, INVERT>),
        ("multiplySelf", 0, op::<true, MULTIPLY>),
        ("preMultiplySelf", 0, op::<true, PRE_MULTIPLY>),
        ("rotateAxisAngleSelf", 0, op::<true, ROTATE_AXIS>),
        ("rotateFromVectorSelf", 0, op::<true, ROTATE_VECTOR>),
        ("rotateSelf", 0, op::<true, ROTATE>),
        ("scale3dSelf", 0, op::<true, SCALE3D>),
        ("scaleSelf", 0, op::<true, SCALE>),
        ("skewXSelf", 0, op::<true, SKEW_X>),
        ("skewYSelf", 0, op::<true, SKEW_Y>),
        ("translateSelf", 0, op::<true, TRANSLATE>),
    ];
    for (n, len, h) in selves {
        method(d, it, rw.proto, n, len, h)?;
    }
    finish(d, it, ro, "DOMMatrixReadOnly", &[("toString", 0, to_string)])?;
    finish(d, it, rw, "DOMMatrix", &[("setMatrixValue", 1, set_matrix_value)])?;
    let m = [ro, rw];
    let _ = d.geom.m.set(m);
    Ok(m)
}

fn ensure_unit(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    ensure(d, it).map(|_| ())
}

lazy_global!(lazy_ro, "DOMMatrixReadOnly", ensure_unit, |d| d.geom.m.get().expect("made")[0].ctor);
lazy_global!(lazy_rw, "DOMMatrix", ensure_unit, |d| d.geom.m.get().expect("made")[1].ctor);

#[inline(never)]
pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    browser_lazy::install(d, it, &[("DOMMatrixReadOnly", lazy_ro as H), ("DOMMatrix", lazy_rw)])
}

fn mat_of(d: &Dom, this: Value) -> Option<Mat> {
    let Value::Object(o) = this else { return None };
    match d.geom.slots.borrow().get(&o) {
        Some(Slot::Matrix(m)) => Some(**m),
        _ => None,
    }
}

fn new_matrix(d: &Dom, it: &mut Interp<'_>, m: Mat) -> R<ObjId> {
    let proto = ensure(d, it)?[m.rw as usize].proto;
    new_obj(d, it, proto, Slot::Matrix(Box::new(m)))
}

/// The matrix of the constructor argument: undefined (identity), a string, or a sequence of 6 or 16 numbers.
fn ctor(d: &Dom, it: &mut Interp<'_>, a: &[Value], rw: bool) -> V {
    let who = if rw { "Failed to construct 'DOMMatrix'" } else { "Failed to construct 'DOMMatrixReadOnly'" };
    let v = arg(a, 0);
    let mut m = match v {
        Value::Undefined => Mat::new(rw),
        _ => {
            let seq = match v {
                Value::Object(_) => it.host_iter_open(v)?,
                _ => None,
            };
            match seq {
                Some(mut iter) => {
                    let mut xs = Vec::new();
                    loop {
                        match it.host_iter_next(&mut iter) {
                            Ok(Some(x)) => match num(it, x, &|| who.to_string()) {
                                Ok(n) => xs.push(n),
                                Err(e) => return Err(it.host_iter_fail(&mut iter, e)),
                            },
                            Ok(None) => break,
                            Err(e) => return Err(e),
                        }
                    }
                    from_numbers(it, &xs, rw, who)?
                }
                None => {
                    let s = d.string_arg(it, v)?;
                    parse_text(d, it, &s, rw, who)?
                }
            }
        }
    };
    m.rw = rw;
    let default = ensure(d, it)?[rw as usize].proto;
    let proto = it.host_proto_from_new_target(default)?;
    Ok(Value::Object(new_obj(d, it, proto, Slot::Matrix(Box::new(m)))?))
}

pub(crate) fn ro_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    ctor(d, it, a, false)
}

pub(crate) fn rw_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    ctor(d, it, a, true)
}

fn from_numbers(it: &mut Interp<'_>, xs: &[f64], rw: bool, who: &str) -> R<Mat> {
    match xs.len() {
        6 => Ok(Mat::affine([xs[0], xs[1], xs[2], xs[3], xs[4], xs[5]], rw)),
        16 => {
            let mut m = Mat { m: [0.0; 16], is2d: false, rw };
            m.m.copy_from_slice(xs);
            Ok(m)
        }
        _ => Err(it.type_error(&format!("{who}: The sequence must contain 6 elements for a 2D matrix or 16 elements for a 3D matrix."))),
    }
}

fn parse_text(d: &Dom, it: &mut Interp<'_>, s: &str, rw: bool, who: &str) -> R<Mat> {
    match parse(s, rw) {
        Ok(m) => Ok(m),
        Err(Fail::Syntax) => Err(d.dom_err(it, "SyntaxError", &format!("{who}: Failed to parse '{s}'."))),
        Err(Fail::Relative) => Err(d.dom_err(it, "SyntaxError", &format!("{who}: Values must be resolvable at parse time"))),
    }
}

/// The matrix of a `DOMMatrixInit`: the members are read in the order of Chrome and checked ("validate and fixup").
pub(crate) fn init_matrix(d: &Dom, it: &mut Interp<'_>, v: Value, who: &str) -> R<Mat> {
    init_mat(d, it, v, who, false)
}

fn init_mat(d: &Dom, it: &mut Interp<'_>, v: Value, who: &str, rw: bool) -> R<Mat> {
    let names = ["a", "b", "c", "d", "e", "f", "m11", "m12", "m21", "m22", "m41", "m42", "is2D", "m13", "m14", "m23", "m24", "m31", "m32", "m33", "m34", "m43", "m44"];
    // `is2D` (member 12) is a boolean; the other members are numbers.
    let dict = |i: usize| if i < 12 { "DOMMatrix2DInit" } else { "DOMMatrixInit" };
    let mut is2d_given: Option<bool> = None;
    let mut r: [Option<f64>; 23] = [None; 23];
    match v {
        Value::Undefined | Value::Null => {}
        Value::Object(_) => {
            for (i, name) in names.into_iter().enumerate() {
                let k = d.key(it, name)?;
                let x = it.get_value(v, k)?;
                if matches!(x, Value::Undefined) {
                    continue;
                }
                if i == 12 {
                    is2d_given = Some(it.to_js_bool(x)?);
                } else {
                    r[i] = Some(num(it, x, &|| format!("{who}: Failed to read the '{name}' property from '{}'", dict(i)))?);
                }
            }
        }
        _ => return Err(it.type_error(&format!("{who}: The provided value is not of type 'DOMMatrixInit'."))),
    }
    let same = |a: Option<f64>, b: Option<f64>| match (a, b) {
        (Some(x), Some(y)) => x == y || (x.is_nan() && y.is_nan()),
        _ => true,
    };
    // (a, m11), (b, m12), (c, m21), (d, m22), (e, m41), (f, m42)
    let pairs = [(0, 6), (1, 7), (2, 8), (3, 9), (4, 10), (5, 11)];
    if pairs.iter().any(|&(x, y)| !same(r[x], r[y])) {
        return Err(it.type_error(&format!("{who}: Property mismatch on matrix initialization.")));
    }
    let pick = |x: usize, y: usize, default: f64| r[y].or(r[x]).unwrap_or(default);
    let mut m = Mat::new(rw);
    m.m[0] = pick(0, 6, 1.0);
    m.m[1] = pick(1, 7, 0.0);
    m.m[4] = pick(2, 8, 0.0);
    m.m[5] = pick(3, 9, 1.0);
    m.m[12] = pick(4, 10, 0.0);
    m.m[13] = pick(5, 11, 0.0);
    // m13, m14, m23, m24, m31, m32, m33, m34, m43, m44 are members 13..22.
    for (member, place, default) in [(13, 2, 0.0), (14, 3, 0.0), (15, 6, 0.0), (16, 7, 0.0), (17, 8, 0.0), (18, 9, 0.0), (19, 10, 1.0), (20, 11, 0.0), (21, 14, 0.0), (22, 15, 1.0)] {
        m.m[place] = r[member].unwrap_or(default);
    }
    let flat = [2, 3, 6, 7, 8, 9, 11, 14].iter().all(|&i| m.m[i] == 0.0) && m.m[10] == 1.0 && m.m[15] == 1.0;
    m.is2d = match is2d_given {
        Some(true) if !flat => return Err(it.type_error(&format!("{who}: The is2D member is set to true but the input matrix is a 3d matrix."))),
        Some(b) => b,
        None => flat,
    };
    Ok(m)
}

// The attributes.

fn get<const K: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    match mat_of(d, this) {
        Some(m) => Ok(Value::Number(m.m[at(K)])),
        None => Err(illegal(it)),
    }
}

fn set<const K: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let Some(Mat { rw: true, .. }) = mat_of(d, this) else { return Err(illegal(it)) };
    let x = num(it, arg(a, 0), &|| format!("Failed to set the '{}' property on 'DOMMatrix'", ATTRS[K]))?;
    if let Value::Object(o) = this
        && let Some(Slot::Matrix(m)) = d.geom.slots.borrow_mut().get_mut(&o)
    {
        let i = at(K);
        m.m[i] = x;
        let off = if matches!(i, 10 | 15) { x != 1.0 } else { matches!(i, 2 | 3 | 6 | 7 | 8 | 9 | 11 | 14) && x != 0.0 };
        if off {
            m.is2d = false;
        }
    }
    Ok(Value::Undefined)
}

fn get_is_2d(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    mat_of(d, this).map(|m| Value::Bool(m.is2d)).ok_or_else(|| illegal(it))
}

fn get_is_identity(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    mat_of(d, this).map(|m| Value::Bool(m.is_identity())).ok_or_else(|| illegal(it))
}

// The operations. `SELF`: the mutating form (`translateSelf`), else a new `DOMMatrix`.

const TRANSLATE: u8 = 0;
const SCALE: u8 = 1;
const SCALE3D: u8 = 2;
const ROTATE: u8 = 3;
const ROTATE_AXIS: u8 = 4;
const ROTATE_VECTOR: u8 = 5;
const SKEW_X: u8 = 6;
const SKEW_Y: u8 = 7;
const INVERT: u8 = 8;
const MULTIPLY: u8 = 9;
const PRE_MULTIPLY: u8 = 10;

/// The number at `a[i]`, or `default` when it is undefined.
fn opt(it: &mut Interp<'_>, a: &[Value], i: usize, who: &dyn Fn() -> String) -> R<Option<f64>> {
    match arg(a, i) {
        Value::Undefined => Ok(None),
        x => Ok(Some(num(it, x, who)?)),
    }
}

/// Keep the result: into the object (`SELF`), or into a new `DOMMatrix`.
fn done(d: &Dom, it: &mut Interp<'_>, this: Value, m: Mat, is_self: bool) -> V {
    if is_self {
        if let Value::Object(o) = this
            && let Some(Slot::Matrix(slot)) = d.geom.slots.borrow_mut().get_mut(&o)
        {
            **slot = m;
        }
        return Ok(this);
    }
    let mut m = m;
    m.rw = true;
    Ok(Value::Object(new_matrix(d, it, m)?))
}

fn op<const SELF: bool, const OP: u8>(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let Some(mut m) = mat_of(d, this) else { return Err(illegal(it)) };
    if SELF && !m.rw {
        return Err(illegal(it));
    }
    let name = match OP {
        TRANSLATE => "translate",
        SCALE => "scale",
        SCALE3D => "scale3d",
        ROTATE => "rotate",
        ROTATE_AXIS => "rotateAxisAngle",
        ROTATE_VECTOR => "rotateFromVector",
        SKEW_X => "skewX",
        SKEW_Y => "skewY",
        INVERT => "inverse",
        MULTIPLY => "multiply",
        _ => "preMultiplySelf",
    };
    let iface = if SELF { "DOMMatrix" } else { "DOMMatrixReadOnly" };
    let suffix = if SELF && OP != PRE_MULTIPLY { "Self" } else { "" };
    let who = || format!("Failed to execute '{name}{suffix}' on '{iface}'");
    let mut n = |i: usize, default: f64| -> R<f64> { Ok(opt(it, a, i, &who)?.unwrap_or(default)) };
    match OP {
        TRANSLATE => {
            let (x, y, z) = (n(0, 0.0)?, n(1, 0.0)?, n(2, 0.0)?);
            m.translate(x, y, z);
        }
        SCALE => {
            let sx = n(0, 1.0)?;
            let sy = opt(it, a, 1, &who)?;
            let sz = opt(it, a, 2, &who)?.unwrap_or(1.0);
            let o = [opt(it, a, 3, &who)?.unwrap_or(0.0), opt(it, a, 4, &who)?.unwrap_or(0.0), opt(it, a, 5, &who)?.unwrap_or(0.0)];
            // `scaleY` has no default: an undefined one is NaN when a later argument is given, else it is `scaleX`.
            let sy = sy.unwrap_or(if a.iter().skip(2).any(|v| !matches!(v, Value::Undefined)) { f64::NAN } else { sx });
            if sz != 1.0 || o[2] != 0.0 {
                m.is2d = false;
            }
            m.scale_about(sx, sy, sz, o);
        }
        SCALE3D => {
            let s = n(0, 1.0)?;
            let o = [n(1, 0.0)?, n(2, 0.0)?, n(3, 0.0)?];
            if s != 1.0 || o[2] != 0.0 {
                m.is2d = false;
            }
            m.scale_about(s, s, s, o);
        }
        ROTATE => {
            let (x, y, z) = (opt(it, a, 0, &who)?, opt(it, a, 1, &who)?, opt(it, a, 2, &who)?);
            // `rotY` has no default: an undefined one is NaN when `rotZ` is given. With neither, the angle turns about z.
            let (rx, ry, rz) = match (y, z) {
                (None, None) => (0.0, 0.0, x.unwrap_or(0.0)),
                (None, Some(_)) => (x.unwrap_or(0.0), f64::NAN, z.unwrap_or(0.0)),
                _ => (x.unwrap_or(0.0), y.unwrap_or(0.0), z.unwrap_or(0.0)),
            };
            m.rotate(rx, ry, rz);
        }
        ROTATE_AXIS => {
            let (x, y, z, deg) = (n(0, 0.0)?, n(1, 0.0)?, n(2, 0.0)?, n(3, 0.0)?);
            m.rotate_axis(x, y, z, deg);
        }
        ROTATE_VECTOR => {
            let (x, y) = (n(0, 0.0)?, n(1, 0.0)?);
            m.rotate_axis(0.0, 0.0, 1.0, atan2(y, x) * 180.0 / PI);
        }
        SKEW_X => m.skew_x(n(0, 0.0)?),
        SKEW_Y => m.skew_y(n(0, 0.0)?),
        INVERT => m.invert(),
        _ => {
            let other = init_mat(d, it, arg(a, 0), &who(), false)?;
            m = if OP == MULTIPLY { m.mul(&other) } else { other.mul(&m) };
            m.rw = true;
        }
    }
    done(d, it, this, m, SELF)
}

fn flip_x(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let Some(mut m) = mat_of(d, this) else { return Err(illegal(it)) };
    m.scale(-1.0, 1.0, 1.0);
    done(d, it, this, m, false)
}

fn flip_y(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let Some(mut m) = mat_of(d, this) else { return Err(illegal(it)) };
    m.scale(1.0, -1.0, 1.0);
    done(d, it, this, m, false)
}

fn scale_non_uniform(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let Some(mut m) = mat_of(d, this) else { return Err(illegal(it)) };
    let who = || "Failed to execute 'scaleNonUniform' on 'DOMMatrixReadOnly'".to_string();
    let (x, y) = (opt(it, a, 0, &who)?.unwrap_or(1.0), opt(it, a, 1, &who)?.unwrap_or(1.0));
    m.scale(x, y, 1.0);
    done(d, it, this, m, false)
}

fn transform_point(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let Some(m) = mat_of(d, this) else { return Err(illegal(it)) };
    let p = point_init(d, it, arg(a, 0), "Failed to execute 'transformPoint' on 'DOMMatrixReadOnly'")?;
    Ok(Value::Object(point_object(d, it, m.transform(p), true)?))
}

fn to_array<const F64: bool>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let Some(m) = mat_of(d, this) else { return Err(illegal(it)) };
    if F64 {
        it.new_typed_array(Elem::F64, m.m.iter().flat_map(|v| v.to_le_bytes()).collect())
    } else {
        it.new_typed_array(Elem::F32, m.m.iter().flat_map(|v| (*v as f32).to_le_bytes()).collect())
    }
}

fn to_json(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let Some(m) = mat_of(d, this) else { return Err(illegal(it)) };
    let o = d.obj(it, it.object_prototype())?;
    for (k, name) in ATTRS.into_iter().enumerate() {
        put(d, it, o, name, Value::Number(m.m[at(k)]))?;
    }
    put(d, it, o, "is2D", Value::Bool(m.is2d))?;
    put(d, it, o, "isIdentity", Value::Bool(m.is_identity()))?;
    Ok(Value::Object(o))
}

fn to_string(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let Some(m) = mat_of(d, this) else { return Err(illegal(it)) };
    if !m.finite() {
        return Err(d.dom_err(it, "InvalidStateError", "Failed to execute 'toString' on 'DOMMatrixReadOnly': DOMMatrix cannot be serialized with NaN or Infinity values."));
    }
    let list = |v: &mut dyn Iterator<Item = f64>| v.map(number_to_string).collect::<Vec<_>>().join(", ");
    let s = if m.is2d { format!("matrix({})", list(&mut AFFINE.iter().map(|&i| m.m[i]))) } else { format!("matrix3d({})", list(&mut m.m.iter().copied())) };
    d.js_str(it, &s)
}

fn set_matrix_value(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let Some(Mat { rw: true, .. }) = mat_of(d, this) else { return Err(illegal(it)) };
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'setMatrixValue' on 'DOMMatrix': 1 argument required, but only 0 present."));
    }
    let s = d.string_arg(it, a[0])?;
    let m = parse_text(d, it, &s, true, "Failed to execute 'setMatrixValue' on 'DOMMatrix'")?;
    done(d, it, this, m, true)
}

// The statics.

fn from_matrix<const RW: bool>(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let who = if RW { "Failed to execute 'fromMatrix' on 'DOMMatrix'" } else { "Failed to execute 'fromMatrix' on 'DOMMatrixReadOnly'" };
    let mut m = init_mat(d, it, arg(a, 0), who, RW)?;
    m.rw = RW;
    Ok(Value::Object(new_matrix(d, it, m)?))
}

fn from_array<const RW: bool, const F64: bool>(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let (name, ty) = if F64 { ("fromFloat64Array", Elem::F64) } else { ("fromFloat32Array", Elem::F32) };
    let class = if F64 { "Float64Array" } else { "Float32Array" };
    let who = format!("Failed to execute '{name}' on '{}'", if RW { "DOMMatrix" } else { "DOMMatrixReadOnly" });
    let Some(&v) = a.first() else { return Err(it.type_error(&format!("{who}: 1 argument required, but only 0 present."))) };
    let view = match v {
        Value::Object(o) if it.heap.view_type(o) == Some(ty) => o,
        _ => return Err(it.type_error(&format!("{who}: parameter 1 is not of type '{class}'."))),
    };
    let len = it.heap.virtual_len(view).map_err(|e| it.heap_error(e))?;
    let xs: Vec<f64> = (0..len).map(|i| it.heap.view_load(view, i).unwrap_or(0.0)).collect();
    let mut m = from_numbers(it, &xs, RW, &who)?;
    m.rw = RW;
    Ok(Value::Object(new_matrix(d, it, m)?))
}
