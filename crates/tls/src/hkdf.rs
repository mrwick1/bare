//! HKDF-SHA256, as in RFC 5869. Test vectors: RFC 5869 appendix A.
//!
//! Extract: PRK = HMAC(salt, IKM). It turns input key material into a uniform 32-byte key.
//! Expand: OKM = T(1) || T(2) || ..., where T(i) = HMAC(PRK, T(i-1) || info || i).
//! `i` is one byte, counting from 1, so the output can be at most 255 * 32 = 8160 bytes.
//! A longer request is an error. It is never cut short.

use crate::hmac::{Hmac, hmac_sha256};
use crate::sha256::DIGEST_LEN;
use std::fmt;

/// The most bytes `expand` can write: 255 blocks of 32 bytes (RFC 5869 §2.3).
pub const MAX_OUTPUT_LEN: usize = 255 * DIGEST_LEN;

// An `enum` lists the possible values. This one has a single case now. Callers `match` on
// it, so a new case later shows up as a compile error in their code, not as a silent bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HkdfError {
    /// The output buffer is longer than `MAX_OUTPUT_LEN`.
    OutputTooLong,
}

// `Display` is the message for people (like JS `error.message`). `Error` marks the type as an error.
impl fmt::Display for HkdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HkdfError::OutputTooLong => write!(f, "HKDF output longer than {MAX_OUTPUT_LEN} bytes"),
        }
    }
}
impl std::error::Error for HkdfError {}

/// HKDF-Extract. An empty `salt` is the same as 32 zero bytes: HMAC pads a short key with zeros.
pub fn extract(salt: &[u8], ikm: &[u8]) -> [u8; DIGEST_LEN] {
    hmac_sha256(salt, ikm)
}

/// HKDF-Expand. Fills all of `okm`. It needs `okm.len() <= MAX_OUTPUT_LEN`, else it
/// returns `OutputTooLong` and writes nothing. An empty `okm` is allowed.
/// `prk` is always 32 bytes here: that is what TLS 1.3 uses. RFC 5869 also allows longer keys.
pub fn expand(prk: &[u8; DIGEST_LEN], info: &[u8], okm: &mut [u8]) -> Result<(), HkdfError> {
    if okm.len() > MAX_OUTPUT_LEN {
        return Err(HkdfError::OutputTooLong);
    }
    // Key the HMAC once; each block clones the keyed state (a 200-byte copy, no heap).
    let keyed = Hmac::new(prk);
    let mut prev = [0u8; DIGEST_LEN];
    // `chunks_mut` gives mutable views of `okm`, 32 bytes each (the last one can be shorter).
    for (i, out) in okm.chunks_mut(DIGEST_LEN).enumerate() {
        let mut h = keyed.clone();
        if i > 0 {
            h.update(&prev); // T(0) is empty, so the first block skips this.
        }
        h.update(info);
        h.update(&[(i + 1) as u8]); // i + 1 <= 255 because of the check above.
        prev = h.finalize();
        out.copy_from_slice(&prev[..out.len()]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sha256::hex;

    fn bytes(range: std::ops::RangeInclusive<u8>) -> Vec<u8> {
        range.collect()
    }

    fn run(ikm: &[u8], salt: &[u8], info: &[u8], l: usize, prk: &str, okm: &str) {
        let p = extract(salt, ikm);
        assert_eq!(hex(&p), prk);
        let mut out = vec![0u8; l];
        expand(&p, info, &mut out).unwrap();
        assert_eq!(hex(&out), okm);
    }

    // RFC 5869 appendix A, test cases 1 to 3 (the SHA-256 cases).
    #[test]
    fn rfc5869_case1() {
        run(
            &[0x0b; 22],
            &bytes(0x00..=0x0c),
            &bytes(0xf0..=0xf9),
            42,
            "077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5",
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865",
        );
    }

    #[test]
    fn rfc5869_case2() {
        run(
            &bytes(0x00..=0x4f),
            &bytes(0x60..=0xaf),
            &bytes(0xb0..=0xff),
            82,
            "06a6b88c5853361a06104c9ceb35b45cef760014904671014a193f40c15fc244",
            "b11e398dc80327a1c8e7f78c596a49344f012eda2d4efad8a050cc4c19afa97c59045a99cac7827271cb41c65e590e09da3275600c2f09b8367793a9aca3db71cc30c58179ec3e87c14c01d5c1f3434f1d87",
        );
    }

    #[test]
    fn rfc5869_case3_empty_salt_and_info() {
        run(
            &[0x0b; 22],
            b"",
            b"",
            42,
            "19ef24a32c717b167f33a91d6f648bdf96596776afdb6377ac434c1c293ccb04",
            "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8",
        );
        // An empty salt equals HashLen zero bytes (RFC 5869 §2.2).
        assert_eq!(extract(b"", &[0x0b; 22]), extract(&[0; 32], &[0x0b; 22]));
    }

    #[test]
    fn maximum_length_works_and_matches_definition() {
        let prk = extract(b"salt", b"ikm");
        let mut out = vec![0u8; MAX_OUTPUT_LEN];
        expand(&prk, b"info", &mut out).unwrap();
        // Build T(1)..T(255) straight from the definition with one-shot HMAC.
        let mut want = Vec::new();
        let mut prev: Vec<u8> = Vec::new();
        for i in 1..=255u8 {
            let mut msg = prev.clone();
            msg.extend_from_slice(b"info");
            msg.push(i);
            prev = hmac_sha256(&prk, &msg).to_vec();
            want.extend_from_slice(&prev);
        }
        assert_eq!(out, want);
    }

    #[test]
    fn too_long_is_an_error_and_writes_nothing() {
        let prk = extract(b"salt", b"ikm");
        let mut out = vec![0xee; MAX_OUTPUT_LEN + 1];
        assert_eq!(expand(&prk, b"", &mut out), Err(HkdfError::OutputTooLong));
        assert!(out.iter().all(|&b| b == 0xee));
        assert!(HkdfError::OutputTooLong.to_string().contains("8160"));
    }

    #[test]
    fn empty_and_partial_blocks() {
        let prk = extract(b"s", b"k");
        expand(&prk, b"i", &mut []).unwrap();
        // A short request is a prefix of a longer one.
        let mut long = [0u8; 100];
        expand(&prk, b"i", &mut long).unwrap();
        for n in [1, 31, 32, 33, 64, 99] {
            let mut short = vec![0u8; n];
            expand(&prk, b"i", &mut short).unwrap();
            assert_eq!(short, long[..n], "length {n}");
        }
    }
}
