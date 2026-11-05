// Symbols and the iterator protocol of the evaluator. Included by `eval.rs` (`include!`), so it shares that module's
// imports and private items. `eval_collections.rs` holds Map and Set and uses the helpers here.
//
// Supported
// - `Symbol()`, `Symbol.for`, `Symbol.keyFor`, `Symbol.prototype.{toString, valueOf, description, @@toPrimitive,
//   @@toStringTag}`, `String(symbol)`, `typeof`, symbol property keys, symbol primitive property access.
// - All 13 well-known symbol identities (`Symbol.iterator` ... `Symbol.unscopables`), each a distinct non-writable,
//   non-enumerable, non-configurable property of `Symbol`. They are not registered, so `Symbol.keyFor` gives `undefined`.
// - Protocols that are observable and honored: `@@toPrimitive` (every ToPrimitive of an object), `@@toStringTag`
//   (`Object.prototype.toString`), `@@hasInstance` (`instanceof`, `Function.prototype[@@hasInstance]`), `@@iterator`
//   (for-of, spread, array patterns, Map/Set constructors) with `IteratorClose` on break, continue to an outer
//   label, return, and a thrown body error. Array iterators, `%IteratorPrototype%`.
// - Read by other modules: species (Array, Promise; Map and Set have the getter only), isConcatSpreadable (`concat`),
//   match, replace, split (String methods, see `eval_builtins.rs`). Identity only: asyncIterator, matchAll, search, unscopables.
//
// Gates and deviations (never silent)
// - `String.prototype[@@iterator]` and the string iterator are here (by code point). `Array.prototype.values`,
//   `keys`, `entries` and `[@@iterator]` share one array iterator prototype; `values` is `[@@iterator]`.
// - A fast path skips the iterator object for an array, string, Map or Set whose `@@iterator` and iterator-prototype
//   `next` are the intrinsics and whose iterator prototype chain has no `return` when the loop starts. `next` is read
//   once per loop, as in the spec, so a later change of `next` is not observable. `return` is read when the loop is
//   closed early: the chain is looked at again then, and a `return` that appeared during the loop gets a real
//   iterator object at the loop's current position.
// - IteratorClose is skipped when the loop ends with a limit, Unsupported or a heap fault (JS must not run after a limit).
// - `Object(symbol)` wrapper objects, `Symbol.prototype` on a boxed receiver, and `new.target` are not supported.

/// Which resource a symbol, iterator or collection cap guards. `Error::Limit(LimitKind::Sc(_))`, never a JS value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScLimit {
    /// Entries of the `Symbol.for` registry.
    Registry,
    /// Tracked Map and Set objects.
    Collections,
    /// Tracked WeakMap and WeakSet objects.
    WeakCollections,
    /// Entries of one WeakMap or WeakSet, or of all of them together (`max_total_weak_entries`).
    WeakEntries,
    /// Slots (live and deleted, not yet compacted) of one Map or Set.
    Entries,
    /// Slots of all Maps and Sets together.
    TotalEntries,
    /// Tracked Date objects.
    Dates,
    /// Length of a string given to `Date.parse` or `new Date(string)`.
    DateString,
    /// Tracked iterator objects (Map, Set and array iterators made by a call).
    Iterators,
    /// Values collected by spread arguments, rest patterns and `iter_to_vec`.
    IterBuffer,
}

/// Caps of the symbol, iterator and collection features. Each is checked before the thing grows.
/// Metadata of unreachable collections and iterators is dropped by [`Interp::collect`].
#[derive(Clone, Copy, Debug)]
pub struct ScLimits {
    pub max_registry: usize,
    pub max_collections: usize,
    pub max_entries: usize,
    pub max_total_entries: usize,
    pub max_iterators: usize,
    pub max_iter_buffer: usize,
    pub max_weak_collections: usize,
    pub max_weak_entries: usize,
    pub max_total_weak_entries: usize,
    pub max_dates: usize,
    /// UTF-16 units of one date string. No supported format is longer than a few dozen.
    pub max_date_string: usize,
}

impl Default for ScLimits {
    fn default() -> Self {
        ScLimits { max_registry: 1 << 16, max_collections: 1 << 20, max_entries: 1 << 21, max_total_entries: 1 << 22, max_iterators: 1 << 20, max_iter_buffer: 1 << 21, max_weak_collections: 1 << 20, max_weak_entries: 1 << 21, max_total_weak_entries: 1 << 22, max_dates: 1 << 20, max_date_string: 128 }
    }
}

const WK_NAMES: [&str; 13] = [
    "iterator", "asyncIterator", "hasInstance", "toPrimitive", "toStringTag", "species", "isConcatSpreadable", "match", "matchAll", "replace", "search", "split", "unscopables",
];

// Indexes into `WK_NAMES`.
const WK_SPECIES: usize = 5;
const WK_CONCAT_SPREADABLE: usize = 6;
const WK_MATCH: usize = 7;
const WK_REPLACE: usize = 9;
const WK_SPLIT: usize = 11;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum IterWhat {
    Keys,
    Values,
    Entries,
}

/// Position of an iteration over a Map or Set. It does not hold an index into the entry list: entries have a
/// growing sequence number, so a compaction or a `clear` never moves an iterator. `pos` is a hint valid while
/// the collection `epoch` is the same.
#[derive(Clone, Copy, Default)]
struct Cursor {
    next_seq: u64,
    pos: u32,
    epoch: u64,
}

/// State of one iterator object made by a call (`map.keys()`, `array[Symbol.iterator]()`).
#[derive(Clone, Copy)]
enum IterState {
    Coll { owner: ObjId, what: IterWhat, cur: Cursor, done: bool },
    Array { src: Value, i: u64, what: IterWhat, done: bool },
    Str { s: StrId, i: usize, done: bool },
}

/// How a started iteration steps. `Plain` is an array-like (by index) or a string (by code point).
#[derive(Clone, Copy)]
enum ItKind {
    Plain,
    Coll { owner: ObjId, what: IterWhat, cur: Cursor },
    Generic { iter: Value, next: Value },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ScFn {
    SymbolCtor,
    SymbolFor,
    SymbolKeyFor,
    SymToString,
    SymValueOf,
    SymDescription,
    FnHasInstance,
    Species,
    IterSelf,
    ArrayValues,
    ArrayKeys,
    ArrayEntries,
    ArrayIterNext,
    StrIter,
    StrIterNext,
    MapCtor,
    MapGet,
    MapSet,
    MapHas,
    MapDelete,
    MapClear,
    MapSize,
    MapForEach,
    MapKeys,
    MapValues,
    MapEntries,
    MapIterNext,
    SetCtor,
    SetAdd,
    SetHas,
    SetDelete,
    SetClear,
    SetSize,
    SetForEach,
    SetValues,
    SetEntries,
    SetIterNext,
    WeakMapCtor,
    WeakMapGet,
    WeakMapSet,
    WeakMapHas,
    WeakMapDelete,
    WeakSetCtor,
    WeakSetAdd,
    WeakSetHas,
    WeakSetDelete,
}

impl ScFn {
    fn is_ctor(&self) -> bool {
        matches!(self, ScFn::MapCtor | ScFn::SetCtor | ScFn::WeakMapCtor | ScFn::WeakSetCtor)
    }
}

struct ScState {
    limits: ScLimits,
    /// `Symbol.for` registry: key string to symbol. Strings are interned, so a `StrId` is the key text.
    registry: FxMap<StrId, SymId>,
    wk: Vec<SymId>,
    symbol_proto: ObjId,
    iter_proto: ObjId,
    array_iter_proto: ObjId,
    map_proto: ObjId,
    set_proto: ObjId,
    map_iter_proto: ObjId,
    set_iter_proto: ObjId,
    symbol_ctor: ObjId,
    f_array_values: ObjId,
    f_array_next: ObjId,
    string_iter_proto: ObjId,
    f_string_iter: ObjId,
    f_string_next: ObjId,
    f_map_entries: ObjId,
    f_map_values: ObjId,
    f_map_keys: ObjId,
    f_map_next: ObjId,
    f_map_set: ObjId,
    f_set_values: ObjId,
    f_set_entries: ObjId,
    f_set_next: ObjId,
    f_set_add: ObjId,
    f_has_instance: ObjId,
    k_iterator: PropertyKey,
    k_to_primitive: PropertyKey,
    k_to_string_tag: PropertyKey,
    k_has_instance: PropertyKey,
    k_next: PropertyKey,
    k_done: PropertyKey,
    k_return: PropertyKey,
    k_set: PropertyKey,
    k_add: PropertyKey,
    hints: [Value; 3],
    colls: FxMap<ObjId, Coll>,
    iters: FxMap<ObjId, IterState>,
    total_entries: usize,
}

impl ScState {
    fn new(o: ObjId, k: PropertyKey) -> ScState {
        ScState {
            limits: ScLimits::default(),
            registry: FxMap::default(),
            wk: Vec::new(),
            symbol_proto: o,
            iter_proto: o,
            array_iter_proto: o,
            map_proto: o,
            set_proto: o,
            map_iter_proto: o,
            set_iter_proto: o,
            symbol_ctor: o,
            f_array_values: o,
            f_array_next: o,
            string_iter_proto: o,
            f_string_iter: o,
            f_string_next: o,
            f_map_entries: o,
            f_map_values: o,
            f_map_keys: o,
            f_map_next: o,
            f_map_set: o,
            f_set_values: o,
            f_set_entries: o,
            f_set_next: o,
            f_set_add: o,
            f_has_instance: o,
            k_iterator: k,
            k_to_primitive: k,
            k_to_string_tag: k,
            k_has_instance: k,
            k_next: k,
            k_done: k,
            k_return: k,
            k_set: k,
            k_add: k,
            hints: [Value::Undefined; 3],
            colls: FxMap::default(),
            iters: FxMap::default(),
            total_entries: 0,
        }
    }

    fn objects(&self) -> [ObjId; 23] {
        [
            self.symbol_proto, self.iter_proto, self.array_iter_proto, self.map_proto, self.set_proto, self.map_iter_proto, self.set_iter_proto, self.symbol_ctor,
            self.f_array_values, self.f_array_next, self.string_iter_proto, self.f_string_iter, self.f_string_next, self.f_map_entries, self.f_map_values, self.f_map_keys, self.f_map_next, self.f_map_set, self.f_set_values,
            self.f_set_entries, self.f_set_next, self.f_set_add, self.f_has_instance,
        ]
    }
}

impl<'p> Interp<'p> {
    // ------------------------------------------------------------ host API

    /// A well-known symbol (an index of `WK_NAMES`) as a property key.
    fn wk_key(&self, i: usize) -> PropertyKey {
        PropertyKey::Symbol(self.sc.wk[i])
    }

    pub fn set_sc_limits(&mut self, l: ScLimits) {
        self.sc.limits = l;
    }

    /// Maps, Sets and iterator objects the evaluator tracks (until `collect` drops the unreachable ones).
    pub fn sc_tracked(&self) -> (usize, usize) {
        (self.sc.colls.len(), self.sc.iters.len())
    }

    /// Entry slots (live and deleted) of all tracked Maps and Sets.
    pub fn sc_entry_slots(&self) -> usize {
        self.sc.total_entries
    }

    // ------------------------------------------------------------ GC

    /// Values the symbol and collection features always own. Seeds both `roots` and `collect`.
    fn sc_roots(&self, out: &mut Vec<Value>) {
        out.extend(self.sc.objects().iter().map(|&o| Value::Object(o)));
        out.extend(self.sc.wk.iter().map(|&s| Value::Symbol(s)));
        for (&k, &s) in &self.sc.registry {
            out.push(Value::String(k));
            out.push(Value::Symbol(s));
        }
        for k in [self.sc.k_iterator, self.sc.k_to_primitive, self.sc.k_to_string_tag, self.sc.k_has_instance, self.sc.k_next, self.sc.k_done, self.sc.k_return, self.sc.k_set, self.sc.k_add] {
            match k {
                PropertyKey::String(s) => out.push(Value::String(s)),
                PropertyKey::Symbol(s) => out.push(Value::Symbol(s)),
                PropertyKey::Index(_) => {}
            }
        }
        out.extend(self.sc.hints);
    }

    /// What one tracked object keeps alive: a collection's keys and values, an iterator's owner or source.
    fn sc_trace(&self, o: ObjId, work: &mut Vec<Value>) {
        if let Some(c) = self.sc.colls.get(&o) {
            for e in c.entries.iter().filter(|e| e.live) {
                work.push(e.key);
                if c.is_map {
                    work.push(e.val);
                }
            }
        }
        match self.sc.iters.get(&o) {
            Some(IterState::Coll { owner, .. }) => work.push(Value::Object(*owner)),
            Some(IterState::Array { src, .. }) => work.push(*src),
            Some(IterState::Str { s, .. }) => work.push(Value::String(*s)),
            None => {}
        }
    }

    /// Every tracked object and what it holds (`roots` is complete at any time, like the Promise map).
    fn sc_all_values(&self, out: &mut Vec<Value>) {
        for &o in self.sc.colls.keys().chain(self.sc.iters.keys()) {
            out.push(Value::Object(o));
            self.sc_trace(o, out);
        }
    }

    fn sc_prune(&mut self, seen: &FxSet<ObjId>) {
        self.sc.colls.retain(|o, _| seen.contains(o));
        self.sc.iters.retain(|o, _| seen.contains(o));
        self.sc.total_entries = self.sc.colls.values().map(|c| c.entries.len()).sum();
    }

    /// Add `n` units of work (a native traversal that is not a step per element).
    fn charge_steps(&mut self, n: u64) -> Res<()> {
        self.steps = self.steps.saturating_add(n);
        if self.steps > self.step_limit {
            return Err(Error::Limit(LimitKind::Steps));
        }
        Ok(())
    }

    // ------------------------------------------------------------ install

    fn sc_native(&mut self, f: ScFn, name: &str, len: u32) -> Res<ObjId> {
        self.native(Nat::Sc(f), name, len)
    }

    fn sc_method(&mut self, on: ObjId, name: &str, f: ScFn, len: u32) -> Res<ObjId> {
        self.method(on, name, Nat::Sc(f), len)
    }

    /// A function stored under a symbol key (non-enumerable).
    fn sc_sym_method(&mut self, on: ObjId, key: PropertyKey, name: &str, f: ScFn, len: u32, writable: bool) -> Res<ObjId> {
        let fo = self.sc_native(f, name, len)?;
        self.def(on, key, Value::Object(fo), writable, false, true)?;
        Ok(fo)
    }

    fn sc_getter(&mut self, on: ObjId, key: PropertyKey, name: &str, f: ScFn) -> Res<()> {
        let g = self.sc_native(f, name, 0)?;
        let d = PropertyDescriptor { get: Some(Some(g)), enumerable: Some(false), configurable: Some(true), ..Default::default() };
        h!(self, self.heap.define_own_property(on, key, &d));
        Ok(())
    }

    fn sc_global(&mut self, name: &str, ctor: ObjId, proto: ObjId) -> Res<()> {
        self.def(ctor, self.k_prototype, Value::Object(proto), false, false, false)?;
        self.def(proto, self.k_constructor, Value::Object(ctor), true, false, true)?;
        let k = h!(self, self.heap.key_from_str(name));
        self.def(self.global, k, Value::Object(ctor), true, false, true)
    }

    fn sc_tag(&mut self, on: ObjId, tag: &str) -> Res<()> {
        let s = h!(self, self.heap.intern_str(tag));
        self.def(on, self.sc.k_to_string_tag, Value::String(s), false, false, true)
    }

    fn install_sc(&mut self) -> Res<()> {
        let (op, fp, ap) = (self.object_proto, self.function_proto, self.array_proto);
        let k = self.k_length;
        self.sc = ScState::new(op, k);
        // keys and the well-known symbols
        let mut wk = Vec::new();
        for n in WK_NAMES {
            let d = h!(self, self.heap.intern_str(&format!("Symbol.{n}")));
            wk.push(h!(self, self.heap.new_symbol(Some(d))));
        }
        let ks = ["next", "done", "return", "set", "add"].map(|s| self.heap.key_from_str(s));
        let [k_next, k_done, k_return, k_set, k_add] = ks;
        self.sc.k_next = h!(self, k_next);
        self.sc.k_done = h!(self, k_done);
        self.sc.k_return = h!(self, k_return);
        self.sc.k_set = h!(self, k_set);
        self.sc.k_add = h!(self, k_add);
        self.sc.k_iterator = PropertyKey::Symbol(wk[0]);
        self.sc.k_has_instance = PropertyKey::Symbol(wk[2]);
        self.sc.k_to_primitive = PropertyKey::Symbol(wk[3]);
        self.sc.k_to_string_tag = PropertyKey::Symbol(wk[4]);
        self.sc.wk = wk.clone();
        for (i, n) in ["default", "number", "string"].iter().enumerate() {
            self.sc.hints[i] = Value::String(h!(self, self.heap.intern_str(n)));
        }
        // Symbol
        let sproto = h!(self, self.heap.new_object(Some(op)));
        let sctor = self.sc_native(ScFn::SymbolCtor, "Symbol", 0)?;
        self.sc.symbol_proto = sproto;
        self.sc.symbol_ctor = sctor;
        self.sc_global("Symbol", sctor, sproto)?;
        self.sc_method(sctor, "for", ScFn::SymbolFor, 1)?;
        self.sc_method(sctor, "keyFor", ScFn::SymbolKeyFor, 1)?;
        for (i, n) in WK_NAMES.iter().enumerate() {
            let key = h!(self, self.heap.key_from_str(n));
            self.def(sctor, key, Value::Symbol(wk[i]), false, false, false)?;
        }
        self.sc_method(sproto, "toString", ScFn::SymToString, 0)?;
        self.sc_method(sproto, "valueOf", ScFn::SymValueOf, 0)?;
        let desc = h!(self, self.heap.key_from_str("description"));
        self.sc_getter(sproto, desc, "get description", ScFn::SymDescription)?;
        self.sc_sym_method(sproto, self.sc.k_to_primitive, "[Symbol.toPrimitive]", ScFn::SymValueOf, 1, false)?;
        self.sc_tag(sproto, "Symbol")?;
        let species = PropertyKey::Symbol(wk[5]);
        // Function.prototype[@@hasInstance]
        self.sc.f_has_instance = self.sc_sym_method(fp, self.sc.k_has_instance, "[Symbol.hasInstance]", ScFn::FnHasInstance, 1, false)?;
        // %IteratorPrototype% and the array iterator
        let ip = h!(self, self.heap.new_object(Some(op)));
        self.sc.iter_proto = ip;
        self.sc_sym_method(ip, self.sc.k_iterator, "[Symbol.iterator]", ScFn::IterSelf, 0, true)?;
        let aip = h!(self, self.heap.new_object(Some(ip)));
        self.sc.array_iter_proto = aip;
        self.sc.f_array_next = self.sc_method(aip, "next", ScFn::ArrayIterNext, 0)?;
        self.sc_tag(aip, "Array Iterator")?;
        let av = self.sc_native(ScFn::ArrayValues, "values", 0)?;
        self.sc.f_array_values = av;
        self.def(ap, self.sc.k_iterator, Value::Object(av), true, false, true)?;
        // `values` is the same function object as `[Symbol.iterator]`, as in the spec.
        let kv = h!(self, self.heap.key_from_str("values"));
        self.def(ap, kv, Value::Object(av), true, false, true)?;
        self.sc_method(ap, "keys", ScFn::ArrayKeys, 0)?;
        self.sc_method(ap, "entries", ScFn::ArrayEntries, 0)?;
        // String.prototype[@@iterator] and the string iterator
        let sip = h!(self, self.heap.new_object(Some(ip)));
        self.sc.string_iter_proto = sip;
        self.sc.f_string_next = self.sc_method(sip, "next", ScFn::StrIterNext, 0)?;
        self.sc_tag(sip, "String Iterator")?;
        self.sc.f_string_iter = self.sc_sym_method(self.ext.string_proto, self.sc.k_iterator, "[Symbol.iterator]", ScFn::StrIter, 0, true)?;
        // Map
        let mproto = h!(self, self.heap.new_object(Some(op)));
        let mctor = self.sc_native(ScFn::MapCtor, "Map", 0)?;
        self.sc.map_proto = mproto;
        self.sc_global("Map", mctor, mproto)?;
        self.sc_getter(mctor, species, "get [Symbol.species]", ScFn::Species)?;
        for (n, f, l) in [("get", ScFn::MapGet, 1), ("set", ScFn::MapSet, 2), ("has", ScFn::MapHas, 1), ("delete", ScFn::MapDelete, 1), ("clear", ScFn::MapClear, 0), ("forEach", ScFn::MapForEach, 1)] {
            let f = self.sc_method(mproto, n, f, l)?;
            if n == "set" {
                self.sc.f_map_set = f;
            }
        }
        let size = h!(self, self.heap.key_from_str("size"));
        self.sc_getter(mproto, size, "get size", ScFn::MapSize)?;
        self.sc.f_map_keys = self.sc_method(mproto, "keys", ScFn::MapKeys, 0)?;
        self.sc.f_map_values = self.sc_method(mproto, "values", ScFn::MapValues, 0)?;
        let me = self.sc_method(mproto, "entries", ScFn::MapEntries, 0)?;
        self.sc.f_map_entries = me;
        self.def(mproto, self.sc.k_iterator, Value::Object(me), true, false, true)?;
        self.sc_tag(mproto, "Map")?;
        let mip = h!(self, self.heap.new_object(Some(ip)));
        self.sc.map_iter_proto = mip;
        self.sc.f_map_next = self.sc_method(mip, "next", ScFn::MapIterNext, 0)?;
        self.sc_tag(mip, "Map Iterator")?;
        // Set
        let tproto = h!(self, self.heap.new_object(Some(op)));
        let tctor = self.sc_native(ScFn::SetCtor, "Set", 0)?;
        self.sc.set_proto = tproto;
        self.sc_global("Set", tctor, tproto)?;
        self.sc_getter(tctor, species, "get [Symbol.species]", ScFn::Species)?;
        for (n, f, l) in [("add", ScFn::SetAdd, 1), ("has", ScFn::SetHas, 1), ("delete", ScFn::SetDelete, 1), ("clear", ScFn::SetClear, 0), ("forEach", ScFn::SetForEach, 1)] {
            let f = self.sc_method(tproto, n, f, l)?;
            if n == "add" {
                self.sc.f_set_add = f;
            }
        }
        self.sc_getter(tproto, size, "get size", ScFn::SetSize)?;
        let sv = self.sc_method(tproto, "values", ScFn::SetValues, 0)?;
        self.sc.f_set_values = sv;
        let keys = h!(self, self.heap.key_from_str("keys"));
        self.def(tproto, keys, Value::Object(sv), true, false, true)?;
        self.def(tproto, self.sc.k_iterator, Value::Object(sv), true, false, true)?;
        self.sc.f_set_entries = self.sc_method(tproto, "entries", ScFn::SetEntries, 0)?;
        self.sc_tag(tproto, "Set")?;
        let sip = h!(self, self.heap.new_object(Some(ip)));
        self.sc.set_iter_proto = sip;
        self.sc.f_set_next = self.sc_method(sip, "next", ScFn::SetIterNext, 0)?;
        self.sc_tag(sip, "Set Iterator")?;
        self.install_weak()
    }

    // ------------------------------------------------------------ dispatch

    fn call_sc(&mut self, f: ScFn, this: Value, args: &[Value], is_new: bool) -> Res<Value> {
        let a0 = arg(args, 0);
        match f {
            ScFn::SymbolCtor => {
                let d = if matches!(a0, Value::Undefined) { None } else { Some(self.coerce_string(a0)?) };
                Ok(Value::Symbol(h!(self, self.heap.new_symbol(d))))
            }
            ScFn::SymbolFor => {
                let key = self.coerce_string(a0)?;
                if let Some(&s) = self.sc.registry.get(&key) {
                    return Ok(Value::Symbol(s));
                }
                if self.sc.registry.len() >= self.sc.limits.max_registry {
                    return Err(Error::Limit(LimitKind::Sc(ScLimit::Registry)));
                }
                let s = h!(self, self.heap.new_symbol(Some(key)));
                self.sc.registry.insert(key, s);
                Ok(Value::Symbol(s))
            }
            ScFn::SymbolKeyFor => {
                let Value::Symbol(s) = a0 else {
                    let t = self.display(a0);
                    return Err(self.type_err(&format!("{t} is not a symbol")));
                };
                let d = h!(self, self.heap.symbol_description(s));
                Ok(match d {
                    Some(d) if self.sc.registry.get(&d) == Some(&s) => Value::String(d),
                    _ => Value::Undefined,
                })
            }
            ScFn::SymToString => {
                let s = self.this_symbol(this, "Symbol.prototype.toString")?;
                Ok(Value::String(self.symbol_descriptive(s)?))
            }
            ScFn::SymValueOf => Ok(Value::Symbol(self.this_symbol(this, "Symbol.prototype.valueOf")?)),
            ScFn::SymDescription => {
                let s = self.this_symbol(this, "Symbol.prototype.description")?;
                Ok(h!(self, self.heap.symbol_description(s)).map_or(Value::Undefined, Value::String))
            }
            ScFn::FnHasInstance => Ok(Value::Bool(self.ordinary_has_instance(this, a0)?)),
            ScFn::Species => Ok(this),
            ScFn::IterSelf => Ok(this),
            ScFn::ArrayValues | ScFn::ArrayKeys | ScFn::ArrayEntries => {
                if is_nullish(this) {
                    return Err(self.type_err("Array.prototype.values called on null or undefined"));
                }
                let what = match f {
                    ScFn::ArrayKeys => IterWhat::Keys,
                    ScFn::ArrayEntries => IterWhat::Entries,
                    _ => IterWhat::Values,
                };
                self.sc_array_iter(this, what)
            }
            ScFn::ArrayIterNext => self.array_iter_next(this),
            ScFn::StrIter => {
                if is_nullish(this) {
                    return Err(self.type_err("String.prototype[Symbol.iterator] called on null or undefined"));
                }
                let s = self.coerce_string(this)?;
                self.sc_track_iter(self.sc.string_iter_proto, IterState::Str { s, i: 0, done: false })
            }
            ScFn::StrIterNext => self.str_iter_next(this),
            _ if f.is_weak() => self.call_weak(f, this, args, is_new),
            _ => self.call_coll(f, this, args, is_new),
        }
    }

    fn this_symbol(&mut self, this: Value, what: &str) -> Res<SymId> {
        match this {
            Value::Symbol(s) => Ok(s),
            _ => Err(self.type_err(&format!("{what} requires that 'this' be a Symbol"))),
        }
    }

    /// `Symbol(description)` as a string.
    fn symbol_descriptive(&mut self, s: SymId) -> Res<StrId> {
        let mut out: Vec<u16> = "Symbol(".encode_utf16().collect();
        if let Some(d) = h!(self, self.heap.symbol_description(s)) {
            self.append_string(&mut out, h!(self, self.heap.str_units(d)))?;
        }
        self.append_string(&mut out, &[b')' as u16])?;
        Ok(h!(self, self.heap.intern_vec(out)))
    }

    /// Text of a symbol for messages and tools. A stale symbol prints as `Symbol()`.
    fn sym_text(&self, s: SymId) -> String {
        let d = self.heap.symbol_description(s).ok().flatten().and_then(|d| self.heap.to_string_lossy(d).ok());
        format!("Symbol({})", d.unwrap_or_default())
    }

    /// The `name` of a function stored under a symbol key: `[description]`, or `""`.
    fn symbol_fn_name(&mut self, s: SymId) -> Res<StrId> {
        let Some(d) = h!(self, self.heap.symbol_description(s)) else { return Ok(h!(self, self.heap.intern_str(""))) };
        let mut out: Vec<u16> = vec![b'[' as u16];
        self.append_string(&mut out, h!(self, self.heap.str_units(d)))?;
        self.append_string(&mut out, &[b']' as u16])?;
        Ok(h!(self, self.heap.intern_vec(out)))
    }

    // ------------------------------------------------------------ protocols that read a well-known symbol

    /// `ToPrimitive` step 2: `@@toPrimitive`. `None` means use the ordinary `valueOf`/`toString` order.
    fn sc_to_primitive(&mut self, v: Value, hint: Hint) -> Res<Option<Value>> {
        let m = self.get_prop(v, self.sc.k_to_primitive)?;
        if is_nullish(m) {
            return Ok(None);
        }
        if !self.is_callable(m) {
            return Err(self.type_err("Symbol.toPrimitive is not a function"));
        }
        let h = self.sc.hints[match hint { Hint::Default => 0, Hint::Number => 1, Hint::String => 2 }];
        let r = self.call_fn(m, v, &[h])?;
        if matches!(r, Value::Object(_)) {
            return Err(self.type_err("Cannot convert object to primitive value"));
        }
        Ok(Some(r))
    }

    /// `Object.prototype.toString` with `@@toStringTag`. `tag` is the builtin tag.
    fn sc_object_to_string(&mut self, this: Value, tag: &str) -> Res<Value> {
        let custom = if is_nullish(this) { Value::Undefined } else { self.get_prop(this, self.sc.k_to_string_tag)? };
        let mut out: Vec<u16> = "[object ".encode_utf16().collect();
        match custom {
            Value::String(s) => self.append_string(&mut out, h!(self, self.heap.str_units(s)))?,
            _ => self.append_string(&mut out, &tag.encode_utf16().collect::<Vec<u16>>())?,
        }
        self.append_string(&mut out, &[b']' as u16])?;
        Ok(Value::String(h!(self, self.heap.intern_vec(out))))
    }

    /// `a instanceof b`: `@@hasInstance`, then the ordinary algorithm.
    fn sc_instanceof(&mut self, a: Value, b: Value) -> Res<Value> {
        let Value::Object(_) = b else { return Err(self.type_err("Right-hand side of 'instanceof' is not an object")) };
        let h = self.get_prop(b, self.sc.k_has_instance)?;
        if let Value::Object(ho) = h {
            if ho != self.sc.f_has_instance {
                if !self.is_callable(h) {
                    return Err(self.type_err("Symbol.hasInstance is not a function"));
                }
                let r = self.call_fn(h, b, &[a])?;
                return Ok(Value::Bool(self.truthy(r)?));
            }
        } else if !is_nullish(h) {
            return Err(self.type_err("Symbol.hasInstance is not a function"));
        }
        if !self.is_callable(b) {
            return Err(self.type_err("Right-hand side of 'instanceof' is not callable"));
        }
        Ok(Value::Bool(self.ordinary_has_instance(b, a)?))
    }

    /// `OrdinaryHasInstance(c, o)`. A bound function asks its target, as `instanceof` does.
    fn ordinary_has_instance(&mut self, c: Value, o: Value) -> Res<bool> {
        if !self.is_callable(c) {
            return Ok(false);
        }
        if let Value::Object(co) = c
            && let Some(Callee::Bound { target, .. }) = self.funcs.get(&co)
        {
            let t = Value::Object(*target);
            let r = self.descend(|it| it.sc_instanceof(o, t))?;
            return self.truthy(r);
        }
        let Value::Object(mut o) = o else { return Ok(false) };
        let Value::Object(callee_proto) = self.get_prop(c, self.k_prototype)? else {
            return Err(self.type_err("Function has non-object prototype in instanceof check"));
        };
        while let Some(p) = h!(self, self.heap.get_prototype_of(o)) {
            self.tick()?;
            if p == callee_proto {
                return Ok(true);
            }
            o = p;
        }
        Ok(false)
    }

    // ------------------------------------------------------------ iterator objects

    fn sc_iter_result(&mut self, value: Value, done: bool) -> Res<Value> {
        let r = h!(self, self.heap.new_object(Some(self.object_proto)));
        self.def(r, self.k_value, value, true, true, true)?;
        self.def(r, self.sc.k_done, Value::Bool(done), true, true, true)?;
        Ok(Value::Object(r))
    }

    fn sc_track_iter(&mut self, proto: ObjId, st: IterState) -> Res<Value> {
        if self.sc.iters.len() >= self.sc.limits.max_iterators {
            return Err(Error::Limit(LimitKind::Sc(ScLimit::Iterators)));
        }
        let o = h!(self, self.heap.new_object(Some(proto)));
        self.sc.iters.insert(o, st);
        Ok(Value::Object(o))
    }

    /// An array iterator object over `src` (shared with `Array.prototype.values/keys/entries`).
    fn sc_array_iter(&mut self, src: Value, what: IterWhat) -> Res<Value> {
        self.sc_track_iter(self.sc.array_iter_proto, IterState::Array { src, i: 0, what, done: false })
    }

    fn array_iter_next(&mut self, this: Value) -> Res<Value> {
        let Value::Object(o) = this else { return Err(self.type_err("next method called on incompatible receiver")) };
        match self.array_iter_step(o)? {
            None => self.sc_iter_result(Value::Undefined, true),
            Some(v) => self.sc_iter_result(v, false),
        }
    }

    /// One step of an array iterator object: the next value, or `None` when it is done.
    fn array_iter_step(&mut self, o: ObjId) -> Res<Option<Value>> {
        let Some(&IterState::Array { src, i, what, done }) = self.sc.iters.get(&o) else {
            return Err(self.type_err("next method called on incompatible receiver"));
        };
        if done {
            return Ok(None);
        }
        // A typed array has its own length: the spec does not read `length` for it (not even through
        // `Array.prototype.values.call`).
        let len = match src {
            Value::Object(so) if let Some((_, _, l)) = self.heap.view_parts(so) => l as u64,
            _ => self.length_of(src)?,
        };
        let save = |it: &mut Self, i: u64, done: bool| {
            if let Some(IterState::Array { i: si, done: sd, .. }) = it.sc.iters.get_mut(&o) {
                *si = i;
                *sd = done;
            }
        };
        if i >= len {
            save(self, i, true);
            return Ok(None);
        }
        let v = match what {
            IterWhat::Keys => Value::Number(i as f64),
            _ => {
                let key = self.index_key(i)?;
                let x = self.get_prop(src, key)?;
                if what == IterWhat::Entries { self.new_array_from(&[Value::Number(i as f64), x])? } else { x }
            }
        };
        save(self, i + 1, false);
        Ok(Some(v))
    }

    fn str_iter_next(&mut self, this: Value) -> Res<Value> {
        let Value::Object(o) = this else { return Err(self.type_err("next method called on incompatible receiver")) };
        match self.str_iter_step(o)? {
            None => self.sc_iter_result(Value::Undefined, true),
            Some(v) => self.sc_iter_result(v, false),
        }
    }

    fn str_iter_step(&mut self, o: ObjId) -> Res<Option<Value>> {
        let Some(&IterState::Str { s, i, done }) = self.sc.iters.get(&o) else { return Err(self.type_err("next method called on incompatible receiver")) };
        let step = if done { None } else { self.str_step(s, i)? };
        let Some((v, n)) = step else {
            if let Some(IterState::Str { done, .. }) = self.sc.iters.get_mut(&o) {
                *done = true;
            }
            return Ok(None);
        };
        if let Some(IterState::Str { i: si, .. }) = self.sc.iters.get_mut(&o) {
            *si = i + n;
        }
        Ok(Some(v))
    }

    /// `next` of an iterator object that this module made, when `next` is the intrinsic one for that state. The
    /// result object would be unobservable (a fresh object that the loop reads at once), so it is not made.
    /// `None` means: call `next` the ordinary way (a user method, or a receiver of the wrong kind, which throws).
    fn iter_native_step(&mut self, io: ObjId, no: ObjId) -> Res<Option<Option<Value>>> {
        let s = &self.sc;
        let coll_is_map = |c: Option<&IterState>| match c {
            Some(IterState::Coll { owner, .. }) => s.colls.get(owner).map(|c| c.is_map),
            _ => None,
        };
        let st = s.iters.get(&io);
        if no == s.f_array_next && matches!(st, Some(IterState::Array { .. })) {
            return self.array_iter_step(io).map(Some);
        }
        if no == s.f_string_next && matches!(st, Some(IterState::Str { .. })) {
            return self.str_iter_step(io).map(Some);
        }
        if no == s.f_map_next && coll_is_map(st) == Some(true) {
            return self.coll_iter_step(io, true).map(Some);
        }
        if no == s.f_set_next && coll_is_map(st) == Some(false) {
            return self.coll_iter_step(io, false).map(Some);
        }
        Ok(None)
    }

    /// The code point of `s` at `i` as a string, and its length in code units.
    fn str_step(&mut self, s: StrId, i: usize) -> Res<Option<(Value, usize)>> {
        let Some((_, n)) = h!(self, self.heap.code_point_at(s, i)) else { return Ok(None) };
        let r = h!(self, self.heap.substring(s, i, i + n as usize));
        Ok(Some((Value::String(r), n as usize)))
    }

    // ------------------------------------------------------------ the generic iteration used by for-of, spread and patterns

    /// Is `proto.next` the intrinsic and is there no `return` on the chain? Then no iterator object is needed.
    fn sc_proto_intact(&mut self, proto: ObjId, next: ObjId) -> Res<bool> {
        let own = h!(self, self.heap.get_own_property(proto, self.sc.k_next));
        let ok = matches!(own, Some(Property::Data { value: Value::Object(f), .. }) if f == next);
        Ok(ok && !h!(self, self.heap.has_property(proto, self.sc.k_return)))
    }

    /// Start iterating `v` (`GetIterator`). `None` means `v` is not iterable.
    fn iter_open(&mut self, v: Value) -> Res<Option<It>> {
        if is_nullish(v) {
            return Ok(None);
        }
        let m = self.get_prop(v, self.sc.k_iterator)?;
        let plain = |kind| Some(It { src: v, i: 0, kind, done: false });
        if is_nullish(m) {
            return Ok(None);
        }
        let Value::Object(mo) = m else { return Err(self.type_err("Symbol.iterator is not a function")) };
        if !self.is_callable(m) {
            return Err(self.type_err("Symbol.iterator is not a function"));
        }
        if matches!(v, Value::String(_)) && mo == self.sc.f_string_iter && self.sc_proto_intact(self.sc.string_iter_proto, self.sc.f_string_next)? {
            return Ok(plain(ItKind::Plain));
        }
        if let Value::Object(o) = v {
            if mo == self.sc.f_array_values && self.sc_proto_intact(self.sc.array_iter_proto, self.sc.f_array_next)? {
                return Ok(plain(ItKind::Plain));
            }
            if let Some(c) = self.sc.colls.get(&o) {
                let (is_map, epoch) = (c.is_map, c.epoch);
                let what = if mo == self.sc.f_map_entries || mo == self.sc.f_set_entries {
                    Some(IterWhat::Entries)
                } else if mo == self.sc.f_map_keys {
                    Some(IterWhat::Keys)
                } else if mo == self.sc.f_map_values || mo == self.sc.f_set_values {
                    Some(IterWhat::Values)
                } else {
                    None
                };
                let (proto, next) = if is_map { (self.sc.map_iter_proto, self.sc.f_map_next) } else { (self.sc.set_iter_proto, self.sc.f_set_next) };
                if let Some(what) = what
                    && self.sc_proto_intact(proto, next)?
                {
                    return Ok(plain(ItKind::Coll { owner: o, what, cur: Cursor { epoch, ..Default::default() } }));
                }
            }
        }
        let iter = self.call_fn(m, v, &[])?;
        if !matches!(iter, Value::Object(_)) {
            return Err(self.type_err("Result of the Symbol.iterator method is not an object"));
        }
        let next = self.get_prop(iter, self.sc.k_next)?;
        Ok(plain(ItKind::Generic { iter, next }))
    }

    fn iter_start(&mut self, v: Value, e: ExprId) -> Res<It> {
        match self.iter_open(v)? {
            Some(it) => Ok(it),
            None => {
                let m = format!("{} is not iterable", self.src_text(e));
                Err(self.type_err(&m))
            }
        }
    }

    fn iter_start_val(&mut self, v: Value) -> Res<It> {
        match self.iter_open(v)? {
            Some(it) => Ok(it),
            None => {
                let t = self.display(v);
                Err(self.type_err(&format!("{t} is not iterable")))
            }
        }
    }

    /// The next value, or `None` when the iteration is done. After `None` or an error the iteration is
    /// done for good: `next` is not called again and `iter_close` does nothing.
    fn iter_next(&mut self, it: &mut It) -> Res<Option<Value>> {
        self.tick()?;
        if it.done {
            return Ok(None);
        }
        let r = self.iter_step(it);
        if !matches!(r, Ok(Some(_))) {
            it.done = true;
        }
        r
    }

    fn iter_step(&mut self, it: &mut It) -> Res<Option<Value>> {
        match &mut it.kind {
            ItKind::Plain => match it.src {
                Value::String(s) => {
                    let Some((v, n)) = self.str_step(s, it.i)? else { return Ok(None) };
                    it.i += n;
                    Ok(Some(v))
                }
                src => {
                    let len = self.get_prop(src, self.k_length)?;
                    let len = self.coerce_number(len)?;
                    if (it.i as f64) >= len {
                        return Ok(None);
                    }
                    let key = self.index_key(it.i as u64)?;
                    let v = self.get_prop(src, key)?;
                    it.i += 1;
                    Ok(Some(v))
                }
            },
            ItKind::Coll { owner, what, cur } => {
                let (owner, what) = (*owner, *what);
                let Some((k, v)) = self.coll_next(owner, cur)? else { return Ok(None) };
                Ok(Some(self.coll_item(what, k, v)?))
            }
            ItKind::Generic { iter, next } => {
                let (iter, next) = (*iter, *next);
                if let (Value::Object(io), Value::Object(no)) = (iter, next)
                    && let Some(step) = self.iter_native_step(io, no)?
                {
                    return Ok(step);
                }
                let r = self.call_fn(next, iter, &[])?;
                if !matches!(r, Value::Object(_)) {
                    let t = self.display(r);
                    return Err(self.type_err(&format!("Iterator result {t} is not an object")));
                }
                let d = self.get_prop(r, self.sc.k_done)?;
                if self.truthy(d)? {
                    return Ok(None);
                }
                Ok(Some(self.get_prop(r, self.k_value)?))
            }
        }
    }

    /// `IteratorClose` for a normal completion (break, return, continue to an outer label, patterns that
    /// did not use every value). Does nothing for an iteration that is done or has no `return`.
    fn iter_close(&mut self, it: &mut It) -> Res<()> {
        if it.done {
            return Ok(());
        }
        it.done = true;
        // A fast path has no iterator object, and `return` is looked up when the loop closes it. Look at the
        // prototype chain again. Only if a `return` appeared since the loop began, make the iterator the loop
        // would have had, at its current position, and close that one.
        let iter = match it.kind {
            ItKind::Generic { iter, .. } => iter,
            ItKind::Plain => {
                let (proto, st) = match it.src {
                    Value::String(s) => (self.sc.string_iter_proto, IterState::Str { s, i: it.i, done: false }),
                    src => (self.sc.array_iter_proto, IterState::Array { src, i: it.i as u64, what: IterWhat::Values, done: false }),
                };
                if !h!(self, self.heap.has_property(proto, self.sc.k_return)) {
                    return Ok(());
                }
                self.sc_track_iter(proto, st)?
            }
            ItKind::Coll { owner, what, cur } => {
                let is_map = self.sc.colls.get(&owner).is_some_and(|c| c.is_map);
                let proto = if is_map { self.sc.map_iter_proto } else { self.sc.set_iter_proto };
                if !h!(self, self.heap.has_property(proto, self.sc.k_return)) {
                    return Ok(());
                }
                self.sc_track_iter(proto, IterState::Coll { owner, what, cur, done: false })?
            }
        };
        // A pending `return` value must survive the JS call that closes the iterator.
        let saved = self.ret;
        let r = (|| {
            let m = self.get_prop(iter, self.sc.k_return)?;
            if is_nullish(m) {
                return Ok(());
            }
            if !self.is_callable(m) {
                return Err(self.type_err("iterator.return is not a function"));
            }
            let res = self.call_fn(m, iter, &[])?;
            if !matches!(res, Value::Object(_)) {
                return Err(self.type_err("iterator.return() did not return an object"));
            }
            Ok(())
        })();
        self.ret = saved;
        r
    }

    /// `IteratorClose` after a throw: an error of `return` is dropped, the original error wins. A limit,
    /// Unsupported or a heap fault is final and runs no JS.
    fn iter_close_throw(&mut self, it: &mut It, err: Error) -> Error {
        if !matches!(err, Error::Throw(_)) {
            return err;
        }
        match self.iter_close(it) {
            Ok(()) | Err(Error::Throw(_)) => err,
            Err(other) => other,
        }
    }

    /// All values of an iterable, at most `max_iter_buffer` (shared by spread arguments, `Array.from` and the like).
    #[allow(dead_code)] // shared with Array.from and Promise combinators after merging
    fn iter_to_vec(&mut self, v: Value) -> Res<Vec<Value>> {
        let mut it = self.iter_start_val(v)?;
        let mut out = Vec::new();
        while let Some(x) = self.iter_next(&mut it)? {
            self.iter_buffer_room(out.len())?;
            out.push(x);
        }
        Ok(out)
    }

    fn iter_buffer_room(&self, len: usize) -> Res<()> {
        if len >= self.sc.limits.max_iter_buffer {
            return Err(Error::Limit(LimitKind::Sc(ScLimit::IterBuffer)));
        }
        Ok(())
    }

    /// The elements of an array pattern against an open iteration. A hole takes one value, a rest takes all.
    fn bind_array_elems(&mut self, l: List<Pat>, it: &mut It, mode: Bind) -> Res<()> {
        let prog = self.prog.clone();
        for q in prog.list(l) {
            match prog.pat(q).kind {
                PatKind::Hole => {
                    self.iter_next(it)?;
                }
                PatKind::Rest(t) => {
                    let mut rest = Vec::new();
                    while let Some(x) = self.iter_next(it)? {
                        self.iter_buffer_room(rest.len())?;
                        rest.push(x);
                    }
                    let arr = self.new_array_from(&rest)?;
                    self.bind_pattern(t, arr, mode)?;
                }
                _ => {
                    let x = self.iter_next(it)?.unwrap_or(Value::Undefined);
                    self.bind_pattern(q, x, mode)?;
                }
            }
        }
        Ok(())
    }

    /// An array pattern: bind the elements, then close the iterator unless it is done. A throw closes it quietly.
    fn bind_array_pattern(&mut self, l: List<Pat>, v: Value, mode: Bind) -> Res<()> {
        let mut it = self.iter_start_val(v)?;
        match self.bind_array_elems(l, &mut it, mode) {
            Ok(()) => self.iter_close(&mut it),
            Err(e) => Err(self.iter_close_throw(&mut it, e)),
        }
    }
}
