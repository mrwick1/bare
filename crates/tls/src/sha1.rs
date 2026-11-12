//! SHA-1, as in FIPS 180-4.
//!
//! SHA-1 reads a message in 64-byte blocks. It keeps 5 numbers of 32 bits (the state). For each
//! block, 80 rounds of mixing change the state. The last block holds padding: a `0x80` byte,
//! zeros, then the message length in bits as a 64-bit big-endian number. If the length does
//! not fit, padding needs one more block.
//!
//! SHA-1 is broken for collisions. It exists for the web `crypto.subtle.digest('SHA-1')` only.
//! Do not use it to protect data.
//!
//! One call hashes a whole message. The padding lives in a small stack buffer; the message is not copied.

/// Bytes in one hash.
pub const DIGEST_LEN: usize = 20;
/// Bytes in one block.
pub const BLOCK_LEN: usize = 64;

const INIT: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];

/// Hash `data` in one call.
pub fn sha1(data: &[u8]) -> [u8; DIGEST_LEN] {
    let mut state = INIT;
    let full = data.len() - data.len() % BLOCK_LEN;
    compress_blocks(&mut state, &data[..full]);
    // The rest, the `0x80` byte and the length: one block, or two if the length does not fit.
    let rest = &data[full..];
    let mut tail = [0u8; 2 * BLOCK_LEN];
    tail[..rest.len()].copy_from_slice(rest);
    tail[rest.len()] = 0x80;
    let n = if rest.len() + 1 + 8 <= BLOCK_LEN { BLOCK_LEN } else { 2 * BLOCK_LEN };
    let bit_len = (data.len() as u64).wrapping_mul(8);
    tail[n - 8..n].copy_from_slice(&bit_len.to_be_bytes());
    compress_blocks(&mut state, &tail[..n]);
    let mut out = [0u8; DIGEST_LEN];
    for (chunk, word) in out.chunks_exact_mut(4).zip(state) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    out
}

// Run whole 64-byte blocks through the state. `data.len()` is a multiple of 64.
fn compress_blocks(state: &mut [u32; 5], data: &[u8]) {
    for block in data.chunks_exact(BLOCK_LEN) {
        compress(state, block.try_into().unwrap());
    }
}

// One block: 80 rounds in four groups of 20. The message schedule expands 16 words to 80.
fn compress(state: &mut [u32; 5], block: &[u8; BLOCK_LEN]) {
    let mut w = [0u32; 80];
    for (i, chunk) in block.chunks_exact(4).enumerate() {
        w[i] = u32::from_be_bytes(chunk.try_into().unwrap());
    }
    for t in 16..80 {
        w[t] = (w[t - 3] ^ w[t - 8] ^ w[t - 14] ^ w[t - 16]).rotate_left(1);
    }
    let [mut a, mut b, mut c, mut d, mut e] = *state;
    for (t, wt) in w.iter().enumerate() {
        let (f, k) = match t / 20 {
            0 => ((b & c) | (!b & d), 0x5A827999),
            1 => (b ^ c ^ d, 0x6ED9EBA1),
            2 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
            _ => (b ^ c ^ d, 0xCA62C1D6),
        };
        let temp = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wt);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = temp;
    }
    for (s, v) in state.iter_mut().zip([a, b, c, d, e]) {
        *s = s.wrapping_add(v);
    }
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
        assert_eq!(hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(hex(&sha1(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")), "84983e441c3bd26ebaae4aa1f95129e5e54670f1");
    }

    // FIPS 180-4 "one million a's". Expected from Python hashlib.
    #[test]
    fn million_a() {
        assert_eq!(hex(&sha1(&[b'a'; 1_000_000])), "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
    }

    // Lengths around the padding boundary: 55 fits one block with the padding, 56 needs two.
    // Expected from Python hashlib.
    #[test]
    fn padding_boundaries() {
        let cases: [(usize, &str); 8] = [
            (0, "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            (55, "a617d006d1ca12671785098a19a87fe58443bde9"),
            (56, "4ad5bb7ae3c4024768d364b77c52128ea3cffebe"),
            (63, "fc8a5ab77259625085ead3ec96515b3b8d933fad"),
            (64, "93249d4c2f8903ebf41ac358473148ae6ddd7042"),
            (119, "edd0f1133d0e4ca5f3e98bb7e0295f31d20d2cdb"),
            (120, "23a58eee587aa1f50d19a969ab36a3fe3e88c393"),
            (128, "3698f9beb7cdf5b1e1ce786672fb234143d4df89"),
        ];
        for (n, want) in cases {
            let data: Vec<u8> = (0..n).map(|i| b'a' + (i % 26) as u8).collect();
            assert_eq!(hex(&sha1(&data)), want, "length {n}");
        }
    }
}
