//! `calc()`: arithmetic with numbers and lengths, like `calc(100% - 2 * 10px)`.
//!
//! The units `em`, `rem`, `vw`, `vh`, `vmin`, `vmax` and `%` are not known when we read the style sheet. But `calc()` only adds, subtracts,
//! multiplies by a number and divides by a number. So the result is always a sum of units, each with a number in front. We call
//! that a `Linear`: `100% - 20px` is `{ pct: 100, px: -20 }`. We can do the arithmetic now and keep the sum for the cascade.
//! JS: like turning `a*(b+c)` into `a*b + a*c` before you know `b` and `c`.
//!
//! What we read: numbers, lengths, percents, `+ - * /`, parentheses, and `calc()` inside `calc()`. `+` and `-` need a space on each side.
//! The standard says so, because `1 -2` is two numbers. A sum must add the same kind of things: `1 + 2px` is not good.
//! `x * y` needs a number on one side. `x / y` needs a number that is not 0 on the right.
//!
//! Left out: `min()`, `max()`, `clamp()` and the other math functions, and the constants `pi` and `e`.
//! Two known differences from Chrome. A `calc()` with one unit that is negative (`width: calc(1px - 5px)`) drops the declaration:
//! Chrome keeps it and makes it 0 later. A mix (`calc(100% - 50px)`) is not made 0 when it is negative. And `x / 0` is not good here:
//! Chrome accepts it.

use crate::tokenizer::Token;
use crate::values::Length;

/// The slots of a `Linear`.
const PX: usize = 0;
const EM: usize = 1;
const REM: usize = 2;
const VW: usize = 3;
const VH: usize = 4;
const VMIN: usize = 5;
const VMAX: usize = 6;
const PCT: usize = 7;

/// A sum of units: `{ px: 1, em: 2 }` is `1px + 2em`. 32 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Linear(pub [f32; 8]);

impl Linear {
    pub fn px(self) -> f32 {
        self.0[PX]
    }

    pub fn pct(self) -> f32 {
        self.0[PCT]
    }

    /// The number of pixels of everything except the percent. `em` is `font_size` pixels, `rem` is `root` pixels,
    /// and the window is `(w, h)` pixels.
    pub fn to_px(self, font_size: f32, root: f32, (w, h): (f32, f32)) -> f32 {
        let v = self.0;
        v[PX] + v[EM] * font_size + v[REM] * root + v[VW] * w / 100.0 + v[VH] * h / 100.0 + v[VMIN] * w.min(h) / 100.0 + v[VMAX] * w.max(h) / 100.0
    }

    fn from_length(l: Length) -> Linear {
        let mut v = [0.0; 8];
        match l {
            Length::Px(x) => v[PX] = x,
            Length::Em(x) => v[EM] = x,
            Length::Rem(x) => v[REM] = x,
            Length::Percent(x) => v[PCT] = x,
            Length::Vw(x) => v[VW] = x,
            Length::Vh(x) => v[VH] = x,
            Length::Vmin(x) => v[VMIN] = x,
            Length::Vmax(x) => v[VMAX] = x,
            Length::Calc(_) => {}
        }
        Linear(v)
    }

    fn map(self, f: impl Fn(f32) -> f32) -> Linear {
        Linear(self.0.map(f))
    }

    /// The `Length` that this is, when it is one unit only. `None` for a mix.
    pub fn single(self) -> Option<Length> {
        let make = [Length::Px, Length::Em, Length::Rem, Length::Vw, Length::Vh, Length::Vmin, Length::Vmax, Length::Percent];
        let mut used = self.0.iter().enumerate().filter(|&(_, &x)| x != 0.0);
        match (used.next(), used.next()) {
            (None, _) => Some(Length::Px(0.0)),
            (Some((i, &x)), None) => Some(make[i](x)),
            _ => None,
        }
    }
}

/// What a `calc()` gives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Calc {
    Number(f32),
    Length(Linear),
}

/// A number or a sum of lengths, while we compute. For a number, the value is in the `px` slot.
#[derive(Clone, Copy)]
struct Term {
    value: Linear,
    number: bool,
}

/// Is this the name of `calc`, with or without the old prefix?
pub fn is_calc(name: &str) -> bool {
    name.eq_ignore_ascii_case("calc") || name.eq_ignore_ascii_case("-webkit-calc") || name.eq_ignore_ascii_case("-moz-calc")
}

/// Parentheses inside parentheses go this deep. Deeper is not good. A loop of `(` cannot fill the call stack.
const MAX_DEPTH: usize = 32;

/// Read the tokens inside `calc( ... )`, without the closing `)`.
pub fn parse(tokens: &[Token<'_>], unit: impl Fn(f32, &str) -> Option<Length> + Copy) -> Option<Calc> {
    let mut p = P { tokens, pos: 0, unit };
    let term = p.sum(0)?;
    p.space();
    if p.pos != tokens.len() || !term.value.0.iter().all(|x| x.is_finite()) {
        return None;
    }
    Some(if term.number { Calc::Number(term.value.px()) } else { Calc::Length(term.value) })
}

struct P<'t, 'a, U> {
    tokens: &'t [Token<'a>],
    pos: usize,
    unit: U,
}

impl<U: Fn(f32, &str) -> Option<Length> + Copy> P<'_, '_, U> {
    fn space(&mut self) -> bool {
        let start = self.pos;
        while self.tokens.get(self.pos) == Some(&Token::Whitespace) {
            self.pos += 1;
        }
        self.pos > start
    }

    fn sum(&mut self, depth: usize) -> Option<Term> {
        let mut acc = self.product(depth)?;
        loop {
            let before = self.space();
            let sign = match self.tokens.get(self.pos) {
                Some(Token::Delim('+')) => 1.0,
                Some(Token::Delim('-')) => -1.0,
                _ => return Some(acc),
            };
            self.pos += 1;
            // A sign needs a space on each side.
            if !before || !self.space() {
                return None;
            }
            let rhs = self.product(depth)?;
            if rhs.number != acc.number {
                return None;
            }
            for (a, b) in acc.value.0.iter_mut().zip(rhs.value.0) {
                *a += sign * b;
            }
        }
    }

    fn product(&mut self, depth: usize) -> Option<Term> {
        let mut acc = self.value(depth)?;
        loop {
            let save = self.pos;
            self.space();
            let divide = match self.tokens.get(self.pos) {
                Some(Token::Delim('*')) => false,
                Some(Token::Delim('/')) => true,
                _ => {
                    self.pos = save;
                    return Some(acc);
                }
            };
            self.pos += 1;
            let rhs = self.value(depth)?;
            acc = match (divide, acc.number, rhs.number) {
                (true, _, true) if rhs.value.px() != 0.0 => {
                    let d = rhs.value.px();
                    Term { value: acc.value.map(|x| x / d), number: acc.number }
                }
                (false, _, true) => {
                    let m = rhs.value.px();
                    Term { value: acc.value.map(|x| x * m), number: acc.number }
                }
                (false, true, false) => {
                    let m = acc.value.px();
                    Term { value: rhs.value.map(|x| x * m), number: false }
                }
                _ => return None,
            };
        }
    }

    fn value(&mut self, depth: usize) -> Option<Term> {
        self.space();
        let token = self.tokens.get(self.pos)?;
        self.pos += 1;
        let number = |x: f32| Term { value: Linear([x, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]), number: true };
        let length = |l: Length| Term { value: Linear::from_length(l), number: false };
        match token {
            Token::Number { value, .. } => Some(number(*value)),
            Token::Percentage(x) => Some(length(Length::Percent(*x))),
            Token::Dimension { value, unit, .. } => Some(length((self.unit)(*value, unit)?)),
            Token::OpenParen => self.group(depth),
            Token::Function(name) if is_calc(name) => self.group(depth),
            _ => None,
        }
    }

    /// After a `(`: a sum and the `)`.
    fn group(&mut self, depth: usize) -> Option<Term> {
        if depth >= MAX_DEPTH {
            return None;
        }
        let inner = self.sum(depth + 1)?;
        self.space();
        (self.tokens.get(self.pos) == Some(&Token::CloseParen)).then(|| {
            self.pos += 1;
            inner
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenizer::tokenize;

    fn unit(value: f32, unit: &str) -> Option<Length> {
        match unit {
            "px" => Some(Length::Px(value)),
            "em" => Some(Length::Em(value)),
            "vw" => Some(Length::Vw(value)),
            _ => None,
        }
    }

    fn calc(text: &str) -> Option<Calc> {
        let tokens: Vec<_> = tokenize(text).collect();
        parse(&tokens, unit)
    }

    fn lin(px: f32, em: f32, pct: f32) -> Option<Calc> {
        Some(Calc::Length(Linear([px, em, 0.0, 0.0, 0.0, 0.0, 0.0, pct])))
    }

    #[test]
    fn arithmetic() {
        assert_eq!(calc("1 + 2 * 3"), Some(Calc::Number(7.0)));
        assert_eq!(calc("(1 + 2) * 3"), Some(Calc::Number(9.0)));
        assert_eq!(calc("10px + 5px"), lin(15.0, 0.0, 0.0));
        assert_eq!(calc("100% - 20px"), lin(-20.0, 0.0, 100.0));
        assert_eq!(calc("2 * 10px"), lin(20.0, 0.0, 0.0));
        assert_eq!(calc("10px * 2"), lin(20.0, 0.0, 0.0));
        assert_eq!(calc("(100% - 10px) / 2"), lin(-5.0, 0.0, 50.0));
        assert_eq!(calc("1em + 2px + 3em"), lin(2.0, 4.0, 0.0));
        assert_eq!(calc("calc(1px + calc(2px * 2))"), lin(5.0, 0.0, 0.0));
        assert_eq!(calc("  ( 1px )  "), lin(1.0, 0.0, 0.0));
        assert_eq!(calc("-5px + 2px"), lin(-3.0, 0.0, 0.0));
    }

    #[test]
    fn mistakes() {
        for bad in [
            "", "1 +", "+ 1", "1+2", "1 +2", "1px+2px", "1 + 2px", "1px + 2", "2px * 3px", "2px / 3px", "1 / 0", "1px / 0", "3 / 2px", "(1px", "1px)", "1px 2px", "foo", "1pc + 1px",
            "min(1px, 2px)", "1px * ", "* 2",
        ] {
            assert_eq!(calc(bad), None, "calc({bad})");
        }
    }

    #[test]
    fn single_unit() {
        assert_eq!(Linear([0.0; 8]).single(), Some(Length::Px(0.0)));
        assert_eq!(Linear([3.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]).single(), Some(Length::Px(3.0)));
        assert_eq!(Linear([0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 7.0]).single(), Some(Length::Percent(7.0)));
        assert_eq!(Linear([1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 7.0]).single(), None);
    }

    #[test]
    fn deep_groups_stop() {
        let text = format!("{}1{}", "(".repeat(100_000), ")".repeat(100_000));
        assert_eq!(calc(&text), None);
    }

    #[test]
    fn to_px() {
        let l = Linear([10.0, 2.0, 1.0, 10.0, 0.0, 0.0, 0.0, 50.0]);
        assert_eq!(l.to_px(20.0, 16.0, (1000.0, 500.0)), 10.0 + 40.0 + 16.0 + 100.0);
        assert_eq!(l.pct(), 50.0);
    }
}
