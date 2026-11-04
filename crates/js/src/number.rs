//! Decimal text to `f64`, shared by the lexer (`lex::number_value`), `Number(string)`
//! (`value::string_to_number`) and the JSON parser. Each caller checks its own grammar first.
//!
//! Rust's float parser is correct for ordinary text. For a token of about a million units with a
//! cancelling exponent it gives infinity, so a long token is first reduced to a short text with
//! the same rounding.

/// Tokens up to this many units go straight to the float parser.
pub const DIRECT_UNITS: usize = 1024;

/// Significant digits kept. Every halfway point between two f64 has at most 768 significant
/// digits (checked over all exponents with BigInt), so the kept digits plus a sticky digit decide
/// every rounding.
const KEEP_DIGITS: usize = 800;

/// The value of a decimal literal: an optional sign, digits with an optional `.`, and an optional
/// exponent. `text` has no separators. `scratch` holds the reduced text of a long token and keeps
/// its capacity for the next call. Gives `None` only when `text` is not a decimal literal.
#[inline]
pub fn decimal_value(text: &str, scratch: &mut String) -> Option<f64> {
    if text.len() <= DIRECT_UNITS {
        return text.parse().ok();
    }
    reduced_value(text.bytes(), scratch)
}

/// Long UTF-16 decimal input without a whole-token UTF-8 allocation.
pub fn decimal_units_value(units: &[u16], scratch: &mut String) -> Option<f64> {
    if units.iter().any(|&u| u > 127) { return None; }
    reduced_value(units.iter().map(|&u| u as u8), scratch)
}

/// Long lexer input whose separators were already checked by the lexer.
pub fn separated_decimal_value(text: &str, scratch: &mut String) -> Option<f64> {
    reduced_value(text.bytes().filter(|&b| b != b'_'), scratch)
}

#[cold]
fn reduced_value(bytes: impl Iterator<Item = u8> + Clone, scratch: &mut String) -> Option<f64> {
    if !decimal_syntax(bytes.clone()) { return None; }
    scratch.clear();
    reduce(bytes, scratch);
    scratch.parse().ok()
}

fn decimal_syntax(bytes: impl Iterator<Item = u8>) -> bool {
    let mut bytes = bytes.peekable();
    if matches!(bytes.peek(), Some(b'+' | b'-')) { bytes.next(); }
    let mut digits = false;
    while bytes.peek().is_some_and(u8::is_ascii_digit) { bytes.next(); digits = true; }
    if bytes.peek() == Some(&b'.') {
        bytes.next();
        while bytes.peek().is_some_and(u8::is_ascii_digit) { bytes.next(); digits = true; }
    }
    if !digits { return false; }
    if matches!(bytes.peek(), Some(b'e' | b'E')) {
        bytes.next();
        if matches!(bytes.peek(), Some(b'+' | b'-')) { bytes.next(); }
        let mut exponent_digits = false;
        while bytes.peek().is_some_and(u8::is_ascii_digit) { bytes.next(); exponent_digits = true; }
        if !exponent_digits { return false; }
    }
    bytes.next().is_none()
}

/// Keep a bounded significant prefix plus a sticky digit. Call only after grammar validation.
fn reduce(bytes: impl Iterator<Item = u8>, out: &mut String) {
    let mut bytes = bytes.peekable();
    let neg = bytes.peek() == Some(&b'-');
    if matches!(bytes.peek(), Some(b'+' | b'-')) { bytes.next(); }
    if neg { out.push('-'); }
    let (mut kept, mut seen, mut sticky, mut in_frac) = (0usize, false, false, false);
    let (mut frac, mut dropped, mut exp) = (0i64, 0i64, 0i64);
    while let Some(c) = bytes.next() {
        match c {
            b'.' => in_frac = true,
            b'e' | b'E' => {
                let eneg = bytes.peek() == Some(&b'-');
                if matches!(bytes.peek(), Some(b'+' | b'-')) { bytes.next(); }
                for d in bytes {
                    exp = (exp * 10 + (d - b'0') as i64).min(1 << 40);
                }
                if eneg {
                    exp = -exp;
                }
                break;
            }
            _ => {
                frac += in_frac as i64;
                seen |= c != b'0';
                if !seen {
                    continue; // leading zero
                }
                if kept < KEEP_DIGITS {
                    out.push(c as char);
                    kept += 1;
                } else {
                    dropped += 1;
                    sticky |= c != b'0';
                }
            }
        }
    }
    if !seen {
        out.push('0');
        return;
    }
    let mut e10 = exp - frac + dropped;
    if sticky {
        out.push('1');
        e10 -= 1;
    }
    // Below 1e-100000 (with at most 801 digits) the value is zero; above 1e100000 it is infinity.
    let e10 = e10.clamp(-100_000, 100_000);
    out.push_str(&format!("e{e10}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reduced text reads back to the same f64 as the float parser, for lengths just past the
    /// direct limit (the float parser is correct there; this checks the reduction, not Rust).
    #[test]
    fn reduction_keeps_the_value() {
        let mut seed = 99u64;
        let mut rnd = |n: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        for _ in 0..300 {
            let len = DIRECT_UNITS as u64 + 1 + rnd(400);
            let mut t = String::new();
            if rnd(2) == 0 {
                t.push('-');
            }
            for _ in 0..len {
                t.push((b'0' + rnd(10) as u8) as char);
            }
            let dot = 1 + rnd(len - 1) as usize;
            t.insert(dot + (t.starts_with('-') as usize), '.');
            t.push_str(&format!("e{}", rnd(1200) as i64 - 600));
            let mut scratch = String::new();
            let got = decimal_value(&t, &mut scratch).unwrap();
            let mut short = String::new();
            reduce(t.bytes(), &mut short);
            assert_eq!(got.to_bits(), short.parse::<f64>().unwrap().to_bits());
            assert_eq!(got.to_bits(), t.parse::<f64>().unwrap().to_bits(), "{t}");
        }
    }

    #[test]
    fn signs_and_zeros() {
        let zeros = "0".repeat(2000);
        let mut s = String::new();
        let neg = decimal_value(&format!("-{zeros}.{zeros}"), &mut s).unwrap();
        assert!(neg == 0.0 && neg.is_sign_negative());
        assert_eq!(decimal_value(&format!("+{zeros}5"), &mut s), Some(5.0));
        assert_eq!(decimal_value(&format!("{zeros}e99999999"), &mut s), Some(0.0));
        assert_eq!(decimal_value(&format!("1{zeros}e-99999999"), &mut s), Some(0.0));
        assert_eq!(decimal_value(&format!("4{zeros}e-{}", 2000 + 324), &mut s), Some(f64::from_bits(1)));
        assert_eq!(decimal_value("1e400", &mut s), Some(f64::INFINITY));
        assert_eq!(decimal_value("x", &mut s), None);
    }
}
