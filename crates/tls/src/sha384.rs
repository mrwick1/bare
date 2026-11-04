//! SHA-384, as in FIPS 180-4.
//!
//! SHA-384 is SHA-512 with other start numbers and a shorter output. SHA-512 reads a message
//! in 128-byte blocks. It keeps 8 numbers of 64 bits (the state). For each block, 80 rounds
//! of mixing change the state. SHA-384 starts from its own 8 numbers and, at the end, keeps
//! only the first 6 of them (48 bytes). It never outputs the last 2.
//! The last block holds padding: a `0x80` byte, zeros, then the message length in bits
//! as a 128-bit big-endian number. If the length does not fit, padding needs one more block.
//!
//! Streaming works as in `sha256.rs`: a partial block waits in `buf`, and full blocks go
//! straight from the caller's slice into `compress`. No heap, no copy of the message.
//!
//! Needed for the certificate signatures (ECDSA P-384 with SHA-384). The chosen TLS cipher
//! suite (AES-128-GCM) does not use it for the key schedule.

/// Bytes in one hash.
pub const DIGEST_LEN: usize = 48;
/// Bytes in one block.
pub const BLOCK_LEN: usize = 128;

// First 64 bits of the fractional parts of the cube roots of the first 80 primes.
// (We computed these with exact integer roots, then checked them against the NIST examples.)
const K: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

// First 64 bits of the fractional parts of the square roots of the 9th to 16th primes.
// SHA-512 uses the roots of the first 8 primes instead.
const INIT: [u64; 8] = [
    0xcbbb9d5dc1059ed8, 0x629a292a367cd507, 0x9159015a3070dd17, 0x152fecd8f70e5939,
    0x67332667ffc00b31, 0x8eb44a8768581511, 0xdb0c2e0d64f98fa7, 0x47b5481dbefa4fa4,
];

// `#[derive(Clone)]` copies every field: a plain memory copy of a small stack value.
// A TLS transcript hash clones the state to read the hash so far and still keep going.
#[derive(Clone)]
pub struct Sha384 {
    state: [u64; 8],
    buf: [u8; BLOCK_LEN],
    buf_len: usize,
    // Total message bytes so far. Finalization encodes the bit count in 128 bits.
    len: u128,
}

impl Default for Sha384 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha384 {
    pub fn new() -> Self {
        Sha384 { state: INIT, buf: [0; BLOCK_LEN], buf_len: 0, len: 0 }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        // `wrapping_add` says "wrap" on purpose; 2^128 bytes cannot happen anyway.
        self.len = self.len.wrapping_add(data.len() as u128);
        if self.buf_len > 0 {
            let take = (BLOCK_LEN - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len < BLOCK_LEN {
                return;
            }
            let block = self.buf;
            compress_blocks(&mut self.state, &block);
            self.buf_len = 0;
        }
        let (blocks, rest) = data.split_at(data.len() - data.len() % BLOCK_LEN);
        compress_blocks(&mut self.state, blocks);
        self.buf[..rest.len()].copy_from_slice(rest);
        self.buf_len = rest.len();
    }

    // `self` (not `&self`) moves the hasher in: nobody can add data after the padding.
    // To read a hash and keep going, call `.clone().finalize()`.
    pub fn finalize(mut self) -> [u8; DIGEST_LEN] {
        let bit_len = self.len.wrapping_mul(8);
        let n = self.buf_len;
        self.buf[n] = 0x80;
        self.buf[n + 1..].fill(0);
        if n + 1 > BLOCK_LEN - 16 {
            // No room for the 16 length bytes: the padding needs a second block.
            let block = self.buf;
            compress_blocks(&mut self.state, &block);
            self.buf = [0; BLOCK_LEN];
        }
        self.buf[BLOCK_LEN - 16..].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buf;
        compress_blocks(&mut self.state, &block);
        let mut out = [0u8; DIGEST_LEN];
        // `zip` stops at the shorter side: 6 words, so the last 2 state words are dropped.
        for (chunk, word) in out.chunks_exact_mut(8).zip(self.state) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

/// Hash `data` in one call.
pub fn sha384(data: &[u8]) -> [u8; DIGEST_LEN] {
    let mut h = Sha384::new();
    h.update(data);
    h.finalize()
}

// Run whole 128-byte blocks through the state. `data.len()` is a multiple of 128.
fn compress_blocks(state: &mut [u64; 8], data: &[u8]) {
    for block in data.chunks_exact(BLOCK_LEN) {
        compress(state, block.try_into().unwrap());
    }
}

// One block: 80 rounds. Same shape as SHA-256 with 64-bit words, other rotation amounts and
// a 16-word ring for the message schedule (word i needs i-2, i-7, i-15, i-16).
// `round!` writes one round; calling it with rotated names avoids moving 8 variables.
fn compress(state: &mut [u64; 8], block: &[u8; BLOCK_LEN]) {
    let mut w = [0u64; 16];
    for (i, chunk) in block.chunks_exact(8).enumerate() {
        w[i] = u64::from_be_bytes(chunk.try_into().unwrap());
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    macro_rules! round {
        ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $i:expr) => {
            let wi = if $i < 16 {
                w[$i]
            } else {
                let w15 = w[($i + 1) & 15];
                let w2 = w[($i + 14) & 15];
                let s0 = w15.rotate_right(1) ^ w15.rotate_right(8) ^ (w15 >> 7);
                let s1 = w2.rotate_right(19) ^ w2.rotate_right(61) ^ (w2 >> 6);
                let n = w[$i & 15].wrapping_add(s0).wrapping_add(w[($i + 9) & 15]).wrapping_add(s1);
                w[$i & 15] = n;
                n
            };
            let s1 = $e.rotate_right(14) ^ $e.rotate_right(18) ^ $e.rotate_right(41);
            let ch = ($e & $f) ^ (!$e & $g);
            let t1 = $h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[$i]).wrapping_add(wi);
            let s0 = $a.rotate_right(28) ^ $a.rotate_right(34) ^ $a.rotate_right(39);
            let maj = ($a & $b) ^ ($a & $c) ^ ($b & $c);
            $d = $d.wrapping_add(t1);
            $h = t1.wrapping_add(s0.wrapping_add(maj));
        };
    }
    // 80 rounds = 10 passes of 8 rounds.
    for pass in 0..10 {
        let i = pass * 8;
        round!(a, b, c, d, e, f, g, h, i);
        round!(h, a, b, c, d, e, f, g, i + 1);
        round!(g, h, a, b, c, d, e, f, i + 2);
        round!(f, g, h, a, b, c, d, e, i + 3);
        round!(e, f, g, h, a, b, c, d, i + 4);
        round!(d, e, f, g, h, a, b, c, i + 5);
        round!(c, d, e, f, g, h, a, b, i + 6);
        round!(b, c, d, e, f, g, h, a, i + 7);
    }
    for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *s = s.wrapping_add(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Own helpers: this file must compile alone, so it does not use `sha256`'s test helpers.
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn xorshift(n: usize, mut seed: u64) -> Vec<u8> {
        (0..n)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect()
    }

    // NIST CSRC "SHA-384 example" (SHA384.pdf): one-block "abc" and the two-block 896-bit
    // message. The empty and million-a values are from FIPS 180-2 / NIST CAVP and hashlib.
    #[test]
    fn nist_vectors() {
        assert_eq!(
            hex(&sha384(b"abc")),
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
        );
        assert_eq!(
            hex(&sha384(b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu")),
            "09330c33f71147e83d192fc782cd1b4753111b173b3b05d22fa08086e3b0f712fcc7c71a557e2db966c3e9fa91746039"
        );
        assert_eq!(
            hex(&sha384(b"")),
            "38b060a751ac96384cd9327eb1b1e36a21fdb71114be07434c0cc7bf63f6e1da274edebfe76f65fbd51ad2f14898b95b"
        );
    }

    #[test]
    fn million_a() {
        let mut h = Sha384::new();
        let chunk = [b'a'; 1000];
        for _ in 0..1000 {
            h.update(&chunk);
        }
        assert_eq!(
            hex(&h.finalize()),
            "9d0e1809716474cb086e834e310a4a1ced149e9c00f248527972cec5704c2a5b07b8b3dc38ecc4ebae97ddd87f3d8985"
        );
    }

    // 111 bytes still fit one block with the padding; 112 need a second block for the length.
    // Expected hashes come from Python hashlib (a second implementation).
    #[test]
    fn padding_boundaries() {
        let cases: [(usize, &str); 14] = [
            (0, "38b060a751ac96384cd9327eb1b1e36a21fdb71114be07434c0cc7bf63f6e1da274edebfe76f65fbd51ad2f14898b95b"),
            (1, "54a59b9f22b0b80880d8427e548b7c23abd873486e1f035dce9cd697e85175033caa88e6d57bc35efae0b5afd3145f31"),
            (111, "b075a2f3b7d768e18f586f1f19586a4f7109ca72ba0120947e7813d25f499f92470d819a4d6e42d3b19be8141627c51d"),
            (112, "5523cfb77f9c55e0ccafec5b87d79cde643012283b71188e408c5aeae919a3f29337574d5c729b339d9553984ab0014e"),
            (113, "4866e80003602b6d764752e40d4e1f8c37cbbb09c811c8fd9f25f3b758f1c2d0ffa787671f67a8948e19beebbfad0501"),
            (127, "fb908cfc02679017ab44a8e1008dec239ea36a381a5c7a75bc6d64f70df306f7df0fcf94cd62a45a15685b5bb93c70d7"),
            (128, "674b2e80ff8d94008de7409c7b1f878f9fae3a0a6dae2f982cca7e3aaef91bf325d3eb568263a2e1e6856ac75070062a"),
            (129, "797d56268a2849f4c8fba0f620f1d3dcc94d16b14243d5aae0015c39aa51c13fc455653ec13926cd91393ca42b44709c"),
            (239, "aed8ea98a04588fec17caacdacff7b6d6be78ce07c7982482e878193add00d5dcb5ee74dfbb026183f5e83b063e4da54"),
            (240, "f4fbf0f4d1573c55061e51718f3fd655a3129e6c55db127a4b8c0e82042b9adc735287b64226887ff8ecc01ecf46a054"),
            (241, "b24aed29ccdc1fb9d5171f7b7d9e98d44531922671ed94820e3e8c545591ce465f499422735d763f5ec29d3a483e5382"),
            (255, "fac56e6a8db13e59492b15656421004819222d6ba2af6a8abc645c0d23bc4186d47ff7a764d84139e054d688d8e31357"),
            (256, "d3cf6cafa6e9cbc2a4a78edff618215ca6afb923d8899761cb11cc6f95a3f61bb5ba57649bacd8634dbf78c7467899c0"),
            (257, "9e3bc4b36e50bedabf5a3c5310ec60ee1143f467c12b3a401e3f41e8a47dd4175e2544790b6e7f4151cee41a43d161ea"),
        ];
        for (n, want) in cases {
            let data: Vec<u8> = (0..n).map(|i| b'a' + (i % 26) as u8).collect();
            assert_eq!(hex(&sha384(&data)), want, "length {n}");
        }
    }

    // Deterministic pseudo-random inputs (xorshift64), expected hashes from Python hashlib.
    #[test]
    fn random_inputs_match_hashlib() {
        let cases: [(usize, u64, &str); 3] = [
            (1000, 0x1234567890abcdef, "978917b325c8f8cee5ef398fe2936a850de491be754c8e2853b0590b97f79c0b80783c74a2271e12f72476ae1cfd0bb6"),
            (4099, 0xfeedface12345678, "055578febc722af547fb0990b4893c20e89025e52f8b94002cbfe658a28b07faa535b7011bdf5d67fc0a5c05b42622ab"),
            (70001, 0x123456789abcdef, "cd942757e007a0791ea300220f3982682232241339d9b78cbf674de51ba4f71a9db5dac23c4ea69a80891c282ac636ca"),
        ];
        for (n, seed, want) in cases {
            assert_eq!(hex(&sha384(&xorshift(n, seed))), want, "length {n}");
        }
    }

    // Any way of cutting the message into pieces must give the same hash, at any start offset.
    #[test]
    fn incremental_matches_one_shot() {
        let data = xorshift(1000, 7);
        let want = sha384(&data);
        for step in [1, 2, 3, 7, 31, 63, 64, 65, 111, 112, 127, 128, 129, 255, 256, 999, 1000] {
            let mut h = Sha384::new();
            for piece in data.chunks(step) {
                h.update(piece);
            }
            assert_eq!(h.finalize(), want, "step {step}");
        }
        let mut h = Sha384::new();
        h.update(b"");
        h.update(&data);
        h.update(b"");
        assert_eq!(h.finalize(), want);
        // Every split point of one message, including 0 and the full length.
        for cut in 0..=data.len() {
            let mut h = Sha384::new();
            h.update(&data[..cut]);
            h.update(&data[cut..]);
            assert_eq!(h.finalize(), want, "cut {cut}");
        }
    }

    // A clone mid-stream is independent of the original (transcript-hash use).
    #[test]
    fn clone_forks_state() {
        let mut a = Sha384::new();
        a.update(&[7u8; 130]); // one full block done, 2 bytes waiting
        let mut b = a.clone();
        assert_eq!(a.clone().finalize(), sha384(&[7u8; 130]));
        a.update(b"c");
        b.update(b"d");
        let mut ac = [7u8; 130].to_vec();
        ac.push(b'c');
        let mut bd = [7u8; 130].to_vec();
        bd.push(b'd');
        assert_eq!(a.finalize(), sha384(&ac));
        assert_eq!(b.finalize(), sha384(&bd));
    }

    // The bit length is the byte count times 8, in 128 bits. This checks only the arithmetic.
    // Real hashes past 2^32 bytes are checked against hashlib by tools/crypto/sha384_load.rs.
    #[test]
    fn length_counter_is_wide() {
        let mut h = Sha384::new();
        h.len = (1u128 << 61) + 5; // bits = 2^64 + 40: needs more than 64 bits
        let bits = h.len.wrapping_mul(8);
        assert_eq!(bits, (1u128 << 64) + 40);
        assert_eq!(&bits.to_be_bytes()[..8], &[0, 0, 0, 0, 0, 0, 0, 1]);
    }
}
