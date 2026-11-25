//! Lazy tables for the properties of built-in objects (`docs/internal/design-js2-lazy-builtins.md`, design B).
//!
//! The methods, the statics and the constants of a built-in object are a `static` table of [`Ent`]. [`Realm::lazy_table`] only
//! records the table; a property is made at the first lookup that needs it, with the same attributes, the same native and the same lazy
//! `length` and `name` as an eager install. So a realm pays for the properties that a script uses, and no more.
//!
//! **The flag.** An object that has a table with entries left has [`F_LAZYTAB`] (`object.rs`). The flag is the only cost on the paths that do not
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
use crate::object::{F_LAZYTAB, Obj, Spot};
use crate::bi_object::Desc;
use crate::string::{Chars, Str};
use crate::value::Value;

/// What a table entry makes.
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    /// A method: a native function, not enumerable, writable and configurable.
    Method(NativeFn),
    /// A native constructor with lazy static members.
    Ctor(NativeFn, &'static [Ent]),
    /// An intrinsic initialized at the first lookup.
    Factory(fn(&mut Realm) -> Value),
    /// A constant: a number, not writable, not enumerable and not configurable.
    Num(f64),
    /// A data property with explicit attributes.
    Data(fn(&mut Realm) -> Value, u8),
    /// A method whose native has `i` in slot 1 (one native for many methods: `Date.prototype`, `bi_date.rs`).
    Indexed(NativeFn, i32),
    /// The global property of this name (`Number.parseInt` is the function `parseInt`), as a method.
    Alias,
    /// The same function as the entry of this name in the same table (`Date.prototype.toGMTString` is `toUTCString`), as a method.
    Same(&'static str),
    /// A method whose key is the well-known symbol `wk[i]` (`bi_symbol.rs`); the name of the entry is the name of the function.
    Sym(NativeFn, usize),
    /// A method that is a top-level function of `prelude.js` of this name (`prelude_pre.rs`): a stub that the first call compiles. The
    /// `length` is the one of the function.
    Script(&'static str),
    /// A method with the key `wk[i]` that is the same function as the entry `of` of the same table (`Array.prototype[Symbol.iterator]`).
    SymSame(usize, &'static str),
    /// An accessor of two natives, not enumerable and configurable: the getter has `length` 0 and the setter 1 (`Object.prototype.__proto__`).
    Acc(NativeFn, NativeFn),
    /// A read-only accessor, optionally keyed by a well-known symbol.
    Getter(NativeFn, Option<usize>, &'static str),
    /// A buffer constructor and its prototype, all made at the first use.
    BufferCtor(i32),
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
#[inline(never)]
pub(crate) const fn m(name: &'static str, len: u8, f: NativeFn) -> Ent {
    Ent { hash: hash(name), name, len, kind: Kind::Method(f) }
}

/// A constructor `name` with the function `length` `len` and static members.
pub(crate) const fn ctor(name: &'static str, len: u8, f: NativeFn, members: &'static [Ent]) -> Ent {
    Ent { hash: hash(name), name, len, kind: Kind::Ctor(f, members) }
}

/// An intrinsic initialized when its property `name` is read.
pub(crate) const fn factory(name: &'static str, f: fn(&mut Realm) -> Value) -> Ent {
    Ent { hash: hash(name), name, len: 0, kind: Kind::Factory(f) }
}

/// A constant `name`.
pub(crate) const fn num(name: &'static str, v: f64) -> Ent {
    Ent { hash: hash(name), name, len: 0, kind: Kind::Num(v) }
}

pub(crate) const fn data(name: &'static str, get: fn(&mut Realm) -> Value, attrs: u8) -> Ent {
    Ent { hash: hash(name), name, len: 0, kind: Kind::Data(get, attrs) }
}

/// A method `name` of the native `f` with `i` in its slot 1.
#[inline(never)]
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

/// A method `name` with the key `wk[i]` (the symbol that `bi_symbol.rs` makes at index `i`), and `length` 0.
pub(crate) const fn sym(name: &'static str, i: usize, f: NativeFn) -> Ent {
    Ent { hash: hash(name), name, len: 0, kind: Kind::Sym(f, i) }
}

/// A method `name` that is the top-level function `top` of `prelude.js`.
pub(crate) const fn script(name: &'static str, top: &'static str) -> Ent {
    Ent { hash: hash(name), name, len: 0, kind: Kind::Script(top) }
}

/// A method with the key `wk[i]` that is the same function as the entry `of` of the same table.
#[inline(never)]
pub(crate) const fn sym_same(name: &'static str, i: usize, of: &'static str) -> Ent {
    Ent { hash: hash(name), name, len: 0, kind: Kind::SymSame(i, of) }
}

/// An accessor `name` of the natives `get` and `set`.
pub(crate) const fn acc(name: &'static str, get: NativeFn, set: NativeFn) -> Ent {
    Ent { hash: hash(name), name, len: 0, kind: Kind::Acc(get, set) }
}

#[inline(never)]
pub(crate) const fn getter(name: &'static str, fn_name: &'static str, f: NativeFn) -> Ent {
    Ent { hash: hash(name), name, len: 0, kind: Kind::Getter(f, None, fn_name) }
}

#[inline(never)]
pub(crate) const fn sym_getter(i: usize, fn_name: &'static str, f: NativeFn) -> Ent {
    Ent { hash: 0, name: "", len: 0, kind: Kind::Getter(f, Some(i), fn_name) }
}

pub(crate) const fn buffer_ctor(name: &'static str, len: u8, id: i32) -> Ent {
    Ent { hash: hash(name), name, len, kind: Kind::BufferCtor(id) }
}

/// Make a cold family's native tables at first use. Materializing the
/// entries here avoids dynamic-loader relocations for their pointer fields.
macro_rules! late_table {
    ($($f:ident($($arg:expr),*)),* $(,)?) => {{
        const N: usize = [$(stringify!($f)),*].len();
        static TABLE: std::sync::OnceLock<[$crate::lazy::Ent; N]> = std::sync::OnceLock::new();
        TABLE.get_or_init(|| [$($f($($arg),*)),*])
    }};
}
pub(crate) use late_table;

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
    /// `%StringIteratorPrototype%` and the shape of its iterators (`bi_string_iter.rs`), made at the first call of `String.prototype[Symbol.iterator]`.
    pub(crate) str_iter: Option<crate::bi_string_iter::StrIter>,
    pub(crate) array_iter_spot: Option<Spot>,
    pub(crate) array_iter_next: Option<(Obj, Str, Value)>,
    /// The entries of kind [`Kind::Sym`] that are not made: the object, the index in `Realm::wk`, the table and the entry.
    syms: Vec<(Obj, usize, usize, usize)>,
    pub(crate) buffer_keys: Option<[Str; 4]>,
    pub(crate) typed_base: Option<Obj>,
}

impl Default for Lazy {
    fn default() -> Self {
        Lazy { tabs: Vec::new(), done: Vec::new(), bits: Vec::new(), shared: Vec::new(), str_iter: None, array_iter_spot: None, array_iter_next: None, syms: Vec::new(), buffer_keys: None, typed_base: None }
    }
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
        let base = l.tabs.last().map_or(0, |t| t.base + t.ents.len());
        l.done.resize((base + ents.len()).div_ceil(64), 0);
        let ti = l.tabs.len();
        for (j, e) in ents.iter().enumerate() {
            l.bits[(e.hash >> 6) as usize & 63] |= 1 << (e.hash & 63);
            if let Kind::Sym(_, i) | Kind::SymSame(i, _) | Kind::Getter(_, Some(i), _) = e.kind {
                l.syms.push((o, i, ti, j));
            }
        }
        l.tabs.push(Tab { obj: o, ents, base, left: ents.len(), has_same: ents.iter().any(|e| matches!(e.kind, Kind::Same(_) | Kind::SymSame(..))) });
        o.set_flag(F_LAZYTAB);
        self.note_home(o);
    }

    /// A test switch: make every entry of every table now, as an eager install did.
    #[cfg(test)]
    pub(crate) fn lazy_everything(&mut self) {
        let mut i = 0;
        while let Some(t) = self.ltab.tabs.get(i) {
            self.lazy_all(t.obj);
            i += 1;
        }
    }

    /// Make the property `k` of `o`, if a table of `o` has it and it is not made yet. Call it where `shapes.find(o's shape, k)` would
    /// be the first look at an own property of `o`.
    #[inline]
    pub(crate) fn lazy_prep(&mut self, o: Obj, k: Str) {
        if o.has_flag(F_LAZYTAB) {
            let home = self.home_realm(o.value());
            self.in_realm(home, |rt| rt.lazy_probe(o, k));
        }
    }

    #[inline(never)]
    fn lazy_probe(&mut self, o: Obj, k: Str) {
        if k.is_symbol() {
            return self.lazy_probe_sym(o, k);
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

    /// The probe for a symbol key: the entries of kind [`Kind::Sym`] that are not made are in `Lazy::syms` (one or two), so a symbol that
    /// is not one of them costs a short scan.
    fn lazy_probe_sym(&mut self, o: Obj, k: Str) {
        let hit = self.ltab.syms.iter().find(|&&(so, wi, _, _)| so == o && self.wk[wi] == k).map(|&(_, _, ti, j)| (ti, j));
        if let Some((ti, j)) = hit {
            self.lazy_make(ti, j);
        }
    }

    /// Make all the properties of `o` that are not made yet (a listing of all names, a freeze).
    #[inline]
    pub(crate) fn lazy_all(&mut self, o: Obj) {
        if o.has_flag(F_LAZYTAB) {
            let home = self.home_realm(o.value());
            self.in_realm(home, |rt| rt.lazy_all_slow(o));
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
            let t = &mut self.state.ltab.tabs[ti];
            let at = t.base + j;
            self.state.ltab.done[at / 64] |= 1 << (at % 64);
            t.left -= 1;
            (t.obj, t.ents[j], t.left)
        };
        if left == 0 && !self.ltab.tabs.iter().any(|t| t.obj == o && t.left > 0) {
            o.clear_flag(F_LAZYTAB);
        }
        let ka = match e.kind {
            Kind::Sym(_, i) | Kind::SymSame(i, _) | Kind::Getter(_, Some(i), _) => {
                self.ltab.syms.retain(|&(_, _, t, n)| (t, n) != (ti, j));
                self.wk[i]
            }
            _ => self.intern(e.name),
        };
        let k = Key::Name(ka);
        let keep = o == self.global || self.ltab.tabs[ti].has_same;
        let v = match e.kind {
            Kind::Method(f) | Kind::Sym(f, _) => self.new_native(f, false, e.len, e.name).value(),
            Kind::Ctor(f, members) => {
                let c = self.new_native(f, true, e.len, e.name);
                self.lazy_table(c, members);
                c.value()
            }
            Kind::Factory(f) => f(self),
            Kind::Indexed(f, i) => {
                let c = self.new_native(f, false, e.len, e.name);
                c.set_slot(&self.shapes, 1, Value::int(i));
                c.value()
            }
            Kind::Num(n) => return self.define_attrs(o, k, 0, Value::number(n)).expect("define"),
            Kind::Data(get, attrs) => {
                let v = get(self);
                return self.define_attrs(o, k, attrs, v).expect("define");
            }
            Kind::Alias => {
                let g = self.global;
                self.lazy_prep(g, ka);
                self.shared_fn(g, e.name).expect("the entry that an alias names")
            }
            Kind::Same(of) | Kind::SymSame(_, of) => {
                let ko = self.intern(of);
                self.lazy_prep(o, ko);
                self.shared_fn(o, of).expect("the entry that an alias names")
            }
            Kind::Script(top) => self.pre_fn(top, e.name).expect("a function of the prelude"),
            Kind::BufferCtor(id) => self.make_buffer_ctor(e.name, e.len, id),
            Kind::Getter(get, _, name) => {
                let f = self.new_native(get, false, 0, name).value();
                self.define_accessor(o, k, f, false, false).expect("define");
                return;
            }
            Kind::Acc(get, set) => {
                let (g, s) = (self.new_native(get, false, 0, "get").value(), self.new_native(set, false, 1, "set").value());
                for (f, prefix) in [(g, 1), (s, 2)] {
                    self.fn_prep(Realm::obj(f).expect("a native function"), self.k_length);
                    self.name_function(f, k, prefix).expect("define the accessor name");
                }
                let d = Desc { get: Some(g), set: Some(s), e: Some(false), c: Some(true), ..Desc::default() };
                self.define_own(o, k, &d).expect("define");
                return;
            }
        };
        if keep && !matches!(e.kind, Kind::Alias | Kind::Same(_) | Kind::SymSame(..) | Kind::Ctor(..)) {
            self.ltab.shared.push((o, e.name, v));
            self.keep(v);
        }
        self.define_attrs(o, k, HIDDEN, v).expect("define");
    }

    /// `%Array.prototype.values%`: made now if it is not, and the same function whatever a script did to the property since. A realm with
    /// no compiler cannot make it (it is a function of the prelude): the property is `undefined` there.
    #[cold]
    pub(crate) fn array_values(&mut self) -> Value {
        if self.lazy.is_none() {
            return Value::UNDEFINED;
        }
        let (ap, k) = (self.array_proto, self.intern("values"));
        self.lazy_prep(ap, k);
        self.shared_fn(ap, "values").expect("the array values entry")
    }

    /// The function that the method `name` of `o` was made as.
    pub(crate) fn shared_fn(&self, o: Obj, name: &str) -> Option<Value> {
        self.ltab.shared.iter().find(|(x, n, _)| *x == o && *n == name).map(|&(_, _, v)| v)
    }
}

/// Is the text of the atom `k` the ASCII text `s`?
fn same_text(k: Str, s: &str) -> bool {
    matches!(k.chars(), Some(Chars::One(b)) if b == s.as_bytes())
}

#[cfg(test)]
#[path = "lazy_tests.rs"]
mod tests;
