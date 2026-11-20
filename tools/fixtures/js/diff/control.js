var out = [];
outer: for (var i = 0; i < 4; i++) { for (var j = 0; j < 4; j++) { if (j == 2) continue outer; if (i == 3) break outer; out.push(i + '' + j); } }
print(out.join());
function sw(v) { switch (v) { case 1: return 'one'; case 'a': return 'A'; case 2: case 3: return 'two-three'; default: return 'dflt'; } }
print(sw(1), sw('a'), sw(3), sw(9), sw('1'));
var k = 0; do { k += 2; } while (k < 7); print(k);
var keys = []; for (var p in { a: 1, b: 2, c: 3 }) keys.push(p); print(keys.join());
var vals = []; for (var v of [1, 2, 3]) vals.push(v * 2); print(vals.join());
function t() { try { return 'try'; } finally { print('finally'); } } print(t());
function u() { try { throw 1; } catch (e) { return 'c' + e; } finally { print('f2'); } } print(u());
var r = (function () { for (var i = 0; ; i++) { if (i > 5) return i; } })(); print(r);
print(1 ? 2 : 3, 0 || 'x', 0 ?? 'y', null ?? 'z', 1 && 2, (1, 2, 3));
var n = 0; var obj = { get a() { n++; return 1; } }; obj?.a; obj.b?.c; print(n);
