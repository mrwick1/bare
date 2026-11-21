//! `Map`, `Set`, `WeakMap` and `WeakSet` (the census group `Sc`, `docs/census/2026-11-18.tsv`). Safe code over [`Realm`]; the storage of
//! the weak ones is `weaktab.rs`. They are installed with the other built-ins (the lazy form of `docs/design-js2-lazy-builtins.md`
//! is not built yet).
//!
//! **The object.** A plain `Object` cell with one hidden slot, `\0cs` (not writable, not configurable, so a script cannot move it), and
//! the kind in the header flags ([`F_KIND`](crate::object::F_KIND): 1 `Map`, 2 `Set`, 3 `WeakMap`, 4 `WeakSet`, 5 and 6 the iterators). An empty `Set` is a
//! 16-byte cell. A class that extends one of them gets the same slot over its own prototype.
//!
//! **`Map` and `Set`: one flat record.** The slot holds a [`Ctx`] (a `Slots` record, scanned by the collector like any record) with this
//! layout, in values:
//!
//! | Index | Content |
//! | --- | --- |
//! | 0 | `FWD`: `undefined`, or the table that replaced this one |
//! | 1 | `STATE`: 0 live, 1 replaced by a copy without the deleted entries, 2 replaced by an empty table (`clear`) |
//! | 2, 3, 4 | `USED` (entries used, deleted ones too), `SIZE` (live entries), `CAP` (entries, a power of two, at least 4) |
//! | 5 .. 5 + CAP/2 | the buckets: the index of the first entry of a chain, or `-1` |
//! | then CAP * W | the entries in insertion order, W = 3 for a `Map` (key, value, next) and 2 for a `Set` (key, next) |
//!
//! A hash chain runs through the `next` field (V8's `OrderedHashMap`): no pointer, no separate cell for an entry, 8 bytes of bucket for
//! two entries. `delete` makes the key the hole and keeps the link, so a chain is never cut. When the entries are full, the table is
//! copied (without the deleted entries) to twice the capacity, or to the same capacity when at most half of it is live. A table that is
//! less than a quarter full after a delete is copied to half the capacity. Keys are `SameValueZero`: a number is normalized (`-0` to `0`,
//! one NaN, an integer as an integer) so equal keys have equal bits, a string is compared by its units (the hash is cached in the
//! string), and any other value by its bits.
//!
//! **Iterators survive a copy.** An iterator (and `forEach`) holds the table it started on and an index into its entries. The copy leaves
//! the old table as it was and sets its `FWD` and `STATE`. A cursor that finds `STATE` not 0 follows `FWD`: after a copy its index drops by
//! the number of deleted entries below it (the old table still shows them), after a `clear` it is 0. So a cursor never skips and never
//! repeats an entry, as the specification says, and a table that no cursor holds is garbage at once.
//!
//! **Weak ones.** The slot holds a [`WeakTab`] (an ephemeron table keyed by address). The collector keeps a value only while its key is
//! alive (`gc.rs`). Only an object is a key: a symbol is a leaf cell, and the collector would never release a value that waits for one.
use crate::lazy::{Ent, m};
use crate::builtins::{NativeFn, arg};
use crate::bi_symbol::{ITERATOR, SPECIES, TO_STRING_TAG};
use crate::cell::Layout;
use crate::heap::Heap;
use crate::interp_rt::{Key, R, Realm};
use crate::object::{Ctx, K_MAP, K_MITER, K_SET, K_SITER, K_WMAP, K_WSET, Obj};
use crate::shapes::{A_C, ShapeId};
use crate::string::Str;
use crate::value::Value;
use crate::weaktab::{WeakTab, mix, weaktab_of};

const FWD: usize = 0;
const STATE: usize = 1;
const USED: usize = 2;
const SIZE: usize = 3;
const CAP: usize = 4;
const HDR: usize = 5;
const MIN_CAP: usize = 4;

/// The state of the collections in a realm: the prototypes and the shapes (a kind has one default shape, pinned), the names of the
/// hidden slots, and the functions that a constructor tests to take a fast path. The prototypes and the functions are roots (`keep`).
pub(crate) struct Colls {
    protos: [Obj; 6],
    shapes: [ShapeId; 6],
    keys: [Str; 3],
    /// `Map.prototype.set`, `Set.prototype.add`, `WeakMap.prototype.set`, `WeakSet.prototype.add`.
    adders: [Value; 4],
    /// `Map.prototype.entries` and `Set.prototype.values`: what `[Symbol.iterator]` is, unless a script changed it.
    iters: [Value; 2],
}

/// The key `v` as a `SameValueZero` key: equal keys have equal bits (strings and BigInts compare by value).
fn norm(v: Value) -> Value {
    match v.as_f64() {
        Some(f) if f.is_nan() => Value::number(f64::NAN),
        // `f + 0.0` is `+0` for `-0` and `f` for the others.
        Some(f) => Realm::num(f + 0.0),
        None => v,
    }
}

// ---- the ordered table of a Map or a Set ----

/// A table of a `Map` (`w` 3) or a `Set` (`w` 2), see the module comment.
#[derive(Clone, Copy)]
struct Tab {
    c: Ctx,
    w: usize,
}

impl Tab {
    fn new(heap: &mut Heap, w: usize, cap: usize) -> Tab {
        let nb = cap / 2;
        let c = Ctx::new(heap, HDR + nb + w * cap, Value::UNDEFINED).expect("memory");
        c.set(STATE, Value::int(0));
        c.set(USED, Value::int(0));
        c.set(SIZE, Value::int(0));
        c.set(CAP, Value::int(cap as i32));
        for b in 0..nb {
            c.set(HDR + b, Value::int(-1));
        }
        Tab { c, w }
    }

    #[inline]
    fn n(self, i: usize) -> usize {
        self.c.get(i).as_int().expect("a count") as usize
    }

    fn used(self) -> usize {
        self.n(USED)
    }

    fn size(self) -> usize {
        self.n(SIZE)
    }

    fn cap(self) -> usize {
        self.n(CAP)
    }

    fn put(self, i: usize, n: usize) {
        self.c.set(i, Value::int(n as i32));
    }

    /// The slot of the bucket of the hash `h`.
    #[inline]
    fn bucket(self, h: u32) -> usize {
        HDR + (h as usize & (self.cap() / 2 - 1))
    }

    /// The slot of the key of entry `e`; the value is next to it in a `Map`, and the link is the last field.
    #[inline]
    fn at(self, e: usize) -> usize {
        HDR + self.cap() / 2 + e * self.w
    }

    fn forward(self) -> Tab {
        Tab { c: Realm::ctx(self.c.get(FWD)).expect("a table"), w: self.w }
    }
}

/// The place in a table: the next entry to look at. It follows the copies of the table (module comment).
struct Cur {
    t: Tab,
    i: usize,
}

impl Cur {
    /// The next live entry: its key and its value (the key again in a `Set`).
    fn next(&mut self) -> Option<(Value, Value)> {
        loop {
            match self.t.n(STATE) {
                0 => break,
                1 => {
                    let t = self.t;
                    self.i -= (0..self.i).filter(|&e| t.c.get(t.at(e)).is_hole()).count();
                }
                _ => self.i = 0,
            }
            self.t = self.t.forward();
        }
        let t = self.t;
        while self.i < t.used() {
            let at = t.at(self.i);
            self.i += 1;
            let k = t.c.get(at);
            if !k.is_hole() {
                return Some((k, if t.w == 3 { t.c.get(at + 1) } else { k }));
            }
        }
        None
    }
}

impl Realm {
    fn colls(&self) -> &Colls {
        self.coll.as_ref().expect("collections are installed")
    }

    fn coll_hash(&mut self, k: Value) -> u32 {
        match Realm::str_of(k) {
            Some(s) => mix(s.hash(&mut self.heap).expect("memory") as u64),
            None => match Realm::big_of(k) {
                Some(b) => mix(crate::bigint::hash(b.bigint()) as u64),
                None => mix(k.bits()),
            },
        }
    }

    fn coll_eq(&mut self, a: Value, b: Value) -> bool {
        a == b || self.strict_eq(a, b)
    }

    /// The slot of the key of the entry for `k` (normalized), if the table has one.
    fn tab_find(&mut self, t: Tab, k: Value, h: u32) -> Option<usize> {
        if Realm::str_of(k).is_none() && !k.is_bigint() {
            // Equal keys have equal bits in this branch. Walk the chain over the slice.
            let w = t.w;
            return t.c.with(|s| {
                let nb = s[CAP].as_int().expect("a count") as usize / 2;
                let mut e = s[HDR + (h as usize & (nb - 1))].as_int().expect("a link");
                while e >= 0 {
                    let at = HDR + nb + e as usize * w;
                    if s[at] == k {
                        return Some(at);
                    }
                    e = s[at + w - 1].as_int().expect("a link");
                }
                None
            });
        }
        let mut e = t.c.get(t.bucket(h)).as_int().expect("a link");
        while e >= 0 {
            let at = t.at(e as usize);
            if self.coll_eq(t.c.get(at), k) {
                return Some(at);
            }
            e = t.c.get(at + t.w - 1).as_int().expect("a link");
        }
        None
    }

    /// Copy the live entries of `t` to a new table of `cap` entries. The old table stays as it was, with its `FWD` and `STATE` set.
    fn tab_rebuild(&mut self, o: Obj, t: Tab, cap: usize) -> Tab {
        let n = Tab::new(&mut self.heap, t.w, cap);
        let mut j = 0;
        for e in 0..t.used() {
            let at = t.at(e);
            let k = t.c.get(at);
            if k.is_hole() {
                continue;
            }
            let h = self.coll_hash(k);
            let nat = n.at(j);
            n.c.set(nat, k);
            if t.w == 3 {
                n.c.set(nat + 1, t.c.get(at + 1));
            }
            let b = n.bucket(h);
            n.c.set(nat + t.w - 1, n.c.get(b));
            n.c.set(b, Value::int(j as i32));
            j += 1;
        }
        n.put(USED, j);
        n.put(SIZE, j);
        t.c.set(FWD, n.c.value());
        t.put(STATE, 1);
        o.set_slot(&self.shapes, 0, n.c.value());
        n
    }

    /// `Map.prototype.set` and `Set.prototype.add` on the table `t` of `o`.
    fn tab_set(&mut self, o: Obj, mut t: Tab, k: Value, v: Value) {
        let k = norm(k);
        let h = self.coll_hash(k);
        if let Some(at) = self.tab_find(t, k, h) {
            if t.w == 3 {
                t.c.set(at + 1, v);
            }
            return;
        }
        let (cap, size) = (t.cap(), t.size());
        if t.used() == cap {
            t = self.tab_rebuild(o, t, if size * 2 > cap { cap * 2 } else { cap });
        }
        let e = t.used();
        let at = t.at(e);
        t.c.set(at, k);
        if t.w == 3 {
            t.c.set(at + 1, v);
        }
        let b = t.bucket(h);
        t.c.set(at + t.w - 1, t.c.get(b));
        t.c.set(b, Value::int(e as i32));
        t.put(USED, e + 1);
        t.put(SIZE, size + 1);
    }

    fn tab_delete(&mut self, o: Obj, t: Tab, k: Value) -> bool {
        let k = norm(k);
        let h = self.coll_hash(k);
        let Some(at) = self.tab_find(t, k, h) else { return false };
        // The key is the hole and the link stays: the chain is not cut.
        t.c.set(at, Value::HOLE);
        if t.w == 3 {
            t.c.set(at + 1, Value::UNDEFINED);
        }
        let (cap, size) = (t.cap(), t.size() - 1);
        t.put(SIZE, size);
        if cap > 2 * MIN_CAP && size * 4 < cap {
            self.tab_rebuild(o, t, cap / 2);
        }
        true
    }

    fn tab_clear(&mut self, o: Obj, t: Tab) {
        if t.used() == 0 {
            return;
        }
        let n = Tab::new(&mut self.heap, t.w, MIN_CAP);
        t.c.set(FWD, n.c.value());
        t.put(STATE, 2);
        o.set_slot(&self.shapes, 0, n.c.value());
    }

    // ---- objects ----

    /// A new collection object of `kind` with `table` in its slot and the prototype of `new.target`.
    fn new_coll(&mut self, kind: u32, table: Value) -> R<Obj> {
        let proto = self.new_proto()?;
        let c = self.colls();
        let i = kind as usize - 1;
        let shape = if proto == c.protos[i] {
            c.shapes[i]
        } else {
            let keys = c.keys;
            self.coll_shape(proto, kind, keys)
        };
        let o = Obj::new(&mut self.heap, &self.shapes, shape).expect("memory");
        o.set_kind(kind);
        o.set_slot(&self.shapes, 0, table);
        Ok(o)
    }

    fn coll_shape(&mut self, proto: Obj, kind: u32, keys: [Str; 3]) -> ShapeId {
        let iter = kind >= K_MITER;
        let mut s = self.shapes.root(Layout::Object, Some(proto.cell()), 1 + iter as u16);
        for k in &keys[iter as usize..iter as usize * 2 + 1] {
            s = self.shapes.add(s, *k, 0).expect("shape");
        }
        s
    }

    /// The table of the `Map` or `Set` `this`, and its object.
    fn this_tab(&mut self, this: Value, kind: u32, what: &str) -> R<(Obj, Tab)> {
        if let Some(o) = Realm::obj(this).filter(|o| o.is_kind(kind))
            && let Some(c) = Realm::ctx(o.slot(&self.shapes, 0))
        {
            return Ok((o, Tab { c, w: if kind == K_MAP { 3 } else { 2 } }));
        }
        self.type_error(&format!("Method {what} called on incompatible receiver"))
    }

    /// Copy live Map or Set entries in insertion order before inspection can run getters.
    pub(crate) fn inspect_collection(&self, o: Obj, limit: usize) -> Option<(bool, Vec<(Value, Value)>)> {
        let kind = o.kind();
        if !matches!(kind, K_MAP | K_SET) {
            return None;
        }
        let c = Realm::ctx(o.slot(&self.shapes, 0))?;
        let is_map = kind == K_MAP;
        let mut cur = Cur { t: Tab { c, w: if is_map { 3 } else { 2 } }, i: 0 };
        let entries = std::iter::from_fn(|| cur.next()).take(limit).collect();
        Some((is_map, entries))
    }

    fn this_weak(&mut self, this: Value, kind: u32, what: &str) -> R<(Obj, WeakTab)> {
        if let Some(o) = Realm::obj(this).filter(|o| o.is_kind(kind))
            && let Some(t) = weaktab_of(o.slot(&self.shapes, 0))
        {
            return Ok((o, t));
        }
        self.type_error(&format!("Method {what} called on incompatible receiver"))
    }

    /// A new iterator over the table `t`; `mode` is 0 for keys, 1 for values and 2 for entries.
    fn new_coll_iter(&mut self, kind: u32, t: Tab, mode: usize) -> Value {
        let c = self.colls();
        let shape = c.shapes[kind as usize - 1];
        let o = Obj::new(&mut self.heap, &self.shapes, shape).expect("memory");
        o.set_kind(kind);
        o.set_slot(&self.shapes, 0, t.c.value());
        o.set_slot(&self.shapes, 1, Value::int(mode as i32));
        o.value()
    }

    /// Run `f` for each value of the iterable `src` (nothing for `undefined` and `null`). An array and a string are walked at once, any other
    /// value through `Symbol.iterator` and `next`. When `f` fails the iterator is closed (`return`) and the error goes on.
    fn each(&mut self, src: Value, f: &mut dyn FnMut(&mut Realm, Value) -> R<()>) -> R<()> {
        if src.is_undefined() || src.is_null() {
            return Ok(());
        }
        if Realm::str_of(src).is_some() || Realm::obj(src).is_some_and(|o| o.is_indexed()) {
            for v in self.iter_values(src)? {
                f(self, v)?;
            }
            return Ok(());
        }
        let itf = self.get_prop(src, Key::Name(self.wk[ITERATOR]))?;
        if !Realm::obj(itf).is_some_and(|o| o.is_callable()) {
            return self.type_error("the value is not iterable");
        }
        let it = self.call_function(itf, src, &[])?;
        if Realm::obj(it).is_none() {
            return self.type_error("Result of the Symbol.iterator method is not an object");
        }
        let next = self.get_named(it, "next")?;
        loop {
            let r = self.call_function(next, it, &[])?;
            if Realm::obj(r).is_none() {
                return self.type_error("Iterator result is not an object");
            }
            let done = self.get_named(r, "done")?;
            if Realm::truthy(done) {
                return Ok(());
            }
            let v = self.get_named(r, "value")?;
            if let Err(e) = f(self, v) {
                // `IteratorClose`: the error of `return` does not replace `e`.
                if let Ok(ret) = self.get_named(it, "return")
                    && Realm::obj(ret).is_some_and(|o| o.is_callable())
                {
                    let _ = self.call_function(ret, it, &[]);
                }
                return Err(e);
            }
        }
    }

    /// The entries of `src` when it is a `Map` or a `Set` whose iteration is the built-in one: (kind, entries). The constructors copy
    /// these with no iterator object and no pair array.
    fn plain_entries(&mut self, src: Value) -> Option<(u32, Vec<(Value, Value)>)> {
        let o = Realm::obj(src)?;
        let kind = o.kind();
        if kind != K_MAP && kind != K_SET {
            return None;
        }
        let want = self.colls().iters[(kind == K_SET) as usize];
        if self.get_prop(src, Key::Name(self.wk[ITERATOR])).ok()? != want {
            return None;
        }
        let t = Tab { c: Realm::ctx(o.slot(&self.shapes, 0))?, w: if kind == K_MAP { 3 } else { 2 } };
        let mut cur = Cur { t, i: 0 };
        Some((kind, std::iter::from_fn(|| cur.next()).collect()))
    }

    /// The adder (`set` or `add`) of `this`, and whether it is the built-in one.
    fn adder(&mut self, this: Value, name: &str, kind: u32) -> R<(Value, bool)> {
        let f = self.get_named(this, name)?;
        if !Realm::obj(f).is_some_and(|o| o.is_callable()) {
            return self.type_error(&format!("'{name}' returned for property '{name}' of object is not a function"));
        }
        Ok((f, f == self.colls().adders[kind as usize - 1]))
    }
}

/// `new Map(iterable)` and `new WeakMap(iterable)`: the entries (items that are `[key, value]`).
fn ctor_pairs(rt: &mut Realm, o: Obj, kind: u32, src: Value) -> R<()> {
    if src.is_undefined() || src.is_null() {
        return Ok(());
    }
    let (adder, builtin) = rt.adder(o.value(), "set", kind)?;
    if builtin && kind == K_MAP
        && let Some((K_MAP, list)) = rt.plain_entries(src)
    {
        for (k, v) in list {
            let t = rt.this_tab(o.value(), K_MAP, "Map")?.1;
            rt.tab_set(o, t, k, v);
        }
        return Ok(());
    }
    rt.each(src, &mut |rt, item| {
        if Realm::obj(item).is_none() {
            return rt.type_error("Iterator value is not an entry object");
        }
        let (k, v) = (rt.get_prop(item, Key::Idx(0))?, rt.get_prop(item, Key::Idx(1))?);
        if builtin { add_pair(rt, o, kind, k, v) } else { rt.call_function(adder, o.value(), &[k, v]).map(|_| ()) }
    })
}

/// `new Set(iterable)` and `new WeakSet(iterable)`.
fn ctor_values(rt: &mut Realm, o: Obj, kind: u32, src: Value) -> R<()> {
    if src.is_undefined() || src.is_null() {
        return Ok(());
    }
    let (adder, builtin) = rt.adder(o.value(), "add", kind)?;
    if builtin && kind == K_SET
        && let Some((K_SET, list)) = rt.plain_entries(src)
    {
        for (k, _) in list {
            let t = rt.this_tab(o.value(), K_SET, "Set")?.1;
            rt.tab_set(o, t, k, Value::UNDEFINED);
        }
        return Ok(());
    }
    rt.each(src, &mut |rt, v| if builtin { add_pair(rt, o, kind, v, Value::UNDEFINED) } else { rt.call_function(adder, o.value(), &[v]).map(|_| ()) })
}

/// Add one entry to the new collection `o` of `kind`.
fn add_pair(rt: &mut Realm, o: Obj, kind: u32, k: Value, v: Value) -> R<()> {
    match kind {
        K_MAP | K_SET => {
            let t = rt.this_tab(o.value(), kind, "set")?.1;
            rt.tab_set(o, t, k, v);
            Ok(())
        }
        _ => weak_put(rt, o, kind, k, v),
    }
}

fn require_new(rt: &mut Realm, name: &str) -> R<()> {
    if Realm::obj(rt.nt).is_none() {
        return rt.type_error(&format!("Constructor {name} requires 'new'"));
    }
    Ok(())
}

fn map_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    require_new(rt, "Map")?;
    let t = Tab::new(&mut rt.heap, 3, MIN_CAP);
    let o = rt.new_coll(K_MAP, t.c.value())?;
    ctor_pairs(rt, o, K_MAP, arg(args, 0))?;
    Ok(o.value())
}

fn set_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    require_new(rt, "Set")?;
    let t = Tab::new(&mut rt.heap, 2, MIN_CAP);
    let o = rt.new_coll(K_SET, t.c.value())?;
    ctor_values(rt, o, K_SET, arg(args, 0))?;
    Ok(o.value())
}

fn weakmap_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    require_new(rt, "WeakMap")?;
    let t = WeakTab::new(&mut rt.heap, 0).expect("memory");
    let o = rt.new_coll(K_WMAP, t.value())?;
    ctor_pairs(rt, o, K_WMAP, arg(args, 0))?;
    Ok(o.value())
}

fn weakset_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    require_new(rt, "WeakSet")?;
    let t = WeakTab::new(&mut rt.heap, 0).expect("memory");
    let o = rt.new_coll(K_WSET, t.value())?;
    ctor_values(rt, o, K_WSET, arg(args, 0))?;
    Ok(o.value())
}

// ---- Map ----

fn map_get(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (_, t) = rt.this_tab(this, K_MAP, "Map.prototype.get")?;
    let k = norm(arg(args, 0));
    let h = rt.coll_hash(k);
    Ok(rt.tab_find(t, k, h).map_or(Value::UNDEFINED, |at| t.c.get(at + 1)))
}

fn map_set(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (o, t) = rt.this_tab(this, K_MAP, "Map.prototype.set")?;
    rt.tab_set(o, t, arg(args, 0), arg(args, 1));
    Ok(this)
}

fn map_has(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (_, t) = rt.this_tab(this, K_MAP, "Map.prototype.has")?;
    let k = norm(arg(args, 0));
    let h = rt.coll_hash(k);
    Ok(Value::bool(rt.tab_find(t, k, h).is_some()))
}

fn map_delete(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (o, t) = rt.this_tab(this, K_MAP, "Map.prototype.delete")?;
    Ok(Value::bool(rt.tab_delete(o, t, arg(args, 0))))
}

fn map_clear(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let (o, t) = rt.this_tab(this, K_MAP, "Map.prototype.clear")?;
    rt.tab_clear(o, t);
    Ok(Value::UNDEFINED)
}

fn map_size(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let (_, t) = rt.this_tab(this, K_MAP, "get Map.prototype.size")?;
    Ok(Value::int(t.size() as i32))
}

fn for_each(rt: &mut Realm, this: Value, args: &[Value], kind: u32, what: &str) -> R<Value> {
    let (_, t) = rt.this_tab(this, kind, what)?;
    let f = arg(args, 0);
    if !Realm::obj(f).is_some_and(|o| o.is_callable()) {
        return rt.type_error("the callback is not a function");
    }
    let mut cur = Cur { t, i: 0 };
    while let Some((k, v)) = cur.next() {
        rt.call_function(f, arg(args, 1), &[v, k, this])?;
    }
    Ok(Value::UNDEFINED)
}

fn map_for_each(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    for_each(rt, this, args, K_MAP, "Map.prototype.forEach")
}

fn map_keys(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let (_, t) = rt.this_tab(this, K_MAP, "Map.prototype.keys")?;
    Ok(rt.new_coll_iter(K_MITER, t, 0))
}

fn map_values(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let (_, t) = rt.this_tab(this, K_MAP, "Map.prototype.values")?;
    Ok(rt.new_coll_iter(K_MITER, t, 1))
}

fn map_entries(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let (_, t) = rt.this_tab(this, K_MAP, "Map.prototype.entries")?;
    Ok(rt.new_coll_iter(K_MITER, t, 2))
}

// ---- Set ----

fn set_add(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (o, t) = rt.this_tab(this, K_SET, "Set.prototype.add")?;
    rt.tab_set(o, t, arg(args, 0), Value::UNDEFINED);
    Ok(this)
}

fn set_has(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (_, t) = rt.this_tab(this, K_SET, "Set.prototype.has")?;
    let k = norm(arg(args, 0));
    let h = rt.coll_hash(k);
    Ok(Value::bool(rt.tab_find(t, k, h).is_some()))
}

fn set_delete(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (o, t) = rt.this_tab(this, K_SET, "Set.prototype.delete")?;
    Ok(Value::bool(rt.tab_delete(o, t, arg(args, 0))))
}

fn set_clear(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let (o, t) = rt.this_tab(this, K_SET, "Set.prototype.clear")?;
    rt.tab_clear(o, t);
    Ok(Value::UNDEFINED)
}

fn set_size(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let (_, t) = rt.this_tab(this, K_SET, "get Set.prototype.size")?;
    Ok(Value::int(t.size() as i32))
}

fn set_for_each(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    for_each(rt, this, args, K_SET, "Set.prototype.forEach")
}

fn set_values(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let (_, t) = rt.this_tab(this, K_SET, "Set.prototype.values")?;
    Ok(rt.new_coll_iter(K_SITER, t, 1))
}

fn set_entries(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let (_, t) = rt.this_tab(this, K_SET, "Set.prototype.entries")?;
    Ok(rt.new_coll_iter(K_SITER, t, 2))
}

// ---- iterators ----

/// `next` of a `Map` or `Set` iterator: the cursor is the table and the packed index (`index << 2 | mode`) in the two slots.
fn iter_next(rt: &mut Realm, this: Value, kind: u32, what: &str) -> R<Value> {
    let Some(o) = Realm::obj(this).filter(|o| o.is_kind(kind)) else {
        return rt.type_error(&format!("{what} called on incompatible receiver"));
    };
    let Some(c) = Realm::ctx(o.slot(&rt.shapes, 0)) else {
        // Done: the iterator dropped its table.
        return Ok(rt.iter_result(Value::UNDEFINED, true));
    };
    let packed = o.slot(&rt.shapes, 1).as_int().expect("a cursor") as usize;
    let mut cur = Cur { t: Tab { c, w: if kind == K_MITER { 3 } else { 2 } }, i: packed >> 2 };
    match cur.next() {
        None => {
            o.set_slot(&rt.shapes, 0, Value::UNDEFINED);
            Ok(rt.iter_result(Value::UNDEFINED, true))
        }
        Some((k, v)) => {
            o.set_slot(&rt.shapes, 0, cur.t.c.value());
            o.set_slot(&rt.shapes, 1, Value::int((cur.i << 2 | packed & 3) as i32));
            let r = match packed & 3 {
                0 => k,
                1 => v,
                _ => rt.new_array(&[k, v]),
            };
            Ok(rt.iter_result(r, false))
        }
    }
}

fn map_iter_next(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    iter_next(rt, this, K_MITER, "Map Iterator.prototype.next")
}

fn set_iter_next(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    iter_next(rt, this, K_SITER, "Set Iterator.prototype.next")
}

fn return_this(_rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    Ok(this)
}

// ---- WeakMap and WeakSet ----

fn weak_key(rt: &mut Realm, k: Value, what: &str) -> R<()> {
    if Realm::obj(k).is_none() {
        return rt.type_error(&format!("Invalid value used as {what}"));
    }
    Ok(())
}

fn weak_put(rt: &mut Realm, o: Obj, kind: u32, k: Value, v: Value) -> R<()> {
    weak_key(rt, k, if kind == K_WMAP { "weak map key" } else { "weak set value" })?;
    let (_, mut t) = rt.this_weak(o.value(), kind, "set")?;
    match t.find(k) {
        Some(i) => t.set_val(i, v),
        None => {
            if t.full() {
                t = t.grown(&mut rt.heap).expect("memory");
                o.set_slot(&rt.shapes, 0, t.value());
            }
            t.add(k, v);
        }
    }
    Ok(())
}

fn weak_get(rt: &mut Realm, this: Value, args: &[Value], kind: u32, what: &str) -> R<Option<(WeakTab, usize)>> {
    let (_, t) = rt.this_weak(this, kind, what)?;
    let k = arg(args, 0);
    Ok(if Realm::obj(k).is_some() { t.find(k).map(|i| (t, i)) } else { None })
}

fn wm_get(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    Ok(weak_get(rt, this, args, K_WMAP, "WeakMap.prototype.get")?.map_or(Value::UNDEFINED, |(t, i)| t.val(i)))
}

fn wm_set(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (o, _) = rt.this_weak(this, K_WMAP, "WeakMap.prototype.set")?;
    weak_put(rt, o, K_WMAP, arg(args, 0), arg(args, 1))?;
    Ok(this)
}

fn wm_has(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    Ok(Value::bool(weak_get(rt, this, args, K_WMAP, "WeakMap.prototype.has")?.is_some()))
}

fn wm_delete(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let hit = weak_get(rt, this, args, K_WMAP, "WeakMap.prototype.delete")?;
    hit.iter().for_each(|(t, i)| t.remove(*i));
    Ok(Value::bool(hit.is_some()))
}

fn ws_add(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (o, _) = rt.this_weak(this, K_WSET, "WeakSet.prototype.add")?;
    weak_put(rt, o, K_WSET, arg(args, 0), Value::UNDEFINED)?;
    Ok(this)
}

fn ws_has(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    Ok(Value::bool(weak_get(rt, this, args, K_WSET, "WeakSet.prototype.has")?.is_some()))
}

fn ws_delete(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let hit = weak_get(rt, this, args, K_WSET, "WeakSet.prototype.delete")?;
    hit.iter().for_each(|(t, i)| t.remove(*i));
    Ok(Value::bool(hit.is_some()))
}

// ---- installation ----

impl Realm {
    pub(crate) fn install_collections(&mut self) {
        let keys = ["\0cs", "\0ct", "\0ci"].map(|n| self.intern(n));
        keys.iter().for_each(|k| self.keep(k.value()));
        let protos: [Obj; 6] = std::array::from_fn(|_| Obj::new(&mut self.heap, &self.shapes, self.s_object).expect("memory"));
        protos.iter().for_each(|p| self.keep(p.value()));
        let shapes: [ShapeId; 6] = std::array::from_fn(|i| self.coll_shape(protos[i], i as u32 + 1, keys));
        self.pins.extend(shapes);
        self.coll = Some(Box::new(Colls { protos, shapes, keys, adders: [Value::UNDEFINED; 4], iters: [Value::UNDEFINED; 2] }));
        let tag = Key::Name(self.wk[TO_STRING_TAG]);
        for (i, name) in ["Map", "Set", "WeakMap", "WeakSet", "Map Iterator", "Set Iterator"].into_iter().enumerate() {
            let v = self.string(name);
            self.define_attrs(protos[i], tag, A_C, v).expect("define");
        }
        let [map, set, wmap, wset, miter, siter] = protos;
        let (mc, sc) = (self.ctor("Map", 0, map_ctor, map), self.ctor("Set", 0, set_ctor, set));
        self.ctor("WeakMap", 0, weakmap_ctor, wmap);
        self.ctor("WeakSet", 0, weakset_ctor, wset);
        for (c, n) in [(mc, "get [Symbol.species]"), (sc, "get [Symbol.species]")] {
            let g = self.new_native(return_this, false, 0, n);
            self.define_accessor(c, Key::Name(self.wk[SPECIES]), g.value(), false, false).expect("define");
        }
        // The functions that the constructors and the iterator aliases hold are made now; the other methods are lazy (`lazy.rs`).
        let mut cells = Vec::new();
        for (o, n, len, f) in [
            (map, "set", 2, map_set as NativeFn),
            (set, "add", 1, set_add),
            (wmap, "set", 2, wm_set),
            (wset, "add", 1, ws_add),
            (map, "entries", 0, map_entries),
            (set, "values", 0, set_values),
        ] {
            self.method(o, n, len, f);
            cells.push(self.get_named(o.value(), n).expect("method"));
        }
        static MAP_PROTO: &[Ent] =
            &[m("get", 1, map_get), m("has", 1, map_has), m("delete", 1, map_delete), m("clear", 0, map_clear), m("forEach", 1, map_for_each), m("keys", 0, map_keys), m("values", 0, map_values)];
        static SET_PROTO: &[Ent] = &[m("has", 1, set_has), m("delete", 1, set_delete), m("clear", 0, set_clear), m("forEach", 1, set_for_each), m("entries", 0, set_entries)];
        static WEAKMAP_PROTO: &[Ent] = &[m("get", 1, wm_get), m("has", 1, wm_has), m("delete", 1, wm_delete)];
        static WEAKSET_PROTO: &[Ent] = &[m("has", 1, ws_has), m("delete", 1, ws_delete)];
        for (o, t) in [(map, MAP_PROTO), (set, SET_PROTO), (wmap, WEAKMAP_PROTO), (wset, WEAKSET_PROTO)] {
            self.lazy_table(o, t);
        }
        for (o, n, len, f) in [(miter, "next", 0, map_iter_next as NativeFn), (siter, "next", 0, set_iter_next)] {
            self.method(o, n, len, f);
        }
        for (o, g, name) in [(map, map_size as NativeFn, "get size"), (set, set_size, "get size")] {
            let f = self.new_native(g, false, 0, name);
            let k = self.intern("size");
            self.define_accessor(o, Key::Name(k), f.value(), false, false).expect("define");
        }
        // `Map.prototype[Symbol.iterator]` is `entries`, and the `Set` ones are `values`; `Set.prototype.keys` is `values` too.
        let (entries, values) = (cells[4], cells[5]);
        let it = Key::Name(self.wk[ITERATOR]);
        self.define_attrs(map, it, crate::builtins::HIDDEN, entries).expect("define");
        self.define_attrs(set, it, crate::builtins::HIDDEN, values).expect("define");
        let k = self.intern("keys");
        self.define_attrs(set, Key::Name(k), crate::builtins::HIDDEN, values).expect("define");
        let me = self.new_native(return_this, false, 0, "[Symbol.iterator]");
        for o in [miter, siter] {
            self.define_attrs(o, it, crate::builtins::HIDDEN, me.value()).expect("define");
        }
        self.keep(me.value());
        let c = self.coll.as_mut().expect("installed");
        c.adders = [cells[0], cells[1], cells[2], cells[3]];
        c.iters = [entries, values];
        cells.iter().for_each(|v| self.consts.push(*v));
    }
}

#[cfg(test)]
#[path = "bi_collections_tests.rs"]
mod tests;
