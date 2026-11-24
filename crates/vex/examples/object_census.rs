//! Layout census of `bare-js2` objects (plan step S1.9, M3 style): bytes of real objects by kind, on the synthetic workloads that the old
//! engine counts with `BARE_JS_STATS=1` (`tools/js/layout_census.sh`; `tools/js/shape_trace.js` and `tools/js/array_trace.js` are the same
//! workloads as scripts).
//!
//!   object_census trace [SCALE]     the React-like trace of `shape_census` with real objects (`Obj::add`, `Site`, `Obj::remove`), and
//!                                   the array `keep` that holds them all. 8,000 x SCALE props objects and elements, 4,000 hooks, 2,000 fibers.
//!   object_census array [N]         N small arrays `[i, i+1, i+2]` in one array, N integers pushed to one array, and N objects in one array.
//!
//! The bytes of a kind are the size classes of its cells and records (the allocator rounds up). One collection with every object
//! rooted checks the sum: its live bytes minus the live bytes before the first object (the keys and the prototype) equal the sum
//! of the kinds and `keep`, and the example stops if they differ. Garbage from growth (the records that an object outgrew) is freed by
//! that collection and is not counted.
use bare_js2::atoms::{Atoms, SharedAtoms};
use bare_js2::cell::Layout;
use bare_js2::gc::{Marker, Stats, collect};
use bare_js2::heap::Heap;
use bare_js2::object::{Obj, cell_bytes};
use bare_js2::shapes::{A_C, A_E, A_W, Shapes, Site};
use bare_js2::string::{Chars, Str};
use bare_js2::value::Value;
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
    CLASSES.iter().copied().find(|&c| c >= bytes).unwrap_or(bytes.next_multiple_of(65536))
}

struct World {
    heap: Heap,
    shapes: Shapes,
    atoms: Atoms,
    /// Rooted for the checking collection: the keys and the prototype.
    pins: Vec<Value>,
}

impl World {
    fn new() -> World {
        World { heap: Heap::default(), shapes: Shapes::default(), atoms: Atoms::new(Arc::new(SharedAtoms::default())), pins: Vec::new() }
    }

    fn key(&mut self, name: &str) -> Str {
        let k = self.atoms.intern(&mut self.heap, Chars::One(name.as_bytes())).expect("memory");
        self.pins.push(k.value());
        k
    }

    fn collect(&mut self, roots: &[Value]) -> Stats {
        let pins = self.pins.clone();
        collect(&mut self.heap, &mut self.shapes, &mut self.atoms, &mut |m: &mut Marker| {
            m.values(&pins);
            m.values(roots);
        })
    }

    /// The bytes of an object: its cell and its overflow record, by size class.
    fn bytes(&self, o: Obj) -> usize {
        let s = self.shapes.get(o.shape(&self.shapes));
        let over = o.overflow_cap(&self.shapes);
        class(cell_bytes(s.layout, s.inline())) + if over > 0 { class(8 + 8 * over) } else { 0 }
    }
}

#[derive(Default)]
struct Kind {
    objects: usize,
    slots: usize,
    bytes: usize,
    overflowed: usize,
}

fn trace(scale: usize) {
    let mut w = World::new();
    let proto = {
        let root = w.shapes.root(Layout::Object, None, 0);
        Obj::new(&mut w.heap, &w.shapes, root).expect("memory")
    };
    w.pins.push(proto.value());
    let names: Vec<Str> = NAMES.iter().map(|n| w.key(n)).collect();
    let element: Vec<Str> = ["$$typeof", "type", "key", "ref", "props", "_owner"].iter().map(|n| w.key(n)).collect();
    let hook: Vec<Str> = ["memoizedState", "baseState", "baseQueue", "queue", "next"].iter().map(|n| w.key(n)).collect();
    let fiber: Vec<Str> = (0..24).map(|i| w.key(&format!("f{i}"))).collect();
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
    let keep_shape = w.shapes.root(Layout::Indexed, Some(proto.cell()), 0);
    let keep = Obj::array(&mut w.heap, &w.shapes, keep_shape, &[]).expect("memory");
    w.pins.push(keep.value());
    let base = w.collect(&[]);
    // After the first collection: a shape that no object uses is freed by it.
    let element_shape = w.shapes.literal(Layout::Object, Some(proto.cell()), &element).expect("fits");
    let hook_shape = w.shapes.literal(Layout::Object, Some(proto.cell()), &hook).expect("fits");
    let mut kinds: Vec<(&str, Kind, Vec<Obj>)> = ["props", "element", "hook", "fiber"].map(|n| (n, Kind::default(), Vec::new())).into();
    let (mut props_site, mut fiber_site) = (Site::new(4), Site::new(8));
    let push = |w: &mut World, k: usize, o: Obj, kinds: &mut Vec<(&str, Kind, Vec<Obj>)>| {
        keep.set_element(&mut w.heap, keep.len(), o.value()).expect("memory");
        kinds[k].2.push(o);
    };
    for n in 0..8000 * scale {
        let v = Value::int(n as i32);
        // The props object: made empty with the site's inline slots, and a property at a time.
        let t = (rng.next() % 40).min(rng.next() % 40) as usize;
        let mut ks = templates[t].clone();
        if rng.next() % 8 == 0 {
            let (i, j) = (rng.next() as usize % ks.len(), rng.next() as usize % ks.len());
            ks.swap(i, j);
        }
        let root = w.shapes.root(Layout::Object, Some(proto.cell()), props_site.inline());
        let props = Obj::new(&mut w.heap, &w.shapes, root).expect("memory");
        for &k in &ks {
            props.add(&mut w.heap, &mut w.shapes, names[k], WRITE, &[v]).expect("memory");
        }
        props_site.observe(w.shapes.get(props.shape(&w.shapes)).slots);
        if rng.next() % 16 == 0 && ks.len() >= 2 {
            assert!(props.remove(&mut w.shapes, names[ks[0]]));
        }
        push(&mut w, 0, props, &mut kinds);
        // The element: a literal, so the shape has its final slots and the object is exact.
        let el = Obj::new(&mut w.heap, &w.shapes, element_shape).expect("memory");
        (0..6).for_each(|i| el.set_slot(&w.shapes, i, if i == 4 { props.value() } else { v }));
        push(&mut w, 1, el, &mut kinds);
        if n % 2 == 0 {
            let h = Obj::new(&mut w.heap, &w.shapes, hook_shape).expect("memory");
            (0..5).for_each(|i| h.set_slot(&w.shapes, i, v));
            push(&mut w, 2, h, &mut kinds);
        }
        if n % 4 == 0 {
            // The constructor sets 24 keys; every 20th fiber deletes `f3`, which sends it to dictionary mode.
            let root = w.shapes.root(Layout::Object, Some(proto.cell()), fiber_site.inline());
            let f = Obj::new(&mut w.heap, &w.shapes, root).expect("memory");
            for &k in &fiber {
                f.add(&mut w.heap, &mut w.shapes, k, WRITE, &[v]).expect("memory");
            }
            fiber_site.observe(w.shapes.get(f.shape(&w.shapes)).slots);
            if n % 80 == 0 {
                assert!(f.remove(&mut w.shapes, fiber[3]));
            }
            push(&mut w, 3, f, &mut kinds);
        }
    }
    for (_, k, objs) in &mut kinds {
        for &o in objs.iter() {
            k.objects += 1;
            k.slots += w.shapes.get(o.shape(&w.shapes)).slots as usize;
            k.bytes += w.bytes(o);
            k.overflowed += (o.overflow_cap(&w.shapes) > 0) as usize;
        }
    }
    // The cell of `keep` is in the base (it is pinned); its record is not.
    let keep_bytes = class(16 + 8 * keep.elements_cap());
    let all = w.collect(&[]);
    let live = all.live_bytes - base.live_bytes;
    let (mut objects, mut bytes) = (0, 0);
    for (name, k, _) in &kinds {
        println!("kind={name} objects={} avg_slots={:.1} overflowed={} bytes_per_object={:.1}", k.objects, k.slots as f64 / k.objects as f64, k.overflowed, k.bytes as f64 / k.objects as f64);
        objects += k.objects;
        bytes += k.bytes;
    }
    let st = w.shapes.stats();
    let shape_bytes = st.node_bytes + st.table_bytes + st.edge_bytes;
    println!("shapes={} dicts={} total_shape_bytes={shape_bytes} (shape ids are not in the heap)", st.shapes, st.dicts);
    println!("objects={objects} cell_bytes={bytes} keep_bytes={keep_bytes} keep_cap={} per_object={:.1} per_object_with_keep={:.1} per_object_with_keep_and_shapes={:.1}", keep.elements_cap(), bytes as f64 / objects as f64, (bytes + keep_bytes) as f64 / objects as f64, (bytes + keep_bytes + shape_bytes) as f64 / objects as f64);
    println!("live_bytes_of_the_collection_minus_the_start={live} sum_of_the_kinds_and_keep={}", bytes + keep_bytes);
    assert_eq!(live, bytes + keep_bytes, "the size-class sum and the heap disagree");
    println!("(the first collection, before any object, keeps the keys and the prototype: {} bytes)", base.live_bytes);
}

fn array(n: usize) {
    let mut w = World::new();
    let arr_shape = w.shapes.root(Layout::Indexed, None, 0);
    let leaf_shape = w.shapes.root(Layout::Object, None, 0);
    // The shapes must stay alive in each collection: an object of each shape is a root.
    let pin_a = Obj::array(&mut w.heap, &w.shapes, arr_shape, &[]).expect("memory");
    let pin_o = Obj::new(&mut w.heap, &w.shapes, leaf_shape).expect("memory");
    w.pins.extend([pin_a.value(), pin_o.value()]);
    let base = w.collect(&[]).live_bytes;

    // N arrays of 3 elements in one array.
    let keep = Obj::array(&mut w.heap, &w.shapes, arr_shape, &[]).expect("memory");
    for i in 0..n {
        let a = Obj::array(&mut w.heap, &w.shapes, arr_shape, &[Value::int(i as i32), Value::int(i as i32 + 1), Value::int(i as i32 + 2)]).expect("memory");
        keep.set_element(&mut w.heap, i as u32, a.value()).expect("memory");
    }
    let s = w.collect(&[keep.value()]);
    let small = w.bytes(pin_a) + class(16 + 8 * 3);
    println!("small_arrays n={n} bytes_per_array={small} (cell {} + record {}) keep_cap={} live_bytes_per_array_with_keep_slot={:.1} marked={}", w.bytes(pin_a), class(16 + 24), keep.elements_cap(), (s.live_bytes - base) as f64 / n as f64, s.marked);
    w.collect(&[]);

    // N integers pushed one at a time.
    let big = Obj::array(&mut w.heap, &w.shapes, arr_shape, &[]).expect("memory");
    for i in 0..n {
        big.set_element(&mut w.heap, i as u32, Value::int(i as i32)).expect("memory");
    }
    let s = w.collect(&[big.value()]);
    println!("pushed_ints n={n} live_bytes={} bytes_per_element={:.2} cap={} work_bytes={}", s.live_bytes - base, (s.live_bytes - base) as f64 / n as f64, big.elements_cap(), s.work_bytes);
    w.collect(&[]);

    // N empty objects in one array: the work list while the collector scans the record.
    let objs = Obj::array(&mut w.heap, &w.shapes, arr_shape, &[]).expect("memory");
    for i in 0..n {
        let o = Obj::new(&mut w.heap, &w.shapes, leaf_shape).expect("memory");
        objs.set_element(&mut w.heap, i as u32, o.value()).expect("memory");
    }
    let s = w.collect(&[objs.value()]);
    println!("array_of_objects n={n} live_bytes={} marked={} work_bytes={} slice={} (a push of every element at once needs {} bytes)", s.live_bytes - base, s.marked, s.work_bytes, bare_js2::gc::SLICE, n * 8);
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let n: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    match a.get(1).map(String::as_str) {
        Some("trace") => trace(n),
        Some("array") => array(if a.get(2).is_some() { n } else { 100_000 }),
        _ => panic!("usage: object_census trace [SCALE] | array [N]"),
    }
}
