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

/// Member writes through site entries (step A3): stores, adds and the cases that must miss. The expected texts come from Node 22.
const WRITE_CASES: &[(&str, &str, &str)] = &[
    ("readonly", r#"var p = {}; Object.defineProperty(p, 'x', { value: 1, writable: false });
var out = [];
function wr(o, v) { o.x = v; return o.x; }
for (var i = 0; i < 4; i++) { var o = Object.create(p); out.push(wr(o, i), Object.keys(o).length); }
var q = { x: 0 }; for (var i = 0; i < 4; i++) { if (i == 2) Object.defineProperty(q, 'x', { writable: false }); out.push(wr(q, i + 10)); }
function swr(o, v) { 'use strict'; o.x = v; }
try { for (var i = 0; i < 3; i++) swr(Object.create(p), i); } catch (e) { out.push(e.constructor.name); }
out.join()"#, r#"1,0,1,0,1,0,1,0,10,11,11,11,TypeError"#),
    ("setter_chain", r#"var log = [], p = {}, out = [];
function wr(o, v) { o.y = v; }
for (var i = 0; i < 6; i++) {
  if (i == 3) Object.defineProperty(p, 'y', { set(v) { log.push(v); }, configurable: true });
  var o = Object.create(p); wr(o, i); out.push(Object.keys(o).join('') || '-');
}
for (var i = 0; i < 3; i++) { if (i == 1) Object.defineProperty(Object.prototype, 'z', { set(v) { log.push('z' + v); }, configurable: true }); var o = {}; o.z = i; out.push(Object.keys(o).length); }
delete Object.prototype.z;
out.join() + '|' + log.join()"#, r#"y,y,y,-,-,-,1,0,0|3,4,5,z1,z2"#),
    ("nonext", r#"var out = [];
function add(o, v) { o.k = v; return o.k; }
for (var i = 0; i < 6; i++) { var o = { a: 1 }; if (i >= 3) Object.preventExtensions(o); out.push(add(o, i)); }
function st(o, v) { o.a = v; return o.a; }
var o = { a: 0 }; for (var i = 0; i < 4; i++) { if (i == 2) Object.preventExtensions(o); out.push(st(o, i)); }
function sadd(o) { 'use strict'; o.k = 1; }
try { for (var i = 0; i < 4; i++) { var o = { a: 1 }; if (i == 3) Object.preventExtensions(o); sadd(o); } } catch (e) { out.push(e.constructor.name); }
out.join()"#, r#"0,1,2,,,,0,1,2,3,TypeError"#),
    ("frozen_sealed", r#"var out = [];
function st(o, v) { o.a = v; return o.a; }
function add(o, v) { o.b = v; return o.b; }
var f = { a: 1 }, s = { a: 1 };
for (var i = 0; i < 5; i++) {
  if (i == 2) { Object.freeze(f); Object.seal(s); }
  out.push(st(f, i), st(s, i), add({ a: 0 }, i), add(Object.isFrozen(f) ? f : {a:0}, i));
}
out.join()"#, r#"0,0,0,0,1,1,1,1,1,2,2,,1,3,3,,1,4,4,"#),
    ("define_on_cached", r#"var out = [];
function st(o, v) { o.x = v; return o.x; }
var a = { x: 0 }, b = { x: 0 };
for (var i = 0; i < 6; i++) {
  if (i == 3) Object.defineProperty(a, 'x', { get() { return 'g'; }, set(v) { out.push('set' + v); }, configurable: true });
  out.push(st(a, i), st(b, i));
}
var c = { x: 0 }, d = { x: 0 };
for (var i = 0; i < 4; i++) { if (i == 2) Object.defineProperty(c, 'x', { enumerable: false }); out.push(st(c, i), st(d, i), Object.keys(c).length); }
out.join()"#, r#"0,0,1,1,2,2,set3,g,3,set4,g,4,set5,g,5,0,0,1,1,1,1,2,2,0,3,3,0"#),
    ("array_length", r#"var out = [];
function setlen(a, n) { a.length = n; return a.length + ':' + a.join('.'); }
var a = [1, 2, 3, 4, 5, 6];
for (var i = 5; i >= 0; i -= 2) out.push(setlen(a, i));
var b = [1, 2]; Object.defineProperty(b, 'length', { writable: false });
for (var i = 0; i < 3; i++) out.push(setlen(b, i));
function tag(x, v) { x.tag = v; return x.tag; }
for (var i = 0; i < 4; i++) out.push(tag([i], i), tag({}, i), tag(function () {}, i));
out.join()"#, r#"5:1.2.3.4.5,3:1.2.3,1:1,2:1.2,2:1.2,2:1.2,0,0,0,1,1,1,2,2,2,3,3,3"#),
    ("proxies", r#"var log = [], out = [];
function wr(o, v) { o.x = v; }
var t = {}, px = new Proxy(t, { set(tt, k, v, r) { log.push(k + v); tt[k] = v * 10; return true; } });
for (var i = 0; i < 4; i++) wr(px, i);
out.push(t.x);
for (var i = 0; i < 4; i++) { var r = Object.create(px); wr(r, i); out.push(Object.keys(r).length); }
var plain = Object.create(new Proxy({}, {}));
for (var i = 0; i < 3; i++) { var r = Object.create(new Proxy({}, {})); wr(r, i); out.push(r.x); }
out.join() + '|' + log.join()"#, r#"30,0,0,0,0,0,1,2|x0,x1,x2,x3,x0,x1,x2,x3"#),
    ("constructors", r#"function P(i) { this.a = i; this.b = i * 2; this.c = 'c' + i; }
var all = []; for (var i = 0; i < 20; i++) all.push(new P(i));
class Q { constructor(i) { this.x = i; this.y = [i]; } }
var qs = []; for (var i = 0; i < 10; i++) qs.push(new Q(i));
var s = 0; for (var o of all) s += o.a + o.b; for (var q of qs) s += q.x + q.y[0];
[s, Object.keys(all[7]).join(), JSON.stringify(all[19]), JSON.stringify(qs[3])].join('|')"#, r#"660|a,b,c|{"a":19,"b":38,"c":"c19"}|{"x":3,"y":[3]}"#),
    ("update_compound", r#"var o = { n: 0, s: '' }, out = [];
for (var i = 0; i < 10; i++) { o.n++; o.s += i; ++o.n; o.n -= 1; }
out.push(o.n, o.s);
function C() {} C.prototype.count = 5;
var cs = []; for (var i = 0; i < 4; i++) { var c = new C(); c.count++; c.count += 2; cs.push(c.count, Object.keys(c).join()); }
out.push(cs.join(' '), C.prototype.count);
var g = { v: 1 }; for (var i = 0; i < 4; i++) { g.v ||= 9; g.w ??= i; g.v &&= g.v + 1; }
out.push(g.v, g.w);
var big = { b: 1n }; for (var i = 0; i < 3; i++) big.b++;
out.push(String(big.b));
out.join()"#, r#"10,0123456789,8 count 8 count 8 count 8 count,5,5,0,4"#),
    ("proto_swap", r#"var log = [], out = [];
var p1 = {}, p2 = { set q(v) { log.push(v); } };
function wr(o, v) { o.q = v; }
for (var i = 0; i < 6; i++) { var o = Object.create(p1); if (i >= 3) Object.setPrototypeOf(o, p2); wr(o, i); out.push(o.hasOwnProperty('q')); }
var r = Object.create(p1); wr(r, 1); wr(r, 2);
Object.setPrototypeOf(p1, p2);
for (var i = 0; i < 3; i++) wr(Object.create(p1), 'x' + i);
out.join() + '|' + log.join()"#, r#"true,true,true,false,false,false|3,4,5,x0,x1,x2"#),
    ("key_order", r#"var out = [];
function mk(k1, k2) { var o = {}; o.a = 1; o[k1] = 2; o.b = 3; o[k2] = 4; return Object.keys(o).join(''); }
for (var i = 0; i < 5; i++) out.push(mk(i % 2 ? 'x' : 'y', 'z'));
function del(o) { o.t = 1; delete o.t; o.t = 2; o.u = 3; return Object.keys(o).join('') + o.t; }
for (var i = 0; i < 4; i++) out.push(del({ s: 0 }));
out.join()"#, r#"aybz,axbz,aybz,axbz,aybz,stu2,stu2,stu2,stu2"#),
];

#[test]
fn programs_that_write_members_match_node() {
    big(|| {
        let mut bad = Vec::new();
        for &(name, src, want) in WRITE_CASES {
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
fn the_counters_show_store_and_add_hits() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        let before = it.ic_counts();
        let src = "function P(i) { this.a = i; this.b = i; } var o = { n: 0 }, s = 0; for (var i = 0; i < 100; i++) { o.n = i; s += new P(i).b; } s + o.n";
        assert_eq!(run(&mut it, src), "5049");
        let c = it.ic_counts();
        // Three write sites, 100 writes each: one first run and one fill for each site, the other 98 hit.
        assert_eq!(c.store_hits - before.store_hits, 98, "o.n");
        assert_eq!(c.heap.add_hits - before.heap.add_hits, 2 * 98, "this.a and this.b");
        assert_eq!(c.heap.fill_store - before.heap.fill_store, 1);
        assert_eq!(c.heap.fill_add - before.heap.fill_add, 2);
    });
}
