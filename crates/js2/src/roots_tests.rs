// Tests of `roots.rs`. Miri runs them in CI, so the counts and the trigger shrink under it. The objects are the stub layout of the
// collector: a header and two value slots, with a shape of the two keys `k0` and `k1`. `Vm` is a fake interpreter: a realm and a
// frame stack, with the root walk of a real one (the frames and one pinned shape).
use super::*;
use crate::atoms::SharedAtoms;
use crate::cell::{CellHeader, Layout};
use crate::gc::Stats;
use crate::heap::{BLOCK, Trigger};
use crate::shapes::{A_C, A_E, A_W, ShapeId};
use crate::string::Chars;
use crate::value::Kind;
use std::collections::HashSet;
use std::sync::Arc;

const MIRI: bool = cfg!(miri);
/// Bytes of an object with two slots (header and 16): its class is 32.
const OBJ: usize = CellHeader::SIZE + 16;

struct Vm {
    heap: Heap,
    shapes: Shapes,
    atoms: Atoms,
    shape: ShapeId,
    frames: Frames,
    codes: Vec<RootMaps>,
    /// How many times a collection walked the roots.
    walks: usize,
}

fn ov(p: NonNull<u8>) -> Value {
    Value::pointer(Kind::Object, p)
}

fn cell(v: Value) -> NonNull<u8> {
    v.as_pointer().expect("a pointer").1
}

fn slot(p: NonNull<u8>, i: usize) -> *mut Value {
    p.as_ptr().wrapping_add(CellHeader::SIZE).cast::<Value>().wrapping_add(i)
}

fn get(v: Value, i: usize) -> Value {
    // SAFETY: a test object has two slots.
    unsafe { slot(cell(v), i).read() }
}

fn set(v: Value, i: usize, x: Value) {
    // SAFETY: as in `get`.
    unsafe { slot(cell(v), i).write(x) }
}

impl Vm {
    fn new(trigger: Trigger) -> Vm {
        let mut heap = Heap::default();
        heap.set_trigger(trigger);
        let (mut shapes, mut atoms) = (Shapes::default(), Atoms::new(Arc::new(SharedAtoms::default())));
        let mut shape = shapes.root(Layout::Object, None, 2);
        for k in ["k0", "k1"] {
            let key = atoms.intern(&mut heap, Chars::One(k.as_bytes())).unwrap();
            shape = shapes.add(shape, key, A_W | A_E | A_C).unwrap();
        }
        Vm { heap, shapes, atoms, shape, frames: Frames::default(), codes: Vec::new(), walks: 0 }
    }

    /// The roots of the interpreter: the frames, and the shape of the objects (no marked object may have it).
    fn roots(&mut self) -> (impl FnMut(&mut Marker) + use<'_>, &mut Heap, &mut Shapes, &mut Atoms) {
        let (frames, codes, shape, walks) = (&self.frames, &self.codes, self.shape, &mut self.walks);
        (
            move |m: &mut Marker| {
                *walks += 1;
                m.shape(shape);
                frames.walk(m, codes);
            },
            &mut self.heap,
            &mut self.shapes,
            &mut self.atoms,
        )
    }

    /// An object made inside the task: the allocation may collect first, with the frames as roots. `a` and `b` are written after the
    /// allocation, so they must be live registers (or not pointers) at the safepoint.
    fn obj(&mut self, a: Value, b: Value) -> Value {
        let header = self.shapes.header(self.shape);
        let (mut roots, heap, shapes, atoms) = self.roots();
        let p = alloc_in_task(heap, shapes, atoms, &mut roots, OBJ).unwrap();
        // SAFETY: a new cell of 24 bytes: the header and 2 slots.
        unsafe {
            p.cast::<CellHeader>().write(header);
            slot(p, 0).write(a);
            slot(p, 1).write(b);
        }
        ov(p)
    }

    fn collect(&mut self) -> Stats {
        let (mut roots, heap, shapes, atoms) = self.roots();
        collect(heap, shapes, atoms, &mut roots)
    }
}

fn small() -> Trigger {
    // A block is 64 KiB and the count grows by a block: the limits are a few blocks.
    Trigger { min: 256 << 10, slack: 256 << 10 }
}

#[test]
fn a_map_names_the_live_registers_of_a_safepoint() {
    let mut m = RootMaps::new(130);
    m.add(4, [0, 63, 64, 129]);
    m.add(9, []);
    m.add(12, 0..130);
    assert_eq!(m.find(4).unwrap().slots().collect::<Vec<_>>(), [0, 63, 64, 129]);
    assert_eq!(m.find(9).unwrap().slots().count(), 0);
    assert_eq!(m.find(12).unwrap().slots().collect::<Vec<_>>(), (0..130).collect::<Vec<_>>());
    assert!(m.find(5).is_none() && m.find(0).is_none() && m.find(13).is_none());
    // 3 words for each of 3 safepoints and 4 bytes for each pc: 84 bytes, and the growth of the two vectors.
    assert!(m.bytes() < 200, "{}", m.bytes());
}

#[test]
#[should_panic(expected = "order of pc")]
fn safepoints_come_in_the_order_of_pc() {
    let mut m = RootMaps::new(4);
    m.add(8, [0]);
    m.add(8, [1]);
}

#[test]
fn a_frame_is_not_cleared_when_it_is_pushed() {
    let mut f = Frames::default();
    f.push(0, 4);
    f.regs()[2] = Value::int(7);
    f.pop();
    f.push(0, 4);
    assert_eq!(f.regs()[2], Value::int(7), "a call does not fill the registers");
    f.pop();
    f.push_native(4);
    assert_eq!(f.regs()[2], Value::UNDEFINED, "a native frame holds values only");
}

#[test]
#[should_panic(expected = "a root map for each safepoint")]
fn a_safepoint_without_a_map_stops_the_collection() {
    let mut vm = Vm::new(small());
    vm.codes.push(RootMaps::new(2));
    vm.frames.push(0, 2);
    vm.frames.set_pc(5);
    vm.collect();
}

/// The check of S1.6: a collection inside a task, with live values in frames and dead values in them.
#[test]
fn a_collection_in_a_task_keeps_the_live_registers_and_frees_the_dead_ones() {
    let mut vm = Vm::new(small());
    // The stack holds the pointers of objects that die and are freed before the frames below exist. A frame has no fill, so those
    // pointers are still in its registers: a collector that read them would mark a freed cell.
    vm.frames.push_native(13);
    for i in 0..13 {
        let junk = vm.obj(Value::UNDEFINED, Value::int(i));
        vm.frames.regs()[i as usize] = junk;
    }
    vm.frames.pop();
    assert_eq!(vm.collect().marked, 0);
    let keys = vm.heap.marked_bytes(); // the atoms k0 and k1, kept by the shape

    // The caller: 6 registers, live at pc 10: 0 and 2. The callee: 5 registers, live at pc 3: 1 and 4.
    let (mut caller, mut callee) = (RootMaps::new(6), RootMaps::new(5));
    caller.add(10, [0, 2]);
    callee.add(3, [1, 4]);
    vm.codes = vec![caller, callee];
    vm.frames.push(0, 6);
    vm.frames.set_pc(10);
    let a2 = vm.obj(Value::UNDEFINED, Value::int(2));
    vm.frames.regs()[0] = a2; // `a2` lives in a register while the next objects are made
    let a = vm.obj(a2, Value::int(1));
    vm.frames.regs()[0] = a;
    set(a2, 0, a); // a cycle
    let b = vm.obj(a, Value::int(3));
    vm.frames.regs()[2] = b;
    // Dead values: the caller's register 1 was live once and its map no longer says so.
    let dead_in_caller = vm.obj(Value::UNDEFINED, Value::int(100));
    vm.frames.regs()[1] = dead_in_caller;
    vm.frames.push(1, 5);
    vm.frames.set_pc(3);
    let (c, d) = (vm.obj(Value::UNDEFINED, Value::int(4)), vm.obj(b, Value::int(5)));
    let dead_in_callee: Vec<Value> = (0..3).map(|i| vm.obj(Value::UNDEFINED, Value::int(200 + i))).collect();
    {
        let r = vm.frames.regs();
        (r[1], r[4]) = (c, d);
        (r[0], r[2], r[3]) = (dead_in_callee[0], dead_in_callee[1], dead_in_callee[2]);
    }
    // A native frame on top: both registers are roots.
    vm.frames.push_native(2);
    let n = vm.obj(Value::UNDEFINED, Value::int(6));
    vm.frames.regs()[0] = n;
    vm.frames.regs()[1] = Value::int(9);
    let dead: Vec<usize> = std::iter::once(dead_in_caller).chain(dead_in_callee).map(|v| cell(v).as_ptr() as usize).collect();

    let stats = vm.collect();
    // a, a2, b, c, d and n: nothing else is marked, and no stale register was read.
    assert_eq!(stats.marked, 6);
    assert_eq!(vm.heap.marked_bytes(), keys + 6 * 32);
    // The dead cells are free: the next allocations take them, and no allocation takes a live cell.
    let live: HashSet<usize> = [a, a2, b, c, d, n].map(|v| cell(v).as_ptr() as usize).into();
    let fresh: Vec<NonNull<u8>> = (0..100).map(|_| vm.heap.alloc(OBJ).unwrap()).collect();
    let fresh_set: HashSet<usize> = fresh.iter().map(|p| p.as_ptr() as usize).collect();
    assert!(live.is_disjoint(&fresh_set));
    for x in &dead {
        assert!(fresh_set.contains(x), "a dead cell is reused");
    }
    // Overwrite the free cells: the live objects keep their values and edges.
    for p in &fresh {
        // SAFETY: a new cell of 24 bytes.
        unsafe { p.as_ptr().write_bytes(0xAB, OBJ) };
    }
    assert_eq!((get(a, 0), get(a, 1)), (a2, Value::int(1)));
    assert_eq!((get(a2, 0), get(a2, 1)), (a, Value::int(2)));
    assert_eq!((get(b, 0), get(b, 1)), (a, Value::int(3)));
    assert_eq!((get(c, 1), get(d, 0), get(d, 1), get(n, 1)), (Value::int(4), b, Value::int(5), Value::int(6)));
    assert_eq!(vm.frames.depth(), 3);
}

#[test]
fn the_allocation_collects_inside_a_task_that_passes_the_trigger() {
    let mut vm = Vm::new(small());
    let mut maps = RootMaps::new(3);
    maps.add(7, [0, 1]);
    vm.codes.push(maps);
    vm.frames.push(0, 3);
    vm.frames.set_pc(7);
    // Register 0 holds a list that grows by one object at each step and is cut every 50 steps; register 1 holds a long-lived object;
    // register 2 is dead at the safepoint and holds a stale pointer to a cell that a collection freed.
    let keep = vm.obj(Value::UNDEFINED, Value::int(-1));
    vm.frames.regs()[1] = keep;
    let steps = if MIRI { 40_000 } else { 400_000 };
    let mut expected_len = 0;
    for i in 0..steps {
        let head = vm.frames.regs()[0];
        let o = vm.obj(head, Value::int(i));
        vm.frames.regs()[0] = o;
        expected_len += 1;
        if i % 50 == 49 {
            vm.frames.regs()[0] = Value::UNDEFINED;
            expected_len = 0;
        }
        vm.frames.regs()[2] = o;
    }
    let tail = steps % 50;
    assert_eq!(expected_len, tail);
    // The list in register 0 is whole.
    let mut n = 0;
    let mut v = vm.frames.regs()[0];
    while v.as_pointer().is_some() {
        assert_eq!(get(v, 1), Value::int(steps - 1 - n));
        v = get(v, 0);
        n += 1;
    }
    assert_eq!(n, tail);
    assert_eq!(get(keep, 1), Value::int(-1));
    // It collected inside the task, many times, and the heap stayed near the limits: `steps` objects of 32 bytes would be 12.8 MB.
    let limit = small().min + small().slack;
    // The count grows by a block, so a cycle allocates the limit rounded up to a block, plus the end of the block that crosses it.
    assert!(vm.walks as i64 >= steps as i64 * 32 / (limit + 2 * BLOCK) as i64 - 1, "{} collections", vm.walks);
    assert!(vm.heap.mapped_bytes() < limit + 4 * BLOCK * 16, "{} bytes mapped", vm.heap.mapped_bytes());
    // The last collection kept the keys, `keep` and one list of at most 50 objects.
    assert!(vm.heap.marked_bytes() <= (2 + 1 + 50) * 32, "{}", vm.heap.marked_bytes());
}

#[test]
fn a_task_below_the_trigger_does_not_collect() {
    let mut vm = Vm::new(small());
    vm.frames.push_native(1);
    // 4,096 objects of 32 bytes: 3 blocks, under the first limit (4 blocks).
    for i in 0..4096 {
        let o = vm.obj(Value::UNDEFINED, Value::int(i));
        vm.frames.regs()[0] = o;
    }
    assert_eq!(vm.walks, 0);
    assert!(!vm.heap.due());
}

#[test]
fn a_shape_with_no_object_survives_when_the_roots_pin_it() {
    let mut vm = Vm::new(small());
    let proto = vm.obj(Value::UNDEFINED, Value::UNDEFINED);
    vm.frames.push_native(1);
    vm.frames.regs()[0] = proto;
    let before = vm.shapes.count();
    // A shape with a prototype and no object. Without a pin the collection frees it, and its id may name another shape later.
    vm.shapes.root(Layout::Object, Some(cell(proto)), 0);
    assert_eq!(vm.shapes.count(), before + 1);
    let stats = vm.collect();
    assert_eq!(stats.marked, 1, "the prototype is marked through its own object");
    assert_eq!(vm.shapes.count(), before, "the lone shape is gone");
    let lone = vm.shapes.root(Layout::Object, Some(cell(proto)), 0);
    let mut pinned_roots = |m: &mut Marker| {
        m.shape(vm.shape);
        m.shape(lone);
        m.value(proto);
    };
    collect(&mut vm.heap, &mut vm.shapes, &mut vm.atoms, &mut pinned_roots);
    assert_eq!(vm.shapes.count(), before + 1, "a pinned shape stays");
    // A shape whose prototype only it names: the pin keeps the prototype cell too.
    let alone = vm.obj(Value::UNDEFINED, Value::int(1));
    let held = vm.shapes.root(Layout::Object, Some(cell(alone)), 0);
    let mut pin = |m: &mut Marker| {
        m.shape(vm.shape);
        m.shape(held);
    };
    let s = collect(&mut vm.heap, &mut vm.shapes, &mut vm.atoms, &mut pin);
    assert_eq!(s.marked, 1);
    assert_eq!(get(alone, 1), Value::int(1));
}
