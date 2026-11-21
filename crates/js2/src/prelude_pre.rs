//! The slots of the prelude (`docs/design-js2-lazy-builtins.md`, design D2).
//!
//! `prelude.js` is a flat text. It has no wrapper function and it never runs as a whole. A name that a prelude function reads and no scope
//! of that function declares is one of the names in [`PRE`]: the compiler reads it with `GetPre slot` (`compile_expr.rs`). The slots are in
//! `Realm::pre`. A slot is `undefined` until its first read; then [`Realm::pre_make`] makes its value and keeps it. A realm that never
//! calls a prelude function pays nothing for the text, for the helpers or for the natives that the helpers use.
//!
//! A slot is one of: a top-level function of `prelude.js` (a stub that the first call compiles), a native that is no property of any
//! object (the original `Object.keys`, whatever a script does to `Object`), or a state object that the realm builds.
//!
//! The public names (`Array.prototype.forEach`) are lazy table entries of kind `Script` (`lazy.rs`). They make a stub of the top-level
//! function of their name at the first lookup. [`Realm::pre_fn`] makes both.
//!
//! **The first seven slots are the helpers** that natives tail-call (`bi_object.rs`: `H_DEFINE` to `H_PAIRS`), in that order.
use crate::bi_symbol::TO_STRING_TAG;
use crate::builtins::{HIDDEN, NativeFn};
use crate::interp_rt::{Key, R, Realm};
use crate::object::Obj;
use crate::shapes::A_C;
use crate::value::Value;

/// What a slot holds.
pub(crate) enum Pd {
    /// The top-level function of `prelude.js` of this name.
    Func(&'static str),
    /// A native with this `length` and `name`.
    Native(NativeFn, u8, &'static str),
    /// `%ArrayIteratorPrototype%`.
    ArrayIter,
}

use Pd::{ArrayIter, Func, Native};

/// The names that a prelude function can read by `GetPre`, and what each is. The index is the slot. A name here that a script function of the
/// prelude declares itself (a parameter, a `var`) is that variable: the slots are only for names that no scope declares.
pub(crate) const PRE: &[(&str, Pd)] = &[
    ("helperDefineProperty", Func("helperDefineProperty")),
    ("helperDefineProperties", Func("helperDefineProperties")),
    ("helperAssign", Func("helperAssign")),
    ("helperSearch", Func("helperSearch")),
    ("helperSpread", Func("helperSpread")),
    ("helperRest", Func("helperRest")),
    ("helperPairs", Func("helperPairs")),
    ("toDesc", Func("toDesc")),
    ("copyData", Func("copyData")),
    ("need", Func("need")),
    ("obj", Func("obj")),
    ("len", Func("len")),
    ("int", Func("int")),
    ("check", Func("check")),
    ("bound", Func("bound")),
    ("defaultCompare", Func("defaultCompare")),
    ("mergeSort", Func("mergeSort")),
    ("sortList", Func("sortList")),
    ("flatten", Func("flatten")),
    ("arrayIterator", Func("arrayIterator")),
    ("arrayIteratorNext", Func("arrayIteratorNext")),
    ("arrayIteratorProto", ArrayIter),
    ("rawDefine", Native(crate::bi_object::object_define_property, 3, "defineProperty")),
    ("keys", Native(crate::bi_object::object_keys, 1, "keys")),
    ("getProto", Native(crate::bi_object::object_get_prototype_of, 1, "getPrototypeOf")),
    ("setProto", Native(crate::bi_object::object_set_prototype_of, 2, "setPrototypeOf")),
    ("getOwn", Native(crate::bi_object::object_own_descriptor, 2, "getOwnPropertyDescriptor")),
    ("ownNames", Native(crate::bi_object::object_own_names, 1, "getOwnPropertyNames")),
    ("set", Native(crate::bi_object::set_strict, 3, "set")),
    ("bind", Native(crate::builtins::bind_callback, 2, "bind")),
];

/// The number of slots.
pub(crate) const NPRE: usize = PRE.len();

/// The slot of the name `name`, if it is one.
pub(crate) fn slot(name: &str) -> Option<u32> {
    PRE.iter().position(|(n, _)| *n == name).map(|i| i as u32)
}

impl Realm {
    /// The function of the prelude that the top-level function `top` is, with the `name` for its `name` property: a stub that the first call
    /// compiles. Nothing is parsed or compiled here.
    pub(crate) fn pre_fn(&mut self, top: &str, name: &str) -> R<Value> {
        let Some(l) = self.lazy.as_mut() else { return Err(self.error(crate::interp_rt::ErrKind::Plain, "a realm with no compiler")) };
        let ci = match l.prelude_fn(&self.prog, top, name) {
            Ok(ci) => ci,
            Err(m) => return Err(self.error(crate::interp_rt::ErrKind::Plain, &m)),
        };
        self.fn_info.extend((self.fn_info.len()..self.prog.len()).map(|i| crate::interp::code_info(&self.prog[i])));
        Ok(self.new_function(ci, Value::UNDEFINED))
    }

    /// Make the value of slot `i` and keep it (the first read of the slot).
    #[cold]
    pub(crate) fn pre_make(&mut self, i: usize) -> R<Value> {
        let v = match &PRE[i].1 {
            Func(top) => self.pre_fn(top, top)?,
            Native(f, len, name) => self.new_native(*f, false, *len, name).value(),
            ArrayIter => self.array_iterator_proto()?,
        };
        self.pre[i] = v;
        self.keep(v);
        Ok(v)
    }

    /// `%ArrayIteratorPrototype%`: an object with `next` and the tag `Array Iterator`, below `%IteratorPrototype%`.
    fn array_iterator_proto(&mut self) -> R<Value> {
        let ip = Realm::obj(self.iterator_prototype()).expect("an object");
        let proto = Obj::new(&mut self.heap, &self.shapes, self.s_object).expect("memory");
        proto.set_proto(&mut self.shapes, Some(ip.cell()));
        // A root while the next function is made.
        self.keep(proto.value());
        let next = self.pre_fn("arrayIteratorNext", "next")?;
        let k = self.intern("next");
        self.define_attrs(proto, Key::Name(k), HIDDEN, next)?;
        let tag = self.string("Array Iterator");
        self.define_attrs(proto, Key::Name(self.wk[TO_STRING_TAG]), A_C, tag)?;
        Ok(proto.value())
    }
}

#[cfg(test)]
#[path = "prelude_pre_tests.rs"]
mod tests;
