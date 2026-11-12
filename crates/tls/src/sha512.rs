//! SHA-512, as in FIPS 180-4.
//!
//! SHA-512 reads a message in 128-byte blocks, with 80 rounds of 64-bit mixing for each block.
//! The round function and the constants are the same as in `sha384.rs`. Only the start numbers
//! differ, and SHA-512 keeps all 8 words of the state. The last block holds padding: a `0x80`
//! byte, zeros, then the message length in bits as a 128-bit big-endian number.
//!
//! One call hashes a whole message. The padding lives in a small stack buffer; the message is not copied.

use crate::sha384::{BLOCK_LEN, compress_blocks};

/// Bytes in one hash.
pub const DIGEST_LEN: usize = 64;

// First 64 bits of the fractional parts of the square roots of the first 8 primes.
const INIT: [u64; 8] = [
    0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1,
    0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
];

/// Hash `data` in one call.
pub fn sha512(data: &[u8]) -> [u8; DIGEST_LEN] {
    let mut state = INIT;
    let full = data.len() - data.len() % BLOCK_LEN;
    compress_blocks(&mut state, &data[..full]);
    // The rest, the `0x80` byte and the length: one block, or two if the length does not fit.
    let rest = &data[full..];
    let mut tail = [0u8; 2 * BLOCK_LEN];
    tail[..rest.len()].copy_from_slice(rest);
    tail[rest.len()] = 0x80;
    let n = if rest.len() + 1 + 16 <= BLOCK_LEN { BLOCK_LEN } else { 2 * BLOCK_LEN };
    let bit_len = (data.len() as u128).wrapping_mul(8);
    tail[n - 16..n].copy_from_slice(&bit_len.to_be_bytes());
    compress_blocks(&mut state, &tail[..n]);
    let mut out = [0u8; DIGEST_LEN];
    for (chunk, word) in out.chunks_exact_mut(8).zip(state) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    // FIPS 180-4 / NIST examples: "abc", the empty message, and the 448-bit two-block message.
    #[test]
    fn nist_vectors() {
        assert_eq!(
            hex(&sha512(b"abc")),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        assert_eq!(
            hex(&sha512(b"")),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
        assert_eq!(
            hex(&sha512(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "204a8fc6dda82f0a0ced7beb8e08a41657c16ef468b228a8279be331a703c33596fd15c13b1b07f9aa1d3bea57789ca031ad85c7a71dd70354ec631238ca3445"
        );
    }

    // Lengths around the padding boundary: 111 fits one block with the padding, 112 needs two.
    // Expected from Python hashlib.
    #[test]
    fn padding_boundaries() {
        let cases: [(usize, &str); 6] = [
            (111, "a467698069eae8ed1e0c6dbfd1b4a247a9f1e7ff4e3af62145ed26f4468bc094610878b7644091141370a47a7638bddc95dbfe8971c34d13c4815d4bb1b3e7f2"),
            (112, "a473c93732eef627d02e86d19047a422b586110848ec17dcea13af282a152f7654b0c711e277fd42c1d94bea8b7fed615c52bb0f849227e16240afffc7c56e29"),
            (113, "65a86015e5e9aa4863c4aea3431e497b6a4860afd48e47fdccdf07172f873a9a080b5ddf5fc38c0f0d37424298cc9cd450a14a0e5fba146847ceb8cb544457d3"),
            (127, "c6ce82cb15c8d7040ef3704b1cb161ae3a7f0ac430433e6d52e3f37bf561271bfd014328660d29ee733a9f7f178671390eb0f80231603c8360246753e6406b8d"),
            (128, "217d3d9c0952c3e4907f06d4fbf34460ee852c6af591b07c2fa1c5e16455836374c95ae33e184227913f8a2e227e3bbd5187ce57aa1bad11a80f622412eb0884"),
            (129, "18d5a20f40cd7903e625ac1e4fc10971fb3bd6cecb12a684f7600975340649eebccd11cc97fe9e36bbcb3ae75d6d7a0dc2e60e0022c579e87c3ee2e3ffaf65cc"),
        ];
        for (n, want) in cases {
            let data: Vec<u8> = (0..n).map(|i| b'a' + (i % 26) as u8).collect();
            assert_eq!(hex(&sha512(&data)), want, "length {n}");
        }
    }

    // FIPS 180-4 "one million a's". Expected from Python hashlib.
    #[test]
    fn million_a() {
        assert_eq!(
            hex(&sha512(&[b'a'; 1_000_000])),
            "e718483d0ce769644e2e42c7bc15b4638e1f98b13b2044285632a803afa973ebde0ff244877ea60a4cb0432ce577c31beb009c5c2c49aa2e4eadb217ad8cc09b"
        );
    }
}
