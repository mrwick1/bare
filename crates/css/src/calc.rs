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
//! `min()`, `max()` and `clamp()` take comma lists of the same sums, and nest in any of these. When all the arguments are in one unit
//! (`min(1px, 2px)`), we pick the answer now. Otherwise the answer depends on `em`, `vw` or a percent that we do not know yet, so
//! we keep a `Node` tree and compute it with the cascade (units) and at layout time (percent).
//!
//! Left out: the other math functions (`round()`, `mod()`, `sin()` ...), and the constants `pi` and `e`.
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
#[derive(Debug, Clone, PartialEq)]
pub enum Calc {
    Number(f32),
    Length(Linear),
    /// A `min()`, `max()` or `clamp()` that we cannot compute before layout.
    Math(Node<Linear>),
}

/// A length that needs `min`, `max` or `clamp` to be known. `L` is the leaf: a `Linear` when we read the sheet, `[px, pct]` after the cascade.
/// JS: an expression tree, like `Math.min(a, b + c)` before `a`, `b` and `c` are known.
#[derive(Debug, Clone, PartialEq)]
pub enum Node<L> {
    Leaf(L),
    /// `k1 * a + k2 * b + ...`
    Sum(Vec<(f32, Node<L>)>),
    Min(Vec<Node<L>>),
    Max(Vec<Node<L>>),
    /// `clamp(lo, value, hi)`
    Clamp(Box<[Node<L>; 3]>),
}

impl<L> Node<L> {
    /// The number that this is, when `leaf` gives the number of each leaf.
    pub fn eval(&self, leaf: &impl Fn(&L) -> f32) -> f32 {
        match self {
            Node::Leaf(l) => leaf(l),
            Node::Sum(v) => v.iter().map(|(k, n)| k * n.eval(leaf)).sum(),
            Node::Min(v) => v.iter().map(|n| n.eval(leaf)).fold(f32::INFINITY, f32::min),
            Node::Max(v) => v.iter().map(|n| n.eval(leaf)).fold(f32::NEG_INFINITY, f32::max),
            Node::Clamp(c) => c[1].eval(leaf).min(c[2].eval(leaf)).max(c[0].eval(leaf)),
        }
    }

    pub fn map<M>(&self, f: &impl Fn(&L) -> M) -> Node<M> {
        match self {
            Node::Leaf(l) => Node::Leaf(f(l)),
            Node::Sum(v) => Node::Sum(v.iter().map(|(k, n)| (*k, n.map(f))).collect()),
            Node::Min(v) => Node::Min(v.iter().map(|n| n.map(f)).collect()),
            Node::Max(v) => Node::Max(v.iter().map(|n| n.map(f)).collect()),
            Node::Clamp(c) => Node::Clamp(Box::new([c[0].map(f), c[1].map(f), c[2].map(f)])),
        }
    }

    /// Is `f` true for any leaf?
    pub fn any(&self, f: &impl Fn(&L) -> bool) -> bool {
        match self {
            Node::Leaf(l) => f(l),
            Node::Sum(v) => v.iter().any(|(_, n)| n.any(f)),
            Node::Min(v) | Node::Max(v) => v.iter().any(|n| n.any(f)),
            Node::Clamp(c) => c.iter().any(|n| n.any(f)),
        }
    }
}

impl Node<[f32; 2]> {
    /// The value as text, with the percent first like Chrome: `min(300px, 50%)`.
    pub fn text(&self) -> String {
        match self {
            Node::Leaf([px, pct]) if *pct == 0.0 => format!("{px}px"),
            Node::Leaf([px, pct]) if *px == 0.0 => format!("{pct}%"),
            Node::Leaf([px, pct]) => format!("calc({pct}% {} {}px)", if *px < 0.0 { '-' } else { '+' }, px.abs()),
            Node::Sum(v) => format!("calc({})", v.iter().map(|(k, n)| format!("{k} * {}", n.text())).collect::<Vec<_>>().join(" + ")),
            Node::Min(v) => format!("min({})", v.iter().map(Node::text).collect::<Vec<_>>().join(", ")),
            Node::Max(v) => format!("max({})", v.iter().map(Node::text).collect::<Vec<_>>().join(", ")),
            Node::Clamp(c) => format!("clamp({}, {}, {})", c[0].text(), c[1].text(), c[2].text()),
        }
    }
}

/// A number or a sum of lengths, while we compute. For a number, the value is in the `px` slot. A `node` is the value when there is one
/// (then `value` is not used).
#[derive(Clone)]
struct Term {
    value: Linear,
    number: bool,
    node: Option<Node<Linear>>,
}

impl Term {
    fn into_node(self) -> Node<Linear> {
        self.node.unwrap_or(Node::Leaf(self.value))
    }

    fn scale(self, k: f32) -> Term {
        match self.node {
            Some(n) => Term { value: self.value, number: false, node: Some(Node::Sum(vec![(k, n)])) },
            None => Term { value: self.value.map(|x| x * k), number: self.number, node: None },
        }
    }
}

/// Is this the name of `calc`, with or without the old prefix?
pub fn is_calc(name: &str) -> bool {
    name.eq_ignore_ascii_case("calc") || name.eq_ignore_ascii_case("-webkit-calc") || name.eq_ignore_ascii_case("-moz-calc")
}

/// Is this the name of `min`, `max` or `clamp`?
pub fn is_math(name: &str) -> bool {
    name.eq_ignore_ascii_case("min") || name.eq_ignore_ascii_case("max") || name.eq_ignore_ascii_case("clamp")
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
    finish(term)
}

fn finish(term: Term) -> Option<Calc> {
    match term.node {
        Some(n) => Some(Calc::Math(n)),
        None if term.number => Some(Calc::Number(term.value.px())),
        None => Some(Calc::Length(term.value)),
    }
}

/// Read the tokens inside `min( ... )`, `max( ... )` or `clamp( ... )`, without the closing `)`. `name` is the function.
pub fn parse_fn(name: &str, tokens: &[Token<'_>], unit: impl Fn(f32, &str) -> Option<Length> + Copy) -> Option<Calc> {
    let mut p = P { tokens, pos: 0, unit };
    let term = p.call(name, 0, false)?;
    if p.pos != tokens.len() || !term.value.0.iter().all(|x| x.is_finite()) {
        return None;
    }
    finish(term)
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
            if acc.node.is_some() || rhs.node.is_some() {
                acc = Term { value: Linear::default(), number: false, node: Some(Node::Sum(vec![(1.0, acc.into_node()), (sign, rhs.into_node())])) };
                continue;
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
                    if acc.node.is_some() { acc.scale(1.0 / d) } else { Term { value: acc.value.map(|x| x / d), number: acc.number, node: None } }
                }
                (false, _, true) => acc.scale(rhs.value.px()),
                (false, true, false) => {
                    let m = acc.value.px();
                    rhs.scale(m)
                }
                _ => return None,
            };
        }
    }

    fn value(&mut self, depth: usize) -> Option<Term> {
        self.space();
        let token = self.tokens.get(self.pos)?;
        self.pos += 1;
        let number = |x: f32| Term { value: Linear([x, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]), number: true, node: None };
        let length = |l: Length| Term { value: Linear::from_length(l), number: false, node: None };
        match token {
            Token::Number { value, .. } => Some(number(*value)),
            Token::Percentage(x) => Some(length(Length::Percent(*x))),
            Token::Dimension { value, unit, .. } => Some(length((self.unit)(*value, unit)?)),
            Token::OpenParen => self.group(depth),
            Token::Function(name) if is_calc(name) => self.group(depth),
            Token::Function(name) if is_math(name) => self.call(name, depth, true),
            _ => None,
        }
    }

    /// The comma list of `min(`, `max(` or `clamp(`, and the `)` when `nested`. Without it the list ends with the tokens.
    fn call(&mut self, name: &str, depth: usize, nested: bool) -> Option<Term> {
        if depth >= MAX_DEPTH {
            return None;
        }
        let mut args = Vec::new();
        loop {
            args.push(self.sum(depth + 1)?);
            self.space();
            match self.tokens.get(self.pos) {
                Some(Token::Comma) => self.pos += 1,
                Some(Token::CloseParen) if nested => {
                    self.pos += 1;
                    break;
                }
                None if !nested => break,
                _ => return None,
            }
        }
        let clamp = name.eq_ignore_ascii_case("clamp");
        if args.iter().any(|a| a.number != args[0].number) || (clamp && args.len() != 3) {
            return None;
        }
        // Only one unit in every argument: we know the order now. `0` is in every unit.
        let slot = |t: &Term| {
            let mut used = t.value.0.iter().enumerate().filter(|&(_, &x)| x != 0.0).map(|(i, _)| i);
            match (t.node.is_some(), used.next(), used.next()) {
                (false, None, _) => Some(None),
                (false, Some(i), None) => Some(Some(i)),
                _ => None,
            }
        };
        let units: Option<Vec<_>> = args.iter().map(slot).collect();
        let one_unit = units.is_some_and(|u| {
            let first = u.iter().flatten().next();
            u.iter().flatten().all(|s| Some(s) == first)
        });
        if one_unit {
            let key = |t: &Term| t.value.0.iter().sum::<f32>();
            let pick = |args: Vec<Term>, max: bool| {
                args.into_iter().reduce(|a, b| if (key(&b) > key(&a)) == max { b } else { a }).unwrap()
            };
            return Some(if clamp {
                let mut it = args.into_iter();
                let (lo, v, hi) = (it.next()?, it.next()?, it.next()?);
                let r = if key(&v) > key(&hi) { hi } else { v };
                if key(&r) < key(&lo) { lo } else { r }
            } else {
                pick(args, name.eq_ignore_ascii_case("max"))
            });
        }
        if args[0].number {
            return None;
        }
        let mut nodes: Vec<_> = args.into_iter().map(Term::into_node).collect();
        let node = if clamp {
            let hi = nodes.pop()?;
            let v = nodes.pop()?;
            Node::Clamp(Box::new([nodes.pop()?, v, hi]))
        } else if name.eq_ignore_ascii_case("max") {
            Node::Max(nodes)
        } else {
            Node::Min(nodes)
        };
        Some(Term { value: Linear::default(), number: false, node: Some(node) })
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

    /// `text` is what is inside `calc( ... )`. It can also be a whole `min(...)`, `max(...)` or `clamp(...)`.
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

    fn node(text: &str) -> Node<Linear> {
        match calc(text) {
            Some(Calc::Math(n)) => n,
            other => panic!("{text}: {other:?}"),
        }
    }

    fn leaf(px: f32, em: f32, pct: f32) -> Node<Linear> {
        Node::Leaf(Linear([px, em, 0.0, 0.0, 0.0, 0.0, 0.0, pct]))
    }

    #[test]
    fn min_max_clamp_in_one_unit_are_computed_now() {
        assert_eq!(calc("min(1px, 2px)"), lin(1.0, 0.0, 0.0));
        assert_eq!(calc("max(1px, 2px, 3px)"), lin(3.0, 0.0, 0.0));
        assert_eq!(calc("clamp(1px, 5px, 3px)"), lin(3.0, 0.0, 0.0));
        assert_eq!(calc("clamp(4px, 1px, 9px)"), lin(4.0, 0.0, 0.0));
        assert_eq!(calc("clamp(1px, 2px, 3px)"), lin(2.0, 0.0, 0.0));
        assert_eq!(calc("MIN(10%, 20%)"), lin(0.0, 0.0, 10.0));
        assert_eq!(calc("min(1px)"), lin(1.0, 0.0, 0.0));
        assert_eq!(calc("min(1, 2)"), Some(Calc::Number(1.0)));
        assert_eq!(calc("max(1 + 1, 3 - 2)"), Some(Calc::Number(2.0)));
        assert_eq!(calc("min(0px, 5%)"), lin(0.0, 0.0, 0.0));
        assert_eq!(calc("min(1px + 1px, 3px)"), lin(2.0, 0.0, 0.0));
    }

    #[test]
    fn min_max_clamp_with_mixed_units_keep_a_tree() {
        assert_eq!(node("min(300px, 80vw)").map(&|_| ()), Node::Min(vec![Node::Leaf(()), Node::Leaf(())]));
        assert_eq!(node("max(1px, 2em)"), Node::Max(vec![leaf(1.0, 0.0, 0.0), leaf(0.0, 2.0, 0.0)]));
        assert_eq!(node("min(300px, 50%)"), Node::Min(vec![leaf(300.0, 0.0, 0.0), leaf(0.0, 0.0, 50.0)]));
        assert_eq!(
            node("clamp(1px, 2vw + 1em, 9px)"),
            Node::Clamp(Box::new([leaf(1.0, 0.0, 0.0), Node::Leaf(Linear([0.0, 1.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0])), leaf(9.0, 0.0, 0.0)]))
        );
    }

    #[test]
    fn math_nests_in_calc_and_in_itself() {
        let all = |n: &Node<Linear>, base: f32| n.eval(&|l: &Linear| l.to_px(10.0, 16.0, (1000.0, 500.0)) + base * l.pct() / 100.0);
        assert_eq!(all(&node("min(max(1px, 2em), 50%)"), 100.0), 20.0);
        assert_eq!(all(&node("min(max(1px, 2em), 50%)"), 30.0), 15.0);
        assert_eq!(all(&node("min(300px, 80vw)"), 0.0), 300.0);
        assert_eq!(all(&node("calc(min(10px, 50%) * 2 + 1px)"), 8.0), 9.0);
        assert_eq!(all(&node("calc(100% - min(10px, 1em))"), 50.0), 40.0);
        assert_eq!(all(&node("clamp(1em, 50%, 20px)"), 100.0), 20.0);
        assert_eq!(all(&node("clamp(1em, 50%, 20px)"), 4.0), 10.0);
        assert_eq!(all(&node("min(calc(1px + 2em), 100%) / 2"), 100.0), 10.5);
        assert_eq!(all(&node("calc(min(1em, 2px) + 1em)"), 0.0), 12.0);
    }

    #[test]
    fn math_mistakes() {
        for bad in [
            "min()", "min(1px,)", "min(,1px)", "min(1px 2px)", "clamp(1px, 2px)", "clamp(1px, 2px, 3px, 4px)", "min(1, 2px)", "min(1em, 2)", "min(1px, foo)", "min(1px, 2px",
            "min(1em, 2px) * min(1em, 2px)", "max(1em, 2px) + 1",
        ] {
            assert_eq!(calc(bad), None, "calc({bad})");
        }
    }

    #[test]
    fn deep_math_stops() {
        let text = format!("{}1px{}", "min(".repeat(100_000), ")".repeat(100_000));
        assert_eq!(calc(&text), None);
    }

    #[test]
    fn mistakes() {
        for bad in [
            "", "1 +", "+ 1", "1+2", "1 +2", "1px+2px", "1 + 2px", "1px + 2", "2px * 3px", "2px / 3px", "1 / 0", "1px / 0", "3 / 2px", "(1px", "1px)", "1px 2px", "foo", "1pc + 1px",
            "1px * ", "* 2",
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
