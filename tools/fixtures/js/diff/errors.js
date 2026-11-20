function kind(f) { try { f(); return 'none'; } catch (e) { return e.name; } }
print(kind(function () { null.x; }), kind(function () { undefinedVar; }), kind(function () { (1)(); }));
print(kind(function () { throw new RangeError('r'); }), kind(function () { throw 'str'; }), kind(function () { [].length = -1; }));
var e = new TypeError('m'); print(e.name, e.message, e instanceof TypeError, e instanceof Error, String(e), Object.prototype.toString.call(e));
print(kind(function () { x = 1; var x; }));
try { try { throw new Error('a'); } finally { print('cleanup'); } } catch (e) { print(e.message); }
print(kind(function () { var o = Object.freeze({}); 'use strict'; o.x = 1; }), kind(function () { Object.defineProperty(1, 'x', {}); }));
print(kind(function () { new Array(-1); }), kind(function () { 'abc'.repeat(-1); }), kind(function () { (1).toFixed(101); }));
print(Error('x') instanceof Error, typeof Error.prototype.toString);
