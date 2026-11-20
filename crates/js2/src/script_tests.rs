// End-to-end tests (S2.6): real JavaScript goes through the lexer, the parser, the compiler and the interpreter, and the test reads a
// global that the script set. There are no built-ins yet, so the scripts use plain syntax (no `Math`, `push` or `Error`).
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
        ("function f() { return this; } var r = typeof f.call;", "r", "undefined"),
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
