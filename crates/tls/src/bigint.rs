//! Unsigned big integers for RSA public-key checks: Montgomery multiplication and modular
//! exponentiation with a small public exponent.
//!
//! Numbers are little-endian `u64` limbs on the stack, up to [`MAX_BITS`] bits. Nothing
//! allocates. This is for PUBLIC values only (signature, modulus, exponent), so it is NOT
//! constant time. No input makes it panic: a bad modulus or base gives `None`.

pub const MAX_BITS: usize = 4096;
const MAX_LIMBS: usize = MAX_BITS / 64;

/// An odd modulus `n` with 3 <= n < 2^4096, and the constants Montgomery multiplication needs.
pub struct Modulus {
    n: [u64; MAX_LIMBS],
    /// Number of limbs in use. `n[k - 1]` is not zero.
    k: usize,
    /// `-n^-1 mod 2^64`.
    n0: u64,
    /// `R^2 mod n` with `R = 2^(64 k)`.
    r2: [u64; MAX_LIMBS],
}

/// Big-endian bytes to limbs. `None` if the value (leading zeros ignored) needs over 4096 bits.
fn from_be(b: &[u8]) -> Option<([u64; MAX_LIMBS], usize)> {
    let skip = b.iter().take_while(|&&x| x == 0).count();
    let b = &b[skip..];
    if b.len() > MAX_BITS / 8 {
        return None;
    }
    let mut l = [0u64; MAX_LIMBS];
    for (i, &x) in b.iter().rev().enumerate() {
        l[i / 8] |= (x as u64) << (8 * (i % 8));
    }
    Some((l, b.len().div_ceil(8)))
}

/// Is `a >= b`? Both have `k` limbs.
fn ge(a: &[u64], b: &[u64]) -> bool {
    for i in (0..a.len()).rev() {
        if a[i] != b[i] {
            return a[i] > b[i];
        }
    }
    true
}

/// `a -= b` over `a.len()` limbs. The borrow out is returned.
fn sub_in_place(a: &mut [u64], b: &[u64]) -> bool {
    let mut borrow = false;
    for i in 0..a.len() {
        let (d, b1) = a[i].overflowing_sub(b[i]);
        let (d, b2) = d.overflowing_sub(borrow as u64);
        a[i] = d;
        borrow = b1 | b2;
    }
    borrow
}

impl Modulus {
    /// `be` is big-endian. `None` if it is even, below 3, or over 4096 bits.
    pub fn new(be: &[u8]) -> Option<Modulus> {
        let (n, k) = from_be(be)?;
        if k == 0 || n[0] & 1 == 0 || (k == 1 && n[0] < 3) {
            return None;
        }
        // Newton iteration for n^-1 mod 2^64: each step doubles the correct bits.
        let mut inv = 1u64;
        for _ in 0..6 {
            inv = inv.wrapping_mul(2u64.wrapping_sub(n[0].wrapping_mul(inv)));
        }
        let mut m = Modulus { n, k, n0: inv.wrapping_neg(), r2: [0; MAX_LIMBS] };
        // x = R mod n: start at 2^(bits - 1), which is below n, and double up to 2^(64 k).
        let bits = 64 * k - n[k - 1].leading_zeros() as usize;
        let mut x = [0u64; MAX_LIMBS];
        x[(bits - 1) / 64] = 1 << ((bits - 1) % 64);
        for _ in 0..64 * k - bits + 1 {
            m.double(&mut x);
        }
        // R^2 = (2 R)^(64 k) in Montgomery form.
        m.double(&mut x);
        let mut r2 = x;
        let e = 64 * k;
        for i in (0..usize::BITS - 1 - e.leading_zeros()).rev() {
            let t = r2;
            m.mul(&t, &t, &mut r2);
            if e >> i & 1 == 1 {
                let t = r2;
                m.mul(&t, &x, &mut r2);
            }
        }
        m.r2 = r2;
        Some(m)
    }

    /// Bit length of the modulus.
    pub fn bits(&self) -> usize {
        64 * self.k - self.n[self.k - 1].leading_zeros() as usize
    }

    /// `x = 2 x mod n` for `x < n`.
    fn double(&self, x: &mut [u64; MAX_LIMBS]) {
        let k = self.k;
        let mut carry = 0;
        for l in &mut x[..k] {
            let c = *l >> 63;
            *l = *l << 1 | carry;
            carry = c;
        }
        if carry == 1 || ge(&x[..k], &self.n[..k]) {
            sub_in_place(&mut x[..k], &self.n[..k]);
        }
    }

    /// Montgomery product `a b R^-1 mod n` for `a, b < n` (CIOS).
    fn mul(&self, a: &[u64; MAX_LIMBS], b: &[u64; MAX_LIMBS], out: &mut [u64; MAX_LIMBS]) {
        let (k, n) = (self.k, &self.n);
        let mut t = [0u64; MAX_LIMBS + 2];
        for &bi in &b[..k] {
            let mut c = 0u64;
            for j in 0..k {
                let v = t[j] as u128 + a[j] as u128 * bi as u128 + c as u128;
                t[j] = v as u64;
                c = (v >> 64) as u64;
            }
            let v = t[k] as u128 + c as u128;
            t[k] = v as u64;
            t[k + 1] = (v >> 64) as u64;
            let m = t[0].wrapping_mul(self.n0);
            let mut c = ((t[0] as u128 + m as u128 * n[0] as u128) >> 64) as u64;
            for j in 1..k {
                let v = t[j] as u128 + m as u128 * n[j] as u128 + c as u128;
                t[j - 1] = v as u64;
                c = (v >> 64) as u64;
            }
            let v = t[k] as u128 + c as u128;
            t[k - 1] = v as u64;
            t[k] = t[k + 1] + (v >> 64) as u64;
        }
        out[..k].copy_from_slice(&t[..k]);
        if t[k] != 0 || ge(&out[..k], &n[..k]) {
            sub_in_place(&mut out[..k], &n[..k]);
        }
    }

    /// `base^e mod n`, written big-endian into `out`, which must be exactly as long as the
    /// modulus in bytes (`bits().div_ceil(8)`). `None` if the base is not below `n`, `e` is
    /// 0, or `out` has the wrong length.
    pub fn pow(&self, base_be: &[u8], e: u32, out: &mut [u8]) -> Option<()> {
        if e == 0 || out.len() != self.bits().div_ceil(8) {
            return None;
        }
        let (a, ak) = from_be(base_be)?;
        let k = self.k;
        if ak > k || (ak == k && ge(&a[..k], &self.n[..k])) {
            return None;
        }
        let mut am = [0u64; MAX_LIMBS];
        self.mul(&a, &self.r2, &mut am);
        let mut acc = am;
        for i in (0..31 - e.leading_zeros()).rev() {
            let t = acc;
            self.mul(&t, &t, &mut acc);
            if e >> i & 1 == 1 {
                let t = acc;
                self.mul(&t, &am, &mut acc);
            }
        }
        let mut one = [0u64; MAX_LIMBS];
        one[0] = 1;
        let t = acc;
        self.mul(&t, &one, &mut acc);
        for (i, o) in out.iter_mut().rev().enumerate() {
            *o = (acc[i / 8] >> (8 * (i % 8))) as u8;
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Slow reference: schoolbook multiply and shift-subtract reduction over bytes, no limbs.
    /// `x * y mod n` for big-endian byte strings, as a bit-serial double-and-add.
    fn ref_mulmod(x: &[u8], y: &[u8], n: &[u8]) -> Vec<u8> {
        let w = n.len() + 1;
        let pad = |v: &[u8]| {
            let mut o = vec![0u8; w - v.len()];
            o.extend_from_slice(v);
            o
        };
        let n = pad(n);
        let ge = |a: &[u8], b: &[u8]| a >= b;
        let sub = |a: &mut Vec<u8>, b: &[u8]| {
            let mut br = 0i16;
            for i in (0..a.len()).rev() {
                let mut d = a[i] as i16 - b[i] as i16 - br;
                br = (d < 0) as i16;
                if d < 0 {
                    d += 256;
                }
                a[i] = d as u8;
            }
        };
        let add = |a: &mut Vec<u8>, b: &[u8]| {
            let mut c = 0u16;
            for i in (0..a.len()).rev() {
                let s = a[i] as u16 + b[i] as u16 + c;
                a[i] = s as u8;
                c = s >> 8;
            }
        };
        let mut r = vec![0u8; w];
        let x = pad(x);
        for byte in pad(y) {
            for bit in (0..8).rev() {
                let t = r.clone();
                add(&mut r, &t);
                if ge(&r, &n) {
                    sub(&mut r, &n);
                }
                if byte >> bit & 1 == 1 {
                    add(&mut r, &x);
                    if ge(&r, &n) {
                        sub(&mut r, &n);
                    }
                }
            }
        }
        r[1..].to_vec()
    }

    fn ref_pow(base: &[u8], e: u32, n: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; n.len()];
        *r.last_mut().unwrap() = 1;
        for i in (0..32).rev() {
            r = ref_mulmod(&r, &r, n);
            if e >> i & 1 == 1 {
                r = ref_mulmod(&r, base, n);
            }
        }
        r
    }

    /// Small deterministic generator (xorshift). The tests need no secrets.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n).map(|_| self.next() as u8).collect()
        }
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn small_known_answers() {
        // 4^13 mod 497 = 445 (textbook), 3^65537 mod 1000003 via the reference.
        let m = Modulus::new(&497u32.to_be_bytes()).unwrap();
        let mut o = [0u8; 2];
        m.pow(&[4], 13, &mut o).unwrap();
        assert_eq!(o, 445u16.to_be_bytes());
        let n = 1_000_003u32.to_be_bytes();
        let m = Modulus::new(&n).unwrap();
        let mut o = [0u8; 3];
        m.pow(&[3], 65537, &mut o).unwrap();
        assert_eq!(o.to_vec(), ref_pow(&[0, 0, 3], 65537, &n[1..])[..].to_vec());
        // 7^3 mod 11 = 2; base 0 gives 0; base 1 gives 1.
        let m = Modulus::new(&[11]).unwrap();
        let mut o = [0u8; 1];
        m.pow(&[7], 3, &mut o).unwrap();
        assert_eq!(o, [2]);
        m.pow(&[0], 5, &mut o).unwrap();
        assert_eq!(o, [0]);
        m.pow(&[1], 5, &mut o).unwrap();
        assert_eq!(o, [1]);
    }

    #[test]
    fn matches_the_reference_for_random_odd_moduli() {
        let mut rng = Rng(0x9e3779b97f4a7c15);
        // Lengths in bytes: limb edges, odd sizes, and the RSA sizes.
        for &len in &[1usize, 2, 7, 8, 9, 15, 16, 17, 31, 33, 64, 100, 129, 256, 384, 512] {
            let cases = if len > 128 { 2 } else { 6 };
            for _ in 0..cases {
                let mut n = rng.bytes(len);
                n[0] |= 0x80 >> (rng.next() % 8); // vary the top bit position, keep n[0] != 0
                *n.last_mut().unwrap() |= 1;
                let Some(m) = Modulus::new(&n) else { continue };
                let nlen = m.bits().div_ceil(8);
                let mut base = rng.bytes(nlen);
                base[0] = 0; // below n for most moduli
                let base = if ge(&from_be(&base).unwrap().0[..m.k], &m.n[..m.k]) { vec![1, 2, 3] } else { base };
                for e in [3u32, 17, 65537, 0xffff_ffff, 0x8000_0001] {
                    let mut out = vec![0u8; nlen];
                    m.pow(&base, e, &mut out).unwrap();
                    let mut b = vec![0u8; nlen - base.len().min(nlen)];
                    b.extend_from_slice(&base[base.len().saturating_sub(nlen)..]);
                    assert_eq!(out, ref_pow(&b, e, &n[n.len() - nlen..]), "len {len} e {e}");
                }
            }
        }
    }

    #[test]
    fn a_modulus_of_all_ones_and_other_edges() {
        for len in [8usize, 64, 512] {
            let n = vec![0xffu8; len];
            let m = Modulus::new(&n).unwrap();
            let mut base = vec![0xffu8; len];
            *base.last_mut().unwrap() = 0xfe; // n - 1
            let mut out = vec![0u8; len];
            m.pow(&base, 65537, &mut out).unwrap();
            assert_eq!(out, base, "(-1)^odd = -1");
            m.pow(&base, 65536 + 1 - 1, &mut out).unwrap();
            let mut one = vec![0u8; len];
            one[len - 1] = 1;
            assert_eq!(out, one, "(-1)^even = 1");
        }
        // The largest allowed modulus (4096 bits) works; one bit more is refused.
        let mut big = vec![0xffu8; 512];
        assert!(Modulus::new(&big).is_some());
        big.insert(0, 1);
        assert!(Modulus::new(&big).is_none());
        // Leading zeros are ignored.
        let mut z = vec![0u8; 300];
        z.extend_from_slice(&[0x01, 0x03]);
        assert_eq!(Modulus::new(&z).unwrap().bits(), 9);
    }

    #[test]
    fn hostile_moduli_do_not_panic() {
        assert!(Modulus::new(&[]).is_none());
        assert!(Modulus::new(&[0]).is_none());
        assert!(Modulus::new(&[0, 0, 0]).is_none());
        assert!(Modulus::new(&[1]).is_none());
        assert!(Modulus::new(&[2]).is_none());
        assert!(Modulus::new(&[4]).is_none());
        assert!(Modulus::new(&[0x10, 0x00]).is_none());
        assert!(Modulus::new(&[3]).is_some());
        assert!(Modulus::new(&vec![0xff; 513]).is_none());
        let mut rng = Rng(7);
        for _ in 0..300 {
            let len = (rng.next() % 530) as usize;
            let n = rng.bytes(len);
            if let Some(m) = Modulus::new(&n) {
                let mut out = vec![0u8; m.bits().div_ceil(8)];
                let base = rng.bytes(len);
                let _ = m.pow(&base, rng.next() as u32, &mut out);
                let _ = m.pow(&base, 65537, &mut out);
            }
        }
    }

    #[test]
    fn bad_pow_arguments_are_refused() {
        let n = hex("c5f1d3a7b9e5f7d1a3c5b7e9f1d3a5b7");
        let m = Modulus::new(&n).unwrap();
        let mut out = [0u8; 16];
        assert!(m.pow(&[2], 0, &mut out).is_none(), "e = 0");
        assert!(m.pow(&n, 3, &mut out).is_none(), "base = n");
        let mut above = n.clone();
        above[15] = 0xb9;
        assert!(m.pow(&above, 3, &mut out).is_none(), "base > n");
        assert!(m.pow(&[0xff; 17], 3, &mut out).is_none(), "base too long");
        assert!(m.pow(&[2], 3, &mut [0u8; 15]).is_none(), "short output");
        assert!(m.pow(&[2], 3, &mut [0u8; 17]).is_none(), "long output");
        assert!(m.pow(&[0, 0, 0, 2], 3, &mut out).is_some(), "leading zeros are fine");
        let mut below = n.clone();
        below[15] = 0xb5;
        assert!(m.pow(&below, 3, &mut out).is_some(), "n - 2");
        assert!(m.pow(&vec![0xff; 600], 3, &mut out).is_none());
    }
}
