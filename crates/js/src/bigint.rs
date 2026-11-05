//! Arbitrary-precision integers for the BigInt primitive. Pure arithmetic: no heap, no JavaScript.
//!
//! A magnitude is a little-endian `[u64]` with no zero limb at the top (zero is the empty slice). A signed value is a
//! sign and a magnitude (`Sv` when borrowed, `Int` when owned). Zero is never negative. A value that fits in `i64` is
//! kept inline in `Value::BigInt`; only the others reach this module as `Int` (see `heap_big.rs`).
//!
//! Algorithms (JS: all of them are the schoolbook or textbook ones, no tricks):
//! - add and subtract: one pass with carry.
//! - multiply: schoolbook below `KARATSUBA` limbs, Karatsuba above. An unbalanced pair is cut in pieces of the short size.
//! - divide: one limb divides with a precomputed reciprocal (Moller and Granlund, "Improved division by invariant integers",
//!   algorithm 4), so no hardware 128-bit division per limb. More limbs use Knuth algorithm D (TAOCP 4.3.1).
//! - bitwise operators: two's complement on `len + 1` limbs, then back to sign and magnitude.
//! - shifts: right shift of a negative value rounds down (towards minus infinity), as the language says.
//! - text: a power-of-two radix reads or writes bit groups. Other radixes divide or multiply by the largest power of the
//!   radix that fits one limb, so the cost is quadratic in the size.
//! - conversion to `f64` rounds to nearest, ties to even, with a sticky bit for the dropped limbs.
//!
//! Size caps are not here. The caller checks the size of a result (`bit_len`, limb counts) before it calls an operation.

use std::cmp::Ordering;

/// Limbs at which multiplication switches from schoolbook to Karatsuba. Measured in `docs/resources.md`.
pub const KARATSUBA: usize = 32;

/// An owned signed integer. `mag` has no zero at the top. Zero has an empty `mag` and `neg == false`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Int {
    pub neg: bool,
    pub mag: Vec<u64>,
}

/// A borrowed signed integer with the same rules as [`Int`].
#[derive(Clone, Copy, Debug)]
pub struct Sv<'a> {
    pub neg: bool,
    pub mag: &'a [u64],
}

impl<'a> Sv<'a> {
    pub fn is_zero(&self) -> bool {
        self.mag.is_empty()
    }
    pub fn negated(self) -> Sv<'a> {
        Sv { neg: !self.neg && !self.mag.is_empty(), mag: self.mag }
    }
}

/// `v` as a borrowed value. `buf` holds the one limb.
pub fn small(v: i64, buf: &mut [u64; 1]) -> Sv<'_> {
    buf[0] = v.unsigned_abs();
    Sv { neg: v < 0, mag: &buf[..(v != 0) as usize] }
}

impl Int {
    pub fn zero() -> Int {
        Int::default()
    }
    pub fn from_i64(v: i64) -> Int {
        Int { neg: v < 0, mag: if v == 0 { Vec::new() } else { vec![v.unsigned_abs()] } }
    }
    pub fn from_u64(v: u64) -> Int {
        Int { neg: false, mag: if v == 0 { Vec::new() } else { vec![v] } }
    }
    pub fn sv(&self) -> Sv<'_> {
        Sv { neg: self.neg, mag: &self.mag }
    }
    /// Sign and magnitude, with the zero rule applied.
    pub fn new(neg: bool, mag: Vec<u64>) -> Int {
        let mag = trim(mag);
        Int { neg: neg && !mag.is_empty(), mag }
    }
    /// The value as `i64` if it fits.
    pub fn to_i64(&self) -> Option<i64> {
        to_i64(self.sv())
    }
}

/// The value as `i64` if it fits.
pub fn to_i64(a: Sv) -> Option<i64> {
    match a.mag {
        [] => Some(0),
        [m] if !a.neg && *m <= i64::MAX as u64 => Some(*m as i64),
        [m] if a.neg && *m <= 1 << 63 => Some((*m as i64).wrapping_neg()),
        _ => None,
    }
}

/// The low 64 bits of the two's complement form (`BigInt.asUintN(64, a)`).
pub fn low_u64(a: Sv) -> u64 {
    let m = a.mag.first().copied().unwrap_or(0);
    if a.neg { m.wrapping_neg() } else { m }
}

pub fn trim(mut v: Vec<u64>) -> Vec<u64> {
    while v.last() == Some(&0) {
        v.pop();
    }
    v
}

pub fn trimmed(s: &[u64]) -> &[u64] {
    let mut n = s.len();
    while n > 0 && s[n - 1] == 0 {
        n -= 1;
    }
    &s[..n]
}

/// Number of bits of the magnitude (0 for zero).
pub fn bit_len(m: &[u64]) -> u64 {
    match m.last() {
        None => 0,
        Some(&t) => m.len() as u64 * 64 - t.leading_zeros() as u64,
    }
}

pub fn cmp_mag(a: &[u64], b: &[u64]) -> Ordering {
    a.len().cmp(&b.len()).then_with(|| a.iter().rev().cmp(b.iter().rev()))
}

pub fn cmp(a: Sv, b: Sv) -> Ordering {
    match (a.neg, b.neg) {
        (false, true) => Ordering::Greater,
        (true, false) => Ordering::Less,
        (false, false) => cmp_mag(a.mag, b.mag),
        (true, true) => cmp_mag(b.mag, a.mag),
    }
}

// ---------------------------------------------------------------- magnitudes

pub fn add_mag(a: &[u64], b: &[u64]) -> Vec<u64> {
    let (a, b) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let mut out = vec![0u64; a.len() + 1];
    let mut carry = 0u64;
    for ((o, &x), &y) in out.iter_mut().zip(a).zip(b) {
        let t = x as u128 + y as u128 + carry as u128;
        *o = t as u64;
        carry = (t >> 64) as u64;
    }
    for (o, &x) in out[b.len()..].iter_mut().zip(&a[b.len()..]) {
        let (s, c) = x.overflowing_add(carry);
        *o = s;
        carry = c as u64;
    }
    out[a.len()] = carry;
    trim(out)
}

/// `t -= x` in place. Needs `t >= x` as numbers and `x` trimmed.
fn sub_assign(t: &mut [u64], x: &[u64]) {
    let mut borrow = 0u64;
    for (o, &y) in t.iter_mut().zip(x) {
        let (d1, b1) = o.overflowing_sub(y);
        let (d2, b2) = d1.overflowing_sub(borrow);
        *o = d2;
        borrow = (b1 | b2) as u64;
    }
    for o in t[x.len()..].iter_mut() {
        if borrow == 0 {
            break;
        }
        let (d, b) = o.overflowing_sub(borrow);
        *o = d;
        borrow = b as u64;
    }
    debug_assert!(borrow == 0);
}

/// `a - b`. Needs `a >= b`.
pub fn sub_mag(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut out = a.to_vec();
    sub_assign(&mut out, trimmed(b));
    trim(out)
}

/// `out[off..] += x`, with carry. The carry must not run past the end of `out`.
fn add_into(out: &mut [u64], off: usize, x: &[u64]) {
    let mut carry = 0u64;
    let (head, tail) = out[off..].split_at_mut(x.len());
    for (o, &y) in head.iter_mut().zip(x) {
        let t = *o as u128 + y as u128 + carry as u128;
        *o = t as u64;
        carry = (t >> 64) as u64;
    }
    for o in tail.iter_mut() {
        if carry == 0 {
            break;
        }
        let (s, c) = o.overflowing_add(carry);
        *o = s;
        carry = c as u64;
    }
    debug_assert!(carry == 0);
}

/// `out += a * b` for one row: `out[..a.len()] += a * m`, the carry goes to `out[a.len()]`.
#[inline(always)]
fn mac_row(out: &mut [u64], a: &[u64], m: u64) {
    let (lo, hi) = out.split_at_mut(a.len());
    let mut carry = 0u64;
    for (o, &x) in lo.iter_mut().zip(a) {
        let t = x as u128 * m as u128 + *o as u128 + carry as u128;
        *o = t as u64;
        carry = (t >> 64) as u64;
    }
    hi[0] = carry;
}

fn mul_school(out: &mut [u64], a: &[u64], b: &[u64]) {
    for (i, &bi) in b.iter().enumerate() {
        if bi != 0 {
            mac_row(&mut out[i..], a, bi);
        }
    }
}

/// `out = x * y`. `out` is zero and has `x.len() + y.len()` limbs. The operands may have zeros at the top.
fn mul_into(out: &mut [u64], x: &[u64], y: &[u64]) {
    let (a, b) = if x.len() >= y.len() { (x, y) } else { (y, x) };
    if b.len() < KARATSUBA {
        mul_school(out, a, b);
    } else if a.len() >= 2 * b.len() {
        let mut tmp = vec![0u64; 2 * b.len()];
        for (k, chunk) in a.chunks(b.len()).enumerate() {
            let t = &mut tmp[..chunk.len() + b.len()];
            t.fill(0);
            mul_into(t, chunk, b);
            add_into(out, k * b.len(), trimmed(t));
        }
    } else {
        // Karatsuba: a = a1 B^h + a0, b = b1 B^h + b0, with B = 2^64.
        let h = a.len() / 2;
        let (a0, a1) = a.split_at(h);
        let (b0, b1) = b.split_at(h);
        let (lo, hi) = out.split_at_mut(2 * h);
        mul_into(lo, a0, b0);
        mul_into(hi, a1, b1);
        let sa = add_mag(trimmed(a0), a1);
        let sb = add_mag(trimmed(b0), b1);
        let mut t = vec![0u64; sa.len() + sb.len()];
        mul_into(&mut t, &sa, &sb);
        sub_assign(&mut t, trimmed(lo));
        sub_assign(&mut t, trimmed(hi));
        add_into(out, h, trimmed(&t));
    }
}

pub fn mul_mag(a: &[u64], b: &[u64]) -> Vec<u64> {
    let (a, b) = (trimmed(a), trimmed(b));
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u64; a.len() + b.len()];
    mul_into(&mut out, a, b);
    trim(out)
}

/// A divisor of one limb, prepared: `d << shift` has the top bit set and `inv` is its reciprocal.
pub struct Div1 {
    d: u64,
    shift: u32,
    inv: u64,
}

impl Div1 {
    pub fn new(d: u64) -> Div1 {
        debug_assert!(d != 0);
        let shift = d.leading_zeros();
        let dn = d << shift;
        let inv = ((((!dn) as u128) << 64 | u64::MAX as u128) / dn as u128) as u64;
        Div1 { d: dn, shift, inv }
    }
    /// Divides `u1:u0` by the divisor. Needs `u1 < d` (normalized).
    #[inline(always)]
    fn div2by1(&self, u1: u64, u0: u64) -> (u64, u64) {
        let q = self.inv as u128 * u1 as u128 + (((u1 as u128) << 64) | u0 as u128);
        let (mut q1, q0) = ((q >> 64) as u64, q as u64);
        q1 = q1.wrapping_add(1);
        let mut r = u0.wrapping_sub(q1.wrapping_mul(self.d));
        if r > q0 {
            q1 = q1.wrapping_sub(1);
            r = r.wrapping_add(self.d);
        }
        if r >= self.d {
            q1 = q1.wrapping_add(1);
            r -= self.d;
        }
        (q1, r)
    }
    /// `a = a / d` in place (not trimmed). Returns the remainder.
    pub fn div_in_place(&self, a: &mut [u64]) -> u64 {
        let s = self.shift;
        let n = a.len();
        if n == 0 {
            return 0;
        }
        let mut rem = if s == 0 { 0 } else { a[n - 1] >> (64 - s) };
        for i in (0..n).rev() {
            let lo = (a[i] << s) | if s > 0 && i > 0 { a[i - 1] >> (64 - s) } else { 0 };
            let (q, r) = self.div2by1(rem, lo);
            a[i] = q;
            rem = r;
        }
        rem >> s
    }
}

pub fn divrem_u64(a: &[u64], d: u64) -> (Vec<u64>, u64) {
    let mut q = a.to_vec();
    let r = Div1::new(d).div_in_place(&mut q);
    (trim(q), r)
}

fn shl_small(a: &[u64], s: u32, extra: bool) -> Vec<u64> {
    let mut out = Vec::with_capacity(a.len() + extra as usize);
    let mut carry = 0u64;
    for &x in a {
        out.push(if s == 0 { x } else { (x << s) | carry });
        carry = if s == 0 { 0 } else { x >> (64 - s) };
    }
    if extra {
        out.push(carry);
    }
    out
}

/// Quotient and remainder of magnitudes. `b` is not zero.
pub fn divrem_mag(a: &[u64], b: &[u64]) -> (Vec<u64>, Vec<u64>) {
    let (a, b) = (trimmed(a), trimmed(b));
    assert!(!b.is_empty(), "division by zero");
    if cmp_mag(a, b) == Ordering::Less {
        return (Vec::new(), a.to_vec());
    }
    if b.len() == 1 {
        let (q, r) = divrem_u64(a, b[0]);
        return (q, if r == 0 { Vec::new() } else { vec![r] });
    }
    // Knuth D. Shift so that the top limb of the divisor has its top bit set.
    let s = b[b.len() - 1].leading_zeros();
    let v = shl_small(b, s, false);
    let mut u = shl_small(a, s, true);
    let n = v.len();
    let m = a.len() - n;
    let mut q = vec![0u64; m + 1];
    let (v1, v2) = (v[n - 1] as u128, v[n - 2] as u128);
    for j in (0..=m).rev() {
        let num = ((u[j + n] as u128) << 64) | u[j + n - 1] as u128;
        let mut qhat = num / v1;
        let mut rhat = num % v1;
        while qhat >> 64 != 0 || qhat * v2 > ((rhat << 64) | u[j + n - 2] as u128) {
            qhat -= 1;
            rhat += v1;
            if rhat >> 64 != 0 {
                break;
            }
        }
        let qhat = qhat as u64;
        // u[j..=j+n] -= qhat * v
        let (mut carry, mut acc) = (0u64, 0i128);
        let (uj, utop) = u[j..=j + n].split_at_mut(n);
        for (x, &y) in uj.iter_mut().zip(&v) {
            let p = qhat as u128 * y as u128 + carry as u128;
            carry = (p >> 64) as u64;
            let t = *x as i128 - (p as u64) as i128 + acc;
            *x = t as u64;
            acc = t >> 64;
        }
        let t = utop[0] as i128 - carry as i128 + acc;
        utop[0] = t as u64;
        let (b1, b2) = (t < 0, false);
        let mut qhat = qhat;
        if b1 | b2 {
            // qhat was one too big: add the divisor back.
            qhat = qhat.wrapping_sub(1);
            let mut c = 0u64;
            for (x, &y) in uj.iter_mut().zip(&v) {
                let t = *x as u128 + y as u128 + c as u128;
                *x = t as u64;
                c = (t >> 64) as u64;
            }
            utop[0] = utop[0].wrapping_add(c);
        }
        q[j] = qhat;
    }
    u.truncate(n);
    let r = trim(shr_mag(&u, s as u64));
    (trim(q), r)
}

pub fn shl_mag(a: &[u64], bits: u64) -> Vec<u64> {
    let a = trimmed(a);
    if a.is_empty() {
        return Vec::new();
    }
    let (limbs, s) = ((bits / 64) as usize, (bits % 64) as u32);
    let mut out = vec![0u64; limbs];
    out.extend(shl_small(a, s, true));
    trim(out)
}

/// `a >> bits` on a magnitude (rounds towards zero).
pub fn shr_mag(a: &[u64], bits: u64) -> Vec<u64> {
    let limbs = bits / 64;
    if limbs >= a.len() as u64 {
        return Vec::new();
    }
    let (limbs, s) = (limbs as usize, (bits % 64) as u32);
    let src = &a[limbs..];
    let mut out = Vec::with_capacity(src.len());
    for i in 0..src.len() {
        let hi = if i + 1 < src.len() { src[i + 1] } else { 0 };
        out.push(if s == 0 { src[i] } else { (src[i] >> s) | (hi << (64 - s)) });
    }
    trim(out)
}

/// `base ^ exp` on a magnitude. The caller has checked the size of the result.
pub fn pow_mag(base: &[u64], mut exp: u64) -> Vec<u64> {
    let base = trimmed(base);
    if exp == 0 {
        return vec![1];
    }
    if base.is_empty() {
        return Vec::new();
    }
    if base == [1] {
        return vec![1];
    }
    if base.iter().take(base.len() - 1).all(|&x| x == 0) && base[base.len() - 1].is_power_of_two() {
        // A power of two: a shift.
        let k = bit_len(base) - 1;
        return shl_mag(&[1], k * exp);
    }
    let mut result = vec![1u64];
    let mut b = base.to_vec();
    loop {
        if exp & 1 == 1 {
            result = mul_mag(&result, &b);
        }
        exp >>= 1;
        if exp == 0 {
            return result;
        }
        b = mul_mag(&b, &b);
    }
}

// ---------------------------------------------------------------- signed operations

pub fn add(a: Sv, b: Sv) -> Int {
    if a.neg == b.neg {
        return Int::new(a.neg, add_mag(a.mag, b.mag));
    }
    match cmp_mag(a.mag, b.mag) {
        Ordering::Equal => Int::zero(),
        Ordering::Greater => Int::new(a.neg, sub_mag(a.mag, b.mag)),
        Ordering::Less => Int::new(b.neg, sub_mag(b.mag, a.mag)),
    }
}

pub fn sub(a: Sv, b: Sv) -> Int {
    add(a, b.negated())
}

pub fn mul(a: Sv, b: Sv) -> Int {
    Int::new(a.neg != b.neg, mul_mag(a.mag, b.mag))
}

/// Truncating division and the remainder (it has the sign of `a`). `b` is not zero.
pub fn divrem(a: Sv, b: Sv) -> (Int, Int) {
    let (q, r) = divrem_mag(a.mag, b.mag);
    (Int::new(a.neg != b.neg, q), Int::new(a.neg, r))
}

pub fn pow(a: Sv, exp: u64) -> Int {
    Int::new(a.neg && exp & 1 == 1, pow_mag(a.mag, exp))
}

pub fn shl(a: Sv, bits: u64) -> Int {
    Int::new(a.neg, shl_mag(a.mag, bits))
}

/// `a >> bits`, rounding towards minus infinity.
pub fn shr(a: Sv, bits: u64) -> Int {
    if !a.neg {
        return Int::new(false, shr_mag(a.mag, bits));
    }
    // -((|a| - 1) >> bits) - 1
    let m = shr_mag(&sub_mag(a.mag, &[1]), bits);
    Int::new(true, add_mag(&m, &[1]))
}

/// The two's complement of `a` on `len` limbs. `len` is greater than the limbs of `a`.
fn twos(a: Sv, len: usize) -> Vec<u64> {
    let mut v = a.mag.to_vec();
    v.resize(len, 0);
    if a.neg {
        let mut carry = true;
        for x in v.iter_mut() {
            let (s, c) = (!*x).overflowing_add(carry as u64);
            *x = s;
            carry = c;
        }
    }
    v
}

fn from_twos(mut v: Vec<u64>) -> Int {
    let neg = v.last().is_some_and(|&t| t >> 63 == 1);
    if neg {
        let mut carry = true;
        for x in v.iter_mut() {
            let (s, c) = (!*x).overflowing_add(carry as u64);
            *x = s;
            carry = c;
        }
    }
    Int::new(neg, v)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bit {
    And,
    Or,
    Xor,
}

pub fn bitop(op: Bit, a: Sv, b: Sv) -> Int {
    let len = a.mag.len().max(b.mag.len()) + 1;
    let (mut x, y) = (twos(a, len), twos(b, len));
    for (p, q) in x.iter_mut().zip(y) {
        *p = match op {
            Bit::And => *p & q,
            Bit::Or => *p | q,
            Bit::Xor => *p ^ q,
        };
    }
    from_twos(x)
}

/// `~a`, that is `-a - 1`.
pub fn not(a: Sv) -> Int {
    let one = [1u64];
    sub(a.negated(), Sv { neg: false, mag: &one })
}

/// `BigInt.asUintN(bits, a)`. `None` when the result would have more than `max_bits` bits.
pub fn as_uint_n(a: Sv, bits: u64, max_bits: u64) -> Option<Int> {
    if !a.neg && bit_len(a.mag) <= bits {
        return Some(Int::new(false, a.mag.to_vec()));
    }
    if a.neg && bits > max_bits {
        return None;
    }
    // The low `bits` bits of the magnitude.
    let (limbs, s) = ((bits / 64) as usize, (bits % 64) as u32);
    let mut low: Vec<u64> = a.mag.iter().take(limbs + 1).copied().collect();
    if low.len() > limbs {
        low[limbs] &= if s == 0 { 0 } else { (1u64 << s) - 1 };
    }
    let low = trim(low);
    if !a.neg || low.is_empty() {
        return Some(Int::new(false, low));
    }
    // 2^bits - low
    Some(Int::new(false, sub_mag(&shl_mag(&[1], bits), &low)))
}

/// `BigInt.asIntN(bits, a)`. `None` when the result would have more than `max_bits` bits.
pub fn as_int_n(a: Sv, bits: u64, max_bits: u64) -> Option<Int> {
    if bits == 0 {
        return Some(Int::zero());
    }
    if bit_len(a.mag) < bits {
        return Some(Int::new(a.neg, a.mag.to_vec()));
    }
    let r = as_uint_n(a, bits, max_bits)?;
    let top = bits - 1;
    let set = r.mag.get((top / 64) as usize).is_some_and(|w| w >> (top % 64) & 1 == 1);
    if !set {
        return Some(r);
    }
    // r - 2^bits is negative: its magnitude is 2^bits - r.
    Some(Int::new(true, sub_mag(&shl_mag(&[1], bits), &r.mag)))
}

// ---------------------------------------------------------------- numbers

/// The nearest `f64` (ties to even). Infinity when the value is too large.
pub fn to_f64(a: Sv) -> f64 {
    let m = a.mag;
    let v = match m.len() {
        0 => 0.0,
        1 => m[0] as f64,
        _ => {
            let bl = bit_len(m);
            let shift = bl - 64;
            let top = shr_mag(m, shift);
            let mut top64 = top[0];
            // Sticky bit: any 1 below the kept 64 bits.
            let (limbs, s) = ((shift / 64) as usize, (shift % 64) as u32);
            let sticky = m[..limbs].iter().any(|&x| x != 0) || (s > 0 && m[limbs] & ((1u64 << s) - 1) != 0);
            top64 |= sticky as u64;
            if shift > 1100 { f64::INFINITY } else { top64 as f64 * 2f64.powi(shift as i32) }
        }
    };
    if a.neg { -v } else { v }
}

/// The integer `x` (finite, no fraction) as an `Int`.
pub fn from_f64(x: f64) -> Int {
    debug_assert!(x.is_finite() && x == x.trunc());
    let bits = x.to_bits();
    let e = ((bits >> 52) & 0x7ff) as i64;
    if e == 0 {
        return Int::zero(); // a finite integer-valued f64 with e == 0 is zero
    }
    let m = (bits & ((1 << 52) - 1)) | (1 << 52);
    let sh = e - 1075;
    let mag = if sh >= 0 { shl_mag(&[m], sh as u64) } else { trim(vec![m >> (-sh) as u32]) };
    Int::new(x < 0.0, mag)
}

/// Compare with a number. `None` for NaN.
pub fn cmp_f64(a: Sv, x: f64) -> Option<Ordering> {
    if x.is_nan() {
        return None;
    }
    if x == f64::INFINITY {
        return Some(Ordering::Less);
    }
    if x == f64::NEG_INFINITY {
        return Some(Ordering::Greater);
    }
    let t = x.trunc();
    let ti = from_f64(t);
    match cmp(a, ti.sv()) {
        Ordering::Equal => Some(if x > t {
            Ordering::Less
        } else if x < t {
            Ordering::Greater
        } else {
            Ordering::Equal
        }),
        o => Some(o),
    }
}

// ---------------------------------------------------------------- text

const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// Digits per limb chunk and the chunk base (`radix ^ digits`, the largest power below 2^64).
fn chunk(radix: u32) -> (usize, u64) {
    let (mut n, mut p) = (0usize, 1u64);
    while let Some(q) = p.checked_mul(radix as u64) {
        p = q;
        n += 1;
    }
    (n, p)
}

/// Limbs at which `to_string_radix` switches from repeated division by one limb to divide and conquer.
const DC_LIMBS: usize = 32;

/// The digits of `w` (little-endian, least significant first), padded with zeros to at least `pad` digits.
fn small_digits(mut w: Vec<u64>, radix: u32, n: usize, base: u64, out: &mut Vec<u8>, pad: usize) {
    let start = out.len();
    let div = Div1::new(base);
    while !w.is_empty() {
        let mut r = div.div_in_place(&mut w);
        while w.last() == Some(&0) {
            w.pop();
        }
        for _ in 0..n {
            out.push(DIGITS[(r % radix as u64) as usize]);
            r /= radix as u64;
            if w.is_empty() && r == 0 {
                break;
            }
        }
    }
    while out.len() < start + pad {
        out.push(b'0');
    }
}

/// Digits of `num`, which is below `bases[k]^2`, as exactly `2 * bases[k].1` digits (zero padded).
fn dc_digits(num: &[u64], radix: u32, n: usize, base: u64, bases: &[(Vec<u64>, usize)], k: usize, out: &mut Vec<u8>) {
    let (big, digits) = &bases[k];
    if num.len() < 8 || k == 0 {
        small_digits(trimmed(num).to_vec(), radix, n, base, out, 2 * digits);
        return;
    }
    let (hi, lo) = divrem_mag(num, big);
    dc_digits(&lo, radix, n, base, bases, k - 1, out);
    dc_digits(&hi, radix, n, base, bases, k - 1, out);
}

/// `Number.prototype.toString(radix)` for a BigInt: lower-case digits, a leading `-` for a negative value.
pub fn to_string_radix(a: Sv, radix: u32) -> String {
    debug_assert!((2..=36).contains(&radix));
    if a.mag.is_empty() {
        return "0".into();
    }
    let mut out: Vec<u8> = Vec::new(); // digits, least significant first
    if radix.is_power_of_two() {
        let k = radix.trailing_zeros() as u64;
        let total = bit_len(a.mag);
        let mut pos = 0;
        while pos < total {
            let (w, o) = ((pos / 64) as usize, (pos % 64) as u32);
            let mut d = a.mag[w] >> o;
            if o as u64 + k > 64 && w + 1 < a.mag.len() {
                d |= a.mag[w + 1] << (64 - o);
            }
            out.push(DIGITS[(d & (radix as u64 - 1)) as usize]);
            pos += k;
        }
        while out.len() > 1 && out.last() == Some(&b'0') {
            out.pop();
        }
    } else {
        let (n, base) = chunk(radix);
        out.reserve((bit_len(a.mag) as f64 / (radix as f64).log2()) as usize + 2);
        if a.mag.len() < DC_LIMBS {
            small_digits(a.mag.to_vec(), radix, n, base, &mut out, 0);
        } else {
            // Divide and conquer: powers base^(2^k), until one has more than half the limbs.
            let mut bases: Vec<(Vec<u64>, usize)> = vec![(vec![base], n)];
            while bases.last().unwrap().0.len() <= a.mag.len() / 2 + 1 {
                let (b, d) = bases.last().unwrap();
                bases.push((mul_mag(b, b), d * 2));
            }
            dc_digits(a.mag, radix, n, base, &bases, bases.len() - 1, &mut out);
        }
        while out.len() > 1 && out.last() == Some(&b'0') {
            out.pop();
        }
    }
    if a.neg {
        out.push(b'-');
    }
    out.reverse();
    // Only ASCII digits and `-`.
    String::from_utf8(out).unwrap()
}

/// The value of `digits` in `radix`. Every byte must be a digit of the radix (the caller checked). Empty gives zero.
pub fn parse_radix(digits: &[u8], radix: u32) -> Int {
    let val = |c: u8| (c as char).to_digit(36).unwrap();
    let mut mag: Vec<u64> = Vec::new();
    if radix.is_power_of_two() {
        let k = radix.trailing_zeros() as u64;
        let total = digits.len() as u64 * k;
        mag = vec![0u64; total.div_ceil(64) as usize];
        for (i, &c) in digits.iter().rev().enumerate() {
            let pos = i as u64 * k;
            let (w, o) = ((pos / 64) as usize, (pos % 64) as u32);
            let d = val(c) as u64;
            mag[w] |= d << o;
            if o as u64 + k > 64 {
                mag[w + 1] |= d >> (64 - o);
            }
        }
        return Int::new(false, mag);
    }
    let (n, _) = chunk(radix);
    for part in digits.chunks(n) {
        let mut mulv = 1u64;
        let mut acc = 0u64;
        for &c in part {
            acc = acc * radix as u64 + val(c) as u64;
            mulv *= radix as u64;
        }
        // mag = mag * mulv + acc
        let mut carry = acc;
        for x in mag.iter_mut() {
            let t = *x as u128 * mulv as u128 + carry as u128;
            *x = t as u64;
            carry = (t >> 64) as u64;
        }
        if carry != 0 {
            mag.push(carry);
        }
    }
    Int::new(false, mag)
}

/// StringToBigInt (section 7.1.14): trim white space; empty is zero; `0x`, `0o` or `0b` and digits (no sign); or an optional
/// sign and decimal digits. Anything else (a point, an exponent, a separator, a trailing `n`) is `None`.
pub fn string_to_bigint(units: &[u16]) -> Option<Int> {
    use crate::value::is_js_space;
    let start = units.iter().position(|&u| !is_js_space(u)).unwrap_or(units.len());
    let end = units.iter().rposition(|&u| !is_js_space(u)).map_or(start, |e| e + 1);
    let t = &units[start..end];
    if t.is_empty() {
        return Some(Int::zero());
    }
    let ascii: Vec<u8> = t.iter().map(|&u| if u < 128 { u as u8 } else { 0 }).collect();
    let (neg, radix, digits) = match ascii.as_slice() {
        [b'0', b'x' | b'X', rest @ ..] => (false, 16, rest),
        [b'0', b'o' | b'O', rest @ ..] => (false, 8, rest),
        [b'0', b'b' | b'B', rest @ ..] => (false, 2, rest),
        [b'-', rest @ ..] => (true, 10, rest),
        [b'+', rest @ ..] => (false, 10, rest),
        all => (false, 10, all),
    };
    if digits.is_empty() || !digits.iter().all(|&c| (c as char).to_digit(radix).is_some()) {
        return None;
    }
    let mut i = parse_radix(digits, radix);
    i.neg = neg && !i.mag.is_empty();
    Some(i)
}

/// The value of a BigInt literal (`10n`, `0xFFn`, `1_000n`, with the `n`). Acorn's rules are the lexer's: the text is valid.
pub fn parse_literal(text: &str) -> Option<Int> {
    let t = text.strip_suffix('n')?;
    let clean: Vec<u8> = t.bytes().filter(|&c| c != b'_').collect();
    let (radix, digits) = match clean.as_slice() {
        [b'0', b'x' | b'X', rest @ ..] => (16, rest),
        [b'0', b'o' | b'O', rest @ ..] => (8, rest),
        [b'0', b'b' | b'B', rest @ ..] => (2, rest),
        all => (10, all),
    };
    if digits.is_empty() || !digits.iter().all(|&c| (c as char).to_digit(radix).is_some()) {
        return None;
    }
    Some(parse_radix(digits, radix))
}

/// A 64-bit hash of the value, for the heap table.
pub fn hash(a: Sv) -> u32 {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let mut h: u64 = a.mag.len() as u64 ^ (a.neg as u64) << 63;
    for &x in a.mag {
        h = (h.rotate_left(5) ^ x).wrapping_mul(K);
    }
    h ^= h >> 32;
    (h.wrapping_mul(K) >> 32) as u32
}

#[cfg(test)]
#[path = "bigint_tests.rs"]
mod tests;
