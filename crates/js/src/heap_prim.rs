//! Primitive wrapper objects (`new String`, `new Number`, `new Boolean`, `Object(symbol)`): the objects with an internal slot
//! `[[StringData]]`, `[[NumberData]]`, `[[BooleanData]]` or `[[SymbolData]]`. A child module of `heap.rs`.
//!
//! Layout: an exotic object (`Exotic::Prim(value)`) with the primitive in `Heap::exotics`, as for a buffer. An ordinary object
//! pays nothing: no field, no pointer, no check on its fast paths. A wrapper is one `Object` (80 bytes), its slot of 8 bytes
//! and one entry of the exotic table. `new Number(1)` does not allocate a string, an element vector or a property.
//!
//! A String wrapper is also a string exotic object (section 10.4.3): its flag is `EXOTIC_STR`, so only it pays for the
//! extra rules. Its own properties are virtual, as the elements of a typed array are: `length` and one index for each code unit
//! below the length. All are non-writable and non-configurable, and the indices are enumerable. A key list gives the
//! indices, then the other indices, then `length`, then the other strings and the symbols (section 10.4.3.3). A define that
//! changes one of them is rejected (`IsCompatiblePropertyDescriptor`). A write and a delete are rejected by the ordinary rules,
//! because the stored property is non-writable and non-configurable. A wrapper of another type has no virtual property.
//!
//! The value of an index is a one-unit string. `exotic_own` takes `&self` and cannot intern, so it gives a placeholder and
//! `realize` (the same hook as for host named objects) makes the string. Only a hit pays for it.
//!
//! The collector marks the string or the symbol that a wrapper holds (`Heap::collect`).

use super::binary::Exotic;
use super::*;

impl Heap {
    /// A wrapper object of the string, number, boolean or symbol `v`. `InvalidKey` for any other value.
    pub fn new_primitive(&mut self, proto: Option<ObjId>, v: Value) -> R<ObjId> {
        if matches!(v, Value::Undefined | Value::Null | Value::Object(_)) {
            return Err(HeapError::InvalidKey);
        }
        self.check_value(v)?;
        let id = self.make_exotic(proto, || Exotic::Prim(v))?;
        if matches!(v, Value::String(_)) {
            self.objects.get_mut(id.index, id.gn).expect("fresh object").exotic = EXOTIC_STR;
        }
        Ok(id)
    }

    /// The primitive that wrapper `o` holds. `None` for any other object.
    pub fn primitive_value(&self, o: ObjId) -> Option<Value> {
        match self.exotic(o)? {
            Exotic::Prim(v) => Some(*v),
            _ => None,
        }
    }

    /// `[[GetOwnProperty]]` of a key on a String wrapper that holds `s`: `Some(property)` for `length` and an index below it
    /// (the value of an index is a placeholder, see `realize`), `None` for the ordinary rules.
    pub(super) fn prim_str_own(&self, s: StrId, key: PropertyKey) -> R<Option<Option<Property>>> {
        let len = self.str_len(s)?;
        Ok(match key {
            PropertyKey::Index(i) if (i as usize) < len => Some(Some(Property::Data { value: Value::Undefined, writable: false, enumerable: true, configurable: false })),
            PropertyKey::String(k) if k.same(self.length) => Some(Some(Property::Data { value: Value::Number(len as f64), writable: false, enumerable: false, configurable: false })),
            _ => None,
        })
    }

    /// The real value of an index of a String wrapper (`realize` calls it).
    pub(super) fn prim_realize(&mut self, o: ObjId, key: PropertyKey, r: Option<Property>) -> R<Option<Property>> {
        let (Some(Exotic::Prim(Value::String(s))), PropertyKey::Index(i), Some(Property::Data { writable, enumerable, configurable, .. })) = (self.exotic(o), key, r) else { return Ok(r) };
        let s = *s;
        let value = Value::String(self.substring(s, i as usize, i as usize + 1)?);
        Ok(Some(Property::Data { value, writable, enumerable, configurable }))
    }

    /// `[[DefineOwnProperty]]` on a String wrapper: a key that it has is checked against its stored property and never changed.
    /// `None`: the ordinary rules.
    pub(super) fn prim_define(&mut self, o: ObjId, key: PropertyKey, d: &PropertyDescriptor) -> R<Option<bool>> {
        let Some(Exotic::Prim(Value::String(s))) = self.exotic(o) else { return Ok(None) };
        let Some(cur) = self.prim_str_own(*s, key)?.flatten() else { return Ok(None) };
        let cur = self.prim_realize(o, key, Some(cur))?;
        Ok(Some(validate_apply(false, cur, d, |a, b| self.str_eq(a, b)).is_some()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapper_holds_its_primitive_and_a_string_wrapper_has_virtual_keys() {
        let mut h = Heap::default();
        let n = h.new_primitive(None, Value::Number(5.0)).unwrap();
        assert!(matches!(h.primitive_value(n), Some(Value::Number(x)) if x == 5.0));
        assert_eq!(h.own_keys(n).unwrap(), vec![]);
        assert_eq!(h.new_primitive(None, Value::Null), Err(HeapError::InvalidKey));
        let s = h.intern_str("ab").unwrap();
        let w = h.new_primitive(None, Value::String(s)).unwrap();
        let length = PropertyKey::String(h.length);
        assert_eq!(h.own_keys(w).unwrap(), vec![PropertyKey::Index(0), PropertyKey::Index(1), length]);
        let Some(Property::Data { value: Value::String(c), writable: false, enumerable: true, configurable: false }) = h.get_own_property(w, PropertyKey::Index(1)).unwrap() else { panic!("index 1") };
        assert_eq!(h.str_units(c).unwrap(), &[b'b' as u16]);
        assert_eq!(h.get_own_property(w, PropertyKey::Index(2)).unwrap().is_none(), true);
        // a define that changes a virtual property is rejected, one that keeps it is accepted
        let same = PropertyDescriptor { value: Some(Value::String(c)), ..Default::default() };
        assert_eq!(h.define_own_property(w, PropertyKey::Index(1), &same), Ok(true));
        assert_eq!(h.define_own_property(w, PropertyKey::Index(1), &PropertyDescriptor::data(Value::Number(1.0))), Ok(false));
        assert_eq!(h.set(w, PropertyKey::Index(0), Value::Number(1.0)), Ok(Set::Rejected));
        assert_eq!(h.delete(w, length), Ok(false));
    }

    #[test]
    fn collect_keeps_the_string_and_the_symbol_of_a_wrapper() {
        let mut h = Heap::default();
        let s = h.intern_str("kept").unwrap();
        let w = h.new_primitive(None, Value::String(s)).unwrap();
        let d = h.intern_str("desc").unwrap();
        let sym = h.new_symbol(Some(d)).unwrap();
        let ws = h.new_primitive(None, Value::Symbol(sym)).unwrap();
        h.collect(&[Value::Object(w), Value::Object(ws)]).unwrap();
        assert_eq!(h.str_units(s).unwrap().len(), 4);
        assert_eq!(h.symbol_description(sym).unwrap(), Some(d));
        let c = h.collect(&[]).unwrap();
        assert_eq!((c.objects, c.symbols), (2, 1));
        assert!(!h.has_exotics());
    }
}
