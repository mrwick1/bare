//! Strict `JSON.parse` text parsing into the heap (no reviver, no `stringify`).
//!
//! Input is UTF-16 code units (`parse`) or UTF-8 bytes (`parse_utf8`). The grammar is RFC 8259
//! as ECMAScript uses it: whitespace is only space, tab, LF and CR; no comments, no trailing
//! commas, no leading zeros, no BOM. Numbers use Rust's float parser after long mantissas
//! are reduced to a bounded significant prefix and sticky digit. `-0` stays `-0`; `1e400` is `Infinity`.
//! Strings keep lone surrogates as given. A repeated object key keeps its first position and
//! takes the last value. `"__proto__"` is an ordinary own property.
//!
//! Safety of the host stack: the parser is iterative. Open containers live in a `Vec`, never in
//! Rust recursion, so no input can overflow the stack. `max_depth` is clamped to
//! `HARD_MAX_DEPTH` so that `Vec` stays bounded even when the caller passes `usize::MAX`.
//!
//! Budgets: source length, depth, node count and decoded text are all checked before the
//! allocation they guard. A node is any value (primitive, array or object). Decoded text is the
//! sum of the UTF-16 length of every string value and every object key occurrence.
//!
//! Heap writes use `define_own_property` with a data descriptor, so no setter, accessor or
//! prototype `__proto__` is ever run or consulted.
//!
//! No automatic GC (read this before using a failed parse): `parse` never calls
//! `Heap::collect`. When it fails after it has allocated, the objects and strings made so far
//! stay in the heap, unreachable from every root. They count against the heap limits until the
//! caller runs `collect` with its own roots. The caller's roots are never touched. The strings are
//! interned, so a string that a live value already uses is shared, not copied. On success the
//! result and everything it reaches are new or shared and are not rooted either: root the result
//! before the next `collect`.
//!
//! Offsets: for `parse` an offset counts UTF-16 units from the start of `src`; it is the first
//! unit that cannot continue the text, or `src.len()` at the end. For a limit error it is the
//! position where the budget would be exceeded (for `Source` it is `max_source_units`). For
//! `parse_utf8`, `Syntax::InvalidUtf8` offsets count bytes; every other offset counts UTF-16
//! units of the decoded text.

use crate::heap::{Heap, HeapError, PropertyDescriptor};
use crate::number;
use crate::value::{ObjId, PropertyKey, StrId, Value};

/// Largest depth any call accepts, whatever `JsonLimits::max_depth` says.
pub const HARD_MAX_DEPTH: usize = 1 << 20;

#[derive(Clone, Copy, Debug)]
pub struct JsonLimits {
    /// Most UTF-16 units (or bytes for `parse_utf8`) of input.
    pub max_source_units: usize,
    /// Most open containers at once. `[[1]]` has depth 2.
    pub max_depth: usize,
    /// Most values in the whole text.
    pub max_nodes: usize,
    /// Most decoded UTF-16 units in all strings and keys together.
    pub max_text_units: usize,
}

impl Default for JsonLimits {
    fn default() -> Self {
        JsonLimits { max_source_units: 1 << 27, max_depth: 1024, max_nodes: 1 << 24, max_text_units: 1 << 27 }
    }
}

/// Prototypes for the new objects. `None` gives a null prototype (the caller must pass its
/// `%Object.prototype%` and `%Array.prototype%` to get JS behavior). Both handles are checked
/// before any allocation.
#[derive(Clone, Copy, Debug, Default)]
pub struct JsonOptions {
    pub object_proto: Option<ObjId>,
    pub array_proto: Option<ObjId>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Syntax {
    UnexpectedEnd,
    UnexpectedChar,
    /// A number that breaks the grammar: `-`, `01`, `1.`, `1e`, `.5`, `+1`.
    BadNumber,
    /// Unknown escape, or a `\u` without four hex digits.
    BadEscape,
    /// A raw unit below 0x20 inside a string.
    ControlChar,
    /// A complete value followed by something other than whitespace.
    TrailingData,
    /// `parse_utf8` only. The offset is a byte offset.
    InvalidUtf8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LimitKind {
    Source,
    Depth,
    Nodes,
    Text,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorKind {
    Syntax(Syntax),
    Limit(LimitKind),
    /// A heap limit or a stale handle in the options (offset 0 for the options).
    Heap(HeapError),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Error {
    pub kind: ErrorKind,
    pub offset: usize,
}

type Res<T> = Result<T, Error>;

fn fail<T>(kind: ErrorKind, offset: usize) -> Res<T> {
    Err(Error { kind, offset })
}

/// Parse UTF-8 JSON text. The byte length is checked against `max_source_units` first, so the
/// conversion to UTF-16 (at most one unit per byte) never exceeds the budget.
pub fn parse_utf8(heap: &mut Heap, src: &[u8], opts: &JsonOptions, lim: &JsonLimits) -> Res<Value> {
    if src.len() > lim.max_source_units {
        return fail(ErrorKind::Limit(LimitKind::Source), lim.max_source_units);
    }
    let s = std::str::from_utf8(src).map_err(|e| Error { kind: ErrorKind::Syntax(Syntax::InvalidUtf8), offset: e.valid_up_to() })?;
    validate_options(heap, opts)?;
    let units: Vec<u16> = s.encode_utf16().collect();
    parse(heap, &units, opts, lim)
}

/// Parse UTF-16 JSON text into a new value. See the module comment for budgets and for what a
/// failure leaves behind.
pub fn parse(heap: &mut Heap, src: &[u16], opts: &JsonOptions, lim: &JsonLimits) -> Res<Value> {
    if src.len() > lim.max_source_units {
        return fail(ErrorKind::Limit(LimitKind::Source), lim.max_source_units);
    }
    validate_options(heap, opts)?;
    let mut p = Parser {
        heap,
        src,
        pos: 0,
        opts: *opts,
        max_depth: lim.max_depth.min(HARD_MAX_DEPTH),
        max_nodes: lim.max_nodes,
        nodes: 0,
        text_left: lim.max_text_units,
        scratch: Vec::new(),
        num: Vec::new(),
    };
    p.run()
}

fn validate_options(heap: &Heap, opts: &JsonOptions) -> Res<()> {
    for p in [opts.object_proto, opts.array_proto].into_iter().flatten() {
        heap.get_prototype_of(p).map_err(|e| Error { kind: ErrorKind::Heap(e), offset: 0 })?;
    }
    Ok(())
}

struct Frame {
    obj: ObjId,
    array: bool,
    next: u32,
    key: PropertyKey,
}

struct Parser<'h, 's> {
    heap: &'h mut Heap,
    src: &'s [u16],
    pos: usize,
    opts: JsonOptions,
    max_depth: usize,
    max_nodes: usize,
    nodes: usize,
    text_left: usize,
    /// Reused buffer for strings that contain escapes. Grows only within the text budget.
    scratch: Vec<u16>,
    /// Reused ASCII buffer for numbers that need the general float parser.
    num: Vec<u8>,
}

fn is_digit(c: u16) -> bool {
    (0x30..=0x39).contains(&c)
}

impl Parser<'_, '_> {
    fn peek(&self) -> Option<u16> {
        self.src.get(self.pos).copied()
    }

    fn syntax<T>(&self, s: Syntax, at: usize) -> Res<T> {
        fail(ErrorKind::Syntax(s), at)
    }

    /// The error for "expected something here": end of input or a wrong unit.
    fn unexpected<T>(&self, at: usize) -> Res<T> {
        self.syntax(if at >= self.src.len() { Syntax::UnexpectedEnd } else { Syntax::UnexpectedChar }, at)
    }

    fn heap_err<T>(&self, e: HeapError, at: usize) -> Res<T> {
        fail(ErrorKind::Heap(e), at)
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(0x20 | 0x09 | 0x0A | 0x0D)) {
            self.pos += 1;
        }
    }

    fn node(&mut self) -> Res<()> {
        if self.nodes >= self.max_nodes {
            return fail(ErrorKind::Limit(LimitKind::Nodes), self.pos);
        }
        self.nodes += 1;
        Ok(())
    }

    fn charge_text(&mut self, n: usize, at: usize) -> Res<()> {
        if n > self.text_left {
            return fail(ErrorKind::Limit(LimitKind::Text), at);
        }
        self.text_left -= n;
        Ok(())
    }

    fn run(&mut self) -> Res<Value> {
        let mut stack: Vec<Frame> = Vec::new();
        'value: loop {
            self.ws();
            let Some(c) = self.peek() else { return self.syntax(Syntax::UnexpectedEnd, self.pos) };
            let mut v = match c {
                0x7B | 0x5B => {
                    let array = c == 0x5B;
                    self.node()?;
                    if stack.len() >= self.max_depth {
                        return fail(ErrorKind::Limit(LimitKind::Depth), self.pos);
                    }
                    let at = self.pos;
                    let made = if array { self.heap.new_array(self.opts.array_proto) } else { self.heap.new_object(self.opts.object_proto) };
                    let obj = match made {
                        Ok(o) => o,
                        Err(e) => return self.heap_err(e, at),
                    };
                    self.pos += 1;
                    self.ws();
                    let close = if array { 0x5D } else { 0x7D };
                    if self.peek() == Some(close) {
                        self.pos += 1;
                        Value::Object(obj)
                    } else {
                        stack.push(Frame { obj, array, next: 0, key: PropertyKey::Index(0) });
                        if !array {
                            self.key(stack.last_mut().unwrap())?;
                        }
                        continue 'value;
                    }
                }
                0x22 => {
                    self.node()?;
                    Value::String(self.string()?)
                }
                0x2D | 0x30..=0x39 => {
                    self.node()?;
                    Value::Number(self.number()?)
                }
                0x74 => {
                    self.node()?;
                    self.literal(b"true")?;
                    Value::Bool(true)
                }
                0x66 => {
                    self.node()?;
                    self.literal(b"false")?;
                    Value::Bool(false)
                }
                0x6E => {
                    self.node()?;
                    self.literal(b"null")?;
                    Value::Null
                }
                _ => return self.syntax(Syntax::UnexpectedChar, self.pos),
            };
            // `v` is complete: hand it to the open container, or finish.
            loop {
                let Some(f) = stack.last_mut() else {
                    self.ws();
                    if self.pos < self.src.len() {
                        return self.syntax(Syntax::TrailingData, self.pos);
                    }
                    return Ok(v);
                };
                let (key, array, obj) = if f.array {
                    let i = f.next;
                    f.next = i.wrapping_add(1);
                    (PropertyKey::Index(i), true, f.obj)
                } else {
                    (f.key, false, f.obj)
                };
                match self.heap.define_own_property(obj, key, &PropertyDescriptor::data(v)) {
                    Ok(true) => {}
                    Ok(false) => return self.heap_err(HeapError::InvalidDescriptor, self.pos),
                    Err(e) => return self.heap_err(e, self.pos),
                }
                self.ws();
                match self.peek() {
                    Some(0x2C) => {
                        self.pos += 1;
                        if !array {
                            self.ws();
                            self.key(stack.last_mut().unwrap())?;
                        }
                        continue 'value;
                    }
                    Some(c) if c == if array { 0x5D } else { 0x7D } => {
                        self.pos += 1;
                        stack.pop();
                        v = Value::Object(obj);
                    }
                    _ => return self.unexpected(self.pos),
                }
            }
        }
    }

    /// Parse `"key" :` and store the key in the frame.
    fn key(&mut self, f: &mut Frame) -> Res<()> {
        if self.peek() != Some(0x22) {
            return self.unexpected(self.pos);
        }
        let s = self.string()?;
        let k = match self.heap.key_from_string(s) {
            Ok(k) => k,
            Err(e) => return self.heap_err(e, self.pos),
        };
        self.ws();
        if self.peek() != Some(0x3A) {
            return self.unexpected(self.pos);
        }
        self.pos += 1;
        f.key = k;
        Ok(())
    }

    fn literal(&mut self, word: &[u8]) -> Res<()> {
        for &b in word {
            if self.peek() != Some(b as u16) {
                return self.unexpected(self.pos);
            }
            self.pos += 1;
        }
        Ok(())
    }

    fn intern(&mut self, from: Option<(usize, usize)>) -> Res<StrId> {
        let r = match from {
            Some((a, b)) => self.heap.intern_units(&self.src[a..b]),
            None => self.heap.intern_units(&self.scratch),
        };
        r.or_else(|e| self.heap_err(e, self.pos))
    }

    /// `self.pos` is at the opening quote.
    fn string(&mut self) -> Res<StrId> {
        let src = self.src;
        let start = self.pos + 1;
        let mut i = start;
        while i < src.len() && src[i] != 0x22 && src[i] != 0x5C && src[i] >= 0x20 {
            i += 1;
        }
        if i < src.len() && src[i] == 0x22 {
            self.charge_text(i - start, start)?;
            let s = self.intern(Some((start, i)));
            self.pos = i + 1;
            return s;
        }
        self.charge_text(i - start, start)?;
        self.scratch.clear();
        self.scratch.extend_from_slice(&src[start..i]);
        loop {
            let Some(&c) = src.get(i) else { return self.syntax(Syntax::UnexpectedEnd, src.len()) };
            match c {
                0x22 => break,
                0x00..=0x1F => return self.syntax(Syntax::ControlChar, i),
                0x5C => {
                    let Some(&e) = src.get(i + 1) else { return self.syntax(Syntax::UnexpectedEnd, src.len()) };
                    let u = match e {
                        0x22 | 0x5C | 0x2F => e,
                        0x62 => 8,
                        0x66 => 12,
                        0x6E => 10,
                        0x72 => 13,
                        0x74 => 9,
                        0x75 => {
                            let mut u = 0u16;
                            for k in 0..4 {
                                let Some(&h) = src.get(i + 2 + k) else { return self.syntax(Syntax::UnexpectedEnd, src.len()) };
                                let d = match h {
                                    0x30..=0x39 => h - 0x30,
                                    0x61..=0x66 => h - 0x61 + 10,
                                    0x41..=0x46 => h - 0x41 + 10,
                                    _ => return self.syntax(Syntax::BadEscape, i + 2 + k),
                                };
                                u = u << 4 | d;
                            }
                            self.charge_text(1, i)?;
                            self.scratch.push(u);
                            i += 6;
                            continue;
                        }
                        _ => return self.syntax(Syntax::BadEscape, i + 1),
                    };
                    self.charge_text(1, i)?;
                    self.scratch.push(u);
                    i += 2;
                }
                _ => {
                    let a = i;
                    while i < src.len() && src[i] != 0x22 && src[i] != 0x5C && src[i] >= 0x20 {
                        i += 1;
                    }
                    self.charge_text(i - a, a)?;
                    self.scratch.extend_from_slice(&src[a..i]);
                }
            }
        }
        let s = self.intern(None);
        self.pos = i + 1;
        s
    }

    /// `self.pos` is at `-` or a digit.
    fn number(&mut self) -> Res<f64> {
        let src = self.src;
        let start = self.pos;
        let mut i = start;
        let neg = src.get(i) == Some(&0x2D);
        i += neg as usize;
        let int_start = i;
        match src.get(i) {
            Some(0x30) => {
                i += 1;
                if src.get(i).is_some_and(|&c| is_digit(c)) {
                    return self.syntax(Syntax::BadNumber, i);
                }
            }
            Some(&c) if is_digit(c) => {
                while src.get(i).is_some_and(|&c| is_digit(c)) {
                    i += 1;
                }
            }
            _ => return self.number_err(i),
        }
        let int_end = i;
        let mut plain = true;
        if src.get(i) == Some(&0x2E) {
            plain = false;
            i += 1;
            i = self.digits(i)?;
        }
        if matches!(src.get(i), Some(0x65 | 0x45)) {
            plain = false;
            i += 1;
            if matches!(src.get(i), Some(0x2B | 0x2D)) {
                i += 1;
            }
            i = self.digits(i)?;
        }
        self.pos = i;
        if plain && int_end - int_start <= 15 {
            // At most 15 digits are exact in an f64: no general parser needed.
            let n = src[int_start..int_end].iter().fold(0u64, |n, &c| n * 10 + (c - 0x30) as u64) as f64;
            return Ok(if neg { -n } else { n });
        }
        self.num.clear();
        self.num.extend(src[start..i].iter().map(|&c| c as u8));
        let text = std::str::from_utf8(&self.num).expect("the number grammar is ASCII");
        Ok(number::decimal_value(text, &mut String::new()).expect("a grammar-checked number parses"))
    }

    /// One or more digits from `i`. Returns the index after them.
    fn digits(&self, mut i: usize) -> Res<usize> {
        if !self.src.get(i).is_some_and(|&c| is_digit(c)) {
            return self.number_err(i);
        }
        while self.src.get(i).is_some_and(|&c| is_digit(c)) {
            i += 1;
        }
        Ok(i)
    }

    fn number_err<T>(&self, at: usize) -> Res<T> {
        self.syntax(if at >= self.src.len() { Syntax::UnexpectedEnd } else { Syntax::BadNumber }, at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heap::{Get, Property};

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }
    fn heap() -> Heap {
        Heap::default()
    }
    fn p(h: &mut Heap, s: &str) -> Res<Value> {
        parse(h, &u(s), &JsonOptions::default(), &JsonLimits::default())
    }
    fn ok(s: &str) -> (Heap, Value) {
        let mut h = heap();
        let v = p(&mut h, s).unwrap_or_else(|e| panic!("{s:?}: {e:?}"));
        (h, v)
    }
    fn err(s: &str) -> Error {
        p(&mut heap(), s).expect_err(s)
    }
    fn syn(s: Syntax, offset: usize) -> Error {
        Error { kind: ErrorKind::Syntax(s), offset }
    }
    fn num(s: &str) -> f64 {
        match ok(s).1 {
            Value::Number(n) => n,
            v => panic!("{v:?}"),
        }
    }
    fn get(h: &mut Heap, o: Value, k: &str) -> Value {
        let Value::Object(o) = o else { panic!() };
        let k = h.key_from_str(k).unwrap();
        match h.get(o, k).unwrap() {
            Get::Value(v) => v,
            g => panic!("{g:?}"),
        }
    }
    fn keys(h: &Heap, o: Value) -> Vec<String> {
        let Value::Object(o) = o else { panic!() };
        h.own_keys(o)
            .unwrap()
            .into_iter()
            .map(|k| match k {
                PropertyKey::Index(i) => i.to_string(),
                PropertyKey::String(s) => h.to_string_lossy(s).unwrap(),
                PropertyKey::Symbol(_) => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn primitives_and_whitespace() {
        assert!(matches!(ok("null").1, Value::Null));
        assert!(matches!(ok(" \t\r\ntrue \n").1, Value::Bool(true)));
        assert!(matches!(ok("false").1, Value::Bool(false)));
        for bad in ["\u{a0}1", "\u{feff}1", "\u{2028}1", "\x0b1", "\x0c1"] {
            assert_eq!(err(bad).kind, ErrorKind::Syntax(Syntax::UnexpectedChar), "{bad:?}");
        }
    }

    #[test]
    fn number_values() {
        assert!(num("-0").is_sign_negative() && num("-0") == 0.0);
        assert!(num("0").is_sign_positive());
        assert!(num("-0.0e5").is_sign_negative());
        assert_eq!(num("1e400"), f64::INFINITY);
        assert_eq!(num("-1e400"), f64::NEG_INFINITY);
        assert_eq!(num("1e-400"), 0.0);
        assert!(num("-1e-400").is_sign_negative());
        assert_eq!(num("1E+2"), 100.0);
        assert_eq!(num("5e-324"), 5e-324);
        assert_eq!(num("2.4703282292062327e-324"), 0.0); // just below half the smallest subnormal
        assert_eq!(num("2.4703282292062328e-324"), 5e-324);
        assert_eq!(num("9007199254740993"), 9007199254740992.0); // ties to even
        assert_eq!(num("9007199254740995"), 9007199254740996.0);
        assert_eq!(num("1.7976931348623157e308"), f64::MAX);
        assert_eq!(num("1.7976931348623159e308"), f64::INFINITY);
        assert_eq!(num("0e99999999999999999999999"), 0.0);
        assert_eq!(num("1e99999999999999999999999"), f64::INFINITY);
        assert_eq!(num("1e-99999999999999999999999"), 0.0);
        assert_eq!(num(&format!("0.{}1e1000", "0".repeat(2000))), 0.0f64.max(num("1e-1001")));
        assert_eq!(num(&format!("1{}e-1000", "0".repeat(1000))), 1.0);
        assert_eq!(num("123456789012345"), 123456789012345.0);
        assert_eq!(num("1234567890123456789"), 1234567890123456800.0);
        assert_eq!(num("0.1"), 0.1);
    }

    /// Regression: Rust's `f64::from_str` returns infinity for a mantissa of about a million
    /// digits with a cancelling exponent (found by the load tool). Node gives the values below.
    #[test]
    fn million_digit_numbers() {
        let m = 1_000_000;
        assert_eq!(num(&format!("{}.5e-{m}", "9".repeat(m))), 1.0);
        assert_eq!(num(&format!("1{}e-{m}", "0".repeat(m))), 1.0);
        assert_eq!(num(&format!("0.{}1e{m}", "0".repeat(m))), 0.1);
        assert_eq!(num(&format!("-0.{}e{m}", "0".repeat(m))), 0.0);
        assert!(num(&format!("-0.{}e{m}", "0".repeat(m))).is_sign_negative());
        assert_eq!(num(&format!("1{}", "0".repeat(m))), f64::INFINITY);
        assert_eq!(num(&format!("0.{}1", "0".repeat(m))), 0.0);
        // The dropped tail still rounds: 2^53 + 1 plus a tiny positive tail rounds up.
        assert_eq!(num(&format!("9007199254740993.{}1", "0".repeat(2000))), 9007199254740994.0);
        assert_eq!(num(&format!("9007199254740993.{}", "0".repeat(2000))), 9007199254740992.0);
    }

    #[test]
    fn number_grammar_errors() {
        use Syntax::*;
        for (s, k, at) in [
            ("-", UnexpectedEnd, 1), ("-a", BadNumber, 1), ("01", BadNumber, 1), ("-01", BadNumber, 2),
            ("1.", UnexpectedEnd, 2), ("1.e1", BadNumber, 2), ("1e", UnexpectedEnd, 2), ("1e+", UnexpectedEnd, 3),
            ("1ex", BadNumber, 2), (".5", UnexpectedChar, 0), ("+1", UnexpectedChar, 0), ("0x1", TrailingData, 1),
            ("1_0", TrailingData, 1), ("Infinity", UnexpectedChar, 0), ("NaN", UnexpectedChar, 0), ("- 1", BadNumber, 1),
            ("[1,-]", BadNumber, 4), ("--1", BadNumber, 1),
        ] {
            assert_eq!(err(s), syn(k, at), "{s}");
        }
    }

    fn str_of(s: &str) -> Vec<u16> {
        let (h, v) = ok(s);
        let Value::String(id) = v else { panic!() };
        h.str_units(id).unwrap().to_vec()
    }

    #[test]
    fn strings_and_escapes() {
        assert_eq!(str_of(r#""a\"\\\/\b\f\n\r\t\u0041\u00e9\uD83D\uDE00""#), u("a\"\\/\u{8}\u{c}\n\r\tAé😀"));
        assert_eq!(str_of(r#""\uD800""#), vec![0xD800]);
        assert_eq!(str_of(r#""\uDC00\uD800""#), vec![0xDC00, 0xD800]);
        assert_eq!(str_of(r#""\ud83d""#), vec![0xD83D]);
        let mut raw = u("\"");
        raw.push(0xD800);
        raw.push(b'"' as u16);
        let mut h = heap();
        let v = parse(&mut h, &raw, &JsonOptions::default(), &JsonLimits::default()).unwrap();
        let Value::String(id) = v else { panic!() };
        assert_eq!(h.str_units(id).unwrap(), &[0xD800]);
        assert_eq!(str_of("\"\u{7f}\u{2028}\""), u("\u{7f}\u{2028}"));
        assert_eq!(str_of(r#""""#), vec![]);
        use Syntax::*;
        for (s, k, at) in [
            ("\"a\nb\"", ControlChar, 2), ("\"\u{1f}\"", ControlChar, 1), ("\"\\x\"", BadEscape, 2), ("\"\\u12\"", BadEscape, 5),
            ("\"\\u12G4\"", BadEscape, 5), ("\"\\U0041\"", BadEscape, 2), ("\"\\'\"", BadEscape, 2), ("\"abc", UnexpectedEnd, 4),
            ("\"abc\\", UnexpectedEnd, 5), ("\"\\u12", UnexpectedEnd, 5), ("'a'", UnexpectedChar, 0), ("\"a\\\nb\"", BadEscape, 3),
        ] {
            assert_eq!(err(s), syn(k, at), "{s:?}");
        }
    }

    #[test]
    fn structure_errors() {
        use Syntax::*;
        for (s, k, at) in [
            ("", UnexpectedEnd, 0), ("  ", UnexpectedEnd, 2), ("[", UnexpectedEnd, 1), ("[1", UnexpectedEnd, 2),
            ("[1,]", UnexpectedChar, 3), ("[,1]", UnexpectedChar, 1), ("[1 2]", UnexpectedChar, 3), ("{", UnexpectedEnd, 1),
            ("{,}", UnexpectedChar, 1), ("{\"a\"}", UnexpectedChar, 4), ("{\"a\":}", UnexpectedChar, 5), ("{\"a\":1,}", UnexpectedChar, 7),
            ("{a:1}", UnexpectedChar, 1), ("{1:1}", UnexpectedChar, 1), ("{'a':1}", UnexpectedChar, 1), ("[1}", UnexpectedChar, 2),
            ("{\"a\":1]", UnexpectedChar, 6), ("1 2", TrailingData, 2), ("[] x", TrailingData, 3), ("nul", UnexpectedEnd, 3),
            ("nulL", UnexpectedChar, 3), ("True", UnexpectedChar, 0), ("tru e", UnexpectedChar, 3), ("// c\n1", UnexpectedChar, 0),
            ("[1]/*c*/", TrailingData, 3), ("{\"a\":1 \"b\":2}", UnexpectedChar, 7), ("[]]", TrailingData, 2),
        ] {
            assert_eq!(err(s), syn(k, at), "{s:?}");
        }
    }

    #[test]
    fn objects_and_arrays() {
        let (mut h, v) = ok(r#"{"b":1,"a":2,"b":3,"2":"x","1":"y","__proto__":5,"":0}"#);
        assert_eq!(keys(&h, v), ["1", "2", "b", "a", "__proto__", ""]);
        assert!(matches!(get(&mut h, v, "b"), Value::Number(n) if n == 3.0));
        assert!(matches!(get(&mut h, v, "__proto__"), Value::Number(n) if n == 5.0));
        let Value::Object(o) = v else { panic!() };
        assert_eq!(h.get_prototype_of(o).unwrap(), None);
        let (mut h, v) = ok(r#"{"a":[1,[2,{"c":null}],[]],"d":{}}"#);
        let Value::Object(arr) = get(&mut h, v, "a") else { panic!() };
        assert!(h.is_array(arr).unwrap());
        let len = h.key_from_str("length").unwrap();
        assert!(matches!(h.get_own_property(arr, len).unwrap(), Some(Property::Data { value: Value::Number(n), .. }) if n == 3.0));
        let Value::Object(d) = get(&mut h, v, "d") else { panic!() };
        assert!(!h.is_array(d).unwrap());
        let (h, v) = ok("[1,2,3]");
        let Value::Object(a) = v else { panic!() };
        assert_eq!(h.own_keys(a).unwrap().len(), 4);
        let (h, v) = ok("[]");
        let Value::Object(a) = v else { panic!() };
        assert!(h.is_array(a).unwrap());
        // Own data property, enumerable/writable/configurable.
        let (mut h, v) = ok(r#"{"x":1}"#);
        let Value::Object(o) = v else { panic!() };
        let k = h.key_from_str("x").unwrap();
        assert!(matches!(h.get_own_property(o, k).unwrap(), Some(Property::Data { writable: true, enumerable: true, configurable: true, .. })));
        // A key that is an index on a plain object, and the largest non-index.
        let (h, v) = ok(r#"{"4294967295":1,"4294967294":2,"01":3}"#);
        assert_eq!(keys(&h, v), ["4294967294", "4294967295", "01"]);
    }

    #[test]
    fn prototype_options_and_no_setters() {
        let mut h = heap();
        let op = h.new_object(None).unwrap();
        // A setter on the prototype for "a" and "__proto__" must not run: the heap cannot run it,
        // but the property must still be an own data property.
        let ap = h.new_object(None).unwrap();
        let o = JsonOptions { object_proto: Some(op), array_proto: Some(ap) };
        let v = parse(&mut h, &u(r#"{"__proto__":1,"a":[]}"#), &o, &JsonLimits::default()).unwrap();
        let Value::Object(obj) = v else { panic!() };
        assert_eq!(h.get_prototype_of(obj).unwrap(), Some(op));
        let a = get(&mut h, v, "a");
        let Value::Object(a) = a else { panic!() };
        assert_eq!(h.get_prototype_of(a).unwrap(), Some(ap));
        let k = h.key_from_str("__proto__").unwrap();
        assert!(matches!(h.get_own_property(obj, k).unwrap(), Some(Property::Data { value: Value::Number(_), .. })));
        // A stale handle is refused before any allocation.
        let dead = h.new_object(None).unwrap();
        h.collect(&[Value::Object(op), Value::Object(ap)]).unwrap();
        let before = h.stats();
        let bad = JsonOptions { object_proto: Some(dead), array_proto: None };
        let e = parse(&mut h, &u("[1]"), &bad, &JsonLimits::default()).unwrap_err();
        assert_eq!(e, Error { kind: ErrorKind::Heap(HeapError::Stale), offset: 0 });
        let bad = JsonOptions { object_proto: None, array_proto: Some(dead) };
        assert!(parse(&mut h, &u("1"), &bad, &JsonLimits::default()).is_err());
        let after = h.stats();
        assert_eq!((before.total_object_allocs, before.total_string_allocs), (after.total_object_allocs, after.total_string_allocs));
    }

    fn lim(f: impl FnOnce(&mut JsonLimits)) -> JsonLimits {
        let mut l = JsonLimits::default();
        f(&mut l);
        l
    }
    fn run(h: &mut Heap, s: &str, l: &JsonLimits) -> Res<Value> {
        parse(h, &u(s), &JsonOptions::default(), l)
    }
    fn limit(k: LimitKind, offset: usize) -> Error {
        Error { kind: ErrorKind::Limit(k), offset }
    }

    #[test]
    fn budgets_fire_before_allocation() {
        let mut h = heap();
        let base = h.stats();
        // Source.
        assert_eq!(run(&mut h, "[1,2,3]", &lim(|l| l.max_source_units = 6)).unwrap_err(), limit(LimitKind::Source, 6));
        assert!(run(&mut h, "[1,2,3]", &lim(|l| l.max_source_units = 7)).is_ok());
        // Depth: `[[1]]` is depth 2; the object that would be the third open container is not made.
        let l = lim(|l| l.max_depth = 2);
        assert!(run(&mut h, "[[1]]", &l).is_ok());
        let b = h.stats().total_object_allocs;
        assert_eq!(run(&mut h, "[[[1]]]", &l).unwrap_err(), limit(LimitKind::Depth, 2));
        assert_eq!(h.stats().total_object_allocs, b + 2);
        assert_eq!(run(&mut h, "[[[", &lim(|l| l.max_depth = 0)).unwrap_err(), limit(LimitKind::Depth, 0));
        // Nodes: `[1,2]` is three nodes.
        assert!(run(&mut h, "[1,2]", &lim(|l| l.max_nodes = 3)).is_ok());
        assert_eq!(run(&mut h, "[1,2]", &lim(|l| l.max_nodes = 2)).unwrap_err(), limit(LimitKind::Nodes, 3));
        // A syntax error wins over a node limit at a non-value.
        assert_eq!(run(&mut h, "[1,]", &lim(|l| l.max_nodes = 2)).unwrap_err(), syn(Syntax::UnexpectedChar, 3));
        // Text: keys and string values, per occurrence, after decoding.
        let l = lim(|l| l.max_text_units = 4);
        assert!(run(&mut h, r#"{"ab":"\u0041\u0042"}"#, &l).is_ok());
        let before = h.stats().total_string_allocs;
        assert_eq!(run(&mut h, r#"{"ab":"\u0041\u0042c"}"#, &l).unwrap_err(), limit(LimitKind::Text, 19));
        assert_eq!(run(&mut h, r#"["abcde"]"#, &l).unwrap_err(), limit(LimitKind::Text, 2));
        assert_eq!(run(&mut h, r#"["ab","cd","e"]"#, &l).unwrap_err(), limit(LimitKind::Text, 12));
        assert!(h.stats().total_string_allocs <= before + 3);
        let _ = base;
    }

    #[test]
    fn heap_limits_are_reported_as_heap_errors() {
        let mut h = Heap::new(crate::heap::HeapLimits { max_objects: 3, ..Default::default() }).unwrap();
        let e = run(&mut h, "[[[[1]]]]", &JsonLimits::default()).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Heap(HeapError::Limit(crate::heap::Limit::Objects)));
        assert_eq!(e.offset, 3);
    }

    #[test]
    fn failed_parse_leaves_reclaimable_garbage_and_never_collects_roots() {
        let mut h = heap();
        let root_s = h.intern_str("keep").unwrap();
        let root_o = h.new_object(None).unwrap();
        let roots = [Value::String(root_s), Value::Object(root_o)];
        h.collect(&roots).unwrap();
        let base = h.stats();
        let e = run(&mut h, r#"{"a":[1,{"b":"str"}],"c":"d","e":"#, &JsonLimits::default()).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Syntax(Syntax::UnexpectedEnd));
        let mid = h.stats();
        assert_eq!(mid.collections, base.collections, "parse must not collect");
        assert!(mid.live_objects > base.live_objects && mid.live_strings > base.live_strings);
        assert!(h.str_units(root_s).is_ok() && h.get_prototype_of(root_o).is_ok());
        h.collect(&roots).unwrap();
        let end = h.stats();
        assert_eq!((end.live_objects, end.live_strings, end.bytes), (base.live_objects, base.live_strings, base.bytes));
        // A string the parse shares with a live root is not duplicated.
        let _ = run(&mut h, r#"["keep""#, &JsonLimits::default()).unwrap_err();
        assert_eq!(h.stats().live_strings, base.live_strings);
    }

    #[test]
    fn deep_nesting_is_iterative() {
        let n = 200_000;
        let big = lim(|l| l.max_depth = n + 1);
        let mut h = heap();
        let mut s = "[".repeat(n);
        s.push_str(&"]".repeat(n));
        assert!(run(&mut h, &s, &big).is_ok());
        let o = "{\"a\":".repeat(n) + "1" + &"}".repeat(n);
        assert!(run(&mut h, &o, &big).is_ok());
        // Default depth refuses it long before the heap fills.
        assert_eq!(run(&mut h, &s, &JsonLimits::default()).unwrap_err(), limit(LimitKind::Depth, 1024));
        // An unterminated flood is a depth error, and a hard cap holds even for usize::MAX.
        let flood = "[".repeat(HARD_MAX_DEPTH + 1);
        let mut h = heap();
        let e = run(&mut h, &flood, &lim(|l| l.max_depth = usize::MAX)).unwrap_err();
        assert_eq!(e, limit(LimitKind::Depth, HARD_MAX_DEPTH));
    }

    #[test]
    fn utf8_entry() {
        let mut h = heap();
        let l = JsonLimits::default();
        let o = JsonOptions::default();
        let v = parse_utf8(&mut h, "\"é😀\"".as_bytes(), &o, &l).unwrap();
        let Value::String(s) = v else { panic!() };
        assert_eq!(h.str_units(s).unwrap(), u("é😀").as_slice());
        assert_eq!(parse_utf8(&mut h, b"[\"a\xff\"]", &o, &l).unwrap_err(), syn(Syntax::InvalidUtf8, 3));
        assert_eq!(parse_utf8(&mut h, b"\"\xed\xa0\x80\"", &o, &l).unwrap_err(), syn(Syntax::InvalidUtf8, 1)); // encoded surrogate
        assert_eq!(parse_utf8(&mut h, b"\xef\xbb\xbf1", &o, &l).unwrap_err(), syn(Syntax::UnexpectedChar, 0)); // BOM
        assert_eq!(parse_utf8(&mut h, b"[1,2]", &o, &lim(|l| l.max_source_units = 4)).unwrap_err(), limit(LimitKind::Source, 4));
        assert_eq!(parse_utf8(&mut h, b"1 x", &o, &l).unwrap_err(), syn(Syntax::TrailingData, 2));
    }
}
