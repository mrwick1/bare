//! RSA signature VERIFICATION with a public key (RFC 8017). No signing, no private keys.
//!
//! - RSASSA-PKCS1-v1_5 (section 8.2, EMSA-PKCS1-v1_5 section 9.2) with SHA-256 or SHA-384.
//!   The encoded message is built from the digest and compared byte for byte with the one
//!   recovered from the signature. A DigestInfo without the NULL parameter, a short
//!   padding string, trailing bytes and any other deviation fail. No BER leniency.
//! - RSASSA-PSS (section 8.1, EMSA-PSS section 9.1.2) with SHA-256 or SHA-384, MGF1 with the
//!   same hash, salt length equal to the hash length and trailer 0xbc. This is what TLS 1.3
//!   `rsa_pss_rsae_sha256` and `rsa_pss_rsae_sha384` mean (RFC 8446 section 4.2.3).
//!
//! Key policy: modulus 2048 to 4096 bits and odd; exponent odd, 3 to 2^32 - 1. The signature
//! must be exactly as long as the modulus and below it.
//!
//! The caller hashes the message and passes the digest. Public values only: not constant time.

use crate::bigint::Modulus;
use crate::der::{self, Reader};
use crate::sha256::sha256;
use crate::sha384::sha384;

pub const MIN_BITS: usize = 2048;
pub const MAX_BITS: usize = crate::bigint::MAX_BITS;
const MAX_LEN: usize = MAX_BITS / 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The key is outside the policy above.
    BadKey,
    BadSignature,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hash {
    Sha256,
    Sha384,
}

impl Hash {
    pub fn len(self) -> usize {
        match self {
            Hash::Sha256 => 32,
            Hash::Sha384 => 48,
        }
    }

    /// Hash `data` into a buffer. The first `len()` bytes are the digest.
    fn digest(self, data: &[u8]) -> [u8; 48] {
        let mut out = [0u8; 48];
        match self {
            Hash::Sha256 => out[..32].copy_from_slice(&sha256(data)),
            Hash::Sha384 => out.copy_from_slice(&sha384(data)),
        }
        out
    }

    /// DER of DigestInfo up to the digest (RFC 8017 section 9.2 note 1).
    fn digest_info(self) -> &'static [u8] {
        match self {
            Hash::Sha256 => &[0x30, 0x31, 0x30, 0x0d, 6, 9, 0x60, 0x86, 0x48, 1, 0x65, 3, 4, 2, 1, 5, 0, 4, 0x20],
            Hash::Sha384 => &[0x30, 0x41, 0x30, 0x0d, 6, 9, 0x60, 0x86, 0x48, 1, 0x65, 3, 4, 2, 2, 5, 0, 4, 0x30],
        }
    }
}

/// An RSA public key that passed the policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicKey<'a> {
    /// Big-endian modulus, no leading zero byte.
    n: &'a [u8],
    e: u32,
}

impl<'a> PublicKey<'a> {
    /// `n` is big-endian (leading zeros are removed). `None` outside the key policy.
    pub fn new(n: &'a [u8], e: &[u8]) -> Option<PublicKey<'a>> {
        let n = &n[n.iter().take_while(|&&b| b == 0).count()..];
        let e = &e[e.iter().take_while(|&&b| b == 0).count()..];
        let bits = (8 * n.len()).checked_sub(n.first()?.leading_zeros() as usize)?;
        if !(MIN_BITS..=MAX_BITS).contains(&bits) || n[n.len() - 1] & 1 == 0 {
            return None;
        }
        if e.len() > 4 || e.last()? & 1 == 0 || e.len() == 1 && e[0] < 3 {
            return None;
        }
        Some(PublicKey { n, e: e.iter().fold(0u32, |a, &b| a << 8 | b as u32) })
    }

    /// RSAPublicKey (RFC 8017 appendix A.1.1): `SEQUENCE { INTEGER n, INTEGER e }`, strict DER.
    pub fn from_pkcs1(der: &'a [u8]) -> Option<PublicKey<'a>> {
        let mut r = Reader::new(der);
        let seq = r.read_tag(der::SEQUENCE).ok()?;
        r.finish().ok()?;
        let mut s = Reader::new(seq.content);
        let n = der::unsigned(s.read_tag(der::INTEGER).ok()?.content).ok()?;
        let e = der::unsigned(s.read_tag(der::INTEGER).ok()?.content).ok()?;
        s.finish().ok()?;
        PublicKey::new(n, e)
    }

    pub fn modulus(&self) -> &'a [u8] {
        self.n
    }

    pub fn exponent(&self) -> u32 {
        self.e
    }

    /// Bit length of the modulus.
    pub fn bits(&self) -> usize {
        8 * self.n.len() - self.n[0].leading_zeros() as usize
    }

    /// `sig^e mod n` into `em[..k]`. `k` (the modulus length in bytes) is returned.
    fn recover(&self, sig: &[u8], em: &mut [u8; MAX_LEN]) -> Result<usize, Error> {
        let k = self.n.len();
        if sig.len() != k {
            return Err(Error::BadSignature);
        }
        let m = Modulus::new(self.n).ok_or(Error::BadKey)?;
        m.pow(sig, self.e, &mut em[..k]).ok_or(Error::BadSignature)?;
        Ok(k)
    }

    /// RSASSA-PKCS1-v1_5-VERIFY. `digest` is the hash of the message.
    pub fn verify_pkcs1(&self, hash: Hash, digest: &[u8], sig: &[u8]) -> Result<(), Error> {
        if digest.len() != hash.len() {
            return Err(Error::BadSignature);
        }
        let mut em = [0u8; MAX_LEN];
        let k = self.recover(sig, &mut em)?;
        let di = hash.digest_info();
        let t = di.len() + digest.len();
        // 00 01 FF..FF 00 DigestInfo digest, with at least 8 bytes of FF. k >= 256 > t + 11.
        let ps = k - t - 3;
        let ok = em[0] == 0
            && em[1] == 1
            && em[2..2 + ps].iter().all(|&b| b == 0xff)
            && em[2 + ps] == 0
            && em[3 + ps..3 + ps + di.len()] == *di
            && em[3 + ps + di.len()..k] == *digest;
        if ok { Ok(()) } else { Err(Error::BadSignature) }
    }

    /// RSASSA-PSS-VERIFY with MGF1 of the same hash and a salt as long as the hash.
    pub fn verify_pss(&self, hash: Hash, digest: &[u8], sig: &[u8]) -> Result<(), Error> {
        let h = hash.len();
        if digest.len() != h {
            return Err(Error::BadSignature);
        }
        let mut full = [0u8; MAX_LEN];
        let k = self.recover(sig, &mut full)?;
        let em_bits = self.bits() - 1;
        let em_len = em_bits.div_ceil(8); // k, or k - 1 when the modulus has 8 j + 1 bits
        if full[..k - em_len].iter().any(|&b| b != 0) {
            return Err(Error::BadSignature); // m >= 2^emBits
        }
        let em = &full[k - em_len..k];
        let db_len = em_len - h - 1; // em_len >= 255 here
        let (masked, rest) = em.split_at(db_len);
        let (m_hash, trailer) = rest.split_at(h);
        let spare = 8 * em_len - em_bits; // leftmost bits of maskedDB that must be zero
        if trailer != [0xbc] || spare != 0 && masked[0] >> (8 - spare) != 0 {
            return Err(Error::BadSignature);
        }
        // DB = maskedDB xor MGF1(H, dbLen), top bits cleared.
        let mut db = [0u8; MAX_LEN];
        mgf1(hash, m_hash, &mut db[..db_len]);
        for (d, m) in db.iter_mut().zip(masked) {
            *d ^= m;
        }
        if spare != 0 {
            db[0] &= 0xff >> spare;
        }
        // DB = 00..00 01 salt, with a salt of exactly h bytes.
        let sep = db_len - h - 1;
        if db[..sep].iter().any(|&b| b != 0) || db[sep] != 1 {
            return Err(Error::BadSignature);
        }
        // H' = Hash(00*8 || mHash || salt).
        let mut m2 = [0u8; 8 + 48 + 48];
        m2[8..8 + h].copy_from_slice(digest);
        m2[8 + h..8 + 2 * h].copy_from_slice(&db[sep + 1..db_len]);
        if hash.digest(&m2[..8 + 2 * h])[..h] == *m_hash { Ok(()) } else { Err(Error::BadSignature) }
    }
}

/// MGF1 (RFC 8017 appendix B.2.1): `out` = Hash(seed || 0) || Hash(seed || 1) ... cut to length.
fn mgf1(hash: Hash, seed: &[u8], out: &mut [u8]) {
    let mut block = [0u8; 48 + 4];
    block[..seed.len()].copy_from_slice(seed);
    for (i, chunk) in out.chunks_mut(hash.len()).enumerate() {
        block[seed.len()..seed.len() + 4].copy_from_slice(&(i as u32).to_be_bytes());
        let d = hash.digest(&block[..seed.len() + 4]);
        chunk.copy_from_slice(&d[..chunk.len()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VECTORS: &str = include_str!("../../../tools/fixtures/rsa/vectors.txt");

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }

    struct Case {
        name: String,
        key: String,
        mode: String,
        digest: Vec<u8>,
        sig: Vec<u8>,
        ok: bool,
    }

    fn load() -> (Vec<(String, Vec<u8>)>, Vec<Case>) {
        let (mut keys, mut cases) = (vec![], vec![]);
        for line in VECTORS.lines().filter(|l| !l.starts_with('#')) {
            let f: Vec<&str> = line.split(' ').collect();
            match f[0] {
                "key" => keys.push((f[1].to_string(), hex(f[2]))),
                "case" => cases.push(Case {
                    name: f[1].into(),
                    key: f[2].into(),
                    mode: f[3].into(),
                    digest: hex(f[4]),
                    sig: hex(f[5]),
                    ok: f[6] == "ok",
                }),
                "cv" => {}
                _ => panic!("{line}"),
            }
        }
        (keys, cases)
    }

    fn run(key: &PublicKey, mode: &str, digest: &[u8], sig: &[u8]) -> Result<(), Error> {
        let (kind, h) = mode.split_once('-').unwrap();
        let hash = if h == "sha256" { Hash::Sha256 } else { Hash::Sha384 };
        match kind {
            "pkcs1" => key.verify_pkcs1(hash, digest, sig),
            _ => key.verify_pss(hash, digest, sig),
        }
    }

    #[test]
    fn every_openssl_and_crafted_vector_gives_the_expected_answer() {
        let (keys, cases) = load();
        assert!(cases.len() > 150);
        let (mut ok, mut bad) = (0, 0);
        for c in &cases {
            let der = &keys.iter().find(|(n, _)| *n == c.key).unwrap().1;
            let key = PublicKey::from_pkcs1(der).unwrap();
            let r = run(&key, &c.mode, &c.digest, &c.sig);
            assert_eq!(r.is_ok(), c.ok, "{} {} {}: {r:?}", c.name, c.key, c.mode);
            if c.ok { ok += 1 } else { bad += 1 }
        }
        assert!(ok >= 36 && bad >= 100, "{ok} {bad}");
    }

    /// Wycheproof-style mutations of every valid signature.
    #[test]
    fn mutated_signatures_fail() {
        let (keys, cases) = load();
        for c in cases.iter().filter(|c| c.ok) {
            let der = &keys.iter().find(|(n, _)| *n == c.key).unwrap().1;
            let key = PublicKey::from_pkcs1(der).unwrap();
            let k = key.modulus().len();
            let mode = c.mode.as_str();
            let fails = |digest: &[u8], sig: &[u8], what: &str| {
                assert!(run(&key, mode, digest, sig).is_err(), "{what}: {} {mode}", c.key);
            };
            // Flipped bits at the start, middle and end.
            for at in [0, k / 2, k - 1] {
                let mut s = c.sig.clone();
                s[at] ^= 0x01;
                fails(&c.digest, &s, "bit flip");
            }
            // Wrong message digest.
            let mut d = c.digest.clone();
            d[0] ^= 0x80;
            fails(&d, &c.sig, "wrong digest");
            // Wrong hash: same bytes under the other hash, and a digest of the other length.
            let other = if mode.ends_with("sha256") { mode.replace("sha256", "sha384") } else { mode.replace("sha384", "sha256") };
            assert!(run(&key, &other, &c.digest, &c.sig).is_err(), "other hash");
            // Wrong padding scheme.
            let swapped = if mode.starts_with("pkcs1") { mode.replace("pkcs1", "pss") } else { mode.replace("pss", "pkcs1") };
            assert!(run(&key, &swapped, &c.digest, &c.sig).is_err(), "other scheme");
            // Wrong lengths.
            fails(&c.digest, &c.sig[1..], "short");
            fails(&c.digest, &c.sig[..k - 1], "short end");
            fails(&c.digest, &[&[0], &c.sig[..]].concat(), "long");
            fails(&c.digest, &[], "empty");
            fails(&c.digest[1..], &c.sig, "short digest");
            fails(&[&c.digest[..], &[0]].concat(), &c.sig, "long digest");
            // Signature not below the modulus: n, n + 1 (when it fits), all ones, zero, one.
            fails(&c.digest, key.modulus(), "sig = n");
            let mut np1 = key.modulus().to_vec();
            let last = np1.len() - 1;
            np1[last] = np1[last].wrapping_add(2); // n + 2, or another value at a carry
            fails(&c.digest, &np1, "sig = n + 2");
            fails(&c.digest, &vec![0xff; k], "sig = 2^k - 1");
            fails(&c.digest, &vec![0; k], "sig = 0");
            let mut one = vec![0; k];
            one[k - 1] = 1;
            fails(&c.digest, &one, "sig = 1");
            let mut nm1 = key.modulus().to_vec();
            nm1[k - 1] -= 1;
            fails(&c.digest, &nm1, "sig = n - 1");
        }
    }

    #[test]
    fn key_policy() {
        let n2048 = {
            let mut n = vec![0xc5u8; 256];
            n[255] = 0xc7;
            n
        };
        fn k<'a>(n: &'a [u8], e: &[u8]) -> Option<PublicKey<'a>> {
            PublicKey::new(n, e)
        }
        assert!(k(&n2048, &[1, 0, 1]).is_some());
        assert_eq!(k(&n2048, &[1, 0, 1]).unwrap().exponent(), 65537);
        assert_eq!(k(&n2048, &[0, 0, 3]).unwrap().exponent(), 3);
        assert_eq!(k(&n2048, &[0xff, 0xff, 0xff, 0xff]).unwrap().exponent(), u32::MAX);
        assert!(k(&n2048, &[1, 0, 0, 0, 1]).is_none(), "e over 32 bits");
        assert!(k(&n2048, &[0, 0, 0, 1, 0, 1]).is_some(), "leading zeros in e are not bits");
        assert!(k(&n2048, &[]).is_none() && k(&n2048, &[0]).is_none() && k(&n2048, &[1]).is_none());
        assert!(k(&n2048, &[2]).is_none() && k(&n2048, &[1, 0, 0]).is_none(), "even e");
        assert!(k(&n2048, &[3]).is_some());
        assert!(k(&[], &[3]).is_none() && k(&[0; 300], &[3]).is_none());
        assert!(k(&n2048[1..], &[3]).is_none(), "2040 bits");
        let mut small = vec![0u8; 5];
        small.extend_from_slice(&n2048);
        assert!(k(&small, &[3]).is_some(), "leading zeros in n are not bits");
        let mut even = n2048.clone();
        even[255] = 0xc6;
        assert!(k(&even, &[3]).is_none(), "even n");
        let mut n2047 = n2048.clone();
        n2047[0] = 0x7f;
        assert!(k(&n2047, &[3]).is_none(), "2047 bits");
        let mut n4096 = vec![0xffu8; 512];
        n4096[0] = 0x80;
        assert!(k(&n4096, &[3]).is_some());
        let mut n4097 = vec![0u8; 513];
        n4097[0] = 1;
        n4097[512] = 1;
        assert!(k(&n4097, &[3]).is_none(), "4097 bits");
    }

    #[test]
    fn rsa_public_key_der_is_strict() {
        let (keys, _) = load();
        let der = &keys.iter().find(|(n, _)| n == "rsa2048").unwrap().1;
        let key = PublicKey::from_pkcs1(der).unwrap();
        assert_eq!((key.bits(), key.exponent()), (2048, 65537));
        let mut t = der.clone();
        t.push(0);
        assert!(PublicKey::from_pkcs1(&t).is_none(), "trailing byte");
        for cut in [0, 1, 4, 100, der.len() - 1] {
            assert!(PublicKey::from_pkcs1(&der[..cut]).is_none(), "cut {cut}");
        }
        // Every single-byte change either fails or gives another valid key: no panic.
        for i in 0..der.len() {
            let mut m = der.clone();
            m[i] ^= 0xff;
            let _ = PublicKey::from_pkcs1(&m);
        }
        let e_changed = {
            let mut m = der.clone();
            let l = m.len();
            m[l - 1] = 0x02; // e = 65538
            m
        };
        assert!(PublicKey::from_pkcs1(&e_changed).is_none(), "even e");
        assert!(PublicKey::from_pkcs1(&[0x30, 0x06, 2, 1, 5, 2, 1, 3]).is_none(), "tiny key");
        assert!(PublicKey::from_pkcs1(&[0x31, 0]).is_none());
    }

    #[test]
    fn garbage_input_never_panics() {
        let (keys, cases) = load();
        let der = &keys.iter().find(|(n, _)| n == "rsa2048").unwrap().1;
        let key = PublicKey::from_pkcs1(der).unwrap();
        let mut x = 0x2545f4914f6cdd1du64;
        for len in [0usize, 1, 255, 256, 257, 300] {
            for _ in 0..8 {
                let sig: Vec<u8> = (0..len).map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    x as u8
                }).collect();
                for mode in ["pkcs1-sha256", "pkcs1-sha384", "pss-sha256", "pss-sha384"] {
                    let d = vec![7u8; if mode.ends_with("256") { 32 } else { 48 }];
                    assert!(run(&key, mode, &d, &sig).is_err());
                }
            }
        }
        // A key of 4096 bits with a signature of 4096 bits of garbage.
        let big = cases.iter().find(|c| c.key == "rsa4096").unwrap();
        let k4 = PublicKey::from_pkcs1(&keys.iter().find(|(n, _)| n == "rsa4096").unwrap().1).unwrap();
        let mut s = big.sig.clone();
        s[10] ^= 4;
        assert!(run(&k4, &big.mode, &big.digest, &s).is_err());
    }
}
