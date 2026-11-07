//! Inline caches for property access. A submodule of `heap.rs`.
//!
//! This file is the API of the cache and nothing more. No cache is filled yet: `ic_own` never hits, and `get_ic` and `set_ic`
//! run the plain `get` and `set`. The evaluator and the bytecode interpreter can call them now. Shapes (step A1) and the real
//! cache (step A2) come later and change only this file and `heap.rs`. The contract is in `docs/decisions.md`
//! ("Shapes and inline caches") and the plan is in `docs/engine-plan.md`.
//!
//! An `Ic` is plain `Copy` data with no pointer. In JS terms it is a small note that a call site keeps: "last time `o.x` had this
//! shape, the value was in this slot". A handle in it that has died fails the generation check of every handle read.
use super::*;

/// One inline cache entry. 24 bytes. A site that has no entry holds `Ic::EMPTY`.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // the fields are written and read from step A2
pub struct Ic {
    /// The shape of the receiver that filled the entry.
    shape: u32,
    /// The slot of the property in the receiver (own hit) or in the holder (prototype hit).
    slot: u32,
    /// `IC_EMPTY`, `IC_OWN`, `IC_PROTO` or `IC_ABSENT`.
    kind: u8,
    /// The object that has the property, for a prototype hit.
    holder: ObjId,
    /// `Heap::proto_epoch` when the entry was filled, for a prototype hit and an absent hit.
    epoch: u32,
}

#[allow(dead_code)] // used from step A2
const IC_EMPTY: u8 = 0;

impl Ic {
    /// The entry of a site that has not run, or that missed. It never hits.
    pub const EMPTY: Ic = Ic { shape: 0, slot: 0, kind: IC_EMPTY, holder: ObjId { index: 0, gn: 0 }, epoch: 0 };
}

impl Heap {
    /// The value of an own data property of `o` when `ic` hits, else `None`. The caller runs `get_ic` on `None`. Always `None` now.
    #[inline(always)]
    pub fn ic_own(&self, _o: ObjId, _ic: &Ic) -> Option<Value> {
        None
    }

    /// `get`, and it fills `ic` for the next read. Out of line: it runs only on a miss. `ic` is not filled yet.
    #[cold]
    pub fn get_ic(&mut self, o: ObjId, key: PropertyKey, _ic: &mut Ic) -> R<Get> {
        self.get(o, key)
    }

    /// `set`, and it fills `ic` for the next write. `ic` is not filled yet.
    #[cold]
    pub fn set_ic(&mut self, o: ObjId, key: PropertyKey, value: Value, _ic: &mut Ic) -> R<Set> {
        self.set(o, key, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stub_never_hits_and_runs_the_plain_path() {
        assert_eq!(size_of::<Ic>(), 24);
        let mut h = Heap::new(HeapLimits::default()).unwrap();
        let o = h.new_object(None).unwrap();
        let k = PropertyKey::String(h.intern_str("x").unwrap());
        let mut ic = Ic::EMPTY;
        assert!(h.ic_own(o, &ic).is_none());
        assert_eq!(h.set_ic(o, k, Value::Number(7.0), &mut ic), Ok(Set::Done));
        assert!(h.ic_own(o, &ic).is_none());
        assert!(matches!(h.get_ic(o, k, &mut ic), Ok(Get::Value(Value::Number(n))) if n == 7.0));
        let missing = PropertyKey::String(h.intern_str("y").unwrap());
        assert!(matches!(h.get_ic(o, missing, &mut ic), Ok(Get::Value(Value::Undefined))));
    }
}
