//! `String.prototype[Symbol.iterator]` and `%StringIteratorPrototype%` (ECMA-262 22.1.5). The method is a lazy table entry that is keyed
//! by a well-known symbol (`lazy.rs`, `Kind::Sym`), so a realm that never asks for it pays nothing. The prototype and the shape of its
//! iterators are made at the first call. A string iterator is an object of kind [`K_SITER_STR`] (the `F_COLL` bits that `bi_collections.rs`
//! uses for `Map` and `Set`) with two hidden slots: the string (`undefined` once the iterator is done) and the position in units.
//! The loop of `for-of`, a spread and `Array.from` read a string primitive directly (`iter.rs`) and do not make an iterator.
use crate::bi_symbol::TO_STRING_TAG;
use crate::interp_rt::{Key, R, Realm};
use crate::cell::Layout;
use crate::object::{COLL_SHIFT, F_COLL, Obj};
use crate::shapes::{A_C, ShapeId};
use crate::value::Value;

/// The kind of a string iterator in the `F_COLL` bits (1 to 6 are the kinds of `bi_collections.rs`).
const K_SITER_STR: u32 = 7;

/// The prototype of the string iterators and their shape, made at the first call of the method.
pub(crate) struct StrIter {
    proto: Obj,
    shape: ShapeId,
}

impl Realm {
    /// `%StringIteratorPrototype%` and the shape of the iterators, made once.
    fn str_iter(&mut self) -> (Obj, ShapeId) {
        if let Some(s) = &self.ltab.str_iter {
            return (s.proto, s.shape);
        }
        let ip = Realm::obj(self.iterator_prototype()).expect("an object");
        let proto = Obj::new(&mut self.heap, &self.shapes, self.s_object).expect("memory");
        proto.set_proto(&mut self.shapes, Some(ip.cell()));
        self.method(proto, "next", 0, string_iterator_next);
        let tag = self.string("String Iterator");
        self.define_attrs(proto, Key::Name(self.wk[TO_STRING_TAG]), A_C, tag).expect("define");
        let mut shape = self.shapes.root(Layout::Object, Some(proto.cell()), 2);
        for n in ["\0ss", "\0sp"] {
            let k = self.intern(n);
            self.keep(k.value());
            shape = self.shapes.add(shape, k, 0).expect("shape");
        }
        self.keep(proto.value());
        self.pins.push(shape);
        self.ltab.str_iter = Some(StrIter { proto, shape });
        (proto, shape)
    }
}

/// `String.prototype[Symbol.iterator]`: `RequireObjectCoercible(this)`, `ToString`, then an iterator at position 0.
pub(crate) fn string_iterator(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    if this.is_undefined() || this.is_null() {
        return rt.type_error("String.prototype[Symbol.iterator] called on null or undefined");
    }
    let s = rt.to_str(this)?;
    let (_, shape) = rt.str_iter();
    let o = Obj::new(&mut rt.heap, &rt.shapes, shape).expect("memory");
    o.set_flag(K_SITER_STR << COLL_SHIFT);
    o.set_slot(&rt.shapes, 0, s.value());
    o.set_slot(&rt.shapes, 1, Value::int(0));
    Ok(o.value())
}

/// `%StringIteratorPrototype%.next`: the next code point (a lone surrogate is one), or `{ value: undefined, done: true }` for good.
fn string_iterator_next(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let Some(o) = Realm::obj(this).filter(|o| o.flags() & F_COLL == K_SITER_STR << COLL_SHIFT) else {
        return rt.type_error("String Iterator.prototype.next called on incompatible receiver");
    };
    let Some(s) = Realm::str_of(o.slot(&rt.shapes, 0)) else {
        return Ok(rt.iter_result(Value::UNDEFINED, true));
    };
    let s = rt.flat(s);
    let i = o.slot(&rt.shapes, 1).as_int().expect("a position") as usize;
    let Some(u) = s.unit_at(i) else {
        o.set_slot(&rt.shapes, 0, Value::UNDEFINED);
        return Ok(rt.iter_result(Value::UNDEFINED, true));
    };
    let lo = s.unit_at(i + 1).filter(|l| (0xD800..0xDC00).contains(&u) && (0xDC00..0xE000).contains(l));
    o.set_slot(&rt.shapes, 1, Value::int((i + 1 + lo.is_some() as usize) as i32));
    let c = match lo {
        Some(l) => rt.make_units(&[u, l]),
        None => rt.make_units(&[u]),
    };
    Ok(rt.iter_result(c, false))
}

#[cfg(test)]
#[path = "bi_string_iter_tests.rs"]
mod tests;
