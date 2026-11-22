//! `String` and `String.prototype` (plan step S2.7). A string is read as a list of 16-bit units: a flat string is a slice, so a read does
//! not copy (a rope is flattened first, once). The methods that make a new string build a `Vec<u16>` and give it to [`Realm::make_units`],
//! which picks the narrow form when it can. `split` takes a string or a `RegExp`; `replace`, `match` and the others with a `RegExp` are in
//! `bi_string_regexp.rs`.
use crate::lazy::{Ent, m};
use crate::bi_array::rel_index;
use crate::builtins::arg;
use crate::interp_rt::{ErrKind, R, Realm};
use crate::string::{Chars, Str};
use crate::value::Value;

impl Realm {
    /// The units of a string.
    pub(crate) fn units(&mut self, s: Str) -> Vec<u16> {
        let s = self.flat(s);
        s.chars().map(|c| c.units().collect()).unwrap_or_default()
    }

    /// Add the units of `s` to `out`.
    pub(crate) fn push_units(&mut self, out: &mut Vec<u16>, s: Str) {
        let s = self.flat(s);
        if let Some(c) = s.chars() {
            out.extend(c.units());
        }
    }

    /// A new string with `u` as its units, in the narrowest form.
    pub(crate) fn make_units(&mut self, u: &[u16]) -> Value {
        Str::new(&mut self.heap, Chars::Two(u)).expect("memory").value()
    }

    /// `ToString(this)` of a string method: `undefined` and `null` throw.
    pub(crate) fn this_str(&mut self, this: Value, name: &str) -> R<Str> {
        if this.is_undefined() || this.is_null() {
            return self.type_error(&format!("String.prototype.{name} called on null or undefined"));
        }
        self.to_str(this)
    }

    pub(crate) fn install_string(&mut self) {
        let proto = self.string_proto;
        let c = self.ctor("String", 1, string_ctor, proto);
        static STRING_STATICS: &[Ent] = &[m("fromCharCode", 1, from_char_code)];
        self.lazy_table(c, STRING_STATICS);
        static STRING_PROTO: &[Ent] = &[
            m("replace", 2, crate::bi_string_regexp::replace),
            m("replaceAll", 2, crate::bi_string_regexp::replace_all),
            m("charAt", 1, char_at),
            m("charCodeAt", 1, char_code_at),
            m("codePointAt", 1, code_point_at),
            m("at", 1, at),
            m("indexOf", 1, index_of),
            m("lastIndexOf", 1, last_index_of),
            m("includes", 1, includes),
            m("startsWith", 1, starts_with),
            m("endsWith", 1, ends_with),
            m("slice", 2, slice),
            m("substring", 2, substring),
            m("substr", 2, substr),
            m("split", 2, split),
            m("toUpperCase", 0, to_upper),
            m("toLowerCase", 0, to_lower),
            m("trim", 0, trim),
            m("trimStart", 0, trim_start),
            m("trimEnd", 0, trim_end),
            m("concat", 1, concat),
            m("repeat", 1, repeat),
            m("padStart", 1, pad_start),
            m("padEnd", 1, pad_end),
            m("localeCompare", 1, locale_compare),
            m("toString", 0, to_string),
            m("valueOf", 0, to_string),
            crate::lazy::sym("[Symbol.iterator]", crate::bi_symbol::ITERATOR, crate::bi_string_iter::string_iterator),
        ];
        self.lazy_table(proto, STRING_PROTO);
        self.install_string_more();
        self.install_uri();
    }
}

fn string_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    // `String(symbol)` is the one conversion that accepts a symbol; `new String(symbol)` throws.
    if let Some(sym) = args.first().and_then(|a| Realm::sym_of(*a)).filter(|_| Realm::obj(rt.nt).is_none()) {
        return Ok(rt.symbol_string(sym));
    }
    let s = if args.is_empty() { rt.string("") } else { rt.to_str(args[0])?.value() };
    if Realm::obj(rt.nt).is_none() {
        return Ok(s);
    }
    let proto = rt.new_proto()?;
    Ok(rt.wrap(proto, s).value())
}

fn from_char_code(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let mut u = Vec::with_capacity(args.len());
    for &a in args {
        u.push(Realm::to_int32(rt.to_number(a)?) as u16);
    }
    Ok(rt.make_units(&u))
}

fn char_at(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let s = rt.this_str(this, "charAt")?;
    let i = rt.to_number(arg(args, 0))?;
    let i = if i.is_nan() { 0.0 } else { i.trunc() };
    let s = rt.flat(s);
    match if i >= 0.0 && i < s.len() as f64 { s.unit_at(i as usize) } else { None } {
        Some(u) => Ok(rt.make_units(&[u])),
        None => Ok(rt.string("")),
    }
}

fn char_code_at(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let s = rt.this_str(this, "charCodeAt")?;
    let i = rt.to_number(arg(args, 0))?;
    let i = if i.is_nan() { 0.0 } else { i.trunc() };
    let s = rt.flat(s);
    Ok(match if i >= 0.0 && i < s.len() as f64 { s.unit_at(i as usize) } else { None } {
        Some(u) => Value::int(u as i32),
        None => Value::number(f64::NAN),
    })
}

fn code_point_at(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let s = rt.this_str(this, "codePointAt")?;
    let i = rt.to_number(arg(args, 0))?;
    let i = if i.is_nan() { 0.0 } else { i.trunc() };
    if i < 0.0 || i >= s.len() as f64 {
        return Ok(Value::UNDEFINED);
    }
    let i = i as usize;
    let s = rt.flat(s);
    let point = match s.chars().expect("flat string") {
        Chars::One(c) => c[i] as i32,
        Chars::Two(c) => match char::decode_utf16(c[i..].iter().copied()).next() {
            Some(Ok(point)) => point as i32,
            _ => c[i] as i32,
        },
    };
    Ok(Value::int(point))
}

fn at(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let s = rt.this_str(this, "at")?;
    let i = rt.to_number(arg(args, 0))?;
    let i = if i.is_nan() { 0.0 } else { i.trunc() };
    let s = rt.flat(s);
    let k = if i < 0.0 { s.len() as f64 + i } else { i };
    match if k >= 0.0 && k < s.len() as f64 { s.unit_at(k as usize) } else { None } {
        Some(u) => Ok(rt.make_units(&[u])),
        None => Ok(Value::UNDEFINED),
    }
}

pub(crate) fn find(h: &[u16], n: &[u16], from: usize) -> Option<usize> {
    if n.is_empty() {
        return (from <= h.len()).then_some(from);
    }
    if n.len() > h.len() {
        return None;
    }
    (from..=h.len() - n.len()).find(|&i| h[i..i + n.len()] == *n)
}

pub(crate) fn pattern(rt: &mut Realm, v: Value) -> R<Vec<u16>> {
    let s = rt.to_str(v)?;
    Ok(rt.units(s))
}

fn index_of(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let s = rt.this_str(this, "indexOf")?;
    let n = pattern(rt, arg(args, 0))?;
    let h = rt.units(s);
    let from = rel_index(rt, arg(args, 1), 0.0, h.len() as f64, false)?;
    Ok(Value::int(find(&h, &n, from).map_or(-1, |i| i as i32)))
}

fn last_index_of(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let s = rt.this_str(this, "lastIndexOf")?;
    let n = pattern(rt, arg(args, 0))?;
    let h = rt.units(s);
    let p = rt.to_number(arg(args, 1))?;
    let start = if p.is_nan() { h.len() } else { p.max(0.0).min(h.len() as f64) as usize };
    if n.len() > h.len() {
        return Ok(Value::int(-1));
    }
    let last = start.min(h.len() - n.len());
    Ok(Value::int((0..=last).rev().find(|&i| h[i..i + n.len()] == *n).map_or(-1, |i| i as i32)))
}

fn includes(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let s = rt.this_str(this, "includes")?;
    let search = arg(args, 0);
    if rt.is_regexp(search)? {
        return rt.type_error("the search value is a regular expression");
    }
    let n = pattern(rt, search)?;
    let h = rt.units(s);
    let from = rel_index(rt, arg(args, 1), 0.0, h.len() as f64, false)?;
    Ok(Value::bool(find(&h, &n, from).is_some()))
}

fn starts_with(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let s = rt.this_str(this, "startsWith")?;
    let search = arg(args, 0);
    if rt.is_regexp(search)? {
        return rt.type_error("the search value is a regular expression");
    }
    let n = pattern(rt, search)?;
    let h = rt.units(s);
    let from = rel_index(rt, arg(args, 1), 0.0, h.len() as f64, false)?;
    Ok(Value::bool(h[from..].starts_with(&n)))
}

fn ends_with(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let s = rt.this_str(this, "endsWith")?;
    let search = arg(args, 0);
    if rt.is_regexp(search)? {
        return rt.type_error("the search value is a regular expression");
    }
    let n = pattern(rt, search)?;
    let h = rt.units(s);
    let end = if arg(args, 1).is_undefined() { h.len() } else { rel_index(rt, arg(args, 1), 0.0, h.len() as f64, false)? };
    Ok(Value::bool(h[..end].ends_with(&n)))
}

fn string_range(rt: &mut Realm, s: Str, start: usize, end: usize) -> Value {
    if start == 0 && end == s.len() {
        return s.value();
    }
    if start >= end {
        return rt.string("");
    }
    let s = rt.flat(s);
    rt.substring(s, start, end)
}

// Argument coercions can reenter JS. Realm::call_function holds the collector during reentry,
// so the string from this_str stays live until the range is copied.
fn slice(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let s = rt.this_str(this, "slice")?;
    let len = s.len() as f64;
    let a = rel_index(rt, arg(args, 0), 0.0, len, true)?;
    let b = rel_index(rt, arg(args, 1), len, len, true)?;
    Ok(string_range(rt, s, a, b))
}

fn substring(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let s = rt.this_str(this, "substring")?;
    let len = s.len() as f64;
    let a = rel_index(rt, arg(args, 0), 0.0, len, false)?;
    let b = rel_index(rt, arg(args, 1), len, len, false)?;
    let (a, b) = (a.min(b), a.max(b));
    Ok(string_range(rt, s, a, b))
}

fn substr(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let s = rt.this_str(this, "substr")?;
    let len = s.len() as f64;
    let a = rel_index(rt, arg(args, 0), 0.0, len, true)?;
    let n = if arg(args, 1).is_undefined() { len } else { rt.to_number(arg(args, 1))? };
    let n = if n.is_nan() { 0.0 } else { n.trunc() };
    let b = (a as f64 + n.max(0.0)).min(len) as usize;
    Ok(string_range(rt, s, a, b))
}

fn split(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (sep, limit) = (arg(args, 0), arg(args, 1));
    use crate::regexp_rt::protocol::{split_limit, string_hook};
    if Realm::obj(sep).is_some()
        && let Some(value) = string_hook(rt, this, sep, crate::bi_symbol::SPLIT, &[this, limit])?
    {
        return Ok(value);
    }
    let s = rt.this_str(this, "split")?;
    let u = rt.units(s);
    let limit = split_limit(rt, limit)?;
    let mut out: Vec<Value> = Vec::new();
    if sep.is_undefined() {
        out.push(s.value());
    } else {
        let p = pattern(rt, sep)?;
        if p.is_empty() {
            for &c in u.iter().take(limit as usize) {
                out.push(rt.make_units(&[c]));
            }
        } else {
            let mut at = 0;
            while let Some(i) = find(&u, &p, at) {
                out.push(rt.make_units(&u[at..i]));
                at = i + p.len();
            }
            out.push(rt.make_units(&u[at..]));
        }
    }
    out.truncate(limit as usize);
    Ok(rt.new_array(&out))
}

fn map_text(rt: &mut Realm, this: Value, name: &str, f: fn(&str) -> String) -> R<Value> {
    let s = rt.this_str(this, name)?;
    let u = rt.units(s);
    let t: Vec<u16> = f(&String::from_utf16_lossy(&u)).encode_utf16().collect();
    Ok(rt.make_units(&t))
}

fn to_upper(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    map_text(rt, this, "toUpperCase", |s| s.to_uppercase())
}

fn to_lower(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    map_text(rt, this, "toLowerCase", |s| s.to_lowercase())
}

/// ECMAScript WhiteSpace and LineTerminator code units.
pub(crate) fn is_space(c: u16) -> bool {
    matches!(c, 9..=13 | 0x20 | 0xA0 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF)
}

fn trim_with(rt: &mut Realm, this: Value, name: &str, start: bool, end: bool) -> R<Value> {
    let s = rt.this_str(this, name)?;
    let u = rt.units(s);
    let a = if start { u.iter().position(|&c| !is_space(c)).unwrap_or(u.len()) } else { 0 };
    let b = if end { u.iter().rposition(|&c| !is_space(c)).map_or(a, |i| i + 1) } else { u.len() };
    if a == 0 && b == u.len() {
        return Ok(s.value());
    }
    Ok(rt.make_units(&u[a..b.max(a)]))
}

fn trim(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    trim_with(rt, this, "trim", true, true)
}

fn trim_start(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    trim_with(rt, this, "trimStart", true, false)
}

fn trim_end(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    trim_with(rt, this, "trimEnd", false, true)
}

fn concat(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let mut acc = rt.this_str(this, "concat")?;
    for &a in args {
        let s = rt.to_str(a)?;
        acc = Str::concat(&mut rt.heap, acc, s).expect("memory");
    }
    Ok(acc.value())
}

fn repeat(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let s = rt.this_str(this, "repeat")?;
    let n = rt.to_number(arg(args, 0))?;
    let n = if n.is_nan() { 0.0 } else { n.trunc() };
    let u = rt.units(s);
    if n < 0.0 || n.is_infinite() || n * u.len() as f64 > crate::string::MAX_LEN as f64 {
        return Err(rt.error(ErrKind::Range, "invalid count value"));
    }
    let mut out = Vec::with_capacity(u.len() * n as usize);
    for _ in 0..n as usize {
        out.extend(&u);
    }
    Ok(rt.make_units(&out))
}

fn pad(rt: &mut Realm, this: Value, args: &[Value], start: bool) -> R<Value> {
    let s = rt.this_str(this, if start { "padStart" } else { "padEnd" })?;
    let n = rt.to_number(arg(args, 0))?;
    let target = if n.is_nan() { 0.0 } else { n.trunc() };
    let u = rt.units(s);
    let filler = if arg(args, 1).is_undefined() { vec![b' ' as u16] } else { pattern(rt, arg(args, 1))? };
    if target <= u.len() as f64 || filler.is_empty() {
        return Ok(s.value());
    }
    if target > crate::string::MAX_LEN as f64 {
        return Err(rt.error(ErrKind::Range, "invalid string length"));
    }
    let need = target as usize - u.len();
    let fill: Vec<u16> = filler.iter().copied().cycle().take(need).collect();
    let out = if start { [fill, u].concat() } else { [u, fill].concat() };
    Ok(rt.make_units(&out))
}

fn pad_start(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    pad(rt, this, args, true)
}

fn pad_end(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    pad(rt, this, args, false)
}

fn locale_compare(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let s = rt.this_str(this, "localeCompare")?;
    let t = pattern(rt, arg(args, 0))?;
    let u = rt.units(s);
    // Canonically equivalent strings are equal (`o` and U+0308 against U+00F6): compare the NFC forms. The order of other strings is
    // that of the units: js2 has no collation data yet.
    let (u, t) = (crate::bi_string_more::normalize_units(&u, true), crate::bi_string_more::normalize_units(&t, true));
    Ok(Value::int(match u.cmp(&t) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }))
}

fn to_string(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    match Realm::str_of(this).or_else(|| rt.prim_of(this).and_then(Realm::str_of)) {
        Some(s) => Ok(s.value()),
        None => rt.type_error("String.prototype.toString requires that 'this' be a String"),
    }
}

#[cfg(test)]
#[path = "bi_string_range_tests.rs"]
mod tests;
