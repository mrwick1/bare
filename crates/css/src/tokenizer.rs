//! The CSS tokenizer: cut the text of a style sheet into tokens.
//!
//! `p.big { margin: 10px 2em }` becomes: an ident `p`, a delim `.`, an ident `big`, whitespace,
//! `{`, whitespace, an ident `margin`, `:`, whitespace, a dimension `10px`, whitespace, a dimension
//! `2em`, whitespace and `}`. The parser (the next topic) works on these tokens, and not on the text.
//! This is the CSS Syntax Level 3 standard, section 4.
//!
//! How is this different from the HTML tokenizer? CSS has no tags. It has about 20 kinds of
//! tokens, and each one is found by looking at the next one to three characters. We do not need a big
//! state machine: one function reads one token, and the next call reads the next.
//!
//! Like the HTML tokenizer, this one:
//! - gives one token at a time (an iterator), so a big style sheet never makes a big list;
//! - does not copy the text. A name or a string is a `Cow<str>`: a slice of the style sheet, or a
//!   new `String` only when something changes it (an escape like `\41`, or a NUL character).
//!
//! No CSS error stops the tokenizer. The standard says what to do for each one. For example a
//! string with a newline in it is a "bad string" token, and the tokenizer goes on after it.
//! The parser decides what to do with a bad token. This is why old style sheets with errors still
//! work: only the broken rule is lost.
//!
//! Left out: nothing from section 4. A style sheet that is not UTF-8 needs the encoding sniffing
//! of the CSS standard, and we do not have it yet (the caller gives us text).
//!
//! Comments compare Rust with JavaScript, because you know JS.

use std::borrow::Cow;

/// One piece of CSS. `'a` is the life of the style sheet text: a token can point into it.
#[derive(Debug, PartialEq, Clone)]
pub enum Token<'a> {
    /// `margin`, `div`, `-webkit-box`. The text is after unescaping.
    Ident(Cow<'a, str>),
    /// `rgb(`. A name and an open parenthesis. The text is the name.
    Function(Cow<'a, str>),
    /// `@media`. The text is the name, without the `@`.
    AtKeyword(Cow<'a, str>),
    /// `#fff`. `is_id` is true when the text is a valid name, like `#main`. `#123` is not.
    /// A selector needs this: `#main` is an id selector, and `#123` is not valid there.
    Hash { value: Cow<'a, str>, is_id: bool },
    /// `"text"` or `'text'`. The text is without the quotes.
    String(Cow<'a, str>),
    /// A string that has a newline in it: `"text` and then a new line.
    BadString,
    /// `url(image.png)`, with no quotes. The text is the address. (`url("image.png")` is a function
    /// token `url(`, a string, and `)`.)
    Url(Cow<'a, str>),
    /// A `url(` that is not valid, like `url(a b)`.
    BadUrl,
    /// A single character with no other meaning, like `.`, `>`, `*`, `+`, `~`, `!`, `=`.
    Delim(char),
    /// `10`, `-2.5`, `1e3`. `is_int` is true for a number with no `.` and no exponent.
    Number { value: f32, is_int: bool },
    /// `50%`. The value is 50.
    Percentage(f32),
    /// `10px`, `2.5em`. A number and a unit.
    Dimension { value: f32, is_int: bool, unit: Cow<'a, str> },
    /// One or more spaces, tabs or newlines.
    Whitespace,
    /// `<!--`. Old pages hid CSS in HTML comments. The parser skips this at the top level.
    Cdo,
    /// `-->`.
    Cdc,
    Colon,
    Semicolon,
    Comma,
    OpenSquare,
    CloseSquare,
    OpenParen,
    CloseParen,
    OpenCurly,
    CloseCurly,
}

/// Cut a style sheet into tokens. Gives an iterator.
/// JS: like a generator. The caller can use each token and throw it away.
pub fn tokenize(input: &str) -> Tokens<'_> {
    // `'_` says: the iterator borrows `input`, so `input` must stay alive while we use it.
    Tokens { input, pos: 0 }
}

/// The tokenizer. It is an iterator of tokens.
pub struct Tokens<'a> {
    input: &'a str,
    pos: usize,
    // The place of the next character to read, counted in bytes.
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Token<'a>;

    /// Read the next token. Gives `None` at the end of the style sheet.
    fn next(&mut self) -> Option<Token<'a>> {
        self.skip_comments();
        self.read_token()
    }
}

impl<'a> Tokens<'a> {
    /// Read the next token. The comments in front of it are already skipped.
    /// The iterator and `next_spanned` both use this.
    #[inline(always)]
    // We ask the compiler to paste this code into both places. A call for each token would cost time.
    fn read_token(&mut self) -> Option<Token<'a>> {
        let c = self.peek()?;
        // `?` returns `None` from the whole function when there is no next character.
        // JS: `if (c === undefined) return null`.
        Some(match c {
            ' ' | '\t' | '\n' | '\r' | '\x0C' => {
                while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r' | '\x0C')) {
                    self.pos += 1;
                }
                Token::Whitespace
            }
            '"' | '\'' => {
                self.pos += 1;
                self.string(c)
            }
            '#' => {
                self.pos += 1;
                let [c0, c1, _] = self.peek3();
                if c0.is_some_and(is_ident_char) || is_valid_escape(c0, c1) {
                    let is_id = starts_ident(self.peek3());
                    Token::Hash { value: self.name(), is_id }
                } else {
                    Token::Delim('#')
                }
            }
            '(' => self.one(Token::OpenParen),
            ')' => self.one(Token::CloseParen),
            '[' => self.one(Token::OpenSquare),
            ']' => self.one(Token::CloseSquare),
            '{' => self.one(Token::OpenCurly),
            '}' => self.one(Token::CloseCurly),
            ',' => self.one(Token::Comma),
            ':' => self.one(Token::Colon),
            ';' => self.one(Token::Semicolon),
            '+' | '.' if starts_number(self.peek3()) => self.numeric(),
            '-' if starts_number(self.peek3()) => self.numeric(),
            '-' if self.rest().starts_with("-->") => {
                self.pos += 3;
                Token::Cdc
            }
            '-' if starts_ident(self.peek3()) => self.ident_like(),
            '<' if self.rest().starts_with("<!--") => {
                self.pos += 4;
                Token::Cdo
            }
            '@' => {
                self.pos += 1;
                if starts_ident(self.peek3()) { Token::AtKeyword(self.name()) } else { Token::Delim('@') }
            }
            '\\' if is_valid_escape(Some('\\'), self.peek3()[1]) => self.ident_like(),
            '0'..='9' => self.numeric(),
            c if is_ident_start(c) => self.ident_like(),
            c => {
                self.pos += c.len_utf8();
                Token::Delim(c)
            }
        })
    }

    /// Read the next token, and tell where it is: the byte where it starts and the byte where it ends.
    /// The parser uses this to cut a slice out of the style sheet. The places are always at the edge of a character.
    pub fn next_spanned(&mut self) -> Option<(Token<'a>, usize, usize)> {
        self.skip_comments();
        let start = self.pos;
        let token = self.read_token()?;
        Some((token, start, self.pos))
    }

    /// The text that is not read yet.
    pub fn rest(&self) -> &'a str {
        &self.input[self.pos..]
    }

    /// Look at the next character, and do not move.
    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
        // `chars()` reads UTF-8 one character at a time. We only decode the ones that we need.
    }

    /// Look at the next three characters, and do not move. The standard often needs to look this far.
    fn peek3(&self) -> [Option<char>; 3] {
        let mut chars = self.rest().chars();
        [chars.next(), chars.next(), chars.next()]
    }

    /// Read a token that is one character, like "(".
    fn one(&mut self, token: Token<'a>) -> Token<'a> {
        self.pos += 1;
        token
    }

    /// Skip the comments in front of the next token: `/* ... */`. A comment with no end goes to the end.
    fn skip_comments(&mut self) {
        while self.rest().starts_with("/*") {
            self.pos += match self.rest()[2..].find("*/") {
                Some(i) => 2 + i + 2,
                None => self.rest().len(),
            };
        }
    }

    /// Read a name: the letters, digits, "-" and "_" of an ident, and escapes. We are at the first one.
    /// Gives a slice of the style sheet. A new `String` is made only when there is an escape or a NUL.
    fn name(&mut self) -> Cow<'a, str> {
        let start = self.pos;
        let mut owned: Option<String> = None;
        loop {
            match self.peek() {
                Some('\\') if is_valid_escape(Some('\\'), self.peek3()[1]) => {
                    let text = owned.get_or_insert_with(|| self.input[start..self.pos].to_string());
                    self.pos += 1;
                    text.push(self.escape());
                }
                Some('\0') => {
                    // A NUL is read as "�".
                    let text = owned.get_or_insert_with(|| self.input[start..self.pos].to_string());
                    self.pos += 1;
                    text.push('\u{FFFD}');
                }
                Some(c) if is_ident_char(c) => {
                    self.pos += c.len_utf8();
                    if let Some(text) = &mut owned {
                        text.push(c);
                    }
                }
                _ => break,
            }
        }
        match owned {
            Some(text) => Cow::Owned(text),
            None => Cow::Borrowed(&self.input[start..self.pos]),
        }
    }

    /// Read an escape. We just read the "\". `\41 ` is "A": up to 6 hex digits, and one space after them.
    /// `\:` is ":": any other character stands for itself.
    fn escape(&mut self) -> char {
        let Some(c) = self.peek() else { return '\u{FFFD}' };
        // A "\" at the very end of the style sheet is "�".
        if !c.is_ascii_hexdigit() {
            self.pos += c.len_utf8();
            return c;
        }
        let mut value: u32 = 0;
        let mut digits = 0;
        while digits < 6
            && let Some(d) = self.peek().and_then(|c| c.to_digit(16))
        {
            value = value * 16 + d;
            self.pos += 1;
            digits += 1;
        }
        match self.peek() {
            Some('\r') if self.rest().starts_with("\r\n") => self.pos += 2,
            Some(' ' | '\t' | '\n' | '\r' | '\x0C') => self.pos += 1,
            // One space after the digits belongs to the escape: `\41 B` is "AB".
            _ => {}
        }
        match value {
            0 | 0xD800..=0xDFFF | 0x110000.. => '\u{FFFD}',
            // Zero, a surrogate (half of a pair, not a letter), or beyond the Unicode range: "�".
            _ => char::from_u32(value).unwrap_or('\u{FFFD}'),
        }
    }

    /// Read an ident, a function (`name(`), or a url. We are at the first character of the name.
    fn ident_like(&mut self) -> Token<'a> {
        let name = self.name();
        if self.peek() != Some('(') {
            return Token::Ident(name);
        }
        self.pos += 1;
        if !name.eq_ignore_ascii_case("url") {
            return Token::Function(name);
        }
        // `url(` is special: with no quote, the content is a url token, and `;` or `/` in it are not CSS.
        // With a quote (`url("a")`), it is a normal function.
        let after_spaces = self.rest().trim_start_matches([' ', '\t', '\n', '\r', '\x0C']);
        if after_spaces.starts_with(['"', '\'']) {
            return Token::Function(name);
        }
        self.url()
    }

    /// Read the content of `url(`, with no quotes. We are after the "(".
    fn url(&mut self) -> Token<'a> {
        self.skip_spaces();
        let start = self.pos;
        let mut owned: Option<String> = None;
        // `end` is where the address ends. Spaces after it do not belong to it.
        loop {
            match self.peek() {
                Some(')') => {
                    let end = self.pos;
                    self.pos += 1;
                    return Token::Url(self.url_text(owned, start, end));
                }
                None => {
                    let end = self.pos;
                    return Token::Url(self.url_text(owned, start, end));
                }
                Some(' ' | '\t' | '\n' | '\r' | '\x0C') => {
                    let end = self.pos;
                    self.skip_spaces();
                    return match self.peek() {
                        Some(')') => {
                            self.pos += 1;
                            Token::Url(self.url_text(owned, start, end))
                        }
                        None => Token::Url(self.url_text(owned, start, end)),
                        _ => {
                            self.bad_url_rest();
                            Token::BadUrl
                        }
                    };
                }
                Some('"' | '\'' | '(') => {
                    self.bad_url_rest();
                    return Token::BadUrl;
                }
                Some(c) if is_non_printable(c) => {
                    self.bad_url_rest();
                    return Token::BadUrl;
                }
                Some('\\') => {
                    if !is_valid_escape(Some('\\'), self.peek3()[1]) {
                        self.bad_url_rest();
                        return Token::BadUrl;
                    }
                    let text = owned.get_or_insert_with(|| self.input[start..self.pos].to_string());
                    self.pos += 1;
                    text.push(self.escape());
                }
                Some(c) => {
                    self.pos += c.len_utf8();
                    if let Some(text) = &mut owned {
                        text.push(c);
                    }
                }
            }
        }
    }

    /// The address of a url token: the slice of the style sheet, or the text that we built when there was an escape.
    fn url_text(&self, owned: Option<String>, start: usize, end: usize) -> Cow<'a, str> {
        match owned {
            Some(text) => Cow::Owned(text),
            None => Cow::Borrowed(&self.input[start..end]),
        }
    }

    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r' | '\x0C')) {
            self.pos += 1;
        }
    }

    /// A url token went wrong. Skip to the ")" that ends it, so that the rest of the style sheet is fine.
    fn bad_url_rest(&mut self) {
        loop {
            match self.peek() {
                Some(')') => {
                    self.pos += 1;
                    return;
                }
                None => return,
                Some('\\') if is_valid_escape(Some('\\'), self.peek3()[1]) => {
                    self.pos += 1;
                    self.escape();
                }
                Some(c) => self.pos += c.len_utf8(),
            }
        }
    }

    /// Read a string. We just read the opening quote `quote`.
    fn string(&mut self, quote: char) -> Token<'a> {
        let start = self.pos;
        let mut owned: Option<String> = None;
        loop {
            let end = self.pos;
            match self.peek() {
                Some(c) if c == quote => {
                    self.pos += 1;
                    return Token::String(match owned {
                        Some(text) => Cow::Owned(text),
                        None => Cow::Borrowed(&self.input[start..end]),
                    });
                }
                None => {
                    // The style sheet ends inside the string. The standard says to end the string there.
                    return Token::String(match owned {
                        Some(text) => Cow::Owned(text),
                        None => Cow::Borrowed(&self.input[start..end]),
                    });
                }
                Some('\n' | '\r' | '\x0C') => return Token::BadString,
                // A newline ends a bad string. We do not read it: it is white space for the next token.
                Some('\\') => {
                    let text = owned.get_or_insert_with(|| self.input[start..self.pos].to_string());
                    self.pos += 1;
                    match self.peek() {
                        None => {}
                        // A "\" at the very end: nothing.
                        Some('\r') if self.rest().starts_with("\r\n") => self.pos += 2,
                        Some('\n' | '\r' | '\x0C') => self.pos += 1,
                        // A "\" before a newline joins the two lines. Nothing is added.
                        Some(_) => text.push(self.escape()),
                    }
                }
                Some('\0') => {
                    let text = owned.get_or_insert_with(|| self.input[start..self.pos].to_string());
                    self.pos += 1;
                    text.push('\u{FFFD}');
                }
                Some(c) => {
                    self.pos += c.len_utf8();
                    if let Some(text) = &mut owned {
                        text.push(c);
                    }
                }
            }
        }
    }

    /// Read a number, and then a unit (`10px`), a percent sign (`50%`), or nothing (`10`).
    fn numeric(&mut self) -> Token<'a> {
        let (value, is_int) = self.number();
        if starts_ident(self.peek3()) {
            Token::Dimension { value, is_int, unit: self.name() }
        } else if self.peek() == Some('%') {
            self.pos += 1;
            Token::Percentage(value)
        } else {
            Token::Number { value, is_int }
        }
    }

    /// Read a number: `+`or `-`, digits, `.` and digits, and an exponent (`e3`, `E-2`). We are at its start.
    fn number(&mut self) -> (f32, bool) {
        let start = self.pos;
        let mut is_int = true;
        if matches!(self.peek(), Some('+' | '-')) {
            self.pos += 1;
        }
        self.digits();
        let [c0, c1, _] = self.peek3();
        if c0 == Some('.') && c1.is_some_and(|c| c.is_ascii_digit()) {
            is_int = false;
            self.pos += 1;
            self.digits();
        }
        let [c0, c1, c2] = self.peek3();
        if matches!(c0, Some('e' | 'E')) && (c1.is_some_and(|c| c.is_ascii_digit()) || (matches!(c1, Some('+' | '-')) && c2.is_some_and(|c| c.is_ascii_digit()))) {
            is_int = false;
            self.pos += 2;
            self.digits();
        }
        let value = self.input[start..self.pos].parse::<f32>().unwrap_or(0.0);
        // `parse` turns the text into a number. A number that is too big becomes infinity: we keep it in range.
        (value.clamp(f32::MIN, f32::MAX), is_int)
    }

    fn digits(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
    }
}

/// A letter, "_", or any character that is not ASCII. A name can start with one of these.
fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || !c.is_ascii()
}

/// An ident start, a digit, or "-".
fn is_ident_char(c: char) -> bool {
    is_ident_start(c) || c.is_ascii_digit() || c == '-'
}

/// A "\" and then a character that is not a newline. It is an escape. At the end of the style sheet
/// (`second` is `None`) it is an escape too.
fn is_valid_escape(first: Option<char>, second: Option<char>) -> bool {
    first == Some('\\') && !matches!(second, Some('\n' | '\r' | '\x0C'))
}

/// Do these three characters start a name? `-a`, `--x`, `a`, `\41` do. `-1` and `1` do not.
fn starts_ident([c0, c1, c2]: [Option<char>; 3]) -> bool {
    match c0 {
        Some('-') => c1.is_some_and(|c| is_ident_start(c) || c == '-') || is_valid_escape(c1, c2),
        Some(c) if is_ident_start(c) => true,
        Some('\\') => is_valid_escape(c0, c1),
        _ => false,
    }
}

/// Do these three characters start a number? `1`, `.5`, `-1`, `+.5` do. `-a` and `.a` do not.
fn starts_number([c0, c1, c2]: [Option<char>; 3]) -> bool {
    let digit = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit());
    match c0 {
        Some('+' | '-') => digit(c1) || (c1 == Some('.') && digit(c2)),
        Some('.') => digit(c1),
        c => digit(c),
    }
}

/// The control characters that cannot be in an unquoted url.
fn is_non_printable(c: char) -> bool {
    matches!(c, '\0'..='\x08' | '\x0B' | '\x0E'..='\x1F' | '\x7F')
}

#[cfg(test)]
// Only compiled for `cargo test`.
mod tests {
    use super::*;

    fn all(input: &str) -> Vec<Token<'_>> {
        tokenize(input).collect()
    }

    fn ident(s: &str) -> Token<'_> {
        Token::Ident(s.into())
    }

    fn dim(value: f32, unit: &str) -> Token<'_> {
        Token::Dimension { value, is_int: value.fract() == 0.0, unit: unit.into() }
    }

    fn num(value: f32, is_int: bool) -> Token<'static> {
        Token::Number { value, is_int }
    }

    #[test]
    fn reads_a_rule() {
        use Token::*;
        assert_eq!(
            all("p.big { margin: 10px 2em }"),
            [ident("p"), Delim('.'), ident("big"), Whitespace, OpenCurly, Whitespace, ident("margin"), Colon, Whitespace, dim(10.0, "px"), Whitespace, dim(2.0, "em"), Whitespace, CloseCurly]
        );
    }

    #[test]
    fn skips_comments() {
        use Token::*;
        assert_eq!(all("a/* x */b"), [ident("a"), ident("b")]);
        assert_eq!(all("a /* x"), [ident("a"), Whitespace]);
        // A comment with no end goes to the end of the style sheet.
        assert_eq!(all("/**/"), []);
        assert_eq!(all("/*/ a */b"), [ident("b")]);
        // `/*/` is not a whole comment: the "*" cannot be the end of its own start.
    }

    #[test]
    fn whitespace_is_one_token() {
        assert_eq!(all(" \t\n\r\n\x0C "), [Token::Whitespace]);
    }

    #[test]
    fn reads_numbers() {
        assert_eq!(all("10"), [num(10.0, true)]);
        assert_eq!(all("-2.5"), [num(-2.5, false)]);
        assert_eq!(all("+.5"), [num(0.5, false)]);
        assert_eq!(all(".5"), [num(0.5, false)]);
        assert_eq!(all("1e3"), [num(1000.0, false)]);
        assert_eq!(all("1E-2"), [num(0.01, false)]);
        assert_eq!(all("0"), [num(0.0, true)]);
        assert_eq!(all("1e999"), [num(f32::MAX, false)]);
        // Too big for the number type: we use the biggest number.
    }

    #[test]
    fn reads_dimensions_and_percentages() {
        assert_eq!(all("10px"), [dim(10.0, "px")]);
        assert_eq!(all("1.5em"), [Token::Dimension { value: 1.5, is_int: false, unit: "em".into() }]);
        assert_eq!(all("50%"), [Token::Percentage(50.0)]);
        assert_eq!(all("1e3px"), [Token::Dimension { value: 1000.0, is_int: false, unit: "px".into() }]);
        // `1e` is not an exponent without a digit: it is a number `1` with the unit `e`.
        assert_eq!(all("1e"), [dim(1.0, "e")]);
        assert_eq!(all("1em"), [dim(1.0, "em")]);
        // "e" and then "m": not an exponent, so `1em` is a number with the unit `em`.
        assert_eq!(all("10-5"), [num(10.0, true), num(-5.0, true)]);
    }

    #[test]
    fn dot_and_plus_and_minus() {
        use Token::*;
        assert_eq!(all(".a"), [Delim('.'), ident("a")]);
        assert_eq!(all("+a"), [Delim('+'), ident("a")]);
        assert_eq!(all("-a"), [ident("-a")]);
        assert_eq!(all("--x"), [ident("--x")]);
        assert_eq!(all("-"), [Delim('-')]);
        assert_eq!(all("a-b_c9"), [ident("a-b_c9")]);
    }

    #[test]
    fn reads_hashes() {
        assert_eq!(all("#fff"), [Token::Hash { value: "fff".into(), is_id: true }]);
        assert_eq!(all("#main"), [Token::Hash { value: "main".into(), is_id: true }]);
        assert_eq!(all("#123"), [Token::Hash { value: "123".into(), is_id: false }]);
        // `#123` is a color-like hash, and not a valid id selector.
        assert_eq!(all("# "), [Token::Delim('#'), Token::Whitespace]);
    }

    #[test]
    fn reads_at_keywords_and_functions() {
        assert_eq!(all("@media"), [Token::AtKeyword("media".into())]);
        assert_eq!(all("@ "), [Token::Delim('@'), Token::Whitespace]);
        assert_eq!(all("rgb(0"), [Token::Function("rgb".into()), num(0.0, true)]);
        assert_eq!(all("calc ("), [ident("calc"), Token::Whitespace, Token::OpenParen]);
        // A space between the name and "(" makes it an ident and a parenthesis.
    }

    #[test]
    fn reads_strings() {
        assert_eq!(all("\"a b\""), [Token::String("a b".into())]);
        assert_eq!(all("'a\"b'"), [Token::String("a\"b".into())]);
        assert_eq!(all("\"\""), [Token::String("".into())]);
        assert_eq!(all("\"abc"), [Token::String("abc".into())]);
        // A string with no end ends at the end of the style sheet.
        assert_eq!(all("\"a\nb\""), [Token::BadString, Token::Whitespace, ident("b"), Token::String("".into())]);
        // A newline ends a bad string. The rest, `b"`, is read as normal CSS: `b`, and a new string with no end.
        assert_eq!(all("\"a\\\nb\""), [Token::String("ab".into())]);
        // A "\" before a newline joins the lines.
        assert_eq!(all("\"a\\\"b\""), [Token::String("a\"b".into())]);
    }

    #[test]
    fn reads_escapes() {
        assert_eq!(all("\\41 B"), [ident("AB")]);
        // `\41 ` is "A". The space after the digits is part of the escape.
        assert_eq!(all("\\41B"), [ident("\u{41B}")]);
        // `\41B` has the digits 4, 1 and B: that is the letter "Л".
        assert_eq!(all("a\\:b"), [ident("a:b")]);
        assert_eq!(all("\\0 "), [ident("\u{FFFD}")]);
        assert_eq!(all("\\D800 "), [ident("\u{FFFD}")]);
        assert_eq!(all("\\110000 "), [ident("\u{FFFD}")]);
        assert_eq!(all("a\\"), [ident("a\u{FFFD}")]);
        // A "\" at the end is "�".
        assert_eq!(all("a\\\nb"), [ident("a"), Token::Delim('\\'), Token::Whitespace, ident("b")]);
        // A "\" before a newline is not an escape.
    }

    #[test]
    fn nul_is_replaced() {
        assert_eq!(all("a\0b"), [ident("a\u{FFFD}b")]);
    }

    #[test]
    fn reads_urls() {
        assert_eq!(all("url(a.png)"), [Token::Url("a.png".into())]);
        assert_eq!(all("url(  a.png  )"), [Token::Url("a.png".into())]);
        assert_eq!(all("URL(a)"), [Token::Url("a".into())]);
        assert_eq!(all("url()"), [Token::Url("".into())]);
        assert_eq!(all("url(a"), [Token::Url("a".into())]);
        assert_eq!(all("url(a\\ b)"), [Token::Url("a b".into())]);
        assert_eq!(all("url(http://x/y;z)"), [Token::Url("http://x/y;z".into())]);
        // The `;` and `//` are part of the address. A normal tokenizer would cut here.
    }

    #[test]
    fn a_quoted_url_is_a_function() {
        assert_eq!(all("url(\"a\")"), [Token::Function("url".into()), Token::String("a".into()), Token::CloseParen]);
        assert_eq!(all("url( 'a')"), [Token::Function("url".into()), Token::Whitespace, Token::String("a".into()), Token::CloseParen]);
        // The spaces stay in the stream. The parser skips them.
    }

    #[test]
    fn bad_urls() {
        use Token::*;
        assert_eq!(all("url(a b) x"), [BadUrl, Whitespace, ident("x")]);
        assert_eq!(all("url(a\"b) x"), [BadUrl, Whitespace, ident("x")]);
        assert_eq!(all("url(a(b) x"), [BadUrl, Whitespace, ident("x")]);
        assert_eq!(all("url(a\x01b) x"), [BadUrl, Whitespace, ident("x")]);
        assert_eq!(all("url(a\\\nb) x"), [BadUrl, Whitespace, ident("x")]);
        // The tokenizer skips to the ")" so that the next rule is still fine.
    }

    #[test]
    fn cdo_and_cdc() {
        use Token::*;
        assert_eq!(all("<!-- a -->"), [Cdo, Whitespace, ident("a"), Whitespace, Cdc]);
        assert_eq!(all("<!-"), [Delim('<'), Delim('!'), Delim('-')]);
        assert_eq!(all("--> "), [Cdc, Whitespace]);
        assert_eq!(all("->"), [Delim('-'), Delim('>')]);
    }

    #[test]
    fn symbols_and_delims() {
        use Token::*;
        assert_eq!(all("()[]{},:;"), [OpenParen, CloseParen, OpenSquare, CloseSquare, OpenCurly, CloseCurly, Comma, Colon, Semicolon]);
        assert_eq!(all("*>+~!=|"), [Delim('*'), Delim('>'), Delim('+'), Delim('~'), Delim('!'), Delim('='), Delim('|')]);
        assert_eq!(all("é"), [ident("é")]);
        // A character that is not ASCII is part of a name.
    }

    #[test]
    fn borrows_from_the_style_sheet_when_it_can() {
        // A `Cow::Borrowed` is a slice of the style sheet: no new `String`.
        let tokens = all("a\\41 b #c \"d\" url(e) f(");
        assert!(matches!(tokens[0], Token::Ident(Cow::Owned(_)))); // An escape changed it.
        assert!(matches!(tokens[2], Token::Hash { value: Cow::Borrowed("c"), .. }));
        assert!(matches!(tokens[4], Token::String(Cow::Borrowed("d"))));
        assert!(matches!(tokens[6], Token::Url(Cow::Borrowed("e"))));
        assert!(matches!(tokens[8], Token::Function(Cow::Borrowed("f"))));
    }

    #[test]
    fn empty_input() {
        assert_eq!(all(""), []);
    }

    #[test]
    fn gives_tokens_one_at_a_time() {
        let sheet = "a{}".repeat(1000);
        let mut tokens = tokenize(&sheet);
        assert_eq!(tokens.next(), Some(ident("a")));
        assert_eq!(tokens.pos, 1);
        // One token read only one token's worth of text.
    }

    #[test]
    fn never_panics_or_loops_on_junk() {
        let pieces = ["a", "-", "--", "1", ".", "e", "+", "%", "px", "#", "@", "\\", "\\41 ", "\"", "'", "\n", " ", "/*", "*/", "/", "*", "(", ")", "{", "}", "[", "]", ";", ":", ",", "url(", "url(a", "<!--", "-->", "!", "é", "\0", "😀", "\r\n", "\x0C", "1e", "1e+", "e-", ".5", "0x"];
        let mut seed: u32 = 12345;
        for _ in 0..4000 {
            let mut sheet = String::new();
            for _ in 0..(seed % 24) {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                // A simple random number generator. The same numbers every run, so a failure can be repeated.
                sheet.push_str(pieces[(seed >> 16) as usize % pieces.len()]);
            }
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            // Each token must use at least one byte, or the loop would never end.
            let mut tokens = tokenize(&sheet);
            let mut last = 0;
            while tokens.next().is_some() {
                assert!(tokens.pos > last, "no progress on {sheet:?}");
                last = tokens.pos;
            }
            assert_eq!(tokens.pos, sheet.len(), "stopped early on {sheet:?}");
        }
    }
}
