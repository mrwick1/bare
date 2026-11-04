//! HMAC-SHA256, as in RFC 2104. Test vectors: RFC 4231.
//!
//! HMAC(key, msg) = H((key ^ opad) || H((key ^ ipad) || msg)), where the key is first
//! padded with zeros to one block (64 bytes), or hashed if it is longer than one block.
//! `ipad` is the byte 0x36 and `opad` is the byte 0x5c, repeated 64 times.
//!
//! `Hmac::new` hashes the two pad blocks once and keeps both hash states. Then `Hmac` can
//! be cloned for each message without hashing the key again (HKDF does this).
//! Nothing here uses the heap: pads are 64-byte arrays on the stack.

use crate::sha256::{BLOCK_LEN, DIGEST_LEN, Sha256, sha256};

#[derive(Clone)]
pub struct Hmac {
    inner: Sha256,
    // The outer hash already holds `key ^ opad`. It waits for the inner digest.
    outer: Sha256,
}

impl Hmac {
    /// Any key length is valid. A key longer than 64 bytes is hashed first (RFC 2104 §2).
    pub fn new(key: &[u8]) -> Self {
        let mut block = [0u8; BLOCK_LEN];
        if key.len() > BLOCK_LEN {
            block[..DIGEST_LEN].copy_from_slice(&sha256(key));
        } else {
            block[..key.len()].copy_from_slice(key);
        }
        let mut inner = Sha256::new();
        let mut outer = Sha256::new();
        // `for b in block.iter_mut()` gives `&mut u8`: `*b` is the byte itself.
        // We change the same array twice (xor 0x36, then xor 0x36 ^ 0x5c) to avoid a second array.
        for b in block.iter_mut() {
            *b ^= 0x36;
        }
        inner.update(&block);
        for b in block.iter_mut() {
            *b ^= 0x36 ^ 0x5c;
        }
        outer.update(&block);
        Hmac { inner, outer }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    pub fn finalize(self) -> [u8; DIGEST_LEN] {
        let mut outer = self.outer;
        outer.update(&self.inner.finalize());
        outer.finalize()
    }
}

/// MAC `data` with `key` in one call.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; DIGEST_LEN] {
    let mut h = Hmac::new(key);
    h.update(data);
    h.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sha256::hex;

    // RFC 4231 section 4, HMAC-SHA-256 lines. `case` is the test case number.
    fn check(case: u32, key: &[u8], data: &[u8], want: &str) {
        let got = hex(&hmac_sha256(key, data));
        // Case 5 is a truncation to 128 bits: compare the first 16 bytes only.
        assert_eq!(&got[..want.len()], want, "RFC 4231 case {case}");
    }

    #[test]
    fn rfc4231() {
        check(1, &[0x0b; 20], b"Hi There", "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
        check(2, b"Jefe", b"what do ya want for nothing?", "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        check(3, &[0xaa; 20], &[0xdd; 50], "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe");
        let key4: Vec<u8> = (1..=25).collect();
        check(4, &key4, &[0xcd; 50], "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b");
        check(5, &[0x0c; 20], b"Test With Truncation", "a3b6167473100ee06e0c796c2955552b");
        check(
            6,
            &[0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First",
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
        );
        check(
            7,
            &[0xaa; 131],
            b"This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm.",
            "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2",
        );
    }

    // Keys at 63, 64, 65 bytes: 64 is the last key used as is, 65 is the first one hashed.
    // Expected values come from Python hmac (a second implementation).
    #[test]
    fn key_length_edges() {
        let cases = [
            (0usize, "eb08c1f56d5ddee07f7bdf80468083da06b64cf4fac64fe3a90883df5feacae4"),
            (63, "1e790456a72e90984a55ba51e9a02e8663e33ee74334be97481e7c40f82fb2c2"),
            (64, "67437bb07e3af377548cc5d9cc6cf9a45a0611033cd59e800e8a27fd4ba418fa"),
            (65, "51a150878fcd81ffca19e1efd75865087e60a5f4ab44c204e5012b8d3467fac2"),
            (200, "92b31a47880e85b71afecae4344d697666612acd74ca51d6030f674e53002411"),
        ];
        for (n, want) in cases {
            assert_eq!(hex(&hmac_sha256(&vec![0x5a; n], b"message")), want, "key length {n}");
        }
    }

    #[test]
    fn incremental_and_reuse() {
        let key = [7u8; 40];
        let data: Vec<u8> = (0..300u32).map(|i| i as u8).collect();
        let want = hmac_sha256(&key, &data);
        for step in [1, 5, 64, 100] {
            let mut h = Hmac::new(&key);
            for piece in data.chunks(step) {
                h.update(piece);
            }
            assert_eq!(h.finalize(), want, "step {step}");
        }
        // A clone of a keyed Hmac is an independent copy.
        let base = Hmac::new(&key);
        let mut a = base.clone();
        let mut b = base.clone();
        a.update(b"x");
        b.update(b"y");
        assert_eq!(a.finalize(), hmac_sha256(&key, b"x"));
        assert_eq!(b.finalize(), hmac_sha256(&key, b"y"));
    }

    #[test]
    fn empty_key_and_data() {
        // Python: hmac.new(b"", b"", "sha256").hexdigest()
        assert_eq!(hex(&hmac_sha256(b"", b"")), "b613679a0814d9ec772f95d778c35fc5ff1697c493715653c6c712144292c5ad");
    }
}
