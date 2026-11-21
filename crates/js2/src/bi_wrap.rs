//! Wrapper objects: `new String(x)`, `new Number(x)`, `new Boolean(x)` and `Object(x)` for a primitive (plan step S2.7 follow-up).
//!
//! A wrapper is a plain `Object` cell with the flag `F_WRAP` and one hidden property, `Realm::k_prim`, that holds the primitive (the
//! `[[PrimitiveValue]]` slot). Its prototype is `String.prototype` and the others, or the `prototype` of a class that extends them. The
//! flag, not the shape, tells a wrapper from another object: a property that a script adds changes the shape, and the hidden
//! property keeps its name in every shape (like the slots of a native function, `builtins.rs`). The value is found by that name, so a
//! script that deletes or redefines the property (`"\0prim"` is a string that a script can build) makes the object an ordinary one,
//! and no code reads a wrong slot.
//!
//! A `String` wrapper also has the own properties `length` and one for each unit (`new String("ab")[1]`): they are not in the shape.
//! [`Realm::string_wrapper`] gives the string, and `get_prop_g`, `own_keys` and the `Object` built-ins ask it. Nothing about a
//! primitive changes: a method of a string, a number or a boolean still runs with the primitive as `this` and no object is made.
use crate::cell::Layout;
use crate::interp_rt::{Key, R, Realm};
use crate::object::{F_WRAP, Obj};
use crate::string::Str;
use crate::value::Value;

impl Realm {
    /// A wrapper object for the primitive `prim` with prototype `proto`.
    pub(crate) fn wrap(&mut self, proto: Obj, prim: Value) -> Obj {
        let root = self.shapes.root(Layout::Object, Some(proto.cell()), 4);
        let shape = self.shapes.add(root, self.k_prim, 0).expect("shape");
        let o = Obj::new(&mut self.heap, &self.shapes, shape).expect("memory");
        o.set_slot(&self.shapes, 0, prim);
        o.set_flag(F_WRAP);
        o
    }

    /// The primitive of a wrapper object, or `None` for any other value.
    pub(crate) fn prim_of(&self, v: Value) -> Option<Value> {
        let o = Realm::obj(v).filter(|o| o.has_flag(F_WRAP))?;
        let e = self.shapes.find(o.shape(&self.shapes), self.k_prim)?;
        Some(o.slot(&self.shapes, e.slot()))
    }

    /// The string of a `String` wrapper object.
    pub(crate) fn string_wrapper(&self, o: Obj) -> Option<Str> {
        if !o.has_flag(F_WRAP) {
            return None;
        }
        self.prim_of(o.value()).and_then(Realm::str_of)
    }

    /// The own property `key` of a `String` wrapper that is not in its shape: `length`, or the unit at an index below it.
    pub(crate) fn string_own(&mut self, s: Str, key: Key) -> Option<Value> {
        let s = self.flat(s);
        match key {
            Key::Name(k) if k == self.k_length => Some(Realm::num(s.len() as f64)),
            Key::Idx(i) => {
                let u = s.unit_at(i as usize)?;
                Some(self.make_units(&[u]))
            }
            Key::Name(_) => None,
        }
    }

    /// `ToObject`: an object is itself, a string, a number or a boolean gets a wrapper, `undefined` and `null` throw.
    pub fn to_object(&mut self, v: Value) -> R<Obj> {
        if let Some(o) = Realm::obj(v) {
            return Ok(o);
        }
        let proto = if Realm::str_of(v).is_some() {
            self.string_proto
        } else if v.is_number() {
            self.number_proto
        } else if v.as_bool().is_some() {
            self.boolean_proto
        } else if Realm::sym_of(v).is_some() {
            self.fn_props(self.symbol_proto);
            self.symbol_proto
        } else {
            return self.type_error("cannot convert undefined or null to an object");
        };
        Ok(self.wrap(proto, v))
    }
}
