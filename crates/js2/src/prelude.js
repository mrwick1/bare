// The built-ins that call a script function (plan step S2.7). `script.rs` compiles this with each script and runs it once, before the
// script. Each function is a plain script function, so a callback is an ordinary `Call` op: no re-entry from Rust, exceptions
// propagate, and the root maps keep every live value. `__def(target, methods)` copies the methods as properties that are not
// enumerable, and the loader sets `__def`, `__helper` and `__set` to `undefined` afterwards (it does not delete it: see `script::load`).
(function (def, helper, set) {
  'use strict';
  var nativeReplace = String.prototype.replace;
  var nativeReplaceAll = String.prototype.replaceAll;
  var rawDefine = Object.defineProperty, keys = Object.keys, getProto = Object.getPrototypeOf, setProto = Object.setPrototypeOf;
  // The descriptor object `d` as a plain object, read with ordinary property reads (a field of `d` may be an accessor).
  function toDesc(d) {
    if (typeof d !== 'object' && typeof d !== 'function' || d === null) throw new TypeError('property description must be an object');
    var n = {};
    if ('enumerable' in d) n.enumerable = d.enumerable;
    if ('configurable' in d) n.configurable = d.configurable;
    if ('value' in d) n.value = d.value;
    if ('writable' in d) n.writable = d.writable;
    if ('get' in d) n.get = d.get;
    if ('set' in d) n.set = d.set;
    return n;
  }
  // The helpers that natives hand work to when they meet an accessor (`bi_object.rs`).
  helper(0, function defineProperty(o, k, d) { return rawDefine(o, k, toDesc(d)); });
  helper(1, function defineProperties(o, props) {
    var ks = keys(props), ds = [], i;
    for (i = 0; i < ks.length; i++) ds[i] = toDesc(props[ks[i]]);
    for (i = 0; i < ks.length; i++) rawDefine(o, ks[i], ds[i]);
    return o;
  });
  helper(2, function assign(t) {
    if (t === undefined || t === null) throw new TypeError('cannot convert undefined or null to an object');
    t = Object(t);
    for (var i = 1; i < arguments.length; i++) {
      var s = arguments[i];
      if (s === undefined || s === null) continue;
      var ks = keys(s), ss = Object.getOwnPropertySymbols(Object(s)), j;
      for (j = 0; j < ks.length; j++) set(t, ks[j], s[ks[j]]);
      for (j = 0; j < ss.length; j++) if (getOwn(s, ss[j]).enumerable) set(t, ss[j], s[ss[j]]);
    }
    return t;
  });
  // `indexOf` (0), `lastIndexOf` (1) and `includes` (2) on an object that has accessors or is no array: the natives hand it over.
  helper(3, function search(kind, o, x, from, hasFrom) {
    var a = obj(o), n = len(a), i;
    if (n === 0) return kind === 2 ? false : -1;
    if (kind === 1) {
      var f = hasFrom ? int(from) : n - 1;
      if (f === -Infinity) return -1;
      for (i = f >= 0 ? (f < n - 1 ? f : n - 1) : n + f; i >= 0; i--) if (i in a && a[i] === x) return i;
      return -1;
    }
    var k = int(from);
    if (k === Infinity) return kind === 2 ? false : -1;
    for (i = k >= 0 ? k : (n + k > 0 ? n + k : 0); i < n; i++) {
      if (kind === 2) {
        var v = a[i];
        if (v === x || v !== v && x !== x) return true;
      } else if (i in a && a[i] === x) return i;
    }
    return kind === 2 ? false : -1;
  });
  var getOwn = Object.getOwnPropertyDescriptor, ownNames = Object.getOwnPropertyNames;
  // `{...src}` into `dst` and `{a, ...rest} = src` when `src` has an accessor: the interpreter hands over (`interp.rs`).
  function copyData(dst, src, excluded) {
    var s = Object(src), ks = ownNames(s), ss = Object.getOwnPropertySymbols(s), i;
    for (i = 0; i < ss.length; i++) ks[ks.length] = ss[i];
    for (i = 0; i < ks.length; i++) {
      var k = ks[i];
      if (excluded !== undefined && excluded.indexOf(k) >= 0) continue;
      var d = getOwn(s, k);
      if (d !== undefined && d.enumerable) rawDefine(dst, k, { value: s[k], writable: true, enumerable: true, configurable: true });
    }
    return dst;
  }
  helper(4, function spread(dst, src) { return copyData(dst, src); });
  helper(5, function rest(src) {
    var ex = [];
    for (var i = 1; i < arguments.length; i++) ex[i - 1] = typeof arguments[i] === 'symbol' ? arguments[i] : '' + arguments[i];
    return copyData({}, src, ex);
  });
  // `Object.values` (0) and `Object.entries` (1) of an object with a getter.
  helper(6, function pairs(kind, o) {
    var ks = keys(o), r = [];
    for (var i = 0; i < ks.length; i++) {
      var d = getOwn(o, ks[i]);
      if (d !== undefined && d.enumerable) {
        var v = o[ks[i]];
        r[r.length] = kind === 0 ? v : [ks[i], v];
      }
    }
    return r;
  });
  rawDefine(Object.prototype, '__proto__', {
    get: function () {
      if (this === undefined || this === null) throw new TypeError('cannot convert undefined or null to an object');
      return getProto(this);
    },
    set: function (p) {
      if (this === undefined || this === null) throw new TypeError('cannot convert undefined or null to an object');
      if (typeof p !== 'object' && typeof p !== 'function' || typeof this !== 'object' && typeof this !== 'function') return;
      setProto(this, p);
    },
    configurable: true
  });
  function need(cb) {
    if (typeof cb !== 'function') throw new TypeError('the callback is not a function');
  }
  // `ToObject(this)`: an object stays as it is.
  function obj(v) {
    if (typeof v === 'object' && v !== null || typeof v === 'function') return v;
    if (v === undefined || v === null) throw new TypeError('Array method called on undefined or null');
    return Object(v);
  }
  // `ToLength(a.length)`.
  function len(a) {
    var n = +a.length;
    if (!(n > 0)) return 0;
    return n > 9007199254740991 ? 9007199254740991 : n - n % 1;
  }
  // `ToIntegerOrInfinity(v)`.
  function int(v) {
    v = +v;
    return v !== v ? 0 : v === Infinity || v === -Infinity ? v : v - v % 1;
  }
  function check(n) {
    if (n > 4294967295) throw new RangeError('invalid array length');
    return n;
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
  // The iterator of `keys`, `values` and `entries`. There are no generators and no symbols yet, so its state is in three own
  // properties whose names start with U+0000: no list of keys shows them. The prototype is built at the first call, not at load.
  var arrayIteratorProto;
  function arrayIterator(a, kind) {
    if (arrayIteratorProto === undefined) {
      arrayIteratorProto = {};
      def(arrayIteratorProto, {
        next: function next() {
          if (typeof this !== 'object' || this === null || !('\0k' in this)) throw new TypeError('next called on a value that is not an array iterator');
          var a = this['\0o'], i = this['\0i'];
          if (a !== undefined && i < len(a)) {
            this['\0i'] = i + 1;
            var k = this['\0k'];
            return { value: k === 0 ? i : k === 1 ? a[i] : [i, a[i]], done: false };
          }
          this['\0o'] = undefined;
          return { value: undefined, done: true };
        }
      }, { next: 0 });
      rawDefine(arrayIteratorProto, Symbol.iterator, { value: function() { return this; }, writable: true, configurable: true });
    }
    var it = Object.create(arrayIteratorProto);
    it['\0o'] = a;
    it['\0i'] = 0;
    it['\0k'] = kind;
    return it;
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
  function sortList(a, n, cmp) {
    var vals = [], k = 0, undefs = 0;
    for (var i = 0; i < n; i++) {
      if (!(i in a)) continue;
      var v = a[i];
      if (v === undefined) undefs++;
      else vals[k++] = v;
    }
    var sorted = mergeSort(vals, k, cmp);
    sorted.length = k;
    for (i = 0; i < undefs; i++) sorted[k + i] = undefined;
    return sorted;
  }
  function flatten(target, source, n, depth, fn, t) {
    var k = target.length;
    for (var i = 0; i < n; i++) {
      if (!(i in source)) continue;
      var e = source[i];
      if (fn !== undefined) e = fn.call(t, e, i, source);
      if (depth > 0 && Array.isArray(e)) k = flatten(target, e, len(e), depth - 1);
      else {
        target[k++] = e;
      }
    }
    return k;
  }
  def(Array.prototype, {
    keys: function keys() { return arrayIterator(obj(this), 0); },
    values: function values() { return arrayIterator(obj(this), 1); },
    entries: function entries() { return arrayIterator(obj(this), 2); },
    forEach: function forEach(cb) {
      var a = obj(this), n = len(a);
      need(cb);
      var f = bound(cb, arguments[1]);
      for (var i = 0; i < n; i++) {
        var v = a[i];
        if (v !== undefined || i in a) f(v, i, a);
      }
    },
    map: function map(cb) {
      var a = obj(this), n = len(a);
      need(cb);
      var c;
      if (Array.isArray(a) && ((c = a.constructor) === null || c !== undefined && typeof c !== 'object' && typeof c !== 'function')) throw new TypeError('the constructor of the array is not an object');
      var f = bound(cb, arguments[1]), r = [];
      check(n);
      for (var i = 0; i < n; i++) {
        var v = a[i];
        if (v !== undefined || i in a) r[i] = f(v, i, a);
      }
      r.length = n;
      return r;
    },
    filter: function filter(cb) {
      var a = obj(this), n = len(a);
      need(cb);
      var c;
      if (Array.isArray(a) && ((c = a.constructor) === null || c !== undefined && typeof c !== 'object' && typeof c !== 'function')) throw new TypeError('the constructor of the array is not an object');
      var f = bound(cb, arguments[1]), r = [], k = 0;
      for (var i = 0; i < n; i++) {
        var v = a[i];
        if ((v !== undefined || i in a) && f(v, i, a)) r[k++] = v;
      }
      return r;
    },
    reduce: function reduce(cb) {
      var a = obj(this), n = len(a), i = 0, acc;
      need(cb);
      if (arguments.length >= 2) acc = arguments[1];
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
    reduceRight: function reduceRight(cb) {
      var a = obj(this), i = len(a) - 1, acc;
      need(cb);
      if (arguments.length >= 2) acc = arguments[1];
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
    some: function some(cb) {
      var a = obj(this), n = len(a);
      need(cb);
      var f = bound(cb, arguments[1]);
      for (var i = 0; i < n; i++) {
        var v = a[i];
        if ((v !== undefined || i in a) && f(v, i, a)) return true;
      }
      return false;
    },
    every: function every(cb) {
      var a = obj(this), n = len(a);
      need(cb);
      var f = bound(cb, arguments[1]);
      for (var i = 0; i < n; i++) {
        var v = a[i];
        if ((v !== undefined || i in a) && !f(v, i, a)) return false;
      }
      return true;
    },
    find: function find(cb) {
      var a = obj(this), n = len(a);
      need(cb);
      var f = bound(cb, arguments[1]);
      for (var i = 0; i < n; i++) {
        var v = a[i];
        if (f(v, i, a)) return v;
      }
      return undefined;
    },
    findIndex: function findIndex(cb) {
      var a = obj(this), n = len(a);
      need(cb);
      var f = bound(cb, arguments[1]);
      for (var i = 0; i < n; i++) {
        if (f(a[i], i, a)) return i;
      }
      return -1;
    },
    findLast: function findLast(cb) {
      var a = obj(this), n = len(a);
      need(cb);
      var f = bound(cb, arguments[1]);
      for (var i = n - 1; i >= 0; i--) {
        var v = a[i];
        if (f(v, i, a)) return v;
      }
      return undefined;
    },
    findLastIndex: function findLastIndex(cb) {
      var a = obj(this), n = len(a);
      need(cb);
      var f = bound(cb, arguments[1]);
      for (var i = n - 1; i >= 0; i--) {
        if (f(a[i], i, a)) return i;
      }
      return -1;
    },
    flat: function flat() {
      var a = obj(this), n = len(a), d = arguments[0] === undefined ? 1 : int(arguments[0]), r = [], c;
      if (Array.isArray(a) && ((c = a.constructor) === null || c !== undefined && typeof c !== 'object' && typeof c !== 'function')) throw new TypeError('the constructor of the array is not an object');
      flatten(r, a, n, d);
      return r;
    },
    flatMap: function flatMap(cb) {
      var a = obj(this), n = len(a), r = [], c;
      need(cb);
      if (Array.isArray(a) && ((c = a.constructor) === null || c !== undefined && typeof c !== 'object' && typeof c !== 'function')) throw new TypeError('the constructor of the array is not an object');
      flatten(r, a, n, 1, cb, arguments[1]);
      return r;
    },
    sort: function sort(cmp) {
      if (cmp !== undefined && typeof cmp !== 'function') throw new TypeError('the comparison function must be a function or undefined');
      var a = obj(this), n = len(a);
      if (cmp === undefined) cmp = defaultCompare;
      var sorted = sortList(a, n, cmp), k = sorted.length;
      for (var i = 0; i < k; i++) a[i] = sorted[i];
      for (; i < n; i++) if (i in a) delete a[i];
      return a;
    },
    toSorted: function toSorted(cmp) {
      if (cmp !== undefined && typeof cmp !== 'function') throw new TypeError('the comparison function must be a function or undefined');
      var a = obj(this), n = len(a), vals = [];
      check(n);
      if (cmp === undefined) cmp = defaultCompare;
      for (var i = 0; i < n; i++) vals[i] = a[i];
      var k = 0, undefs = 0, vs = [];
      for (i = 0; i < n; i++) {
        if (vals[i] === undefined) undefs++;
        else vs[k++] = vals[i];
      }
      var sorted = mergeSort(vs, k, cmp), r = [];
      for (i = 0; i < k; i++) r[i] = sorted[i];
      for (; i < n; i++) r[i] = undefined;
      return r;
    },
    toLocaleString: function toLocaleString() {
      var a = obj(this), n = len(a), r = '';
      for (var i = 0; i < n; i++) {
        if (i > 0) r += ',';
        var v = a[i];
        if (v !== undefined && v !== null) r += v.toLocaleString();
      }
      return r;
    }
  }, { keys: 0, values: 0, entries: 0, forEach: 1, map: 1, filter: 1, reduce: 1, reduceRight: 1, some: 1, every: 1, find: 1, findIndex: 1, findLast: 1, findLastIndex: 1, flat: 0, flatMap: 1, sort: 1, toSorted: 1, toLocaleString: 0 });
  rawDefine(Array.prototype, Symbol.iterator, { value: Array.prototype.values, writable: true, configurable: true });
  def(Array, {
    of: function of() {
      var n = arguments.length, C = this, r = typeof C === 'function' && C !== Array ? new C(n) : [];
      for (var i = 0; i < n; i++) rawDefine(r, i, { value: arguments[i], writable: true, enumerable: true, configurable: true });
      r.length = n;
      return r;
    },
    from: function from(src) {
      var fn = arguments[1], t = arguments[2];
      if (src === undefined || src === null) throw new TypeError('the value is not iterable');
      if (fn !== undefined) need(fn);
      var a = obj(src), n = len(a), r = [], f = fn === undefined ? undefined : bound(fn, t);
      check(n);
      for (var i = 0; i < n; i++) r[i] = f === undefined ? a[i] : f(a[i], i);
      r.length = n;
      return r;
    }
  }, { of: 0, from: 1 });
  def(String.prototype, {
    replace: function replace(p, r) { return replaceWith(nativeReplace, this, p, r); },
    replaceAll: function replaceAll(p, r) { return replaceWith(nativeReplaceAll, this, p, r); }
  }, { replace: 2, replaceAll: 2 });
})(__def, __helper, __set);
