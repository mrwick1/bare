//! Shape census of `bare-vex` on a synthetic React-like trace (plan step S1.8). `tools/js/shape_trace.js` is the same trace as a script: run it
//! in the old engine with `BARE_JS_STATS=1` (`tools/js/layout_census.sh`, `docs/internal/progress.md` S1.8) to compare. The two must make the same objects
//! with the same keys in the same order; both use xorshift32 from the same seed, and the order of the calls to it is the same.
//!
//!   shape_census [SCALE]      SCALE (default 1) multiplies the number of objects: 8,000 x SCALE elements and props objects, 2,000 fibers, 4,000 hooks
//!
//! The trace per step n: a props object (a template of 1 to 8 of 16 names, chosen with a bias to the first templates; one in 8 swaps two keys;
//! one in 16 deletes its first key, which keeps a small object in the tree), an element (an object literal of 6 keys that holds it), every
//! second step a hook (a literal of 5 keys), every fourth step a fiber (a constructor that sets 24 keys; every 20th fiber deletes `f3`, which sends it to dictionary mode). Every object stays alive.
//!
//! Object cells are made for real, but there is no object module yet (S1.9): a cell is the 8-byte header, the inline slots of its shape and, in an
//! object that has more slots than that, a pointer word to an overflow record (an 8-byte header and the extra slots). The census prints two
//! layouts: A, the pointer word exists only in an object that overflows; B, every object has the pointer word.
use bare_vex::atoms::{Atoms, SharedAtoms};
use bare_vex::cell::Layout;
use bare_vex::heap::Heap;
use bare_vex::shapes::{Shapes, ShapeId, Site, A_C, A_E, A_W};
use bare_vex::string::{Chars, Str};
use std::ptr::NonNull;
use std::sync::Arc;

const WRITE: u8 = A_W | A_E | A_C;
const NAMES: [&str; 16] = ["className", "id", "onClick", "onChange", "style", "children", "href", "value", "disabled", "title", "role", "tabIndex", "name", "type", "placeholder", "ref"];
const CLASSES: [usize; 16] = [16, 32, 48, 64, 80, 96, 128, 160, 192, 256, 384, 512, 1024, 2048, 4096, 8192];

struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}

fn class(bytes: usize) -> usize {
    *CLASSES.iter().find(|&&c| c >= bytes).expect("small")
}

#[derive(Default)]
struct Kind {
    objects: usize,
    slots: usize,
    layout_a: usize,
    layout_b: usize,
    overflowed: usize,
    inline: usize,
}

struct Census {
    heap: Heap,
    shapes: Shapes,
    atoms: Atoms,
    kinds: Vec<(&'static str, Kind)>,
    cells: Vec<(ShapeId, NonNull<u8>)>,
}

impl Census {
    fn key(&mut self, name: &str) -> Str {
        self.atoms.intern(&mut self.heap, Chars::One(name.as_bytes())).expect("memory")
    }

    /// A finished object of kind `k` with shape `s`: its cell, and the overflow record when the shape has more slots than inline ones.
    fn finish(&mut self, k: usize, s: ShapeId) {
        let sh = self.shapes.get(s);
        let (slots, inline) = (sh.slots as usize, sh.inline() as usize);
        let over = slots.saturating_sub(inline);
        let cell = self.heap.alloc(8 + 8 * inline + if over > 0 { 8 } else { 0 }).expect("memory");
        // SAFETY: a cell of at least 8 bytes: the header.
        unsafe { cell.as_ptr().cast::<bare_vex::cell::CellHeader>().write(self.shapes.header(s)) };
        if over > 0 {
            self.heap.alloc(8 + 8 * over).expect("memory");
        }
        let kind = &mut self.kinds[k].1;
        kind.objects += 1;
        kind.slots += slots;
        kind.inline += inline;
        kind.overflowed += (over > 0) as usize;
        kind.layout_a += class(8 + 8 * inline + if over > 0 { 8 } else { 0 }) + if over > 0 { class(8 + 8 * over) } else { 0 };
        kind.layout_b += class(8 + 8 * inline + 8) + if over > 0 { class(8 + 8 * over) } else { 0 };
        self.cells.push((s, cell));
    }
}

fn main() {
    let scale: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let mut heap = Heap::default();
    let atoms = Atoms::new(Arc::new(SharedAtoms::default()));
    let proto = heap.alloc(16).expect("memory");
    let mut c = Census {
        heap,
        shapes: Shapes::default(),
        atoms,
        kinds: ["props", "element", "hook", "fiber"].map(|n| (n, Kind::default())).into(),
        cells: Vec::new(),
    };
    let names: Vec<Str> = NAMES.iter().map(|n| c.key(n)).collect();
    let element: Vec<Str> = ["$$typeof", "type", "key", "ref", "props", "_owner"].iter().map(|n| c.key(n)).collect();
    let hook: Vec<Str> = ["memoizedState", "baseState", "baseQueue", "queue", "next"].iter().map(|n| c.key(n)).collect();
    let fiber: Vec<Str> = (0..24).map(|i| c.key(&format!("f{i}"))).collect();
    let mut rng = Rng(2463534242);
    let templates: Vec<Vec<usize>> = (0..40)
        .map(|_| {
            let size = 1 + rng.next() as usize % 8;
            let mut idx: Vec<usize> = (0..16).collect();
            for i in 0..size {
                let j = i + rng.next() as usize % (16 - i);
                idx.swap(i, j);
            }
            idx.truncate(size);
            idx
        })
        .collect();
    let element_shape = c.shapes.literal(Layout::Object, Some(proto), &element).expect("fits");
    let hook_shape = c.shapes.literal(Layout::Object, Some(proto), &hook).expect("fits");
    let (mut props_site, mut fiber_site) = (Site::new(4), Site::new(8));
    for n in 0..8000 * scale {
        // The props object.
        let t = (rng.next() % 40).min(rng.next() % 40) as usize;
        let mut ks = templates[t].clone();
        if rng.next() % 8 == 0 {
            let (i, j) = (rng.next() as usize % ks.len(), rng.next() as usize % ks.len());
            ks.swap(i, j);
        }
        let mut s = c.shapes.root(Layout::Object, Some(proto), props_site.inline());
        for &k in &ks {
            s = c.shapes.add(s, names[k], WRITE).expect("fits");
        }
        props_site.observe(c.shapes.get(s).slots);
        if rng.next() % 16 == 0 && ks.len() >= 2 {
            s = c.shapes.remove(s, names[ks[0]]).expect("present").shape;
        }
        c.finish(0, s);
        c.finish(1, element_shape);
        if n % 2 == 0 {
            c.finish(2, hook_shape);
        }
        if n % 4 == 0 {
            let mut s = c.shapes.root(Layout::Object, Some(proto), fiber_site.inline());
            for &k in &fiber {
                s = c.shapes.add(s, k, WRITE).expect("fits");
            }
            fiber_site.observe(c.shapes.get(s).slots);
            if n % 80 == 0 {
                s = c.shapes.remove(s, fiber[3]).expect("present").shape;
            }
            c.finish(3, s);
        }
    }
    // A collection in which every object is live frees the shapes that only an object that went to dictionary mode used (its tree shape).
    let before = c.shapes.count();
    c.heap.start_mark();
    for &(s, cell) in &c.cells {
        // SAFETY: a live cell of the heap.
        unsafe { c.heap.mark(cell) };
        c.shapes.note_live(&mut c.heap, c.shapes.header(s));
    }
    c.shapes.finish_marks(&mut c.heap);
    c.heap.finish_mark();
    println!("shapes before the collection={before} after={}", c.shapes.count());
    let st = c.shapes.stats();
    println!("shapes={} dicts={} tables={} entries={} entry_room={} node_bytes={} table_bytes={} edge_bytes={} total_shape_bytes={}", st.shapes, st.dicts, st.tables, st.entries, st.entry_room, st.node_bytes, st.table_bytes, st.edge_bytes, st.node_bytes + st.table_bytes + st.edge_bytes);
    let (mut all, mut a, mut b, mut slots) = (0, 0, 0, 0);
    for (name, k) in &c.kinds {
        println!("kind={name} objects={} avg_slots={:.1} avg_inline={:.1} overflowed={} bytes_per_object_A={:.1} bytes_per_object_B={:.1}", k.objects, k.slots as f64 / k.objects as f64, k.inline as f64 / k.objects as f64, k.overflowed, k.layout_a as f64 / k.objects as f64, k.layout_b as f64 / k.objects as f64);
        all += k.objects;
        a += k.layout_a;
        b += k.layout_b;
        slots += k.slots;
    }
    let shape_bytes = st.node_bytes + st.table_bytes + st.edge_bytes;
    println!("objects={all} slots={slots} cell_bytes_A={a} cell_bytes_B={b} per_object_A={:.1} per_object_B={:.1} per_object_with_shapes_A={:.1} per_object_with_shapes_B={:.1}", a as f64 / all as f64, b as f64 / all as f64, (a + shape_bytes) as f64 / all as f64, (b + shape_bytes) as f64 / all as f64);
    println!("mapped={} (blocks of 64 KiB, includes the cells of the model)", c.heap.mapped_bytes());
}
