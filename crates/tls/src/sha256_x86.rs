//! SHA-256 block function with the x86-64 SHA extensions (Intel SHA-NI, AMD Zen).
//!
//! The CPU has instructions for two rounds at once (`sha256rnds2`) and for the message
//! schedule (`sha256msg1`, `sha256msg2`). We call them through `std::arch`, which is
//! part of the standard library: each function is one CPU instruction, not borrowed code.
//! The round loop below is our own. The portable code in `sha256.rs` is the reference:
//! a test checks that both give the same state for many inputs.
//!
//! Register layout: the instructions want the state as two vectors, ABEF and CDGH,
//! not as `[a, b, c, d, e, f, g, h]`. We convert on entry and on exit.

use crate::sha256::K;
use std::arch::x86_64::*;

/// True when this CPU has every instruction set that `compress_blocks` needs.
/// `is_x86_feature_detected!` runs `cpuid` once and caches the answer.
pub fn available() -> bool {
    is_x86_feature_detected!("sha") && is_x86_feature_detected!("sse2") && is_x86_feature_detected!("ssse3") && is_x86_feature_detected!("sse4.1")
}

/// Run all 64-byte blocks of `data` through the state. `data.len()` must be a multiple of 64.
///
/// # Safety
/// The CPU must have the `sha`, `ssse3` and `sse4.1` features: call `available()` first.
/// Calling a `#[target_feature]` function on a CPU without them is undefined behavior.
// `unsafe fn` means "the caller must uphold a rule that the compiler cannot check".
// The rule is written in the `# Safety` section, as the Rust docs ask.
#[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
pub unsafe fn compress_blocks(state: &mut [u32; 8], data: &[u8]) {
    debug_assert!(data.len().is_multiple_of(64));
    unsafe {
        // Convert [a,b,c,d][e,f,g,h] to ABEF and CDGH.
        let dcba = _mm_loadu_si128(state.as_ptr() as *const __m128i);
        let hgfe = _mm_loadu_si128(state.as_ptr().add(4) as *const __m128i);
        let cdab = _mm_shuffle_epi32(dcba, 0xB1);
        let efgh = _mm_shuffle_epi32(hgfe, 0x1B);
        let mut abef = _mm_alignr_epi8(cdab, efgh, 8);
        let mut cdgh = _mm_blend_epi16(efgh, cdab, 0xF0);
        // The message words are big-endian; this shuffle reverses the bytes of each 32-bit word.
        let swap = _mm_set_epi64x(0x0c0d0e0f_08090a0b, 0x04050607_00010203);

        for block in data.chunks_exact(64) {
            let (save_abef, save_cdgh) = (abef, cdgh);
            let p = block.as_ptr() as *const __m128i;
            // `m` holds the last 4 groups of 4 schedule words (a ring of 4 vectors).
            let mut m = [
                _mm_shuffle_epi8(_mm_loadu_si128(p), swap),
                _mm_shuffle_epi8(_mm_loadu_si128(p.add(1)), swap),
                _mm_shuffle_epi8(_mm_loadu_si128(p.add(2)), swap),
                _mm_shuffle_epi8(_mm_loadu_si128(p.add(3)), swap),
            ];
            for g in 0..16 {
                if g >= 4 {
                    // W[t] = W[t-16] + s0(W[t-15]) + W[t-7] + s1(W[t-2]), four words at once.
                    let (w16, w12, w8, w4) = (m[g & 3], m[(g + 1) & 3], m[(g + 2) & 3], m[(g + 3) & 3]);
                    let t = _mm_add_epi32(_mm_sha256msg1_epu32(w16, w12), _mm_alignr_epi8(w4, w8, 4));
                    m[g & 3] = _mm_sha256msg2_epu32(t, w4);
                }
                let k = _mm_loadu_si128(K.as_ptr().add(4 * g) as *const __m128i);
                let wk = _mm_add_epi32(m[g & 3], k);
                // Two rounds with the low half of wk, then two with the high half.
                cdgh = _mm_sha256rnds2_epu32(cdgh, abef, wk);
                abef = _mm_sha256rnds2_epu32(abef, cdgh, _mm_shuffle_epi32(wk, 0x0E));
            }
            abef = _mm_add_epi32(abef, save_abef);
            cdgh = _mm_add_epi32(cdgh, save_cdgh);
        }

        // Back to [a,b,c,d][e,f,g,h].
        let feba = _mm_shuffle_epi32(abef, 0x1B);
        let dchg = _mm_shuffle_epi32(cdgh, 0xB1);
        let dcba = _mm_blend_epi16(feba, dchg, 0xF0);
        let hgfe = _mm_alignr_epi8(dchg, feba, 8);
        _mm_storeu_si128(state.as_mut_ptr() as *mut __m128i, dcba);
        _mm_storeu_si128(state.as_mut_ptr().add(4) as *mut __m128i, hgfe);
    }
}
