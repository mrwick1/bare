//! Own properties and `Object` (plan step S2.7, follow-up): property descriptors, `[[DefineOwnProperty]]`, extensibility, `freeze`,
//! `seal`, the order of the own keys, and the functions of `Object` and `Object.prototype`. Safe code over [`Realm`].
//!
//! **Where a property lives.** A named property is in the shape of the object (`shapes.rs`) with its attributes. An element of an array
//! is in the elements record and has the attributes writable, enumerable and configurable, except that three header flags can take
//! away `writable` from all elements (`F_ELEMS_RO`) or `configurable` from all (`F_ELEMS_FIXED`), or `writable` from `length`
//! (`F_LEN_RO`): `freeze` and `seal` of an array need no per-element work. An element with other attributes, or an accessor, moves the
//! whole array to the slow form (`F_SLOWIDX`): every index property is then in the shape under the name `"0"`, `"1"`, ... and the
//! elements record is empty. Only an array that a script gives such an element goes there.
//!
//! **A native does not call a script function** (`builtins.rs`). A built-in that meets an accessor (a getter of a descriptor, a setter
//! of the target of `Object.assign`) hands the work to a JavaScript helper of `prelude.js` through the tail channel (`Realm::helper`).
//! The common case, data properties, stays in Rust.
use crate::builtins::{NativeFn, arg};
use crate::cell::Layout;
use crate::interp_rt::{ErrKind, Key, R, Realm, Slot};
use crate::object::{F_ELEMS_FIXED, F_ELEMS_RO, F_LEN_RO, F_SLOWIDX, Obj};
use crate::shapes::{A_ACC, A_C, A_E, A_W};
use crate::string::{Chars, Str};
use crate::value::Value;

/// A property descriptor as a script gives it: each field may be missing.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Desc {
    pub value: Option<Value>,
    pub get: Option<Value>,
    pub set: Option<Value>,
    pub w: Option<bool>,
    pub e: Option<bool>,
    pub c: Option<bool>,
}

impl Desc {
    fn is_accessor(&self) -> bool {
        self.get.is_some() || self.set.is_some()
    }

    fn is_data(&self) -> bool {
        self.value.is_some() || self.w.is_some()
    }
}

/// An own property: a data value or a getter and a setter, with the attribute bits (`A_ACC` is set for an accessor).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Own {
    Data(Value, u8),
    Acc(Value, Value, u8),
}

impl Own {
    pub(crate) fn attrs(&self) -> u8 {
        match *self {
            Own::Data(_, a) | Own::Acc(_, _, a) => a,
        }
    }
}

/// The helpers of `prelude.js` that a native tail-calls (index in `Realm::helpers`).
pub(crate) const H_DEFINE: usize = 0;
pub(crate) const H_DEFINE_ALL: usize = 1;
pub(crate) const H_ASSIGN: usize = 2;
pub(crate) const H_SEARCH: usize = 3;
pub(crate) const H_SPREAD: usize = 4;
pub(crate) const H_REST: usize = 5;
pub(crate) const H_PAIRS: usize = 6;
pub(crate) const HELPERS: usize = 7;

/// The array index that the text of the atom `k` is, if it is one (a canonical decimal below 2^32 - 1).
pub(crate) fn atom_index(k: Str) -> Option<u32> {
    if k.is_symbol() {
        return None;
    }
    let s = k.resolve();
    let Some(Chars::One(b)) = s.chars() else { return None };
    if b.is_empty() || b.len() > 10 || !b[0].is_ascii_digit() || (b[0] == b'0' && b.len() > 1) || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let n: u32 = std::str::from_utf8(b).ok()?.parse().ok()?;
    (n != u32::MAX).then_some(n)
}

impl Realm {
    // ---- helpers written in JavaScript ----

    /// Ask the interpreter to call the helper `h` of the prelude with `args` (the tail channel): the result of the native is its result.
    pub(crate) fn helper(&mut self, h: usize, args: &[Value]) -> R<Value> {
        let f = self.helpers[h];
        self.tail_args.clear();
        self.tail_args.extend_from_slice(args);
        self.tail = Some((f, Value::UNDEFINED));
        Ok(Value::UNDEFINED)
    }

    /// Set the arguments of a helper that an op calls (the op reads `tail_args`).
    pub(crate) fn helper_args(&mut self, args: &[Value]) {
        self.tail_args.clear();
        self.tail_args.extend_from_slice(args);
    }

    // ---- comparison ----

    /// `SameValue`.
    pub(crate) fn same_value(&mut self, x: Value, y: Value) -> bool {
        if let (Some(a), Some(b)) = (x.as_f64(), y.as_f64()) {
            return if a.is_nan() { b.is_nan() } else { a == b && a.is_sign_negative() == b.is_sign_negative() };
        }
        self.strict_eq(x, y)
    }

    // ---- reading own properties ----

    /// Is `o` an array (`length` is writable and not configurable) rather than an arguments object?
    fn array_like_length(&self, o: Obj) -> Option<u8> {
        if !o.is_indexed() {
            return None;
        }
        Some(if self.is_array(o) { if o.has_flag(F_LEN_RO) { 0 } else { A_W } } else { A_W | A_C })
    }

    /// The own property `key` of `o`.
    pub(crate) fn get_own(&mut self, o: Obj, key: Key) -> Option<Own> {
        if let Some(s) = self.string_wrapper(o)
            && let Some(v) = self.string_own(s, key)
        {
            return Some(Own::Data(v, if matches!(key, Key::Idx(_)) { A_E } else { 0 }));
        }
        let k = match key {
            Key::Idx(i) => {
                if o.is_indexed()
                    && let Some(v) = o.get_element(i)
                {
                    let mut at = A_E;
                    if !o.has_flag(F_ELEMS_RO) {
                        at |= A_W;
                    }
                    if !o.has_flag(F_ELEMS_FIXED) {
                        at |= A_C;
                    }
                    return Some(Own::Data(v, at));
                }
                self.idx_atom(i)
            }
            Key::Name(k) => {
                if k == self.k_length
                    && let Some(at) = self.array_like_length(o)
                {
                    return Some(Own::Data(Realm::num(o.len() as f64), at));
                }
                self.fn_prep(o, k);
                k
            }
        };
        let e = self.shapes.find(o.shape(&self.shapes), k)?;
        Some(if e.attrs() & A_ACC != 0 {
            Own::Acc(o.slot(&self.shapes, e.slot()), o.slot(&self.shapes, e.slot() + 1), e.attrs())
        } else {
            Own::Data(o.slot(&self.shapes, e.slot()), e.attrs())
        })
    }

    /// The own property `key` of any value (`ToObject` for a string: its characters and `length`).
    fn get_own_of(&mut self, v: Value, key: Key) -> R<Option<Own>> {
        if let Some(o) = Realm::obj(v) {
            return self.get_own_h(o, key);
        }
        if let Some(s) = Realm::str_of(v) {
            let s = self.flat(s);
            return Ok(match key {
                Key::Idx(i) => s.unit_at(i as usize).map(|u| {
                    let c = self.string(&String::from_utf16_lossy(&[u]));
                    Own::Data(c, A_E)
                }),
                Key::Name(k) if k == self.k_length => Some(Own::Data(Realm::num(s.len() as f64), 0)),
                Key::Name(_) => None,
            });
        }
        if v.is_undefined() || v.is_null() {
            return self.type_error("cannot convert undefined or null to an object");
        }
        Ok(None)
    }

    /// The own string keys of an object in the order of the specification: array indexes ascending, then the names as they were added.
    /// `enumerable` leaves out the properties that are not enumerable (and `length` of an array).
    pub(crate) fn own_key_list(&mut self, o: Obj, enumerable: bool) -> Vec<Value> {
        if !enumerable {
            self.fn_prep_all(o);
        }
        let id = o.shape(&self.shapes);
        let mut idx: Vec<(u32, Value)> = Vec::new();
        let mut names: Vec<Value> = Vec::new();
        for e in self.shapes.entries(id) {
            if enumerable && e.attrs() & A_E == 0 {
                continue;
            }
            let k = e.key();
            if k.is_symbol() {
                continue;
            }
            match atom_index(k) {
                Some(i) => idx.push((i, k.value())),
                None if self.is_hidden_key(k) => {}
                None => names.push(k.value()),
            }
        }
        let elems = if o.is_indexed() { o.indices() } else { Vec::new() };
        let mut out = Vec::with_capacity(elems.len() + idx.len() + names.len() + 1);
        if idx.is_empty() {
            for i in elems {
                out.push(self.string(&i.to_string()));
            }
        } else {
            for i in elems {
                idx.push((i, Value::UNDEFINED));
            }
            idx.sort_by_key(|p| p.0);
            for (i, v) in idx {
                out.push(if v.is_undefined() { self.string(&i.to_string()) } else { v });
            }
        }
        // A `String` wrapper has the units of its string (and `length`) before its other properties; they are not in its shape.
        let wrapped = self.string_wrapper(o);
        if let Some(s) = wrapped {
            let units: Vec<Value> = (0..s.len()).map(|i| self.string(&i.to_string())).collect();
            out.splice(0..0, units);
        }
        if !enumerable && (o.is_indexed() || wrapped.is_some()) {
            out.push(self.k_length.value());
        }
        out.extend(names);
        out
    }

    /// The own symbol keys of an object in the order they were added (`Object.getOwnPropertySymbols`, and the symbols that `Object.assign`
    /// copies). `enumerable` leaves out the properties that are not enumerable.
    pub(crate) fn own_symbol_list(&mut self, o: Obj, enumerable: bool) -> Vec<Value> {
        self.fn_prep_all(o);
        self.shapes.entries(o.shape(&self.shapes)).filter(|e| e.key().is_symbol() && (!enumerable || e.attrs() & A_E != 0)).map(|e| e.key().key_value()).collect()
    }

    /// Is `k` the name of an internal slot (`\0code`, a private name)?
    fn is_hidden_key(&self, k: Str) -> bool {
        matches!(k.resolve().chars(), Some(Chars::One(b)) if b.first() == Some(&0)) || matches!(k.resolve().chars(), Some(Chars::Two(u)) if u.first() == Some(&0))
    }

    // ---- extensibility, seal, freeze ----

    pub(crate) fn prevent_extensions(&mut self, o: Obj) {
        if o.extensible() {
            o.prevent_extensions(&mut self.shapes);
        }
    }

    /// `seal` (`freeze` false) or `freeze`: no new property, no property configurable, and for `freeze` no data property writable.
    pub(crate) fn lock(&mut self, o: Obj, freeze: bool) -> R<()> {
        self.fn_prep_all(o);
        self.prevent_extensions(o);
        if o.is_indexed() {
            o.set_flag(F_ELEMS_FIXED | if freeze { F_ELEMS_RO | F_LEN_RO } else { 0 });
        }
        let entries: Vec<_> = self.shapes.entries(o.shape(&self.shapes)).collect();
        for e in entries {
            let at = e.attrs();
            let new = if at & A_ACC != 0 { at & !A_C } else if freeze { at & !(A_W | A_C) } else { at & !A_C };
            if new != at {
                let vals: Vec<Value> = (0..e.width()).map(|j| o.slot(&self.shapes, e.slot() + j)).collect();
                self.reconfigure(o, e.key(), new, &vals)?;
            }
        }
        Ok(())
    }

    /// Is `o` sealed (`frozen` false) or frozen?
    pub(crate) fn is_locked(&mut self, o: Obj, frozen: bool) -> bool {
        if o.extensible() {
            return false;
        }
        self.fn_prep_all(o);
        if o.is_indexed() {
            let has = !o.is_empty() && !o.indices().is_empty();
            if has && (!o.has_flag(F_ELEMS_FIXED) || (frozen && !o.has_flag(F_ELEMS_RO))) {
                return false;
            }
            if frozen && !o.has_flag(F_LEN_RO) {
                return false;
            }
        }
        self.shapes.entries(o.shape(&self.shapes)).all(|e| e.attrs() & A_C == 0 && (!frozen || e.attrs() & A_ACC != 0 || e.attrs() & A_W == 0))
    }

    // ---- defining ----

    /// An index property of an array in the elements record, with the default attributes, becomes a named property of the shape.
    fn to_slow_index(&mut self, o: Obj) -> R<()> {
        if o.has_flag(F_SLOWIDX) {
            return Ok(());
        }
        let mut at = A_E;
        if !o.has_flag(F_ELEMS_RO) {
            at |= A_W;
        }
        if !o.has_flag(F_ELEMS_FIXED) {
            at |= A_C;
        }
        let len = o.len();
        let items: Vec<(u32, Value)> = o.indices().into_iter().map(|i| (i, o.get_element(i).unwrap_or(Value::UNDEFINED))).collect();
        for (i, v) in items {
            let k = self.idx_atom(i);
            self.add_checked(o, k, at, &[v])?;
        }
        o.set_len(&mut self.heap, 0).expect("memory");
        o.set_len(&mut self.heap, len).expect("memory");
        o.clear_flag(F_ELEMS_RO | F_ELEMS_FIXED);
        o.set_flag(F_SLOWIDX);
        Ok(())
    }

    /// `o.add` that ignores `F_NOEXT` (the caller has checked) and turns a failure into a `RangeError`.
    fn add_checked(&mut self, o: Obj, k: Str, attrs: u8, vals: &[Value]) -> R<()> {
        match o.add(&mut self.heap, &mut self.shapes, k, attrs, vals) {
            Some(_) => Ok(()),
            None => Err(self.error(ErrKind::Range, "out of memory")),
        }
    }

    /// `[[DefineOwnProperty]]` (`ValidateAndApplyPropertyDescriptor`): false when the specification says to refuse.
    pub(crate) fn define_own(&mut self, o: Obj, key: Key, d: &Desc) -> R<bool> {
        if let Key::Name(k) = key
            && k == self.k_length
            && self.array_like_length(o).is_some()
            && self.is_array(o)
        {
            return self.define_length(o, d);
        }
        if self.api.hs.active && o.has_flag(crate::object::F_HOST) && let Some(ok) = self.hs_write(o, key, crate::api_host::HsWrite::Define(d))? {
            return Ok(ok);
        }
        let cur = self.get_own(o, key);
        let Some(cur) = cur else {
            if !o.extensible() {
                return Ok(false);
            }
            if let Key::Idx(i) = key
                && o.is_indexed()
                && i >= o.len()
                && o.has_flag(F_LEN_RO)
            {
                return Ok(false);
            }
            let (attrs, vals) = new_property(d);
            return self.create_own(o, key, attrs, &vals).map(|_| true);
        };
        // The checks of a property that exists.
        let cat = cur.attrs();
        if cat & A_C == 0 {
            if d.c == Some(true) || d.e.is_some_and(|e| e != (cat & A_E != 0)) {
                return Ok(false);
            }
            if d.is_accessor() || d.is_data() {
                let cur_acc = matches!(cur, Own::Acc(..));
                if d.is_accessor() != cur_acc && (d.is_accessor() || d.is_data()) {
                    return Ok(false);
                }
                match cur {
                    Own::Acc(g, s, _) => {
                        if d.get.is_some_and(|x| !self.same_value(x, g)) || d.set.is_some_and(|x| !self.same_value(x, s)) {
                            return Ok(false);
                        }
                    }
                    Own::Data(v, _) => {
                        if cat & A_W == 0 && (d.w == Some(true) || d.value.is_some_and(|x| !self.same_value(x, v))) {
                            return Ok(false);
                        }
                    }
                }
            }
        }
        // The new attributes and values.
        let e_bit = d.e.map_or(cat & A_E, |b| if b { A_E } else { 0 });
        let c_bit = d.c.map_or(cat & A_C, |b| if b { A_C } else { 0 });
        let (attrs, vals): (u8, Vec<Value>) = match cur {
            Own::Data(..) if d.is_accessor() => (A_ACC | e_bit | c_bit, vec![d.get.unwrap_or(Value::UNDEFINED), d.set.unwrap_or(Value::UNDEFINED)]),
            Own::Data(v, _) => (d.w.map_or(cat & A_W, |b| if b { A_W } else { 0 }) | e_bit | c_bit, vec![d.value.unwrap_or(v)]),
            Own::Acc(..) if d.is_data() => (d.w.map_or(0, |b| if b { A_W } else { 0 }) | e_bit | c_bit, vec![d.value.unwrap_or(Value::UNDEFINED)]),
            Own::Acc(g, s, _) => (A_ACC | e_bit | c_bit, vec![d.get.unwrap_or(g), d.set.unwrap_or(s)]),
        };
        self.write_own(o, key, attrs, &vals).map(|_| true)
    }

    /// Make a property that the object does not have.
    fn create_own(&mut self, o: Obj, key: Key, attrs: u8, vals: &[Value]) -> R<()> {
        match key {
            Key::Idx(i) if o.is_indexed() && !o.has_flag(F_SLOWIDX) => {
                if attrs == A_W | A_E | A_C {
                    if o.set_element(&mut self.heap, i, vals[0]).is_none() {
                        return Err(self.error(ErrKind::Range, "invalid array length"));
                    }
                    return Ok(());
                }
                self.to_slow_index(o)?;
                self.create_own(o, key, attrs, vals)
            }
            Key::Idx(i) => {
                let k = self.idx_atom(i);
                self.add_checked(o, k, attrs, vals)?;
                if o.is_indexed() && i >= o.len() {
                    o.set_len(&mut self.heap, i + 1).expect("memory");
                }
                Ok(())
            }
            Key::Name(k) => self.add_checked(o, k, attrs, vals),
        }
    }

    /// Replace the attributes and values of a property that exists.
    fn write_own(&mut self, o: Obj, key: Key, attrs: u8, vals: &[Value]) -> R<()> {
        let k = match key {
            Key::Idx(i) if o.is_indexed() && !o.has_flag(F_SLOWIDX) => {
                let uniform = A_E | if o.has_flag(F_ELEMS_RO) { 0 } else { A_W } | if o.has_flag(F_ELEMS_FIXED) { 0 } else { A_C };
                if o.get_element(i).is_some() {
                    if attrs == uniform {
                        o.set_element(&mut self.heap, i, vals[0]).expect("memory");
                        return Ok(());
                    }
                    self.to_slow_index(o)?;
                    return self.write_own(o, key, attrs, vals);
                }
                self.idx_atom(i)
            }
            Key::Idx(i) => self.idx_atom(i),
            Key::Name(k) => k,
        };
        let e = self.shapes.find(o.shape(&self.shapes), k).expect("the property exists");
        if e.attrs() == attrs {
            for (j, &v) in vals.iter().enumerate() {
                o.set_slot(&self.shapes, e.slot() + j as u32, v);
            }
            Ok(())
        } else {
            self.reconfigure(o, k, attrs, vals)
        }
    }

    /// `ArraySetLength` for a descriptor of `length`.
    fn define_length(&mut self, o: Obj, d: &Desc) -> R<bool> {
        if d.c == Some(true) || d.e == Some(true) || d.is_accessor() {
            return Ok(false);
        }
        let ro = o.has_flag(F_LEN_RO);
        let Some(v) = d.value else {
            if d.w == Some(true) && ro {
                return Ok(false);
            }
            if d.w == Some(false) {
                o.set_flag(F_LEN_RO);
            }
            return Ok(true);
        };
        let n = self.to_number(v)?;
        if n < 0.0 || n.fract() != 0.0 || n >= 4294967296.0 || n.is_nan() {
            return Err(self.error(ErrKind::Range, "invalid array length"));
        }
        let n = n as u32;
        let ok = if n == o.len() {
            !(ro && d.w == Some(true))
        } else if ro || (d.w == Some(true) && ro) {
            false
        } else {
            self.truncate(o, n)
        };
        if d.w == Some(false) {
            o.set_flag(F_LEN_RO);
        }
        Ok(ok)
    }

    /// Set the length of an array to `n`. A shorter length deletes the elements above it from the top; one that is not configurable stops
    /// the deletion (the length stays above it) and the result is false.
    pub(crate) fn truncate(&mut self, o: Obj, n: u32) -> bool {
        if n >= o.len() {
            o.set_len(&mut self.heap, n).expect("memory");
            return true;
        }
        if o.has_flag(F_SLOWIDX) {
            let mut names: Vec<(u32, Str, bool)> = Vec::new();
            for e in self.shapes.entries(o.shape(&self.shapes)) {
                if let Some(i) = atom_index(e.key())
                    && i >= n
                {
                    names.push((i, e.key(), e.attrs() & A_C != 0));
                }
            }
            names.sort_by_key(|p| std::cmp::Reverse(p.0));
            for (i, k, conf) in names {
                if !conf {
                    o.set_len(&mut self.heap, i + 1).expect("memory");
                    return false;
                }
                o.remove(&mut self.shapes, k);
            }
        } else if o.has_flag(F_ELEMS_FIXED) {
            if let Some(&top) = o.indices().iter().rev().find(|&&i| i >= n) {
                o.set_len(&mut self.heap, top + 1).expect("memory");
                return false;
            }
        }
        o.set_len(&mut self.heap, n).expect("memory");
        true
    }

    // ---- reading a descriptor object ----

    /// The descriptor that the object `d` describes. `Ok(None)` when a field is an accessor property of `d` (it needs a call): the
    /// caller uses a helper of the prelude.
    fn read_desc(&mut self, d: Value) -> R<Option<Desc>> {
        let Some(o) = Realm::obj(d) else { return self.type_error("property description must be an object") };
        let mut out = Desc::default();
        for (name, slot) in [("enumerable", 0), ("configurable", 1), ("value", 2), ("writable", 3), ("get", 4), ("set", 5)] {
            let k = self.intern(name);
            let v = match self.lookup_g(o, k) {
                None => continue,
                Some(Slot::Data(v)) => v,
                Some(Slot::Acc(..)) => return Ok(None),
            };
            match slot {
                0 => out.e = Some(Realm::truthy(v)),
                1 => out.c = Some(Realm::truthy(v)),
                2 => out.value = Some(v),
                3 => out.w = Some(Realm::truthy(v)),
                4 => out.get = Some(v),
                _ => out.set = Some(v),
            }
        }
        for f in [out.get, out.set].into_iter().flatten() {
            if !f.is_undefined() && !Realm::obj(f).is_some_and(|f| f.is_callable()) {
                return self.type_error("getter and setter must be functions");
            }
        }
        if out.is_accessor() && out.is_data() {
            return self.type_error("invalid property descriptor: cannot both specify accessors and a value or writable");
        }
        Ok(Some(out))
    }

    /// The descriptor object of an own property.
    pub(crate) fn from_own(&mut self, p: Own) -> Value {
        let d = Obj::new(&mut self.heap, &self.shapes, self.s_object).expect("memory");
        let at = p.attrs();
        match p {
            Own::Data(v, _) => {
                self.set_own(d, "value", v);
                self.set_own(d, "writable", Value::bool(at & A_W != 0));
            }
            Own::Acc(g, s, _) => {
                self.set_own(d, "get", g);
                self.set_own(d, "set", s);
            }
        }
        self.set_own(d, "enumerable", Value::bool(at & A_E != 0));
        self.set_own(d, "configurable", Value::bool(at & A_C != 0));
        d.value()
    }

    /// `Set(o, key, v, true)`: a write that fails (not writable, not extensible, no setter) is a `TypeError`. A setter is called through
    /// the tail channel when `last` is true (use it as the last step of a native); else a setter is a `TypeError`.
    pub(crate) fn set_checked(&mut self, recv: Value, key: Key, v: Value, last: bool) -> R<()> {
        let Some(o) = Realm::obj(recv) else {
            if recv.is_undefined() || recv.is_null() {
                return self.type_error("cannot set properties of undefined or null");
            }
            return Ok(());
        };
        let mut p = Some(o);
        let mut own = true;
        while let Some(c) = p {
            match self.get_own(c, key) {
                Some(Own::Acc(_, s, _)) => {
                    if s.is_undefined() {
                        return self.type_error("cannot set a property that has only a getter");
                    }
                    if !last {
                        return self.type_error("a setter cannot be called from here");
                    }
                    self.tail_args.clear();
                    self.tail_args.push(v);
                    self.tail = Some((s, recv));
                    return Ok(());
                }
                Some(Own::Data(_, at)) => {
                    if at & A_W == 0 {
                        return self.type_error("cannot assign to a read-only property");
                    }
                    break;
                }
                None => {}
            }
            p = self.proto_of(c);
            own = false;
        }
        let _ = own;
        match self.get_own(o, key) {
            Some(Own::Data(_, at)) if at & A_W != 0 => self.set_prop(recv, key, v),
            Some(_) => Ok(()),
            None if !o.extensible() => self.type_error("cannot add a property to a non-extensible object"),
            // A script function has no own `length` or `name` yet; both are read only.
            None if o.is_callable() && matches!(key, Key::Name(k) if k == self.k_length || k == self.k_name) => {
                self.type_error("cannot assign to a read-only property")
            }
            None if o.is_indexed() && matches!(key, Key::Idx(i) if i >= o.len()) && o.has_flag(F_LEN_RO) => {
                self.type_error("cannot add an element past the read-only length of an array")
            }
            None => self.set_prop(recv, key, v),
        }
    }

    // ---- Object ----

    pub(crate) fn install_object(&mut self) {
        let proto = self.object_proto;
        let c = self.ctor("Object", 1, object_ctor, proto);
        for (n, len, f) in [
            ("assign", 2, object_assign as NativeFn),
            ("getOwnPropertyDescriptor", 2, object_own_descriptor),
            ("getOwnPropertyDescriptors", 1, object_own_descriptors),
            ("getOwnPropertyNames", 1, object_own_names),
            ("getOwnPropertySymbols", 1, object_own_symbols),
            ("is", 2, object_is),
            ("preventExtensions", 1, object_prevent_extensions),
            ("seal", 1, object_seal),
            ("create", 2, object_create),
            ("defineProperties", 2, object_define_properties),
            ("defineProperty", 3, object_define_property),
            ("freeze", 1, object_freeze),
            ("getPrototypeOf", 1, object_get_prototype_of),
            ("setPrototypeOf", 2, object_set_prototype_of),
            ("isExtensible", 1, object_is_extensible),
            ("isFrozen", 1, object_is_frozen),
            ("isSealed", 1, object_is_sealed),
            ("keys", 1, object_keys),
            ("entries", 1, object_entries),
            ("fromEntries", 1, object_from_entries),
            ("values", 1, object_values),
            ("hasOwn", 2, object_has_own),
        ] {
            self.method(c, n, len, f);
        }
        for (n, len, f) in [
            ("__defineGetter__", 2, define_getter as NativeFn),
            ("__defineSetter__", 2, define_setter),
            ("hasOwnProperty", 1, has_own),
            ("__lookupGetter__", 1, lookup_getter),
            ("__lookupSetter__", 1, lookup_setter),
            ("isPrototypeOf", 1, is_prototype_of),
            ("propertyIsEnumerable", 1, property_is_enumerable),
            ("toString", 0, object_to_string),
            ("valueOf", 0, object_value_of),
            ("toLocaleString", 0, to_locale_string),
        ] {
            self.method(proto, n, len, f);
        }
        let g = self.global;
        self.method(g, "__helper", 2, register_helper);
        self.method(g, "__set", 3, set_strict);
    }
}

/// The attributes and values that a descriptor gives to a new property (a missing field is false or `undefined`).
fn new_property(d: &Desc) -> (u8, Vec<Value>) {
    let ec = d.e.map_or(0, |b| if b { A_E } else { 0 }) | d.c.map_or(0, |b| if b { A_C } else { 0 });
    if d.is_accessor() {
        (A_ACC | ec, vec![d.get.unwrap_or(Value::UNDEFINED), d.set.unwrap_or(Value::UNDEFINED)])
    } else {
        (d.w.map_or(0, |b| if b { A_W } else { 0 }) | ec, vec![d.value.unwrap_or(Value::UNDEFINED)])
    }
}

/// `ToObject` for the built-ins here: an object stays; a string, number or boolean has no own properties but a string's characters.
fn to_obj(rt: &mut Realm, v: Value) -> R<Value> {
    if v.is_undefined() || v.is_null() {
        return rt.type_error("cannot convert undefined or null to an object");
    }
    Ok(v)
}

fn object_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    // `new Object()` from `super()` of a class that extends it: the prototype comes from `new.target`, and the argument is not read.
    // `Object(v)` and `new Object(v)` give `ToObject(v)`: an object is itself, a primitive gets a wrapper.
    let a = arg(args, 0);
    if !a.is_undefined() && !a.is_null() && (Realm::obj(rt.nt).is_none() || rt.nt == rt.callee) {
        return Ok(rt.to_object(a)?.value());
    }
    if Realm::obj(rt.nt).is_none() && Realm::obj(a).is_some() {
        return Ok(a);
    }
    let proto = rt.new_proto()?;
    let shape = rt.shapes.root(Layout::Object, Some(proto.cell()), 4);
    Ok(Obj::new(&mut rt.heap, &rt.shapes, shape).expect("memory").value())
}

fn object_keys(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let v = to_obj(rt, arg(args, 0))?;
    rt.own_keys(v)
}

/// The own enumerable keys of `v` and the value of each. A getter is a call: `Err` through `Ok(None)` when one is met.
fn own_pairs(rt: &mut Realm, v: Value) -> R<Option<Vec<(Value, Value)>>> {
    let o = to_obj(rt, v)?;
    let keys = rt.own_keys(o)?;
    let ko = Realm::obj(keys).expect("an array");
    let mut out = Vec::with_capacity(ko.len() as usize);
    for i in 0..ko.len() {
        let k = ko.get_element(i).expect("a key");
        let key = rt.key_of(k)?;
        match rt.get_own_of(v, key)? {
            Some(Own::Acc(..)) => return Ok(None),
            Some(Own::Data(x, _)) => out.push((k, x)),
            None => {}
        }
    }
    Ok(Some(out))
}

fn object_values(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let Some(p) = own_pairs(rt, arg(args, 0))? else { return rt.helper(H_PAIRS, &[Value::int(0), arg(args, 0)]) };
    let v: Vec<Value> = p.into_iter().map(|p| p.1).collect();
    Ok(rt.new_array(&v))
}

fn object_entries(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let Some(pairs) = own_pairs(rt, arg(args, 0))? else { return rt.helper(H_PAIRS, &[Value::int(1), arg(args, 0)]) };
    let v: Vec<Value> = pairs.into_iter().map(|p| rt.new_array(&[p.0, p.1])).collect();
    Ok(rt.new_array(&v))
}

/// Does any enumerable own property of `v` or any setter on the target path need a call? (Then `assign` uses the helper.)
fn has_accessor(rt: &mut Realm, v: Value) -> bool {
    let Some(o) = Realm::obj(v) else { return false };
    rt.shapes.entries(o.shape(&rt.shapes)).any(|e| e.attrs() & A_ACC != 0 && e.attrs() & A_E != 0)
}

fn object_assign(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let target = rt.to_object(arg(args, 0))?.value();
    let slow = args.iter().skip(1).any(|&s| has_accessor(rt, s)) || {
        // A setter on the target or its prototypes, or a getter on the keys, needs the helper.
        let mut chain = Realm::obj(target);
        let mut found = false;
        while let Some(c) = chain {
            if has_any_accessor(rt, c) {
                found = true;
                break;
            }
            chain = rt.proto_of(c);
        }
        found
    };
    if slow {
        return rt.helper(H_ASSIGN, args);
    }
    for &src in args.iter().skip(1) {
        if src.is_undefined() || src.is_null() {
            continue;
        }
        let Some(pairs) = own_pairs(rt, src)? else { return rt.helper(H_ASSIGN, args) };
        for (k, v) in pairs {
            let key = rt.key_of(k)?;
            rt.set_checked(target, key, v, false)?;
        }
        for k in Realm::obj(src).map(|o| rt.own_symbol_list(o, true)).unwrap_or_default() {
            let key = rt.key_of(k)?;
            match rt.get_own_of(src, key)? {
                Some(Own::Data(v, _)) => rt.set_checked(target, key, v, false)?,
                _ => return rt.helper(H_ASSIGN, args),
            }
        }
    }
    Ok(target)
}

fn has_any_accessor(rt: &Realm, o: Obj) -> bool {
    rt.shapes.entries(o.shape(&rt.shapes)).any(|e| e.attrs() & A_ACC != 0)
}

fn object_create(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let p = arg(args, 0);
    let proto = match (Realm::obj(p), p.is_null()) {
        (Some(o), _) => Some(o.cell()),
        (None, true) => None,
        _ => return rt.type_error("object prototype may only be an object or null"),
    };
    let shape = rt.shapes.root(Layout::Object, proto, 4);
    let o = Obj::new(&mut rt.heap, &rt.shapes, shape).expect("memory");
    if !arg(args, 1).is_undefined() {
        // A tail call to the native or the helper would lose `o`: define here, and let the helper return `o` in the slow case.
        return define_all(rt, o, arg(args, 1));
    }
    Ok(o.value())
}

/// `ObjectDefineProperties(o, props)`. The result is `o` (a helper's result too).
fn define_all(rt: &mut Realm, o: Obj, props: Value) -> R<Value> {
    let props = to_obj(rt, props)?;
    let keys = rt.own_keys(props)?;
    let ko = Realm::obj(keys).expect("an array");
    let mut work: Vec<(Key, Desc)> = Vec::new();
    let syms = Realm::obj(props).map(|o| rt.own_symbol_list(o, true)).unwrap_or_default();
    for kv in (0..ko.len()).map(|i| ko.get_element(i).expect("a key")).chain(syms) {
        let key = rt.key_of(kv)?;
        let Some(Own::Data(dv, _)) = rt.get_own_of(props, key)? else { return rt.helper(H_DEFINE_ALL, &[o.value(), props]) };
        let Some(d) = rt.read_desc(dv)? else { return rt.helper(H_DEFINE_ALL, &[o.value(), props]) };
        work.push((key, d));
    }
    for (key, d) in work {
        if !rt.define_own(o, key, &d)? {
            return rt.type_error("cannot redefine property");
        }
    }
    Ok(o.value())
}

fn object_define_property(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let Some(o) = Realm::obj(arg(args, 0)) else { return rt.type_error("Object.defineProperty called on a non-object") };
    let key = rt.key_of(arg(args, 1))?;
    let Some(d) = rt.read_desc(arg(args, 2))? else { return rt.helper(H_DEFINE, args) };
    if !rt.define_own(o, key, &d)? {
        return rt.type_error("cannot redefine property");
    }
    Ok(o.value())
}

fn object_define_properties(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let Some(o) = Realm::obj(arg(args, 0)) else { return rt.type_error("Object.defineProperties called on a non-object") };
    define_all(rt, o, arg(args, 1))
}

fn object_own_symbols(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let v = to_obj(rt, arg(args, 0))?;
    let list = Realm::obj(v).map(|o| rt.own_symbol_list(o, false)).unwrap_or_default();
    Ok(rt.new_array(&list))
}

fn object_get_prototype_of(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let v = arg(args, 0);
    if let Some(o) = Realm::obj(v) {
        return Ok(rt.proto_of(o).map_or(Value::NULL, |p| p.value()));
    }
    Ok(if Realm::str_of(v).is_some() {
        rt.string_proto.value()
    } else if v.is_number() {
        rt.number_proto.value()
    } else if v.as_bool().is_some() {
        rt.boolean_proto.value()
    } else if Realm::sym_of(v).is_some() {
        rt.fn_props(rt.symbol_proto);
        rt.symbol_proto.value()
    } else {
        return rt.type_error("cannot convert undefined or null to an object");
    })
}

fn object_set_prototype_of(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let (v, p) = (arg(args, 0), arg(args, 1));
    if v.is_undefined() || v.is_null() {
        return rt.type_error("Object.setPrototypeOf called on null or undefined");
    }
    if Realm::obj(p).is_none() && !p.is_null() {
        return rt.type_error("object prototype may only be an object or null");
    }
    if let Some(o) = Realm::obj(v)
        && !rt.set_proto_checked(o, Realm::obj(p))?
    {
        return rt.type_error("cannot change the prototype of this object");
    }
    Ok(v)
}

impl Realm {
    /// `[[SetPrototypeOf]]`: false when the object is not extensible (and the prototype differs) or the change would make a cycle.
    pub(crate) fn set_proto_checked(&mut self, o: Obj, p: Option<Obj>) -> R<bool> {
        if self.proto_of(o) == p {
            return Ok(true);
        }
        // `Object.prototype` is an immutable prototype exotic object.
        if !o.extensible() || o == self.object_proto {
            return Ok(false);
        }
        let mut c = p;
        while let Some(x) = c {
            if x == o {
                return Ok(false);
            }
            c = self.proto_of(x);
        }
        o.set_proto(&mut self.shapes, p.map(|p| p.cell()));
        Ok(true)
    }
}

fn object_own_names(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let v = to_obj(rt, arg(args, 0))?;
    if let Some(s) = Realm::str_of(v) {
        let mut out: Vec<Value> = (0..s.len()).map(|i| rt.string(&i.to_string())).collect();
        out.push(rt.k_length.value());
        return Ok(rt.new_array(&out));
    }
    let Some(o) = Realm::obj(v) else { return Ok(rt.new_array(&[])) };
    let out = rt.own_key_list_h(o, false)?;
    Ok(rt.new_array(&out))
}

fn object_own_descriptor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let v = to_obj(rt, arg(args, 0))?;
    let key = rt.key_of(arg(args, 1))?;
    Ok(match rt.get_own_of(v, key)? {
        Some(p) => rt.from_own(p),
        None => Value::UNDEFINED,
    })
}

fn object_own_descriptors(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let v = to_obj(rt, arg(args, 0))?;
    let out = Obj::new(&mut rt.heap, &rt.shapes, rt.s_object).expect("memory");
    if let Some(o) = Realm::obj(v) {
        let mut keys = rt.own_key_list_h(o, false)?;
        keys.extend(rt.own_symbol_list(o, false));
        for k in keys {
            let key = rt.key_of(k)?;
            if let Some(p) = rt.get_own_h(o, key)? {
                let d = rt.from_own(p);
                rt.define(out, key, d)?;
            }
        }
    }
    Ok(out.value())
}

fn object_is(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    Ok(Value::bool(rt.same_value(arg(args, 0), arg(args, 1))))
}

fn object_prevent_extensions(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let v = arg(args, 0);
    if let Some(o) = Realm::obj(v) {
        rt.prevent_extensions(o);
    }
    Ok(v)
}

fn object_is_extensible(_rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    Ok(Value::bool(Realm::obj(arg(args, 0)).is_some_and(|o| o.extensible())))
}

fn object_freeze(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let v = arg(args, 0);
    if let Some(o) = Realm::obj(v) {
        rt.lock(o, true)?;
    }
    Ok(v)
}

fn object_seal(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let v = arg(args, 0);
    if let Some(o) = Realm::obj(v) {
        rt.lock(o, false)?;
    }
    Ok(v)
}

fn object_is_frozen(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    Ok(Value::bool(Realm::obj(arg(args, 0)).is_none_or(|o| rt.is_locked(o, true))))
}

fn object_is_sealed(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    Ok(Value::bool(Realm::obj(arg(args, 0)).is_none_or(|o| rt.is_locked(o, false))))
}

fn object_has_own(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let v = to_obj(rt, arg(args, 0))?;
    let key = rt.key_of(arg(args, 1))?;
    Ok(Value::bool(rt.get_own_of(v, key)?.is_some()))
}

fn object_from_entries(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let it = arg(args, 0);
    if it.is_undefined() || it.is_null() {
        return rt.type_error("the value is not iterable");
    }
    let out = Obj::new(&mut rt.heap, &rt.shapes, rt.s_object).expect("memory");
    for e in rt.iter_values(it)? {
        let Some(eo) = Realm::obj(e) else { return rt.type_error("an entry is not an object") };
        let (k, v) = (rt.get_prop(eo.value(), Key::Idx(0))?, rt.get_prop(eo.value(), Key::Idx(1))?);
        let key = rt.key_of(k)?;
        rt.define(out, key, v)?;
    }
    Ok(out.value())
}

fn this_object(rt: &mut Realm, this: Value) -> R<Value> {
    to_obj(rt, this)
}

fn has_own(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let key = rt.key_of(arg(args, 0))?;
    let v = this_object(rt, this)?;
    Ok(Value::bool(rt.get_own_of(v, key)?.is_some()))
}

fn property_is_enumerable(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let key = rt.key_of(arg(args, 0))?;
    let v = this_object(rt, this)?;
    Ok(Value::bool(rt.get_own_of(v, key)?.is_some_and(|p| p.attrs() & A_E != 0)))
}

fn define_accessor_method(rt: &mut Realm, this: Value, args: &[Value], setter: bool) -> R<Value> {
    let v = this_object(rt, this)?;
    let f = arg(args, 1);
    if !Realm::obj(f).is_some_and(|f| f.is_callable()) {
        return rt.type_error("the accessor is not a function");
    }
    let key = rt.key_of(arg(args, 0))?;
    if let Some(o) = Realm::obj(v) {
        let d = Desc { get: (!setter).then_some(f), set: setter.then_some(f), e: Some(true), c: Some(true), ..Desc::default() };
        if !rt.define_own(o, key, &d)? {
            return rt.type_error("cannot define the accessor");
        }
    }
    Ok(Value::UNDEFINED)
}

fn define_getter(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    define_accessor_method(rt, this, args, false)
}

fn define_setter(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    define_accessor_method(rt, this, args, true)
}

fn lookup_accessor(rt: &mut Realm, this: Value, args: &[Value], setter: bool) -> R<Value> {
    let v = this_object(rt, this)?;
    let key = rt.key_of(arg(args, 0))?;
    let mut c = Realm::obj(v);
    while let Some(o) = c {
        match rt.get_own(o, key) {
            Some(Own::Acc(g, s, _)) => return Ok(if setter { s } else { g }),
            Some(Own::Data(..)) => return Ok(Value::UNDEFINED),
            None => {}
        }
        c = rt.proto_of(o);
    }
    Ok(Value::UNDEFINED)
}

fn lookup_getter(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    lookup_accessor(rt, this, args, false)
}

fn lookup_setter(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    lookup_accessor(rt, this, args, true)
}

pub(crate) fn object_to_string(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let tag = if this.is_undefined() {
        "Undefined"
    } else if this.is_null() {
        "Null"
    } else if Realm::str_of(this).is_some() {
        "String"
    } else if this.is_number() {
        "Number"
    } else if this.as_bool().is_some() {
        "Boolean"
    } else if let Some(o) = Realm::obj(this) {
        if let Some(p) = rt.prim_of(this) {
            if Realm::str_of(p).is_some() {
                "String"
            } else if p.is_number() {
                "Number"
            } else if Realm::sym_of(p).is_some() {
                "Object"
            } else {
                "Boolean"
            }
        } else if o.is_callable() {
            "Function"
        } else if rt.is_array(o) {
            "Array"
        } else if rt.is_error(this) {
            "Error"
        } else if o.has_flag(crate::object::F_REGEXP) {
            "RegExp"
        } else if o.has_flag(crate::bi_date::F_DATE) {
            "Date"
        } else if o.is_indexed() {
            "Arguments"
        } else {
            "Object"
        }
    } else {
        "Object"
    };
    // `Symbol.toStringTag` of the value (a string) replaces the built-in tag. `undefined` and `null` have none.
    let tag = match if this.is_undefined() || this.is_null() { Value::UNDEFINED } else { rt.get_prop(this, Key::Name(rt.wk[crate::bi_symbol::TO_STRING_TAG]))? } {
        t if Realm::str_of(t).is_some() => rt.text(Realm::str_of(t).expect("a string")),
        _ => tag.to_string(),
    };
    Ok(rt.string(&format!("[object {tag}]")))
}

fn object_value_of(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    to_obj(rt, this)
}

/// `Object.prototype.toLocaleString`: `this.toString()`, through the tail channel.
fn to_locale_string(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    to_obj(rt, this)?;
    let f = rt.get_prop(this, Key::Name(rt.k_to_string))?;
    rt.tail_args.clear();
    rt.tail = Some((f, this));
    Ok(Value::UNDEFINED)
}

fn is_prototype_of(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let Some(mut o) = Realm::obj(arg(args, 0)) else { return Ok(Value::FALSE) };
    let Some(p) = Realm::obj(this_object(rt, this)?) else { return Ok(Value::FALSE) };
    while let Some(n) = rt.proto_of(o) {
        if n == p {
            return Ok(Value::TRUE);
        }
        o = n;
    }
    Ok(Value::FALSE)
}

/// `__helper(index, function)`: the prelude registers the JavaScript helpers that the natives call (`Realm::helper`).
fn register_helper(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let i = arg(args, 0).as_int().unwrap_or(0) as usize;
    if i >= HELPERS {
        return rt.type_error("no such helper");
    }
    rt.helpers[i] = arg(args, 1);
    rt.keep(arg(args, 1));
    Ok(Value::UNDEFINED)
}

/// `__set(target, key, value)` for the helpers: `Set(target, key, value, true)`.
fn set_strict(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let key = rt.key_of(arg(args, 1))?;
    rt.set_checked(arg(args, 0), key, arg(args, 2), true)?;
    Ok(Value::UNDEFINED)
}
