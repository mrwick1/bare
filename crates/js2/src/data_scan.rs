//! Check and walk the JSON subset of JavaScript data literals.
use crate::ast::Span;

/// Limit speculative scans to sources sized like a bulk data script.
pub(crate) const BULK_SOURCE_BYTES: usize = 2 << 20;

/// Small literals stay on the ordinary parser path.
pub(crate) const MIN_BYTES: u32 = 1024;
/// Bound the stack used to scan and lower a recipe.
pub(crate) const MAX_DEPTH: usize = 64;

/// A checked span can stand for a literal with no JS expressions or prototype setters.
pub(crate) fn scan_span(source: &str, depth: usize, list: u32) -> Option<(u32, u32)> {
    let mut scan = Scan::new(source, list);
    scan.value(depth.min(MAX_DEPTH), &mut |_| Some(()))?;
    // Empty containers, even nested ones, can be valid assignment patterns.
    (scan.scalar && scan.at >= MIN_BYTES as usize).then_some((u32::try_from(scan.at).ok()?, u32::try_from(scan.nodes).ok()?))
}

pub(crate) enum Part {
    Open(bool),
    Close(u32),
    String(Span),
    Number(Span),
    Bool(bool),
    Null,
}

pub(crate) struct Scan<'a> {
    source: &'a str,
    pub(crate) at: usize,
    nodes: usize,
    list: u32,
    scalar: bool,
}

impl<'a> Scan<'a> {
    pub(crate) fn new(source: &'a str, list: u32) -> Self {
        Self { source, at: 0, nodes: 0, list, scalar: false }
    }

    fn peek(&self) -> Option<u8> {
        self.source.as_bytes().get(self.at).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.at += 1;
        }
    }

    fn span(&self, start: usize) -> Span {
        Span { start: start as u32, end: self.at as u32 }
    }

    fn string(&mut self) -> Option<(Span, bool)> {
        let start = self.at;
        self.at += 1;
        let mut escaped = false;
        loop {
            match self.peek()? {
                b'"' => {
                    self.at += 1;
                    return Some((self.span(start), escaped));
                }
                b'\\' => {
                    escaped = true;
                    self.at += 1;
                    match self.peek()? {
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => self.at += 1,
                        b'u' if self.source.as_bytes().get(self.at + 1..self.at + 5)?.iter().all(u8::is_ascii_hexdigit) => self.at += 5,
                        _ => return None,
                    }
                }
                0..=31 => return None,
                _ => self.at += 1,
            }
        }
    }

    fn key(&mut self) -> Option<Span> {
        if self.peek()? != b'"' {
            return None;
        }
        let (span, escaped) = self.string()?;
        let body = &self.source[span.start as usize + 1..span.end as usize - 1];
        if body == "__proto__" {
            return None;
        }
        if escaped && body.len() >= "__proto__".len() {
            use crate::lex::{Kind, Token, decode_string};
            let mut units = Vec::new();
            let token = Token { kind: Kind::String { legacy_octal: false }, start: span.start, end: span.end, newline_before: false, escapes: true };
            decode_string(self.source, &token, &mut units).ok()?;
            if units.into_iter().eq("__proto__".encode_utf16()) {
                return None;
            }
        }
        self.ws();
        if self.peek()? != b':' {
            return None;
        }
        self.at += 1;
        Some(span)
    }

    fn digits(&mut self) -> Option<()> {
        let start = self.at;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        (self.at != start).then_some(())
    }

    fn number(&mut self) -> Option<Span> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
            self.nodes += 1;
        }
        match self.peek()? {
            b'0' => self.at += 1,
            b'1'..=b'9' => self.digits()?,
            _ => return None,
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            self.digits()?;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            self.digits()?;
        }
        Some(self.span(start))
    }

    pub(crate) fn value(&mut self, depth: usize, part: &mut impl FnMut(Part) -> Option<()>) -> Option<()> {
        self.ws();
        self.nodes += 1;
        let value = match self.peek()? {
            open @ (b'[' | b'{') if depth > 0 => {
                self.at += 1;
                let object = open == b'{';
                let close = if object { b'}' } else { b']' };
                part(Part::Open(object))?;
                self.ws();
                let mut len = 0;
                if self.peek()? != close {
                    loop {
                        if len == self.list {
                            return None;
                        }
                        len += 1;
                        self.nodes += 1 + usize::from(object);
                        if object {
                            part(Part::String(self.key()?))?;
                        }
                        self.value(depth - 1, part)?;
                        self.ws();
                        match self.peek()? {
                            b',' => self.at += 1,
                            c if c == close => break,
                            _ => return None,
                        }
                        self.ws();
                    }
                }
                self.at += 1;
                return part(Part::Close(len));
            }
            b'"' => Part::String(self.string()?.0),
            b'-' | b'0'..=b'9' => Part::Number(self.number()?),
            b't' | b'f' | b'n' => {
                let (word, value) = match self.peek()? {
                    b't' => ("true", Part::Bool(true)),
                    b'f' => ("false", Part::Bool(false)),
                    _ => ("null", Part::Null),
                };
                if !self.source[self.at..].starts_with(word) {
                    return None;
                }
                self.at += word.len();
                value
            }
            _ => return None,
        };
        self.scalar = true;
        part(value)
    }
}
