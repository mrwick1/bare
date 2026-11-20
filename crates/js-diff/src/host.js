// The host script of both engines: it runs before the test, as its own script, so a test keeps its directive prologue. Everything it
// makes is a non-enumerable global. The one name `__engine` is a plain global that `lib.rs` puts after it ('old' or 'new').
(function (g) {
  // The tests change the built-ins (`Array.prototype.join` has a test that deletes it), so the host keeps its own.
  var join = Array.prototype.join, push = Array.prototype.push;
  function hide(name, value) { Object.defineProperty(g, name, { value: value, writable: true, configurable: true }); }
  hide('__log', []);
  hide('print', function print() { push.call(g.__log, join.call(arguments, ' ')); });
  hide('$262', { global: g, gc: function () {}, agent: {} });
  // A value as text. The same code runs on both engines, so a difference in the text is a difference in the engines.
  function show(v, d) {
    var t = typeof v;
    if (t === 'number') return v === 0 && 1 / v < 0 ? '-0' : String(v);
    if (t === 'string') return '"' + v + '"';
    if (v === null) return 'null';
    if (t === 'function') return 'function';
    if (t !== 'object') return String(v);
    if (d > 4) return '...';
    var out = [], i;
    if (v instanceof Error) return 'Error(' + v.name + ': ' + v.message + ')';
    if (Array.isArray(v)) {
      for (i = 0; i < v.length; i++) push.call(out, i in v ? show(v[i], d + 1) : '<hole>');
      return '[' + join.call(out, ',') + ']';
    }
    var keys = Object.keys(v);
    for (i = 0; i < keys.length; i++) push.call(out, keys[i] + ':' + show(v[keys[i]], d + 1));
    return '{' + join.call(out, ',') + '}';
  }
  // What `print` collected, then the value of the global that `__name` names (the differential runner sets `__name`).
  hide('__finish', function () {
    var out = join.call(g.__log, '\n');
    if (typeof g.__name === 'string') out += (out ? '\n' : '') + '=> ' + show(g[g.__name], 0);
    return out;
  });
  // A thrown value as `constructor name`, the character 1, and the message. The name comes from `instanceof`, because `Function.name`
  // is not built in the new engine yet.
  hide('__thrown', function (e) {
    var n = 'unknown', m = '', k = ['TypeError', 'RangeError', 'ReferenceError', 'SyntaxError', 'EvalError', 'URIError', 'Test262Error', 'Error'];
    try {
      for (var i = 0; i < k.length; i++) {
        if (typeof g[k[i]] === 'function' && e instanceof g[k[i]]) { n = k[i]; break; }
      }
      if (n === 'unknown') n = e.constructor.name;
    } catch (_) {}
    try { m = (typeof e === 'object' && e !== null) ? String(e.message) : String(e); } catch (_) {}
    return n + '\u0001' + m;
  });
})(globalThis);
