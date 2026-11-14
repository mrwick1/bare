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

/// Object literals, closures and the lazy `prototype` (step A4): each program runs every literal and closure site more than once, so the
/// later runs take the preset shapes. The output is from Node 22.
const LITERAL_CASES: &[(&str, &str, &str)] = &[
("computed keys", r#"var out = [], s = Symbol("s");
function f(k) { return { a: 1, [k]: 2, b: 3 }; }
var ks = ["x", "a", "0", s, "b", "x", 7];
for (var i = 0; i < ks.length; i++) { var o = f(ks[i]); out.push(Object.keys(o).join("") + ":" + o.a + o.b + (o[ks[i]])); }
out.join()"#, r#"axb:132,ab:232,0ab:132,ab:132,ab:133,axb:132,7ab:132"#),
("__proto__ in a literal", r#"var out = [], P = { p: 1 };
function f(p) { return { __proto__: p, x: 1 }; }
function g(p) { return { ["__proto__"]: p, x: 1 }; }
function h(__proto__) { return { __proto__, x: 1 }; }
function q(p) { return { "__proto__": p, x: 1 }; }
for (var i = 0; i < 3; i++) {
  var a = f(i == 1 ? null : P), b = g(P), c = h(P), d = q(5);
  out.push([Object.getPrototypeOf(a) === P, Object.getPrototypeOf(a) === null, a.p, Object.keys(a).join(""),
    Object.getPrototypeOf(b) === Object.prototype, Object.keys(b).join(""), b.__proto__ === P,
    Object.keys(c).join(""), Object.getPrototypeOf(c) === Object.prototype, Object.getPrototypeOf(d) === Object.prototype, Object.keys(d).join("")].join(" "));
}
out.join("|")"#, r#"true false 1 x true __proto__x true __proto__x true true x|false true  x true __proto__x true __proto__x true true x|true false 1 x true __proto__x true __proto__x true true x"#),
("spread", r#"var out = [];
function f(src) { return { a: 1, ...src, b: 2 }; }
var srcs = [{}, { c: 3 }, { a: 9 }, { b: 8, d: 4 }, null, "xy", [5]];
for (var i = 0; i < srcs.length; i++) { var o = f(srcs[i]); out.push(Object.keys(o).map(k => k + o[k]).join("")); }
out.join()"#, r#"a1b2,a1c3b2,a9b2,a1b2d4,a1b2,0x1ya1b2,05a1b2"#),
("getters and setters", r#"var out = [];
function f(v) { var box = v; return { get a() { return box; }, set a(x) { box = x * 2; }, b: v, get c() { return this.b + 1; } }; }
for (var i = 0; i < 4; i++) {
  var o = f(i); o.a = i + 1;
  var d = Object.getOwnPropertyDescriptor(o, "a");
  out.push(Object.keys(o).join("") + o.a + o.c + typeof d.get + typeof d.set + d.enumerable + d.configurable);
}
out.join()"#, r#"abc21functionfunctiontruetrue,abc42functionfunctiontruetrue,abc63functionfunctiontruetrue,abc84functionfunctiontruetrue"#),
("duplicate keys", r#"var out = [];
function f(i) { return { a: 1, b: i, a: 3 }; }
function g(i) { return { a: i, get a() { return 7; } }; }
function h(i) { return { get a() { return 7; }, a: i }; }
function m(i) { return { a: 1, a() { return i; } }; }
for (var i = 0; i < 3; i++) {
  var o = f(i), p = g(i), q = h(i), r = m(i);
  out.push(Object.keys(o).join("") + o.a + o.b + p.a + q.a + typeof Object.getOwnPropertyDescriptor(q, "a").value + r.a() + r.a.name);
}
out.join()"#, r#"ab3070number0a,ab3171number1a,ab3272number2a"#),
("one site, different key sets", r#"var out = [];
function f(k, v) { return { x: v, [k]: v, y: v }; }
function n(i) { return { 1: i, b: i, "2": i, c: i }; }
for (var i = 0; i < 6; i++) {
  var o = f(i % 2 ? "x" : "z" + (i % 3), i), q = n(i);
  out.push(Object.keys(o).join("") + JSON.stringify(q));
}
out.join("|")"#, r#"xz0y{"1":0,"2":0,"b":0,"c":0}|xy{"1":1,"2":1,"b":1,"c":1}|xz2y{"1":2,"2":2,"b":2,"c":2}|xy{"1":3,"2":3,"b":3,"c":3}|xz1y{"1":4,"2":4,"b":4,"c":4}|xy{"1":5,"2":5,"b":5,"c":5}"#),
("values, names and later changes", r#"var out = [];
function f(i) { return { f: function () {}, g: () => 1, h() {}, C: class {}, n: i, s: "v" + i }; }
for (var i = 0; i < 3; i++) {
  var o = f(i);
  out.push([o.f.name, o.g.name, o.h.name, o.C.name, o.n, o.s, Object.keys(o).join("")].join(" "));
  o.extra = 1; delete o.f; o.n = 99; Object.freeze(o);
}
var t = 0;
function thr(i) { return { a: i, b: (function () { if (i == 2) throw new Error("x" + i); return i; })(), c: 3 }; }
for (var i = 0; i < 4; i++) { try { var o = thr(i); t += o.a + o.b + o.c; } catch (e) { out.push(e.message); } }
out.push(t);
var objs = []; for (var i = 0; i < 3; i++) objs.push({ p: i, q: { r: i, s: [i] } });
objs[0].p = "w"; out.push(JSON.stringify(objs));
out.join("|")"#, r#"f g h C 0 v0 fghCns|f g h C 1 v1 fghCns|f g h C 2 v2 fghCns|x2|17|[{"p":"w","q":{"r":0,"s":[0]}},{"p":1,"q":{"r":1,"s":[1]}},{"p":2,"q":{"r":2,"s":[2]}}]"#),
("the lazy prototype of a function", r#""use strict"; var out = [];
function mk() { return function F(a, b) { this.a = a; }; }
for (var i = 0; i < 3; i++) {
  var f = mk();
  out.push(Object.getOwnPropertyNames(f).join(",") + " " + ("prototype" in f) + " " + Object.keys(f).length + " " + f.length + f.name);
  var d = Object.getOwnPropertyDescriptor(f, "prototype");
  if (i == 0) { out.push(typeof d.value + d.writable + d.enumerable + d.configurable + (d.value.constructor === f) + Object.keys(d.value).length); }
  if (i == 1) { var np = { k: 1 }; f.prototype = np; out.push((new f(1).k) + "" + (Object.getPrototypeOf(new f(2)) === np)); }
  if (i == 2) { try { delete f.prototype; out.push("deleted"); } catch (e) { out.push(e.constructor.name + (typeof f.prototype)); } }
  var g = mk(); var x = new g(5); out.push(x.a + "" + (Object.getPrototypeOf(x) === g.prototype) + (g.prototype.constructor === g));
  var h = mk(); out.push(String(Reflect.deleteProperty(h, "prototype")) + Object.getOwnPropertyNames(h).join(","));
  var k = mk(); Object.defineProperty(k, "prototype", { value: 3 }); out.push(k.prototype + "" + Object.getOwnPropertyDescriptor(k, "prototype").writable);
  var z = mk(); Object.defineProperty(z, "name", { value: "Z" }); z.prototype.q = 4; out.push(z.name + new z().q + Object.getOwnPropertyNames(z).join(","));
}
var arrow = () => 1, am = async function () {}, gen = function* () {}, meth = { m() {} }.m, ag = async function* () {};
out.push(["prototype" in arrow, "prototype" in am, Object.getPrototypeOf(am) === Object.getPrototypeOf(async function () {}),
  typeof gen.prototype, Object.getPrototypeOf(gen.prototype) === Object.getPrototypeOf(function* () {}).prototype, "prototype" in meth, typeof ag.prototype,
  Object.getOwnPropertyNames(gen).join(","), Object.getOwnPropertyNames(am).join(","), Object.getOwnPropertyNames(ag).join(",")].join(" "));
out.join("|")"#, r#"length,name,prototype true 0 2F|objecttruefalsefalsetrue0|5truetrue|falselength,name,prototype|3true|Z4length,name,prototype|length,name,prototype true 0 2F|1true|5truetrue|falselength,name,prototype|3true|Z4length,name,prototype|length,name,prototype true 0 2F|TypeErrorobject|5truetrue|falselength,name,prototype|3true|Z4length,name,prototype|false false true object true false object length,name,prototype length,name length,name,prototype"#),
];

#[test]
fn programs_with_literals_and_closures_match_node() {
    big(|| {
        let mut bad = Vec::new();
        for &(name, src, want) in LITERAL_CASES {
            let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
            let got = run(&mut it, src);
            if got != want {
                bad.push(format!("{name}: node {want}, bare {got}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    });
}

/// The RSS of this process in KiB, from `/proc/self/status` (0 where it is missing).
fn rss_kib() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix("VmRSS:")).and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok()).unwrap_or(0)
}

/// Step A6: 1 M objects with distinct key sets, with a collect after each 10,000. The shapes of dead objects are freed, so the shape
/// table stays small, and new objects still get shapes (the table never fills).
#[test]
fn a_soak_of_objects_with_distinct_key_sets_keeps_the_shape_table_small() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        run(&mut it, "var n = 0; function mk(m) { var s = 0; for (var i = 0; i < m; i++) { var o = {}; o['k' + (n++)] = i; o.v = 1; s += o.v; } return s; }");
        let (mut rss10, mut most) = (0, 0);
        for round in 0..100 {
            let before = it.heap.footprint().shapes;
            assert_eq!(run(&mut it, "mk(10000)"), "10000");
            let made = it.heap.footprint().shapes - before;
            assert!(made >= 19_000, "round {round}: only {made} new shapes");
            it.collect(&[]).unwrap();
            most = most.max(it.heap.footprint().shapes);
            if round == 10 {
                rss10 = rss_kib();
            }
        }
        eprintln!("soak: shapes after collect at most {most}, RSS {rss10} KiB at round 10, {} KiB at round 100", rss_kib());
        assert!(most < 10_000, "{most} shapes after a collect");
    });
}

/// Step A6: a collect that sweeps the shapes while every kind of entry is filled (own, prototype and absent reads, stores, adds, a
/// literal site, the traps of a proxy handler). The entries are reset, the sites give the same results and fill again.
#[test]
fn a_shape_sweep_with_live_caches_resets_them_and_the_sites_stay_right() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        let src = r#"function Pt(v) { this.v = v; }
Pt.prototype.get = function () { return this.v; };
function rd(o) { return o.v + o.get() + (o.none === undefined ? 0 : 100); }
function wr(o, x) { o.v = x; return o.v; }
function lit(i) { return { a: i, b: i + 1 }; }
var px = new Proxy({}, { get(t, k) { return k === 'q' ? 7 : undefined; } });
function run() { var s = 0; for (var i = 0; i < 5; i++) { var o = new Pt(i); s += rd(o) + wr(o, i * 2); var l = lit(i); s += l.a + l.b + Object.keys(l).length + px.q; } return s; }
var n = 0; function churn(m) { for (var i = 0; i < m; i++) { var o = {}; o['c' + (n++)] = i; } }
run() + run() + run()"#;
        assert_eq!(run(&mut it, src), "330");
        assert_eq!(run(&mut it, "churn(10000)"), "undefined");
        let sweeps = it.heap.shape_sweeps();
        it.collect(&[]).unwrap();
        assert_eq!(it.heap.shape_sweeps(), sweeps + 1, "the collect sweeps the shapes");
        assert_eq!(run(&mut it, "run()"), "110");
        let before = it.ic_counts();
        assert_eq!(run(&mut it, "run() + '' + Object.keys(lit(3)).join() + lit(3).b"), "110a,b4");
        let c = it.ic_counts();
        assert!(c.own_hits > before.own_hits && c.store_hits > before.store_hits, "the entries fill again");
        assert!(c.heap.add_hits > before.heap.add_hits && c.heap.chain_hits > before.heap.chain_hits);
    });
}

/// Step B3: a collect that sweeps the shapes while the entries and the global cells of bytecode code are filled. They are empty after
/// it, and the code gives the same results and fills them again.
#[test]
fn a_shape_sweep_resets_the_caches_of_bytecode_code() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        it.set_exec(Exec::Byte);
        // The entries that are not empty, and the cells that hold a position.
        let filled = |it: &Interp| {
            let empty = format!("{:?}", Ic::EMPTY);
            let (mut ics, mut cells) = (0, 0);
            for c in it.bc.codes.iter().flat_map(|s| s.iter()) {
                if let BcSlot::Byte(c) = c {
                    ics += c.code.ics.iter().filter(|ic| format!("{:?}", ic.get()) != empty).count();
                    cells += c.cells.iter().filter(|cell| cell.pos.get() != u32::MAX).count();
                }
            }
            (ics, cells)
        };
        let src = r#"function Pt(v) { this.v = v; }
var total = 0;
function rd(o) { return o.v + (o.none === undefined ? 0 : 100); }
function wr(o, x) { o.v = x; return o.v; }
function run() { var s = 0; for (var i = 0; i < 5; i++) { var o = new Pt(i); s += rd(o) + wr(o, i * 2); total = total + 1; } return s + total; }
var n = 0; function churn(m) { for (var i = 0; i < m; i++) { var o = {}; o['c' + (n++)] = i; } }
run() + run()"#;
        assert_eq!(run(&mut it, src), "75");
        let (ics, cells) = filled(&it);
        assert!(ics > 0 && cells > 0, "the code fills its entries ({ics}) and cells ({cells})");
        assert_eq!(run(&mut it, "churn(10000)"), "undefined");
        let sweeps = it.heap.shape_sweeps();
        it.collect(&[]).unwrap();
        assert_eq!(it.heap.shape_sweeps(), sweeps + 1, "the collect sweeps the shapes");
        assert_eq!(filled(&it), (0, 0), "the sweep empties the entries and the cells");
        assert_eq!(run(&mut it, "run()"), "45");
        let (ics, cells) = filled(&it);
        assert!(ics > 0 && cells > 0, "the entries and cells fill again");
        assert!(it.bc_stats().runs > 0 && it.bc_stats().compiled > 0);
    });
}

/// Step B3: a function made in a `with` body reads names through the object, so it runs on the tree walker. Byte mode gives the
/// result of tree mode.
#[test]
fn a_function_inside_with_gives_the_same_result_in_byte_mode() {
    big(|| {
        let src = r#"var o = { prop: 'before' }; var f; var g;
with (o) { f = function () { return prop; }; g = function () { return function () { return prop; }; }; }
o.prop = 'after'; var a = f(); delete o.prop; var h = g(); var b; try { b = h(); } catch (e) { b = e.name; }
a + ' ' + b"#;
        let mut out = Vec::new();
        for mode in [Exec::Tree, Exec::Byte] {
            let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
            it.set_exec(mode);
            out.push(run(&mut it, src));
        }
        assert_eq!(out[0], "after ReferenceError");
        assert_eq!(out[0], out[1]);
    });
}
