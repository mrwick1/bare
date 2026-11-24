//! AES with 128, 192 and 256-bit keys, both directions, and the GCM and CBC modes that Web Crypto uses (`crypto.subtle`).
//! Learning code, NOT audited. TLS uses `aes_gcm.rs`, not this file.
//!
//! Sources:
//! - FIPS 197 (AES): <https://csrc.nist.gov/pubs/fips/197/final>
//! - NIST SP 800-38D (GCM), SP 800-38A (CBC), RFC 5652 section 6.3 (PKCS #7 padding).
//!
//! The cipher uses S-box tables, so it is not constant-time. It is for page data, not for TLS.
//! GCM takes a 96-bit IV only (the IV that the spec recommends).

const SBOX: [u8; 256] = sbox();
const INV_SBOX: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        t[SBOX[i] as usize] = i as u8;
        i += 1;
    }
    t
};

const fn xt(a: u8) -> u8 {
    (a << 1) ^ if a & 0x80 != 0 { 0x1b } else { 0 }
}

const fn mul(mut a: u8, mut b: u8) -> u8 {
    let mut r = 0;
    while b != 0 {
        if b & 1 != 0 {
            r ^= a;
        }
        a = xt(a);
        b >>= 1;
    }
    r
}

/// The S-box of FIPS 197 section 5.1.1: the inverse in GF(2^8), then the affine map.
const fn sbox() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        // x^254 is the inverse (0 maps to 0).
        let x = i as u8;
        let mut inv = 1u8;
        let mut k = 0;
        while k < 254 {
            inv = mul(inv, x);
            k += 1;
        }
        if x == 0 {
            inv = 0;
        }
        let b = inv;
        t[i] = b ^ b.rotate_left(1) ^ b.rotate_left(2) ^ b.rotate_left(3) ^ b.rotate_left(4) ^ 0x63;
        i += 1;
    }
    t
}

/// An expanded AES key. The round keys are zeroed on drop.
pub struct Aes {
    rk: [[u8; 16]; 15],
    rounds: usize,
}

impl Drop for Aes {
    fn drop(&mut self) {
        // SAFETY: a valid, aligned, exclusive reference to a field.
        unsafe { std::ptr::write_volatile(&mut self.rk, [[0; 16]; 15]) };
    }
}

impl Aes {
    /// `None` unless the key is 16, 24 or 32 bytes.
    pub fn new(key: &[u8]) -> Option<Aes> {
        let nk = key.len() / 4;
        if !matches!(key.len(), 16 | 24 | 32) {
            return None;
        }
        let rounds = nk + 6;
        let mut w = vec![[0u8; 4]; 4 * (rounds + 1)];
        for (i, c) in key.chunks(4).enumerate() {
            w[i].copy_from_slice(c);
        }
        let mut rcon = 1u8;
        for i in nk..w.len() {
            let mut t = w[i - 1];
            if i % nk == 0 {
                t = [SBOX[t[1] as usize] ^ rcon, SBOX[t[2] as usize], SBOX[t[3] as usize], SBOX[t[0] as usize]];
                rcon = xt(rcon);
            } else if nk > 6 && i % nk == 4 {
                t = t.map(|b| SBOX[b as usize]);
            }
            for j in 0..4 {
                w[i][j] = w[i - nk][j] ^ t[j];
            }
        }
        let mut rk = [[0u8; 16]; 15];
        for (r, k) in rk.iter_mut().take(rounds + 1).enumerate() {
            for c in 0..4 {
                k[4 * c..4 * c + 4].copy_from_slice(&w[4 * r + c]);
            }
        }
        w.fill([0; 4]);
        Some(Aes { rk, rounds })
    }

    pub fn encrypt_block(&self, b: &mut [u8; 16]) {
        xor(b, &self.rk[0]);
        for r in 1..=self.rounds {
            for x in b.iter_mut() {
                *x = SBOX[*x as usize];
            }
            shift_rows(b, false);
            if r != self.rounds {
                for c in b.chunks_mut(4) {
                    let [a0, a1, a2, a3] = [c[0], c[1], c[2], c[3]];
                    c[0] = mul(a0, 2) ^ mul(a1, 3) ^ a2 ^ a3;
                    c[1] = a0 ^ mul(a1, 2) ^ mul(a2, 3) ^ a3;
                    c[2] = a0 ^ a1 ^ mul(a2, 2) ^ mul(a3, 3);
                    c[3] = mul(a0, 3) ^ a1 ^ a2 ^ mul(a3, 2);
                }
            }
            xor(b, &self.rk[r]);
        }
    }

    pub fn decrypt_block(&self, b: &mut [u8; 16]) {
        xor(b, &self.rk[self.rounds]);
        for r in (0..self.rounds).rev() {
            shift_rows(b, true);
            for x in b.iter_mut() {
                *x = INV_SBOX[*x as usize];
            }
            xor(b, &self.rk[r]);
            if r != 0 {
                for c in b.chunks_mut(4) {
                    let [a0, a1, a2, a3] = [c[0], c[1], c[2], c[3]];
                    c[0] = mul(a0, 14) ^ mul(a1, 11) ^ mul(a2, 13) ^ mul(a3, 9);
                    c[1] = mul(a0, 9) ^ mul(a1, 14) ^ mul(a2, 11) ^ mul(a3, 13);
                    c[2] = mul(a0, 13) ^ mul(a1, 9) ^ mul(a2, 14) ^ mul(a3, 11);
                    c[3] = mul(a0, 11) ^ mul(a1, 13) ^ mul(a2, 9) ^ mul(a3, 14);
                }
            }
        }
    }
}

fn xor(b: &mut [u8; 16], k: &[u8; 16]) {
    for (x, y) in b.iter_mut().zip(k) {
        *x ^= y;
    }
}

/// The state is column-major: byte `r + 4c` is row `r`, column `c`. Row `r` turns left by `r` (right when `inv`).
fn shift_rows(b: &mut [u8; 16], inv: bool) {
    let s = *b;
    for r in 1..4 {
        for c in 0..4 {
            let from = if inv { (c + 4 - r) % 4 } else { (c + r) % 4 };
            b[r + 4 * c] = s[r + 4 * from];
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Error {
    /// A bad key, IV or tag length, or bad padding.
    Bad,
    /// The GCM tag does not match.
    AuthFailed,
}

/// GF(2^128) multiply of SP 800-38D section 6.3 (bit 0 is the high bit of byte 0).
fn gf_mul(x: u128, y: u128) -> u128 {
    let (mut z, mut v) = (0u128, y);
    for i in 0..128 {
        if (x >> (127 - i)) & 1 != 0 {
            z ^= v;
        }
        v = (v >> 1) ^ if v & 1 != 0 { 0xe1 << 120 } else { 0 };
    }
    z
}

fn ghash(h: u128, aad: &[u8], ct: &[u8]) -> u128 {
    let mut y = 0u128;
    for part in [aad, ct] {
        for c in part.chunks(16) {
            let mut b = [0u8; 16];
            b[..c.len()].copy_from_slice(c);
            y = gf_mul(y ^ u128::from_be_bytes(b), h);
        }
    }
    let lens = ((aad.len() as u128 * 8) << 64) | (ct.len() as u128 * 8);
    gf_mul(y ^ lens, h)
}

/// CTR of GCM: the counter is the low 32 bits of the block, starting at `j0 + 1`.
fn gctr(aes: &Aes, j0: [u8; 16], buf: &mut [u8]) {
    let mut ctr = j0;
    for c in buf.chunks_mut(16) {
        let n = u32::from_be_bytes([ctr[12], ctr[13], ctr[14], ctr[15]]).wrapping_add(1);
        ctr[12..].copy_from_slice(&n.to_be_bytes());
        let mut k = ctr;
        aes.encrypt_block(&mut k);
        for (x, y) in c.iter_mut().zip(k) {
            *x ^= y;
        }
    }
}

fn gcm_parts(key: &[u8], iv: &[u8], tag_len: usize) -> Result<(Aes, u128, [u8; 16]), Error> {
    if iv.len() != 12 || !(4..=16).contains(&tag_len) {
        return Err(Error::Bad);
    }
    let aes = Aes::new(key).ok_or(Error::Bad)?;
    let mut h = [0u8; 16];
    aes.encrypt_block(&mut h);
    let mut j0 = [0u8; 16];
    j0[..12].copy_from_slice(iv);
    j0[15] = 1;
    Ok((aes, u128::from_be_bytes(h), j0))
}

fn gcm_tag(aes: &Aes, h: u128, j0: [u8; 16], aad: &[u8], ct: &[u8]) -> [u8; 16] {
    let mut t = j0;
    aes.encrypt_block(&mut t);
    (u128::from_be_bytes(t) ^ ghash(h, aad, ct)).to_be_bytes()
}

/// AES-GCM: the ciphertext, then the first `tag_len` bytes of the tag.
pub fn gcm_seal(key: &[u8], iv: &[u8], aad: &[u8], data: &[u8], tag_len: usize) -> Result<Vec<u8>, Error> {
    let (aes, h, j0) = gcm_parts(key, iv, tag_len)?;
    let mut out = data.to_vec();
    gctr(&aes, j0, &mut out);
    let tag = gcm_tag(&aes, h, j0, aad, &out);
    out.extend_from_slice(&tag[..tag_len]);
    Ok(out)
}

/// AES-GCM open of `gcm_seal` output. No plaintext comes out when the tag does not match.
pub fn gcm_open(key: &[u8], iv: &[u8], aad: &[u8], data: &[u8], tag_len: usize) -> Result<Vec<u8>, Error> {
    let (aes, h, j0) = gcm_parts(key, iv, tag_len)?;
    let Some(n) = data.len().checked_sub(tag_len) else { return Err(Error::AuthFailed) };
    let (ct, tag) = data.split_at(n);
    let want = gcm_tag(&aes, h, j0, aad, ct);
    if want[..tag_len].iter().zip(tag).fold(0u8, |d, (a, b)| d | (a ^ b)) != 0 {
        return Err(Error::AuthFailed);
    }
    let mut out = ct.to_vec();
    gctr(&aes, j0, &mut out);
    Ok(out)
}

/// AES-CBC with PKCS #7 padding.
pub fn cbc_encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>, Error> {
    let aes = Aes::new(key).ok_or(Error::Bad)?;
    let mut prev: [u8; 16] = iv.try_into().map_err(|_| Error::Bad)?;
    let pad = 16 - data.len() % 16;
    let mut out = Vec::with_capacity(data.len() + pad);
    out.extend_from_slice(data);
    out.resize(data.len() + pad, pad as u8);
    for c in out.chunks_mut(16) {
        let mut b: [u8; 16] = c.try_into().expect("16 bytes");
        xor(&mut b, &prev);
        aes.encrypt_block(&mut b);
        c.copy_from_slice(&b);
        prev = b;
    }
    Ok(out)
}

/// AES-CBC decrypt, then remove and check the PKCS #7 padding.
pub fn cbc_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>, Error> {
    let aes = Aes::new(key).ok_or(Error::Bad)?;
    let mut prev: [u8; 16] = iv.try_into().map_err(|_| Error::Bad)?;
    if data.is_empty() || data.len() % 16 != 0 {
        return Err(Error::Bad);
    }
    let mut out = data.to_vec();
    for c in out.chunks_mut(16) {
        let ct: [u8; 16] = c.try_into().expect("16 bytes");
        let mut b = ct;
        aes.decrypt_block(&mut b);
        xor(&mut b, &prev);
        c.copy_from_slice(&b);
        prev = ct;
    }
    let pad = *out.last().expect("not empty") as usize;
    if pad == 0 || pad > 16 || out[out.len() - pad..].iter().any(|&b| b as usize != pad) {
        return Err(Error::Bad);
    }
    out.truncate(out.len() - pad);
    Ok(out)
}

#[cfg(test)]
#[path = "aes_tests.rs"]
mod tests;
