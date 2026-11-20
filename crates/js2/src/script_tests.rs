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
fn check(cases: &[(&str, &str, &str)]) {
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
        ("try { /a+/g; } catch (e) { var r = e.name; }", "r", "TypeError"),
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
    for src in ["function* g() {}", "async function f() {}", "with ({}) {}"] {
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
        ("var r = typeof __def + typeof Symbol + typeof undefined;", "r", "undefinedundefinedundefined"),
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
