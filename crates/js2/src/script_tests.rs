// End-to-end tests (S2.6, S2.7): real JavaScript goes through the lexer, the parser, the compiler and the interpreter, and the test reads
// a global that the script set. The built-ins of S2.7 (`builtins.rs` and the `bi_*` files, `prelude.js`) have their tests at the end.
use super::*;
use crate::heap::Trigger;

const MIRI: bool = cfg!(miri);

/// Run `src`, then read the global `name` as text.
fn ev(src: &str, name: &str) -> String {
    match run(src) {
        Ok(mut vm) => vm.global_text(name),
        Err(e) => panic!("{e}\n--- source:\n{src}"),
    }
}

/// The same under constant collection (a tiny trigger: nearly every op that allocates collects first).
fn ev_gc(src: &str, name: &str) -> String {
    let mut vm = load(src).unwrap_or_else(|e| panic!("{e}\n--- source:\n{src}"));
    let k = if MIRI { 4 << 10 } else { 1 << 10 };
    vm.realm.heap.set_trigger(Trigger { min: k, slack: k });
    if let Err(e) = vm.run_main(0) {
        panic!("uncaught {e:?}\n--- source:\n{src}");
    }
    vm.global_text(name)
}

/// Each case is `(source, global, expected text)`. Each runs normally and under constant collection. Under Miri it runs only
/// normally (Miri is 100 times slower; `a_few_programs_under_constant_collection` and the stress test keep the collector covered), and
/// a case with `**` is skipped (Miri adds a random error to the result of `pow`).
pub(crate) fn check(cases: &[(&str, &str, &str)]) {
    for (src, name, want) in cases {
        if MIRI && src.contains("**") {
            continue;
        }
        assert_eq!(ev(src, name), *want, "{src}");
        if !MIRI {
            assert_eq!(ev_gc(src, name), *want, "under collection: {src}");
        }
    }
}

#[test]
fn a_few_programs_under_constant_collection() {
    // Run in Miri too: one program each for classes with fields and `super()`, getters, spread, `arguments` and a compound assignment
    // to a global (its root map once lacked the result register).
    check_gc(&[
        ("var log = ''; class P { constructor() { log += 'P'; } } class D extends P { f = (log += 'f'); constructor() { log += '1'; super(); log += '2'; } } new D();", "log", "1Pf2"),
        ("class A { #v = 1; get v() { return this.#v; } } class B extends A { get v() { return super.v + 1; } } var r = new B().v;", "r", "2"),
        ("function f() { return [...arguments, ...[9]]; } var r = f(1, 2);", "r", "[1,2,9]"),
        ("var o = {...{a: 1}, get b() { return 2; }}; var r = o.a + o.b;", "r", "3"),
    ]);
}

fn check_gc(cases: &[(&str, &str, &str)]) {
    for (src, name, want) in cases {
        assert_eq!(ev_gc(src, name), *want, "under collection: {src}");
    }
}

#[test]
fn arithmetic_and_strings() {
    check(&[
        ("var r = (1 + 2) * 3 - 4 / 2 + 7 % 4 + 2 ** 3;", "r", "18"),
        ("var r = 0.1 + 0.2 > 0.3; var s = 7 / 2; var t = -7 % 3; var u = 1 / 0;", "r", "true"),
        ("var s = 7 / 2;", "s", "3.5"),
        ("var t = -7 % 3;", "t", "-1"),
        ("var u = 5 / 0 - 1 / 0;", "u", "NaN"),
        ("var r = 2147483647 + 1;", "r", "2147483648"),
        ("var r = (5 & 3) | (8 ^ 2) | (1 << 4) | (256 >> 2) | (-1 >>> 28);", "r", "95"),
        ("var r = \"a\" + 1 + 2 + `x${3 + 4}y`;", "r", "a12x7y"),
        ("var r = \"b\" > \"a\" && \"abc\" < \"abd\" && !(\"a\" == \"b\");", "r", "true"),
        ("var r = typeof 1 + typeof \"s\" + typeof undefined + typeof null + typeof {} + typeof function () {};", "r", "numberstringundefinedobjectobjectfunction"),
        ("var a = 1, b = \"1\"; var r = (a == b) + \",\" + (a === b) + \",\" + (null == undefined) + \",\" + (null === undefined);", "r", "true,false,true,false"),
        ("var r = null ?? 5; var s = 0 ?? 5; var t = 0 || 5; var u = 1 && 2;", "t", "5"),
        ("var o = {a: {b: 1}}; var r = o?.a?.b + (o?.x?.y === undefined);", "r", "2"),
        ("var x = 5; x += 3; x -= 1; x *= 2; x /= 7; x **= 3; var r = x;", "r", "8"),
        ("var i = 5; var r = i++ + ++i + i-- + --i;", "r", "24"),
    ]);
}

#[test]
fn control_flow_loops_and_switch() {
    check(&[
        ("var s = 0; for (var i = 0; i < 10; i++) { if (i == 5) continue; if (i == 8) break; s += i; }", "s", "23"),
        ("var w = 0; while (w < 3) w++; var d = 0; do { d += 2; } while (d < 7);", "d", "8"),
        ("var r = 0; outer: for (var i = 0; i < 4; i++) { for (var j = 0; j < 4; j++) { if (j == 2) continue outer; if (i == 3) break outer; r += 1; } }", "r", "6"),
        ("function f(x) { switch (x) { case 1: return 'one'; case 2: case 3: return 'few'; default: return 'many'; } } var r = f(1) + f(3) + f(9);", "r", "onefewmany"),
        ("var r = ''; for (var k in {a: 1, b: 2}) r += k; for (var v of [7, 8]) r += v;", "r", "ab78"),
        ("var r = 0; for (var c of [5, 6, 7]) r += c;", "r", "18"),
        ("function f(n) { return n < 2 ? n : f(n - 1) + f(n - 2); } var r = f(15);", "r", "610"),
        ("var r = 5 > 3 ? 'y' : 'n';", "r", "y"),
        ("var x = 3; var r; if (x > 5) r = 'a'; else if (x > 2) r = 'b'; else r = 'c';", "r", "b"),
    ]);
}

#[test]
fn functions_closures_and_scope() {
    check(&[
        ("function mk() { var n = 0; return function () { return ++n; }; } var c = mk(); c(); c(); var r = c();", "r", "3"),
        ("var fs = []; for (let i = 0; i < 3; i++) fs[i] = function () { return i; }; var r = fs[0]() + fs[1]() * 10 + fs[2]() * 100;", "r", "210"),
        ("var fs = []; for (var i = 0; i < 3; i++) fs[i] = function () { return i; }; var r = fs[0]() + fs[1]() * 10;", "r", "33"),
        ("var add = (a) => (b) => a + b; var r = add(2)(5);", "r", "7"),
        ("var o = {v: 4, m() { return this.v; }, a: () => typeof this}; var r = o.m();", "r", "4"),
        ("function f() { return g(); function g() { return 'hoisted'; } } var r = f();", "r", "hoisted"),
        ("try { x; let x = 1; } catch (e) { var r = e.name; }", "r", "ReferenceError"),
        ("try { y; } catch (e) { var r = e.name + ': ' + e.message; }", "r", "ReferenceError: y is not defined"),
        ("function f() { const k = 1; try { k = 2; } catch (e) { return e.name; } } var r = f();", "r", "TypeError"),
        ("var f = function fact(n) { return n <= 1 ? 1 : n * fact(n - 1); }; var r = f(6);", "r", "720"),
        ("function f(a, b = a + 1, c = b * 2) { return [a, b, c]; } var r = [f(1), f(1, 5)];", "r", "[[1,2,4],[1,5,10]]"),
        ("function f() { return this; } var r = typeof f.call;", "r", "function"),
        ("var r = (function () { return typeof arguments; })();", "r", "object"),
        ("function f(a, b) { return arguments.length + a + arguments[1]; } var r = f(1, 2, 3);", "r", "6"),
        ("function f(a, ...r) { return r; } var r = f(1, 2, 3, 4);", "r", "[2,3,4]"),
        ("function f(a, ...r) { return r.length; } var r = f();", "r", "0"),
        ("var r = (function () { var a = 1; { let a = 2; } return a; })();", "r", "1"),
    ]);
}

#[test]
fn objects_and_arrays() {
    check(&[
        ("var o = {a: 1, b: {c: 2}}; o.d = 3; o['e'] = 4; var r = o.a + o.b.c + o.d + o.e;", "r", "10"),
        ("var o = {a: 1, b: 2, c: 3}; delete o.a; var r = ('a' in o) + ',' + ('b' in o); var k = ''; for (var x in o) k += x;", "k", "bc"),
        ("var o = {a: 1, ['b' + 1]: 2, 3: 'x'}; var r = o.b1 + o[3] + o.a;", "r", "2x1"),
        ("var o = {a: 1}; var p = {__proto__: o, b: 2}; var r = p.a + p.b;", "r", "3"),
        ("var a = [1, 2, 3]; a[5] = 6; var r = a.length + a[1];", "r", "8"),
        ("var a = [1, 2, 3, 4]; a.length = 2; var r = a;", "r", "[1,2]"),
        ("var a = [1, , 3]; var r = a.length + ',' + (1 in a);", "r", "3,false"),
        ("var a = []; for (var i = 0; i < 5; i++) a[i] = i * i; var r = a;", "r", "[0,1,4,9,16]"),
        ("var o = {x: 1}; o.self = o; var r = typeof o.self.self.x;", "r", "number"),
        ("var o = {get x() { return 7; }, set x(v) { this._x = v; }, _x: 0}; o.x = 5; var r = o.x + o._x;", "r", "12"),
        ("var o = {get a() { return 1; }}; var r = o.a; o.a = 9; var s = o.a;", "s", "1"),
        ("var r = {a: 1, b: 'two', c: [3, {d: null}]};", "r", "{a:1,b:\"two\",c:[3,{d:null}]}"),
        ("var o = {f() { return 1; }}; var r = o.f() + (typeof o.f);", "r", "1function"),
    ]);
}

#[test]
fn exceptions() {
    check(&[
        ("var log = ''; try { log += 'a'; throw {m: 'x'}; } catch (e) { log += 'b' + e.m; } finally { log += 'c'; }", "log", "abxc"),
        ("function f() { try { return 'try'; } finally { g = 'fin'; } } var r = f() + g; var g;", "r", "tryfin"),
        ("function f() { for (var i = 0; i < 3; i++) { try { if (i == 1) break; } finally { r += 'f' + i; } } } var r = ''; f();", "r", "f0f1"),
        ("try { null.x; } catch (e) { var r = e.name + ': ' + e.message; }", "r", "TypeError: cannot read properties of null (reading 'x')"),
        ("try { undefined.f(); } catch (e) { var r = e.name; }", "r", "TypeError"),
        ("try { var q = 1; q(); } catch (e) { var r = e.message; }", "r", "not a function"),
        ("function f() { throw 'deep'; } function g() { f(); } try { g(); } catch (e) { var r = e; }", "r", "deep"),
        ("var r = ''; try { try { throw 1; } finally { r += 'a'; } } catch (e) { r += 'b' + e; }", "r", "ab1"),
        ("function f(n) { return f(n + 1); } try { f(0); } catch (e) { var r = e.name; }", "r", "RangeError"),
        ("var r; try { throw 1; } catch { r = 'no binding'; }", "r", "no binding"),
    ]);
}

#[test]
fn destructuring_spread_and_rest() {
    check(&[
        ("var {a, b: {c}, ...rest} = {a: 1, b: {c: 2}, d: 3, e: 4}; var r = a + ',' + c + ',' + rest.d + rest.e + ',' + ('a' in rest);", "r", "1,2,34,false"),
        ("var [x, , y = 10, ...zs] = [1, 2, undefined, 4, 5]; var r = [x, y, zs];", "r", "[1,10,[4,5]]"),
        ("function f({a, b = 2}, [c]) { return a + b + c; } var r = f({a: 1}, [3]);", "r", "6"),
        ("var a = 1, b = 2; [a, b] = [b, a]; var r = a + ',' + b;", "r", "2,1"),
        ("function f(a, b, c) { return a + b + c; } var arr = [1, 2, 3]; var r = f(...arr) + [...arr, ...arr].length;", "r", "12"),
        ("function f() { return arguments.length; } var r = f(...[1, 2], 3, ...'ab');", "r", "5"),
        ("function P(a, b) { this.s = a + b; } var args = [3, 4]; var r = new P(...args).s;", "r", "7"),
        ("var o = {...{a: 1, b: 2}, b: 3, ...null, c: 4}; var r = o;", "r", "{a:1,b:3,c:4}"),
        ("var r = [...'héllo'].length;", "r", "5"),
        ("var a = [1, 2]; var r = [0, ...a, 3, ...[4]];", "r", "[0,1,2,3,4]"),
        ("var r = [1, , 3]; var s = [...r].length;", "s", "3"),
        ("function f(x, ...[a, b]) { return a + b; } var r = f(0, 1, 2);", "r", "3"),
        ("try { var {x} = null; } catch (e) { var r = e.name; }", "r", "TypeError"),
        ("for (var [k, v] of [[1, 2], [3, 4]]) var r = (r || 0) + k * v;", "r", "14"),
    ]);
}

#[test]
fn tagged_templates_regexp_and_bigint() {
    check(&[
        ("function tag(s, v) { return s[0] + v + s.raw[1]; } var r = tag`a${1}b`;", "r", "a1b"),
        ("function id(s) { return s; } function t() { return id`x`; } var r = t() === t();", "r", "true"),
        ("function id(s) { return s; } var r = id`a\\n${0}` === id`a\\n${0}`;", "r", "false"),
        ("var r = /a+/g.test('caab');", "r", "true"),
        ("try { var x = 10n; } catch (e) { var r = e.name; }", "r", "TypeError"),
    ]);
}

#[test]
fn classes_inheritance_super_and_new_target() {
    check(&[
        (
            "class A { constructor(x) { this.x = x; } m() { return this.x + 1; } static make() { return new A(5); } }
             class B extends A { constructor(x, y) { super(x); this.y = y; } m() { return super.m() * 10; } }
             var b = new B(3, 4); var r = [b.x, b.y, b.m(), A.make().x, b instanceof A, b instanceof B];",
            "r",
            "[3,4,40,5,true,true]",
        ),
        (
            "class A { get double() { return this.x * 2; } set double(v) { this.x = v / 2; } constructor() { this.x = 1; } }
             var a = new A(); a.double = 20; var r = a.x + ',' + a.double;",
            "r",
            "10,20",
        ),
        (
            "class A { static s() { return 'sa'; } static get g() { return 'ga'; } } class B extends A { static s() { return super.s() + 'b'; } }
             var r = B.s() + B.g;",
            "r",
            "sabga",
        ),
        ("class A {} var a = new A(); var r = (a instanceof A) + ',' + (typeof A) + ',' + (A.prototype.constructor === A);", "r", "true,function,true"),
        ("class A { constructor() { this.n = 1; } } class B extends A {} class C extends B {} var r = new C().n;", "r", "1"),
        ("class A { constructor(...a) { this.n = a.length; } } class B extends A {} var r = new B(1, 2, 3).n;", "r", "3"),
        ("class A {} try { A(); } catch (e) { var r = e.name; }", "r", "TypeError"),
        ("class F { constructor() { this.f = 1; } } class A extends F {} var r = new A().f;", "r", "1"),
        (
            "class A { a = 1; b = this.a + 1; static s = 3; static t = A.s * 2; } var x = new A(); var r = [x.a, x.b, A.s, A.t];",
            "r",
            "[1,2,3,6]",
        ),
        (
            "class P { a = 1; } class D extends P { z = this.a + 1; constructor() { super(); this.w = this.z + 1; } } var d = new D(); var r = [d.a, d.z, d.w];",
            "r",
            "[1,2,3]",
        ),
        (
            "var log = ''; class P { constructor() { log += 'P'; } } class D extends P { f = (log += 'f'); constructor() { log += '1'; super(); log += '2'; } } new D();",
            "log",
            "1Pf2",
        ),
        ("class S { static x; static { S.x = 7; } } var r = S.x;", "r", "7"),
        ("function F() { this.nt = new.target === F; } var r = new F().nt;", "r", "true"),
        ("function G() { return typeof new.target; } var r = G();", "r", "undefined"),
        ("class A { constructor() { this.t = new.target.name2; } } class B extends A {} B.name2 = 'bee'; var r = new B().t;", "r", "bee"),
        (
            "class P { m() { return 1; } } class Q extends P { m() { var f = () => super.m(); return f() + 1; } } var r = new Q().m();",
            "r",
            "2",
        ),
        (
            "class P { constructor() { this.p = 1; } } class R extends P { constructor() { var f = () => super(); f(); this.k = 2; } } var o = new R(); var r = o.p + o.k;",
            "r",
            "3",
        ),
        ("class A { constructor() { return {o: 1}; } } var r = new A().o;", "r", "1"),
        ("class B {} class A extends B { constructor() { try { this.x; } catch (e) { var n = e.name; } super(); g = n; } } var g; new A();", "g", "ReferenceError"),
        (
            "class A { m() { return 'a'; } } var o = { m() { return super.m ? 'has' : 'none'; } }; var r = o.m();",
            "r",
            "none",
        ),
        ("var C = class Named { who() { return typeof Named; } }; var r = new C().who();", "r", "function"),
        ("class K { ['comp' + 1]() { return 1; } static ['s' + 2] = 5; } var r = new K().comp1() + K.s2;", "r", "6"),
    ]);
}

#[test]
fn private_names_and_accessors() {
    check(&[
        (
            "class C { #p = 2; static #q = 4; getP() { return this.#p; } setP(v) { this.#p = v; } static getQ() { return C.#q; } #m() { return 5; } callM() { return this.#m(); }
               static has(o) { return #p in o; } }
             var c = new C(); c.setP(9); var r = [c.getP(), C.getQ(), c.callM(), C.has(c), C.has({})];",
            "r",
            "[9,4,5,true,false]",
        ),
        (
            "class C { #v = 1; get #g() { return this.#v * 10; } set #g(x) { this.#v = x; } bump() { this.#g = 5; return this.#g; } } var r = new C().bump();",
            "r",
            "50",
        ),
        ("class C { #p = 1; static read(o) { return o.#p; } } try { C.read({}); } catch (e) { var r = e.name; }", "r", "TypeError"),
        ("class C { #m() {} static w(o) { o.#m = 1; } } try { C.w(new C()); } catch (e) { var r = e.name; }", "r", "TypeError"),
        ("class C { #p = 1; getKeys() { var k = ''; for (var x in this) k += x; return k; } } var r = '[' + new C().getKeys() + ']';", "r", "[]"),
        ("function mk() { return class { #p = 1; static get(o) { return o.#p; } }; } var A = mk(), B = mk(); try { B.get(new A()); } catch (e) { var r = e.name; }", "r", "TypeError"),
        ("class A { static #s() { return 's'; } static t() { return A.#s(); } } var r = A.t();", "r", "s"),
        ("class A { get v() { return 1; } } class B extends A { get v() { return super.v + 1; } } var r = new B().v;", "r", "2"),
        ("class A { set v(x) { this._v = x * 2; } } class B extends A { set v(x) { super.v = x + 1; } } var b = new B(); b.v = 1; var r = b._v;", "r", "4"),
    ]);
}

#[test]
fn unbuilt_constructs_stop_the_load_and_the_rest_is_clear() {
    for src in ["with ({}) {}"] {
        let e = run(src).err().unwrap_or_else(|| panic!("{src} ran"));
        assert!(e.starts_with("not supported yet"), "{src}: {e}");
    }
    assert!(run("var = ;").err().unwrap().starts_with("SyntaxError"));
    assert_eq!(run("throw {a: 1};").err().unwrap(), "{a:1}");
    assert_eq!(run("throw 'x';").err().unwrap(), "x");
}

#[test]
fn a_loop_budget_stops_a_script() {
    let mut vm = load("var i = 0; while (true) i++;").unwrap();
    vm.fuel = 1000;
    assert!(matches!(vm.run_main(0), Err(Exit::Fuel)));
}

#[test]
fn many_objects_survive_collections_through_getters_classes_and_spread() {
    let src = "
      class Node { #id; constructor(i) { this.#id = i; this.kids = []; } get id() { return this.#id; } add(k) { this.kids[this.kids.length] = k; return this; } }
      function build(n) { var root = new Node(0), cur = root; for (var i = 1; i < n; i++) { var k = new Node(i); cur.add(k); cur = k; } return root; }
      function sum(nd) { var s = 0; while (nd) { s += nd.id; nd = nd.kids[0]; } return s; }
      var total = 0;
      for (var round = 0; round < 20; round++) { var parts = [...[1, 2], ...[3]]; total += sum(build(30)) + parts.length + ({...{a: 1}}).a; }";
    let n = if MIRI { 1 } else { 20 };
    let src = src.replace("round < 20", &format!("round < {n}"));
    let want = (0..n).map(|_| 435 + 3 + 1).sum::<i32>().to_string();
    assert_eq!(ev(&src, "total"), want);
    assert_eq!(ev_gc(&src, "total"), want);
}

// ---- S2.7: the core built-ins. Each group runs normally and under constant collection (`check`). Under Miri a group runs only its first
// cases: each run compiles the prelude too, and Miri is slow. ----

fn check_builtins(cases: &[(&str, &str, &str)]) {
    check(if MIRI { &cases[..cases.len().min(3)] } else { cases });
}

#[test]
fn object_and_object_prototype() {
    check_builtins(&[
        ("var o = {a: 1, b: 2}; var r = Object.keys(o).join();", "r", "a,b"),
        ("var r = Object.values({a: 1, b: 'x'}).join('|');", "r", "1|x"),
        ("var r = Object.entries({a: 1, b: 2}).map(function (e) { return e[0] + e[1]; }).join();", "r", "a1,b2"),
        ("var t = Object.assign({a: 1}, {b: 2}, {a: 3}); var r = t.a + ',' + t.b;", "r", "3,2"),
        ("var p = {hi: 5}; var o = Object.create(p); o.own = 1; var r = o.hi + ',' + Object.keys(o).join() + ',' + (Object.getPrototypeOf(o) === p);", "r", "5,own,true"),
        ("var o = Object.create(null); o.k = 1; var r = Object.getPrototypeOf(o) + ',' + o.k + ',' + typeof o.toString;", "r", "null,1,undefined"),
        ("var o = {}; Object.defineProperty(o, 'x', {value: 7}); o.x = 9; var r = o.x + ',' + Object.keys(o).length;", "r", "7,0"),
        ("var o = {}; Object.defineProperty(o, 'g', {get: function () { return 42; }, enumerable: true}); var r = o.g + ',' + Object.keys(o).join();", "r", "42,g"),
        ("var o = {}; Object.defineProperties(o, {a: {value: 1, enumerable: true}, b: {value: 2}}); var r = Object.keys(o).join() + Object.getOwnPropertyNames(o).join();", "r", "aa,b"),
        ("var o = {a: 1}; var d = Object.getOwnPropertyDescriptor(o, 'a'); var r = d.value + ',' + d.writable + ',' + d.enumerable + ',' + d.configurable;", "r", "1,true,true,true"),
        ("var o = Object.freeze({a: 1}); o.a = 2; var r = o.a + ',' + Object.isFrozen(o) + ',' + Object.isFrozen({a: 1});", "r", "1,true,false"),
        ("var o = {}; Object.setPrototypeOf(o, {z: 3}); var r = o.z;", "r", "3"),
        ("var o = {a: 1}; var r = o.hasOwnProperty('a') + ',' + o.hasOwnProperty('toString') + ',' + ('toString' in o) + ',' + [1].hasOwnProperty(0) + ',' + [1].hasOwnProperty('length');", "r", "true,false,true,true,true"),
        ("var t = Object.prototype.toString; var r = [t.call([]), t.call(null), t.call(undefined), t.call({}), t.call(function () {}), t.call(1), t.call('s'), t.call(true), t.call(new Error())].join();", "r", "[object Array],[object Null],[object Undefined],[object Object],[object Function],[object Number],[object String],[object Boolean],[object Error]"),
        ("var o = {}; var r = (o.valueOf() === o) + ',' + Object.prototype.isPrototypeOf.call(Object.prototype, o) + ',' + o.propertyIsEnumerable('x');", "r", "true,true,false"),
        ("var o = new Object(); o.q = 1; var r = typeof o + o.q + (Object(5) === 5) + (Object(o) === o);", "r", "object1falsetrue"),
        ("var r = typeof Object + typeof Object.keys + (Object.prototype.constructor === Object) + (({}).constructor === Object);", "r", "functionfunctiontruetrue"),
        ("var a = [3, 4]; var r = Object.keys(a).join() + Object.entries('ab').join('/');", "r", "0,10,a/1,b"),
    ]);
}

#[test]
fn function_prototype_and_plain_function_prototypes() {
    check_builtins(&[
        ("function f(a, b) { return this.k + a + b; } var r = f.call({k: 1}, 2, 3);", "r", "6"),
        ("function f(a, b) { return this.k + a + b; } var r = f.apply({k: 1}, [2, 3]);", "r", "6"),
        ("function f(a, b) { return this.k * 10 + a + b; } var g = f.bind({k: 2}, 5); var r = g(1);", "r", "26"),
        ("function f() { return g.apply(null, arguments); } function g(a, b) { return a + b; } var r = f(2, 3);", "r", "5"),
        ("var r = Math.max.apply(null, [3, 9, 4]) + Math.min.call(null, 5, 2);", "r", "11"),
        ("var arr = []; var p = arr.push.bind(arr); p(1); p(2, 3); var r = arr.length + ',' + arr.join('');", "r", "3,123"),
        ("var r = Object.prototype.hasOwnProperty.call({a: 1}, 'a') + ',' + Array.prototype.slice.call([1, 2, 3], 1).join('');", "r", "true,23"),
        ("function f(a, b, c) { return a + b + c; } var g = f.bind(null, 1).bind(null, 2); var r = g(3);", "r", "6"),
        ("function f() { return typeof f.toString() + typeof f.call + typeof f.apply + typeof f.bind; } var r = f();", "r", "stringfunctionfunctionfunction"),
        ("try { Object.prototype.hasOwnProperty.call.call(1); } catch (e) { var r = e.name; }", "r", "TypeError"),
        ("function F() { this.x = 1; } F.prototype.get = function () { return this.x; }; var r = new F().get();", "r", "1"),
        ("function F() {} var r = (new F() instanceof F) + ',' + (F.prototype.constructor === F) + ',' + (Object.getPrototypeOf(new F()) === F.prototype);", "r", "true,true,true"),
        ("function F() {} F.prototype = {m: function () { return 'm'; }}; var r = new F().m();", "r", "m"),
        ("function A(v) { this.v = v; } function B(v) { A.call(this, v); } B.prototype = Object.create(A.prototype); var r = new B(4).v + ',' + (new B(1) instanceof A);", "r", "4,true"),
        ("function F() { this.a = 1; } F.prototype.hi = function () { return 'hi'; }; class G extends F { } var g = new G(); var r = g.a + g.hi();", "r", "1hi"),
        ("var m = {a() { return 1; }}; var r = typeof m.a.prototype + typeof (function () {}).prototype;", "r", "objectobject"),
        ("var f = function () {}; f.x = 1; var r = f() === undefined && f.x === 1 && typeof f.prototype === 'object';", "r", "true"),
        ("var bound = function (a) { return this.v + a; }.bind({v: 1}); var o = {m: bound}; var r = o.m(2) + ',' + [5, 6].map(bound).join();", "r", "3,6,7"),
        ("var r = typeof Function + (function () {} instanceof Function) + (Function.prototype === Object.getPrototypeOf(function () {}));", "r", "functiontruetrue"),
    ]);
}

#[test]
fn errors_and_their_subclasses() {
    check_builtins(&[
        ("var e = new Error('boom'); var r = e.message + ',' + e.name + ',' + (e instanceof Error) + ',' + String(e);", "r", "boom,Error,true,Error: boom"),
        ("var e = new TypeError('t'); var r = (e instanceof TypeError) + ',' + (e instanceof Error) + ',' + e.name + ',' + e.toString();", "r", "true,true,TypeError,TypeError: t"),
        ("try { null.x; } catch (e) { var r = (e instanceof TypeError) + ',' + e.name + ',' + (e instanceof RangeError); }", "r", "true,TypeError,false"),
        ("try { undefinedVar; } catch (e) { var r = (e instanceof ReferenceError) + ',' + (e instanceof Error) + ',' + e.message; }", "r", "true,true,undefinedVar is not defined"),
        ("try { new Array(-1); } catch (e) { var r = e.name + ',' + (e instanceof RangeError); }", "r", "RangeError,true"),
        ("var r = new SyntaxError('s').name + new RangeError('x').name + new ReferenceError('y').name;", "r", "SyntaxErrorRangeErrorReferenceError"),
        ("var e = Error('no new'); var r = e.message + (e instanceof Error);", "r", "no newtrue"),
        ("var e = new Error(); var r = e.message === '' && !e.hasOwnProperty('message') && Object.keys(new Error('x')).length === 0;", "r", "true"),
        ("class E1 extends Error { constructor(m) { super(m); this.code = 5; } } try { throw new E1('x'); } catch (e) { var r = e.message + e.code + (e instanceof E1) + (e instanceof Error) + e.name; }", "r", "x5truetrueError"),
        ("class E2 extends TypeError {} var e = new E2('q'); var r = (e instanceof E2) + ',' + (e instanceof TypeError) + ',' + e.message + ',' + e.name;", "r", "true,true,q,TypeError"),
        ("class E3 extends Error { f = 7; constructor(m) { super(m); } } var r = new E3('m').f;", "r", "7"),
        ("var r = Error.prototype.toString.call({name: 'N', message: 'M'}) + '|' + Error.prototype.toString.call({}) + '|' + Error.prototype.toString.call({name: '', message: 'only'});", "r", "N: M|Error|only"),
        ("var e = new Error('a'); e.message = 'b'; e.extra = 1; var r = e.message + Object.keys(e).join();", "r", "bextra"),
        ("var c = 0; for (var i = 0; i < 30; i++) { try { throw new Error('e' + i); } catch (e) { c += e.message.length; } } var r = c;", "r", "80"),
        ("try { throw new Error('x'); } catch (e) { var r = typeof e.stack + (Object.getPrototypeOf(e) === Error.prototype); }", "r", "undefinedtrue"),
    ]);
}

#[test]
fn to_primitive() {
    check_builtins(&[
        ("var r = '' + [1];", "r", "1"),
        ("var r = '' + [1, [2, 3]] + '|' + ({});", "r", "1,2,3|[object Object]"),
        ("var r = [] + [] + 'x';", "r", "x"),
        ("var r = [5] * 2 + [3] - 1;", "r", "102"),
        ("var r = ([1, 2] == '1,2') + ',' + ({} == '[object Object]') + ',' + ([2] > 1) + ',' + ([2] == 2) + ',' + (null == []);", "r", "true,true,true,true,false"),
        ("var r = String([1, [2]]) + String({}) + String(null) + String(undefined);", "r", "1,2[object Object]nullundefined"),
        ("var r = `${[1, 2]}-${{}}`;", "r", "1,2-[object Object]"),
        ("var o = {}; o[[1]] = 'x'; var r = o['1'];", "r", "x"),
        ("var r = +[] + ',' + +[7] + ',' + +[1, 2] + ',' + (-[3]);", "r", "0,7,NaN,-3"),
        ("var r = (function () {}) + '';", "r", "function () { [native code] }"),
        ("var a = []; a.push(a); try { '' + a; } catch (e) { var r = e.name; }", "r", "RangeError"),
        ("var o = {valueOf: function () { return 3; }}; var r = o + 1;", "r", "4"),
        ("var e = new Error('m'); var r = '' + e + (e + '' === String(e));", "r", "Error: mtrue"),
    ]);
}

#[test]
fn array_constructor_and_methods() {
    check_builtins(&[
        ("var a = []; var n = a.push(1, 2, 3); var r = n + ',' + a.length + ',' + a.pop() + ',' + a.length;", "r", "3,3,3,2"),
        ("var a = [1, 2, 3]; var s = a.shift(); a.unshift(7, 8); var r = s + ',' + a.join() + ',' + a.length;", "r", "1,7,8,2,3,4"),
        ("var r = [1, 2, 3, 4, 5].slice(1, 3).join() + '|' + [1, 2, 3].slice(-2).join() + '|' + [1, 2, 3].slice().join() + '|' + [1, 2, 3].slice(5).length;", "r", "2,3|2,3|1,2,3|0"),
        ("var a = [1, 2, 3, 4, 5]; var d = a.splice(1, 2, 'x', 'y', 'z'); var r = d.join() + '|' + a.join();", "r", "2,3|1,x,y,z,4,5"),
        ("var a = [1, 2, 3]; a.splice(1, 0, 9); var b = [1, 2, 3, 4]; b.splice(-1); var c = [1, 2, 3]; c.splice(1); var r = a.join() + '|' + b.join() + '|' + c.join();", "r", "1,9,2,3|1,2,3|1"),
        ("var r = [1, 2].concat([3, 4], 5, [[6]]).length + ',' + [1].concat([2], [3]).join('-');", "r", "6,1-2-3"),
        ("var r = [1, null, undefined, 'a', [2, 3]].join('-') + '|' + [].join() + '|' + [1, 2, 3].join('') + '|' + ['x'].join(', ');", "r", "1---a-2,3||123|x"),
        ("var a = [1, 2, 3, 2, NaN]; var r = a.indexOf(2) + ',' + a.indexOf(2, 2) + ',' + a.lastIndexOf(2) + ',' + a.indexOf(9) + ',' + a.indexOf(NaN) + ',' + a.includes(NaN) + ',' + a.includes(3) + ',' + a.includes(7) + ',' + ['a'].indexOf('a');", "r", "1,3,3,-1,-1,true,true,false,0"),
        ("var a = [1, 2, 3]; a.reverse(); var r = a.join() + ',' + Array.isArray(a) + ',' + Array.isArray({}) + ',' + Array.isArray('s');", "r", "3,2,1,true,false,false"),
        ("var r = new Array(3).length + ',' + new Array(1, 2).join() + ',' + Array(2, 3).length + ',' + new Array('3').length + ',' + Array().length;", "r", "3,1,2,2,1,0"),
        ("var r = [1, 2, 3].forEach(function () {}) + '|' + [1, 2, 3].map(function (x) { return x * 2; }).join() + '|' + [1, 2, 3, 4].filter(function (x) { return x % 2; }).join();", "r", "undefined|2,4,6|1,3"),
        ("var t = 0; [1, 2, 3].forEach(function (v, i, a) { t += v * 10 + i + a.length; }); var r = t;", "r", "72"),
        ("var r = [1, 2, 3].reduce(function (a, b) { return a + b; }) + ',' + [1, 2, 3].reduce(function (a, b) { return a + b; }, 10) + ',' + [[1], [2]].reduceRight(function (a, b) { return a.concat(b); }).join();", "r", "6,16,2,1"),
        ("try { [].reduce(function () {}); } catch (e) { var r = e.name; }", "r", "TypeError"),
        ("var r = [1, 2, 3].some(function (x) { return x > 2; }) + ',' + [1, 2, 3].every(function (x) { return x > 2; }) + ',' + [].every(function () { return false; }) + ',' + [].some(function () { return true; });", "r", "true,false,true,false"),
        ("var r = [5, 6, 7].find(function (x) { return x > 5; }) + ',' + [5, 6, 7].findIndex(function (x) { return x > 5; }) + ',' + [5].find(function (x) { return x > 9; }) + ',' + [5].findIndex(function (x) { return x > 9; });", "r", "6,1,undefined,-1"),
        ("var r = [3, 1, 2].sort().join() + '|' + [10, 9, 1, 100].sort().join() + '|' + [10, 9, 1, 100].sort(function (a, b) { return a - b; }).join() + '|' + ['b', undefined, 'a'].sort().join();", "r", "1,2,3|1,10,100,9|1,9,10,100|a,b,"),
        ("var a = [5, 2, 8, 1, 9, 3, 7, 4, 6, 0]; a.sort(function (x, y) { return y - x; }); var r = a.join('');", "r", "9876543210"),
        ("var p = [{k: 1, v: 'a'}, {k: 0, v: 'b'}, {k: 1, v: 'c'}, {k: 0, v: 'd'}]; p.sort(function (x, y) { return x.k - y.k; }); var r = p.map(function (e) { return e.v; }).join('');", "r", "bdac"),
        ("var r = [1, 2, 3].map(function (x) { return this.m * x; }, {m: 3}).join() + Array.from('abc').join('-') + Array.from({length: 3}, function (v, i) { return i * i; }).join();", "r", "3,6,9a-b-c0,1,4"),
        ("var o = {}; var a = [1, 2]; a.x = 5; var r = a.map(function (v) { return v + 1; }).length + ',' + a.length;", "r", "2,2"),
        ("class Stack extends Array { top() { return this[this.length - 1]; } } var s = new Stack(); s.push(1); s.push(2); var r = s.top() + ',' + s.length + ',' + (s instanceof Array) + ',' + (s instanceof Stack) + ',' + Array.isArray(s);", "r", "2,2,true,true,true"),
        ("var r = [1, [2, [3]]].toString() + '|' + [1, 2].toString() + '|' + typeof [].forEach + typeof [].sort;", "r", "1,2,3|1,2|functionfunction"),
        ("try { [].forEach(1); } catch (e) { var r = e.name; }", "r", "TypeError"),
        ("function f() { return Array.prototype.slice.call(arguments, 1).join(); } var r = f(1, 2, 3);", "r", "2,3"),
        ("var a = [1, 2, 3]; var out = []; for (var k in a) out.push(k); var r = out.join();", "r", "0,1,2"),
    ]);
}

#[test]
fn string_prototype() {
    check_builtins(&[
        ("var s = 'hello'; var r = s.charAt(1) + s.charAt(9) + '|' + s.charCodeAt(0) + ',' + s.charCodeAt(9) + ',' + s.at(-1) + ',' + s.codePointAt(1);", "r", "e|104,NaN,o,101"),
        ("var r = 'abcabc'.indexOf('c') + ',' + 'abcabc'.indexOf('c', 3) + ',' + 'abcabc'.lastIndexOf('b') + ',' + 'abc'.indexOf('z') + ',' + 'abc'.indexOf('') + ',' + 'abc'.includes('bc') + ',' + 'abc'.includes('bc', 2);", "r", "2,5,4,-1,0,true,false"),
        ("var r = 'abc'.startsWith('ab') + ',' + 'abc'.startsWith('b', 1) + ',' + 'abc'.endsWith('bc') + ',' + 'abc'.endsWith('b', 2) + ',' + 'abc'.endsWith('x');", "r", "true,true,true,true,false"),
        ("var s = 'abcdef'; var r = s.slice(1, 3) + '|' + s.slice(-2) + '|' + s.slice(2, -1) + '|' + s.slice(4, 1) + '|' + s.substring(4, 1) + '|' + s.substring(-3, 2) + '|' + s.substr(1, 2) + '|' + s.substr(-2);", "r", "bc|ef|cde||bcd|ab|bc|ef"),
        ("var r = 'a,b,,c'.split(',').length + ',' + 'abc'.split('').join('-') + ',' + 'abc'.split().length + ',' + 'a-b-c'.split('-', 2).join('+') + ',' + ''.split(',').length + ',' + 'ab'.split('ab').length;", "r", "4,a-b-c,1,a+b,1,2"),
        ("var r = 'Hello'.toUpperCase() + 'WORLD'.toLowerCase() + 'straße'.toUpperCase();", "r", "HELLOworldSTRASSE"),
        ("var r = '[' + '  a b  '.trim() + '][' + '  a '.trimStart() + '][' + '  a '.trimEnd() + '][' + '   '.trim() + ']';", "r", "[a b][a ][  a][]"),
        ("var r = 'ab'.concat('c', 1, null) + 'ab'.repeat(3) + '|' + 'ab'.repeat(0) + '|' + '5'.padStart(3, '0') + 'x'.padEnd(3, 'ab') + '|' + 'abc'.padStart(2);", "r", "abc1nullababab||005xab|abc"),
        ("var r = 'a-b-c'.replace('-', '+') + ',' + 'a-b-c'.replaceAll('-', '+') + ',' + 'abc'.replace('z', 'y') + ',' + 'abc'.replace('b', '[$&]') + ',' + 'abc'.replace('b', '$$') + ',' + 'abc'.replace('b', \"$`|$'\");", "r", "a+b-c,a+b+c,abc,a[b]c,a$c,aa|cc"),
        ("var r = 'abc'.replace('b', function (m, i, s) { return m.toUpperCase() + i + s.length; }) + ',' + 'aXbX'.replaceAll('X', function (m, i) { return i; });", "r", "aB13c,a1b3"),
        ("var r = 'ab'.replaceAll('', '-') + ',' + 'abc'.replace('', '_');", "r", "-a-b-,_abc"),
        ("var r = 'b'.localeCompare('a') + ',' + 'a'.localeCompare('a') + ',' + 'a'.localeCompare('b') + ',' + 'x'.toString() + 'y'.valueOf();", "r", "1,0,-1,xy"),
        ("var r = String.fromCharCode(72, 105) + String(12) + String(true) + String() + 'x'.length + typeof String;", "r", "Hi12true1function"),
        ("var s = ''; for (var i = 0; i < 40; i++) s += String.fromCharCode(97 + i % 26); var r = s.length + s.slice(26, 30) + s.toUpperCase().indexOf('Z');", "r", "40abcd25"),
        ("var r = 'é'.length + 'é'.toUpperCase() + '日本'.charAt(1) + '日本'.charCodeAt(0) + '日本語'.slice(1) + '\\u{1F600}'.length;", "r", "1É本26085本語2"),
        ("try { String.prototype.trim.call(null); } catch (e) { var r = e.name; }", "r", "TypeError"),
        ("var r = typeof new String('a') + new String('ab').length;", "r", "object2"),
    ]);
}

#[test]
fn number_boolean_math_and_global_functions() {
    check_builtins(&[
        ("var r = Number('12') + Number('') + Number(null) + Number(true) + Number([5]) + ',' + Number(undefined) + ',' + Number('x') + ',' + Number();", "r", "18,NaN,NaN,0"),
        ("var r = (255).toString(16) + ',' + (255).toString(2) + ',' + (-255).toString(36) + ',' + (0.5).toString(2) + ',' + (10).toString() + ',' + (1.5).toString();", "r", "ff,11111111,-73,0.1,10,1.5"),
        ("var r = (1.005).toFixed(2) + ',' + (2).toFixed(2) + ',' + (-0.0001).toFixed(2) + ',' + (123.456).toFixed(1) + ',' + (0).toFixed(0);", "r", "1.00,2.00,0.00,123.5,0"),
        ("var r = Number.isInteger(5) + ',' + Number.isInteger(5.5) + ',' + Number.isInteger('5') + ',' + Number.isFinite(1 / 0) + ',' + Number.isNaN(NaN) + ',' + Number.isNaN('x') + ',' + Number.isSafeInteger(2 ** 53) + ',' + Number.MAX_SAFE_INTEGER;", "r", "true,false,false,false,true,false,false,9007199254740991"),
        ("var r = parseInt('42px') + ',' + parseInt('0x1f') + ',' + parseInt('  -7') + ',' + parseInt('z', 36) + ',' + parseInt('101', 2) + ',' + parseInt('x') + ',' + parseInt('3.9') + ',' + parseInt('') + ',' + parseInt('12', 1);", "r", "42,31,-7,35,5,NaN,3,NaN,NaN"),
        ("var r = parseFloat('3.14abc') + ',' + parseFloat('.5') + ',' + parseFloat('-1e3x') + ',' + parseFloat('abc') + ',' + parseFloat('1e') + ',' + parseFloat('Infinity') + ',' + Number.parseFloat('2.5') + ',' + Number.parseInt('8');", "r", "3.14,0.5,-1000,NaN,1,Infinity,2.5,8"),
        ("var r = isNaN('x') + ',' + isNaN('5') + ',' + isFinite('5') + ',' + isFinite(Infinity) + ',' + isNaN(undefined);", "r", "true,false,true,false,true"),
        ("var r = Boolean(0) + ',' + Boolean('a') + ',' + Boolean([]) + ',' + Boolean() + ',' + true.toString() + ',' + false.valueOf() + ',' + (5).valueOf();", "r", "false,true,true,false,true,false,5"),
        ("var r = Math.floor(2.7) + ',' + Math.floor(-2.1) + ',' + Math.ceil(2.1) + ',' + Math.ceil(-2.7) + ',' + Math.round(2.5) + ',' + Math.round(-2.5) + ',' + Math.round(2.4) + ',' + Math.trunc(-2.9) + ',' + Math.round(-0.4);", "r", "2,-3,3,-2,3,-2,2,-2,0"),
        ("var r = Math.abs(-3) + ',' + Math.sign(-5) + ',' + Math.sign(0) + ',' + Math.sqrt(16) + ',' + Math.cbrt(27) + ',' + Math.pow(2, 10) + ',' + Math.pow(2, -1) + ',' + Math.hypot(3, 4) + ',' + Math.imul(65536, 65536) + ',' + Math.imul(3, 4);", "r", "3,-1,0,4,3,1024,0.5,5,0,12"),
        ("var r = Math.max(1, 5, 3) + ',' + Math.min(4, 2, 8) + ',' + Math.max() + ',' + Math.min() + ',' + Math.max(1, NaN) + ',' + Math.max('7', 2) + ',' + Math.max.apply(null, [1, 9, 4]);", "r", "5,2,-Infinity,Infinity,NaN,7,9"),
        ("var r = Math.PI.toFixed(5) + ',' + Math.E.toFixed(3) + ',' + Math.log(1) + ',' + Math.exp(0) + ',' + Math.log2(8) + ',' + Math.log10(1000) + ',' + Math.sin(0) + ',' + Math.cos(0) + ',' + Math.atan2(0, 1) + ',' + Math.fround(5.5);", "r", "3.14159,2.718,0,1,3,3,0,1,0,5.5"),
        ("var ok = true; for (var i = 0; i < 20; i++) { var x = Math.random(); if (!(x >= 0 && x < 1)) ok = false; } var r = ok + ',' + (Math.random() !== Math.random());", "r", "true,true"),
        ("var r = typeof globalThis + (globalThis === self) + (globalThis.Math === Math) + (this === globalThis) + typeof self.Object;", "r", "objecttruetruetruefunction"),
        ("var r = typeof __def + typeof Symbol + typeof undefined;", "r", "undefinedfunctionundefined"),
        ("var g = 1; var r = globalThis.g + ',' + self.parseInt('9') + ',' + (Number.NaN !== Number.NaN) + ',' + Number.EPSILON.toFixed(0) + ',' + (Number.MIN_VALUE > 0);", "r", "1,9,true,0,true"),
        ("var r = typeof new Number(1) + (new Number(5) + 1);", "r", "object6"),
        ("try { (5).toString(1); } catch (e) { var r = e.name; }", "r", "RangeError"),
        ("try { Number.prototype.toString.call('x'); } catch (e) { var r = e.name; }", "r", "TypeError"),
    ]);
}

#[test]
fn eval_load_programs_give_the_result_of_the_old_engine() {
    // The expected text is what the old engine gives (`eval_load CASE 1`, checked in the S2.7 step).
    let cases = [
        (include_str!("../examples/data/eval_closures.js"), "23222546"),
        (include_str!("../examples/data/eval_objects.js"), "19442497,119979"),
        (include_str!("../examples/data/eval_throws.js"), "69629"),
        (include_str!("../examples/data/eval_webpack.js"), "3890,150,50"),
    ];
    for (src, want) in cases {
        let src = eval_program(src);
        assert_eq!(ev(&src, "__result"), want, "{src:.80}");
        if !MIRI {
            assert_eq!(ev_gc(&src, "__result"), want, "under collection: {src:.80}");
        }
    }
}

#[test]
fn a_host_script_runs_before_the_script_and_the_directive_prologue_stays() {
    let mut vm = load_with("var h = 1;", "'use strict'; var r = h + 1; var s = (function () { return this === undefined; })();").unwrap();
    vm.run_main(0).unwrap();
    assert_eq!((vm.global_text("r"), vm.global_text("s")), ("2".into(), "true".into()));
    assert!(load_with("var h = ;", "var r = 1;").err().unwrap().starts_with("host script:"));
}

#[test]
fn a_top_level_let_const_and_class_are_visible_to_the_next_script() {
    let host = "let a = 1; const b = 2; class C { m() { return 3; } } let later; let o = { k: 5 };";
    let src = "var r = a + b + new C().m(); a = 10; var s = a; var t; try { b = 1; } catch (e) { t = e.name; }
               var u = typeof later + typeof C + typeof nothing; later = 4; var w = (function () { return a + later; })();
               var junk; for (var i = 0; i < 3000; i++) { junk = [i, { i: i }]; } var x = o.k + (this.a === undefined ? 'g' : 'p');
               var y; try { zzz; } catch (e) { y = e.name; }";
    let mut vm = load_with(host, src).unwrap();
    vm.run_main(0).unwrap();
    let got: Vec<String> = ["r", "s", "t", "u", "w", "x", "y"].iter().map(|n| vm.global_text(n)).collect();
    assert_eq!(got, ["6", "10", "TypeError", "undefinedfunctionundefined", "14", "5g", "ReferenceError"]);
    // The names are not properties of the global object, and a name that is declared twice is a SyntaxError.
    let mut vm = load_with("let a = 1;", "let a = 2;").unwrap();
    assert!(vm.run_main(0).is_err());
    let mut vm = load_with("var a = 1;", "let a = 2;").unwrap();
    assert!(vm.run_main(0).is_err());
    // The check comes before the first statement of the script.
    let mut vm = load_with("let a = 1;", "var side = 1; let a = 2;").unwrap();
    assert!(vm.run_main(0).is_err());
    assert_eq!(vm.global_text("side"), "undefined");
    let mut vm = load_with("let a = 1;", "var a = 2;").unwrap();
    assert!(vm.run_main(0).is_err());
    let mut vm = load_with("let a = 1;", "var r = typeof globalThis.a + delete a;").unwrap();
    vm.run_main(0).unwrap();
    assert_eq!(vm.global_text("r"), "undefinedfalse");
}

/// Run the script `a` (it may throw), then the script `b`, in one realm; read the global `name` as text. `gc`: constant collection.
fn two_scripts(a: &str, b: &str, name: &str, gc: bool) -> Result<String, String> {
    let shared = Arc::new(SharedAtoms::default());
    let mut prog = compile(b, &shared, Limits::default())?;
    let pre = prog.codes.len();
    prog.codes = compile_after(prog.codes, PRELUDE, &shared, Limits::default(), None, None)?.0;
    let at = prog.codes.len();
    prog.codes = compile_after(prog.codes, a, &shared, Limits::default(), None, None)?.0;
    let mut vm = Vm::new(prog, shared);
    vm.run_main(pre).expect("the prelude");
    if gc {
        vm.realm.heap.set_trigger(Trigger { min: 1 << 10, slack: 1 << 10 });
    }
    assert!(vm.run_main(at).is_err(), "script a must throw: {a}");
    vm.run_main(0).map_err(|e| format!("{e:?}"))?;
    Ok(vm.global_text(name))
}

#[test]
fn a_top_level_name_stays_after_the_script_throws() {
    let catch = "var r; try { NAME; r = 'value'; } catch (e) { r = e.name; }";
    for gc in [false, true] {
        let ok = |a: &str, b: &str| two_scripts(a, b, "r", gc).unwrap();
        // A name that is initialized before the throw keeps its value (`let`, `const`, `class`, and a heap value).
        assert_eq!(ok("let x = 1; throw 0;", "var r = x;"), "1");
        assert_eq!(ok("let y; throw 0; y = 2;", "var r = typeof y;"), "undefined");
        assert_eq!(ok("const k = 5; throw 0;", "var r = k; try { k = 1; } catch (e) { r += e.name; }"), "5TypeError");
        assert_eq!(ok("class C { m() { return 3; } } throw 0;", "var r = new C().m();"), "3");
        assert_eq!(ok("let o = { a: [1, 2, 3] }; for (var i = 0; i < 2000; i++) { var j = [i]; } throw 0;", "var r = o.a.length;"), "3");
        // A name that is declared after the throw is in its dead zone.
        assert_eq!(ok("throw 0; let z = 2;", &catch.replace("NAME", "z")), "ReferenceError");
        assert_eq!(ok("let a = 1; throw 0; const b = 2;", "var r = a; try { b; } catch (e) { r += e.name; }"), "1ReferenceError");
        assert_eq!(ok("throw 0; class D {}", &catch.replace("NAME", "D")), "ReferenceError");
        // A throw inside a function that the script calls, and a throw that a `try` of the script does not catch.
        assert_eq!(ok("let x = 1; function f() { throw 0; } f();", "var r = x;"), "1");
        assert_eq!(ok("let x = 1; try { throw 0; } finally { x = 2; }", "var r = x;"), "2");
        // The name is declared: a later `let`, `var` or function of the name is a SyntaxError, also for a name in its dead zone.
        for a in ["let x = 1; throw 0;", "throw 0; let x = 1;"] {
            for b in ["let x = 2;", "var x = 2;", "function x() {}"] {
                assert!(two_scripts(a, b, "r", gc).is_err(), "{a} then {b}");
            }
        }
    }
}

#[test]
fn a_parenthesized_optional_member_call_keeps_this() {
    let o = "var o = { m() { return this.v; }, v: 7 }; var n = null; ";
    check(&[
        (&format!("{o} var r = (o?.m)();"), "r", "7"),
        (&format!("{o} var r = (o?.m)?.();"), "r", "7"),
        (&format!("{o} var r = (o.m)?.();"), "r", "7"),
        (&format!("{o} var r = 0; try {{ (n?.m)(); }} catch (e) {{ r = e.name; }}"), "r", "TypeError"),
        (&format!("{o} var r = (n?.m)?.();"), "r", "undefined"),
    ]);
}

#[test]
fn built_ins_survive_constant_collection_with_many_live_values() {
    // Run in Miri too (one program): sort, map, filter, split, join, slice, splice, concat, bind and Object.entries, all holding many
    // fresh cells while the collector runs at nearly every allocation.
    check_gc(&[(
        "var words = []; for (var i = 0; i < 24; i++) words.push('w' + ((i * 7) % 24));
         var sorted = words.slice().sort();
         var lens = sorted.map(function (w) { return w.length; }).filter(function (n) { return n == 3; }).length;
         var parts = sorted.join('/').split('/');
         var o = {}; parts.forEach(function (p, i) { o[p] = i; });
         var e = Object.entries(o).map(function (kv) { return kv[0] + kv[1]; });
         var sp = e.slice(); sp.splice(2, 3, 'x', 'y'); var all = e.concat(sp, [sorted]);
         var add = function (a, b) { return a + this.k + b; }; var bound = add.bind({k: '-'}, 'L');
         var tail = [bound('R'), String(sorted.slice(0, 2)), Object.keys(o).length, all.length, lens].join();
         var r = parts[0] + ',' + parts[23] + ',' + e[5] + ',' + sp.length + ',' + tail;",
        "r",
        "w0,w9,w135,23,L-R,w0,w1,24,48,14",
    )]);
}

// ---- switch with a jump table ----

/// `src` with every `case LITERAL:` turned into `case (void 0, LITERAL):`, which is no constant: the compiler keeps the chain of compares.
fn as_chain(src: &str) -> String {
    let mut out = String::new();
    let mut rest = src;
    while let Some(i) = rest.find("case ") {
        let after = &rest[i + 5..];
        let end = after.find(':').expect("a case has a colon");
        out.push_str(&rest[..i]);
        out.push_str(&format!("case (void 0, {}):", &after[..end]));
        rest = &after[end + 1..];
    }
    out + rest
}

/// The ops of the table switches in the code of `src` (the script and every function; all pass the verifier).
fn table_ops(src: &str) -> usize {
    // The atoms of the code live in `shared`: it must outlive the verifier.
    let shared = crate::atoms::SharedAtoms::default();
    let prog = crate::script::compile(src, &shared, Default::default()).expect("compile");
    prog.codes
        .iter()
        .map(|c| {
            crate::verify::verify(c).unwrap_or_else(|e| panic!("{e}\n{}", c.disassemble()));
            c.disassemble().lines().filter(|l| l.contains(" SwitchI ") || l.contains(" SwitchA ")).count()
        })
        .sum()
}

/// A function `sw(v)` with the `switch` body `cases` (`r` is a string that it builds, `return r` follows), run on each of `vals`.
/// The text of all the results is the same with the table, with the chain, and under constant collection.
fn switch_same(cases: &str, vals: &str, want: &str) {
    let src = format!(
        "function sw(v) {{ var r = ''; switch (v) {{ {cases} }} return r; }} var vals = [{vals}]; var out = ''; for (var i = 0; i < vals.length; i++) out += sw(vals[i]) + ','; var result = out;"
    );
    let chain = as_chain(&src);
    assert_eq!(table_ops(&src), 1, "no table: {src}");
    assert_eq!(table_ops(&chain), 0, "a table in the chain version: {chain}");
    check(&[(&src, "result", want)]);
    check(&[(&chain, "result", want)]);
}

#[test]
fn a_dense_integer_switch_dispatches_like_the_chain() {
    // `default` in the middle, fallthrough from 0 to 1, a gap at 2 and 4, a `break`, and a case after the default.
    let cases = "case 0: r += 'a'; case 1: r += 'b'; break; default: r += 'd'; case 5: r += 'e'; break; case 3: r += 'c';";
    // Ints, doubles that are integers (`6 / 2`, `-0`), values out of range, fractions, NaN, other types.
    let vals = "0, 1, 2, 3, 4, 5, 6, -1, 1.0, 6 / 2, -0, 0 / 0, 1.5, 2147483648, -2147483649, 1e300, -1e300, 1 / 0, '1', '0', null, undefined, true, false, {}";
    // The first 11 values are 0, 1, 2, 3, 4, 5, 6, -1, 1.0, 3.0 and -0: 2, 4, 6 and -1 get the default and fall into `case 5`.
    let want = format!("ab,b,de,c,de,e,de,de,b,c,ab,{}", "de,".repeat(14));
    switch_same(cases, vals, &want);
}

#[test]
fn a_switch_on_negative_and_shifted_integers() {
    // A table that starts below 0, and one that ends at the top of the 32-bit range.
    switch_same(
        "case -2: r += 'a'; break; case -1: r += 'b'; break; case 1: r += 'c'; break;",
        "-3, -2, -1, 0, 1, 2, -2.0, -0",
        ",a,b,,c,,a,,",
    );
    switch_same(
        "case 2147483645: r += 'a'; break; case 2147483646: r += 'b'; break; case 2147483647: r += 'c'; break; default: r += 'd';",
        "2147483644, 2147483645, 2147483646, 2147483647, 2147483648, -2147483648, 2147483647.0, -1",
        "d,a,b,c,d,d,c,d,",
    );
}

#[test]
fn a_string_switch_dispatches_like_the_chain() {
    // One-byte and two-byte cases, an empty string, `default` in the middle and a case with a fallthrough.
    let cases = "case 'a': r += 1; case 'bb': r += 2; break; default: r += 'd'; case 'ccc': r += 3; break; case 'é': r += 4; case '日本': r += 5; break; case '': r += 6;";
    // A string made by `+` is not an atom (its hash is not known yet): 'b' + 'b' is the case 'bb', '日' + '本' is the case '日本'.
    // `1` and `'1'` and objects are not any case. A string that differs in the last unit or in its width is not a case either.
    let vals = "'a', 'bb', 'ccc', 'é', '日本', '', 'A', 'b', 'bbb', 'ab', 'b' + 'b', '日' + '本', '日' + '木', 1, null, undefined, {}, 'ccd', 'cc', 'e'";
    // 'a' falls into 'bb' (1, 2). 'é' falls into '日本' (4, 5). The empty string is the last case: only 6.
    let want = "12,2,3,45,5,6,d3,d3,d3,d3,2,5,d3,d3,d3,d3,d3,d3,d3,d3,";
    switch_same(cases, vals, want);
}

#[test]
fn a_string_switch_flattens_a_rope() {
    // A string of 66 units made by `+` is a rope: the hash needs its units. The case has the same 66 units.
    let long = "0123456789012345678901234567890123";
    let cases = format!("case '{long}{long}': r += 'long'; break; case 'x': r += 'x'; break; case 'y': r += 'y'; break;");
    let vals = format!("'{long}' + '{long}', '{long}', 'x', ('{long}' + '{long}') + 'z'");
    switch_same(&cases, &vals, "long,,x,,");
}

#[test]
fn a_switch_of_many_cases_finds_each_one() {
    // 40 cases: the integer table has 40 entries; the string table has 64 slots (at most 3/4 full), so keys collide and the probe runs. Each key finds its own
    // case and a key that is not a case gets the default. The names `'s' + k` are made at run time, so no atom is the discriminant.
    let ints: String = (0..40).map(|k| format!("case {k}: r += {}; break; ", k * 7)).collect();
    let strs: String = (0..40).map(|k| format!("case 's{k}': r += {}; break; ", k * 7)).collect();
    for (cases, key) in [(ints, "k"), (strs, "'s' + k")] {
        let src = format!(
            "function sw(v) {{ var r = 0; switch (v) {{ {cases} default: r = -1; }} return r; }} var sum = 0, bad = 0; for (var k = 0; k < 45; k++) {{ var got = sw({key}); if (k < 40 ? got !== k * 7 : got !== -1) bad++; sum += got; }} var result = bad + ':' + sum;"
        );
        assert_eq!(table_ops(&src), 1);
        check(&[(&src, "result", &format!("0:{}", (0..40).map(|k| k * 7).sum::<i32>() - 5))]);
    }
}

#[test]
fn a_switch_with_a_case_that_is_not_a_constant_keeps_the_order_of_evaluation() {
    // The cases `t(1)` and `t(2)` run in order until one matches: the log is `12`, and `t(9)` after the match never runs.
    let src = "var log = ''; function t(x) { log += x; return x; } function sw(v) { switch (v) { case 3: return 'c'; case 4: return 'd'; case 5: return 'e'; case t(1): return 'a'; case t(2): return 'b'; case t(9): return 'z'; } return '-'; } var result = sw(2) + ':' + log;";
    assert_eq!(table_ops(src), 0);
    check(&[(src, "result", "b:12")]);
    // The discriminant is evaluated once, and before any case.
    let src = "var n = 0; function d() { n++; return 3; } function sw() { switch (d()) { case 1: return 'a'; case 2: return 'b'; case 3: return 'c'; case 4: return 'd'; } } var result = sw() + n;";
    assert_eq!(table_ops(src), 1);
    check(&[(src, "result", "c1")]);
}

#[test]
fn switch_in_loops_with_continue_break_labels_and_lexical_cases() {
    // `continue` and `break` out of the switch, a nested switch, a labeled `break` out of two, and a `let` in a case that a closure keeps.
    let src = "function f(v) { var out = ''; for (var i = 0; i < 6; i++) { switch (v + i) { case 1: out += 'a'; continue; case 2: out += 'b'; break; case 3: { let q = i; out += q; } case 4: out += 'c'; break; default: out += '-'; } out += '.'; } return out; } var result = f(0);";
    assert_eq!(table_ops(src), 1);
    // i = 0 gives 0: default `-.`; i = 1: `a` and continue (no `.`); i = 2: `b.`; i = 3: `3c.`; i = 4: `c.`; i = 5: `-.`.
    check(&[(src, "result", "-.ab.3c.c.-.")]);
    let src = "function f() { var out = ''; outer: for (var i = 0; i < 3; i++) { switch (i) { case 0: out += 'x'; break; case 1: switch ('k') { case 'a': out += 'a'; break; case 'b': out += 'b'; break; case 'k': out += 'K'; break outer; } case 2: out += 'two'; break; case 3: out += 'three'; } out += ';'; } return out; } var result = f();";
    assert_eq!(table_ops(src), 2);
    check(&[(src, "result", "x;K")]);
    // A closure that captures a `let` of the switch scope, and a read of it before its case ran (the dead zone).
    let src = "function f(v) { var g = null; switch (v) { case 1: let x = 10; g = () => x; break; case 2: let y = 20; g = () => y; break; case 3: try { x = 1; } catch (e) { g = () => 'tdz'; } break; case 4: break; } return g ? g() : 'none'; } var result = '' + f(1) + f(2) + f(3) + f(4) + f(5);";
    assert_eq!(table_ops(src), 1);
    check(&[(src, "result", "1020tdznonenone")]);
}

#[test]
fn built_in_properties_have_the_attributes_the_specification_gives() {
    check_builtins(&[
        ("var d = Object.getOwnPropertyDescriptor(Array.prototype, 'push'); var r = [d.writable, d.enumerable, d.configurable].join();", "r", "true,false,true"),
        ("var d = Object.getOwnPropertyDescriptor(Array.prototype.push, 'length'); var r = [d.value, d.writable, d.enumerable, d.configurable].join();", "r", "1,false,false,true"),
        ("var r = Object.getOwnPropertyNames(Array.prototype.push).join();", "r", "length,name"),
        ("var r = Array.prototype.splice.length + ',' + Array.prototype.reduce.length + ',' + Array.prototype.forEach.name + ',' + Object.assign.length;", "r", "2,1,forEach,2"),
        ("var d = Object.getOwnPropertyDescriptor(Array, 'prototype'); var r = [d.writable, d.enumerable, d.configurable].join();", "r", "false,false,false"),
        ("var d = Object.getOwnPropertyDescriptor(Math, 'PI'); var r = [d.writable, d.enumerable, d.configurable].join();", "r", "false,false,false"),
        ("var r = RangeError.length + ',' + Object.getPrototypeOf(RangeError).name + ',' + Object.prototype.toString.call(RangeError.prototype) + ',' + (new RangeError('x', {cause: 1}).cause);", "r", "1,Error,[object Object],1"),
        ("var f = Array.prototype.map.bind(null, 1); var r = f.length + ',' + f.name;", "r", "0,bound map"),
    ]);
}

#[test]
fn accessors_in_descriptors_assign_spread_and_rest() {
    check_builtins(&[
        ("var o = {}; Object.defineProperty(o, 'x', {get: function () { return 7; }, configurable: true}); var d = Object.getOwnPropertyDescriptor(o, 'x'); var r = o.x + ',' + typeof d.get + ',' + d.set + ',' + d.enumerable;", "r", "7,function,undefined,false"),
        ("var log = ''; var s = {get a() { log += 'a'; return 1; }, b: 2}; var t = Object.assign({}, s); var r = t.a + ',' + t.b + ',' + log + ',' + Object.getOwnPropertyDescriptor(t, 'a').writable;", "r", "1,2,a,true"),
        ("var t = {set z(v) { this.w = v * 2; }}; Object.assign(t, {z: 4}); var r = t.w;", "r", "8"),
        ("var s = {get a() { return 1; }, b: 2}; var c = {...s}; var r = Object.getOwnPropertyDescriptor(c, 'a').value + ',' + c.b;", "r", "1,2"),
        ("var s = {get a() { return 1; }, b: 2, c: 3}; var {b, ...rest} = s; var r = b + ',' + Object.keys(rest).join();", "r", "2,a,c"),
        ("var r = Object.entries({get a() { return 1; }, b: 2}).join('|');", "r", "a,1|b,2"),
        ("var o = {a: 1}; var p = {b: 2}; o.__proto__ = p; var r = o.b + ',' + (o.__proto__ === p);", "r", "2,true"),
        ("var r; try { var o = {}; Object.setPrototypeOf(Object.create(o), o); var c = Object.create(o); Object.setPrototypeOf(o, c); } catch (e) { r = e.name; }", "r", "TypeError"),
        ("var o = Object.defineProperties({}, {a: {value: 1, enumerable: true}, b: {get: function () { return 2; }}}); var r = o.a + o.b + ',' + Object.keys(o).join();", "r", "3,a"),
    ]);
}

#[test]
fn frozen_sealed_and_non_extensible_objects() {
    check_builtins(&[
        ("var o = Object.preventExtensions({a: 1}); o.b = 2; var r = Object.isExtensible(o) + ',' + o.b + ',' + o.a;", "r", "false,undefined,1"),
        ("var o = Object.freeze({a: 1}); o.a = 2; delete o.a; var r = o.a + ',' + Object.isFrozen(o) + ',' + Object.isSealed(o);", "r", "1,true,true"),
        ("var o = Object.seal({a: 1}); o.a = 2; delete o.a; o.b = 1; var r = o.a + ',' + o.b + ',' + Object.isFrozen(o) + ',' + Object.isSealed(o);", "r", "2,undefined,false,true"),
        ("var a = Object.freeze([1, 2]); a[0] = 9; var r = a[0] + ',' + a.length + ',' + Object.isFrozen(a);", "r", "1,2,true"),
        ("var a = Object.freeze([1, 2]); var r; try { a.push(3); } catch (e) { r = e.name + a.length; }", "r", "TypeError2"),
        ("var a = Object.preventExtensions([1]); a[1] = 2; a[0] = 5; var r = a.length + ',' + a[0];", "r", "1,5"),
        ("var a = [1, 2, 3]; Object.defineProperty(a, 'length', {writable: false}); a[5] = 1; var r = a.length + ',' + a[5];", "r", "3,undefined"),
    ]);
}

#[test]
fn array_methods_work_on_array_likes_and_follow_the_specification() {
    check_builtins(&[
        ("var o = {length: 3, 0: 'a', 1: 'b', 2: 'c'}; var r = Array.prototype.join.call(o, '-') + ',' + Array.prototype.slice.call(o, 1).join() + ',' + Array.prototype.indexOf.call(o, 'c');", "r", "a-b-c,b,c,2"),
        ("var o = {length: 2, 0: 1, 1: 2}; Array.prototype.push.call(o, 3); Array.prototype.reverse.call(o); var r = o.length + ',' + o[0] + o[1] + o[2];", "r", "3,321"),
        ("var o = {length: 3, 0: 1, 1: 2, 2: 3}; var m = Array.prototype.map.call(o, function (x) { return x * 2; }); var r = m.join() + ',' + Array.prototype.every.call(o, function (x) { return x > 0; });", "r", "2,4,6,true"),
        ("var r = Array.prototype.concat.call(1, [2], [3, [4]]).length;", "r", "4"),
        ("var r = [1, 2, 3, 4].at(-1) + ',' + [1, 2, 3].with(1, 9).join() + ',' + [3, 1, 2].toSorted().join() + ',' + [1, 2, 3].toReversed().join() + ',' + [1, 2, 3, 4].toSpliced(1, 2, 'x').join();", "r", "4,1,9,3,1,2,3,3,2,1,1,x,4"),
        ("var a = [1, 2, 3, 4, 5]; a.copyWithin(0, 3); a.fill(0, 3); var r = a.join();", "r", "4,5,3,0,0"),
        ("var r = [1, [2, [3, [4]]]].flat(2).join() + ',' + [1, 2].flatMap(function (x) { return [x, x * 10]; }).join() + ',' + [1, 2, 3].findLast(function (x) { return x < 3; }) + ',' + [1, 2, 3].findLastIndex(function (x) { return x > 5; });", "r", "1,2,3,4,1,10,2,20,2,-1"),
        ("var r = Array.of(7, 8).join() + ',' + Array.from({length: 2, 0: 'x', 1: 'y'}).join();", "r", "7,8,x,y"),
        ("var r; try { new Array(-1); } catch (e) { r = e.name; }", "r", "RangeError"),
        ("var a = [1, , 3]; var r = a.indexOf(undefined) + ',' + a.includes(undefined) + ',' + a.lastIndexOf(3) + ',' + [NaN].includes(NaN) + ',' + [NaN].indexOf(NaN);", "r", "-1,true,2,true,-1"),
        ("var a = []; a[2] = 1; var r = a.slice().length + ',' + (0 in a.slice()) + ',' + a.splice(1, 2).length + ',' + a.length;", "r", "3,false,2,1"),
        ("var r; try { Array.prototype.pop.call(''); } catch (e) { r = e.name; }", "r", "TypeError"),
    ]);
}

fn waiting(vm: &Vm) -> usize {
    vm.realm.prog.iter().filter(|c| c.words.first().is_some_and(|w| *w as u8 == crate::bytecode::Op::Lazy as u8)).count()
}

#[test]
fn prelude_functions_are_compiled_at_their_first_call() {
    let src = "var r = [3, 1, 2].sort().join(); var m = [1, 2].map(function (x) { return x * 2; }).join(); var s = [1, 2, 3].reduce(function (a, b) { return a + b; });";
    let mut vm = load(src).unwrap();
    let before = waiting(&vm);
    assert!(before > 40, "the methods of the prelude wait: {before}");
    vm.run_main(0).unwrap();
    let after = waiting(&vm);
    assert!(after < before && after > before - 25, "sort, map, reduce and the helpers they call were compiled: {before} -> {after}");
    assert_eq!((vm.global_text("r"), vm.global_text("m"), vm.global_text("s")), ("1,2,3".into(), "2,4".into(), "6".into()));
    // A second call compiles nothing.
    let mut vm2 = load("var a = [2, 1]; a.sort(); a.sort(); a.sort();").unwrap();
    vm2.run_main(0).unwrap();
    let once = waiting(&vm2);
    let mut vm3 = load("var a = [2, 1]; a.sort();").unwrap();
    vm3.run_main(0).unwrap();
    assert_eq!(waiting(&vm3), once);
}

#[test]
fn a_function_that_waits_has_its_name_and_length_before_and_after_its_first_call() {
    check(&[
        ("var r = Array.prototype.sort.name + Array.prototype.sort.length + Array.prototype.reduce.name + Array.prototype.reduce.length + Array.prototype.forEach.length;", "r", "sort1reduce11"),
        ("var a = [3, 1, 2]; a.sort(); a.reduce(function (x, y) { return x + y; }); var r = Array.prototype.sort.name + Array.prototype.sort.length + Array.prototype.reduce.name + Array.prototype.reduce.length;", "r", "sort1reduce1"),
        ("var d = Object.getOwnPropertyDescriptor(Array.prototype.map, 'name'); var r = d.value + ',' + d.writable + ',' + d.configurable;", "r", "map,false,true"),
    ]);
}

#[test]
fn a_lazy_function_gets_its_arguments_the_first_time_and_after() {
    // `reduce` reads `arguments`; the callback of `forEach` gets 3 arguments; `Array.from` has parameters and reads `arguments`.
    check(&[
        ("var r = [1, 2, 3].reduce(function (a, b) { return a + b; }, 10);", "r", "16"),
        ("var r = [1, 2, 3].reduce(function (a, b) { return a + b; });", "r", "6"),
        ("var n = 0; [1, 2, 3].forEach(function (v, i, a) { n += v * 10 + i + a.length; });", "n", "72"),
        ("var r = Array.from([1, 2, 3], function (x) { return x + 1; }).join();", "r", "2,3,4"),
        ("var f = Array.prototype.map; var r = f.call([1, 2], function (x) { return x; }, undefined, 5, 6).join();", "r", "1,2"),
        ("var r = Array.of(7, 8, 9).join(); var q = Array.of(1).join();", "q", "1"),
    ]);
}

#[test]
fn a_lazy_compile_that_fails_throws_a_plain_error() {
    struct Fails;
    impl crate::interp::LazyCompile for Fails {
        fn compile(&mut self, _: &crate::codes::Codes, _: usize) -> Result<(), String> {
            Err("no compiler".into())
        }
    }
    let mut vm = load("var e; try { [1].forEach(function () {}); } catch (x) { e = x.name + ': ' + x.message; }").unwrap();
    vm.realm.lazy = Some(Box::new(Fails));
    vm.run_main(0).unwrap();
    assert_eq!(vm.global_text("e"), "Error: no compiler");
}

#[test]
fn the_names_of_typeof_are_shared_atoms_like_the_string_literals() {
    // A literal is loaded as its shared atom, so `typeof x === 'object'` is a pointer test only if `typeof` gives the shared atom. A
    // script that has none of these words must not change that (the prelude is compiled at the first call of its functions).
    let vm = load("var x = 1;").unwrap();
    for v in [Value::UNDEFINED, Value::NULL, Value::bool(true), Value::int(1), vm.realm.global.value()] {
        let s = Realm::str_of(vm.realm.typeof_name(v)).expect("a string");
        assert!(s.is_shared(), "{}", vm.realm.text(s));
    }
}

/// `load` runs the prelude and then hides `__def`. It must not delete it: a delete of a property that is not the last one puts the global
/// object in dictionary mode, and then no global access can be cached (`Shapes::cacheable`).
#[test]
fn load_keeps_the_global_object_in_the_shape_tree() {
    let vm = load("var x = 1;").unwrap();
    let id = vm.realm.global.shape(&vm.realm.shapes);
    assert!(!vm.realm.shapes.get(id).is_dict(), "the global object is in dictionary mode after load");
    assert!(vm.realm.shapes.cacheable(id));
}

/// A script cannot read the function that `__def` was, and the name is not enumerable.
#[test]
fn def_is_hidden_after_load() {
    check(&[
        ("var r = typeof __def + Object.keys(globalThis).indexOf('__def');", "r", "undefined-1"),
        ("var __def = 5; var r = __def;", "r", "5"),
        ("var r = typeof __helper + typeof __set + Object.keys(globalThis).indexOf('__helper') + Object.keys(globalThis).indexOf('__set');", "r", "undefinedundefined-1-1"),
    ]);
}

/// A write that a frozen, sealed or non-extensible object refuses is silent in sloppy code and a `TypeError` in strict code, for a
/// named property, an index, the `length` of an array and a new property.
#[test]
fn a_refused_write_throws_in_strict_code_only() {
    let strict = |body: &str| format!("'use strict'; var r; try {{ {body}; r = 'no throw'; }} catch (e) {{ r = e.name; }}");
    check(&[
        (&strict("var o = Object.freeze({a: 1}); o.a = 2"), "r", "TypeError"),
        (&strict("var o = Object.preventExtensions({a: 1}); o.b = 2"), "r", "TypeError"),
        (&strict("var o = Object.preventExtensions({a: 1}); o.a = 2"), "r", "no throw"),
        (&strict("var a = Object.freeze([1, 2]); a[0] = 5"), "r", "TypeError"),
        (&strict("var a = Object.freeze([1, 2]); a[5] = 5"), "r", "TypeError"),
        (&strict("var a = Object.freeze([1, 2]); a.length = 0"), "r", "TypeError"),
        (&strict("var a = Object.freeze([1, 2]); a.push(3)"), "r", "TypeError"),
        (&strict("var o = {}; Object.defineProperty(o, 'x', {value: 1}); o.x = 2"), "r", "TypeError"),
        (&strict("var p = Object.freeze({x: 1}); var o = Object.create(p); o.x = 2"), "r", "TypeError"),
        (&strict("var o = {get g() { return 1; }}; o.g = 2"), "r", "TypeError"),
        (&strict("var o = Object.seal({a: 1}); o.a = 2; delete o.a"), "r", "TypeError"),
        ("var o = Object.freeze({a: 1}); o.a = 2; o.b = 3; var a = Object.freeze([1]); a[0] = 2; a[3] = 1; a.length = 0; var r = o.a + ',' + o.b + ',' + a.join() + ',' + a.length;", "r", "1,undefined,1,1"),
    ]);
}

/// A script function has its `length` and `name` made at the first look at its own properties, a built-in at the first use of one of
/// them: `freeze`, `isFrozen`, `defineProperty` and the descriptors must see both.
#[test]
fn freeze_and_define_see_the_lazy_length_and_name_of_functions() {
    check(&[
        ("function f(a, b) {} Object.freeze(f); var d = Object.getOwnPropertyDescriptor(f, 'name'); var r = d.value + ',' + d.writable + ',' + d.configurable + ',' + Object.isFrozen(f) + ',' + f.length;", "r", "f,false,false,true,2"),
        ("var m = Math.max; Object.freeze(m); var d = Object.getOwnPropertyDescriptor(m, 'length'); var r = d.value + ',' + d.configurable + ',' + Object.isFrozen(m) + ',' + Object.isFrozen(Math.min);", "r", "2,false,true,false"),
        ("var g = function () {}; var r = Object.isFrozen(g) + ',' + Object.isSealed(Object.seal(g)) + ',' + Object.getOwnPropertyNames(g).slice(0, 2).join();", "r", "false,true,length,name"),
        ("function f() {} Object.defineProperty(f, 'x', {value: 1}); var n = Object.getOwnPropertyNames(f); var r = n.slice(0, 2).join() + ',' + (n.indexOf('x') > 1);", "r", "length,name,true"),
        ("function f() {} Object.preventExtensions(f); var r = f.name + ',' + f.length + ',' + Object.getOwnPropertyNames(f).slice(0, 2).join();", "r", "f,0,length,name"),
        ("var f = Array.prototype.push; Object.preventExtensions(f); var r = f.name + ',' + f.length;", "r", "push,1"),
    ]);
}

/// `Vm::run` keeps the base of the registers in a pointer (`regs`). The stack is a `Vec` that a deep call chain reallocates (it starts
/// with 256 words), so the pointer must be set again after each growth. Each caller below has live registers before and after the call
/// that grows the stack, and reads `this` and the arguments from registers right after it.
#[test]
fn a_stack_that_grows_in_a_callee_keeps_the_registers_of_the_callers() {
    check(&[
        ("function sum(n) { var a = n, b = 2 * n; if (n == 0) return 0; var r = sum(n - 1); return r + a + b; } var r = sum(200);", "r", "60300"),
        ("function C(n) { this.n = n; this.k = n == 0 ? 0 : new C(n - 1).k + n; } var r = new C(200).k;", "r", "20100"),
        ("var o = { f: function (n, acc) { var t = this; if (n == 0) return acc; return t.f(n - 1, acc + n); } }; var r = o.f(200, 0);", "r", "20100"),
        ("function f(n, ...rest) { var x = n * 2; if (n == 0) return rest.length; var y = f(n - 1, n, n); return y + x - 2 * n; } var r = f(200);", "r", "2"),
    ]);
}

/// The ops of the cold function (`Vm::exec::<Cold>`) enter frames, grow the stack and unwind. The hot loop sets `regs`, `code`, `words`
/// and `fb` again when the cold function returns: each caller below has live registers that it reads after the cold op. The stack starts
/// with 256 words, so the recursion grows it inside `CallSuper` and `CallSpread`, and `ThrowConst` unwinds 150 frames.
#[test]
fn a_cold_op_that_grows_the_stack_or_unwinds_keeps_the_registers_of_the_caller() {
    check(&[
        ("class A { constructor(n) { this.k = n; } } class B extends A { constructor(n) { var a = n, b = 2 * n; super(n); this.d = n == 0 ? 0 : new B(n - 1).d + a + b; } } var r = new B(200).d;", "r", "60300"),
        ("function f(n) { var a = n; if (n == 0) return 0; var r = f(...[n - 1]); return r + a; } var r = f(300);", "r", "45150"),
        ("function f(n) { var a = n; if (n == 0) { const c = 1; c = 2; return 0; } if (n == 150) { try { return f(n - 1); } catch (e) { return -a; } } return f(n - 1) + a; } var r = f(300);", "r", "33675"),
    ]);
}

/// A `GetGlobal` or `SetGlobal` site holds the address of the slot of the global (`Shapes::watch_tag`). Each case changes the global
/// object between two runs of one site: an add that grows its overflow record many times, a delete that goes back to an older shape
/// (the record of that shape is not the one that the old address pointed into), an attribute change, an accessor, a new prototype.
/// Under Miri a stale address is a use after free.
#[test]
fn a_global_cache_follows_every_change_of_the_global_object() {
    check(&[
        // Adds: the loop reads and writes `x` at the same sites while 300 other globals are added (the overflow record regrows).
        ("var x = 1; var s = 0; for (var i = 0; i < 300; i++) { s += x; globalThis['g' + i] = i; x = x + 1; } var r = s + ',' + x + ',' + g299;", "r", "45150,301,299"),
        // Back to an older shape: add 40 globals, read `x`, delete them last first, read `x`, again.
        (
            "var x = 7; var s = 0; for (var round = 0; round < 3; round++) { for (var i = 0; i < 40; i++) { globalThis['t' + i] = i; s += x; } for (var i = 39; i >= 0; i--) { delete globalThis['t' + i]; s += x; x = x + 1; } } var r = s + ',' + x;",
            "r",
            "13620,127",
        ),
        // A global that is deleted (a global made by an assignment: a `var` is not configurable) is gone for a site that has an entry; the same name added again is found.
        ("globalThis.y = 5; var r = ''; function rd() { try { return y; } catch (e) { return e.name; } } for (var i = 0; i < 4; i++) { r += rd(); } delete globalThis.y; r += rd(); globalThis.y = 9; r += rd() + rd();", "r", "5555ReferenceError18"),
        // An attribute change (a dictionary shape) and a read-only global: a store from a site that has an entry is dropped.
        ("var z = 1; function st(v) { z = v; } function rz() { return z; } st(2); st(3); Object.defineProperty(globalThis, 'z', { writable: false }); st(4); var r = rz() + ',' + Object.getOwnPropertyDescriptor(globalThis, 'z').writable;", "r", "3,false"),
        // A setter on a global (a getter on a global is not supported by `GetGlobal` yet: it throws; the field read `globalThis.w` works).
        ("globalThis.w = 1; function gw() { return w; } function sw(v) { w = v; } gw(); gw(); sw(1); sw(1); var log = ''; Object.defineProperty(globalThis, 'w', { get: function () { return 42; }, set: function (v) { log += v; }, configurable: true }); sw(5); var r = globalThis.w + ',' + log;", "r", "42,5"),
        // A new prototype for the global object: a name that only the prototype has.
        ("var p = 1; function rp() { return typeof q === 'undefined' ? 'no' : q; } rp(); rp(); Object.setPrototypeOf(globalThis, { q: 'proto' }); var r = rp() + p;", "r", "proto1"),
    ]);
}

/// `AddX`, `SubX`, `MulX` and `ModX` hold a 32-bit constant in the word `x` (a constant that does not fit the 16-bit immediate): the
/// result is that of the register form for an int, a double that is not an int, a string, an object, `-0` and an overflow. A `Jmp` to a
/// `Ret` is that `Ret`: a `return` of a conditional, inside `try` with `finally`, in a `for-of`, in a `catch`.
#[test]
fn a_constant_of_32_bits_and_a_jump_to_a_return_give_the_same_values() {
    check(&[
        (r#"var out=[]; function k1(x){ return [x+100000, x-100000, x*100000, x%100003, 100000*x, 3000000-x].join(); } out.push(k1(5)+"|"+k1("7")+"|"+k1(2147483647)+"|"+k1(-4.5)+"|"+k1(-2147483648)+"|"+k1(null)+"|"+k1(true)); out.push(String((-100003)%100003)+","+1/(-100003 % 100003)+","+(function(a){return 1/(a%100003)})(-100003)+","+(function(a){return a+100000})("s")); out.push((function(a){return -(a*70000)})(0)+","+(function(a){return 1/(a*70000)})(-0)+","+(function(a){return 1/(a*70000)})(-0));  var r = out.join('|');"#, "r", "100005,-99995,500000,5,500000,2999995|7100000,-99993,700000,7,700000,2999993|2147583647,2147383647,214748364700000,19225,214748364700000,-2144483647|99995.5,-100004.5,-450000,-4.5,-450000,3000004.5|-2147383648,-2147583648,-214748364800000,-19226,-214748364800000,2150483648|100000,-100000,0,0,0,3000000|100001,-99999,100000,1,100000,2999999|0,-Infinity,-Infinity,s100000|0,-Infinity,-Infinity"),
        (r#"var out = []; function t1(c){ try { return c ? 1 : 2 } finally { out.push("f"+c) } } function t2(c){ var r; if (c) { r = 'a' } else { r = 'b' } return r } function t3(c){ for (var x of [1,2,3]) { if (c == x) return x*10 } return c ? 'n' : 'z' } function t4(c){ try { if (c) throw 1; return c ? 7 : 8 } catch (e) { return c ? 'c1' : 'c0' } } out.push([t1(true),t1(false),t2(1),t2(0),t3(2),t3(9),t3(0),t4(0),t4(1)].join()); var r = out.join('|'); "#, "r", "ftrue|ffalse|1,2,a,b,20,n,z,8,c1"),
    ]);
}

/// Every shape of callee through the call path of `Vm::run` (`enter!`, `ret!`): fewer and more arguments than parameters, `arguments`,
/// a rest parameter, a default value, a native tail call, `new` (with an object as the result), a getter and a setter, an arrow, a closure.
#[test]
fn every_shape_of_callee_gives_its_value() {
    check(&[(r#"function none() { return 1 } function two(a, b) { return a + ',' + b } function args() { return arguments.length + ':' + arguments[0] + arguments[2] } function rest(a, ...r) { return a + ':' + r.length + r.join('') } function def(a, b = a + 1) { return a + b } function nat(a, b) { return Math.max(a, b) } function Ctor(a) { this.a = a; this.n = arguments.length } function ctorRet(a) { this.a = a; return { z: 9 } } var o = { get g() { return this.v + 1 }, set g(x) { this.v = x * 2 }, v: 1, m(a) { return this.v + a } }; var arrow = (a, b) => a * b; var cl = (function () { var c = 0; return function (d) { c += d; return c } })(); var out = [none(9, 9, 9), two(1), two(1, 2, 3), args(7, 8, 9, 10), args(), rest(1), rest(1, 2, 3), def(1), def(1, 5), nat(3, 8), new Ctor(4).a + '/' + new Ctor(4, 5, 6).n, new ctorRet(1).z, (o.g = 5, o.g), o.m(3), arrow(6, 7), arrow(6), cl(1), cl(2), (function () { return this === undefined || this === globalThis })()]; var r = out.join('|');"#, "r", "1|1,undefined|1,2|4:79|0:undefinedundefined|1:0|1:223|3|6|8|4/3|9|11|13|42|NaN|1|3|true")]);
}

/// The cache of a global site and the other ways to reach a global name: strict writes (`SetGlobalS`), `delete` of a name
/// (`DeleteGlobal`), a `var` that cannot be deleted, a property that becomes read-only, a getter and a setter that grow the stack.
#[test]
fn a_global_cache_agrees_with_strict_writes_delete_and_attributes() {
    check(&[
        // A strict write to a name that was deleted is a ReferenceError at a site that had an entry; the name added again works.
        ("'use strict'; globalThis.s = 0; function ws(x) { s = x; } for (var i = 0; i < 4; i++) ws(i); delete globalThis.s; var r; try { ws(4); r = 'no'; } catch (e) { r = e.name; } globalThis.s = 9; ws(5); r += ',' + s;", "r", "ReferenceError,5"),
        // A read-only global: the store of a site that had an entry throws in strict code and is dropped in sloppy code.
        ("'use strict'; globalThis.n = 0; function wn(x) { n = x; } wn(1); wn(2); wn(3); Object.defineProperty(globalThis, 'n', { writable: false }); var r; try { wn(4); r = 'no'; } catch (e) { r = e.name; } r += ',' + n;", "r", "TypeError,3"),
        ("globalThis.n = 0; function wn(x) { n = x; } wn(1); wn(2); wn(3); Object.defineProperty(globalThis, 'n', { writable: false }); wn(4); var r = n;", "r", "3"),
        // `delete name` (DeleteGlobal) of a property that a site has an entry for: the next read is a ReferenceError.
        ("globalThis.d = 1; function rd() { try { return d; } catch (e) { return e.name; } } rd(); rd(); var r = rd() + ',' + delete d + ',' + rd();", "r", "1,true,ReferenceError"),
        // A `var` cannot be deleted: the entry stays right.
        ("var nv = 1; function rn() { return nv; } rn(); rn(); var r = rn() + ',' + delete nv + ',' + rn();", "r", "1,false,1"),
        // A getter and a setter of the global that run a deep recursion (the stack grows while the frame of `use` is live).
        ("var gv; function rec(n) { var a = n; return n == 0 ? 0 : rec(n - 1) + a; } Object.defineProperty(globalThis, 'gg', { get: function () { return rec(300); }, set: function (v) { gv = rec(300) + v; }, configurable: true }); function use() { var p = 1, q = 2; var t = gg; gg = 5; return p + q + t + gv; } var r = use();", "r", "90308"),
    ]);
}

/// A global lexical binding (a `let`, `const` or `class` of an earlier or later script) hides a property of the global object of the same
/// name. A site that has an entry for the property must not give it (`declare_lex` changes the tag of the global object).
#[test]
fn a_global_lexical_binding_hides_the_property_that_a_site_cached() {
    // The host makes the globals that the script writes (`var a, b, e, d`), so the script adds nothing to the global object after its
    // `let` and `const` are declared: only `declare_lex` can end the entries of `rd`, `wr` and `wc`.
    let host = "var a, b, e, d; globalThis.v = 1; function rd() { return v; } function wr(x) { v = x; } rd(); rd(); rd(); wr(1); wr(1); wr(1); globalThis.c = 1; function wc(x) { c = x; } wc(1); wc(1); wc(1);";
    // A function expression uses the names, so they are slots of a context and are declared before the first statement.
    let src = "let v = 2; const c = 3; (function () { return v + c; }); a = rd(); wr(5); b = v + ',' + globalThis.v; try { wc(4); } catch (x) { e = x.name; } d = c + ',' + globalThis.c;";
    let mut vm = load_with(host, src).unwrap();
    vm.run_main(0).unwrap();
    let got: Vec<String> = ["a", "b", "e", "d"].iter().map(|n| vm.global_text(n)).collect();
    assert_eq!(got, ["2", "5,1", "TypeError", "3,1"]);
}

/// A frozen global object: a site that had an entry for a property stores nothing after `Object.freeze(globalThis)`.
#[test]
fn a_frozen_global_object_stops_the_stores_of_the_sites() {
    let host = "globalThis.fz = 1; function wf(x) { fz = x; } function rf() { return fz; } wf(2); wf(3); wf(3); rf(); rf(); Object.freeze(globalThis);";
    let mut vm = load_with(host, "wf(9);").unwrap();
    vm.run_main(0).unwrap();
    assert_eq!(vm.global_text("fz"), "3");
    let mut vm = load_with(&format!("'use strict'; {host}"), "'use strict'; var t; try { wf(9); } catch (e) { t = e.name; }").unwrap();
    // The `var t` of the script is not possible on a frozen object, so the script cannot report: the throw is the exit.
    assert!(vm.run_main(0).is_err() || vm.global_text("fz") == "3");
}

/// The global cache with the built-ins of attributes: a global object that is not extensible, sealed or frozen, a data property that
/// becomes an accessor (and back), `defineProperty` with `writable: false`, and a strict store to each.
#[test]
fn a_global_cache_follows_the_attributes_that_the_object_built_ins_give() {
    let strict = |body: &str| format!("'use strict'; {body}");
    check(&[
        ("globalThis.pe = 1; function wp(x) { pe = x; } function nw(x) { newg = x; } wp(1); wp(2); wp(3); Object.preventExtensions(globalThis); wp(7); nw(1); var r = pe + ',' + typeof newg;", "r", "7,undefined"),
        (&strict("globalThis.pe = 1; function wp(x) { pe = x; } function nw(x) { newg = x; } wp(1); wp(2); wp(3); Object.preventExtensions(globalThis); wp(7); var r = pe; try { nw(1); } catch (e) { r += e.name; }"), "r", "7ReferenceError"),
        ("globalThis.sl = 1; function ws(x) { sl = x; } function rs() { return sl; } ws(2); ws(3); rs(); rs(); Object.seal(globalThis); ws(4); var r = rs() + ',' + delete globalThis.sl + ',' + rs();", "r", "4,false,4"),
        // A data property becomes an accessor and the sites that had an entry call the accessor.
        ("globalThis.ac = 1; var log = ''; function ra() { return ac; } function wa(x) { ac = x; } ra(); ra(); wa(1); wa(1); Object.defineProperty(globalThis, 'ac', { get: function () { log += 'g'; return 42; }, set: function (v) { log += 's' + v; }, configurable: true }); var r = ra() + ',' + (wa(5), log);", "r", "42,gs5"),
        // ... and back to a data property.
        ("globalThis.ab = 1; var log = ''; function ra() { return ab; } ra(); ra(); Object.defineProperty(globalThis, 'ab', { get: function () { return 'acc'; }, configurable: true }); var r = ra(); Object.defineProperty(globalThis, 'ab', { value: 'data', writable: true, configurable: true }); r += ra();", "r", "accdata"),
        (&strict("globalThis.ro = 1; function wr(x) { ro = x; } wr(2); wr(3); Object.defineProperty(globalThis, 'ro', { writable: false }); var r; try { wr(4); r = 'no'; } catch (e) { r = e.name; } r += ro;"), "r", "TypeError3"),
        // A getter without a setter: a strict store throws, a sloppy store does nothing, a read calls the getter.
        (&strict("globalThis.go = 1; function wg(x) { go = x; } function rg() { return go; } rg(); rg(); wg(1); wg(1); Object.defineProperty(globalThis, 'go', { get: function () { return 9; }, configurable: true }); var r = rg(); try { wg(3); } catch (e) { r += e.name; }"), "r", "9TypeError"),
    ]);
    let host = "globalThis.fz = 1; function wf(x) { fz = x; } function rf() { return fz; } wf(2); wf(3); wf(3); rf(); rf(); Object.freeze(globalThis);";
    let mut vm = load_with(host, "wf(9);").unwrap();
    vm.run_main(0).unwrap();
    assert_eq!(vm.global_text("fz"), "3");
    let mut vm = load_with(&format!("'use strict'; {host}"), "'use strict'; wf(9);").unwrap();
    assert!(vm.run_main(0).is_err(), "a strict store to a frozen global throws");
    assert_eq!(vm.global_text("fz"), "3");
}

// ---- Lazy compile of the functions of a user script (`Lazy`, `Op::Lazy`): a function is compiled at its first call.

#[test]
fn the_functions_of_a_user_script_are_compiled_at_their_first_call() {
    let src = "function a() { return 1; } function b() { return function inner() { return 2; }; } function c() { return 3; } var r = a() + b()();";
    let mut vm = load(src).unwrap();
    let before = waiting(&vm);
    vm.run_main(0).unwrap();
    // a and b are compiled at their first call, and `inner` is made as a stub in b and compiled at its call; c waits.
    assert_eq!(vm.global_text("r"), "3");
    assert_eq!(waiting(&vm), before - 2, "a and b were compiled; c waits; inner was added and compiled");
    // A second call compiles nothing more.
    let mut vm2 = load("function a() { return 1; } a(); a(); a();").unwrap();
    vm2.run_main(0).unwrap();
    let mut vm3 = load("function a() { return 1; } a();").unwrap();
    vm3.run_main(0).unwrap();
    assert_eq!(waiting(&vm2), waiting(&vm3));
}

#[test]
fn a_function_that_is_never_called_is_never_compiled() {
    // A construct that the compiler does not build (`with`) inside a function: the load works and the call fails.
    let src = "function bad() { with ({}) { return 1; } } function good() { return 2; } var r = good();";
    let vm = run(src).unwrap();
    let mut vm = vm;
    assert_eq!(vm.global_text("r"), "2");
}

#[test]
fn an_unbuilt_construct_in_a_function_fails_at_its_first_call_not_at_load() {
    let src = "function bad() { with ({}) { return 1; } } var e; try { bad(); } catch (x) { e = x.name + ': ' + x.message.slice(0, 17); } try { bad(); } catch (x) { e += '|' + x.name; }";
    assert_eq!(ev(src, "e"), "Error: not supported yet|Error");
    // The top level has the construct: the load fails, as before.
    assert!(load("with ({}) {}").err().unwrap().starts_with("not supported yet"));
}

#[test]
fn early_errors_in_a_function_that_is_never_called_still_fail_the_load() {
    for src in [
        "function f() { let a; let a; }",
        "function f() { return 1 + ; }",
        "function f() { 'use strict'; var yield; }",
        "var o = { m() { break; } };",
        "function f() { function g() { new.target = 1; } }",
        "class C { m() { return this.#x; } }",
    ] {
        let e = load(src).err().unwrap_or_else(|| panic!("{src} loaded"));
        assert!(e.starts_with("SyntaxError"), "{src}: {e}");
    }
}

#[test]
fn a_first_call_from_a_getter_a_setter_a_constructor_and_a_native_works() {
    check(&[
        // Getter and setter, in an object literal and in a class, each first called by a property access.
        ("var o = { k: 1, get x() { return this.k + 1; }, set x(v) { this.k = v * 2; } }; o.x = 5; var r = o.x;", "r", "11"),
        ("class A { k = 1; get x() { return this.k + 1; } set x(v) { this.k = v * 2; } static get s() { return 's'; } } var a = new A(); a.x = 3; var r = a.x + A.s;", "r", "7s"),
        // A constructor with fields (the initializers are inline children of the constructor), a base class and a derived class.
        ("class C { a = 1; b = this.a + 1; constructor(x) { this.c = x + 10; } } var o = new C(5); var r = o.a + ',' + o.b + ',' + o.c;", "r", "1,2,15"),
        ("class C { a = 1; constructor() { this.c = 2; } } class D extends C { d = 3; constructor() { super(); this.e = 4; } } var o = new D(); var r = [o.a, o.c, o.d, o.e].join();", "r", "1,2,3,4"),
        ("class C { static n = 1; static { C.m = C.n + 1; } static f() { return C.m; } } var r = C.f();", "r", "2"),
        // A native that asks the interpreter to call a function (`Outcome::Enter`): call, apply, bind, and a callback of the prelude.
        ("function f(a, b) { return this.k + a + b; } var r = f.call({ k: 1 }, 2, 3);", "r", "6"),
        ("function f(a, b) { return this.k + a + b; } var r = f.apply({ k: 1 }, [2, 3]);", "r", "6"),
        ("function f(a, b) { return this.k + a + b; } var g = f.bind({ k: 1 }, 2); var r = g(3);", "r", "6"),
        ("function f(x) { return x * 2; } var r = [1, 2, 3].map(f).join();", "r", "2,4,6"),
        // A function inside a function that waits, an arrow that reads `arguments` and `this`, and a rest parameter.
        ("function f() { var g = () => arguments.length + this.k; return g(); } var r = f.call({ k: 1 }, 7, 8);", "r", "3"),
        ("function f(a, ...r) { return a + r.length; } var r = f(10, 1, 2, 3);", "r", "13"),
        // A closure over a variable that the first compile must find in the chain.
        ("function mk() { var n = 0; return function () { return ++n; }; } var c = mk(); c(); c(); var r = c();", "r", "3"),
        // A method with `super` is compiled at load (its code may need `CF_LINK`), and a lazy function around it works.
        ("class A { v() { return 1; } } function wrap() { class B extends A { v() { return super.v() + 1; } } return new B().v(); } var r = wrap();", "r", "2"),
    ]);
}

#[test]
fn a_user_function_first_called_in_a_nested_run_is_compiled_there() {
    // The user functions below wait as stubs (#777). Their first call is inside a getter, a setter, `valueOf` or `toString` that a native
    // runs, so the stub compiles in a nested run. Each case has a stub that keeps its parameter registers (plain parameters) and one that
    // makes the array of arguments (`arguments`, a default value, a rest parameter). `check` also runs each case under constant collection.
    check(&[
        ("function p(a, b) { return a * b; } var o = {get g() { return p(6, 7); }}; var r = Object.assign({}, o).g;", "r", "42"),
        ("function q() { return arguments.length; } var o = {get g() { return q(1, 2, 3); }}; var r = Object.assign({}, o).g;", "r", "3"),
        ("var s; function p(a, b) { return a + b; } var o = {set v(x) { s = p(x, 1); }}; Object.assign(o, {v: 41}); var r = s;", "r", "42"),
        ("var s; function d(a, b = 2) { return a + b; } var o = {set v(x) { s = d(x); }}; Object.assign(o, {v: 40}); var r = s;", "r", "42"),
        ("function p(a) { return a + 1; } var o = {valueOf: function () { return p(41); }}; var r = o + 0;", "r", "42"),
        ("function d(a, ...m) { return a + m.length; } var o = {toString: function () { return 'v' + d(40, 1, 2); }}; var r = `${o}` + ('' + o);", "r", "v42v42"),
        // Two levels: `valueOf` -> a getter -> `valueOf` again. Each level first calls a stub that waits, and the loops above call it after.
        ("function a1(x) { return x + 1; } function a2() { return arguments[0] * 2; } function a3(x, y) { return x - y; }
          var inner = {valueOf: function () { return a3(a2(10), a1(4)); }};
          var mid = {get g() { return a1(inner) + a2(1); }};
          var outer = {valueOf: function () { return Object.assign({}, mid).g + a3(9, 1); }};
          var r = (outer + 0) + ',' + a1(1) + ',' + a2(2) + ',' + a3(5, 3) + ',' + (inner + 0);",
         "r", "26,2,4,2,15"),
        // A stub that compiles in a nested run and has a function inside it: the child is a new stub there, and an outer run calls it.
        ("function mk(n) { return function (x) { return x + n; }; } var o = {valueOf: function () { return mk(1)(2); }}; var r = (o + 0) + ',' + mk(10)(5);", "r", "3,15"),
    ]);
}

#[test]
fn a_function_that_is_called_from_rust_is_compiled_at_that_call() {
    let mut vm = load("function add(a, b) { return a + b; } function get() { return 'g'; }").unwrap();
    vm.run_main(0).unwrap();
    let f = vm.global("add");
    let v = vm.call(f, Value::UNDEFINED, &[Value::int(2), Value::int(3)]).unwrap();
    assert_eq!(vm.display(v, false), "5");
    // A function with fewer arguments than parameters.
    let v = vm.call(f, Value::UNDEFINED, &[Value::int(2)]).unwrap();
    assert_eq!(vm.display(v, false), "NaN");
    let g = vm.global("get");
    let v = vm.call(g, Value::UNDEFINED, &[]).unwrap();
    assert_eq!(vm.display(v, false), "g");
}

#[test]
fn the_stubs_of_user_functions_keep_a_program_that_is_not_called_small() {
    let mut src = String::new();
    for i in 0..200 {
        src.push_str(&format!("function f{i}(a) {{ var s = 0; for (var i = 0; i < a; i++) s += i * {i}; return s; }}\n"));
    }
    src.push_str("var r = f7(4);");
    let mut vm = load(&src).unwrap();
    assert!(waiting(&vm) >= 200 + 40, "the 200 functions and the prelude wait: {}", waiting(&vm));
    vm.run_main(0).unwrap();
    assert_eq!(vm.global_text("r"), "42");
    let words: usize = vm.realm.prog.iter().map(|c| c.words.len()).sum();
    assert!(words < 4000, "only the script, the wrapper of the prelude and f7 hold code: {words} words");
}

#[test]
fn a_stub_that_keeps_the_arguments_in_registers_and_a_stub_with_an_array_agree() {
    // Plain parameters and no `arguments`: the stub has the parameter registers (no array). Anything else: the array. Each first call
    // must give the same result for fewer, equal and more arguments.
    check(&[
        ("function f(a, b) { return a + ',' + b; } var r = f(1) + '|' + f(1, 2, 3);", "r", "1,undefined|1,2"),
        ("function f(a, b) { return a + ',' + b; } var r = f(1, 2, 3);", "r", "1,2"),
        ("function f(a, b) { return a + ',' + b; } var r = f();", "r", "undefined,undefined"),
        ("function f(a, a2, a3, a4, a5, a6, a7, a8, a9) { return a9 + a; } var r = f(1, 2, 3, 4, 5, 6, 7, 8, 9);", "r", "10"),
        ("function f(a, a) { return a; } var r = f(1, 2);", "r", "2"),
        ("function f(a) { return arguments.length + a; } var r = f(1, 2, 3);", "r", "4"),
        ("function f(a, b = 5) { return a + b; } var r = f(1) + f(1, 2);", "r", "9"),
        ("function f(a, ...r) { return a + r.length; } var r = f(1, 2, 3);", "r", "3"),
        ("function f({ x }, [y]) { return x + y; } var r = f({ x: 1 }, [2]);", "r", "3"),
        ("function f(a) { var g = () => arguments[1]; return g(); } var r = f(1, 7);", "r", "7"),
        ("function f(a) { 'use strict'; var arguments2 = a; return arguments2; } var r = f(4);", "r", "4"),
        ("function f(a) { return function (b) { return a + b; }; } var r = f(1)(2) + f(10)(20);", "r", "33"),
        ("var o = { m(a, b) { return this.k + a + b; }, k: 1 }; var r = o.m(2, 3) + '|' + o.m(2);", "r", "6|NaN"),
    ]);
}

#[test]
fn new_on_an_arrow_function_a_method_or_an_accessor_is_a_type_error_before_the_first_call() {
    // The stub of a function that was never called must already say that it is no constructor.
    check(&[
        ("var r; try { new (() => 1)(); r = 'no'; } catch (e) { r = e instanceof TypeError; }", "r", "true"),
        ("var r; try { new ({ m() {} }).m(); r = 'no'; } catch (e) { r = e instanceof TypeError; }", "r", "true"),
        ("var r; try { new (Object.getOwnPropertyDescriptor({ get a() { return 1; } }, 'a').get)(); r = 'no'; } catch (e) { r = e instanceof TypeError; }", "r", "true"),
        ("var r; try { new (Object.getOwnPropertyDescriptor({ set a(v) {} }, 'a').set)(); r = 'no'; } catch (e) { r = e instanceof TypeError; }", "r", "true"),
        ("var r; try { new (class { static m() {} }).m(); r = 'no'; } catch (e) { r = e instanceof TypeError; }", "r", "true"),
        ("var r; try { Reflect.construct(function () {}, [], () => 1); r = 'no'; } catch (e) { r = e instanceof TypeError; }", "r", "true"),
        ("var f = function () { this.a = 1; }; var r = new f().a;", "r", "1"),
    ]);
}

#[test]
fn the_generator_and_async_function_constructors_build_functions_of_their_kind() {
    check(&[
        // The constructors are reachable from a function of the kind, and have the spec properties.
        (
            "var G = Object.getPrototypeOf(function* () {}).constructor, A = Object.getPrototypeOf(async function () {}).constructor, \
             AG = Object.getPrototypeOf(async function* () {}).constructor; \
             var r = [G.name, A.name, AG.name, G.length, Object.getPrototypeOf(G) === Function, G.prototype === Object.getPrototypeOf(function* () {}), \
             Object.getOwnPropertyDescriptor(G.prototype, 'constructor').writable, Object.getOwnPropertyDescriptor(G, 'prototype').writable].join();",
            "r",
            "GeneratorFunction,AsyncFunction,AsyncGeneratorFunction,1,true,true,false,false",
        ),
        (
            "var G = Object.getPrototypeOf(function* () {}).constructor; var g = new G('a', 'b', 'yield a; yield b; return 3;'); \
             var it = g(1, 2); var r = [g.name, g.length, Object.getPrototypeOf(g) === G.prototype, it.next().value, it.next().value, it.next().value].join();",
            "r",
            "anonymous,2,true,1,2,3",
        ),
        // A call without `new` is the same, and a bad text is a SyntaxError that does not reach the global scope.
        (
            "var G = Object.getPrototypeOf(function* () {}).constructor; var r = []; \
             try { G('yield', ''); } catch (e) { r.push(e instanceof SyntaxError); } \
             try { G('}) ; ({'); } catch (e) { r.push(e instanceof SyntaxError); } \
             var A = Object.getPrototypeOf(async function () {}).constructor; \
             try { A('await', ''); } catch (e) { r.push(e instanceof SyntaxError); } \
             try { A('', 'super()'); } catch (e) { r.push(e instanceof SyntaxError); } \
             r = r.join();",
            "r",
            "true,true,true,true",
        ),
        // The function has the global scope, not the scope of the caller.
        (
            "var x = 'global'; var r; (function () { var x = 'local'; var G = Object.getPrototypeOf(function* () {}).constructor; r = G('yield x')().next().value; })();",
            "r",
            "global",
        ),
    ]);
}
