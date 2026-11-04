//! Managed heap: strings, symbols and objects with property descriptors, and a collector.
//!
//! Root contract (read this first)
//! - Every `StrId`, `SymId` and `ObjId` is valid until the next `collect` that does not reach it
//!   from the `roots` you pass. After that the handle is stale: every method returns
//!   `HeapError::Stale` for it, even if the slot was reused (the generation differs).
//! - Anything the interpreter still needs (stack, scopes, globals, pending jobs, host state)
//!   must be in `roots` when it calls `collect`. The heap never collects by itself.
//! - A handle kept only in a Rust local and not in `roots` is dead after `collect`.
//! - Handles of one heap must not be given to another heap (not detected).
//!
//! Storage: one slab (a `Vec` of slots with a free list) for each kind. A slot has a generation
//! number. Freeing a slot adds 1. A slot whose generation would pass `u32::MAX` is retired and
//! never reused, so a stale handle can never match a new owner.
//!
//! Collector: mark and sweep. Marking uses an explicit work list, not Rust recursion, so a long
//! prototype chain or a deep object graph cannot overflow the stack. Cycles are collected when
//! no root reaches them.
//!
//! Not supported (needs the evaluator): calling getters and setters, `ToPrimitive` on objects
//! (`NeedsExecution`), `typeof` of a callable (`"function"`), Symbol.for registry, Proxy,
//! checking that an accessor is callable. Accessors are stored, validated and traced; the
//! caller gets the getter or setter object and must call it.

use crate::value::*;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// Hard bounds of one heap. Every allocation checks them. The defaults are for one tab.
#[derive(Clone, Copy, Debug)]
pub struct HeapLimits {
    /// Most object slots (live, free and retired).
    pub max_objects: usize,
    pub max_strings: usize,
    pub max_symbols: usize,
    /// Most UTF-16 code units in one string.
    pub max_string_units: usize,
    /// Most accounted bytes (see `Heap::bytes`). Counts payload by length, not allocator slack.
    pub max_bytes: usize,
}

impl Default for HeapLimits {
    fn default() -> Self {
        HeapLimits {
            max_objects: 1 << 24,
            max_strings: 1 << 24,
            max_symbols: 1 << 20,
            max_string_units: (1 << 29) - 24, // the V8 limit
            max_bytes: 1 << 30,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Limit {
    Objects,
    Strings,
    Symbols,
    StringLength,
    Bytes,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HeapError {
    /// The handle is dead or from a freed generation (a JS engine would never see this).
    Stale,
    Limit(Limit),
    /// A `PropertyKey::String` that is an array index, or a symbol/string handle that is stale.
    InvalidKey,
    /// A descriptor with both data and accessor fields (a TypeError in JS).
    InvalidDescriptor,
    /// Array length that is not a uint32 (a RangeError in JS).
    InvalidArrayLength,
    /// A symbol used where JS throws a TypeError (`"" + sym`, `+sym`).
    SymbolConversion,
    /// The operation needs the evaluator (`ToPrimitive` on an object). Nothing was changed.
    NeedsExecution(&'static str),
}

// ---------- property model ----------

/// An own property as stored. Array elements and the array `length` use the same type.
#[derive(Clone, Copy, Debug)]
pub enum Property {
    Data { value: Value, writable: bool, enumerable: bool, configurable: bool },
    /// `get`/`set` of `None` means `undefined`. The heap does not check they are callable.
    Accessor { get: Option<ObjId>, set: Option<ObjId>, enumerable: bool, configurable: bool },
}

impl Property {
    pub fn enumerable(&self) -> bool {
        match *self {
            Property::Data { enumerable, .. } | Property::Accessor { enumerable, .. } => enumerable,
        }
    }
    pub fn configurable(&self) -> bool {
        match *self {
            Property::Data { configurable, .. } | Property::Accessor { configurable, .. } => configurable,
        }
    }
}

/// A partial descriptor, as given to `Object.defineProperty`. `None` is "field absent".
/// `get: Some(None)` is `get: undefined`.
#[derive(Clone, Copy, Debug, Default)]
pub struct PropertyDescriptor {
    pub value: Option<Value>,
    pub writable: Option<bool>,
    pub get: Option<Option<ObjId>>,
    pub set: Option<Option<ObjId>>,
    pub enumerable: Option<bool>,
    pub configurable: Option<bool>,
}

impl PropertyDescriptor {
    /// What `o[k] = v` creates: a data property with all three flags true.
    pub fn data(value: Value) -> Self {
        PropertyDescriptor { value: Some(value), writable: Some(true), enumerable: Some(true), configurable: Some(true), ..Default::default() }
    }
    fn is_accessor(&self) -> bool {
        self.get.is_some() || self.set.is_some()
    }
    fn is_data(&self) -> bool {
        self.value.is_some() || self.writable.is_some()
    }
}

/// Result of `get`: a value, or an accessor that the caller must call with `this` = the receiver.
#[derive(Clone, Copy, Debug)]
pub enum Get {
    /// `Undefined` when no property exists on the chain.
    Value(Value),
    /// `getter` is `None` when the accessor has no getter (the result is `undefined`).
    Accessor { getter: Option<ObjId> },
}

/// Result of `set`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Set {
    Done,
    /// Failed (read-only, non-extensible, accessor without setter). Strict code throws a TypeError.
    Rejected,
    /// An inherited or own accessor: the caller must call this setter with the value.
    Setter(ObjId),
}

/// ValidateAndApplyPropertyDescriptor (section 10.1.6.3) on one property. `None` = rejected.
fn validate_apply(extensible: bool, cur: Option<Property>, d: &PropertyDescriptor) -> Option<Property> {
    if d.is_accessor() && d.is_data() {
        return None; // callers check first; keep this safe
    }
    let Some(cur) = cur else {
        if !extensible {
            return None;
        }
        let (e, c) = (d.enumerable.unwrap_or(false), d.configurable.unwrap_or(false));
        return Some(if d.is_accessor() {
            Property::Accessor { get: d.get.flatten(), set: d.set.flatten(), enumerable: e, configurable: c }
        } else {
            Property::Data { value: d.value.unwrap_or(Value::Undefined), writable: d.writable.unwrap_or(false), enumerable: e, configurable: c }
        });
    };
    let cur_accessor = matches!(cur, Property::Accessor { .. });
    if !cur.configurable() {
        if d.configurable == Some(true) || d.enumerable.is_some_and(|e| e != cur.enumerable()) {
            return None;
        }
        if (d.is_accessor() && !cur_accessor) || (d.is_data() && cur_accessor) {
            return None;
        }
        match cur {
            Property::Accessor { get, set, .. } => {
                if d.get.is_some_and(|g| g != get) || d.set.is_some_and(|s| s != set) {
                    return None;
                }
            }
            Property::Data { value, writable: false, .. } => {
                if d.writable == Some(true) || d.value.is_some_and(|v| !v.same_value(value)) {
                    return None;
                }
            }
            _ => {}
        }
    }
    let e = d.enumerable.unwrap_or(cur.enumerable());
    let c = d.configurable.unwrap_or(cur.configurable());
    Some(match cur {
        // Kind changes keep enumerable and configurable and reset the rest.
        Property::Data { .. } if d.is_accessor() => Property::Accessor { get: d.get.flatten(), set: d.set.flatten(), enumerable: e, configurable: c },
        Property::Accessor { .. } if d.is_data() => Property::Data { value: d.value.unwrap_or(Value::Undefined), writable: d.writable.unwrap_or(false), enumerable: e, configurable: c },
        Property::Data { value, writable, .. } => Property::Data { value: d.value.unwrap_or(value), writable: d.writable.unwrap_or(writable), enumerable: e, configurable: c },
        Property::Accessor { get, set, .. } => Property::Accessor { get: d.get.unwrap_or(get), set: d.set.unwrap_or(set), enumerable: e, configurable: c },
    })
}

// ---------- object storage ----------

/// Hasher for property keys. A key is two small integers (a kind and an index or handle), so a
/// multiply-and-rotate hash is enough; the default SipHash was 35% of lookup time (measured).
/// It is not DoS-proof, which is fine: the keys are heap handles, not page-chosen text, and a
/// page can only slow its own tab.
#[derive(Default)]
struct KeyHasher(u64);

impl KeyHasher {
    fn add(&mut self, x: u64) {
        self.0 = (self.0.rotate_left(5) ^ x).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl Hasher for KeyHasher {
    fn write(&mut self, bytes: &[u8]) {
        bytes.iter().for_each(|&b| self.add(b as u64));
    }
    fn write_u32(&mut self, x: u32) {
        self.add(x as u64);
    }
    fn write_u64(&mut self, x: u64) {
        self.add(x);
    }
    fn write_usize(&mut self, x: usize) {
        self.add(x as u64);
    }
    fn finish(&self) -> u64 {
        self.0.rotate_left(26) // multiply leaves the best bits on top; hashbrown reads the low ones
    }
}

type KeyMap = HashMap<PropertyKey, u32, BuildHasherDefault<KeyHasher>>;

/// Own named properties in insertion order. A removed entry leaves a hole (`None`) so the other
/// positions stay valid; holes are squeezed out when they are more than half. Objects with more
/// than `INDEX_AT` entries get a hash index from key to position.
struct PropMap {
    entries: Vec<Option<(PropertyKey, Property)>>,
    live: u32,
    /// How many entries have an `Index` key (lets dense element writes skip a lookup).
    index_keys: u32,
    index: Option<Box<KeyMap>>,
}

const INDEX_AT: usize = 8;

impl PropMap {
    fn new() -> Self {
        PropMap { entries: Vec::new(), live: 0, index_keys: 0, index: None }
    }
    fn find(&self, k: PropertyKey) -> Option<usize> {
        match &self.index {
            Some(m) => m.get(&k).map(|&p| p as usize),
            None => self.entries.iter().position(|e| matches!(e, Some((kk, _)) if *kk == k)),
        }
    }
    fn get(&self, k: PropertyKey) -> Option<Property> {
        self.find(k).and_then(|p| self.entries[p].map(|e| e.1))
    }
    fn set(&mut self, k: PropertyKey, p: Property) {
        if let Some(pos) = self.find(k) {
            self.entries[pos].as_mut().unwrap().1 = p;
            return;
        }
        let pos = self.entries.len();
        self.entries.push(Some((k, p)));
        self.live += 1;
        self.index_keys += matches!(k, PropertyKey::Index(_)) as u32;
        match &mut self.index {
            Some(m) => {
                m.insert(k, pos as u32);
            }
            None if self.entries.len() > INDEX_AT => self.rebuild_index(),
            None => {}
        }
    }
    fn remove(&mut self, k: PropertyKey) -> bool {
        let Some(pos) = self.find(k) else { return false };
        self.entries[pos] = None;
        self.live -= 1;
        self.index_keys -= matches!(k, PropertyKey::Index(_)) as u32;
        if let Some(m) = &mut self.index {
            m.remove(&k);
        }
        while matches!(self.entries.last(), Some(None)) {
            self.entries.pop();
        }
        if self.entries.len() > 16 && self.entries.len() > 2 * self.live as usize {
            self.entries.retain(|e| e.is_some());
            self.rebuild_index();
        }
        true
    }
    fn rebuild_index(&mut self) {
        if self.entries.len() <= INDEX_AT {
            self.index = None;
            return;
        }
        let m: KeyMap = self.entries.iter().enumerate().filter_map(|(i, e)| e.map(|(k, _)| (k, i as u32))).collect();
        self.index = Some(Box::new(m));
    }
    fn iter(&self) -> impl Iterator<Item = (PropertyKey, Property)> + '_ {
        self.entries.iter().filter_map(|e| *e)
    }
}

/// Largest gap of holes a dense element vector accepts. A write farther out goes to `props`.
const ELEM_GAP: usize = 1024;

struct Object {
    proto: Option<ObjId>,
    extensible: bool,
    is_array: bool,
    /// Dense index properties that are plain data (writable, enumerable, configurable).
    /// `None` is a hole. An index lives here or in `props`, never in both.
    elems: Vec<Option<Value>>,
    /// Everything else: named keys, symbols, and index properties that are sparse or special.
    /// An array's `length` is the first entry.
    props: PropMap,
}

const ENTRY_BYTES: usize = std::mem::size_of::<Option<(PropertyKey, Property)>>();
const ELEM_BYTES: usize = std::mem::size_of::<Option<Value>>();
/// Accounted cost of one hash-index entry (key + position + table overhead, estimate).
const INDEX_ENTRY_BYTES: usize = 24;

fn obj_bytes(o: &Object) -> usize {
    std::mem::size_of::<Slot<Object>>()
        + o.elems.len() * ELEM_BYTES
        + o.props.entries.len() * ENTRY_BYTES
        + if o.props.index.is_some() { o.props.live as usize * INDEX_ENTRY_BYTES } else { 0 }
}

struct Str {
    units: Box<[u16]>,
    hash: u32,
}

struct Symbol {
    description: Option<StrId>,
}

fn str_bytes(s: &Str) -> usize {
    std::mem::size_of::<Slot<Str>>() + s.units.len() * 2 + 8 // 8: two interner slots at load 0.5
}

// ---------- slabs ----------

struct Slot<T> {
    gn: u32,
    val: Option<T>,
}

struct Slab<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    marks: Vec<bool>,
    live: usize,
    retired: usize,
    max: usize,
    allocs: u64,
}

impl<T> Slab<T> {
    fn new(max: usize) -> Self {
        Slab { slots: Vec::new(), free: Vec::new(), marks: Vec::new(), live: 0, retired: 0, max, allocs: 0 }
    }
    fn alloc(&mut self, v: T, limit: Limit) -> Result<(u32, u32), HeapError> {
        let (i, gn) = if let Some(i) = self.free.pop() {
            (i, self.slots[i as usize].gn)
        } else if self.slots.len() < self.max && self.slots.len() < u32::MAX as usize {
            self.slots.push(Slot { gn: 0, val: None });
            (self.slots.len() as u32 - 1, 0)
        } else {
            return Err(HeapError::Limit(limit));
        };
        self.slots[i as usize].val = Some(v);
        self.live += 1;
        self.allocs += 1;
        Ok((i, gn))
    }
    fn get(&self, index: u32, gn: u32) -> Option<&T> {
        let s = self.slots.get(index as usize)?;
        if s.gn == gn { s.val.as_ref() } else { None }
    }
    fn get_mut(&mut self, index: u32, gn: u32) -> Option<&mut T> {
        let s = self.slots.get_mut(index as usize)?;
        if s.gn == gn { s.val.as_mut() } else { None }
    }
    /// Free a slot: bump the generation, or retire the slot when the generation cannot grow.
    fn release(&mut self, i: usize) {
        let s = &mut self.slots[i];
        s.val = None;
        self.live -= 1;
        match s.gn.checked_add(1) {
            Some(g) => {
                s.gn = g;
                self.free.push(i as u32);
            }
            None => self.retired += 1,
        }
    }
    /// Free every live slot whose mark is clear and clear all marks. `f` sees each freed value.
    fn sweep(&mut self, mut f: impl FnMut(&T)) -> usize {
        let mut freed = 0;
        for i in 0..self.slots.len() {
            if self.slots[i].val.is_none() {
                continue;
            }
            if self.marks[i] {
                self.marks[i] = false;
            } else {
                f(self.slots[i].val.as_ref().unwrap());
                self.release(i);
                freed += 1;
            }
        }
        freed
    }
}

// ---------- string interner ----------

/// Weak hash set of string slots: open addressing, linear probing, power-of-two size.
/// An entry is `slot index + 1`; 0 is empty. It never keeps a string alive: after a sweep
/// `rebuild` drops the entries of freed strings.
struct Interner {
    table: Vec<u32>,
    count: usize,
}

fn hash_units(units: &[u16]) -> u32 {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let mut h: u64 = units.len() as u64;
    // Four code units per step: the hash is a dependent chain, so fewer steps is faster (measured).
    let mut chunks = units.chunks_exact(4);
    for c in &mut chunks {
        let w = c[0] as u64 | (c[1] as u64) << 16 | (c[2] as u64) << 32 | (c[3] as u64) << 48;
        h = (h.rotate_left(5) ^ w).wrapping_mul(K);
    }
    for &u in chunks.remainder() {
        h = (h.rotate_left(5) ^ u as u64).wrapping_mul(K);
    }
    // Final mix: the table reads the low bits, and a product's low-middle bits are weak.
    h ^= h >> 32;
    (h.wrapping_mul(K) >> 32) as u32
}

impl Interner {
    fn find(&self, strs: &Slab<Str>, hash: u32, units: &[u16]) -> Option<u32> {
        let mask = self.table.len() - 1;
        let mut p = hash as usize & mask;
        loop {
            let e = self.table[p];
            if e == 0 {
                return None;
            }
            let s = strs.slots[e as usize - 1].val.as_ref().unwrap();
            if s.hash == hash && *s.units == *units {
                return Some(e - 1);
            }
            p = (p + 1) & mask;
        }
    }
    fn insert_slot(&mut self, hash: u32, slot: u32) {
        let mask = self.table.len() - 1;
        let mut p = hash as usize & mask;
        while self.table[p] != 0 {
            p = (p + 1) & mask;
        }
        self.table[p] = slot + 1;
        self.count += 1;
    }
    fn rebuild(&mut self, strs: &Slab<Str>) {
        let size = (strs.live * 2).next_power_of_two().max(16);
        self.table.clear();
        self.table.resize(size, 0);
        self.count = 0;
        for (i, s) in strs.slots.iter().enumerate() {
            if let Some(s) = &s.val {
                self.insert_slot(s.hash, i as u32);
            }
        }
    }
}

// ---------- the heap ----------


/// Counters for the load tool and for tests.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub live_objects: usize,
    pub live_strings: usize,
    pub live_symbols: usize,
    /// Slots ever created for each kind (live + free + retired).
    pub object_slots: usize,
    pub string_slots: usize,
    pub symbol_slots: usize,
    /// Slots that reached the last generation and are never reused.
    pub retired_slots: usize,
    /// Accounted bytes of live payload (length based; allocator slack not included).
    pub bytes: usize,
    pub total_object_allocs: u64,
    pub total_string_allocs: u64,
    pub total_symbol_allocs: u64,
    pub collections: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Collected {
    pub objects: usize,
    pub strings: usize,
    pub symbols: usize,
}

pub struct Heap {
    limits: HeapLimits,
    objects: Slab<Object>,
    strings: Slab<Str>,
    symbols: Slab<Symbol>,
    interner: Interner,
    bytes: usize,
    collections: u64,
    gray: Vec<u32>,
    /// The string `"length"`. A permanent root, so arrays can always name their length key.
    length: StrId,
}

impl Default for Heap {
    fn default() -> Self {
        Heap::new(HeapLimits::default())
    }
}

type R<T> = Result<T, HeapError>;

impl Heap {
    pub fn new(limits: HeapLimits) -> Heap {
        let mut h = Heap {
            limits,
            objects: Slab::new(limits.max_objects),
            strings: Slab::new(limits.max_strings.max(1)),
            symbols: Slab::new(limits.max_symbols),
            interner: Interner { table: vec![0; 16], count: 0 },
            bytes: 0,
            collections: 0,
            gray: Vec::new(),
            length: StrId { index: 0, gn: 0 },
        };
        h.length = h.intern_str("length").expect("the first string always fits");
        h
    }

    pub fn limits(&self) -> HeapLimits {
        self.limits
    }

    pub fn stats(&self) -> Stats {
        Stats {
            live_objects: self.objects.live,
            live_strings: self.strings.live,
            live_symbols: self.symbols.live,
            object_slots: self.objects.slots.len(),
            string_slots: self.strings.slots.len(),
            symbol_slots: self.symbols.slots.len(),
            retired_slots: self.objects.retired + self.strings.retired + self.symbols.retired,
            bytes: self.bytes,
            total_object_allocs: self.objects.allocs,
            total_string_allocs: self.strings.allocs,
            total_symbol_allocs: self.symbols.allocs,
            collections: self.collections,
        }
    }

    // ----- handle checks -----

    fn obj(&self, id: ObjId) -> R<&Object> {
        self.objects.get(id.index, id.gn).ok_or(HeapError::Stale)
    }
    fn str(&self, id: StrId) -> R<&Str> {
        self.strings.get(id.index, id.gn).ok_or(HeapError::Stale)
    }
    fn sym(&self, id: SymId) -> R<&Symbol> {
        self.symbols.get(id.index, id.gn).ok_or(HeapError::Stale)
    }
    fn check_value(&self, v: Value) -> R<()> {
        match v {
            Value::String(s) => self.str(s).map(drop),
            Value::Symbol(s) => self.sym(s).map(drop),
            Value::Object(o) => self.obj(o).map(drop),
            _ => Ok(()),
        }
    }
    fn check_opt_obj(&self, o: Option<ObjId>) -> R<()> {
        o.map_or(Ok(()), |o| self.obj(o).map(drop))
    }
    fn check_key(&self, k: PropertyKey) -> R<()> {
        match k {
            PropertyKey::Index(_) => Ok(()),
            PropertyKey::Symbol(s) => self.sym(s).map(drop),
            PropertyKey::String(s) => {
                let u = &self.str(s)?.units;
                if u.first().is_some_and(|c| (0x30..=0x39).contains(c)) && parse_index(u).is_some() {
                    return Err(HeapError::InvalidKey);
                }
                Ok(())
            }
        }
    }
    fn charge(&self, extra: usize) -> R<()> {
        if self.bytes.saturating_add(extra) > self.limits.max_bytes { Err(HeapError::Limit(Limit::Bytes)) } else { Ok(()) }
    }


    // ----- strings -----

    /// The unique string with these code units (copied if new). Lone surrogates are kept as given.
    /// The same content gives the same handle while that string is alive.
    pub fn intern_units(&mut self, units: &[u16]) -> R<StrId> {
        self.intern(std::borrow::Cow::Borrowed(units))
    }

    /// Like `intern_units`, but takes the vector: no copy when the string is new.
    pub fn intern_vec(&mut self, units: Vec<u16>) -> R<StrId> {
        self.intern(std::borrow::Cow::Owned(units))
    }

    pub fn intern_str(&mut self, s: &str) -> R<StrId> {
        self.intern_vec(s.encode_utf16().collect())
    }

    fn intern(&mut self, units: std::borrow::Cow<[u16]>) -> R<StrId> {
        let hash = hash_units(&units);
        if let Some(i) = self.interner.find(&self.strings, hash, &units) {
            return Ok(StrId { index: i, gn: self.strings.slots[i as usize].gn });
        }
        if units.len() > self.limits.max_string_units {
            return Err(HeapError::Limit(Limit::StringLength));
        }
        let s = Str { units: units.into_owned().into_boxed_slice(), hash };
        let cost = str_bytes(&s);
        self.charge(cost)?;
        if (self.interner.count + 1) * 2 > self.interner.table.len() {
            // Grow before the insert so the load stays at most 0.5 and a probe always ends.
            let size = self.interner.table.len() * 2;
            self.interner.table.clear();
            self.interner.table.resize(size, 0);
            self.interner.count = 0;
            for (i, sl) in self.strings.slots.iter().enumerate() {
                if let Some(old) = &sl.val {
                    self.interner.insert_slot(old.hash, i as u32);
                }
            }
        }
        let (index, gn) = self.strings.alloc(s, Limit::Strings)?;
        self.interner.insert_slot(hash, index);
        self.bytes += cost;
        Ok(StrId { index, gn })
    }

    /// The code units of a string. Borrowed from the heap: no copy.
    pub fn str_units(&self, s: StrId) -> R<&[u16]> {
        Ok(&self.str(s)?.units)
    }

    pub fn str_len(&self, s: StrId) -> R<usize> {
        Ok(self.str(s)?.units.len())
    }

    /// `s.charCodeAt(i)`; `None` when out of range (JS gives NaN).
    pub fn code_unit_at(&self, s: StrId, i: usize) -> R<Option<u16>> {
        Ok(self.str(s)?.units.get(i).copied())
    }

    /// `s.codePointAt(i)`: the code point and how many code units it uses (1 or 2).
    /// A lone surrogate is returned as its own value with 1 unit.
    pub fn code_point_at(&self, s: StrId, i: usize) -> R<Option<(u32, u8)>> {
        let u = &self.str(s)?.units;
        let Some(&hi) = u.get(i) else { return Ok(None) };
        if (0xD800..0xDC00).contains(&hi) {
            if let Some(&lo) = u.get(i + 1).filter(|l| (0xDC00..0xE000).contains(*l)) {
                return Ok(Some((0x10000 + (((hi as u32) - 0xD800) << 10) + (lo as u32 - 0xDC00), 2)));
            }
        }
        Ok(Some((hi as u32, 1)))
    }

    /// `a + b` for strings.
    pub fn concat(&mut self, a: StrId, b: StrId) -> R<StrId> {
        let (x, y) = (&self.str(a)?.units, &self.str(b)?.units);
        if x.len() + y.len() > self.limits.max_string_units {
            return Err(HeapError::Limit(Limit::StringLength));
        }
        let mut v = Vec::with_capacity(x.len() + y.len());
        v.extend_from_slice(x);
        v.extend_from_slice(y);
        self.intern_vec(v)
    }

    /// `s.substring(start, end)` with both ends clamped to the length (and swapped if reversed).
    pub fn substring(&mut self, s: StrId, start: usize, end: usize) -> R<StrId> {
        let u = &self.str(s)?.units;
        let (a, b) = (start.min(u.len()), end.min(u.len()));
        let (a, b) = (a.min(b), a.max(b));
        if a == 0 && b == u.len() {
            return Ok(s);
        }
        let part = u[a..b].to_vec();
        self.intern_vec(part)
    }

    /// For logs and tests: UTF-8 text, with U+FFFD for each lone surrogate.
    pub fn to_string_lossy(&self, s: StrId) -> R<String> {
        Ok(String::from_utf16_lossy(&self.str(s)?.units))
    }

    // ----- symbols -----

    /// A new symbol. Each call is a distinct identity, even with the same description.
    pub fn new_symbol(&mut self, description: Option<StrId>) -> R<SymId> {
        if let Some(d) = description {
            self.str(d)?;
        }
        let cost = std::mem::size_of::<Slot<Symbol>>();
        self.charge(cost)?;
        let (index, gn) = self.symbols.alloc(Symbol { description }, Limit::Symbols)?;
        self.bytes += cost;
        Ok(SymId { index, gn })
    }

    pub fn symbol_description(&self, s: SymId) -> R<Option<StrId>> {
        Ok(self.sym(s)?.description)
    }

    // ----- keys and conversions -----

    /// The key for a string: `Index` for a canonical array index, else `String`.
    pub fn key_from_string(&self, s: StrId) -> R<PropertyKey> {
        Ok(match parse_index(&self.str(s)?.units) {
            Some(i) => PropertyKey::Index(i),
            None => PropertyKey::String(s),
        })
    }

    pub fn key_from_str(&mut self, s: &str) -> R<PropertyKey> {
        let id = self.intern_str(s)?;
        self.key_from_string(id)
    }

    /// The key as a JS value: a string (made from the index for `Index`) or a symbol.
    pub fn key_to_value(&mut self, k: PropertyKey) -> R<Value> {
        Ok(match k {
            PropertyKey::Index(i) => Value::String(self.intern_str(&i.to_string())?),
            PropertyKey::String(s) => Value::String(s),
            PropertyKey::Symbol(s) => Value::Symbol(s),
        })
    }

    /// ToPropertyKey for primitives. An object needs `ToPrimitive`, so it gives `NeedsExecution`.
    pub fn to_property_key(&mut self, v: Value) -> R<PropertyKey> {
        match v {
            Value::Symbol(s) => {
                self.sym(s)?;
                Ok(PropertyKey::Symbol(s))
            }
            Value::Number(n) if n >= 0.0 && n <= MAX_INDEX as f64 && n as u32 as f64 == n => Ok(PropertyKey::Index(n as u32)),
            _ => {
                let s = self.to_string(v)?;
                self.key_from_string(s)
            }
        }
    }

    pub fn to_boolean(&self, v: Value) -> R<bool> {
        self.check_value(v)?;
        Ok(match v {
            Value::Undefined | Value::Null => false,
            Value::Bool(b) => b,
            Value::Number(n) => !(n == 0.0 || n.is_nan()),
            Value::String(s) => !self.str(s)?.units.is_empty(),
            Value::Symbol(_) | Value::Object(_) => true,
        })
    }

    pub fn to_number(&self, v: Value) -> R<f64> {
        self.check_value(v)?;
        match v {
            Value::Undefined => Ok(f64::NAN),
            Value::Null => Ok(0.0),
            Value::Bool(b) => Ok(b as u8 as f64),
            Value::Number(n) => Ok(n),
            Value::String(s) => Ok(string_to_number(&self.str(s)?.units)),
            Value::Symbol(_) => Err(HeapError::SymbolConversion),
            Value::Object(_) => Err(HeapError::NeedsExecution("ToNumber of an object calls valueOf/toString")),
        }
    }

    pub fn to_string(&mut self, v: Value) -> R<StrId> {
        self.check_value(v)?;
        match v {
            Value::String(s) => Ok(s),
            Value::Undefined => self.intern_str("undefined"),
            Value::Null => self.intern_str("null"),
            Value::Bool(b) => self.intern_str(if b { "true" } else { "false" }),
            Value::Number(n) => self.intern_str(&number_to_string(n)),
            Value::Symbol(_) => Err(HeapError::SymbolConversion),
            Value::Object(_) => Err(HeapError::NeedsExecution("ToString of an object calls toString/valueOf")),
        }
    }

    /// The `typeof` result. A callable object also reports `"object"`: the heap has no
    /// function objects yet, so `"function"` is the evaluator's job.
    pub fn type_of(&self, v: Value) -> R<&'static str> {
        self.check_value(v)?;
        Ok(match v {
            Value::Undefined => "undefined",
            Value::Null | Value::Object(_) => "object",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Symbol(_) => "symbol",
        })
    }

    /// `a == b`. Object against a primitive needs `ToPrimitive`: `NeedsExecution`.
    pub fn loose_equals(&self, a: Value, b: Value) -> R<bool> {
        use Value::*;
        self.check_value(a)?;
        self.check_value(b)?;
        Ok(match (a, b) {
            (Undefined | Null, Undefined | Null) => true,
            (Undefined | Null, _) | (_, Undefined | Null) => false,
            (Number(x), String(s)) | (String(s), Number(x)) => x == string_to_number(&self.str(s)?.units),
            (Bool(x), _) => return self.loose_equals(Number(x as u8 as f64), b),
            (_, Bool(y)) => return self.loose_equals(a, Number(y as u8 as f64)),
            (Object(_), Object(_)) => a.strict_equals(b),
            (Object(_), _) | (_, Object(_)) => return Err(HeapError::NeedsExecution("== of an object and a primitive calls ToPrimitive")),
            _ => a.strict_equals(b),
        })
    }

    // ----- objects -----

    fn alloc_object(&mut self, proto: Option<ObjId>, is_array: bool) -> R<ObjId> {
        self.check_opt_obj(proto)?;
        let mut props = PropMap::new();
        if is_array {
            let len = PropertyKey::String(self.length);
            props.set(len, Property::Data { value: Value::Number(0.0), writable: true, enumerable: false, configurable: false });
        }
        let o = Object { proto, extensible: true, is_array, elems: Vec::new(), props };
        let cost = obj_bytes(&o);
        self.charge(cost)?;
        let (index, gn) = self.objects.alloc(o, Limit::Objects)?;
        self.bytes += cost;
        Ok(ObjId { index, gn })
    }

    /// A new ordinary object (`Object.create(proto)`).
    pub fn new_object(&mut self, proto: Option<ObjId>) -> R<ObjId> {
        self.alloc_object(proto, false)
    }

    /// A new empty array with the exotic `length`.
    pub fn new_array(&mut self, proto: Option<ObjId>) -> R<ObjId> {
        self.alloc_object(proto, true)
    }

    pub fn is_array(&self, o: ObjId) -> R<bool> {
        Ok(self.obj(o)?.is_array)
    }

    pub fn get_prototype_of(&self, o: ObjId) -> R<Option<ObjId>> {
        Ok(self.obj(o)?.proto)
    }

    /// `Object.setPrototypeOf`. `false` for a non-extensible object with another prototype,
    /// or when the change would make a cycle.
    pub fn set_prototype_of(&mut self, o: ObjId, proto: Option<ObjId>) -> R<bool> {
        let obj = self.obj(o)?;
        self.check_opt_obj(proto)?;
        if obj.proto == proto {
            return Ok(true);
        }
        if !obj.extensible {
            return Ok(false);
        }
        let mut p = proto;
        while let Some(x) = p {
            if x == o {
                return Ok(false);
            }
            p = self.obj(x)?.proto;
        }
        self.objects.get_mut(o.index, o.gn).unwrap().proto = proto;
        Ok(true)
    }

    pub fn is_extensible(&self, o: ObjId) -> R<bool> {
        Ok(self.obj(o)?.extensible)
    }

    pub fn prevent_extensions(&mut self, o: ObjId) -> R<()> {
        self.obj(o)?;
        self.objects.get_mut(o.index, o.gn).unwrap().extensible = false;
        Ok(())
    }

    fn own(&self, o: &Object, key: PropertyKey) -> Option<Property> {
        if let PropertyKey::Index(i) = key {
            if let Some(Some(v)) = o.elems.get(i as usize) {
                return Some(Property::Data { value: *v, writable: true, enumerable: true, configurable: true });
            }
        }
        o.props.get(key)
    }

    /// `Object.getOwnPropertyDescriptor`.
    pub fn get_own_property(&self, o: ObjId, key: PropertyKey) -> R<Option<Property>> {
        self.check_key(key)?;
        Ok(self.own(self.obj(o)?, key))
    }

    pub fn has_own_property(&self, o: ObjId, key: PropertyKey) -> R<bool> {
        Ok(self.get_own_property(o, key)?.is_some())
    }

    /// `key in o`: own or inherited.
    pub fn has_property(&self, o: ObjId, key: PropertyKey) -> R<bool> {
        self.check_key(key)?;
        let mut cur = Some(o);
        while let Some(c) = cur {
            let obj = self.obj(c)?;
            if self.own(obj, key).is_some() {
                return Ok(true);
            }
            cur = obj.proto;
        }
        Ok(false)
    }

    /// The first property with this key on the prototype chain, and the object that holds it.
    pub fn lookup(&self, o: ObjId, key: PropertyKey) -> R<Option<(ObjId, Property)>> {
        self.check_key(key)?;
        let mut cur = Some(o);
        while let Some(c) = cur {
            let obj = self.obj(c)?;
            if let Some(p) = self.own(obj, key) {
                return Ok(Some((c, p)));
            }
            cur = obj.proto;
        }
        Ok(None)
    }

    /// `o[key]` without calling anything.
    pub fn get(&self, o: ObjId, key: PropertyKey) -> R<Get> {
        Ok(match self.lookup(o, key)? {
            None => Get::Value(Value::Undefined),
            Some((_, Property::Data { value, .. })) => Get::Value(value),
            Some((_, Property::Accessor { get, .. })) => Get::Accessor { getter: get },
        })
    }

    /// `o[key] = value` (OrdinarySet with the receiver `o`). Never calls a setter: it returns it.
    pub fn set(&mut self, o: ObjId, key: PropertyKey, value: Value) -> R<Set> {
        self.check_value(value)?;
        match self.lookup(o, key)? {
            Some((_, Property::Accessor { set: Some(s), .. })) => return Ok(Set::Setter(s)),
            Some((_, Property::Accessor { set: None, .. })) => return Ok(Set::Rejected),
            Some((_, Property::Data { writable: false, .. })) => return Ok(Set::Rejected),
            _ => {}
        }
        // Writable data property found (maybe on a prototype), or nothing: act on the receiver.
        let ok = match self.get_own_property(o, key)? {
            Some(Property::Accessor { .. }) => false,
            Some(Property::Data { .. }) => self.define_own_property(o, key, &PropertyDescriptor { value: Some(value), ..Default::default() })?,
            None => self.define_own_property(o, key, &PropertyDescriptor::data(value))?,
        };
        Ok(if ok { Set::Done } else { Set::Rejected })
    }

    /// `delete o[key]`. `false` for a non-configurable property.
    pub fn delete(&mut self, o: ObjId, key: PropertyKey) -> R<bool> {
        self.check_key(key)?;
        let Some(p) = self.own(self.obj(o)?, key) else { return Ok(true) };
        if !p.configurable() {
            return Ok(false);
        }
        self.edit(o, |ob| ob.remove(key));
        Ok(true)
    }

    /// `Reflect.ownKeys`: array indices ascending, then strings in creation order, then symbols
    /// in creation order. An array's `length` is the first string.
    pub fn own_keys(&self, o: ObjId) -> R<Vec<PropertyKey>> {
        let ob = self.obj(o)?;
        let mut idx: Vec<u32> = ob.elems.iter().enumerate().filter(|(_, v)| v.is_some()).map(|(i, _)| i as u32).collect();
        let sparse = ob.props.index_keys > 0;
        let (mut strs, mut syms) = (Vec::new(), Vec::new());
        for (k, _) in ob.props.iter() {
            match k {
                PropertyKey::Index(i) => idx.push(i),
                PropertyKey::String(_) => strs.push(k),
                PropertyKey::Symbol(_) => syms.push(k),
            }
        }
        if sparse {
            idx.sort_unstable();
        }
        let mut out: Vec<PropertyKey> = idx.into_iter().map(PropertyKey::Index).collect();
        out.extend(strs);
        out.extend(syms);
        Ok(out)
    }

    /// Run `f` on an object and keep `bytes` exact. `id` must be valid.
    fn edit<T>(&mut self, id: ObjId, f: impl FnOnce(&mut Object) -> T) -> T {
        let o = self.objects.get_mut(id.index, id.gn).expect("edit on a checked handle");
        let before = obj_bytes(o);
        let r = f(o);
        let after = obj_bytes(o);
        self.bytes = self.bytes + after - before;
        r
    }

    /// `Object.defineProperty` as a boolean (`Reflect.defineProperty`): `false` when rejected.
    /// Errors: stale handles, a descriptor mixing data and accessor fields, an invalid array
    /// length, or a size limit.
    pub fn define_own_property(&mut self, o: ObjId, key: PropertyKey, d: &PropertyDescriptor) -> R<bool> {
        let obj = self.obj(o)?;
        self.check_key(key)?;
        if let Some(v) = d.value {
            self.check_value(v)?;
        }
        self.check_opt_obj(d.get.flatten())?;
        self.check_opt_obj(d.set.flatten())?;
        if d.is_accessor() && d.is_data() {
            return Err(HeapError::InvalidDescriptor);
        }
        if !obj.is_array {
            return self.ordinary_define(o, key, d);
        }
        if key == PropertyKey::String(self.length) {
            return self.array_set_length(o, d);
        }
        let PropertyKey::Index(i) = key else { return self.ordinary_define(o, key, d) };
        let (len, writable) = self.array_len(obj);
        if i >= len && !writable {
            return Ok(false);
        }
        if !self.ordinary_define(o, key, d)? {
            return Ok(false);
        }
        if i >= len {
            self.edit(o, |ob| ob.set_length(Some(i + 1), None));
        }
        Ok(true)
    }

    fn array_len(&self, o: &Object) -> (u32, bool) {
        match o.props.get(PropertyKey::String(self.length)) {
            Some(Property::Data { value: Value::Number(n), writable, .. }) => (n as u32, writable),
            _ => unreachable!("an array always has a data `length`"),
        }
    }

    /// ValidateAndApply plus storage for one key, without the array rules.
    fn ordinary_define(&mut self, o: ObjId, key: PropertyKey, d: &PropertyDescriptor) -> R<bool> {
        let obj = self.obj(o)?;
        let cur = self.own(obj, key);
        let Some(new) = validate_apply(obj.extensible, cur, d) else { return Ok(false) };
        let in_elems = matches!(key, PropertyKey::Index(i) if matches!(obj.elems.get(i as usize), Some(Some(_))));
        let extra = match key {
            PropertyKey::Index(i) if obj.dense_ok(i, &new) => (i as usize + 1).saturating_sub(obj.elems.len()) * ELEM_BYTES,
            _ if cur.is_none() || in_elems => ENTRY_BYTES + INDEX_ENTRY_BYTES,
            _ => 0,
        };
        self.charge(extra)?;
        self.edit(o, |ob| ob.store(key, new));
        Ok(true)
    }

    /// ArraySetLength (section 10.4.2.4).
    fn array_set_length(&mut self, o: ObjId, d: &PropertyDescriptor) -> R<bool> {
        let key = PropertyKey::String(self.length);
        let Some(v) = d.value else { return self.ordinary_define(o, key, d) };
        let num = self.to_number(v)?;
        let n = to_uint32(num);
        if n as f64 != num {
            return Err(HeapError::InvalidArrayLength);
        }
        let mut nd = *d;
        nd.value = Some(Value::Number(n as f64));
        let (old, writable) = self.array_len(self.obj(o)?);
        if n >= old {
            return self.ordinary_define(o, key, &nd);
        }
        if !writable {
            return Ok(false);
        }
        // Set the length first (writable stays true until the deletes are done), then delete
        // from the top. A non-configurable element stops the delete: length is its index + 1.
        let new_writable = d.writable != Some(false);
        if !new_writable {
            nd.writable = Some(true);
        }
        if !self.ordinary_define(o, key, &nd)? {
            return Ok(false);
        }
        let blocked = self.edit(o, |ob| {
            let floor = ob.props.iter().filter_map(|(k, p)| match k {
                PropertyKey::Index(i) if i >= n && !p.configurable() => Some(i),
                _ => None,
            }).max();
            let keep = floor.map_or(n, |f| f + 1);
            ob.truncate_elems(keep as usize);
            if ob.props.index_keys > 0 {
                let dead: Vec<_> = ob.props.iter().map(|e| e.0).filter(|k| matches!(k, PropertyKey::Index(i) if *i >= keep)).collect();
                for k in dead {
                    ob.props.remove(k);
                }
            }
            if floor.is_some() {
                ob.set_length(Some(keep), None);
            }
            floor.is_some()
        });
        if !new_writable {
            self.edit(o, |ob| ob.set_length(None, Some(false)));
        }
        Ok(!blocked)
    }

    // ----- collection -----

    /// Mark everything reachable from `roots`, free the rest, and return what was freed.
    /// All handles not reached become stale. Errors (stale root) leave the heap unchanged.
    pub fn collect(&mut self, roots: &[Value]) -> R<Collected> {
        for &r in roots {
            self.check_value(r)?;
        }
        self.collections += 1;
        let Heap { objects, strings, symbols, gray, length, .. } = self;
        strings.marks.clear();
        strings.marks.resize(strings.slots.len(), false);
        symbols.marks.clear();
        symbols.marks.resize(symbols.slots.len(), false);
        objects.marks.clear();
        objects.marks.resize(objects.slots.len(), false);
        gray.clear();

        let mut m = Marker { smarks: &mut strings.marks, ymarks: &mut symbols.marks, omarks: &mut objects.marks, ysl: &symbols.slots, gray };
        m.smarks[length.index as usize] = true;
        for &r in roots {
            m.value(r);
        }
        // Work list: pop an object, mark everything it points to. No recursion.
        let osl = &objects.slots;
        while let Some(i) = m.gray.pop() {
            let o = osl[i as usize].val.as_ref().unwrap();
            if let Some(p) = o.proto {
                m.obj(p);
            }
            for v in o.elems.iter().flatten() {
                m.value(*v);
            }
            for (k, p) in o.props.iter() {
                match k {
                    PropertyKey::String(s) => m.smarks[s.index as usize] = true,
                    PropertyKey::Symbol(s) => m.sym(s),
                    PropertyKey::Index(_) => {}
                }
                match p {
                    Property::Data { value, .. } => m.value(value),
                    Property::Accessor { get, set, .. } => {
                        get.map(|g| m.obj(g));
                        set.map(|s| m.obj(s));
                    }
                }
            }
        }

        let mut freed_bytes = 0;
        let c = Collected {
            objects: objects.sweep(|o| freed_bytes += obj_bytes(o)),
            strings: strings.sweep(|s| freed_bytes += str_bytes(s)),
            symbols: symbols.sweep(|_| freed_bytes += std::mem::size_of::<Slot<Symbol>>()),
        };
        self.bytes -= freed_bytes;
        // The interner is weak: rebuild it from the strings that survived.
        self.interner.rebuild(&self.strings);
        Ok(c)
    }
}

/// Mark bits, split from the slabs so the work loop can read objects while it marks.
struct Marker<'a> {
    smarks: &'a mut Vec<bool>,
    ymarks: &'a mut Vec<bool>,
    omarks: &'a mut Vec<bool>,
    ysl: &'a [Slot<Symbol>],
    gray: &'a mut Vec<u32>,
}

impl Marker<'_> {
    fn obj(&mut self, o: ObjId) {
        if !std::mem::replace(&mut self.omarks[o.index as usize], true) {
            self.gray.push(o.index);
        }
    }
    fn sym(&mut self, s: SymId) {
        if !std::mem::replace(&mut self.ymarks[s.index as usize], true) {
            if let Some(d) = self.ysl[s.index as usize].val.as_ref().unwrap().description {
                self.smarks[d.index as usize] = true;
            }
        }
    }
    fn value(&mut self, v: Value) {
        match v {
            Value::String(s) => self.smarks[s.index as usize] = true,
            Value::Symbol(s) => self.sym(s),
            Value::Object(o) => self.obj(o),
            _ => {}
        }
    }
}

impl Object {
    /// Can this index property live in `elems`? Plain data, and not too far past the end.
    fn dense_ok(&self, i: u32, p: &Property) -> bool {
        matches!(p, Property::Data { writable: true, enumerable: true, configurable: true, .. }) && i as usize <= self.elems.len() + ELEM_GAP
    }

    fn store(&mut self, key: PropertyKey, p: Property) {
        let PropertyKey::Index(i) = key else { return self.props.set(key, p) };
        match p {
            Property::Data { value, .. } if self.dense_ok(i, &p) => {
                let i = i as usize;
                if i >= self.elems.len() {
                    self.elems.resize(i + 1, None);
                }
                self.elems[i] = Some(value);
                if self.props.index_keys > 0 {
                    self.props.remove(key);
                }
            }
            _ => {
                if let Some(e) = self.elems.get_mut(i as usize) {
                    *e = None;
                    self.trim_elems();
                }
                self.props.set(key, p);
            }
        }
    }

    fn remove(&mut self, key: PropertyKey) -> bool {
        if let PropertyKey::Index(i) = key {
            if let Some(e @ Some(_)) = self.elems.get_mut(i as usize) {
                *e = None;
                self.trim_elems();
                return true;
            }
        }
        self.props.remove(key)
    }

    fn trim_elems(&mut self) {
        while matches!(self.elems.last(), Some(None)) {
            self.elems.pop();
        }
    }

    fn truncate_elems(&mut self, n: usize) {
        self.elems.truncate(n);
        self.trim_elems();
    }

    /// Write the array `length` entry (the first one): a new value and/or `writable`.
    fn set_length(&mut self, len: Option<u32>, writable: Option<bool>) {
        let Some(Some((_, Property::Data { value, writable: w, .. }))) = self.props.entries.first_mut() else { unreachable!() };
        if let Some(l) = len {
            *value = Value::Number(l as f64);
        }
        if let Some(x) = writable {
            *w = x;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(v: f64) -> Value {
        Value::Number(v)
    }
    fn key(h: &mut Heap, s: &str) -> PropertyKey {
        h.key_from_str(s).unwrap()
    }
    fn val(h: &Heap, o: ObjId, k: PropertyKey) -> Value {
        match h.get(o, k).unwrap() {
            Get::Value(v) => v,
            Get::Accessor { .. } => panic!("accessor"),
        }
    }
    fn num(h: &Heap, o: ObjId, k: PropertyKey) -> f64 {
        match val(h, o, k) {
            Value::Number(x) => x,
            v => panic!("{v:?}"),
        }
    }
    fn setn(h: &mut Heap, o: ObjId, k: PropertyKey, x: f64) -> Set {
        h.set(o, k, n(x)).unwrap()
    }
    fn names(h: &Heap, o: ObjId) -> Vec<String> {
        h.own_keys(o)
            .unwrap()
            .into_iter()
            .map(|k| match k {
                PropertyKey::Index(i) => i.to_string(),
                PropertyKey::String(s) => h.to_string_lossy(s).unwrap(),
                PropertyKey::Symbol(_) => "@".into(),
            })
            .collect()
    }
    fn locked(w: bool, e: bool, c: bool, v: f64) -> PropertyDescriptor {
        PropertyDescriptor { value: Some(n(v)), writable: Some(w), enumerable: Some(e), configurable: Some(c), ..Default::default() }
    }

    #[test]
    fn strings_are_utf16_and_unique() {
        let mut h = Heap::default();
        let a = h.intern_str("a😀b").unwrap();
        assert_eq!(h.str_len(a).unwrap(), 4); // the emoji is a surrogate pair
        assert_eq!(h.code_unit_at(a, 1).unwrap(), Some(0xD83D));
        assert_eq!(h.code_unit_at(a, 2).unwrap(), Some(0xDE00));
        assert_eq!(h.code_unit_at(a, 4).unwrap(), None);
        assert_eq!(h.code_point_at(a, 1).unwrap(), Some((0x1F600, 2)));
        assert_eq!(h.code_point_at(a, 2).unwrap(), Some((0xDE00, 1))); // lone low surrogate
        assert_eq!(h.code_point_at(a, 9).unwrap(), None);
        assert_eq!(h.intern_str("a😀b").unwrap(), a); // same content, same handle
        let lone = h.intern_units(&[0xD800]).unwrap();
        assert_eq!(h.str_units(lone).unwrap(), &[0xD800]);
        assert_eq!(h.code_point_at(lone, 0).unwrap(), Some((0xD800, 1)));
        assert_eq!(h.to_string_lossy(lone).unwrap(), "\u{FFFD}");
        assert_ne!(lone, h.intern_units(&[0xFFFD]).unwrap()); // not replaced
        let hi = h.intern_units(&[0xD83D]).unwrap();
        let lo = h.intern_units(&[0xDE00]).unwrap();
        let joined = h.concat(hi, lo).unwrap();
        assert_eq!(h.code_point_at(joined, 0).unwrap(), Some((0x1F600, 2)));
        assert_eq!(joined, h.intern_str("😀").unwrap()); // joined halves equal the literal
        let empty = h.intern_str("").unwrap();
        assert_eq!(h.str_len(empty).unwrap(), 0);
        let emoji = h.intern_str("😀").unwrap();
        assert_eq!(h.substring(a, 3, 1).unwrap(), h.substring(a, 1, 3).unwrap());
        assert_eq!(h.substring(a, 1, 3).unwrap(), emoji);
    }

    #[test]
    fn substring_clamps_and_swaps() {
        let mut h = Heap::default();
        let a = h.intern_str("abcdef").unwrap();
        let cd = h.intern_str("cd").unwrap();
        assert_eq!(h.substring(a, 2, 4).unwrap(), cd);
        assert_eq!(h.substring(a, 4, 2).unwrap(), cd);
        assert_eq!(h.substring(a, 0, 99).unwrap(), a);
        let e = h.intern_str("").unwrap();
        assert_eq!(h.substring(a, 9, 20).unwrap(), e);
    }

    #[test]
    fn primitive_conversions() {
        let mut h = Heap::default();
        let s = h.intern_str("").unwrap();
        let z = h.intern_str("0").unwrap(); // the string "0" is true
        assert!(!h.to_boolean(Value::String(s)).unwrap());
        assert!(h.to_boolean(Value::String(z)).unwrap());
        assert!(!h.to_boolean(n(f64::NAN)).unwrap());
        assert!(!h.to_boolean(n(-0.0)).unwrap());
        let o = h.new_object(None).unwrap();
        assert!(h.to_boolean(Value::Object(o)).unwrap());
        assert_eq!(h.to_number(Value::String(z)).unwrap(), 0.0);
        assert!(h.to_number(Value::Undefined).unwrap().is_nan());
        assert_eq!(h.to_number(Value::Bool(true)).unwrap(), 1.0);
        assert_eq!(h.to_number(Value::Null).unwrap(), 0.0);
        let sym = h.new_symbol(None).unwrap();
        assert_eq!(h.to_number(Value::Symbol(sym)), Err(HeapError::SymbolConversion));
        assert_eq!(h.to_string(Value::Symbol(sym)), Err(HeapError::SymbolConversion));
        assert!(matches!(h.to_number(Value::Object(o)), Err(HeapError::NeedsExecution(_))));
        let t = h.to_string(n(-1.5)).unwrap();
        assert_eq!(h.to_string_lossy(t).unwrap(), "-1.5");
        assert_eq!(h.type_of(Value::Null).unwrap(), "object");
        assert_eq!(h.type_of(Value::Symbol(sym)).unwrap(), "symbol");
        // 1 == "1", true == "1", null == undefined, null != 0, NaN != NaN, objects by identity.
        let one = h.intern_str("1").unwrap();
        assert!(h.loose_equals(n(1.0), Value::String(one)).unwrap());
        assert!(h.loose_equals(Value::Bool(true), Value::String(one)).unwrap());
        assert!(h.loose_equals(Value::Null, Value::Undefined).unwrap());
        assert!(!h.loose_equals(Value::Null, n(0.0)).unwrap());
        assert!(!h.loose_equals(n(f64::NAN), n(f64::NAN)).unwrap());
        assert!(h.loose_equals(Value::Object(o), Value::Object(o)).unwrap());
        assert!(!h.loose_equals(Value::Object(o), Value::Null).unwrap());
        assert!(!h.loose_equals(Value::Symbol(sym), n(1.0)).unwrap());
        assert!(matches!(h.loose_equals(Value::Object(o), n(1.0)), Err(HeapError::NeedsExecution(_))));
        assert!(matches!(h.loose_equals(Value::Bool(true), Value::Object(o)), Err(HeapError::NeedsExecution(_))));
    }

    #[test]
    fn keys_canonicalize() {
        let mut h = Heap::default();
        assert_eq!(key(&mut h, "7"), PropertyKey::Index(7));
        assert!(matches!(key(&mut h, "07"), PropertyKey::String(_)));
        assert!(matches!(key(&mut h, "4294967295"), PropertyKey::String(_)));
        assert_eq!(h.to_property_key(n(3.0)).unwrap(), PropertyKey::Index(3));
        assert_eq!(h.to_property_key(n(-0.0)).unwrap(), PropertyKey::Index(0));
        let k = h.to_property_key(n(1.5)).unwrap();
        assert_eq!(k, key(&mut h, "1.5"));
        assert_eq!(h.to_property_key(n(-1.0)).unwrap(), key(&mut h, "-1"));
        assert_eq!(h.to_property_key(n(4294967295.0)).unwrap(), key(&mut h, "4294967295"));
        // A hand-built String key that is an index breaks the invariant: rejected.
        let s = h.intern_str("5").unwrap();
        let o = h.new_object(None).unwrap();
        assert_eq!(h.get(o, PropertyKey::String(s)).err(), Some(HeapError::InvalidKey));
        let v = h.key_to_value(PropertyKey::Index(12)).unwrap();
        assert!(v.strict_equals(Value::String(h.intern_str("12").unwrap())));
    }

    #[test]
    fn data_property_basics_and_nan_zero_bits() {
        let mut h = Heap::default();
        let o = h.new_object(None).unwrap();
        let a = key(&mut h, "a");
        assert!(matches!(h.get(o, a).unwrap(), Get::Value(Value::Undefined)));
        assert_eq!(setn(&mut h, o, a, 1.0), Set::Done);
        assert_eq!(num(&h, o, a), 1.0);
        let p = h.get_own_property(o, a).unwrap().unwrap();
        assert!(matches!(p, Property::Data { writable: true, enumerable: true, configurable: true, .. }));
        let nan = f64::from_bits(0x7ff8_0000_0000_1234);
        setn(&mut h, o, a, nan);
        assert_eq!(num(&h, o, a).to_bits(), nan.to_bits());
        setn(&mut h, o, a, -0.0);
        assert!(num(&h, o, a).is_sign_negative());
        assert!(h.has_own_property(o, a).unwrap());
        assert!(h.delete(o, a).unwrap());
        assert!(!h.has_own_property(o, a).unwrap());
        assert!(h.delete(o, a).unwrap()); // deleting a missing key is true
    }

    #[test]
    fn descriptor_transitions() {
        let mut h = Heap::default();
        let o = h.new_object(None).unwrap();
        let k = key(&mut h, "k");
        // Defaults for a new property are all false.
        let d = PropertyDescriptor { value: Some(n(1.0)), ..Default::default() };
        assert!(h.define_own_property(o, k, &d).unwrap());
        assert!(matches!(h.get_own_property(o, k).unwrap(), Some(Property::Data { writable: false, enumerable: false, configurable: false, .. })));
        // Non-configurable, non-writable: value change, flag raise and kind change are rejected.
        assert!(!h.define_own_property(o, k, &PropertyDescriptor { value: Some(n(2.0)), ..Default::default() }).unwrap());
        assert!(h.define_own_property(o, k, &PropertyDescriptor { value: Some(n(1.0)), ..Default::default() }).unwrap()); // same value ok
        assert!(!h.define_own_property(o, k, &PropertyDescriptor { writable: Some(true), ..Default::default() }).unwrap());
        assert!(!h.define_own_property(o, k, &PropertyDescriptor { enumerable: Some(true), ..Default::default() }).unwrap());
        assert!(!h.define_own_property(o, k, &PropertyDescriptor { configurable: Some(true), ..Default::default() }).unwrap());
        assert!(!h.define_own_property(o, k, &PropertyDescriptor { get: Some(None), ..Default::default() }).unwrap());
        assert!(!h.delete(o, k).unwrap());
        assert_eq!(setn(&mut h, o, k, 5.0), Set::Rejected);
        // writable true -> false is allowed on a non-configurable property; back is not.
        let w = key(&mut h, "w");
        assert!(h.define_own_property(o, w, &locked(true, true, false, 1.0)).unwrap());
        assert!(h.define_own_property(o, w, &PropertyDescriptor { value: Some(n(9.0)), writable: Some(false), ..Default::default() }).unwrap());
        assert_eq!(num(&h, o, w), 9.0);
        assert!(!h.define_own_property(o, w, &PropertyDescriptor { writable: Some(true), ..Default::default() }).unwrap());
        // A configurable property can change kind; enumerable/configurable carry over.
        let c = key(&mut h, "c");
        assert!(h.define_own_property(o, c, &locked(false, true, true, 1.0)).unwrap());
        let g = h.new_object(None).unwrap();
        assert!(h.define_own_property(o, c, &PropertyDescriptor { get: Some(Some(g)), ..Default::default() }).unwrap());
        assert!(matches!(h.get_own_property(o, c).unwrap(), Some(Property::Accessor { get: Some(x), set: None, enumerable: true, configurable: true }) if x == g));
        // And back to data: value resets to undefined, writable to false.
        assert!(h.define_own_property(o, c, &PropertyDescriptor { value: Some(n(3.0)), ..Default::default() }).unwrap());
        assert!(matches!(h.get_own_property(o, c).unwrap(), Some(Property::Data { writable: false, enumerable: true, configurable: true, .. })));
        // Mixed data and accessor fields are a TypeError.
        let bad = PropertyDescriptor { value: Some(n(1.0)), get: Some(None), ..Default::default() };
        assert_eq!(h.define_own_property(o, c, &bad), Err(HeapError::InvalidDescriptor));
        // Non-configurable accessor: only identical get/set pass.
        let ac = key(&mut h, "ac");
        let d = PropertyDescriptor { get: Some(Some(g)), configurable: Some(false), ..Default::default() };
        assert!(h.define_own_property(o, ac, &d).unwrap());
        assert!(h.define_own_property(o, ac, &PropertyDescriptor { get: Some(Some(g)), ..Default::default() }).unwrap());
        assert!(!h.define_own_property(o, ac, &PropertyDescriptor { get: Some(None), ..Default::default() }).unwrap());
        assert!(!h.define_own_property(o, ac, &PropertyDescriptor { set: Some(Some(g)), ..Default::default() }).unwrap());
        // Empty descriptor on an existing property is a no-op that succeeds; on a missing one it creates.
        assert!(h.define_own_property(o, ac, &PropertyDescriptor::default()).unwrap());
        let e = key(&mut h, "e");
        assert!(h.define_own_property(o, e, &PropertyDescriptor::default()).unwrap());
        assert!(matches!(val(&h, o, e), Value::Undefined));
        // Non-extensible: no new keys, existing keys may change.
        h.prevent_extensions(o).unwrap();
        let nw = key(&mut h, "nw");
        assert!(!h.define_own_property(o, nw, &PropertyDescriptor::data(n(1.0))).unwrap());
        assert_eq!(setn(&mut h, o, nw, 1.0), Set::Rejected);
        assert_eq!(setn(&mut h, o, c, 1.0), Set::Rejected); // c is non-writable
        assert!(h.delete(o, c).unwrap());
    }

    #[test]
    fn accessors_are_returned_not_called() {
        let mut h = Heap::default();
        let o = h.new_object(None).unwrap();
        let f = h.new_object(None).unwrap();
        let k = key(&mut h, "x");
        let d = PropertyDescriptor { get: Some(Some(f)), set: Some(Some(f)), enumerable: Some(true), configurable: Some(true), ..Default::default() };
        assert!(h.define_own_property(o, k, &d).unwrap());
        assert!(matches!(h.get(o, k).unwrap(), Get::Accessor { getter: Some(g) } if g == f));
        assert_eq!(h.set(o, k, n(1.0)).unwrap(), Set::Setter(f));
        let child = h.new_object(Some(o)).unwrap();
        assert_eq!(h.set(child, k, n(1.0)).unwrap(), Set::Setter(f)); // inherited setter
        assert!(!h.has_own_property(child, k).unwrap());
        let ro = key(&mut h, "ro");
        let d = PropertyDescriptor { get: Some(Some(f)), configurable: Some(true), ..Default::default() };
        h.define_own_property(o, ro, &d).unwrap();
        assert_eq!(h.set(o, ro, n(1.0)).unwrap(), Set::Rejected); // no setter
        let none = key(&mut h, "none");
        assert!(matches!(h.get(o, none).unwrap(), Get::Value(Value::Undefined)));
    }

    #[test]
    fn enumeration_order() {
        let mut h = Heap::default();
        let o = h.new_object(None).unwrap();
        let s1 = h.new_symbol(None).unwrap();
        for k in ["b", "2", "a", "10", "1", "-1", "01"] {
            let k = key(&mut h, k);
            setn(&mut h, o, k, 0.0);
        }
        setn(&mut h, o, PropertyKey::Symbol(s1), 0.0);
        let late = key(&mut h, "0");
        setn(&mut h, o, late, 0.0);
        // Integers ascending numeric (not string) order, then strings and symbols by creation.
        assert_eq!(names(&h, o), ["0", "1", "2", "10", "b", "a", "-1", "01", "@"]);
        // Updating keeps the position; delete and re-add moves to the end.
        let b = key(&mut h, "b");
        setn(&mut h, o, b, 1.0);
        assert_eq!(names(&h, o)[4], "b");
        h.delete(o, b).unwrap();
        setn(&mut h, o, b, 1.0);
        assert_eq!(names(&h, o), ["0", "1", "2", "10", "a", "-1", "01", "b", "@"]);
    }

    #[test]
    fn many_keys_use_the_index_and_survive_deletes() {
        let mut h = Heap::default();
        let o = h.new_object(None).unwrap();
        let ks: Vec<_> = (0..200).map(|i| key(&mut h, &format!("k{i}"))).collect();
        for (i, &k) in ks.iter().enumerate() {
            setn(&mut h, o, k, i as f64);
        }
        for (i, &k) in ks.iter().enumerate() {
            if i % 3 != 0 {
                assert!(h.delete(o, k).unwrap());
            }
        }
        for (i, &k) in ks.iter().enumerate() {
            assert_eq!(h.has_own_property(o, k).unwrap(), i % 3 == 0);
            if i % 3 == 0 {
                assert_eq!(num(&h, o, k), i as f64);
            }
        }
        let want: Vec<String> = (0..200).filter(|i| i % 3 == 0).map(|i| format!("k{i}")).collect();
        assert_eq!(names(&h, o), want);
        for &k in &ks {
            h.delete(o, k).unwrap();
        }
        assert!(names(&h, o).is_empty());
        setn(&mut h, o, ks[5], 5.0);
        assert_eq!(names(&h, o), ["k5"]);
    }

    #[test]
    fn prototype_chain_and_cycles() {
        let mut h = Heap::default();
        let a = h.new_object(None).unwrap();
        let b = h.new_object(Some(a)).unwrap();
        let c = h.new_object(Some(b)).unwrap();
        let k = key(&mut h, "k");
        setn(&mut h, a, k, 7.0);
        assert_eq!(num(&h, c, k), 7.0);
        assert!(h.has_property(c, k).unwrap());
        assert!(!h.has_own_property(c, k).unwrap());
        assert_eq!(setn(&mut h, c, k, 8.0), Set::Done); // shadows, does not write the proto
        assert_eq!((num(&h, a, k), num(&h, c, k)), (7.0, 8.0));
        // Cycles rejected: self, direct, long.
        assert!(!h.set_prototype_of(a, Some(a)).unwrap());
        assert!(!h.set_prototype_of(a, Some(c)).unwrap());
        assert!(!h.set_prototype_of(b, Some(c)).unwrap());
        assert!(h.set_prototype_of(c, Some(a)).unwrap());
        assert!(h.set_prototype_of(b, Some(c)).unwrap()); // fine now: b -> c -> a
        assert!(!h.set_prototype_of(a, Some(b)).unwrap());
        assert_eq!(h.get_prototype_of(a).unwrap(), None);
        // Read-only inherited property blocks a set on the child.
        let ro = key(&mut h, "ro");
        h.define_own_property(a, ro, &locked(false, true, true, 1.0)).unwrap();
        assert_eq!(setn(&mut h, b, ro, 2.0), Set::Rejected);
        // Frozen prototype link.
        let d = h.new_object(None).unwrap();
        h.prevent_extensions(d).unwrap();
        assert!(h.set_prototype_of(d, None).unwrap()); // same value allowed
        assert!(!h.set_prototype_of(d, Some(a)).unwrap());
    }

    #[test]
    fn long_prototype_chain_does_not_recurse() {
        let mut h = Heap::default();
        let mut cur = h.new_object(None).unwrap();
        let root = cur;
        for _ in 0..200_000 {
            cur = h.new_object(Some(cur)).unwrap();
        }
        let k = key(&mut h, "k");
        setn(&mut h, root, k, 1.0);
        assert_eq!(num(&h, cur, k), 1.0);
        let c = h.collect(&[Value::Object(cur)]).unwrap();
        assert_eq!(c.objects, 0);
        assert!(!h.set_prototype_of(root, Some(cur)).unwrap());
    }

    #[test]
    fn arrays_length_holes_and_boundaries() {
        let mut h = Heap::default();
        let a = h.new_array(None).unwrap();
        let len = key(&mut h, "length");
        assert_eq!(num(&h, a, len), 0.0);
        assert_eq!(names(&h, a), ["length"]);
        assert!(matches!(h.get_own_property(a, len).unwrap(), Some(Property::Data { writable: true, enumerable: false, configurable: false, .. })));
        setn(&mut h, a, PropertyKey::Index(2), 9.0); // leaves holes 0 and 1
        assert_eq!(num(&h, a, len), 3.0);
        assert!(!h.has_own_property(a, PropertyKey::Index(0)).unwrap());
        assert!(matches!(val(&h, a, PropertyKey::Index(1)), Value::Undefined));
        assert_eq!(names(&h, a), ["2", "length"]);
        // Shrinking removes the elements; growing adds holes.
        assert!(h.define_own_property(a, len, &PropertyDescriptor { value: Some(n(2.0)), ..Default::default() }).unwrap());
        assert!(!h.has_own_property(a, PropertyKey::Index(2)).unwrap());
        assert_eq!(setn(&mut h, a, len, 5.0), Set::Done);
        assert_eq!(num(&h, a, len), 5.0);
        assert_eq!(names(&h, a), ["length"]);
        // Length given as a numeric string, and invalid lengths.
        let s = h.intern_str("1").unwrap();
        assert!(h.define_own_property(a, len, &PropertyDescriptor { value: Some(Value::String(s)), ..Default::default() }).unwrap());
        assert_eq!(num(&h, a, len), 1.0);
        for bad in [-1.0, 1.5, f64::NAN, 4294967296.0, f64::INFINITY] {
            let d = PropertyDescriptor { value: Some(n(bad)), ..Default::default() };
            assert_eq!(h.define_own_property(a, len, &d), Err(HeapError::InvalidArrayLength), "{bad}");
        }
        assert_eq!(num(&h, a, len), 1.0);
        // The largest index 2^32-2 gives length 2^32-1 and uses the sparse path.
        setn(&mut h, a, PropertyKey::Index(MAX_INDEX), 1.0);
        assert_eq!(num(&h, a, len), 4294967295.0);
        // 2^32-1 is a plain string key, not an element.
        let big = key(&mut h, "4294967295");
        setn(&mut h, a, big, 1.0);
        assert_eq!(num(&h, a, len), 4294967295.0);
        assert_eq!(names(&h, a), ["0", "4294967294", "length", "4294967295"].map(String::from).iter().filter(|s| *s != "0").cloned().collect::<Vec<_>>());
        // A huge length costs no memory: it is only a number.
        assert!(h.define_own_property(a, len, &PropertyDescriptor { value: Some(n(0.0)), ..Default::default() }).unwrap());
        assert_eq!(names(&h, a), ["length", "4294967295"]);
    }

    #[test]
    fn array_length_stops_at_non_configurable_element() {
        let mut h = Heap::default();
        let a = h.new_array(None).unwrap();
        let len = key(&mut h, "length");
        for i in 0..6 {
            setn(&mut h, a, PropertyKey::Index(i), i as f64);
        }
        // Element 3 is non-configurable (sparse path); 4 and 5 are dense.
        assert!(h.define_own_property(a, PropertyKey::Index(3), &locked(true, true, false, 3.0)).unwrap());
        let shrink = PropertyDescriptor { value: Some(n(1.0)), writable: Some(false), ..Default::default() };
        assert!(!h.define_own_property(a, len, &shrink).unwrap()); // blocked
        assert_eq!(num(&h, a, len), 4.0);
        assert_eq!(names(&h, a), ["0", "1", "2", "3", "length"]);
        assert!(matches!(h.get_own_property(a, len).unwrap(), Some(Property::Data { writable: false, .. })));
        // Length is read-only now: no growth through length or index.
        assert!(!h.define_own_property(a, PropertyKey::Index(4), &PropertyDescriptor::data(n(1.0))).unwrap());
        assert_eq!(setn(&mut h, a, len, 9.0), Set::Rejected);
        assert_eq!(setn(&mut h, a, PropertyKey::Index(0), 5.0), Set::Done); // existing element still writable
        // Shrink to the same length is fine; below is rejected.
        assert!(h.define_own_property(a, len, &PropertyDescriptor { value: Some(n(4.0)), ..Default::default() }).unwrap());
        assert!(!h.define_own_property(a, len, &PropertyDescriptor { value: Some(n(2.0)), ..Default::default() }).unwrap());
        // length cannot be deleted or made configurable.
        assert!(!h.delete(a, len).unwrap());
        assert!(!h.define_own_property(a, len, &PropertyDescriptor { configurable: Some(true), ..Default::default() }).unwrap());
    }

    #[test]
    fn dense_and_sparse_elements_stay_consistent() {
        let mut h = Heap::default();
        let a = h.new_array(None).unwrap();
        let len = key(&mut h, "length");
        setn(&mut h, a, PropertyKey::Index(5000), 1.0); // beyond the gap: sparse
        assert_eq!(h.objects.get(a.index, a.gn).unwrap().elems.len(), 0);
        setn(&mut h, a, PropertyKey::Index(0), 2.0);
        // Special attributes move an element out of the dense vector, and back.
        assert!(h.define_own_property(a, PropertyKey::Index(0), &locked(false, true, true, 2.0)).unwrap());
        assert!(h.objects.get(a.index, a.gn).unwrap().elems.is_empty());
        assert_eq!(setn(&mut h, a, PropertyKey::Index(0), 3.0), Set::Rejected);
        assert!(h.define_own_property(a, PropertyKey::Index(0), &PropertyDescriptor { writable: Some(true), ..Default::default() }).unwrap());
        assert_eq!(h.objects.get(a.index, a.gn).unwrap().elems.len(), 1);
        assert_eq!(h.objects.get(a.index, a.gn).unwrap().props.index_keys, 1); // only 5000 left sparse
        assert_eq!(names(&h, a), ["0", "5000", "length"]);
        assert_eq!(num(&h, a, len), 5001.0);
        // Deleting the last dense element trims the vector.
        assert!(h.delete(a, PropertyKey::Index(0)).unwrap());
        assert!(h.objects.get(a.index, a.gn).unwrap().elems.is_empty());
        assert!(h.delete(a, PropertyKey::Index(5000)).unwrap());
        assert_eq!(names(&h, a), ["length"]);
        assert_eq!(num(&h, a, len), 5001.0); // delete does not shrink the length
        // Ordinary objects take integer keys too, in numeric order.
        let o = h.new_object(None).unwrap();
        for i in [30u32, 4, 1_000_000, 4] {
            setn(&mut h, o, PropertyKey::Index(i), i as f64);
        }
        assert_eq!(names(&h, o), ["4", "30", "1000000"]);
    }

    #[test]
    fn stale_handles_and_slot_reuse() {
        let mut h = Heap::default();
        let a = h.new_object(None).unwrap();
        let s = h.intern_str("temp").unwrap();
        let sym = h.new_symbol(Some(s)).unwrap();
        let keep = h.new_object(None).unwrap();
        let c = h.collect(&[Value::Object(keep)]).unwrap();
        assert_eq!(c, Collected { objects: 1, strings: 1, symbols: 1 });
        assert_eq!(h.get_prototype_of(a), Err(HeapError::Stale));
        assert_eq!(h.str_len(s), Err(HeapError::Stale));
        assert_eq!(h.symbol_description(sym), Err(HeapError::Stale));
        assert_eq!(h.collect(&[Value::Object(a)]), Err(HeapError::Stale)); // stale root
        let b = h.new_object(None).unwrap(); // reuses a's slot
        assert_eq!(b.index, a.index);
        assert_ne!(b.gn, a.gn);
        assert_eq!(h.is_extensible(a), Err(HeapError::Stale)); // the old handle does not see b
        assert!(h.is_extensible(b).unwrap());
        // Storing a stale handle is refused.
        let k = key(&mut h, "k");
        assert_eq!(h.set(b, k, Value::Object(a)), Err(HeapError::Stale));
        assert_eq!(h.new_object(Some(a)).err(), Some(HeapError::Stale));
        assert_eq!(h.set_prototype_of(b, Some(a)), Err(HeapError::Stale));
        // Strings re-interned after death get a new generation, and old handles stay dead.
        let s2 = h.intern_str("temp").unwrap();
        assert_ne!(s2, s);
        assert!(h.str_len(s).is_err());
        assert_eq!(h.str_len(s2).unwrap(), 4);
    }

    #[test]
    fn generation_wrap_retires_the_slot() {
        let mut h = Heap::new(HeapLimits { max_objects: 2, ..Default::default() });
        let a = h.new_object(None).unwrap();
        // Pretend this slot has been reused 2^32-1 times.
        h.objects.slots[a.index as usize].gn = u32::MAX;
        let old = ObjId { index: a.index, gn: u32::MAX };
        assert!(h.is_extensible(old).is_ok());
        h.collect(&[]).unwrap();
        assert_eq!(h.stats().retired_slots, 1);
        assert_eq!(h.is_extensible(old), Err(HeapError::Stale)); // gn matches but the slot is empty
        let b = h.new_object(None).unwrap();
        assert_ne!(b.index, old.index); // the retired slot is never reused
        // The retired slot still counts against the cap: capacity is 2 slots, 1 retired, 1 used.
        assert_eq!(h.new_object(None).err(), Some(HeapError::Limit(Limit::Objects)));
        assert_eq!(h.is_extensible(old), Err(HeapError::Stale));
        h.collect(&[]).unwrap();
        assert_eq!(h.stats().live_objects, 0);
        assert!(h.new_object(None).is_ok());
    }

    #[test]
    fn allocation_caps() {
        let mut h = Heap::new(HeapLimits { max_objects: 3, max_strings: 4, max_symbols: 2, max_string_units: 8, ..Default::default() });
        for _ in 0..3 {
            h.new_object(None).unwrap();
        }
        assert_eq!(h.new_object(None).err(), Some(HeapError::Limit(Limit::Objects)));
        h.new_symbol(None).unwrap();
        h.new_symbol(None).unwrap();
        assert_eq!(h.new_symbol(None).err(), Some(HeapError::Limit(Limit::Symbols)));
        // "length" is string 1 of 4.
        h.intern_str("a").unwrap();
        h.intern_str("b").unwrap();
        h.intern_str("c").unwrap();
        assert_eq!(h.intern_str("d").err(), Some(HeapError::Limit(Limit::Strings)));
        assert!(h.intern_str("a").is_ok()); // an existing string needs no slot
        assert_eq!(h.intern_str("123456789").err(), Some(HeapError::Limit(Limit::StringLength)));
        // Collection frees slots again; a failed alloc changed nothing.
        let before = h.stats();
        h.collect(&[]).unwrap();
        assert!(h.stats().bytes < before.bytes);
        assert!(h.new_object(None).is_ok());
        assert!(h.intern_str("d").is_ok());
    }

    #[test]
    fn byte_budget_bounds_growth_and_stays_exact() {
        let mut h = Heap::new(HeapLimits { max_bytes: 20_000, ..Default::default() });
        let o = h.new_object(None).unwrap();
        let mut i = 0u32;
        let err = loop {
            let k = key_or_err(&mut h, &format!("key{i}"));
            let k = match k {
                Ok(k) => k,
                Err(e) => break e,
            };
            match h.set(o, k, n(1.0)) {
                Ok(_) => i += 1,
                Err(e) => break e,
            }
        };
        assert_eq!(err, HeapError::Limit(Limit::Bytes));
        assert!(i > 20);
        assert!(h.stats().bytes <= 20_000);
        // Bytes equal the sum recomputed from scratch, before and after collection.
        let recount = |h: &Heap| {
            h.objects.slots.iter().filter_map(|s| s.val.as_ref()).map(obj_bytes).sum::<usize>()
                + h.strings.slots.iter().filter_map(|s| s.val.as_ref()).map(str_bytes).sum::<usize>()
                + h.symbols.live * std::mem::size_of::<Slot<Symbol>>()
        };
        assert_eq!(h.stats().bytes, recount(&h));
        h.collect(&[Value::Object(o)]).unwrap();
        assert_eq!(h.stats().bytes, recount(&h));
        // Dense elements are charged too.
        let mut h = Heap::new(HeapLimits { max_bytes: 20_000, ..Default::default() });
        let a = h.new_array(None).unwrap();
        let mut hit = false;
        for j in 0..100_000 {
            if h.set(a, PropertyKey::Index(j), n(0.0)).is_err() {
                hit = true;
                break;
            }
        }
        assert!(hit);
        assert_eq!(h.stats().bytes, recount(&h));
    }
    fn key_or_err(h: &mut Heap, s: &str) -> R<PropertyKey> {
        h.key_from_str(s)
    }

    #[test]
    fn unreachable_cycles_are_collected_and_reachable_ones_survive() {
        let mut h = Heap::default();
        let k = key(&mut h, "next");
        // A ring of 100 objects, each pointing to the next (and to itself as prototype of none).
        let ring: Vec<_> = (0..100).map(|_| h.new_object(None).unwrap()).collect();
        for i in 0..100 {
            let v = Value::Object(ring[(i + 1) % 100]);
            assert_eq!(h.set(ring[i], k, v).unwrap(), Set::Done);
        }
        // A prototype cycle cannot exist, but a property/proto mix can: x.proto = y, y.p = x.
        let (x, y) = (h.new_object(None).unwrap(), h.new_object(None).unwrap());
        h.set_prototype_of(x, Some(y)).unwrap();
        let p = key(&mut h, "p");
        h.set(y, p, Value::Object(x)).unwrap();
        // Dead: strings and symbols reachable only from dead objects go too.
        let dead_s = h.intern_str("only-from-ring").unwrap();
        h.set(ring[50], k, Value::String(dead_s)).unwrap(); // breaks the ring at 50; 51..99,0..49 still a chain
        let sym = h.new_symbol(Some(dead_s)).unwrap();
        h.set(x, p, Value::Symbol(sym)).unwrap();
        let before = h.stats();
        let live_before = before.live_objects;
        assert_eq!(live_before, 102);
        // Root one ring member: 50 -> ... hold ring[0]: 0..=50 reachable (50's next is a string now).
        let c = h.collect(&[Value::Object(ring[0])]).unwrap();
        assert_eq!(c.objects, 102 - 51);
        assert_eq!(h.stats().live_objects, 51);
        assert_eq!(c.symbols, 1);
        assert_eq!(h.stats().live_strings, 3); // "length", "next", "only-from-ring" (held by ring[50])
        assert_eq!(h.str_len(dead_s).unwrap(), 14); // alive: ring[50] holds it
        assert!(h.is_extensible(ring[10]).is_ok());
        assert_eq!(h.is_extensible(ring[60]), Err(HeapError::Stale));
        assert_eq!(h.is_extensible(x), Err(HeapError::Stale));
        // Reachable data are intact.
        assert!(val(&h, ring[0], k).strict_equals(Value::Object(ring[1])));
        // No roots: everything but the permanent string goes.
        h.collect(&[]).unwrap();
        let s = h.stats();
        assert_eq!((s.live_objects, s.live_strings, s.live_symbols), (0, 1, 0));
    }

    #[test]
    fn every_edge_kind_is_traced() {
        let mut h = Heap::default();
        let holder = h.new_object(None).unwrap();
        let proto = h.new_object(None).unwrap();
        h.set_prototype_of(holder, Some(proto)).unwrap();
        let key_str = h.intern_str("keystr").unwrap();
        let val_str = h.intern_str("valstr").unwrap();
        let desc_str = h.intern_str("descstr").unwrap();
        let sym = h.new_symbol(Some(desc_str)).unwrap();
        let getter = h.new_object(None).unwrap();
        let setter = h.new_object(None).unwrap();
        let elem = h.new_object(None).unwrap();
        let sparse_elem = h.new_object(None).unwrap();
        let ks = PropertyKey::String(key_str);
        h.set(holder, ks, Value::String(val_str)).unwrap();
        h.set(holder, PropertyKey::Symbol(sym), n(1.0)).unwrap();
        let acc = key(&mut h, "acc");
        let d = PropertyDescriptor { get: Some(Some(getter)), set: Some(Some(setter)), ..Default::default() };
        h.define_own_property(holder, acc, &d).unwrap();
        let arr = h.new_array(None).unwrap();
        h.set(arr, PropertyKey::Index(0), Value::Object(elem)).unwrap();
        h.define_own_property(arr, PropertyKey::Index(1), &locked(true, true, false, 0.0)).unwrap();
        h.define_own_property(arr, PropertyKey::Index(2), &PropertyDescriptor { value: Some(Value::Object(sparse_elem)), ..Default::default() }).unwrap();
        let ak = key(&mut h, "arr");
        h.set(holder, ak, Value::Object(arr)).unwrap();
        let junk = h.new_object(None).unwrap();
        let junk_s = h.intern_str("junk").unwrap();
        let jk = key(&mut h, "x");
        h.set(junk, jk, Value::String(junk_s)).unwrap();
        let c = h.collect(&[Value::Object(holder)]).unwrap();
        assert_eq!(c.objects, 1);
        assert_eq!(c.strings, 2); // "junk" and "x"; every other string is held
        for id in [proto, getter, setter, elem, sparse_elem, arr] {
            assert!(h.is_extensible(id).is_ok());
        }
        assert_eq!(h.to_string_lossy(key_str).unwrap(), "keystr");
        assert_eq!(h.to_string_lossy(val_str).unwrap(), "valstr");
        assert_eq!(h.to_string_lossy(desc_str).unwrap(), "descstr");
        assert!(h.is_extensible(junk).is_err());
    }

    #[test]
    fn interner_is_weak_and_repeated_gc_is_stable() {
        let mut h = Heap::default();
        let keep_s = h.intern_str("keep").unwrap();
        let mut last = h.stats();
        for round in 0..20 {
            for i in 0..500 {
                h.intern_str(&format!("tmp-{round}-{i}")).unwrap();
            }
            let o = h.new_object(None).unwrap();
            let c = h.collect(&[Value::String(keep_s)]).unwrap();
            assert_eq!(c.strings, 500);
            assert_eq!(c.objects, 1);
            let s = h.stats();
            assert_eq!((s.live_strings, s.live_objects), (2, 0));
            assert_eq!(s.string_slots, last.string_slots.max(502)); // slots are reused, no growth
            last = s;
            let _ = o;
        }
        assert_eq!(h.intern_str("keep").unwrap(), keep_s);
        assert!(h.interner.count == h.strings.live);
        assert!(h.interner.table.len() <= 64); // the table shrank back
        assert_eq!(h.stats().collections, 20);
    }

    #[test]
    fn deep_graph_collects_without_recursion() {
        let mut h = Heap::default();
        let k = key(&mut h, "next");
        let head = h.new_object(None).unwrap();
        let mut cur = head;
        for _ in 0..300_000 {
            let nx = h.new_object(None).unwrap();
            h.set(cur, k, Value::Object(nx)).unwrap();
            cur = nx;
        }
        assert_eq!(h.collect(&[Value::Object(head)]).unwrap().objects, 0);
        assert_eq!(h.collect(&[]).unwrap().objects, 300_001);
    }

    #[test]
    fn layout_sizes_do_not_grow() {
        use std::mem::size_of;
        // Measured on x86-64: Value 16, Property 32, entry 48, Object 80, Slot<Object> 88.
        assert!(size_of::<Value>() <= 16);
        assert!(size_of::<Property>() <= 32);
        assert!(ENTRY_BYTES <= 48);
        assert!(size_of::<Slot<Object>>() <= 88);
        assert!(size_of::<Slot<Str>>() <= 32);
    }

    #[test]
    fn string_hash_spreads_similar_strings() {
        let mut h = Heap::default();
        for i in 0..100_000 {
            h.intern_str(&format!("k{i}")).unwrap();
        }
        // Longest run of occupied interner entries (linear probing): a bad hash makes long runs.
        let (mut run, mut longest) = (0, 0);
        for &e in h.interner.table.iter().chain(h.interner.table.iter()) {
            run = if e != 0 { run + 1 } else { 0 };
            longest = longest.max(run);
        }
        assert!(longest < 64, "longest probe run {longest}");
    }

    #[test]
    fn symbols_are_distinct_keys() {
        let mut h = Heap::default();
        let d = h.intern_str("d").unwrap();
        let (a, b) = (h.new_symbol(Some(d)).unwrap(), h.new_symbol(Some(d)).unwrap());
        assert_ne!(a, b);
        assert!(!Value::Symbol(a).strict_equals(Value::Symbol(b)));
        let o = h.new_object(None).unwrap();
        h.set(o, PropertyKey::Symbol(a), n(1.0)).unwrap();
        assert!(h.has_own_property(o, PropertyKey::Symbol(a)).unwrap());
        assert!(!h.has_own_property(o, PropertyKey::Symbol(b)).unwrap());
        assert_eq!(h.symbol_description(a).unwrap(), Some(d));
        assert_eq!(h.to_property_key(Value::Symbol(b)).unwrap(), PropertyKey::Symbol(b));
    }
}
