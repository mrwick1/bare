//! Shapes: the hidden classes of objects (`docs/design-engine-rewrite.md` 6.4, 6.11 items 12 and 15, plan step S1.8).
//!
//! An object cell holds a 32-bit shape id in its header (`cell.rs`). The shape says which keys the object has, in which slot each
//! value is, which prototype it has and how many slots are inline. Two objects made by `{a: 1, b: 2}` share one shape. In JS terms:
//! the shape is the description "a, then b", and the object holds only the two values.
//!
//! - Transition tree. A shape is a node. Adding the key `k` with attributes `a` to shape `S` gives the child of `S` for `(k, a)`,
//!   made once. A node keeps its first child in two fields and the others in a map, so a chain where each shape has one follower
//!   (the common case) never probes a map. A root is `(layout, prototype, inline capacity)`; the prototype lives in the shape.
//! - Shared descriptor tables. The keys of a chain sit in one [`Table`]; a shape uses its first `count` entries. A shape that adds
//!   an entry the table already has at that position reuses the table, so shapes that differ only in prototype or in inline
//!   capacity share one table (design 6.11 item 15). Only a branch that adds another entry copies the prefix. An entry is 12 bytes.
//!   A table of more than [`SCAN_MAX`] entries gets a hash index at its first probe.
//! - Dictionary mode. An object that outgrows the tree gets a shape of its own (a dictionary shape) with a table that only this
//!   object uses and that changes in place. Its slots do not move: they keep their numbers, a deleted slot is reused, and the
//!   shape id stays. A cache guarded by shape ID must skip dictionaries ([`Shapes::cacheable`]). An object goes to dictionary mode when
//!   a property other than the last one added is deleted, when an attribute changes (`freeze`, `seal`, data to accessor), or when it
//!   would have more than [`MAX_PROPS`] properties. A dictionary shape never returns to the tree.
//! - Prototype change. [`Shapes::with_proto`] makes one node with the same table and the new prototype (the same shape every time
//!   for one old shape and one prototype), so `Object.setPrototypeOf` does not cost a dictionary.
//! - Slack: [`Site`] decides the inline capacity of the objects that one allocation site makes (below).
//!
//! The collector's hooks (as in S1.4): the marker calls [`Shapes::note_live`] for each marked object (one bit; it returns the
//! prototype of a shape the first time, and the collector pushes that cell on its work list). After the marking, the collector calls
//! [`Shapes::finish_marks`]. It frees each shape that no marked object uses and that is not a parent of a used shape, frees the
//! tables that only they used, and marks the key atoms of the shapes that remain. So it must run before the atom sweep
//! (`Atoms::sweep`) and before `Heap::finish_mark`.
//!
//! A shape id that is not in the header of a live cell is valid only until the next `finish_marks`, which may free the shape and give
//! its id to a new one. [`Shapes::epoch`] changes when a tree shape was freed: a feedback record or a [`Site`] that holds an id also
//! holds the epoch and drops the id when it differs.
use crate::cell::{CellHeader, Layout};
use crate::heap::Heap;
use crate::string::Str;
use std::cell::{Cell, OnceCell};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::ptr::NonNull;

/// Attribute bits of a property. `A_ACC`: an accessor, which uses two slots (getter, then setter).
pub const A_W: u8 = 1;
pub const A_E: u8 = 2;
pub const A_C: u8 = 4;
pub const A_ACC: u8 = 8;

/// Most properties of an object in the tree. The 129th goes to dictionary mode.
pub const MAX_PROPS: usize = 128;
/// Most live shapes. Past it no new shape is made and the object goes to dictionary mode. The collector frees unused shapes each cycle;
/// this bounds the growth between two collections.
pub const SHAPE_CAP: usize = 1 << 20;
/// A table up to this long is searched by a scan; a longer one has a hash index.
pub const SCAN_MAX: usize = 8;
/// Most inline slots of an object: the header and the slots fill a 256-byte cell.
pub const MAX_INLINE: u16 = 30;

const NONE: u32 = u32::MAX;
/// The first value of the tag of the watched object. A shape id is below `SHAPE_CAP` (2^20), so a tag never equals a shape id.
const WATCH_TAG0: u32 = 1 << 31;
const F_DICT: u8 = 1;
const F_FREE: u8 = 2;
const F_INDEX: u8 = 4;
const F_ENUM: u8 = 8;
/// The tag of a prototype edge in the child map; an attribute byte never has it.
const PROTO_EDGE: u64 = 0x80;

/// A key hasher for keys that are cell addresses (aligned to 16) or small numbers: multiply, then rotate the high bits down.
#[derive(Default)]
struct Fx(u64);

impl Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0.rotate_left(26)
    }
    fn write(&mut self, bytes: &[u8]) {
        bytes.iter().for_each(|&b| self.write_u64(b as u64));
    }
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

type Map<K, V> = HashMap<K, V, BuildHasherDefault<Fx>>;

/// The id of a shape. It is the shape word of the header of the objects that have the shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ShapeId(u32);

impl ShapeId {
    #[inline]
    pub(crate) fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// The id as stored in the header (`CellHeader::shape_id`).
    #[inline]
    pub fn raw(self) -> u32 {
        self.0
    }
}

/// One own property of a shape: its key (a cell address), the first slot of its value and its attribute bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C, packed(4))]
pub struct Entry {
    key: u64,
    /// The attribute bits above bit 28, the slot below.
    sa: u32,
}

impl Entry {
    /// The entry of a deleted property of a dictionary table.
    const TOMB: Entry = Entry { key: 0, sa: 0 };

    fn new(key: u64, slot: u32, attrs: u8) -> Entry {
        debug_assert!(key != 0 && slot < 1 << 28 && attrs < 16);
        Entry { key, sa: (attrs as u32) << 28 | slot }
    }

    /// The key atom.
    pub fn key(self) -> Str {
        let k = self.key;
        Str::from_cell(NonNull::new(std::ptr::with_exposed_provenance_mut(k as usize)).expect("an entry has a key"))
    }

    #[inline]
    pub fn slot(self) -> u32 {
        self.sa & ((1 << 28) - 1)
    }

    #[inline]
    pub fn attrs(self) -> u8 {
        (self.sa >> 28) as u8
    }

    #[inline]
    fn bits(self) -> u64 {
        self.key
    }

    fn is_tomb(self) -> bool {
        self.bits() == 0
    }

    /// Slots of the value: 2 for an accessor.
    #[inline]
    pub fn width(self) -> u32 {
        1 + (self.attrs() & A_ACC != 0) as u32
    }
}

fn key_bits(k: Str) -> u64 {
    k.cell().as_ptr().expose_provenance() as u64
}

fn addr_bits(p: Option<NonNull<u8>>) -> u64 {
    p.map_or(0, |p| p.as_ptr().expose_provenance() as u64)
}

/// The edge of the child map for adding `key` with `attrs`, or for a new prototype.
#[inline]
fn add_edge(key: u64, attrs: u8) -> u64 {
    key << 8 | attrs as u64
}

/// An open-addressing index of a table: key to position. A slot is 0 (empty), `DELETED`, or the position plus 1.
type Index = Box<[u32]>;

const DELETED: u32 = u32::MAX;

fn slot_of(key: u64, mask: usize) -> usize {
    (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) as usize & mask
}

/// The keys of a chain of shapes. A tree shape with `count` properties uses `entries[..count]`; a branch may have added more entries
/// after them. A dictionary table is used by one shape, uses all its entries and has deleted ones as `TOMB`.
struct Table {
    entries: Vec<Entry>,
    /// Made at the first probe of a table of more than `SCAN_MAX` entries. It has room for twice the entries.
    index: OnceCell<Index>,
    /// Dictionary only: deleted slots to reuse, and the number of `TOMB` entries.
    free: Vec<u32>,
    tombs: u32,
    dict: bool,
}

impl Table {
    fn new(entries: Vec<Entry>, dict: bool) -> Table {
        Table { entries, index: OnceCell::new(), free: Vec::new(), tombs: 0, dict }
    }

    #[inline]
    fn pos(&self, key: u64, n: usize) -> Option<usize> {
        if n <= SCAN_MAX {
            return self.entries[..n].iter().position(|e| e.bits() == key);
        }
        self.probe(key).filter(|&p| p < n)
    }

    fn build(&self) -> Index {
        let mut slots = vec![0u32; (self.entries.len() * 2).next_power_of_two().max(16)].into_boxed_slice();
        let mask = slots.len() - 1;
        for (p, e) in self.entries.iter().enumerate().filter(|(_, e)| !e.is_tomb()) {
            let mut i = slot_of(e.bits(), mask);
            while slots[i] != 0 {
                i = (i + 1) & mask;
            }
            slots[i] = p as u32 + 1;
        }
        slots
    }

    /// The position of `key` in the whole table.
    #[inline(never)]
    fn probe(&self, key: u64) -> Option<usize> {
        let slots = self.index.get_or_init(|| self.build());
        let mask = slots.len() - 1;
        let mut i = slot_of(key, mask);
        loop {
            match slots[i] {
                0 => return None,
                DELETED => {}
                v if self.entries[v as usize - 1].bits() == key => return Some(v as usize - 1),
                _ => {}
            }
            i = (i + 1) & mask;
        }
    }

    fn push(&mut self, e: Entry) {
        self.entries.push(e);
        let Some(slots) = self.index.get_mut() else { return };
        if self.entries.len() * 2 > slots.len() {
            self.index = OnceCell::new();
            return;
        }
        let mask = slots.len() - 1;
        let mut i = slot_of(e.bits(), mask);
        while slots[i] != 0 && slots[i] != DELETED {
            i = (i + 1) & mask;
        }
        slots[i] = self.entries.len() as u32;
    }

    /// Drop `key` from the index of a dictionary table (its entry becomes a `TOMB`).
    fn unindex(&mut self, key: u64) {
        let Some(slots) = self.index.get_mut() else { return };
        let mask = slots.len() - 1;
        let mut i = slot_of(key, mask);
        loop {
            match slots[i] {
                0 => return,
                v if v != DELETED && self.entries[v as usize - 1].bits() == key => {
                    slots[i] = DELETED;
                    return;
                }
                _ => i = (i + 1) & mask,
            }
        }
    }

    fn bytes(&self) -> usize {
        use std::mem::size_of;
        size_of::<Table>() + self.entries.capacity() * size_of::<Entry>() + self.free.capacity() * 4 + self.index.get().map_or(0, |m| m.len() * 4)
    }
}

type Edges = Map<u64, u32>;

/// A shape: a node of the transition tree, or a dictionary shape.
pub struct Shape {
    /// The address of the prototype cell; 0 for none. It is the same along a chain of parents.
    proto: u64,
    /// The first child, tested before the map: `child_edge` is 0 when there is none.
    child_edge: u64,
    children: Option<Box<Edges>>,
    /// The shape before the last key was added; `NONE` for a root, a shape of `with_proto` and a dictionary shape.
    parent: u32,
    table: u32,
    child: u32,
    /// Slots in use (an accessor uses two). A dictionary shape counts the slots that were ever given.
    pub slots: u32,
    count: u16,
    inline: u16,
    pub layout: Layout,
    flags: u8,
}

impl Shape {
    fn new(layout: Layout, proto: u64, inline: u16) -> Shape {
        Shape { proto, child_edge: 0, children: None, parent: NONE, table: 0, child: 0, slots: 0, count: 0, inline, layout, flags: 0 }
    }

    /// The prototype cell of the objects of this shape.
    pub fn proto(&self) -> Option<NonNull<u8>> {
        NonNull::new(std::ptr::with_exposed_provenance_mut(self.proto as usize))
    }

    /// The inline slots of an object of this shape; the slots from here on are in its overflow record.
    #[inline]
    pub fn inline(&self) -> u16 {
        self.inline
    }

    pub fn is_dict(&self) -> bool {
        self.flags & F_DICT != 0
    }

    /// The number of properties of a tree shape (a dictionary shape: see [`Shapes::len_of`]).
    pub fn count(&self) -> usize {
        self.count as usize
    }
}

/// The result of [`Shapes::remove`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Removed {
    /// The shape of the object after the delete.
    pub shape: ShapeId,
    /// The property that went. The caller clears its slots.
    pub entry: Entry,
    /// True when the slots above `entry.slot()` move down by `entry.width()` (the object stayed in the tree and the later keys have new
    /// slots). False for the last property, which has no later slot, and for a dictionary shape, whose slots do not move.
    pub shifts: bool,
}

/// The counts of [`Shapes::stats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Live shapes, without the sentinel, and how many of them are dictionary shapes.
    pub shapes: usize,
    pub dicts: usize,
    /// Live tables, their entries, and the entries that their vectors have room for.
    pub tables: usize,
    pub entries: usize,
    pub entry_room: usize,
    /// Bytes of the shape nodes and the id bookkeeping (capacity).
    pub node_bytes: usize,
    /// Bytes of the tables: entries, indexes, free lists (capacity).
    pub table_bytes: usize,
    /// Bytes of the transition maps (capacity).
    pub edge_bytes: usize,
}

pub struct Shapes {
    /// `nodes[0]` is a sentinel: shape 0 is no shape (a string).
    nodes: Vec<Shape>,
    /// `None`: a free slot. Table 0 is the table of every chain's first entry and is never freed.
    tables: Vec<Option<Table>>,
    free_nodes: Vec<u32>,
    free_tables: Vec<u32>,
    /// `(proto, inline)` and layout to root.
    roots: Map<(u64, u8), u32>,
    /// One bit per id: some marked cell has this shape.
    live: Vec<u64>,
    scratch: Vec<bool>,
    epoch: u32,
    /// The address of the watched object (the global object of the realm), or 0. `Obj::set_shape` changes `watch_tag` when this
    /// object gets another shape: a cache of the address of one of its slots then stops matching (see [`Shapes::watch_tag`]).
    watch: usize,
    watch_tag: Cell<u32>,
    /// Bumped when an object that a prototype entry of a feedback record depends on (`F_PROTO`) changes, and at each collection.
    proto_epoch: u64,
}

impl Default for Shapes {
    fn default() -> Self {
        Shapes { nodes: vec![Shape::new(Layout::OneByte, 0, 0)], tables: vec![Some(Table::new(Vec::new(), false))], free_nodes: Vec::new(), free_tables: Vec::new(), roots: Map::default(), live: vec![0], scratch: Vec::new(), epoch: 0, watch: 0, watch_tag: Cell::new(WATCH_TAG0), proto_epoch: 1 }
    }
}

impl Shapes {
    /// Watch the object at `addr` (the global object). One object at a time.
    pub fn watch(&mut self, addr: usize) {
        self.watch = addr;
    }

    /// The global mutation tag. An add, deletion, attribute change or prototype change calls `Obj::set_shape`, which bumps the
    /// tag even if the dictionary keeps its shape ID. Slot-record moves and global lexical declarations also bump the tag.
    /// A global-variable cache (`GetGlobal`, `SetGlobal`) holds the tag and the address of a slot. The tag is never 0 and never a shape id.
    #[inline]
    pub fn watch_tag(&self) -> u32 {
        self.watch_tag.get()
    }

    /// Invalidate the global caches if the watched object is at `addr`.
    #[inline]
    pub(crate) fn changed(&self, addr: usize) {
        if addr == self.watch {
            self.watch_tag.set(self.watch_tag.get() + 1);
        }
    }

    #[inline]
    fn node(&self, id: ShapeId) -> &Shape {
        &self.nodes[id.0 as usize]
    }

    #[inline]
    fn table(&self, t: u32) -> &Table {
        self.tables[t as usize].as_ref().expect("a shape's table is live")
    }

    #[inline]
    fn table_mut(&mut self, t: u32) -> &mut Table {
        self.tables[t as usize].as_mut().expect("a shape's table is live")
    }

    fn live_count(&self) -> usize {
        self.nodes.len() - 1 - self.free_nodes.len()
    }

    /// A node with a new id (a freed id is reused). `None` at `SHAPE_CAP`.
    fn make(&mut self, s: Shape) -> Option<ShapeId> {
        if self.live_count() >= SHAPE_CAP {
            return None;
        }
        Some(self.push(s))
    }

    fn push(&mut self, s: Shape) -> ShapeId {
        let id = match self.free_nodes.pop() {
            Some(i) => {
                self.nodes[i as usize] = s;
                i
            }
            None => {
                self.nodes.push(s);
                if self.nodes.len() > self.live.len() * 64 {
                    self.live.push(0);
                }
                self.nodes.len() as u32 - 1
            }
        };
        ShapeId(id)
    }

    fn make_table(&mut self, t: Table) -> u32 {
        match self.free_tables.pop() {
            Some(i) => {
                self.tables[i as usize] = Some(t);
                i
            }
            None => {
                self.tables.push(Some(t));
                self.tables.len() as u32 - 1
            }
        }
    }

    /// The shape of an empty object of `layout` with prototype `proto` and `inline` inline slots (at most `MAX_INLINE`). It is made once.
    pub fn root(&mut self, layout: Layout, proto: Option<NonNull<u8>>, inline: u16) -> ShapeId {
        let inline = inline.min(MAX_INLINE);
        let p = addr_bits(proto);
        if let Some(&r) = self.roots.get(&(p | (inline as u64) << 48, layout as u8)) {
            return ShapeId(r);
        }
        // `SHAPE_CAP` does not apply to a root: there is one for each live prototype and capacity, and no object can be made without it.
        let id = self.push(Shape::new(layout, p, inline));
        self.roots.insert((p | (inline as u64) << 48, layout as u8), id.0);
        id
    }

    /// The shape that `from` has after `key` is added with `attrs`, in the next slot. `None`: the object goes to dictionary mode (it would
    /// have more than `MAX_PROPS` properties, or the shapes are at `SHAPE_CAP`). `from` is not a dictionary shape and does not have `key`.
    #[inline]
    pub fn add(&mut self, from: ShapeId, key: Str, attrs: u8) -> Option<ShapeId> {
        let edge = add_edge(key_bits(key), attrs);
        let s = &self.nodes[from.0 as usize];
        debug_assert!(!s.is_dict());
        if s.child_edge == edge {
            return Some(ShapeId(s.child));
        }
        self.add_miss(from, edge, key_bits(key), attrs)
    }

    #[inline(never)]
    fn add_miss(&mut self, from: ShapeId, edge: u64, key: u64, attrs: u8) -> Option<ShapeId> {
        let s = &self.nodes[from.0 as usize];
        if let Some(&c) = s.children.as_ref().and_then(|m| m.get(&edge)) {
            return Some(ShapeId(c));
        }
        debug_assert!(s.child_edge == 0 || self.table(s.table).pos(key, s.count as usize).is_none(), "the key is already in the shape");
        if s.count as usize >= MAX_PROPS || self.live_count() >= SHAPE_CAP {
            return None;
        }
        let (count, slots, table, layout, proto, inline, flags) = (s.count as usize, s.slots, s.table, s.layout, s.proto, s.inline, s.flags);
        let entry = Entry::new(key, slots, attrs);
        let t = self.table_mut(table);
        // The tip of a table adds to it. A table that already has this entry here is shared. Any other shape copies its prefix first.
        let table = if t.entries.len() == count {
            t.push(entry);
            table
        } else if t.entries.get(count) == Some(&entry) {
            table
        } else {
            let mut copy = Table::new(Vec::with_capacity(count + 1), false);
            t.entries[..count].iter().for_each(|&e| copy.push(e));
            copy.push(entry);
            self.make_table(copy)
        };
        let mut n = Shape::new(layout, proto, inline);
        n.parent = from.0;
        n.table = table;
        n.count = count as u16 + 1;
        n.slots = slots + entry.width();
        n.flags = flags;
        if attrs & A_E != 0 {
            n.flags |= F_ENUM;
        }
        if flags & F_INDEX == 0 && crate::bi_object::atom_index(entry.key()).is_some() {
            n.flags |= F_INDEX;
        }
        let id = self.make(n).expect("room was checked");
        let s = &mut self.nodes[from.0 as usize];
        if s.child_edge == 0 {
            (s.child_edge, s.child) = (edge, id.0);
        } else {
            s.children.get_or_insert_with(Default::default).insert(edge, id.0);
        }
        Some(id)
    }

    /// The shape of an object literal with `keys` in order (each a writable, enumerable, configurable data property): its root has exactly
    /// `keys.len()` inline slots, so no slack is spent. `None` for a key list that does not fit the tree.
    pub fn literal(&mut self, layout: Layout, proto: Option<NonNull<u8>>, keys: &[Str]) -> Option<ShapeId> {
        let mut s = self.root(layout, proto, keys.len() as u16);
        for &k in keys {
            s = self.add(s, k, A_W | A_E | A_C)?;
        }
        Some(s)
    }

    /// The entry of `key` in `id`.
    #[inline]
    pub fn find(&self, id: ShapeId, key: Str) -> Option<Entry> {
        let s = self.node(id);
        let t = self.table(s.table);
        let n = if s.is_dict() { t.entries.len() } else { s.count as usize };
        t.pos(key_bits(key), n).map(|p| t.entries[p])
    }

    /// The number of properties of `id`.
    pub fn len_of(&self, id: ShapeId) -> usize {
        let s = self.node(id);
        let t = self.table(s.table);
        if s.is_dict() { t.entries.len() - t.tombs as usize } else { s.count as usize }
    }

    /// The properties of `id` in the order they were added.
    pub fn entries(&self, id: ShapeId) -> impl Iterator<Item = Entry> + '_ {
        let s = self.node(id);
        let n = if s.is_dict() { usize::MAX } else { s.count as usize };
        self.table(s.table).entries.iter().take(n).copied().filter(|e| !e.is_tomb())
    }

    /// Whether the shape has a named descriptor for a canonical array index.
    pub(crate) fn has_indexed_name(&self, id: ShapeId) -> bool {
        let s = self.node(id);
        if s.is_dict() {
            self.entries(id).any(|e| crate::bi_object::atom_index(e.key()).is_some())
        } else {
            s.flags & F_INDEX != 0
        }
    }

    /// A conservative test. Enumerable symbols and internal names also set this flag.
    pub(crate) fn may_have_enumerable(&self, id: ShapeId) -> bool {
        let s = self.node(id);
        if s.is_dict() {
            self.entries(id).any(|e| e.attrs() & A_E != 0)
        } else {
            s.flags & F_ENUM != 0
        }
    }

    /// The immutable prefix of the descriptor table for a tree shape.
    pub(crate) fn tree_entries(&self, id: ShapeId) -> &[Entry] {
        let s = self.node(id);
        debug_assert!(!s.is_dict());
        &self.table(s.table).entries[..s.count()]
    }

    /// False for a dictionary shape. Its table changes in place, so the shape ID cannot guard a cache entry.
    #[inline]
    pub fn cacheable(&self, id: ShapeId) -> bool {
        !self.node(id).is_dict()
    }

    /// The shape before the last key was added; `None` for a root, a shape of `with_proto` and a dictionary shape.
    pub fn parent(&self, id: ShapeId) -> Option<ShapeId> {
        Some(ShapeId(self.node(id).parent)).filter(|p| p.0 != NONE)
    }

    pub fn get(&self, id: ShapeId) -> &Shape {
        self.node(id)
    }

    /// The shape of the header `h`: one load from the table of ids.
    #[inline]
    pub fn of(&self, h: CellHeader) -> &Shape {
        &self.nodes[h.shape_id() as usize]
    }

    /// The id in the header `h`.
    #[inline]
    pub fn id_of(&self, h: CellHeader) -> ShapeId {
        ShapeId(h.shape_id())
    }

    /// The header of an object cell with this shape.
    pub fn header(&self, id: ShapeId) -> CellHeader {
        CellHeader::object(id.0, self.node(id).layout, 0)
    }

    /// Changes when a tree shape was freed: an id that a record kept from before may name another shape now.
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// The epoch of the prototype entries (`interp_rt.rs`): an entry is valid while it equals this. It starts at 1.
    #[inline]
    pub fn proto_epoch(&self) -> u64 {
        self.proto_epoch
    }

    /// Drop every prototype entry. (A plain field, not a `Cell`: a `Cell` in `Shapes` would stop the compiler from keeping a shape in a
    /// register across a store, which costs instructions in `Obj::new` and the loop.)
    pub fn bump_proto_epoch(&mut self) {
        self.proto_epoch += 1;
    }

    /// The shape of an object that is like `id` with prototype `proto`: the same table, the same slots and the same inline capacity.
    /// A dictionary shape changes in place and keeps its id. `id` has no parent after this, so deleting its last key goes to
    /// dictionary mode.
    pub fn with_proto(&mut self, id: ShapeId, proto: Option<NonNull<u8>>) -> ShapeId {
        let p = addr_bits(proto);
        let s = self.node(id);
        if s.proto == p {
            return id;
        }
        if s.is_dict() {
            self.nodes[id.0 as usize].proto = p;
            return id;
        }
        if s.count == 0 {
            let (layout, inline) = (s.layout, s.inline);
            return self.root(layout, proto, inline);
        }
        let edge = p << 8 | PROTO_EDGE;
        if s.child_edge == edge {
            return ShapeId(s.child);
        }
        if let Some(&c) = s.children.as_ref().and_then(|m| m.get(&edge)) {
            return ShapeId(c);
        }
        let mut n = Shape::new(s.layout, p, s.inline);
        (n.table, n.count, n.slots) = (s.table, s.count, s.slots);
        n.flags = s.flags;
        // A shape at the cap cannot be made: the object goes to its own dictionary shape.
        let Some(c) = self.make(n) else {
            let d = self.to_dict(id);
            self.nodes[d.0 as usize].proto = p;
            return d;
        };
        let s = &mut self.nodes[id.0 as usize];
        if s.child_edge == 0 {
            (s.child_edge, s.child) = (edge, c.0);
        } else {
            s.children.get_or_insert_with(Default::default).insert(edge, c.0);
        }
        c
    }

    /// A dictionary shape for one object that has shape `id`: a copy of the table that nothing else uses. The slots keep their numbers,
    /// so the object's values do not move. A dictionary shape gives itself. Call it again for another object: each gets its own.
    pub fn to_dict(&mut self, id: ShapeId) -> ShapeId {
        let s = self.node(id);
        if s.is_dict() {
            return id;
        }
        let (layout, proto, inline, slots, count, table) = (s.layout, s.proto, s.inline, s.slots, s.count as usize, s.table);
        let entries = self.table(table).entries[..count].to_vec();
        let t = self.make_table(Table::new(entries, true));
        let mut n = Shape::new(layout, proto, inline);
        (n.flags, n.table, n.slots) = (F_DICT, t, slots);
        match self.make(n) {
            Some(d) => d,
            None => {
                // No id is free: undo the table. The caller keeps the tree shape; this can happen only at `SHAPE_CAP`.
                self.tables[t as usize] = None;
                self.free_tables.push(t);
                id
            }
        }
    }

    /// Add `key` with `attrs` to the dictionary shape `id`. Returns the entry (its slot). `key` is not in the shape.
    pub fn dict_add(&mut self, id: ShapeId, key: Str, attrs: u8) -> Entry {
        let s = self.node(id);
        debug_assert!(s.is_dict() && self.find(id, key).is_none());
        let (table, slots) = (s.table, s.slots);
        let acc = attrs & A_ACC != 0;
        let t = self.table_mut(table);
        let slot = if acc { None } else { t.free.pop() };
        let slot = slot.unwrap_or(slots);
        let e = Entry::new(key_bits(key), slot, attrs);
        t.push(e);
        let end = slot + e.width();
        if end > slots {
            self.nodes[id.0 as usize].slots = end;
        }
        e
    }

    /// Remove `key` from `id` (an object has deleted it). `None` when the shape has no such key, or when it needs a dictionary shape
    /// and none can be made. In a tree shape of at most `SCAN_MAX` properties, or when `key` is the last, the object stays in the tree: the
    /// shape before `key` takes the later keys again, so objects that drop the same key share their new shapes (a reviver that drops a key
    /// of each record). Then every slot above the removed one moves down by its width ([`Removed::shifts`]). Another delete from a
    /// bigger object moves it to its own dictionary shape (slots stay). A dictionary shape keeps its id.
    pub fn remove(&mut self, id: ShapeId, key: Str) -> Option<Removed> {
        let e = self.find(id, key)?;
        let s = self.node(id);
        if !s.is_dict() {
            let n = s.count as usize;
            let pos = self.table(s.table).entries[..n].iter().position(|x| x.bits() == e.bits()).expect("found");
            if n <= SCAN_MAX || pos + 1 == n {
                let mut later = [e; SCAN_MAX];
                let k = n - pos - 1;
                later[..k].copy_from_slice(&self.table(s.table).entries[pos + 1..n]);
                // Walk up to the shape before `key`; a shape without a parent (after `with_proto`) cannot.
                let mut base = Some(id);
                for _ in pos..n {
                    base = base.and_then(|b| self.parent(b));
                }
                let replayed = base.and_then(|b| later[..k].iter().try_fold(b, |s, x| self.add(s, x.key(), x.attrs())));
                if let Some(shape) = replayed {
                    return Some(Removed { shape, entry: e, shifts: k > 0 });
                }
            }
            let d = self.to_dict(id);
            if !self.node(d).is_dict() {
                // The shapes are at the cap: no dictionary shape can be made. The object keeps the property.
                return None;
            }
            return self.remove(d, key);
        }
        let s = self.node(id);
        let table = s.table;
        let t = self.table_mut(table);
        let p = t.pos(e.bits(), t.entries.len()).expect("found");
        t.unindex(e.bits());
        t.entries[p] = Entry::TOMB;
        t.tombs += 1;
        t.free.extend(e.slot()..e.slot() + e.width());
        // Trailing tombstones go; a table that is mostly tombstones is packed.
        while t.entries.last().is_some_and(|x| x.is_tomb()) {
            t.entries.pop();
            t.tombs -= 1;
        }
        if t.tombs as usize * 2 > t.entries.len() && t.entries.len() > 16 {
            t.entries.retain(|x| !x.is_tomb());
            t.tombs = 0;
            t.index = OnceCell::new();
        }
        Some(Removed { shape: id, entry: e, shifts: false })
    }

    /// Change the attributes of `key` (`freeze`, `seal`, `defineProperty`): the object moves to its own dictionary shape. Returns that
    /// shape, the old entry and the new one. When the property changes between data and accessor, the new entry has other slots and
    /// the caller moves the values; otherwise the slots are the same.
    pub fn reconfigure(&mut self, id: ShapeId, key: Str, attrs: u8) -> Option<(ShapeId, Entry, Entry)> {
        let old = self.find(id, key)?;
        let d = self.to_dict(id);
        if !self.node(d).is_dict() {
            return None;
        }
        let s = self.node(d);
        let (table, slots) = (s.table, s.slots);
        let t = self.table_mut(table);
        let p = t.pos(old.bits(), t.entries.len()).expect("found");
        let mut new = Entry::new(old.bits(), old.slot(), attrs);
        if new.width() != old.width() {
            // The two kinds use another number of slots: the new entry takes fresh ones, the old ones are free.
            new = Entry::new(old.bits(), slots, attrs);
            t.free.extend(old.slot()..old.slot() + old.width());
            self.nodes[d.0 as usize].slots = slots + new.width();
        }
        self.table_mut(table).entries[p] = new;
        Some((d, old, new))
    }

    /// A marked object cell with header `h` uses its shape: keep it. One bit is set; nothing else is read. Returns the prototype cell
    /// of the shape the first time the shape is seen in a collection, and the collector pushes it on its work list. A prototype that
    /// only a shape holds would be freed otherwise.
    #[inline]
    pub fn note_live(&mut self, _heap: &mut Heap, h: CellHeader) -> Option<NonNull<u8>> {
        let id = h.shape_id() as usize;
        let (w, b) = (id / 64, 1u64 << (id % 64));
        let old = self.live[w];
        if old & b != 0 {
            return None;
        }
        self.live[w] = old | b;
        self.nodes[id].proto()
    }

    /// After the marking: free the shapes and tables that no marked cell used, mark the key atoms of the others, and clear the bits.
    /// Call it before the atom sweep and before `Heap::finish_mark`.
    pub fn finish_marks(&mut self, heap: &mut Heap) {
        let n = self.nodes.len();
        let mut keep = std::mem::take(&mut self.scratch);
        keep.clear();
        keep.resize(n, false);
        keep[0] = true;
        for (w, bits) in self.live.iter_mut().enumerate() {
            let mut b = std::mem::take(bits);
            while b != 0 {
                let mut i = (w * 64 + b.trailing_zeros() as usize) as u32;
                b &= b - 1;
                // A shape keeps its parents: a parent's keys are a prefix of the table, and the tree links the two.
                while i != NONE && !std::mem::replace(&mut keep[i as usize], true) {
                    i = self.nodes[i as usize].parent;
                }
            }
        }
        // Free the nodes that nothing keeps. A tree table is freed below, when no kept node uses it.
        let mut used = vec![0u32; self.tables.len()];
        used[0] = 1;
        let mut freed_tree = false;
        for (i, s) in self.nodes.iter_mut().enumerate().skip(1) {
            if s.flags & F_FREE != 0 {
                continue;
            }
            if keep[i] {
                if !s.is_dict() {
                    let u = &mut used[s.table as usize];
                    *u = (*u).max(s.count as u32 + 1);
                }
                continue;
            }
            if s.is_dict() {
                self.tables[s.table as usize] = None;
                self.free_tables.push(s.table);
            } else {
                freed_tree = true;
            }
            *s = Shape::new(Layout::OneByte, 0, 0);
            s.flags = F_FREE;
            self.free_nodes.push(i as u32);
        }
        // The kept nodes forget their freed children.
        for s in self.nodes.iter_mut().filter(|s| s.flags & F_FREE == 0) {
            if s.child_edge != 0 && !keep[s.child as usize] {
                (s.child_edge, s.child) = (0, 0);
            }
            if let Some(m) = &mut s.children {
                m.retain(|_, c| keep[*c as usize]);
                if m.is_empty() {
                    s.children = None;
                }
            }
        }
        self.roots.retain(|_, r| keep[*r as usize]);
        // Tables: free the unused ones, cut the entries that only freed shapes used, and mark the keys of the rest.
        for (ti, slot) in self.tables.iter_mut().enumerate() {
            let Some(t) = slot else { continue };
            if !t.dict {
                let u = used[ti] as usize;
                if u == 0 {
                    *slot = None;
                    self.free_tables.push(ti as u32);
                    continue;
                }
                if t.entries.len() > u - 1 {
                    t.entries.truncate(u - 1);
                    t.entries.shrink_to_fit();
                    t.index = OnceCell::new();
                }
            }
            for e in t.entries.iter().filter(|e| !e.is_tomb()) {
                let k = e.key();
                if !k.is_shared() {
                    // SAFETY: a key is a live atom cell of `heap`: an earlier call marked it, or the table got it since the last call.
                    unsafe { heap.mark(k.cell()) };
                }
            }
        }
        if freed_tree {
            self.epoch = self.epoch.wrapping_add(1);
        }
        self.scratch = keep;
    }

    /// Live shapes, without the sentinel.
    pub fn count(&self) -> usize {
        self.live_count()
    }

    /// Bytes of the table of ids and the live bits, outside the heap.
    pub fn table_bytes(&self) -> usize {
        self.nodes.capacity() * std::mem::size_of::<Shape>() + self.live.capacity() * 8 + self.free_nodes.capacity() * 4
    }

    pub fn stats(&self) -> Stats {
        use std::mem::size_of;
        let live = self.nodes.iter().filter(|s| s.flags & F_FREE == 0).count() - 1;
        let tables = self.tables.iter().flatten();
        Stats {
            shapes: live,
            dicts: self.nodes.iter().filter(|s| s.is_dict()).count(),
            tables: tables.clone().count(),
            entries: tables.clone().map(|t| t.entries.len() - t.tombs as usize).sum(),
            entry_room: tables.clone().map(|t| t.entries.capacity()).sum(),
            node_bytes: self.table_bytes() + self.tables.capacity() * size_of::<Option<Table>>(),
            table_bytes: tables.map(Table::bytes).sum(),
            edge_bytes: self.nodes.iter().filter_map(|s| s.children.as_ref()).map(|m| size_of::<Edges>() + m.capacity() * (size_of::<(u64, u32)>() + 1)).sum::<usize>() + self.roots.capacity() * (size_of::<((u64, u8), u32)>() + 1),
        }
    }
}

/// Slack tracking for one allocation site (a constructor call, an object literal without fixed keys): how many inline slots its objects
/// get. The first `SLACK_RUNS` objects get `initial` slots, more than they usually need. The engine calls [`Site::observe`] with the
/// slot count of each object when the site's code has finished with it. After `SLACK_RUNS` observations the capacity is the most
/// slots any of them used. A later object that needs more widens the capacity, at most twice. Objects that exist keep their shape
/// and their capacity; only new objects get the new one (a root with the new capacity, `Shapes::root`).
#[derive(Clone, Copy, Debug)]
pub struct Site {
    inline: u16,
    max: u16,
    runs: u8,
    widened: u8,
}

/// Observations before a site settles.
pub const SLACK_RUNS: u8 = 8;

impl Site {
    pub const fn new(initial: u16) -> Site {
        Site { inline: if initial > MAX_INLINE { MAX_INLINE } else { initial }, max: 0, runs: 0, widened: 0 }
    }

    /// The inline slots for the next object of the site.
    pub fn inline(&self) -> u16 {
        self.inline
    }

    pub fn settled(&self) -> bool {
        self.runs >= SLACK_RUNS
    }

    /// An object of the site ended with `used` slots. True when `inline` changed: the next object has another root shape.
    pub fn observe(&mut self, used: u32) -> bool {
        self.max = self.max.max(used.min(MAX_INLINE as u32) as u16);
        self.runs = self.runs.saturating_add(1);
        let was = self.inline;
        if self.runs == SLACK_RUNS {
            self.inline = self.max;
        } else if self.runs > SLACK_RUNS && self.max > self.inline && self.widened < 2 {
            self.inline = self.max;
            self.widened += 1;
        }
        self.inline != was
    }
}

#[cfg(test)]
#[path = "shapes_tests.rs"]
mod tests;
