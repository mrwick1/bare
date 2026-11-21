//! Lazy tables for the properties of built-in objects (`docs/design-js2-lazy-builtins.md`, design B).
//!
//! The methods, the statics and the constants of a built-in object are a `static` table of [`Ent`]. [`Realm::lazy_table`] only
//! records the table; a property is made at the first lookup that needs it, with the same attributes, the same native and the same lazy
//! `length` and `name` as an eager install. So a realm pays for the properties that a script uses, and no more.
//!
//! **The flag.** An object that has a table with entries left has [`F_LAZYTAB`]. The flag is the only cost on the paths that do not
//! look at a name of such an object. Each place that reads, tests, defines, deletes or lists an own property of an object calls
//! [`Realm::lazy_prep`] (one name) or [`Realm::lazy_all`] (all names) first. Both are called from `fn_prep` and `fn_prep_all`
//! (`builtins.rs`), from the walks along a prototype chain (`lookup_g`, `proto_setter`, `proto_target`), and from `define_attrs`.
//! A new place that reads `shapes.find` of an object that a script can name must call one of them, or a property that is not yet made
//! looks absent.
//!
//! **The shape.** The inline caches (`interp_rt.rs`) answer a read that a prototype gives from the shape of the receiver alone, so an
//! object whose names are not all made must not share its shape with an object that has other names. [`Realm::lazy_table`] adds a hidden
//! key (its name starts with NUL, as the internal slots do, so no listing shows it) that makes the shape of the object its own.
//!
//! **The probe.** A bit filter of 4,096 bits holds the hashes of all names of all tables of the realm. A name that is not in it (almost
//! every miss) costs one test. For the others the probe scans the tables of the object for the cached hash of the atom and compares the
//! text. A table keeps its entries in the order of the declaration; each table has a bit for each entry in `Lazy::done`, so a property that
//! a script deleted is not made again.
//!
//! **Order.** A lazy property is made when it is first used, so the own keys of a built-in object come in the order of first use, then
//! the rest in table order after a listing. ECMA-262 does not fix this order (owner decision 2, 2026-11-20).
use crate::builtins::{HIDDEN, NativeFn};
use crate::interp_rt::{Key, Realm};
use crate::object::Obj;
use crate::string::{Chars, Str};
use crate::value::Value;

/// Object header flag: the object has a lazy table with entries left. Bits 16 to 18 are `F_COLL`, bit 19 is `F_DATE` or `F_HOST`, bit 20 is
/// `F_LAZYM`, bit 23 is `F_PROTO`.
pub const F_LAZYTAB: u32 = 1 << 21;

/// What a table entry makes.
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    /// A method: a native function, not enumerable, writable and configurable.
    Method(NativeFn),
    /// A constant: a number, not writable, not enumerable and not configurable.
    Num(f64),
    /// A method whose native has `i` in slot 1 (one native for many methods: `Date.prototype`, `bi_date.rs`).
    Indexed(NativeFn, i32),
    /// The global property of this name (`Number.parseInt` is the function `parseInt`), as a method.
    Alias,
    /// The same function as the entry of this name in the same table (`Date.prototype.toGMTString` is `toUTCString`), as a method.
    Same(&'static str),
}

/// One entry of a table: a name, the `length` of its function, and what it makes.
#[derive(Clone, Copy)]
pub(crate) struct Ent {
    hash: u32,
    name: &'static str,
    len: u8,
    kind: Kind,
}

/// The hash of `Chars::hash` for a name of ASCII bytes, so that a table needs no atom at the start.
const fn hash(s: &str) -> u32 {
    let b = s.as_bytes();
    let mut h = b.len() as u32;
    let mut i = 0;
    while i < b.len() {
        assert!(b[i] < 128, "a table name is ASCII");
        h = (h.rotate_left(5) ^ b[i] as u32).wrapping_mul(0x9E37_79B9);
        i += 1;
    }
    h ^= h >> 16;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    if h == 0 { 1 } else { h }
}

/// A method `name` with the function `length` `len`.
pub(crate) const fn m(name: &'static str, len: u8, f: NativeFn) -> Ent {
    Ent { hash: hash(name), name, len, kind: Kind::Method(f) }
}

/// A constant `name`.
pub(crate) const fn num(name: &'static str, v: f64) -> Ent {
    Ent { hash: hash(name), name, len: 0, kind: Kind::Num(v) }
}

/// A method `name` of the native `f` with `i` in its slot 1.
pub(crate) const fn indexed(name: &'static str, len: u8, f: NativeFn, i: i32) -> Ent {
    Ent { hash: hash(name), name, len, kind: Kind::Indexed(f, i) }
}

/// A method that is the same function as the entry `of` of the same table.
pub(crate) const fn same(name: &'static str, of: &'static str) -> Ent {
    Ent { hash: hash(name), name, len: 0, kind: Kind::Same(of) }
}

/// A method that is the same function as the global of the same name.
pub(crate) const fn alias(name: &'static str) -> Ent {
    Ent { hash: hash(name), name, len: 0, kind: Kind::Alias }
}

/// A table of one object, and its place in `Lazy::done`.
struct Tab {
    obj: Obj,
    ents: &'static [Ent],
    base: usize,
    /// The entries that are not made yet.
    left: usize,
    /// Has the table a [`Kind::Same`] entry? Then the functions that it makes are kept in `Lazy::shared`.
    has_same: bool,
}

/// The lazy tables of a realm.
pub(crate) struct Lazy {
    tabs: Vec<Tab>,
    /// A bit for each entry of each table: made (or deleted by a script after it was made).
    done: Vec<u64>,
    /// The filter of the hashes of all names: 4,096 bits.
    bits: Vec<u64>,
    /// The function that each method of the global object (and each method of a table with a `Same` entry) was made as (a root), for an
    /// alias of it: a script may delete or replace the property, and `Number.parseInt` is still the original function.
    shared: Vec<(Obj, &'static str, Value)>,
}

impl Default for Lazy {
    fn default() -> Self {
        Lazy { tabs: Vec::new(), done: Vec::new(), bits: Vec::new(), shared: Vec::new() }
    }
}

#[cfg(test)]
thread_local! {
    /// A test switch: a realm that makes every table entry at once, as the eager install did.
    pub(crate) static EAGER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl Realm {
    /// Give `o` the properties of `ents`, made at their first use. Call it after the eager properties of `o` are defined.
    pub(crate) fn lazy_table(&mut self, o: Obj, ents: &'static [Ent]) {
        if !self.ltab.tabs.iter().any(|t| t.obj == o) {
            // The inline caches key a read that a prototype answers by the shape of the receiver. Two objects of one shape must have the
            // same names, so an object with names that are not made yet gets a shape of its own: a hidden key that no other object has.
            let marker = format!("\0lt{}", self.ltab.tabs.len());
            self.hidden(o, &marker, Value::UNDEFINED);
        }
        let l = &mut self.ltab;
        if l.bits.is_empty() {
            l.bits = vec![0; 64];
        }
        let base = l.tabs.iter().map(|t| t.ents.len()).sum::<usize>();
        l.done.resize((base + ents.len()).div_ceil(64), 0);
        for e in ents {
            l.bits[(e.hash >> 6) as usize & 63] |= 1 << (e.hash & 63);
        }
        l.tabs.push(Tab { obj: o, ents, base, left: ents.len(), has_same: ents.iter().any(|e| matches!(e.kind, Kind::Same(_))) });
        o.set_flag(F_LAZYTAB);
        #[cfg(test)]
        if EAGER.with(|e| e.get()) {
            self.lazy_all(o);
        }
    }

    /// Make the property `k` of `o`, if a table of `o` has it and it is not made yet. Call it where `shapes.find(o's shape, k)` would
    /// be the first look at an own property of `o`.
    #[inline]
    pub(crate) fn lazy_prep(&mut self, o: Obj, k: Str) {
        if o.has_flag(F_LAZYTAB) {
            self.lazy_probe(o, k);
        }
    }

    #[inline(never)]
    fn lazy_probe(&mut self, o: Obj, k: Str) {
        if k.is_symbol() {
            return;
        }
        let h = match k.cached_hash() {
            0 => k.chars().map_or(0, |c| c.hash()),
            h => h,
        };
        if self.ltab.bits[(h >> 6) as usize & 63] & (1 << (h & 63)) == 0 {
            return;
        }
        for ti in 0..self.ltab.tabs.len() {
            let t = &self.ltab.tabs[ti];
            if t.obj != o || t.left == 0 {
                continue;
            }
            let Some(j) = t.ents.iter().position(|e| e.hash == h && same_text(k, e.name)) else { continue };
            if self.ltab.done[(t.base + j) / 64] >> ((t.base + j) % 64) & 1 == 0 {
                self.lazy_make(ti, j);
            }
            return;
        }
    }

    /// Make all the properties of `o` that are not made yet (a listing of all names, a freeze).
    #[inline]
    pub(crate) fn lazy_all(&mut self, o: Obj) {
        if o.has_flag(F_LAZYTAB) {
            self.lazy_all_slow(o);
        }
    }

    #[inline(never)]
    fn lazy_all_slow(&mut self, o: Obj) {
        for ti in 0..self.ltab.tabs.len() {
            if self.ltab.tabs[ti].obj != o {
                continue;
            }
            for j in 0..self.ltab.tabs[ti].ents.len() {
                let at = self.ltab.tabs[ti].base + j;
                if self.ltab.done[at / 64] >> (at % 64) & 1 == 0 {
                    self.lazy_make(ti, j);
                }
            }
        }
        o.clear_flag(F_LAZYTAB);
    }

    /// Make entry `j` of table `ti`.
    #[cold]
    fn lazy_make(&mut self, ti: usize, j: usize) {
        let (o, e, left) = {
            let t = &mut self.ltab.tabs[ti];
            let at = t.base + j;
            self.ltab.done[at / 64] |= 1 << (at % 64);
            t.left -= 1;
            (t.obj, t.ents[j], t.left)
        };
        if left == 0 && !self.ltab.tabs.iter().any(|t| t.obj == o && t.left > 0) {
            o.clear_flag(F_LAZYTAB);
        }
        let ka = self.intern(e.name);
        let k = Key::Name(ka);
        let keep = o == self.global || self.ltab.tabs[ti].has_same;
        let v = match e.kind {
            Kind::Method(f) => self.new_native(f, false, e.len, e.name).value(),
            Kind::Indexed(f, i) => {
                let c = self.new_native(f, false, e.len, e.name);
                c.set_slot(&self.shapes, 1, Value::int(i));
                c.value()
            }
            Kind::Num(n) => return self.define_attrs(o, k, 0, Value::number(n)).expect("define"),
            Kind::Alias => {
                let g = self.global;
                self.lazy_prep(g, ka);
                self.shared_fn(g, e.name)
            }
            Kind::Same(of) => {
                let ko = self.intern(of);
                self.lazy_prep(o, ko);
                self.shared_fn(o, of)
            }
        };
        if keep && !matches!(e.kind, Kind::Alias | Kind::Same(_)) {
            self.ltab.shared.push((o, e.name, v));
            self.keep(v);
        }
        self.define_attrs(o, k, HIDDEN, v).expect("define");
    }

    /// The function that the method `name` of `o` was made as.
    fn shared_fn(&self, o: Obj, name: &str) -> Value {
        self.ltab.shared.iter().find(|(x, n, _)| *x == o && *n == name).expect("the entry that an alias names").2
    }
}

/// Is the text of the atom `k` the ASCII text `s`?
fn same_text(k: Str, s: &str) -> bool {
    matches!(k.chars(), Some(Chars::One(b)) if b == s.as_bytes())
}

#[cfg(test)]
#[path = "lazy_tests.rs"]
mod tests;
