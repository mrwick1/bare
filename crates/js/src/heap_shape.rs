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
//! Shapes are never freed in this step (step A6). A dead key in a shape never matches a live key: a freed handle gets a new
//! generation.
//!
//! The inline cache is the API of step A0 and nothing more. No cache is filled yet: `ic_own` never hits, and `get_ic` and `set_ic`
//! run the plain `get` and `set`. The evaluator and the bytecode interpreter can call them now. The real cache (step A2) changes
//! only this file and `heap.rs`. The contract is in `docs/decisions.md` ("Shapes and inline caches") and the plan is in
//! `docs/engine-plan.md`.
//!
//! An `Ic` is plain `Copy` data with no pointer. In JS terms it is a small note that a call site keeps: "last time `o.x` had this
//! shape, the value was in this slot". A handle in it that has died fails the generation check of every handle read.
use super::*;

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
    index: Option<Box<KeyMap>>,
}

impl Table {
    /// The entry of `key` among the first `n` of a table with a hash index.
    #[inline(never)]
    fn probe(&self, key: PropertyKey, n: usize) -> Option<&Entry> {
        let p = *self.index.as_ref()?.get(&key)? as usize;
        if p < n { Some(&self.entries[p]) } else { None }
    }

    fn push(&mut self, e: Entry) {
        self.entries.push(e);
        let n = self.entries.len();
        match &mut self.index {
            Some(m) => {
                m.insert(e.key, n as u32 - 1);
            }
            None if n > SCAN_MAX => self.index = Some(Box::new(self.entries.iter().enumerate().map(|(i, e)| (e.key, i as u32)).collect())),
            None => {}
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
}

impl Shapes {
    pub(super) fn new(length: PropertyKey) -> Shapes {
        let none = (PropertyKey::Index(0), 0, 0);
        let shape = |count, table| Shape { parent: 0, count, nslots: count, table, child: none, children: None };
        let tables = vec![
            Table { entries: Vec::new(), index: None },
            Table { entries: vec![Entry { key: length, slot: 0, attrs: A_W }], index: None },
        ];
        Shapes { shapes: vec![shape(0, 0), shape(0, 0), shape(1, 1), shape(0, 0)], tables }
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
            + self.tables.iter().map(|t| t.entries.capacity() * size_of::<Entry>() + t.index.as_ref().map_or(0, |m| m.capacity() * (size_of::<(PropertyKey, u32)>() + 1))).sum::<usize>()
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
    #[inline]
    pub(super) fn find(&self, shape: u32, key: PropertyKey) -> Option<&Entry> {
        let s = &self.shapes[shape as usize];
        let t = &self.tables[s.table as usize];
        let n = s.count as usize;
        if n <= SCAN_MAX {
            return t.entries[..n].iter().find(|e| e.key == key);
        }
        t.probe(key, n)
    }

    /// The shape that `from` has after `key` is added with `attrs`. `None`: the object goes to dictionary mode (it would have more than
    /// `MAX_PROPS` properties, or the shape table is full).
    #[inline]
    pub(super) fn add(&mut self, from: u32, key: PropertyKey, attrs: u8) -> Option<u32> {
        let s = &self.shapes[from as usize];
        if s.child.2 != 0 && s.child.0 == key && s.child.1 == attrs {
            return Some(s.child.2);
        }
        self.add_slow(from, key, attrs)
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
            let mut copy = Table { entries: Vec::with_capacity(count as usize + 1), index: None };
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

    pub(super) fn count(&self, shape: u32) -> usize {
        self.shapes[shape as usize].count as usize
    }

    pub(super) fn parent(&self, shape: u32) -> u32 {
        self.shapes[shape as usize].parent
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
#[inline]
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
    #[inline]
    pub(super) fn get_prop(&self, sh: &Shapes, key: PropertyKey) -> Option<Property> {
        match &self.dict {
            Some(d) => d.get(key),
            None if matches!(key, PropertyKey::Index(_)) => None,
            None => sh.find(self.shape, key).map(|e| build(e.attrs, &self.slots[e.slot as usize..])),
        }
    }

    pub(super) fn props<'a>(&'a self, sh: &'a Shapes) -> Props<'a> {
        match &self.dict {
            Some(d) => Props::Dict(d.entries.iter()),
            None => Props::Shape(sh.entries(self.shape).iter(), &self.slots),
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
            Some(_) => {}
            None if !matches!(key, PropertyKey::Index(_)) => {
                if let Some(next) = sh.add(self.shape, key, attrs) {
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
            }
            None => {}
        }
        self.to_dict(sh);
        self.dict.as_mut().unwrap().set(key, p);
    }

    /// Remove the own named property `key`. Only the last property added leaves shape mode intact.
    pub(super) fn remove_prop(&mut self, sh: &mut Shapes, key: PropertyKey) -> bool {
        if let Some(d) = &mut self.dict {
            return d.remove(key);
        }
        let Some(pos) = sh.pos(self.shape, key) else { return false };
        let parent = sh.parent(self.shape);
        if pos + 1 == sh.count(self.shape) {
            self.slots.truncate(sh.entries(self.shape)[pos].slot as usize);
            self.shape = parent;
        } else {
            self.to_dict(sh);
            self.dict.as_mut().unwrap().remove(key);
        }
        true
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

/// One inline cache entry. 24 bytes. A site that has no entry holds `Ic::EMPTY`.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // the fields are written and read from step A2
pub struct Ic {
    /// The shape of the receiver that filled the entry.
    shape: u32,
    /// The slot of the property in the receiver (own hit) or in the holder (prototype hit).
    slot: u32,
    /// `IC_EMPTY`, `IC_OWN`, `IC_PROTO` or `IC_ABSENT`.
    kind: u8,
    /// The object that has the property, for a prototype hit.
    holder: ObjId,
    /// `Heap::proto_epoch` when the entry was filled, for a prototype hit and an absent hit.
    epoch: u32,
}

#[allow(dead_code)] // used from step A2
const IC_EMPTY: u8 = 0;

impl Ic {
    /// The entry of a site that has not run, or that missed. It never hits.
    pub const EMPTY: Ic = Ic { shape: 0, slot: 0, kind: IC_EMPTY, holder: ObjId { index: 0, gn: 0 }, epoch: 0 };
}

impl Heap {
    /// The value of an own data property of `o` when `ic` hits, else `None`. The caller runs `get_ic` on `None`. Always `None` now.
    #[inline(always)]
    pub fn ic_own(&self, _o: ObjId, _ic: &Ic) -> Option<Value> {
        None
    }

    /// `get`, and it fills `ic` for the next read. Out of line: it runs only on a miss. `ic` is not filled yet.
    #[cold]
    pub fn get_ic(&mut self, o: ObjId, key: PropertyKey, _ic: &mut Ic) -> R<Get> {
        self.get(o, key)
    }

    /// `set`, and it fills `ic` for the next write. `ic` is not filled yet.
    #[cold]
    pub fn set_ic(&mut self, o: ObjId, key: PropertyKey, value: Value, _ic: &mut Ic) -> R<Set> {
        self.set(o, key, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stub_never_hits_and_runs_the_plain_path() {
        assert_eq!(size_of::<Ic>(), 24);
        let mut h = Heap::new(HeapLimits::default()).unwrap();
        let o = h.new_object(None).unwrap();
        let k = PropertyKey::String(h.intern_str("x").unwrap());
        let mut ic = Ic::EMPTY;
        assert!(h.ic_own(o, &ic).is_none());
        assert_eq!(h.set_ic(o, k, Value::Number(7.0), &mut ic), Ok(Set::Done));
        assert!(h.ic_own(o, &ic).is_none());
        assert!(matches!(h.get_ic(o, k, &mut ic), Ok(Get::Value(Value::Number(n))) if n == 7.0));
        let missing = PropertyKey::String(h.intern_str("y").unwrap());
        assert!(matches!(h.get_ic(o, missing, &mut ic), Ok(Get::Value(Value::Undefined))));
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
        // Deleting the first of two: dictionary mode, order of the rest kept, re-add goes last.
        assert!(h.delete(o, a).unwrap());
        assert!(in_dict(&h, o));
        assert_eq!(shape_of(&h, o), 0);
        assert_eq!(keys_text(&mut h, o), ["b"]);
        setn(&mut h, o, a, 4.0);
        assert_eq!(keys_text(&mut h, o), ["b", "a"]);
        // Deleting a missing key, or the only property, is fine and a non-configurable property stays.
        let p = h.new_object(None).unwrap();
        assert!(h.delete(p, a).unwrap());
        assert!(def(&mut h, p, a, attrs(1.0, true, true, false)));
        assert!(!h.delete(p, a).unwrap());
        assert!(!in_dict(&h, p));
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
        // 1. delete a property that is not the last
        let o = mk(&mut h);
        h.delete(o, a).unwrap();
        assert!(in_dict(&h, o));
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
        let w = h.new_primitive(None, Value::Number(1.0)).unwrap();
        let buf = h.new_buffer(None, 4).unwrap();
        let idx = h.new_indexed(None).unwrap();
        for o in [px, named, w, buf, idx] {
            assert_eq!(shape_of(&h, o), ROOT_EXOTIC);
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
        assert_eq!(up, ROOT_EXOTIC);
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
        let d = h.new_object(None).unwrap();
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
        // the same after a move to dictionary mode: positions are kept
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
