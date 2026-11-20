//! A JavaScript value in 64 bits (NaN-boxing, `docs/design-engine-rewrite.md` 6.1).
//!
//! A double is stored as it is. Every other value has a tag in the top 16 bits, in the NaN space from `0xFFF9`:
//!
//! | Top 16 bits | Value | Low 48 bits |
//! | --- | --- | --- |
//! | below `0xFFF9` | a double | (part of the double) |
//! | `0xFFF9` | a 32-bit integer | the integer in the low 32 bits |
//! | `0xFFFA` | `undefined`, `null`, `false`, `true`, a hole | 0, 1, 2, 3, 4 |
//! | `0xFFFB` to `0xFFFE` | a string, symbol, BigInt or object | the address of the cell |
//!
//! So "is a double" is one unsigned compare. A NaN whose bits reach `0xFFF9` would look like a tagged value, so [`Value::number`]
//! replaces it with the canonical NaN. Hardware arithmetic never makes one: the x86 default NaN is `0xFFF8...`.
//!
//! Addresses: a cell address must fit 48 bits. The heap checks this for each block it gets from the OS (the address invariant);
//! here it is a debug check only. Values hold addresses as integers, through the exposed-provenance API, so this module needs no
//! `unsafe` code. Reading a cell through the address is the heap's job.
use std::ptr::NonNull;

const TAG_SHIFT: u32 = 48;
const PAYLOAD: u64 = (1 << TAG_SHIFT) - 1;
/// The first bit pattern that is not a double.
const FIRST_TAGGED: u64 = 0xFFF9 << TAG_SHIFT;
const TAG_INT: u64 = 0xFFF9;
const TAG_SPECIAL: u64 = 0xFFFA;
const TAG_STRING: u64 = 0xFFFB;
/// The NaN that stands for every NaN whose bits would reach the tag space.
pub const CANONICAL_NAN: u64 = 0x7FF8_0000_0000_0000;

/// The kind of cell that a pointer value points to. Its tag is `0xFFFB` plus the kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    String = 0,
    Symbol = 1,
    BigInt = 2,
    Object = 3,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Value(u64);

impl Value {
    pub const UNDEFINED: Value = Value(TAG_SPECIAL << TAG_SHIFT);
    pub const NULL: Value = Value(TAG_SPECIAL << TAG_SHIFT | 1);
    pub const FALSE: Value = Value(TAG_SPECIAL << TAG_SHIFT | 2);
    pub const TRUE: Value = Value(TAG_SPECIAL << TAG_SHIFT | 3);
    /// The value of an array element that does not exist (a hole). It is not a number or a pointer, and no script sees it.
    pub const HOLE: Value = Value(TAG_SPECIAL << TAG_SHIFT | 4);

    /// A double. A NaN that would reach the tag space becomes [`CANONICAL_NAN`].
    #[inline]
    pub fn number(f: f64) -> Value {
        let b = f.to_bits();
        Value(if b >= FIRST_TAGGED { CANONICAL_NAN } else { b })
    }

    #[inline]
    pub fn int(i: i32) -> Value {
        Value(TAG_INT << TAG_SHIFT | i as u32 as u64)
    }

    #[inline]
    pub fn bool(b: bool) -> Value {
        if b { Value::TRUE } else { Value::FALSE }
    }

    /// A pointer to a cell of kind `kind`. The address must fit 48 bits (see the module comment).
    #[inline]
    pub fn pointer(kind: Kind, p: NonNull<u8>) -> Value {
        let a = p.as_ptr().expose_provenance() as u64;
        debug_assert!(a <= PAYLOAD, "the address {a:#x} does not fit 48 bits");
        Value((TAG_STRING + kind as u64) << TAG_SHIFT | a)
    }

    /// The raw bits.
    #[inline]
    pub fn bits(self) -> u64 {
        self.0
    }

    #[inline]
    fn tag(self) -> u64 {
        self.0 >> TAG_SHIFT
    }

    /// A double or an integer.
    #[inline]
    pub fn is_number(self) -> bool {
        self.0 < FIRST_TAGGED || self.tag() == TAG_INT
    }

    /// The number of a double or an integer.
    #[inline]
    pub fn as_f64(self) -> Option<f64> {
        if self.0 < FIRST_TAGGED {
            Some(f64::from_bits(self.0))
        } else {
            self.as_int().map(f64::from)
        }
    }

    #[inline]
    pub fn as_int(self) -> Option<i32> {
        (self.tag() == TAG_INT).then_some(self.0 as u32 as i32)
    }

    #[inline]
    pub fn as_bool(self) -> Option<bool> {
        match self {
            Value::TRUE => Some(true),
            Value::FALSE => Some(false),
            _ => None,
        }
    }

    #[inline]
    pub fn is_undefined(self) -> bool {
        self == Value::UNDEFINED
    }

    #[inline]
    pub fn is_hole(self) -> bool {
        self == Value::HOLE
    }

    #[inline]
    pub fn is_null(self) -> bool {
        self == Value::NULL
    }

    /// True for a pointer value (a string, symbol, bigint or object); no address check.
    #[inline(always)]
    pub(crate) fn is_pointer(self) -> bool {
        self.tag().wrapping_sub(TAG_STRING) < 4
    }

    /// The kind and the address of a pointer value, with the provenance that [`Value::pointer`] exposed.
    #[inline]
    pub fn as_pointer(self) -> Option<(Kind, NonNull<u8>)> {
        let kind = match self.tag().wrapping_sub(TAG_STRING) {
            0 => Kind::String,
            1 => Kind::Symbol,
            2 => Kind::BigInt,
            3 => Kind::Object,
            _ => return None,
        };
        Some((kind, NonNull::new(std::ptr::with_exposed_provenance_mut((self.0 & PAYLOAD) as usize))?))
    }
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(i) = self.as_int() {
            write!(f, "Int({i})")
        } else if let Some(n) = self.as_f64() {
            write!(f, "Number({n})")
        } else if let Some((k, p)) = self.as_pointer() {
            write!(f, "{k:?}({p:p})")
        } else {
            match self.0 & PAYLOAD {
                0 => f.write_str("Undefined"),
                1 => f.write_str("Null"),
                2 => f.write_str("False"),
                3 => f.write_str("True"),
                _ => f.write_str("Hole"),
            }
        }
    }
}

#[cfg(test)]
#[path = "value_tests.rs"]
mod tests;
