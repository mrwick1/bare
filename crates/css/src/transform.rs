//! The `transform` function list as one 2D affine matrix.
//!
//! A plain move (`translateX(10px)`, units that agree, not zero) stays in `TranslateX` and `TranslateY`: they are two `Length` values, and
//! an element with no transform pays nothing. Any other list (`rotate`, `scale`, `skew`, `matrix`, a zero move, a mix of units) is a
//! `Grid::Transform` in the side list of the grids (see `grid.rs`), and `TranslateX` points to it. `TranslateY` is then 0.
//!
//! The list is kept as a short list of steps, not as a matrix: a percent in `translate()` is of the border box, which is known only after
//! layout, and a step after a rotation mixes the width and the height into the move. `resolve` gives the matrix for a box size.
//! Steps with no percent and no unit that needs the font (`em`) fold into one matrix as the list is read.
//!
//! Left out: a real 3D transform (`rotateX`, `rotateY`, `perspective`, a `translateZ` that is not 0, `matrix3d` that is not flat) is a good
//! value that does nothing, as before. `resolve` gives `None` for it: Chrome writes `matrix3d(...)` and we do not.

use crate::grid::{len_tag, len_value};
use crate::values::Length;
use std::hash::{Hash, Hasher};

/// One step of a transform list.
#[derive(Debug, Clone, Copy)]
pub enum Func {
    /// A move. A length or a percent after the cascade.
    Translate(Length, Length),
    /// `a b c d e f` as in CSS `matrix()`: x' = a x + c y + e, y' = b x + d y + f.
    Matrix([f32; 6]),
    /// A 3D step that has no 2D matrix.
    Solid,
}

// The numbers come from the parser, so they are never NaN: `==` is a good equality.
impl PartialEq for Func {
    fn eq(&self, other: &Func) -> bool {
        match (self, other) {
            (Func::Translate(a, b), Func::Translate(c, d)) => a == c && b == d,
            (Func::Matrix(a), Func::Matrix(b)) => a == b,
            (Func::Solid, Func::Solid) => true,
            _ => false,
        }
    }
}
impl Eq for Func {}

impl Hash for Func {
    fn hash<H: Hasher>(&self, h: &mut H) {
        match *self {
            Func::Translate(x, y) => {
                (0u8, len_tag(x), len_tag(y), len_value(x).to_bits(), len_value(y).to_bits()).hash(h);
            }
            Func::Matrix(m) => {
                1u8.hash(h);
                m.map(f32::to_bits).hash(h);
            }
            Func::Solid => 2u8.hash(h),
        }
    }
}

const ID: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `a * b`: `b` first, then `a`.
fn mul(a: [f64; 6], b: [f64; 6]) -> [f64; 6] {
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}

/// The matrix of the list for a border box of `w` by `h` pixels. `None` for a list with a 3D step.
pub fn resolve(list: &[Func], w: f32, h: f32) -> Option<[f32; 6]> {
    let px = |l: Length, of: f32| match l {
        Length::Px(x) => f64::from(x),
        Length::Percent(p) => f64::from(p) * f64::from(of) / 100.0,
        _ => 0.0,
    };
    let mut m = ID;
    for f in list {
        let step = match *f {
            Func::Translate(x, y) => [1.0, 0.0, 0.0, 1.0, px(x, w), px(y, h)],
            Func::Matrix(a) => a.map(f64::from),
            Func::Solid => return None,
        };
        m = mul(m, step);
    }
    Some(m.map(|x| x as f32))
}

/// `sin` and `cos` of an angle in degrees. A multiple of 90 is exact (Chrome writes `0`, not `6.12323e-17`).
fn sin_cos(deg: f64) -> (f64, f64) {
    if deg % 90.0 == 0.0 {
        return match (deg / 90.0).rem_euclid(4.0) as u8 {
            0 => (0.0, 1.0),
            1 => (1.0, 0.0),
            2 => (0.0, -1.0),
            _ => (-1.0, 0.0),
        };
    }
    deg.to_radians().sin_cos()
}

/// An angle as degrees: `45deg`, `0.5turn`, `1rad`, `100grad`, or a bare `0`.
fn angle(t: &str) -> Option<f64> {
    let t = t.trim().to_ascii_lowercase();
    let (n, k) = if let Some(n) = t.strip_suffix("deg") {
        (n, 1.0)
    } else if let Some(n) = t.strip_suffix("grad") {
        (n, 0.9)
    } else if let Some(n) = t.strip_suffix("rad") {
        (n, 180.0 / std::f64::consts::PI)
    } else if let Some(n) = t.strip_suffix("turn") {
        (n, 360.0)
    } else if t == "0" {
        return Some(0.0);
    } else {
        return None;
    };
    n.parse::<f64>().ok().map(|v| v * k).filter(|v| v.is_finite())
}

fn number(t: &str) -> Option<f64> {
    t.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

fn rotate(deg: f64) -> [f64; 6] {
    let (s, c) = sin_cos(deg);
    [c, s, -s, c, 0.0, 0.0]
}

/// One function with its arguments (the text between the commas), or `None` when the name or the arguments are not good. `len` reads
/// a length, a percent or `0`.
pub fn function(name: &str, args: &[&str], len: &mut dyn FnMut(&str) -> Option<Length>) -> Option<Func> {
    let zero = Length::Px(0.0);
    let flat = |m: [f64; 6]| Some(Func::Matrix(m.map(|x| x as f32)));
    let nums = |n: usize| -> Option<Vec<f64>> { (args.len() == n).then(|| args.iter().map(|a| number(a)).collect::<Option<Vec<_>>>()).flatten() };
    // A step in `z` only: nothing in 2D when it is the neutral value.
    let solid = |neutral: bool| Some(if neutral { Func::Translate(zero, zero) } else { Func::Solid });
    match (name, args.len()) {
        ("translatex", 1) => Some(Func::Translate(len(args[0])?, zero)),
        ("translatey", 1) => Some(Func::Translate(zero, len(args[0])?)),
        ("translate", 1) => Some(Func::Translate(len(args[0])?, zero)),
        ("translate", 2) => Some(Func::Translate(len(args[0])?, len(args[1])?)),
        ("translatez", 1) => match len(args[0])? {
            Length::Percent(_) => None,
            z => solid(z == zero),
        },
        ("translate3d", 3) => match len(args[2])? {
            Length::Percent(_) => None,
            z if z == zero => Some(Func::Translate(len(args[0])?, len(args[1])?)),
            _ => Some(Func::Solid),
        },
        ("scale", 1) => nums(1).and_then(|n| flat([n[0], 0.0, 0.0, n[0], 0.0, 0.0])),
        ("scale", 2) => nums(2).and_then(|n| flat([n[0], 0.0, 0.0, n[1], 0.0, 0.0])),
        ("scalex", 1) => nums(1).and_then(|n| flat([n[0], 0.0, 0.0, 1.0, 0.0, 0.0])),
        ("scaley", 1) => nums(1).and_then(|n| flat([1.0, 0.0, 0.0, n[0], 0.0, 0.0])),
        ("scalez", 1) => nums(1).and_then(|n| solid(n[0] == 1.0)),
        ("scale3d", 3) => nums(3).and_then(|n| if n[2] == 1.0 { flat([n[0], 0.0, 0.0, n[1], 0.0, 0.0]) } else { Some(Func::Solid) }),
        ("rotate" | "rotatez", 1) => flat(rotate(angle(args[0])?)),
        ("rotatex" | "rotatey", 1) => solid(angle(args[0])? % 360.0 == 0.0),
        ("rotate3d", 4) => {
            let (x, y, z) = (number(args[0])?, number(args[1])?, number(args[2])?);
            let deg = angle(args[3])?;
            if x == 0.0 && y == 0.0 && z != 0.0 { flat(rotate(deg * z.signum())) } else { Some(Func::Solid) }
        }
        ("skew", 1) => flat([1.0, 0.0, sin_cos_tan(angle(args[0])?)?, 1.0, 0.0, 0.0]),
        ("skew", 2) => flat([1.0, sin_cos_tan(angle(args[1])?)?, sin_cos_tan(angle(args[0])?)?, 1.0, 0.0, 0.0]),
        ("skewx", 1) => flat([1.0, 0.0, sin_cos_tan(angle(args[0])?)?, 1.0, 0.0, 0.0]),
        ("skewy", 1) => flat([1.0, sin_cos_tan(angle(args[0])?)?, 0.0, 1.0, 0.0, 0.0]),
        ("matrix", 6) => nums(6).and_then(|n| flat([n[0], n[1], n[2], n[3], n[4], n[5]])),
        ("matrix3d", 16) => nums(16).map(|n| {
            // Flat when z does not take part: the third row and column are the identity, and w is 1.
            let flat3 = [(2, 0.0), (3, 0.0), (6, 0.0), (7, 0.0), (8, 0.0), (9, 0.0), (10, 1.0), (11, 0.0), (14, 0.0), (15, 1.0)];
            if flat3.iter().all(|&(i, v)| n[i] == v) { Func::Matrix([n[0], n[1], n[4], n[5], n[12], n[13]].map(|x| x as f32)) } else { Func::Solid }
        }),
        ("perspective", 1) => Some(Func::Solid),
        _ => None,
    }
}

/// `tan` of an angle in degrees, or `None` when it is not finite (`skew(90deg)`).
fn sin_cos_tan(deg: f64) -> Option<f64> {
    let (s, c) = sin_cos(deg);
    (c != 0.0).then(|| s / c)
}

/// Join the neighbors that are matrices. A list of many steps with no percent is one matrix.
pub fn fold(list: Vec<Func>) -> Vec<Func> {
    let mut out: Vec<Func> = Vec::with_capacity(list.len());
    for f in list {
        match (out.last_mut(), f) {
            (Some(Func::Matrix(a)), Func::Matrix(b)) => *a = mul(a.map(f64::from), b.map(f64::from)).map(|x| x as f32),
            (Some(Func::Translate(Length::Px(ax), Length::Px(ay))), Func::Translate(Length::Px(bx), Length::Px(by))) => {
                *ax += bx;
                *ay += by;
            }
            _ => out.push(f),
        }
    }
    out
}

/// A number as Chrome writes it in a transform: 6 significant digits, no trailing zeros, no `-0`; `1.23457e+06` and `1e-05` for a number
/// that needs the exponent (it is below 1e-4 (`%g`) or has more than 6 digits before the point).
pub fn num(n: f32) -> String {
    let v = f64::from(n);
    if v == 0.0 {
        return "0".into();
    }
    let s = format!("{v:.5e}");
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..6).contains(&exp) {
        let t = format!("{v:.*}", (5 - exp).clamp(0, 30) as usize);
        return if t.contains('.') { t.trim_end_matches('0').trim_end_matches('.').into() } else { t };
    }
    let mant = if mant.contains('.') { mant.trim_end_matches('0').trim_end_matches('.') } else { mant };
    format!("{mant}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
}

/// `matrix(a, b, c, d, e, f)`.
pub fn text(m: [f32; 6]) -> String {
    format!("matrix({}, {}, {}, {}, {}, {})", num(m[0]), num(m[1]), num(m[2]), num(m[3]), num(m[4]), num(m[5]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn len(t: &str) -> Option<Length> {
        let t = t.trim();
        if t == "0" {
            Some(Length::Px(0.0))
        } else if let Some(p) = t.strip_suffix('%') {
            p.parse().ok().map(Length::Percent)
        } else {
            t.strip_suffix("px")?.parse().ok().map(Length::Px)
        }
    }

    fn list(src: &[(&str, &[&str])]) -> Vec<Func> {
        fold(src.iter().map(|(n, a)| function(n, a, &mut len).unwrap_or_else(|| panic!("{n}"))).collect())
    }

    // The expected texts are from Chrome 14x (headless `getComputedStyle(el).transform` on a 100px by 200px box).
    fn chrome(src: &[(&str, &[&str])]) -> String {
        text(resolve(&list(src), 100.0, 200.0).unwrap())
    }

    #[test]
    fn matrices_as_chrome_writes_them() {
        assert_eq!(chrome(&[("rotate", &["90deg"])]), "matrix(0, 1, -1, 0, 0, 0)");
        assert_eq!(chrome(&[("rotate", &["180deg"])]), "matrix(-1, 0, 0, -1, 0, 0)");
        assert_eq!(chrome(&[("rotate", &["0.25turn"])]), "matrix(0, 1, -1, 0, 0, 0)");
        assert_eq!(chrome(&[("scale", &["2"])]), "matrix(2, 0, 0, 2, 0, 0)");
        assert_eq!(chrome(&[("scale", &["0.5", "2"])]), "matrix(0.5, 0, 0, 2, 0, 0)");
        assert_eq!(chrome(&[("scalex", &["-1"])]), "matrix(-1, 0, 0, 1, 0, 0)");
        assert_eq!(chrome(&[("translate", &["0", "0"])]), "matrix(1, 0, 0, 1, 0, 0)");
        assert_eq!(chrome(&[("translatey", &["50%"])]), "matrix(1, 0, 0, 1, 0, 100)");
        assert_eq!(chrome(&[("translatex", &["10px"]), ("rotate", &["45deg"])]), "matrix(0.707107, 0.707107, -0.707107, 0.707107, 10, 0)");
        assert_eq!(chrome(&[("skewx", &["45deg"])]), "matrix(1, 0, 1, 1, 0, 0)");
        assert_eq!(chrome(&[("matrix", &["1", "2", "3", "4", "5", "6"])]), "matrix(1, 2, 3, 4, 5, 6)");
        assert_eq!(chrome(&[("rotate", &["30deg"]), ("translate", &["10px", "0"])]), "matrix(0.866025, 0.5, -0.5, 0.866025, 8.66025, 5)");
        assert_eq!(chrome(&[("translate", &["50%", "50%"]), ("rotate", &["90deg"])]), "matrix(0, 1, -1, 0, 50, 100)");
        // A percent after a turn: the width goes to the y of the move.
        assert_eq!(chrome(&[("rotate", &["90deg"]), ("translate", &["50%", "0"])]), "matrix(0, 1, -1, 0, 0, 50)");
        assert_eq!(chrome(&[("translatex", &["1234567px"])]), "matrix(1, 0, 0, 1, 1.23457e+06, 0)");
        assert_eq!(chrome(&[("translatex", &["0.00001px"])]), "matrix(1, 0, 0, 1, 1e-05, 0)");
    }

    #[test]
    fn solid_steps() {
        assert_eq!(resolve(&list(&[("rotatex", &["30deg"])]), 1.0, 1.0), None);
        assert_eq!(resolve(&list(&[("perspective", &["100px"])]), 1.0, 1.0), None);
        assert_eq!(resolve(&list(&[("translate3d", &["1px", "2px", "3px"])]), 1.0, 1.0), None);
        // A neutral z is 2D.
        assert_eq!(text(resolve(&list(&[("translate3d", &["1px", "2px", "0"])]), 1.0, 1.0).unwrap()), "matrix(1, 0, 0, 1, 1, 2)");
        assert_eq!(text(resolve(&list(&[("rotatex", &["0deg"])]), 1.0, 1.0).unwrap()), "matrix(1, 0, 0, 1, 0, 0)");
    }
}
