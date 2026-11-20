function add(a, b) { return a + b; }
print(add(1, 2), add(1), typeof add);
function mk() { var n = 0; return { inc: function () { return ++n; }, get: function () { return n; } }; }
var c = mk(); c.inc(); c.inc(); print(c.get());
print((function () { return arguments.length; })(1, 2, 3), (function (a) { arguments[0] = 9; return a; })(1));
print((function () { 'use strict'; return this === undefined; })());
print(add.call(null, 1, 2), add.apply(null, [3, 4]), add.bind(null, 10)(5));
var fact = function f(n) { return n < 2 ? 1 : n * f(n - 1); }; print(fact(10));
print(((a, b = 2, ...r) => a + b + r.length)(1), ((a, b = 2, ...r) => a + b + r.length)(1, 5, 7, 8));
var x = 'outer'; function sh() { var x = 'inner'; return x; } print(sh(), x);
print(typeof hoisted, hoisted2()); var hoisted = 1; function hoisted2() { return 'h2'; }
var fs = []; for (var i = 0; i < 3; i++) fs.push(function () { return i; }); print(fs[0](), fs[2]());
var gs = []; for (let j = 0; j < 3; j++) gs.push(function () { return j; }); print(gs[0](), gs[2]());
print(new (function P(v) { this.v = v; })(3).v, (function () { return new.target === undefined; })());
