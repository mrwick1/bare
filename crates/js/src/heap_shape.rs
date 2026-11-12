//! Shapes (hidden classes) and inline caches for property access. A submodule of `heap.rs`.
//!
//! Shapes (step A1). An object with named properties keeps their keys and attributes in a *shape* and their values in `slots`. In
//! JS terms: two objects made by `{a: 1, b: 2}` share one description of "a then b", and each holds only the two values. A shape is a
//! node of a transition tree: adding the key `k` with the attributes `a` to shape `S` gives the child of `S` for `(k, a)`, made
//! once. The keys of a chain sit in one shared `Table` (a shape uses the first `count` entries), so a chain of 100 shapes
//! costs one table, not 100. A table with more than 8 entries has a hash index. Shape 0 means "dictionary mode": the object owns a
//! `PropMap` (the old layout) in `dict` and has no slots. An object goes to dictionary mode when
//! - a property other than the last one added is deleted,
//! - an attribute of a property changes (also `freeze`, `seal`, and data to accessor),
//! - `setPrototypeOf` changes its prototype,
//! - it would get more than `MAX_PROPS` (128) properties, or an index key that does not live in `elems`,
//!
//! An exotic object (proxy, host named or indexed, byte buffer or view, String wrapper) is not cacheable either. It starts from its own root
//! shape, `ROOT_EXOTIC`, which no ordinary object has, so a cache filled for an ordinary shape never hits it, and the fill refuses it. This
//! is the plan's "roots differ by object kind". It costs no `PropMap` (48 bytes) for each wrapper, which a dictionary would.
//! The object goes to dictionary mode for the reasons above like any other.
//! - the shape table is full (`SHAPE_CAP`): no new shape is made.
//! The chain of an object is as long as its property count, so the "longer than 1024" rule of the plan can never fire and is not coded.
//! A dead key in a shape never matches a live key: a freed handle gets a new generation.
//!
//! Shape sweep (step A6). A collect frees the shapes that no live object uses, when the shape count has doubled since the last sweep
//! (and is at least `SWEEP_MIN`). The mark sets the shape of each live object and its parents. The sweep keeps the live shapes in
//! order with new ids, drops the dead transitions and the table entries past the longest live user, and rewrites the shape of each
//! live object. Every `Ic` and every literal site shape is then stale: the heap resets its stubs, and the evaluator resets its
//! entries when `Heap::shape_sweeps` changes.
//!
//! Inline caches (step A2). A site (`o.x` in the tree walker, a property instruction in bytecode) keeps an `Ic`: "last time, the receiver had
//! this shape, and the property was in this slot of the receiver, or of this prototype, or nowhere". `Heap::ic_own` answers an own data hit
//! inline: one handle check, one compare, one load. `Heap::get_ic` answers the other kinds out of line, and on a miss it walks the chain
//! as `lookup` does and fills the entry.
//!
//! A prototype hit needs more than the shape of the receiver: two objects with one shape can have different prototypes, and the
//! prototypes can change. So the entry keeps the receiver's prototype at fill time (`first`) and `Heap::proto_epoch`. Each prototype
//! that a fill walks over gets the flag `ic_chain`. A change of the keys, the attributes or the prototype of a flagged object bumps the
//! epoch, and every prototype and absent entry stops hitting. A change of a value needs nothing: the hit reads the value from the holder.
//! A fill refuses a chain with an exotic object, a dictionary holder, a lazy `prototype` key or an `on*` name of the host: those keep
//! the plain walk. The contract is in `docs/decisions.md` ("Shapes and inline caches") and the plan is in `docs/engine-plan.md`.
//!
//! An `Ic` is plain `Copy` data with no pointer. A handle in it that has died fails the generation check of every handle read. A shape
//! id in an entry never names another shape: a shape sweep gives new ids, and every entry is reset after it.
use super::*;
use std::cell::{Cell, OnceCell};

// ---------- shapes ----------

/// Attribute bits of a shape entry. `A_ACC`: an accessor, which uses two slots (get, then set).
pub(super) const A_W: u8 = 1;
pub(super) const A_E: u8 = 2;
pub(super) const A_C: u8 = 4;
pub(super) const A_ACC: u8 = 8;

/// Most properties of an object in shape mode. The 129th goes to dictionary mode.
pub(super) const MAX_PROPS: usize = 128;
/// Most shapes. Past it no new shape is made and the object that needs one goes to dictionary mode.
pub(super) const SHAPE_CAP: usize = 65_536;
/// The shape of an empty ordinary object, of an empty array (its only entry is `length`, slot 0) and of an empty exotic object.
pub(super) const ROOT: u32 = 1;
pub(super) const ROOT_ARRAY: u32 = 2;
pub(super) const ROOT_EXOTIC: u32 = 3;
/// The root of an exotic object whose own part answers only numeric keys (a buffer, a view, a text codec, a host list, a Number,
/// Boolean or Symbol wrapper). The cache fills for its other keys.
pub(super) const ROOT_PLAIN: u32 = 4;

/// A table up to this long is searched by a scan; a longer one has a hash index.
const SCAN_MAX: usize = 8;

/// One own property of a shape: its key, the first slot of its value, and its attribute bits.
#[derive(Clone, Copy)]
pub(super) struct Entry {
    pub(super) key: PropertyKey,
    pub(super) slot: u32,
    pub(super) attrs: u8,
}

/// The keys of a chain of shapes. A shape with `count` properties uses `entries[..count]`. Another branch may have added more entries
/// after them. The table is copied when a second branch starts.
struct Table {
    entries: Vec<Entry>,
    /// Key to position, made at the first probe of a table of more than `SCAN_MAX` entries: most tables (the builtins at start) are never probed.
    index: OnceCell<Box<KeyMap>>,
}

impl Table {
    /// The entry of `key` among the first `n` of a table with a hash index.
    #[inline(never)]
    fn probe(&self, key: PropertyKey, n: usize) -> Option<&Entry> {
        let index = self.index.get_or_init(|| Box::new(self.entries.iter().enumerate().map(|(i, e)| (e.key, i as u32)).collect()));
        let p = *index.get(&key)? as usize;
        if p < n { Some(&self.entries[p]) } else { None }
    }

    fn push(&mut self, e: Entry) {
        self.entries.push(e);
        if let Some(m) = self.index.get_mut() {
            m.insert(e.key, self.entries.len() as u32 - 1);
        }
    }
}

type Children = HashMap<(PropertyKey, u8), u32, BuildHasherDefault<KeyHasher>>;

/// A node of the transition tree.
pub(super) struct Shape {
    /// The shape before the last key was added. 0 for a root.
    parent: u32,
    count: u16,
    /// Slots in use: the slot count of an object with this shape.
    nslots: u16,
    table: u32,
    /// The first child, tested before the map: a shape that is followed by one key (the common case) never touches the map.
    child: (PropertyKey, u8, u32),
    children: Option<Box<Children>>,
}

/// All shapes and key tables of a heap. `shapes[0]` is a sentinel: shape 0 is dictionary mode.
pub(super) struct Shapes {
    shapes: Vec<Shape>,
    tables: Vec<Table>,
    /// The keys and the shape of `Object::init_fn`.
    fn_shape: Option<((PropertyKey, PropertyKey), u32)>,
    /// `fn_shape` with a lazy `prototype` added last (`Heap::new_function`): the shape of most closures.
    fn_lazy: Option<((PropertyKey, PropertyKey, PropertyKey), u32)>,
    /// Recent transitions that the `child` entry missed, direct-mapped by `from` and `key`: `(from, key, attrs, to)`. A root has hundreds
    /// of children, and every object literal starts there: without this, most first keys take a probe of the children map.
    recent: Box<[(u32, PropertyKey, u8, u32); RECENT]>,
    /// Recent answers of `Object::shape_get`, direct-mapped as `recent`: `(shape, key, slot, attrs)`, with `slot == u32::MAX` when the
    /// shape has no such key. A shape never changes, so an answer stays true. Native code reads with no inline cache (option bags,
    /// iterator results, a reviver's holders), and most of its reads ask the same few shapes and keys again.
    found: Box<[Cell<(u32, PropertyKey, u32, u8)>; RECENT]>,
}

const RECENT: usize = 256;
/// Entries of `Heap::stubs`. `recent_at` maps to them.
pub(super) const STUBS: usize = RECENT;

/// The slot of `(from, key)` in `Shapes::recent`.
#[inline(always)]
fn recent_at(from: u32, key: PropertyKey) -> usize {
    let k = match key {
        PropertyKey::Index(i) => i,
        PropertyKey::String(s) => s.index,
        PropertyKey::Symbol(s) => !s.index,
    };
    ((k ^ from.rotate_left(16)).wrapping_mul(0x9e37_79b9) >> 24) as usize
}

impl Shapes {
    pub(super) fn new(length: PropertyKey) -> Shapes {
        let none = (PropertyKey::Index(0), 0, 0);
        let shape = |count, table| Shape { parent: 0, count, nslots: count, table, child: none, children: None };
        let tables = vec![
            Table { entries: Vec::new(), index: OnceCell::new() },
            Table { entries: vec![Entry { key: length, slot: 0, attrs: A_W }], index: OnceCell::new() },
        ];
        let recent = Box::new([(0, PropertyKey::Index(0), 0, 0); RECENT]);
        // Shape 0 is dictionary mode, which never asks `found`, so the first state never matches.
        let found = Box::new(std::array::from_fn(|_| Cell::new((0, PropertyKey::Index(0), 0, 0))));
        Shapes { shapes: vec![shape(0, 0), shape(0, 0), shape(1, 1), shape(0, 0), shape(0, 0)], tables, fn_shape: None, fn_lazy: None, recent, found }
    }

    /// How many shapes exist (the sentinel and the three roots included).
    pub(super) fn len(&self) -> usize {
        self.shapes.len()
    }

    /// The bytes of the shapes and their tables, by capacity.
    pub(super) fn bytes(&self) -> usize {
        use std::mem::size_of;
        self.shapes.capacity() * size_of::<Shape>()
            + self.shapes.iter().filter_map(|s| s.children.as_ref()).map(|m| m.capacity() * (size_of::<((PropertyKey, u8), u32)>() + 1)).sum::<usize>()
            + self.tables.capacity() * size_of::<Table>()
            + self.tables.iter().map(|t| t.entries.capacity() * size_of::<Entry>() + t.index.get().map_or(0, |m| m.capacity() * (size_of::<(PropertyKey, u32)>() + 1))).sum::<usize>()
    }

    /// The properties of `shape` in order.
    #[inline]
    pub(super) fn entries(&self, shape: u32) -> &[Entry] {
        let s = &self.shapes[shape as usize];
        &self.tables[s.table as usize].entries[..s.count as usize]
    }

    /// The position of `key` among the properties of `shape`.
    pub(super) fn pos(&self, shape: u32, key: PropertyKey) -> Option<usize> {
        let s = &self.shapes[shape as usize];
        let t = &self.tables[s.table as usize];
        let n = s.count as usize;
        t.entries[..n].iter().position(|e| e.key == key)
    }

    /// The entry of `key` in `shape`. One walk: the scan of a short table is inline, a hash probe is a call.
    #[inline(always)]
    pub(super) fn find(&self, shape: u32, key: PropertyKey) -> Option<&Entry> {
        let s = &self.shapes[shape as usize];
        let t = &self.tables[s.table as usize];
        let n = s.count as usize;
        if n <= SCAN_MAX {
            return t.entries[..n].iter().find(|e| e.key == key);
        }
        t.probe(key, n)
    }

    /// `to` is `from` with `key` added last with `attrs`, in the next slot.
    pub(super) fn added(&self, from: u32, to: u32, key: PropertyKey, attrs: u8) -> bool {
        let (f, t) = (&self.shapes[from as usize], &self.shapes[to as usize]);
        t.parent == from
            && t.nslots == f.nslots + 1
            && self.entries(to).last().is_some_and(|e| e.key == key && e.attrs == attrs && e.slot == f.nslots as u32)
    }

    /// The shape that `from` has after `key` is added with `attrs`. `None`: the object goes to dictionary mode (it would have more than
    /// `MAX_PROPS` properties, or the shape table is full).
    #[inline(always)]
    pub(super) fn add(&mut self, from: u32, key: PropertyKey, attrs: u8) -> Option<u32> {
        let s = &self.shapes[from as usize];
        if s.child.2 != 0 && s.child.0 == key && s.child.1 == attrs {
            return Some(s.child.2);
        }
        self.add_miss(from, key, attrs)
    }

    /// `add` when the first child is another key: the recent transitions, then the children map or a new shape.
    #[inline(never)]
    fn add_miss(&mut self, from: u32, key: PropertyKey, attrs: u8) -> Option<u32> {
        let r = &self.recent[recent_at(from, key)];
        if r.0 == from && r.1 == key && r.2 == attrs && r.3 != 0 {
            return Some(r.3);
        }
        let to = self.add_slow(from, key, attrs)?;
        self.recent[recent_at(from, key)] = (from, key, attrs, to);
        Some(to)
    }

    #[inline(never)]
    fn add_slow(&mut self, from: u32, key: PropertyKey, attrs: u8) -> Option<u32> {
        let s = &self.shapes[from as usize];
        if let Some(&id) = s.children.as_ref().and_then(|m| m.get(&(key, attrs))) {
            return Some(id);
        }
        if s.count as usize >= MAX_PROPS || self.shapes.len() >= SHAPE_CAP {
            return None;
        }
        let (count, nslots) = (s.count, s.nslots);
        let entry = Entry { key, slot: nslots as u32, attrs };
        // The tip of its table adds to it. A shape that another branch has extended copies its part first.
        let table = if self.tables[s.table as usize].entries.len() == count as usize {
            s.table
        } else {
            let t = &self.tables[s.table as usize];
            let mut copy = Table { entries: Vec::with_capacity(count as usize + 1), index: OnceCell::new() };
            t.entries[..count as usize].iter().for_each(|&e| copy.push(e));
            self.tables.push(copy);
            self.tables.len() as u32 - 1
        };
        self.tables[table as usize].push(entry);
        let id = self.shapes.len() as u32;
        self.shapes.push(Shape { parent: from, count: count + 1, nslots: nslots + 1 + (attrs & A_ACC != 0) as u16, table, child: (PropertyKey::Index(0), 0, 0), children: None });
        let s = &mut self.shapes[from as usize];
        if s.child.2 == 0 {
            s.child = (key, attrs, id);
        } else {
            s.children.get_or_insert_with(Default::default).insert((key, attrs), id);
        }
        Some(id)
    }

    /// `add` of `key` with `attrs` to `from` finds a shape that exists.
    pub(super) fn has_child(&self, from: u32, key: PropertyKey, attrs: u8) -> bool {
        let s = &self.shapes[from as usize];
        s.child.2 != 0 && s.child.0 == key && s.child.1 == attrs || s.children.as_ref().is_some_and(|m| m.contains_key(&(key, attrs)))
    }

    /// The shape of a function with the read-only, configurable `length` and `name` (`keys`), and with a writable `prototype` (`proto`:
    /// not enumerable, not configurable) when given. Both are kept: a realm makes thousands of functions with these keys. `None`: the
    /// shape table is full.
    #[inline(always)]
    pub(super) fn fn_shape(&mut self, keys: (PropertyKey, PropertyKey), proto: Option<PropertyKey>) -> Option<u32> {
        let base = match self.fn_shape {
            Some((k, s)) if k == keys => s,
            _ => {
                let s = self.add(ROOT, keys.0, A_C).and_then(|s| self.add(s, keys.1, A_C))?;
                self.fn_shape = Some((keys, s));
                s
            }
        };
        let Some(kp) = proto else { return Some(base) };
        match self.fn_lazy {
            Some((k, s)) if k == (keys.0, keys.1, kp) => Some(s),
            _ => {
                let s = self.add(base, kp, A_W)?;
                self.fn_lazy = Some(((keys.0, keys.1, kp), s));
                Some(s)
            }
        }
    }

    /// `shape` holds `n` writable, enumerable, configurable data properties in slots `0..n`, from the root of ordinary objects: the shape
    /// that an object literal with `n` named keys gives.
    pub(super) fn is_literal(&self, shape: u32, n: usize) -> bool {
        let s = &self.shapes[shape as usize];
        shape != 0 && s.count as usize == n && s.nslots as usize == n && self.entries(shape).iter().all(|e| e.attrs == A_W | A_E | A_C) && {
            let mut r = shape;
            while self.shapes[r as usize].parent != 0 {
                r = self.shapes[r as usize].parent;
            }
            r == ROOT
        }
    }

    /// Mark the parents of `shape` in `live` (step A6). A parent's keys are a prefix of its child's, so the caller marked them already.
    #[inline(never)]
    pub(super) fn mark_parents(&self, shape: u32, live: &mut [bool]) {
        let mut p = self.shapes[shape as usize].parent;
        while p != 0 && !std::mem::replace(&mut live[p as usize], true) {
            p = self.shapes[p as usize].parent;
        }
    }

    /// Free the shapes that `live` does not mark, and the tables and transitions that only they used (step A6). The other shapes keep
    /// their order and get new ids: the result maps an old id to the new one (`u32::MAX` for a freed shape). The roots, the dictionary
    /// sentinel and the preset function shapes stay. The recent transitions and answers are cleared. Every cache that holds a shape id
    /// must be reset after this.
    pub(super) fn sweep(&mut self, live: &mut [bool]) -> Vec<u32> {
        live[..=ROOT_PLAIN as usize].fill(true);
        for s in [self.fn_shape.map(|f| f.1), self.fn_lazy.map(|f| f.1)].into_iter().flatten() {
            live[s as usize] = true;
            self.mark_parents(s, live);
        }
        let mut remap = vec![u32::MAX; self.shapes.len()];
        // For each table: 1 + the most entries that a live shape uses, or 0 when no live shape uses it.
        let mut used = vec![0u32; self.tables.len()];
        let mut next = 0;
        for (i, s) in self.shapes.iter().enumerate() {
            if live[i] {
                remap[i] = next;
                next += 1;
                let u = &mut used[s.table as usize];
                *u = (*u).max(s.count as u32 + 1);
            }
        }
        let mut tmap = vec![u32::MAX; self.tables.len()];
        let mut t = 0;
        let mut i = 0;
        self.tables.retain_mut(|tb| {
            let n = used[i];
            i += 1;
            if n == 0 {
                return false;
            }
            if tb.entries.len() > n as usize - 1 {
                // The positions of the index past the cut would name the keys that a branch adds later.
                tb.entries.truncate(n as usize - 1);
                tb.entries.shrink_to_fit();
                tb.index = OnceCell::new();
            }
            tmap[i - 1] = t;
            t += 1;
            true
        });
        let mut i = 0;
        self.shapes.retain_mut(|s| {
            i += 1;
            if !live[i - 1] {
                return false;
            }
            s.parent = remap[s.parent as usize];
            s.table = tmap[s.table as usize];
            if s.child.2 != 0 {
                let c = remap[s.child.2 as usize];
                s.child = if c == u32::MAX { (PropertyKey::Index(0), 0, 0) } else { (s.child.0, s.child.1, c) };
            }
            if let Some(m) = &mut s.children {
                m.retain(|_, c| remap[*c as usize] != u32::MAX);
                m.values_mut().for_each(|c| *c = remap[*c as usize]);
                if m.is_empty() {
                    s.children = None;
                }
            }
            true
        });
        self.shapes.shrink_to(2 * self.shapes.len());
        self.tables.shrink_to(2 * self.tables.len());
        self.fn_shape = self.fn_shape.map(|(k, s)| (k, remap[s as usize]));
        self.fn_lazy = self.fn_lazy.map(|(k, s)| (k, remap[s as usize]));
        self.recent.fill((0, PropertyKey::Index(0), 0, 0));
        self.found.iter().for_each(|c| c.set((0, PropertyKey::Index(0), 0, 0)));
        remap
    }

    pub(super) fn count(&self, shape: u32) -> usize {
        self.shapes[shape as usize].count as usize
    }

    pub(super) fn parent(&self, shape: u32) -> u32 {
        self.shapes[shape as usize].parent
    }
}

/// What a read of the property `p` gives.
fn get_of(p: Property) -> Get {
    match p {
        Property::Data { value, .. } => Get::Value(value),
        Property::Accessor { get, .. } => Get::Accessor { getter: get },
    }
}

fn obj_of(v: Value) -> Option<ObjId> {
    if let Value::Object(o) = v { Some(o) } else { None }
}

/// The attribute bits of a property.
pub(super) fn attrs_of(p: &Property) -> u8 {
    match *p {
        Property::Data { writable, enumerable, configurable, .. } => writable as u8 * A_W | enumerable as u8 * A_E | configurable as u8 * A_C,
        Property::Accessor { enumerable, configurable, .. } => A_ACC | enumerable as u8 * A_E | configurable as u8 * A_C,
    }
}

/// The property that `attrs` and the slots (from the first slot of the property on) describe.
#[inline(always)]
fn build(attrs: u8, slots: &[Value]) -> Property {
    let (enumerable, configurable) = (attrs & A_E != 0, attrs & A_C != 0);
    if attrs & A_ACC != 0 {
        Property::Accessor { get: obj_of(slots[0]), set: obj_of(slots[1]), enumerable, configurable }
    } else {
        Property::Data { value: slots[0], writable: attrs & A_W != 0, enumerable, configurable }
    }
}

/// The own named properties of an object in order (`Object::props`).
pub(super) enum Props<'a> {
    Shape(std::slice::Iter<'a, Entry>, &'a [Value]),
    Dict(std::slice::Iter<'a, Option<(PropertyKey, Property)>>),
}

impl Iterator for Props<'_> {
    type Item = (PropertyKey, Property);
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Props::Shape(es, slots) => es.next().map(|e| (e.key, build(e.attrs, &slots[e.slot as usize..]))),
            Props::Dict(es) => es.find_map(|e| *e),
        }
    }
}

impl Object {
    /// The own named property `key` (an index key only if it is in `dict`; `elems` is the caller's business).
    #[inline(always)]
    pub(super) fn get_prop(&self, sh: &Shapes, key: PropertyKey) -> Option<Property> {
        match &self.dict {
            Some(d) => d.get(key),
            // Each property of a shape has a slot: no slot, no property, and the shape is not read.
            None if matches!(key, PropertyKey::Index(_)) || self.slots.is_empty() => None,
            None => self.shape_get(sh, key),
        }
    }

    /// `get_prop` in shape mode. Out of line, as `PropMap::find`: the callers of `Heap::own` stay small enough to inline it.
    #[inline(never)]
    fn shape_get(&self, sh: &Shapes, key: PropertyKey) -> Option<Property> {
        let (shape, k, slot, attrs) = sh.found[recent_at(self.shape, key)].get();
        if shape == self.shape && k == key {
            return (slot != u32::MAX).then(|| build(attrs, &self.slots[slot as usize..]));
        }
        self.shape_find(sh, key)
    }

    /// `shape_get` when `found` does not know the pair: the walk of the table, and a new answer in `found`. Out of line, so the hit
    /// path saves few registers.
    #[inline(never)]
    fn shape_find(&self, sh: &Shapes, key: PropertyKey) -> Option<Property> {
        let c = &sh.found[recent_at(self.shape, key)];
        let e = sh.find(self.shape, key);
        c.set(match e {
            Some(e) => (self.shape, key, e.slot, e.attrs),
            None => (self.shape, key, u32::MAX, 0),
        });
        e.map(|e| build(e.attrs, &self.slots[e.slot as usize..]))
    }

    pub(super) fn props<'a>(&'a self, sh: &'a Shapes) -> Props<'a> {
        match &self.dict {
            Some(d) => Props::Dict(d.entries.iter()),
            None => Props::Shape(sh.entries(self.shape).iter(), &self.slots),
        }
    }

    /// Run `f` on each own named key in order. Cheaper than `props` when only the keys are needed: no property is built.
    #[inline]
    pub(super) fn each_key(&self, sh: &Shapes, mut f: impl FnMut(PropertyKey)) {
        match &self.dict {
            Some(d) => d.entries.iter().flatten().for_each(|(k, _)| f(*k)),
            None => sh.entries(self.shape).iter().for_each(|e| f(e.key)),
        }
    }

    /// The number of own named properties.
    /// How many index keys are in `dict`. An object in shape mode has none.
    #[inline]
    pub(super) fn index_keys(&self) -> u32 {
        self.dict.as_ref().map_or(0, |d| d.index_keys)
    }

    /// Switch to dictionary mode. The properties keep their positions (`own_pos` hints stay true).
    pub(super) fn to_dict(&mut self, sh: &Shapes) {
        if self.dict.is_some() {
            return;
        }
        let es = sh.entries(self.shape);
        let mut m = PropMap::new();
        m.entries.reserve_exact(es.len());
        m.entries.extend(self.props(sh).map(Some));
        m.live = es.len() as u32;
        m.rebuild_index();
        self.slots = Vec::new();
        self.shape = 0;
        self.dict = Some(Box::new(m));
    }

    /// Leave dictionary mode: the properties get shapes and slots, in their order. `false` (no change) when the dictionary holds an index
    /// key, too many properties, an array `length` that is read-only, or when the shape table is full.
    pub(super) fn to_shapes(&mut self, sh: &mut Shapes) -> bool {
        let Some(d) = &self.dict else { return true };
        if d.index_keys > 0 || d.live as usize > MAX_PROPS {
            return false;
        }
        let mut props = d.iter();
        let (mut shape, mut slots) = (ROOT, Vec::with_capacity(d.live as usize));
        if self.is_array {
            let Some((_, Property::Data { value, writable: true, .. })) = props.next() else { return false };
            (shape, slots) = (ROOT_ARRAY, vec![value]);
        }
        for (k, p) in props {
            let Some(next) = sh.add(shape, k, attrs_of(&p)) else { return false };
            shape = next;
            match p {
                Property::Data { value, .. } => slots.push(value),
                Property::Accessor { get, set, .. } => {
                    slots.push(get.map_or(Value::Undefined, Value::Object));
                    slots.push(set.map_or(Value::Undefined, Value::Object));
                }
            }
        }
        (self.shape, self.slots, self.dict) = (shape, slots, None);
        true
    }

    /// Write the own named property `key`: add it, or replace it. A change of attributes, an index key, or a full shape table
    /// sends the object to dictionary mode.
    pub(super) fn set_prop(&mut self, sh: &mut Shapes, key: PropertyKey, p: Property) {
        if let Some(d) = &mut self.dict {
            return d.set(key, p);
        }
        let attrs = attrs_of(&p);
        match sh.find(self.shape, key) {
            Some(&Entry { slot, attrs: a, .. }) if a == attrs => {
                let s = slot as usize;
                match p {
                    Property::Data { value, .. } => self.slots[s] = value,
                    Property::Accessor { get, set, .. } => {
                        self.slots[s] = get.map_or(Value::Undefined, Value::Object);
                        self.slots[s + 1] = set.map_or(Value::Undefined, Value::Object);
                    }
                }
                return;
            }
            Some(_) => {
                self.to_dict(sh);
                return self.dict.as_mut().unwrap().set(key, p);
            }
            None => {}
        }
        self.add_prop(sh, key, p);
    }

    /// Give an object of the root shape the two read-only, configurable data properties `keys` (`Heap::init_function`). The shape of the
    /// pair is kept: a realm makes hundreds of functions, and each skips two transition lookups. `false`: the shape table is full.
    pub(super) fn init_fn(&mut self, sh: &mut Shapes, keys: (PropertyKey, PropertyKey), v: [Value; 2]) -> bool {
        let Some(shape) = sh.fn_shape(keys, None) else { return false };
        self.shape = shape;
        self.slots.extend(v);
        true
    }

    /// Add the own named property `key`, which the object does not have (the caller has looked).
    #[inline]
    pub(super) fn add_prop(&mut self, sh: &mut Shapes, key: PropertyKey, p: Property) {
        if let Some(d) = &mut self.dict {
            return d.set(key, p);
        }
        if !matches!(key, PropertyKey::Index(_))
            && let Some(next) = sh.add(self.shape, key, attrs_of(&p))
        {
            self.shape = next;
            match p {
                Property::Data { value, .. } => self.slots.push(value),
                Property::Accessor { get, set, .. } => {
                    self.slots.push(get.map_or(Value::Undefined, Value::Object));
                    self.slots.push(set.map_or(Value::Undefined, Value::Object));
                }
            }
            return;
        }
        self.to_dict(sh);
        self.dict.as_mut().unwrap().set(key, p);
    }

    /// Remove the own named property `key`. The last key, or any key of an object of at most `SCAN_MAX` properties, keeps shape
    /// mode: the shape before `key` takes the later keys again, so objects that drop the same key share the new shapes (a reviver
    /// that drops a key of each record). Another key of a larger object sends it to dictionary mode.
    pub(super) fn remove_prop(&mut self, sh: &mut Shapes, key: PropertyKey) -> bool {
        if let Some(d) = &mut self.dict {
            return d.remove(key);
        }
        let Some(pos) = sh.pos(self.shape, key) else { return false };
        let n = sh.count(self.shape);
        let gone = sh.entries(self.shape)[pos];
        let mut base = sh.parent(self.shape);
        if n <= SCAN_MAX || pos + 1 == n {
            // Slots follow the order of the keys, so the later values only move down by the width of the removed one.
            let mut later = [gone; SCAN_MAX];
            later[..n - pos - 1].copy_from_slice(&sh.entries(self.shape)[pos + 1..]);
            for _ in pos + 1..n {
                base = sh.parent(base);
            }
            if let Some(next) = later[..n - pos - 1].iter().try_fold(base, |s, e| sh.add(s, e.key, e.attrs)) {
                let w = 1 + (gone.attrs & A_ACC != 0) as usize;
                self.slots.drain(gone.slot as usize..gone.slot as usize + w);
                self.shape = next;
                return true;
            }
        }
        self.to_dict(sh);
        self.dict.as_mut().unwrap().remove(key);
        true
    }

    /// Change the value of the own data property `key`. `false`: no such property (an accessor, or not there).
    #[inline]
    pub(super) fn set_value(&mut self, sh: &Shapes, key: PropertyKey, v: Value) -> bool {
        match &mut self.dict {
            Some(d) => match d.find(key).map(|p| &mut d.entries[p]) {
                Some(Some((_, Property::Data { value, .. }))) => {
                    *value = v;
                    true
                }
                _ => false,
            },
            None => match sh.find(self.shape, key) {
                Some(e) if e.attrs & A_ACC == 0 => {
                    self.slots[e.slot as usize] = v;
                    true
                }
                _ => false,
            },
        }
    }

    /// Change the value of the data property at position `pos` (the hint of `own_pos`) if it is `key` and `writable` allows. `false`: not
    /// that property, an accessor, or read-only.
    #[inline]
    pub(super) fn write_at(&mut self, sh: &Shapes, pos: usize, key: PropertyKey, v: Value) -> bool {
        match &mut self.dict {
            Some(d) => match d.entries.get_mut(pos) {
                Some(Some((k, Property::Data { value, writable: true, .. }))) if *k == key => {
                    *value = v;
                    true
                }
                _ => false,
            },
            None => self.write_slot(sh, pos, key, v),
        }
    }

    /// `write_at` for an object in shape mode. Out of line: the hints are for the global object, which is a dictionary.
    #[inline(never)]
    fn write_slot(&mut self, sh: &Shapes, pos: usize, key: PropertyKey, v: Value) -> bool {
        match sh.entries(self.shape).get(pos) {
            Some(e) if e.key == key && e.attrs & (A_W | A_ACC) == A_W => {
                self.slots[e.slot as usize] = v;
                true
            }
            _ => false,
        }
    }

    /// The value of the data property at position `pos` if it is `key`.
    #[inline]
    pub(super) fn read_at(&self, sh: &Shapes, pos: usize, key: PropertyKey) -> Option<Value> {
        match &self.dict {
            Some(d) => match d.entries.get(pos) {
                Some(Some((k, Property::Data { value, .. }))) if *k == key => Some(*value),
                _ => None,
            },
            None => self.read_slot(sh, pos, key),
        }
    }

    /// `read_at` for an object in shape mode. Out of line, as `write_slot`.
    #[inline(never)]
    fn read_slot(&self, sh: &Shapes, pos: usize, key: PropertyKey) -> Option<Value> {
        match sh.entries(self.shape).get(pos) {
            Some(e) if e.key == key && e.attrs & A_ACC == 0 => Some(self.slots[e.slot as usize]),
            _ => None,
        }
    }

    /// Write the array `length` entry (the first one): a new value and/or `writable`. A read-only `length` is a change of attributes.
    pub(super) fn set_length(&mut self, sh: &Shapes, len: Option<u32>, writable: Option<bool>) {
        if writable == Some(false) {
            self.to_dict(sh);
        }
        match &mut self.dict {
            None => {
                if let Some(l) = len {
                    self.slots[0] = Value::Number(l as f64);
                }
            }
            Some(d) => {
                let Some(Some((_, Property::Data { value, writable: w, .. }))) = d.entries.first_mut() else { unreachable!() };
                if let Some(l) = len {
                    *value = Value::Number(l as f64);
                }
                if let Some(x) = writable {
                    *w = x;
                }
            }
        }
    }

    /// Room for `n` more own properties.
    pub(super) fn reserve_props(&mut self, n: usize) {
        match &mut self.dict {
            Some(d) => d.entries.reserve_exact(n),
            None => self.slots.reserve_exact(n),
        }
    }
}

/// One inline cache entry: 24 bytes in a release build (a debug build also keeps the key, to check every hit). A site that has no
/// entry holds `Ic::EMPTY`.
///
/// Kinds. `IC_OWN`: a data property of the receiver, in `slot`. `IC_OWN_ACC`: an accessor of the receiver (getter in `slot`). `IC_PROTO`
/// and `IC_PROTO_ACC`: the property is on the prototype at `depth` (1 is the receiver's prototype). `IC_ABSENT`: no object of the chain
/// has the key, and the chain has `depth` prototypes. The last three hold the receiver's first prototype in `first` and the epoch.
#[derive(Clone, Copy, Debug)]
pub struct Ic {
    /// The shape of the receiver that filled the entry. Kinds other than `IC_OWN` add `SHAPE_TAG`, so the inline `ic_own` needs one
    /// compare. `EMPTY` holds `u32::MAX`, which no shape has.
    shape: u32,
    /// The first slot of the property, in the receiver (own kinds) or in the holder (prototype kinds).
    slot: u32,
    kind: u8,
    depth: u8,
    /// The prototype of the receiver at fill time (prototype and absent kinds with `depth` > 0).
    first: ObjId,
    /// `Heap::proto_epoch` at fill time (prototype and absent kinds).
    epoch: u32,
    /// Write misses of the site so far (`set_fill`). Past `MISS_MAX` the site is `IC_MEGA`. It sits in padding.
    misses: u8,
    #[cfg(debug_assertions)]
    key: Option<PropertyKey>,
}

const IC_EMPTY: u8 = 0;
const IC_OWN: u8 = 1;
const IC_OWN_ACC: u8 = 2;
const IC_PROTO: u8 = 3;
const IC_PROTO_ACC: u8 = 4;
const IC_ABSENT: u8 = 5;
/// An own property of one dictionary receiver (a frozen or sealed object, a template object): `first` is the receiver and `depth` the
/// position of the key in its `PropMap` (below 256). The key is a string: `epoch` holds its index and `slot` its generation. `shape`
/// is 0, the shape of every dictionary, with no tag: the inline `ic_own` passes the shape test, finds no slot (a dictionary has none)
/// and asks `ic_dict_own`. A hit checks the identity and the key at that position, so a delete or a move only misses.
const IC_DICT: u8 = 6;
/// `IC_OWN` that is also writable data: a write hits too (`ic_store`). A read treats it as `IC_OWN`. Never an array's `length`.
const IC_STORE: u8 = 7;
/// A write that adds `key` to a receiver of shape S (in `shape`, tagged): `slot` holds the shape S' after the add, whose last slot is
/// the new value. As for `IC_ABSENT`, `first`, `depth` and the epoch say that the chain has no setter and no read-only property for
/// `key`. A read never hits it.
const IC_ADD: u8 = 8;
/// A write site that missed `MISS_MAX` times (many shapes or prototypes, a setter, a refused receiver): every write is the plain `set`,
/// with no fill. A read never hits it.
const IC_MEGA: u8 = 9;
const MISS_MAX: u8 = 16;
/// Added to the shape of every kind but `IC_OWN`. Shape ids stay below `SHAPE_CAP`, so the tag never makes a real id.
const SHAPE_TAG: u32 = 1 << 31;

impl Ic {
    /// A write site that missed too often: its writes take the plain `set`.
    #[inline]
    pub fn is_mega(&self) -> bool {
        self.kind == IC_MEGA
    }

    /// The entry of a site that has not run, or that a fill refused. It never hits.
    pub const EMPTY: Ic = Ic {
        shape: u32::MAX,
        slot: 0,
        kind: IC_EMPTY,
        depth: 0,
        first: ObjId { index: 0, gn: 0 },
        epoch: 0,
        misses: 0,
        #[cfg(debug_assertions)]
        key: None,
    };
}

/// Counters of the cache (`Heap::ic_stats`). The evaluator counts the inline own hits itself.
#[derive(Clone, Copy, Debug, Default)]
pub struct IcStats {
    /// Hits in `get_ic`: an own accessor, a prototype or an absent key (and own data when the caller did not try `ic_own`).
    pub chain_hits: u64,
    /// Fills by the kind they made.
    pub fill_own: u64,
    pub fill_proto: u64,
    pub fill_absent: u64,
    /// Misses that could not fill: a dictionary or exotic object, a lazy name, an index key, a long chain.
    pub refused: u64,
    /// Bumps of `proto_epoch`.
    pub epoch_bumps: u64,
    /// Writes: add hits in `set_ic`, and fills of store and add entries. The evaluator counts the store hits.
    pub add_hits: u64,
    pub fill_store: u64,
    pub fill_add: u64,
}

// ---------- shape sweep (step A6) ----------

/// The least shape count at which a collect sweeps the shapes. A realm makes about 2,800.
pub(super) const SWEEP_MIN: usize = 8192;

impl Heap {
    /// The second half of a shape sweep in `collect`, after the objects are swept: free the unmarked shapes, give the live objects the
    /// new ids, and reset the stub entries. The evaluator resets its own entries when `shape_sweeps` changes.
    #[inline(never)]
    pub(super) fn sweep_shapes(&mut self, live: &mut [bool]) {
        let remap = self.shapes.sweep(live);
        for o in &mut self.objects.live {
            o.shape = remap[o.shape as usize];
        }
        self.stubs.fill((PropertyKey::Index(0), Ic::EMPTY));
        self.shape_sweeps += 1;
        // A full table sweeps at the next collect, even when most shapes live.
        self.shape_sweep_at = (2 * self.shapes.len()).clamp(SWEEP_MIN, SHAPE_CAP);
    }

    /// Shape sweeps so far. After a sweep, every `Ic` and every shape id kept outside the heap is stale and must be reset.
    pub fn shape_sweeps(&self) -> u64 {
        self.shape_sweeps
    }
}

// ---------- preset shapes (step A4) ----------

impl Heap {
    /// A new ordinary object that starts in `shape` with the values `slots`: an object literal whose site knows its final shape skips the
    /// transition and the define of each key. `shape` must be an object literal shape of `slots.len()` keys (`Heap::literal_shape`).
    pub(crate) fn new_object_with_shape(&mut self, proto: Option<ObjId>, shape: u32, slots: Vec<Value>) -> R<ObjId> {
        if shape as usize >= self.shapes.len() || shape == 0 || self.shapes.entries(shape).len() != slots.len() {
            return Err(HeapError::NeedsExecution("new_object_with_shape: the slots do not match the shape"));
        }
        debug_assert!(self.shapes.is_literal(shape, slots.len()));
        for &v in &slots {
            self.check_value(v)?;
        }
        let extra = slots.len() * SLOT_BYTES;
        self.charge(extra)?;
        let o = self.alloc_object(proto, false)?;
        let ob = self.objects.get_mut(o.index, o.gn).expect("just made");
        (ob.shape, ob.slots) = (shape, slots);
        self.bytes += extra;
        Ok(o)
    }

    /// The shape of `o` when it is an object literal shape of `n` keys (see `Shapes::is_literal`) and `o` has nothing else: its site can
    /// make the next object with `new_object_with_shape`. `None`: the object is in dictionary mode, has elements, or has other keys.
    pub(crate) fn literal_shape(&self, o: ObjId, n: usize) -> Option<u32> {
        let ob = self.objects.get(o.index, o.gn)?;
        let ok = ob.dict.is_none() && ob.elems.is_empty() && ob.exotic == 0 && ob.lazy == 0 && ob.extensible && self.shapes.is_literal(ob.shape, n);
        ok.then_some(ob.shape)
    }

    /// A new function object with the read-only, configurable `length` and `name`, and a lazy `prototype` when `lazy_proto` is set (as
    /// `init_function` and `add_lazy_prototype` give, in one step): it starts in the preset function shape. `cap` slots are reserved. As
    /// in `init_function`, the keys and the name are the caller's live handles: they are not checked.
    pub(crate) fn new_function(&mut self, proto: Option<ObjId>, length: (PropertyKey, f64), name: (PropertyKey, StrId), lazy_proto: bool, cap: usize) -> R<ObjId> {
        let kp = match (lazy_proto, self.lazy) {
            (false, _) => None,
            (true, Some((_, kp, _))) => Some(kp),
            (true, None) => return Err(HeapError::NeedsExecution("set_lazy_prototype first")),
        };
        let Some(shape) = self.shapes.fn_shape((length.0, name.0), kp) else {
            let o = self.new_object(proto)?;
            self.reserve_props(o, cap)?;
            self.init_function(o, length, name)?;
            if lazy_proto {
                self.add_lazy_prototype(o)?;
            }
            return Ok(o);
        };
        let mut slots = Vec::with_capacity(cap.max(2 + lazy_proto as usize));
        slots.extend([Value::Number(length.1), Value::String(name.1)]);
        if lazy_proto {
            slots.push(Value::Undefined);
        }
        let extra = slots.len() * SLOT_BYTES;
        self.charge(extra)?;
        let o = self.alloc_object(proto, false)?;
        let ob = self.objects.get_mut(o.index, o.gn).expect("just made");
        (ob.shape, ob.slots) = (shape, slots);
        if lazy_proto {
            ob.lazy |= LAZY_PROTO;
        }
        self.bytes += extra;
        Ok(o)
    }
}

impl Heap {
    /// The value of an own data property of `o` when `ic` hits, else `None`. The caller runs `get_ic` on `None`. One handle check,
    /// one compare and one load.
    #[inline(always)]
    pub fn ic_own(&self, o: ObjId, ic: &Ic) -> Option<Value> {
        let ob = self.objects.get(o.index, o.gn)?;
        if ob.shape != ic.shape {
            return None;
        }
        let Some(&v) = ob.slots.get(ic.slot as usize) else { return self.ic_dict_own(o, ob, ic) };
        #[cfg(debug_assertions)]
        self.ic_verify(o, ic, Get::Value(v));
        Some(v)
    }

    /// `ic_own` for an `IC_DICT` entry: the data property of the receiver at the entry's position, if it is still the key.
    #[inline(never)]
    fn ic_dict_own(&self, o: ObjId, ob: &Object, ic: &Ic) -> Option<Value> {
        if ic.kind != IC_DICT || o != ic.first {
            return None;
        }
        match ob.dict.as_ref()?.entries.get(ic.depth as usize)? {
            Some((PropertyKey::String(s), Property::Data { value, .. })) if s.index == ic.epoch && s.gn == ic.slot => {
                #[cfg(debug_assertions)]
                self.ic_verify(o, ic, Get::Value(*value));
                Some(*value)
            }
            _ => None,
        }
    }

    /// `o` has the shape that `ic` was filled for (a hit is likely). Picks the entry of a native site with more than one entry.
    #[inline]
    pub fn ic_shape_is(&self, o: ObjId, ic: &Ic) -> bool {
        self.objects.get(o.index, o.gn).is_some_and(|ob| ob.shape == ic.shape & !SHAPE_TAG)
    }

    /// `get`, through `ic`: a hit of any kind, else the plain walk, which fills `ic` for the next read. Out of line, not cold: the
    /// prototype hits (methods) run here.
    #[inline(never)]
    pub fn get_ic(&mut self, o: ObjId, key: PropertyKey, ic: &mut Ic) -> R<Get> {
        if let Some(g) = self.ic_hit(o, key, ic) {
            self.ic_stats.chain_hits += 1;
            #[cfg(debug_assertions)]
            self.ic_verify(o, ic, g);
            return Ok(g);
        }
        self.ic_miss(o, key, ic)
    }

    /// `get` for a read that has no site of its own: native code (option bags, iterator results, the holders of a reviver, symbol
    /// keys such as `Symbol.toPrimitive`). It uses the entry of `stubs` for the receiver's shape and the key, as a
    /// site uses its own. Two pairs that map to one entry take turns. An index key, and a key other than a string on a dictionary
    /// (which has no entry kind for it), is the plain read. A pair that the fill refused keeps an empty entry with the receiver's shape:
    /// its next reads are the plain read at once (a lazy `prototype`, an exotic receiver or chain). The plain read is always right, so
    /// a refusal that stops being true costs only speed. Only chosen native sites use it (`Interp::get_prop_nat`): on every read, a
    /// refused pair (a lazy `prototype`, a new object of a churn loop) costs more than the plain read.
    #[inline]
    pub fn get_stub(&mut self, o: ObjId, key: PropertyKey) -> R<Get> {
        let shape = match self.objects.get(o.index, o.gn) {
            Some(ob) if matches!(key, PropertyKey::String(_)) || !matches!(key, PropertyKey::Index(_)) && ob.shape != 0 => ob.shape,
            _ => return self.get(o, key),
        };
        let i = recent_at(shape, key);
        let (k, mut ic) = self.stubs[i];
        if k == key {
            if let Some(g) = self.ic_hit(o, key, &ic) {
                self.ic_stats.chain_hits += 1;
                #[cfg(debug_assertions)]
                self.ic_verify(o, &ic, g);
                return Ok(g);
            }
            if ic.kind == IC_EMPTY && ic.shape == shape {
                return self.get(o, key);
            }
        }
        ic = Ic::EMPTY;
        let r = self.ic_miss(o, key, &mut ic);
        if ic.kind == IC_EMPTY {
            ic.shape = shape;
        }
        self.stubs[i] = (key, ic);
        r
    }

    /// The miss of `get_ic` and `get_stub`: the plain read, and a new entry in `ic` when the receiver allows one.
    #[inline(never)]
    fn ic_miss(&mut self, o: ObjId, key: PropertyKey, ic: &mut Ic) -> R<Get> {
        // A receiver that is never cached (a proxy, a host object, a String wrapper) skips the fill.
        if self.ic_refuses(o) {
            return self.get_refused(o, key);
        }
        if self.objects.get(o.index, o.gn).is_some_and(|ob| ob.dict.is_some() && !ob.fast_on_use) {
            return self.get_dict(o, key, ic);
        }
        self.ic_fill(o, key, ic)
    }

    /// `get_ic` for a dictionary receiver: an own key fills an `IC_DICT` entry. Any other key is the plain read, which starts at the
    /// prototype when the receiver is ordinary, so the own key is searched once.
    #[cold]
    #[inline(never)]
    fn get_dict(&mut self, o: ObjId, key: PropertyKey, ic: &mut Ic) -> R<Get> {
        *ic = Ic::EMPTY;
        let ob = self.obj(o)?;
        let PropertyKey::String(s) = key else { return self.get_refused(o, key) };
        if ob.exotic != 0 || ob.lazy & (LAZY_PROTO | LAZY_CHAIN | LAZY_HS) != 0 {
            return self.get_refused(o, key);
        }
        self.check_key(key)?;
        let d = ob.dict.as_ref().expect("a dictionary receiver");
        if let Some(pos) = d.find(key) {
            let p = d.entries[pos].expect("find gives a live entry").1;
            if pos <= u8::MAX as usize {
                *ic = Ic { shape: 0, slot: s.gn, kind: IC_DICT, depth: pos as u8, first: o, epoch: s.index, ..Ic::EMPTY };
            }
            #[cfg(debug_assertions)]
            {
                ic.key = Some(key);
            }
            self.ic_stats.fill_own += 1;
            return Ok(get_of(p));
        }
        let proto = ob.proto;
        self.ic_stats.refused += 1;
        match proto {
            Some(p) => self.get(p, key),
            None => Ok(Get::Value(Value::Undefined)),
        }
    }

    /// `get` for a receiver that `ic_refuses`, counted as a refusal.
    pub fn get_refused(&mut self, o: ObjId, key: PropertyKey) -> R<Get> {
        self.ic_stats.refused += 1;
        self.get(o, key)
    }

    /// The cache never fills for `o`: a proxy, a named host object or a String wrapper. A dictionary fills only `IC_DICT` entries.
    #[inline]
    pub fn ic_refuses(&self, o: ObjId) -> bool {
        self.objects.get(o.index, o.gn).is_some_and(|ob| ob.exotic > EXOTIC)
    }

    #[inline(always)]
    fn ic_hit(&self, o: ObjId, key: PropertyKey, ic: &Ic) -> Option<Get> {
        let ob = self.objects.get(o.index, o.gn)?;
        if ob.shape != ic.shape & !SHAPE_TAG {
            return None;
        }
        let slot = ic.slot as usize;
        match ic.kind {
            IC_OWN | IC_STORE => return ob.slots.get(slot).map(|&v| Get::Value(v)),
            IC_OWN_ACC => return ob.slots.get(slot).map(|&v| Get::Accessor { getter: obj_of(v) }),
            IC_EMPTY | IC_ADD | IC_MEGA => return None,
            IC_DICT => {
                if o != ic.first {
                    return None;
                }
                return match ob.dict.as_ref()?.entries.get(ic.depth as usize)? {
                    Some((k, p)) if *k == key => Some(get_of(*p)),
                    _ => None,
                };
            }
            _ => {}
        }
        if ic.epoch != self.proto_epoch {
            return None;
        }
        if ic.depth == 0 {
            return ob.proto.is_none().then_some(Get::Value(Value::Undefined));
        }
        if ob.proto != Some(ic.first) {
            return None;
        }
        if ic.kind == IC_ABSENT {
            return Some(Get::Value(Value::Undefined));
        }
        // The epoch says that no object of the chain changed its prototype or its keys since the fill, so the holder is `depth - 1` steps on.
        let mut h = self.objects.get(ic.first.index, ic.first.gn)?;
        for _ in 1..ic.depth {
            let p = h.proto?;
            h = self.objects.get(p.index, p.gn)?;
        }
        if let Some(d) = &h.dict {
            return d.entries.get(slot)?.map(|(_, p)| get_of(p));
        }
        let v = *h.slots.get(slot)?;
        Some(if ic.kind == IC_PROTO { Get::Value(v) } else { Get::Accessor { getter: obj_of(v) } })
    }

    /// The miss: the plain walk, and a new entry when the chain allows one. The walk is `lookup` for an ordinary chain. Any chain that
    /// `lookup` treats in a special way (an exotic object, a lazy name, a host ask) runs `get` and fills nothing.
    #[cold]
    #[inline(never)]
    fn ic_fill(&mut self, o: ObjId, key: PropertyKey, ic: &mut Ic) -> R<Get> {
        *ic = Ic::EMPTY;
        match self.ic_make(o, key)? {
            Some((new, g)) => {
                *ic = new;
                Ok(g)
            }
            None => {
                self.ic_stats.refused += 1;
                self.get(o, key)
            }
        }
    }

    fn ic_make(&mut self, o: ObjId, key: PropertyKey) -> R<Option<(Ic, Get)>> {
        self.check_key(key)?;
        if self.obj(o)?.fast_on_use {
            self.make_fast(o);
        }
        let recv = self.obj(o)?;
        if matches!(key, PropertyKey::Index(_)) || recv.dict.is_some() || matches!(self.lazy, Some((_, kp, _)) if kp == key) {
            return Ok(None);
        }
        // A plain exotic object (`ROOT_PLAIN`) is ordinary for a key that is not numeric. The key of a site never changes, so one test
        // at the fill is enough.
        let plain = !matches!(key, PropertyKey::String(_)) || !super::binary::canonical_numeric(self, key);
        if recv.exotic != 0 && !(recv.exotic == EXOTIC && plain) {
            return Ok(None);
        }
        let got = |attrs: u8, v: Value| if attrs & A_ACC != 0 { Get::Accessor { getter: obj_of(v) } } else { Get::Value(v) };
        let mut ic = Ic { shape: recv.shape, kind: IC_OWN, ..Ic::EMPTY };
        #[cfg(debug_assertions)]
        {
            ic.key = Some(key);
        }
        if let Some(&Entry { slot, attrs, .. }) = self.shapes.find(recv.shape, key) {
            ic.slot = slot;
            if attrs & A_ACC != 0 {
                ic.kind = IC_OWN_ACC;
                ic.shape |= SHAPE_TAG;
            } else if attrs & A_W != 0 && recv.exotic == 0 && recv.lazy == 0 && !(recv.is_array && key == PropertyKey::String(self.length)) {
                ic.kind = IC_STORE;
            }
            let g = got(attrs, recv.slots[slot as usize]);
            self.ic_stats.fill_own += 1;
            return Ok(Some((ic, g)));
        }
        if self.proto_epoch == u32::MAX {
            return Ok(None);
        }
        let on = matches!(key, PropertyKey::String(s) if self.on_name(s));
        let first = recv.proto;
        let mut indexed = recv.exotic == EXOTIC && matches!(self.exotic(o), Some(super::binary::Exotic::Indexed));
        let (mut cur, mut depth, mut found) = (first, 0u8, None);
        while let Some(c) = cur {
            if self.obj(c)?.fast_on_use {
                self.make_fast(c);
            }
            let ob = self.obj(c)?;
            // An open host named object never hides a property of its chain (`named_hidden`), so a hit behind it stays valid.
            let open = ob.exotic == EXOTIC_NAMED && matches!(key, PropertyKey::String(_)) && self.named_open(c);
            if depth == u8::MAX || ob.exotic != 0 && !(ob.exotic == EXOTIC && plain) && !open || on && ob.lazy & LAZY_CHAIN != 0 {
                return Ok(None);
            }
            indexed |= open;
            indexed |= ob.exotic == EXOTIC && matches!(self.exotic(c), Some(super::binary::Exotic::Indexed));
            depth += 1;
            if let Some(p) = self.own(ob, key) {
                // A dictionary holder (a prototype with more than `MAX_PROPS` keys, or one that changed attributes) is read at its entry
                // position: a delete or a new key moves positions, and both bump the epoch.
                let slot = match &ob.dict {
                    Some(d) => d.find(key).expect("own found it") as u32,
                    None => self.shapes.find(ob.shape, key).expect("own found it").slot,
                };
                found = Some((slot, p));
                break;
            }
            cur = ob.proto;
        }
        // Indexed hosts and open named objects can expose live names without changing a heap shape or prototype epoch.
        if found.is_none() && indexed {
            return Ok(None);
        }
        // Flag the walked prototypes: a later change of their keys or of their prototype bumps the epoch.
        let mut cur = first;
        for _ in 0..depth {
            let c = cur.expect("walked above");
            let ob = self.objects.get_mut(c.index, c.gn).expect("walked above");
            ob.ic_chain = true;
            cur = ob.proto;
        }
        ic.shape |= SHAPE_TAG;
        ic.depth = depth;
        ic.first = first.unwrap_or(ic.first);
        ic.epoch = self.proto_epoch;
        Ok(Some(match found {
            Some((slot, p)) => {
                ic.slot = slot;
                ic.kind = if matches!(p, Property::Accessor { .. }) { IC_PROTO_ACC } else { IC_PROTO };
                self.ic_stats.fill_proto += 1;
                (ic, get_of(p))
            }
            None => {
                ic.kind = IC_ABSENT;
                self.ic_stats.fill_absent += 1;
                (ic, Get::Value(Value::Undefined))
            }
        }))
    }

    /// Setup mode: while it is on, an object that would make a new shape for a new key goes to a dictionary instead (`fast_on_use`).
    /// `Interp::realm` turns it on while it installs the builtins. A host can use it while it installs its own objects.
    pub fn set_setup(&mut self, on: bool) {
        self.setup = on;
    }

    /// Give shapes to a dictionary of setup mode. Its keys and their order stay, so no cache entry changes.
    #[cold]
    fn make_fast(&mut self, o: ObjId) {
        self.edit_with(o, |ob, sh| {
            ob.fast_on_use = false;
            ob.to_shapes(sh);
        });
    }

    /// A key, an attribute or the prototype of `o` changes. If a fill walked over `o`, every prototype and absent entry stops hitting.
    #[inline]
    pub(super) fn chain_changed(&mut self, o: ObjId) {
        if self.objects.get(o.index, o.gn).is_some_and(|ob| ob.ic_chain) {
            self.proto_epoch = self.proto_epoch.saturating_add(1);
            self.ic_stats.epoch_bumps += 1;
        }
    }

    /// The counters of the cache.
    pub fn ic_stats(&self) -> IcStats {
        self.ic_stats
    }

    /// Debug builds: a hit must give what the plain walk gives. A wrong cache is a silent wrong answer, so this panics.
    #[cfg(debug_assertions)]
    fn ic_verify(&self, o: ObjId, ic: &Ic, hit: Get) {
        let key = ic.key.expect("a filled entry has its key");
        let mut cur = Some(o);
        let want = loop {
            let Some(c) = cur else { break Get::Value(Value::Undefined) };
            let ob = self.obj(c).expect("a live chain");
            assert!(ob.exotic <= EXOTIC, "a cache hit through a named, String or proxy object");
            if let Some(p) = self.own(ob, key) {
                break get_of(p);
            }
            cur = ob.proto;
        };
        let same = match (hit, want) {
            (Get::Value(a), Get::Value(b)) => a.same_value(b),
            (Get::Accessor { getter: a }, Get::Accessor { getter: b }) => a == b,
            _ => false,
        };
        assert!(same, "inline cache hit {hit:?} but the walk gives {want:?} ({ic:?})");
    }

    /// The store hit: `o[key] = v` when `ic` is an `IC_STORE` entry for the shape of `o` (an own writable data property). One handle
    /// check, one compare, a kind test and one store. `false`: the caller runs `set_ic`.
    #[inline(always)]
    pub fn ic_store(&mut self, o: ObjId, ic: &Ic, v: Value) -> bool {
        if ic.kind != IC_STORE || self.check_value(v).is_err() {
            return false;
        }
        let Some(ob) = self.objects.get_mut(o.index, o.gn) else { return false };
        if ob.shape != ic.shape {
            return false;
        }
        let Some(slot) = ob.slots.get_mut(ic.slot as usize) else { return false };
        *slot = v;
        #[cfg(debug_assertions)]
        self.ic_verify_set(o, ic);
        true
    }

    /// `set` through `ic`: an add hit (the shape transition of the entry), else the plain write, which fills `ic` for the next write.
    /// The caller tried `ic_store` first. Never calls a setter: it returns it, as `set` does.
    #[inline(never)]
    pub fn set_ic(&mut self, o: ObjId, key: PropertyKey, value: Value, ic: &mut Ic) -> R<Set> {
        if ic.kind == IC_ADD && self.ic_add(o, ic, value)? {
            self.ic_stats.add_hits += 1;
            return Ok(Set::Done);
        }
        if ic.kind == IC_MEGA {
            return self.set(o, key, value);
        }
        self.set_fill(o, key, value, ic)
    }

    /// The add hit: `o` has the shape S of the entry, the same prototype and no change on the chain (the epoch), and it can take a new
    /// key. The new key goes to the end of the slots and the shape becomes S' (in `slot`), as `add_data_fast` does it.
    #[inline(always)]
    fn ic_add(&mut self, o: ObjId, ic: &Ic, v: Value) -> R<bool> {
        let Some(ob) = self.objects.get(o.index, o.gn) else { return Ok(false) };
        if ob.shape != ic.shape & !SHAPE_TAG || !ob.extensible || ob.lazy != 0 || ob.ic_chain || ic.epoch != self.proto_epoch {
            return Ok(false);
        }
        if if ic.depth == 0 { ob.proto.is_some() } else { ob.proto != Some(ic.first) } {
            return Ok(false);
        }
        #[cfg(debug_assertions)]
        self.ic_verify_set(o, ic);
        self.check_value(v)?;
        self.charge(SLOT_BYTES)?;
        let ob = self.objects.get_mut(o.index, o.gn).expect("checked above");
        ob.shape = ic.slot;
        ob.slots.push(v);
        self.bytes += SLOT_BYTES;
        Ok(true)
    }

    /// The miss of `set_ic`: the plain `set`, and an entry when the write was a store to an own writable data property or a plain add
    /// of a new key to an ordinary object in shape mode, with no exotic object, lazy name, setter or read-only property on the chain.
    #[cold]
    #[inline(never)]
    fn set_fill(&mut self, o: ObjId, key: PropertyKey, value: Value, ic: &mut Ic) -> R<Set> {
        let misses = ic.misses + 1;
        *ic = Ic { misses, ..Ic::EMPTY };
        if misses >= MISS_MAX {
            ic.kind = IC_MEGA;
            return self.set(o, key, value);
        }
        let Some(ob) = self.objects.get(o.index, o.gn) else { return self.set(o, key, value) };
        let length = PropertyKey::String(self.length);
        if matches!(key, PropertyKey::Index(_))
            || ob.exotic != 0
            || ob.lazy != 0
            || ob.dict.is_some()
            || ob.fast_on_use
            || self.setup
            || ob.is_array && key == length
            || matches!(self.lazy, Some((_, kp, _)) if kp == key)
        {
            self.ic_stats.refused += 1;
            return self.set(o, key, value);
        }
        let (shape, nslots) = (ob.shape, ob.slots.len());
        let mut new = Ic { shape, misses, ..Ic::EMPTY };
        #[cfg(debug_assertions)]
        {
            new.key = Some(key);
        }
        if let Some(&Entry { slot, attrs, .. }) = self.shapes.find(shape, key) {
            let r = self.set(o, key, value)?;
            if attrs & (A_W | A_ACC) == A_W {
                new.slot = slot;
                new.kind = IC_STORE;
                *ic = new;
                self.ic_stats.fill_store += 1;
            }
            return Ok(r);
        }
        // A new own key: the chain must hold no setter and no read-only property for it, and stay so (the epoch).
        let first = ob.proto;
        let ok = ob.extensible && !ob.ic_chain && self.proto_epoch != u32::MAX;
        let on = matches!(key, PropertyKey::String(s) if self.on_name(s));
        let (mut cur, mut depth) = (if ok { first } else { None }, 0u8);
        let mut ok = ok;
        while let Some(c) = cur {
            if self.obj(c)?.fast_on_use {
                self.make_fast(c);
            }
            let ob = self.obj(c)?;
            if depth == u8::MAX || ob.exotic != 0 || on && ob.lazy & LAZY_CHAIN != 0 || self.is_lazy(ob, key) {
                ok = false;
                break;
            }
            depth += 1;
            if let Some(p) = self.own(ob, key) {
                ok = matches!(p, Property::Data { writable: true, .. });
                break;
            }
            cur = ob.proto;
        }
        let r = self.set(o, key, value)?;
        if !ok || !matches!(r, Set::Done) {
            self.ic_stats.refused += 1;
            return Ok(r);
        }
        // The write must have been the plain transition: one more slot, the key added to S with all attributes.
        let ob = self.obj(o)?;
        if ob.dict.is_some() || ob.slots.len() != nslots + 1 || !self.shapes.added(shape, ob.shape, key, A_W | A_E | A_C) {
            self.ic_stats.refused += 1;
            return Ok(r);
        }
        new.slot = ob.shape;
        let mut cur = first;
        for _ in 0..depth {
            let c = cur.expect("walked above");
            let ob = self.objects.get_mut(c.index, c.gn).expect("walked above");
            ob.ic_chain = true;
            cur = ob.proto;
        }
        new.shape |= SHAPE_TAG;
        new.kind = IC_ADD;
        new.depth = depth;
        new.first = first.unwrap_or(new.first);
        new.epoch = self.proto_epoch;
        *ic = new;
        self.ic_stats.fill_add += 1;
        Ok(r)
    }

    /// Debug builds: a store or add hit must be a write that `set` would make in the same way. Checked before the write.
    #[cfg(debug_assertions)]
    fn ic_verify_set(&self, o: ObjId, ic: &Ic) {
        let key = ic.key.expect("a filled entry has its key");
        let ob = self.obj(o).expect("a live receiver");
        assert!(ob.exotic == 0 && ob.dict.is_none() && !(ob.is_array && key == PropertyKey::String(self.length)), "{ic:?}");
        if ic.kind == IC_STORE {
            assert!(matches!(self.own(ob, key), Some(Property::Data { writable: true, .. })), "store hit on {key:?}: {ic:?}");
            return;
        }
        assert!(ob.extensible && self.own(ob, key).is_none(), "add hit on {key:?}: {ic:?}");
        let mut cur = ob.proto;
        while let Some(c) = cur {
            let p = self.obj(c).expect("a live chain");
            assert!(p.exotic == 0, "an add hit through an exotic prototype");
            match self.own(p, key) {
                Some(Property::Data { writable: true, .. }) => break,
                Some(p) => panic!("add hit, but the chain has {p:?} for {key:?}"),
                None => cur = p.proto,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- inline caches (step A2) -----

    /// One read through a site entry, as the evaluator does it: the inline own hit, then `get_ic`. It must equal the plain `get`.
    fn read(h: &mut Heap, o: ObjId, key: PropertyKey, ic: &mut Ic) -> Get {
        let g = match h.ic_own(o, ic) {
            Some(v) => Get::Value(v),
            None => h.get_ic(o, key, ic).unwrap(),
        };
        let want = h.get(o, key).unwrap();
        let same = match (g, want) {
            (Get::Value(a), Get::Value(b)) => a.same_value(b),
            (Get::Accessor { getter: a }, Get::Accessor { getter: b }) => a == b,
            _ => false,
        };
        assert!(same, "{g:?} against {want:?}");
        g
    }
    fn rnum(h: &mut Heap, o: ObjId, key: PropertyKey, ic: &mut Ic) -> Option<f64> {
        match read(h, o, key, ic) {
            Get::Value(Value::Number(x)) => Some(x),
            Get::Value(Value::Undefined) => None,
            g => panic!("{g:?}"),
        }
    }
    /// The counters that changed since `before`: (chain hits, fills, refusals, epoch bumps).
    fn delta(h: &Heap, before: IcStats) -> (u64, u64, u64, u64) {
        let a = h.ic_stats();
        let fills = |s: IcStats| s.fill_own + s.fill_proto + s.fill_absent;
        (a.chain_hits - before.chain_hits, fills(a) - fills(before), a.refused - before.refused, a.epoch_bumps - before.epoch_bumps)
    }

    #[test]
    fn an_entry_is_24_bytes_and_the_empty_entry_never_hits() {
        if !cfg!(debug_assertions) {
            assert_eq!(size_of::<Ic>(), 24);
        }
        let mut h = Heap::default();
        let x = k(&mut h, "x");
        let d = h.new_dict_object(None).unwrap();
        let o = h.new_object(None).unwrap();
        assert!(h.ic_own(d, &Ic::EMPTY).is_none() && h.ic_own(o, &Ic::EMPTY).is_none());
        assert!(h.ic_hit(d, x, &Ic::EMPTY).is_none() && h.ic_hit(o, x, &Ic::EMPTY).is_none());
    }

    #[test]
    fn a_dictionary_receiver_is_cached_by_identity_and_position() {
        let mut h = Heap::default();
        let (x, y) = (k(&mut h, "x"), k(&mut h, "y"));
        let mk = |h: &mut Heap, a: f64| {
            let o = h.new_object(None).unwrap();
            setn(h, o, x, a);
            setn(h, o, y, a + 1.0);
            assert!(h.set_integrity(o, true).unwrap());
            o
        };
        let (a, b) = (mk(&mut h, 1.0), mk(&mut h, 10.0));
        assert!(in_dict(&h, a) && in_dict(&h, b), "freeze is dictionary mode");
        let (s0, mut ic) = (h.ic_stats(), Ic::EMPTY);
        assert_eq!(rnum(&mut h, a, y, &mut ic), Some(2.0));
        assert_eq!(h.ic_own(a, &ic).and_then(num), Some(2.0), "the inline test answers a frozen receiver");
        assert!(h.ic_own(b, &ic).is_none(), "another dictionary of the same keys is another receiver");
        assert_eq!(rnum(&mut h, b, y, &mut ic), Some(11.0));
        assert_eq!(delta(&h, s0), (0, 2, 0, 0));
        // A dictionary that loses the key misses, and the key at a new position refills.
        let d = h.new_dict_object(None).unwrap();
        setn(&mut h, d, x, 1.0);
        setn(&mut h, d, y, 2.0);
        let mut ic = Ic::EMPTY;
        assert_eq!(rnum(&mut h, d, x, &mut ic), Some(1.0));
        assert!(h.delete(d, x).unwrap());
        assert!(h.ic_own(d, &ic).is_none());
        assert_eq!(rnum(&mut h, d, x, &mut ic), None);
        setn(&mut h, d, x, 3.0);
        assert_eq!(rnum(&mut h, d, x, &mut ic), Some(3.0));
        assert_eq!(h.ic_own(d, &ic).and_then(num), Some(3.0));
        // An accessor of a dictionary receiver is answered by `get_ic`, not by the inline test.
        let g = h.new_object(None).unwrap();
        let acc = PropertyDescriptor { get: Some(Some(g)), set: Some(None), enumerable: Some(true), configurable: Some(false), ..Default::default() };
        assert!(h.define_own_property(d, y, &acc).unwrap());
        let mut ic = Ic::EMPTY;
        read(&mut h, d, y, &mut ic);
        assert!(h.ic_own(d, &ic).is_none());
        assert!(matches!(read(&mut h, d, y, &mut ic), Get::Accessor { getter: Some(f) } if f == g));
        // A key on the prototype of a dictionary receiver is the plain read, and fills nothing.
        let p = h.new_object(None).unwrap();
        let z = k(&mut h, "z");
        setn(&mut h, p, z, 5.0);
        let e = h.new_dict_object(Some(p)).unwrap();
        let (s0, mut ic) = (h.ic_stats(), Ic::EMPTY);
        assert_eq!(rnum(&mut h, e, z, &mut ic), Some(5.0));
        assert_eq!(rnum(&mut h, e, z, &mut ic), Some(5.0));
        assert_eq!(delta(&h, s0), (0, 0, 2, 0));
    }

    #[test]
    fn stub_entries_hit_and_keep_refusals() {
        let mut h = Heap::default();
        let (x, y) = (k(&mut h, "x"), k(&mut h, "y"));
        let p = h.new_object(None).unwrap();
        setn(&mut h, p, y, 9.0);
        let (a, b) = (h.new_object(Some(p)).unwrap(), h.new_object(Some(p)).unwrap());
        setn(&mut h, a, x, 1.0);
        setn(&mut h, b, x, 2.0);
        let stub = |h: &mut Heap, o, key| match h.get_stub(o, key).unwrap() {
            Get::Value(v) => num(v),
            g => panic!("{g:?}"),
        };
        let s0 = h.ic_stats();
        assert_eq!(stub(&mut h, a, x), Some(1.0));
        assert_eq!(stub(&mut h, b, x), Some(2.0), "b has the shape of a");
        assert_eq!(stub(&mut h, a, y), Some(9.0));
        assert_eq!(stub(&mut h, b, y), Some(9.0));
        assert_eq!(delta(&h, s0), (2, 2, 0, 0));
        // A new key on the prototype bumps the epoch: the entry misses and refills.
        let w = k(&mut h, "w");
        setn(&mut h, p, w, 0.0);
        setn(&mut h, p, y, 8.0);
        assert_eq!(stub(&mut h, a, y), Some(8.0));
        // A refused pair keeps its refusal: the second read is the plain read with no new fill attempt.
        let target = h.new_object(None).unwrap();
        let handler = h.new_object(None).unwrap();
        let proxy = h.new_proxy(target, handler).unwrap();
        let s0 = h.ic_stats();
        assert!(h.get_stub(proxy, x).is_err() && h.get_stub(proxy, x).is_err(), "the proxy answers, as for `get`");
        assert_eq!(delta(&h, s0), (0, 0, 1, 0));
        // An index key and a symbol key of a dictionary are the plain read.
        let d = h.new_dict_object(None).unwrap();
        let sym = PropertyKey::Symbol(h.new_symbol(None).unwrap());
        assert_eq!(h.set(d, sym, n(4.0)), Ok(Set::Done));
        assert_eq!(h.set(d, PropertyKey::Index(3), n(5.0)), Ok(Set::Done));
        assert_eq!(stub(&mut h, d, sym), Some(4.0));
        assert_eq!(stub(&mut h, d, PropertyKey::Index(3)), Some(5.0));
    }

    #[test]
    fn shape_reads_stay_right_when_answers_share_a_slot_of_found() {
        // 600 objects of 600 shapes, each read for a present and an absent key twice: more pairs than `found` has slots.
        let mut h = Heap::default();
        let keys: Vec<_> = (0..40).map(|i| k(&mut h, &format!("k{i}"))).collect();
        let mut objs = Vec::new();
        for i in 0..600usize {
            let o = h.new_object(None).unwrap();
            for j in 0..(i % 7 + 1) {
                setn(&mut h, o, keys[(i + j * 3) % 40], (i * 100 + j) as f64);
            }
            objs.push(o);
        }
        for _ in 0..2 {
            for (i, &o) in objs.iter().enumerate() {
                for j in 0..(i % 7 + 1) {
                    assert_eq!(getn(&mut h, o, keys[(i + j * 3) % 40]), (i * 100 + j) as f64);
                }
                let absent = keys[(i + 1) % 40];
                if !(0..(i % 7 + 1)).any(|j| (i + j * 3) % 40 == (i + 1) % 40) {
                    assert!(matches!(h.get(o, absent).unwrap(), Get::Value(Value::Undefined)));
                }
            }
        }
    }

    #[test]
    fn a_collection_keeps_the_keys_of_shared_shapes() {
        let mut h = Heap::default();
        let x = k(&mut h, "a_key_only_shapes_hold");
        let (a, b) = (h.new_object(None).unwrap(), h.new_object(None).unwrap());
        setn(&mut h, a, x, 1.0);
        setn(&mut h, b, x, 2.0);
        h.collect(&[Value::Object(a), Value::Object(b)]).unwrap();
        assert_eq!(keys_text(&mut h, a), ["a_key_only_shapes_hold"]);
        assert_eq!(keys_text(&mut h, b), ["a_key_only_shapes_hold"]);
        assert_eq!(getn(&mut h, b, x), 2.0);
    }

    #[test]
    fn a_shape_sweep_frees_dead_shapes_and_keeps_the_live_ones_and_their_parents() {
        let mut h = Heap::default();
        let p: Vec<_> = (0..4).map(|i| k(&mut h, &format!("p{i}"))).collect();
        let obj = |h: &mut Heap, keys: &[usize]| {
            let o = h.new_object(None).unwrap();
            keys.iter().for_each(|&i| setn(h, o, p[i], i as f64));
            o
        };
        // `keep` has no object for its parents. `branch` copies the start of its table. `dead` adds a key at the tip of that table.
        let keep = obj(&mut h, &[0, 1, 2]);
        let branch = obj(&mut h, &[0, 3]);
        obj(&mut h, &[0, 1, 2, 3]);
        for i in 0..SWEEP_MIN {
            let z = k(&mut h, &format!("z{i}"));
            setn(&mut h, keep, z, 0.0);
            assert!(h.delete(keep, z).unwrap());
        }
        let sweeps = h.shape_sweeps();
        h.collect(&[Value::Object(keep), Value::Object(branch)]).unwrap();
        assert_eq!(h.shape_sweeps(), sweeps + 1);
        // The sentinel, the 4 roots, p0, p0 p1, p0 p1 p2 and p0 p3.
        assert_eq!(h.shapes.len(), 9);
        assert_eq!(keys_text(&mut h, keep), ["p0", "p1", "p2"]);
        assert_eq!(keys_text(&mut h, branch), ["p0", "p3"]);
        assert_eq!((getn(&mut h, keep, p[2]), getn(&mut h, branch, p[3])), (2.0, 3.0));
        // The parents live: the same keys reach the same shape, with no new one.
        let again = obj(&mut h, &[0, 1, 2]);
        assert_eq!((shape_of(&h, again), h.shapes.len()), (shape_of(&h, keep), 9));
        // The dead shape and its table entry are gone: p3 after p0 p1 p2 makes a new shape at the tip of the cut table.
        setn(&mut h, again, p[3], 3.0);
        assert_eq!(h.shapes.len(), 10);
        assert_eq!(keys_text(&mut h, again), ["p0", "p1", "p2", "p3"]);
        assert_eq!(getn(&mut h, again, p[3]), 3.0);
        // No growth since the sweep: the next collect does not sweep.
        h.collect(&[Value::Object(keep)]).unwrap();
        assert_eq!(h.shape_sweeps(), sweeps + 1);
    }

    #[test]
    fn an_own_hit_needs_the_same_shape_and_another_shape_refills() {
        let mut h = Heap::default();
        let (x, y) = (k(&mut h, "x"), k(&mut h, "y"));
        let (a, b, c) = (h.new_object(None).unwrap(), h.new_object(None).unwrap(), h.new_object(None).unwrap());
        setn(&mut h, a, x, 1.0);
        setn(&mut h, b, x, 2.0);
        setn(&mut h, c, y, 0.0);
        setn(&mut h, c, x, 3.0);
        let mut ic = Ic::EMPTY;
        assert_eq!(rnum(&mut h, a, x, &mut ic), Some(1.0));
        assert_eq!(h.ic_own(b, &ic).and_then(num), Some(2.0), "b has the shape of a");
        assert!(h.ic_own(c, &ic).is_none(), "c has another shape");
        assert_eq!(rnum(&mut h, c, x, &mut ic), Some(3.0));
        assert_eq!(h.ic_own(c, &ic).and_then(num), Some(3.0));
        setn(&mut h, c, x, 4.0);
        assert_eq!(h.ic_own(c, &ic).and_then(num), Some(4.0), "a value change keeps the shape");
    }

    #[test]
    fn shadowing_on_the_receiver_a_middle_prototype_and_the_holder() {
        let mut h = Heap::default();
        let x = k(&mut h, "x");
        let holder = h.new_object(None).unwrap();
        let mid = h.new_object(Some(holder)).unwrap();
        let r = h.new_object(Some(mid)).unwrap();
        setn(&mut h, holder, x, 1.0);
        let mut ic = Ic::EMPTY;
        let s0 = h.ic_stats();
        assert_eq!(rnum(&mut h, r, x, &mut ic), Some(1.0));
        assert_eq!(rnum(&mut h, r, x, &mut ic), Some(1.0));
        assert_eq!(delta(&h, s0), (1, 1, 0, 0), "one fill, then a prototype hit");
        // A value change on the holder needs no invalidation.
        setn(&mut h, holder, x, 2.0);
        let s0 = h.ic_stats();
        assert_eq!(rnum(&mut h, r, x, &mut ic), Some(2.0));
        assert_eq!(delta(&h, s0), (1, 0, 0, 0));
        // The same key on the middle prototype: the epoch moves, the entry refills at depth 1.
        setn(&mut h, mid, x, 3.0);
        let s0 = h.ic_stats();
        assert_eq!(rnum(&mut h, r, x, &mut ic), Some(3.0));
        assert_eq!(rnum(&mut h, r, x, &mut ic), Some(3.0));
        assert_eq!(delta(&h, s0), (1, 1, 0, 0));
        assert_eq!(h.ic_stats().epoch_bumps, 1);
        // On the receiver: its shape changes.
        setn(&mut h, r, x, 4.0);
        assert_eq!(rnum(&mut h, r, x, &mut ic), Some(4.0));
        assert_eq!(h.ic_own(r, &ic).and_then(num), Some(4.0));
        // Delete on the holder (a flagged object): the epoch moves. Delete on the receiver: its shape goes back.
        let mut ic2 = Ic::EMPTY;
        let r2 = h.new_object(Some(holder)).unwrap();
        assert_eq!(rnum(&mut h, r2, x, &mut ic2), Some(2.0));
        assert!(h.delete(holder, x).unwrap());
        assert_eq!(rnum(&mut h, r2, x, &mut ic2), None);
        assert!(h.delete(r, x).unwrap());
        assert_eq!(rnum(&mut h, r, x, &mut ic), Some(3.0));
    }

    #[test]
    fn an_absent_key_hits_until_a_prototype_gets_it() {
        let mut h = Heap::default();
        let (x, y) = (k(&mut h, "x"), k(&mut h, "y"));
        let p = h.new_object(None).unwrap();
        let r = h.new_object(Some(p)).unwrap();
        let bare = h.new_object(None).unwrap();
        setn(&mut h, r, y, 0.0);
        setn(&mut h, bare, y, 0.0);
        let mut ic = Ic::EMPTY;
        assert_eq!(rnum(&mut h, r, x, &mut ic), None);
        let s0 = h.ic_stats();
        assert_eq!(rnum(&mut h, r, x, &mut ic), None);
        assert_eq!(delta(&h, s0), (1, 0, 0, 0));
        assert_eq!(rnum(&mut h, bare, x, &mut ic), None, "the same shape with no prototype misses and refills");
        assert_eq!(rnum(&mut h, r, x, &mut ic), None);
        setn(&mut h, p, x, 5.0);
        assert_eq!(rnum(&mut h, r, x, &mut ic), Some(5.0));
        // A chain with no prototype: the entry holds depth 0.
        let mut ic0 = Ic::EMPTY;
        assert_eq!(rnum(&mut h, bare, x, &mut ic0), None);
        let s0 = h.ic_stats();
        assert_eq!(rnum(&mut h, bare, x, &mut ic0), None);
        assert_eq!(delta(&h, s0), (1, 0, 0, 0));
    }

    #[test]
    fn prototype_replacement_and_two_prototypes_with_one_shape() {
        let mut h = Heap::default();
        let x = k(&mut h, "x");
        let (p1, p2, p3) = (h.new_object(None).unwrap(), h.new_object(None).unwrap(), h.new_object(None).unwrap());
        setn(&mut h, p1, x, 1.0);
        setn(&mut h, p2, x, 2.0);
        let mid = h.new_object(Some(p1)).unwrap();
        let (a, b) = (h.new_object(Some(mid)).unwrap(), h.new_object(Some(p2)).unwrap());
        let mut ic = Ic::EMPTY;
        assert_eq!(rnum(&mut h, a, x, &mut ic), Some(1.0));
        assert_eq!(rnum(&mut h, b, x, &mut ic), Some(2.0), "same shape, another prototype: a miss");
        assert_eq!(rnum(&mut h, a, x, &mut ic), Some(1.0));
        // The prototype of a flagged middle object changes.
        assert!(h.set_prototype_of(mid, Some(p2)).unwrap());
        assert_eq!(rnum(&mut h, a, x, &mut ic), Some(2.0));
        assert!(h.set_prototype_of(mid, Some(p3)).unwrap());
        assert_eq!(rnum(&mut h, a, x, &mut ic), None);
        // The prototype of the receiver changes: it goes to dictionary mode and is never cached.
        assert!(h.set_prototype_of(a, Some(p1)).unwrap());
        let s0 = h.ic_stats();
        assert_eq!(rnum(&mut h, a, x, &mut ic), Some(1.0));
        assert_eq!(rnum(&mut h, a, x, &mut ic), Some(1.0));
        assert_eq!(delta(&h, s0), (0, 0, 2, 0));
    }

    #[test]
    fn a_getter_turned_into_a_data_property_and_back() {
        let mut h = Heap::default();
        let x = k(&mut h, "x");
        let (g, g2) = (h.new_object(None).unwrap(), h.new_object(None).unwrap());
        let p = h.new_object(None).unwrap();
        let r = h.new_object(Some(p)).unwrap();
        let acc = |f| PropertyDescriptor { get: Some(Some(f)), configurable: Some(true), ..Default::default() };
        assert!(def(&mut h, p, x, acc(g)));
        let mut ic = Ic::EMPTY;
        assert!(matches!(read(&mut h, r, x, &mut ic), Get::Accessor { getter: Some(f) } if f == g));
        assert!(matches!(read(&mut h, r, x, &mut ic), Get::Accessor { getter: Some(f) } if f == g));
        // Another getter with the same attributes: read from the holder at hit time.
        assert!(def(&mut h, p, x, acc(g2)));
        assert!(matches!(read(&mut h, r, x, &mut ic), Get::Accessor { getter: Some(f) } if f == g2));
        assert!(def(&mut h, p, x, attrs(7.0, true, true, true)));
        assert_eq!(rnum(&mut h, r, x, &mut ic), Some(7.0));
        // An own accessor hits in `get_ic`, never in the inline own test.
        let o = h.new_object(None).unwrap();
        assert!(def(&mut h, o, x, acc(g)));
        let mut ic = Ic::EMPTY;
        read(&mut h, o, x, &mut ic);
        assert!(h.ic_own(o, &ic).is_none());
        assert!(matches!(read(&mut h, o, x, &mut ic), Get::Accessor { getter: Some(f) } if f == g));
    }

    #[test]
    fn proxies_and_numeric_keys_of_views_are_never_cached() {
        let mut h = Heap::default();
        let (x, y) = (k(&mut h, "x"), k(&mut h, "y"));
        let target = h.new_object(None).unwrap();
        let handler = h.new_object(None).unwrap();
        let proxy = h.new_proxy(target, handler).unwrap();
        let r = h.new_object(Some(proxy)).unwrap();
        let mut ic = Ic::EMPTY;
        let s0 = h.ic_stats();
        assert_eq!(h.get_ic(r, x, &mut ic).map(|_| ()), h.get(r, x).map(|_| ()), "the proxy answers, as for `get`");
        assert!(h.get(r, x).is_err());
        assert_eq!(delta(&h, s0), (0, 0, 1, 0));
        // A view is cached for a named key, not for a canonical numeric string, which its own part answers.
        let proto = h.new_object(None).unwrap();
        setn(&mut h, proto, x, 7.0);
        let buf = h.new_buffer(None, 8).unwrap();
        let view = h.new_view(Some(proto), buf, 0, 8).unwrap();
        let (s0, mut ic) = (h.ic_stats(), Ic::EMPTY);
        assert_eq!(rnum(&mut h, view, x, &mut ic), Some(7.0));
        assert_eq!(rnum(&mut h, view, x, &mut ic), Some(7.0));
        assert_eq!(delta(&h, s0), (1, 1, 0, 0));
        let half = k(&mut h, "1.5");
        let (s0, mut ic) = (h.ic_stats(), Ic::EMPTY);
        setn(&mut h, proto, half, 3.0);
        assert_eq!(rnum(&mut h, view, half, &mut ic), None);
        assert_eq!(rnum(&mut h, view, half, &mut ic), None);
        assert_eq!(delta(&h, s0), (0, 0, 2, 1), "the new key of the flagged prototype bumps the epoch");
        // A receiver in dictionary mode.
        let d = h.new_dict_object(None).unwrap();
        setn(&mut h, d, x, 1.0);
        setn(&mut h, d, y, 2.0);
        assert!(h.delete(d, x).unwrap());
        assert!(in_dict(&h, d));
        let mut ic = Ic::EMPTY;
        assert_eq!(rnum(&mut h, d, y, &mut ic), Some(2.0));
        assert_eq!(rnum(&mut h, d, y, &mut ic), Some(2.0));
        // A holder in dictionary mode is read at its entry position: a value write shows, a delete bumps the epoch.
        let r2 = h.new_object(Some(d)).unwrap();
        let mut ic = Ic::EMPTY;
        let s0 = h.ic_stats();
        assert_eq!(rnum(&mut h, r2, y, &mut ic), Some(2.0));
        assert_eq!(rnum(&mut h, r2, y, &mut ic), Some(2.0));
        assert_eq!(delta(&h, s0), (1, 1, 0, 0));
        setn(&mut h, d, y, 5.0);
        assert_eq!(rnum(&mut h, r2, y, &mut ic), Some(5.0));
        assert!(h.delete(d, y).unwrap());
        assert_eq!(rnum(&mut h, r2, y, &mut ic), None);
        assert_eq!(delta(&h, s0), (2, 2, 0, 1));
        // An index key (a site has one key, so it has its own entry).
        let (s0, mut ic) = (h.ic_stats(), Ic::EMPTY);
        let i = PropertyKey::Index(0);
        assert_eq!(h.define_own_property(r2, i, &PropertyDescriptor::data(n(9.0))), Ok(true));
        assert_eq!(rnum(&mut h, r2, i, &mut ic), Some(9.0));
        assert_eq!(delta(&h, s0), (0, 0, 1, 0));
    }

    #[test]
    fn a_collect_that_frees_the_holder_and_reuses_its_slot() {
        let mut h = Heap::default();
        let x = k(&mut h, "x");
        let p = h.new_object(None).unwrap();
        setn(&mut h, p, x, 1.0);
        let r = h.new_object(Some(p)).unwrap();
        let mut ic = Ic::EMPTY;
        assert_eq!(rnum(&mut h, r, x, &mut ic), Some(1.0));
        h.collect(&[Value::String(match x { PropertyKey::String(s) => s, _ => unreachable!() })]).unwrap();
        assert!(h.obj(p).is_err() && h.obj(r).is_err());
        assert!(matches!(h.get_ic(r, x, &mut ic), Err(HeapError::Stale)), "a dead receiver is an error, as for `get`");
        // New objects in the freed slots: the same shapes, but new generations.
        let q = h.new_object(None).unwrap();
        setn(&mut h, q, x, 2.0);
        let r2 = h.new_object(Some(q)).unwrap();
        assert_eq!(rnum(&mut h, r2, x, &mut ic), Some(2.0));
    }

    #[test]
    fn polymorphic_sites_with_2_and_5_shapes_read_the_right_values() {
        let mut h = Heap::default();
        let ks: Vec<PropertyKey> = (0..5).map(|i| k(&mut h, &format!("k{i}"))).collect();
        let x = k(&mut h, "x");
        let p = h.new_object(None).unwrap();
        for n_shapes in [2, 5] {
            let objs: Vec<ObjId> = (0..n_shapes)
                .map(|i| {
                    let o = h.new_object(Some(p)).unwrap();
                    setn(&mut h, o, ks[i], 0.0);
                    setn(&mut h, o, x, i as f64);
                    o
                })
                .collect();
            let mut ic = Ic::EMPTY;
            for round in 0..20 {
                for (i, &o) in objs.iter().enumerate() {
                    assert_eq!(rnum(&mut h, o, x, &mut ic), Some(i as f64), "round {round}");
                }
            }
        }
    }

    #[test]
    fn a_saturated_epoch_fills_only_own_entries() {
        let mut h = Heap::default();
        let x = k(&mut h, "x");
        let p = h.new_object(None).unwrap();
        setn(&mut h, p, x, 1.0);
        let r = h.new_object(Some(p)).unwrap();
        let mut ic = Ic::EMPTY;
        assert_eq!(rnum(&mut h, r, x, &mut ic), Some(1.0));
        h.proto_epoch = u32::MAX - 1;
        let u = k(&mut h, "unused");
        setn(&mut h, p, u, 0.0);
        assert_eq!(h.proto_epoch, u32::MAX);
        let s0 = h.ic_stats();
        assert_eq!(rnum(&mut h, r, x, &mut ic), Some(1.0));
        assert_eq!(rnum(&mut h, r, x, &mut ic), Some(1.0));
        assert_eq!(delta(&h, s0), (0, 0, 2, 0));
        setn(&mut h, p, x, 3.0);
        assert_eq!(h.proto_epoch, u32::MAX, "it does not wrap");
        let o = h.new_object(None).unwrap();
        setn(&mut h, o, x, 4.0);
        let mut ic = Ic::EMPTY;
        assert_eq!(rnum(&mut h, o, x, &mut ic), Some(4.0));
        assert_eq!(h.ic_own(o, &ic).and_then(num), Some(4.0));
    }

    // ----- shapes (step A1) -----

    fn k(h: &mut Heap, s: &str) -> PropertyKey {
        h.key_from_str(s).unwrap()
    }
    fn n(v: f64) -> Value {
        Value::Number(v)
    }
    fn num(v: Value) -> Option<f64> {
        if let Value::Number(x) = v { Some(x) } else { None }
    }
    fn setn(h: &mut Heap, o: ObjId, key: PropertyKey, v: f64) {
        assert_eq!(h.set(o, key, n(v)), Ok(Set::Done));
    }
    fn getn(h: &mut Heap, o: ObjId, key: PropertyKey) -> f64 {
        match h.get(o, key).unwrap() {
            Get::Value(Value::Number(x)) => x,
            v => panic!("{v:?}"),
        }
    }
    fn shape_of(h: &Heap, o: ObjId) -> u32 {
        h.obj(o).unwrap().shape
    }
    fn in_dict(h: &Heap, o: ObjId) -> bool {
        h.obj(o).unwrap().dict.is_some()
    }
    fn keys_text(h: &mut Heap, o: ObjId) -> Vec<String> {
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
    fn def(h: &mut Heap, o: ObjId, key: PropertyKey, d: PropertyDescriptor) -> bool {
        h.define_own_property(o, key, &d).unwrap()
    }
    fn attrs(v: f64, w: bool, e: bool, c: bool) -> PropertyDescriptor {
        PropertyDescriptor { value: Some(n(v)), writable: Some(w), enumerable: Some(e), configurable: Some(c), ..Default::default() }
    }

    #[test]
    fn a_literal_shape_makes_the_same_object_and_a_function_starts_in_the_preset_shape() {
        let mut h = Heap::default();
        let (a, b) = (k(&mut h, "a"), k(&mut h, "b"));
        let o = h.new_object(None).unwrap();
        def(&mut h, o, a, attrs(1.0, true, true, true));
        def(&mut h, o, b, attrs(2.0, true, true, true));
        let s = h.literal_shape(o, 2).unwrap();
        assert_eq!(h.literal_shape(o, 3), None);
        let p = h.new_object_with_shape(None, s, vec![n(5.0), n(6.0)]).unwrap();
        assert_eq!((shape_of(&h, p), getn(&mut h, p, a), getn(&mut h, p, b)), (s, 5.0, 6.0));
        assert_eq!(keys_text(&mut h, p), ["a", "b"]);
        setn(&mut h, p, a, 7.0);
        assert_eq!((getn(&mut h, p, a), getn(&mut h, o, a)), (7.0, 1.0));
        assert!(h.new_object_with_shape(None, s, vec![n(5.0)]).is_err());
        // A read-only key, a key on an array, or a dictionary object is not a literal shape.
        let q = h.new_object(None).unwrap();
        def(&mut h, q, a, attrs(1.0, false, true, true));
        assert_eq!(h.literal_shape(q, 1), None);
        let arr = h.new_array(None).unwrap();
        def(&mut h, arr, a, attrs(1.0, true, true, true));
        assert_eq!(h.literal_shape(arr, 1), None);
        h.prevent_extensions(o).unwrap();
        assert_eq!(h.literal_shape(o, 2), None);

        let op = h.new_object(None).unwrap();
        let (kl, kn, kp, kc) = (k(&mut h, "length"), k(&mut h, "name"), k(&mut h, "prototype"), k(&mut h, "constructor"));
        h.set_lazy_prototype(op, kp, kc).unwrap();
        let name = h.intern_str("f").unwrap();
        let f = h.new_function(None, (kl, 2.0), (kn, name), true, 3).unwrap();
        let g = h.new_function(None, (kl, 0.0), (kn, name), true, 3).unwrap();
        assert_eq!(shape_of(&h, f), shape_of(&h, g));
        assert_eq!(keys_text(&mut h, f), ["length", "name", "prototype"]);
        assert_eq!(getn(&mut h, f, kl), 2.0);
        let Get::Value(Value::Object(pr)) = h.get(f, kp).unwrap() else { panic!("prototype") };
        assert!(matches!(h.get(pr, kc).unwrap(), Get::Value(Value::Object(c)) if c == f));
        let m = h.new_function(None, (kl, 0.0), (kn, name), false, 2).unwrap();
        assert_eq!(keys_text(&mut h, m), ["length", "name"]);
    }

    #[test]
    fn objects_that_add_the_same_keys_share_one_shape_and_hold_only_values() {
        let mut h = Heap::default();
        let (a, b) = (k(&mut h, "a"), k(&mut h, "b"));
        let (x, y) = (h.new_object(None).unwrap(), h.new_object(None).unwrap());
        assert_eq!(shape_of(&h, x), ROOT);
        for o in [x, y] {
            setn(&mut h, o, a, 1.0);
            setn(&mut h, o, b, 2.0);
        }
        assert_eq!(shape_of(&h, x), shape_of(&h, y));
        assert!(!in_dict(&h, x));
        assert_eq!(h.obj(x).unwrap().slots.len(), 2);
        let z = h.new_object(None).unwrap();
        setn(&mut h, z, b, 2.0);
        setn(&mut h, z, a, 1.0);
        assert_ne!(shape_of(&h, z), shape_of(&h, x), "the order of keys makes the shape");
        assert_eq!(keys_text(&mut h, z), ["b", "a"]);
        assert_eq!(size_of::<Object>(), 80);
    }

    #[test]
    fn key_order_is_integers_then_strings_then_symbols() {
        let mut h = Heap::default();
        let o = h.new_object(None).unwrap();
        let s = h.new_symbol(None).unwrap();
        let (z, b, a) = (k(&mut h, "z"), k(&mut h, "b"), k(&mut h, "a"));
        setn(&mut h, o, PropertyKey::Symbol(s), 0.0);
        setn(&mut h, o, z, 0.0);
        setn(&mut h, o, PropertyKey::Index(10), 0.0);
        setn(&mut h, o, b, 0.0);
        setn(&mut h, o, PropertyKey::Index(2), 0.0);
        setn(&mut h, o, a, 0.0);
        assert_eq!(keys_text(&mut h, o), ["2", "10", "z", "b", "a", "@"]);
    }

    #[test]
    fn attributes_are_in_the_shape_and_a_different_attribute_makes_another_shape() {
        let mut h = Heap::default();
        let key = k(&mut h, "p");
        let (x, y) = (h.new_object(None).unwrap(), h.new_object(None).unwrap());
        assert!(def(&mut h, x, key, attrs(1.0, true, false, true)));
        assert!(def(&mut h, y, key, attrs(1.0, false, true, true)));
        assert_ne!(shape_of(&h, x), shape_of(&h, y));
        assert!(matches!(h.get_own_property(x, key).unwrap(), Some(Property::Data { writable: true, enumerable: false, configurable: true, .. })));
        assert!(matches!(h.get_own_property(y, key).unwrap(), Some(Property::Data { writable: false, enumerable: true, configurable: true, .. })));
        assert_eq!(h.set(y, key, n(2.0)), Ok(Set::Rejected));
        // A value change keeps the shape.
        let before = shape_of(&h, x);
        setn(&mut h, x, key, 9.0);
        assert_eq!(shape_of(&h, x), before);
        assert_eq!(getn(&mut h, x, key), 9.0);
    }

    #[test]
    fn an_accessor_uses_two_slots() {
        let mut h = Heap::default();
        let (key, after) = (k(&mut h, "acc"), k(&mut h, "after"));
        let (g, s) = (h.new_object(None).unwrap(), h.new_object(None).unwrap());
        let o = h.new_object(None).unwrap();
        let d = PropertyDescriptor { get: Some(Some(g)), set: Some(Some(s)), enumerable: Some(true), configurable: Some(true), ..Default::default() };
        assert!(def(&mut h, o, key, d));
        setn(&mut h, o, after, 5.0);
        assert_eq!(h.obj(o).unwrap().slots.len(), 3);
        assert!(!in_dict(&h, o));
        assert!(matches!(h.get_own_property(o, key).unwrap(), Some(Property::Accessor { get: Some(a), set: Some(b), .. }) if a == g && b == s));
        assert_eq!(getn(&mut h, o, after), 5.0);
        assert!(matches!(h.get(o, key), Ok(Get::Accessor { getter: Some(a) }) if a == g));
        assert_eq!(h.set(o, key, n(1.0)), Ok(Set::Setter(s)));
        // A getter with no setter keeps the shape and stores `undefined` in the second slot.
        let p = h.new_object(None).unwrap();
        let only_get = PropertyDescriptor { get: Some(Some(g)), enumerable: Some(true), configurable: Some(true), ..Default::default() };
        assert!(def(&mut h, p, key, only_get));
        assert_eq!(h.set(p, key, n(1.0)), Ok(Set::Rejected));
        assert!(matches!(h.get_own_property(p, key).unwrap(), Some(Property::Accessor { get: Some(_), set: None, .. })));
        // The two objects differ only in what the slots hold, so they share the shape.
        assert_eq!(h.obj(p).unwrap().slots.len(), 2);
        // Changing the getter of an accessor keeps the shape.
        let t = shape_of(&h, o);
        assert!(def(&mut h, o, key, PropertyDescriptor { get: Some(Some(s)), ..Default::default() }));
        assert_eq!(shape_of(&h, o), t);
        assert!(matches!(h.get_own_property(o, key).unwrap(), Some(Property::Accessor { get: Some(a), .. }) if a == s));
    }

    #[test]
    fn deleting_the_last_property_added_keeps_shape_mode_and_re_adding_reuses_the_shape() {
        let mut h = Heap::default();
        let (a, b) = (k(&mut h, "a"), k(&mut h, "b"));
        let o = h.new_object(None).unwrap();
        setn(&mut h, o, a, 1.0);
        let one = shape_of(&h, o);
        setn(&mut h, o, b, 2.0);
        let two = shape_of(&h, o);
        assert!(h.delete(o, b).unwrap());
        assert!(!in_dict(&h, o));
        assert_eq!(shape_of(&h, o), one);
        assert_eq!(h.obj(o).unwrap().slots.len(), 1);
        setn(&mut h, o, b, 3.0);
        assert_eq!(shape_of(&h, o), two);
        assert_eq!(getn(&mut h, o, b), 3.0);
        // Deleting the first of two: the object takes the shape of an object with only `b`, and a re-add goes last.
        assert!(h.delete(o, a).unwrap());
        let q = h.new_object(None).unwrap();
        setn(&mut h, q, b, 0.0);
        assert!(!in_dict(&h, o));
        assert_eq!(shape_of(&h, o), shape_of(&h, q));
        assert_eq!((keys_text(&mut h, o), getn(&mut h, o, b)), (vec!["b".to_string()], 3.0));
        setn(&mut h, o, a, 4.0);
        assert_eq!(keys_text(&mut h, o), ["b", "a"]);
        assert_eq!((getn(&mut h, o, a), getn(&mut h, o, b)), (4.0, 3.0));
        // Deleting a missing key, or the only property, is fine and a non-configurable property stays.
        let p = h.new_object(None).unwrap();
        assert!(h.delete(p, a).unwrap());
        assert!(def(&mut h, p, a, attrs(1.0, true, true, false)));
        assert!(!h.delete(p, a).unwrap());
        assert!(!in_dict(&h, p));
    }

    #[test]
    fn deleting_a_middle_key_moves_the_later_values_down_and_keeps_accessors() {
        let mut h = Heap::default();
        let (a, g, c, d) = (k(&mut h, "a"), k(&mut h, "g"), k(&mut h, "c"), k(&mut h, "d"));
        let getter = h.new_object(None).unwrap();
        let mk = |h: &mut Heap| {
            let o = h.new_object(None).unwrap();
            setn(h, o, a, 1.0);
            assert!(def(h, o, g, PropertyDescriptor { get: Some(Some(getter)), enumerable: Some(true), configurable: Some(true), ..Default::default() }));
            setn(h, o, c, 3.0);
            setn(h, o, d, 4.0);
            o
        };
        // The accessor (two slots) goes: `c` and `d` move down by two.
        let o = mk(&mut h);
        assert!(h.delete(o, g).unwrap());
        assert!(!in_dict(&h, o));
        assert_eq!(h.obj(o).unwrap().slots.len(), 3);
        assert_eq!(keys_text(&mut h, o), ["a", "c", "d"]);
        assert_eq!((getn(&mut h, o, a), getn(&mut h, o, c), getn(&mut h, o, d)), (1.0, 3.0, 4.0));
        // A data key before the accessor goes: the accessor moves down by one and keeps its getter.
        let p = mk(&mut h);
        assert!(h.delete(p, a).unwrap());
        assert_eq!(keys_text(&mut h, p), ["g", "c", "d"]);
        assert!(matches!(h.get_own_property(p, g).unwrap(), Some(Property::Accessor { get: Some(x), .. }) if x == getter));
        assert_eq!((getn(&mut h, p, c), getn(&mut h, p, d)), (3.0, 4.0));
        // Two objects that drop the same key share the new shape.
        let q = mk(&mut h);
        assert!(h.delete(q, a).unwrap());
        assert_eq!(shape_of(&h, p), shape_of(&h, q));
    }

    #[test]
    fn every_dictionary_trigger() {
        let mut h = Heap::default();
        let (a, b, c) = (k(&mut h, "a"), k(&mut h, "b"), k(&mut h, "c"));
        let mk = |h: &mut Heap| {
            let o = h.new_object(None).unwrap();
            setn(h, o, a, 1.0);
            setn(h, o, b, 2.0);
            o
        };
        // 1. delete a property that is not the last, of an object with more than SCAN_MAX properties
        let o = mk(&mut h);
        for i in 0..SCAN_MAX {
            let key = k(&mut h, &format!("q{i}"));
            setn(&mut h, o, key, 0.0);
        }
        h.delete(o, a).unwrap();
        assert!(in_dict(&h, o));
        assert_eq!(getn(&mut h, o, b), 2.0);
        // 2. change an attribute of an existing property
        let o = mk(&mut h);
        assert!(def(&mut h, o, a, PropertyDescriptor { enumerable: Some(false), ..Default::default() }));
        assert!(in_dict(&h, o));
        // 2b. data to accessor
        let o = mk(&mut h);
        assert!(def(&mut h, o, a, PropertyDescriptor { get: Some(None), ..Default::default() }));
        assert!(in_dict(&h, o));
        // 3. setPrototypeOf (to another prototype; to the same one is a no-op)
        let o = mk(&mut h);
        let p = h.new_object(None).unwrap();
        assert!(!h.set_prototype_of(o, None).map(|_| in_dict(&h, o)).unwrap());
        assert!(h.set_prototype_of(o, Some(p)).unwrap());
        assert!(in_dict(&h, o));
        assert_eq!(getn(&mut h, o, a), 1.0);
        // 4. freeze and seal
        for frozen in [true, false] {
            let o = mk(&mut h);
            assert!(h.set_integrity(o, frozen).unwrap());
            assert!(in_dict(&h, o));
            assert!(h.test_integrity(o, frozen).unwrap());
        }
        let empty = h.new_object(None).unwrap();
        h.set_integrity(empty, true).unwrap();
        assert!(!in_dict(&h, empty), "an empty object has no property to change");
        // 5. an index key that is not dense
        let o = mk(&mut h);
        assert!(def(&mut h, o, PropertyKey::Index(3), attrs(1.0, false, true, true)));
        assert!(in_dict(&h, o));
        // a dense index goes to `elems` and the object stays in shape mode
        let o = mk(&mut h);
        setn(&mut h, o, PropertyKey::Index(0), 1.0);
        assert!(!in_dict(&h, o));
        // 6. more than MAX_PROPS properties: the 129th goes to a dictionary and nothing is lost
        let o = h.new_object(None).unwrap();
        let keys: Vec<_> = (0..MAX_PROPS + 3).map(|i| k(&mut h, &format!("p{i}"))).collect();
        for (i, &key) in keys.iter().enumerate() {
            setn(&mut h, o, key, i as f64);
            assert_eq!(in_dict(&h, o), i >= MAX_PROPS, "after {} properties", i + 1);
        }
        for (i, &key) in keys.iter().enumerate() {
            assert_eq!(getn(&mut h, o, key), i as f64);
        }
        assert_eq!(h.own_keys(o).unwrap(), keys);
        let _ = c;
    }

    #[test]
    fn exotic_objects_have_their_own_root_and_are_never_in_an_ordinary_shape() {
        let mut h = Heap::default();
        let target = h.new_object(None).unwrap();
        let handler = h.new_object(None).unwrap();
        let px = h.new_proxy(target, handler).unwrap();
        let named = h.new_named(None).unwrap();
        let text = h.intern_str("ab").unwrap();
        let sw = h.new_primitive(None, Value::String(text)).unwrap();
        let w = h.new_primitive(None, Value::Number(1.0)).unwrap();
        let buf = h.new_buffer(None, 4).unwrap();
        let idx = h.new_indexed(None).unwrap();
        for o in [px, named, sw] {
            assert_eq!(shape_of(&h, o), ROOT_EXOTIC);
        }
        for o in [w, buf, idx] {
            assert_eq!(shape_of(&h, o), ROOT_PLAIN);
        }
        // a property of one grows the exotic subtree, away from the ordinary roots
        let key = k(&mut h, "own");
        let sym = PropertyKey::Symbol(h.new_symbol(None).unwrap());
        setn(&mut h, buf, key, 1.0);
        setn(&mut h, w, sym, 1.0);
        let s = shape_of(&h, buf);
        let mut up = s;
        while h.shapes.parent(up) != 0 {
            up = h.shapes.parent(up);
        }
        assert_eq!(up, ROOT_PLAIN);
        let o = h.new_object(None).unwrap();
        setn(&mut h, o, key, 1.0);
        assert_ne!(shape_of(&h, o), s);
        assert_eq!(getn(&mut h, buf, key), 1.0);
    }

    #[test]
    fn arrays_keep_length_in_slot_zero() {
        let mut h = Heap::default();
        let a = h.new_array(None).unwrap();
        let len = k(&mut h, "length");
        assert_eq!(shape_of(&h, a), ROOT_ARRAY);
        assert_eq!(h.obj(a).unwrap().slots.len(), 1);
        for i in 0..5 {
            setn(&mut h, a, PropertyKey::Index(i), i as f64);
        }
        assert_eq!(num(h.obj(a).unwrap().slots[0]), Some(5.0));
        assert_eq!(h.array_length(a), Ok(Some(5)));
        assert_eq!(getn(&mut h, a, len), 5.0);
        let extra = k(&mut h, "extra");
        setn(&mut h, a, extra, 1.0);
        assert!(!in_dict(&h, a));
        assert_eq!(keys_text(&mut h, a), ["0", "1", "2", "3", "4", "length", "extra"]);
        // a shorter length keeps shape mode; a read-only length is an attribute change
        assert!(def(&mut h, a, len, PropertyDescriptor { value: Some(n(2.0)), ..Default::default() }));
        assert!(!in_dict(&h, a));
        assert_eq!(h.array_length(a), Ok(Some(2)));
        assert!(def(&mut h, a, len, PropertyDescriptor { writable: Some(false), ..Default::default() }));
        assert!(in_dict(&h, a));
        assert_eq!(h.set(a, PropertyKey::Index(7), n(1.0)), Ok(Set::Rejected));
        assert_eq!(h.array_length(a), Ok(Some(2)));
        let b = h.new_array(None).unwrap();
        assert_eq!(shape_of(&h, b), ROOT_ARRAY, "a new array has the root again");
    }

    #[test]
    fn a_table_longer_than_eight_keys_is_searched_by_hash_and_a_branch_copies_its_part() {
        let mut h = Heap::default();
        let keys: Vec<_> = (0..20).map(|i| k(&mut h, &format!("k{i}"))).collect();
        let (x, y) = (h.new_object(None).unwrap(), h.new_object(None).unwrap());
        for (i, &key) in keys.iter().enumerate() {
            setn(&mut h, x, key, i as f64);
        }
        // y follows x for 12 keys, then adds a different key at 13: the table is copied for that branch
        for (i, &key) in keys[..12].iter().enumerate() {
            setn(&mut h, y, key, i as f64);
        }
        let other = k(&mut h, "other");
        setn(&mut h, y, other, 99.0);
        let tables = h.shapes.tables.len();
        assert!(tables >= 3, "the branch has its own table: {tables}");
        for (i, &key) in keys.iter().enumerate() {
            assert_eq!(getn(&mut h, x, key), i as f64);
        }
        assert_eq!(getn(&mut h, y, other), 99.0);
        assert_eq!(h.get(y, keys[15]).map(|g| matches!(g, Get::Value(Value::Undefined))), Ok(true));
        assert_eq!(h.get(x, other).map(|g| matches!(g, Get::Value(Value::Undefined))), Ok(true));
        assert_eq!(h.own_keys(x).unwrap(), keys);
        assert!(h.shapes.pos(shape_of(&h, x), keys[19]).is_some());
        assert!(h.shapes.pos(shape_of(&h, y), keys[19]).is_none());
    }

    #[test]
    fn the_shape_cap_sends_new_shapes_to_dictionary_mode() {
        let mut h = Heap::default();
        let mut made = 0;
        // One object for each distinct key makes one shape for each key.
        let mut last = None;
        while h.shapes.len() < SHAPE_CAP {
            let o = h.new_object(None).unwrap();
            let key = k(&mut h, &format!("s{made}"));
            setn(&mut h, o, key, 1.0);
            made += 1;
            last = Some((o, key));
        }
        let (o, key) = last.unwrap();
        assert!(!in_dict(&h, o), "the shape that filled the table was made");
        let p = h.new_object(None).unwrap();
        let fresh = k(&mut h, "no-room");
        setn(&mut h, p, fresh, 7.0);
        assert!(in_dict(&h, p));
        assert_eq!(getn(&mut h, p, fresh), 7.0);
        // Known shapes still work, and the first object is untouched.
        let q = h.new_object(None).unwrap();
        setn(&mut h, q, key, 2.0);
        assert!(!in_dict(&h, q));
        assert_eq!(h.shapes.len(), SHAPE_CAP);
    }

    #[test]
    fn collect_traces_slots_and_the_keys_of_the_shape() {
        let mut h = Heap::default();
        let o = h.new_object(None).unwrap();
        let inner = h.new_object(None).unwrap();
        let s = h.intern_str("kept-string").unwrap();
        let key = PropertyKey::String(h.intern_str("kept-key").unwrap());
        let (g, st) = (h.new_object(None).unwrap(), h.new_object(None).unwrap());
        let acc = k(&mut h, "acc");
        let sym = h.new_symbol(None).unwrap();
        setn(&mut h, o, key, 0.0);
        let (ko, ks) = (k(&mut h, "obj"), k(&mut h, "str"));
        assert_eq!(h.set(o, ko, Value::Object(inner)), Ok(Set::Done));
        assert_eq!(h.set(o, ks, Value::String(s)), Ok(Set::Done));
        assert_eq!(h.set(o, PropertyKey::Symbol(sym), n(1.0)), Ok(Set::Done));
        assert!(def(&mut h, o, acc, PropertyDescriptor { get: Some(Some(g)), set: Some(Some(st)), enumerable: Some(true), configurable: Some(true), ..Default::default() }));
        let dead = h.new_object(None).unwrap();
        let (live_before, c) = (h.stats().live_objects, h.collect(&[Value::Object(o)]).unwrap());
        assert_eq!(c.objects, 1, "only `dead` goes: {live_before}");
        assert_eq!(h.obj(dead).err(), Some(HeapError::Stale));
        assert!(h.obj(inner).is_ok() && h.obj(g).is_ok() && h.obj(st).is_ok());
        assert_eq!(h.to_string_lossy(s).unwrap(), "kept-string");
        assert!(h.symbol_description(sym).is_ok());
        assert_eq!(h.to_string_lossy(match key { PropertyKey::String(s) => s, _ => unreachable!() }).unwrap(), "kept-key");
        let mut out = Vec::new();
        h.stored_edges(o, &mut out).unwrap();
        assert!(out.iter().any(|v| matches!(v, Value::Object(x) if *x == inner)));
        assert!(out.iter().any(|v| matches!(v, Value::Object(x) if *x == g)));
        // a dictionary object traces the same way
        let d = h.new_dict_object(None).unwrap();
        let (a, b) = (k(&mut h, "da"), k(&mut h, "db"));
        h.set(d, a, Value::Object(inner)).unwrap();
        setn(&mut h, d, b, 1.0);
        h.delete(d, a).unwrap();
        let fresh = h.new_object(None).unwrap();
        h.set(d, a, Value::Object(fresh)).unwrap();
        assert!(in_dict(&h, d));
        let live = h.stats().live_objects;
        let c = h.collect(&[Value::Object(o), Value::Object(d)]).unwrap();
        assert_eq!(h.stats().live_objects, live - c.objects);
        assert_eq!(c.objects, 0);
    }

    #[test]
    fn footprint_counts_slots_and_dictionary_entries_apart() {
        let mut h = Heap::default();
        let base = h.footprint();
        let (a, b, c) = (k(&mut h, "fa"), k(&mut h, "fb"), k(&mut h, "fc"));
        let o = h.new_object(None).unwrap();
        h.reserve_props(o, 3).unwrap();
        for key in [a, b, c] {
            setn(&mut h, o, key, 1.0);
        }
        let f = h.footprint();
        assert_eq!((f.slots_used - base.slots_used, f.slots_cap - base.slots_cap), (3 * SLOT_BYTES, 3 * SLOT_BYTES));
        assert_eq!(f.entries_used, base.entries_used);
        assert!(f.shapes >= 4 && f.shape_bytes > 0);
        // A new prototype sends `o` to a dictionary, where a delete leaves a hole.
        let pr = h.new_object(None).unwrap();
        h.set_prototype_of(o, Some(pr)).unwrap();
        h.delete(o, a).unwrap();
        let g = h.footprint();
        assert_eq!(g.dict_objects, base.dict_objects + 1);
        assert_eq!(g.slots_used, base.slots_used);
        assert_eq!(g.entries_used - base.entries_used, 3 * ENTRY_BYTES, "the deleted first entry leaves a hole, as before");
        // accounted bytes follow the real object: 16 bytes for each slot, not 48
        let before = h.used_bytes();
        let p = h.new_object(None).unwrap();
        let empty = h.used_bytes() - before;
        for key in [a, b, c] {
            setn(&mut h, p, key, 1.0);
        }
        assert_eq!(h.used_bytes() - before - empty, 3 * SLOT_BYTES);
    }

    #[test]
    fn position_hints_work_on_shape_slots_and_on_dictionary_objects() {
        let mut h = Heap::default();
        let (a, b, c) = (k(&mut h, "ha"), k(&mut h, "hb"), k(&mut h, "hc"));
        let o = h.new_object(None).unwrap();
        for (i, key) in [a, b, c].into_iter().enumerate() {
            setn(&mut h, o, key, i as f64 + 10.0);
        }
        let pos = h.own_pos(o, b).unwrap();
        assert_eq!(h.own_data_at(o, pos, b).and_then(num), Some(11.0));
        assert!(h.own_data_at(o, pos, a).is_none(), "another key at that position");
        assert!(h.set_own_data_at(o, pos, b, n(50.0)));
        assert_eq!(getn(&mut h, o, b), 50.0);
        let missing = k(&mut h, "missing");
        assert!(h.own_pos(o, missing).is_none());
        // a read-only property and an accessor refuse the write hint
        let ro = k(&mut h, "ro");
        assert!(def(&mut h, o, ro, attrs(1.0, false, true, true)));
        let rp = h.own_pos(o, ro).unwrap();
        assert!(!h.set_own_data_at(o, rp, ro, n(2.0)));
        assert_eq!(h.own_data_at(o, rp, ro).and_then(num), Some(1.0));
        let acc = k(&mut h, "acc2");
        assert!(def(&mut h, o, acc, PropertyDescriptor { get: Some(None), set: Some(None), enumerable: Some(true), configurable: Some(true), ..Default::default() }));
        let ap = h.own_pos(o, acc).unwrap();
        assert!(h.own_data_at(o, ap, acc).is_none());
        assert!(!h.set_own_data_at(o, ap, acc, n(1.0)));
        // the same after a move to dictionary mode (a new prototype) and a delete: positions are kept
        let pr = h.new_object(None).unwrap();
        h.set_prototype_of(o, Some(pr)).unwrap();
        h.delete(o, a).unwrap();
        assert!(in_dict(&h, o));
        let pos2 = h.own_pos(o, b).unwrap();
        assert_eq!(h.own_data_at(o, pos2, b).and_then(num), Some(50.0));
        assert!(h.set_own_data_at(o, pos2, b, n(60.0)));
        assert_eq!(getn(&mut h, o, b), 60.0);
        assert!(!h.set_own_data_at(o, rp.min(pos2), ro, n(0.0)) || getn(&mut h, o, ro) == 0.0);
    }

    // ----- property test -----

    /// A model of the own properties of one object: the old `PropMap` for the named keys and a map for the indexes.
    struct Model {
        named: PropMap,
        idx: std::collections::BTreeMap<u32, Property>,
        extensible: bool,
    }

    impl Model {
        fn cur(&self, key: PropertyKey) -> Option<Property> {
            match key {
                PropertyKey::Index(i) => self.idx.get(&i).copied(),
                _ => self.named.get(key),
            }
        }
        fn put(&mut self, key: PropertyKey, p: Property) {
            match key {
                PropertyKey::Index(i) => {
                    self.idx.insert(i, p);
                }
                _ => self.named.set(key, p),
            }
        }
        fn define(&mut self, key: PropertyKey, d: &PropertyDescriptor) -> bool {
            match validate_apply(self.extensible, self.cur(key), d) {
                Some(p) => {
                    self.put(key, p);
                    true
                }
                None => false,
            }
        }
        fn delete(&mut self, key: PropertyKey) -> bool {
            match self.cur(key) {
                Some(p) if !p.configurable() => false,
                Some(_) => {
                    match key {
                        PropertyKey::Index(i) => {
                            self.idx.remove(&i);
                        }
                        _ => {
                            self.named.remove(key);
                        }
                    }
                    true
                }
                None => true,
            }
        }
        /// `o[key] = v` on an object with no prototype properties: the own data property, or a new one.
        fn set(&mut self, key: PropertyKey, v: Value) -> Set {
            match self.cur(key) {
                Some(Property::Accessor { set: Some(s), .. }) => Set::Setter(s),
                Some(Property::Accessor { set: None, .. }) | Some(Property::Data { writable: false, .. }) => Set::Rejected,
                Some(Property::Data { .. }) => {
                    let d = PropertyDescriptor { value: Some(v), ..Default::default() };
                    if self.define(key, &d) { Set::Done } else { Set::Rejected }
                }
                None => if self.define(key, &PropertyDescriptor::data(v)) { Set::Done } else { Set::Rejected },
            }
        }
        fn freeze(&mut self, frozen: bool) {
            self.extensible = false;
            let all: Vec<_> = self.idx.keys().map(|&i| PropertyKey::Index(i)).chain(self.named.iter().map(|e| e.0)).collect();
            for key in all {
                let p = self.cur(key).unwrap();
                let np = match p {
                    Property::Data { value, writable, enumerable, .. } => Property::Data { value, writable: writable && !frozen, enumerable, configurable: false },
                    Property::Accessor { get, set, enumerable, .. } => Property::Accessor { get, set, enumerable, configurable: false },
                };
                self.put(key, np);
            }
        }
        fn keys(&self) -> Vec<PropertyKey> {
            let mut out: Vec<_> = self.idx.keys().map(|&i| PropertyKey::Index(i)).collect();
            let named: Vec<_> = self.named.iter().map(|e| e.0).collect();
            out.extend(named.iter().filter(|k| matches!(k, PropertyKey::String(_))));
            out.extend(named.iter().filter(|k| matches!(k, PropertyKey::Symbol(_))));
            out
        }
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as usize % n
        }
    }

    fn same(a: Option<Property>, b: Option<Property>) -> bool {
        format!("{a:?}") == format!("{b:?}")
    }

    /// 100,000 random operations on six objects against a model. After each one the touched object has the same keys, in the same order,
    /// with the same values and attributes. The objects share keys, so they share shapes and meet every kind of transition.
    #[test]
    fn random_operations_agree_with_a_plain_property_map() {
        let mut h = Heap::default();
        let mut keys: Vec<PropertyKey> = (0..14).map(|i| k(&mut h, &format!("key{i}"))).collect();
        keys.extend([PropertyKey::Symbol(h.new_symbol(None).unwrap()), PropertyKey::Symbol(h.new_symbol(None).unwrap())]);
        keys.extend([0, 1, 2, 5, 9, 4000].map(PropertyKey::Index));
        let protos: Vec<ObjId> = (0..3).map(|_| h.new_object(None).unwrap()).collect();
        let fns: Vec<ObjId> = (0..3).map(|_| h.new_object(None).unwrap()).collect();
        let mut objs: Vec<ObjId> = (0..6).map(|_| h.new_object(None).unwrap()).collect();
        let mut models: Vec<Model> = objs.iter().map(|_| Model { named: PropMap::new(), idx: Default::default(), extensible: true }).collect();
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let (mut dict_seen, mut shape_seen) = (0, 0);
        let mut at_most = 0;
        for step in 0..100_000 {
            let i = rng.next(objs.len());
            let (o, m) = (objs[i], &mut models[i]);
            let key = keys[rng.next(keys.len())];
            let v = n(rng.next(1000) as f64);
            let flag = |r: &mut Rng| match r.next(3) { 0 => None, 1 => Some(true), _ => Some(false) };
            match rng.next(100) {
                0..=29 => {
                    let mut d = PropertyDescriptor { value: Some(v), writable: flag(&mut rng), enumerable: flag(&mut rng), configurable: flag(&mut rng), ..Default::default() };
                    match rng.next(5) {
                        0 => d.value = None,
                        1 => {
                            d.value = None;
                            d.writable = None;
                            d.get = Some(Some(fns[rng.next(3)]));
                            if rng.next(2) == 0 {
                                d.set = Some(if rng.next(2) == 0 { None } else { Some(fns[rng.next(3)]) });
                            }
                        }
                        _ => {}
                    }
                    assert_eq!(h.define_own_property(o, key, &d).unwrap(), m.define(key, &d), "define, step {step}");
                }
                30..=59 => assert_eq!(h.set(o, key, v).unwrap(), m.set(key, v), "set, step {step}"),
                60..=74 => assert_eq!(h.delete(o, key).unwrap(), m.delete(key), "delete, step {step}"),
                75..=76 => {
                    let frozen = rng.next(2) == 0;
                    assert!(h.set_integrity(o, frozen).unwrap());
                    m.freeze(frozen);
                }
                77..=81 => {
                    let p = if rng.next(4) == 0 { None } else { Some(protos[rng.next(3)]) };
                    let same_proto = h.get_prototype_of(o).unwrap() == p;
                    assert_eq!(h.set_prototype_of(o, p).unwrap(), same_proto || m.extensible, "setPrototypeOf, step {step}");
                }
                _ => {
                    // A fresh object takes its place: a frozen object or a dictionary stays so, and shape mode must keep being tested.
                    objs[i] = h.new_object(None).unwrap();
                    models[i] = Model { named: PropMap::new(), idx: Default::default(), extensible: true };
                    continue;
                }
            }
            let ob = h.obj(o).unwrap();
            if ob.dict.is_some() { dict_seen += 1 } else { shape_seen += 1 }
            at_most = at_most.max(h.shapes.len());
            let want = models[i].keys();
            assert_eq!(h.own_keys(o).unwrap(), want, "keys, step {step}");
            for &key in &want {
                let (got, exp) = (h.get_own_property(o, key).unwrap(), models[i].cur(key));
                assert!(same(got, exp), "property {key:?}, step {step}: {got:?} against {exp:?}");
            }
            for &key in &keys {
                assert_eq!(h.has_own_property(o, key).unwrap(), models[i].cur(key).is_some(), "has, step {step}");
            }
            assert_eq!(h.is_extensible(o).unwrap(), models[i].extensible);
        }
        assert!(dict_seen > 1000 && shape_seen > 1000, "both modes ran: {dict_seen} dictionary, {shape_seen} shape");
        assert!(at_most > 20, "shapes were made: {at_most}");
    }
}
