// The built-ins that call a script function (plan step S2.7). `script.rs` compiles this with each script and runs it once, before the
// script. Each function is a plain script function, so a callback is an ordinary `Call` op: no re-entry from Rust, exceptions
// propagate, and the root maps keep every live value. `__def(target, methods)` copies the methods as properties that are not
// enumerable, and the loader removes `__def` afterwards.
(function (def) {
  var nativeReplace = String.prototype.replace;
  var nativeReplaceAll = String.prototype.replaceAll;
  function need(cb) {
    if (typeof cb !== 'function') throw new TypeError('the callback is not a function');
  }
  // A callback with `thisArg` bound; with no `thisArg` the callback itself, so the common call is a direct call.
  function bound(cb, t) {
    return t === undefined ? cb : cb.bind(t);
  }
  function defaultCompare(x, y) {
    x = '' + x;
    y = '' + y;
    return x < y ? -1 : x > y ? 1 : 0;
  }
  // A stable merge sort of the first `n` elements of `a`. Returns the array that holds the result.
  function mergeSort(a, n, cmp) {
    var src = a, dst = new Array(n);
    for (var w = 1; w < n; w *= 2) {
      for (var lo = 0; lo < n; lo += 2 * w) {
        var mid = lo + w < n ? lo + w : n, hi = lo + 2 * w < n ? lo + 2 * w : n;
        var i = lo, j = mid, k = lo;
        while (i < mid && j < hi) {
          if (cmp(src[j], src[i]) < 0) dst[k++] = src[j++];
          else dst[k++] = src[i++];
        }
        while (i < mid) dst[k++] = src[i++];
        while (j < hi) dst[k++] = src[j++];
      }
      var t = src; src = dst; dst = t;
    }
    return src;
  }
  function replaceWith(native, s, p, r) {
    if (typeof r !== 'function') return native.call(s, p, r);
    s = '' + s;
    if (typeof p !== 'string') throw new TypeError('a RegExp pattern is not supported');
    var out = '', at = 0, i;
    while ((i = s.indexOf(p, at)) >= 0) {
      out += s.slice(at, i) + r(p, i, s);
      at = i + p.length;
      if (native === nativeReplace) break;
      if (p.length === 0) {
        if (at >= s.length) break;
        out += s[at++];
      }
    }
    return out + s.slice(at);
  }
  def(Array.prototype, {
    forEach: function forEach(cb, t) {
      need(cb);
      var a = this, n = a.length, f = bound(cb, t);
      for (var i = 0; i < n; i++) {
        var v = a[i];
        if (v !== undefined || i in a) f(v, i, a);
      }
    },
    map: function map(cb, t) {
      need(cb);
      var a = this, n = a.length, f = bound(cb, t), r = [];
      for (var i = 0; i < n; i++) {
        var v = a[i];
        if (v !== undefined || i in a) r[i] = f(v, i, a);
      }
      r.length = n;
      return r;
    },
    filter: function filter(cb, t) {
      need(cb);
      var a = this, n = a.length, f = bound(cb, t), r = [], k = 0;
      for (var i = 0; i < n; i++) {
        var v = a[i];
        if ((v !== undefined || i in a) && f(v, i, a)) r[k++] = v;
      }
      return r;
    },
    reduce: function reduce(cb, init) {
      need(cb);
      var a = this, n = a.length, i = 0, acc;
      if (arguments.length >= 2) acc = init;
      else {
        while (i < n && !(i in a)) i++;
        if (i >= n) throw new TypeError('reduce of an empty array with no initial value');
        acc = a[i++];
      }
      for (; i < n; i++) {
        var v = a[i];
        if (v !== undefined || i in a) acc = cb(acc, v, i, a);
      }
      return acc;
    },
    reduceRight: function reduceRight(cb, init) {
      need(cb);
      var a = this, i = a.length - 1, acc;
      if (arguments.length >= 2) acc = init;
      else {
        while (i >= 0 && !(i in a)) i--;
        if (i < 0) throw new TypeError('reduce of an empty array with no initial value');
        acc = a[i--];
      }
      for (; i >= 0; i--) {
        var v = a[i];
        if (v !== undefined || i in a) acc = cb(acc, v, i, a);
      }
      return acc;
    },
    some: function some(cb, t) {
      need(cb);
      var a = this, n = a.length, f = bound(cb, t);
      for (var i = 0; i < n; i++) {
        var v = a[i];
        if ((v !== undefined || i in a) && f(v, i, a)) return true;
      }
      return false;
    },
    every: function every(cb, t) {
      need(cb);
      var a = this, n = a.length, f = bound(cb, t);
      for (var i = 0; i < n; i++) {
        var v = a[i];
        if ((v !== undefined || i in a) && !f(v, i, a)) return false;
      }
      return true;
    },
    find: function find(cb, t) {
      need(cb);
      var a = this, n = a.length, f = bound(cb, t);
      for (var i = 0; i < n; i++) {
        var v = a[i];
        if (f(v, i, a)) return v;
      }
      return undefined;
    },
    findIndex: function findIndex(cb, t) {
      need(cb);
      var a = this, n = a.length, f = bound(cb, t);
      for (var i = 0; i < n; i++) {
        if (f(a[i], i, a)) return i;
      }
      return -1;
    },
    sort: function sort(cmp) {
      if (cmp !== undefined && typeof cmp !== 'function') throw new TypeError('the comparison function must be a function or undefined');
      if (cmp === undefined) cmp = defaultCompare;
      var a = this, n = a.length, vals = [], k = 0, undefs = 0;
      for (var i = 0; i < n; i++) {
        var v = a[i];
        if (v === undefined) undefs++;
        else vals[k++] = v;
      }
      var sorted = mergeSort(vals, k, cmp);
      for (i = 0; i < k; i++) a[i] = sorted[i];
      for (; i < k + undefs; i++) a[i] = undefined;
      return a;
    }
  });
  def(Array, {
    from: function from(src, fn, t) {
      if (src === undefined || src === null) throw new TypeError('the value is not iterable');
      if (fn !== undefined) need(fn);
      var n = src.length >>> 0, r = [], f = fn === undefined ? undefined : bound(fn, t);
      for (var i = 0; i < n; i++) r[i] = f === undefined ? src[i] : f(src[i], i);
      r.length = n;
      return r;
    }
  });
  def(String.prototype, {
    replace: function replace(p, r) { return replaceWith(nativeReplace, this, p, r); },
    replaceAll: function replaceAll(p, r) { return replaceWith(nativeReplaceAll, this, p, r); }
  });
})(__def);
