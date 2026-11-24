//! A fast hasher for the maps of the parser and the compiler: the multiply and rotate hash of rustc (FxHash).
//!
//! The keys are names and numbers of the page's own script. The std hasher (SipHash) resists a crafted set of keys, but a script can
//! only slow its own page, as any slow script can. FxHash costs a few instructions for each 8 bytes (design-parse-cost.md, H1).

use std::hash::{BuildHasherDefault, Hasher};

/// A `HashMap` with [`Fx`].
pub(crate) type FxMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<Fx>>;

const K: u64 = 0xf135_7aea_2e62_a9c5;

#[derive(Default, Clone, Copy)]
pub(crate) struct Fx(u64);

impl Fx {
    #[inline]
    fn add(&mut self, w: u64) {
        self.0 = self.0.wrapping_add(w).wrapping_mul(K);
    }
}

impl Hasher for Fx {
    #[inline]
    fn write(&mut self, b: &[u8]) {
        let mut c = b.chunks_exact(8);
        for w in &mut c {
            self.add(u64::from_le_bytes(w.try_into().expect("8 bytes")));
        }
        let r = c.remainder();
        if !r.is_empty() {
            let mut x = [0u8; 8];
            x[..r.len()].copy_from_slice(r);
            self.add(u64::from_le_bytes(x) ^ ((r.len() as u64) << 59));
        }
    }
    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(i as u64)
    }
    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.add(i as u64)
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(i as u64)
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i)
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64)
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.0.rotate_left(26)
    }
}
