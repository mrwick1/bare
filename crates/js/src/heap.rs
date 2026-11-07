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
//! Storage: one slab for each kind. A handle names a slot. A slot is 8 bytes of metadata: a
//! generation number and the position of its value in a dense `Vec` of live values. Freeing a slot
//! adds 1 to the generation. A slot whose generation would pass `u32::MAX` is retired and never
//! reused, so a stale handle can never match a new owner. Metadata is never released; the dense
//! payload `Vec` is, so a large burst of dead objects does not keep Object-sized capacity.
//!
//! Collector: mark and sweep. Marking uses an explicit work list, not Rust recursion, so a long
//! prototype chain or a deep object graph cannot overflow the stack. Cycles are collected when
//! no root reaches them.
//!
//! Host fast paths (`array_length`, `dense_elems`, `edit_dense`, `fill_array`) let the evaluator read, change or make a plain
//! dense array in one step when no script can tell the difference. `set_integrity` and `test_integrity` are `Object.seal`,
//! `Object.freeze`, `Object.isSealed` and `Object.isFrozen`.
//!
//! Host named properties (`new_named`, `heap_named.rs`) make an object whose own string-keyed properties belong to the host
//! (WebIDL named properties). Reads are answered at once by a `NamedSource`. A write, a define or a delete goes to the host
//! through the evaluator (`HeapError::NeedsNamed`, `Host::named_write`), because it can throw and needs the evaluator's budget.
//!
//! Not supported (needs the evaluator): calling getters and setters, `ToPrimitive` on objects
//! (`NeedsExecution`), `typeof` of a callable (`"function"`), Symbol.for registry, running a `Proxy` trap (the heap answers
//! `HeapError::Proxy` and the evaluator runs it: `heap_proxy.rs`), checking that an accessor is callable. Accessors are stored, validated and traced; the
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
    /// Most BigInt entries (a BigInt that fits in `i64` is inline and has no entry).
    pub max_bigs: usize,
    /// Most 64-bit limbs in one BigInt. 2^18 limbs are 2^24 bits, about 5 million decimal digits.
    pub max_big_limbs: usize,
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
            max_bigs: 1 << 24,
            max_big_limbs: 1 << 18,
            max_string_units: (1 << 29) - 24, // max_bytes also bounds string payload
            max_bytes: 1 << 30,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Limit {
    Objects,
    Strings,
    Symbols,
    Bigs,
    /// One BigInt would have more limbs than `max_big_limbs`.
    BigLimbs,
    StringLength,
    Bytes,
    /// One byte buffer is longer than `set_buffer_limits` allows.
    BufferLength,
    /// All live byte buffers together would pass `set_buffer_limits`.
    BufferBytes,
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
    /// A symbol written to an element of a typed array (ToNumber of a symbol: a TypeError in JS).
    SymbolToNumber,
    /// A BigInt where JS converts to a number (`+1n`, `Math.abs(1n)`: a TypeError in JS).
    BigIntToNumber,
    /// The operation needs the evaluator (`ToPrimitive` on an object). Nothing was changed.
    NeedsExecution(&'static str),
    /// A value that is not a BigInt written to an element of a `BigInt64Array` or `BigUint64Array`: the evaluator converts it
    /// with ToBigInt and repeats the write. Nothing was changed.
    NeedsBigInt,
    /// Inside the heap only: a read of a `BigUint64Array` element above `i64::MAX`, which needs a heap entry (the read path
    /// has no `&mut`). `get_own_property` and `lookup` catch it. It never reaches a caller.
    BigUnsigned(u64),
    /// Inside the heap only: `to_elem_number` met a BigInt view. The callers write the BigInt another way.
    BigElement,
    /// A property of a host-indexed object (`new_indexed`) has no JavaScript object yet. Nothing was changed. The evaluator asks
    /// the host to make it (`Host::fill_indexed`) and repeats the operation.
    NeedsHost { owner: ObjId, index: u32 },
    /// A host-indexed object has no host source, or the host cannot answer now (its state is borrowed). It stops the script.
    HostUnavailable,
    /// A write, define or delete of a property of a host named object (`new_named`) needs the host and the evaluator. The request
    /// waits in the heap (`take_named_request`). The evaluator runs `Host::named_write`, gives the answer back
    /// (`complete_named`) and repeats the operation, which then returns that answer. Nothing was changed when it asked.
    NeedsNamed,
    /// The operation met a `Proxy` (`heap_proxy.rs`). The heap cannot run a trap, so it changed nothing and names the proxy. The
    /// evaluator runs the internal method of that proxy from there. A path with no such handler turns this into `Unsupported`.
    Proxy(ObjId),
}

#[path = "heap_big.rs"]
mod big;
#[path = "heap_binary.rs"]
mod binary;
pub use binary::{load, sort_elems, store, Decoder, Elem, ExoticKind, IndexedSource, Slot, TYPED};
#[path = "heap_named.rs"]
mod named;
pub use named::{NamedOp, NamedRequest, NamedSource};
#[path = "heap_prim.rs"]
mod prim;
#[path = "heap_proxy.rs"]
mod proxy;

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
/// `inline(always)`: `ordinary_define` was its only caller and had it inlined. `heap_prim.rs` and `is_compatible_descriptor` are more
/// callers. Without this the compiler outlines it and every plain `define` pays a call.
#[inline(always)]
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
            Property::Data { value, writable: false, .. }
                if d.writable == Some(true) || d.value.is_some_and(|v| !v.same_value(value)) => return None,
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
/// This is not a DoS-resistant hash. Heap limits bound storage, not hash-collision work.
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
    /// 0 for an ordinary object. `EXOTIC`: the payload is in `Heap::exotics` (a buffer, a view, a text codec). See
    /// `heap_binary.rs`. `EXOTIC_NAMED`: also an object whose string-keyed own properties are answered by the host
    /// (`Heap::new_named`). `EXOTIC_STR`: a String wrapper (`Heap::new_primitive`). One byte, so the object stays within its size budget below.
    exotic: u8,
    /// A function whose own `prototype` holds a placeholder: the object is made on first use (`Heap::unlazy`).
    lazy_proto: bool,
    /// Dense index properties that are plain data (writable, enumerable, configurable).
    /// `None` is a hole. An index lives here or in `props`, never in both.
    elems: Vec<Option<Value>>,
    /// Everything else: named keys, symbols, and index properties that are sparse or special.
    /// An array's `length` is the first entry.
    props: PropMap,
}

const EXOTIC: u8 = 1;
const EXOTIC_NAMED: u8 = 2;
/// A String wrapper object: virtual `length` and index properties (`heap_prim.rs`).
const EXOTIC_STR: u8 = 4;
/// A `Proxy` (`heap_proxy.rs`). It shares the `exotic != 0` test of the other exotic objects, so an ordinary object pays nothing.
const EXOTIC_PROXY: u8 = 8;

// The flags fit in the padding.
const _: () = assert!(std::mem::size_of::<Object>() <= 80);

const ENTRY_BYTES: usize = std::mem::size_of::<Option<(PropertyKey, Property)>>();
const ELEM_BYTES: usize = std::mem::size_of::<Option<Value>>();
/// Accounted cost of one hash-index entry (key + position + table overhead, estimate).
const INDEX_ENTRY_BYTES: usize = 24;

/// Accounted cost of one live value in a slab: the value and its 8-byte slot metadata.
const fn slot_bytes<T>() -> usize {
    std::mem::size_of::<T>() + std::mem::size_of::<Meta>()
}

fn obj_bytes(o: &Object) -> usize {
    slot_bytes::<Object>()
        + o.elems.len() * ELEM_BYTES
        + o.props.entries.len() * ENTRY_BYTES
        + if o.props.index.is_some() { o.props.live as usize * INDEX_ENTRY_BYTES } else { 0 }
}

struct Str {
    units: Box<[u16]>,
    hash: u32,
    /// The first unit is a digit, so the string may be an array index. Read without a jump to `units`, a separate block that is often cold.
    digit: bool,
}

struct Symbol {
    description: Option<StrId>,
}

fn string_cost(units: usize) -> usize {
    slot_bytes::<Str>() + units * 2 + 8 // 8: two interner slots at load 0.5
}

fn str_bytes(s: &Str) -> usize {
    string_cost(s.units.len())
}

// ---------- slabs ----------

/// Vacant marker for `Meta::dense`. A slab never has `u32::MAX` slots, so it is never a position.
const VACANT: u32 = u32::MAX;

/// Stable per-slot data. `dense` is the position of the value in `Slab::live`, or `VACANT`.
struct Meta {
    gn: u32,
    dense: u32,
}

/// Slots are stable; values are dense. `meta[slot].dense` and `back[dense]` are inverse:
/// `back[meta[i].dense] == i` for every occupied slot. A sweep compacts `live` and `back` in order
/// and rewrites `dense` of the survivors that moved. A free slot is on `free` exactly once; a retired slot is on neither.
struct Slab<T> {
    meta: Vec<Meta>,
    live: Vec<T>,
    back: Vec<u32>,
    free: Vec<u32>,
    /// Mark bits by dense position. Only valid during `collect`.
    marks: Vec<bool>,
    retired: usize,
    max: usize,
    allocs: u64,
    /// `allocs` at the previous sweep, to measure how much a mutator allocates between sweeps.
    allocs_at_sweep: u64,
}

/// Payload capacity is released only when at least this many bytes would be freed.
const TRIM_MIN_BYTES: usize = 1 << 20;

impl<T> Slab<T> {
    fn new(max: usize) -> Self {
        Slab { meta: Vec::new(), live: Vec::new(), back: Vec::new(), free: Vec::new(), marks: Vec::new(), retired: 0, max, allocs: 0, allocs_at_sweep: 0 }
    }
    fn can_alloc(&self) -> bool {
        !self.free.is_empty() || (self.meta.len() < self.max && self.meta.len() < u32::MAX as usize)
    }
    fn alloc(&mut self, v: T, limit: Limit) -> Result<(u32, u32), HeapError> {
        let (i, gn) = if let Some(i) = self.free.pop() {
            (i, self.meta[i as usize].gn)
        } else if self.meta.len() < self.max && self.meta.len() < u32::MAX as usize {
            self.meta.push(Meta { gn: 0, dense: VACANT });
            (self.meta.len() as u32 - 1, 0)
        } else {
            return Err(HeapError::Limit(limit));
        };
        self.meta[i as usize].dense = self.live.len() as u32;
        self.live.push(v);
        self.back.push(i);
        self.allocs += 1;
        Ok((i, gn))
    }
    /// `VACANT` is `u32::MAX`, and `live` never has that many values, so a vacant slot misses here.
    fn get(&self, index: u32, gn: u32) -> Option<&T> {
        let m = self.meta.get(index as usize)?;
        if m.gn == gn { self.live.get(m.dense as usize) } else { None }
    }
    fn get_mut(&mut self, index: u32, gn: u32) -> Option<&mut T> {
        let m = self.meta.get(index as usize)?;
        if m.gn == gn { self.live.get_mut(m.dense as usize) } else { None }
    }
    /// Free every live value whose mark is clear, in allocation order. `f` sees each freed value.
    /// Survivors keep their slots and their relative order; their dense positions shift down.
    /// Marks are left stale: `collect` rebuilds them.
    fn sweep(&mut self, mut f: impl FnMut(&T)) -> usize {
        let before = self.live.len();
        let Slab { meta, live, back, free, marks, retired, .. } = self;
        // One pass: drop the dead in order, and compact `back` and rewrite `dense` in step.
        let (mut i, mut w) = (0, 0);
        live.retain(|v| {
            let slot = back[i] as usize;
            let keep = marks[i];
            if keep {
                if w != i {
                    back[w] = slot as u32;
                    meta[slot].dense = w as u32;
                }
                w += 1;
            } else {
                f(v);
                let m = &mut meta[slot];
                m.dense = VACANT;
                match m.gn.checked_add(1) {
                    Some(g) => {
                        m.gn = g;
                        free.push(slot as u32);
                    }
                    None => *retired += 1,
                }
            }
            i += 1;
            keep
        });
        back.truncate(w);
        self.marks.clear();
        self.trim();
        self.allocs_at_sweep = self.allocs;
        before - self.live.len()
    }
    /// Give back large excess payload capacity. Keep room for twice the live values and for as
    /// many new values as were allocated since the previous sweep, so steady churn does not
    /// shrink and regrow every round. A burst that dies with no new allocation is released.
    fn trim(&mut self) {
        let since = (self.allocs - self.allocs_at_sweep).min(usize::MAX as u64) as usize;
        let keep = (self.live.len() * 2).max(self.live.len().saturating_add(since));
        let excess = self.live.capacity().saturating_sub(keep) * std::mem::size_of::<T>();
        if excess >= TRIM_MIN_BYTES && self.live.capacity() >= keep * 2 {
            self.live.shrink_to(keep);
            self.back.shrink_to(keep);
            self.marks.shrink_to(keep);
        }
    }
    /// Capacity bytes of the slab's own vectors (not of buffers the values own).
    fn capacity_bytes(&self) -> usize {
        use std::mem::size_of;
        self.meta.capacity() * size_of::<Meta>() + self.live.capacity() * size_of::<T>()
            + (self.back.capacity() + self.free.capacity()) * 4 + self.marks.capacity()
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

/// Strings up to this many code units are built on the stack before the interner lookup.
const SHORT_UNITS: usize = 64;

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
    /// Entries are dense positions in `strs.live`. A sweep moves values, so `rebuild` follows it.
    fn find(&self, strs: &Slab<Str>, hash: u32, units: &[u16]) -> Option<u32> {
        let mask = self.table.len() - 1;
        let mut p = hash as usize & mask;
        loop {
            let e = self.table[p];
            if e == 0 {
                return None;
            }
            let s = &strs.live[e as usize - 1];
            if s.hash == hash && *s.units == *units {
                return Some(e - 1);
            }
            p = (p + 1) & mask;
        }
    }
    fn insert_slot(&mut self, hash: u32, dense: u32) {
        let mask = self.table.len() - 1;
        let mut p = hash as usize & mask;
        while self.table[p] != 0 {
            p = (p + 1) & mask;
        }
        self.table[p] = dense + 1;
        self.count += 1;
    }
    fn rebuild(&mut self, strs: &Slab<Str>) {
        let size = (strs.live.len() * 2).next_power_of_two().max(16);
        self.table.clear();
        self.table.resize(size, 0);
        self.count = 0;
        for (d, s) in strs.live.iter().enumerate() {
            self.insert_slot(s.hash, d as u32);
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
    /// BigInts too large for `i64`.
    pub live_bigints: usize,
    /// Slots ever created for each kind (live + free + retired).
    pub object_slots: usize,
    pub string_slots: usize,
    pub symbol_slots: usize,
    /// Slots that reached the last generation and are never reused.
    pub retired_slots: usize,
    /// Accounted bytes of live payload (length based; allocator slack not included).
    pub bytes: usize,
    /// Capacity, in values, of the dense object payload vector and of the slot metadata vector.
    pub object_payload_capacity: usize,
    pub object_metadata_capacity: usize,
    /// Capacity bytes of all slab vectors (metadata, payload, mappings, free lists, marks).
    pub slab_capacity_bytes: usize,
    pub total_object_allocs: u64,
    pub total_string_allocs: u64,
    pub total_symbol_allocs: u64,
    pub collections: u64,
    /// Live exotic objects (buffers, views, text codecs) and the bytes of the live buffers.
    pub live_exotics: usize,
    pub buffer_bytes: usize,
}

/// Capacity bytes of the heap, by part (`Heap::footprint`). `*_used` is the part that holds an entry; the rest of `*_cap` is slack.
#[derive(Clone, Copy, Debug, Default)]
pub struct Footprint {
    pub object_slab: usize,
    pub string_slab: usize,
    pub interner: usize,
    pub entries_used: usize,
    pub entries_cap: usize,
    pub elems_used: usize,
    pub elems_cap: usize,
    pub index_tables: usize,
    pub string_units: usize,
    pub objects_with_props: usize,
    /// Objects, and capacity bytes of their property vectors, by the number of entries: 0, 1, 2, 3, 4, 5 to 8, 9 to 16, 17 to 64, more.
    pub by_len: [(usize, usize, usize); 9],
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
    bigs: Slab<big::Big>,
    big_table: big::BigTable,
    interner: Interner,
    bytes: usize,
    collections: u64,
    gray: Vec<u32>,
    /// The string `"length"`. A permanent root, so arrays can always name their length key.
    length: StrId,
    exotics: binary::ExMap,
    /// Bytes of the live buffers, and the caps (one buffer, all buffers).
    buffer_bytes: usize,
    max_buffer_bytes: usize,
    max_buffer_len: usize,
    /// The host that answers for the indexed properties of `new_indexed` objects. The heap owns it, so it dies with the heap.
    /// An `Rc` makes `Heap` `!Send`: a heap stays on its thread. A moved heap cannot lose its host silently.
    indexed: Option<std::rc::Rc<dyn IndexedSource>>,
    /// The host that answers for `new_named` objects, and the one request and answer in flight (`heap_named.rs`).
    named: Box<named::NamedState>,
    /// `Object.prototype` and the keys `prototype` and `constructor`, for lazy function prototypes. `None`: no
    /// object is lazy.
    lazy: Option<(ObjId, PropertyKey, PropertyKey)>,
}

impl Default for Heap {
    fn default() -> Self {
        Heap::new(HeapLimits::default()).expect("default limits fit the permanent length string")
    }
}

type R<T> = Result<T, HeapError>;

impl Heap {
    /// Fails when the limits cannot hold the permanent length-key string.
    pub fn new(limits: HeapLimits) -> R<Heap> {
        let mut h = Heap {
            limits,
            objects: Slab::new(limits.max_objects),
            strings: Slab::new(limits.max_strings),
            symbols: Slab::new(limits.max_symbols),
            bigs: Slab::new(limits.max_bigs),
            big_table: big::BigTable::new(),
            interner: Interner { table: vec![0; 16], count: 0 },
            bytes: 0,
            collections: 0,
            gray: Vec::new(),
            length: StrId { index: 0, gn: 0 },
            exotics: Default::default(),
            buffer_bytes: 0,
            max_buffer_bytes: binary::DEFAULT_BUFFER_CAP,
            max_buffer_len: binary::DEFAULT_BUFFER_CAP,
            indexed: None,
            named: Box::default(),
            lazy: None,
        };
        h.length = h.intern_str("length")?;
        Ok(h)
    }

    pub fn limits(&self) -> HeapLimits {
        self.limits
    }

    /// Where the memory of the heap is, by capacity (real bytes asked of the allocator, not the accounted `Stats::bytes`). It walks every object, so it is for a load tool, not for a hot path.
    pub fn footprint(&self) -> Footprint {
        let mut f = Footprint { object_slab: self.objects.capacity_bytes(), string_slab: self.strings.capacity_bytes(), interner: self.interner.table.capacity() * 4, ..Default::default() };
        for o in &self.objects.live {
            f.entries_used += o.props.entries.len() * ENTRY_BYTES;
            f.entries_cap += o.props.entries.capacity() * ENTRY_BYTES;
            f.elems_used += o.elems.len() * ELEM_BYTES;
            f.elems_cap += o.elems.capacity() * ELEM_BYTES;
            f.index_tables += o.props.index.as_ref().map_or(0, |m| m.capacity() * (std::mem::size_of::<(PropertyKey, u32)>() + 1));
            f.objects_with_props += (!o.props.entries.is_empty()) as usize;
            let n = o.props.entries.len();
            let b = match n { 0..=4 => n, 5..=8 => 5, 9..=16 => 6, 17..=64 => 7, _ => 8 };
            f.by_len[b].0 += 1;
            f.by_len[b].1 += n * ENTRY_BYTES;
            f.by_len[b].2 += o.props.entries.capacity() * ENTRY_BYTES;
        }
        f.string_units = self.strings.live.iter().map(|s| s.units.len() * 2).sum();
        f
    }

    /// Accounted bytes of the live and the dead objects and strings that a sweep has not freed yet (`Stats::bytes`, with no copy of the rest).
    pub fn used_bytes(&self) -> usize {
        self.bytes
    }

    pub fn stats(&self) -> Stats {
        Stats {
            live_objects: self.objects.live.len(),
            live_strings: self.strings.live.len(),
            live_symbols: self.symbols.live.len(),
            live_bigints: self.bigs.live.len(),
            object_slots: self.objects.meta.len(),
            string_slots: self.strings.meta.len(),
            symbol_slots: self.symbols.meta.len(),
            object_payload_capacity: self.objects.live.capacity(),
            object_metadata_capacity: self.objects.meta.capacity(),
            slab_capacity_bytes: self.objects.capacity_bytes() + self.strings.capacity_bytes() + self.symbols.capacity_bytes() + self.bigs.capacity_bytes(),
            retired_slots: self.objects.retired + self.strings.retired + self.symbols.retired + self.bigs.retired,
            bytes: self.bytes,
            total_object_allocs: self.objects.allocs,
            total_string_allocs: self.strings.allocs,
            total_symbol_allocs: self.symbols.allocs,
            collections: self.collections,
            live_exotics: self.exotics.len(),
            buffer_bytes: self.buffer_bytes,
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
            Value::BigHeap(b) => self.check_big(b),
            _ => Ok(()),
        }
    }
    fn check_opt_obj(&self, o: Option<ObjId>) -> R<()> {
        o.map_or(Ok(()), |o| self.obj(o).map(drop))
    }
    fn check_key(&self, k: PropertyKey) -> R<()> {
        match k {
            PropertyKey::Index(i) if i <= MAX_INDEX => Ok(()),
            PropertyKey::Index(_) => Err(HeapError::InvalidKey),
            PropertyKey::Symbol(s) => self.sym(s).map(drop),
            PropertyKey::String(s) => {
                let st = self.str(s)?;
                if st.digit && parse_index(&st.units).is_some() {
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
        let units = if s.is_ascii() { s.len() } else { s.encode_utf16().count() };
        if units > self.limits.max_string_units { return Err(HeapError::Limit(Limit::StringLength)); }
        if string_cost(units) > self.limits.max_bytes { return Err(HeapError::Limit(Limit::Bytes)); }
        // A short string is looked up from a stack buffer: only a new string allocates (V8 `StringTable::LookupKey`).
        if s.len() <= SHORT_UNITS {
            let mut buf = [0u16; SHORT_UNITS];
            let mut n = 0;
            for u in s.encode_utf16() {
                buf[n] = u;
                n += 1;
            }
            return self.intern(std::borrow::Cow::Borrowed(&buf[..n]));
        }
        self.intern_vec(s.encode_utf16().collect())
    }

    fn intern(&mut self, units: std::borrow::Cow<[u16]>) -> R<StrId> {
        if units.len() > self.limits.max_string_units { return Err(HeapError::Limit(Limit::StringLength)); }
        let cost = string_cost(units.len());
        if cost > self.limits.max_bytes { return Err(HeapError::Limit(Limit::Bytes)); }
        let hash = hash_units(&units);
        if let Some(i) = self.interner.find(&self.strings, hash, &units) {
            let index = self.strings.back[i as usize];
            return Ok(StrId { index, gn: self.strings.meta[index as usize].gn });
        }
        self.charge(cost)?;
        if !self.strings.can_alloc() { return Err(HeapError::Limit(Limit::Strings)); }
        let s = Str { digit: units.first().is_some_and(|c| (0x30..=0x39).contains(c)), units: units.into_owned().into_boxed_slice(), hash };
        if (self.interner.count + 1) * 2 > self.interner.table.len() {
            // Grow before the insert so the load stays at most 0.5 and a probe always ends.
            let size = self.interner.table.len() * 2;
            self.interner.table.clear();
            self.interner.table.resize(size, 0);
            self.interner.count = 0;
            for (d, old) in self.strings.live.iter().enumerate() {
                self.interner.insert_slot(old.hash, d as u32);
            }
        }
        let (index, gn) = self.strings.alloc(s, Limit::Strings)?;
        self.interner.insert_slot(hash, self.strings.live.len() as u32 - 1);
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
        if (0xD800..0xDC00).contains(&hi)
            && let Some(&lo) = u.get(i + 1).filter(|l| (0xDC00..0xE000).contains(*l)) {
            return Ok(Some((0x10000 + (((hi as u32) - 0xD800) << 10) + (lo as u32 - 0xDC00), 2)));
        }
        Ok(Some((hi as u32, 1)))
    }

    /// `a + b` for strings.
    pub fn concat(&mut self, a: StrId, b: StrId) -> R<StrId> {
        let (x, y) = (&self.str(a)?.units, &self.str(b)?.units);
        if x.len() + y.len() > self.limits.max_string_units {
            return Err(HeapError::Limit(Limit::StringLength));
        }
        let n = x.len() + y.len();
        if n <= SHORT_UNITS {
            let mut buf = [0u16; SHORT_UNITS];
            buf[..x.len()].copy_from_slice(x);
            buf[x.len()..n].copy_from_slice(y);
            return self.intern(std::borrow::Cow::Borrowed(&buf[..n]));
        }
        let mut v = Vec::with_capacity(n);
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
        let cost = slot_bytes::<Symbol>();
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
        let st = self.str(s)?;
        Ok(match if st.digit { parse_index(&st.units) } else { None } {
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
        self.check_key(k)?;
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
            Value::BigInt(n) => n != 0,
            Value::Symbol(_) | Value::Object(_) | Value::BigHeap(_) => true, // a heap BigInt is never zero
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
            Value::BigInt(_) | Value::BigHeap(_) => Err(HeapError::BigIntToNumber),
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
            Value::BigInt(_) | Value::BigHeap(_) => self.bigint_to_string(v, 10),
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
            Value::BigInt(_) | Value::BigHeap(_) => "bigint",
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
            (BigInt(_) | BigHeap(_), BigInt(_) | BigHeap(_)) => a.strict_equals(b),
            (BigInt(_) | BigHeap(_), Number(x)) | (Number(x), BigInt(_) | BigHeap(_)) => {
                let big = if a.is_bigint() { a } else { b };
                let mut buf = [0u64; 1];
                crate::bigint::cmp_f64(self.bigint_sv(big, &mut buf)?, x) == Some(std::cmp::Ordering::Equal)
            }
            (BigInt(_) | BigHeap(_), String(s)) | (String(s), BigInt(_) | BigHeap(_)) => {
                let big = if a.is_bigint() { a } else { b };
                let mut buf = [0u64; 1];
                let v = self.bigint_sv(big, &mut buf)?;
                crate::bigint::string_to_bigint(&self.str(s)?.units).is_some_and(|n| crate::bigint::cmp(v, n.sv()).is_eq())
            }
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
        if !self.objects.can_alloc() { return Err(HeapError::Limit(Limit::Objects)); }
        let mut props = PropMap::new();
        if is_array {
            let len = PropertyKey::String(self.length);
            props.set(len, Property::Data { value: Value::Number(0.0), writable: true, enumerable: false, configurable: false });
        }
        let o = Object { proto, extensible: true, is_array, exotic: 0, lazy_proto: false, elems: Vec::new(), props };
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

    /// `IsArray` of an object that is not a proxy. A proxy is `Err(Proxy)`: the answer is the one of its target.
    pub fn is_array(&self, o: ObjId) -> R<bool> {
        let ob = self.obj(o)?;
        if ob.exotic == EXOTIC_PROXY {
            return Err(HeapError::Proxy(o));
        }
        Ok(ob.is_array)
    }

    pub fn get_prototype_of(&self, o: ObjId) -> R<Option<ObjId>> {
        let ob = self.obj(o)?;
        if ob.exotic == EXOTIC_PROXY {
            return Err(HeapError::Proxy(o));
        }
        Ok(ob.proto)
    }

    /// The prototype as stored, for the collector. A proxy has none (its target and handler are traced instead).
    pub fn stored_prototype(&self, o: ObjId) -> R<Option<ObjId>> {
        Ok(self.obj(o)?.proto)
    }

    /// `Object.setPrototypeOf`. `false` for a non-extensible object with another prototype,
    /// or when the change would make a cycle.
    pub fn set_prototype_of(&mut self, o: ObjId, proto: Option<ObjId>) -> R<bool> {
        let obj = self.obj(o)?;
        if obj.exotic == EXOTIC_PROXY {
            return Err(HeapError::Proxy(o));
        }
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
        let ob = self.obj(o)?;
        if ob.exotic == EXOTIC_PROXY {
            return Err(HeapError::Proxy(o));
        }
        Ok(ob.extensible)
    }

    pub fn prevent_extensions(&mut self, o: ObjId) -> R<()> {
        if self.obj(o)?.exotic == EXOTIC_PROXY {
            return Err(HeapError::Proxy(o));
        }
        self.objects.get_mut(o.index, o.gn).unwrap().extensible = false;
        Ok(())
    }

    fn own(&self, o: &Object, key: PropertyKey) -> Option<Property> {
        if let PropertyKey::Index(i) = key
            && let Some(Some(v)) = o.elems.get(i as usize) {
            return Some(Property::Data { value: *v, writable: true, enumerable: true, configurable: true });
        }
        o.props.get(key)
    }

    /// Give the heap what a lazy function prototype needs: `Object.prototype` and the keys `prototype` and
    /// `constructor`. The realm does this once.
    pub fn set_lazy_prototype(&mut self, object_proto: ObjId, k_prototype: PropertyKey, k_constructor: PropertyKey) -> R<()> {
        self.obj(object_proto)?;
        self.lazy = Some((object_proto, k_prototype, k_constructor));
        Ok(())
    }

    /// Give the function `f` an own `prototype` (writable, not enumerable, not configurable) whose object, an
    /// ordinary object with `constructor: f`, is made only when its value is first read or redefined. The key is
    /// added now, so key order and `in` are as if the object existed.
    pub fn add_lazy_prototype(&mut self, f: ObjId) -> R<()> {
        let Some((_, kp, _)) = self.lazy else { return Err(HeapError::NeedsExecution("set_lazy_prototype first")) };
        self.obj(f)?;
        self.charge(ENTRY_BYTES)?;
        self.edit(f, |ob| {
            ob.props.set(kp, Property::Data { value: Value::Undefined, writable: true, enumerable: false, configurable: false });
            ob.lazy_proto = true;
        });
        Ok(())
    }

    /// True when reading `key` of `ob` must first make its prototype object. Every path that reads or redefines an
    /// own property value checks this on the object it already holds, so other objects pay one flag test. Key lists,
    /// `in`, `delete` (never allowed: not configurable) and tracing do not need the object.
    #[inline]
    fn is_lazy(&self, ob: &Object, key: PropertyKey) -> bool {
        ob.lazy_proto && matches!(self.lazy, Some((_, kp, _)) if kp == key)
    }

    /// Make the prototype object of the lazy function `o`. Call only when `is_lazy` is true.
    #[cold]
    fn unlazy(&mut self, o: ObjId) -> R<()> {
        let Some((op, kp, kc)) = self.lazy else { return Ok(()) };
        let p = self.alloc_object(Some(op), false)?;
        let d = PropertyDescriptor { value: Some(Value::Object(o)), writable: Some(true), enumerable: Some(false), configurable: Some(true), ..Default::default() };
        self.define_own_property(p, kc, &d)?;
        self.edit(o, |ob| {
            ob.lazy_proto = false;
            if let Some(Property::Data { value, writable, enumerable, configurable }) = ob.props.get(kp) {
                debug_assert!(matches!(value, Value::Undefined));
                ob.props.set(kp, Property::Data { value: Value::Object(p), writable, enumerable, configurable });
            }
        });
        Ok(())
    }

    /// `Object.getOwnPropertyDescriptor`.
    pub fn get_own_property(&mut self, o: ObjId, key: PropertyKey) -> R<Option<Property>> {
        self.check_key(key)?;
        let mut ob = self.obj(o)?;
        if self.is_lazy(ob, key) {
            self.unlazy(o)?;
            ob = self.obj(o)?;
        }
        if ob.exotic != 0 {
            match self.exotic_own(o, key, true) {
                Ok(Some(r)) => return self.realize(o, key, r),
                Ok(None) => {}
                Err(e) => return self.exotic_err(e).map(Some),
            }
        }
        Ok(self.own(ob, key))
    }

    /// The own property as stored, without making a lazy `prototype`
    /// object. That value reads as `undefined`; the attributes are exact.
    /// For GC tracing and attribute checks only.
    pub fn peek_own_property(&self, o: ObjId, key: PropertyKey) -> R<Option<Property>> {
        self.check_key(key)?;
        let ob = self.obj(o)?;
        if ob.exotic != 0 && let Some(r) = self.exotic_own_peek(o, key)? {
            return Ok(r);
        }
        Ok(self.own(ob, key))
    }

    pub fn has_own_property(&self, o: ObjId, key: PropertyKey) -> R<bool> {
        self.check_key(key)?;
        let ob = self.obj(o)?;
        if ob.exotic != 0 && let Some(r) = self.exotic_own(o, key, false)? {
            return Ok(r.is_some());
        }
        Ok(self.own(ob, key).is_some())
    }

    /// `key in o`: own or inherited.
    pub fn has_property(&self, o: ObjId, key: PropertyKey) -> R<bool> {
        self.check_key(key)?;
        let mut cur = Some(o);
        while let Some(c) = cur {
            let obj = self.obj(c)?;
            if obj.exotic != 0 && let Some(r) = self.exotic_own(c, key, false)? {
                return Ok(r.is_some());
            }
            if self.own(obj, key).is_some() {
                return Ok(true);
            }
            cur = obj.proto;
        }
        Ok(false)
    }

    /// The first property with this key on the prototype chain, and the object that holds it.
    pub fn lookup(&mut self, o: ObjId, key: PropertyKey) -> R<Option<(ObjId, Property)>> {
        self.check_key(key)?;
        let mut cur = Some(o);
        while let Some(c) = cur {
            let mut obj = self.obj(c)?;
            if self.is_lazy(obj, key) {
                self.unlazy(c)?;
                obj = self.obj(c)?;
            }
            if obj.exotic != 0 {
                match self.exotic_own(c, key, true) {
                    Ok(Some(r)) => return self.realize_hit(c, key, r),
                    Ok(None) => {}
                    Err(e) => return self.exotic_err(e).map(|p| Some((c, p))),
                }
            }
            if let Some(p) = self.own(obj, key) {
                return Ok(Some((c, p)));
            }
            cur = obj.proto;
        }
        Ok(None)
    }

    /// The position of the own named property `key` of `o`: the hint for `own_data_at` and `set_own_data_at`.
    pub fn own_pos(&self, o: ObjId, key: PropertyKey) -> Option<u32> {
        self.obj(o).ok()?.props.find(key).map(|p| p as u32)
    }

    /// The value of the own data property `key` of `o` if it sits at position `hint`. `None` is not "absent": it asks the caller to
    /// use `lookup` (another position, an accessor, a lazy or exotic object). A hit is what `lookup` would give.
    #[inline]
    pub fn own_data_at(&self, o: ObjId, hint: u32, key: PropertyKey) -> Option<Value> {
        let ob = self.obj(o).ok()?;
        match ob.props.entries.get(hint as usize) {
            Some(Some((k, Property::Data { value, .. }))) if *k == key && ob.exotic == 0 && !ob.lazy_proto => Some(*value),
            _ => None,
        }
    }

    /// Store `v` in the own writable data property `key` of `o` if it sits at position `hint`. `false` asks the caller to use `set`.
    #[inline]
    pub fn set_own_data_at(&mut self, o: ObjId, hint: u32, key: PropertyKey, v: Value) -> bool {
        if self.check_value(v).is_err() {
            return false;
        }
        let Some(ob) = self.objects.get_mut(o.index, o.gn) else { return false };
        if ob.exotic != 0 || ob.lazy_proto {
            return false;
        }
        match ob.props.entries.get_mut(hint as usize) {
            Some(Some((k, Property::Data { value, writable: true, .. }))) if *k == key => {
                *value = v;
                true
            }
            _ => false,
        }
    }

    /// `o[key]` without calling anything.
    pub fn get(&mut self, o: ObjId, key: PropertyKey) -> R<Get> {
        Ok(match self.lookup(o, key)? {
            None => Get::Value(Value::Undefined),
            Some((_, Property::Data { value, .. })) => Get::Value(value),
            Some((_, Property::Accessor { get, .. })) => Get::Accessor { getter: get },
        })
    }

    /// `o[key] = value` (OrdinarySet with the receiver `o`). Never calls a setter: it returns it.
    pub fn set(&mut self, o: ObjId, key: PropertyKey, value: Value) -> R<Set> {
        self.check_value(value)?;
        if self.has_exotics() && let Some(r) = self.exotic_set(o, key, value)? {
            return Ok(r);
        }
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
        let ob = self.obj(o)?;
        if ob.exotic != 0 {
            if ob.exotic == EXOTIC_NAMED && let Some(r) = self.named_delete(o, key)? {
                return Ok(r);
            }
            if let Some(r) = self.exotic_own(o, key, false)? {
                return Ok(r.is_none()); // a valid index of a view or of a host-indexed object cannot be deleted
            }
        }
        let Some(p) = self.own(self.obj(o)?, key) else { return Ok(true) };
        if !p.configurable() {
            return Ok(false);
        }
        self.edit(o, |ob| ob.remove(key));
        Ok(true)
    }

    /// `Reflect.ownKeys`: array indices ascending, then strings in creation order, then symbols
    /// in creation order. An array's `length` is the first string.
    pub fn own_keys(&mut self, o: ObjId) -> R<Vec<PropertyKey>> {
        let rest = self.own_keys_with(o, true)?;
        let ex = self.obj(o)?.exotic;
        if ex == EXOTIC_PROXY {
            return Err(HeapError::Proxy(o));
        }
        if ex != EXOTIC_NAMED {
            return Ok(rest);
        }
        let mut keys = self.named_keys(o)?;
        keys.extend(rest);
        Ok(keys)
    }

    /// Upper bound on keys and storage slots scanned by `own_keys`, without allocating.
    /// Includes dense holes, removed property slots and virtual byte-view indices.
    pub fn own_key_slots(&self, o: ObjId) -> R<usize> {
        let ob = self.obj(o)?;
        let virtual_len = self.virtual_len(o)?;
        Ok(ob.elems.len().saturating_add(ob.props.entries.len()).saturating_add(virtual_len))
    }

    /// Like `own_keys`, but without the virtual integer indices of a byte view. For tracing: those hold no references.
    /// The values that the stored properties of `o` point to (elements, data values, accessor functions), pushed on `out`. The same edges as
    /// `collect` follows, with no key lookup and no copy of the key list: the evaluator's collect pass uses it for each object.
    pub fn stored_edges(&self, o: ObjId, out: &mut Vec<Value>) -> R<()> {
        let ob = self.obj(o)?;
        out.extend(ob.elems.iter().flatten());
        for (_, p) in ob.props.iter() {
            match p {
                Property::Data { value, .. } => out.push(value),
                Property::Accessor { get, set, .. } => {
                    out.extend(get.map(Value::Object));
                    out.extend(set.map(Value::Object));
                }
            }
        }
        Ok(())
    }

    fn own_keys_with(&self, o: ObjId, virt: bool) -> R<Vec<PropertyKey>> {
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
        let mut out: Vec<PropertyKey> = if matches!(ob.exotic, EXOTIC | EXOTIC_STR) && virt { self.view_keys(o)? } else { Vec::new() };
        out.extend(idx.into_iter().map(PropertyKey::Index));
        if ob.exotic == EXOTIC_STR && virt {
            out.push(PropertyKey::String(self.length)); // a String wrapper has `length` before its other string keys
        }
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

    /// The `length` and `name` of a function object that has just been made and has no other property (both read-only,
    /// not enumerable, configurable). The same result as two `define_own_property` calls, in one step: a realm makes
    /// hundreds of host functions at start, and each saved check shows in the start-up cost.
    pub fn init_function(&mut self, o: ObjId, length: (PropertyKey, f64), name: (PropertyKey, StrId)) -> R<()> {
        self.obj(o)?;
        self.charge(2 * ENTRY_BYTES)?;
        let data = |value| Property::Data { value, writable: false, enumerable: false, configurable: true };
        self.edit(o, |ob| {
            ob.props.entries.reserve_exact(2);
            ob.props.set(length.0, data(Value::Number(length.1)));
            ob.props.set(name.0, data(Value::String(name.1)));
        });
        Ok(())
    }

    /// `Object.defineProperty` as a boolean (`Reflect.defineProperty`): `false` when rejected.
    /// Errors: stale handles, a descriptor mixing data and accessor fields, an invalid array
    /// length, or a size limit.
    pub fn define_own_property(&mut self, o: ObjId, key: PropertyKey, d: &PropertyDescriptor) -> R<bool> {
        let mut obj = self.obj(o)?;
        self.check_key(key)?;
        if self.is_lazy(obj, key) {
            self.unlazy(o)?;
            obj = self.obj(o)?;
        }
        if let Some(v) = d.value {
            self.check_value(v)?;
        }
        self.check_opt_obj(d.get.flatten())?;
        self.check_opt_obj(d.set.flatten())?;
        if d.is_accessor() && d.is_data() {
            return Err(HeapError::InvalidDescriptor);
        }
        let (exotic, is_array) = (obj.exotic, obj.is_array);
        if exotic != 0 {
            // A bit test, not `==`: a third `==` makes the compiler build a jump table that every plain `define` would pay for.
            if exotic & EXOTIC_PROXY != 0 {
                return Err(HeapError::Proxy(o));
            }
            if exotic == EXOTIC_NAMED && !matches!(key, PropertyKey::Symbol(_)) {
                return self.named_write(o, key, NamedOp::Define(*d));
            }
            if exotic == EXOTIC_STR && let Some(r) = self.prim_define(o, key, d)? {
                return Ok(r);
            }
            if let Some(r) = self.view_define(o, key, d)? {
                return Ok(r);
            }
        }
        let obj = self.obj(o)?;
        if !is_array {
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
            _ if cur.is_none() || in_elems => ENTRY_BYTES + if obj.props.index.is_some() {
                INDEX_ENTRY_BYTES
            } else if obj.props.entries.len() + 1 > INDEX_AT {
                (obj.props.live as usize + 1) * INDEX_ENTRY_BYTES
            } else { 0 },
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

    /// `Array.prototype.splice` on dense storage in one move, for `shift`, `unshift` and `splice` (a host fast path).
    /// It replaces `del` elements at `start` with `items` and returns the removed ones (`None` is a hole).
    /// It answers `Ok(None)` and changes nothing unless the result is exactly what the generic algorithm gives:
    /// `o` is an array of length `len` with all elements in dense storage (no accessor, no sparse or special
    /// index), a writable `length`, no indexed property on the prototype chain (a view has virtual ones), and an extensible array (moving a
    /// value into a hole adds a property, which a non-extensible array refuses). Errors: a stale handle or a size limit.
    pub fn splice_dense(&mut self, o: ObjId, len: u32, start: usize, del: usize, items: &[Value]) -> R<Option<Vec<Option<Value>>>> {
        for &v in items {
            self.check_value(v)?;
        }
        let obj = self.obj(o)?;
        if !obj.is_array || obj.props.index_keys != 0 || obj.elems.len() != len as usize || start.saturating_add(del) > len as usize {
            return Ok(None);
        }
        let (alen, writable) = self.array_len(obj);
        let new_len = (len as u64 - del as u64) + items.len() as u64;
        if alen != len || !writable || new_len > MAX_INDEX as u64 + 1 || !obj.extensible {
            return Ok(None);
        }
        if !self.no_indexed_protos(obj.proto)? {
            return Ok(None);
        }
        self.charge(items.len().saturating_sub(del) * ELEM_BYTES)?;
        Ok(Some(self.edit(o, |ob| {
            let removed: Vec<Option<Value>> = ob.elems.splice(start..start + del, items.iter().map(|&v| Some(v))).collect();
            ob.trim_elems();
            ob.set_length(Some(new_len as u32), None);
            removed
        })))
    }

    /// True when no object on the chain from `p` holds an indexed property. A view prototype has integer-indexed
    /// keys that are not stored in `elems` (any length, even 0), so an exotic object counts as holding some.
    fn no_indexed_protos(&self, mut p: Option<ObjId>) -> R<bool> {
        while let Some(c) = p {
            let po = self.obj(c)?;
            if !po.elems.is_empty() || po.props.index_keys != 0 || po.exotic != 0 {
                return Ok(false);
            }
            p = po.proto;
        }
        Ok(true)
    }

    /// The `length` of an array, or `None` for another object.
    pub fn array_length(&self, o: ObjId) -> R<Option<u32>> {
        let obj = self.obj(o)?;
        Ok(obj.is_array.then(|| self.array_len(obj).0))
    }

    /// The elements of a plain dense array, for a read-only host fast path (`at`, `lastIndexOf`, `flat`, `toReversed`).
    /// `None` unless every index below `len` is in dense storage or is a hole that reads `undefined` on the whole chain:
    /// `o` is an array of length `len`, with no accessor, sparse or special index, and no indexed property on its prototypes.
    /// The slice is valid until the next heap change, and a hole is `None`.
    pub fn dense_elems(&self, o: ObjId, len: u64) -> R<Option<&[Option<Value>]>> {
        let obj = self.obj(o)?;
        if !obj.is_array || obj.props.index_keys != 0 || obj.elems.len() as u64 != len || len > MAX_INDEX as u64 {
            return Ok(None);
        }
        if self.array_len(obj).0 as u64 != len || !self.no_indexed_protos(obj.proto)? {
            return Ok(None);
        }
        Ok(Some(&obj.elems))
    }

    /// Change the elements of a plain dense array in place, for `reverse` and `copyWithin` (the order of elements may
    /// change, the length never does). The same conditions as `splice_dense`, which also keep every write on an existing
    /// element or a hole of an extensible array. `Ok(false)` and no change unless they hold.
    pub fn edit_dense(&mut self, o: ObjId, len: u32, f: impl FnOnce(&mut [Option<Value>])) -> R<bool> {
        let obj = self.obj(o)?;
        if !obj.is_array || obj.props.index_keys != 0 || obj.elems.len() != len as usize || !obj.extensible {
            return Ok(false);
        }
        if self.array_len(obj).0 != len || !self.no_indexed_protos(obj.proto)? {
            return Ok(false);
        }
        self.edit(o, |ob| {
            f(&mut ob.elems);
            ob.trim_elems();
        });
        Ok(true)
    }

    /// Give a new empty array its elements in one step (a host fast path for results that no script can see before
    /// they are done). `len` is the array length: more than `elems.len()` when the end is holes. Checks the byte cap
    /// before it stores. `Ok(false)` and no change when `o` is not an empty array with no other index property.
    pub fn fill_array(&mut self, o: ObjId, mut elems: Vec<Option<Value>>, len: u32) -> R<bool> {
        for v in elems.iter().flatten() {
            self.check_value(*v)?;
        }
        let obj = self.obj(o)?;
        if !obj.is_array || !obj.elems.is_empty() || obj.props.index_keys != 0 || !obj.extensible {
            return Ok(false);
        }
        while matches!(elems.last(), Some(None)) {
            elems.pop();
        }
        if elems.len() > len as usize {
            return Ok(false);
        }
        self.charge(elems.len() * ELEM_BYTES)?;
        self.edit(o, |ob| {
            ob.elems = elems;
            ob.set_length(Some(len), None);
        });
        Ok(true)
    }

    /// `SetIntegrityLevel` (`Object.seal`, and `Object.freeze` when `frozen`) in one pass over the own properties: the
    /// object stops being extensible, every property stops being configurable, and with `frozen` every data property stops
    /// being writable. Dense elements move to the sparse property table, which keeps their attributes. A byte view or a
    /// host-indexed object that has elements cannot be frozen: `Ok(false)` and no change. As in V8, sealing one only stops
    /// it from being extensible (the specification says a view's elements are configurable, so sealing it would fail).
    pub fn set_integrity(&mut self, o: ObjId, frozen: bool) -> R<bool> {
        let ob = self.obj(o)?;
        if ob.exotic == EXOTIC_PROXY {
            return Err(HeapError::Proxy(o));
        }
        if frozen && ob.exotic != 0 && ob.exotic != EXOTIC_STR && self.virtual_len(o)? > 0 {
            return Ok(false);
        }
        let moved = ob.elems.iter().flatten().count();
        self.charge(moved * (ENTRY_BYTES + INDEX_ENTRY_BYTES))?;
        self.edit(o, |ob| {
            ob.extensible = false;
            for (i, v) in std::mem::take(&mut ob.elems).into_iter().enumerate() {
                if let Some(value) = v {
                    ob.props.set(PropertyKey::Index(i as u32), Property::Data { value, writable: !frozen, enumerable: true, configurable: false });
                }
            }
            for (_, p) in ob.props.entries.iter_mut().flatten() {
                match p {
                    Property::Data { writable, configurable, .. } => {
                        *configurable = false;
                        *writable &= !frozen;
                    }
                    Property::Accessor { configurable, .. } => *configurable = false,
                }
            }
        });
        Ok(true)
    }

    /// `TestIntegrityLevel` (`Object.isSealed`, and `Object.isFrozen` when `frozen`), without a key list.
    pub fn test_integrity(&self, o: ObjId, frozen: bool) -> R<bool> {
        let ob = self.obj(o)?;
        if ob.exotic == EXOTIC_PROXY {
            return Err(HeapError::Proxy(o));
        }
        if ob.extensible || ob.elems.iter().any(Option::is_some) || (frozen && ob.exotic != 0 && ob.exotic != EXOTIC_STR && self.virtual_len(o)? > 0) {
            return Ok(false);
        }
        Ok(ob.props.iter().all(|(_, p)| !(p.configurable() || frozen && matches!(p, Property::Data { writable: true, .. }))))
    }

    // ----- collection -----

    /// Mark everything reachable from `roots`, free the rest, and return what was freed.
    /// All handles not reached become stale. Errors (stale root) leave the heap unchanged.
    pub fn collect(&mut self, roots: &[Value]) -> R<Collected> {
        for &r in roots {
            self.check_value(r)?;
        }
        self.collections += 1;
        let Heap { objects, strings, symbols, bigs, gray, length, exotics, .. } = self;
        for sl in [&mut strings.marks, &mut symbols.marks] {
            sl.clear();
        }
        strings.marks.resize(strings.live.len(), false);
        symbols.marks.resize(symbols.live.len(), false);
        bigs.marks.clear();
        bigs.marks.resize(bigs.live.len(), false);
        objects.marks.clear();
        objects.marks.resize(objects.live.len(), false);
        gray.clear();

        let mut m = Marker {
            smeta: &strings.meta, smarks: &mut strings.marks,
            ymeta: &symbols.meta, ymarks: &mut symbols.marks, yl: &symbols.live,
            ometa: &objects.meta, omarks: &mut objects.marks, gray,
            bmeta: &bigs.meta, bmarks: &mut bigs.marks,
        };
        m.str(*length);
        for &r in roots {
            m.value(r);
        }
        // Work list of dense positions: pop an object, mark everything it points to. No recursion.
        let ol = &objects.live;
        while let Some(d) = m.gray.pop() {
            let o = &ol[d as usize];
            if let Some(p) = o.proto {
                m.obj(p);
            }
            if o.exotic != 0 {
                match exotics.get(&objects.back[d as usize]) {
                    Some(binary::Exotic::View { buffer, .. }) => m.obj(*buffer),
                    Some(binary::Exotic::Prim(v)) => m.value(*v),
                    Some(binary::Exotic::Proxy(Some((target, handler)))) => {
                        m.obj(*target);
                        m.obj(*handler);
                    }
                    _ => {}
                }
            }
            for v in o.elems.iter().flatten() {
                m.value(*v);
            }
            for (k, p) in o.props.iter() {
                match k {
                    PropertyKey::String(s) => m.str(s),
                    PropertyKey::Symbol(s) => m.sym(s),
                    PropertyKey::Index(_) => {}
                }
                match p {
                    Property::Data { value, .. } => m.value(value),
                    Property::Accessor { get, set, .. } => {
                        if let Some(g) = get { m.obj(g); }
                        if let Some(s) = set { m.obj(s); }
                    }
                }
            }
        }

        let mut freed_bytes = 0;
        let c = Collected {
            objects: objects.sweep(|o| freed_bytes += obj_bytes(o)),
            strings: strings.sweep(|s| freed_bytes += str_bytes(s)),
            symbols: symbols.sweep(|_| freed_bytes += slot_bytes::<Symbol>()),
        };
        bigs.sweep(|b| freed_bytes += big::big_bytes(b));
        self.bytes -= freed_bytes;
        self.prune_exotics();
        // The interner is weak: rebuild it from the strings that survived.
        self.interner.rebuild(&self.strings);
        self.big_table.rebuild(&self.bigs);
        Ok(c)
    }
}

/// Mark bits by dense position, split from the slabs so the work loop can read objects while it
/// marks. Handles reached from live objects are valid, so the metadata lookups cannot be vacant.
struct Marker<'a> {
    smeta: &'a [Meta],
    smarks: &'a mut Vec<bool>,
    ymeta: &'a [Meta],
    ymarks: &'a mut Vec<bool>,
    yl: &'a [Symbol],
    ometa: &'a [Meta],
    omarks: &'a mut Vec<bool>,
    gray: &'a mut Vec<u32>,
    bmeta: &'a [Meta],
    bmarks: &'a mut Vec<bool>,
}

impl Marker<'_> {
    fn obj(&mut self, o: ObjId) {
        let d = self.ometa[o.index as usize].dense;
        if !std::mem::replace(&mut self.omarks[d as usize], true) {
            self.gray.push(d);
        }
    }
    fn str(&mut self, s: StrId) {
        self.smarks[self.smeta[s.index as usize].dense as usize] = true;
    }
    fn sym(&mut self, s: SymId) {
        let d = self.ymeta[s.index as usize].dense as usize;
        if !std::mem::replace(&mut self.ymarks[d], true)
            && let Some(desc) = self.yl[d].description {
            self.str(desc);
        }
    }
    fn value(&mut self, v: Value) {
        match v {
            Value::String(s) => self.str(s),
            Value::Symbol(s) => self.sym(s),
            Value::Object(o) => self.obj(o),
            Value::BigHeap(b) => self.big(b),
            _ => {}
        }
    }
    /// Out of line: a marked heap BigInt is rare, and a call-site that is bigger is not inlined into the marking loops.
    #[cold]
    #[inline(never)]
    fn big(&mut self, b: BigId) {
        self.bmarks[self.bmeta[b.index as usize].dense as usize] = true;
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
        if let PropertyKey::Index(i) = key
            && let Some(e @ Some(_)) = self.elems.get_mut(i as usize) {
            *e = None;
            self.trim_elems();
            return true;
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

    #[test]
    fn invalid_limits_do_not_panic() {
        for limits in [HeapLimits { max_bytes: 0, ..Default::default() }, HeapLimits { max_string_units: 0, ..Default::default() }, HeapLimits { max_strings: 0, ..Default::default() }] {
            let result = std::panic::catch_unwind(|| Heap::new(limits));
            assert!(result.is_ok());
            assert!(result.unwrap().is_err());
        }
    }

    #[test]
    fn keys_that_start_with_a_digit_are_told_from_index_keys() {
        let mut h = Heap::default();
        for (text, index) in [("0", Some(0)), ("7", Some(7)), ("4294967294", Some(u32::MAX - 1)), ("01", None), ("1a", None), ("4294967295", None), ("a1", None), ("", None), ("\u{0663}", None)] {
            let s = h.intern_str(text).unwrap();
            let want = index.map_or(PropertyKey::String(s), PropertyKey::Index);
            assert_eq!(h.key_from_string(s).unwrap(), want, "{text:?}");
        }
        // A string key that spells an index is refused, as before.
        let o = h.new_object(None).unwrap();
        let five = h.intern_str("5").unwrap();
        assert_eq!(h.get(o, PropertyKey::String(five)).err(), Some(HeapError::InvalidKey));
        let one_a = h.intern_str("1a").unwrap();
        assert!(matches!(h.get(o, PropertyKey::String(one_a)), Ok(Get::Value(Value::Undefined))));
    }

    #[test]
    fn maximum_u32_is_not_an_index_key() {
        let mut h = Heap::default();
        let a = h.new_array(None).unwrap();
        assert_eq!(h.set(a, PropertyKey::Index(u32::MAX), Value::Null), Err(HeapError::InvalidKey));
    }

    #[test]
    fn property_index_creation_respects_byte_cap() {
        let mut h = Heap::default();
        let keys: Vec<_> = (0..=INDEX_AT).map(|i| h.key_from_str(&format!("key{i}")).unwrap()).collect();
        let o = h.new_object(None).unwrap();
        for &k in &keys[..INDEX_AT] { h.set(o, k, Value::Null).unwrap(); }
        let before = h.stats().bytes;
        h.limits.max_bytes = before + ENTRY_BYTES + INDEX_ENTRY_BYTES;
        assert_eq!(h.set(o, keys[INDEX_AT], Value::Null), Err(HeapError::Limit(Limit::Bytes)));
        assert_eq!(h.stats().bytes, before);
        assert_eq!(h.own_keys(o).unwrap().len(), INDEX_AT);
    }

    #[test]
    fn stale_keys_cannot_be_converted_to_values() {
        let mut h = Heap::default();
        let s = h.intern_str("temporary").unwrap();
        let y = h.new_symbol(None).unwrap();
        h.collect(&[]).unwrap();
        for k in [PropertyKey::String(s), PropertyKey::Symbol(y)] {
            assert!(matches!(h.key_to_value(k), Err(HeapError::Stale)));
        }
    }

    fn n(v: f64) -> Value {
        Value::Number(v)
    }
    fn key(h: &mut Heap, s: &str) -> PropertyKey {
        h.key_from_str(s).unwrap()
    }
    fn val(h: &mut Heap, o: ObjId, k: PropertyKey) -> Value {
        match h.get(o, k).unwrap() {
            Get::Value(v) => v,
            Get::Accessor { .. } => panic!("accessor"),
        }
    }
    fn num(h: &mut Heap, o: ObjId, k: PropertyKey) -> f64 {
        match val(h, o, k) {
            Value::Number(x) => x,
            v => panic!("{v:?}"),
        }
    }
    fn setn(h: &mut Heap, o: ObjId, k: PropertyKey, x: f64) -> Set {
        h.set(o, k, n(x)).unwrap()
    }
    fn names(h: &mut Heap, o: ObjId) -> Vec<String> {
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
        assert_eq!(num(&mut h, o, a), 1.0);
        let p = h.get_own_property(o, a).unwrap().unwrap();
        assert!(matches!(p, Property::Data { writable: true, enumerable: true, configurable: true, .. }));
        let nan = f64::from_bits(0x7ff8_0000_0000_1234);
        setn(&mut h, o, a, nan);
        assert_eq!(num(&mut h, o, a).to_bits(), nan.to_bits());
        setn(&mut h, o, a, -0.0);
        assert!(num(&mut h, o, a).is_sign_negative());
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
        assert_eq!(num(&mut h, o, w), 9.0);
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
        assert!(matches!(val(&mut h, o, e), Value::Undefined));
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
        assert_eq!(names(&mut h, o), ["0", "1", "2", "10", "b", "a", "-1", "01", "@"]);
        // Updating keeps the position; delete and re-add moves to the end.
        let b = key(&mut h, "b");
        setn(&mut h, o, b, 1.0);
        assert_eq!(names(&mut h, o)[4], "b");
        h.delete(o, b).unwrap();
        setn(&mut h, o, b, 1.0);
        assert_eq!(names(&mut h, o), ["0", "1", "2", "10", "a", "-1", "01", "b", "@"]);
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
                assert_eq!(num(&mut h, o, k), i as f64);
            }
        }
        let want: Vec<String> = (0..200).filter(|i| i % 3 == 0).map(|i| format!("k{i}")).collect();
        assert_eq!(names(&mut h, o), want);
        for &k in &ks {
            h.delete(o, k).unwrap();
        }
        assert!(names(&mut h, o).is_empty());
        setn(&mut h, o, ks[5], 5.0);
        assert_eq!(names(&mut h, o), ["k5"]);
    }

    #[test]
    fn prototype_chain_and_cycles() {
        let mut h = Heap::default();
        let a = h.new_object(None).unwrap();
        let b = h.new_object(Some(a)).unwrap();
        let c = h.new_object(Some(b)).unwrap();
        let k = key(&mut h, "k");
        setn(&mut h, a, k, 7.0);
        assert_eq!(num(&mut h, c, k), 7.0);
        assert!(h.has_property(c, k).unwrap());
        assert!(!h.has_own_property(c, k).unwrap());
        assert_eq!(setn(&mut h, c, k, 8.0), Set::Done); // shadows, does not write the proto
        assert_eq!((num(&mut h, a, k), num(&mut h, c, k)), (7.0, 8.0));
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
        assert_eq!(num(&mut h, cur, k), 1.0);
        let c = h.collect(&[Value::Object(cur)]).unwrap();
        assert_eq!(c.objects, 0);
        assert!(!h.set_prototype_of(root, Some(cur)).unwrap());
    }

    #[test]
    fn arrays_length_holes_and_boundaries() {
        let mut h = Heap::default();
        let a = h.new_array(None).unwrap();
        let len = key(&mut h, "length");
        assert_eq!(num(&mut h, a, len), 0.0);
        assert_eq!(names(&mut h, a), ["length"]);
        assert!(matches!(h.get_own_property(a, len).unwrap(), Some(Property::Data { writable: true, enumerable: false, configurable: false, .. })));
        setn(&mut h, a, PropertyKey::Index(2), 9.0); // leaves holes 0 and 1
        assert_eq!(num(&mut h, a, len), 3.0);
        assert!(!h.has_own_property(a, PropertyKey::Index(0)).unwrap());
        assert!(matches!(val(&mut h, a, PropertyKey::Index(1)), Value::Undefined));
        assert_eq!(names(&mut h, a), ["2", "length"]);
        // Shrinking removes the elements; growing adds holes.
        assert!(h.define_own_property(a, len, &PropertyDescriptor { value: Some(n(2.0)), ..Default::default() }).unwrap());
        assert!(!h.has_own_property(a, PropertyKey::Index(2)).unwrap());
        assert_eq!(setn(&mut h, a, len, 5.0), Set::Done);
        assert_eq!(num(&mut h, a, len), 5.0);
        assert_eq!(names(&mut h, a), ["length"]);
        // Length given as a numeric string, and invalid lengths.
        let s = h.intern_str("1").unwrap();
        assert!(h.define_own_property(a, len, &PropertyDescriptor { value: Some(Value::String(s)), ..Default::default() }).unwrap());
        assert_eq!(num(&mut h, a, len), 1.0);
        for bad in [-1.0, 1.5, f64::NAN, 4294967296.0, f64::INFINITY] {
            let d = PropertyDescriptor { value: Some(n(bad)), ..Default::default() };
            assert_eq!(h.define_own_property(a, len, &d), Err(HeapError::InvalidArrayLength), "{bad}");
        }
        assert_eq!(num(&mut h, a, len), 1.0);
        // The largest index 2^32-2 gives length 2^32-1 and uses the sparse path.
        setn(&mut h, a, PropertyKey::Index(MAX_INDEX), 1.0);
        assert_eq!(num(&mut h, a, len), 4294967295.0);
        // 2^32-1 is a plain string key, not an element.
        let big = key(&mut h, "4294967295");
        setn(&mut h, a, big, 1.0);
        assert_eq!(num(&mut h, a, len), 4294967295.0);
        assert_eq!(names(&mut h, a), ["0", "4294967294", "length", "4294967295"].map(String::from).iter().filter(|s| *s != "0").cloned().collect::<Vec<_>>());
        // A huge length costs no memory: it is only a number.
        assert!(h.define_own_property(a, len, &PropertyDescriptor { value: Some(n(0.0)), ..Default::default() }).unwrap());
        assert_eq!(names(&mut h, a), ["length", "4294967295"]);
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
        assert_eq!(num(&mut h, a, len), 4.0);
        assert_eq!(names(&mut h, a), ["0", "1", "2", "3", "length"]);
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
        assert_eq!(names(&mut h, a), ["0", "5000", "length"]);
        assert_eq!(num(&mut h, a, len), 5001.0);
        // Deleting the last dense element trims the vector.
        assert!(h.delete(a, PropertyKey::Index(0)).unwrap());
        assert!(h.objects.get(a.index, a.gn).unwrap().elems.is_empty());
        assert!(h.delete(a, PropertyKey::Index(5000)).unwrap());
        assert_eq!(names(&mut h, a), ["length"]);
        assert_eq!(num(&mut h, a, len), 5001.0); // delete does not shrink the length
        // Ordinary objects take integer keys too, in numeric order.
        let o = h.new_object(None).unwrap();
        for i in [30u32, 4, 1_000_000, 4] {
            setn(&mut h, o, PropertyKey::Index(i), i as f64);
        }
        assert_eq!(names(&mut h, o), ["4", "30", "1000000"]);
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
        let mut h = Heap::new(HeapLimits { max_objects: 2, ..Default::default() }).unwrap();
        let a = h.new_object(None).unwrap();
        // Pretend this slot has been reused 2^32-1 times.
        h.objects.meta[a.index as usize].gn = u32::MAX;
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
        let mut h = Heap::new(HeapLimits { max_objects: 3, max_strings: 4, max_symbols: 2, max_string_units: 8, ..Default::default() }).unwrap();
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
        let mut h = Heap::new(HeapLimits { max_bytes: 20_000, ..Default::default() }).unwrap();
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
            h.objects.live.iter().map(obj_bytes).sum::<usize>()
                + h.strings.live.iter().map(str_bytes).sum::<usize>()
                + h.symbols.live.len() * slot_bytes::<Symbol>()
        };
        assert_eq!(h.stats().bytes, recount(&h));
        h.collect(&[Value::Object(o)]).unwrap();
        assert_eq!(h.stats().bytes, recount(&h));
        // Dense elements are charged too.
        let mut h = Heap::new(HeapLimits { max_bytes: 20_000, ..Default::default() }).unwrap();
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
        assert!(val(&mut h, ring[0], k).strict_equals(Value::Object(ring[1])));
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
        assert!(h.interner.count == h.strings.live.len());
        assert!(h.interner.table.len() <= 64); // the table shrank back
        assert_eq!(h.stats().collections, 20);
    }

    /// Longest and total probe distance (slots from the home slot) over all interned strings, and the load.
    fn probe_stats(h: &Heap) -> (usize, usize, f64) {
        let t = &h.interner.table;
        let mask = t.len() - 1;
        let (mut max, mut sum) = (0, 0);
        for (p, &e) in t.iter().enumerate().filter(|(_, e)| **e != 0) {
            let d = p.wrapping_sub(h.strings.live[e as usize - 1].hash as usize) & mask;
            max = max.max(d);
            sum += d;
        }
        (max, sum, h.interner.count as f64 / t.len() as f64)
    }

    /// Extra names must not make the interner slow or change the cost of a hot loop (resources.md, "Why an unrelated
    /// name moved a row"): the load stays at most 0.5 and probes stay short (measured: longest 9, mean 0.43).
    #[test]
    fn interner_probes_stay_short_for_realm_names_and_padding() {
        let mut it = crate::eval::Interp::realm(Heap::default(), Default::default()).unwrap();
        it.run_script("typeof Intl + typeof BigInt + typeof Proxy + typeof Reflect").unwrap();
        let n = it.heap.stats().live_strings;
        assert!(n > 300, "the realm names are interned: {n}");
        for pad in [0, 1, 3, 6, 12, 32, 1000] {
            for i in 0..pad {
                it.heap.intern_str(&format!("zzpad{i}")).unwrap();
            }
            let (max, sum, load) = probe_stats(&it.heap);
            let live = it.heap.stats().live_strings;
            assert!(load <= 0.5, "load {load} with {pad} extra names");
            assert!(max <= 12, "longest probe {max} with {pad} extra names");
            assert!(sum * 5 <= live * 3, "mean probe {} with {pad} extra names", sum as f64 / live as f64);
        }
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
        // Measured on x86-64: Value 16, Property 32, entry 48, Object 80, Meta 8.
        assert!(size_of::<Value>() <= 16);
        assert!(size_of::<Property>() <= 32);
        const { assert!(ENTRY_BYTES <= 48); }
        assert!(slot_bytes::<Object>() <= 88);
        assert_eq!(size_of::<Meta>(), 8);
        assert!(slot_bytes::<Str>() <= 32);
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

    // ----- dense payload remapping -----

    /// The slot and dense mappings agree, the free list is exact, and the interner matches.
    fn check_slabs(h: &Heap) {
        fn one<T>(s: &Slab<T>) {
            assert_eq!(s.live.len(), s.back.len());
            for (d, &slot) in s.back.iter().enumerate() {
                assert_eq!(s.meta[slot as usize].dense as usize, d);
            }
            assert_eq!(s.meta.iter().filter(|m| m.dense != VACANT).count(), s.live.len());
            let mut free = s.free.clone();
            free.sort_unstable();
            free.dedup();
            assert_eq!(free.len(), s.free.len(), "slot on the free list twice");
            assert!(s.free.iter().all(|&i| s.meta[i as usize].dense == VACANT));
            assert_eq!(s.free.len() + s.retired + s.live.len(), s.meta.len());
        }
        one(&h.objects);
        one(&h.strings);
        one(&h.symbols);
        assert_eq!(h.interner.count, h.strings.live.len());
        for sl in &h.strings.live {
            let i = h.interner.find(&h.strings, sl.hash, &sl.units).unwrap();
            assert!(std::ptr::eq(&h.strings.live[i as usize], sl));
        }
    }

    /// Unlinked objects; object i holds `n = i`.
    fn numbered(h: &mut Heap, count: usize) -> Vec<ObjId> {
        let kn = key(h, "n");
        (0..count)
            .map(|i| {
                let o = h.new_object(None).unwrap();
                setn(h, o, kn, i as f64);
                o
            })
            .collect()
    }

    #[test]
    fn removing_a_middle_object_remaps_survivors() {
        let mut h = Heap::default();
        let kn = key(&mut h, "n");
        let o = numbered(&mut h, 5);
        let roots = [o[0], o[2], o[4]].map(Value::Object);
        let c = h.collect(&roots).unwrap();
        assert_eq!(c.objects, 2);
        check_slabs(&h);
        for i in [0, 2, 4] {
            assert_eq!(num(&mut h, o[i], kn), i as f64);
        }
        for i in [1, 3] {
            assert_eq!(h.get_prototype_of(o[i]), Err(HeapError::Stale));
        }
        // Freed slots are reused; old handles to them stay stale and survivors stay intact.
        let a = h.new_object(None).unwrap();
        let b = h.new_object(None).unwrap();
        assert!([o[1].index, o[3].index].contains(&a.index) && [o[1].index, o[3].index].contains(&b.index));
        assert!(h.is_extensible(o[1]).is_err() && h.is_extensible(o[3]).is_err());
        setn(&mut h, a, kn, 100.0);
        setn(&mut h, b, kn, 200.0);
        assert_eq!((num(&mut h, a, kn), num(&mut h, b, kn)), (100.0, 200.0));
        for i in [0, 2, 4] {
            assert_eq!(num(&mut h, o[i], kn), i as f64);
        }
        check_slabs(&h);
    }

    #[test]
    fn repeated_collections_keep_mappings_for_every_live_fraction() {
        let mut h = Heap::default();
        let kn = key(&mut h, "n");
        let length = Value::String(h.length);
        let sn = match kn {
            PropertyKey::String(a) => Value::String(a),
            _ => unreachable!(),
        };
        // (live percent, step): which objects survive each round, over fresh and reused slots.
        let mut live: Vec<(ObjId, f64)> = Vec::new();
        let mut next = 0.0;
        for (round, pct) in [0usize, 100, 10, 50, 90, 1, 100, 0, 33, 67].into_iter().enumerate() {
            for _ in 0..3000 {
                let o = h.new_object(None).unwrap();
                setn(&mut h, o, kn, next);
                live.push((o, next));
                next += 1.0;
            }
            // Keep a pseudo-random pct of all objects, scattered over the dense array.
            let mut x = round as u64 * 7919 + 1;
            let mut keep = Vec::new();
            for &(o, v) in &live {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                if (x >> 33) % 100 < pct as u64 {
                    keep.push((o, v));
                }
            }
            let mut roots: Vec<Value> = keep.iter().map(|&(o, _)| Value::Object(o)).collect();
            roots.extend([length, sn]);
            let before = h.stats();
            let c = h.collect(&roots).unwrap();
            assert_eq!(c.objects, live.len() - keep.len());
            assert_eq!(h.stats().live_objects, keep.len());
            assert_eq!(h.stats().object_slots, before.object_slots);
            check_slabs(&h);
            for &(o, v) in &keep {
                assert_eq!(num(&mut h, o, kn), v);
            }
            for &(o, _) in live.iter().filter(|e| !keep.iter().any(|k| k.0 == e.0)).take(50) {
                assert_eq!(h.is_extensible(o), Err(HeapError::Stale));
            }
            live = keep;
        }
    }

    /// `stored_edges` is what the evaluator's collect pass follows for an object: dense elements, sparse elements, data values and both accessor
    /// functions. Primitive values are listed too (the caller skips them). A missing getter or setter adds nothing.
    #[test]
    fn stored_edges_lists_every_value_a_property_holds() {
        let mut h = Heap::default();
        let o = h.new_object(None).unwrap();
        let (getter, setter, elem, sparse, data) = (h.new_object(None).unwrap(), h.new_object(None).unwrap(), h.new_object(None).unwrap(), h.new_object(None).unwrap(), h.new_object(None).unwrap());
        let (acc, only_get, dk) = (key(&mut h, "acc"), key(&mut h, "only_get"), key(&mut h, "data"));
        h.set(o, dk, Value::Object(data)).unwrap();
        h.define_own_property(o, acc, &PropertyDescriptor { get: Some(Some(getter)), set: Some(Some(setter)), ..Default::default() }).unwrap();
        h.define_own_property(o, only_get, &PropertyDescriptor { get: Some(Some(getter)), set: Some(None), ..Default::default() }).unwrap();
        h.set(o, PropertyKey::Index(0), Value::Object(elem)).unwrap();
        h.define_own_property(o, PropertyKey::Index(5000), &PropertyDescriptor { value: Some(Value::Object(sparse)), ..Default::default() }).unwrap();
        let mut out = Vec::new();
        h.stored_edges(o, &mut out).unwrap();
        let mut got: Vec<ObjId> = out.iter().filter_map(|v| if let Value::Object(x) = v { Some(*x) } else { None }).collect();
        got.sort_by_key(|x| x.index);
        let mut want = vec![getter, setter, elem, sparse, data, getter];
        want.sort_by_key(|x| x.index);
        assert_eq!(got, want);
        assert_eq!(out.len(), 6, "no other value is listed");
        let stale = h.new_object(None).unwrap();
        h.collect(&[]).unwrap();
        assert_eq!(h.stored_edges(stale, &mut out), Err(HeapError::Stale));
    }

    #[test]
    fn collection_keeps_every_edge_kind_after_positions_shift() {
        let mut h = Heap::default();
        // Dead objects first, so every survivor moves down in the dense array.
        for _ in 0..10 {
            h.new_object(None).unwrap();
        }
        let holder = h.new_object(None).unwrap();
        for _ in 0..7 {
            h.new_object(None).unwrap();
        }
        let proto = h.new_object(None).unwrap();
        h.set_prototype_of(holder, Some(proto)).unwrap();
        let key_str = h.intern_str("keystr").unwrap();
        let val_str = h.intern_str("valstr").unwrap();
        let desc_str = h.intern_str("descstr").unwrap();
        let sym = h.new_symbol(Some(desc_str)).unwrap();
        let (getter, setter, elem, sparse_elem) = (h.new_object(None).unwrap(), h.new_object(None).unwrap(), h.new_object(None).unwrap(), h.new_object(None).unwrap());
        h.set(holder, PropertyKey::String(key_str), Value::String(val_str)).unwrap();
        h.set(holder, PropertyKey::Symbol(sym), n(1.0)).unwrap();
        let acc = key(&mut h, "acc");
        h.define_own_property(holder, acc, &PropertyDescriptor { get: Some(Some(getter)), set: Some(Some(setter)), ..Default::default() }).unwrap();
        let arr = h.new_array(None).unwrap();
        h.set(arr, PropertyKey::Index(0), Value::Object(elem)).unwrap();
        h.define_own_property(arr, PropertyKey::Index(2), &PropertyDescriptor { value: Some(Value::Object(sparse_elem)), ..Default::default() }).unwrap();
        let ak = key(&mut h, "arr");
        h.set(holder, ak, Value::Object(arr)).unwrap();
        let before = h.stats().bytes;
        for round in 0..3 {
            let c = h.collect(&[Value::Object(holder)]).unwrap();
            assert_eq!(c.objects, if round == 0 { 17 } else { 0 });
            check_slabs(&h);
            for id in [proto, getter, setter, elem, sparse_elem, arr] {
                assert!(h.is_extensible(id).is_ok());
            }
            assert_eq!(h.get_prototype_of(holder).unwrap(), Some(proto));
            assert!(matches!(val(&mut h, holder, PropertyKey::String(key_str)), Value::String(x) if x == val_str));
            assert_eq!(num(&mut h, holder, PropertyKey::Symbol(sym)), 1.0);
            assert!(matches!(h.get_own_property(holder, acc).unwrap(), Some(Property::Accessor { get: Some(g), set: Some(t), .. }) if g == getter && t == setter));
            assert!(matches!(val(&mut h, arr, PropertyKey::Index(0)), Value::Object(x) if x == elem));
            assert!(matches!(val(&mut h, arr, PropertyKey::Index(2)), Value::Object(x) if x == sparse_elem));
            assert_eq!(h.symbol_description(sym).unwrap(), Some(desc_str));
            assert_eq!(h.to_string_lossy(desc_str).unwrap(), "descstr");
            assert_eq!(h.intern_str("valstr").unwrap(), val_str); // the interner follows moved strings
        }
        assert!(h.stats().bytes < before);
    }

    #[test]
    fn retired_slot_in_the_middle_does_not_break_remapping() {
        let mut h = Heap::new(HeapLimits { max_objects: 8, ..Default::default() }).unwrap();
        let kn = key(&mut h, "n");
        let o = numbered(&mut h, 6);
        h.objects.meta[o[2].index as usize].gn = u32::MAX; // next free retires it
        let stale = ObjId { index: o[2].index, gn: u32::MAX };
        assert!(h.is_extensible(stale).is_ok());
        let roots = [o[0], o[1], o[3], o[5]].map(Value::Object);
        assert_eq!(h.collect(&roots).unwrap().objects, 2);
        check_slabs(&h);
        assert_eq!(h.stats().retired_slots, 1);
        assert_eq!(h.is_extensible(stale), Err(HeapError::Stale));
        for i in [0, 1, 3, 5] {
            assert_eq!(num(&mut h, o[i], kn), i as f64);
        }
        // 8 slots: 6 created (1 retired, 1 free) and 2 never created.
        let a = h.new_object(None).unwrap();
        assert_eq!(a.index, o[4].index);
        for _ in 0..2 {
            assert_ne!(h.new_object(None).unwrap().index, o[2].index);
        }
        assert_eq!(h.new_object(None).err(), Some(HeapError::Limit(Limit::Objects)));
        check_slabs(&h);
        assert_eq!(h.is_extensible(stale), Err(HeapError::Stale));
    }

    #[test]
    fn dead_burst_releases_payload_but_keeps_slots_and_generations() {
        let mut h = Heap::default();
        let o = numbered(&mut h, 60_000);
        let slots = h.stats().object_slots;
        assert!(h.stats().object_payload_capacity >= 60_000);
        // Keep every 100th: a small live fraction scattered through the array.
        let keep: Vec<Value> = o.iter().step_by(100).map(|&x| Value::Object(x)).collect();
        let kn = key(&mut h, "n");
        let mut roots = keep.clone();
        roots.push(Value::String(h.length));
        roots.push(match kn { PropertyKey::String(s) => Value::String(s), _ => unreachable!() });
        let c = h.collect(&roots).unwrap();
        assert_eq!(c.objects, 60_000 - 600);
        // The sweep cannot tell this burst from steady churn, so it keeps the room for the 60k
        // allocations it just saw. The next sweep, with no new allocation, releases it.
        assert!(h.stats().object_payload_capacity >= 60_000);
        assert_eq!(h.collect(&roots).unwrap().objects, 0);
        let st = h.stats();
        assert!(st.object_payload_capacity <= 4 * 600, "payload capacity {}", st.object_payload_capacity);
        assert_eq!(st.object_slots, slots); // metadata is kept
        assert!(st.object_metadata_capacity >= slots);
        check_slabs(&h);
        for (i, &x) in o.iter().enumerate().step_by(100) {
            assert_eq!(num(&mut h, x, kn), i as f64);
        }
        // Every dead handle is stale, even after its slot is reused.
        for _ in 0..1000 {
            h.new_object(None).unwrap();
        }
        assert!(o.iter().enumerate().filter(|(i, _)| i % 100 != 0).all(|(_, &x)| h.is_extensible(x).is_err()));
        // All slots reused: none were lost.
        assert_eq!(h.stats().object_slots, slots);
        check_slabs(&h);
    }

    #[test]
    fn steady_churn_does_not_shrink_and_regrow() {
        let mut h = Heap::default();
        let mut caps = Vec::new();
        for _ in 0..6 {
            for _ in 0..30_000 {
                h.new_object(None).unwrap();
            }
            h.collect(&[]).unwrap();
            caps.push(h.stats().object_payload_capacity);
            check_slabs(&h);
        }
        // The first round may trim; after that the capacity stays put while the churn continues.
        assert!(caps[2..].iter().all(|&c| c == caps[2]), "{caps:?}");
        assert!(caps[2] >= 30_000);
        // The churn stops: the next sweep releases the capacity.
        h.collect(&[]).unwrap();
        assert!(h.stats().object_payload_capacity <= 4);
    }
}
