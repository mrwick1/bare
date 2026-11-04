// Numeric builtins. Included by `eval.rs` (`include!`), so it shares that module's imports and private items.
// Every native here is `Nat::Ao(AoFn::..)`; `call_ao` forwards to `call_numeric`.
//
// Supported
// - `Math.clz32` (ToUint32, then leading zeros), `Math.log` (`f64::ln`: log(-0) = -Infinity, log(x<0) = NaN),
//   `Math.LN2` (writable false, enumerable false, configurable false).
// - `Math.random`: xorshift128+ state per realm (`AoState::rng`). The state is seeded once, at the first call, with
//   16 bytes from `/dev/urandom`. A failed read, or an all-zero seed, is an explicit `Unsupported`; there is no time
//   or PID fallback. The state is not cryptographic and nothing else reads it. Each call does no allocation.
//   Tests set `rng` directly to get a fixed sequence.
// - `Number.prototype.toString(radix)` for primitive numbers and the intrinsic prototype's zero value.
//   Radix 10 uses `number_to_string`. The other radixes follow V8's `DoubleToRadixCString`: the spec leaves non-10
//   radixes to the implementation, and V8's digit rule (digits up to half the gap to the next double, round-half-even,
//   integer part divided by the radix while the quotient is >= 2^53) is what Node shows. The output is at most ~1100 characters.
//   `Number.prototype` is `AoState::number_proto`: a GC root, so it survives `delete globalThis.Number`.
//   Property reads on a number primitive search it (`get_prop`).
// Not supported: other Number methods, `Number.prototype` on wrapper objects, `Object(5)`.

/// Longest non-10 text: 1024 integer digits (radix 2), 1074 fraction digits, sign and point.
const RADIX_BUF: usize = 2200;
const RADIX_DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// Digits of `x` (finite, any sign) in `radix` (2..=36, not 10) into `buf`; the text is `buf[start..end]`.
/// Ported from V8 12.4.254 `src/numbers/conversions.cc`, `DoubleToRadixCString`.
/// Copyright 2011 the V8 project authors. All rights reserved.
/// See `../LICENSE-V8` for the BSD-3-Clause terms.
/// Source: https://github.com/v8/v8/blob/12.4.254/src/numbers/conversions.cc
fn radix_text(x: f64, radix: u32, buf: &mut [u8; RADIX_BUF]) -> (usize, usize) {
    let r = radix as f64;
    let (mut int_at, mut frac_at) = (RADIX_BUF / 2, RADIX_BUF / 2);
    let negative = x < 0.0;
    let x = x.abs();
    let mut integer = x.floor();
    let mut fraction = x - integer;
    let next = f64::from_bits(x.to_bits() + 1); // x is finite, so this is the next double (or infinity at MAX)
    let mut delta = (0.5 * (next - x)).max(f64::from_bits(1));
    if fraction >= delta {
        buf[frac_at] = b'.';
        frac_at += 1;
        loop {
            fraction *= r;
            delta *= r;
            let digit = fraction as usize;
            buf[frac_at] = RADIX_DIGITS[digit];
            frac_at += 1;
            fraction -= digit as f64;
            if (fraction > 0.5 || (fraction == 0.5 && digit & 1 == 1)) && fraction + delta > 1.0 {
                // Round up, carrying back through the written digits.
                loop {
                    frac_at -= 1;
                    if frac_at == RADIX_BUF / 2 {
                        integer += 1.0; // carry into the integer part
                        break;
                    }
                    let c = buf[frac_at];
                    let d = if c > b'9' { c - b'a' + 10 } else { c - b'0' } as u32;
                    if d + 1 < radix {
                        buf[frac_at] = RADIX_DIGITS[(d + 1) as usize];
                        frac_at += 1;
                        break;
                    }
                }
                break;
            }
            if fraction < delta {
                break;
            }
        }
    }
    // Node 22 (V8 12.4) keeps dividing while the quotient still has 2^53 or more, and zero-fills the digits it drops.
    // (Fitted against Node over 29180 random large integers in radixes 2..36: no difference.)
    while integer / r >= 9007199254740992.0 {
        integer /= r;
        int_at -= 1;
        buf[int_at] = b'0';
    }
    loop {
        let rem = integer % r;
        int_at -= 1;
        buf[int_at] = RADIX_DIGITS[rem as usize];
        integer = (integer - rem) / r;
        if integer <= 0.0 {
            break;
        }
    }
    if negative {
        int_at -= 1;
        buf[int_at] = b'-';
    }
    (int_at, frac_at)
}

impl<'p> Interp<'p> {
    /// `Math.clz32`, `Math.log`, `Math.LN2`, `Math.random` and `Number.prototype.toString`.
    fn install_numeric(&mut self, math: ObjId) -> Res<()> {
        use AoFn::*;
        for (name, f, len) in [("clz32", MathClz32, 1), ("log", MathLog, 1), ("random", MathRandom, 0)] {
            self.method(math, name, Nat::Ao(f), len)?;
        }
        let k = h!(self, self.heap.key_from_str("LN2"));
        self.def(math, k, Value::Number(std::f64::consts::LN_2), false, false, false)?;
        let np = h!(self, self.heap.new_object(Some(self.object_proto)));
        self.ao.number_proto = Some(np);
        self.method(np, "toString", Nat::Ao(NumToString), 1)?;
        let kn = h!(self, self.heap.key_from_str("Number"));
        let Get::Value(Value::Object(nc)) = h!(self, self.heap.get(self.global, kn)) else { return Err(unsupported("Number constructor missing", Span::default())) };
        self.def(np, self.k_constructor, Value::Object(nc), true, false, true)?;
        self.def(nc, self.k_prototype, Value::Object(np), false, false, false)
    }

    fn call_numeric(&mut self, f: AoFn, this: Value, args: &[Value]) -> Res<Value> {
        match f {
            AoFn::MathClz32 => {
                let x = self.coerce_number(arg(args, 0))?;
                Ok(Value::Number(to_uint32(x).leading_zeros() as f64))
            }
            AoFn::MathLog => Ok(Value::Number(self.coerce_number(arg(args, 0))?.ln())),
            AoFn::MathRandom => self.math_random(),
            _ => self.number_to_string_radix(this, arg(args, 0)),
        }
    }

    fn math_random(&mut self) -> Res<Value> {
        let s = match &mut self.ao.rng {
            Some(s) => s,
            none => none.insert(os_seed().ok_or_else(|| unsupported("Math.random: the OS gave no random seed", Span::default()))?),
        };
        let (mut s1, s0) = (s[0], s[1]);
        s[0] = s0;
        s1 ^= s1 << 23;
        s1 ^= s1 >> 17;
        s1 ^= s0;
        s1 ^= s0 >> 26;
        s[1] = s1;
        Ok(Value::Number((s0.wrapping_add(s1) >> 11) as f64 * (1.0 / 9007199254740992.0)))
    }

    fn number_to_string_radix(&mut self, this: Value, radix: Value) -> Res<Value> {
        let x = match this {
            Value::Number(x) => x,
            Value::Object(o) if Some(o) == self.ao.number_proto => 0.0,
            _ => return Err(self.type_err("Number.prototype.toString requires that 'this' be a Number")),
        };
        let radix = if matches!(radix, Value::Undefined) { 10.0 } else { self.ao_int(radix)? };
        if !(2.0..=36.0).contains(&radix) {
            return Err(self.mk_err(RANGE_ERROR, "toString() radix must be between 2 and 36"));
        }
        let radix = radix as u32;
        if radix == 10 || !x.is_finite() || x == 0.0 {
            return Ok(Value::String(h!(self, self.heap.intern_str(&number_to_string(x)))));
        }
        let mut buf = [0u8; RADIX_BUF];
        let (a, b) = radix_text(x, radix, &mut buf);
        self.work(b - a, 5)?;
        let text = std::str::from_utf8(&buf[a..b]).expect("ASCII digits");
        Ok(Value::String(h!(self, self.heap.intern_str(text))))
    }

    /// GC roots of this module.
    fn numeric_roots(&self, v: &mut Vec<Value>) {
        v.extend(self.ao.number_proto.map(Value::Object));
    }
}

/// 16 bytes of OS randomness, never all zero. `None` when the source cannot be read.
fn os_seed() -> Option<[u64; 2]> {
    seed_from(&mut std::fs::File::open("/dev/urandom").ok()?)
}

fn seed_from(source: &mut impl std::io::Read) -> Option<[u64; 2]> {
    let mut b = [0u8; 16];
    source.read_exact(&mut b).ok()?;
    let s = [u64::from_le_bytes(b[..8].try_into().ok()?), u64::from_le_bytes(b[8..].try_into().ok()?)];
    (s != [0, 0]).then_some(s)
}
