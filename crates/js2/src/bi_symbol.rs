//! `Symbol` (a step before the iterator protocol and the promise combinators, `docs/design-js2-async.md` decision 2).
//!
//! **A symbol** is a cell of the layout of a flat string with the flag `FLAG_SYMBOL` (`string.rs`). The units of the cell are the
//! description; `FLAG_NO_DESC` marks `Symbol()` (its `description` is `undefined`). A value of kind `Kind::Symbol` points to it. The
//! cell has no edge, so the collector marks it as a leaf string, and nothing else in the heap changes.
//!
//! **A symbol as a property key.** A key of a shape is the address of a cell, and atoms are the only strings that are keys. A symbol
//! is a different cell from every atom, so it cannot collide with a string key, and a key of the kind `Key::Name(s)` with
//! `s.is_symbol()` needs no new key type, no new shape code and no new lookup. The places that list keys (`own_key_list`, the
//! enumeration of `for-in` and `Object.keys`) skip a symbol, and `Realm::own_symbol_list` lists only symbols.
//!
//! **The members are lazy.** `Realm::new` makes the well-known symbols and the global `Symbol` (one native cell and one property).
//! The properties of `Symbol` and of `Symbol.prototype` are made at the first read of either, as the flag `F_LAZY` is for the `length`
//! and `name` of a function (`Realm::fn_props`). So a script that never names a symbol pays for none of them.
//!
//! **Wired:** `typeof`, `Symbol()`, `Symbol.for`, `Symbol.keyFor`, `toString`, `valueOf`, `description`, `Symbol.prototype[Symbol.toPrimitive]`
//! and `[Symbol.toStringTag]`, the symbols as keys of the property operations and of the object literal and class, `ToPrimitive`
//! (`Symbol.toPrimitive`), `instanceof` (`Symbol.hasInstance`), `Object.prototype.toString` (`Symbol.toStringTag`),
//! `Object.getOwnPropertySymbols`, `Object.assign` and the spread of an object (symbols are copied), `Object.getOwnPropertyDescriptors`,
//! `Object.defineProperties`, and the symbols for the iterator and RegExp protocols.
use crate::builtins::{HIDDEN, NativeFn, arg};
use crate::heap::Heap;
use crate::interp_rt::{Key, R, Realm};
use crate::object::{F_LAZY, Obj};
use crate::shapes::A_C;
use crate::string::{Chars, FLAG_REGISTERED, Str};
use crate::value::Value;

/// The number of well-known symbols that `Symbol` has, and their places in `Realm::wk`.
pub(crate) const WK: usize = 12;
#[allow(dead_code)] // `iterator`, `asyncIterator` and `species` are read by steps G3 and P3.
pub(crate) const ITERATOR: usize = 0;
#[allow(dead_code)]
pub(crate) const ASYNC_ITERATOR: usize = 1;
#[allow(dead_code)]
pub(crate) const SPECIES: usize = 2;
pub(crate) const TO_STRING_TAG: usize = 3;
pub(crate) const HAS_INSTANCE: usize = 4;
pub(crate) const TO_PRIMITIVE: usize = 5;
pub(crate) const MATCH: usize = 6;
pub(crate) const MATCH_ALL: usize = 7;
pub(crate) const REPLACE: usize = 8;
pub(crate) const SEARCH: usize = 9;
pub(crate) const SPLIT: usize = 10;
pub(crate) const UNSCOPABLES: usize = 11;
/// The descriptions of the well-known symbols: `"Symbol."` and the name of the property of `Symbol`.
const WK_DESC: [&str; WK] = ["Symbol.iterator", "Symbol.asyncIterator", "Symbol.species", "Symbol.toStringTag", "Symbol.hasInstance", "Symbol.toPrimitive",
    "Symbol.match", "Symbol.matchAll", "Symbol.replace", "Symbol.search", "Symbol.split", "Symbol.unscopables"];
/// The name of the native `Function.prototype[Symbol.hasInstance]`: `Realm::instance_of` skips the call for it.
pub(crate) const HAS_INSTANCE_NAME: &str = "[Symbol.hasInstance]";

/// The well-known symbols of a new realm.
pub(crate) fn well_known(heap: &mut Heap) -> [Str; WK] {
    WK_DESC.map(|d| Str::new_symbol(heap, Some(Chars::One(d.as_bytes()))).expect("memory"))
}

impl Realm {
    /// `Symbol(description)` for the text of a symbol: `Symbol.prototype.toString` and the message of an error.
    pub fn symbol_text(&self, s: Str) -> String {
        format!("Symbol({})", self.text(s))
    }

    pub(crate) fn install_symbol(&mut self) {
        let c = self.new_native(symbol_ctor, true, 0, "Symbol");
        let g = self.global;
        self.hidden(g, "Symbol", c.value());
        // The root keeps `Symbol` alive if a script deletes the global. The members come at the first read (`symbol_members`).
        self.keep(c.value());
        c.set_flag(F_LAZY);
        self.symbol_proto.set_flag(F_LAZY);
        self.symbol_ctor = Some(c);
    }

    /// Make the properties of `Symbol` and of `Symbol.prototype`. `fn_props` calls it at the first read of either object.
    #[cold]
    pub(crate) fn symbol_members(&mut self) {
        let (c, p) = (self.symbol_ctor.expect("Symbol is installed"), self.symbol_proto);
        c.clear_flag(F_LAZY);
        p.clear_flag(F_LAZY);
        self.define_attrs(c, Key::Name(self.k_prototype), 0, p.value()).expect("define");
        self.define_attrs(p, Key::Name(self.k_constructor), HIDDEN, c.value()).expect("define");
        for (n, len, f) in [("for", 1, symbol_for as NativeFn), ("keyFor", 1, symbol_key_for)] {
            self.method(c, n, len, f);
        }
        for (i, d) in WK_DESC.iter().enumerate() {
            let k = self.intern(&d["Symbol.".len()..]);
            self.define_attrs(c, Key::Name(k), 0, self.wk[i].key_value()).expect("define");
        }
        self.method(p, "toString", 0, symbol_to_string);
        self.method(p, "valueOf", 0, symbol_value_of);
        let getter = self.new_native(symbol_description, false, 0, "get description");
        let k = self.intern("description");
        self.define_accessor(p, Key::Name(k), getter.value(), false, false).expect("define");
        let f = self.new_native(symbol_value_of, false, 1, "[Symbol.toPrimitive]");
        self.define_attrs(p, Key::Name(self.wk[TO_PRIMITIVE]), A_C, f.value()).expect("define");
        let tag = self.string("Symbol");
        self.define_attrs(p, Key::Name(self.wk[TO_STRING_TAG]), A_C, tag).expect("define");
    }

    /// `Function.prototype[Symbol.hasInstance]`.
    pub(crate) fn install_has_instance(&mut self) {
        let f = self.new_native(function_has_instance, false, 1, HAS_INSTANCE_NAME);
        let p = self.function_proto;
        self.define_attrs(p, Key::Name(self.wk[HAS_INSTANCE]), 0, f.value()).expect("define");
    }

    /// The symbol of `this`: a symbol, or a `Symbol` wrapper object.
    fn this_symbol(&mut self, this: Value, what: &str) -> R<Str> {
        match Realm::sym_of(this).or_else(|| self.prim_of(this).and_then(Realm::sym_of)) {
            Some(s) => Ok(s),
            None => self.type_error(&format!("Symbol.prototype.{what} requires that 'this' be a Symbol")),
        }
    }

    /// `SymbolDescriptiveString`: `Symbol(description)` as a string value.
    pub(crate) fn symbol_string(&mut self, s: Str) -> Value {
        let units: Vec<u16> = "Symbol(".encode_utf16().chain(s.chars().expect("a symbol is flat").units()).chain(")".encode_utf16()).collect();
        Str::new(&mut self.heap, Chars::Two(&units)).expect("memory").value()
    }

    /// The description of a symbol as a string value: a copy of its units, or `undefined`.
    pub(crate) fn symbol_description_value(&mut self, s: Str) -> Value {
        if s.symbol_has_no_desc() {
            return Value::UNDEFINED;
        }
        Str::new(&mut self.heap, s.chars().expect("a symbol is flat")).expect("memory").value()
    }
}

fn symbol_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    if Realm::obj(rt.nt).is_some() {
        return rt.type_error("Symbol is not a constructor");
    }
    let d = arg(args, 0);
    let s = if d.is_undefined() {
        Str::new_symbol(&mut rt.heap, None)
    } else {
        let t = rt.to_str(d)?;
        let t = rt.flat(t);
        Str::new_symbol(&mut rt.heap, t.chars())
    };
    Ok(s.expect("memory").key_value())
}

/// `Symbol.for(key)`: the registry is an object that maps the text of a key to its symbol.
fn symbol_for(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let t = rt.to_str(arg(args, 0))?;
    let t = rt.flat(t);
    let k = rt.atoms.atomize(&mut rt.heap, t).expect("memory");
    let reg = match rt.registry {
        Some(r) => r,
        None => {
            let shape = rt.shapes.root(crate::cell::Layout::Object, None, 4);
            let r = Obj::new(&mut rt.heap, &rt.shapes, shape).expect("memory");
            rt.keep(r.value());
            rt.registry = Some(r);
            r
        }
    };
    if let Some(e) = rt.shapes.find(reg.shape(&rt.shapes), k) {
        return Ok(reg.slot(&rt.shapes, e.slot()));
    }
    let s = Str::new_symbol(&mut rt.heap, k.chars()).expect("memory");
    s.add_flags(FLAG_REGISTERED);
    rt.define_attrs(reg, Key::Name(k), 0, s.key_value())?;
    Ok(s.key_value())
}

fn symbol_key_for(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let Some(s) = Realm::sym_of(arg(args, 0)) else { return rt.type_error("Symbol.keyFor requires a symbol") };
    Ok(if s.is_registered() { rt.symbol_description_value(s) } else { Value::UNDEFINED })
}

fn symbol_to_string(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let s = rt.this_symbol(this, "toString")?;
    Ok(rt.symbol_string(s))
}

fn symbol_value_of(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    Ok(rt.this_symbol(this, "valueOf")?.key_value())
}

fn symbol_description(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let s = rt.this_symbol(this, "description")?;
    Ok(rt.symbol_description_value(s))
}

/// `Function.prototype[Symbol.hasInstance](v)`: `OrdinaryHasInstance(this, v)`.
fn function_has_instance(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let Some(f) = Realm::obj(this) else { return Ok(Value::FALSE) };
    Ok(Value::bool(rt.ordinary_has_instance(f, arg(args, 0))?))
}

#[cfg(test)]
#[path = "bi_symbol_tests.rs"]
mod tests;
