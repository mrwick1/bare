//! ECDSA signature verification on NIST P-256 and P-384. Learning code, NOT audited.
//!
//! Sources:
//! - FIPS 186-5 section 6.4.2 (ECDSA verification) and SP 800-186 (curve constants):
//!   <https://csrc.nist.gov/pubs/fips/186-5/final>, <https://csrc.nist.gov/pubs/sp/800/186/final>
//! - SEC 1 v2 section 2.3.4 (uncompressed point encoding) and 3.2.2 (public key validation):
//!   <https://www.secg.org/sec1-v2.pdf>
//! - RFC 8446 section 4.2.3 (TLS 1.3 uses ECDSA with P-256/SHA-256 and P-384/SHA-384).
//!
//! Verification takes only public inputs. The arithmetic runs in VARIABLE time (branches and loop
//! lengths depend on the data). Never use this code to multiply a secret that lives for long.
//!
//! One exception: `p256_public` and `p256_shared` do P-256 ECDH for a TLS 1.2 client. They multiply a SECRET
//! scalar with the same variable-time code. The scalar is a fresh random value for one handshake and is dropped
//! after it, so no attacker can measure many runs against one secret. That is the only reason this is acceptable.
//! There is no signing.
//!
//! What `verify` checks, in this order:
//! 1. The digest is 32 bytes (SHA-256) or 48 bytes (SHA-384). Nothing else is accepted.
//! 2. The public key is exactly `04 || X || Y` (SEC 1 uncompressed), with X and Y
//!    big-endian, fixed width, below the field prime, and on the curve. The point at
//!    infinity has no uncompressed encoding, so it fails the length check. Both curves have
//!    cofactor 1, so "on the curve" means the point has order n and SEC 1 validation is complete.
//! 3. The signature is exactly `r || s`, fixed width, with 1 <= r, s <= n-1.
//! 4. u1*G + u2*Q is not infinity and its x coordinate, taken mod n, equals r.
//!
//! Digest and curve do not have to match in size. FIPS 186-5 uses the leftmost bits of the
//! hash: a 48-byte digest on P-256 is cut to its first 32 bytes, and a 32-byte digest on
//! P-384 is used as it is (an integer below 2^256). There is no low-s rule: both s and n-s
//! are valid ECDSA signatures, so both pass.
//!
//! Memory is fixed: one call uses only its stack, and a call (also the first one, which builds
//! the table below) ran inside a 16 KiB thread stack, the smallest that Linux gives. Per curve
//! there is one read-only table of odd multiples of G (2 KiB for P-256, 3 KiB for P-384),
//! built on the first call with about 1000 field multiplications. `verify` never allocates.
//!
//! Layout of the code:
//! - [`Field`]: arithmetic modulo an odd prime in Montgomery form, shared by the field
//!   prime p and the group order n, and by both curves through a const generic `N`.
//! - [`wnaf`]: signed digit strings for the scalars, so that few additions are needed.
//! - [`Params`]: one curve (p, n, b, G) and the point math in Jacobian coordinates.
//! - [`verify`]: the public function.
//!
//! Tried and dropped, because they measured no faster: a separate squaring routine, a fused
//! multiply-and-reduce loop, `#[inline(always)]` on `mul`, other window widths.

use std::sync::OnceLock;

/// A number as `N` 64-bit limbs, least significant limb first. `const N: usize` is a
/// const generic: `[u64; N]` is a different type for each N, like a template parameter.
/// P-256 uses N = 4 and P-384 uses N = 6, so one copy of the code serves both curves.
type Limbs<const N: usize> = [u64; N];

/// The curves that `verify` supports.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Curve {
    /// secp256r1 / prime256v1 (NIST P-256).
    P256,
    /// secp384r1 (NIST P-384).
    P384,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Error {
    /// The digest is not 32 or 48 bytes long.
    BadDigest,
    /// The key is not a valid uncompressed point of the chosen curve.
    BadPublicKey,
    /// The signature has a wrong length, r or s is out of range, or the check fails.
    BadSignature,
}

impl Curve {
    /// Bytes in one coordinate, one scalar (r or s) and the group order: 32 or 48.
    pub const fn size(self) -> usize {
        match self {
            Curve::P256 => 32,
            Curve::P384 => 48,
        }
    }
}

/// Verify an ECDSA signature over `digest` (the hash of the message, not the message).
///
/// `public_key` is `04 || X || Y`. `sig` is `r || s`, each `curve.size()` bytes,
/// big-endian. A caller that has a DER `SEQUENCE { INTEGER r, INTEGER s }` must strip
/// the DER and left-pad r and s to the fixed width first.
pub fn verify(curve: Curve, public_key: &[u8], digest: &[u8], sig: &[u8]) -> Result<(), Error> {
    if digest.len() != 32 && digest.len() != 48 {
        return Err(Error::BadDigest);
    }
    match curve {
        Curve::P256 => P256.verify(public_key, digest, sig),
        Curve::P384 => P384.verify(public_key, digest, sig),
    }
}

/// The public key `04 || X || Y` of a P-256 ECDH secret. `scalar` is 32 random bytes. `None` when it is 0 mod n
/// (probability 2^-256). Variable time: read the module docs before you use it.
pub fn p256_public(scalar: &[u8; 32]) -> Option<[u8; 65]> {
    let k = P256.secret(scalar)?;
    let g = Point { x: P256.p.to_mont(&P256.gx), y: P256.p.to_mont(&P256.gy), z: P256.p.one };
    let [x, y] = P256.mul_affine(&k, &g)?;
    let mut out = [4u8; 65];
    for (i, limb) in x.iter().rev().chain(y.iter().rev()).enumerate() {
        out[1 + 8 * i..9 + 8 * i].copy_from_slice(&limb.to_be_bytes());
    }
    Some(out)
}

/// The ECDH shared secret (the X coordinate, 32 bytes) of a P-256 secret and the peer's `04 || X || Y`. The peer key
/// must be on the curve. Variable time: read the module docs before you use it.
pub fn p256_shared(scalar: &[u8; 32], peer: &[u8]) -> Result<[u8; 32], Error> {
    let q = P256.parse_point(peer)?;
    let k = P256.secret(scalar).ok_or(Error::BadSignature)?;
    let [x, _] = P256.mul_affine(&k, &q).ok_or(Error::BadPublicKey)?;
    let mut out = [0u8; 32];
    for (i, limb) in x.iter().rev().enumerate() {
        out[8 * i..8 * i + 8].copy_from_slice(&limb.to_be_bytes());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Limb helpers. `const fn` can run at compile time, so the curve constants and the
// Montgomery constants below are computed by the compiler. In JS you would compute them
// at start-up. Inside a `const fn` only `while` loops work (not `for`).

/// a + b + carry-in `c`, as a 64-bit result and a 64-bit carry. `u128` is a 128-bit integer
/// (JS has only BigInt for this). The sum cannot overflow it: it is at most 2^128 - 1.
const fn mac(a: u64, b: u64, c: u64, d: u64) -> (u64, u64) {
    let t = a as u128 + (b as u128) * (c as u128) + d as u128;
    (t as u64, (t >> 64) as u64)
}

/// a >= b.
const fn geq<const N: usize>(a: &Limbs<N>, b: &Limbs<N>) -> bool {
    let mut i = N;
    while i > 0 {
        i -= 1;
        if a[i] != b[i] {
            return a[i] > b[i];
        }
    }
    true
}

/// (a + b mod 2^(64N), carry out).
const fn add_raw<const N: usize>(a: &Limbs<N>, b: &Limbs<N>) -> (Limbs<N>, bool) {
    let mut r = [0u64; N];
    let mut c = false;
    let mut i = 0;
    while i < N {
        let (s1, c1) = a[i].overflowing_add(b[i]);
        let (s2, c2) = s1.overflowing_add(c as u64);
        r[i] = s2;
        c = c1 | c2;
        i += 1;
    }
    (r, c)
}

/// (a - b mod 2^(64N), borrow out).
const fn sub_raw<const N: usize>(a: &Limbs<N>, b: &Limbs<N>) -> (Limbs<N>, bool) {
    let mut r = [0u64; N];
    let mut br = false;
    let mut i = 0;
    while i < N {
        let (d1, b1) = a[i].overflowing_sub(b[i]);
        let (d2, b2) = d1.overflowing_sub(br as u64);
        r[i] = d2;
        br = b1 | b2;
        i += 1;
    }
    (r, br)
}

/// (a + b) mod m, for a, b < m.
const fn add_mod<const N: usize>(a: &Limbs<N>, b: &Limbs<N>, m: &Limbs<N>) -> Limbs<N> {
    let (s, c) = add_raw(a, b);
    if c || geq(&s, m) { sub_raw(&s, m).0 } else { s }
}

/// Parse big-endian hex (no `0x`) at compile time. A bad digit stops the build.
const fn hex<const N: usize>(s: &str) -> Limbs<N> {
    let b = s.as_bytes();
    assert!(b.len() <= 16 * N);
    let mut out = [0u64; N];
    let mut i = 0;
    while i < b.len() {
        let d = match b[i] {
            b'0'..=b'9' => b[i] - b'0',
            b'a'..=b'f' => b[i] - b'a' + 10,
            _ => panic!("bad hex digit"),
        };
        let mut j = N - 1; // shift the whole number left by 4 bits, then add the digit
        while j > 0 {
            out[j] = out[j] << 4 | out[j - 1] >> 60;
            j -= 1;
        }
        out[0] = out[0] << 4 | d as u64;
        i += 1;
    }
    out
}

/// Big-endian bytes (exactly 8N of them) to limbs.
fn from_be<const N: usize>(b: &[u8]) -> Limbs<N> {
    let mut out = [0u64; N];
    for (limb, chunk) in out.iter_mut().rev().zip(b.chunks_exact(8)) {
        *limb = u64::from_be_bytes(chunk.try_into().unwrap());
    }
    out
}

fn bit<const N: usize>(x: &Limbs<N>, i: usize) -> bool {
    x[i / 64] >> (i % 64) & 1 == 1
}

fn is_zero<const N: usize>(x: &Limbs<N>) -> bool {
    x.iter().all(|&l| l == 0)
}

// ---------------------------------------------------------------------------------------
// Montgomery arithmetic.
//
// Montgomery form stores x as x*R mod m, with R = 2^(64N). Then a multiplication needs no
// division by m: `mul` computes a*b/R mod m with only shifts and multiplies of limbs
// (the CIOS method, Koc, Acar and Kaliski 1996). Values stay in [0, m), so each number
// has exactly one representation and equality is a plain limb compare.

struct Field<const N: usize> {
    m: Limbs<N>,
    /// -m^-1 mod 2^64.
    n0: u64,
    /// R mod m: the number 1 in Montgomery form.
    one: Limbs<N>,
    /// R^2 mod m: multiply by it to enter Montgomery form.
    r2: Limbs<N>,
}

impl<const N: usize> Field<N> {
    /// `m` must be odd. Does a few hundred doublings at compile time, nothing at run time.
    const fn new(m: Limbs<N>) -> Self {
        // Newton's iteration for m^-1 mod 2^64. Each step doubles the correct low bits.
        let mut inv = 1u64;
        let mut i = 0;
        while i < 6 {
            inv = inv.wrapping_mul(2u64.wrapping_sub(m[0].wrapping_mul(inv)));
            i += 1;
        }
        // R mod m: start at 1 and double 64N times. R^2 mod m: 64N more doublings.
        let mut x = [0u64; N];
        x[0] = 1;
        let mut i = 0;
        while i < 64 * N {
            x = add_mod(&x, &x, &m);
            i += 1;
        }
        let one = x;
        let mut i = 0;
        while i < 64 * N {
            x = add_mod(&x, &x, &m);
            i += 1;
        }
        Field { m, n0: inv.wrapping_neg(), one, r2: x }
    }

    fn add(&self, a: &Limbs<N>, b: &Limbs<N>) -> Limbs<N> {
        add_mod(a, b, &self.m)
    }

    fn sub(&self, a: &Limbs<N>, b: &Limbs<N>) -> Limbs<N> {
        let (d, borrow) = sub_raw(a, b);
        if borrow { add_raw(&d, &self.m).0 } else { d }
    }

    /// Montgomery product a*b/R mod m, for a, b < m (CIOS: multiply a row, reduce a row).
    fn mul(&self, a: &Limbs<N>, b: &Limbs<N>) -> Limbs<N> {
        let m = &self.m;
        let mut t = [0u64; N];
        let mut hi = 0u64; // the limb above t: 0 or 1
        for &bi in b.iter() {
            let mut c = 0;
            for j in 0..N {
                (t[j], c) = mac(t[j], a[j], bi, c);
            }
            let (s, o1) = hi.overflowing_add(c);
            // Add q*m with q chosen so that the lowest limb becomes 0, then drop it.
            let q = t[0].wrapping_mul(self.n0);
            (_, c) = mac(t[0], q, m[0], 0);
            for j in 1..N {
                (t[j - 1], c) = mac(t[j], q, m[j], c);
            }
            let (s, o2) = s.overflowing_add(c);
            t[N - 1] = s;
            hi = o1 as u64 + o2 as u64;
        }
        if hi != 0 || geq(&t, m) { sub_raw(&t, m).0 } else { t }
    }

    fn sqr(&self, a: &Limbs<N>) -> Limbs<N> {
        self.mul(a, a)
    }

    fn to_mont(&self, a: &Limbs<N>) -> Limbs<N> {
        self.mul(a, &self.r2)
    }

    /// a^(m-2) = a^-1 mod m, by Fermat's little theorem (m is prime). `a` is in Montgomery
    /// form and so is the result. Square and multiply; the exponent is public.
    fn inv(&self, a: &Limbs<N>) -> Limbs<N> {
        let mut two = [0u64; N];
        two[0] = 2;
        let e = sub_raw(&self.m, &two).0;
        let mut r = self.one;
        for i in (0..64 * N).rev() {
            r = self.sqr(&r);
            if bit(&e, i) {
                r = self.mul(&r, a);
            }
        }
        r
    }
}

// ---------------------------------------------------------------------------------------
// Signed windowed digits (wNAF).
//
// Write k = sum of d[i] * 2^i where each d[i] is 0 or odd with |d[i]| < 2^(w-1), and a
// nonzero digit is followed by at least w-1 zeros. Then only one digit in w+1 is nonzero
// on average, and a table of the odd multiples 1P, 3P, ..., (2^(w-1)-1)P is enough.
// (Bos and Coster 1989; Hankerson, Menezes and Vanstone, "Guide to Elliptic Curve
// Cryptography", algorithm 3.35.)

/// Window widths: G gets a table of 2^(WG-2) = 32 points, Q a table of 2^(WQ-2) = 8.
const WG: usize = 7;
const WQ: usize = 5;
const GT: usize = 1 << (WG - 2);
const QT: usize = 1 << (WQ - 2);
/// One digit for each bit of a 384-bit scalar, plus one for a last carry.
const MAX_DIGITS: usize = 385;

/// Fill `out` with the digits of `k` (least significant first) and return how many are used.
/// `out` must start as all zeros. Needs w <= 8 so that a digit fits in an `i8`.
fn wnaf<const N: usize>(k: &Limbs<N>, w: usize, out: &mut [i8; MAX_DIGITS]) -> usize {
    let bit_at = |i: usize| i < 64 * N && bit(k, i);
    // `carry` is a +1 that an earlier negative digit left for the bits above it.
    let (mut i, mut carry, mut len) = (0, false, 0);
    while i < 64 * N + 1 {
        if bit_at(i) == carry {
            i += 1; // bit + carry is even: the digit is 0
            continue;
        }
        // bit + carry is odd: take the next w bits (plus the carry) as one odd number
        // below 2^w. If it is 2^(w-1) or more, use it minus 2^w and carry 1 upward.
        let mut win = carry as i32;
        for j in 0..w {
            win += (bit_at(i + j) as i32) << j;
        }
        carry = win >= 1 << (w - 1);
        out[i] = (win - if carry { 1 << w } else { 0 }) as i8;
        len = i + 1;
        i += w;
    }
    len
}

// ---------------------------------------------------------------------------------------
// Curve y^2 = x^3 - 3x + b over GF(p), and points in Jacobian coordinates.
//
// A Jacobian point (X, Y, Z) stands for the affine point (X/Z^2, Y/Z^3). Doubling and
// adding then need no division, so a whole scalar multiplication needs no inversion.
// Z = 0 means the point at infinity. All coordinates are in Montgomery form.

#[derive(Clone, Copy)]
struct Point<const N: usize> {
    x: Limbs<N>,
    y: Limbs<N>,
    z: Limbs<N>,
}

struct Params<const N: usize> {
    p: Field<N>,
    n: Field<N>,
    b: Limbs<N>,
    gx: Limbs<N>,
    gy: Limbs<N>,
    /// Odd multiples of G, affine, in Montgomery form. Built on first use (see `g_table`).
    gtab: OnceLock<[[Limbs<N>; 2]; GT]>,
}

// FIPS 186-5 / SP 800-186 values (also in SEC 2 as secp256r1 and secp384r1).
// `static` is a global that lives for the whole run, like a module-level `const` in JS.
static P256: Params<4> = Params {
    p: Field::new(hex("ffffffff00000001000000000000000000000000ffffffffffffffffffffffff")),
    n: Field::new(hex("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551")),
    b: hex("5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b"),
    gx: hex("6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296"),
    gy: hex("4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5"),
    gtab: OnceLock::new(),
};

static P384: Params<6> = Params {
    p: Field::new(hex(
        "fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff0000000000000000ffffffff",
    )),
    n: Field::new(hex(
        "ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973",
    )),
    b: hex("b3312fa7e23ee7e4988e056be3f82d19181d9c6efe8141120314088f5013875ac656398d8a2ed19d2a85c8edd3ec2aef"),
    gx: hex("aa87ca22be8b05378eb1c71ef320ad746e1d3b628ba79b9859f741e082542a385502f25dbf55296c3a545e3872760ab7"),
    gy: hex("3617de4a96262c6f5d9e98bf9292dc29f8f41dbd289a147ce9da3113b5f0b8c00a60b1ce1d7e819d7a431d7c90ea0e5f"),
    gtab: OnceLock::new(),
};

impl<const N: usize> Params<N> {
    const INF: Point<N> = Point { x: [0; N], y: [0; N], z: [0; N] };

    /// Point doubling, formulas "dbl-2001-b" for a = -3 (hyperelliptic.org EFD).
    /// Doubling infinity gives Z = 0 again. A finite point never doubles to infinity
    /// because the group order is odd, so no point has Y = 0.
    fn dbl(&self, a: &Point<N>) -> Point<N> {
        let f = &self.p;
        let delta = f.sqr(&a.z);
        let gamma = f.sqr(&a.y);
        let beta = f.mul(&a.x, &gamma);
        let t = f.mul(&f.sub(&a.x, &delta), &f.add(&a.x, &delta));
        let alpha = f.add(&f.add(&t, &t), &t); // 3 (X - Z^2)(X + Z^2)
        let beta2 = f.add(&beta, &beta);
        let beta4 = f.add(&beta2, &beta2);
        let x3 = f.sub(&f.sqr(&alpha), &f.add(&beta4, &beta4));
        let z3 = f.sub(&f.sub(&f.sqr(&f.add(&a.y, &a.z)), &gamma), &delta);
        let g2 = f.sqr(&gamma);
        let g4 = f.add(&g2, &g2);
        let g8 = f.add(&g4, &g4);
        let y3 = f.sub(&f.mul(&alpha, &f.sub(&beta4, &x3)), &f.add(&g8, &g8));
        Point { x: x3, y: y3, z: z3 }
    }

    /// General addition, formulas "add-2007-bl". It handles the special cases that the
    /// formulas cannot: either input is infinity, a == b (use doubling), a == -b (infinity).
    /// The branches are fine because every input is public.
    fn add(&self, a: &Point<N>, b: &Point<N>) -> Point<N> {
        if is_zero(&a.z) {
            return *b;
        }
        if is_zero(&b.z) {
            return *a;
        }
        let f = &self.p;
        let z1z1 = f.sqr(&a.z);
        let z2z2 = f.sqr(&b.z);
        let u1 = f.mul(&a.x, &z2z2);
        let u2 = f.mul(&b.x, &z1z1);
        let s1 = f.mul(&f.mul(&a.y, &b.z), &z2z2);
        let s2 = f.mul(&f.mul(&b.y, &a.z), &z1z1);
        let h = f.sub(&u2, &u1);
        let r = f.sub(&s2, &s1);
        if is_zero(&h) {
            return if is_zero(&r) { self.dbl(a) } else { Self::INF };
        }
        let r = f.add(&r, &r);
        let i = f.sqr(&f.add(&h, &h));
        let j = f.mul(&h, &i);
        let v = f.mul(&u1, &i);
        let x3 = f.sub(&f.sub(&f.sqr(&r), &j), &f.add(&v, &v));
        let s1j = f.mul(&s1, &j);
        let y3 = f.sub(&f.mul(&r, &f.sub(&v, &x3)), &f.add(&s1j, &s1j));
        let zs = f.sub(&f.sub(&f.sqr(&f.add(&a.z, &b.z)), &z1z1), &z2z2);
        Point { x: x3, y: y3, z: f.mul(&zs, &h) }
    }

    /// `add` for a second point that is affine (Z = 1): 7 multiplications and 4 squarings
    /// instead of 11 and 5. Formulas "madd-2007-bl".
    fn add_affine(&self, a: &Point<N>, b: &[Limbs<N>; 2]) -> Point<N> {
        let f = &self.p;
        if is_zero(&a.z) {
            return Point { x: b[0], y: b[1], z: f.one };
        }
        let z1z1 = f.sqr(&a.z);
        let u2 = f.mul(&b[0], &z1z1);
        let s2 = f.mul(&f.mul(&b[1], &a.z), &z1z1);
        let h = f.sub(&u2, &a.x);
        let r = f.sub(&s2, &a.y);
        if is_zero(&h) {
            return if is_zero(&r) { self.dbl(a) } else { Self::INF };
        }
        let hh = f.sqr(&h);
        let i = f.add(&hh, &hh);
        let i = f.add(&i, &i);
        let j = f.mul(&h, &i);
        let r = f.add(&r, &r);
        let v = f.mul(&a.x, &i);
        let x3 = f.sub(&f.sub(&f.sqr(&r), &j), &f.add(&v, &v));
        let yj = f.mul(&a.y, &j);
        let y3 = f.sub(&f.mul(&r, &f.sub(&v, &x3)), &f.add(&yj, &yj));
        let z3 = f.sub(&f.sub(&f.sqr(&f.add(&a.z, &h)), &z1z1), &hh);
        Point { x: x3, y: y3, z: z3 }
    }

    /// 1G, 3G, 5G, ... (2*GT-1)G as affine points. All use one shared inversion
    /// (Montgomery's trick: invert the product of all Z, then peel off one Z at a time).
    /// The table is built once per process and then only read, so it is fixed memory.
    fn g_table(&self) -> &[[Limbs<N>; 2]; GT] {
        self.gtab.get_or_init(|| {
            let f = &self.p;
            let g = Point { x: f.to_mont(&self.gx), y: f.to_mont(&self.gy), z: f.one };
            let g2 = self.dbl(&g);
            let mut pts = [g; GT];
            for i in 1..GT {
                pts[i] = self.add(&pts[i - 1], &g2);
            }
            let mut prefix = [f.one; GT]; // prefix[i] = Z_0 * ... * Z_i
            let mut run = f.one;
            for i in 0..GT {
                run = f.mul(&run, &pts[i].z);
                prefix[i] = run;
            }
            let mut inv = f.inv(&run); // 1 / (Z_0 * ... * Z_last)
            let mut out = [[[0u64; N]; 2]; GT];
            for i in (0..GT).rev() {
                let zi = if i > 0 { f.mul(&inv, &prefix[i - 1]) } else { inv }; // 1 / Z_i
                inv = f.mul(&inv, &pts[i].z);
                let zi2 = f.sqr(&zi);
                out[i] = [f.mul(&pts[i].x, &zi2), f.mul(&pts[i].y, &f.mul(&zi2, &zi))];
            }
            out
        })
    }

    /// u1*G + u2*Q. Both scalars become signed digit strings (wNAF, see `wnaf`) and share
    /// one chain of doublings (Strauss/Shamir). At a nonzero digit d, add d*G or d*Q from a
    /// table of odd multiples; a negative digit uses the negated point (same X, Y -> p - Y).
    /// G has a big table (WG = 7: one digit in 8) and Q a small one made per call (WQ = 5:
    /// one digit in 6, so 8 table points cost 1 doubling and 7 additions).
    fn mul_add(&self, u1: &Limbs<N>, u2: &Limbs<N>, q: &Point<N>) -> Point<N> {
        let f = &self.p;
        let (mut d1, mut d2) = ([0i8; MAX_DIGITS], [0i8; MAX_DIGITS]);
        let len = wnaf(u1, WG, &mut d1).max(wnaf(u2, WQ, &mut d2));
        let q2 = self.dbl(q);
        let mut qt = [*q; QT];
        for i in 1..QT {
            qt[i] = self.add(&qt[i - 1], &q2);
        }
        let gt = self.g_table();
        let neg = |y: &Limbs<N>| f.sub(&[0; N], y);
        let mut r = Self::INF;
        for i in (0..len).rev() {
            r = self.dbl(&r);
            if d1[i] != 0 {
                let t = &gt[d1[i].unsigned_abs() as usize / 2]; // |d| = 2k+1 sits at index k
                r = self.add_affine(&r, &[t[0], if d1[i] < 0 { neg(&t[1]) } else { t[1] }]);
            }
            if d2[i] != 0 {
                let t = &qt[d2[i].unsigned_abs() as usize / 2];
                r = self.add(&r, &Point { x: t.x, y: if d2[i] < 0 { neg(&t.y) } else { t.y }, z: t.z });
            }
        }
        r
    }

    /// A public key `04 || X || Y`: both coordinates below p, and Y^2 = X^3 - 3X + b.
    fn parse_point(&self, key: &[u8]) -> Result<Point<N>, Error> {
        let size = 8 * N;
        let f = &self.p;
        if key.len() != 1 + 2 * size || key[0] != 4 {
            return Err(Error::BadPublicKey);
        }
        let (x, y) = (from_be::<N>(&key[1..1 + size]), from_be::<N>(&key[1 + size..]));
        if geq(&x, &f.m) || geq(&y, &f.m) {
            return Err(Error::BadPublicKey);
        }
        let (xm, ym) = (f.to_mont(&x), f.to_mont(&y));
        let x2 = f.sqr(&xm);
        let x3 = f.mul(&x2, &xm);
        let rhs = f.add(&f.sub(&x3, &f.add(&f.add(&xm, &xm), &xm)), &f.to_mont(&self.b));
        if f.sqr(&ym) != rhs {
            return Err(Error::BadPublicKey);
        }
        Ok(Point { x: xm, y: ym, z: f.one })
    }

    /// k*Q as affine coordinates in plain (not Montgomery) form. `None` at infinity. Variable time.
    fn mul_affine(&self, k: &Limbs<N>, q: &Point<N>) -> Option<[Limbs<N>; 2]> {
        let f = &self.p;
        let r = self.mul_add(&[0; N], k, q);
        if is_zero(&r.z) {
            return None;
        }
        let zi = f.inv(&r.z);
        let zi2 = f.sqr(&zi);
        let mut one = [0u64; N];
        one[0] = 1;
        Some([f.mul(&f.mul(&r.x, &zi2), &one), f.mul(&f.mul(&r.y, &f.mul(&zi2, &zi)), &one)])
    }

    /// A secret scalar from 32 big-endian bytes: reduced once mod n (the value is below 2n), not zero.
    fn secret(&self, scalar: &[u8; 32]) -> Option<Limbs<N>> {
        let k = from_be::<N>(scalar);
        let k = if geq(&k, &self.n.m) { sub_raw(&k, &self.n.m).0 } else { k };
        if is_zero(&k) { None } else { Some(k) }
    }

    fn verify(&self, key: &[u8], digest: &[u8], sig: &[u8]) -> Result<(), Error> {
        let size = 8 * N;
        let f = &self.p;
        let n = &self.n;

        let q = self.parse_point(key)?;

        // Signature: r || s, each in [1, n-1].
        if sig.len() != 2 * size {
            return Err(Error::BadSignature);
        }
        let (r, s) = (from_be::<N>(&sig[..size]), from_be::<N>(&sig[size..]));
        if is_zero(&r) || is_zero(&s) || geq(&r, &n.m) || geq(&s, &n.m) {
            return Err(Error::BadSignature);
        }

        // e = the leftmost `size` bytes of the digest as a number, reduced mod n. The
        // digest is 32 or 48 bytes (checked by the caller) and `size` is 32 or 48. A shorter
        // digest is padded with zeros on the left. One subtraction reduces it, because
        // e < 2^(8*size) < 2n for both orders.
        let used = &digest[..digest.len().min(size)];
        let mut padded = [0u8; 48];
        padded[size - used.len()..size].copy_from_slice(used);
        let mut e = from_be::<N>(&padded[..size]);
        if geq(&e, &n.m) {
            e = sub_raw(&e, &n.m).0;
        }

        // u1 = e/s and u2 = r/s mod n. `mul(e, w)` with w = (1/s) in Montgomery form
        // gives the plain product e*(1/s), so the scalars come out in plain form.
        let w = n.inv(&n.to_mont(&s));
        let (u1, u2) = (n.mul(&e, &w), n.mul(&r, &w));

        let big_r = self.mul_add(&u1, &u2, &q);
        if is_zero(&big_r.z) {
            return Err(Error::BadSignature);
        }

        // Accept if x(R) mod n == r, where x(R) = X / Z^2 is in [0, p). That means
        // x(R) == r, or x(R) == r + n when r + n < p. Compare X with r*Z^2 (and
        // (r+n)*Z^2) instead of dividing by Z^2, so no inversion mod p is needed.
        let z2 = f.sqr(&big_r.z);
        if f.mul(&f.to_mont(&r), &z2) == big_r.x {
            return Ok(());
        }
        let (rn, carry) = add_raw(&r, &n.m);
        if !carry && !geq(&rn, &f.m) && f.mul(&f.to_mont(&rn), &z2) == big_r.x {
            return Ok(());
        }
        Err(Error::BadSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        assert!(s.len().is_multiple_of(2));
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }

    fn curve(name: &str) -> Curve {
        match name {
            "p256" => Curve::P256,
            "p384" => Curve::P384,
            _ => panic!("curve {name}"),
        }
    }

    #[test]
    fn concurrent_generator_table_initialization() {
        fn check<const N: usize>(source: &Params<N>) {
            let params = Params {
                p: Field::new(source.p.m), n: Field::new(source.n.m),
                b: source.b, gx: source.gx, gy: source.gy, gtab: OnceLock::new(),
            };
            let gate = std::sync::Barrier::new(4);
            let pointers = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..4).map(|_| scope.spawn(|| {
                    gate.wait();
                    params.g_table().as_ptr() as usize
                })).collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect::<Vec<_>>()
            });
            assert!(pointers.iter().all(|p| *p == pointers[0]));
            assert_eq!(params.g_table(), source.g_table());
        }
        check(&P256);
        check(&P384);
    }

    /// Public vectors: NIST CAVS 11.0 (SigVer, PKV), Project Wycheproof, and a few
    /// hand cases signed by OpenSSL. Provenance: data/crypto/ecdsa/README.md.
    #[test]
    fn public_vectors() {
        let text = include_str!("../../../data/crypto/ecdsa/vectors.txt");
        let mut keys = std::collections::HashMap::new();
        let (mut sigs, mut pkvs, mut ok_sigs, mut ok_pkvs) = (0, 0, 0, 0);
        for line in text.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
            let f: Vec<&str> = line.split(' ').collect();
            match f[0] {
                "key" => {
                    keys.insert(f[1], (curve(f[2]), unhex(f[3])));
                }
                "sig" => {
                    let (c, key) = &keys[f[1]];
                    let sig = if f[3] == "-" { vec![] } else { unhex(f[3]) };
                    let got = verify(*c, key, &unhex(f[2]), &sig);
                    assert_eq!(got.is_ok(), f[4] == "ok", "{line}: {got:?}");
                    sigs += 1;
                    ok_sigs += got.is_ok() as u32;
                }
                "pkv" => {
                    // A valid key must also verify as a key: signature checks need it on the curve.
                    // Here the key alone is checked through a dummy signature: only the
                    // key error matters.
                    let c = curve(f[1]);
                    let key = unhex(f[2]);
                    let dummy = vec![1u8; 2 * c.size()];
                    let got = verify(c, &key, &vec![0u8; c.size()], &dummy);
                    assert_eq!(got != Err(Error::BadPublicKey), f[3] == "ok", "{line}: {got:?}");
                    pkvs += 1;
                    ok_pkvs += (f[3] == "ok") as u32;
                }
                _ => panic!("{line}"),
            }
        }
        // Counts guard against a parser that silently skips lines.
        assert_eq!((sigs, pkvs), (656, 24));
        assert!(ok_sigs > 300 && ok_pkvs == 8, "{ok_sigs} {ok_pkvs}");
    }

    /// Montgomery multiplication against a schoolbook reference built from `add` only.
    #[test]
    fn mont_mul_matches_double_and_add() {
        fn check<const N: usize>(f: &Field<N>) {
            let mut seed = 0x9e3779b97f4a7c15u64;
            let mut next = || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed
            };
            // Random limbs, then the extremes 0, 1 and m-1.
            let mut vals: Vec<Limbs<N>> = (0..40)
                .map(|_| {
                    let mut x = [0u64; N];
                    x.iter_mut().for_each(|l| *l = next());
                    while geq(&x, &f.m) {
                        x[N - 1] >>= 1;
                    }
                    x
                })
                .collect();
            let mut one = [0u64; N];
            one[0] = 1;
            vals.extend([[0; N], one, sub_raw(&f.m, &one).0]);
            for a in &vals {
                for b in &vals {
                    // a*b by doubling: sum of a*2^i for the set bits of b.
                    let mut want = [0u64; N];
                    for i in (0..64 * N).rev() {
                        want = f.add(&want, &want);
                        if bit(b, i) {
                            want = f.add(&want, a);
                        }
                    }
                    // mul(to_mont(a), to_mont(b)) is a*b in Montgomery form.
                    let got = f.mul(&f.to_mont(a), &f.to_mont(b));
                    assert_eq!(f.mul(&got, &one), want);
                }
                if !is_zero(a) {
                    assert_eq!(f.mul(&f.inv(&f.to_mont(a)), &f.to_mont(a)), f.one);
                }
            }
        }
        check(&P256.p);
        check(&P256.n);
        check(&P384.p);
        check(&P384.n);
    }

    fn on_curve<const N: usize>(c: &Params<N>) {
        let f = &c.p;
        let (x, y) = (f.to_mont(&c.gx), f.to_mont(&c.gy));
        let rhs = f.add(&f.sub(&f.mul(&f.sqr(&x), &x), &f.add(&f.add(&x, &x), &x)), &f.to_mont(&c.b));
        assert_eq!(f.sqr(&y), rhs);
    }

    /// G is on the curve, and (n-1)G + G is infinity: the order of G is n.
    #[test]
    fn generator_has_order_n() {
        fn check<const N: usize>(c: &Params<N>) {
            on_curve(c);
            let f = &c.p;
            let g = Point { x: f.to_mont(&c.gx), y: f.to_mont(&c.gy), z: f.one };
            let mut one = [0u64; N];
            one[0] = 1;
            let nm1 = sub_raw(&c.n.m, &one).0;
            let zero = [0u64; N];
            // (n-1)*G + 1*G, and (n-1)*G alone is not infinity.
            assert!(is_zero(&c.mul_add(&nm1, &one, &g).z));
            assert!(!is_zero(&c.mul_add(&nm1, &zero, &g).z));
            // 2G + 1G + ... : n*G computed as (n-1)G + G agrees with G + (n-1)G.
            assert!(is_zero(&c.mul_add(&zero, &c.n.m, &g).z));
        }
        check(&P256);
        check(&P384);
    }

    /// The plain double-and-add chain with Shamir's trick: slow but easy to trust. It is the
    /// reference for the wNAF code in `mul_add`.
    fn mul_add_ref<const N: usize>(c: &Params<N>, u1: &Limbs<N>, u2: &Limbs<N>, q: &Point<N>) -> Point<N> {
        let f = &c.p;
        let g = Point { x: f.to_mont(&c.gx), y: f.to_mont(&c.gy), z: f.one };
        let table = [Params::<N>::INF, g, *q, c.add(&g, q)];
        let mut r = Params::<N>::INF;
        for i in (0..64 * N).rev() {
            r = c.dbl(&r);
            let sel = bit(u1, i) as usize | (bit(u2, i) as usize) << 1;
            if sel != 0 {
                r = c.add(&r, &table[sel]);
            }
        }
        r
    }

    /// Affine x and y of a finite point, in Montgomery form.
    fn affine<const N: usize>(c: &Params<N>, p: &Point<N>) -> [Limbs<N>; 2] {
        let f = &c.p;
        let zi = f.inv(&p.z);
        let zi2 = f.sqr(&zi);
        [f.mul(&p.x, &zi2), f.mul(&p.y, &f.mul(&zi2, &zi))]
    }

    fn xorshift(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    /// wNAF digits rebuild the scalar and keep the digit rules.
    #[test]
    fn wnaf_properties() {
        fn check<const N: usize>(c: &Params<N>) {
            let mut seed = 12345u64;
            let mut ks: Vec<Limbs<N>> = (0..300)
                .map(|_| {
                    let mut k = [0u64; N];
                    k.iter_mut().for_each(|l| *l = xorshift(&mut seed));
                    if xorshift(&mut seed).is_multiple_of(4) {
                        k[N - 1] = u64::MAX; // top limb full: the last carry digit
                    }
                    k
                })
                .collect();
            let mut one = [0u64; N];
            one[0] = 1;
            ks.extend([[0; N], one, sub_raw(&c.n.m, &one).0, [u64::MAX; N]]);
            for k in &ks {
                for w in [WG, WQ, 2, 3, 8] {
                    let mut d = [0i8; MAX_DIGITS];
                    let len = wnaf(k, w, &mut d);
                    let mut last = None;
                    for (i, &x) in d.iter().enumerate() {
                        if x != 0 {
                            assert!(x % 2 != 0 && (x as i32).abs() < 1 << (w - 1), "digit {x} w={w}");
                            assert!(last.is_none_or(|l| i >= l + w), "gap");
                            assert!(i < len);
                            last = Some(i);
                        }
                    }
                    // Horner from the top, modulo n. The value 2^64N - 1 is >= n, so compare mod n.
                    let n = &c.n;
                    let mut acc = [0u64; N];
                    for i in (0..len).rev() {
                        acc = n.add(&acc, &acc);
                        let mut v = [0u64; N];
                        v[0] = (d[i] as i32).unsigned_abs() as u64;
                        acc = if d[i] < 0 { n.sub(&acc, &v) } else { n.add(&acc, &v) };
                    }
                    let mut want = *k;
                    while geq(&want, &n.m) {
                        want = sub_raw(&want, &n.m).0;
                    }
                    assert_eq!(acc, want, "w={w}");
                }
            }
        }
        check(&P256);
        check(&P384);
    }

    /// `mul_add` (wNAF, tables) equals the plain chain on random and edge scalars, with
    /// random Q and Q = +-G, 2G.
    #[test]
    fn mul_add_matches_reference() {
        fn check<const N: usize>(c: &Params<N>) {
            let f = &c.p;
            let mut seed = 777u64;
            let g = Point { x: f.to_mont(&c.gx), y: f.to_mont(&c.gy), z: f.one };
            let mut qs = vec![g, c.dbl(&g), Point { y: f.sub(&[0; N], &g.y), ..g }];
            let mut one = [0u64; N];
            one[0] = 1;
            let mut scalars: Vec<Limbs<N>> = vec![[0; N], one, sub_raw(&c.n.m, &one).0];
            for _ in 0..24 {
                let mut k = [0u64; N];
                k.iter_mut().for_each(|l| *l = xorshift(&mut seed));
                k[N - 1] >>= 1 + xorshift(&mut seed) % 3; // below n
                scalars.push(k);
            }
            for i in 0..4 {
                qs.push(c.mul_add(&scalars[3 + i], &[0; N], &g)); // arbitrary points kG
            }
            for q in &qs {
                for u1 in &scalars {
                    for u2 in scalars.iter().step_by(2) {
                        let (a, b) = (c.mul_add(u1, u2, q), mul_add_ref(c, u1, u2, q));
                        assert_eq!(is_zero(&a.z), is_zero(&b.z));
                        if !is_zero(&a.z) {
                            assert_eq!(affine(c, &a), affine(c, &b));
                        }
                    }
                }
            }
        }
        check(&P256);
        check(&P384);
    }

    // The constants must match the standard. Known values from SP 800-186 are used in the
    // vector files; here the checks are structural: p and n are odd and in the right range.
    #[test]
    fn constants_shape() {
        assert_eq!(P256.p.m[3], 0xffffffff00000001);
        assert_eq!(P384.n.m[5], u64::MAX);
        assert!(P256.p.m[0] & 1 == 1 && P384.p.m[0] & 1 == 1);
        // n < p for both curves, which `verify` relies on to keep r < p.
        assert!(!geq(&P256.n.m, &P256.p.m) && !geq(&P384.n.m, &P384.p.m));
    }

    /// Results that are infinity must be rejected. With Q = G and e = 1, r = n-1 gives
    /// (e + r)/s * G = nG = infinity. With Q = -G, r = e = 1 gives (e - r)/s * G = infinity.
    #[test]
    fn infinity_result_is_rejected() {
        for c in [Curve::P256, Curve::P384] {
            let size = c.size();
            let (gx, gy, order, p) = match c {
                Curve::P256 => (P256.gx.to_vec(), P256.gy.to_vec(), P256.n.m.to_vec(), P256.p.m.to_vec()),
                Curve::P384 => (P384.gx.to_vec(), P384.gy.to_vec(), P384.n.m.to_vec(), P384.p.m.to_vec()),
            };
            let be = |l: &[u64]| l.iter().rev().flat_map(|x| x.to_be_bytes()).collect::<Vec<u8>>();
            let g = |neg: bool| {
                let mut k = vec![4u8];
                k.extend(be(&gx));
                if neg {
                    // -G has y' = p - y.
                    let mut y = [0u64; 6];
                    y[..gy.len()].copy_from_slice(&gy);
                    let mut pp = [0u64; 6];
                    pp[..p.len()].copy_from_slice(&p);
                    k.extend(be(&sub_raw(&pp, &y).0[..gy.len()]));
                } else {
                    k.extend(be(&gy));
                }
                k
            };
            let mut digest = vec![0u8; size];
            digest[size - 1] = 1;
            let mut nm1 = be(&order);
            nm1[size - 1] -= 1; // the lowest byte of n is 0x51 / 0x73, so no borrow
            let mut one = vec![0u8; size];
            one[size - 1] = 1;
            let s = one.clone();
            assert_eq!(verify(c, &g(false), &digest, &[nm1.clone(), s.clone()].concat()), Err(Error::BadSignature));
            assert_eq!(verify(c, &g(true), &digest, &[one.clone(), s.clone()].concat()), Err(Error::BadSignature));
        }
    }

    #[test]
    fn range_and_encoding_errors() {
        // Valid key and signature from the vector file give a base to damage.
        let text = include_str!("../../../data/crypto/ecdsa/vectors.txt");
        let key_line = text.lines().find(|l| l.starts_with("key hand_p256_d1 ")).unwrap();
        let sig_line = text.lines().find(|l| l.starts_with("sig hand_p256_d1 ") && l.contains(" ok hand-openssl-d1-0")).unwrap();
        let key = unhex(key_line.split(' ').nth(3).unwrap());
        let sf: Vec<&str> = sig_line.split(' ').collect();
        let (digest, sig) = (unhex(sf[2]), unhex(sf[3]));
        let c = Curve::P256;
        assert_eq!(verify(c, &key, &digest, &sig), Ok(()));
        // The same bytes on the other curve fail.
        assert!(verify(Curve::P384, &key, &digest, &sig).is_err());
        // Digest length.
        assert_eq!(verify(c, &key, &digest[..31], &sig), Err(Error::BadDigest));
        assert_eq!(verify(c, &key, &[], &sig), Err(Error::BadDigest));
        // A 48-byte digest on P-256 is cut to its first 32 bytes (FIPS 186-5), so this passes.
        assert_eq!(verify(c, &key, &[digest.clone(), vec![0; 16]].concat(), &sig), Ok(()));
        assert!(verify(c, &key, &[vec![0; 16], digest.clone()].concat(), &sig).is_err());
        // Signature length.
        assert_eq!(verify(c, &key, &digest, &sig[..63]), Err(Error::BadSignature));
        assert_eq!(verify(c, &key, &digest, &[&sig[..], &[0]].concat()), Err(Error::BadSignature));
        // r = 0, s = 0, r = n, s = n, r = 2^256-1.
        let n = P256.n.m.iter().rev().flat_map(|x| x.to_be_bytes()).collect::<Vec<u8>>();
        for (r, s) in [
            (vec![0; 32], sig[32..].to_vec()),
            (sig[..32].to_vec(), vec![0; 32]),
            (n.clone(), sig[32..].to_vec()),
            (sig[..32].to_vec(), n.clone()),
            (vec![255; 32], sig[32..].to_vec()),
            (sig[..32].to_vec(), vec![255; 32]),
        ] {
            assert_eq!(verify(c, &key, &digest, &[r, s].concat()), Err(Error::BadSignature));
        }
        // Key: wrong prefix, compressed, hybrid, length, all zero, X >= p, Y >= p, off curve.
        let mut bad = key.clone();
        bad[0] = 2;
        assert_eq!(verify(c, &bad, &digest, &sig), Err(Error::BadPublicKey));
        bad[0] = 6;
        assert_eq!(verify(c, &bad, &digest, &sig), Err(Error::BadPublicKey));
        assert_eq!(verify(c, &key[..33], &digest, &sig), Err(Error::BadPublicKey));
        assert_eq!(verify(c, &key[1..], &digest, &sig), Err(Error::BadPublicKey));
        assert_eq!(verify(c, &[0], &digest, &sig), Err(Error::BadPublicKey));
        assert_eq!(verify(c, &[&[4u8][..], &[0u8; 64]].concat(), &digest, &sig), Err(Error::BadPublicKey));
        let p = P256.p.m.iter().rev().flat_map(|x| x.to_be_bytes()).collect::<Vec<u8>>();
        let mut xp = key.clone();
        xp[1..33].copy_from_slice(&p); // X = p, which is X = 0 mod p but not canonical
        assert_eq!(verify(c, &xp, &digest, &sig), Err(Error::BadPublicKey));
        let mut yp = key.clone();
        yp[33..].copy_from_slice(&[255; 32]);
        assert_eq!(verify(c, &yp, &digest, &sig), Err(Error::BadPublicKey));
        let mut off = key.clone();
        off[64] ^= 1;
        assert_eq!(verify(c, &off, &digest, &sig), Err(Error::BadPublicKey));
    }

    /// A signature must also fail for a flipped bit anywhere in r, s or the digest.
    #[test]
    fn every_flipped_bit_fails() {
        let text = include_str!("../../../data/crypto/ecdsa/vectors.txt");
        let key_line = text.lines().find(|l| l.starts_with("key hand_p384_d2 ")).unwrap();
        let sig_line = text.lines().find(|l| l.starts_with("sig hand_p384_d2 ") && l.contains(" ok hand-openssl-d2-1")).unwrap();
        let key = unhex(key_line.split(' ').nth(3).unwrap());
        let sf: Vec<&str> = sig_line.split(' ').collect();
        let (digest, sig) = (unhex(sf[2]), unhex(sf[3]));
        assert_eq!(verify(Curve::P384, &key, &digest, &sig), Ok(()));
        for i in 0..sig.len() * 8 {
            let mut s = sig.clone();
            s[i / 8] ^= 1 << (i % 8);
            assert!(verify(Curve::P384, &key, &digest, &s).is_err(), "sig bit {i}");
        }
        for i in 0..digest.len() * 8 {
            let mut d = digest.clone();
            d[i / 8] ^= 1 << (i % 8);
            assert!(verify(Curve::P384, &key, &d, &sig).is_err(), "digest bit {i}");
        }
    }

    // RFC 5903 section 8.1 (256-bit random ECP group): both private values, both public keys, the shared X.
    const I: &str = "c88f01f510d9ac3f70a292daa2316de544e9aab8afe84049c62a9c57862d1433";
    const R: &str = "c6ef9c5d78ae012a011164acb397ce2088685d8f06bf9be0b283ab46476bee53";
    const GI: &str = "04dad0b65394221cf9b051e1feca5787d098dfe637fc90b9ef945d0c37725811805271a0461cdb8252d61f1c456fa3e59ab1f45b33accf5f58389e0577b8990bb3";
    const GR: &str = "04d12dfb5289c8d4f81208b70270398c342296970a0bccb74c736fc7554494bf6356fbf3ca366cc23e8157854c13c58d6aac23f046ada30f8353e74f33039872ab";
    const Z: &str = "d6840f6b42f6edafd13116e0e12565202fef8e9ece7dce03812464d04b9442de";

    #[test]
    fn p256_ecdh_rfc5903() {
        let (i, r): ([u8; 32], [u8; 32]) = (unhex(I).try_into().unwrap(), unhex(R).try_into().unwrap());
        assert_eq!(p256_public(&i).unwrap()[..], unhex(GI)[..]);
        assert_eq!(p256_public(&r).unwrap()[..], unhex(GR)[..]);
        assert_eq!(p256_shared(&i, &unhex(GR)).unwrap()[..], unhex(Z)[..]);
        assert_eq!(p256_shared(&r, &unhex(GI)).unwrap()[..], unhex(Z)[..]);
    }

    #[test]
    fn p256_ecdh_refuses_bad_input() {
        let i: [u8; 32] = unhex(I).try_into().unwrap();
        let good = unhex(GR);
        // Wrong prefix or length, a coordinate that is not below p, a point off the curve, and the point at infinity.
        let mut bad = vec![good.clone(), good[..64].to_vec(), good[..1].to_vec(), vec![]];
        bad[0][0] = 2;
        let mut big = good.clone();
        big[1..33].fill(0xff);
        bad.push(big);
        let mut off = good.clone();
        off[64] ^= 1;
        bad.push(off);
        bad.push([&[4u8][..], &[0; 64]].concat());
        for b in bad {
            assert!(p256_shared(&i, &b).is_err(), "{}", b.len());
        }
        // A zero scalar, and n itself (zero mod n), make no key. n + 1 makes the key of 1.
        let n = unhex("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551");
        assert!(p256_public(&[0; 32]).is_none() && p256_public(&n.clone().try_into().unwrap()).is_none());
        let mut n1 = n;
        n1[31] += 1;
        let g = p256_public(&n1.try_into().unwrap()).unwrap();
        let mut one = [0u8; 32];
        one[31] = 1;
        assert_eq!(g, p256_public(&one).unwrap());
        assert_eq!(g[1..33], unhex("6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296")[..]);
        // The shared secret of two fresh pairs agrees.
        let (a, b) = ([0x31u8; 32], [0x77u8; 32]);
        assert_eq!(p256_shared(&a, &p256_public(&b).unwrap()), p256_shared(&b, &p256_public(&a).unwrap()));
    }
}
