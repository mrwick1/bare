// The synthetic React-like trace of `crates/js2/examples/shape_census.rs` (plan step S1.8), as a script for the old engine.
// Prefix it with `var SCALE = N;` (N = 1 is the census; N = 0 makes the empty baseline). The two programs make the same objects with the
// same keys in the same order: xorshift32 from one seed, called in the same order.
var S = SCALE;
var s = 2463534242;
function rnd() { s ^= s << 13; s >>>= 0; s ^= s >>> 17; s ^= s << 5; s >>>= 0; return s; }
var P = ["className", "id", "onClick", "onChange", "style", "children", "href", "value", "disabled", "title", "role", "tabIndex", "name", "type", "placeholder", "ref"];
var T = [];
for (var t = 0; t < 40; t++) {
  var size = 1 + rnd() % 8;
  var idx = [];
  for (var i = 0; i < 16; i++) idx.push(i);
  for (i = 0; i < size; i++) { var j = i + rnd() % (16 - i); var x = idx[i]; idx[i] = idx[j]; idx[j] = x; }
  T.push(idx.slice(0, size));
}
function Fiber(n) { for (var k = 0; k < 24; k++) this["f" + k] = n; }
var keep = [];
var p, q;
function at(m) { return m === p ? q : (m === q ? p : m); }
for (var n = 0; n < 8000 * S; n++) {
  var a = rnd() % 40, b = rnd() % 40;
  var tpl = T[a < b ? a : b];
  var len = tpl.length;
  p = -1; q = -1;
  if (rnd() % 8 === 0) { p = rnd() % len; q = rnd() % len; }
  var props = {};
  for (var m = 0; m < len; m++) props[P[tpl[at(m)]]] = n;
  if (rnd() % 16 === 0 && len >= 2) delete props[P[tpl[at(0)]]];
  keep.push(props);
  keep.push({ $$typeof: 1, type: n, key: n, ref: n, props: props, _owner: n });
  if (n % 2 === 0) keep.push({ memoizedState: n, baseState: n, baseQueue: n, queue: n, next: n });
  if (n % 4 === 0) {
    var f = new Fiber(n);
    if (n % 80 === 0) delete f.f3;
    keep.push(f);
  }
}
keep.length;
