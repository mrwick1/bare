//! SHA-256, as in FIPS 180-4.
//!
//! SHA-256 reads a message in 64-byte blocks. It keeps 8 numbers of 32 bits (the state).
//! For each block, 64 rounds of mixing change the state. At the end, the state is the hash.
//! The last block holds padding: a `0x80` byte, zeros, then the message length in bits
//! as a 64-bit big-endian number. If the length does not fit, padding needs one more block.
//!
//! Streaming: `update` can be called many times. A partial block waits in `buf`.
//! Full blocks go straight from the caller's slice into `compress`: no copy, no heap.

/// Bytes in one hash.
pub const DIGEST_LEN: usize = 32;
/// Bytes in one block.
pub const BLOCK_LEN: usize = 64;

// `const` is a value fixed at build time, like a JS `const` that the compiler inlines.
// First 32 bits of the fractional parts of the cube roots of the first 64 primes.
pub(crate) const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

// First 32 bits of the fractional parts of the square roots of the first 8 primes.
const INIT: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

// `#[derive(Clone)]` writes a `.clone()` method that copies every field.
// HMAC uses it to save a hash state in the middle of a message and reuse it.
// The state is a small array on the stack, so a clone is a plain memory copy.
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    buf: [u8; BLOCK_LEN],
    buf_len: usize,
    // Total message bytes so far. `u64` cannot overflow in practice (2^64 bytes).
    len: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Sha256 { state: INIT, buf: [0; BLOCK_LEN], buf_len: 0, len: 0 }
    }

    // `&mut self` borrows the hasher so the method can change it. `data: &[u8]` is a view
    // of bytes owned by the caller: we read it and never copy the whole message.
    pub fn update(&mut self, mut data: &[u8]) {
        // Debug builds panic on integer overflow, release builds wrap. Say "wrap" on purpose.
        self.len = self.len.wrapping_add(data.len() as u64);
        if self.buf_len > 0 {
            // Fill the waiting partial block first.
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
        // Split `data` into whole blocks and a rest shorter than 64 bytes. `split_at`
        // gives two views of the same bytes: nothing is copied.
        let (blocks, rest) = data.split_at(data.len() - data.len() % BLOCK_LEN);
        compress_blocks(&mut self.state, blocks);
        self.buf[..rest.len()].copy_from_slice(rest);
        self.buf_len = rest.len();
    }

    // `self` (not `&self`) moves the hasher into the call. The caller cannot use it again,
    // so nobody can add data after the padding. The compiler enforces this.
    pub fn finalize(mut self) -> [u8; DIGEST_LEN] {
        let bit_len = self.len.wrapping_mul(8);
        let n = self.buf_len;
        self.buf[n] = 0x80;
        self.buf[n + 1..].fill(0);
        if n + 1 > BLOCK_LEN - 8 {
            // No room for the 8 length bytes: the padding needs a second block.
            let block = self.buf;
            compress_blocks(&mut self.state, &block);
            self.buf = [0; BLOCK_LEN];
        }
        self.buf[BLOCK_LEN - 8..].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buf;
        compress_blocks(&mut self.state, &block);
        let mut out = [0u8; DIGEST_LEN];
        for (chunk, word) in out.chunks_exact_mut(4).zip(self.state) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

/// Hash `data` in one call.
pub fn sha256(data: &[u8]) -> [u8; DIGEST_LEN] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize()
}

// Run whole 64-byte blocks through the state. `data.len()` is a multiple of 64.
// On x86-64 with the SHA extensions we use the CPU instructions (`sha256_x86.rs`).
// Else we use the portable code below. The check runs once per call, not once per block.
fn compress_blocks(state: &mut [u32; 8], data: &[u8]) {
    #[cfg(all(target_arch = "x86_64", not(feature = "force-portable")))]
    if crate::sha256_x86::available() {
        // SAFETY: `available()` just confirmed that the CPU has the needed features.
        unsafe { crate::sha256_x86::compress_blocks(state, data) };
        return;
    }
    compress_blocks_soft(state, data);
}

fn compress_blocks_soft(state: &mut [u32; 8], data: &[u8]) {
    for block in data.chunks_exact(BLOCK_LEN) {
        compress_soft(state, block.try_into().unwrap());
    }
}

// One block: 64 rounds. The message schedule `w` keeps only the last 16 words (a ring):
// word i needs words i-2, i-7, i-15 and i-16, so older words are never read again.
// `wrapping_add` is addition modulo 2^32, which is what the standard means by "+".
// JS numbers have no 32-bit unsigned type, so JS needs `>>> 0` after every add.
//
// `macro_rules!` is a text template that the compiler expands before it checks types.
// `round!` writes one round. Instead of moving 8 variables each round (h = g; g = f; ...),
// we call it 8 times with the names rotated. The compiler then keeps all 8 in registers.
fn compress_soft(state: &mut [u32; 8], block: &[u8; BLOCK_LEN]) {
    let mut w = [0u32; 16];
    for (i, chunk) in block.chunks_exact(4).enumerate() {
        w[i] = u32::from_be_bytes(chunk.try_into().unwrap());
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    macro_rules! round {
        ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $i:expr) => {
            let wi = if $i < 16 {
                w[$i]
            } else {
                let w15 = w[($i + 1) & 15];
                let w2 = w[($i + 14) & 15];
                let s0 = w15.rotate_right(7) ^ w15.rotate_right(18) ^ (w15 >> 3);
                let s1 = w2.rotate_right(17) ^ w2.rotate_right(19) ^ (w2 >> 10);
                let n = w[$i & 15].wrapping_add(s0).wrapping_add(w[($i + 9) & 15]).wrapping_add(s1);
                w[$i & 15] = n;
                n
            };
            let s1 = $e.rotate_right(6) ^ $e.rotate_right(11) ^ $e.rotate_right(25);
            let ch = ($e & $f) ^ (!$e & $g);
            let t1 = $h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[$i]).wrapping_add(wi);
            let s0 = $a.rotate_right(2) ^ $a.rotate_right(13) ^ $a.rotate_right(22);
            let maj = ($a & $b) ^ ($a & $c) ^ ($b & $c);
            $d = $d.wrapping_add(t1);
            $h = t1.wrapping_add(s0.wrapping_add(maj));
        };
    }
    // 64 rounds = 8 passes of 8 rounds. `i` is a plain loop counter; the compiler unrolls
    // the inner 8 because the names are fixed, and the `& 15` ring indexes need no bounds check.
    for pass in 0..8 {
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
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The NIST 896-bit message: 8-letter windows "abcdefgh", "bcdefghi", ... "nopqrstu".
    fn nist_896_bit() -> String {
        (0..14).map(|i| &"abcdefghijklmnopqrstu"[i..i + 8]).collect()
    }

    // FIPS 180-4 / NIST CSRC "SHA-256 example" vectors, and the empty message.
    #[test]
    fn fips_vectors() {
        assert_eq!(hex(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            hex(&sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            hex(&sha256(nist_896_bit().as_bytes())),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
    }

    #[test]
    fn million_a() {
        let mut h = Sha256::new();
        let chunk = [b'a'; 1000];
        for _ in 0..1000 {
            h.update(&chunk);
        }
        assert_eq!(hex(&h.finalize()), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    }

    // Lengths 55, 56, 63, 64, 65 sit on the padding edges: 55 bytes still fit one block,
    // 56 bytes need a second block for the length. The expected hashes come from
    // Python hashlib (a second implementation), not from the standard text.
    #[test]
    fn padding_boundaries() {
        let cases: [(usize, &str); 9] = [
            (55, "595615dbe4f0f407ae397d08b4c2cb870cb9b0e11937416f950c5160acf9c005"),
            (56, "784f623b787495078e93ff28a25b581df0584055a7e71d8cd90c454716b92f51"),
            (57, "808f0738aa4401bdee842e5a15a7baad5809f976d8eb6f9bd2683cebd2e8d671"),
            (63, "5ca3e1ef5207490eac01a795e5cc94d59582a5118bf9534665c8668d87aa647c"),
            (64, "2fcd5a0d60e4c941381fcc4e00a4bf8be422c3ddfafb93c809e8d1e2bfffae8e"),
            (65, "1b3cd1877ab2f2f19f7be001722554f336cb799df0329de0bb4c118dc6abc06d"),
            (119, "faef67da856d6fd9c8d12f9ed0a4fefd3cf0ce085ab43e2907418d457e3c354b"),
            (120, "c9512b08619c19fbb503c7da6b46ef20301e5f7a7a5f43989182398536f5c5c8"),
            (128, "6c05be2c4268843ae47e68e611277ce62c02153f2f4d2e1e2a1a4b44f766cf74"),
        ];
        for (n, want) in cases {
            let data: Vec<u8> = (0..n).map(|i| b'a' + (i % 26) as u8).collect();
            assert_eq!(hex(&sha256(&data)), want, "length {n}");
        }
    }

    // Any way of cutting the message into pieces must give the same hash.
    #[test]
    fn incremental_matches_one_shot() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 + 3) as u8).collect();
        let want = sha256(&data);
        for step in [1, 2, 3, 7, 31, 63, 64, 65, 128, 129, 999, 1000] {
            let mut h = Sha256::new();
            for piece in data.chunks(step) {
                h.update(piece);
            }
            assert_eq!(h.finalize(), want, "step {step}");
        }
        // Empty updates change nothing.
        let mut h = Sha256::new();
        h.update(b"");
        h.update(&data);
        h.update(b"");
        assert_eq!(h.finalize(), want);
    }

    // Both block functions must agree, whatever the CPU. On a CPU without SHA, only the
    // portable path runs and this test checks it against itself (the vectors above still apply).
    #[test]
    fn hardware_and_portable_agree() {
        let mut seed = 0x1234_5678_9abc_def0u64;
        let mut data = vec![0u8; 64 * 40];
        for b in data.iter_mut() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *b = seed as u8;
        }
        for blocks in [1, 2, 3, 17, 40] {
            let mut soft = INIT;
            compress_blocks_soft(&mut soft, &data[..64 * blocks]);
            let mut fast = INIT;
            compress_blocks(&mut fast, &data[..64 * blocks]);
            assert_eq!(soft, fast, "{blocks} blocks");
            #[cfg(all(target_arch = "x86_64", not(feature = "force-portable")))]
            if crate::sha256_x86::available() {
                let mut hw = INIT;
                unsafe { crate::sha256_x86::compress_blocks(&mut hw, &data[..64 * blocks]) };
                assert_eq!(soft, hw, "{blocks} blocks, hardware");
            }
        }
    }

    // The portable path alone, on the FIPS "abc" vector: one padded block built by hand.
    #[test]
    fn portable_path_abc() {
        let mut block = [0u8; 64];
        block[..3].copy_from_slice(b"abc");
        block[3] = 0x80;
        block[63] = 24; // message length in bits
        let mut state = INIT;
        compress_blocks_soft(&mut state, &block);
        let out: Vec<u8> = state.iter().flat_map(|w| w.to_be_bytes()).collect();
        assert_eq!(hex(&out), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn clone_forks_state() {
        let mut a = Sha256::new();
        a.update(b"ab");
        let mut b = a.clone();
        a.update(b"c");
        b.update(b"d");
        assert_eq!(hex(&a.finalize()), hex(&sha256(b"abc")));
        assert_eq!(hex(&b.finalize()), hex(&sha256(b"abd")));
    }
}
