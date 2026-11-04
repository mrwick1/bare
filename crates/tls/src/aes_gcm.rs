//! AES-128-GCM, written from the specifications. Learning code, NOT audited.
//!
//! Sources:
//! - FIPS 197 (AES): <https://csrc.nist.gov/pubs/fips/197/final>
//! - NIST SP 800-38D (GCM): <https://csrc.nist.gov/pubs/sp/800/38/d/final>
//! - RFC 8446 section 5.3 (TLS 1.3 per-record nonce).
//!
//! Two backends give the same bytes:
//! - Portable: no table lookup and no branch on secret data. The S-box is computed
//!   as a GF(2^8) inverse (x^254) plus the affine map, 16 bytes at a time with
//!   integer lanes in a `u128`. GHASH is a masked bit-by-bit GF(2^128) multiply.
//! - x86_64: AES-NI and PCLMULQDQ through `std::arch`, chosen at run time by
//!   feature detection. Other CPUs use the portable backend.
//!
//! Rules for callers:
//! - A (key, nonce) pair must never seal two different messages. This module
//!   cannot check that. For TLS 1.3 use [`tls13_nonce`] with a per-record sequence number.
//! - `open_in_place_detached` leaves the buffer unchanged when it fails.
//! - Lengths are bounded as in SP 800-38D: data at most 2^36-32 bytes, AAD at most 2^61-1 bytes.

pub const KEY_LEN: usize = 16;
pub const NONCE_LEN: usize = 12;
pub const TAG_LEN: usize = 16;
/// Largest data length for one (key, nonce): the 32-bit block counter starts at 2
/// and must not wrap, so (2^32 - 2) blocks of 16 bytes.
pub const MAX_DATA_LEN: u64 = (1 << 36) - 32;
/// Largest AAD length: the bit length must fit in 64 bits.
pub const MAX_AAD_LEN: u64 = (1 << 61) - 1;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Error {
    /// Data or AAD is longer than the limits above.
    TooLong,
    /// The tag does not match.
    AuthFailed,
}

/// TLS 1.3 per-record nonce (RFC 8446 section 5.3): the 64-bit sequence number,
/// big-endian and left-padded to 12 bytes, XOR the static write IV.
pub fn tls13_nonce(iv: &[u8; NONCE_LEN], seq: u64) -> [u8; NONCE_LEN] {
    let mut n = *iv;
    for (n, s) in n[4..].iter_mut().zip(seq.to_be_bytes()) {
        *n ^= s;
    }
    n
}

fn check_lens(aad_len: u64, data_len: u64) -> Result<(), Error> {
    if aad_len > MAX_AAD_LEN || data_len > MAX_DATA_LEN {
        return Err(Error::TooLong);
    }
    Ok(())
}

/// AES-128-GCM key. Holds the round keys and the GHASH key. Zeroed on drop.
pub struct Aes128Gcm {
    rk: [[u8; 16]; 11],
    h: u128,
    /// H^1..H^8 in the byte-reflected form used by the PCLMUL code. Zero when `hw` is false.
    hpow: [[u8; 16]; 8],
    hw: bool,
}

impl Drop for Aes128Gcm {
    fn drop(&mut self) {
        // Volatile writes so the compiler does not remove the wipe.
        // SAFETY: each pointer is a valid, aligned, exclusive reference to a field.
        unsafe {
            std::ptr::write_volatile(&mut self.rk, [[0; 16]; 11]);
            std::ptr::write_volatile(&mut self.h, 0);
            std::ptr::write_volatile(&mut self.hpow, [[0; 16]; 8]);
        }
    }
}

impl Aes128Gcm {
    /// Uses AES-NI and PCLMULQDQ when the CPU has them, else the portable code.
    pub fn new(key: &[u8; KEY_LEN]) -> Self {
        let mut s = Self::new_portable(key);
        #[cfg(target_arch = "x86_64")]
        if hw::available() {
            // SAFETY: `available` checked the CPU features.
            s.hpow = unsafe { hw::powers(&s.h.to_be_bytes()) };
            s.hw = true;
        }
        s
    }

    /// Always the portable backend. Used for tests and as the reference.
    pub fn new_portable(key: &[u8; KEY_LEN]) -> Self {
        let rk = expand_key(key);
        // H = E(K, 0^128). `encrypt_block` works on little-endian blocks; GHASH on big-endian.
        let h = u128::from_be_bytes(encrypt_block(&rk, 0).to_le_bytes());
        Aes128Gcm { rk, h, hpow: [[0; 16]; 8], hw: false }
    }

    pub fn is_accelerated(&self) -> bool {
        self.hw
    }

    /// Encrypts `buf` in place and returns the tag. `nonce` must be unique per key.
    pub fn seal_in_place_detached(
        &self,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        buf: &mut [u8],
    ) -> Result<[u8; TAG_LEN], Error> {
        check_lens(aad.len() as u64, buf.len() as u64)?;
        self.ctr(nonce, 2, buf);
        Ok(self.tag(nonce, aad, buf))
    }

    /// Checks the tag over `aad` and the ciphertext in `buf`, then decrypts in place.
    /// On `Err`, `buf` is not changed: no unauthenticated plaintext is made.
    pub fn open_in_place_detached(
        &self,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        buf: &mut [u8],
        tag: &[u8; TAG_LEN],
    ) -> Result<(), Error> {
        check_lens(aad.len() as u64, buf.len() as u64)?;
        let expect = self.tag(nonce, aad, buf);
        let mut diff = 0u8;
        for (a, b) in expect.iter().zip(tag) {
            diff |= a ^ b;
        }
        if diff != 0 {
            return Err(Error::AuthFailed);
        }
        self.ctr(nonce, 2, buf);
        Ok(())
    }

    /// Tag = GHASH(aad, ct) XOR E(K, nonce || 1).
    fn tag(&self, nonce: &[u8; NONCE_LEN], aad: &[u8], ct: &[u8]) -> [u8; TAG_LEN] {
        let mut mask = [0u8; 16];
        self.ctr(nonce, 1, &mut mask);
        let mut t = self.ghash(aad, ct);
        for (t, m) in t.iter_mut().zip(mask) {
            *t ^= m;
        }
        t
    }

    /// XOR the CTR keystream, starting at counter value `ctr`, into `buf`.
    /// Callers keep `ctr + blocks <= 2^32` (see `MAX_DATA_LEN`), so it never wraps.
    fn ctr(&self, nonce: &[u8; NONCE_LEN], ctr: u32, buf: &mut [u8]) {
        #[cfg(target_arch = "x86_64")]
        if self.hw {
            // SAFETY: `hw` is only set after `hw::available()` returned true.
            return unsafe { hw::ctr_xor(&self.rk, nonce, ctr, buf) };
        }
        ctr_portable(&self.rk, nonce, ctr, buf)
    }

    fn ghash(&self, aad: &[u8], ct: &[u8]) -> [u8; 16] {
        #[cfg(target_arch = "x86_64")]
        if self.hw {
            // SAFETY: as in `ctr`.
            return unsafe { hw::ghash(&self.hpow, aad, ct) };
        }
        ghash_portable(self.h, aad, ct)
    }
}

// ---------------------------------------------------------------- portable AES

// Each byte of a u128 is one lane. All ops below are the same for every input.
const M1: u128 = 0x0101_0101_0101_0101_0101_0101_0101_0101;
const M7F: u128 = 0x7f7f_7f7f_7f7f_7f7f_7f7f_7f7f_7f7f_7f7f;

/// Lane value 0 or 1 -> 0x00 or 0xff, without a multiply.
fn spread(m: u128) -> u128 {
    (m << 8) - m
}

/// Multiply every byte lane by x in GF(2^8) (polynomial x^8+x^4+x^3+x+1).
fn xtime(a: u128) -> u128 {
    let h = (a >> 7) & M1; // lane 0 or 1; times 0x1b = h ^ h<<1 ^ h<<3 ^ h<<4
    ((a & M7F) << 1) ^ h ^ (h << 1) ^ (h << 3) ^ (h << 4)
}

/// Lane-wise GF(2^8) product. The loop runs 8 times for any input.
fn gmul(mut a: u128, b: u128) -> u128 {
    let mut r = 0;
    for i in 0..8 {
        r ^= a & spread((b >> i) & M1);
        a = xtime(a);
    }
    r
}

/// Squares of the basis bytes x^0..x^7 in GF(2^8). Squaring is linear over GF(2).
const SQ: [u8; 8] = {
    let mut t = [0u8; 8];
    let mut i = 0;
    while i < 8 {
        let (mut a, mut r, mut b) = (1u8 << i, 0u8, 1u8 << i);
        while b != 0 {
            if b & 1 != 0 {
                r ^= a;
            }
            a = (a << 1) ^ if a & 0x80 != 0 { 0x1b } else { 0 };
            b >>= 1;
        }
        t[i] = r;
        i += 1;
    }
    t
};

/// Lane-wise square: XOR of the squared basis bytes for the set bits.
fn gsq(a: u128) -> u128 {
    let mut r = 0;
    for i in 0..8 {
        r ^= spread((a >> i) & M1) & (SQ[i] as u128 * M1);
    }
    r
}

/// Rotate every byte lane left by `k` bits (1..=7).
fn rotl8(x: u128, k: u32) -> u128 {
    let hi = ((0xffu128 << k) & 0xff) * M1;
    let lo = ((1u128 << k) - 1) * M1;
    ((x << k) & hi) | ((x >> (8 - k)) & lo)
}

/// AES S-box on 16 lanes: inverse x^254 (0 maps to 0), then the affine map.
fn sub_bytes(x: u128) -> u128 {
    // Addition chain for x^254: x^2, x^3, x^12, x^15, x^240, x^252, x^254.
    let x2 = gsq(x);
    let x3 = gmul(x2, x);
    let x12 = gsq(gsq(x3));
    let x15 = gmul(x12, x3);
    let x240 = gsq(gsq(gsq(gsq(x15))));
    let inv = gmul(gmul(x240, x12), x2);
    inv ^ rotl8(inv, 1) ^ rotl8(inv, 2) ^ rotl8(inv, 3) ^ rotl8(inv, 4) ^ (0x63 * M1)
}

/// Row r of the state (byte i is row i%4, column i/4) rotates left by r columns.
/// The indices are fixed, so no secret picks a position.
fn shift_rows(x: u128) -> u128 {
    let s = x.to_le_bytes();
    let mut o = [0u8; 16];
    for c in 0..4 {
        for r in 0..4 {
            o[r + 4 * c] = s[r + 4 * ((c + r) % 4)];
        }
    }
    u128::from_le_bytes(o)
}

/// Rotate each 32-bit column so that row r+k moves to row r.
fn col_rot(x: u128, k: u32) -> u128 {
    let keep = (((1u128 << (32 - 8 * k)) - 1) as u32) as u128 * 0x0000_0001_0000_0001_0000_0001_0000_0001;
    ((x >> (8 * k)) & keep) | ((x << (32 - 8 * k)) & !keep)
}

fn mix_columns(x: u128) -> u128 {
    let (r1, r2, r3) = (col_rot(x, 1), col_rot(x, 2), col_rot(x, 3));
    xtime(x) ^ xtime(r1) ^ r1 ^ r2 ^ r3
}

fn expand_key(key: &[u8; 16]) -> [[u8; 16]; 11] {
    let mut w = [[0u8; 4]; 44];
    for i in 0..4 {
        w[i].copy_from_slice(&key[4 * i..4 * i + 4]);
    }
    let mut rcon = 1u8;
    for i in 4..44 {
        let mut t = w[i - 1];
        if i % 4 == 0 {
            // i is public. RotWord, SubWord, then Rcon.
            t.rotate_left(1);
            let s = sub_bytes(u32::from_le_bytes(t) as u128).to_le_bytes();
            t = [s[0] ^ rcon, s[1], s[2], s[3]];
            rcon = (xtime(rcon as u128) & 0xff) as u8;
        }
        for j in 0..4 {
            w[i][j] = w[i - 4][j] ^ t[j];
        }
    }
    let mut rk = [[0u8; 16]; 11];
    for (r, k) in rk.iter_mut().enumerate() {
        for c in 0..4 {
            k[4 * c..4 * c + 4].copy_from_slice(&w[4 * r + c]);
        }
    }
    rk
}

/// One block; the block is the 16 bytes read as a little-endian `u128`.
fn encrypt_block(rk: &[[u8; 16]; 11], block: u128) -> u128 {
    let mut s = block ^ u128::from_le_bytes(rk[0]);
    for k in &rk[1..10] {
        s = mix_columns(shift_rows(sub_bytes(s))) ^ u128::from_le_bytes(*k);
    }
    shift_rows(sub_bytes(s)) ^ u128::from_le_bytes(rk[10])
}

fn ctr_portable(rk: &[[u8; 16]; 11], nonce: &[u8; 12], mut ctr: u32, buf: &mut [u8]) {
    for chunk in buf.chunks_mut(16) {
        let mut cb = [0u8; 16];
        cb[..12].copy_from_slice(nonce);
        cb[12..].copy_from_slice(&ctr.to_be_bytes());
        let ks = encrypt_block(rk, u128::from_le_bytes(cb)).to_le_bytes();
        for (b, k) in chunk.iter_mut().zip(ks) {
            *b ^= k;
        }
        ctr = ctr.wrapping_add(1);
    }
}

// -------------------------------------------------------------- portable GHASH

/// Product in GF(2^128) with the GCM bit order (SP 800-38D algorithm 1).
/// Block bit 0 is the most significant bit of the big-endian `u128`.
fn gf_mul(x: u128, h: u128) -> u128 {
    const R: u128 = 0xe1 << 120;
    let (mut z, mut v) = (0u128, h);
    for i in 0..128 {
        z ^= v & ((x >> (127 - i)) & 1).wrapping_neg();
        v = (v >> 1) ^ (R & (v & 1).wrapping_neg());
    }
    z
}

fn padded_be(chunk: &[u8]) -> u128 {
    let mut b = [0u8; 16];
    b[..chunk.len()].copy_from_slice(chunk);
    u128::from_be_bytes(b)
}

fn ghash_portable(h: u128, aad: &[u8], ct: &[u8]) -> [u8; 16] {
    let mut y = 0u128;
    for part in [aad, ct] {
        for chunk in part.chunks(16) {
            y = gf_mul(y ^ padded_be(chunk), h);
        }
    }
    let lens = ((aad.len() as u128 * 8) << 64) | (ct.len() as u128 * 8);
    gf_mul(y ^ lens, h).to_be_bytes()
}

// ------------------------------------------------------------------- x86_64 HW

#[cfg(target_arch = "x86_64")]
mod hw {
    use std::arch::x86_64::*;

    pub fn available() -> bool {
        is_x86_feature_detected!("aes")
            && is_x86_feature_detected!("pclmulqdq")
            && is_x86_feature_detected!("ssse3")
            && is_x86_feature_detected!("sse4.1")
    }

    /// 8 blocks at once hide the latency of AESENC.
    ///
    /// # Safety
    /// The CPU must have the features in `available`.
    #[target_feature(enable = "aes,ssse3,sse4.1")]
    pub unsafe fn ctr_xor(rk: &[[u8; 16]; 11], nonce: &[u8; 12], ctr: u32, buf: &mut [u8]) {
        let mut k = [_mm_setzero_si128(); 11];
        for i in 0..11 {
            // SAFETY: `rk[i]` is 16 readable bytes; loadu has no alignment need.
            k[i] = unsafe { _mm_loadu_si128(rk[i].as_ptr().cast()) };
        }
        let mut nb = [0u8; 16];
        nb[..12].copy_from_slice(nonce);
        // SAFETY: `nb` is 16 bytes.
        let base = unsafe { _mm_loadu_si128(nb.as_ptr().cast()) };
        let mut ctr = ctr;
        let counter_block = |c: u32| _mm_insert_epi32::<3>(base, c.swap_bytes() as i32);

        let mut chunks = buf.chunks_exact_mut(128);
        for c in &mut chunks {
            let mut s = [_mm_setzero_si128(); 8];
            for j in 0..8 {
                s[j] = _mm_xor_si128(counter_block(ctr.wrapping_add(j as u32)), k[0]);
            }
            for rkey in &k[1..10] {
                for b in s.iter_mut() {
                    *b = _mm_aesenc_si128(*b, *rkey);
                }
            }
            for j in 0..8 {
                let ks = _mm_aesenclast_si128(s[j], k[10]);
                let p = c[16 * j..].as_mut_ptr().cast::<__m128i>();
                // SAFETY: `c` has 128 bytes, so block j is in range.
                unsafe { _mm_storeu_si128(p, _mm_xor_si128(ks, _mm_loadu_si128(p))) };
            }
            ctr = ctr.wrapping_add(8);
        }
        for c in chunks.into_remainder().chunks_mut(16) {
            let mut s = _mm_xor_si128(counter_block(ctr), k[0]);
            for rkey in &k[1..10] {
                s = _mm_aesenc_si128(s, *rkey);
            }
            s = _mm_aesenclast_si128(s, k[10]);
            let mut ks = [0u8; 16];
            // SAFETY: `ks` is 16 bytes.
            unsafe { _mm_storeu_si128(ks.as_mut_ptr().cast(), s) };
            for (b, k) in c.iter_mut().zip(ks) {
                *b ^= k;
            }
            ctr = ctr.wrapping_add(1);
        }
    }

    // GHASH uses byte-reversed blocks: the register then holds the big-endian
    // value, with the x^0 coefficient in bit 127. A carry-less product of two such
    // values is the true product shifted right by one bit, so `reduce` first shifts
    // the 256-bit product left by 1 and then reduces by x^128+x^7+x^2+x+1.

    #[target_feature(enable = "ssse3")]
    fn bswap(x: __m128i) -> __m128i {
        _mm_shuffle_epi8(x, _mm_set_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15))
    }

    /// Unreduced 256-bit carry-less product as (low, high).
    #[target_feature(enable = "pclmulqdq,sse2")]
    fn clmul(a: __m128i, b: __m128i) -> (__m128i, __m128i) {
        let lo = _mm_clmulepi64_si128::<0x00>(a, b);
        let hi = _mm_clmulepi64_si128::<0x11>(a, b);
        let mid = _mm_xor_si128(_mm_clmulepi64_si128::<0x10>(a, b), _mm_clmulepi64_si128::<0x01>(a, b));
        (_mm_xor_si128(lo, _mm_slli_si128::<8>(mid)), _mm_xor_si128(hi, _mm_srli_si128::<8>(mid)))
    }

    #[target_feature(enable = "sse2")]
    fn reduce(lo: __m128i, hi: __m128i) -> __m128i {
        // Shift the 256-bit value left by one bit.
        let (cl, ch) = (_mm_srli_epi32::<31>(lo), _mm_srli_epi32::<31>(hi));
        let carry_into_hi = _mm_srli_si128::<12>(cl);
        let lo = _mm_or_si128(_mm_slli_epi32::<1>(lo), _mm_slli_si128::<4>(cl));
        let hi = _mm_or_si128(
            _mm_or_si128(_mm_slli_epi32::<1>(hi), _mm_slli_si128::<4>(ch)),
            carry_into_hi,
        );
        // Fold the low half into the high half.
        let t = _mm_xor_si128(
            _mm_xor_si128(_mm_slli_epi32::<31>(lo), _mm_slli_epi32::<30>(lo)),
            _mm_slli_epi32::<25>(lo),
        );
        let t_hi = _mm_srli_si128::<4>(t);
        let lo = _mm_xor_si128(lo, _mm_slli_si128::<12>(t));
        let f = _mm_xor_si128(
            _mm_xor_si128(_mm_srli_epi32::<1>(lo), _mm_srli_epi32::<2>(lo)),
            _mm_xor_si128(_mm_srli_epi32::<7>(lo), t_hi),
        );
        _mm_xor_si128(hi, _mm_xor_si128(lo, f))
    }

    /// H^1..H^8 for the 8-block aggregated GHASH. `h` is the GHASH key in block order.
    ///
    /// # Safety
    /// The CPU must have the features in `available`.
    #[target_feature(enable = "pclmulqdq,ssse3,sse2")]
    pub unsafe fn powers(h: &[u8; 16]) -> [[u8; 16]; 8] {
        // SAFETY: `h` is 16 bytes.
        let h1 = bswap(unsafe { _mm_loadu_si128(h.as_ptr().cast()) });
        let mut p = [h1; 8];
        for i in 1..8 {
            let (lo, hi) = clmul(p[i - 1], h1);
            p[i] = reduce(lo, hi);
        }
        let mut out = [[0u8; 16]; 8];
        for i in 0..8 {
            // SAFETY: `out[i]` is 16 bytes.
            unsafe { _mm_storeu_si128(out[i].as_mut_ptr().cast(), p[i]) };
        }
        out
    }

    /// Adds the product x*h to the Karatsuba sums: lo = a0*b0, hi = a1*b1,
    /// mid = (a0^a1)*(b0^b1). The cross term is mid^lo^hi, fixed once at the end.
    #[target_feature(enable = "pclmulqdq,sse2")]
    fn mul_acc(x: __m128i, h: __m128i, acc: &mut [__m128i; 3]) {
        let xs = _mm_xor_si128(x, _mm_srli_si128::<8>(x));
        let hs = _mm_xor_si128(h, _mm_srli_si128::<8>(h));
        acc[0] = _mm_xor_si128(acc[0], _mm_clmulepi64_si128::<0x00>(x, h));
        acc[1] = _mm_xor_si128(acc[1], _mm_clmulepi64_si128::<0x11>(x, h));
        acc[2] = _mm_xor_si128(acc[2], _mm_clmulepi64_si128::<0x00>(xs, hs));
    }

    #[target_feature(enable = "pclmulqdq,sse2")]
    fn finish(acc: [__m128i; 3]) -> __m128i {
        let mid = _mm_xor_si128(acc[2], _mm_xor_si128(acc[0], acc[1]));
        reduce(
            _mm_xor_si128(acc[0], _mm_slli_si128::<8>(mid)),
            _mm_xor_si128(acc[1], _mm_srli_si128::<8>(mid)),
        )
    }

    #[target_feature(enable = "pclmulqdq,ssse3,sse2")]
    fn absorb(hp: &[__m128i; 8], mut y: __m128i, data: &[u8]) -> __m128i {
        let mut groups = data.chunks_exact(128);
        for q in &mut groups {
            let mut acc = [_mm_setzero_si128(); 3];
            for j in 0..8 {
                // SAFETY: `q` has 128 bytes.
                let mut x = bswap(unsafe { _mm_loadu_si128(q[16 * j..].as_ptr().cast()) });
                if j == 0 {
                    x = _mm_xor_si128(x, y);
                }
                mul_acc(x, hp[7 - j], &mut acc);
            }
            y = finish(acc);
        }
        // Up to 7 full blocks and one partial block: one reduction for the whole tail.
        let tail = groups.remainder();
        if !tail.is_empty() {
            let n = tail.len().div_ceil(16);
            let mut acc = [_mm_setzero_si128(); 3];
            for (j, chunk) in tail.chunks(16).enumerate() {
                let mut b = [0u8; 16];
                b[..chunk.len()].copy_from_slice(chunk);
                // SAFETY: `b` is 16 bytes.
                let mut x = bswap(unsafe { _mm_loadu_si128(b.as_ptr().cast()) });
                if j == 0 {
                    x = _mm_xor_si128(x, y);
                }
                mul_acc(x, hp[n - 1 - j], &mut acc);
            }
            y = finish(acc);
        }
        y
    }

    /// # Safety
    /// The CPU must have the features in `available`.
    #[target_feature(enable = "pclmulqdq,ssse3,sse2")]
    pub unsafe fn ghash(hpow: &[[u8; 16]; 8], aad: &[u8], ct: &[u8]) -> [u8; 16] {
        let mut hp = [_mm_setzero_si128(); 8];
        for i in 0..8 {
            // SAFETY: `hpow[i]` is 16 bytes.
            hp[i] = unsafe { _mm_loadu_si128(hpow[i].as_ptr().cast()) };
        }
        let mut y = absorb(&hp, _mm_setzero_si128(), aad);
        y = absorb(&hp, y, ct);
        let lens = ((aad.len() as u128 * 8) << 64) | (ct.len() as u128 * 8);
        y = absorb(&hp, y, &lens.to_be_bytes());
        let mut out = [0u8; 16];
        // SAFETY: `out` is 16 bytes.
        unsafe { _mm_storeu_si128(out.as_mut_ptr().cast(), bswap(y)) };
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }
    fn arr<const N: usize>(s: &str) -> [u8; N] {
        unhex(s).try_into().unwrap()
    }

    /// Backends to test: portable always, accelerated when this CPU has it.
    fn backends(key: &[u8; 16]) -> Vec<Aes128Gcm> {
        let mut v = vec![Aes128Gcm::new_portable(key)];
        let a = Aes128Gcm::new(key);
        if a.is_accelerated() {
            v.push(a);
        }
        v
    }

    // NIST CAVP "GCM Test Vectors" (gcmtestvectors.zip), keysize 128, IVlen 96, Taglen 128:
    // https://csrc.nist.gov/CSRC/media/Projects/Cryptographic-Algorithm-Validation-Program/documents/mac/gcmtestvectors.zip
    // zip sha256 f9fc479e134cde2980b3bb7cddbcb567b2cd96fd753835243ed067699f26a023
    // gcmEncryptExtIV128.rsp sha256 30156236f768b0d81e58a3e5bd3bd79ce294cf9ef0ba0a84cf7b869c89ebd9fd
    // gcmDecrypt128.rsp      sha256 0d1f98d0c9e176ede1f3e85484dd13de92c3436ca97df378ec3481e04c19a732
    // Fields: (valid, Key, IV, PT, AAD, CT, Tag). For the FAIL cases PT is empty and must not be produced.
    // Each case is the first one in its file for that [PTlen][AADlen] group.
    const CAVP: &[(bool, &str, &str, &str, &str, &str, &str)] = &[
    // gcmEncryptExtIV128.rsp [PTlen=0][AADlen=0] Count=0
    (true, "11754cd72aec309bf52f7687212e8957", "3c819d9a9bed087615030b65", "", "", "", "250327c674aaf477aef2675748cf6971"),
    // gcmEncryptExtIV128.rsp [PTlen=0][AADlen=128] Count=0
    (true, "77be63708971c4e240d1cb79e8d77feb", "e0e00f19fed7ba0136a797f3", "", "7a43ec1d9c0a5a78a0b16533a6213cab", "", "209fcc8d3675ed938e9c7166709dd946"),
    // gcmEncryptExtIV128.rsp [PTlen=0][AADlen=160] Count=0
    (true, "2fb45e5b8f993a2bfebc4b15b533e0b4", "5b05755f984d2b90f94b8027", "", "e85491b2202caf1d7dce03b97e09331c32473941", "", "c75b7832b2a2d9bd827412b6ef5769db"),
    // gcmEncryptExtIV128.rsp [PTlen=128][AADlen=0] Count=0
    (true, "7fddb57453c241d03efbed3ac44e371c", "ee283a3fc75575e33efd4887", "d5de42b461646c255c87bd2962d3b9a2", "", "2ccda4a5415cb91e135c2a0f78c9b2fd", "b36d1df9b9d5e596f83e8b7f52971cb3"),
    // gcmEncryptExtIV128.rsp [PTlen=128][AADlen=128] Count=0
    (true, "c939cc13397c1d37de6ae0e1cb7c423c", "b3d8cc017cbb89b39e0f67e2", "c3b3c41f113a31b73d9a5cd432103069", "24825602bd12a984e0092d3e448eda5f", "93fe7d9e9bfd10348a5606e5cafa7354", "0032a1dc85f1c9786925a2e71d8272dd"),
    // gcmEncryptExtIV128.rsp [PTlen=256][AADlen=0] Count=0
    (true, "9971071059abc009e4f2bd69869db338", "07a9a95ea3821e9c13c63251", "f54bc3501fed4f6f6dfb5ea80106df0bd836e6826225b75c0222f6e859b35983", "", "0556c159f84ef36cb1602b4526b12009c775611bffb64dc0d9ca9297cd2c6a01", "7870d9117f54811a346970f1de090c41"),
    // gcmEncryptExtIV128.rsp [PTlen=408][AADlen=160] Count=0
    (true, "fe47fcce5fc32665d2ae399e4eec72ba", "5adb9609dbaeb58cbd6e7275", "7c0e88c88899a779228465074797cd4c2e1498d259b54390b85e3eef1c02df60e743f1b840382c4bccaf3bafb4ca8429bea063", "88319d6e1d3ffa5f987199166c8a9b56c2aeba5a", "98f4826f05a265e6dd2be82db241c0fbbbf9ffb1c173aa83964b7cf5393043736365253ddbc5db8778371495da76d269e5db3e", "291ef1982e4defedaa2249f898556b47"),
    // gcmEncryptExtIV128.rsp [PTlen=104][AADlen=0] Count=0
    (true, "fe9bb47deb3a61e423c2231841cfd1fb", "4d328eb776f500a2f7fb47aa", "f1cc3818e421876bb6b8bbd6c9", "", "b88c5c1977b35b517b0aeae967", "43fd4727fe5cdb4b5b42818dea7ef8c9"),
    // gcmEncryptExtIV128.rsp [PTlen=408][AADlen=384] Count=0
    (true, "3c50622868f450aa0928990c15e1eb36", "811d5290768d57e7d87bb6c7", "edd0a8f82833e919740fe2bf9edecf4ac86c72dc89490cef7b6983aaaf99fc856c5cc87d63f98a7c861bf3271fea6da86a15ab", "dae2c7e0a3d3fd2bc04eca19b15178a003b5cf84890c28c2a615f20f8adb427f70698c12b2ef87780c1193fbb8cd1674", "a51425b0608d3b4b46d4ec05ca1ddaf02bdd2089ae0554ecfb2a1c84c63d82dc71ddb9ab1b1f0b49de2ad27c2b5173e7000aa6", "bd9b5efca48008cd973a4f7d2c723844"),
    // gcmEncryptExtIV128.rsp [PTlen=256][AADlen=384] Count=0
    (true, "48b7f337cdf9252687ecc760bd8ec184", "3e894ebb16ce82a53c3e05b2", "bb2bac67a4709430c39c2eb9acfabc0d456c80d30aa1734e57997d548a8f0603", "7d924cfd37b3d046a96eb5e132042405c8731e06509787bbeb41f258275746495e884d69871f77634c584bb007312234", "d263228b8ce051f67e9baf1ce7df97d10cd5f3bc972362055130c7d13c3ab2e7", "71446737ca1fa92e6d026d7d2ed1aa9c"),
    // gcmDecrypt128.rsp (FAIL) [PTlen=0][AADlen=0] Count=1
    (false, "a49a5e26a2f8cb63d05546c2a62f5343", "907763b19b9b4ab6bd4f0281", "", "", "", "a2be08210d8c470a8df6e8fbd79ec5cf"),
    // gcmDecrypt128.rsp (FAIL) [PTlen=0][AADlen=0] Count=3
    (false, "d8cd400a0a73d114cd3ecf36537cab3d", "3c162c9f16a49b8fe6c92a81", "", "", "", "4203aec165f9d397cf9009770a088c16"),
    // gcmDecrypt128.rsp (FAIL) [PTlen=0][AADlen=0] Count=6
    (false, "c91aab7ebe13653a71a4232fd1beb793", "7799464b6de6383da0daec52", "", "", "", "00c4f7033f3c05e9d531f3ca573dc98d"),
    // gcmDecrypt128.rsp (FAIL) [PTlen=0][AADlen=0] Count=8
    (false, "1bd49e553457459aee1b5d83e7c216a2", "2b37cf40ed2685eb2a907cd0", "", "", "", "fcb41d17fdb023d4d14f84a387d3ad77"),
    ];

    #[test]
    fn fips197_block_vectors() {
        // FIPS 197 appendix B and C.1.
        let cases = [
            ("2b7e151628aed2a6abf7158809cf4f3c", "3243f6a8885a308d313198a2e0370734", "3925841d02dc09fbdc118597196a0b32"),
            ("000102030405060708090a0b0c0d0e0f", "00112233445566778899aabbccddeeff", "69c4e0d86a7b0430d8cdb78070b4c55a"),
        ];
        for (k, p, c) in cases {
            let rk = expand_key(&arr(k));
            let out = encrypt_block(&rk, u128::from_le_bytes(arr(p))).to_le_bytes();
            assert_eq!(out.to_vec(), unhex(c));
        }
        // FIPS 197 appendix A.1 last round key of key 2b7e...
        let rk = expand_key(&arr("2b7e151628aed2a6abf7158809cf4f3c"));
        assert_eq!(rk[10].to_vec(), unhex("d014f9a8c9ee2589e13f0cc8b6630ca6"));
    }

    #[test]
    fn sbox_matches_fips197_table_samples() {
        // FIPS 197 figure 7: S(00)=63, S(01)=7c, S(53)=ed, S(ff)=16.
        for (i, o) in [(0x00u8, 0x63u8), (0x01, 0x7c), (0x53, 0xed), (0xff, 0x16)] {
            assert_eq!(sub_bytes(i as u128).to_le_bytes()[0], o);
        }
    }

    #[test]
    fn sbox_all_256_values_match_definition() {
        // Scalar, table-free, branchy reference from FIPS 197 5.1.1 (test only):
        // multiplicative inverse by search, then the affine transform.
        fn mul(mut a: u8, mut b: u8) -> u8 {
            let mut r = 0;
            while b != 0 {
                if b & 1 != 0 {
                    r ^= a;
                }
                a = (a << 1) ^ if a & 0x80 != 0 { 0x1b } else { 0 };
                b >>= 1;
            }
            r
        }
        let want = |x: u8| {
            let inv = (0..=255u8).find(|&y| mul(x, y) == 1).unwrap_or(0);
            inv ^ inv.rotate_left(1) ^ inv.rotate_left(2) ^ inv.rotate_left(3) ^ inv.rotate_left(4) ^ 0x63
        };
        // All 16 lanes at once: lane j holds value 16*i + j.
        for i in 0..16u8 {
            let mut lanes = [0u8; 16];
            for (j, l) in lanes.iter_mut().enumerate() {
                *l = 16 * i + j as u8;
            }
            let out = sub_bytes(u128::from_le_bytes(lanes)).to_le_bytes();
            for j in 0..16 {
                assert_eq!(out[j], want(lanes[j]), "S({:#04x})", lanes[j]);
            }
        }
    }

    #[test]
    fn cavp_vectors() {
        assert!(CAVP.len() >= 12);
        for (i, &(valid, k, iv, pt, aad, ct, tag)) in CAVP.iter().enumerate() {
            let (key, nonce, tag) = (arr::<16>(k), arr::<12>(iv), arr::<16>(tag));
            let (aad, ct) = (unhex(aad), unhex(ct));
            for g in backends(&key) {
                let mut buf = ct.clone();
                let r = g.open_in_place_detached(&nonce, &aad, &mut buf, &tag);
                if valid {
                    assert_eq!(r, Ok(()), "open case {i} accel={}", g.is_accelerated());
                    assert_eq!(buf, unhex(pt), "pt case {i}");
                    let mut buf = unhex(pt);
                    let t = g.seal_in_place_detached(&nonce, &aad, &mut buf).unwrap();
                    assert_eq!((buf, t), (ct.clone(), tag), "seal case {i} accel={}", g.is_accelerated());
                } else {
                    assert_eq!(r, Err(Error::AuthFailed), "fail case {i}");
                    assert_eq!(buf, ct, "buffer changed on failure, case {i}");
                }
            }
        }
    }

    /// Small deterministic generator for test data only.
    struct Rng(u64);
    impl Rng {
        fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n)
                .map(|_| {
                    self.0 ^= self.0 << 13;
                    self.0 ^= self.0 >> 7;
                    self.0 ^= self.0 << 17;
                    (self.0 >> 24) as u8
                })
                .collect()
        }
    }

    #[test]
    fn backends_agree_and_roundtrip_all_small_lengths() {
        let mut rng = Rng(0x9e3779b97f4a7c15);
        let key: [u8; 16] = rng.bytes(16).try_into().unwrap();
        let bs = backends(&key);
        let lens = (0..=70).chain([127, 128, 129, 255, 256, 257, 1000, 16384, 16640]);
        for pl in lens {
            for al in [0usize, 1, 5, 15, 16, 17, 63, 64, 65, 100] {
                let nonce: [u8; 12] = rng.bytes(12).try_into().unwrap();
                let (pt, aad) = (rng.bytes(pl), rng.bytes(al));
                let mut outs = vec![];
                for g in &bs {
                    let mut b = pt.clone();
                    let t = g.seal_in_place_detached(&nonce, &aad, &mut b).unwrap();
                    let mut d = b.clone();
                    g.open_in_place_detached(&nonce, &aad, &mut d, &t).unwrap();
                    assert_eq!(d, pt);
                    outs.push((b, t));
                }
                assert!(outs.windows(2).all(|w| w[0] == w[1]), "pl={pl} al={al}");
            }
        }
    }

    #[test]
    fn any_change_fails_and_leaves_buffer() {
        let mut rng = Rng(12345);
        let key: [u8; 16] = rng.bytes(16).try_into().unwrap();
        let nonce: [u8; 12] = rng.bytes(12).try_into().unwrap();
        for g in backends(&key) {
            for (pl, al) in [(0usize, 0usize), (0, 20), (1, 0), (16, 5), (33, 17), (200, 3)] {
                let (pt, aad) = (rng.bytes(pl), rng.bytes(al));
                let mut ct = pt.clone();
                let tag = g.seal_in_place_detached(&nonce, &aad, &mut ct).unwrap();
                let try_open = |n: &[u8; 12], a: &[u8], c: &[u8], t: &[u8; 16]| {
                    let mut b = c.to_vec();
                    let r = g.open_in_place_detached(n, a, &mut b, t);
                    assert_eq!(r, Err(Error::AuthFailed));
                    assert_eq!(b, c, "buffer must stay unchanged");
                };
                for bit in 0..128 {
                    let mut t = tag;
                    t[bit / 8] ^= 1 << (bit % 8);
                    try_open(&nonce, &aad, &ct, &t);
                }
                for bit in 0..96 {
                    let mut n = nonce;
                    n[bit / 8] ^= 1 << (bit % 8);
                    try_open(&n, &aad, &ct, &tag);
                }
                for i in 0..ct.len() {
                    let mut c = ct.clone();
                    c[i] ^= 0x80;
                    try_open(&nonce, &aad, &c, &tag);
                }
                for i in 0..aad.len() {
                    let mut a = aad.clone();
                    a[i] ^= 1;
                    try_open(&nonce, &a, &ct, &tag);
                }
                // Moving a byte between AAD and ciphertext, or truncating, must fail.
                if !ct.is_empty() {
                    try_open(&nonce, &aad, &ct[..ct.len() - 1], &tag);
                }
                let mut longer = aad.clone();
                longer.push(0);
                try_open(&nonce, &longer, &ct, &tag);
            }
        }
    }

    #[test]
    fn length_limits() {
        assert_eq!(check_lens(0, MAX_DATA_LEN), Ok(()));
        assert_eq!(check_lens(0, MAX_DATA_LEN + 1), Err(Error::TooLong));
        assert_eq!(check_lens(MAX_AAD_LEN, 0), Ok(()));
        assert_eq!(check_lens(MAX_AAD_LEN + 1, 0), Err(Error::TooLong));
        assert_eq!(check_lens(u64::MAX, u64::MAX), Err(Error::TooLong));
        // The last block of a maximum-size message uses counter 2^32-1: no wrap.
        assert_eq!((MAX_DATA_LEN / 16) + 1, (1 << 32) - 1);
    }

    #[test]
    fn counter_near_top_agrees_between_backends() {
        // Start 2^32-12: 11 blocks (176 bytes) end exactly at counter 2^32-1.
        let key = [7u8; 16];
        let nonce = [9u8; 12];
        let bs = backends(&key);
        let mut outs = vec![];
        for g in &bs {
            let mut b = vec![0u8; 176];
            g.ctr(&nonce, u32::MAX - 10, &mut b);
            outs.push(b);
        }
        assert!(outs.windows(2).all(|w| w[0] == w[1]));
    }

    #[test]
    fn tls13_nonce_xors_sequence_into_low_8_bytes() {
        let iv: [u8; 12] = arr("000102030405060708090a0b");
        assert_eq!(tls13_nonce(&iv, 0), iv);
        let n = tls13_nonce(&iv, 0x0102_0304_0506_0708);
        assert_eq!(n[..4], iv[..4]);
        let want: Vec<u8> = (0..8).map(|i| iv[4 + i] ^ (i as u8 + 1)).collect();
        assert_eq!(n[4..].to_vec(), want);
        assert_ne!(tls13_nonce(&iv, 1), tls13_nonce(&iv, 2));
    }
}
