// BigInt in the evaluator: literals, ToNumeric, the operators, comparison, `BigInt()`, `BigInt.asIntN`, `BigInt.asUintN` and
// `BigInt.prototype`. Included by `eval.rs` (`include!`), so it shares that module's imports and private items. The
// arithmetic is in `bigint.rs`; the heap form (a value that does not fit in `i64`) is in `heap_big.rs`.
//
// Representation: `Value::BigInt(i64)` is inline. `Value::BigHeap` is a shared heap entry for a value outside `i64` (see
// `heap_big.rs`). Two `i64` operands never reach the limb arithmetic: `+`, `-`, `*`, `/` and `%` run in `i128`, which is exact
// for them. Only a result outside `i64` is made as a heap entry.
//
// Supported
// - Literals (`10n`, `0xFFn`, `0o7n`, `0b1n`, `1_000n`), BigInt property keys in object literals and classes (the key is the
//   decimal text), `typeof`, `!`, `-`, `~`, `++`, `--`, and the binary operators `+ - * / % ** << >> & | ^`. Mixing a BigInt and
//   a Number is a TypeError, `>>>` and unary `+` are TypeErrors, division by zero and a negative exponent are RangeErrors.
// - `<`, `<=`, `>`, `>=`, `==` and `!=` with a Number or a String compare the exact mathematical values. `===`, `Object.is`,
//   `Map` and `Set` use the one-handle-per-value rule of the heap.
// - ToString (also for `+` with a string, template literals and property keys), ToBoolean, ToPrimitive of an object whose
//   `valueOf` or `Symbol.toPrimitive` gives a BigInt, `Number(1n)`, `BigInt(x)` (a function, not a constructor),
//   `BigInt.asIntN`, `BigInt.asUintN`, `BigInt.prototype.toString(radix)`, `valueOf`, `toLocaleString`, `@@toStringTag`.
// - `JSON.stringify` of a BigInt is the V8 TypeError. BigInt64Array, BigUint64Array and the DataView BigInt methods use these values.
//
// Limits (a `Limit`, never a wrong result): a BigInt of more than `HeapLimits::max_big_limbs` limbs. The size of a result is
// checked before the operation allocates it. An operation charges steps for its work: `work / 64`, where `work` is the number
// of limb steps (the Karatsuba count for a large product, `la * lb` for a division, `limbs^2 / 2` for text).
//
// Gates (`Unsupported`)
// - `Object(1n)`, `new Object(1n)` and `Reflect.construct` of a wrapper: there are no wrapper objects in this evaluator.
// - `BigInt.prototype.toLocaleString` with arguments, or on a value of more than 3 digits: it needs `Intl.NumberFormat`
//   (grouping). A short value without arguments gives the same text as `toString`.
//
// State: `BigState` holds the prototype and the constructor. Both are GC roots, so they survive `delete globalThis.BigInt`.

use crate::bigint as bi;

struct BigState {
    proto: ObjId,
    ctor: ObjId,
}

impl BigState {
    fn new(o: ObjId) -> BigState {
        BigState { proto: o, ctor: o }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BigFn {
    Ctor,
    AsIntN,
    AsUintN,
    ToString,
    ValueOf,
    ToLocaleString,
}

/// Why an operation on BigInts stopped.
enum BigErr {
    DivZero,
    NegExp,
    UShr,
    TooBig,
}

const MIX: &str = "Cannot mix BigInt and other types, use explicit conversions";

/// The value of `v` in `i128`, as a BigInt value.
fn i128_parts(v: i128) -> bi::Int {
    let m = v.unsigned_abs();
    bi::Int::new(v < 0, vec![m as u64, (m >> 64) as u64])
}

/// Work of a product of `la` by `lb` limbs, in limb steps: schoolbook below the Karatsuba size, else `n^1.585`.
fn mul_work(la: usize, lb: usize) -> u64 {
    let (a, b) = (la.max(lb) as f64, la.min(lb) as f64);
    if b < bi::KARATSUBA as f64 { (a * b) as u64 } else { (a * b.powf(0.585)) as u64 }
}

/// The operation of `op` on two BigInts that are not both `i64`, or that need the limb routines. `max` is the limb cap.
fn big_compute(op: BinaryOp, a: bi::Sv, b: bi::Sv, max: u64) -> Result<bi::Int, BigErr> {
    use BinaryOp::*;
    let (la, lb) = (a.mag.len() as u64, b.mag.len() as u64);
    Ok(match op {
        Add | Sub => {
            if la.max(lb) + 1 > max {
                return Err(BigErr::TooBig);
            }
            if op == Add { bi::add(a, b) } else { bi::sub(a, b) }
        }
        Mul => {
            if la + lb > max {
                return Err(BigErr::TooBig);
            }
            bi::mul(a, b)
        }
        Div | Mod => {
            if b.is_zero() {
                return Err(BigErr::DivZero);
            }
            let (q, r) = bi::divrem(a, b);
            if op == Div { q } else { r }
        }
        BitAnd => bi::bitop(bi::Bit::And, a, b),
        BitOr => bi::bitop(bi::Bit::Or, a, b),
        BitXor => bi::bitop(bi::Bit::Xor, a, b),
        Exp => {
            if b.neg {
                return Err(BigErr::NegExp);
            }
            if b.is_zero() {
                return Ok(bi::Int::from_i64(1));
            }
            if a.is_zero() || a.mag == [1] {
                // 0 ** e, 1 ** e and (-1) ** e do not grow, so a huge exponent is fine.
                return Ok(bi::Int::new(a.neg && b.mag[0] & 1 == 1, a.mag.to_vec()));
            }
            let e = match b.mag {
                [e] => *e,
                _ => return Err(BigErr::TooBig),
            };
            let bits = (bi::bit_len(a.mag) as u128) * e as u128;
            if bits / 64 + 1 > max as u128 {
                return Err(BigErr::TooBig);
            }
            bi::pow(a, e)
        }
        Shl | Shr => {
            // A count beyond `i64` only matters through its sign: it is far past any size (left) or any value (right).
            let n = bi::to_i64(b).unwrap_or(if b.neg { -(1 << 62) } else { 1 << 62 });
            let left = (op == Shl) == (n >= 0);
            let n = n.unsigned_abs();
            if left {
                if a.is_zero() {
                    return Ok(bi::Int::zero());
                }
                if la.saturating_add(n / 64 + 1) > max {
                    return Err(BigErr::TooBig);
                }
                bi::shl(a, n)
            } else {
                bi::shr(a, n)
            }
        }
        UShr => return Err(BigErr::UShr),
        _ => unreachable!("not an arithmetic operator"),
    })
}

impl<'p> Interp<'p> {
    // ------------------------------------------------------------ GC and install

    fn big_roots(&self, out: &mut Vec<Value>) {
        out.extend([self.big.proto, self.big.ctor].map(Value::Object));
    }

    fn install_bigint(&mut self) -> Res<()> {
        let proto = h!(self, self.heap.new_object(Some(self.object_proto)));
        let ctor = self.native(Nat::Big(BigFn::Ctor), "BigInt", 1)?;
        self.sc_global("BigInt", ctor, proto)?;
        self.big = BigState { proto, ctor };
        self.method(ctor, "asUintN", Nat::Big(BigFn::AsUintN), 2)?;
        self.method(ctor, "asIntN", Nat::Big(BigFn::AsIntN), 2)?;
        self.method(proto, "toLocaleString", Nat::Big(BigFn::ToLocaleString), 0)?;
        self.method(proto, "toString", Nat::Big(BigFn::ToString), 0)?;
        self.method(proto, "valueOf", Nat::Big(BigFn::ValueOf), 0)?;
        self.sc_tag(proto, "BigInt")
    }

    // ------------------------------------------------------------ values

    /// A BigInt value from an `i128` result (exact for `i64` operands of `+`, `-` and `*`).
    fn big_i128(&mut self, v: i128) -> Res<Value> {
        match i64::try_from(v) {
            Ok(n) => Ok(Value::BigInt(n)),
            Err(_) => Ok(h!(self, self.heap.new_bigint(i128_parts(v)))),
        }
    }

    fn range_err(&mut self, msg: &str) -> Error {
        self.mk_err(RANGE_ERROR, msg)
    }

    /// The value of a BigInt literal or of a BigInt property key (its source text, with the `n`).
    #[inline(never)]
    fn big_literal(&mut self, sp: Span) -> Res<Value> {
        let t = self.prog.text(sp);
        let d = t.strip_suffix('n').unwrap_or(t);
        if d.len() <= 18 && d.bytes().all(|c| c.is_ascii_digit()) {
            return Ok(Value::BigInt(d.bytes().fold(0i64, |a, c| a * 10 + (c - b'0') as i64)));
        }
        let work = t.len() as u64;
        let Some(i) = bi::parse_literal(t) else { return Err(unsupported("BigInt literal", sp)) };
        self.spend(work * work / 4096)?;
        Ok(h!(self, self.heap.new_bigint(i)))
    }

    /// The property key text of a BigInt literal key: its decimal value.
    #[inline(never)]
    fn big_key_string(&mut self, sp: Span) -> Res<StrId> {
        let v = self.big_literal(sp)?;
        Ok(h!(self, self.heap.bigint_to_string(v, 10)))
    }

    /// Limbs of a BigInt value (an inline value is 0 or 1).
    fn big_limbs(&self, v: Value) -> usize {
        match v {
            Value::BigInt(n) => (n != 0) as usize,
            _ => self.heap.bigint_limbs(v),
        }
    }

    // ------------------------------------------------------------ conversions

    /// ToNumeric (section 7.1.3): a Number or a BigInt.
    #[inline(never)]
    fn to_numeric(&mut self, v: Value) -> Res<Value> {
        if let Value::Number(_) = v {
            return Ok(v);
        }
        let p = self.coerce_primitive(v, Hint::Number)?;
        self.numeric_of_prim(p)
    }

    /// ToNumeric of a primitive.
    #[inline(never)]
    fn numeric_of_prim(&mut self, p: Value) -> Res<Value> {
        if p.is_bigint() { Ok(p) } else { Ok(Value::Number(self.coerce_number(p)?)) }
    }

    /// ToBigInt (section 7.1.13).
    fn to_bigint(&mut self, v: Value) -> Res<Value> {
        let p = self.coerce_primitive(v, Hint::Number)?;
        match p {
            Value::BigInt(_) | Value::BigHeap(_) => Ok(p),
            Value::Bool(b) => Ok(Value::BigInt(b as i64)),
            Value::String(s) => {
                let n = h!(self, self.heap.str_len(s)) as u64;
                self.spend(n * n / 4096)?;
                let units = h!(self, self.heap.str_units(s));
                match bi::string_to_bigint(units) {
                    Some(i) => Ok(h!(self, self.heap.new_bigint(i))),
                    None => {
                        let t = self.display(p);
                        Err(self.mk_err(SYNTAX_ERROR, &format!("Cannot convert {t} to a BigInt")))
                    }
                }
            }
            Value::Symbol(_) => Err(self.type_err("Cannot convert Symbol() to a BigInt")),
            _ => {
                let t = self.display(p);
                Err(self.type_err(&format!("Cannot convert {t} to a BigInt")))
            }
        }
    }

    /// `Number(big)`: the nearest double.
    fn big_to_f64(&mut self, v: Value) -> Res<f64> {
        let mut buf = [0u64; 1];
        let a = self.heap.bigint_sv(v, &mut buf).map_err(Error::Heap)?;
        Ok(bi::to_f64(a))
    }

    /// ToIndex-like conversion for the width of `asIntN`: an integer from 0 to 2^53 - 1.
    fn to_bit_width(&mut self, v: Value) -> Res<u64> {
        let n = self.coerce_number(v)?;
        let n = if n.is_nan() { 0.0 } else { n.trunc() };
        if !(0.0..=9007199254740991.0).contains(&n) {
            return Err(self.range_err("Invalid value: not (convertible to) a safe integer"));
        }
        Ok(n as u64)
    }

    // ------------------------------------------------------------ operators

    /// An arithmetic or bitwise operator on two numerics that are not both Numbers.
    #[inline(never)]
    fn big_arith(&mut self, op: BinaryOp, a: Value, b: Value) -> Res<Value> {
        use BinaryOp::*;
        if !(a.is_bigint() && b.is_bigint()) {
            return Err(self.type_err(MIX));
        }
        if let (Value::BigInt(x), Value::BigInt(y)) = (a, b) {
            let (xw, yw) = (x as i128, y as i128);
            match op {
                Add => return self.big_i128(xw + yw),
                Sub => return self.big_i128(xw - yw),
                Mul => return self.big_i128(xw * yw),
                Div | Mod if y == 0 => return Err(self.range_err("Division by zero")),
                Div => return self.big_i128(xw / yw),
                Mod => return self.big_i128(xw % yw),
                BitAnd => return Ok(Value::BigInt(x & y)),
                BitOr => return Ok(Value::BigInt(x | y)),
                BitXor => return Ok(Value::BigInt(x ^ y)),
                Shr if (0..64).contains(&y) => return Ok(Value::BigInt(x >> y)),
                Shl if (0..64).contains(&y) => return self.big_i128(xw << y),
                Exp if y >= 0 && x != 0 && x.unsigned_abs() > 1 => {
                    if let Ok(e) = u32::try_from(y)
                        && let Some(r) = x.checked_pow(e)
                    {
                        return Ok(Value::BigInt(r));
                    }
                }
                _ => {}
            }
        }
        // The limb routines. Charge the work and check the size of the result before computing.
        let (la, lb) = (self.big_limbs(a), self.big_limbs(b));
        let max = self.heap.limits().max_big_limbs as u64;
        let work = match op {
            Mul => mul_work(la, lb),
            Div | Mod => (la * lb) as u64,
            // The result has `bits(a) * b` bits and costs about two products of that size. A base of 0, 1 or -1, and a
            // negative exponent (a RangeError), cost nothing.
            Exp => match b {
                Value::BigInt(e) if e < 0 => 0,
                _ if self.heap.bigint_bits(a) <= 1 => 0,
                _ => {
                    let e = if let Value::BigInt(e) = b { e as f64 } else { 1e18 };
                    let limbs = (self.heap.bigint_bits(a) as f64 * e) / 64.0 + 1.0;
                    // A result over the size cap is refused below. It costs nothing, so the size cap speaks first.
                    if limbs > max as f64 { 0 } else { (limbs.powf(1.585) * 2.0) as u64 }
                }
            },
            _ => (la + lb) as u64,
        };
        self.spend(work / 64)?;
        let (mut ba, mut bb) = ([0u64; 1], [0u64; 1]);
        let r = {
            let sa = self.heap.bigint_sv(a, &mut ba).map_err(Error::Heap)?;
            let sb = self.heap.bigint_sv(b, &mut bb).map_err(Error::Heap)?;
            big_compute(op, sa, sb, max)
        };
        match r {
            Ok(i) => Ok(h!(self, self.heap.new_bigint(i))),
            Err(BigErr::DivZero) => Err(self.range_err("Division by zero")),
            // V8 12.4 (Node 22) fills the message template with no argument, so the text starts with "undefined". Match it.
            Err(BigErr::NegExp) => Err(self.range_err("undefined must be positive")),
            Err(BigErr::UShr) => Err(self.type_err("BigInts have no unsigned right shift, use >> instead")),
            Err(BigErr::TooBig) => Err(Error::Limit(LimitKind::Heap(Limit::BigLimbs))),
        }
    }

    /// `-v` for a BigInt.
    #[inline(never)]
    fn big_neg(&mut self, v: Value) -> Res<Value> {
        if let Value::BigInt(n) = v {
            return self.big_i128(-(n as i128));
        }
        let mut buf = [0u64; 1];
        let i = {
            let a = self.heap.bigint_sv(v, &mut buf).map_err(Error::Heap)?;
            bi::Int::new(!a.neg, a.mag.to_vec())
        };
        Ok(h!(self, self.heap.new_bigint(i)))
    }

    /// `~v` for a BigInt.
    #[inline(never)]
    fn big_not(&mut self, v: Value) -> Res<Value> {
        if let Value::BigInt(n) = v {
            return Ok(Value::BigInt(!n));
        }
        let mut buf = [0u64; 1];
        let i = {
            let a = self.heap.bigint_sv(v, &mut buf).map_err(Error::Heap)?;
            bi::not(a)
        };
        Ok(h!(self, self.heap.new_bigint(i)))
    }

    /// `v + 1n` or `v - 1n` (`++` and `--`).
    #[inline(never)]
    fn big_step(&mut self, v: Value, up: bool) -> Res<Value> {
        self.big_arith(if up { BinaryOp::Add } else { BinaryOp::Sub }, v, Value::BigInt(1))
    }

    /// The order of two primitives when at least one is a BigInt (IsLessThan, section 7.2.14). `None` is `undefined`.
    #[inline(never)]
    fn big_order(&mut self, a: Value, b: Value) -> Res<Option<std::cmp::Ordering>> {
        // A String against a BigInt reads the string as a BigInt; a text that is not one makes the comparison `undefined`.
        let conv = |me: &mut Self, v: Value, other_big: bool| -> Res<Result<Value, Option<std::cmp::Ordering>>> {
            match v {
                Value::String(s) if other_big => {
                    let n = h!(me, me.heap.str_len(s)) as u64;
                    me.spend(n * n / 4096)?;
                    let units = h!(me, me.heap.str_units(s));
                    match bi::string_to_bigint(units) {
                        Some(i) => Ok(Ok(h!(me, me.heap.new_bigint(i)))),
                        None => Ok(Err(None)),
                    }
                }
                _ => Ok(Ok(me.numeric_of_prim(v)?)),
            }
        };
        let (a, b) = match (conv(self, a, b.is_bigint())?, conv(self, b, a.is_bigint())?) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(r), _) | (_, Err(r)) => return Ok(r),
        };
        let (mut ba, mut bb) = ([0u64; 1], [0u64; 1]);
        Ok(match (a, b) {
            (Value::Number(x), _) => {
                let sb = self.heap.bigint_sv(b, &mut bb).map_err(Error::Heap)?;
                bi::cmp_f64(sb, x).map(std::cmp::Ordering::reverse)
            }
            (_, Value::Number(y)) => {
                let sa = self.heap.bigint_sv(a, &mut ba).map_err(Error::Heap)?;
                bi::cmp_f64(sa, y)
            }
            _ => {
                let sa = self.heap.bigint_sv(a, &mut ba).map_err(Error::Heap)?;
                let sb = self.heap.bigint_sv(b, &mut bb).map_err(Error::Heap)?;
                Some(bi::cmp(sa, sb))
            }
        })
    }

    // ------------------------------------------------------------ natives

    /// The BigInt of a method receiver, or the V8 TypeError. There are no wrapper objects.
    fn this_bigint(&mut self, this: Value, name: &str) -> Res<Value> {
        if this.is_bigint() {
            return Ok(this);
        }
        Err(self.type_err(&format!("BigInt.prototype.{name} requires that 'this' be a BigInt")))
    }

    fn call_bigint(&mut self, f: BigFn, this: Value, args: &[Value]) -> Res<Value> {
        let (a0, a1) = (arg(args, 0), arg(args, 1));
        match f {
            BigFn::Ctor => {
                let p = self.coerce_primitive(a0, Hint::Number)?;
                let Value::Number(n) = p else { return self.to_bigint(p) };
                if !n.is_finite() || n.trunc() != n {
                    let t = self.display(p);
                    return Err(self.range_err(&format!("The number {t} cannot be converted to a BigInt because it is not an integer")));
                }
                Ok(h!(self, self.heap.new_bigint(bi::from_f64(n))))
            }
            BigFn::AsIntN | BigFn::AsUintN => {
                let bits = self.to_bit_width(a0)?;
                let v = self.to_bigint(a1)?;
                let max_bits = self.heap.limits().max_big_limbs as u64 * 64;
                let mut buf = [0u64; 1];
                let r = {
                    let a = self.heap.bigint_sv(v, &mut buf).map_err(Error::Heap)?;
                    if f == BigFn::AsIntN { bi::as_int_n(a, bits, max_bits) } else { bi::as_uint_n(a, bits, max_bits) }
                };
                match r {
                    Some(i) => Ok(h!(self, self.heap.new_bigint(i))),
                    None => Err(Error::Limit(LimitKind::Heap(Limit::BigLimbs))),
                }
            }
            BigFn::ToString => {
                let v = self.this_bigint(this, "toString")?;
                let radix = match a0 {
                    Value::Undefined => 10,
                    r => {
                        let n = self.coerce_number(r)?;
                        let n = if n.is_nan() { 0.0 } else { n.trunc() };
                        if !(2.0..=36.0).contains(&n) {
                            return Err(self.range_err("toString() radix argument must be between 2 and 36"));
                        }
                        n as u32
                    }
                };
                let limbs = self.big_limbs(v) as u64;
                self.spend(if radix.is_power_of_two() { limbs / 64 } else { limbs * limbs / 128 })?;
                Ok(Value::String(h!(self, self.heap.bigint_to_string(v, radix))))
            }
            BigFn::ValueOf => self.this_bigint(this, "valueOf"),
            BigFn::ToLocaleString => {
                let v = self.this_bigint(this, "toLocaleString")?;
                match v {
                    Value::BigInt(n) if args.is_empty() && n.unsigned_abs() < 1000 => Ok(Value::String(h!(self, self.heap.bigint_to_string(v, 10)))),
                    _ => Err(unsupported("BigInt.prototype.toLocaleString of more than 3 digits, or with arguments (needs Intl.NumberFormat)", Span::default())),
                }
            }
        }
    }
}
