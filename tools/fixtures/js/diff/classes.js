class A { constructor(x) { this.x = x; } get dbl() { return this.x * 2; } static make() { return new A(1); } m() { return 'A.m'; } }
class B extends A { constructor() { super(5); this.y = 1; } m() { return 'B>' + super.m(); } }
var b = new B();
print(b.x, b.y, b.dbl, b.m(), A.make().x, b instanceof A, Object.getPrototypeOf(B) === A);
class C { #p = 1; static s = 'st'; f = 7; static #q() { return 3; } inc() { return ++this.#p; } static sq() { return C.#q(); } }
var c = new C(); print(c.inc(), c.inc(), C.s, c.f, C.sq());
class E extends Error { constructor(m) { super(m); this.name = 'E'; } }
try { throw new E('boom'); } catch (e) { print(e instanceof E, e instanceof Error, e.message, e.name); }
print(typeof A, (function () { try { A(); } catch (e) { return e.name; } })());
class D { static { D.init = 'ok'; } } print(D.init);
