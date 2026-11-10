// A minimal in-memory `indexedDB` (no persistence, no key generators beyond `autoIncrement`). Requests run one after another
// in a microtask; a transaction that has no request left completes. Made at the first read of `indexedDB` or an `IDB*` name (`browser_shims.rs`).
(function () {
  'use strict';
  var P = Promise, dbs = {}, H = new WeakMap(), D = new WeakMap(), TS = new WeakMap(), OS = new WeakMap(), CS = new WeakMap(), IX = new WeakMap();
  var clone = typeof structuredClone === 'function' ? structuredClone : function (v) { return v; };
  // `new DOMException(...)` is not a constructor here, so the error inherits from the prototype.
  var codes = { AbortError: 20, NotFoundError: 8, InvalidStateError: 11, InvalidAccessError: 15 };
  function err(msg, name) {
    var e = Object.create(DOMException.prototype);
    Object.defineProperties(e, { message: { value: msg, writable: true, configurable: true }, name: { value: name, writable: true, configurable: true }, code: { value: codes[name] || 0, configurable: true } });
    return e;
  }
  function later(f) { P.resolve().then(f); }
  function def(o, k, v) { Object.defineProperty(o, k, { value: v, writable: true, configurable: true }); }
  function names(list) {
    var a = list.slice().sort();
    def(a, 'contains', function (n) { return a.indexOf(String(n)) >= 0; });
    def(a, 'item', function (i) { return i in a ? a[i] : null; });
    return a;
  }
  function handlers(C, types) {
    types.forEach(function (t) {
      Object.defineProperty(C.prototype, 'on' + t, {
        get: function () { var h = H.get(this); return (h && h[t]) || null; },
        set: function (f) {
          var h = H.get(this);
          if (!h) H.set(this, h = {});
          if (!(t in h)) this.addEventListener(t, function (e) { if (typeof h[t] === 'function') h[t].call(this, e); });
          h[t] = typeof f === 'function' ? f : null;
        },
        enumerable: true, configurable: true
      });
    });
  }

  // ---- keys
  function rank(k) { return typeof k === 'number' ? 1 : k instanceof Date ? 2 : typeof k === 'string' ? 3 : Array.isArray(k) ? 5 : 0; }
  function valid(k) {
    var r = rank(k);
    return r === 5 ? k.every(valid) : r === 1 ? k === k : r === 2 ? +k === +k : r === 3;
  }
  function key(k) {
    if (!valid(k)) throw err('The parameter is not a valid key.', 'DataError');
    return k;
  }
  function cmp(a, b) {
    var x = rank(a), y = rank(b);
    if (x !== y) return x < y ? -1 : 1;
    if (x === 5) {
      for (var i = 0, n = Math.min(a.length, b.length); i < n; i++) {
        var c = cmp(a[i], b[i]);
        if (c) return c;
      }
      return a.length < b.length ? -1 : a.length > b.length ? 1 : 0;
    }
    if (x === 2) { a = +a; b = +b; }
    return a < b ? -1 : a > b ? 1 : 0;
  }
  function IDBKeyRange() { throw new TypeError('Illegal constructor'); }
  function range(lo, up, lop, uop) {
    var r = Object.create(IDBKeyRange.prototype);
    Object.defineProperties(r, { lower: { value: lo, enumerable: true }, upper: { value: up, enumerable: true }, lowerOpen: { value: !!lop, enumerable: true }, upperOpen: { value: !!uop, enumerable: true } });
    return r;
  }
  function inr(r, k) {
    var c;
    if (r.lower !== undefined && ((c = cmp(k, r.lower)) < 0 || (c === 0 && r.lowerOpen))) return false;
    if (r.upper !== undefined && ((c = cmp(k, r.upper)) > 0 || (c === 0 && r.upperOpen))) return false;
    return true;
  }
  var ALL = range(undefined, undefined, false, false);
  function rng(q) { return q == null ? ALL : q instanceof IDBKeyRange ? q : range(key(q), q, false, false); }
  def(IDBKeyRange, 'only', function (v) { key(v); return range(v, v, false, false); });
  def(IDBKeyRange, 'lowerBound', function (l, open) { return range(key(l), undefined, open, true); });
  def(IDBKeyRange, 'upperBound', function (u, open) { return range(undefined, key(u), true, open); });
  def(IDBKeyRange, 'bound', function (l, u, lo, uo) {
    var c = cmp(key(l), key(u));
    if (c > 0 || (c === 0 && (lo || uo))) throw err('The lower key is greater than the upper key.', 'DataError');
    return range(l, u, lo, uo);
  });
  def(IDBKeyRange.prototype, 'includes', function (k) { return inr(this, key(k)); });

  // ---- rows: one sorted array of [key, value] for each store
  function at(rows, k) {
    var lo = 0, hi = rows.length;
    while (lo < hi) {
      var m = (lo + hi) >> 1;
      if (cmp(rows[m][0], k) < 0) lo = m + 1; else hi = m;
    }
    return lo;
  }
  function pick(st, q) {
    var out = [];
    for (var i = q.lower === undefined ? 0 : at(st.rows, q.lower); i < st.rows.length; i++) {
      var k = st.rows[i][0];
      if (q.upper !== undefined && cmp(k, q.upper) > 0) break;
      if (inr(q, k)) out.push(st.rows[i]);
    }
    return out;
  }
  function extract(v, kp) {
    if (Array.isArray(kp)) return kp.map(function (p) { return extract(v, p); });
    if (kp === '') return v;
    var p = kp.split('.');
    for (var i = 0; i < p.length; i++) {
      if (v == null || !(p[i] in Object(v))) return undefined;
      v = v[p[i]];
    }
    return v;
  }
  function inject(v, kp, k) {
    var p = kp.split('.');
    for (var i = 0; i < p.length - 1; i++) v = v[p[i]] || (v[p[i]] = {});
    v[p[p.length - 1]] = k;
  }
  function touch(t, st) {
    if (t.snap.has(st)) return;
    t.snap.add(st);
    var rows = st.rows.slice(), next = st.next;
    t.undo.push(function () { st.rows = rows; st.next = next; });
  }
  function store(t, st, v, k, add) {
    touch(t, st);
    if (st.kp !== null) {
      if (k !== undefined) throw err('The object store uses in-line keys and the key parameter was provided.', 'DataError');
      k = extract(v, st.kp);
    }
    if (k === undefined) {
      if (!st.auto) throw err('The object store uses out-of-line keys and has no key generator and the key parameter was not provided.', 'DataError');
      k = st.next;
      if (st.kp !== null) inject(v, st.kp, k);
    } else {
      key(k);
    }
    Object.keys(st.indexes).forEach(function (name) { unique(st, st.indexes[name], v, k); });
    if (st.auto && typeof k === 'number' && k >= st.next) st.next = Math.floor(k) + 1;
    var i = at(st.rows, k);
    if (i < st.rows.length && cmp(st.rows[i][0], k) === 0) {
      if (add) throw err('Unable to add key to index. A record with this key already exists.', 'ConstraintError');
      st.rows[i] = [k, v];
    } else {
      st.rows.splice(i, 0, [k, v]);
    }
    return k;
  }

  // ---- requests and transactions
  class IDBRequest extends EventTarget {}
  class IDBOpenDBRequest extends IDBRequest {}
  class IDBDatabase extends EventTarget {}
  class IDBTransaction extends EventTarget {}
  class IDBObjectStore {}
  function IDBIndex() { throw new TypeError('Illegal constructor'); }
  class IDBCursor {}
  class IDBFactory {}
  handlers(IDBRequest, ['success', 'error']);
  handlers(IDBOpenDBRequest, ['upgradeneeded', 'blocked']);
  handlers(IDBDatabase, ['abort', 'close', 'error', 'versionchange']);
  handlers(IDBTransaction, ['abort', 'complete', 'error']);
  function req(src, tx, C) {
    var r = new (C || IDBRequest)();
    r.result = undefined; r.error = null; r.source = src; r.transaction = tx; r.readyState = 'pending';
    return r;
  }
  function ok(r, v) {
    r.result = v; r.readyState = 'done'; r.error = null;
    r.dispatchEvent(new Event('success'));
  }
  function kill(tx, e) {
    var t = TS.get(tx);
    if (t.dead) return;
    t.dead = true;
    t.q.length = 0;
    for (var i = t.undo.length; i--;) t.undo[i]();
    tx.error = e;
    later(function () {
      tx.dispatchEvent(new Event('abort', { bubbles: true }));
      if (t.open) {
        t.open.error = err('The transaction was aborted, so the request cannot be fulfilled.', 'AbortError');
        t.open.result = undefined;
        t.open.dispatchEvent(new Event('error', { bubbles: true, cancelable: true }));
      }
    });
  }
  function fail(r, e, tx) {
    r.error = e; r.readyState = 'done';
    var a = new Event('error', { bubbles: true, cancelable: true }), b = new Event('error', { bubbles: true, cancelable: true });
    r.dispatchEvent(a);
    tx.dispatchEvent(b);
    if (!a.defaultPrevented && !b.defaultPrevented) kill(tx, e);
  }
  function kick(tx) {
    var t = TS.get(tx);
    if (t.on) return;
    t.on = true;
    later(function step() {
      if (t.dead) return;
      var o = t.q.shift();
      if (!o) {
        t.dead = true;
        tx.dispatchEvent(new Event('complete'));
        if (t.after) t.after();
        return;
      }
      var v;
      try { v = o[0](); } catch (e) { fail(o[1], e, tx); later(step); return; }
      ok(o[1], v);
      later(step);
    });
  }
  function newTx(db, mode, list) {
    var tx = new IDBTransaction(), data = D.get(db).data;
    tx.db = db; tx.mode = mode; tx.error = null; tx.durability = 'default';
    Object.defineProperty(tx, 'objectStoreNames', { get: function () { return names(mode === 'versionchange' ? Object.keys(data.stores) : list); }, enumerable: true, configurable: true });
    TS.set(tx, { q: [], on: false, dead: false, undo: [], snap: new Set(), os: {}, open: null, after: null, data: data, list: list });
    return tx;
  }
  function live(tx) {
    var t = TS.get(tx);
    if (t.dead) throw err('The transaction has finished.', 'TransactionInactiveError');
    return t;
  }
  function osObj(tx, st) {
    var o = new IDBObjectStore();
    o.name = st.name; o.keyPath = st.kp; o.autoIncrement = st.auto; o.transaction = tx;
    Object.defineProperty(o, 'indexNames', { get: function () { return names(Object.keys(st.indexes)); }, enumerable: true });
    OS.set(o, { tx: tx, st: st, indexes: new Map() });
    return o;
  }
  def(IDBTransaction.prototype, 'objectStore', function (name) {
    var t = live(this);
    name = String(name);
    var st = t.data.stores[name];
    if (!st || (this.mode !== 'versionchange' && t.list.indexOf(name) < 0)) throw err('The specified object store was not found.', 'NotFoundError');
    return t.os[name] || (t.os[name] = osObj(this, st));
  });
  def(IDBTransaction.prototype, 'abort', function () { live(this); kill(this, null); });
  def(IDBTransaction.prototype, 'commit', function () { live(this); });

  // ---- object stores
  function op(os, write, f) {
    var o = osState(os), t = live(o.tx);
    if (write && o.tx.mode === 'readonly') throw err('The transaction is read-only.', 'ReadOnlyError');
    var r = req(os, o.tx);
    t.q.push([function () { return f(o.st, t); }, r]);
    kick(o.tx);
    return r;
  }
  var proto = IDBObjectStore.prototype;
  def(proto, 'put', function (v, k) { v = clone(v); return op(this, true, function (st, t) { return store(t, st, v, k, false); }); });
  def(proto, 'add', function (v, k) { v = clone(v); return op(this, true, function (st, t) { return store(t, st, v, k, true); }); });
  def(proto, 'get', function (q) { return op(this, false, function (st) { var r = pick(st, rng(q)); return r.length ? clone(r[0][1]) : undefined; }); });
  def(proto, 'getKey', function (q) { return op(this, false, function (st) { var r = pick(st, rng(q)); return r.length ? r[0][0] : undefined; }); });
  def(proto, 'getAll', function (q, n) { return op(this, false, function (st) { return pick(st, rng(q)).slice(0, n || undefined).map(function (r) { return clone(r[1]); }); }); });
  def(proto, 'getAllKeys', function (q, n) { return op(this, false, function (st) { return pick(st, rng(q)).slice(0, n || undefined).map(function (r) { return r[0]; }); }); });
  def(proto, 'count', function (q) { return op(this, false, function (st) { return pick(st, rng(q)).length; }); });
  def(proto, 'delete', function (q) {
    q = rng(q);
    return op(this, true, function (st, t) {
      touch(t, st);
      st.rows = st.rows.filter(function (r) { return !inr(q, r[0]); });
    });
  });
  def(proto, 'clear', function () { return op(this, true, function (st, t) { touch(t, st); st.rows = []; }); });
  def(proto, 'openCursor', function (q, dir) {
    q = rng(q);
    var os = this, c = new IDBCursor(), s = { i: -1, list: null, os: os, r: null };
    dir = dir === undefined ? 'next' : String(dir);
    var r = op(os, false, function (st) {
      s.list = pick(st, q);
      if (dir.indexOf('prev') === 0) s.list.reverse();
      return move(c, s, 1);
    });
    s.r = r;
    CS.set(c, s);
    c.source = os; c.direction = dir; c.request = r;
    return r;
  });
  function move(c, s, n) {
    s.i += n;
    var row = s.list[s.i];
    if (!row) return null;
    c.key = c.primaryKey = row[0];
    c.value = clone(row[1]);
    return c;
  }
  function step(c, n) {
    var s = CS.get(c), o = OS.get(s.os);
    live(o.tx).q.push([function () { return move(c, s, n); }, s.r]);
    kick(o.tx);
  }
  def(IDBCursor.prototype, 'continue', function () { step(this, 1); });
  def(IDBCursor.prototype, 'advance', function (n) { step(this, n >= 1 ? Math.floor(n) : 1); });
  def(IDBCursor.prototype, 'update', function (v) {
    var s = CS.get(this), k = this.primaryKey;
    v = clone(v);
    return op(s.os, true, function (st, t) { return store(t, st, v, st.kp === null ? k : undefined, false); });
  });
  def(IDBCursor.prototype, 'delete', function () {
    var k = this.primaryKey;
    return op(CS.get(this).os, true, function (st, t) { touch(t, st); st.rows = st.rows.filter(function (r) { return cmp(r[0], k) !== 0; }); });
  });

  // Index entries are derived from the transaction's current store rows. This keeps
  // writes, deletes and rollback on the same source of truth.
  function ixKeys(ix, value) {
    var k = extract(value, ix.kp), out = [];
    var ks = ix.multi && Array.isArray(k) ? k : [k];
    ks.forEach(function (v) {
      if (valid(v) && !out.some(function (x) { return cmp(x, v) === 0; })) out.push(v);
    });
    return out;
  }
  function ixRows(st, ix, q) {
    var rows = [];
    st.rows.forEach(function (row) {
      ixKeys(ix, row[1]).forEach(function (k) { if (inr(q, k)) rows.push([k, row[0], row[1]]); });
    });
    rows.sort(function (a, b) { return cmp(a[0], b[0]) || cmp(a[1], b[1]); });
    return rows;
  }
  function unique(st, ix, value, primary) {
    if (!ix.unique) return;
    var ks = ixKeys(ix, value);
    st.rows.forEach(function (row) {
      if (cmp(row[0], primary) === 0) return;
      ixKeys(ix, row[1]).forEach(function (k) {
        if (ks.some(function (v) { return cmp(v, k) === 0; })) throw err('An index key already exists.', 'ConstraintError');
      });
    });
  }
  function osState(os) {
    var s = OS.get(os);
    if (!s) throw new TypeError('Illegal invocation');
    if (TS.get(s.tx).data.stores[s.st.name] !== s.st) throw err('The object store was deleted.', 'InvalidStateError');
    return s;
  }
  function schema(os) {
    var s = osState(os);
    if (s.tx.mode !== 'versionchange') throw err('A version change transaction is required.', 'InvalidStateError');
    live(s.tx);
    return s;
  }
  function ixObj(os, ix) {
    var cache = OS.get(os).indexes;
    if (cache.has(ix)) return cache.get(ix);
    var obj = Object.create(IDBIndex.prototype);
    cache.set(ix, obj);
    IX.set(obj, { os: os, ix: ix });
    return obj;
  }
  function ixState(obj) {
    var s = IX.get(obj);
    if (!s) throw new TypeError('Illegal invocation');
    return s;
  }
  ['name', 'keyPath', 'unique', 'multiEntry', 'objectStore'].forEach(function (prop) {
    Object.defineProperty(IDBIndex.prototype, prop, { get: function () {
      var s = ixState(this);
      return prop === 'objectStore' ? s.os : prop === 'keyPath' ? clone(s.ix.kp) : s.ix[prop === 'multiEntry' ? 'multi' : prop];
    }, enumerable: true, configurable: true });
  });
  def(proto, 'createIndex', function (name, kp, opts) {
    var s = schema(this), os = this, t = live(s.tx);
    name = String(name);
    if (s.st.indexes[name]) throw err('The index already exists.', 'ConstraintError');
    if (kp === undefined || kp === null) throw new TypeError('A key path is required');
    kp = Array.isArray(kp) ? kp.map(String) : String(kp);
    function path(p) { return p === '' || p.split('.').every(function (x) { return /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(x); }); }
    if (!(Array.isArray(kp) ? kp.every(path) : path(kp))) throw err('Invalid key path.', 'SyntaxError');
    var ix = { name: name, kp: kp, unique: !!(opts && opts.unique), multi: !!(opts && opts.multiEntry) };
    if (ix.multi && Array.isArray(kp)) throw err('Compound multiEntry index.', 'InvalidAccessError');
    s.st.indexes[name] = ix;
    t.undo.push(function () { delete s.st.indexes[name]; });
    op(os, false, function (st) { st.rows.forEach(function (row) { unique(st, ix, row[1], row[0]); }); });
    return ixObj(os, ix);
  });
  def(proto, 'deleteIndex', function (name) {
    var s = schema(this), t = live(s.tx);
    name = String(name);
    var ix = s.st.indexes[name];
    if (!ix) throw err('The index was not found.', 'NotFoundError');
    delete s.st.indexes[name];
    t.undo.push(function () { s.st.indexes[name] = ix; });
  });
  def(proto, 'index', function (name) {
    var s = osState(this); live(s.tx);
    var ix = s.st.indexes[String(name)];
    if (!ix) throw err('The index was not found.', 'NotFoundError');
    return ixObj(this, ix);
  });
  function ixOp(obj, q, f) {
    var s = ixState(obj);
    var st = osState(s.os).st;
    if (st.indexes[s.ix.name] !== s.ix) throw err('The index was deleted.', 'InvalidStateError');
    q = rng(q);
    var r = op(s.os, false, function (st) { return f(ixRows(st, s.ix, q)); });
    r.source = obj;
    return r;
  }
  def(IDBIndex.prototype, 'get', function (q) { ixState(this); if (q == null) throw err('A key is required.', 'DataError'); return ixOp(this, q, function (rows) { return rows.length ? clone(rows[0][2]) : undefined; }); });
  def(IDBIndex.prototype, 'getKey', function (q) { ixState(this); if (q == null) throw err('A key is required.', 'DataError'); return ixOp(this, q, function (rows) { return rows.length ? clone(rows[0][1]) : undefined; }); });
  ['getAll', 'getAllKeys', 'count'].forEach(function (method) {
    def(IDBIndex.prototype, method, function (q, n) {
      ixState(this);
      if (n !== undefined && (!Number.isFinite(Number(n)) || Number(n) < 0 || Number(n) > 4294967295)) throw new TypeError('Invalid count');
      return ixOp(this, q, function (rows) {
        if (method === 'count') return rows.length;
        return rows.slice(0, n ? Math.floor(n) : undefined).map(function (r) { return clone(r[method === 'getAll' ? 2 : 1]); });
      });
    });
  });

  // ---- databases
  def(IDBDatabase.prototype, 'close', function () { D.get(this).closed = true; });
  Object.defineProperty(IDBDatabase.prototype, 'objectStoreNames', { get: function () { return names(Object.keys(D.get(this).data.stores)); }, enumerable: true, configurable: true });
  def(IDBDatabase.prototype, 'createObjectStore', function (name, opts) {
    var d = D.get(this), t = d.vtx && TS.get(d.vtx);
    if (!t || t.dead) throw err('The database is not running a version change transaction.', 'InvalidStateError');
    name = String(name);
    if (d.data.stores[name]) throw err('An object store with the specified name already exists.', 'ConstraintError');
    var st = { name: name, kp: opts && opts.keyPath != null ? opts.keyPath : null, auto: !!(opts && opts.autoIncrement), next: 1, rows: [], indexes: Object.create(null) };
    d.data.stores[name] = st;
    t.undo.push(function () { delete d.data.stores[name]; });
    return t.os[name] = osObj(d.vtx, st);
  });
  def(IDBDatabase.prototype, 'deleteObjectStore', function (name) {
    var d = D.get(this), t = d.vtx && TS.get(d.vtx);
    if (!t || t.dead) throw err('The database is not running a version change transaction.', 'InvalidStateError');
    name = String(name);
    var st = d.data.stores[name];
    if (!st) throw err('The specified object store was not found.', 'NotFoundError');
    delete d.data.stores[name];
    delete t.os[name];
    t.undo.push(function () { d.data.stores[name] = st; });
  });
  def(IDBDatabase.prototype, 'transaction', function (list, mode) {
    var d = D.get(this);
    if (d.closed) throw err('The database connection is closing.', 'InvalidStateError');
    list = (Array.isArray(list) ? list : [list]).map(String);
    list.forEach(function (n) { if (!d.data.stores[n]) throw err('One of the specified object stores was not found.', 'NotFoundError'); });
    if (!list.length) throw err('The storeNames parameter is empty.', 'InvalidAccessError');
    mode = mode === undefined ? 'readonly' : String(mode);
    var tx = newTx(this, mode, list);
    kick(tx);
    return tx;
  });
  function conn(name, data) {
    var db = new IDBDatabase();
    db.name = name; db.version = data.version;
    D.set(db, { data: data, closed: false, vtx: null });
    return db;
  }
  function open(name, version) {
    name = String(name);
    if (version !== undefined) {
      version = Number(version);
      if (!(version >= 1 && version <= 9007199254740991)) throw new TypeError("Failed to execute 'open' on 'IDBFactory': The version provided must be a positive integer.");
      version = Math.floor(version);
    }
    var r = req(null, null, IDBOpenDBRequest);
    later(function () {
      var cur = dbs[name], old = cur ? cur.version : 0, v = version === undefined ? (old || 1) : version;
      r.readyState = 'done';
      if (v < old) {
        r.error = err('The requested version (' + v + ') is less than the existing version (' + old + ').', 'VersionError');
        r.dispatchEvent(new Event('error', { bubbles: true, cancelable: true }));
        return;
      }
      var data = cur || (dbs[name] = { version: 0, stores: {} }), db = conn(name, data);
      r.result = db;
      if (v === old) {
        r.dispatchEvent(new Event('success'));
        return;
      }
      var tx = newTx(db, 'versionchange', []), t = TS.get(tx), d = D.get(db), e = new Event('upgradeneeded');
      d.vtx = tx; t.open = r; r.transaction = tx;
      t.undo.push(function () { data.version = old; if (!old) delete dbs[name]; });
      data.version = db.version = v;
      t.after = function () {
        d.vtx = null; r.transaction = null;
        r.dispatchEvent(new Event('success'));
      };
      e.oldVersion = old; e.newVersion = v;
      r.dispatchEvent(e);
      kick(tx);
    });
    return r;
  }
  function deleteDatabase(name) {
    name = String(name);
    var r = req(null, null, IDBOpenDBRequest);
    later(function () {
      var old = dbs[name] ? dbs[name].version : 0, e = new Event('success');
      delete dbs[name];
      r.readyState = 'done';
      e.oldVersion = old; e.newVersion = null;
      r.dispatchEvent(e);
    });
    return r;
  }
  def(IDBFactory.prototype, 'open', open);
  def(IDBFactory.prototype, 'deleteDatabase', deleteDatabase);
  def(IDBFactory.prototype, 'cmp', function (a, b) { return cmp(key(a), key(b)); });
  def(IDBFactory.prototype, 'databases', function () { return P.resolve(Object.keys(dbs).map(function (n) { return { name: n, version: dbs[n].version }; })); });
  var all = { IDBRequest: IDBRequest, IDBOpenDBRequest: IDBOpenDBRequest, IDBDatabase: IDBDatabase, IDBTransaction: IDBTransaction, IDBObjectStore: IDBObjectStore, IDBIndex: IDBIndex, IDBCursor: IDBCursor, IDBFactory: IDBFactory, IDBKeyRange: IDBKeyRange };
  Object.keys(all).forEach(function (n) {
    Object.defineProperty(all[n].prototype, Symbol.toStringTag, { value: n, configurable: true });
    def(globalThis, n, all[n]);
  });
  def(globalThis, 'indexedDB', new IDBFactory());
})();
