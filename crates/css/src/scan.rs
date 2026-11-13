//! A byte scanner that finds where a style rule ends, with no tokens.
//!
//! The parser (`parser.rs`) cuts a style rule in two: the prelude (`a, b > c`) up to the `{`, and the block up to the `}` that
//! closes it. To find the `}` it reads every token of the block. This scanner finds the same two places by looking at bytes. It
//! costs about 9 instructions per byte, and the tokenizer costs about 75.
//!
//! It must give exactly the places that the parser gives. The parser's rules are these:
//! - `(`, `[` and `{` open a block. A block ends at its own closing token: `]` does not close `(`, and a closing token of
//!   another kind, or one with no block open, is ignored. This is `Parser::skip_block`.
//! - A string, a comment, an unquoted `url(...)` and an escape (`\}`) hide the characters in them.
//! - In a prelude, a `{` with no block open ends the prelude. A `}` with no block open ends the rule that is cut (when `nested`).
//!
//! The scanner does not copy the tokenizer for the rare cases. It gives `Found::Bail` instead, and the parser reads that rule
//! with the tokenizer, as before: an escape in the block, an escape next to a `(` (it could make the name `url`), or a nesting
//! of more than 64 blocks. A wrong answer here would move the edge of a rule, so every rule that the scanner is not sure about
//! goes the slow way.

/// What the scanner found for the rule that starts at `start`.
#[derive(Debug, PartialEq)]
pub(crate) enum Found {
    /// A rule with a block. The prelude is `start..prelude_end` (not trimmed). The block is `body_start..body_end`. The next rule
    /// starts at `next`. `body_end` is the place of the `}`, or the end of the text when the block is not closed.
    /// `motion` is `true` when a declaration of the block may have a name that starts with `transition` or `animation`.
    Rule { prelude_end: usize, body_start: usize, body_end: usize, next: usize, motion: bool },
    /// There is no block: the text ended, or (when `nested`) the `}` of the block around us came first. The caller goes on at `at`.
    Cut { at: usize },
    /// Not sure: use the tokenizer.
    Bail,
}

const PLAIN: u8 = 0;
const OPEN_CURLY: u8 = 1;
const CLOSE_CURLY: u8 = 2;
const OPEN_PAREN: u8 = 3;
const CLOSE_PAREN: u8 = 4;
const OPEN_SQUARE: u8 = 5;
const CLOSE_SQUARE: u8 = 6;
const QUOTE: u8 = 7;
const SLASH: u8 = 8;
const BACKSLASH: u8 = 9;
const SEMICOLON: u8 = 10;

/// The class of each byte. Only the bytes that can change what we scan are not `PLAIN`.
const CLASS: [u8; 256] = {
    let mut t = [PLAIN; 256];
    t[b'{' as usize] = OPEN_CURLY;
    t[b'}' as usize] = CLOSE_CURLY;
    t[b'(' as usize] = OPEN_PAREN;
    t[b')' as usize] = CLOSE_PAREN;
    t[b'[' as usize] = OPEN_SQUARE;
    t[b']' as usize] = CLOSE_SQUARE;
    t[b'"' as usize] = QUOTE;
    t[b'\'' as usize] = QUOTE;
    t[b'/' as usize] = SLASH;
    t[b'\\' as usize] = BACKSLASH;
    t[b';' as usize] = SEMICOLON;
    t
};

/// The deepest nesting of blocks that we follow.
const DEPTH: usize = 64;

/// A byte of a name: a letter, a digit, `-`, `_`, or a part of a character that is not ASCII.
fn is_name(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b >= 0x80
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0C')
}

/// Find the rule that starts at `start`, which is the first byte of its first token.
pub(crate) fn rule(text: &str, start: usize, nested: bool) -> Found {
    let b = text.as_bytes();
    let n = b.len();
    // The closing class that each open block waits for. Entry 0 is the block of the rule.
    let mut stack = [CLOSE_CURLY; DEPTH];
    let mut depth = 0;
    let mut i = start;
    // The place of the last escape that is outside a string and a url: a name next to it could be `url`.
    let mut last_escape = usize::MAX;
    // `body_start` is 0 while we are in the prelude (a block cannot start at byte 0: the `{` comes before it).
    let (mut prelude_end, mut body_start) = (0, 0);
    let mut motion = false;
    // The comments that touch each other and end at the place we are: `a /* b */ /* c */` ends its last token before the first `/*`
    // only when no space follows the last comment. The parser ends the prelude at the end of its last token, and a comment is not one.
    let (mut comments_start, mut comments_end) = (0, 0);
    loop {
        while i < n && CLASS[b[i] as usize] == PLAIN {
            i += 1;
        }
        if i >= n {
            return if body_start == 0 { Found::Cut { at: n } } else { Found::Rule { prelude_end, body_start, body_end: n, next: n, motion } };
        }
        match CLASS[b[i] as usize] {
            OPEN_CURLY if depth == 0 && body_start == 0 => {
                prelude_end = if comments_end == i { comments_start } else { i };
                i += 1;
                body_start = i;
                stack[0] = CLOSE_CURLY;
                depth = 1;
                motion = names_motion(b, i);
            }
            CLOSE_CURLY => {
                if depth > 0 && stack[depth - 1] == CLOSE_CURLY {
                    depth -= 1;
                    if depth == 0 {
                        // Only the block of the rule can bring us here: in the prelude, `depth` is 0 and we gave `Cut` or ignored the `}`.
                        return Found::Rule { prelude_end, body_start, body_end: i, next: i + 1, motion };
                    }
                } else if depth == 0 && body_start == 0 && nested {
                    return Found::Cut { at: i };
                }
                i += 1;
            }
            OPEN_CURLY | OPEN_SQUARE | OPEN_PAREN => {
                let c = b[i];
                if c == b'(' {
                    if last_escape != usize::MAX && i - last_escape <= 24 {
                        return Found::Bail;
                    }
                    if i >= start + 3 && b[i - 3..i].eq_ignore_ascii_case(b"url") {
                        // A NUL goes on a name that is open, and starts none: we do not look further back.
                        if i > start + 3 && b[i - 4] == 0 {
                            return Found::Bail;
                        }
                    }
                    if i >= start + 3 && b[i - 3..i].eq_ignore_ascii_case(b"url") && starts_name(b, i - 3, start) {
                        let mut j = i + 1;
                        while j < n && is_space(b[j]) {
                            j += 1;
                        }
                        if j < n && matches!(b[j], b'"' | b'\'') {
                            // `url("a")` is a function with a string in it.
                        } else {
                            // An unquoted url ends at the first `)` that is not escaped. A bad one does too. It opens no block.
                            i = url_end(b, i + 1);
                            continue;
                        }
                    }
                }
                if depth == DEPTH {
                    return Found::Bail;
                }
                stack[depth] = match c {
                    b'(' => CLOSE_PAREN,
                    b'[' => CLOSE_SQUARE,
                    _ => CLOSE_CURLY,
                };
                depth += 1;
                i += 1;
            }
            CLOSE_PAREN | CLOSE_SQUARE => {
                if depth > 0 && stack[depth - 1] == CLASS[b[i] as usize] {
                    depth -= 1;
                }
                i += 1;
            }
            QUOTE => i = string_end(b, i),
            SLASH => {
                if b.get(i + 1) == Some(&b'*') {
                    if comments_end != i {
                        comments_start = i;
                    }
                    i = match text[i + 2..].find("*/") {
                        Some(k) => i + 2 + k + 2,
                        None => n,
                    };
                    comments_end = i;
                } else {
                    i += 1;
                }
            }
            BACKSLASH => {
                if body_start != 0 {
                    // An escape in a block could be in the name of a property.
                    return Found::Bail;
                }
                last_escape = i;
                i = escape_end(b, i + 1);
            }
            SEMICOLON => {
                if body_start != 0 && depth == 1 && !motion {
                    motion = names_motion(b, i + 1);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
}

/// Does a name start at `at`? It does when the byte before is not a part of a token that goes on with name bytes: a name, a number, a `#`,
/// an `@`, an escape. A `<!--` is a token of its own, so `<!--url(` has the name `url`.
fn starts_name(b: &[u8], at: usize, start: usize) -> bool {
    at == start || {
        let c = b[at - 1];
        !(is_name(c) || matches!(c, b'\\' | b'@' | b'#')) || (c == b'-' && at >= start + 4 && &b[at - 4..at] == b"<!--")
    }
}

/// After an escape: the place where the text goes on. We are after the `\`. An escape is `\` and one character, or `\` and up to
/// 6 hex digits and one space (CSS Syntax 4.3.7). A `\` and a newline is no escape: the `\` stands alone.
fn escape_end(b: &[u8], mut i: usize) -> usize {
    match b.get(i) {
        None => i,
        Some(b'\n' | b'\r' | b'\x0C') => i,
        Some(c) if c.is_ascii_hexdigit() => {
            let end = (i + 6).min(b.len());
            while i < end && b[i].is_ascii_hexdigit() {
                i += 1;
            }
            match b.get(i) {
                Some(b'\r') if b.get(i + 1) == Some(&b'\n') => i + 2,
                Some(c) if is_space(*c) => i + 1,
                _ => i,
            }
        }
        Some(_) => i + 1,
    }
}

/// After a string: the place after the closing quote. `i` is at the opening quote. A newline ends a string that is bad: the
/// tokenizer does not read the newline, and neither do we.
fn string_end(b: &[u8], i: usize) -> usize {
    let quote = b[i];
    let mut i = i + 1;
    while i < b.len() {
        match b[i] {
            c if c == quote => return i + 1,
            b'\n' | b'\r' | b'\x0C' => return i,
            b'\\' => {
                i += 1;
                match b.get(i) {
                    None => return i,
                    Some(b'\r') if b.get(i + 1) == Some(&b'\n') => i += 2,
                    Some(b'\n' | b'\r' | b'\x0C') => i += 1,
                    _ => i = escape_end(b, i),
                }
            }
            _ => i += 1,
        }
    }
    i
}

/// After an unquoted url: the place after its `)`, or the end of the text. `i` is after the `(`.
fn url_end(b: &[u8], mut i: usize) -> usize {
    while i < b.len() {
        match b[i] {
            b')' => return i + 1,
            b'\\' => i += 2,
            _ => i += 1,
        }
    }
    b.len()
}

/// Does the declaration that starts at `i` (after a `{` or a `;`) have a name that may be `transition…` or `animation…`? Spaces and
/// comments before the name are skipped. We say `true` when a comment does not end.
fn names_motion(b: &[u8], mut i: usize) -> bool {
    loop {
        while i < b.len() && is_space(b[i]) {
            i += 1;
        }
        if !b[i..].starts_with(b"/*") {
            break;
        }
        match b[i + 2..].windows(2).position(|w| w == b"*/") {
            Some(end) => i += end + 4,
            None => return true,
        }
    }
    let head = &b[i..(i + 6).min(b.len())];
    head.eq_ignore_ascii_case(b"transi") || head.eq_ignore_ascii_case(b"animat")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cut(text: &str) -> (&str, &str, &str) {
        match rule(text, 0, false) {
            Found::Rule { prelude_end, body_start, body_end, next, .. } => (&text[..prelude_end], &text[body_start..body_end], &text[next..]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn finds_the_block() {
        assert_eq!(cut("a{b:c} d{}"), ("a", "b:c", " d{}"));
        assert_eq!(cut("a{b:\"}\"}x"), ("a", "b:\"}\"", "x"));
        assert_eq!(cut("a{b:url(}}x)}x"), ("a", "b:url(}}x)", "x"));
        assert_eq!(cut("a{b:url( \"}\" )}x"), ("a", "b:url( \"}\" )", "x"));
        assert_eq!(cut("a{b:(1}2)}x"), ("a", "b:(1}2)", "x"));
        assert_eq!(cut("a{b:/*}*/c}x"), ("a", "b:/*}*/c", "x"));
        assert_eq!(cut("a[b=\"{\"]{c:d}x"), ("a[b=\"{\"]", "c:d", "x"));
        assert_eq!(cut("a{b:c;d"), ("a", "b:c;d", ""));
    }

    #[test]
    fn bails_when_it_is_not_sure() {
        assert_eq!(rule("a{b:\\}}", 0, false), Found::Bail);
        assert_eq!(rule("a\\75rl(x{}", 0, false), Found::Bail);
        assert_eq!(rule(&format!("a{{b:{}1}}", "(".repeat(100)), 0, false), Found::Bail);
        // An escape in a prelude is fine when it is not next to a `(`.
        assert!(matches!(rule(".a\\:b{c:d}", 0, false), Found::Rule { .. }));
        // An escape in a string is fine.
        assert!(matches!(rule("a{content:\"\\f101\"}", 0, false), Found::Rule { .. }));
    }

    #[test]
    fn motion_is_a_name_that_starts_like_transition_or_animation() {
        let motion = |t: &str| matches!(rule(t, 0, false), Found::Rule { motion: true, .. });
        assert!(motion("a{transition:1s}"));
        assert!(motion("a{x:y; TRANSITION-delay:1s}"));
        assert!(motion("a{x:y;animation-name:z}"));
        assert!(motion("a{/* x */ transition:1s}") && motion("a{x:y; /**/ /* y */animation:z}"));
        assert!(!motion("a{/* transition */ color:red}") && !motion("a{x:y;/* animation */}"));
        assert!(!motion("a{x:y;transform:none;align-items:x;--transition:1;background:transition}"));
    }
}
