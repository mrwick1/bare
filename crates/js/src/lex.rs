//! The JavaScript lexer (ES2022 script syntax): cut source text into tokens.
//!
//! The parser calls [`Lexer::next`] once per token and says which *lexical goal* to use. The same
//! `/` is division in one place and the start of a regexp in another, and only the parser knows
//! which. The same `}` ends a block in one place and continues a template string in another. So:
//!
//! - [`Goal::Div`]: `/` and `/=` are operators. A backtick starts a template (this is true in every goal).
//! - [`Goal::RegExp`]: `/` starts a regexp literal `/body/flags`. Use it where an expression may start.
//! - [`Goal::TemplateTail`]: the next token must be the `}` that closes a `${ ... }`. The lexer scans
//!   the template text after it, up to the next `${` or the closing backtick.
//!
//! The lexer keeps no nesting stack. For `` `a${ `b${c}` }` `` the parser knows how many `${` are
//! open. If the parser already has a token and finds out later that it needs another goal (it read `/`
//! and now sees an expression start), it calls [`Lexer::rescan`] with that token.
//!
//! A [`Token`] is 12 bytes: a kind, a byte span into the source, and two flags. It has no `String`.
//! The source is borrowed. Values are decoded only on request ([`decode_string`], [`decode_template`],
//! [`ident_name`], [`number_value`]). Strings decode to UTF-16 code units, because JavaScript strings
//! are sequences of UTF-16 code units, and `"\uD800"` (a lone surrogate) is valid JavaScript but not
//! valid UTF-8 or Rust `String`.
//!
//! This is scanning, not parsing. Left out on purpose:
//! - Regexp *grammar*: the lexer finds where `/.../flags` ends and checks the flags. It does not check
//!   the pattern (`/(/` scans fine). Only a regexp engine can do that.
//! - Nothing for identifiers: names may have non-ASCII letters (`é`, `日本`) and `\u00e9` or `\u{1F600}`
//!   escapes. The ID_Start and ID_Continue sets come from `icu_properties` (Unicode data, not
//!   `char::is_alphabetic`). A non-ASCII character that is not in them is an error ([`ErrorKind::NonAscii`]).
//! - HTML-like comments (`<!--`, `-->`, Annex B) are comments only when the parser asks for them
//!   ([`Lexer::set_html_comments`], for a script, not a module). Else they scan as operators.
//! - Strict-mode rules (legacy octal, reserved words as names). The token reports the facts
//!   (`Kind::String { legacy_octal }`, `NumberBase::LegacyOctal`, `escapes`); the parser decides.
//! - BigInt values. `10n` scans as [`Kind::BigInt`]; no value is computed.
//! - Context words (`let`, `async`, `of`, `get`, `static`, `yield`, `await`) are [`Kind::Ident`].
//!   Only always-reserved words are [`Kind::Keyword`].
//!
//! Comments are JS comments for readers who know JS only: a `Result` is like a function that
//! returns a value or throws, but the error is part of the return type.

use crate::number;
use std::borrow::Cow;

/// The most source bytes the lexer accepts (64 MiB). Offsets are `u32`, so they never overflow.
pub const MAX_SOURCE_LEN: usize = 64 << 20;

/// Which grammar the next token follows. See the module comment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Goal {
    Div,
    RegExp,
    TemplateTail,
}

macro_rules! words {
    ($(#[$m:meta])* $name:ident { $($v:ident = $s:literal),* $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name { $($v),* }
        impl $name {
            pub fn as_str(self) -> &'static str { match self { $($name::$v => $s),* } }
        }
    };
}

words!(
    /// A punctuator or operator. `as_str` gives its source text.
    Punct {
        LBrace = "{", RBrace = "}", LParen = "(", RParen = ")", LBracket = "[", RBracket = "]",
        Dot = ".", Ellipsis = "...", Semi = ";", Comma = ",", Lt = "<", Gt = ">", LtEq = "<=", GtEq = ">=",
        EqEq = "==", NotEq = "!=", EqEqEq = "===", NotEqEq = "!==", Plus = "+", Minus = "-", Star = "*",
        Slash = "/", Percent = "%", StarStar = "**", PlusPlus = "++", MinusMinus = "--", Shl = "<<",
        Shr = ">>", UShr = ">>>", Amp = "&", Pipe = "|", Caret = "^", Bang = "!", Tilde = "~",
        AmpAmp = "&&", PipePipe = "||", QQ = "??", Question = "?", QDot = "?.", Colon = ":", Assign = "=",
        PlusAssign = "+=", MinusAssign = "-=", StarAssign = "*=", SlashAssign = "/=", PercentAssign = "%=",
        StarStarAssign = "**=", ShlAssign = "<<=", ShrAssign = ">>=", UShrAssign = ">>>=", AmpAssign = "&=",
        PipeAssign = "|=", CaretAssign = "^=", AmpAmpAssign = "&&=", PipePipeAssign = "||=", QQAssign = "??=",
        Arrow = "=>",
    }
);

words!(
    /// An always-reserved word (written without escapes). `let`, `static`, `yield`, `await`, `async`, `of`
    /// and the like are context words: they come as `Kind::Ident`.
    Keyword {
        Break = "break", Case = "case", Catch = "catch", Class = "class", Const = "const", Continue = "continue",
        Debugger = "debugger", Default = "default", Delete = "delete", Do = "do", Else = "else", Enum = "enum",
        Export = "export", Extends = "extends", False = "false", Finally = "finally", For = "for",
        Function = "function", If = "if", Import = "import", In = "in", Instanceof = "instanceof", New = "new",
        Null = "null", Return = "return", Super = "super", Switch = "switch", This = "this", Throw = "throw",
        True = "true", Try = "try", Typeof = "typeof", Var = "var", Void = "void", While = "while", With = "with",
    }
);

impl Keyword {
    fn from_bytes(s: &[u8]) -> Option<Keyword> {
        use Keyword::*;
        // Reject by length first: most identifiers are not keywords.
        if !(2..=10).contains(&s.len()) {
            return None;
        }
        Some(match s {
            b"break" => Break, b"case" => Case, b"catch" => Catch, b"class" => Class, b"const" => Const,
            b"continue" => Continue, b"debugger" => Debugger, b"default" => Default, b"delete" => Delete,
            b"do" => Do, b"else" => Else, b"enum" => Enum, b"export" => Export, b"extends" => Extends,
            b"false" => False, b"finally" => Finally, b"for" => For, b"function" => Function, b"if" => If,
            b"import" => Import, b"in" => In, b"instanceof" => Instanceof, b"new" => New, b"null" => Null,
            b"return" => Return, b"super" => Super, b"switch" => Switch, b"this" => This, b"throw" => Throw,
            b"true" => True, b"try" => Try, b"typeof" => Typeof, b"var" => Var, b"void" => Void,
            b"while" => While, b"with" => With,
            _ => return None,
        })
    }
}

/// How a number is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumberBase {
    /// `12`, `1.5`, `.5`, `1e3`, `1_000`.
    Decimal,
    Hex,
    Octal,
    Binary,
    /// `017`: Annex B octal. No separators. The parser rejects it in strict mode.
    LegacyOctal,
    /// `089`, `08.5`: Annex B decimal with a leading zero. No separators.
    NonOctalDecimal,
}

/// Which part of a template string a token is. The span includes the delimiters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplatePart {
    /// `` `text` ``
    NoSubstitution,
    /// `` `text${ ``
    Head,
    /// `}text${`
    Middle,
    /// `` }text` ``
    Tail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// End of source. `start == end == source length`.
    Eof,
    /// A name that is not always-reserved, or any name written with `\u` escapes.
    Ident,
    /// `#name`. The span includes `#`.
    PrivateName,
    Keyword(Keyword),
    Punct(Punct),
    Number(NumberBase),
    /// `10n`. The span includes `n`. No value is computed.
    BigInt(NumberBase),
    /// `"..."` or `'...'`, quotes included. `legacy_octal`: has `\1`..`\7`, `\0` before a digit, `\8` or `\9`.
    String { legacy_octal: bool },
    /// `bad_escape`: the cooked value does not exist (legal in a tagged template, an error in an untagged one).
    /// [`decode_template`] with `raw = false` reports where.
    Template { part: TemplatePart, bad_escape: bool },
    /// `/body/flags`. The pattern is not checked; the flags are.
    RegExp,
}

/// One token. `start..end` are byte offsets into the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: Kind,
    pub start: u32,
    pub end: u32,
    /// A line terminator (LF, CR, U+2028, U+2029) lies between the previous token and this one,
    /// inside whitespace or comments. The parser needs this for automatic semicolon insertion.
    pub newline_before: bool,
    /// The token has a `\` escape: the source text is not the value. A directive `"use strict"` must have none.
    pub escapes: bool,
}

impl Token {
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        src.get(self.start as usize..self.end as usize).unwrap_or("")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    SourceTooLarge,
    /// A character that starts no token (`@`, a lone `#`, a control character).
    UnexpectedChar,
    /// A non-ASCII character outside strings, comments and regexps. Not supported: see the module comment.
    NonAscii,
    UnterminatedString,
    UnterminatedComment,
    UnterminatedTemplate,
    UnterminatedRegExp,
    InvalidEscape,
    /// A malformed number: `0x`, `1e`, `1_`, `1__0`, `0_1`, `3in`.
    InvalidNumber,
    InvalidRegExpFlag,
    DuplicateRegExpFlag,
    InvalidPrivateName,
    /// `Goal::TemplateTail` was used and the next token is not `}`.
    ExpectedTemplateContinuation,
    /// A token given to a decode function or `rescan` is not a token of this source.
    BadToken,
}

/// An error with a byte offset. [`Error::location`] gives line and UTF-16 column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub offset: u32,
}

/// Line (from 1) and column in UTF-16 code units (from 0). Acorn uses the same numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Location {
    pub line: u32,
    pub column: u32,
}

impl Error {
    pub fn location(&self, src: &str) -> Location {
        location(src, self.offset as usize)
    }
}

/// Line and UTF-16 column of a byte offset (clamped to the source and to a character boundary).
pub fn location(src: &str, byte: usize) -> Location {
    location_in(src, &[], byte)
}

/// The scan state at a character boundary: what `location` knows after reading `src[..byte]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    byte: u32,
    line: u32,
    column: u32,
    prev_cr: bool,
}

const MARK_START: Mark = Mark { byte: 0, line: 1, column: 0, prev_cr: false };

/// Bytes between two marks: a location reads at most this many bytes once the marks exist.
pub const MARK_STEP: usize = 4096;

/// Read `src[m.byte..end]` (`end` on a character boundary) byte by byte: an ASCII byte is one UTF-16 unit, a
/// lead byte of a 4-byte character two, a continuation byte none. LS and PS (E2 80 A8/A9) end a line.
fn scan(src: &str, mut m: Mark, end: usize) -> Mark {
    let b = src.as_bytes();
    let mut i = m.byte as usize;
    while i < end {
        let c = b[i];
        // Most bytes: one test, then a count without branches.
        if c > b'\r' && c != 0xE2 {
            m.column += u32::from(c & 0xC0 != 0x80) + u32::from(c >= 0xF0);
            m.prev_cr = false;
            i += 1;
            continue;
        }
        match c {
            b'\n' => {
                if !m.prev_cr {
                    m.line += 1;
                    m.column = 0;
                }
                m.prev_cr = false;
            }
            b'\r' => {
                m.line += 1;
                m.column = 0;
                m.prev_cr = true;
            }
            0xE2 if b[i + 1] == 0x80 && matches!(b[i + 2], 0xA8 | 0xA9) => {
                m.line += 1;
                m.column = 0;
                m.prev_cr = false;
                i += 3;
                continue;
            }
            // a control byte below `\r`, or the lead byte of another 3-byte character: one unit
            _ => {
                m.prev_cr = false;
                m.column += 1;
            }
        }
        i += 1;
    }
    Mark { byte: end as u32, ..m }
}

/// One mark at the first character boundary at or after each multiple of `MARK_STEP`: 16 B per 4 KiB of source.
pub fn location_marks(src: &str) -> Box<[Mark]> {
    let mut out = Vec::with_capacity(src.len() / MARK_STEP + 1);
    let mut m = MARK_START;
    let mut t = 0;
    while t < src.len() {
        let mut e = t;
        while !src.is_char_boundary(e) {
            e += 1;
        }
        m = scan(src, m, e);
        out.push(m);
        t += MARK_STEP;
    }
    out.into_boxed_slice()
}

/// `location` that starts at the last of `marks` (from `location_marks`) at or before the offset.
pub fn location_in(src: &str, marks: &[Mark], byte: usize) -> Location {
    let mut end = byte.min(src.len());
    while !src.is_char_boundary(end) {
        end -= 1;
    }
    let from = match marks.partition_point(|m| m.byte as usize <= end) {
        0 => MARK_START,
        i => marks[i - 1],
    };
    let m = scan(src, from, end);
    Location { line: m.line, column: m.column }
}

/// The offset in UTF-16 code units of a byte offset. This is the unit that Acorn and JavaScript use.
pub fn utf16_offset(src: &str, byte: usize) -> usize {
    let mut end = byte.min(src.len());
    while !src.is_char_boundary(end) {
        end -= 1;
    }
    src[..end].chars().map(char::len_utf16).sum()
}

/// Latin-1 is fixed: the letters, and the middle dot (ID_Continue only).
#[inline]
fn latin1_id(cp: u32, first: bool) -> bool {
    matches!(cp, 0xAA | 0xB5 | 0xBA | 0xC0..=0xD6 | 0xD8..=0xF6 | 0xF8..=0xFF) || (cp == 0xB7 && !first)
}

/// Is `cp` (not ASCII) allowed in a name? ID_Start for the first character, ID_Continue after it (plus ZWNJ and ZWJ).
fn id_char(cp: u32, first: bool) -> bool {
    use icu_properties::{props::{IdContinue, IdStart}, CodePointSetData};
    if cp < 0x100 {
        return latin1_id(cp, first);
    }
    if first {
        CodePointSetData::new::<IdStart>().contains32(cp)
    } else {
        cp == 0x200C || cp == 0x200D || CodePointSetData::new::<IdContinue>().contains32(cp)
    }
}

const IDSTART: u8 = 1;
const IDPART: u8 = 2;

static CLASS: [u8; 128] = {
    let mut t = [0u8; 128];
    let mut i = 0;
    while i < 128 {
        let c = i as u8;
        if c.is_ascii_alphabetic() || c == b'$' || c == b'_' {
            t[i] = IDSTART | IDPART;
        } else if c.is_ascii_digit() {
            t[i] = IDPART;
        }
        i += 1;
    }
    t
};

fn cls(c: u8) -> u8 {
    if c < 128 { CLASS[c as usize] } else { 0 }
}

/// The lexer. One instance scans one source text from the start to the end.
pub struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    /// After an error the lexer returns that error again. It never goes on past a bad token.
    failed: Option<Error>,
    /// Annex B HTML-like comments are comments (script goal).
    html: bool,
}

impl<'a> Lexer<'a> {
    /// Fails with `SourceTooLarge` when `src` is longer than [`MAX_SOURCE_LEN`] bytes.
    pub fn new(src: &'a str) -> Result<Lexer<'a>, Error> {
        if src.len() > MAX_SOURCE_LEN {
            return Err(Error { kind: ErrorKind::SourceTooLarge, offset: MAX_SOURCE_LEN as u32 });
        }
        Ok(Lexer { src, pos: 0, failed: None, html: false })
    }

    /// A lexer over `src` that starts at byte `pos` (a token start): the second parse of a lazy function.
    pub fn starting_at(src: &'a str, pos: u32) -> Result<Lexer<'a>, Error> {
        let mut l = Lexer::new(src)?;
        l.pos = (pos as usize).min(src.len());
        Ok(l)
    }

    /// Go on scanning at byte `pos`, a token start that the parser found by itself (the end of a data literal).
    pub fn seek(&mut self, pos: u32) {
        self.pos = (pos as usize).min(self.src.len());
    }

    /// Annex B (script goal only): `<!--` starts a comment to the end of the line, and so does `-->` at the start of a line
    /// (after only spaces and comments) or of the source. Off by default.
    pub fn set_html_comments(&mut self, on: bool) {
        self.html = on;
    }

    pub fn source(&self) -> &'a str {
        self.src
    }

    /// The byte offset where the next token search starts.
    pub fn pos(&self) -> u32 {
        self.pos as u32
    }

    /// The next token, with trivia (spaces, comments, line terminators) before it skipped.
    pub fn next(&mut self, goal: Goal) -> Result<Token, Error> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        let r = self.scan(goal);
        if let Err(e) = r {
            self.failed = Some(e);
        }
        r
    }

    /// Scan the token at `tok.start` again with another goal. The new token has the same
    /// `newline_before`. Use it when `tok` was read as `/`, `/=` or `}` and the parser now knows better.
    /// The lexer continues after the new token. It also clears an earlier error.
    pub fn rescan(&mut self, tok: &Token, goal: Goal) -> Result<Token, Error> {
        let s = tok.start as usize;
        if s >= self.src.len() || !self.src.is_char_boundary(s) {
            return Err(Error { kind: ErrorKind::BadToken, offset: tok.start });
        }
        self.pos = s;
        self.failed = None;
        let r = self.scan_at(goal, tok.newline_before);
        if let Err(e) = r {
            self.failed = Some(e);
        }
        r
    }

    fn scan(&mut self, goal: Goal) -> Result<Token, Error> {
        let nl = self.skip_trivia()?;
        self.scan_at(goal, nl)
    }

    fn err(&self, kind: ErrorKind, offset: usize) -> Error {
        Error { kind, offset: offset as u32 }
    }

    fn at(&self, i: usize) -> u8 {
        self.src.as_bytes().get(i).copied().unwrap_or(0)
    }

    /// Skip spaces, comments and line terminators. Returns true if a line terminator was in them.
    fn skip_trivia(&mut self) -> Result<bool, Error> {
        let b = self.src.as_bytes();
        let mut nl = false;
        let first = self.pos == 0;
        if self.pos == 0 && b.starts_with(b"#!") {
            self.pos = line_end(b, 2);
        }
        while let Some(&c) = b.get(self.pos) {
            match c {
                b' ' | b'\t' | 0x0B | 0x0C => self.pos += 1,
                b'\n' | b'\r' => {
                    nl = true;
                    self.pos += 1;
                }
                b'/' => match b.get(self.pos + 1) {
                    Some(b'/') => self.pos = line_end(b, self.pos + 2),
                    Some(b'*') => {
                        let start = self.pos;
                        // Jump from `*` to `*`; then look for line terminators in the text between them.
                        let mut i = start + 2;
                        loop {
                            let Some(n) = b[i..].iter().position(|&c| c == b'*') else {
                                return Err(self.err(ErrorKind::UnterminatedComment, start));
                            };
                            nl = nl || line_end(&b[i..i + n], 0) < n;
                            i += n + 1;
                            if b.get(i) == Some(&b'/') {
                                break;
                            }
                        }
                        self.pos = i + 1;
                    }
                    _ => break,
                },
                b'<' if self.html && b[self.pos..].starts_with(b"<!--") => self.pos = line_end(b, self.pos + 4),
                b'-' if self.html && (nl || first) && b[self.pos..].starts_with(b"-->") => self.pos = line_end(b, self.pos + 3),
                0xC2 | 0xE1 | 0xE2 | 0xE3 | 0xEF => match unicode_space(b, self.pos) {
                    Some((n, is_lt)) => {
                        nl |= is_lt;
                        self.pos += n;
                    }
                    None => break,
                },
                _ => break,
            }
        }
        Ok(nl)
    }

    fn scan_at(&mut self, goal: Goal, nl: bool) -> Result<Token, Error> {
        let start = self.pos;
        let b = self.src.as_bytes();
        let Some(&c) = b.get(start) else {
            return Ok(Token { kind: Kind::Eof, start: start as u32, end: start as u32, newline_before: nl, escapes: false });
        };
        if goal == Goal::TemplateTail {
            if c != b'}' {
                return Err(self.err(ErrorKind::ExpectedTemplateContinuation, start));
            }
            return self.template(start, nl);
        }
        let mut escapes = false;
        let kind = match c {
            b'"' | b'\'' => {
                let (legacy_octal, esc) = self.string(c)?;
                escapes = esc;
                Kind::String { legacy_octal }
            }
            b'`' => return self.template(start, nl),
            b'0'..=b'9' => self.number(start)?,
            b'.' if self.at(start + 1).is_ascii_digit() => self.number(start)?,
            b'/' if goal == Goal::RegExp => {
                self.regexp(start)?;
                Kind::RegExp
            }
            b'#' => {
                let n = self.at(start + 1);
                if n >= 0x80 {
                    if self.unicode_id(start + 1, true).is_none() {
                        return Err(self.err(ErrorKind::NonAscii, start + 1));
                    }
                } else if cls(n) & IDSTART == 0 && n != b'\\' {
                    return Err(self.err(ErrorKind::InvalidPrivateName, start));
                }
                self.pos += 1;
                escapes = self.ident()?;
                Kind::PrivateName
            }
            _ if cls(c) & IDSTART != 0 || c == b'\\' => {
                escapes = self.ident()?;
                match if escapes { None } else { Keyword::from_bytes(&self.src.as_bytes()[start..self.pos]) } {
                    Some(k) => Kind::Keyword(k),
                    None => Kind::Ident,
                }
            }
            _ => match self.punct(c) {
                Some((p, n)) => {
                    self.pos += n;
                    Kind::Punct(p)
                }
                None if c >= 0x80 => {
                    if self.unicode_id(start, true).is_none() {
                        return Err(self.err(ErrorKind::NonAscii, start));
                    }
                    escapes = self.ident()?;
                    Kind::Ident
                }
                None => return Err(self.err(ErrorKind::UnexpectedChar, start)),
            },
        };
        Ok(Token { kind, start: start as u32, end: self.pos as u32, newline_before: nl, escapes })
    }

    fn punct(&self, c: u8) -> Option<(Punct, usize)> {
        use Punct::*;
        let p = self.pos;
        let (b1, b2, b3) = (self.at(p + 1), self.at(p + 2), self.at(p + 3));
        Some(match c {
            b'{' => (LBrace, 1),
            b'}' => (RBrace, 1),
            b'(' => (LParen, 1),
            b')' => (RParen, 1),
            b'[' => (LBracket, 1),
            b']' => (RBracket, 1),
            b';' => (Semi, 1),
            b',' => (Comma, 1),
            b':' => (Colon, 1),
            b'~' => (Tilde, 1),
            b'.' if b1 == b'.' && b2 == b'.' => (Ellipsis, 3),
            b'.' => (Dot, 1),
            b'<' => match (b1, b2) {
                (b'<', b'=') => (ShlAssign, 3),
                (b'<', _) => (Shl, 2),
                (b'=', _) => (LtEq, 2),
                _ => (Lt, 1),
            },
            b'>' => match (b1, b2, b3) {
                (b'>', b'>', b'=') => (UShrAssign, 4),
                (b'>', b'>', _) => (UShr, 3),
                (b'>', b'=', _) => (ShrAssign, 3),
                (b'>', _, _) => (Shr, 2),
                (b'=', _, _) => (GtEq, 2),
                _ => (Gt, 1),
            },
            b'=' => match (b1, b2) {
                (b'=', b'=') => (EqEqEq, 3),
                (b'=', _) => (EqEq, 2),
                (b'>', _) => (Arrow, 2),
                _ => (Assign, 1),
            },
            b'!' => match (b1, b2) {
                (b'=', b'=') => (NotEqEq, 3),
                (b'=', _) => (NotEq, 2),
                _ => (Bang, 1),
            },
            b'+' => match b1 {
                b'+' => (PlusPlus, 2),
                b'=' => (PlusAssign, 2),
                _ => (Plus, 1),
            },
            b'-' => match b1 {
                b'-' => (MinusMinus, 2),
                b'=' => (MinusAssign, 2),
                _ => (Minus, 1),
            },
            b'*' => match (b1, b2) {
                (b'*', b'=') => (StarStarAssign, 3),
                (b'*', _) => (StarStar, 2),
                (b'=', _) => (StarAssign, 2),
                _ => (Star, 1),
            },
            b'/' if b1 == b'=' => (SlashAssign, 2),
            b'/' => (Slash, 1),
            b'%' if b1 == b'=' => (PercentAssign, 2),
            b'%' => (Percent, 1),
            b'^' if b1 == b'=' => (CaretAssign, 2),
            b'^' => (Caret, 1),
            b'&' => match (b1, b2) {
                (b'&', b'=') => (AmpAmpAssign, 3),
                (b'&', _) => (AmpAmp, 2),
                (b'=', _) => (AmpAssign, 2),
                _ => (Amp, 1),
            },
            b'|' => match (b1, b2) {
                (b'|', b'=') => (PipePipeAssign, 3),
                (b'|', _) => (PipePipe, 2),
                (b'=', _) => (PipeAssign, 2),
                _ => (Pipe, 1),
            },
            b'?' => match (b1, b2) {
                (b'?', b'=') => (QQAssign, 3),
                (b'?', _) => (QQ, 2),
                // `a?.5:1` is `a ? .5 : 1`, so `?.` before a digit is just `?`.
                (b'.', d) if !d.is_ascii_digit() => (QDot, 2),
                _ => (Question, 1),
            },
            _ => return None,
        })
    }

    /// The length in bytes of the character at `at` (not ASCII) if it belongs in a name.
    #[cold]
    #[inline(never)]
    fn unicode_id(&self, at: usize, first: bool) -> Option<usize> {
        let b = self.src.as_bytes();
        if b[at] < 0xC4 {
            // A two-byte character below U+0100 (the lead byte is 0xC2 or 0xC3): no decoding needed.
            return latin1_id(u32::from(b[at] & 3) << 6 | u32::from(b[at + 1] & 0x3F), first).then_some(2);
        }
        let ch = self.src[at..].chars().next()?;
        id_char(ch as u32, first).then(|| ch.len_utf8())
    }

    /// An identifier name from `self.pos`. Returns true if it has `\u` escapes.
    fn ident(&mut self) -> Result<bool, Error> {
        let b = self.src.as_bytes();
        let (mut esc, mut first) = (false, true);
        while let Some(&c) = b.get(self.pos) {
            if cls(c) & IDPART != 0 {
                self.pos += 1;
            } else if c == b'\\' {
                let at = self.pos;
                if b.get(at + 1) != Some(&b'u') {
                    return Err(self.err(ErrorKind::InvalidEscape, at));
                }
                let mut i = at + 2;
                let Some(cp) = unicode_escape(b, &mut i) else {
                    return Err(self.err(ErrorKind::InvalidEscape, at));
                };
                let ok = if cp < 0x80 { cls(cp as u8) & if first { IDSTART } else { IDPART } != 0 } else { id_char(cp, first) };
                if !ok {
                    let kind = if cp >= 0x80 { ErrorKind::NonAscii } else { ErrorKind::InvalidEscape };
                    return Err(self.err(kind, at));
                }
                self.pos = i;
                esc = true;
            } else if c >= 0x80 {
                // A letter continues the name. Anything else ends it; the next token reports it if it is bad.
                match self.unicode_id(self.pos, first) {
                    Some(n) => self.pos += n,
                    None => break,
                }
            } else {
                break;
            }
            first = false;
        }
        Ok(esc)
    }

    /// Digits of `radix` with `_` separators (only if `sep`). Returns the digit count.
    fn digits(&mut self, radix: u32, sep: bool) -> Result<usize, Error> {
        let b = self.src.as_bytes();
        let is_digit = |c: u8| (c as char).is_digit(radix);
        let mut n = 0;
        loop {
            match b.get(self.pos) {
                Some(&c) if is_digit(c) => {
                    self.pos += 1;
                    n += 1;
                }
                Some(b'_') => {
                    // A separator must sit between two digits.
                    if !(sep && n > 0 && b.get(self.pos + 1).is_some_and(|&d| is_digit(d))) {
                        return Err(self.err(ErrorKind::InvalidNumber, self.pos));
                    }
                    self.pos += 1;
                }
                _ => return Ok(n),
            }
        }
    }

    /// An optional exponent `e+12`. Returns true if there was one.
    fn exponent(&mut self) -> Result<bool, Error> {
        if !matches!(self.at(self.pos), b'e' | b'E') {
            return Ok(false);
        }
        let mut p = self.pos + 1;
        if matches!(self.at(p), b'+' | b'-') {
            p += 1;
        }
        if !self.at(p).is_ascii_digit() {
            return Err(self.err(ErrorKind::InvalidNumber, self.pos));
        }
        self.pos = p;
        self.digits(10, true)?;
        Ok(true)
    }

    fn number(&mut self, start: usize) -> Result<Kind, Error> {
        let c = self.at(start);
        let (mut base, mut big_ok) = (NumberBase::Decimal, true);
        if c == b'.' {
            self.pos += 1;
            self.digits(10, true)?;
            self.exponent()?;
            big_ok = false;
        } else if c == b'0' && matches!(self.at(start + 1), b'x' | b'X' | b'o' | b'O' | b'b' | b'B') {
            let (b, radix) = match self.at(start + 1) | 0x20 {
                b'x' => (NumberBase::Hex, 16),
                b'o' => (NumberBase::Octal, 8),
                _ => (NumberBase::Binary, 2),
            };
            base = b;
            self.pos += 2;
            if self.digits(radix, true)? == 0 {
                return Err(self.err(ErrorKind::InvalidNumber, start));
            }
        } else if c == b'0' && self.at(start + 1).is_ascii_digit() {
            self.pos += 1;
            self.digits(10, false)?;
            big_ok = false;
            if self.src.as_bytes()[start..self.pos].iter().all(|&d| d < b'8') {
                base = NumberBase::LegacyOctal;
            } else {
                base = NumberBase::NonOctalDecimal;
                if self.at(self.pos) == b'.' {
                    self.pos += 1;
                    self.digits(10, false)?;
                }
                self.exponent()?;
            }
        } else {
            if c == b'0' {
                self.pos += 1; // `0_1` is not allowed: the `_` is caught below.
            } else {
                self.digits(10, true)?;
            }
            if self.at(self.pos) == b'.' {
                self.pos += 1;
                self.digits(10, true)?;
                big_ok = false;
            }
            if self.exponent()? {
                big_ok = false;
            }
        }
        let big = big_ok && self.at(self.pos) == b'n';
        if big {
            self.pos += 1;
        }
        // `3in`, `0x1g`, `1_` and `5a`: a number may not touch a name or a digit.
        let n = self.at(self.pos);
        if cls(n) != 0 || n == b'\\' {
            return Err(self.err(ErrorKind::InvalidNumber, self.pos));
        }
        Ok(if big { Kind::BigInt(base) } else { Kind::Number(base) })
    }

    /// A quoted string. Returns (has legacy octal escape, has escape).
    fn string(&mut self, quote: u8) -> Result<(bool, bool), Error> {
        let b = self.src.as_bytes();
        let start = self.pos;
        let (mut octal, mut esc) = (false, false);
        let mut i = start + 1;
        loop {
            // Skip a run of plain bytes. The stop bytes are all ASCII or 0xE2 (for U+2028/9, which are legal here).
            match b[i..].iter().position(|&c| c == quote || c == b'\\' || c == b'\n' || c == b'\r') {
                None => return Err(self.err(ErrorKind::UnterminatedString, start)),
                Some(n) => i += n,
            }
            match b[i] {
                b'\\' => {
                    esc = true;
                    i += 1;
                    match escape(b, &mut i, false, None) {
                        Esc::Ok => {}
                        Esc::Legacy => octal = true,
                        Esc::Bad(at) if at < b.len() && i < b.len() => return Err(self.err(ErrorKind::InvalidEscape, at)),
                        Esc::Bad(_) => return Err(self.err(ErrorKind::UnterminatedString, start)),
                    }
                }
                c if c == quote => {
                    self.pos = i + 1;
                    return Ok((octal, esc));
                }
                _ => return Err(self.err(ErrorKind::UnterminatedString, start)),
            }
        }
    }

    /// From a backtick or a `}` to the next `${` or closing backtick.
    fn template(&mut self, start: usize, nl: bool) -> Result<Token, Error> {
        let b = self.src.as_bytes();
        let head = b[start] == b'`';
        let (mut bad, mut esc) = (false, false);
        let mut i = start + 1;
        let part = loop {
            match b[i..].iter().position(|&c| c == b'`' || c == b'$' || c == b'\\') {
                None => return Err(self.err(ErrorKind::UnterminatedTemplate, start)),
                Some(n) => i += n,
            }
            match b[i] {
                b'`' => {
                    i += 1;
                    break if head { TemplatePart::NoSubstitution } else { TemplatePart::Tail };
                }
                b'$' if b.get(i + 1) == Some(&b'{') => {
                    i += 2;
                    break if head { TemplatePart::Head } else { TemplatePart::Middle };
                }
                b'\\' => {
                    esc = true;
                    i += 1;
                    if let Esc::Bad(_) = escape(b, &mut i, true, None) {
                        bad = true;
                    }
                }
                _ => i += 1,
            }
        };
        self.pos = i;
        Ok(Token { kind: Kind::Template { part, bad_escape: bad }, start: start as u32, end: i as u32, newline_before: nl, escapes: esc })
    }

    /// `/body/flags`. Finds the end and checks the flags. It does not check the pattern.
    fn regexp(&mut self, start: usize) -> Result<(), Error> {
        let b = self.src.as_bytes();
        let unterminated = |s: &Self| s.err(ErrorKind::UnterminatedRegExp, start);
        let (mut i, mut class) = (start + 1, false);
        loop {
            match b.get(i) {
                None | Some(b'\n' | b'\r') => return Err(unterminated(self)),
                Some(0xE2) if is_ls_ps(b, i) => return Err(unterminated(self)),
                Some(b'\\') => match b.get(i + 1) {
                    None | Some(b'\n' | b'\r') => return Err(unterminated(self)),
                    Some(0xE2) if is_ls_ps(b, i + 1) => return Err(unterminated(self)),
                    Some(_) => i += 2,
                },
                Some(b'[') => {
                    class = true;
                    i += 1;
                }
                Some(b']') => {
                    class = false;
                    i += 1;
                }
                Some(b'/') if !class => {
                    i += 1;
                    break;
                }
                Some(_) => i += 1,
            }
        }
        let mut seen = 0u8;
        while let Some(&c) = b.get(i) {
            if c == b'\\' {
                return Err(self.err(ErrorKind::InvalidRegExpFlag, i));
            }
            if cls(c) & IDPART == 0 {
                break; // Non-ASCII here is a space or the start of an unsupported name; the next token decides.
            }
            let Some(bit) = b"dgimsuvy".iter().position(|&f| f == c) else {
                return Err(self.err(ErrorKind::InvalidRegExpFlag, i));
            };
            if seen & (1 << bit) != 0 {
                return Err(self.err(ErrorKind::DuplicateRegExpFlag, i));
            }
            seen |= 1 << bit;
            i += 1;
        }
        self.pos = i;
        Ok(())
    }
}

fn is_ls_ps(b: &[u8], i: usize) -> bool {
    b.get(i + 1) == Some(&0x80) && matches!(b.get(i + 2), Some(0xA8 | 0xA9))
}

/// The end of a `//` comment: the offset of the first line terminator at or after `from`, or the end.
fn line_end(b: &[u8], from: usize) -> usize {
    let mut i = from;
    // 0xE2 starts many characters, so it can be a false stop: check and go on.
    while let Some(n) = b[i..].iter().position(|&c| c == b'\n' || c == b'\r' || c == 0xE2) {
        i += n;
        if b[i] != 0xE2 || is_ls_ps(b, i) {
            return i;
        }
        i += 1;
    }
    b.len()
}

/// A non-ASCII white space or line terminator at `i`: (byte length, is a line terminator).
/// The list is the Unicode `Zs` category plus U+FEFF, U+2028 and U+2029, as ECMA-262 says.
fn unicode_space(b: &[u8], i: usize) -> Option<(usize, bool)> {
    match (b[i], b.get(i + 1).copied(), b.get(i + 2).copied()) {
        (0xC2, Some(0xA0), _) => Some((2, false)),
        (0xEF, Some(0xBB), Some(0xBF)) => Some((3, false)),
        (0xE1, Some(0x9A), Some(0x80)) => Some((3, false)),
        (0xE2, Some(0x80), Some(0x80..=0x8A | 0xAF)) => Some((3, false)),
        (0xE2, Some(0x80), Some(0xA8 | 0xA9)) => Some((3, true)),
        (0xE2, Some(0x81), Some(0x9F)) => Some((3, false)),
        (0xE3, Some(0x80), Some(0x80)) => Some((3, false)),
        _ => None,
    }
}

/// After `\u`: `XXXX` or `{X...}`. Returns the code point and moves `i`.
fn unicode_escape(b: &[u8], i: &mut usize) -> Option<u32> {
    let hex = |c: u8| (c as char).to_digit(16);
    if b.get(*i) == Some(&b'{') {
        let (mut j, mut v) = (*i + 1, 0u32);
        let first = j;
        while let Some(d) = b.get(j).copied().and_then(hex) {
            v = v * 16 + d;
            if v > 0x10FFFF {
                return None;
            }
            j += 1;
        }
        if j == first || b.get(j) != Some(&b'}') {
            return None;
        }
        *i = j + 1;
        Some(v)
    } else {
        let mut v = 0;
        for k in 0..4 {
            v = v * 16 + b.get(*i + k).copied().and_then(hex)?;
        }
        *i += 4;
        Some(v)
    }
}

enum Esc {
    Ok,
    /// Legacy octal or `\8` `\9`. Legal in a string, not in a template.
    Legacy,
    /// Not a valid escape. The offset is where the backslash is.
    Bad(usize),
}

fn push_cp(out: &mut Vec<u16>, cp: u32) {
    if cp < 0x10000 {
        out.push(cp as u16);
    } else {
        let c = cp - 0x10000;
        out.push(0xD800 + (c >> 10) as u16);
        out.push(0xDC00 + (c & 0x3FF) as u16);
    }
}

/// Read one escape. `i` is the offset just after the backslash. On return `i` is after the escape,
/// or (when it is bad) after the first character of it. With `out`, the value is appended.
/// `tpl`: template rules (no legacy octal, no `\8`).
fn escape(b: &[u8], i: &mut usize, tpl: bool, mut out: Option<&mut Vec<u16>>) -> Esc {
    let bs = *i - 1;
    let Some(&c) = b.get(*i) else { return Esc::Bad(bs) };
    *i += 1;
    let mut push = |u: u32| {
        if let Some(o) = out.as_deref_mut() {
            push_cp(o, u)
        }
    };
    match c {
        b'\n' => {}
        b'\r' => {
            if b.get(*i) == Some(&b'\n') {
                *i += 1;
            }
        }
        0xE2 if is_ls_ps(b, *i - 1) => *i += 2,
        b'n' => push(10),
        b't' => push(9),
        b'b' => push(8),
        b'f' => push(12),
        b'v' => push(11),
        b'r' => push(13),
        b'x' => {
            let hex = |k: usize| b.get(*i + k).and_then(|&c| (c as char).to_digit(16));
            match (hex(0), hex(1)) {
                (Some(h), Some(l)) => {
                    push(h * 16 + l);
                    *i += 2;
                }
                _ => return Esc::Bad(bs),
            }
        }
        b'u' => match unicode_escape(b, i) {
            Some(cp) => push(cp),
            None => return Esc::Bad(bs),
        },
        b'0'..=b'9' => {
            let next_digit = b.get(*i).is_some_and(u8::is_ascii_digit);
            if c == b'0' && !next_digit {
                push(0);
            } else if tpl {
                return Esc::Bad(bs);
            } else if c >= b'8' {
                push(c as u32);
                return Esc::Legacy;
            } else {
                let max = if c <= b'3' { 3 } else { 2 };
                let mut v = (c - b'0') as u32;
                let mut n = 1;
                while n < max && b.get(*i).is_some_and(|d| (b'0'..=b'7').contains(d)) {
                    v = v * 8 + (b[*i] - b'0') as u32;
                    *i += 1;
                    n += 1;
                }
                push(v);
                return Esc::Legacy;
            }
        }
        _ => {
            // Any other character stands for itself (`\'`, `\\`, `\é`). Copy the whole UTF-8 character.
            let n = match c {
                0..=0x7F => 1,
                0xC0..=0xDF => 2,
                0xE0..=0xEF => 3,
                _ => 4,
            };
            match b.get(*i - 1..*i - 1 + n).and_then(|s| std::str::from_utf8(s).ok()).and_then(|s| s.chars().next()) {
                Some(ch) => {
                    push(ch as u32);
                    *i += n - 1;
                }
                None => return Esc::Bad(bs),
            }
        }
    }
    Esc::Ok
}

enum Mode {
    Str,
    Cooked,
    Raw,
}

fn decode_body(src: &str, s: usize, e: usize, mode: Mode, out: &mut Vec<u16>) -> Result<(), Error> {
    let bad = |offset: usize| Error { kind: ErrorKind::InvalidEscape, offset: offset as u32 };
    let b = &src.as_bytes()[..e];
    let tpl = !matches!(mode, Mode::Str);
    let raw = matches!(mode, Mode::Raw);
    let mut i = s;
    while i < e {
        let run = i;
        while i < e && !((b[i] == b'\\' && !raw) || (b[i] == b'\r' && tpl)) {
            i += 1;
        }
        // The stop bytes are ASCII, so this slice is on character boundaries.
        let plain = &src[run..i];
        if plain.is_ascii() {
            out.extend(plain.bytes().map(u16::from)); // a simple widening loop: much faster than `encode_utf16`
        } else {
            out.extend(plain.encode_utf16());
        }
        if i >= e {
            break;
        }
        if b[i] == b'\r' {
            // Templates read CR and CRLF as LF.
            out.push(10);
            i += 1;
            if b.get(i) == Some(&b'\n') {
                i += 1;
            }
            continue;
        }
        i += 1;
        if let Esc::Bad(at) = escape(b, &mut i, tpl, Some(out)) {
            return Err(bad(at));
        }
    }
    Ok(())
}

fn bad_token(tok: &Token) -> Error {
    Error { kind: ErrorKind::BadToken, offset: tok.start }
}

/// Append the value of a string token to `out` as UTF-16 code units. Appends nothing for a plain
/// empty string. A lone surrogate escape stays a lone surrogate. On error `out` may hold a partial value.
pub fn decode_string(src: &str, tok: &Token, out: &mut Vec<u16>) -> Result<(), Error> {
    let text = src.get(tok.start as usize..tok.end as usize).ok_or_else(|| bad_token(tok))?;
    if !matches!(tok.kind, Kind::String { .. }) || text.len() < 2
        || !matches!(text.as_bytes()[0], b'\'' | b'"') || text.as_bytes().last() != text.as_bytes().first()
    {
        return Err(bad_token(tok));
    }
    decode_body(src, tok.start as usize + 1, tok.end as usize - 1, Mode::Str, out)
}

/// Append the cooked (`raw = false`) or raw (`raw = true`) value of a template token as UTF-16 code units.
/// Both read CR and CRLF as LF. Cooked fails on a bad escape (see `Kind::Template::bad_escape`).
pub fn decode_template(src: &str, tok: &Token, raw: bool, out: &mut Vec<u16>) -> Result<(), Error> {
    let Kind::Template { part, .. } = tok.kind else { return Err(bad_token(tok)) };
    let tail = match part {
        TemplatePart::NoSubstitution | TemplatePart::Tail => 1,
        TemplatePart::Head | TemplatePart::Middle => 2,
    };
    let text = src.get(tok.start as usize..tok.end as usize).ok_or_else(|| bad_token(tok))?;
    let first = match part { TemplatePart::NoSubstitution | TemplatePart::Head => b'`', _ => b'}' };
    let ending = if tail == 1 { "`" } else { "${" };
    if text.len() < 1 + tail || text.as_bytes()[0] != first || !text.ends_with(ending) {
        return Err(bad_token(tok));
    }
    decode_body(src, tok.start as usize + 1, tok.end as usize - tail, if raw { Mode::Raw } else { Mode::Cooked }, out)
}

/// The name of an `Ident`, `Keyword` or `PrivateName` token, without `#`, with `\u` escapes decoded.
/// Borrowed from the source when it has no escape.
pub fn ident_name<'a>(src: &'a str, tok: &Token) -> Result<Cow<'a, str>, Error> {
    let skip = match tok.kind {
        Kind::Ident | Kind::Keyword(_) => 0,
        Kind::PrivateName => 1,
        _ => return Err(bad_token(tok)),
    };
    let text = src.get(tok.start as usize..tok.end as usize).ok_or_else(|| bad_token(tok))?;
    let text = if skip == 1 { text.strip_prefix('#').ok_or_else(|| bad_token(tok))? } else { text };
    if !tok.escapes {
        return Ok(Cow::Borrowed(text));
    }
    let b = text.as_bytes();
    let mut s = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && b.get(i + 1) == Some(&b'u') {
            i += 2;
            match unicode_escape(b, &mut i).and_then(char::from_u32) {
                Some(c) => s.push(c),
                None => return Err(bad_token(tok)),
            }
        } else {
            let ch = text[i..].chars().next().ok_or_else(|| bad_token(tok))?;
            s.push(ch);
            i += ch.len_utf8();
        }
    }
    Ok(Cow::Owned(s))
}

/// The value of a `Number` token, rounded to the nearest `f64` (ties to even), also for hex, octal and
/// binary with more than 53 bits. `BigInt` tokens give `None`.
pub fn number_value(src: &str, tok: &Token) -> Option<f64> {
    let Kind::Number(base) = tok.kind else { return None };
    let text = src.get(tok.start as usize..tok.end as usize)?;
    let radix_bits = match base {
        NumberBase::Hex => 4,
        NumberBase::Octal | NumberBase::LegacyOctal => 3,
        NumberBase::Binary => 1,
        _ => {
            if text.len() > number::DIRECT_UNITS && text.contains('_') {
                return number::separated_decimal_value(text, &mut String::new());
            }
            let digits: Cow<str> = if text.contains('_') { text.replace('_', "").into() } else { text.into() };
            return number::decimal_value(&digits, &mut String::new());
        }
    };
    let digits = match base {
        NumberBase::Hex => text.strip_prefix("0x").or_else(|| text.strip_prefix("0X"))?,
        NumberBase::Octal => text.strip_prefix("0o").or_else(|| text.strip_prefix("0O"))?,
        NumberBase::Binary => text.strip_prefix("0b").or_else(|| text.strip_prefix("0B"))?,
        _ => text,
    };
    if digits.is_empty() { return None; }
    // Keep the top 64 bits. Dropped digits only matter as a "sticky" bit for rounding.
    let (mut m, mut shift, mut sticky) = (0u64, 0i32, false);
    for c in digits.bytes().filter(|&c| c != b'_') {
        let d = (c as char).to_digit(1 << radix_bits)? as u64;
        if m >> (64 - radix_bits) == 0 {
            m = m << radix_bits | d;
        } else {
            shift += radix_bits;
            sticky |= d != 0;
        }
    }
    Some((m | sticky as u64) as f64 * 2f64.powi(shift))
}

/// The pattern and the flags of a `RegExp` token: `/a[/]b/gi` gives (`a[/]b`, `gi`).
pub fn regexp_parts<'a>(src: &'a str, tok: &Token) -> Option<(&'a str, &'a str)> {
    if tok.kind != Kind::RegExp {
        return None;
    }
    let t = src.get(tok.start as usize..tok.end as usize)?.strip_prefix('/')?;
    // Flags have no `/`, so the last `/` ends the pattern.
    let i = t.rfind('/')?;
    Some((&t[..i], &t[i + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use Kind::{BigInt, Eof, Ident, Number, PrivateName, RegExp, Template};

    #[test]
    fn html_like_comments_only_when_asked() {
        // Checked with Node 22: `x = 1 <!-- 2` is `x = 1`; `-->` is a comment only at the start of a line or of the source.
        let texts = |src: &str, on: bool| {
            let mut l = Lexer::new(src).unwrap();
            l.set_html_comments(on);
            let mut v = Vec::new();
            loop {
                let t = l.next(Goal::Div).unwrap();
                if t.kind == Eof {
                    return v.join(" ");
                }
                v.push(t.text(src).to_string());
            }
        };
        assert_eq!(texts("a <!-- b c\nd", true), "a d");
        assert_eq!(texts("    <!-- note -->\nif (x) y", true), "if ( x ) y");
        assert_eq!(texts("a\n  --> b\nc", true), "a c");
        assert_eq!(texts("--> b\nc", true), "c");
        assert_eq!(texts("a /*\n*/ --> b\nc", true), "a c");
        assert_eq!(texts("a --> b", true), "a -- > b");
        assert_eq!(texts("a <!-- b", false), "a < ! -- b");
    }

    /// All tokens with the `Div` goal, up to and without `Eof`.
    fn lex(src: &str) -> Result<Vec<Token>, Error> {
        let mut l = Lexer::new(src)?;
        let mut v = Vec::new();
        loop {
            let t = l.next(Goal::Div)?;
            if t.kind == Eof {
                return Ok(v);
            }
            v.push(t);
        }
    }

    fn texts(src: &str) -> Vec<&str> {
        lex(src).unwrap().iter().map(|t| t.text(src)).collect()
    }

    fn err(src: &str) -> ErrorKind {
        lex(src).unwrap_err().kind
    }

    fn string(src: &str) -> Vec<u16> {
        let t = lex(src).unwrap()[0];
        let mut out = Vec::new();
        decode_string(src, &t, &mut out).unwrap();
        out
    }

    fn u16s(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn malformed_public_token_spans_do_not_panic() {
        let string = Token { kind: Kind::String { legacy_octal: false }, start: 0, end: 3, newline_before: false, escapes: false };
        assert_eq!(decode_string("'é'", &string, &mut vec![]).unwrap_err().kind, ErrorKind::BadToken);
        let huge = Token { start: u32::MAX, end: u32::MAX, ..string };
        assert_eq!(decode_string("", &huge, &mut vec![]).unwrap_err().kind, ErrorKind::BadToken);
        let template = Token { kind: Kind::Template { part: TemplatePart::Tail, bad_escape: false }, ..string };
        assert_eq!(decode_template("}é`", &template, false, &mut vec![]).unwrap_err().kind, ErrorKind::BadToken);
        let number = Token { kind: Kind::Number(NumberBase::Hex), end: 1, ..string };
        assert_eq!(number_value("x", &number), None);
        assert_eq!(number_value("0b2", &Token { kind: Kind::Number(NumberBase::Binary), end: 3, ..number }), None);
    }

    #[test]
    fn maximal_munch() {
        let src = ">>>= >>> >> >= > **= ** * ?? ??= ?. ? ... . === == = !== != ! => <<= << <= < &&= && & ||= || | ^= ^ ~ ++ -- += -= %= /= : ; , ( ) [ ] { }";
        let t = lex(src).unwrap();
        let got: Vec<&str> = t.iter().map(|t| match t.kind { Kind::Punct(p) => p.as_str(), k => panic!("{k:?}") }).collect();
        assert_eq!(got.join(" "), src);
        assert_eq!(texts("a+++b"), ["a", "++", "+", "b"]);
        assert_eq!(texts("a>>>>b"), [("a"), ">>>", ">", "b"]);
        assert_eq!(texts("x=>y"), ["x", "=>", "y"]);
    }

    #[test]
    fn optional_chain_before_digit_is_conditional() {
        assert_eq!(texts("a?.5:1"), ["a", "?", ".5", ":", "1"]);
        assert_eq!(texts("a?.b"), ["a", "?.", "b"]);
        assert_eq!(texts("a?.[0]"), ["a", "?.", "[", "0", "]"]);
    }

    #[test]
    fn keywords_and_names() {
        let t = lex("if ifx $a _b a1 let async of await yield static null this").unwrap();
        assert_eq!(t[0].kind, Kind::Keyword(Keyword::If));
        assert!(t[1..9].iter().all(|t| t.kind == Ident));
        assert_eq!(t[10 + 1].kind, Kind::Keyword(Keyword::Null));
        assert_eq!(t[12].kind, Kind::Keyword(Keyword::This));
        // Escaped: a name, not a keyword, and flagged.
        let src = "\\u0069f";
        let t = lex(src).unwrap()[0];
        assert_eq!((t.kind, t.escapes), (Ident, true));
        assert_eq!(ident_name(src, &t).unwrap(), "if");
        let src = "a\\u{62}c";
        assert_eq!(ident_name(src, &lex(src).unwrap()[0]).unwrap(), "abc");
        assert!(matches!(ident_name("abc", &lex("abc").unwrap()[0]).unwrap(), Cow::Borrowed(_)));
    }

    #[test]
    fn private_names() {
        let src = "this.#x in #\\u0079";
        let t = lex(src).unwrap();
        assert_eq!(t[2].kind, PrivateName);
        assert_eq!(t[2].text(src), "#x");
        assert_eq!(ident_name(src, &t[2]).unwrap(), "x");
        assert_eq!(ident_name(src, &t[4]).unwrap(), "y");
        assert_eq!(err("#"), ErrorKind::InvalidPrivateName);
        assert_eq!(err("# x"), ErrorKind::InvalidPrivateName);
        assert_eq!(err("#1"), ErrorKind::InvalidPrivateName);
        assert_eq!(err("#€"), ErrorKind::NonAscii);
    }

    #[test]
    fn non_ascii_names_follow_id_start_and_id_continue() {
        let name = |src: &str| {
            let t = lex(src).unwrap();
            assert_eq!(t.len(), 1, "{src}");
            assert_eq!(t[0].kind, Kind::Ident, "{src}");
            ident_name(src, &t[0]).unwrap().into_owned()
        };
        assert_eq!(name("é"), "é");
        assert_eq!(name("a\u{b7}"), "a\u{b7}"); // the middle dot continues a name
        assert_eq!(err("\u{b7}"), ErrorKind::NonAscii);
        assert_eq!(err("\u{d7}"), ErrorKind::NonAscii); // the multiplication sign is not a letter
        assert_eq!(err("\u{b1}"), ErrorKind::NonAscii);
        assert_eq!(name("日本"), "日本");
        assert_eq!(name("a\u{3b2}b"), "aβb");
        assert_eq!(name("\\u00e9"), "é");
        assert_eq!(name("a\\u{3b2}"), "aβ");
        assert_eq!(name("x\u{200c}y"), "x\u{200c}y"); // ZWNJ continues a name
        assert_eq!(name("a\u{0301}"), "a\u{0301}"); // a combining mark continues a name
        assert_eq!(err("\u{0301}"), ErrorKind::NonAscii); // but cannot start one
        assert_eq!(err("\\u{1F600}"), ErrorKind::NonAscii); // an emoji is not a letter
        assert_eq!(err("€"), ErrorKind::NonAscii);
        assert_eq!(err("a€"), ErrorKind::NonAscii);
        assert_eq!(lex("#é").unwrap()[0].kind, Kind::PrivateName);
        assert_eq!(lex("({À:'A'})").unwrap().len(), 7);
        assert_eq!(err("\\u0031"), ErrorKind::InvalidEscape); // a digit cannot start a name
        assert_eq!(err("a\\x41"), ErrorKind::InvalidEscape);
        assert_eq!(err("a\\u12"), ErrorKind::InvalidEscape);
        assert_eq!(err("@"), ErrorKind::UnexpectedChar);
        assert_eq!(err("a\u{0}"), ErrorKind::UnexpectedChar);
        // But non-ASCII is fine in strings, comments, templates, regexps and as white space.
        assert_eq!(lex("'é' /*日本*/ `ü` // ö").unwrap().len(), 2);
        assert_eq!(texts("a\u{a0}b\u{feff}c\u{3000}d\u{2003}e\u{202f}f"), ["a", "b", "c", "d", "e", "f"]);
    }

    #[test]
    fn numbers_valid() {
        let cases: &[(&str, NumberBase, f64)] = &[
            ("0", NumberBase::Decimal, 0.0),
            ("12", NumberBase::Decimal, 12.0),
            ("1.5", NumberBase::Decimal, 1.5),
            ("5.", NumberBase::Decimal, 5.0),
            (".5", NumberBase::Decimal, 0.5),
            ("5.e3", NumberBase::Decimal, 5000.0),
            ("1e3", NumberBase::Decimal, 1000.0),
            ("1E-2", NumberBase::Decimal, 0.01),
            ("2.5e+2", NumberBase::Decimal, 250.0),
            ("1_000_000", NumberBase::Decimal, 1e6),
            ("1_0.2_5e1_0", NumberBase::Decimal, 10.25e10),
            ("0x1F", NumberBase::Hex, 31.0),
            ("0XfF_ff", NumberBase::Hex, 65535.0),
            ("0b101", NumberBase::Binary, 5.0),
            ("0B1_1", NumberBase::Binary, 3.0),
            ("0o17", NumberBase::Octal, 15.0),
            ("017", NumberBase::LegacyOctal, 15.0),
            ("00", NumberBase::LegacyOctal, 0.0),
            ("089", NumberBase::NonOctalDecimal, 89.0),
            ("08.5", NumberBase::NonOctalDecimal, 8.5),
            ("0.5", NumberBase::Decimal, 0.5),
            ("1e400", NumberBase::Decimal, f64::INFINITY),
            ("0xFFFFFFFFFFFFFFFF", NumberBase::Hex, 18446744073709551616.0),
            // 2^53 + 1 is a tie: rounds to even (2^53). With a lower bit set, it rounds up.
            ("0x20000000000001", NumberBase::Hex, 9007199254740992.0),
            ("0x20000000000003", NumberBase::Hex, 9007199254740996.0),
            // Digits past 64 bits only act as a sticky bit: an exact tie stays even, a tie plus a far 1 rounds up.
            ("0x200000000000010000000000000001", NumberBase::Hex, 1.6615349947311452e35),
            ("0x2000000000000100000000000000000000", NumberBase::Hex, 1.0889035741470031e40),
            ("0x20000000000001000000000000000000001", NumberBase::Hex, 1.7422457186352053e41),
            ("0x1000000000000000000000000000000000000000000000000000000000", NumberBase::Hex, 2f64.powi(228)),
        ];
        for &(s, base, v) in cases {
            let t = lex(s).unwrap();
            assert_eq!(t.len(), 1, "{s}");
            assert_eq!(t[0].kind, Number(base), "{s}");
            assert_eq!(number_value(s, &t[0]), Some(v), "{s}");
        }
        assert_eq!(texts("1..toString"), ["1.", ".", "toString"]);
        assert_eq!(texts("1.5.toFixed"), ["1.5", ".", "toFixed"]);
        assert_eq!(texts("1+2"), ["1", "+", "2"]);
        assert_eq!(texts("a.5"), ["a", ".5"]);
        assert_eq!(texts("0.0.1"), ["0.0", ".1"]);
        assert_eq!(texts("07.5"), ["07", ".5"]);
    }

    /// Long decimal literals (over a million units): Rust's float parser gives infinity for a
    /// cancelling exponent. Each expected value is the one Node gives for the same text.
    #[test]
    fn long_decimal_literals() {
        let m = 1_000_000;
        let z = "0".repeat(m);
        let val = |s: &str| {
            let t = lex(s).unwrap();
            assert_eq!(t.len(), 1, "{s:.40}");
            number_value(s, &t[0]).unwrap()
        };
        assert_eq!(val(&format!("{}.5e-{m}", "9".repeat(m))), 1.0);
        assert_eq!(val(&format!("1{z}e-{m}")), 1.0);
        assert_eq!(val(&format!("0.{z}1e{m}")), 0.1);
        assert_eq!(val(&format!("1{z}e-{}", m - 300)), 1e300);
        assert_eq!(val(&format!("4{z}e-{}", m + 324)), f64::from_bits(1));
        assert_eq!(val(&format!("2{z}e-{}", m + 324)), 0.0);
        assert_eq!(val(&format!("1{z}e-{}", m - 309)), f64::INFINITY);
        assert_eq!(val(&format!("17976931348623157{z}e-{}", m - 292)), f64::MAX);
        // Separators are stripped before the value is taken.
        let digits = "9".repeat(m).as_bytes().chunks(1000).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join("_");
        assert_eq!(val(&format!("{digits}.5e-{m}")), 1.0);
        // 1 + 2^-53 is exactly halfway between 1 and 1 + 2^-52: ties to even gives 1. A dropped
        // tail above the tie rounds up, even when it is far past the kept digits.
        let half = "1.00000000000000011102230246251565404236316680908203125";
        assert_eq!(val(&format!("{half}{z}")), 1.0);
        assert_eq!(val(&format!("{half}{}", "0".repeat(2000))), 1.0);
        assert_eq!(val(&format!("{half}{z}1")), 1.0 + f64::EPSILON);
        assert_eq!(val(&format!("{half}{}1", "0".repeat(2000))), 1.0 + f64::EPSILON);
    }

    #[test]
    fn bigint_scans() {
        for (s, base) in [("10n", NumberBase::Decimal), ("0xFn", NumberBase::Hex), ("0b1n", NumberBase::Binary), ("0o7n", NumberBase::Octal), ("0n", NumberBase::Decimal)] {
            let t = lex(s).unwrap();
            assert_eq!(t[0].kind, BigInt(base), "{s}");
            assert_eq!(number_value(s, &t[0]), None);
        }
        for s in ["1.5n", "1e3n", ".5n", "07n", "089n", "1nn", "1n_"] {
            assert_eq!(err(s), ErrorKind::InvalidNumber, "{s}");
        }
    }

    #[test]
    fn numbers_invalid() {
        for s in [
            "0x", "0b", "0o", "0xg", "0b2", "0o8", "1e", "1e+", "1e-x", "3in", "1_", "1__0", "0_1", "1_.5", "1._5", "1e_5",
            "0x_1", "0x1_", "1.5_", "08_1", "07_1", "1a", "0x1g", "5\\u0061", "0b1_", "1_e5", "00_1",
        ] {
            assert_eq!(err(s), ErrorKind::InvalidNumber, "{s}");
        }
        // The offset points at the bad character.
        assert_eq!(lex("1__0").unwrap_err().offset, 1);
        assert_eq!(lex("12abc").unwrap_err().offset, 2);
        assert_eq!(lex("0x").unwrap_err().offset, 0);
    }

    #[test]
    fn strings_decode_utf16() {
        assert_eq!(string("'abc'"), u16s("abc"));
        assert_eq!(string("\"\""), []);
        assert_eq!(string(r#""a\nb\t\r\b\f\v\0\\\'\"\/""#), u16s("a\nb\t\r\u{8}\u{c}\u{b}\0\\'\"/"));
        assert_eq!(string("'\\x41\\u0042\\u{43}\\u{1F600}'"), u16s("ABC😀"));
        assert_eq!(string("'😀'"), [0xD83D, 0xDE00]); // a real emoji: two code units
        assert_eq!(string("'\\uD83D\\uDE00'"), [0xD83D, 0xDE00]); // an escaped pair: same two units
        assert_eq!(string(r"'\uD800'"), [0xD800]); // a lone high surrogate
        assert_eq!(string(r"'a\uDC00b'"), [0x61, 0xDC00, 0x62]); // a lone low surrogate
        assert_eq!(string(r"'\u{D800}'"), [0xD800]);
        assert_eq!(string(r"'\u{10FFFF}'"), [0xDBFF, 0xDFFF]);
        assert_eq!(string(r"'\u{0000041}'"), [0x41]);
        assert_eq!(string("'a\\\nb'"), u16s("ab")); // line continuation
        assert_eq!(string("'a\\\r\nb'"), u16s("ab"));
        assert_eq!(string("'a\\\u{2028}b'"), u16s("ab"));
        assert_eq!(string("'a\u{2028}b\u{2029}'"), u16s("a\u{2028}b\u{2029}")); // legal raw in a string
        assert_eq!(string(r"'\é'"), u16s("é"));
        assert_eq!(string(r"'\😀'"), [0xD83D, 0xDE00]);
        assert_eq!(string("'日本語'"), u16s("日本語"));
        // Legacy octal.
        assert_eq!(string(r"'\101\7\08\1\377\400'"), [0x41, 7, 0, b'8' as u16, 1, 0xFF, 0x20, b'0' as u16]);
        assert_eq!(string(r"'\8\9'"), u16s("89"));
    }

    #[test]
    fn string_flags_and_raw_spans() {
        let src = r#"'a' "b\n" 'c\1' '\0' '\08'"#;
        let t = lex(src).unwrap();
        assert_eq!((t[0].kind, t[0].escapes), (Kind::String { legacy_octal: false }, false));
        assert_eq!((t[1].kind, t[1].escapes), (Kind::String { legacy_octal: false }, true));
        assert_eq!(t[2].kind, Kind::String { legacy_octal: true });
        assert_eq!(t[3].kind, Kind::String { legacy_octal: false }); // \0 alone is fine
        assert_eq!(t[4].kind, Kind::String { legacy_octal: true }); // \0 before a digit is octal
        assert_eq!(t[1].text(src), "\"b\\n\"");
    }

    #[test]
    fn strings_invalid() {
        for s in [r"'\x4'", r"'\xg0'", r"'\u12'", r"'\u{}'", r"'\u{110000}'", r"'\u{12'", r"'\uXYZW'", r"'\u{1g}'"] {
            assert_eq!(err(s), ErrorKind::InvalidEscape, "{s}");
        }
        assert_eq!(lex(r"'ab\x4'").unwrap_err().offset, 3); // at the backslash
        for s in ["'abc", "\"abc", "'abc\n'", "'abc\r'", "'abc\\", "'a\\'", "'\\", "'"] {
            assert_eq!(err(s), ErrorKind::UnterminatedString, "{s}");
        }
        assert_eq!(lex("x = 'abc\n'").unwrap_err().offset, 4); // at the opening quote
    }

    #[test]
    fn decode_appends_and_checks_token() {
        let src = "'ab'";
        let t = lex(src).unwrap()[0];
        let mut out = vec![1u16];
        decode_string(src, &t, &mut out).unwrap();
        assert_eq!(out, [1, 0x61, 0x62]);
        let fake = Token { kind: Kind::String { legacy_octal: false }, start: 0, end: 99, newline_before: false, escapes: false };
        assert_eq!(decode_string(src, &fake, &mut out).unwrap_err().kind, ErrorKind::BadToken);
        let fake = Token { end: 1, ..fake };
        assert_eq!(decode_string(src, &fake, &mut out).unwrap_err().kind, ErrorKind::BadToken);
        let num = lex("1").unwrap()[0];
        assert_eq!(decode_string("1", &num, &mut out).unwrap_err().kind, ErrorKind::BadToken);
        assert_eq!(number_value("'a'", &t), None);
    }

    #[test]
    fn comments_and_newline_metadata() {
        let nl = |src: &str| lex(src).unwrap().iter().map(|t| t.newline_before).collect::<Vec<_>>();
        assert_eq!(nl("a\nb"), [false, true]);
        assert_eq!(nl("a\rb"), [false, true]);
        assert_eq!(nl("a\r\nb"), [false, true]);
        assert_eq!(nl("a\u{2028}b"), [false, true]);
        assert_eq!(nl("a\u{2029}b"), [false, true]);
        assert_eq!(nl("a b"), [false, false]);
        assert_eq!(nl("a /* x */ b"), [false, false]);
        assert_eq!(nl("a /* x\n */ b"), [false, true]); // a newline inside a block comment counts
        assert_eq!(nl("a /* \u{2028} */ b"), [false, true]);
        assert_eq!(nl("a // x\nb"), [false, true]);
        assert_eq!(nl("a // x \u{2029} b"), [false, true]); // U+2029 ends a line comment
        assert_eq!(nl("a // x b"), [false]);
        // 0xE2 starts many characters (euro, check mark). They are not line terminators.
        assert_eq!(nl("a // \u{20ac}\u{2713} b"), [false]);
        assert_eq!(nl("a /* \u{20ac}\u{2713} */ b"), [false, false]);
        assert_eq!(nl("a // \u{20ac}\nb"), [false, true]);
        assert_eq!(nl("a /* ** * / \u{20ac}\u{2028} * */ b"), [false, true]);
        assert_eq!(nl("a /* * ** **/ b"), [false, false]);
        assert_eq!(nl("\n a"), [true]); // the first token too
        assert_eq!(nl("a\n\n/**/ \t\u{b}\u{c} b"), [false, true]);
        assert_eq!(texts("a/**/b/***/c/* * / */d"), ["a", "b", "c", "d"]);
        assert_eq!(texts("a//b\nc"), ["a", "c"]);
        assert_eq!(texts("a//b\r\nc"), ["a", "c"]);
        assert_eq!(texts("a/*/ b */c"), ["a", "c"]);
        // Hashbang: only at offset 0.
        assert_eq!(texts("#!/usr/bin/env node\nfoo"), ["foo"]);
        assert_eq!(err("\n#!x"), ErrorKind::InvalidPrivateName);
        assert_eq!(err(" #!x"), ErrorKind::InvalidPrivateName);
        // Errors.
        assert_eq!(lex("a /* x").unwrap_err(), Error { kind: ErrorKind::UnterminatedComment, offset: 2 });
        assert_eq!(err("/*"), ErrorKind::UnterminatedComment);
        assert_eq!(err("/*/"), ErrorKind::UnterminatedComment);
        assert_eq!(err("/* *"), ErrorKind::UnterminatedComment);
    }

    #[test]
    fn slash_goal() {
        let src = "a / b /= c";
        let mut l = Lexer::new(src).unwrap();
        let kinds: Vec<_> = (0..5).map(|_| l.next(Goal::Div).unwrap().kind).collect();
        assert_eq!(kinds[1], Kind::Punct(Punct::Slash));
        assert_eq!(kinds[3], Kind::Punct(Punct::SlashAssign));
        // The same bytes with the regexp goal.
        let src = "/ b /= c/gi";
        let mut l = Lexer::new(src).unwrap();
        let t = l.next(Goal::RegExp).unwrap();
        assert_eq!((t.kind, t.text(src)), (RegExp, "/ b /"));
        assert_eq!(l.next(Goal::Div).unwrap().kind, Kind::Punct(Punct::Assign));
        let src = "/a/ /b/g";
        let mut l = Lexer::new(src).unwrap();
        assert_eq!(l.next(Goal::RegExp).unwrap().text(src), "/a/");
        assert_eq!(l.next(Goal::RegExp).unwrap().text(src), "/b/g");
        // Comments win over regexps in every goal.
        let src = "// c\n/*x*/ /y/";
        let mut l = Lexer::new(src).unwrap();
        assert_eq!(l.next(Goal::RegExp).unwrap().text(src), "/y/");
        // Rescan: the parser read `/=` and now knows it starts a regexp. newline_before is kept.
        let src = "x\n/=a=/.test(y)";
        let mut l = Lexer::new(src).unwrap();
        l.next(Goal::Div).unwrap();
        let slash = l.next(Goal::Div).unwrap();
        assert_eq!((slash.kind, slash.newline_before), (Kind::Punct(Punct::SlashAssign), true));
        let re = l.rescan(&slash, Goal::RegExp).unwrap();
        assert_eq!((re.kind, re.text(src), re.newline_before), (RegExp, "/=a=/", true));
        assert_eq!(l.next(Goal::Div).unwrap().kind, Kind::Punct(Punct::Dot));
    }

    #[test]
    fn regexp_boundaries() {
        let ok = |src: &str| {
            let mut l = Lexer::new(src).unwrap();
            let t = l.next(Goal::RegExp).unwrap();
            assert_eq!(t.kind, RegExp, "{src}");
            assert_eq!(l.next(Goal::Div).unwrap().kind, Eof, "{src}");
            regexp_parts(src, &t).map(|(p, f)| (p.to_string(), f.to_string())).unwrap()
        };
        assert_eq!(ok("/a/"), ("a".into(), "".into()));
        assert_eq!(ok(r"/a\/b/gi"), (r"a\/b".into(), "gi".into()));
        assert_eq!(ok("/[/]/"), ("[/]".into(), "".into())); // a slash in a class
        assert_eq!(ok(r"/[\]/]+/u"), (r"[\]/]+".into(), "u".into()));
        assert_eq!(ok(r"/^\/journeys\/(?!new\/?$)[^/]+\/?$/"), (r"^\/journeys\/(?!new\/?$)[^/]+\/?$".into(), "".into()));
        assert_eq!(ok("/(/"), ("(".into(), "".into())); // the pattern is not checked
        assert_eq!(ok("/a/dgimsuy"), ("a".into(), "dgimsuy".into()));
        assert_eq!(ok("/日本/"), ("日本".into(), "".into()));
        assert_eq!(ok(r"/\\/"), (r"\\".into(), "".into()));
        assert_eq!(ok("/[[]/"), ("[[]".into(), "".into()));
        assert_eq!(ok("/a/\u{a0}"), ("a".into(), "".into()));
        let rl = |s: &str| Lexer::new(s).unwrap().next(Goal::RegExp).unwrap_err().kind;
        for s in ["/a", "/a\nb/", "/a\\\n/", "/[/]", "/a\u{2028}/", "/\\", "/a\\"] {
            assert_eq!(rl(s), ErrorKind::UnterminatedRegExp, "{s:?}");
        }
        assert_eq!(rl("/a/gg"), ErrorKind::DuplicateRegExpFlag);
        assert_eq!(rl("/a/gig"), ErrorKind::DuplicateRegExpFlag);
        assert_eq!(rl("/a/vv"), ErrorKind::DuplicateRegExpFlag);
        assert_eq!(rl("/a/x"), ErrorKind::InvalidRegExpFlag);
        assert_eq!(rl("/a/g1"), ErrorKind::InvalidRegExpFlag);
        assert_eq!(rl("/a/\\u0067"), ErrorKind::InvalidRegExpFlag);
    }

    #[test]
    fn regexp_flag_followed_by_unsupported_char() {
        let src = "/a/g€";
        let mut l = Lexer::new(src).unwrap();
        assert_eq!(l.next(Goal::RegExp).unwrap().text(src), "/a/g");
        assert_eq!(l.next(Goal::Div).unwrap_err().kind, ErrorKind::NonAscii);
    }

    /// Scan a whole template tree the way a parser does: the caller keeps the `${` count.
    fn template_parts(src: &str) -> Vec<(TemplatePart, std::string::String)> {
        let mut l = Lexer::new(src).unwrap();
        let mut out = Vec::new();
        let mut depth = 0; // open `${`
        let mut braces = Vec::new(); // `{` depth inside each `${`
        loop {
            let t = l.next(Goal::Div).unwrap();
            match t.kind {
                Eof => return out,
                Template { part, .. } => {
                    let mut v = Vec::new();
                    decode_template(src, &t, false, &mut v).unwrap();
                    out.push((part, std::string::String::from_utf16(&v).unwrap()));
                    match part {
                        TemplatePart::Head => {
                            depth += 1;
                            braces.push(0);
                        }
                        TemplatePart::Tail => {
                            depth -= 1;
                            braces.pop();
                        }
                        _ => {}
                    }
                }
                Kind::Punct(Punct::LBrace) if depth > 0 => *braces.last_mut().unwrap() += 1,
                Kind::Punct(Punct::RBrace) if depth > 0 => {
                    if *braces.last().unwrap() == 0 {
                        // This `}` closes `${`: the parser rescans it as a template continuation.
                        let t = l.rescan(&t, Goal::TemplateTail).unwrap();
                        let Template { part, .. } = t.kind else { panic!() };
                        let mut v = Vec::new();
                        decode_template(src, &t, false, &mut v).unwrap();
                        out.push((part, std::string::String::from_utf16(&v).unwrap()));
                        if part == TemplatePart::Tail {
                            depth -= 1;
                            braces.pop();
                        }
                    } else {
                        *braces.last_mut().unwrap() -= 1;
                    }
                }
                _ => {}
            }
        }
    }

    #[test]
    fn templates_nested_by_caller() {
        use TemplatePart::*;
        assert_eq!(template_parts("`abc`"), [(NoSubstitution, "abc".into())]);
        assert_eq!(template_parts("`${a}`"), [(Head, "".into()), (Tail, "".into())]);
        assert_eq!(
            template_parts("`a${b}c${d}e`"),
            [(Head, "a".into()), (Middle, "c".into()), (Tail, "e".into())]
        );
        // Braces, objects and a nested template inside `${ }`.
        assert_eq!(
            template_parts("`a${ {x:{}}.x }b${ `c${d}e` }f`"),
            [(Head, "a".into()), (Middle, "b".into()), (Head, "c".into()), (Tail, "e".into()), (Tail, "f".into())]
        );
        assert_eq!(template_parts("`$ {a} $$ \\${x} \\` $`"), [(NoSubstitution, "$ {a} $$ ${x} ` $".into())]);
    }

    #[test]
    fn template_goal_direct() {
        let src = "a }mid${ b }end`";
        let mut l = Lexer::new(src).unwrap();
        l.next(Goal::Div).unwrap();
        let t = l.next(Goal::TemplateTail).unwrap();
        assert_eq!((t.kind, t.text(src)), (Template { part: TemplatePart::Middle, bad_escape: false }, "}mid${"));
        l.next(Goal::Div).unwrap();
        let t = l.next(Goal::TemplateTail).unwrap();
        assert_eq!((t.kind, t.text(src)), (Template { part: TemplatePart::Tail, bad_escape: false }, "}end`"));
        // Not a `}`: an error, and it stays an error.
        let mut l = Lexer::new("a b").unwrap();
        l.next(Goal::Div).unwrap();
        let e = l.next(Goal::TemplateTail).unwrap_err();
        assert_eq!((e.kind, e.offset), (ErrorKind::ExpectedTemplateContinuation, 2));
        assert_eq!(l.next(Goal::Div).unwrap_err(), e);
        // Unterminated.
        assert_eq!(err("`abc"), ErrorKind::UnterminatedTemplate);
        assert_eq!(err("`a\\"), ErrorKind::UnterminatedTemplate);
        let mut l = Lexer::new("}abc").unwrap();
        assert_eq!(l.next(Goal::TemplateTail).unwrap_err().kind, ErrorKind::UnterminatedTemplate);
    }

    #[test]
    fn template_values() {
        let one = |src: &str, raw: bool| {
            let t = Lexer::new(src).unwrap().next(Goal::Div).unwrap();
            let mut v = Vec::new();
            decode_template(src, &t, raw, &mut v).map(|_| v)
        };
        assert_eq!(one("`a\\nb`", false).unwrap(), u16s("a\nb"));
        assert_eq!(one("`a\\nb`", true).unwrap(), u16s("a\\nb")); // raw keeps the backslash
        assert_eq!(one("`a\r\nb\rc`", false).unwrap(), u16s("a\nb\nc")); // CRLF and CR read as LF
        assert_eq!(one("`a\r\nb\rc`", true).unwrap(), u16s("a\nb\nc"));
        assert_eq!(one("`a\\\r\nb`", false).unwrap(), u16s("ab"));
        assert_eq!(one("`a\\\r\nb`", true).unwrap(), u16s("a\\\nb"));
        assert_eq!(one("`\\uD800\\u{1F600}😀`", false).unwrap(), [0xD800, 0xD83D, 0xDE00, 0xD83D, 0xDE00]);
        assert_eq!(one("`\\0`", false).unwrap(), [0]);
        // Bad escapes: the token is fine, the cooked value is not, the raw value is.
        for s in ["`\\01`", "`\\1`", "`\\8`", "`\\xg`", "`\\u12`", "`\\u{110000}`", "`\\u{`"] {
            let t = lex(s).unwrap()[0];
            assert_eq!(t.kind, Template { part: TemplatePart::NoSubstitution, bad_escape: true }, "{s}");
            assert_eq!(one(s, false).unwrap_err().kind, ErrorKind::InvalidEscape, "{s}");
            assert!(one(s, true).is_ok(), "{s}");
        }
        // The text after a bad escape is read normally: this closes the template at the last backtick.
        assert_eq!(lex("`\\u{110000}` x").unwrap().len(), 2);
        let src = "`\\xg ${a}`";
        let t = Lexer::new(src).unwrap().next(Goal::Div).unwrap();
        assert_eq!(t.kind, Template { part: TemplatePart::Head, bad_escape: true });
        assert_eq!(one("`${a}`", false).unwrap(), []);
        // Head, middle and tail strip their own delimiters.
        let src = "`a${x}b${y}c`";
        let mut l = Lexer::new(src).unwrap();
        let h = l.next(Goal::Div).unwrap();
        let x = l.next(Goal::Div).unwrap();
        let rb = l.next(Goal::Div).unwrap();
        assert_eq!(x.text(src), "x");
        let m = l.rescan(&rb, Goal::TemplateTail).unwrap();
        let mut v = Vec::new();
        decode_template(src, &h, true, &mut v).unwrap();
        decode_template(src, &m, true, &mut v).unwrap();
        assert_eq!(v, u16s("ab"));
        let nonstr = lex("1").unwrap()[0];
        assert_eq!(decode_template("1", &nonstr, false, &mut v).unwrap_err().kind, ErrorKind::BadToken);
    }

    #[test]
    fn rescan_rejects_foreign_tokens() {
        let mut l = Lexer::new("ab").unwrap();
        let t = Token { kind: Eof, start: 9, end: 9, newline_before: false, escapes: false };
        assert_eq!(l.rescan(&t, Goal::Div).unwrap_err().kind, ErrorKind::BadToken);
        let mut l = Lexer::new("é").unwrap();
        let t = Token { start: 1, ..t };
        assert_eq!(l.rescan(&t, Goal::Div).unwrap_err().kind, ErrorKind::BadToken);
    }

    #[test]
    fn eof_and_errors_are_sticky() {
        let mut l = Lexer::new("  ").unwrap();
        let t = l.next(Goal::Div).unwrap();
        assert_eq!((t.kind, t.start, t.end), (Eof, 2, 2));
        assert_eq!(l.next(Goal::Div).unwrap().kind, Eof);
        let mut l = Lexer::new("a @ b").unwrap();
        l.next(Goal::Div).unwrap();
        let e = l.next(Goal::Div).unwrap_err();
        assert_eq!(e, Error { kind: ErrorKind::UnexpectedChar, offset: 2 });
        assert_eq!(l.next(Goal::Div).unwrap_err(), e);
    }

    #[test]
    fn source_cap_is_exact() {
        let big = " ".repeat(MAX_SOURCE_LEN);
        let mut l = Lexer::new(&big).unwrap();
        let t = l.next(Goal::Div).unwrap();
        assert_eq!((t.kind, t.start), (Eof, MAX_SOURCE_LEN as u32));
        let over = " ".repeat(MAX_SOURCE_LEN + 1);
        let e = Lexer::new(&over).err().unwrap();
        assert_eq!(e.kind, ErrorKind::SourceTooLarge);
        assert!(MAX_SOURCE_LEN < u32::MAX as usize);
    }

    #[test]
    fn locations_count_utf16() {
        let src = "ab\n'😀' @";
        let e = lex(src).unwrap_err();
        assert_eq!(e.offset, 10); // bytes: a b \n ' (4 bytes) ' space
        assert_eq!(e.location(src), Location { line: 2, column: 5 }); // quote 1, emoji 2, quote 1, space 1
        assert_eq!(location("a\r\nb", 3), Location { line: 2, column: 0 });
        assert_eq!(location("a\rb", 2), Location { line: 2, column: 0 });
        assert_eq!(location("a\u{2028}bc", 6), Location { line: 2, column: 2 });
        assert_eq!(location("é", 1), Location { line: 1, column: 0 }); // inside a character: clamped back
        assert_eq!(location("ab", 99), Location { line: 1, column: 2 });
        assert_eq!(utf16_offset("a😀b", 5), 3);
        assert_eq!(utf16_offset("a😀b", 1), 1);
    }

    #[test]
    fn tokens_are_small() {
        assert!(std::mem::size_of::<Token>() <= 12);
    }

    /// Random mixes of syntax characters: no panic, every token inside the source and moving forward,
    /// a failed lexer stays failed. A fixed seed, so a failure repeats.
    #[test]
    fn fuzz_terminates_and_stays_in_bounds() {
        const PARTS: &[&str] = &[
            "a", "if", "1", "0x", "1_0", ".", "..", "...", "'", "\"", "`", "${", "}", "{", "/", "/*", "*/", "//", "\\", "\\u", "\\u{", "é", "😀", "\n", "\r\n", "\u{2028}", " ", "#", "#!", "?.", "?", "=", ">", ">>>", "e+", "n", "[", "]", "(", ")", "$", "_", "\0", "\u{a0}",
        ];
        let mut seed = 0x9E3779B97F4A7C15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20000 {
            let n = next() % 12;
            let src: std::string::String = (0..n).map(|_| PARTS[(next() % PARTS.len() as u64) as usize]).collect();
            let mut l = Lexer::new(&src).unwrap();
            let mut last = 0u32;
            for step in 0..src.len() + 2 {
                let goal = [Goal::Div, Goal::RegExp, Goal::TemplateTail][(next() % 3) as usize];
                match l.next(goal) {
                    Ok(t) => {
                        assert!(t.start <= t.end && t.end as usize <= src.len(), "{src:?}");
                        assert!(src.is_char_boundary(t.start as usize) && src.is_char_boundary(t.end as usize), "{src:?}");
                        assert!(t.start >= last, "{src:?}");
                        assert!(t.kind == Eof || t.end > t.start, "{src:?}");
                        last = t.end;
                        if t.kind == Eof {
                            break;
                        }
                        let _ = decode_string(&src, &t, &mut Vec::new());
                        let _ = decode_template(&src, &t, false, &mut Vec::new());
                        let _ = ident_name(&src, &t);
                        let _ = number_value(&src, &t);
                        let _ = regexp_parts(&src, &t);
                    }
                    Err(e) => {
                        assert!(e.offset as usize <= src.len(), "{src:?}");
                        assert_eq!(l.next(goal).unwrap_err(), e);
                        let _ = e.location(&src);
                        break;
                    }
                }
                assert!(step <= src.len(), "no progress on {src:?}");
            }
        }
    }
    #[test]
    fn forged_long_decimal_tokens_do_not_panic_or_parse_junk() {
        for suffix in ["e!", "..1", "x", "e", "e+"] {
            let source = "1".repeat(2000) + suffix;
            let token = Token { kind: Kind::Number(NumberBase::Decimal), start: 0, end: source.len() as u32, newline_before: false, escapes: false };
            assert!(std::panic::catch_unwind(|| number_value(&source, &token)).unwrap().is_none());
        }
    }


    #[test]
    fn marked_locations_match_the_full_scan() {
        // CR, CRLF split at many offsets, LS, PS, 2-, 3- and 4-byte characters, across several marks
        let mut src = String::new();
        for i in 0..3000 {
            src.push_str(["ab\r\n", "é\r", "x\u{2028}", "😀\n", "€", "\u{2029}q", "lo"][i % 7]);
        }
        // the character-by-character scan that the byte scan replaced
        let reference = |byte: usize| {
            let mut end = byte.min(src.len());
            while !src.is_char_boundary(end) {
                end -= 1;
            }
            let (mut line, mut column, mut prev_cr) = (1u32, 0u32, false);
            for ch in src[..end].chars() {
                match ch {
                    '\n' if prev_cr => {}
                    '\n' | '\r' | '\u{2028}' | '\u{2029}' => (line, column) = (line + 1, 0),
                    _ => column += ch.len_utf16() as u32,
                }
                prev_cr = ch == '\r';
            }
            Location { line, column }
        };
        let marks = location_marks(&src);
        assert_eq!(marks.len(), src.len().div_ceil(MARK_STEP));
        for b in 0..=src.len() + 3 {
            let want = reference(b);
            assert_eq!(location_in(&src, &marks, b), want, "byte {b}");
            assert_eq!(location(&src, b), want, "byte {b}");
        }
        assert_eq!(location_in("", &location_marks(""), 5), Location { line: 1, column: 0 });
    }
}
