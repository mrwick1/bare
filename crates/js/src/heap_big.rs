//! BigInts too large for `i64`. A submodule of `heap.rs`.
//!
//! A BigInt that fits in `i64` is `Value::BigInt` and never reaches the heap. Any other BigInt is one entry in `bigs`: the
//! limbs (little-endian `u64`), a sign and a hash. The heap keeps one entry for each value, like strings: `new_bigint` looks
//! the value up in a hash table first. So `Value::BigHeap` handles are equal exactly when the values are equal, and
//! `strict_equals`, `SameValueZero`, `Map` keys and `Set` members need no heap and no comparison of limbs.
//!
//! Cost of one entry: the slot (8 bytes of metadata, 4 of back index, 1 mark), the entry itself (24 bytes), the limbs, and 8
//! bytes of table at load 0.5. A value of 2 limbs (up to 2^128) is about 65 bytes. Entries are traced and swept like strings.
//! The table is weak and is rebuilt after a sweep.

use super::*;
use crate::bigint::{self, Int, Sv};

pub(super) struct Big {
    limbs: Box<[u64]>,
    neg: bool,
    hash: u32,
}

impl Big {
    fn sv(&self) -> Sv<'_> {
        Sv { neg: self.neg, mag: &self.limbs }
    }
}

/// Weak hash set of `bigs` entries: open addressing, linear probing, power-of-two size. An entry is a dense position plus one.
pub(super) struct BigTable {
    table: Vec<u32>,
    count: usize,
}

impl BigTable {
    pub(super) fn new() -> Self {
        BigTable { table: vec![0; 16], count: 0 }
    }
    fn find(&self, bigs: &Slab<Big>, hash: u32, a: Sv) -> Option<u32> {
        let mask = self.table.len() - 1;
        let mut p = hash as usize & mask;
        loop {
            let e = self.table[p];
            if e == 0 {
                return None;
            }
            let b = &bigs.live[e as usize - 1];
            if b.hash == hash && b.neg == a.neg && *b.limbs == *a.mag {
                return Some(e - 1);
            }
            p = (p + 1) & mask;
        }
    }
    fn insert_slot(&mut self, hash: u32, dense: u32) {
        let mask = self.table.len() - 1;
        let mut p = hash as usize & mask;
        while self.table[p] != 0 {
            p = (p + 1) & mask;
        }
        self.table[p] = dense + 1;
        self.count += 1;
    }
    fn reset(&mut self, bigs: &Slab<Big>, size: usize) {
        self.table.clear();
        self.table.resize(size, 0);
        self.count = 0;
        for (d, b) in bigs.live.iter().enumerate() {
            self.insert_slot(b.hash, d as u32);
        }
    }
    pub(super) fn rebuild(&mut self, bigs: &Slab<Big>) {
        self.reset(bigs, (bigs.live.len() * 2).next_power_of_two().max(16));
    }
}

fn big_cost(limbs: usize) -> usize {
    slot_bytes::<Big>() + limbs * 8 + 8
}

pub(super) fn big_bytes(b: &Big) -> usize {
    big_cost(b.limbs.len())
}

impl Heap {
    /// `i` as a BigInt value: inline when it fits in `i64`, else the one shared heap entry for its value.
    /// A value with more than `max_big_limbs` limbs is `Limit::BigLimbs`.
    pub fn new_bigint(&mut self, i: Int) -> R<Value> {
        if let Some(v) = i.to_i64() {
            return Ok(Value::BigInt(v));
        }
        self.big_entry(i.neg, i.mag)
    }

    /// The BigInt for an unsigned 64-bit value (a `BigUint64Array` element).
    pub fn bigint_from_u64(&mut self, v: u64) -> R<Value> {
        match i64::try_from(v) {
            Ok(n) => Ok(Value::BigInt(n)),
            Err(_) => self.big_entry(false, vec![v]),
        }
    }

    /// Does a result of `limbs` limbs fit the cap? Check this before an operation allocates its result.
    pub fn big_fits(&self, limbs: u64) -> bool {
        limbs <= self.limits.max_big_limbs as u64
    }

    fn big_entry(&mut self, neg: bool, mag: Vec<u64>) -> R<Value> {
        if mag.len() > self.limits.max_big_limbs {
            return Err(HeapError::Limit(Limit::BigLimbs));
        }
        let a = Sv { neg, mag: &mag };
        let hash = bigint::hash(a);
        if let Some(d) = self.big_table.find(&self.bigs, hash, a) {
            let index = self.bigs.back[d as usize];
            return Ok(Value::BigHeap(BigId { index, gn: self.bigs.meta[index as usize].gn }));
        }
        let cost = big_cost(mag.len());
        self.charge(cost)?;
        if !self.bigs.can_alloc() {
            return Err(HeapError::Limit(Limit::Bigs));
        }
        if (self.big_table.count + 1) * 2 > self.big_table.table.len() {
            // Grow before the insert so the load stays at most 0.5 and a probe always ends.
            let size = self.big_table.table.len() * 2;
            self.big_table.reset(&self.bigs, size);
        }
        let (index, gn) = self.bigs.alloc(Big { limbs: mag.into_boxed_slice(), neg, hash }, Limit::Bigs)?;
        self.big_table.insert_slot(hash, self.bigs.live.len() as u32 - 1);
        self.bytes += cost;
        Ok(Value::BigHeap(BigId { index, gn }))
    }

    fn big(&self, id: BigId) -> R<&Big> {
        self.bigs.get(id.index, id.gn).ok_or(HeapError::Stale)
    }

    pub(super) fn check_big(&self, id: BigId) -> R<()> {
        self.big(id).map(drop)
    }

    /// The signed value of a BigInt. `buf` holds the limb of an inline value. Not a BigInt: `Stale` (the caller checked).
    pub fn bigint_sv<'a>(&'a self, v: Value, buf: &'a mut [u64; 1]) -> R<Sv<'a>> {
        match v {
            Value::BigInt(n) => Ok(bigint::small(n, buf)),
            Value::BigHeap(id) => Ok(self.big(id)?.sv()),
            _ => Err(HeapError::Stale),
        }
    }

    /// The text of a BigInt in `radix` (2 to 36). A text longer than the string cap is `Limit::StringLength`.
    pub fn bigint_to_string(&mut self, v: Value, radix: u32) -> R<StrId> {
        let mut buf = [0u64; 1];
        let a = self.bigint_sv(v, &mut buf)?;
        let digits = (bigint::bit_len(a.mag) as f64 / (radix as f64).log2()) as usize + 2;
        if digits > self.limits.max_string_units || string_cost(digits) > self.limits.max_bytes {
            return Err(HeapError::Limit(Limit::StringLength));
        }
        let text = match v {
            Value::BigInt(n) if radix == 10 => n.to_string(),
            _ => bigint::to_string_radix(a, radix),
        };
        self.intern_str(&text)
    }

    /// The decimal text of a BigInt, for tools and messages. Empty for anything else. It makes no heap entry.
    pub fn bigint_text(&self, v: Value) -> String {
        let mut buf = [0u64; 1];
        self.bigint_sv(v, &mut buf).map(|a| bigint::to_string_radix(a, 10)).unwrap_or_default()
    }

    /// Limbs of a BigInt value (an inline value has 0 or 1). 0 for anything else.
    pub fn bigint_limbs(&self, v: Value) -> usize {
        match v {
            Value::BigInt(n) => (n != 0) as usize,
            Value::BigHeap(id) => self.big(id).map_or(0, |b| b.limbs.len()),
            _ => 0,
        }
    }

    /// Bits of the magnitude of a BigInt value (0 for zero). 0 for anything else.
    pub fn bigint_bits(&self, v: Value) -> u64 {
        match v {
            Value::BigInt(n) => 64 - n.unsigned_abs().leading_zeros() as u64,
            Value::BigHeap(id) => self.big(id).map_or(0, |b| bigint::bit_len(&b.limbs)),
            _ => 0,
        }
    }

    /// The low 64 bits of the two's complement form of a BigInt (`BigInt.asUintN(64, v)`). 0 for anything else.
    pub fn bigint_low_u64(&self, v: Value) -> u64 {
        match v {
            Value::BigInt(n) => n as u64,
            Value::BigHeap(id) => self.big(id).map_or(0, |b| bigint::low_u64(b.sv())),
            _ => 0,
        }
    }

    /// Live BigInt entries (not the inline ones).
    pub fn live_bigints(&self) -> usize {
        self.bigs.live.len()
    }
}
