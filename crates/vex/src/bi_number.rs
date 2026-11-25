//! `Number`, `Boolean`, `Math` and the global number functions (plan step S2.7). `new Number(1)` and `new Boolean(true)` make a wrapper
//! object (`bi_wrap.rs`); a method of a primitive runs with the primitive as `this`, and the methods also accept a wrapper.
//! Known gaps: `toFixed` rounds the exact binary value half to even where the language rounds half up; `toString(radix)` of a fraction
//! gives at most 52 digits; `toPrecision`, `toExponential` and `Intl` are missing; a number prints with Rust's rules (`Realm::num_to_string`).
use crate::builtins::arg;
use crate::lazy::{Ent, alias, m, num};
use crate::interp_rt::{ErrKind, Key, R, Realm};
use crate::value::Value;

impl Realm {
    pub(crate) fn install_number(&mut self) {
        let proto = self.number_proto;
        let c = self.ctor("Number", 1, number_ctor, proto);
        // Register the globals together. `Number.parseInt` and `Number.parseFloat` alias two of these functions.
        let g = self.global;
        static GLOBAL_FUNCTIONS: &[Ent] = &[
            m("parseInt", 2, parse_int), m("parseFloat", 1, parse_float),
            m("isNaN", 1, global_is_nan), m("isFinite", 1, global_is_finite),
            crate::lazy::factory("BigInt", crate::bi_bigint::bigint_global),
            m("eval", 1, crate::api::eval_native), crate::bi_proxy::PROXY,
        ];
        self.lazy_table(g, GLOBAL_FUNCTIONS);
        static NUMBER_STATICS: &[Ent] = &[
            m("isInteger", 1, is_integer),
            m("isFinite", 1, number_is_finite),
            m("isNaN", 1, number_is_nan),
            m("isSafeInteger", 1, is_safe_integer),
            num("MAX_SAFE_INTEGER", 9007199254740991.0),
            num("MIN_SAFE_INTEGER", -9007199254740991.0),
            num("EPSILON", f64::EPSILON),
            num("MAX_VALUE", f64::MAX),
            num("MIN_VALUE", 5e-324),
            num("NaN", f64::NAN),
            num("POSITIVE_INFINITY", f64::INFINITY),
            num("NEGATIVE_INFINITY", f64::NEG_INFINITY),
            // `Number.parseInt` and `Number.parseFloat` are the same function objects as the globals.
            alias("parseFloat"),
            alias("parseInt"),
        ];
        self.lazy_table(c, NUMBER_STATICS);
        static NUMBER_PROTO: &[Ent] = &[
            m("toString", 1, number_to_string),
            m("toFixed", 1, to_fixed),
            m("toExponential", 1, to_exponential),
            m("toPrecision", 1, to_precision),
            m("toLocaleString", 0, to_locale_string),
            m("valueOf", 0, number_value_of),
        ];
        self.lazy_table(proto, NUMBER_PROTO);
        let b = self.boolean_proto;
        self.ctor("Boolean", 1, boolean_ctor, b);
        static BOOLEAN_PROTO: &[Ent] = &[m("toString", 0, boolean_to_string), m("valueOf", 0, boolean_value_of)];
        self.lazy_table(b, BOOLEAN_PROTO);
        let math = Obj::new_plain(self);
        self.hidden(g, "Math", math.value());
        let tag = self.string("Math");
        self.define_attrs(math, Key::Name(self.wk[crate::bi_symbol::TO_STRING_TAG]), crate::shapes::A_C, tag).expect("define");
        static MATH: &[Ent] = &[
            num("PI", std::f64::consts::PI),
            num("E", std::f64::consts::E),
            num("LN2", std::f64::consts::LN_2),
            num("LN10", std::f64::consts::LN_10),
            num("LOG2E", std::f64::consts::LOG2_E),
            num("LOG10E", std::f64::consts::LOG10_E),
            num("SQRT2", std::f64::consts::SQRT_2),
            num("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
            m("floor", 1, floor),
            m("ceil", 1, ceil),
            m("round", 1, round),
            m("trunc", 1, trunc),
            m("abs", 1, abs),
            m("sign", 1, sign),
            m("sqrt", 1, sqrt),
            m("cbrt", 1, cbrt),
            m("exp", 1, exp),
            m("log", 1, log),
            m("log2", 1, log2),
            m("log10", 1, log10),
            m("sin", 1, sin),
            m("cos", 1, cos),
            m("tan", 1, tan),
            m("asin", 1, asin),
            m("acos", 1, acos),
            m("atan", 1, atan),
            m("atan2", 2, atan2),
            m("pow", 2, pow),
            m("max", 2, max),
            m("min", 2, min),
            m("hypot", 2, hypot),
            m("imul", 2, imul),
            m("f16round", 1, f16round),
            m("fround", 1, fround),
            m("clz32", 1, clz32),
            m("sinh", 1, sinh),
            m("cosh", 1, cosh),
            m("tanh", 1, tanh),
            m("asinh", 1, asinh),
            m("acosh", 1, acosh),
            m("atanh", 1, atanh),
            m("expm1", 1, expm1),
            m("log1p", 1, log1p),
            m("random", 0, random),
        ];
        self.lazy_table(math, MATH);
    }
}

use crate::object::Obj;

impl Obj {
    /// A new plain object (`{}`) of the realm.
    pub(crate) fn new_plain(rt: &mut Realm) -> Obj {
        Obj::new(&mut rt.heap, &rt.shapes, rt.state.s_object).expect("memory")
    }
}

fn num_arg(rt: &mut Realm, args: &[Value], i: usize) -> R<f64> {
    rt.to_number(arg(args, i))
}

pub(crate) fn string_to_number(t: &str) -> f64 {
    let t = t.trim_matches(|c: char| u16::try_from(c as u32).is_ok_and(crate::bi_string::is_space));
    match t.as_bytes() {
        [] => 0.0,
        [b'0', b'b' | b'B', rest @ ..] => radix_integer(rest, 1),
        [b'0', b'o' | b'O', rest @ ..] => radix_integer(rest, 3),
        [b'0', b'x' | b'X', rest @ ..] => radix_integer(rest, 4),
        b"Infinity" | b"+Infinity" => f64::INFINITY,
        b"-Infinity" => f64::NEG_INFINITY,
        b if b.iter().all(|c| c.is_ascii_digit() || matches!(c, b'+' | b'-' | b'.' | b'e' | b'E')) => t.parse().unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

/// Keep the high bits and a sticky bit so that the cast rounds only once.
fn radix_integer(digits: &[u8], bits: u32) -> f64 {
    if digits.is_empty() {
        return f64::NAN;
    }
    let (mut high, mut dropped, mut sticky) = (0u64, 0u32, false);
    for &b in digits {
        let Some(d) = (b as char).to_digit(1 << bits) else { return f64::NAN };
        if high >> (64 - bits) == 0 {
            high = (high << bits) | d as u64;
        } else {
            dropped += bits;
            sticky |= d != 0;
        }
    }
    (high | sticky as u64) as f64 * 2f64.powi(dropped.min(2000) as i32)
}

fn number_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let n = if args.is_empty() {
        Value::int(0)
    } else {
        let p = rt.to_primitive(args[0], crate::builtins::Hint::Number)?;
        Realm::num(match Realm::big_of(p) {
            Some(b) => crate::bigint::to_f64(b.bigint()),
            None => rt.to_number(p)?,
        })
    };
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
        group_decimal(&mut g, int);
        format!("{}{g}{}{frac}", if x.is_sign_negative() { "-" } else { "" }, if frac.is_empty() { "" } else { "." })
    };
    Ok(rt.string(&t))
}

/// Add comma separators to an unsigned ASCII decimal integer.
pub(crate) fn group_decimal(out: &mut String, digits: &str) {
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
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
unary!(f16round, |x| crate::bi_typed::decode_f16(crate::bi_typed::encode_f16(x)));
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

/// xorshift64*, seeded from the OS at the first call. With `BARE_SEED=<number>` in the environment, the seed is that number, so a run
/// that replays recorded replies makes the same values each time (as `crypto.getRandomValues` does in the browser).
fn random(rt: &mut Realm, _this: Value, _args: &[Value]) -> R<Value> {
    if rt.rng == 0 {
        static SEED: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
        rt.rng = match SEED.get_or_init(|| std::env::var("BARE_SEED").ok()?.parse().ok()) {
            Some(seed) => seed.wrapping_mul(0x9E37_79B9_7F4A_7C15),
            None => {
                let mut b = [0u8; 8];
                let _ = bare_os::random::fill(&mut b);
                u64::from_le_bytes(b)
            }
        } | 1;
    }
    let mut x = rt.rng;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    rt.rng = x;
    let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
    Ok(Value::number(r as f64 / (1u64 << 53) as f64))
}
