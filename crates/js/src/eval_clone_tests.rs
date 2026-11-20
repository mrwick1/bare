//! Structured clone in the old engine (`docs/design-host-clone.md`, plan step 3): `Engine::clone_out` and `Engine::clone_in` with a host
//! that has no page. The browser runs the whole set of cases through the DOM (`browser_clone_tests.rs`); these tests cover what the engine
//! alone must keep: the record, the cost model, the limits, a record read twice, the dense array tag and a damaged record.

use super::*;
use crate::heap::Heap;
use bare_js_api::clone::RecordWriter;
use bare_js_api::{CloneHost, CloneLimits, CloneOpts, Cloned, Engine, Local, Moved, Refused, Scope};
use std::any::Any;

/// A host with no page: nothing is a host object, a listed object can move when `movable` is set, and a refusal is a `TypeError`.
#[derive(Default)]
struct Host {
    movable: bool,
    refused: Vec<Refused>,
}

impl<'q> CloneHost<Interp<'q>> for Host {
    fn is_host_object(&self, _: &Value) -> bool {
        false
    }
    fn can_transfer(&self, _: &Value) -> bool {
        self.movable
    }
    fn transfer_out(&mut self, _: &Value) -> Box<dyn Any> {
        Box::new(7u32)
    }
    fn transfer_in<'s>(&mut self, s: &mut Scope<'s, Interp<'q>>, items: Vec<Box<dyn Any>>) -> Result<Vec<Local<'s>>, bare_js_api::Error<'s>> {
        assert_eq!(*items[0].downcast_ref::<u32>().unwrap(), 7);
        let o = s.new_object()?;
        let k = s.string("moved").unwrap();
        let v = s.number(7.0);
        s.define(o, k, v, bare_js_api::Attrs::ALL).unwrap();
        Ok(vec![o])
    }
    fn data_clone_error<'s>(&mut self, s: &mut Scope<'s, Interp<'q>>, what: Refused) -> bare_js_api::Error<'s> {
        self.refused.push(what);
        s.type_error("refused")
    }
}

fn limits() -> CloneLimits {
    CloneLimits { bytes: 1 << 20, depth: 16, nodes: 1 << 10, names: ["t-bytes", "t-depth", "t-nodes"] }
}

fn realm() -> Interp<'static> {
    Interp::realm(Heap::default(), EvalLimits { max_steps: 1_000_000, ..Default::default() }).unwrap()
}

fn script(it: &mut Interp, src: &str) -> Value {
    it.run_script(src).unwrap()
}

/// `clone_out` of `v` with `list`, as the old error.
fn clone_out(it: &mut Interp<'static>, v: Value, list: &[Value], l: CloneLimits, h: &mut Host) -> Result<Cloned, Error> {
    let base = it.stack_len();
    let root = it.raw_local(&v);
    let items: Vec<Local<'_>> = list.iter().map(|x| it.raw_local(x)).collect();
    let r = it.clone_out(root, &CloneOpts { limits: l, transfer: &items }, h).map_err(|e| it.api_unerr(e));
    it.stack_truncate(base);
    r
}

/// `clone_copy` of `v`, as the old error.
fn clone_copy(it: &mut Interp<'static>, v: Value, l: CloneLimits, h: &mut Host) -> Result<(Value, usize), Error> {
    let base = it.stack_len();
    let root = it.raw_local(&v);
    let r = it.clone_copy(root, &l, h).map(|(x, cost)| (it.raw(x), cost)).map_err(|e| it.api_unerr(e));
    it.stack_truncate(base);
    r
}

fn clone_in(it: &mut Interp<'static>, c: &mut Cloned, h: &mut Host) -> Result<(Value, Vec<Value>), Error> {
    let base = it.stack_len();
    let r = it.clone_in(c, h).map(|ci| (it.raw(ci.value), ci.hosts.iter().map(|&l| it.raw(l)).collect())).map_err(|e| it.api_unerr(e));
    it.stack_truncate(base);
    r
}

fn set(it: &mut Interp<'static>, name: &str, v: Value) {
    let (g, k) = (it.global_object(), it.heap.key_from_str(name).unwrap());
    it.define_data(g, k, v, true, true, true).unwrap();
}

/// Make `v` the global `r`, run `src` and show the answer.
fn show(it: &mut Interp<'static>, v: Value, src: &str) -> String {
    set(it, "r", v);
    let v = script(it, src);
    it.display(v)
}

#[test]
fn clone_round_trip_keeps_cycles_and_shared_objects() {
    let mut it = realm();
    let mut h = Host::default();
    let v = script(&mut it, "var s = {z: 1}; var o = {n: 1.5, t: 'héllo', self: null, a: [s, s, , 4], m: new Map([[1, s]]), d: new Date(5), e: -0}; o.self = o; o");
    let mut c = clone_out(&mut it, v, &[], limits(), &mut h).unwrap();
    let (r, hosts) = clone_in(&mut it, &mut c, &mut h).unwrap();
    assert!(hosts.is_empty());
    let text = show(&mut it, r, "[r !== o, r.self === r, r.a[0] === r.a[1], r.a[0] !== s, 2 in r.a, r.a.length, r.m.get(1) === r.a[0], r.d.getTime(), Object.is(r.e, -0), r.t].join()");
    assert_eq!(text, "true,true,true,true,false,4,true,5,true,héllo");
}

#[test]
fn clone_cost_is_the_model_of_the_old_walk() {
    let mut it = realm();
    let mut h = Host::default();
    // A node 48, the key `a` 16 + 2, the string 'bc' 16 + 4: 86.
    let v = script(&mut it, "({a: 'bc'})");
    let c = clone_out(&mut it, v, &[], limits(), &mut h).unwrap();
    assert_eq!(c.cost(), 86);
}

#[test]
fn clone_record_can_be_read_again_while_it_has_no_moved_item() {
    let mut it = realm();
    let mut h = Host::default();
    let v = script(&mut it, "({a: [1, 2], b: 'x'})");
    let mut c = clone_out(&mut it, v, &[], limits(), &mut h).unwrap();
    let (r1, _) = clone_in(&mut it, &mut c, &mut h).unwrap();
    let (r2, _) = clone_in(&mut it, &mut c, &mut h).unwrap();
    set(&mut it, "r1", r1);
    let text = show(&mut it, r2, "[r1 !== r, JSON.stringify(r1) === JSON.stringify(r), JSON.stringify(r)].join()");
    assert_eq!(text, r#"true,true,{"a":[1,2],"b":"x"}"#);
}

#[test]
fn clone_limits_name_their_errors() {
    let mut it = realm();
    let mut h = Host::default();
    let v = script(&mut it, "var a = []; for (var i = 0, p = a; i < 20; i++) { var q = []; p.push(q); p = q; } a");
    assert!(matches!(clone_out(&mut it, v, &[], limits(), &mut h), Err(Error::Limit(LimitKind::Host("t-depth")))));
    let v = script(&mut it, "[{}, {}, {}]");
    assert!(matches!(clone_out(&mut it, v, &[], CloneLimits { nodes: 3, ..limits() }, &mut h), Err(Error::Limit(LimitKind::Host("t-nodes")))));
    assert!(matches!(clone_out(&mut it, v, &[], CloneLimits { bytes: 100, ..limits() }, &mut h), Err(Error::Limit(LimitKind::Host("t-bytes")))));
}

#[test]
fn clone_refusals_go_to_the_host() {
    let mut it = realm();
    let mut h = Host::default();
    for (src, what) in [("({f: function () {}})", Refused::Function), ("Symbol('s')", Refused::Symbol), ("new Promise(function () {})", Refused::Object), ("(function () { var b = new ArrayBuffer(4); b.transfer(); return b; })()", Refused::DetachedBuffer)] {
        let v = script(&mut it, src);
        let e = clone_out(&mut it, v, &[], limits(), &mut h);
        assert!(matches!(e, Err(Error::Throw(_))), "{src}");
        assert_eq!(h.refused.pop(), Some(what), "{src}");
    }
}

#[test]
fn clone_moves_a_listed_buffer_and_a_listed_host_item() {
    let mut it = realm();
    let mut h = Host { movable: true, ..Default::default() };
    let v = script(&mut it, "var b = new Uint8Array([1, 2, 3]).buffer; var p = {}; ({b: b, v: new Uint8Array(b, 1, 2), p: p})");
    let list = [script(&mut it, "b"), script(&mut it, "p")];
    let mut c = clone_out(&mut it, v, &list, limits(), &mut h).unwrap();
    assert!(matches!(c.moved(), [Moved::Buffer(b), Moved::Host(_)] if b == &[1, 2, 3]));
    let n = script(&mut it, "b.byteLength");
    assert_eq!(it.display(n), "0", "the source buffer is detached");
    let (r, hosts) = clone_in(&mut it, &mut c, &mut h).unwrap();
    assert_eq!(hosts.len(), 1);
    let text = show(&mut it, r, "[r.b.byteLength, r.v.buffer === r.b, r.v[0], r.p.moved].join()");
    assert_eq!(text, "3,true,2,7");
}

#[test]
fn clone_listed_item_that_cannot_move_stops_before_any_item_moves() {
    let mut it = realm();
    let mut h = Host::default();
    let v = script(&mut it, "var b = new ArrayBuffer(4); var p = {}; 1");
    let list = [script(&mut it, "b"), script(&mut it, "p")];
    assert!(matches!(clone_out(&mut it, v, &list, limits(), &mut h), Err(Error::Throw(_))));
    assert_eq!(h.refused, [Refused::DetachedTransfer]);
    let n = script(&mut it, "b.byteLength");
    assert_eq!(it.display(n), "4");
}

#[test]
fn clone_dense_array_tag_and_holes() {
    let mut it = realm();
    let mut h = Host::default();
    // The step budget starts with the first script.
    script(&mut it, "0");
    let mut w = RecordWriter::new();
    w.array_dense(3);
    w.int(1);
    w.hole();
    w.string(&[120]);
    w.key(1, &[112]);
    w.boolean(true);
    w.end();
    let mut c = Cloned::new(w.finish(), Vec::new(), 0);
    let (r, _) = clone_in(&mut it, &mut c, &mut h).unwrap();
    assert_eq!(show(&mut it, r, "[r.length, 0 in r, 1 in r, r[2], r.p, Array.isArray(r)].join()"), "3,true,false,x,true,true");
}

#[test]
fn clone_damaged_record_is_an_error_and_not_a_panic() {
    let mut it = realm();
    let mut h = Host::default();
    let v = script(&mut it, "({a: [1, 'two', {b: 3n}], c: new Map([[1, 2]])})");
    let c = clone_out(&mut it, v, &[], limits(), &mut h).unwrap();
    let bytes = c.bytes().to_vec();
    for n in 0..bytes.len() {
        let mut cut = Cloned::new(bytes[..n].into(), Vec::new(), 0);
        assert!(matches!(clone_in(&mut it, &mut cut, &mut h), Err(Error::Unsupported(_))), "cut at {n}");
    }
    let mut trailing = Cloned::new([&bytes[..], &[0]].concat().into(), Vec::new(), 0);
    assert!(matches!(clone_in(&mut it, &mut trailing, &mut h), Err(Error::Unsupported(_))));
}

#[test]
fn clone_view_outside_its_buffer_is_a_damaged_record() {
    let mut it = realm();
    let mut h = Host::default();
    script(&mut it, "0");
    // `(type, byte offset, length)`: 11 is a `DataView`, 5 is an `Int32Array` (4 bytes each), 0 is an `Int8Array`.
    for (ty, off, len) in [(0u8, 3usize, 2usize), (0, 5, 0), (0, 0, 5), (5, 0, 2), (5, 4, 1), (11, usize::MAX, 2), (5, 0, usize::MAX)] {
        let mut w = RecordWriter::new();
        w.view(ty, off, len);
        w.buffer(&[1, 2, 3, 4]);
        w.view_done();
        let mut c = Cloned::new(w.finish(), Vec::new(), 0);
        assert!(matches!(clone_in(&mut it, &mut c, &mut h), Err(Error::Unsupported(_))), "type {ty}, offset {off}, length {len}");
    }
    // A view that fits is read.
    let mut w = RecordWriter::new();
    w.view(0, 1, 3);
    w.buffer(&[1, 2, 3, 4]);
    w.view_done();
    let mut c = Cloned::new(w.finish(), Vec::new(), 0);
    let (r, _) = clone_in(&mut it, &mut c, &mut h).unwrap();
    assert_eq!(show(&mut it, r, "[r.byteOffset, r.length, r[0]].join()"), "1,3,2");
}

// ---------- the fast path against the generic path (`docs/design-host-clone.md`, plan step 4) ----------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() >> 11) as usize % n
    }
    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

/// A random graph of 2 to 5 objects and arrays as a script. Getters (and some plain functions they call) delete, add, redefine and
/// overwrite keys of any object of the graph, also of the objects that the walk is inside of, and log their own run in `L`.
/// The script gives the root.
fn random_graph(seed: u64) -> String {
    let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let n = 2 + r.below(4);
    let array: Vec<bool> = (0..n).map(|i| i > 0 && r.chance(35)).collect();
    let mut getters = 0;
    let value = |r: &mut Rng| -> String {
        match r.below(20) {
            0 => "1".into(),
            1 => "2.5".into(),
            2 => "-0".into(),
            3 => "'s'".into(),
            4 => "'\\u00e9'".into(),
            5 => "'\\ud800'".into(),
            6 => "true".into(),
            7 => "null".into(),
            8 => "undefined".into(),
            9 => ["3n", "0n", "(-2n) ** 70n"][r.below(3)].into(),
            10 => "new Date(5)".into(),
            11 => format!("new Map([[1, o{}]])", r.below(n)),
            12 => format!("[o{}, {}]", r.below(n), r.below(9)),
            13 if r.chance(10) => "function () {}".into(),
            _ => format!("o{}", r.below(n)),
        }
    };
    let mutation = |r: &mut Rng, v: &dyn Fn(&mut Rng) -> String| -> String {
        // The root is the parent of most objects: a getter of a child changes it often.
        let (a, k) = (if r.chance(40) { 0 } else { r.below(n) }, r.below(5));
        match r.below(if array[a] { 12 } else { 7 }) {
            0 => format!("delete o{a}.k{k};"),
            1 | 2 => format!("o{a}.k{k} = {};", v(r)),
            3 => format!("o{a}.f{} = {};", r.below(3), v(r)),
            4 => format!("Object.defineProperty(o{a}, 'k{k}', {{value: {}, enumerable: {}, configurable: true, writable: true}});", v(r), r.chance(70)),
            5 => format!("Object.defineProperty(o{a}, 'k{k}', {{get: function () {{ L.push('h'); return {}; }}, enumerable: true, configurable: true}});", v(r)),
            6 => format!("o{a}.k{k} = o{};", r.below(n)),
            7 => format!("o{a}.length = {};", r.below(3)),
            8 => format!("o{a}.push({});", v(r)),
            9 => format!("delete o{a}[{}];", r.below(3)),
            10 => format!("o{a}.pop();"),
            _ => format!("o{a}[{}] = {};", r.below(4), v(r)),
        }
    };
    let mut src = String::from("L.length = 0;\n(function () {\nvar ");
    src += &(0..n).map(|i| format!("o{i} = {}", if array[i] { "[]" } else { "{}" })).collect::<Vec<_>>().join(", ");
    src += ";\n";
    // Plain properties and getters, in a random order, so that a getter sits before, between and after plain keys.
    for _ in 0..(n * 4) {
        // The root keeps plain data most of the time, so that the walk starts on the fast path at it; its children carry the getters.
        let i = if r.chance(35) { 0 } else { r.below(n) };
        let k = r.below(5);
        let line = match r.below(10) {
            0..=3 if array[i] => format!("o{i}.push({});", value(&mut r)),
            0..=5 => format!("o{i}.k{k} = {};", value(&mut r)),
            6 if array[i] => format!("o{i}[{}] = {};", r.below(5), value(&mut r)),
            6 | 7 => format!("o{i}.k{k} = {};", value(&mut r)),
            _ if i == 0 && r.chance(75) => format!("o0.k{k} = o{};", 1 + r.below(n - 1)),
            _ => {
                getters += 1;
                let body: String = (0..r.below(4)).map(|_| mutation(&mut r, &value)).collect::<Vec<_>>().join(" ");
                let throws = if r.chance(3) { "throw new Error('boom');" } else { "" };
                let ret = value(&mut r);
                format!("Object.defineProperty(o{i}, 'k{k}', {{get: function () {{ L.push('g{getters}'); {body} {throws} return {ret}; }}, enumerable: {}, configurable: true}});", r.chance(85))
            }
        };
        src += &line;
        src += "\n";
    }
    src += "return o0;\n})()";
    src
}

const DUMP: &str = "function dump(root) {
  var seen = [];
  function d(x) {
    if (x === null) return 'null';
    var t = typeof x;
    if (t === 'number') return Object.is(x, -0) ? '-0' : String(x);
    if (t === 'string') return JSON.stringify(x);
    if (t === 'function') return 'fn';
    if (t !== 'object') return String(x);
    var i = seen.indexOf(x);
    if (i >= 0) return '#' + i;
    seen.push(x);
    var out = (Array.isArray(x) ? 'A' + x.length : x instanceof Map ? 'M' : x instanceof Date ? 'D' + x.getTime() : 'O') + '{';
    if (x instanceof Map) x.forEach(function (v, k) { out += d(k) + '=>' + d(v) + ','; });
    var ks = Reflect.ownKeys(x);
    for (var j = 0; j < ks.length; j++) {
      var k = ks[j], p = Object.getOwnPropertyDescriptor(x, k);
      if (Array.isArray(x) && k === 'length') continue;
      out += String(k) + (p.enumerable ? ':' : '~') + ('get' in p || 'set' in p ? 'acc' : d(p.value)) + ',';
    }
    return out + '}';
  }
  return d(root);
}
globalThis.L = [];
0";

/// One case in one realm with the fast path off for the bits of `generic`: the answer as text (the copy, the graph after the walk, the
/// log of the getters, the cost) and the counts of the fast path. `fused` makes the copy with `clone_copy`, not with `clone_out` and `clone_in`.
fn run_graph(it: &mut Interp<'static>, src: &str, generic: u8, l: CloneLimits, fused: bool) -> (String, (u32, u32)) {
    let mut h = Host::default();
    CLONE_GENERIC.with(|c| c.set(generic));
    CLONE_FAST_COUNT.with(|c| c.set((0, 0)));
    let root = script(it, src);
    set(it, "orig", root);
    let copy = if fused {
        clone_copy(it, root, l, &mut h)
    } else {
        match clone_out(it, root, &[], l, &mut h) {
            Ok(mut c) => {
                let cost = c.cost();
                clone_in(it, &mut c, &mut h).map(|(v, _)| (v, cost))
            }
            Err(e) => Err(e),
        }
    };
    let count = CLONE_FAST_COUNT.with(|c| c.get());
    CLONE_GENERIC.with(|c| c.set(0));
    let head = match copy {
        Ok((v, cost)) => {
            set(it, "r", v);
            let d = script(it, "dump(r)");
            format!("{cost} {}", it.display(d))
        }
        Err(Error::Throw(v)) => format!("throw {}", it.display(v)),
        Err(Error::Limit(_)) => "limit".into(),
        Err(e) => format!("error {e:?}"),
    };
    let tail = script(it, "dump(orig) + ' ' + L.join()");
    (format!("{head}\n{}", it.display(tail)), count)
}

#[test]
fn clone_fast_path_gives_what_the_generic_path_gives() {
    let big = EvalLimits { max_steps: u64::MAX / 4, ..Default::default() };
    let mut realms: Vec<Interp<'static>> = Vec::new();
    let (mut fast_objects, mut fell_back, mut thrown, mut limited) = (0, 0, 0, 0);
    // `CLONE_FUZZ_SEEDS=20000` runs a longer search.
    let seeds = std::env::var("CLONE_FUZZ_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(1000u64);
    for seed in 0..seeds {
        // A realm takes a limited number of scripts, so a new set of six every 400 cases.
        if seed % 400 == 0 {
            realms = (0..6).map(|_| Interp::realm(Heap::default(), big).unwrap()).collect();
            realms.iter_mut().for_each(|it| {
                script(it, DUMP);
            });
        }
        let src = random_graph(seed);
        // Some cases run under small limits, to compare the order of the errors.
        let l = match seed % 5 {
            0 => CloneLimits { bytes: 300, ..limits() },
            1 => CloneLimits { depth: 2, ..limits() },
            2 => CloneLimits { nodes: 3, ..limits() },
            _ => limits(),
        };
        // Both off, fast write only, fast read only, both on; then the fused copy with both off and with both on.
        let modes = [GENERIC_OUT | GENERIC_IN, GENERIC_IN, GENERIC_OUT, 0, GENERIC_OUT | GENERIC_IN, 0];
        let answers: Vec<(String, (u32, u32))> = realms.iter_mut().zip(modes).enumerate().map(|(i, (it, m))| run_graph(it, &src, m, l, i >= 4)).collect();
        for (i, (a, _)) in answers.iter().enumerate().skip(1) {
            assert_eq!(a, &answers[0].0, "seed {seed}, mode {:?}\n{src}", modes[i]);
        }
        assert_eq!(answers[0].1, (0, 0), "the generic walk never starts the fast path");
        assert_eq!(answers[4].1, (0, 0), "the generic fused walk never starts the fast path");
        assert_eq!(answers[5].1, answers[3].1, "seed {seed}: the fused walk takes the fast path where the walk does\n{src}");
        fast_objects += answers[1].1.0;
        fell_back += answers[1].1.1;
        thrown += answers[0].0.starts_with("throw") as u32;
        limited += (answers[0].0.starts_with("limit") || answers[0].0.starts_with("error")) as u32;
    }
    eprintln!("fast objects {fast_objects}, handed back {fell_back}, throws {thrown}, limits or errors {limited}");
    // The cases must reach the paths that they are for.
    assert!(fast_objects > 1000, "objects on the fast path: {fast_objects}");
    assert!(fell_back > 40, "objects handed back to the generic path half way: {fell_back}");
    assert!(thrown > 5, "cases where a getter throws: {thrown}");
    assert!(limited > 100, "cases that stop at a limit: {limited}");
}

// ---------- the fused copy (`docs/design-host-clone.md`, plan step 5) ----------

/// The one case where `clone_copy` differs from `clone_out` and `clone_in`: the heap ends the script. The copy of a big graph needs more
/// memory than the heap has left. The two-step clone runs every getter, then fails to build the copy. The fused clone fails while it
/// builds, so a getter after that point has not run. Both fail with the same error, and a heap limit ends the script, so no page sees it.
#[test]
fn clone_copy_out_of_heap_stops_before_a_later_getter() {
    use crate::heap::HeapLimits;
    let big = EvalLimits { max_steps: u64::MAX / 4, ..Default::default() };
    let src = "globalThis.L = [];
        var rows = []; for (var i = 0; i < 300; i++) rows.push({a: i, b: [i, i]});
        var root = {rows: rows}; Object.defineProperty(root, 'z', {get: function () { L.push('z'); return 1; }, enumerable: true, configurable: true});
        root";
    // The heap has room for half of the copy.
    let (used, grew) = {
        let mut it = Interp::realm(Heap::default(), big).unwrap();
        let root = script(&mut it, src);
        let used = it.heap.used_bytes();
        clone_copy(&mut it, root, limits(), &mut Host::default()).unwrap();
        (used, it.heap.used_bytes() - used)
    };
    assert!(grew > 20_000, "{grew}");
    let limit = HeapLimits { max_bytes: used + grew / 2, ..Default::default() };
    let run = |fused: bool| {
        let mut it = Interp::realm(Heap::new(limit).unwrap(), big).unwrap();
        let mut h = Host::default();
        let root = script(&mut it, src);
        let r = if fused {
            clone_copy(&mut it, root, limits(), &mut h).map(|_| ())
        } else {
            clone_out(&mut it, root, &[], limits(), &mut h).and_then(|mut c| clone_in(&mut it, &mut c, &mut h)).map(|_| ())
        };
        // A number, so that the read needs no memory from a heap that is full.
        let log = script(&mut it, "L.length");
        (format!("{r:?}"), it.display(log))
    };
    let (two_step, fused) = (run(false), run(true));
    assert!(two_step.0.contains("Limit") || two_step.0.contains("Heap"), "{two_step:?}");
    assert_eq!(two_step.0, fused.0, "the same error");
    assert_eq!((two_step.1.as_str(), fused.1.as_str()), ("1", "0"), "the two-step clone ran the getter, the fused clone stopped before it");
}
