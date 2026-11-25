//! RSA private keys for the Web Crypto API: key generation, signing with the Chinese remainder theorem (CRT), and the public
//! operation. Learning code, NOT audited. Padding (EMSA-PKCS1-v1_5) is in the caller.
//!
//! NOT constant time, and no blinding: `bigint::Modulus::pow_big` branches on the exponent bits, and the Montgomery multiply
//! branches on its final subtraction. Key generation uses a random start and an incremental search (a sieve of the odd primes
//! below 2000, then Miller-Rabin with a random base), so the primes are not uniform. Use the code only where no attacker can time
//! many signatures. Numbers are big-endian bytes without leading zeros outside this file, and little-endian `u64` limbs inside.

use crate::bigint::Modulus;

/// Limits of the modulus in bits. The upper one is that of `bigint`.
pub const MIN_BITS: usize = 256;
pub const MAX_BITS: usize = crate::bigint::MAX_BITS;

/// All fields are big-endian, without leading zeros.
#[derive(Clone)]
pub struct PrivateKey {
    pub n: Vec<u8>,
    pub e: Vec<u8>,
    pub d: Vec<u8>,
    pub p: Vec<u8>,
    pub q: Vec<u8>,
    pub dp: Vec<u8>,
    pub dq: Vec<u8>,
    pub qi: Vec<u8>,
}

type Nat = Vec<u64>;

fn from_be(b: &[u8]) -> Nat {
    let mut n = vec![0u64; b.len().div_ceil(8)];
    for (i, &x) in b.iter().rev().enumerate() {
        n[i / 8] |= (x as u64) << (8 * (i % 8));
    }
    trim(&mut n);
    n
}

fn trim(n: &mut Nat) {
    while n.last() == Some(&0) {
        n.pop();
    }
}

/// Big-endian bytes without leading zeros (empty for 0).
pub fn to_be(n: &[u64]) -> Vec<u8> {
    let mut v: Vec<u8> = n.iter().rev().flat_map(|l| l.to_be_bytes()).collect();
    let skip = v.iter().take_while(|&&x| x == 0).count();
    v.drain(..skip);
    v
}

/// The value of `b` without leading zeros.
pub fn strip(b: &[u8]) -> &[u8] {
    &b[b.iter().take_while(|&&x| x == 0).count()..]
}

fn bits(n: &[u64]) -> usize {
    n.last().map_or(0, |l| 64 * n.len() - l.leading_zeros() as usize)
}

fn mul(a: &[u64], b: &[u64]) -> Nat {
    let mut r = vec![0u64; a.len() + b.len()];
    for (i, &x) in a.iter().enumerate() {
        let mut c = 0u128;
        for (j, &y) in b.iter().enumerate() {
            let v = r[i + j] as u128 + x as u128 * y as u128 + c;
            r[i + j] = v as u64;
            c = v >> 64;
        }
        r[i + b.len()] = c as u64;
    }
    trim(&mut r);
    r
}

fn add_small(n: &mut Nat, x: u64) {
    let mut c = x;
    for l in n.iter_mut() {
        let (v, o) = l.overflowing_add(c);
        (*l, c) = (v, o as u64);
        if c == 0 {
            return;
        }
    }
    n.push(c);
}

fn add(a: &[u64], b: &[u64]) -> Nat {
    let mut r = a.to_vec();
    r.resize(a.len().max(b.len()) + 1, 0);
    let mut c = false;
    for (i, x) in r.iter_mut().enumerate() {
        let (v, c1) = x.overflowing_add(b.get(i).copied().unwrap_or(0));
        let (v, c2) = v.overflowing_add(c as u64);
        (*x, c) = (v, c1 | c2);
    }
    trim(&mut r);
    r
}

/// `n - x` for `n >= x`.
fn sub_small(n: &mut Nat, x: u64) {
    let mut b = x;
    for l in n.iter_mut() {
        let (v, o) = l.overflowing_sub(b);
        *l = v;
        if !o {
            break;
        }
        b = 1;
    }
    trim(n);
}

fn mul_small(n: &[u64], x: u64) -> Nat {
    mul(n, &[x])
}

/// Quotient and remainder of `n / x`.
fn div_small(n: &[u64], x: u64) -> (Nat, u64) {
    let (mut q, mut r) = (vec![0u64; n.len()], 0u128);
    for i in (0..n.len()).rev() {
        let v = r << 64 | n[i] as u128;
        q[i] = (v / x as u128) as u64;
        r = v % x as u128;
    }
    trim(&mut q);
    (q, r as u64)
}

/// `x^-1 mod m` for `x` and `m` below 2^64 (`m > 1`), by the extended Euclidean algorithm. `None` if they share a factor.
fn inv_small(x: u64, m: u64) -> Option<u64> {
    let (mut r0, mut r1, mut t0, mut t1) = (m as i128, (x % m) as i128, 0i128, 1i128);
    while r1 != 0 {
        let q = r0 / r1;
        (r0, r1) = (r1, r0 - q * r1);
        (t0, t1) = (t1, t0 - q * t1);
    }
    (r0 == 1).then(|| t0.rem_euclid(m as i128) as u64)
}

/// `e^-1 mod m` for a small `e` that has no factor in common with `m`: `(1 + k m) / e` for the `k` below `e` that makes it exact.
fn inv_mod(m: &[u64], e: u64) -> Option<Nat> {
    let k = e - inv_small(div_small(m, e).1, e)?;
    let mut t = mul_small(m, k);
    add_small(&mut t, 1);
    Some(div_small(&t, e).0)
}

fn small_primes() -> &'static [u32] {
    static P: std::sync::OnceLock<Vec<u32>> = std::sync::OnceLock::new();
    P.get_or_init(|| (3..2000u32).filter(|n| (2..*n).take_while(|d| d * d <= *n).all(|d| n % d != 0)).collect())
}

fn random(n: usize) -> Option<Vec<u8>> {
    let mut b = vec![0u8; n];
    crate::random::fill(&mut b).ok()?;
    Some(b)
}

/// Miller-Rabin with `rounds` bases: the first is 2, the others are random. `c` is odd and has at least 16 bits.
fn probably_prime(c: &[u64], rounds: usize) -> Option<bool> {
    let cb = to_be(c);
    let m = Modulus::new(&cb)?;
    let mut nm1 = c.to_vec();
    sub_small(&mut nm1, 1);
    let s = nm1.iter().position(|&l| l != 0).map_or(0, |i| 64 * i + nm1[i].trailing_zeros() as usize);
    let mut d = vec![0u64; nm1.len()];
    for i in s..64 * nm1.len() {
        d[(i - s) / 64] |= (nm1[i / 64] >> (i % 64) & 1) << ((i - s) % 64);
    }
    let (d, mut nm1) = (to_be(&d), to_be(&nm1));
    nm1.splice(0..0, vec![0u8; cb.len() - nm1.len()]);
    for round in 0..rounds {
        let base = if round == 0 {
            vec![2]
        } else {
            // Fewer bytes than `n`, so below it; the low bit pair makes it at least 2.
            let mut b = random(cb.len() - 1)?;
            *b.last_mut()? |= 2;
            b
        };
        let mut x = m.pow_big(&base, &d)?;
        let mut witness = strip(&x) != [1] && x != nm1;
        for _ in 1..s {
            if !witness {
                break;
            }
            x = m.mul_mod(&x, &x)?;
            witness = x != nm1;
        }
        if witness {
            return Some(false);
        }
    }
    Some(true)
}

/// A random prime of exactly `bits` bits (the top two bits are set) with `gcd(e, p - 1) = 1`.
fn gen_prime(bits_: usize, e: u64) -> Option<Nat> {
    let sieve = small_primes();
    loop {
        let mut b = random(bits_.div_ceil(8))?;
        let extra = 8 * b.len() - bits_;
        b[0] &= 0xff >> extra;
        b[0] |= 0xc0 >> extra;
        if bits_ % 8 == 1 {
            // The second bit is in the next byte.
            b[1] |= 0x80;
        }
        *b.last_mut()? |= 1;
        let base = from_be(&b);
        let mut res: Vec<u32> = sieve.iter().map(|&p| div_small(&base, p as u64).1 as u32).collect();
        'delta: for delta in (0..20_000u32).step_by(2) {
            for (r, &p) in res.iter_mut().zip(sieve) {
                if (*r + delta) % p == 0 {
                    continue 'delta;
                }
            }
            let mut c = base.clone();
            add_small(&mut c, delta as u64);
            if bits(&c) != bits_ {
                break;
            }
            let mut pm1 = c.clone();
            sub_small(&mut pm1, 1);
            if inv_small(div_small(&pm1, e).1, e).is_none() {
                continue;
            }
            if probably_prime(&c, 5)? {
                return Some(c);
            }
        }
    }
}

impl PrivateKey {
    /// A new key of `bits` bits (a multiple of 8 in `MIN_BITS..=MAX_BITS`) and public exponent `e` (odd, 3 to 2^32 - 1).
    pub fn generate(bits_: usize, e: u32) -> Option<PrivateKey> {
        if !(MIN_BITS..=MAX_BITS).contains(&bits_) || bits_ % 8 != 0 || e < 3 || e % 2 == 0 {
            return None;
        }
        let e64 = e as u64;
        let (mut p, mut q) = (gen_prime(bits_ - bits_ / 2, e64)?, gen_prime(bits_ / 2, e64)?);
        if bits(&p) == bits(&q) && p < q {
            std::mem::swap(&mut p, &mut q);
        }
        if p == q {
            return Self::generate(bits_, e);
        }
        let (mut p1, mut q1) = (p.clone(), q.clone());
        sub_small(&mut p1, 1);
        sub_small(&mut q1, 1);
        let phi = mul(&p1, &q1);
        let n = mul(&p, &q);
        let (pb, qb) = (to_be(&p), to_be(&q));
        let mut p2 = p.clone();
        sub_small(&mut p2, 2);
        let k = PrivateKey {
            n: to_be(&n),
            e: to_be(&[e64]),
            d: to_be(&inv_mod(&phi, e64)?),
            dp: to_be(&inv_mod(&p1, e64)?),
            dq: to_be(&inv_mod(&q1, e64)?),
            qi: Modulus::new(&pb)?.pow_big(&qb, &to_be(&p2))?,
            p: pb,
            q: qb,
        };
        let k = PrivateKey { qi: strip(&k.qi).to_vec(), ..k };
        k.check().then_some(k)
    }

    /// The key is consistent: `p q = n`, and a signature of a test value verifies with `e`.
    pub fn check(&self) -> bool {
        let n = from_be(&self.n);
        (MIN_BITS..=MAX_BITS).contains(&bits(&n)) && mul(&from_be(&self.p), &from_be(&self.q)) == n && self.sign_raw(&[2]).is_some()
    }

    /// `m^d mod n` by CRT, for `m < n`. The result is checked with the public key (a fault in the CRT would leak a factor).
    pub fn sign_raw(&self, m: &[u8]) -> Option<Vec<u8>> {
        let (pm, qm) = (Modulus::new(&self.p)?, Modulus::new(&self.q)?);
        let m1 = pm.pow_big(m, &self.dp)?;
        let m2 = qm.pow_big(m, &self.dq)?;
        let h = pm.mul_mod(&self.qi, &pm.sub_mod(&m1, &m2)?)?;
        let s = add(&mul(&from_be(&h), &from_be(&self.q)), &from_be(&m2));
        let k = self.n.len();
        let mut out = to_be(&s);
        if out.len() > k {
            return None;
        }
        out.splice(0..0, vec![0u8; k - out.len()]);
        (public_op(&self.n, &self.e, &out)? == strip_pad(m, k)).then_some(out)
    }
}

fn strip_pad(m: &[u8], k: usize) -> Vec<u8> {
    let m = strip(m);
    let mut v = vec![0u8; k.saturating_sub(m.len())];
    v.extend_from_slice(m);
    v
}

/// `s^e mod n` as bytes as long as `n`. `None` if `s >= n`, `n` is even or too large, or `e` is 0 or over 32 bits.
pub fn public_op(n: &[u8], e: &[u8], s: &[u8]) -> Option<Vec<u8>> {
    let (n, e) = (strip(n), strip(e));
    let e = u32::try_from(from_be(e).first().copied().unwrap_or(0)).ok().filter(|_| e.len() <= 4)?;
    let m = Modulus::new(n)?;
    let mut out = vec![0u8; m.bits().div_ceil(8)];
    m.pow(s, e, &mut out)?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_number_helpers() {
        assert_eq!(inv_small(3, 7), Some(5));
        assert_eq!(inv_small(6, 9), None);
        // d = 65537^-1 mod (2^61 - 1), and no inverse when 65537 divides the modulus.
        let m = (1u64 << 61) - 1;
        let d = inv_mod(&[m], 65537).unwrap();
        assert_eq!(div_small(&mul(&d, &[65537]), m).1, 1);
        assert!(inv_mod(&[65537 * 3], 65537).is_none());
    }

    #[test]
    fn miller_rabin_separates_primes_from_carmichael_numbers() {
        // 2^127 - 1 is prime. 561, 41041 and 1 000 000 007 * 998 244 353 (a semiprime) are not.
        assert_eq!(probably_prime(&[u64::MAX, u64::MAX >> 1], 5), Some(true));
        assert_eq!(probably_prime(&[1_000_000_007], 5), Some(true));
        assert_eq!(probably_prime(&[561 * 1_000_003], 5), Some(false));
        assert_eq!(probably_prime(&[41041 * 65537], 5), Some(false));
        assert_eq!(probably_prime(&mul(&[1_000_000_007], &[998_244_353]), 5), Some(false));
    }

    #[test]
    fn generated_key_signs_and_verifies() {
        let k = PrivateKey::generate(512, 65537).unwrap();
        assert_eq!(k.n.len(), 64);
        assert_eq!(k.n[0] & 0x80, 0x80);
        let mut m = vec![0u8; 64];
        m[0] = 0x00;
        m[1..].iter_mut().enumerate().for_each(|(i, b)| *b = (i as u8).wrapping_mul(7).wrapping_add(1));
        let s = k.sign_raw(&m).unwrap();
        assert_eq!(public_op(&k.n, &k.e, &s).unwrap(), m);
        // d works without CRT too: m^d mod n equals the CRT result.
        assert_eq!(Modulus::new(&k.n).unwrap().pow_big(&m, &k.d).unwrap(), s);
        assert!(public_op(&k.n, &k.e, &k.n).is_none());
    }
}

#[cfg(test)]
mod keygen_time {
    /// Prints the time of ten 2048-bit key generations. Run: `cargo test --release -p bare-tls --lib keygen_time -- --nocapture --ignored`.
    #[test]
    #[ignore]
    fn rsa_2048() {
        for _ in 0..10 {
            let t = std::time::Instant::now();
            let k = super::PrivateKey::generate(2048, 65537).unwrap();
            eprintln!("rsa-2048 keygen {:?} ({} bytes n)", t.elapsed(), k.n.len());
        }
    }
}
