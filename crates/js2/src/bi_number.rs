//! `Number`, `Boolean`, `Math` and the global number functions (plan step S2.7). `new Number(1)` and `new Boolean(true)` make a wrapper
//! object (`bi_wrap.rs`); a method of a primitive runs with the primitive as `this`, and the methods also accept a wrapper.
//! Known gaps: `toFixed` rounds the exact binary value half to even where the language rounds half up; `toString(radix)` of a fraction
//! gives at most 52 digits; `toPrecision`, `toExponential` and `Intl` are missing; a number prints with Rust's rules (`Realm::num_to_string`).
use crate::builtins::{NativeFn, arg};
use crate::interp_rt::{ErrKind, R, Realm};
use crate::value::Value;

impl Realm {
    pub(crate) fn install_number(&mut self) {
        let proto = self.number_proto;
        let c = self.ctor("Number", 1, number_ctor, proto);
        for (n, len, f) in [("isInteger", 1, is_integer as NativeFn), ("isFinite", 1, number_is_finite), ("isNaN", 1, number_is_nan), ("isSafeInteger", 1, is_safe_integer)] {
            self.method(c, n, len, f);
        }
        for (n, v) in [
            ("MAX_SAFE_INTEGER", 9007199254740991.0),
            ("MIN_SAFE_INTEGER", -9007199254740991.0),
            ("EPSILON", f64::EPSILON),
            ("MAX_VALUE", f64::MAX),
            ("MIN_VALUE", 5e-324),
            ("NaN", f64::NAN),
            ("POSITIVE_INFINITY", f64::INFINITY),
            ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
        ] {
            let k = self.intern(n);
            self.define_attrs(c, crate::interp_rt::Key::Name(k), 0, Value::number(v)).expect("define");
        }
        for (n, len, f) in [
            ("toString", 1, number_to_string as NativeFn),
            ("toFixed", 1, to_fixed),
            ("toExponential", 1, to_exponential),
            ("toPrecision", 1, to_precision),
            ("toLocaleString", 0, to_locale_string),
            ("valueOf", 0, number_value_of),
        ] {
            self.method(proto, n, len, f);
        }
        let b = self.boolean_proto;
        self.ctor("Boolean", 1, boolean_ctor, b);
        self.method(b, "toString", 0, boolean_to_string);
        self.method(b, "valueOf", 0, boolean_value_of);
        let g = self.global;
        for (n, len, f) in [("parseInt", 2, parse_int as NativeFn), ("parseFloat", 1, parse_float), ("isNaN", 1, global_is_nan), ("isFinite", 1, global_is_finite)] {
            self.method(g, n, len, f);
        }
        // `Number.parseInt` and `Number.parseFloat` are the same function objects as the globals.
        for n in ["parseFloat", "parseInt"] {
            let v = self.get_named(g.value(), n).expect("a global");
            self.hidden(c, n, v);
        }
        let m = Obj::new_plain(self);
        self.hidden(g, "Math", m.value());
        for (n, v) in [("PI", std::f64::consts::PI), ("E", std::f64::consts::E), ("LN2", std::f64::consts::LN_2), ("LN10", std::f64::consts::LN_10), ("LOG2E", std::f64::consts::LOG2_E), ("LOG10E", std::f64::consts::LOG10_E), ("SQRT2", std::f64::consts::SQRT_2), ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2)] {
            let k = self.intern(n);
            self.define_attrs(m, crate::interp_rt::Key::Name(k), 0, Value::number(v)).expect("define");
        }
        for (n, len, f) in [
            ("floor", 1, floor as NativeFn),
            ("ceil", 1, ceil),
            ("round", 1, round),
            ("trunc", 1, trunc),
            ("abs", 1, abs),
            ("sign", 1, sign),
            ("sqrt", 1, sqrt),
            ("cbrt", 1, cbrt),
            ("exp", 1, exp),
            ("log", 1, log),
            ("log2", 1, log2),
            ("log10", 1, log10),
            ("sin", 1, sin),
            ("cos", 1, cos),
            ("tan", 1, tan),
            ("asin", 1, asin),
            ("acos", 1, acos),
            ("atan", 1, atan),
            ("atan2", 2, atan2),
            ("pow", 2, pow),
            ("max", 2, max),
            ("min", 2, min),
            ("hypot", 2, hypot),
            ("imul", 2, imul),
            ("fround", 1, fround),
            ("clz32", 1, clz32),
            ("sinh", 1, sinh),
            ("cosh", 1, cosh),
            ("tanh", 1, tanh),
            ("asinh", 1, asinh),
            ("acosh", 1, acosh),
            ("atanh", 1, atanh),
            ("expm1", 1, expm1),
            ("log1p", 1, log1p),
            ("random", 0, random),
        ] {
            self.method(m, n, len, f);
        }
    }
}

use crate::object::Obj;

impl Obj {
    /// A new plain object (`{}`) of the realm.
    pub(crate) fn new_plain(rt: &mut Realm) -> Obj {
        Obj::new(&mut rt.heap, &rt.shapes, rt.s_object).expect("memory")
    }
}

fn num_arg(rt: &mut Realm, args: &[Value], i: usize) -> R<f64> {
    rt.to_number(arg(args, i))
}

fn number_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let n = if args.is_empty() { Value::int(0) } else { Realm::num(rt.to_number(args[0])?) };
    if Realm::obj(rt.nt).is_none() {
        return Ok(n);
    }
    let proto = rt.new_proto()?;
    Ok(rt.wrap(proto, n).value())
}

fn boolean_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let b = Value::bool(Realm::truthy(arg(args, 0)));
    if Realm::obj(rt.nt).is_none() {
        return Ok(b);
    }
    let proto = rt.new_proto()?;
    Ok(rt.wrap(proto, b).value())
}

/// The boolean of `this`: a boolean, or a `Boolean` wrapper.
fn this_bool(rt: &mut Realm, this: Value, what: &str) -> R<bool> {
    match this.as_bool().or_else(|| rt.prim_of(this).and_then(Value::as_bool)) {
        Some(b) => Ok(b),
        None => rt.type_error(&format!("Boolean.prototype.{what} requires that 'this' be a Boolean")),
    }
}

fn boolean_to_string(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let b = this_bool(rt, this, "toString")?;
    Ok(rt.string(if b { "true" } else { "false" }))
}

fn boolean_value_of(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    Ok(Value::bool(this_bool(rt, this, "valueOf")?))
}

/// The number of `this`: a number, or a `Number` wrapper.
fn this_num(rt: &mut Realm, this: Value, what: &str) -> R<f64> {
    match this.as_f64().or_else(|| rt.prim_of(this).and_then(Value::as_f64)) {
        Some(f) => Ok(f),
        None => rt.type_error(&format!("Number.prototype.{what} requires that 'this' be a Number")),
    }
}

fn number_value_of(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let x = this_num(rt, this, "valueOf")?;
    Ok(Realm::num(x))
}

const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

fn radix_string(x: f64, radix: u32) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    let (neg, x) = (x < 0.0, x.abs());
    let (mut int, mut frac) = (x.trunc(), x.fract());
    let mut digits: Vec<u8> = Vec::new();
    if int == 0.0 {
        digits.push(b'0');
    }
    while int >= 1.0 {
        digits.push(DIGITS[(int % radix as f64) as usize]);
        int = (int / radix as f64).trunc();
    }
    digits.reverse();
    if frac > 0.0 {
        digits.push(b'.');
        for _ in 0..52 {
            frac *= radix as f64;
            let d = frac.trunc();
            digits.push(DIGITS[d as usize]);
            frac -= d;
            if frac == 0.0 {
                break;
            }
        }
    }
    let s = String::from_utf8(digits).expect("ascii");
    if neg { format!("-{s}") } else { s }
}

fn number_to_string(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let x = this_num(rt, this, "toString")?;
    let r = arg(args, 0);
    let radix = if r.is_undefined() { 10.0 } else { rt.to_number(r)?.trunc() };
    if !(2.0..=36.0).contains(&radix) {
        return Err(rt.error(ErrKind::Range, "toString() radix must be between 2 and 36"));
    }
    let t = if radix == 10.0 { Realm::num_to_string(x) } else { radix_string(x, radix as u32) };
    Ok(rt.string(&t))
}

fn to_fixed(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let x = this_num(rt, this, "toFixed")?;
    let d = rt.to_number(arg(args, 0))?;
    let d = if d.is_nan() { 0.0 } else { d.trunc() };
    if !(0.0..=100.0).contains(&d) {
        return Err(rt.error(ErrKind::Range, "toFixed() digits argument must be between 0 and 100"));
    }
    let t = if x.is_finite() && x.abs() < 1e21 { format!("{:.*}", d as usize, x) } else { Realm::num_to_string(x) };
    // `-0.00` is `0.00` for the language.
    let t = if t.starts_with('-') && t[1..].bytes().all(|b| b == b'0' || b == b'.') { t[1..].to_string() } else { t };
    Ok(rt.string(&t))
}

/// `ToIntegerOrInfinity` of the digits argument of `toFixed`, `toExponential` and `toPrecision`.
fn digits_arg(rt: &mut Realm, v: Value) -> R<f64> {
    let d = rt.to_number(v)?;
    Ok(if d.is_nan() { 0.0 } else { d.trunc() })
}

/// The digits and the decimal exponent of `x` (finite, above 0) from Rust's `{:e}` text: `1.2345e-7` is `("12345", -7)`.
fn sci_digits(t: &str) -> (String, i32) {
    let (m, e) = t.split_once('e').expect("an exponent");
    (m.replace('.', ""), e.parse().expect("an exponent number"))
}

/// The `p` significant digits of `a` (finite, above 0) and the decimal exponent, rounded half up on the exact binary value. Rust's
/// `{:.N e}` rounds an exact tie to even where the language takes the larger digit (`(25).toExponential(0)` is `3e+1`), so the exact
/// expansion (at most 767 significant digits) is cut here.
fn round_sig(a: f64, p: usize) -> (String, i32) {
    let (mut d, mut e) = sci_digits(&format!("{:.*e}", 800, a));
    let up = d.as_bytes().get(p).is_some_and(|&c| c >= b'5');
    d.truncate(p);
    if up {
        let mut b = d.into_bytes();
        let mut i = b.len();
        while i > 0 && b[i - 1] == b'9' {
            b[i - 1] = b'0';
            i -= 1;
        }
        if i > 0 {
            b[i - 1] += 1;
        } else {
            // 99 + 1: the digits are `10` and the exponent grows.
            b[0] = b'1';
            e += 1;
        }
        d = String::from_utf8(b).expect("ascii");
    }
    (d, e)
}

/// `d.ddd` and `e+N` or `e-N`.
fn exp_text(digits: &str, e: i32) -> String {
    let m = if digits.len() > 1 { format!("{}.{}", &digits[..1], &digits[1..]) } else { digits.to_string() };
    format!("{m}e{}{}", if e < 0 { '-' } else { '+' }, e.abs())
}

fn to_exponential(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let x = this_num(rt, this, "toExponential")?;
    let f = if arg(args, 0).is_undefined() { None } else { Some(digits_arg(rt, args[0])?) };
    if !x.is_finite() {
        return Ok(rt.string(&Realm::num_to_string(x)));
    }
    if let Some(f) = f
        && !(0.0..=100.0).contains(&f)
    {
        return Err(rt.error(ErrKind::Range, "toExponential() argument must be between 0 and 100"));
    }
    let (neg, a) = (x < 0.0, x.abs());
    let (digits, e) = match f {
        _ if a == 0.0 => ("0".repeat(f.map_or(0, |f| f as usize) + 1), 0),
        Some(f) => round_sig(a, f as usize + 1),
        None => sci_digits(&format!("{a:e}")),
    };
    let t = exp_text(&digits, e);
    Ok(rt.string(&if neg { format!("-{t}") } else { t }))
}

fn to_precision(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let x = this_num(rt, this, "toPrecision")?;
    if arg(args, 0).is_undefined() {
        return Ok(rt.string(&Realm::num_to_string(x)));
    }
    let p = digits_arg(rt, args[0])?;
    if !x.is_finite() {
        return Ok(rt.string(&Realm::num_to_string(x)));
    }
    if !(1.0..=100.0).contains(&p) {
        return Err(rt.error(ErrKind::Range, "toPrecision() argument must be between 1 and 100"));
    }
    let p = p as usize;
    let (neg, a) = (x < 0.0, x.abs());
    let t = if a == 0.0 {
        if p == 1 { "0".to_string() } else { format!("0.{}", "0".repeat(p - 1)) }
    } else {
        let (d, e) = round_sig(a, p);
        if e < -6 || e >= p as i32 {
            exp_text(&d, e)
        } else if e >= 0 {
            let k = e as usize + 1;
            if k == d.len() { d } else { format!("{}.{}", &d[..k], &d[k..]) }
        } else {
            format!("0.{}{d}", "0".repeat((-e - 1) as usize))
        }
    };
    Ok(rt.string(&if neg { format!("-{t}") } else { t }))
}

/// `toLocaleString` for the locale `en-US`, with no `Intl`: at most 3 fraction digits and commas between groups of 3 integer digits.
fn to_locale_string(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let x = this_num(rt, this, "toLocaleString")?;
    let t = if x.is_nan() {
        "NaN".to_string()
    } else if x.is_infinite() {
        if x > 0.0 { "∞".to_string() } else { "-∞".to_string() }
    } else {
        let f = format!("{:.3}", x.abs());
        let f = f.trim_end_matches('0').trim_end_matches('.');
        let (int, frac) = f.split_once('.').unwrap_or((f, ""));
        let mut g = String::new();
        for (i, c) in int.chars().enumerate() {
            if i > 0 && (int.len() - i) % 3 == 0 {
                g.push(',');
            }
            g.push(c);
        }
        format!("{}{g}{}{frac}", if x.is_sign_negative() { "-" } else { "" }, if frac.is_empty() { "" } else { "." })
    };
    Ok(rt.string(&t))
}

fn is_integer(_rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    Ok(Value::bool(arg(args, 0).as_f64().is_some_and(|f| f.is_finite() && f.fract() == 0.0)))
}

fn is_safe_integer(_rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    Ok(Value::bool(arg(args, 0).as_f64().is_some_and(|f| f.is_finite() && f.fract() == 0.0 && f.abs() <= 9007199254740991.0)))
}

fn number_is_finite(_rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    Ok(Value::bool(arg(args, 0).as_f64().is_some_and(f64::is_finite)))
}

fn number_is_nan(_rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    Ok(Value::bool(arg(args, 0).as_f64().is_some_and(f64::is_nan)))
}

fn global_is_nan(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    Ok(Value::bool(num_arg(rt, args, 0)?.is_nan()))
}

fn global_is_finite(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    Ok(Value::bool(num_arg(rt, args, 0)?.is_finite()))
}

fn trimmed(rt: &mut Realm, v: Value) -> R<String> {
    let s = rt.to_str(v)?;
    let t = rt.text(s);
    Ok(t.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}').to_string())
}

fn parse_int(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let t = trimmed(rt, arg(args, 0))?;
    let mut radix = Realm::to_int32(rt.to_number(arg(args, 1))?) as u32;
    let (neg, mut t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(&t)),
    };
    if (radix == 0 || radix == 16) && (t.starts_with("0x") || t.starts_with("0X")) {
        t = &t[2..];
        radix = 16;
    }
    if radix == 0 {
        radix = 10;
    }
    if !(2..=36).contains(&radix) {
        return Ok(Value::number(f64::NAN));
    }
    let mut n = 0.0f64;
    let mut any = false;
    for c in t.chars() {
        let Some(d) = c.to_digit(radix) else { break };
        n = n * radix as f64 + d as f64;
        any = true;
    }
    if !any {
        return Ok(Value::number(f64::NAN));
    }
    Ok(Realm::num(if neg { -n } else { n }))
}

fn parse_float(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let t = trimmed(rt, arg(args, 0))?;
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    for w in ["Infinity"] {
        if t[i..].starts_with(w) {
            return Ok(Value::number(if t.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY }));
        }
    }
    let digits = |i: &mut usize| {
        let s = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i > s
    };
    let int = digits(&mut i);
    let mut frac = false;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        frac = digits(&mut i);
    }
    if !int && !frac {
        return Ok(Value::number(f64::NAN));
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if digits(&mut j) {
            i = j;
        }
    }
    Ok(Realm::num(t[..i].parse().unwrap_or(f64::NAN)))
}

macro_rules! unary {
    ($name:ident, |$x:ident| $e:expr) => {
        fn $name(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
            let $x = num_arg(rt, args, 0)?;
            Ok(Realm::num($e))
        }
    };
}

unary!(floor, |x| x.floor());
unary!(ceil, |x| x.ceil());
unary!(round, |x| js_round(x));
unary!(trunc, |x| x.trunc());
unary!(abs, |x| x.abs());
unary!(sign, |x| if x.is_nan() || x == 0.0 { x } else { x.signum() });
unary!(sqrt, |x| x.sqrt());
unary!(cbrt, |x| x.cbrt());
unary!(exp, |x| x.exp());
unary!(log, |x| x.ln());
unary!(log2, |x| x.log2());
unary!(log10, |x| x.log10());
unary!(sin, |x| x.sin());
unary!(cos, |x| x.cos());
unary!(tan, |x| x.tan());
unary!(asin, |x| x.asin());
unary!(acos, |x| x.acos());
unary!(atan, |x| x.atan());
unary!(fround, |x| x as f32 as f64);
unary!(clz32, |x| Realm::to_int32(x).leading_zeros() as f64);
unary!(sinh, |x| x.sinh());
unary!(cosh, |x| x.cosh());
unary!(tanh, |x| x.tanh());
unary!(asinh, |x| x.asinh());
unary!(acosh, |x| x.acosh());
unary!(atanh, |x| x.atanh());
unary!(expm1, |x| x.exp_m1());
unary!(log1p, |x| x.ln_1p());

/// Half up: `Math.round(-2.5)` is -2. `x - floor(x) >= 0.5` is exact, where `floor(x + 0.5)` is wrong for 0.49999999999999994.
fn js_round(x: f64) -> f64 {
    if !x.is_finite() || x.fract() == 0.0 {
        return x;
    }
    let f = x.floor();
    let r = if x - f >= 0.5 { f + 1.0 } else { f };
    if r == 0.0 && x < 0.0 { -0.0 } else { r }
}

fn atan2(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let (y, x) = (num_arg(rt, args, 0)?, num_arg(rt, args, 1)?);
    Ok(Realm::num(y.atan2(x)))
}

fn pow(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let (x, y) = (num_arg(rt, args, 0)?, num_arg(rt, args, 1)?);
    Ok(Realm::num(crate::interp::pow(x, y)))
}

fn max(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let mut r = f64::NEG_INFINITY;
    for i in 0..args.len() {
        let x = num_arg(rt, args, i)?;
        if x.is_nan() || r.is_nan() {
            r = f64::NAN;
        } else if x > r || (x == 0.0 && r == 0.0 && x.is_sign_positive()) {
            r = x;
        }
    }
    Ok(Realm::num(r))
}

fn min(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let mut r = f64::INFINITY;
    for i in 0..args.len() {
        let x = num_arg(rt, args, i)?;
        if x.is_nan() || r.is_nan() {
            r = f64::NAN;
        } else if x < r || (x == 0.0 && r == 0.0 && x.is_sign_negative()) {
            r = x;
        }
    }
    Ok(Realm::num(r))
}

fn hypot(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    // Every argument is converted first, so a throw from a later one is not skipped by an infinity before it.
    let mut v = Vec::with_capacity(args.len());
    for i in 0..args.len() {
        v.push(num_arg(rt, args, i)?);
    }
    if v.iter().any(|x| x.is_infinite()) {
        return Ok(Value::number(f64::INFINITY));
    }
    if v.iter().any(|x| x.is_nan()) {
        return Ok(Value::number(f64::NAN));
    }
    // Scale by the largest magnitude, so a square that overflows or underflows does not change the result.
    let m = v.iter().fold(0.0f64, |m, x| m.max(x.abs()));
    if m == 0.0 {
        return Ok(Value::int(0));
    }
    Ok(Realm::num(m * v.iter().map(|x| (x / m) * (x / m)).sum::<f64>().sqrt()))
}

fn imul(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let (a, b) = (Realm::to_int32(num_arg(rt, args, 0)?), Realm::to_int32(num_arg(rt, args, 1)?));
    Ok(Value::int(a.wrapping_mul(b)))
}

/// xorshift64*, seeded from the OS at the first call.
fn random(rt: &mut Realm, _this: Value, _args: &[Value]) -> R<Value> {
    if rt.rng == 0 {
        let mut b = [0u8; 8];
        let _ = bare_os::random::fill(&mut b);
        rt.rng = u64::from_le_bytes(b) | 1;
    }
    let mut x = rt.rng;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    rt.rng = x;
    let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
    Ok(Value::number(r as f64 / (1u64 << 53) as f64))
}
