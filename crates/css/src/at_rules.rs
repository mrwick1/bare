//! At-rules that need a decision: `@media`, `@supports` and `@import`.
//!
//! The parser gives each at-rule as a name, a prelude (text) and a body. This file reads the prelude and says yes or no.
//! The cascade then uses the rules inside a block that says yes, and drops the others.
//!
//! What we read:
//! - `@media`: the types `all` and `screen` (`print` and the others are no), `not`, `only`, `and`, `,`, the features
//!   `width`, `height`, `orientation`, `resolution`, `prefers-color-scheme`, `hover`, `pointer`, and the range
//!   syntax `(400px <= width <= 700px)`. A feature that we do not know is no.
//! - `@supports`: `(property: value)`, `selector(...)`, `not`, `and`, `or`, and groups in `( )`.
//! - `@import`: the address, `layer`, `supports(...)` and a media query.
//!
//! Left out: `@container` (it needs the size of a box, so it waits for layout), and a `@layer` statement before an `@import` (it comes after the layers of the imports).
//!
//! The window is the one the caller gives. The page is a screen, 1 pixel per CSS pixel, with a mouse. `em` is 16 pixels.

use bare_html::dom::Document;

use crate::selector::parse_selector_list;
use crate::values::{Side, parse_declaration};

/// Blocks in parentheses go this deep. Deeper ones are no. This keeps `((((((...` from filling the stack.
const MAX_DEPTH: usize = 32;

/// Does this `@media` prelude match a window of `viewport` pixels? An empty prelude matches.
pub fn media_matches(prelude: &str, viewport: (f32, f32)) -> bool {
    let prelude = prelude.trim();
    prelude.is_empty()
        || split_top(prelude, ',')
            .iter()
            .any(|q| query_matches(q, viewport))
    // JS: like `window.matchMedia(prelude).matches`.
}

fn query_matches(query: &str, viewport: (f32, f32)) -> bool {
    let mut words = words(query).into_iter().peekable();
    let mut negate = false;
    match words.peek().map(|w| w.to_ascii_lowercase()).as_deref() {
        Some("not") => {
            negate = true;
            words.next();
        }
        Some("only") => {
            words.next();
        }
        _ => {}
    }
    let mut ok = true;
    let mut expect_term = true;
    for w in words {
        if !expect_term {
            // Between two terms there must be `and`.
            if !w.eq_ignore_ascii_case("and") {
                return false;
            }
            expect_term = true;
            continue;
        }
        expect_term = false;
        ok &= match w.strip_prefix('(').and_then(|w| w.strip_suffix(')')) {
            Some(inner) => feature(inner, viewport, 0),
            None => matches!(w.to_ascii_lowercase().as_str(), "all" | "screen"),
        };
    }
    !expect_term && ok != negate
}

/// One media feature, without its parentheses: `min-width: 600px`.
fn feature(inner: &str, (w, h): (f32, f32), depth: usize) -> bool {
    let inner = inner.trim();
    if depth >= MAX_DEPTH {
        return false;
    }
    // `((hover))`, `(not (hover))`, `((hover) and (pointer))`: a condition in a group.
    if inner.starts_with('(')
        || inner
            .get(..4)
            .is_some_and(|s| s.eq_ignore_ascii_case("not "))
    {
        return condition(inner, &mut |t| {
            t.strip_prefix('(')
                .and_then(|t| t.strip_suffix(')'))
                .is_some_and(|t| feature(t, (w, h), depth + 1))
        });
    }
    if let Some((name, value)) = inner.split_once(':') {
        let (name, value) = (
            name.trim().to_ascii_lowercase(),
            value.trim().to_ascii_lowercase(),
        );
        let (min, max) = (name.starts_with("min-"), name.starts_with("max-"));
        let base = name.trim_start_matches("min-").trim_start_matches("max-");
        let compare = |actual: f32, wanted: Option<f32>| match wanted {
            Some(v) if min => actual >= v,
            Some(v) if max => actual <= v,
            Some(v) => actual == v,
            None => false,
        };
        return match base {
            "width" => compare(w, px(&value)),
            "height" => compare(h, px(&value)),
            "resolution" => compare(1.0, dppx(&value)),
            _ if min || max => false,
            "orientation" => value == if h >= w { "portrait" } else { "landscape" },
            "prefers-color-scheme" => value == "light",
            "prefers-reduced-motion" => value == "no-preference",
            "hover" | "any-hover" => value == "hover",
            "pointer" | "any-pointer" => value == "fine",
            _ => false,
        };
    }
    if inner.contains(['<', '>', '=']) {
        return range(inner, (w, h));
    }
    // `(hover)` with no value is true when the feature is on.
    matches!(
        inner.to_ascii_lowercase().as_str(),
        "hover" | "any-hover" | "pointer" | "any-pointer" | "color" | "width" | "height"
    )
}

/// The range syntax: `width >= 600px`, `600px <= width`, `400px <= width <= 700px`.
fn range(text: &str, (w, h): (f32, f32)) -> bool {
    // Cut the text into operands and operators.
    let mut operands = Vec::new();
    let mut ops = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if matches!(bytes[i], b'<' | b'>' | b'=') {
            operands.push(text[start..i].trim());
            let len = if bytes.get(i + 1) == Some(&b'=') && bytes[i] != b'=' {
                2
            } else {
                1
            };
            ops.push(&text[i..i + len]);
            i += len;
            start = i;
        } else {
            i += 1;
        }
    }
    operands.push(text[start..].trim());
    if operands.len() < 2 || operands.len() > 3 {
        return false;
    }
    let number = |s: &str| match s.to_ascii_lowercase().as_str() {
        "width" => Some(w),
        "height" => Some(h),
        s => px(s),
    };
    operands.windows(2).zip(&ops).all(|(pair, op)| {
        let (Some(a), Some(b)) = (number(pair[0]), number(pair[1])) else {
            return false;
        };
        match *op {
            "<" => a < b,
            "<=" => a <= b,
            ">" => a > b,
            ">=" => a >= b,
            _ => a == b,
        }
    })
}

/// A length in pixels: `600px`, `40em`, `0`. Only the units that do not need a box.
fn px(text: &str) -> Option<f32> {
    let end = text
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+')))
        .unwrap_or(text.len());
    let n: f32 = text[..end].parse().ok()?;
    match text[end..].trim() {
        "px" => Some(n),
        "em" | "rem" => Some(n * 16.0),
        "pt" => Some(n * 4.0 / 3.0),
        "" if n == 0.0 => Some(0.0),
        _ => None,
    }
}

/// A resolution in dots per pixel: `2dppx`, `192dpi`, `2x`.
fn dppx(text: &str) -> Option<f32> {
    let end = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let n: f32 = text[..end].parse().ok()?;
    match &text[end..] {
        "dppx" | "x" => Some(n),
        "dpi" => Some(n / 96.0),
        "dpcm" => Some(n * 2.54 / 96.0),
        _ => None,
    }
}

/// Does this `@supports` prelude say yes? `(display: grid)`, `not (foo: bar)`, `(a: b) and (c: d)`, `selector(a > b)`.
pub fn supports_matches(doc: &mut Document, prelude: &str) -> bool {
    supports(doc, prelude, 0)
}

fn supports(doc: &mut Document, text: &str, depth: usize) -> bool {
    if depth >= MAX_DEPTH {
        return false;
    }
    condition(text, &mut |term| {
        let term = term.trim();
        if let Some(arg) = term
            .get(..9)
            .filter(|s| s.eq_ignore_ascii_case("selector("))
            .and_then(|_| term[9..].strip_suffix(')'))
        {
            return parse_selector_list(arg, doc).is_some();
        }
        let Some(inner) = term.strip_prefix('(').and_then(|t| t.strip_suffix(')')) else {
            return false;
        };
        let inner = inner.trim();
        if inner.starts_with('(')
            || inner
                .get(..4)
                .is_some_and(|s| s.eq_ignore_ascii_case("not "))
        {
            return supports(doc, inner, depth + 1);
        }
        let Some((name, value)) = inner.split_once(':') else {
            return false;
        };
        let (value, important) = match value.trim().rsplit_once('!') {
            Some((v, i)) if i.trim().eq_ignore_ascii_case("important") => (v.trim(), true),
            _ => (value.trim(), false),
        };
        // A custom property takes any value. A normal one must parse.
        name.trim().starts_with("--")
            || parse_declaration(
                &name.trim().to_ascii_lowercase(),
                value,
                important,
                &mut Side::default(),
                &mut Vec::new(),
            )
    })
}

/// A condition: `term`, `not term`, `term and term and ...`, or `term or term or ...`. `and` and `or` do not mix without
/// a group. `test` says if one term is true.
fn condition(text: &str, test: &mut dyn FnMut(&str) -> bool) -> bool {
    let words = words(text);
    match words.as_slice() {
        [] => false,
        [n, term] if n.eq_ignore_ascii_case("not") => !test(term),
        [term] => test(term),
        _ => {
            let op = words[1].to_ascii_lowercase();
            if op != "and" && op != "or" {
                return false;
            }
            let mut result = op == "and";
            for (i, w) in words.iter().enumerate() {
                if i % 2 == 1 {
                    // Every operator must be the same.
                    if !w.eq_ignore_ascii_case(&op) {
                        return false;
                    }
                } else if test(w) == (op == "or") {
                    result = op == "or";
                }
            }
            words.len() % 2 == 1 && result
        }
    }
}

/// An `@import` that applies: the address, and the layer that the sheet goes in.
#[derive(Debug, PartialEq)]
pub struct Import {
    pub href: String,
    /// `layer(x)` gives `Some("x")`, `layer` with no name gives `Some("")`, and no `layer` gives `None`.
    pub layer: Option<String>,
}

/// The `@import` rules at the start of a style sheet, for the media of `viewport`. The caller fetches them,
/// reads them first, and then reads this sheet: an import is before the rules of the sheet.
pub fn imports(
    doc: &mut Document,
    rules: &[crate::parser::Rule<'_>],
    viewport: (f32, f32),
) -> Vec<Import> {
    use crate::parser::{AtBody, Rule};
    let mut out = Vec::new();
    for rule in rules {
        let Rule::At(at) = rule else { break };
        match (&*at.name, &at.body) {
            ("charset", _) | ("layer", AtBody::None) => {}
            // The standard: an `@import` after any other rule is not valid.
            ("import", AtBody::None) => {
                if let Some((href, rest)) = import_target(at.prelude)
                    && let Some(layer) = import_ok(doc, rest, viewport)
                {
                    out.push(Import { href: href.to_string(), layer });
                }
            }
            _ => break,
        }
    }
    out
}

/// Split `url(a.css) screen` or `"a.css" screen` into the address and the rest.
fn import_target(prelude: &str) -> Option<(&str, &str)> {
    let p = prelude.trim_start();
    let (inner, rest) = if p.get(..4).is_some_and(|s| s.eq_ignore_ascii_case("url(")) {
        let end = p.find(')')?;
        (p[4..end].trim(), &p[end + 1..])
    } else {
        let quote = p.chars().next().filter(|c| matches!(c, '"' | '\''))?;
        let end = p[1..].find(quote)? + 1;
        (&p[1..end], &p[end + 1..])
    };
    let href = inner.trim_matches(['"', '\'']);
    (!href.is_empty()).then_some((href, rest))
}

/// The part after the address: `layer(x) supports(...) media`. `None` when the import does not apply.
/// Otherwise the layer of the sheet (see `Import::layer`).
fn import_ok(doc: &mut Document, rest: &str, viewport: (f32, f32)) -> Option<Option<String>> {
    let mut rest = rest.trim();
    let mut layer = None;
    if let Some(r) = strip_word(rest, "layer(") {
        layer = Some(r[..paren_end(r)].trim().to_string());
        rest = after_paren(r).trim_start();
    } else if rest.len() >= 5
        && rest[..5].eq_ignore_ascii_case("layer")
        && rest[5..].starts_with(char::is_whitespace)
    {
        layer = Some(String::new());
        rest = rest[5..].trim_start();
    } else if rest.eq_ignore_ascii_case("layer") {
        layer = Some(String::new());
        rest = "";
    }
    if let Some(r) = strip_word(rest, "supports(") {
        let end = paren_end(r);
        let cond = &r[..end];
        // `supports(display: grid)` is a bare declaration. `supports(not (a: b))` is a condition.
        let ok = if cond.contains(':')
            && !cond.trim_start().starts_with('(')
            && !cond.trim_start().to_ascii_lowercase().starts_with("not")
        {
            supports(doc, &format!("({cond})"), 0)
        } else {
            supports(doc, cond, 0)
        };
        if !ok {
            return None;
        }
        rest = r.get(end + 1..).unwrap_or("").trim_start();
    }
    media_matches(rest, viewport).then_some(layer)
}

fn strip_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    text.get(..word.len())
        .filter(|s| s.eq_ignore_ascii_case(word))
        .map(|_| &text[word.len()..])
}

/// The text after the `)` that closes a group. The text starts after the `(`.
fn after_paren(text: &str) -> &str {
    text.get(paren_end(text) + 1..).unwrap_or("")
}

/// The place of the `)` that closes the group in `text`, which starts after the `(`.
fn paren_end(text: &str) -> usize {
    let mut depth = 0;
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => return i,
            ')' => depth -= 1,
            _ => {}
        }
    }
    text.len()
}

/// Cut `text` at each `sep` that is not inside `( )`.
fn split_top(text: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0usize, 0);
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            c if c == sep && depth == 0 => {
                out.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&text[start..]);
    out
}

/// Cut `text` at the spaces that are not inside `( )`. `(a: b) and (c: d)` is three words.
fn words(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0usize, None);
    for (i, c) in text.char_indices() {
        match c {
            c if c.is_whitespace() && depth == 0 => {
                if let Some(s) = start.take() {
                    out.push(&text[s..i]);
                }
                continue;
            }
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        start.get_or_insert(i);
    }
    if let Some(s) = start {
        out.push(&text[s..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bare_html::tree::parse;

    const VIEW: (f32, f32) = (1000.0, 500.0);

    #[test]
    fn media_types_and_logic() {
        for (q, want) in [
            ("", true),
            ("all", true),
            ("screen", true),
            ("print", false),
            ("not print", true),
            ("not screen", false),
            ("only screen", true),
            ("print, screen", true),
            ("print, speech", false),
            ("screen and (min-width: 600px)", true),
            ("screen and (min-width: 1001px)", false),
            ("print and (min-width: 600px)", false),
            ("(max-width: 1000px)", true),
            ("(max-width: 999px)", false),
            ("(width: 1000px)", true),
            ("(min-height: 500px) and (max-height: 500px)", true),
            ("(min-width: 40em)", true),
            ("(min-width: 70em)", false),
            ("(orientation: landscape)", true),
            ("(orientation: portrait)", false),
            ("(prefers-color-scheme: dark)", false),
            ("(prefers-color-scheme: light)", true),
            ("(hover: hover) and (pointer: fine)", true),
            ("(min-resolution: 2dppx)", false),
            ("(min-resolution: 1dppx)", true),
            ("(-webkit-min-device-pixel-ratio: 2)", false),
            ("(width >= 600px)", true),
            ("(width < 600px)", false),
            ("(600px <= width)", true),
            ("(400px <= width <= 700px)", false),
            ("(400px <= width <= 1000px)", true),
            ("(min-width: 600px) and", false),
            ("(min-width: 600px) (max-width: 2000px)", false),
            ("((min-width: 600px) and (hover))", true),
            ("(not (min-width: 600px))", false),
            ("(hover)", true),
            ("(monochrome)", false),
        ] {
            assert_eq!(media_matches(q, VIEW), want, "@media {q}");
        }
    }

    #[test]
    fn supports_conditions() {
        let mut doc = parse("");
        for (q, want) in [
            ("(display: grid)", true),
            ("(display: nonsense)", false),
            ("(nonsense: grid)", false),
            ("(--custom: anything)", true),
            ("not (display: nonsense)", true),
            ("(display: grid) and (color: red)", true),
            ("(display: grid) and (color: nonsense)", false),
            ("(display: nonsense) or (color: red)", true),
            ("(display: nonsense) or (color: nonsense)", false),
            ("(display: grid) and (color: red) or (width: 1px)", false),
            ("((display: grid) and (color: red))", true),
            ("(color: red !important)", true),
            ("selector(a > b)", true),
            ("selector(a >)", false),
            ("", false),
            ("display: grid", false),
        ] {
            assert_eq!(supports_matches(&mut doc, q), want, "@supports {q}");
        }
    }

    #[test]
    fn deep_groups_stop() {
        let q = format!("{}min-width: 1px{}", "(".repeat(5000), ")".repeat(5000));
        assert!(!media_matches(&q, VIEW));
        let mut doc = parse("");
        assert!(!supports_matches(&mut doc, &q));
    }

    #[test]
    fn import_list() {
        let mut doc = parse("");
        let sheet = "@charset \"utf-8\"; @import url(a.css); @import 'b.css' print; @import \"c.css\" screen and (min-width: 600px);\
            @import url(\"d.css\") layer(x) supports(display: grid) (min-width: 1px); @import url(e.css) supports(display: nonsense);\
            @import url(f.css) layer;\
            a { color: red } @import url(late.css);";
        let rules = crate::parser::parse_stylesheet(sheet);
        let list = imports(&mut doc, &rules, VIEW);
        let seen: Vec<_> = list.iter().map(|i| (i.href.as_str(), i.layer.as_deref())).collect();
        assert_eq!(seen, [("a.css", None), ("c.css", None), ("d.css", Some("x")), ("f.css", Some(""))]);
    }
}
