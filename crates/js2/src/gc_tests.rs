// Tests of `gc.rs`. Miri runs them in CI, so the counts shrink under it. The objects are the stub layout of the collector: a
// header and N value slots. Their shape has the N keys `k0`, `k1`, ... as data properties, so it has N slots.
use super::*;
use crate::atoms::SharedAtoms;
use crate::heap::{BLOCK, POOL_KEEP};
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
        let root = self.shapes.root(Layout::Object, None, 4);
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
        let pin = self.make(r, n);
        self.pins.push(ov(pin));
        r
    }

    fn make(&mut self, r: ShapeId, n: usize) -> NonNull<u8> {
        let p = self.heap.alloc((CellHeader::SIZE + 8 * n).max(16)).unwrap();
        // SAFETY: a new cell with room for the header.
        unsafe { p.cast::<CellHeader>().write(self.shapes.header(r)) };
        (0..n).for_each(|i| set(p, i, Value::UNDEFINED));
        p
    }

    /// An object with `n` slots, all `undefined`.
    fn obj(&mut self, n: usize) -> NonNull<u8> {
        let r = self.shape(n);
        self.make(r, n)
    }

    /// An object with `n` slots and a dictionary shape of its own (no other object has its shape).
    fn own(&mut self, n: usize) -> NonNull<u8> {
        let r = self.new_shape(n, true);
        self.make(r, n)
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
fn a_long_chain_needs_one_chunk_and_a_wide_object_needs_one_entry_for_each_edge() {
    let mut r = Realm::new();
    let w = if MIRI { 2 } else { 5 } * CHUNK + 10;
    let chunks = w.div_ceil(CHUNK);
    r.shape(w);
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
    assert_eq!(s.work_bytes, CHUNK * 8);

    let wide = r.obj(w);
    for i in 0..w {
        let c = r.obj(1);
        set(wide, i, ov(c));
    }
    let s = r.collect(&[ov(wide)]);
    assert_eq!(s.marked, base.marked + w as u64 + 1);
    assert_eq!(s.work_bytes, chunks * CHUNK * 8);

    // A leaf is marked and never pushed: the same object with strings in its slots needs the one chunk for the object itself.
    let wide = r.obj(w);
    for i in 0..w {
        let s = Str::from_str(&mut r.heap, "a leaf string").unwrap();
        set(wide, i, s.value());
    }
    let s = r.collect(&[ov(wide)]);
    assert_eq!(s.marked, base.marked + w as u64 + 1);
    assert_eq!(s.work_bytes, CHUNK * 8);
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
    let o = r.make(shape, 1);
    let s = r.collect(&[ov(o)]);
    // The object, its prototype, and what the prototype names.
    assert_eq!(s.marked, base.marked + 3);
    let fresh: Vec<NonNull<u8>> = (0..50).map(|_| r.obj(1)).collect();
    assert!(!fresh.contains(&proto) && !fresh.contains(&child));
    assert_eq!(get(proto, 0), ov(child));
    // Many objects of one shape make the prototype one entry of the work list, and the shape stays after the collection.
    let many: Vec<Value> = (0..20).map(|_| ov(r.make(shape, 1))).collect();
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
    let (kept, gone) = (r.make(live, 2), r.make(dead, 1));
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
    let reuse = r.make(next, 1);
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
