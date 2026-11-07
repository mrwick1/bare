//! Inline cache tests through the evaluator (step A2). The expected texts come from Node 22. A debug build also compares every cache hit
//! with the plain walk (`Heap::ic_verify`), so these programs check the cache twice.

use super::*;
use crate::heap::Heap;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn run(it: &mut Interp, src: &str) -> String {
    match it.run_script(src) {
        Ok(v) => it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", it.get_str(v, "name"), it.get_str(v, "message")),
        Err(e) => format!("error:{e:?}"),
    }
}

/// Each program reads its member sites more than twice: the first run fills nothing, the second fills, the others hit.
const CASES: &[(&str, &str, &str)] = &[
    ("shadow", r#"var holder = { x: 1 }, mid = Object.create(holder), r = Object.create(mid), out = [];
function rd(o) { return o.x; }
for (var i = 0; i < 12; i++) {
  if (i == 3) holder.x = 2;
  if (i == 5) mid.x = 3;
  if (i == 7) r.x = 4;
  if (i == 9) delete r.x;
  if (i == 10) delete mid.x;
  if (i == 11) delete holder.x;
  out.push(rd(r));
}
out.join()"#, "1,1,1,2,2,3,3,4,4,3,2,"),
    ("methods", r#"class A { constructor(v) { this.v = v; } get g() { return this.v * 10; } m() { return this.v + 1; } }
class B extends A { m() { return this.v + 100; } }
var xs = [new A(1), new B(2), new A(3), new B(4)], s = [];
for (var k = 0; k < 3; k++) for (var o of xs) s.push(o.m() + o.g + (o.missing === undefined ? 0 : 1000));
A.prototype.missing = 1;
for (var o of xs) s.push(o.missing);
B.prototype.g = 5;
for (var o of xs) s.push(o.g);
s.join()"#, "12,122,34,144,12,122,34,144,12,122,34,144,1,1,1,1,10,20,30,40"),
    ("proto_swap", r#"var p1 = { x: 'p1' }, p2 = { x: 'p2' }, p3 = {}, mid = Object.create(p1), a = Object.create(mid), out = [];
function rd(o) { return o.x; }
for (var i = 0; i < 9; i++) {
  if (i == 3) Object.setPrototypeOf(mid, p2);
  if (i == 5) Object.setPrototypeOf(mid, p3);
  if (i == 7) Object.setPrototypeOf(a, p1);
  out.push(String(rd(a)));
}
out.join()"#, "p1,p1,p1,p2,p2,undefined,undefined,p1,p1"),
    ("getter_to_data", r#"var p = {}, r = Object.create(p), out = [], n = 0;
Object.defineProperty(p, 'x', { get() { return 'g' + (n++); }, configurable: true });
function rd(o) { return o.x; }
for (var i = 0; i < 8; i++) {
  if (i == 3) Object.defineProperty(p, 'x', { get() { return 'h'; }, configurable: true });
  if (i == 5) Object.defineProperty(p, 'x', { value: 'data', writable: true, configurable: true });
  if (i == 7) Object.defineProperty(p, 'x', { get() { return 'again'; }, configurable: true });
  out.push(rd(r));
}
out.join()"#, "g0,g1,g2,h,h,data,data,again"),
    ("proxy_chain", r#"var log = [], target = { x: 1 };
var px = new Proxy(target, { get(t, k, rcv) { log.push(String(k)); return k === 'x' ? 7 : undefined; } });
var r = Object.create(px), out = [];
function rd(o) { return o.x; }
for (var i = 0; i < 4; i++) out.push(rd(r));
r.x = 2;
for (var i = 0; i < 2; i++) out.push(rd(r));
out.join() + '|' + log.join()"#, "7,7,7,7,2,2|x,x,x,x"),
    ("poly", r#"function rd(o) { return o.x; }
function mk(n) { var o = {}; o['k' + n] = n; o.x = n * 2; return o; }
var two = [mk(0), mk(1)], five = [mk(0), mk(1), mk(2), mk(3), mk(4)], s = 0;
for (var i = 0; i < 50; i++) { for (var o of two) s += rd(o); for (var o of five) s += rd(o); }
s"#, "1100"),
    ("dict", r#"var o = { a: 1, b: 2, c: 3 }, out = [];
function rd(o) { return o.b; }
for (var i = 0; i < 6; i++) { if (i == 2) delete o.a; if (i == 4) o.b = 'B'; out.push(rd(o)); }
Object.freeze(o);
out.push(rd(o), rd(o));
out.join()"#, "2,2,2,2,B,B,B,B"),
    ("frozen_proto", r#"var p = { x: 1 }, r = Object.create(p), out = [];
function rd(o) { return o.x; }
for (var i = 0; i < 4; i++) { if (i == 2) Object.freeze(p); out.push(rd(r)); }
out.join()"#, "1,1,1,1"),
    ("fn_props", r#"function f(a, b) {} function g() {}
var out = [];
for (var i = 0; i < 4; i++) { out.push(f.length, g.length, f.name, typeof f.call, typeof f.prototype, f.prototype.constructor === f, g.prototype.constructor === g); }
out.join()"#, "2,0,f,function,object,true,true,2,0,f,function,object,true,true,2,0,f,function,object,true,true,2,0,f,function,object,true,true"),
    ("arrays", r#"var a = [1, 2, 3], b = [4], out = [];
for (var i = 0; i < 4; i++) { out.push(a.length, b.length, typeof a.push, a.missing); if (i == 1) { a.length = 1; Array.prototype.missing = 'm'; } }
delete Array.prototype.missing;
out.join()"#, "3,1,function,,3,1,function,,1,1,function,m,1,1,function,m"),
    ("strings", r#"var out = [];
for (var i = 0; i < 4; i++) { var s = 'ab' + i; out.push(s.length, typeof s.slice, s.missing); if (i == 1) String.prototype.missing = 'sm'; }
delete String.prototype.missing;
out.join()"#, "3,function,,3,function,,3,function,sm,3,function,sm"),
    ("globals", r#"var out = [];
for (var i = 0; i < 4; i++) { out.push(typeof Math.max, Math.PI > 3, typeof globalThis.Math, JSON.stringify.length); }
out.join()"#, "function,true,object,3,function,true,object,3,function,true,object,3,function,true,object,3"),
];

#[test]
fn programs_that_change_shapes_and_chains_match_node() {
    big(|| {
        let mut bad = Vec::new();
        for &(name, src, want) in CASES {
            let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
            let got = run(&mut it, src);
            if got != want {
                bad.push(format!("{name}: node {want}, bare {got}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    });
}

#[test]
fn the_counters_show_own_prototype_and_absent_hits() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        let before = it.ic_counts();
        let src = "class P { m() { return 1; } } var o = new P(); o.v = 2; var s = 0; for (var i = 0; i < 100; i++) s += o.m() + o.v + (o.none === undefined ? 1 : 0); s";
        assert_eq!(run(&mut it, src), "400");
        let c = it.ic_counts();
        let (reads, own, chain) = (c.reads - before.reads, c.own_hits - before.own_hits, c.heap.chain_hits - before.heap.chain_hits);
        // Three sites, 100 reads each: one first run and one fill for each site, the other 98 hit.
        assert_eq!(reads, 300);
        assert_eq!(own, 98, "o.v");
        assert_eq!(chain, 2 * 98, "o.m and o.none");
        assert_eq!(c.heap.fill_proto - before.heap.fill_proto, 1);
        assert_eq!(c.heap.fill_absent - before.heap.fill_absent, 1);
        assert_eq!(c.heap.fill_own - before.heap.fill_own, 1);
    });
}

#[test]
fn a_collect_between_runs_keeps_the_entries_right() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        let src = "function rd(o) { return o.x; } var keep = Object.create({ x: 1 }); var s = 0; for (var i = 0; i < 5; i++) s += rd(keep) + rd(Object.create({ x: 10 })); s";
        assert_eq!(run(&mut it, src), "55");
        it.collect(&[]).unwrap();
        let src = "var t = 0; for (var i = 0; i < 5; i++) t += rd(keep) + rd(Object.create({ x: 100 })); keep = null; t";
        assert_eq!(run(&mut it, src), "505");
        it.collect(&[]).unwrap();
        let src = "var u = 0; for (var i = 0; i < 5; i++) u += rd(Object.create({ x: 1000 })); u";
        assert_eq!(run(&mut it, src), "5000");
    });
}
