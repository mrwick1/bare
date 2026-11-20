// Tests of `gc.rs`. Miri runs them in CI, so the counts shrink under it. The objects are real (`object.rs`): the shape of an object of
// N slots has the N keys `k0`, `k1`, ... as data properties and 8 inline slots, so up to 8 slots are in the cell and the rest in an
// overflow record. `get` and `set` are for the inline slots (`i < 8`); `Realm::get` and `Realm::set` reach any slot.
use super::*;
use crate::atoms::SharedAtoms;
use crate::heap::{BLOCK, POOL_KEEP};
use crate::object::Obj;
use crate::shapes::{A_C, A_E, A_W, MAX_PROPS, ShapeId};
use crate::string::Chars;
use crate::value::Kind;
use std::sync::Arc;

const MIRI: bool = cfg!(miri);

const WRITE: u8 = A_W | A_E | A_C;

/// A realm for the tests: a heap, shapes and atoms, with one pinned object for each shape. A shape that no marked object uses is freed
/// and its id is reused, so a test must not make a cell with the id of a shape that no live cell uses.
struct Realm {
    heap: Heap,
    shapes: Shapes,
    atoms: Atoms,
    shared: Arc<SharedAtoms>,
    by_slots: Vec<Option<ShapeId>>,
    /// The atoms `k0`, `k1`, ...: each is a key of the pinned shape that first needed it.
    keys: Vec<Str>,
    pins: Vec<Value>,
}

fn ov(p: NonNull<u8>) -> Value {
    Value::pointer(Kind::Object, p)
}

fn slot(p: NonNull<u8>, i: usize) -> *mut Value {
    p.as_ptr().wrapping_add(CellHeader::SIZE).cast::<Value>().wrapping_add(i)
}

fn get(p: NonNull<u8>, i: usize) -> Value {
    // SAFETY: a test object has the slot.
    unsafe { slot(p, i).read() }
}

fn set(p: NonNull<u8>, i: usize, v: Value) {
    // SAFETY: as in `get`.
    unsafe { slot(p, i).write(v) }
}

fn entry(t: NonNull<u8>, i: usize) -> (Value, Value) {
    let (_, e) = table(t);
    // SAFETY: a test table has the entry.
    unsafe { (e.add(2 * i).read(), e.add(2 * i + 1).read()) }
}

fn set_entry(t: NonNull<u8>, i: usize, k: Value, v: Value) {
    let (_, e) = table(t);
    // SAFETY: as in `entry`.
    unsafe {
        e.add(2 * i).write(k);
        e.add(2 * i + 1).write(v);
    }
}

impl Realm {
    /// A realm that has the shapes (and their pins) of the objects with 0 to 8 slots.
    fn new() -> Realm {
        let shared = Arc::new(SharedAtoms::default());
        let mut r = Realm { heap: Heap::default(), shapes: Shapes::default(), atoms: Atoms::new(shared.clone()), shared, by_slots: Vec::new(), keys: Vec::new(), pins: Vec::new() };
        (0..=8).for_each(|n| {
            r.shape(n);
        });
        r
    }

    /// The first `n` atoms of `k0`, `k1`, ...
    fn keys(&mut self, n: usize) -> Vec<Str> {
        while self.keys.len() < n {
            let name = format!("k{}", self.keys.len());
            self.keys.push(self.atoms.intern(&mut self.heap, Chars::One(name.as_bytes())).unwrap());
        }
        self.keys[..n].to_vec()
    }

    /// A shape of `n` slots with no prototype and the keys `k0` to `k(n-1)`: a tree shape (shared by all objects of this size), or a
    /// dictionary shape of its own when `n` is over `MAX_PROPS`. `dict` asks for a dictionary shape in any case.
    fn new_shape(&mut self, n: usize, dict: bool) -> ShapeId {
        let keys = self.keys(n);
        let root = self.shapes.root(Layout::Object, None, 8);
        if n > MAX_PROPS || dict {
            let d = self.shapes.to_dict(root);
            keys.iter().for_each(|&k| {
                self.shapes.dict_add(d, k, WRITE);
            });
            d
        } else {
            keys.iter().fold(root, |s, &k| self.shapes.add(s, k, WRITE).unwrap())
        }
    }

    /// The shape of the objects with `n` slots. The first call makes it, with an object that every collection of the test marks.
    fn shape(&mut self, n: usize) -> ShapeId {
        if self.by_slots.len() <= n {
            self.by_slots.resize(n + 1, None);
        }
        if let Some(r) = self.by_slots[n] {
            return r;
        }
        let r = self.new_shape(n, false);
        self.by_slots[n] = Some(r);
        let pin = self.make(r);
        self.pins.push(ov(pin));
        r
    }

    fn make(&mut self, r: ShapeId) -> NonNull<u8> {
        Obj::new(&mut self.heap, &self.shapes, r).unwrap().cell()
    }

    fn set(&self, p: NonNull<u8>, i: usize, v: Value) {
        Obj::from_cell(p).set_slot(&self.shapes, i as u32, v)
    }

    /// An object with `n` slots, all `undefined`.
    fn obj(&mut self, n: usize) -> NonNull<u8> {
        let r = self.shape(n);
        self.make(r)
    }

    /// An object with `n` slots and a dictionary shape of its own (no other object has its shape).
    fn own(&mut self, n: usize) -> NonNull<u8> {
        let r = self.new_shape(n, true);
        self.make(r)
    }

    /// A weak table of `len` empty entries.
    fn table(&mut self, len: usize) -> NonNull<u8> {
        let p = self.heap.alloc(TABLE_ENTRIES + 16 * len).unwrap();
        // SAFETY: a new cell with room for the header and the length.
        unsafe {
            p.cast::<CellHeader>().write(CellHeader::new(Layout::WeakTable, 0));
            p.as_ptr().wrapping_add(TABLE_LEN).cast::<u32>().write(len as u32);
        }
        (0..len).for_each(|i| set_entry(p, i, Value::UNDEFINED, Value::UNDEFINED));
        p
    }

    fn collect(&mut self, roots: &[Value]) -> Stats {
        let pins = self.pins.clone();
        collect(&mut self.heap, &mut self.shapes, &mut self.atoms, &mut |m: &mut Marker| {
            m.values(&pins);
            m.values(roots);
        })
    }

    /// Marked cells and bytes of a collection with only the pins as roots.
    fn base(&mut self) -> Stats {
        self.collect(&[])
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn unreachable_cells_die_and_reachable_ones_stay() {
    let mut r = Realm::new();
    let base = r.base();
    let (a, b, c, dead) = (r.obj(2), r.obj(2), r.obj(1), r.obj(1));
    set(a, 0, ov(b));
    set(a, 1, Value::int(7));
    set(b, 0, ov(c));
    set(dead, 0, ov(c));
    let s = r.collect(&[ov(a)]);
    assert_eq!(s.marked, base.marked + 3);
    assert!(s.live_bytes > base.live_bytes);
    // The dead cell's memory comes back, and the live cells keep their content.
    let fresh: Vec<NonNull<u8>> = (0..50).map(|_| r.obj(1)).collect();
    assert!(![a, b, c].iter().any(|x| fresh.contains(x)));
    assert_eq!((get(a, 0), get(a, 1), get(b, 0)), (ov(b), Value::int(7), ov(c)));
    // Without the root, everything goes.
    r.collect(&[]);
    assert_eq!(r.collect(&[]).live_bytes, base.live_bytes);
}

#[test]
fn a_cycle_is_marked_once_and_dies_with_no_root() {
    let mut r = Realm::new();
    let base = r.base();
    let (a, b) = (r.obj(1), r.obj(2));
    set(a, 0, ov(b));
    set(b, 0, ov(a));
    set(b, 1, ov(b));
    assert_eq!(r.collect(&[ov(a)]).marked, base.marked + 2);
    assert_eq!(r.collect(&[]).live_bytes, base.live_bytes);
}

#[test]
fn a_long_chain_needs_one_chunk_and_a_wide_object_is_scanned_in_slices() {
    let mut r = Realm::new();
    let w = if MIRI { 4 } else { 16 } * SLICE + 10;
    // The pinned object of `w` slots has a long record too: one range waits (24 bytes) in every collection of this test.
    r.shape(w);
    r.shape(SLICE / 2);
    let base = r.base();
    let n = if MIRI { 300 } else { 200_000 };
    let head = r.obj(1);
    let mut tail = head;
    for _ in 1..n {
        let next = r.obj(1);
        set(tail, 0, ov(next));
        tail = next;
    }
    let s = r.collect(&[ov(head)]);
    assert_eq!(s.marked, base.marked + n as u64);
    assert_eq!(s.work_bytes, CHUNK * 8 + 24);

    // The wide object has an overflow record of `w - 8` values. The marker scans it `SLICE` values at a time, so the work list holds
    // at most `SLICE` cells (4 chunks, and the spare chunk) and two ranges wait (48 bytes: this record and the pinned one), however
    // many edges the record has. Without slices it would hold `w` entries.
    let slice_peak = (SLICE / CHUNK + 1) * CHUNK * 8 + 48;
    let wide = r.obj(w);
    for i in 0..w {
        let c = r.obj(1);
        r.set(wide, i, ov(c));
    }
    let s = r.collect(&[ov(wide)]);
    assert_eq!(s.marked, base.marked + w as u64 + 2);
    assert!(s.work_bytes >= SLICE * 8 && s.work_bytes <= slice_peak && slice_peak < w * 8, "{} bytes", s.work_bytes);

    // A leaf is marked and never pushed: with strings in its slots the object needs one entry for the record, and one range waits.
    let wide = r.obj(w);
    for i in 0..w {
        let s = Str::from_str(&mut r.heap, "a leaf string").unwrap();
        r.set(wide, i, s.value());
    }
    let s = r.collect(&[ov(wide)]);
    assert_eq!(s.marked, base.marked + w as u64 + 2);
    assert_eq!(s.work_bytes, CHUNK * 8 + 48);

    // A record of up to `SLICE` values waits for nothing.
    let narrow = r.obj(SLICE / 2);
    for i in 0..SLICE / 2 {
        let c = r.obj(1);
        r.set(narrow, i, ov(c));
    }
    let s = r.collect(&[ov(narrow)]);
    assert_eq!(s.marked, base.marked + SLICE as u64 / 2 + 2);
    assert!(s.work_bytes >= SLICE / 2 * 8 && s.work_bytes <= (SLICE / 2 / CHUNK + 1) * CHUNK * 8 + 24, "{} bytes", s.work_bytes);
}

#[test]
fn ropes_keep_their_parts_and_a_flattened_rope_drops_them() {
    let mut r = Realm::new();
    let base = r.base();
    let long = |h: &mut Heap, c: char| Str::from_str(h, &c.to_string().repeat(40)).unwrap();
    let (a, b, c) = (long(&mut r.heap, 'a'), long(&mut r.heap, 'b'), long(&mut r.heap, 'c'));
    let bc = Str::concat(&mut r.heap, b, c).unwrap();
    let abc = Str::concat(&mut r.heap, a, bc).unwrap();
    assert!(abc.is_rope() && bc.is_rope());
    assert_eq!(r.collect(&[abc.value()]).marked, base.marked + 5);
    assert_eq!(abc.children()[1], Some(bc));
    let flat = abc.flatten(&mut r.heap).unwrap();
    assert!(!abc.is_rope());
    // The root is `Indirect` now: it names the flat string only.
    assert_eq!(r.collect(&[abc.value()]).marked, base.marked + 2);
    assert_eq!(flat.len(), 120);
    assert_eq!(r.collect(&[]).live_bytes, base.live_bytes);
}

#[test]
fn a_shared_atom_is_skipped() {
    let mut r = Realm::new();
    let base = r.base();
    let shared = r.shared.intern(Chars::One(b"a name of the source text, shared by all realms")).unwrap();
    assert!(shared.is_shared());
    let before = r.shared.mapped_bytes();
    let local = Str::from_str(&mut r.heap, "a string of the realm, long enough to make a rope").unwrap();
    let rope = Str::concat(&mut r.heap, shared, local).unwrap();
    assert!(rope.is_rope());
    let o = r.obj(1);
    set(o, 0, shared.value());
    // The marked cells are the rope, the local string and the object. The shared atom is in another heap.
    assert_eq!(r.collect(&[rope.value(), ov(o)]).marked, base.marked + 3);
    assert_eq!(r.shared.mapped_bytes(), before);
    assert_eq!(rope.children(), [Some(shared), Some(local)]);
    assert_eq!(shared.chars().unwrap().len(), shared.len());
    assert_eq!(get(o, 0), shared.value());
}

#[test]
fn the_atom_table_drops_the_dead_atoms() {
    let mut r = Realm::new();
    let n = if MIRI { 40 } else { 2_000 };
    // The keys of the pinned shapes are atoms of the table, and they stay.
    let pinned = r.atoms.runtime().len();
    let all: Vec<Str> = (0..n).map(|i| r.atoms.intern(&mut r.heap, Chars::One(format!("atom number {i}").as_bytes())).unwrap()).collect();
    let live: Vec<Value> = all.iter().step_by(2).map(|s| s.value()).collect();
    let s = r.collect(&live);
    assert_eq!(s.atoms_dropped, n / 2);
    assert_eq!(r.atoms.runtime().len(), pinned + n - n / 2);
    for i in (0..n).step_by(2) {
        assert_eq!(r.atoms.runtime().find(&Chars::One(format!("atom number {i}").as_bytes())), Some(all[i]));
    }
    assert_eq!(r.atoms.runtime().find(&Chars::One(b"atom number 1")), None);
}

#[test]
fn the_host_clears_its_weak_references_after_the_marking() {
    struct Host {
        strong: Vec<Value>,
        weak: Vec<Value>,
    }
    impl Roots for Host {
        fn walk(&mut self, m: &mut Marker) {
            m.values(&self.strong);
        }
        fn weak(&mut self, live: &Live) {
            // SAFETY: each weak value is a cell of the heap that is not freed yet.
            self.weak.retain(|&v| unsafe { live.value(v) });
        }
    }
    let mut r = Realm::new();
    let (a, b, c) = (r.obj(1), r.obj(1), r.obj(1));
    set(a, 0, ov(c));
    let mut host = Host { strong: r.pins.clone(), weak: vec![ov(a), ov(b), ov(c), Value::int(3)] };
    host.strong.push(ov(a));
    collect(&mut r.heap, &mut r.shapes, &mut r.atoms, &mut host);
    assert_eq!(host.weak, [ov(a), ov(c), Value::int(3)]);
}

// ---- ephemerons

// The roots are marked first and traced after, the last root first. So a table that comes after the object that holds its key in the
// roots is traced before the key is marked, and its entry waits.

#[test]
fn a_value_lives_with_its_key_only() {
    let mut r = Realm::new();
    let base = r.base();
    let (t, h, k1, k2, v1, v2) = (r.table(2), r.obj(1), r.obj(0), r.obj(0), r.obj(1), r.obj(1));
    set(h, 0, ov(k1));
    set(v1, 0, Value::int(1));
    set_entry(t, 0, ov(k1), ov(v1));
    set_entry(t, 1, ov(k2), ov(v2));
    // The table is traced first: both entries wait, and `k1` releases its value.
    let s = r.collect(&[ov(h), ov(t)]);
    assert_eq!(s.marked, base.marked + 4);
    assert_eq!((s.waited, s.released, s.cleared), (2, 1, 1));
    assert_eq!(entry(t, 0), (ov(k1), ov(v1)));
    assert_eq!(entry(t, 1), (Value::UNDEFINED, Value::UNDEFINED));
    assert_eq!(get(v1, 0), Value::int(1));
    // The key is marked first: the table finds it marked and does not wait.
    let t2 = r.table(1);
    set_entry(t2, 0, ov(k1), ov(v1));
    let s = r.collect(&[ov(t2), ov(h)]);
    assert_eq!((s.marked, s.waited, s.released), (base.marked + 4, 0, 0));
    // A table that nothing keeps frees its entries, and the keys do not live because of it.
    assert_eq!(r.collect(&[ov(k1)]).marked, base.marked + 1);
}

#[test]
fn a_chain_of_tables_is_linear_in_any_order() {
    let mut r = Realm::new();
    let base = r.base();
    let n = if MIRI { 50 } else { 20_000 };
    let keys: Vec<NonNull<u8>> = (0..=n).map(|_| r.obj(0)).collect();
    let tables: Vec<NonNull<u8>> = (0..n).map(|_| r.table(1)).collect();
    for i in 0..n {
        set_entry(tables[i], 0, ov(keys[i]), ov(keys[i + 1]));
    }
    // The last table is traced first, then the one before it: every entry but the first waits, and each key releases the next.
    let roots: Vec<Value> = std::iter::once(ov(keys[0])).chain(tables.iter().map(|&t| ov(t))).collect();
    let s = r.collect(&roots);
    assert_eq!(s.marked, base.marked + (2 * n + 1) as u64);
    assert_eq!((s.waited, s.released, s.cleared), (n as u64 - 1, n as u64 - 1, 0));
    assert!(s.weak_bytes > 0);
    // The same chain with a gap: the keys after the gap die, and their entries are cleared.
    set_entry(tables[n / 2], 0, Value::UNDEFINED, Value::UNDEFINED);
    let s = r.collect(&roots);
    assert_eq!(s.marked, base.marked + (n + n / 2 + 1) as u64);
}

#[test]
fn a_value_that_names_its_key_does_not_keep_it_alive() {
    let mut r = Realm::new();
    let base = r.base();
    let (t, k, v) = (r.table(2), r.obj(1), r.obj(1));
    // `t[k] = k`, and `t[v] = [v]`.
    set_entry(t, 0, ov(k), ov(k));
    set(v, 0, ov(v));
    set_entry(t, 1, ov(v), ov(v));
    let s = r.collect(&[ov(t)]);
    assert_eq!((s.marked, s.cleared), (base.marked + 1, 2));
}

#[test]
fn a_table_reached_through_a_value_has_its_entries_released() {
    let mut r = Realm::new();
    let base = r.base();
    let (t1, t2, h1, h2, k1, k2, v) = (r.table(1), r.table(1), r.obj(1), r.obj(1), r.obj(0), r.obj(0), r.obj(1));
    set(h1, 0, ov(k1));
    set(h2, 0, ov(k2));
    set_entry(t1, 0, ov(k1), ov(t2));
    set_entry(t2, 0, ov(k2), ov(v));
    // `t1` is traced first and waits for `k1`. `t2` is marked when `k1` is, and then waits for nothing: `k2` is marked already.
    let s = r.collect(&[ov(h1), ov(h2), ov(t1)]);
    assert_eq!((s.marked, s.waited, s.released), (base.marked + 7, 1, 1));
    // Both tables are traced before their keys are marked.
    let s = r.collect(&[ov(h2), ov(t2), ov(h1), ov(t1)]);
    assert_eq!((s.marked, s.waited, s.released), (base.marked + 7, 2, 2));
    // Only `k1` is marked: `t2` lives and its entry is cleared.
    let s = r.collect(&[ov(h1), ov(t1)]);
    assert_eq!((s.marked, s.cleared), (base.marked + 4, 1));
}

#[test]
fn a_set_has_no_value_to_wait_for() {
    let mut r = Realm::new();
    let (t, k1, k2) = (r.table(3), r.obj(0), r.obj(0));
    set_entry(t, 0, ov(k1), Value::TRUE);
    set_entry(t, 1, ov(k2), Value::int(4));
    let s = r.collect(&[ov(t), ov(k1)]);
    assert_eq!((s.waited, s.cleared), (0, 1));
    assert_eq!(entry(t, 0), (ov(k1), Value::TRUE));
    assert_eq!(entry(t, 1), (Value::UNDEFINED, Value::UNDEFINED));
}

// ---- shapes

#[test]
fn a_prototype_that_only_a_shape_holds_survives() {
    let mut r = Realm::new();
    let base = r.base();
    let (proto, child) = (r.obj(1), r.obj(0));
    set(proto, 0, ov(child));
    // The object has no edge to `proto`: only its shape names it.
    let k = r.keys(1);
    let root = r.shapes.root(Layout::Object, Some(proto), 4);
    let shape = r.shapes.add(root, k[0], WRITE).unwrap();
    let o = r.make(shape);
    let s = r.collect(&[ov(o)]);
    // The object, its prototype, and what the prototype names.
    assert_eq!(s.marked, base.marked + 3);
    let fresh: Vec<NonNull<u8>> = (0..50).map(|_| r.obj(1)).collect();
    assert!(!fresh.contains(&proto) && !fresh.contains(&child));
    assert_eq!(get(proto, 0), ov(child));
    // Many objects of one shape make the prototype one entry of the work list, and the shape stays after the collection.
    let many: Vec<Value> = (0..20).map(|_| ov(r.make(shape))).collect();
    assert_eq!(r.collect(&many).marked, base.marked + 22);
    assert_eq!(r.shapes.get(shape).proto(), Some(proto));
    // With no object, the shape goes and so does the prototype.
    assert_eq!(r.collect(&[]).live_bytes, base.live_bytes);
}

#[test]
fn a_shape_that_no_marked_cell_uses_is_freed_and_its_id_is_reused() {
    let mut r = Realm::new();
    let base = r.base();
    let (live_key, dead_key) = (r.atoms.intern(&mut r.heap, Chars::One(b"a key of a live shape")).unwrap(), r.atoms.intern(&mut r.heap, Chars::One(b"a key of a dead shape")).unwrap());
    let k = r.keys(1);
    let root = r.shapes.root(Layout::Object, None, 2);
    let a = r.shapes.add(root, k[0], WRITE).unwrap();
    let live = r.shapes.add(a, live_key, WRITE).unwrap();
    // A branch of the tree: its table is its own, and its key is in no other shape.
    let dead = r.shapes.add(root, dead_key, WRITE).unwrap();
    let (kept, gone) = (r.make(live), r.make(dead));
    let (count, epoch) = (r.shapes.count(), r.shapes.epoch());
    let _ = gone;
    let s = r.collect(&[ov(kept)]);
    // `live` and its parents stay. `dead` is freed, with the atom that only it named.
    assert_eq!(r.shapes.count(), count - 1);
    assert_ne!(r.shapes.epoch(), epoch);
    assert_eq!((s.marked, s.atoms_dropped), (base.marked + 1, 1));
    assert_eq!(r.atoms.runtime().find(&Chars::One(b"a key of a dead shape")), None);
    assert_eq!(r.atoms.runtime().find(&Chars::One(b"a key of a live shape")), Some(live_key));
    assert_eq!(r.shapes.entries(live).map(|e| e.key()).collect::<Vec<_>>(), [k[0], live_key]);
    // The next shape takes the freed id.
    let next = r.shapes.add(root, k[0], A_W).unwrap();
    assert_eq!(next, dead);
    // A collection that frees nothing keeps the epoch.
    let reuse = r.make(next);
    let epoch = r.shapes.epoch();
    r.collect(&[ov(kept), ov(reuse)]);
    assert_eq!(r.shapes.epoch(), epoch);
}

// ---- stress

/// One random graph: its cells, its strong edges, its tables, and a model of what a collection keeps.
struct Graph {
    cells: Vec<Value>,
    /// 0 object, 1 flat string, 2 rope, 3 weak table.
    kind: Vec<u8>,
    edges: Vec<Vec<usize>>,
    entries: Vec<Vec<(usize, usize)>>,
    roots: Vec<usize>,
    /// The object has a dictionary shape of its own.
    own: Vec<bool>,
}

fn build(r: &mut Realm, rng: &mut Rng, n: usize, table_share: usize) -> Graph {
    let mut g = Graph { cells: Vec::new(), kind: Vec::new(), edges: vec![Vec::new(); n], entries: vec![Vec::new(); n], roots: Vec::new(), own: vec![false; n] };
    let mut strings = Vec::new();
    for i in 0..n {
        let roll = rng.below(100);
        let (v, k) = if roll < 55 {
            let n = 1 + rng.below(6);
            // Most objects share the shape of their size; one in eight has a dictionary shape of its own.
            g.own[i] = rng.below(8) == 0;
            let o = if g.own[i] { r.own(n) } else { r.obj(n) };
            set(o, 0, Value::int(i as i32));
            (ov(o), 0)
        } else if roll < 80 || strings.is_empty() && roll < 90 {
            let s = Str::from_str(&mut r.heap, &format!("{i:07}").repeat(5 + rng.below(4))).unwrap();
            strings.push(i);
            (s.value(), 1)
        } else if roll < 90 {
            let (ia, ib) = (strings[rng.below(strings.len())], strings[rng.below(strings.len())]);
            // SAFETY: both are strings of this heap.
            let (a, b) = unsafe { (Str::from_value(g.cells[ia]).unwrap(), Str::from_value(g.cells[ib]).unwrap()) };
            let rope = Str::concat(&mut r.heap, a, b).unwrap();
            g.edges[i] = vec![ia, ib];
            strings.push(i);
            (rope.value(), 2)
        } else if roll < 90 + table_share {
            (ov(r.table(1 + rng.below(6))), 3)
        } else {
            let o = r.obj(2);
            set(o, 0, Value::int(i as i32));
            (ov(o), 0)
        };
        g.cells.push(v);
        g.kind.push(k);
    }
    let keyable: Vec<usize> = (0..n).filter(|&i| g.kind[i] == 0 || g.kind[i] == 3).collect();
    for i in 0..n {
        let (_, p) = g.cells[i].as_pointer().unwrap();
        match g.kind[i] {
            0 => {
                let slots = r.shapes.of(unsafe { p.cast::<CellHeader>().read() }).slots as usize;
                for s in 1..slots {
                    if rng.below(10) < 6 {
                        let j = rng.below(n);
                        set(p, s, g.cells[j]);
                        g.edges[i].push(j);
                    } else {
                        set(p, s, Value::int(-1));
                    }
                }
            }
            3 => {
                let (len, _) = table(p);
                for e in 0..len {
                    let k = keyable[rng.below(keyable.len())];
                    let v = rng.below(n + 1);
                    // A value one past the last cell stands for a primitive.
                    set_entry(p, e, g.cells[k], if v == n { Value::int(5) } else { g.cells[v] });
                    g.entries[i].push((k, v));
                }
            }
            _ => {}
        }
    }
    g.roots = (0..n / 40 + 1).map(|_| rng.below(n)).collect();
    g
}

/// The cells a collection keeps, by the rules of the spec: strong edges, and the value of an entry whose key is kept.
fn model(g: &Graph) -> Vec<bool> {
    let n = g.cells.len();
    let mut keep = vec![false; n];
    let mut stack = g.roots.clone();
    loop {
        while let Some(i) = stack.pop() {
            if !keep[i] {
                keep[i] = true;
                stack.extend(&g.edges[i]);
            }
        }
        for i in (0..n).filter(|&i| keep[i]) {
            for &(k, v) in &g.entries[i] {
                if keep[k] && v < n && !keep[v] {
                    stack.push(v);
                }
            }
        }
        if stack.is_empty() {
            return keep;
        }
    }
}

fn check(r: &mut Realm, g: &Graph, keep: &[bool], base: Stats) -> Stats {
    let n = g.cells.len();
    let roots: Vec<Value> = g.roots.iter().map(|&i| g.cells[i]).collect();
    let s = r.collect(&roots);
    let kept = keep.iter().filter(|&&k| k).count() as u64;
    assert_eq!(s.marked, base.marked + kept);
    // The shapes of the dead objects are freed: the dictionary shapes that remain are those of the kept objects.
    assert_eq!(r.shapes.stats().dicts, (0..n).filter(|&i| keep[i] && g.own[i]).count());
    // Allocate over the dead cells: nothing live is overwritten.
    let spam: Vec<NonNull<u8>> = (0..n.min(if MIRI { 60 } else { 20_000 })).map(|i| r.heap.alloc([16, 40, 64, 200, 1000][i % 5]).unwrap()).collect();
    for &p in &spam {
        // SAFETY: the cell has at least 16 bytes.
        unsafe { std::ptr::write_bytes(p.as_ptr(), 0xAA, 16) };
    }
    for i in (0..n).filter(|&i| keep[i]) {
        let (_, p) = g.cells[i].as_pointer().unwrap();
        assert!(!spam.contains(&p), "the cell {i} was handed out again");
    }
    for i in (0..n).filter(|&i| keep[i]) {
        let (_, p) = g.cells[i].as_pointer().unwrap();
        match g.kind[i] {
            0 => assert_eq!(get(p, 0), Value::int(i as i32), "object {i}"),
            1 => assert_eq!(Str::from_cell(p).len() % 7, 0),
            2 => {
                let c = Str::from_cell(p).children();
                assert_eq!(c.map(|c| c.unwrap().value()), [g.cells[g.edges[i][0]], g.cells[g.edges[i][1]]]);
            }
            _ => {
                for (e, &(k, v)) in g.entries[i].iter().enumerate() {
                    let want = if keep[k] { (g.cells[k], if v == n { Value::int(5) } else { g.cells[v] }) } else { (Value::UNDEFINED, Value::UNDEFINED) };
                    assert_eq!(entry(p, e), want, "table {i} entry {e}");
                }
            }
        }
    }
    s
}

fn stress(seed: u64, n: usize, table_share: usize) -> Stats {
    let mut r = Realm::new();
    let mut rng = Rng(seed);
    let base = r.base();
    let start_mapped = r.heap.mapped_bytes();
    let shapes_at_start = r.shapes.count();
    assert!(base.live_bytes > 0);
    let g = build(&mut r, &mut rng, n, table_share);
    let keep = model(&g);
    let first = check(&mut r, &g, &keep, base);
    // A second collection of the same graph finds the same cells.
    let second = check(&mut r, &g, &keep, base);
    assert_eq!((second.marked, second.live_bytes), (first.marked, first.live_bytes));
    // The graph dies: the live bytes return to the start, and the heap gives its blocks back.
    r.collect(&[]);
    let end = r.collect(&[]);
    assert_eq!((end.marked, end.live_bytes), (base.marked, base.live_bytes));
    assert_eq!((r.shapes.stats().dicts, r.shapes.count()), (0, shapes_at_start));
    r.collect(&[]);
    assert!(r.heap.mapped_bytes() <= start_mapped + POOL_KEEP * BLOCK, "{} against {start_mapped}", r.heap.mapped_bytes());
    first
}

#[test]
fn a_random_graph_keeps_exactly_what_the_model_keeps() {
    let (n, seeds) = if MIRI { (150, 2) } else { (60_000, 3) };
    for seed in 1..=seeds {
        let s = stress(seed * 7919, n, 4);
        assert!(MIRI || s.waited > 0 && s.released > 0 && s.cleared > 0, "{s:?}");
    }
}

#[test]
fn a_random_graph_without_tables_has_a_small_work_list() {
    let n = if MIRI { 200 } else { 300_000 };
    let s = stress(42, n, 0);
    // Peak scratch of the work list against the cells that are alive.
    println!("stress n={n}: {s:?}");
    assert!(s.work_bytes * 20 < s.live_bytes.max(1) || MIRI, "{s:?}");
}

#[test]
fn live_bytes_return_to_the_start_after_each_round_of_a_growing_graph() {
    let mut r = Realm::new();
    let base = r.base();
    for round in 1..=if MIRI { 3 } else { 6 } {
        let n = round * if MIRI { 50 } else { 20_000 };
        let objs: Vec<NonNull<u8>> = (0..n).map(|_| r.obj(3)).collect();
        for (i, &o) in objs.iter().enumerate() {
            set(o, 0, ov(objs[(i + 1) % n]));
            set(o, 1, Str::from_str(&mut r.heap, "kept by the object").unwrap().value());
        }
        let s = r.collect(&[ov(objs[0])]);
        assert_eq!(s.marked, base.marked + 2 * n as u64);
        assert_eq!(r.collect(&[]).live_bytes, base.live_bytes);
    }
}

// ---- experiment S1.10: sticky marks, minor collections and the write barrier ----

impl Realm {
    fn minor(&mut self, roots: &[Value]) -> Stats {
        let pins = self.pins.clone();
        collect_minor(&mut self.heap, &mut self.shapes, &mut |m: &mut Marker| {
            m.values(&pins);
            m.values(roots);
        })
    }

    /// Make many small objects that hold ints: they are handed the cells that a collection freed, if any.
    fn scribble(&mut self, n: usize) -> Vec<NonNull<u8>> {
        (0..n)
            .map(|i| {
                let p = self.obj(4);
                (0..4).for_each(|k| set(p, k, Value::int(-(i as i32) - 1)));
                p
            })
            .collect()
    }
}

#[test]
fn a_minor_collection_keeps_what_an_old_object_names_after_a_store() {
    let mut r = Realm::new();
    let (old, dead_old) = (r.obj(2), r.obj(2));
    r.collect(&[ov(old), ov(dead_old)]);
    // `old` and `dead_old` are old now. Store a young object into `old` and drop `dead_old` from the roots.
    let (young, garbage) = (r.obj(2), r.obj(2));
    set(young, 0, Value::int(7));
    r.set(old, 0, ov(young));
    set(garbage, 0, Value::int(9));
    let s = r.minor(&[ov(old)]);
    // Only the young object that the old one names is marked.
    assert_eq!(s.marked, 1);
    let fresh = r.scribble(2000);
    assert!(fresh.contains(&garbage), "the young garbage is handed out again");
    assert!(!fresh.contains(&young) && !fresh.contains(&old), "the young survivor and the old object stay");
    assert!(!fresh.contains(&dead_old), "a minor collection does not free an old cell");
    assert_eq!(get(young, 0), Value::int(7));
    assert_eq!(get(old, 0), ov(young));
    // The survivor is old now: a store of another young object is remembered again.
    let young2 = r.obj(2);
    r.set(young, 1, ov(young2));
    r.minor(&[ov(old)]);
    let fresh = r.scribble(2000);
    assert!(!fresh.contains(&young2));
    assert_eq!(get(young, 1), ov(young2));
    // A full collection frees the old garbage that no root names.
    r.collect(&[ov(old)]);
    let fresh = r.scribble(4000);
    assert!(fresh.contains(&dead_old));
    assert!(!fresh.contains(&young2) && !fresh.contains(&young));
}

#[test]
fn the_barrier_covers_records_elements_ropes_and_shape_changes() {
    let mut r = Realm::new();
    let k = r.keys(12);
    let proto_root = r.shapes.root(Layout::Object, None, 2);
    let arr_shape = r.shapes.root(Layout::Indexed, None, 0);
    let plain = r.obj(0);
    let arr = Obj::array(&mut r.heap, &r.shapes, arr_shape, &[Value::int(1), Value::int(2)]).unwrap().cell();
    let wide = Obj::new(&mut r.heap, &r.shapes, proto_root).unwrap();
    let a = Str::from_str(&mut r.heap, &"a".repeat(40)).unwrap();
    let b = Str::from_str(&mut r.heap, &"b".repeat(40)).unwrap();
    let rope = Str::concat(&mut r.heap, a, b).unwrap();
    let roots = [ov(plain), ov(arr), wide.value(), rope.value()];
    r.collect(&roots);
    // Everything is old. Now each kind of store gets a young cell.
    // 1. An element store into the elements record.
    let ye = r.obj(1);
    set(ye, 0, Value::int(11));
    Obj::from_cell(arr).set_element(&mut r.heap, 1, ov(ye)).unwrap();
    // 2. A property add that grows the object into an overflow record, and a store into that record afterwards.
    let (yo, yr) = (r.obj(1), r.obj(1));
    set(yo, 0, Value::int(12));
    set(yr, 0, Value::int(13));
    for (i, &key) in k.iter().enumerate().take(5) {
        wide.add(&mut r.heap, &mut r.shapes, key, WRITE, &[if i == 0 { ov(yo) } else { Value::int(i as i32) }]).unwrap();
    }
    assert!(wide.overflow_cap(&r.shapes) > 0);
    let slot = wide.slot(&r.shapes, 4);
    assert_eq!(slot, Value::int(4));
    wide.set_slot(&r.shapes, 4, ov(yr));
    // 3. A rope that flattens: the old rope now names a young flat string.
    let flat = rope.flatten(&mut r.heap).unwrap();
    // 4. A prototype change on an old object: a young prototype that only the shape names.
    let proto = r.obj(1);
    set(proto, 0, Value::int(14));
    plain_set_proto(&mut r, plain, proto);
    let s = r.minor(&roots);
    // ye, yo, yr, the overflow record, the prototype and the flat string.
    assert_eq!(s.marked, 6);
    let fresh = r.scribble(4000);
    for live in [ye, yo, yr, proto, flat.cell()] {
        assert!(!fresh.contains(&live));
    }
    assert_eq!(get(ye, 0), Value::int(11));
    assert_eq!(get(yo, 0), Value::int(12));
    assert_eq!(get(yr, 0), Value::int(13));
    assert_eq!(get(proto, 0), Value::int(14));
    assert_eq!(Obj::from_cell(plain).proto(&r.shapes), Some(proto));
    assert_eq!(flat.len(), 80);
    // A full collection afterwards agrees: the same cells are alive.
    r.collect(&roots);
    let fresh = r.scribble(4000);
    for live in [ye, yo, yr, proto, flat.cell()] {
        assert!(!fresh.contains(&live));
    }
    assert_eq!(get(yr, 0), Value::int(13));
}

fn plain_set_proto(r: &mut Realm, o: NonNull<u8>, proto: NonNull<u8>) {
    Obj::from_cell(o).set_proto(&mut r.shapes, Some(proto));
}

/// A random graph of objects with 4 slots (slot 3 holds the id). Random stores, new objects, dropped roots, minor and full
/// collections. After each collection a model of the reachable set must still read back: freed cells are overwritten by new ones first.
fn gen_stress(seed: u64, steps: usize) {
    let mut r = Realm::new();
    let mut rng = Rng(seed | 1);
    // nodes[i] = (cell, edges: slot 0..3 -> node index or None); id = i.
    let mut nodes: Vec<(NonNull<u8>, [Option<usize>; 3])> = Vec::new();
    let mut roots: Vec<usize> = Vec::new();
    let new_node = |r: &mut Realm, nodes: &mut Vec<(NonNull<u8>, [Option<usize>; 3])>| {
        let p = r.obj(4);
        set(p, 3, Value::int(nodes.len() as i32));
        nodes.push((p, [None; 3]));
        nodes.len() - 1
    };
    let reach = |nodes: &Vec<(NonNull<u8>, [Option<usize>; 3])>, roots: &[usize]| {
        let mut seen = vec![false; nodes.len()];
        let mut stack: Vec<usize> = roots.to_vec();
        while let Some(i) = stack.pop() {
            if !std::mem::replace(&mut seen[i], true) {
                stack.extend(nodes[i].1.iter().flatten());
            }
        }
        seen
    };
    for step in 0..steps {
        for _ in 0..1 + rng.below(40) {
            match rng.below(10) {
                0..=3 => {
                    // A new object, stored into a random live node (or made a root).
                    let n = new_node(&mut r, &mut nodes);
                    let live = reach(&nodes, &roots);
                    let hosts: Vec<usize> = (0..n).filter(|&i| live[i]).collect();
                    if hosts.is_empty() || rng.below(8) == 0 {
                        roots.push(n);
                    } else {
                        let h = hosts[rng.below(hosts.len())];
                        let s = rng.below(3);
                        nodes[h].1[s] = Some(n);
                        r.set(nodes[h].0, s, ov(nodes[n].0));
                    }
                }
                4..=6 => {
                    // Overwrite a slot of a random live node with another live node or nothing.
                    let live = reach(&nodes, &roots);
                    let l: Vec<usize> = (0..nodes.len()).filter(|&i| live[i]).collect();
                    if l.is_empty() {
                        continue;
                    }
                    let (h, s) = (l[rng.below(l.len())], rng.below(3));
                    if rng.below(3) == 0 {
                        nodes[h].1[s] = None;
                        r.set(nodes[h].0, s, Value::UNDEFINED);
                    } else {
                        let t = l[rng.below(l.len())];
                        nodes[h].1[s] = Some(t);
                        r.set(nodes[h].0, s, ov(nodes[t].0));
                    }
                }
                7 if !roots.is_empty() => {
                    roots.swap_remove(rng.below(roots.len()));
                }
                _ => {
                    new_node(&mut r, &mut nodes);
                }
            }
        }
        let rv: Vec<Value> = roots.iter().map(|&i| ov(nodes[i].0)).collect();
        if rng.below(6) == 0 { r.collect(&rv) } else { r.minor(&rv) };
        // Dead nodes' cells may be freed: forget them in the model by never reading them; overwrite the freed memory.
        let live = reach(&nodes, &roots);
        let junk = r.scribble(300);
        for (i, &(p, e)) in nodes.iter().enumerate() {
            if !live[i] {
                continue;
            }
            assert!(!junk.contains(&p), "step {step}: live node {i} was handed out again");
            assert_eq!(get(p, 3), Value::int(i as i32), "step {step}: node {i}");
            for (s, t) in e.iter().enumerate() {
                let want = t.map_or(Value::UNDEFINED, |t| ov(nodes[t].0));
                assert_eq!(get(p, s), want, "step {step}: node {i} slot {s}");
            }
        }
        // A dead node is not read again: forget its edges, so a later store never names it.
        for i in 0..nodes.len() {
            if !live[i] {
                nodes[i].1 = [None; 3];
            }
        }
    }
}

#[test]
fn random_minor_and_full_collections_keep_every_reachable_node() {
    let (seeds, steps) = if MIRI { (1, 30) } else { (8, 400) };
    for seed in 0..seeds {
        gen_stress(0x9E37_79B9 + seed * 7919, steps);
    }
}

#[test]
fn a_store_into_a_long_old_record_remembers_its_card_only() {
    let mut r = Realm::new();
    for n in [5_000usize, if MIRI { 20_000 } else { 300_000 }] {
        // A collection frees a shape that no object uses, so each round makes its own.
        let arr_shape = r.shapes.root(Layout::Indexed, None, 0);
        let vals = vec![Value::int(1); n];
        let arr = Obj::array(&mut r.heap, &r.shapes, arr_shape, &vals).unwrap();
        r.collect(&[arr.value()]);
        // Stores of young objects at the start, the middle, the last index and one beyond the capacity of the record.
        let at = [0usize, 17, n / 2 + 3, n - 1];
        let young: Vec<NonNull<u8>> = at.iter().map(|&i| {
            let y = r.obj(1);
            set(y, 0, Value::int(i as i32));
            arr.set_element(&mut r.heap, i as u32, ov(y)).unwrap();
            y
        }).collect();
        let garbage = r.obj(1);
        let s = r.minor(&[arr.value()]);
        assert_eq!(s.marked, young.len() as u64, "only the stored objects are marked, and the old elements are not rescanned");
        let fresh = r.scribble(5000);
        assert!(fresh.contains(&garbage));
        for (&i, &y) in at.iter().zip(&young) {
            assert!(!fresh.contains(&y), "young object stored at {i} was freed");
            assert_eq!(get(y, 0), Value::int(i as i32));
            assert_eq!(arr.get_element(i as u32), Some(ov(y)));
        }
        // A second round on the same (now old) objects: stores into other cards are remembered again.
        let y2 = r.obj(1);
        set(y2, 0, Value::int(99));
        arr.set_element(&mut r.heap, (n / 4) as u32, ov(y2)).unwrap();
        r.minor(&[arr.value()]);
        let fresh = r.scribble(5000);
        assert!(!fresh.contains(&y2));
        assert_eq!(get(y2, 0), Value::int(99));
        r.collect(&[]);
    }
}
