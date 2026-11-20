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
