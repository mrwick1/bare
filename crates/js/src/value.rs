//! JavaScript values, handles and the pure conversions that need no heap and no execution.
//!
//! A `Value` is a small `Copy` enum (JS: a tagged union, like `typeof x` plus the data).
//! Strings, symbols and objects live in the heap (`heap.rs`). A `Value` holds only a typed
//! handle: an index into a slot table and a generation number. A handle is not a Rust
//! reference, so the heap can move, free and reuse storage without a dangling pointer.
//!
//! The heap makes every string unique by content (interning). So two `StrId` are equal
//! exactly when the two strings have the same UTF-16 code units. That is why `strict_equals`
//! needs no heap. Handles from two different heaps are not comparable (caller contract).

use crate::number;

/// Largest array index: 2^32 - 2. `2^32 - 1` is not an index; it is the largest length.
pub const MAX_INDEX: u32 = u32::MAX - 1;

macro_rules! handle {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        pub struct $name {
            pub(crate) index: u32,
            pub(crate) gn: u32,
        }
    };
}
handle!(/// Handle of a string (UTF-16 code units).
    StrId);
handle!(/// Handle of a symbol. Each `new_symbol` call makes a distinct identity.
    SymId);
handle!(/// Handle of an object.
    ObjId);
handle!(/// Handle of a BigInt too large for `i64`. Like strings, equal values share one handle (see `heap_big.rs`).
    BigId);

/// A JavaScript value. A BigInt is `BigInt` when it fits in `i64` (inline, nothing to collect) and `BigHeap` otherwise.
/// The two forms never overlap: `BigHeap` never holds a value that fits in `i64`, so equal BigInts have equal forms.
/// No `PartialEq`: `==` on `f64` is wrong for NaN and `Object.is`. Use the methods below.
#[derive(Clone, Copy, Debug)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    /// Stored as a plain `f64`: the NaN payload and the sign of zero stay as given.
    Number(f64),
    String(StrId),
    Symbol(SymId),
    Object(ObjId),
    BigInt(i64),
    BigHeap(BigId),
}

impl Value {
    /// Is this a BigInt of either form?
    pub fn is_bigint(self) -> bool {
        matches!(self, Value::BigInt(_) | Value::BigHeap(_))
    }

    /// `a === b`.
    pub fn strict_equals(self, other: Value) -> bool {
        use Value::*;
        match (self, other) {
            (Undefined, Undefined) | (Null, Null) => true,
            (Bool(a), Bool(b)) => a == b,
            (Number(a), Number(b)) => a == b, // NaN != NaN, +0 == -0
            (String(a), String(b)) => a == b,
            (Symbol(a), Symbol(b)) => a == b,
            (Object(a), Object(b)) => a == b,
            (BigInt(a), BigInt(b)) => a == b,
            (BigHeap(a), BigHeap(b)) => a == b, // the heap keeps one handle for each value
            _ => false,
        }
    }

    /// `Object.is(a, b)`: NaN is NaN, +0 is not -0.
    pub fn same_value(self, other: Value) -> bool {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => {
                (a.is_nan() && b.is_nan()) || (a == b && a.is_sign_negative() == b.is_sign_negative())
            }
            _ => self.strict_equals(other),
        }
    }

    /// SameValueZero (`Map`, `Set`, `includes`): NaN is NaN, +0 is -0.
    pub fn same_value_zero(self, other: Value) -> bool {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => (a.is_nan() && b.is_nan()) || a == b,
            _ => self.strict_equals(other),
        }
    }
}

/// A property key. Invariant: `String` never holds a canonical array index; that case is
/// always `Index`. `Heap::key_from_string` builds keys that keep this invariant, and the heap
/// rejects a key that breaks it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PropertyKey {
    Index(u32),
    String(StrId),
    Symbol(SymId),
}

/// The array index of `units` if it is canonical: `"0"`, or no leading zero, at most 2^32-2.
pub fn parse_index(units: &[u16]) -> Option<u32> {
    if units.is_empty() || units.len() > 10 {
        return None;
    }
    if units[0] == b'0' as u16 {
        return (units.len() == 1).then_some(0);
    }
    let mut n: u64 = 0;
    for &u in units {
        if !(b'0' as u16..=b'9' as u16).contains(&u) {
            return None;
        }
        n = n * 10 + (u - b'0' as u16) as u64;
    }
    (n <= MAX_INDEX as u64).then_some(n as u32)
}

/// ToUint32 (section 7.1.7): wrap modulo 2^32. NaN and infinity give 0.
pub fn to_uint32(n: f64) -> u32 {
    if !n.is_finite() {
        return 0;
    }
    // `rem_euclid` keeps the result in [0, 2^32) for negative numbers too.
    n.trunc().rem_euclid(4294967296.0) as u32
}

/// ToInt32 (section 7.1.6).
pub fn to_int32(n: f64) -> i32 {
    to_uint32(n) as i32
}

/// Number::toString(10) (section 6.1.6.1.20): the shortest digits that read back to `x`.
pub fn number_to_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x == 0.0 {
        return "0".into(); // also for -0
    }
    if x.is_infinite() {
        return if x < 0.0 { "-Infinity" } else { "Infinity" }.into();
    }
    // Rust's `{:e}` prints the shortest round-trip digits as `d.ddde-7`. Take digits and exponent.
    let mut sci = format!("{:e}", x.abs());
    // With 16 or 17 digits several decimals can read back to `x`. The spec wants the closest
    // to `x`; Rust's shortest output does not always pick it. The correctly rounded decimal of
    // the same length is the closest, so use it when it also reads back to `x`.
    let len = sci.split('e').next().unwrap().len() - sci.contains('.') as usize;
    if len >= 16 {
        let exact = format!("{:.*e}", len - 1, x.abs());
        if exact.parse::<f64>() == Ok(x.abs()) {
            sci = exact;
        }
    }
    let (mant, exp) = sci.split_once('e').unwrap();
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exp.parse::<i32>().unwrap() + 1; // value = 0.digits * 10^n
    let mut s = String::new();
    if x < 0.0 {
        s.push('-');
    }
    if k <= n && n <= 21 {
        s += &digits;
        s.extend(std::iter::repeat_n('0', (n - k) as usize));
    } else if 0 < n && n <= 21 {
        s += &digits[..n as usize];
        s.push('.');
        s += &digits[n as usize..];
    } else if -6 < n && n <= 0 {
        s += "0.";
        s.extend(std::iter::repeat_n('0', (-n) as usize));
        s += &digits;
    } else {
        s += &digits[..1];
        if k > 1 {
            s.push('.');
            s += &digits[1..];
        }
        s.push('e');
        s.push(if n - 1 < 0 { '-' } else { '+' });
        s += &(n - 1).abs().to_string();
    }
    s
}

/// ECMAScript WhiteSpace and LineTerminator code units.
pub fn is_js_space(u: u16) -> bool {
    matches!(u, 9..=13 | 0x20 | 0xA0 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF)
}

/// `parseInt` (section 19.2.5): the longest digit prefix of `units` in `radix` (ToInt32 of the argument). Else NaN.
pub fn parse_int(units: &[u16], radix: i32) -> f64 {
    let mut t = &units[units.iter().position(|&u| !is_js_space(u)).unwrap_or(units.len())..];
    let neg = t.first() == Some(&(b'-' as u16));
    if neg || t.first() == Some(&(b'+' as u16)) {
        t = &t[1..];
    }
    let mut r = radix;
    let mut strip = true;
    if r != 0 {
        if !(2..=36).contains(&r) {
            return f64::NAN;
        }
        strip = r == 16;
    } else {
        r = 10;
    }
    if strip && t.len() >= 2 && t[0] == b'0' as u16 && (t[1] == b'x' as u16 || t[1] == b'X' as u16) {
        t = &t[2..];
        r = 16;
    }
    let digit = |u: u16| char::from_u32(u as u32).and_then(|c| c.to_digit(36)).filter(|&d| (d as i32) < r);
    let n = t.iter().take_while(|&&u| digit(u).is_some()).count();
    if n == 0 {
        return f64::NAN;
    }
    let v = if r == 10 {
        let s: String = t[..n].iter().map(|&u| u as u8 as char).collect();
        s.parse::<f64>().unwrap_or(f64::NAN)
    } else if matches!(r, 2 | 4 | 8 | 16 | 32) {
        radix_digits(&t[..n], (r as u32).trailing_zeros())
    } else {
        t[..n].iter().fold(0.0, |a, &u| a * r as f64 + digit(u).unwrap() as f64)
    };
    if neg { -v } else { v }
}

/// The characters `encodeURI` keeps besides those of `encodeURIComponent` (uriReserved and `#`).
pub const URI_RESERVED: &str = ";/?:@&=+$,#";

/// `encodeURIComponent` (section 19.2.6.5), or `encodeURI` with `extra` = [`URI_RESERVED`]: percent-encode the UTF-8
/// bytes of everything but `A-Za-z0-9-_.!~*'()` and `extra`. `None` for a lone surrogate (URIError).
pub fn encode_uri_component(units: &[u16], extra: &str) -> Option<String> {
    let mut out = String::with_capacity(units.len());
    for c in char::decode_utf16(units.iter().copied()) {
        let c = c.ok()?;
        if c.is_ascii_alphanumeric() || "-_.!~*'()".contains(c) || extra.contains(c) {
            out.push(c);
        } else {
            for b in c.encode_utf8(&mut [0; 4]).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    Some(out)
}

/// Decode (section 19.2.6.1) for `decodeURIComponent` (`reserved` empty) and `decodeURI` ([`URI_RESERVED`]): each
/// `%XX` sequence is one UTF-8 character. An escape of a reserved ASCII character stays as it is. `None` for a bad
/// escape or bad UTF-8 (URIError).
pub fn decode_uri(units: &[u16], reserved: &str) -> Option<Vec<u16>> {
    let hex = |i: usize| -> Option<u8> {
        let h = |u: u16| char::from_u32(u as u32)?.to_digit(16);
        if *units.get(i)? != b'%' as u16 {
            return None;
        }
        Some((h(*units.get(i + 1)?)? * 16 + h(*units.get(i + 2)?)?) as u8)
    };
    let mut out = Vec::with_capacity(units.len());
    let mut k = 0;
    while k < units.len() {
        if units[k] != b'%' as u16 {
            out.push(units[k]);
            k += 1;
            continue;
        }
        let b = hex(k)?;
        if b < 0x80 {
            if reserved.contains(b as char) {
                out.extend_from_slice(&units[k..k + 3]);
            } else {
                out.push(b as u16);
            }
            k += 3;
            continue;
        }
        let n = b.leading_ones() as usize;
        if !(2..=4).contains(&n) {
            return None;
        }
        let mut bytes = [b, 0, 0, 0];
        for (j, slot) in bytes.iter_mut().enumerate().take(n).skip(1) {
            *slot = hex(k + 3 * j).filter(|c| c & 0xC0 == 0x80)?;
        }
        // from_utf8 refuses overlong forms, surrogates and values above U+10FFFF
        let c = std::str::from_utf8(&bytes[..n]).ok()?.chars().next()?;
        out.extend_from_slice(c.encode_utf16(&mut [0; 2]));
        k += 3 * n;
    }
    Some(out)
}

/// `escape` (section B.2.1.1): `%XX` below 256 and `%uXXXX` above, except `A-Za-z0-9@*_+-./`.
pub fn escape(units: &[u16]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(units.len());
    for &u in units {
        let _ = match escape_size(u) {
            1 => write!(out, "{}", u as u8 as char),
            3 => write!(out, "%{u:02X}"),
            _ => write!(out, "%u{u:04X}"),
        };
    }
    out
}

/// The length of `escape` output for one unit: 1 (kept), 3 (`%XX`) or 6 (`%uXXXX`).
pub fn escape_size(u: u16) -> usize {
    if u < 128 && ((u as u8).is_ascii_alphanumeric() || b"@*_+-./".contains(&(u as u8))) {
        1
    } else if u < 256 {
        3
    } else {
        6
    }
}

/// `unescape` (section B.2.1.2): `%uXXXX` and `%XX` become their unit. Any other `%` stays.
pub fn unescape(units: &[u16]) -> Vec<u16> {
    let h = |i: usize, n: usize| -> Option<u16> {
        let mut v = 0u16;
        for &u in units.get(i..i + n)? {
            v = v * 16 + char::from_u32(u as u32)?.to_digit(16)? as u16;
        }
        Some(v)
    };
    let mut out = Vec::with_capacity(units.len());
    let mut k = 0;
    while k < units.len() {
        let u = units[k];
        if u == b'%' as u16 {
            if units.get(k + 1) == Some(&(b'u' as u16)) {
                if let Some(v) = h(k + 2, 4) {
                    out.push(v);
                    k += 6;
                    continue;
                }
            } else if let Some(v) = h(k + 1, 2) {
                out.push(v);
                k += 3;
                continue;
            }
        }
        out.push(u);
        k += 1;
    }
    out
}

/// StringToNumber (section 7.1.4.1.1): `Number("...")`. Invalid text gives NaN.
pub fn string_to_number(units: &[u16]) -> f64 {
    let start = units.iter().position(|&u| !is_js_space(u)).unwrap_or(units.len());
    let end = units.iter().rposition(|&u| !is_js_space(u)).map_or(start, |p| p + 1);
    let t = &units[start..end];
    if t.is_empty() {
        return 0.0;
    }
    if t.len() > 2 && t[0] == b'0' as u16 {
        let bits = match t[1] as u8 as char {
            'x' | 'X' if t[1] < 128 => 4,
            'o' | 'O' if t[1] < 128 => 3,
            'b' | 'B' if t[1] < 128 => 1,
            _ => 0,
        };
        if bits != 0 {
            return radix_digits(&t[2..], bits);
        }
    }
    if t.len() > number::DIRECT_UNITS {
        return number::decimal_units_value(t, &mut String::new()).unwrap_or(f64::NAN);
    }
    // Decimal text is pure ASCII, so any other unit is NaN. Then check the grammar by hand;
    // Rust's parser also accepts "inf", "nan" and "infinity", which JS does not.
    if !t.iter().all(|&u| u < 128) {
        return f64::NAN;
    }
    let s: String = t.iter().map(|&u| u as u8 as char).collect();
    let body = s.strip_prefix(['+', '-']).unwrap_or(&s);
    if body == "Infinity" {
        return if s.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    let b = body.as_bytes();
    let mut i = 0;
    let int = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    i += int;
    let mut frac = 0;
    if b.get(i) == Some(&b'.') {
        frac = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
        i += 1 + frac;
    }
    if int + frac == 0 {
        return f64::NAN;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let e = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if e == 0 {
            return f64::NAN;
        }
        i += e;
    }
    if i != b.len() {
        return f64::NAN;
    }
    number::decimal_value(&s, &mut String::new()).unwrap_or(f64::NAN)
}

/// Digits of a `0x`/`0o`/`0b` literal with `bits` bits per digit. Rounds once, to nearest even:
/// keep the top 64 bits, OR a sticky bit for the dropped ones, then convert.
fn radix_digits(d: &[u16], bits: u32) -> f64 {
    let (mut m, mut dropped, mut sticky) = (0u64, 0u32, false);
    for &u in d {
        let v = match u {
            0x30..=0x39 => u - 0x30,
            0x61..=0x7a => u - 0x61 + 10,
            0x41..=0x5a => u - 0x41 + 10,
            _ => return f64::NAN,
        } as u64;
        if v >= 1 << bits {
            return f64::NAN;
        }
        if m >> (64 - bits) == 0 {
            m = (m << bits) | v;
        } else {
            dropped += bits;
            sticky |= v != 0;
        }
    }
    // `m as f64` rounds to nearest even; the sticky bit sits far below the 53 kept bits.
    let f = (m | sticky as u64) as f64;
    f * 2f64.powi(dropped.min(2000) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(v: f64) -> Value {
        Value::Number(v)
    }
    fn u16s(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn value_is_small_and_copy() {
        assert!(std::mem::size_of::<Value>() <= 16);
        let v = Value::Number(1.0);
        let w = v; // Copy: `v` stays usable
        assert!(v.strict_equals(w));
    }

    #[test]
    fn equality_numbers() {
        let nan = f64::NAN;
        assert!(!n(nan).strict_equals(n(nan)));
        assert!(n(nan).same_value(n(nan)));
        assert!(n(nan).same_value_zero(n(nan)));
        assert!(n(0.0).strict_equals(n(-0.0)));
        assert!(!n(0.0).same_value(n(-0.0)));
        assert!(n(0.0).same_value_zero(n(-0.0)));
        assert!(n(-0.0).same_value(n(-0.0)));
        // A NaN with another payload is still NaN.
        assert!(n(f64::from_bits(0x7ff8_0000_0000_1234)).same_value(n(nan)));
        assert!(n(f64::INFINITY).strict_equals(n(f64::INFINITY)));
    }

    #[test]
    fn equality_other_types() {
        let a = StrId { index: 1, gn: 0 };
        let b = StrId { index: 1, gn: 1 }; // same slot, newer generation: a different string
        assert!(Value::String(a).strict_equals(Value::String(a)));
        assert!(!Value::String(a).strict_equals(Value::String(b)));
        assert!(Value::Undefined.strict_equals(Value::Undefined));
        assert!(!Value::Undefined.strict_equals(Value::Null));
        assert!(!Value::Bool(false).strict_equals(n(0.0)));
        let s = SymId { index: 1, gn: 0 };
        let o = ObjId { index: 1, gn: 0 };
        assert!(!Value::Symbol(s).strict_equals(Value::Object(o)));
        assert!(Value::Object(o).same_value(Value::Object(o)));
    }

    #[test]
    fn index_parsing() {
        assert_eq!(parse_index(&u16s("0")), Some(0));
        assert_eq!(parse_index(&u16s("42")), Some(42));
        assert_eq!(parse_index(&u16s("4294967294")), Some(MAX_INDEX));
        for bad in ["", "01", "-1", "1.5", "4294967295", "99999999999", "1e3", " 1", "a"] {
            assert_eq!(parse_index(&u16s(bad)), None, "{bad}");
        }
    }

    #[test]
    fn int_conversions() {
        assert_eq!(to_uint32(-1.0), u32::MAX);
        assert_eq!(to_uint32(4294967296.0), 0);
        assert_eq!(to_uint32(4294967297.5), 1);
        assert_eq!(to_uint32(f64::NAN), 0);
        assert_eq!(to_uint32(-0.5), 0);
        assert_eq!(to_int32(2147483648.0), i32::MIN);
        assert_eq!(to_int32(-2147483649.0), i32::MAX);
    }

    #[test]
    fn number_strings() {
        for (x, s) in [
            (0.0, "0"), (-0.0, "0"), (1.0, "1"), (-1.5, "-1.5"), (0.1, "0.1"), (100.0, "100"),
            (1e21, "1e+21"), (1e20, "100000000000000000000"), (123456789012345680000.0, "123456789012345680000"),
            (1.5e-7, "1.5e-7"), (1e-6, "0.000001"), (1e-7, "1e-7"), (0.000123, "0.000123"),
            (f64::MAX, "1.7976931348623157e+308"), (5e-324, "5e-324"), (f64::NAN, "NaN"),
            (f64::INFINITY, "Infinity"), (f64::NEG_INFINITY, "-Infinity"), (12.34, "12.34"),
            (4294967295.0, "4294967295"),
            // Two 17-digit decimals read back to this value; JS prints the closer one (found by the Node oracle).
            (f64::from_bits(0xc3150a20d1948211), "-1480527645581444.2"), (0.30000000000000004, "0.30000000000000004"),
        ] {
            assert_eq!(number_to_string(x), s);
        }
    }

    #[test]
    fn string_numbers() {
        let p = |s: &str| string_to_number(&u16s(s));
        assert_eq!(p(""), 0.0);
        assert_eq!(p("  \n\t "), 0.0);
        assert_eq!(p(" 12 "), 12.0);
        assert_eq!(p("\u{feff}7\u{2028}"), 7.0);
        assert_eq!(p("1e3"), 1000.0);
        assert_eq!(p(".5"), 0.5);
        assert_eq!(p("5."), 5.0);
        assert_eq!(p("-.5e-1"), -0.05);
        assert!(p("-0").is_sign_negative() && p("-0") == 0.0);
        assert_eq!(p("Infinity"), f64::INFINITY);
        assert_eq!(p("-Infinity"), f64::NEG_INFINITY);
        assert_eq!(p("0x1F"), 31.0);
        assert_eq!(p("0b101"), 5.0);
        assert_eq!(p("0o17"), 15.0);
        for bad in ["inf", "nan", "infinity", "Infinit", "1_0", "0x", "-0x1", "0b2", "1e", "e1", ".", "+", "1 2", "0xg", "١"] {
            assert!(p(bad).is_nan(), "{bad}");
        }
        assert_eq!(p("0xFFFFFFFFFFFFFFFFFFFF"), 1208925819614629174706175.0);
        assert_eq!(p("0x20000000000001"), 9007199254740992.0); // 2^53+1 ties to even
        assert_eq!(p("0x20000000000003"), 9007199254740996.0);
        assert_eq!(p(&format!("0x1{}", "0".repeat(300))), f64::INFINITY);
        // A lone surrogate is not ASCII.
        assert!(string_to_number(&[0xD800]).is_nan());
    }

    /// Long decimal strings (over a million units): the float parser gives infinity for a
    /// cancelling exponent. Each expected value is the one Node gives for the same text.
    #[test]
    fn long_string_numbers() {
        let m = 1_000_000;
        let z = "0".repeat(m);
        let p = |s: &str| string_to_number(&u16s(s));
        assert_eq!(p(&format!("{}.5e-{m}", "9".repeat(m))), 1.0);
        assert_eq!(p(&format!(" -{}.5e-{m} \n", "9".repeat(m))), -1.0);
        assert_eq!(p(&format!("+{}.5e-{m}", "9".repeat(m))), 1.0);
        assert_eq!(p(&format!("0.{z}1e{m}")), 0.1);
        assert_eq!(p(&format!("1{z}e-{m}")), 1.0);
        assert!(p(&format!("-0.{z}e{m}")).is_sign_negative() && p(&format!("-0.{z}e{m}")) == 0.0);
        assert_eq!(p(&format!("1{z}e-{}", m - 300)), 1e300);
        assert_eq!(p(&format!("4{z}e-{}", m + 324)), f64::from_bits(1));
        assert_eq!(p(&format!("-4{z}e-{}", m + 324)), -f64::from_bits(1));
        assert_eq!(p(&format!("2{z}e-{}", m + 324)), 0.0);
        assert_eq!(p(&format!("1{z}e-{}", m - 309)), f64::INFINITY);
        assert_eq!(p(&format!("17976931348623157{z}e-{}", m - 292)), f64::MAX);
        // 2^1024 - 2^970 is exactly halfway between MAX and 2^1024: ties to even overflows.
        let h = "179769313486231580793728971405303415079934132710037826936173778980444968292764750946649017977587207096330286416692887910946555547851940402630657488671505820681908902000708383676273854845817711531764475730270069855571366959622842914819860834936475292719074168444365510704342711559699508093042880177904174497792";
        assert_eq!(p(&format!("{z}{h}")), f64::INFINITY);
        assert_eq!(p(&format!("{h}{}1", "0".repeat(2000))), f64::INFINITY);
        assert_eq!(p(&format!("{z}{}1", &h[..h.len() - 1])), f64::MAX);
        assert_eq!(p(&format!("{}1.{}", &h[..h.len() - 1], "9".repeat(100_000))), f64::MAX);
    }
}
