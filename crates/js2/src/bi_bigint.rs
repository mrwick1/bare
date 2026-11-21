//! ECMAScript BigInt conversions, operators and built-ins. The limb arithmetic is in `bigint.rs`.
use std::cmp::Ordering;

use crate::bigint::{self as big, Int, Sv};
use crate::blob::Blob;
use crate::builtins::{HIDDEN, Hint, arg};
use crate::bytecode::Op;
use crate::interp::STOP;
use crate::interp_rt::{ErrKind, Key, R, Realm};
use crate::lazy::{Ent, m};
use crate::object::Obj;
use crate::shapes::A_C;
use crate::string::Str;
use crate::value::Value;

// The old engine allows this many limbs per BigInt by default.
const MAX_BITS: u64 = (1 << 18) * 64;
const MIX: &str = "cannot mix BigInt and other numeric types";

impl Realm {
    pub(crate) fn make_bigint(&mut self, a: Sv<'_>) -> R<Value> {
        if big::bit_len(a.mag) > MAX_BITS {
            return Err(self.error(ErrKind::Range, "BigInt is too large"));
        }
        Blob::new_bigint(&mut self.heap, a).ok_or_else(|| self.error(ErrKind::Range, "out of memory"))
    }

    pub(crate) fn load_bigint(&mut self, atom: Str) -> R<Value> {
        let Some(crate::string::Chars::One(bytes)) = atom.chars() else {
            return Err(self.error(ErrKind::Syntax, "invalid BigInt literal"));
        };
        self.bigint_work((bytes.len() as u64).pow(2) / 4096)?;
        let n = big::parse_digits(bytes).ok_or_else(|| self.error(ErrKind::Syntax, "invalid BigInt literal"))?;
        self.make_bigint(n.sv())
    }

    fn bigint_work(&mut self, work: u64) -> R<()> {
        self.fuel = self.fuel.checked_sub(work).ok_or(STOP)?;
        Ok(())
    }

    fn parse_bigint(&mut self, s: Str) -> R<Option<Int>> {
        let units = self.units(s);
        self.bigint_work((units.len() as u64).pow(2) / 4096)?;
        Ok(big::string_to_bigint(&units))
    }

    pub(crate) fn to_bigint(&mut self, v: Value) -> R<Value> {
        let p = self.to_primitive(v, Hint::Number)?;
        self.bigint_from_primitive(p, false)
    }

    fn bigint_from_primitive(&mut self, p: Value, allow_number: bool) -> R<Value> {
        if p.is_bigint() {
            return Ok(p);
        }
        let n = if let Some(b) = p.as_bool() {
            Int::from_u64(b as u64)
        } else if let Some(s) = Realm::str_of(p) {
            self.parse_bigint(s)?.ok_or_else(|| self.error(ErrKind::Syntax, "invalid BigInt string"))?
        } else if let Some(f) = p.as_f64().filter(|_| allow_number) {
            if !f.is_finite() || f.fract() != 0.0 {
                return Err(self.error(ErrKind::Range, "BigInt requires an integer"));
            }
            big::from_f64(f)
        } else {
            return self.type_error("cannot convert value to BigInt");
        };
        self.make_bigint(n.sv())
    }

    #[inline(never)]
    pub(crate) fn to_numeric(&mut self, v: Value) -> R<Value> {
        let p = self.to_primitive(v, Hint::Number)?;
        if p.is_bigint() || p.is_number() { Ok(p) } else { self.to_number(p).map(Realm::num) }
    }

    #[cold]
    pub(crate) fn numeric_binary(&mut self, op: Op, a: Value, b: Value) -> R<Value> {
        let (a, b) = (self.to_numeric(a)?, self.to_numeric(b)?);
        if let (Some(x), Some(y)) = (a.as_f64(), b.as_f64()) {
            let ints = || (Realm::to_int32(x), Realm::to_int32(y));
            return Ok(match op {
                Op::Sub => Value::number(x - y),
                Op::Mul => Value::number(x * y),
                Op::Div => Value::number(x / y),
                Op::Mod => Value::number(x % y),
                Op::Exp => Value::number(crate::interp::pow(x, y)),
                Op::BitAnd => { let (x, y) = ints(); Value::int(x & y) }
                Op::BitOr => { let (x, y) = ints(); Value::int(x | y) }
                Op::BitXor => { let (x, y) = ints(); Value::int(x ^ y) }
                Op::Shl => { let (x, y) = ints(); Value::int(x << (y & 31)) }
                Op::Shr => { let (x, y) = ints(); Value::int(x >> (y & 31)) }
                Op::UShr => { let (x, y) = ints(); Realm::num(((x as u32) >> (y & 31)) as f64) }
                _ => unreachable!("binary numeric operation"),
            });
        }
        self.bigint_binary(op, a, b)
    }

    pub(crate) fn bigint_binary(&mut self, op: Op, a: Value, b: Value) -> R<Value> {
        let (Some(a), Some(b)) = (Realm::big_of(a), Realm::big_of(b)) else { return self.type_error(MIX) };
        let (a, b) = (a.bigint(), b.bigint());
        let (la, lb) = (a.mag.len() as u64, b.mag.len() as u64);
        let work = match op {
            Op::Mul if la.min(lb) >= big::KARATSUBA as u64 => (la.max(lb) as f64 * big::pow_estimate(la.min(lb) as f64, 0.585)) as u64,
            Op::Mul | Op::Div | Op::Mod => la * lb,
            _ => la.max(lb),
        };
        self.bigint_work(work / 64)?;
        let too_big = |rt: &mut Realm| Err(rt.error(ErrKind::Range, "BigInt is too large"));
        let n = match op {
            Op::Add => big::add(a, b),
            Op::Sub => big::sub(a, b),
            Op::Mul => {
                if big::bit_len(a.mag) + big::bit_len(b.mag) > MAX_BITS + 1 && !a.is_zero() && !b.is_zero() {
                    return too_big(self);
                }
                big::mul(a, b)
            }
            Op::Div | Op::Mod => {
                if b.is_zero() {
                    return Err(self.error(ErrKind::Range, "BigInt division by zero"));
                }
                let (q, r) = big::divrem(a, b);
                if op == Op::Div { q } else { r }
            }
            Op::BitAnd => big::bitop(big::Bit::And, a, b),
            Op::BitOr => big::bitop(big::Bit::Or, a, b),
            Op::BitXor => big::bitop(big::Bit::Xor, a, b),
            Op::UShr => return self.type_error("BigInt has no unsigned right shift"),
            Op::Exp => {
                if b.neg {
                    return Err(self.error(ErrKind::Range, "negative BigInt exponent"));
                }
                if b.is_zero() {
                    return self.make_bigint(Sv { neg: false, mag: &[1] });
                }
                if a.is_zero() || a.mag == [1] {
                    return self.make_bigint(Sv { neg: a.neg && b.mag[0] & 1 != 0, mag: a.mag });
                }
                let [e] = b.mag else { return too_big(self) };
                let bound = (big::bit_len(a.mag) as u128 - 1) * *e as u128;
                if bound >= MAX_BITS as u128 {
                    return too_big(self);
                }
                self.bigint_work((bound as u64 / 64).pow(2) / 64)?;
                big::pow(a, *e)
            }
            Op::Shl | Op::Shr => {
                let n = b.mag.first().copied().filter(|_| b.mag.len() == 1).unwrap_or(if b.is_zero() { 0 } else { u64::MAX });
                if (op == Op::Shl) != b.neg {
                    if !a.is_zero() && big::bit_len(a.mag).saturating_add(n) > MAX_BITS {
                        return too_big(self);
                    }
                    big::shl(a, n)
                } else {
                    big::shr(a, n)
                }
            }
            _ => unreachable!("binary BigInt operation"),
        };
        self.make_bigint(n.sv())
    }

    #[cold]
    pub(crate) fn numeric_unary(&mut self, op: Op, v: Value) -> R<Value> {
        let p = self.to_numeric(v)?;
        let Some(b) = Realm::big_of(p) else {
            let x = p.as_f64().expect("ToNumeric");
            return Ok(match op {
                Op::Neg => Value::number(-x),
                Op::BitNot => Value::int(!Realm::to_int32(x)),
                Op::Inc => Realm::num(x + 1.0),
                Op::Dec => Realm::num(x - 1.0),
                _ => unreachable!("unary numeric operation"),
            });
        };
        let b = b.bigint();
        let n = match op {
            Op::Neg => return self.make_bigint(b.negated()),
            Op::BitNot => big::not(b),
            Op::Inc | Op::Dec => big::add(b, Sv { neg: op == Op::Dec, mag: &[1] }),
            _ => unreachable!("unary BigInt operation"),
        };
        self.make_bigint(n.sv())
    }

    pub(crate) fn bigint_text(&mut self, b: Blob, radix: u32) -> R<String> {
        let n = b.words().len() as u64;
        self.bigint_work(if radix.is_power_of_two() { n / 64 } else { n * n / 128 })?;
        Ok(big::to_string_radix(b.bigint(), radix))
    }

    /// Compare a BigInt with a primitive. Strings use StringToBigInt, with no rounding.
    pub(crate) fn bigint_order(&mut self, a: Blob, b: Value) -> R<Option<Ordering>> {
        let a = a.bigint();
        if let Some(b) = Realm::big_of(b) {
            return Ok(Some(big::cmp(a, b.bigint())));
        }
        if let Some(s) = Realm::str_of(b) {
            return Ok(self.parse_bigint(s)?.map(|b| big::cmp(a, b.sv())));
        }
        Ok(big::cmp_f64(a, self.to_number(b)?))
    }

    #[cold]
    pub(crate) fn bigint_intrinsics(&mut self) -> (Obj, Obj) {
        if let Some(pair) = self.bigint_intrinsics {
            return pair;
        }
        let p = Obj::new(&mut self.heap, &self.shapes, self.s_object).expect("memory");
        let c = self.new_native(bigint_ctor, true, 1, "BigInt");
        self.bigint_intrinsics = Some((c, p));
        self.keep(p.value());
        self.keep(c.value());
        let tag = self.string("BigInt");
        self.define_attrs(c, Key::Name(self.k_prototype), 0, p.value()).expect("define");
        self.define_attrs(p, Key::Name(self.k_constructor), HIDDEN, c.value()).expect("define");
        self.define_attrs(p, Key::Name(self.wk[crate::bi_symbol::TO_STRING_TAG]), A_C, tag).expect("define");
        static STATICS: &[Ent] = &[m("asIntN", 2, as_int_n), m("asUintN", 2, as_uint_n)];
        static PROTO: &[Ent] = &[m("toString", 0, to_string), m("toLocaleString", 0, to_locale_string), m("valueOf", 0, value_of)];
        self.lazy_table(c, STATICS);
        self.lazy_table(p, PROTO);
        (c, p)
    }
}

pub(crate) fn bigint_global(rt: &mut Realm) -> Value {
    rt.bigint_intrinsics().0.value()
}

fn bigint_ctor(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    if !rt.nt.is_undefined() {
        return rt.type_error("BigInt cannot be called with new");
    }
    let p = rt.to_primitive(arg(args, 0), Hint::Number)?;
    rt.bigint_from_primitive(p, true)
}

fn truncate(rt: &mut Realm, args: &[Value], f: fn(Sv<'_>, u64, u64) -> Option<Int>) -> R<Value> {
    let width = rt.to_number(arg(args, 0))?.trunc();
    let width = if width.is_nan() { 0.0 } else { width };
    if !(0.0..=9_007_199_254_740_991.0).contains(&width) {
        return Err(rt.error(ErrKind::Range, "invalid BigInt bit width"));
    }
    let b = rt.to_bigint(arg(args, 1))?;
    let a = Realm::big_of(b).expect("ToBigInt");
    let n = f(a.bigint(), width as u64, MAX_BITS).ok_or_else(|| rt.error(ErrKind::Range, "BigInt is too large"))?;
    rt.make_bigint(n.sv())
}

fn as_int_n(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    truncate(rt, args, big::as_int_n)
}

fn as_uint_n(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    truncate(rt, args, big::as_uint_n)
}

fn value_of(rt: &mut Realm, this: Value, _: &[Value]) -> R<Value> {
    if this.is_bigint() {
        return Ok(this);
    }
    if let Some(b) = rt.prim_of(this).filter(|v| v.is_bigint()) {
        return Ok(b);
    }
    rt.type_error("receiver is not a BigInt")
}

fn to_string(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let v = value_of(rt, this, &[])?;
    let r = arg(args, 0);
    let r = if r.is_undefined() { 10.0 } else { rt.to_number(r)?.trunc() };
    if !(2.0..=36.0).contains(&r) {
        return Err(rt.error(ErrKind::Range, "BigInt radix must be between 2 and 36"));
    }
    let b = Realm::big_of(v).expect("BigInt");
    let text = rt.bigint_text(b, r as u32)?;
    Ok(rt.string(&text))
}

fn to_locale_string(rt: &mut Realm, this: Value, _: &[Value]) -> R<Value> {
    let v = value_of(rt, this, &[])?;
    let b = Realm::big_of(v).expect("BigInt");
    let text = rt.bigint_text(b, 10)?;
    let (sign, digits) = text.strip_prefix('-').map_or(("", text.as_str()), |digits| ("-", digits));
    let mut grouped = String::from(sign);
    crate::bi_number::group_decimal(&mut grouped, digits);
    Ok(rt.string(&grouped))
}
