//! Large data literals kept as source text, not as a syntax tree (`ExprKind::Data`).
//!
//! An array or object literal that holds only JSON values costs one tree node (about 35 bytes) for each value. The parser checks such a
//! literal with [`scan`] and keeps its source span in one node. The evaluator builds the heap values straight from the text with [`build`].
//! V8 does the same for a literal of a function that runs once (`CreateObjectLiteral` makes boilerplates from constant data) and advises
//! `JSON.parse` for a large literal (<https://v8.dev/blog/cost-of-javascript-2019>). We copied the idea, not code.
//!
//! What counts as data is the JSON grammar, in script text: double-quoted strings with the JSON escapes, numbers without `+`, a leading `.`,
//! a leading zero, a separator or a base prefix, `true`, `false`, `null`, arrays and objects with string keys. JSON white space only
//! (no comment, no trailing comma, no hole, no spread, no sign with a space). Anything else makes [`scan`] give up, and the parser
//! builds the tree as before, so the syntax errors, the limits and the values of such a literal are the ones of the tree.
//!
//! The values are those of the tree walk: a literal `{"a":1}` and `JSON.parse` give the same properties in the same order. The one
//! difference of grammar is a key `__proto__`: in a literal it sets the prototype, in JSON it makes a property. [`scan`] gives up when a
//! key is `__proto__` (also when an escape spells it), so [`build`] never meets one.

use crate::heap::{Heap, HeapError, PropertyDescriptor};
use crate::value::{ObjId, Value};

/// A literal shorter than this many bytes stays a tree. Measured (docs/progress.md): the text path costs less than the tree from 64 bytes up.
/// 128 is the first size where one literal saves more than 2% of a 16 KiB script.
pub(crate) const MIN_BYTES: usize = 128;

/// Containers open at once. A deeper literal stays a tree, and the tree has its own depth limit.
const MAX_DEPTH: u32 = 64;

const PROTO: &[u8] = b"__proto__";

fn ws(b: &[u8], mut i: usize) -> usize {
    while matches!(b.get(i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        i += 1;
    }
    i
}

fn digits(b: &[u8], mut i: usize) -> Option<usize> {
    let from = i;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    (i > from).then_some(i)
}

/// The end of the number at `i`.
fn number(b: &[u8], mut i: usize) -> Option<usize> {
    if b.get(i) == Some(&b'-') {
        i += 1;
    }
    match b.get(i)? {
        b'0' => i += 1,
        b'1'..=b'9' => i = digits(b, i)?,
        _ => return None,
    }
    if b.get(i) == Some(&b'.') {
        i = digits(b, i + 1)?;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        i = digits(b, i)?;
    }
    Some(i)
}

/// The end of the string at `i` (the opening quote), and whether it holds an escape.
fn string(b: &[u8], i: usize) -> Option<(usize, bool)> {
    let mut i = i + 1;
    let mut esc = false;
    loop {
        match *b.get(i)? {
            b'"' => return Some((i + 1, esc)),
            b'\\' => {
                esc = true;
                match *b.get(i + 1)? {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => i += 2,
                    b'u' => {
                        if !b.get(i + 2..i + 6)?.iter().all(u8::is_ascii_hexdigit) {
                            return None;
                        }
                        i += 6;
                    }
                    _ => return None,
                }
            }
            0..=0x1F => return None,
            _ => i += 1,
        }
    }
}

/// Whether the key text `body` (between the quotes; `esc`: it holds an escape) spells `__proto__`.
fn is_proto(body: &str, esc: bool) -> bool {
    if !esc {
        return body.as_bytes() == PROTO;
    }
    let mut u = Vec::new();
    unescape(body, &mut u);
    u.iter().copied().eq(PROTO.iter().map(|&c| c as u16))
}

/// The end of `"key" :` at `i`, or `None` for a key `__proto__`.
fn key(b: &[u8], s: &str, i: usize) -> Option<usize> {
    if *b.get(i)? != b'"' {
        return None;
    }
    let (end, esc) = string(b, i)?;
    if end - i - 2 >= PROTO.len() && is_proto(&s[i + 1..end - 1], esc) {
        return None;
    }
    let i = ws(b, end);
    (*b.get(i)? == b':').then_some(i + 1)
}

/// The end of the data literal that starts at `at` (a `[` or a `{` of `src`), or `None` when the text is not data.
pub(crate) fn scan(src: &str, at: usize) -> Option<usize> {
    let b = src.as_bytes();
    let (mut i, mut depth, mut objs) = (at, 0u32, 0u64);
    loop {
        // A value is expected at `i`.
        i = ws(b, i);
        match *b.get(i)? {
            open @ (b'{' | b'[') => {
                if depth == MAX_DEPTH {
                    return None;
                }
                i = ws(b, i + 1);
                let close = if open == b'{' { b'}' } else { b']' };
                if b.get(i) == Some(&close) {
                    i += 1;
                } else {
                    depth += 1;
                    objs = objs << 1 | (open == b'{') as u64;
                    if open == b'{' {
                        i = ws(b, key(b, src, i)?);
                    }
                    continue;
                }
            }
            b'"' => i = string(b, i)?.0,
            b'-' | b'0'..=b'9' => i = number(b, i)?,
            b't' if b[i..].starts_with(b"true") => i += 4,
            b'f' if b[i..].starts_with(b"false") => i += 5,
            b'n' if b[i..].starts_with(b"null") => i += 4,
            _ => return None,
        }
        // A value ended at `i`: close containers, or go on to the next one.
        loop {
            if depth == 0 {
                return Some(i);
            }
            i = ws(b, i);
            let obj = objs & 1 == 1;
            match *b.get(i)? {
                b',' => {
                    i = ws(b, i + 1);
                    if obj {
                        i = key(b, src, i)?;
                    }
                    break;
                }
                b'}' if obj => {}
                b']' if !obj => {}
                _ => return None,
            }
            (i, depth, objs) = (i + 1, depth - 1, objs >> 1);
        }
    }
}

/// The units of a string body (between the quotes) that [`string`] accepted.
fn unescape(s: &str, out: &mut Vec<u16>) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let from = i;
        while i < b.len() && b[i] != b'\\' {
            i += 1;
        }
        out.extend(s[from..i].encode_utf16());
        if i == b.len() {
            return;
        }
        let u = match b[i + 1] {
            b'b' => 8,
            b'f' => 12,
            b'n' => 10,
            b'r' => 13,
            b't' => 9,
            b'u' => {
                let hex = std::str::from_utf8(&b[i + 2..i + 6]).unwrap_or("0");
                out.push(u16::from_str_radix(hex, 16).unwrap_or(0));
                i += 6;
                continue;
            }
            c => c as u16,
        };
        out.push(u);
        i += 2;
    }
}

struct Build<'a> {
    heap: &'a mut Heap,
    s: &'a str,
    b: &'a [u8],
    i: usize,
    object_proto: ObjId,
    array_proto: ObjId,
    /// The elements of the arrays that are open. An array takes its part when it closes.
    elems: Vec<Option<Value>>,
    units: Vec<u16>,
    long: String,
}

type R<T> = Result<T, HeapError>;

impl Build<'_> {
    fn bad<T>(&self) -> R<T> {
        // `scan` accepted this text, so the grammar holds. This is a bug, not an input.
        Err(HeapError::InvalidDescriptor)
    }

    fn string(&mut self) -> R<Value> {
        let start = self.i + 1;
        let rest = &self.b[start..];
        let n = rest.iter().position(|&c| c == b'"' || c == b'\\').unwrap_or(rest.len());
        if rest.get(n) == Some(&b'"') {
            self.i = start + n + 1;
            return Ok(Value::String(self.heap.intern_str(&self.s[start..start + n])?));
        }
        let (end, _) = match string(self.b, self.i) {
            Some(r) => r,
            None => return self.bad(),
        };
        self.units.clear();
        unescape(&self.s[start..end - 1], &mut self.units);
        self.i = end;
        Ok(Value::String(self.heap.intern_units(&self.units)?))
    }

    fn number(&mut self) -> R<f64> {
        let start = self.i;
        let Some(end) = number(self.b, start) else { return self.bad() };
        self.i = end;
        let neg = self.b[start] == b'-';
        let int = &self.b[start + neg as usize..end];
        // Up to 15 digits are exact in an f64, so no general parser is needed.
        if int.len() <= 15 && int.iter().all(u8::is_ascii_digit) {
            let n = int.iter().fold(0u64, |n, &c| n * 10 + (c - b'0') as u64) as f64;
            return Ok(if neg { -n } else { n });
        }
        match crate::number::decimal_value(&self.s[start..end], &mut self.long) {
            Some(n) => Ok(n),
            None => self.bad(),
        }
    }

    fn value(&mut self) -> R<Value> {
        self.i = ws(self.b, self.i);
        Ok(match self.b.get(self.i) {
            Some(b'"') => self.string()?,
            Some(b'{') => self.object()?,
            Some(b'[') => self.array()?,
            Some(b't') => {
                self.i += 4;
                Value::Bool(true)
            }
            Some(b'f') => {
                self.i += 5;
                Value::Bool(false)
            }
            Some(b'n') => {
                self.i += 4;
                Value::Null
            }
            _ => Value::Number(self.number()?),
        })
    }

    fn array(&mut self) -> R<Value> {
        let base = self.elems.len();
        self.i = ws(self.b, self.i + 1);
        if self.b.get(self.i) != Some(&b']') {
            loop {
                let v = self.value()?;
                self.elems.push(Some(v));
                self.i = ws(self.b, self.i);
                let c = self.b.get(self.i).copied();
                self.i += 1;
                match c {
                    Some(b',') => {}
                    Some(b']') => break,
                    _ => return self.bad(),
                }
            }
        } else {
            self.i += 1;
        }
        let a = self.heap.new_array(Some(self.array_proto))?;
        let items: Vec<Option<Value>> = self.elems.drain(base..).collect();
        let len = items.len() as u32;
        if !items.is_empty() && !self.heap.fill_array(a, items, len)? {
            return self.bad();
        }
        Ok(Value::Object(a))
    }

    fn object(&mut self) -> R<Value> {
        let o = self.heap.new_object(Some(self.object_proto))?;
        self.i = ws(self.b, self.i + 1);
        if self.b.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(Value::Object(o));
        }
        loop {
            self.i = ws(self.b, self.i);
            let Value::String(k) = self.string()? else { return self.bad() };
            let k = self.heap.key_from_string(k)?;
            self.i = ws(self.b, self.i) + 1; // the `:`
            let v = self.value()?;
            if !self.heap.define_own_property(o, k, &PropertyDescriptor::data(v))? {
                return self.bad();
            }
            self.i = ws(self.b, self.i);
            let c = self.b.get(self.i).copied();
            self.i += 1;
            match c {
                Some(b',') => {}
                Some(b'}') => return Ok(Value::Object(o)),
                _ => return self.bad(),
            }
        }
    }
}

/// The value of the data literal `text` (a span that [`scan`] accepted), with the prototypes of the realm. Nothing runs and the heap does
/// not collect, so a failure leaves only garbage behind.
pub(crate) fn build(heap: &mut Heap, object_proto: ObjId, array_proto: ObjId, text: &str) -> R<Value> {
    let mut b = Build { heap, s: text, b: text.as_bytes(), i: 0, object_proto, array_proto, elems: Vec::new(), units: Vec::new(), long: String::new() };
    b.value()
}

#[cfg(test)]
#[path = "data_lit_tests.rs"]
mod tests;
