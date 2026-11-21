//! The iterator protocol of the loop (`docs/design-js2-async.md` 6.3, plan step G3). Cold helpers of `IterOpen`, `IterNext`, `IterClose` and
//! `YieldStarStep`; the loop of `interp.rs` keeps the array path (an index) inline.
//!
//! **The record** of an iteration is 3 registers: the iterator, the *kind* and a flag for early close of a fast array.
//!
//! | Kind (register 1) | The iterator is | `IterNext` |
//! | --- | --- | --- |
//! | an integer | an array with the default iterator (or the array of the code points of a string) | reads the element at the index |
//! | `undefined` | a generator object | resumes it ([`Realm::gen_nested`]) |
//! | anything else | any object; the word is its `next` (`null` when it has none) | calls it |
//!
//! The loop skips the generator protocol on an intrinsic generator. It uses the array path only when the iterator and its `next` are
//! the standard methods. If an array loop ends early, it calls a `return` method added to the iterator prototype.
use crate::generator::{MODE_NEXT, MODE_RETURN, MODE_THROW};
use crate::interp::STOP;
use crate::bi_object::Own;
use crate::interp_rt::{Key, R, Realm};
use crate::object::Obj;
use crate::shapes::A_ACC;
use crate::value::Value;

/// What `YieldStarStep` found (written in the register after its result).
pub const YS_YIELD: i32 = 0;
pub const YS_DONE: i32 = 1;
pub const YS_RETURN: i32 = 2;

impl Realm {
    /// An array can use indexed steps only if ordinary lookup uses the original iterator methods.
    #[inline]
    pub(crate) fn plain_array_iterator(&mut self, o: Obj) -> bool {
        if o.shape(&self.shapes) != self.s_array {
            return false;
        }
        let ap = self.array_proto;
        if let Some(spot) = self.ltab.array_iter_spot
            && let Some(method) = ap.read_spot(spot)
        {
            if method != self.args_iter {
                return false;
            }
        } else if !self.cache_array_iterator() {
            return false;
        }
        if let Some((proto, key, next)) = self.ltab.array_iter_next {
            matches!(self.get_own(proto, Key::Name(key)), Some(Own::Data(f, _)) if f == next)
        } else {
            true
        }
    }

    #[cold]
    fn cache_array_iterator(&mut self) -> bool {
        if self.lazy.is_none() {
            return true;
        }
        if self.args_iter.is_undefined() {
            self.args_iter = self.array_values();
        }
        let (ap, key) = (self.array_proto, self.wk[crate::bi_symbol::ITERATOR]);
        self.lazy_prep(ap, key);
        let Some(e) = self.shapes.find(ap.shape(&self.shapes), key).filter(|e| e.attrs() & A_ACC == 0) else { return false };
        self.ltab.array_iter_spot = ap.spot(&self.shapes, e.slot());
        ap.slot(&self.shapes, e.slot()) == self.args_iter
    }

    /// Make the iterator at the current index if an early exit might call its `return`.
    pub(crate) fn close_array_iterator(&mut self, array: Value, index: u32, throwing: bool) -> R<()> {
        let proto = self.ltab.array_iter_next.map_or(self.api.iter_proto, |(p, _, _)| p.value());
        if proto.is_undefined() || self.args_iter.is_undefined() {
            return Ok(());
        }
        let ret = self.intern("return");
        let result = (|| {
            if self.has_prop(proto, Key::Name(ret))? {
                let it = self.call_function(self.args_iter, array, &[])?;
                let pos = self.intern("\0i");
                self.set_prop(it, Key::Name(pos), Realm::num(index as f64))?;
                self.iter_close_inner(it, Value::NULL)?;
            }
            Ok(())
        })();
        match result {
            Err(e) if throwing && e != STOP => Ok(()),
            result => result,
        }
    }

    /// `GetIterator(v, sync)`: the iterator and its `next` method.
    #[cold]
    pub(crate) fn iter_open_generic(&mut self, v: Value) -> R<(Value, Value)> {
        let m = if v.is_undefined() || v.is_null() { Value::UNDEFINED } else { self.get_prop(v, Key::Name(self.wk[crate::bi_symbol::ITERATOR]))? };
        if !Realm::obj(m).is_some_and(|o| o.is_callable()) {
            return self.type_error("the value is not iterable");
        }
        self.iter_open_method(v, m)
    }

    pub(crate) fn iter_open_method(&mut self, v: Value, method: Value) -> R<(Value, Value)> {
        let it = self.call_function(method, v, &[])?;
        if Realm::obj(it).is_none() {
            return self.type_error("the result of Symbol.iterator is not an object");
        }
        let next = self.get_named(it, "next")?;
        Ok((it, if Realm::obj(next).is_some_and(|o| o.is_callable()) { next } else { Value::NULL }))
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

    /// Step a delegated iterator. On suspension the original iterator result is the value returned to the caller.
    pub(crate) fn yield_star_step(&mut self, it: Value, kind: Value, mode: i32, v: Value) -> R<(Value, i32)> {
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
        if Realm::truthy(d) {
            let end = if mode == MODE_RETURN { YS_RETURN } else { YS_DONE };
            return Ok((self.get_named(r, "value")?, end));
        }
        Ok((r, YS_YIELD))
    }
}
