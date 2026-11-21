//! The iterator protocol of the loop (`docs/design-js2-async.md` 6.3, plan step G3). Cold helpers of `IterOpen`, `IterNext`, `IterClose` and
//! `YieldStarStep`; the loop of `interp.rs` keeps the array path (an index) inline.
//!
//! **The record** of an iteration is 3 registers: the iterator, the *kind* and a word that only a string loop uses (0).
//!
//! | Kind (register 1) | The iterator is | `IterNext` |
//! | --- | --- | --- |
//! | an integer | an array (or the array of the code points of a string) | reads the element at the index |
//! | `undefined` | a generator object | resumes it ([`Realm::gen_nested`]) |
//! | anything else | any object; the word is its `next` (`null` when it has none) | calls it |
//!
//! A generator and an array are *intrinsic*: the loop does not read `Symbol.iterator`, `next` or `done` for them. A script that changes
//! `%GeneratorPrototype%.next` or `Array.prototype[Symbol.iterator]` does not change a `for-of` over them (test262 has a few such tests).
use crate::generator::{MODE_NEXT, MODE_RETURN, MODE_THROW};
use crate::interp::STOP;
use crate::interp_rt::{Key, R, Realm};
use crate::value::Value;

/// What `YieldStarStep` found (written in the register after its result).
pub const YS_YIELD: i32 = 0;
pub const YS_DONE: i32 = 1;
pub const YS_RETURN: i32 = 2;

impl Realm {
    /// `GetIterator(v, sync)` for a value that is no array and no string: the iterator and its kind word (see the module comment).
    #[cold]
    pub(crate) fn iter_open_generic(&mut self, v: Value) -> R<(Value, Value)> {
        let m = if v.is_undefined() || v.is_null() { Value::UNDEFINED } else { self.get_prop(v, Key::Name(self.wk[crate::bi_symbol::ITERATOR]))? };
        if !Realm::obj(m).is_some_and(|o| o.is_callable()) {
            return self.type_error("the value is not iterable");
        }
        let it = self.call_function(m, v, &[])?;
        if Realm::obj(it).is_none() {
            return self.type_error("the result of Symbol.iterator is not an object");
        }
        if Realm::is_generator(it) {
            return Ok((it, Value::UNDEFINED));
        }
        let next = self.get_named(it, "next")?;
        Ok((it, if next.is_undefined() { Value::NULL } else { next }))
    }

    /// The step of a generic iterator: `next.call(it)`, then `done` and `value` of the result. `None` at the end.
    pub(crate) fn iter_step(&mut self, it: Value, next: Value) -> R<Option<Value>> {
        let r = self.call_function(next, it, &[])?;
        self.iter_unpack(r)
    }

    /// The step of a record that is no array: a generator or a generic iterator.
    #[inline]
    pub(crate) fn iter_step_any(&mut self, it: Value, kind: Value) -> R<Option<Value>> {
        if kind.is_undefined() {
            let (v, done) = self.gen_nested(it, MODE_NEXT, Value::UNDEFINED)?;
            return Ok((!done).then_some(v));
        }
        self.iter_step(it, kind)
    }

    fn iter_unpack(&mut self, r: Value) -> R<Option<Value>> {
        if Realm::obj(r).is_none() {
            return self.type_error("the result of an iterator is not an object");
        }
        let d = self.get_named(r, "done")?;
        if Realm::truthy(d) {
            return Ok(None);
        }
        self.get_named(r, "value").map(Some)
    }

    /// `IteratorClose`. For a throw (`throwing`) the original error wins: an error of `return` is dropped (but not the stop of a budget).
    pub(crate) fn iter_close(&mut self, it: Value, kind: Value, throwing: bool) -> R<()> {
        if kind.as_int().is_some() {
            return Ok(());
        }
        let r = self.iter_close_inner(it, kind);
        match r {
            Err(e) if throwing && e != STOP => Ok(()),
            r => r,
        }
    }

    fn iter_close_inner(&mut self, it: Value, kind: Value) -> R<()> {
        if kind.is_undefined() {
            return self.gen_nested(it, MODE_RETURN, Value::UNDEFINED).map(|_| ());
        }
        let ret = self.get_named(it, "return")?;
        if ret.is_undefined() || ret.is_null() {
            return Ok(());
        }
        let r = self.call_function(ret, it, &[])?;
        if Realm::obj(r).is_none() {
            return self.type_error("the result of return() is not an object");
        }
        Ok(())
    }

    /// `yield*` (the loop of 14.4.14, steps a to c) for a generator or a generic iterator (the loop does an array): send `v` in `mode` to the iterator of the record and give the answer: the value, and
    /// [`YS_YIELD`] (the iterator gave a value: yield it), [`YS_DONE`] (it finished: the value of the expression) or [`YS_RETURN`] (it
    /// finished after a `return(v)`: return the value). The value of an inner result is read here, not by the one who gets the result
    /// (a difference that only a getter of `value` sees).
    pub(crate) fn yield_star_step(&mut self, it: Value, kind: Value, mode: i32, v: Value) -> R<(Value, i32)> {
        let fin = |m: i32| if m == MODE_RETURN { YS_RETURN } else { YS_DONE };
        if kind.is_undefined() {
            let (r, done) = self.gen_nested(it, mode, v)?;
            return Ok(if done { (r, fin(mode)) } else { (r, YS_YIELD) });
        }
        let r = match mode {
            MODE_NEXT => self.call_function(kind, it, &[v])?,
            MODE_THROW => {
                let t = self.get_named(it, "throw")?;
                if t.is_undefined() || t.is_null() {
                    self.iter_close(it, kind, false)?;
                    return self.type_error("the iterator does not have a throw method");
                }
                self.call_function(t, it, &[v])?
            }
            _ => {
                let t = self.get_named(it, "return")?;
                if t.is_undefined() || t.is_null() {
                    return Ok((v, YS_RETURN));
                }
                self.call_function(t, it, &[v])?
            }
        };
        if Realm::obj(r).is_none() {
            return self.type_error("the result of an iterator is not an object");
        }
        let d = self.get_named(r, "done")?;
        let value = self.get_named(r, "value")?;
        Ok(if Realm::truthy(d) { (value, fin(mode)) } else { (value, YS_YIELD) })
    }
}
