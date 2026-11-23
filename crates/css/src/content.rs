//! The `content` property: the text of a `::before` or `::after` box (CSS Generated Content 3).
//!
//! A value is 8 bytes and cannot hold text. So the computed value is a number (`Value::Number`) of the text in `urls` (the text table of
//! the sheets), as for `background-image`. The text is the value as `getComputedStyle` gives it: `"a" attr(title)`. `text` reads it again
//! to make the text of the box. `normal` and `none` are keywords, and they make no box.
//!
//! Known: strings, `attr(name)`, `open-quote` and `close-quote` (the English quotes), `no-open-quote` and `no-close-quote` (no text).
//! Left out: `counter()` and `counters()` give no text, `url()` and the other images give no text, and the quotes do not nest.

use std::borrow::Cow;

use crate::tokenizer::{Token, tokenize};
use crate::values::{Declared, Keyword, Property, Value};

/// One item of the value.
enum Item<'a> {
    Text(Cow<'a, str>),
    Attr(Cow<'a, str>),
    /// A keyword or a function that is kept as written: `open-quote`, `counter(x)`.
    Word(String),
}

/// Read the items. `None` when the value is not a list of known items.
fn items(value: &str) -> Option<Vec<Item<'_>>> {
    let mut out = Vec::new();
    let mut tokens = tokenize(value).filter(|t| *t != Token::Whitespace);
    while let Some(t) = tokens.next() {
        out.push(match t {
            Token::String(s) => Item::Text(s),
            Token::Ident(w) if ["open-quote", "close-quote", "no-open-quote", "no-close-quote"].iter().any(|k| w.eq_ignore_ascii_case(k)) => Item::Word(w.to_ascii_lowercase()),
            Token::Function(f) if f.eq_ignore_ascii_case("attr") => {
                let (Some(Token::Ident(name)), Some(Token::CloseParen)) = (tokens.next(), tokens.next()) else { return None };
                Item::Attr(name)
            }
            Token::Function(f) if ["counter", "counters", "url", "image-set", "linear-gradient", "radial-gradient"].iter().any(|k| f.eq_ignore_ascii_case(k)) => {
                // Kept as written. It gives no text (see the top of the file).
                let mut word = format!("{}(", f.to_ascii_lowercase());
                let mut depth = 1;
                for t in tokens.by_ref() {
                    match t {
                        Token::CloseParen => depth -= 1,
                        Token::Function(_) | Token::OpenParen => depth += 1,
                        _ => {}
                    }
                    if depth == 0 {
                        break;
                    }
                    word.push_str(&token_text(&t)?);
                }
                if depth != 0 {
                    return None;
                }
                word.push(')');
                Item::Word(word)
            }
            Token::Url(u) => Item::Word(format!("url({u})")),
            _ => return None,
        });
    }
    (!out.is_empty()).then_some(out)
}

/// The text of a token inside a kept function.
fn token_text(t: &Token<'_>) -> Option<String> {
    Some(match t {
        Token::Ident(s) => s.to_string(),
        Token::String(s) => quoted(s),
        Token::Function(f) => format!("{f}("),
        Token::OpenParen => "(".into(),
        Token::Comma => ", ".into(),
        Token::Number { value, .. } => value.to_string(),
        Token::Delim(c) => c.to_string(),
        _ => return None,
    })
}

/// A CSS string with double quotes, as `getComputedStyle` writes it.
fn quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Read a declaration of `content` into `out`. `false` when the value is not good.
pub(crate) fn declaration(value: &str, important: bool, out: &mut Vec<Declared>) -> bool {
    let v = value.trim();
    let keyword = |k| Some(Value::Keyword(k));
    let found = match v.to_ascii_lowercase().as_str() {
        "normal" => keyword(Keyword::Normal),
        "none" => keyword(Keyword::None),
        "inherit" => Some(Value::Inherit),
        "initial" => Some(Value::Initial),
        "unset" => Some(Value::Unset),
        _ => items(v).map(|list| {
            let text: Vec<String> = list
                .into_iter()
                .map(|i| match i {
                    Item::Text(s) => quoted(&s),
                    Item::Attr(n) => format!("attr({n})"),
                    Item::Word(w) => w,
                })
                .collect();
            let id = crate::urls::intern(&text.join(" "));
            if id == 0 { Value::Keyword(Keyword::None) } else { Value::Number(id as f32) }
        }),
    };
    let Some(value) = found else { return false };
    out.push(Declared { property: Property::Content, value, important, mirror: false });
    true
}

/// The value as `getComputedStyle` gives it.
pub fn serialize(v: Value) -> String {
    match v {
        Value::Number(id) => crate::urls::get(id as u32).map_or_else(|| "none".into(), |t| t.to_string()),
        Value::Keyword(Keyword::None) => "none".into(),
        _ => "normal".into(),
    }
}

/// The text of the box, or `None` when the value makes no box (`normal`, `none`). `attr` gives the value of an attribute of the element.
pub fn text(v: Value, attr: impl Fn(&str) -> Option<String>) -> Option<String> {
    let Value::Number(id) = v else { return None };
    let source = crate::urls::get(id as u32)?;
    let mut out = String::new();
    for item in items(&source)? {
        match item {
            Item::Text(s) => out.push_str(&s),
            Item::Attr(n) => out.push_str(&attr(&n).unwrap_or_default()),
            Item::Word(w) if w == "open-quote" => out.push('\u{201c}'),
            Item::Word(w) if w == "close-quote" => out.push('\u{201d}'),
            Item::Word(_) => {}
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: &str) -> Option<Value> {
        let mut out = Vec::new();
        declaration(v, false, &mut out).then(|| out[0].value)
    }

    #[test]
    fn values_parse_and_serialize_as_in_chrome() {
        assert_eq!(parse("normal"), Some(Value::Keyword(Keyword::Normal)));
        assert_eq!(parse("NONE"), Some(Value::Keyword(Keyword::None)));
        assert_eq!(parse("inherit"), Some(Value::Inherit));
        for (v, want) in [("'a'", "\"a\""), ("\"\"", "\"\""), ("'x' attr(title) \"y\"", "\"x\" attr(title) \"y\""), ("'q\"'", "\"q\\\"\""), ("open-quote 'a' close-quote", "open-quote \"a\" close-quote"), ("counter(item) '. '", "counter(item) \". \"")] {
            assert_eq!(parse(v).map(serialize).as_deref(), Some(want), "{v}");
        }
        for v in ["", "red", "attr()", "attr(a b)", "'a' 2px", "counter(x"] {
            assert_eq!(parse(v), None, "{v:?}");
        }
    }

    #[test]
    fn text_reads_strings_attributes_and_quotes() {
        let attr = |n: &str| (n == "title").then(|| "T".to_string());
        assert_eq!(text(parse("'a' attr(title) attr(nope) 'b'").unwrap(), attr).as_deref(), Some("aTb"));
        assert_eq!(text(parse("''").unwrap(), attr).as_deref(), Some(""));
        assert_eq!(text(parse("open-quote 'x' close-quote counter(n)").unwrap(), attr).as_deref(), Some("\u{201c}x\u{201d}"));
        assert_eq!(text(parse("none").unwrap(), attr), None);
        assert_eq!(text(parse("normal").unwrap(), attr), None);
    }
}
