//! Modern colors use floats until the final sRGB paint value. Deferred expressions own
//! their postorder nodes, so moving a Side needs only one root-index relocation.
use crate::{
    color::Color,
    tokenizer::{Token, Tokens, tokenize},
};
use color::{ColorSpaceTag as Space, DynamicColor, HueDirection};

// Most Side values never need deferred colors. Keep their empty arena at one word.
#[derive(Debug, Default, Clone, PartialEq)]
#[expect(clippy::box_collection, reason = "one-word empty Side storage; allocate the vector header only for deferred colors")]
pub struct Arena(Option<Box<Vec<Expression>>>);

impl Arena {
    pub fn len(&self) -> usize {
        self.0.as_ref().map_or(0, |v| v.len())
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn get(&self, index: usize) -> Option<&Expression> {
        self.0.as_ref()?.get(index)
    }
    pub fn push(&mut self, expression: Expression) {
        self.0.get_or_insert_with(Default::default).push(expression);
    }
    pub fn clear(&mut self) {
        self.0 = None;
    }
    pub fn extend(&mut self, other: Self) {
        match (&mut self.0, other.0) {
            (None, other) => self.0 = other,
            (Some(here), Some(other)) => here.extend(*other),
            _ => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expression {
    nodes: Vec<Node>,
    text: String,
    dependent: bool,
}
#[derive(Debug, Clone, PartialEq)]
enum Node {
    Absolute(DynamicColor),
    Current,
    Mix { left: usize, right: usize, space: Space, hue: HueDirection, t: f32, alpha: f32 },
}

pub fn is_function(name: &str) -> bool {
    ["hwb", "lab", "lch", "oklab", "oklch", "color", "color-mix"].iter().any(|s| name.eq_ignore_ascii_case(s))
}

pub fn read(name: &str, tokens: &mut Tokens<'_>) -> Option<Expression> {
    let rest = tokens.rest();
    let mut depth = 1;
    for token in tokens.by_ref() {
        match token {
            Token::Function(_) | Token::OpenParen => depth += 1,
            Token::CloseParen => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            break;
        }
    }
    // The tokenizer consumes comments and strings without confusing their parentheses.
    let used = rest.len() - tokens.rest().len();
    let text = format!("{name}({}", &rest[..used]);
    parse(&text)
}

// Tokenize once and precompute matching function ends. Nested mix parsing then visits
// each node once; neither Rust recursion nor repeated suffix scans depend on depth.
fn parse(text: &str) -> Option<Expression> {
    let mut tokenizer = tokenize(text);
    let mut ts = Vec::new();
    let mut ends = Vec::new();
    let mut stack = Vec::new();
    while let Some((token, start, end)) = tokenizer.next_spanned() {
        if matches!(token, Token::Whitespace) {
            continue;
        }
        let i = ts.len();
        ends.push(i);
        match token {
            Token::Function(_) => stack.push(i),
            Token::CloseParen => {
                let open = stack.pop()?;
                ends[open] = i;
            }
            Token::OpenParen | Token::OpenCurly | Token::OpenSquare | Token::BadString | Token::BadUrl => return None,
            _ => {}
        }
        ts.push((token, start, end));
    }
    if !stack.is_empty() || ts.is_empty() {
        return None;
    }
    enum Work {
        Parse(usize, usize),
        Mix(Space, HueDirection, f32, f32),
    }
    let mut work = vec![Work::Parse(0, ts.len())];
    let mut roots = Vec::new();
    let mut nodes = Vec::new();
    let mut dependent = false;
    while let Some(task) = work.pop() {
        match task {
            Work::Mix(space, hue, t, alpha) => {
                let right = roots.pop()?;
                let left = roots.pop()?;
                roots.push(nodes.len());
                nodes.push(Node::Mix { left, right, space, hue, t, alpha });
            }
            Work::Parse(start, end) => {
                if start >= end {
                    return None;
                }
                if matches!(&ts[start].0, Token::Ident(s) if s.eq_ignore_ascii_case("currentcolor")) && end == start + 1 {
                    dependent = true;
                    roots.push(nodes.len());
                    nodes.push(Node::Current);
                    continue;
                }
                if !matches!(&ts[start].0, Token::Function(s) if s.eq_ignore_ascii_case("color-mix")) {
                    let absolute = color::parse_color(&text[ts[start].1..ts[end - 1].2]).ok()?;
                    if !absolute.components.iter().all(|v| v.is_finite()) || !absolute.convert(Space::Oklch).components.iter().all(|v| v.is_finite()) {
                        return None;
                    }
                    roots.push(nodes.len());
                    nodes.push(Node::Absolute(absolute));
                    continue;
                }
                if ends[start] + 1 != end {
                    return None;
                }
                let mut commas = Vec::new();
                let mut i = start + 1;
                while i < end - 1 {
                    if matches!(ts[i].0, Token::Comma) {
                        commas.push(i);
                    }
                    i = ends[i] + 1;
                }
                let [a, b] = *commas.as_slice() else {
                    return None;
                };
                let mut header = Vec::new();
                for token in &ts[start + 1..a] {
                    let Token::Ident(s) = &token.0 else {
                        return None;
                    };
                    header.push(s.to_ascii_lowercase());
                }
                let [intro, space_name, tail @ ..] = header.as_slice() else {
                    return None;
                };
                if intro != "in" {
                    return None;
                }
                let space = match space_name.as_str() {
                    "srgb" => Space::Srgb,
                    "srgb-linear" => Space::LinearSrgb,
                    "lab" => Space::Lab,
                    "lch" => Space::Lch,
                    "oklab" => Space::Oklab,
                    "oklch" => Space::Oklch,
                    "hsl" => Space::Hsl,
                    "hwb" => Space::Hwb,
                    "display-p3" => Space::DisplayP3,
                    "a98-rgb" => Space::A98Rgb,
                    "prophoto-rgb" => Space::ProphotoRgb,
                    "rec2020" => Space::Rec2020,
                    "xyz" | "xyz-d65" => Space::XyzD65,
                    "xyz-d50" => Space::XyzD50,
                    _ => return None,
                };
                let hue = match tail {
                    [] => HueDirection::Shorter,
                    [direction, suffix] if suffix == "hue" && matches!(space, Space::Lch | Space::Oklch | Space::Hsl | Space::Hwb) => {
                        match direction.as_str() {
                            "shorter" => HueDirection::Shorter,
                            "longer" => HueDirection::Longer,
                            "increasing" => HueDirection::Increasing,
                            "decreasing" => HueDirection::Decreasing,
                            _ => return None,
                        }
                    }
                    _ => return None,
                };
                let operand = |mut lo: usize, mut hi: usize| -> Option<(usize, usize, Option<f32>)> {
                    if lo >= hi {
                        return None;
                    }
                    let mut weight = None;
                    if let Token::Percentage(v) = ts[lo].0 {
                        weight = Some(v);
                        lo += 1;
                    }
                    if lo < hi
                        && let Token::Percentage(v) = ts[hi - 1].0
                    {
                        if weight.is_some() {
                            return None;
                        }
                        weight = Some(v);
                        hi -= 1;
                    }
                    if lo >= hi || weight.is_some_and(|v| !v.is_finite() || !(0.0..=100.0).contains(&v)) {
                        return None;
                    }
                    Some((lo, hi, weight))
                };
                let (lo1, hi1, w1) = operand(a + 1, b)?;
                let (lo2, hi2, w2) = operand(b + 1, end - 1)?;
                let (w1, w2) = match (w1, w2) {
                    (None, None) => (50.0, 50.0),
                    (Some(x), None) => (x, 100.0 - x),
                    (None, Some(y)) => (100.0 - y, y),
                    (Some(x), Some(y)) => (x, y),
                };
                let sum = w1 + w2;
                work.push(Work::Mix(space, hue, if sum == 0.0 { 0.5 } else { w2 / sum }, (sum / 100.0).min(1.0)));
                work.push(Work::Parse(lo2, hi2));
                work.push(Work::Parse(lo1, hi1));
            }
        }
    }
    (roots.len() == 1).then(|| Expression { nodes, text: text.into(), dependent })
}

impl Expression {
    pub fn dependent(&self) -> bool {
        self.dependent
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn resolve(&self, current: Color) -> Color {
        let mut values = Vec::<DynamicColor>::with_capacity(self.nodes.len());
        for node in &self.nodes {
            let value = match *node {
                Node::Absolute(c) => c,
                Node::Current => DynamicColor {
                    cs: Space::Srgb,
                    flags: Default::default(),
                    components: [current.r as f32 / 255.0, current.g as f32 / 255.0, current.b as f32 / 255.0, current.a as f32 / 255.0],
                },
                Node::Mix { left, right, space, hue, t, alpha } => {
                    let mut mixed = values[left].interpolate(values[right], space, hue).eval(t);
                    mixed.components[3] *= alpha;
                    mixed
                }
            };
            values.push(value);
        }
        paint(*values.last().expect("parsed color has a root"))
    }
}

fn in_gamut(c: DynamicColor) -> bool {
    c.components[..3].iter().all(|v| (0.0..=1.0).contains(v))
}
fn clipped(mut c: DynamicColor) -> DynamicColor {
    for v in &mut c.components[..3] {
        *v = v.clamp(0.0, 1.0);
    }
    c
}

fn delta(a: DynamicColor, b: DynamicColor) -> f32 {
    let a = a.convert(Space::Oklab);
    let b = b.convert(Space::Oklab);
    a.components[..3].iter().zip(&b.components[..3]).map(|(x, y)| (x - y).powi(2)).sum::<f32>().sqrt()
}

// CSS Color 4 local-MINDE binary search in OKLCH. Delta E uses Euclidean Oklab.
fn paint(color: DynamicColor) -> Color {
    let mut rgb = color.convert(Space::Srgb);
    if !in_gamut(rgb) {
        let origin = color.convert(Space::Oklch);
        let light = origin.components[0];
        if light >= 1.0 {
            rgb.components[..3].fill(1.0);
        } else if light <= 0.0 {
            rgb.components[..3].fill(0.0);
        } else {
            let (mut min, mut max, mut min_in_gamut) = (0.0, origin.components[1], true);
            let mut mapped = clipped(rgb);
            if delta(mapped, origin) >= 0.02 {
                while max - min > 0.0001 {
                    let chroma = (min + max) / 2.0;
                    let mut candidate = origin;
                    candidate.components[1] = chroma;
                    let candidate_rgb = candidate.convert(Space::Srgb);
                    if min_in_gamut && in_gamut(candidate_rgb) {
                        min = chroma;
                        continue;
                    }
                    mapped = clipped(candidate_rgb);
                    let delta = delta(mapped, candidate);
                    if delta < 0.02 {
                        if 0.02 - delta < 0.0001 {
                            break;
                        }
                        min_in_gamut = false;
                        min = chroma;
                    } else {
                        max = chroma;
                    }
                }
            }
            rgb = mapped;
        }
    }
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color { r: byte(rgb.components[0]), g: byte(rgb.components[1]), b: byte(rgb.components[2]), a: byte(rgb.components[3]) }
}
