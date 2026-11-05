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

/// `parseFloat` (section 19.2.4): the value of the longest StrDecimalLiteral prefix after leading white space, else NaN.
/// `is_space` is the white space test of `value::is_js_space`.
pub fn float_prefix(units: &[u16], is_space: impl Fn(u16) -> bool, scratch: &mut String) -> f64 {
    let t = &units[units.iter().position(|&u| !is_space(u)).unwrap_or(units.len())..];
    let at = |i: usize| t.get(i).map_or(0, |&u| if u < 128 { u as u8 } else { 0 });
    let neg = at(0) == b'-';
    let mut i = matches!(at(0), b'+' | b'-') as usize;
    let sign = if neg { -1.0 } else { 1.0 };
    if t[i..].iter().take(8).copied().eq("Infinity".encode_utf16()) {
        return sign * f64::INFINITY;
    }
    let start = i;
    let mut digits = 0;
    while at(i).is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if at(i) == b'.' {
        let dot = i;
        i += 1;
        while at(i).is_ascii_digit() {
            i += 1;
            digits += 1;
        }
        if digits == 0 {
            i = dot;
        }
    }
    if digits == 0 {
        return f64::NAN;
    }
    if matches!(at(i), b'e' | b'E') {
        let mut j = i + 1 + matches!(at(i + 1), b'+' | b'-') as usize;
        let e = j;
        while at(j).is_ascii_digit() {
            j += 1;
        }
        if j > e {
            i = j;
        }
    }
    let text: String = t[start..i].iter().map(|&u| u as u8 as char).collect();
    sign * decimal_value(&text, scratch).unwrap_or(f64::NAN)
}

/// The exact decimal value of finite `x > 0`: its significant digits (ASCII, no leading or trailing zeros) and the
/// power of ten of the first digit, so `x = d0.d1d2... * 10^exp`. A double has at most 767 significant digits.
pub fn exact_digits(x: f64) -> (Vec<u8>, i32) {
    let bits = x.to_bits();
    let (be, frac) = ((bits >> 52) as i32 & 0x7ff, bits & ((1 << 52) - 1));
    let (m, e) = if be == 0 { (frac, -1074) } else { (frac | 1 << 52, be - 1075) };
    // limbs of base 10^9, least significant first
    let mut n: Vec<u32> = vec![(m % 1_000_000_000) as u32, (m / 1_000_000_000 % 1_000_000_000) as u32, (m / 1_000_000_000_000_000_000) as u32];
    let mul = |n: &mut Vec<u32>, k: u32| {
        let mut carry = 0u64;
        for l in n.iter_mut() {
            let v = *l as u64 * k as u64 + carry;
            *l = (v % 1_000_000_000) as u32;
            carry = v / 1_000_000_000;
        }
        while carry > 0 {
            n.push((carry % 1_000_000_000) as u32);
            carry /= 1_000_000_000;
        }
    };
    // x = m * 2^e: for e < 0 it is m * 5^-e / 10^-e
    let (mut left, base, chunk, step) = if e >= 0 { (e, 2u32, 1u32 << 29, 29) } else { (-e, 5u32, 1_220_703_125u32, 13) };
    while left >= step {
        mul(&mut n, chunk);
        left -= step;
    }
    mul(&mut n, base.pow(left as u32));
    while n.len() > 1 && n[n.len() - 1] == 0 {
        n.pop();
    }
    let mut d = Vec::with_capacity(9 * n.len());
    for (i, &l) in n.iter().rev().enumerate() {
        let mut b = [b'0'; 9];
        let mut v = l;
        for slot in b.iter_mut().rev() {
            *slot = b'0' + (v % 10) as u8;
            v /= 10;
        }
        // the top limb has no leading zeros
        let skip = if i == 0 { b.iter().take(8).take_while(|&&c| c == b'0').count() } else { 0 };
        d.extend_from_slice(&b[skip..]);
    }
    let exp = d.len() as i32 - 1 + e.min(0);
    while d.len() > 1 && d[d.len() - 1] == b'0' {
        d.pop();
    }
    (d, exp)
}

/// `digits` rounded half up (ties away from zero) to `k >= 1` digits, padded with zeros. A carry out of the first
/// digit adds 1 to `exp`.
fn round_digits(mut d: Vec<u8>, mut exp: i32, k: usize) -> (Vec<u8>, i32) {
    if d.len() > k {
        let up = d[k] >= b'5';
        d.truncate(k);
        if up {
            let mut i = k;
            loop {
                if i == 0 {
                    d.insert(0, b'1');
                    d.truncate(k);
                    exp += 1;
                    break;
                }
                i -= 1;
                if d[i] == b'9' {
                    d[i] = b'0';
                } else {
                    d[i] += 1;
                    break;
                }
            }
        }
    }
    d.resize(k, b'0');
    (d, exp)
}

fn text(d: &[u8]) -> &str {
    std::str::from_utf8(d).expect("ASCII digits")
}

/// `Number.prototype.toFixed` for finite `x` with `|x| < 1e21` and `f` in 0..=100: round half up on the exact value.
pub fn to_fixed(x: f64, f: usize) -> String {
    let mut out = String::new();
    if x < 0.0 {
        out.push('-');
    }
    let n = if x == 0.0 {
        vec![b'0']
    } else {
        let (d, exp) = exact_digits(x.abs());
        let k = exp + 1 + f as i32; // digits of round(|x| * 10^f)
        if k < 0 || (k == 0 && d[0] < b'5') {
            vec![b'0']
        } else if k == 0 {
            vec![b'1']
        } else {
            let (r, e) = round_digits(d, exp, k as usize);
            let mut r = r;
            if e > exp {
                r.push(b'0'); // the carry made one more digit
            }
            r
        }
    };
    if f == 0 {
        out.push_str(text(&n));
        return out;
    }
    let mut m = n;
    if m.len() <= f {
        let mut z = vec![b'0'; f + 1 - m.len()];
        z.extend_from_slice(&m);
        m = z;
    }
    let (a, b) = m.split_at(m.len() - f);
    out.push_str(text(a));
    out.push('.');
    out.push_str(text(b));
    out
}

/// The significant digits and exponent of finite `x != 0` for `toExponential` (f = fraction digits, `None` for the
/// shortest round-trip digits) and `toPrecision` (p digits).
fn sci_digits(x: f64, k: Option<usize>) -> (Vec<u8>, i32) {
    match k {
        Some(k) => {
            let (d, exp) = exact_digits(x.abs());
            round_digits(d, exp, k)
        }
        None => {
            // the digits of Number::toString (Rust's `{:e}` breaks a tie between two shortest texts another way)
            let s = crate::value::number_to_string(x.abs());
            let (mant, e) = s.split_once('e').unwrap_or((&s, "0"));
            let point = mant.find('.').unwrap_or(mant.len()) as i32;
            let all: Vec<u8> = mant.bytes().filter(|&b| b != b'.').collect();
            let lead = all.iter().take_while(|&&b| b == b'0').count();
            let mut d = all[lead..].to_vec();
            while d.len() > 1 && d[d.len() - 1] == b'0' {
                d.pop();
            }
            (d, e.trim_start_matches('+').parse::<i32>().expect("exponent") + point - 1 - lead as i32)
        }
    }
}

fn exp_text(out: &mut String, d: &[u8], e: i32) {
    out.push(d[0] as char);
    if d.len() > 1 {
        out.push('.');
        out.push_str(text(&d[1..]));
    }
    out.push('e');
    out.push(if e < 0 { '-' } else { '+' });
    out.push_str(&e.unsigned_abs().to_string());
}

/// `Number.prototype.toExponential` for finite `x` and `f` (fraction digits, 0..=100, or `None` when undefined).
pub fn to_exponential(x: f64, f: Option<usize>) -> String {
    let mut out = String::new();
    if x < 0.0 {
        out.push('-');
    }
    let (d, e) = if x == 0.0 { (vec![b'0'; f.unwrap_or(0) + 1], 0) } else { sci_digits(x, f.map(|f| f + 1)) };
    exp_text(&mut out, &d, e);
    out
}

/// `Number.prototype.toPrecision` for finite `x` and `p` in 1..=100.
pub fn to_precision(x: f64, p: usize) -> String {
    let mut out = String::new();
    if x < 0.0 {
        out.push('-');
    }
    let (d, e) = if x == 0.0 { (vec![b'0'; p], 0) } else { sci_digits(x, Some(p)) };
    if e < -6 || e >= p as i32 {
        exp_text(&mut out, &d, e);
    } else if e == p as i32 - 1 {
        out.push_str(text(&d));
    } else if e >= 0 {
        out.push_str(text(&d[..e as usize + 1]));
        out.push('.');
        out.push_str(text(&d[e as usize + 1..]));
    } else {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', (-(e + 1)) as usize));
        out.push_str(text(&d));
    }
    out
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

    #[test]
    fn exact_digits_are_the_whole_value() {
        let t = |x: f64| {
            let (d, e) = exact_digits(x);
            (String::from_utf8(d).unwrap(), e)
        };
        assert_eq!(t(0.1), ("1000000000000000055511151231257827021181583404541015625".into(), -1));
        assert_eq!(t(1.0), ("1".into(), 0));
        assert_eq!(t(1e21), ("1".into(), 21));
        assert_eq!(t(2f64.powi(64)), ("18446744073709551616".into(), 19));
        let (d, e) = exact_digits(f64::from_bits(1)); // 5e-324: 751 significant digits
        assert_eq!((d.len(), e, &d[..5]), (751, -324, &b"49406"[..]));
        let (d, e) = exact_digits(f64::MAX);
        assert_eq!((d.len(), e, &d[..5]), (309, 308, &b"17976"[..]));
    }

    #[test]
    fn rounding_is_half_up_on_the_exact_value() {
        assert_eq!(round_digits(b"999".to_vec(), 2, 2), (b"10".to_vec(), 3));
        assert_eq!(round_digits(b"125".to_vec(), 0, 2), (b"13".to_vec(), 0));
        assert_eq!(round_digits(b"1".to_vec(), 0, 3), (b"100".to_vec(), 0));
        // 1.005 is 1.00499999999999989...: no tie, so it rounds down; 0.5 and 2.5 are exact ties and go up
        for (x, f, want) in [(1.005, 2, "1.00"), (0.5, 0, "1"), (2.5, 0, "3"), (-1.5, 0, "-2"), (0.000001, 7, "0.0000010"), (0.04, 1, "0.0"), (0.05, 1, "0.1"), (999.99, 1, "1000.0")] {
            assert_eq!(to_fixed(x, f), want, "{x}");
        }
        assert_eq!(to_precision(123.456, 2), "1.2e+2");
        assert_eq!(to_precision(0.000001234, 2), "0.0000012");
        assert_eq!(to_precision(1e-7, 1), "1e-7");
        assert_eq!(to_exponential(-8958.816528320312, None), "-8.958816528320312e+3");
        assert_eq!(to_exponential(0.0, Some(2)), "0.00e+0");
    }

    #[test]
    fn float_prefix_reads_the_longest_number() {
        let v = |t: &str| float_prefix(&t.encode_utf16().collect::<Vec<_>>(), |u| u == 32, &mut String::new());
        assert_eq!(v("  -1.5e3x"), -1500.0);
        assert_eq!(v("1e"), 1.0);
        assert_eq!(v("1e+"), 1.0);
        assert_eq!(v(".5"), 0.5);
        assert_eq!(v("5."), 5.0);
        assert_eq!(v("-Infinityx"), f64::NEG_INFINITY);
        assert!(v(".").is_nan() && v("e5").is_nan() && v("").is_nan() && v("infinity").is_nan());
        assert!(v("-0").is_sign_negative());
    }
}
