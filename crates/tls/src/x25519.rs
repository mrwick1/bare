//! X25519, as in RFC 7748 section 5. Test vectors: RFC 7748 sections 5.2 and 6.1.
//!
//! X25519 turns a 32-byte secret scalar and a 32-byte public point (the u-coordinate on
//! Curve25519) into a 32-byte shared value. The math lives in the field of integers mod
//! p = 2^255 - 19. A field element is five `u64` limbs of 51 bits each (radix 2^51):
//! value = l0 + l1*2^51 + l2*2^102 + l3*2^153 + l4*2^204. A limb has 13 spare bits, so
//! sums of products fit in `u128` and we carry only once per multiplication.
//! Because 2^255 = 19 (mod p), a carry out of the top limb goes back into limb 0 times 19.
//!
//! Constant-time design (source level only, see the limits below):
//! - The ladder runs a fixed 255 steps. Loop counts and indexes depend only on public data.
//! - The secret bit chooses between two values with a mask (`cswap`), never with `if`.
//! - There are no tables and no array indexes that come from secret data.
//! - Field code has no data-dependent branches. `/` and `%` are never used on secrets.
//!
//! Limits: source-level structure is not a timing guarantee. Other targets and compilers
//! may change the shape of this code. Secrets are not wiped from the stack after use. Hand-written crypto is for
//! learning only. See the warning in lib.rs.

use std::fmt;

/// Bytes in a scalar, a u-coordinate and a shared value.
pub const KEY_LEN: usize = 32;

/// The base point, u = 9 (RFC 7748 section 4.1).
pub const BASEPOINT: [u8; KEY_LEN] = {
    let mut b = [0u8; KEY_LEN];
    b[0] = 9;
    b
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum X25519Error {
    /// The shared value is all zeros. The peer sent a low-order point. TLS 1.3 says abort (RFC 8446 section 7.4.2).
    AllZero,
}

impl fmt::Display for X25519Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            X25519Error::AllZero => write!(f, "X25519 shared secret is all zero (low-order peer key)"),
        }
    }
}
impl std::error::Error for X25519Error {}

/// The `X25519(k, u)` function of RFC 7748, bit for bit. It clamps `scalar`, masks the top
/// bit of `u`, and accepts every 32-byte `u`, also values >= p (they are taken mod p).
/// It does NOT reject low-order points: the result is then all zeros. TLS code must use
/// `shared_secret`. This function stays public for the RFC vectors.
pub fn x25519(scalar: &[u8; KEY_LEN], u: &[u8; KEY_LEN]) -> [u8; KEY_LEN] {
    let mut k = *scalar;
    k[0] &= 248; // The low 3 bits become 0: the scalar is a multiple of 8 (the cofactor).
    k[31] &= 127; // The top bit becomes 0 ...
    k[31] |= 64; // ... and bit 254 becomes 1, so every scalar has the same length.

    let x1 = Fe::from_bytes(u);
    let (mut x2, mut z2) = (Fe::ONE, Fe::ZERO);
    let (mut x3, mut z3) = (x1, Fe::ONE);
    let mut swap = 0u64;
    // Montgomery ladder (RFC 7748 section 5). Each step does one differential add and one double.
    for t in (0..255).rev() {
        let bit = ((k[t / 8] >> (t % 8)) & 1) as u64;
        swap ^= bit;
        Fe::cswap(swap, &mut x2, &mut x3);
        Fe::cswap(swap, &mut z2, &mut z3);
        swap = bit;

        let a = x2.add(&z2);
        let aa = a.square();
        let b = x2.sub(&z2);
        let bb = b.square();
        let e = aa.sub(&bb);
        let c = x3.add(&z3);
        let d = x3.sub(&z3);
        let da = d.mul(&a);
        let cb = c.mul(&b);
        x3 = da.add(&cb).square();
        z3 = x1.mul(&da.sub(&cb).square());
        x2 = aa.mul(&bb);
        z2 = e.mul(&aa.add(&e.mul_small(121665)));
    }
    Fe::cswap(swap, &mut x2, &mut x3);
    Fe::cswap(swap, &mut z2, &mut z3);
    // x2 / z2 in the field. If z2 = 0 (low-order input), invert gives 0, so the result is 0.
    x2.mul(&z2.invert()).to_bytes()
}

/// The public key for a secret scalar: `X25519(scalar, 9)`.
pub fn public_key(scalar: &[u8; KEY_LEN]) -> [u8; KEY_LEN] {
    x25519(scalar, &BASEPOINT)
}

/// The TLS-facing call: the shared secret, or `AllZero` if the peer key is a low-order point.
pub fn shared_secret(scalar: &[u8; KEY_LEN], peer_public: &[u8; KEY_LEN]) -> Result<[u8; KEY_LEN], X25519Error> {
    let out = x25519(scalar, peer_public);
    // OR all bytes together. The loop has no early exit. The branch below leaks only
    // "was it zero", which the peer learns anyway from the aborted handshake.
    let acc = out.iter().fold(0u8, |a, &b| a | b);
    if acc == 0 { Err(X25519Error::AllZero) } else { Ok(out) }
}

const MASK51: u64 = (1 << 51) - 1;

/// A field element mod 2^255 - 19. `Copy` means assignment copies the 40 bytes (like a JS number).
/// Limb bounds: outputs of `mul`, `square`, `mul_small` and `from_bytes` are "tight": limbs
/// below 2^51 + 2^13. `add` of two tight values and `sub` of two tight values give limbs
/// below 2^54, with no carry. `mul` and `square` accept limbs below 2^54, so the ladder
/// never needs a carry after an add or a sub. The debug-build tests run the extremes:
/// Rust checks every `+` and `*` for overflow there and panics.
#[derive(Clone, Copy)]
struct Fe([u64; 5]);

impl Fe {
    const ZERO: Fe = Fe([0; 5]);
    const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    /// Reads 32 bytes little-endian. The top bit is masked off (RFC 7748 section 5).
    /// A value in [p, 2^255) stays as is: it equals (value - p) mod p, so no check is needed.
    fn from_bytes(b: &[u8; KEY_LEN]) -> Fe {
        let w = |i: usize| u64::from_le_bytes(b[8 * i..8 * i + 8].try_into().unwrap());
        let (w0, w1, w2, w3) = (w(0), w(1), w(2), w(3) & (u64::MAX >> 1));
        Fe([
            w0 & MASK51,
            ((w0 >> 51) | (w1 << 13)) & MASK51,
            ((w1 >> 38) | (w2 << 26)) & MASK51,
            ((w2 >> 25) | (w3 << 39)) & MASK51,
            w3 >> 12,
        ])
    }

    /// Carry each limb into the next. The top carry wraps to limb 0 times 19.
    /// Limbs go in below 2^63 and come out below 2^51 + 2^13.
    fn carry(l: [u64; 5]) -> Fe {
        let c0 = l[0] >> 51;
        let c1 = (l[1] + c0) >> 51;
        let c2 = (l[2] + c1) >> 51;
        let c3 = (l[3] + c2) >> 51;
        let c4 = (l[4] + c3) >> 51;
        Fe([
            (l[0] & MASK51) + 19 * c4,
            (l[1] + c0) & MASK51,
            (l[2] + c1) & MASK51,
            (l[3] + c2) & MASK51,
            (l[4] + c3) & MASK51,
        ])
    }

    /// The canonical 32 bytes, fully reduced into [0, p).
    fn to_bytes(self) -> [u8; KEY_LEN] {
        let h = Fe::carry(self.0).0;
        // Is h >= p? Add 19 and see whether the sum reaches 2^255: the carry q is then 1.
        let mut q = (h[0] + 19) >> 51;
        q = (h[1] + q) >> 51;
        q = (h[2] + q) >> 51;
        q = (h[3] + q) >> 51;
        q = (h[4] + q) >> 51;
        // Add 19*q, then drop bit 255. This subtracts p exactly when h >= p.
        let h = Fe([h[0] + 19 * q, h[1], h[2], h[3], h[4]]);
        let l = Fe::carry_no_wrap(h.0);
        let mut out = [0u8; KEY_LEN];
        // Repack five 51-bit limbs into four 64-bit words.
        let w = [
            l[0] | (l[1] << 51),
            (l[1] >> 13) | (l[2] << 38),
            (l[2] >> 26) | (l[3] << 25),
            (l[3] >> 39) | (l[4] << 12),
        ];
        for i in 0..4 {
            out[8 * i..8 * i + 8].copy_from_slice(&w[i].to_le_bytes());
        }
        out
    }

    /// Like `carry`, but the carry out of limb 4 is dropped (used after the `+19q` trick).
    fn carry_no_wrap(l: [u64; 5]) -> [u64; 5] {
        let c0 = l[0] >> 51;
        let c1 = (l[1] + c0) >> 51;
        let c2 = (l[2] + c1) >> 51;
        let c3 = (l[3] + c2) >> 51;
        [l[0] & MASK51, (l[1] + c0) & MASK51, (l[2] + c1) & MASK51, (l[3] + c2) & MASK51, (l[4] + c3) & MASK51]
    }

    fn add(&self, o: &Fe) -> Fe {
        let (a, b) = (&self.0, &o.0);
        Fe([a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3], a[4] + b[4]])
    }

    /// a - b, without a carry. We add 4p first so no limb goes below 0. 4p in limbs is
    /// (2^53 - 76, 2^53 - 4, ...). This needs every limb of `o` below 2^53 - 76.
    fn sub(&self, o: &Fe) -> Fe {
        const FOUR_P0: u64 = 4 * (MASK51 - 18);
        const FOUR_P: u64 = 4 * MASK51;
        let (a, b) = (&self.0, &o.0);
        Fe([
            a[0] + FOUR_P0 - b[0],
            a[1] + FOUR_P - b[1],
            a[2] + FOUR_P - b[2],
            a[3] + FOUR_P - b[3],
            a[4] + FOUR_P - b[4],
        ])
    }

    /// Schoolbook product of two 5-limb numbers. Terms of weight 2^255 and above are
    /// folded back with the factor 19 (because 2^255 = 19). `as u128` widens a `u64`.
    fn mul(&self, o: &Fe) -> Fe {
        let (a, b) = (&self.0, &o.0);
        let m = |x: u64, y: u64| x as u128 * y as u128;
        let (b1, b2, b3, b4) = (19 * b[1], 19 * b[2], 19 * b[3], 19 * b[4]);
        let r0 = m(a[0], b[0]) + m(a[1], b4) + m(a[2], b3) + m(a[3], b2) + m(a[4], b1);
        let r1 = m(a[0], b[1]) + m(a[1], b[0]) + m(a[2], b4) + m(a[3], b3) + m(a[4], b2);
        let r2 = m(a[0], b[2]) + m(a[1], b[1]) + m(a[2], b[0]) + m(a[3], b4) + m(a[4], b3);
        let r3 = m(a[0], b[3]) + m(a[1], b[2]) + m(a[2], b[1]) + m(a[3], b[0]) + m(a[4], b4);
        let r4 = m(a[0], b[4]) + m(a[1], b[3]) + m(a[2], b[2]) + m(a[3], b[1]) + m(a[4], b[0]);
        Fe::reduce_wide([r0, r1, r2, r3, r4])
    }

    /// a*a with 15 products instead of 25: a_i*a_j and a_j*a_i are the same, so we count
    /// them once and double one factor. Limbs must be below 2^54 so the sums fit.
    fn square(&self) -> Fe {
        let a = &self.0;
        let m = |x: u64, y: u64| x as u128 * y as u128;
        let (d0, d1, d2, d3) = (2 * a[0], 2 * a[1], 2 * a[2], 2 * a[3]);
        let (a3_19, a4_19) = (19 * a[3], 19 * a[4]);
        let r0 = m(a[0], a[0]) + m(d1, a4_19) + m(d2, a3_19);
        let r1 = m(d0, a[1]) + m(d2, a4_19) + m(a[3], a3_19);
        let r2 = m(d0, a[2]) + m(a[1], a[1]) + m(d3, a4_19);
        let r3 = m(d0, a[3]) + m(d1, a[2]) + m(a[4], a4_19);
        let r4 = m(d0, a[4]) + m(d1, a[3]) + m(a[2], a[2]);
        Fe::reduce_wide([r0, r1, r2, r3, r4])
    }

    /// Multiply by a small constant below 2^32.
    fn mul_small(&self, k: u64) -> Fe {
        let a = &self.0;
        let m = |x: u64| x as u128 * k as u128;
        Fe::reduce_wide([m(a[0]), m(a[1]), m(a[2]), m(a[3]), m(a[4])])
    }

    /// Carry five wide accumulators down to 51-bit limbs. The top carry is below 2^61 for
    /// our bounds, so `19 * c4` fits in `u64`.
    fn reduce_wide(r: [u128; 5]) -> Fe {
        let m = MASK51 as u128;
        let c0 = r[0] >> 51;
        let r1 = r[1] + c0;
        let c1 = r1 >> 51;
        let r2 = r[2] + c1;
        let c2 = r2 >> 51;
        let r3 = r[3] + c2;
        let c3 = r3 >> 51;
        let r4 = r[4] + c3;
        let c4 = (r4 >> 51) as u64;
        let l0 = (r[0] & m) as u64 + 19 * c4;
        // One more short carry so limb 0 is below 2^51 again.
        Fe([l0 & MASK51, (r1 & m) as u64 + (l0 >> 51), (r2 & m) as u64, (r3 & m) as u64, (r4 & m) as u64])
    }

    /// Swap `a` and `b` if `swap` is 1, keep them if 0. No branch: `mask` is all ones or all zeros.
    fn cswap(swap: u64, a: &mut Fe, b: &mut Fe) {
        let mask = 0u64.wrapping_sub(swap);
        for i in 0..5 {
            let t = mask & (a.0[i] ^ b.0[i]);
            a.0[i] ^= t;
            b.0[i] ^= t;
        }
    }

    fn square_n(&self, n: u32) -> Fe {
        let mut r = *self;
        for _ in 0..n {
            r = r.square();
        }
        r
    }

    /// z^(p-2) = z^(2^255 - 21), the inverse by Fermat's little theorem. The chain of
    /// squarings is fixed, so the time does not depend on z. 0 maps to 0.
    fn invert(&self) -> Fe {
        let z2 = self.square();
        let z9 = z2.square_n(2).mul(self); // z^9
        let z11 = z9.mul(&z2); // z^11
        let z_5 = z11.square().mul(&z9); // z^(2^5 - 1)
        let z_10 = z_5.square_n(5).mul(&z_5); // z^(2^10 - 1)
        let z_20 = z_10.square_n(10).mul(&z_10);
        let z_40 = z_20.square_n(20).mul(&z_20);
        let z_50 = z_40.square_n(10).mul(&z_10);
        let z_100 = z_50.square_n(50).mul(&z_50);
        let z_200 = z_100.square_n(100).mul(&z_100);
        let z_250 = z_200.square_n(50).mul(&z_50);
        z_250.square_n(5).mul(&z11) // z^(2^255 - 32 + 11)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: &str) -> [u8; 32] {
        let mut o = [0u8; 32];
        for i in 0..32 {
            o[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
        }
        o
    }

    #[test]
    fn rfc7748_section_5_2_vectors() {
        let r = x25519(
            &h("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4"),
            &h("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c"),
        );
        assert_eq!(r, h("c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552"));
        let r = x25519(
            &h("4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d"),
            &h("e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493"),
        );
        assert_eq!(r, h("95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957"));
    }

    /// The RFC iteration: k = u = 9; then repeat (k, u) = (X25519(k, u), k).
    fn iterate(n: u32) -> [u8; 32] {
        let (mut k, mut u) = (BASEPOINT, BASEPOINT);
        for _ in 0..n {
            let next = x25519(&k, &u);
            u = k;
            k = next;
        }
        k
    }

    #[test]
    fn rfc7748_iterations_1_and_1000() {
        assert_eq!(iterate(1), h("422c8e7a6227d7bca1350b3e2bb7279f7897b87bb6854b783c60e80311ae3079"));
        assert_eq!(iterate(1000), h("684cf59ba83309552800ef566f2f4d3c1c3887c49360e3875f2eb94d99532c51"));
    }

    /// 1,000,000 iterations: about a minute in release. Run: rustc --edition=2024 --test -O ... -- --ignored
    #[test]
    #[ignore]
    fn rfc7748_iteration_1_000_000() {
        assert_eq!(iterate(1_000_000), h("7c3911e0ab2586fd864497297e575e6f3bc601c0883c30df5f4dd2d24f665424"));
    }

    #[test]
    fn rfc7748_section_6_1_alice_and_bob() {
        let a = h("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        let b = h("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
        let (pa, pb) = (public_key(&a), public_key(&b));
        assert_eq!(pa, h("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"));
        assert_eq!(pb, h("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f"));
        let k = h("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
        assert_eq!(shared_secret(&a, &pb), Ok(k));
        assert_eq!(shared_secret(&b, &pa), Ok(k));
    }

    #[test]
    fn clamping_ignores_the_bits_the_rfc_masks() {
        let k = h("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4");
        let u = h("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c");
        let want = x25519(&k, &u);
        let mut k2 = k;
        k2[0] ^= 7; // low 3 bits are cleared
        k2[31] ^= 0x80; // bit 255 is cleared
        assert_eq!(x25519(&k2, &u), want);
        let mut k3 = k;
        k3[31] &= !0x40; // bit 254 is forced to 1: clearing it changes nothing
        assert_eq!(x25519(&k3, &u), want);
        // A scalar of all zeros and of all ones still run and are clamped the same way.
        assert_eq!(x25519(&[0; 32], &BASEPOINT), x25519(&{ let mut z = [0u8; 32]; z[31] = 0x40; z }, &BASEPOINT));
        assert_eq!(x25519(&[0xff; 32], &BASEPOINT), x25519(&{ let mut z = [0xffu8; 32]; z[0] = 248; z[31] = 127; z }, &BASEPOINT));
    }

    #[test]
    fn top_bit_of_u_is_masked() {
        let k = h("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4");
        let u = h("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c");
        let mut hi = u;
        hi[31] ^= 0x80;
        assert_eq!(x25519(&k, &u), x25519(&k, &hi));
    }

    #[test]
    fn noncanonical_u_equals_reduced_u() {
        // u + p for small u: the bytes of p + u are ed ff .. ff 7f plus u. RFC 7748 says reduce mod p.
        let k = h("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        for small in [2u8, 3, 9, 17] {
            let mut nc = [0xffu8; 32];
            nc[0] = 0xed + small; // 0xed + 17 < 0x100, so no byte carry
            nc[31] = 0x7f;
            let mut canon = [0u8; 32];
            canon[0] = small;
            assert_eq!(x25519(&k, &nc), x25519(&k, &canon), "u = p + {small}");
            nc[31] = 0xff; // and with the top bit set too
            assert_eq!(x25519(&k, &nc), x25519(&k, &canon), "top bit, u = p + {small}");
        }
        // The largest value, 2^255 - 1 (top bit masked), is p + 18.
        let mut canon = [0u8; 32];
        canon[0] = 18;
        assert_eq!(x25519(&k, &[0xff; 32]), x25519(&k, &canon));
    }

    #[test]
    fn low_order_points_give_all_zero_and_are_rejected() {
        let k = h("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        let points = [
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0100000000000000000000000000000000000000000000000000000000000000",
            "e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800",
            "5f9c95bca3508c24b1d0b1559c83ef5b04445cc4581c8e86d8224eddd09f1157",
            "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
            "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
            "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
        ];
        for p in points {
            let mut u = h(p);
            for top in [0x00u8, 0x80] {
                u[31] = (u[31] & 0x7f) | top;
                assert_eq!(x25519(&k, &u), [0; 32], "raw {p} top {top:#x}");
                assert_eq!(shared_secret(&k, &u), Err(X25519Error::AllZero), "{p} top {top:#x}");
            }
        }
    }

    #[test]
    fn field_round_trip_and_reduction() {
        // to_bytes(from_bytes(x)) is x for canonical x, and x - p for p <= x < 2^255.
        let mut x = [0u8; 32];
        for (i, byte) in x.iter_mut().enumerate() {
            *byte = (i as u8).wrapping_mul(37).wrapping_add(11);
        }
        x[31] &= 0x7f;
        x[31] &= 0x3f; // below p
        assert_eq!(Fe::from_bytes(&x).to_bytes(), x);
        let p_plus_5 = {
            let mut b = [0xffu8; 32];
            b[0] = 0xed + 5;
            b[31] = 0x7f;
            b
        };
        let mut five = [0u8; 32];
        five[0] = 5;
        assert_eq!(Fe::from_bytes(&p_plus_5).to_bytes(), five);
        // invert: x * x^-1 = 1.
        let fx = Fe::from_bytes(&x);
        let mut one = [0u8; 32];
        one[0] = 1;
        assert_eq!(fx.mul(&fx.invert()).to_bytes(), one);
        assert_eq!(Fe::ZERO.invert().to_bytes(), [0; 32]);
        // The loosest ladder inputs: tight limbs (2^51 + 2^13 - 1) through add and sub, then mul and square.
        let t = Fe([(1 << 51) + (1 << 13) - 1; 5]);
        let z = Fe([0; 5]);
        for v in [t.add(&t), t.sub(&z), z.sub(&t)] {
            assert_eq!(v.square().to_bytes(), v.mul(&v).to_bytes());
            assert_eq!(v.mul(&t).to_bytes(), t.mul(&v).to_bytes());
            let _ = v.mul_small(121665);
        }
        // square must equal mul(self, self) for extreme limbs.
        for fill in [0u64, 1, MASK51, (1 << 54) - 1, 1 << 53] {
            let f = Fe([fill; 5]);
            assert_eq!(f.square().to_bytes(), f.mul(&f).to_bytes(), "limbs {fill:#x}");
        }
    }
}
