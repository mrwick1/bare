// `indexedDB`: the API objects, the events and the request order. Requests run one after another in a microtask; a transaction
// that has no request left completes. Made at the first read of `indexedDB` or an `IDB*` name (`browser_shims.rs`). The source gives
// a function; the host calls it with `idb(op, ...)`, the session store of the origin (`browser_idb.rs`), which holds the data.
(function (idb) {
  'use strict';
  var P = Promise, H = new WeakMap(), D = new WeakMap(), TS = new WeakMap(), OS = new WeakMap(), CS = new WeakMap(), IX = new WeakMap();
  var clone = typeof structuredClone === 'function' ? structuredClone : function (v) { return v; };
  // `new DOMException(...)` is not a constructor here, so the error inherits from the prototype.
  var codes = { AbortError: 20, NotFoundError: 8, InvalidStateError: 11, InvalidAccessError: 15 };
  function err(msg, name) {
    var e = Object.create(DOMException.prototype);
    Object.defineProperties(e, { message: { value: msg, writable: true, configurable: true }, name: { value: name, writable: true, configurable: true }, code: { value: codes[name] || 0, configurable: true } });
    return e;
  }
  function later(f) { P.resolve().then(f); }
  // A new task. The standard runs each request in its own task; here a run of up to `BATCH` requests shares one microtask chain,
  // then the next one waits for a task. Between tasks the host can collect, so the events of a long transaction do not pile up
  // (`BrowserLimits::max_events`). A `MessageChannel` message has no 4 ms clamp, unlike a nested `setTimeout`.
  var BATCH = 64, mc = null, tasks = [];
  function task(f) {
    if (typeof MessageChannel !== 'function') { setTimeout(f, 0); return; }
    if (!mc) { mc = new MessageChannel(); mc.port1.onmessage = function () { tasks.shift()(); }; }
    tasks.push(f);
    mc.port2.postMessage(0);
  }
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
  // The key as the host takes it: a `Date` is `{ t: time }`.
  function hk(k) { return k instanceof Date ? { t: +k } : Array.isArray(k) ? k.map(hk) : k; }
  function lo(q) { return q.lower === undefined ? undefined : hk(q.lower); }
  function up(q) { return q.upper === undefined ? undefined : hk(q.upper); }
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

  // ---- rows: in the host store
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
  function store(t, st, v, k, add) {
    if (st.kp !== null) {
      if (k !== undefined) throw err('The object store uses in-line keys and the key parameter was provided.', 'DataError');
      k = extract(v, st.kp);
    }
    if (k === undefined) {
      if (!st.auto) throw err('The object store uses out-of-line keys and has no key generator and the key parameter was not provided.', 'DataError');
      k = idb(12, t.name, st.name);
      if (st.kp !== null) inject(v, st.kp, k);
    } else {
      key(k);
    }
    var ix = [];
    for (var n in st.indexes) ix.push([n, ixKeys(st.indexes[n], v).map(hk)]);
    idb(13, t.id, t.name, st.name, hk(k), v, add, ix);
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
    if (t.id) idb(5, t.id);
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
    var n = 0;
    later(function step() {
      if (t.dead) { t.on = false; return; }
      var o = t.q.shift();
      if (!o) {
        // Queue momentarily empty. A spec transaction commits only when control returns to the event
        // loop with no request pending, not on the first empty microtask. The `idb` wrapper and
        // `await` enqueue the next request several microtask hops later, inside this same task, so we
        // wait for the next task boundary and commit only if the queue is still empty then.
        t.on = false;
        if (t.graceOn) return;
        t.graceOn = true;
        setTimeout(function () {
          t.graceOn = false;
          if (t.dead || t.on) return;
          if (t.q.length) { kick(tx); return; }
          t.dead = true;
          if (t.id) idb(4, t.id);
          tx.dispatchEvent(new Event('complete'));
          if (t.after) t.after();
        }, 0);
        return;
      }
      var v;
      var next = ++n % BATCH ? later : task;
      try { v = o[0](); } catch (e) { fail(o[1], e, tx); next(step); return; }
      ok(o[1], v);
      next(step);
    });
  }
  function newTx(db, mode, list) {
    var tx = new IDBTransaction(), data = D.get(db).data;
    tx.db = db; tx.mode = mode; tx.error = null; tx.durability = 'default';
    Object.defineProperty(tx, 'objectStoreNames', { get: function () { return names(mode === 'versionchange' ? Object.keys(data.stores) : list); }, enumerable: true, configurable: true });
    // A read-only transaction changes nothing, so it has no undo list in the host (id 0).
    TS.set(tx, { q: [], on: false, dead: false, undo: [], os: {}, open: null, after: null, graceOn: false, data: data, list: list, name: D.get(db).name, id: mode === 'readonly' ? 0 : idb(3) });
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
    Object.defineProperty(o, 'indexNames', { get: function () { return names(Object.keys(osState(this).st.indexes)); }, enumerable: true });
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
  // `what`: 0 the first value, 1 the first key, 2 values, 3 keys, 4 the count. `ix` is the index name or null.
  function query(t, st, ix, q, what, n) { return idb(14, t.name, st.name, ix, lo(q), up(q), q.lowerOpen, q.upperOpen, what, n); }
  ['get', 'getKey', 'getAll', 'getAllKeys', 'count'].forEach(function (m, what) {
    def(proto, m, function (q, n) { q = rng(q); return op(this, false, function (st, t) { return query(t, st, null, q, what, n); }); });
  });
  def(proto, 'delete', function (q) {
    q = rng(q);
    return op(this, true, function (st, t) { idb(15, t.id, t.name, st.name, lo(q), up(q), q.lowerOpen, q.upperOpen); });
  });
  def(proto, 'clear', function () { return op(this, true, function (st, t) { idb(16, t.id, t.name, st.name); }); });
  // A cursor over the store rows or the entries of index `ix` (a name or null) in range `q`. The host walks from the position of
  // the last hit (its key bytes). A key cursor has no value.
  var DIRS = ['next', 'nextunique', 'prev', 'prevunique'];
  function cursor(os, src, ix, q, dir, keys) {
    var c = new IDBCursor(), s = { os: os, r: null, ix: ix, q: q, dir: 0, keys: keys, pk: null, pp: null };
    dir = dir === undefined ? 'next' : String(dir);
    s.dir = DIRS.indexOf(dir);
    if (s.dir < 0) throw new TypeError('Invalid cursor direction');
    var r = op(os, false, function (st, t) { return move(c, s, st, t, 1); });
    s.r = r;
    CS.set(c, s);
    c.source = src; c.direction = dir; c.request = r;
    r.source = src;
    return r;
  }
  def(proto, 'openCursor', function (q, dir) { return cursor(this, this, null, rng(q), dir, false); });
  def(proto, 'openKeyCursor', function (q, dir) { return cursor(this, this, null, rng(q), dir, true); });
  function move(c, s, st, t, n) {
    var q = s.q, hit = idb(17, t.name, st.name, s.ix, lo(q), up(q), q.lowerOpen, q.upperOpen, s.dir, s.pk, s.pp, n, s.keys);
    if (!hit) return null;
    c.key = hit[0];
    c.primaryKey = hit[1];
    if (!s.keys) c.value = hit[2];
    s.pk = hit[3]; s.pp = hit[4];
    return c;
  }
  function step(c, n) {
    var s = CS.get(c), o = osState(s.os), t = live(o.tx);
    t.q.push([function () { return move(c, s, o.st, t, n); }, s.r]);
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
    return op(CS.get(this).os, true, function (st, t) { var h = hk(k); idb(15, t.id, t.name, st.name, h, h, false, false); });
  });

  // The keys of `value` in index `ix`: valid keys only, no duplicates.
  function ixKeys(ix, value) {
    var k = extract(value, ix.kp), out = [];
    var ks = ix.multi && Array.isArray(k) ? k : [k];
    ks.forEach(function (v) {
      if (valid(v) && !out.some(function (x) { return cmp(x, v) === 0; })) out.push(v);
    });
    return out;
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
    idb(10, t.id, t.name, s.st.name, name, kp, ix.unique, ix.multi);
    s.st.indexes[name] = ix;
    t.undo.push(function () { delete s.st.indexes[name]; });
    op(os, false, function (st, t) {
      var rows = idb(18, t.name, st.name);
      idb(19, t.id, t.name, st.name, name, rows.map(function (row) { return [hk(row[0]), ixKeys(ix, row[1]).map(hk)]; }));
    });
    return ixObj(os, ix);
  });
  def(proto, 'deleteIndex', function (name) {
    var s = schema(this), t = live(s.tx);
    name = String(name);
    var ix = s.st.indexes[name];
    if (!ix) throw err('The index was not found.', 'NotFoundError');
    idb(11, t.id, t.name, s.st.name, name);
    delete s.st.indexes[name];
    t.undo.push(function () { s.st.indexes[name] = ix; });
  });
  def(proto, 'index', function (name) {
    var s = osState(this); live(s.tx);
    var ix = s.st.indexes[String(name)];
    if (!ix) throw err('The index was not found.', 'NotFoundError');
    return ixObj(this, ix);
  });
  function ixLive(obj) {
    var s = ixState(obj);
    if (osState(s.os).st.indexes[s.ix.name] !== s.ix) throw err('The index was deleted.', 'InvalidStateError');
    return s;
  }
  ['get', 'getKey', 'getAll', 'getAllKeys', 'count'].forEach(function (method, what) {
    def(IDBIndex.prototype, method, function (q, n) {
      var s = ixLive(this);
      if (what < 2 && q == null) throw err('A key is required.', 'DataError');
      if (n !== undefined && (!Number.isFinite(Number(n)) || Number(n) < 0 || Number(n) > 4294967295)) throw new TypeError('Invalid count');
      q = rng(q);
      var r = op(s.os, false, function (st, t) { return query(t, st, s.ix.name, q, what, n); });
      r.source = this;
      return r;
    });
  });
  ['openCursor', 'openKeyCursor'].forEach(function (method) {
    def(IDBIndex.prototype, method, function (q, dir) {
      var x = ixLive(this);
      return cursor(x.os, this, x.ix.name, rng(q), dir, method === 'openKeyCursor');
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
    var st = { name: name, kp: opts && opts.keyPath != null ? opts.keyPath : null, auto: !!(opts && opts.autoIncrement), indexes: Object.create(null) };
    idb(8, t.id, d.name, name, st.kp, st.auto);
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
    idb(9, t.id, d.name, name);
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
    D.set(db, { name: name, data: data, closed: false, vtx: null });
    return db;
  }
  // The schema of database `name` from the host: `{ version, stores: { name: { name, kp, auto, indexes } } }`.
  function load(name, version) {
    var data = { version: version, stores: {} };
    idb(7, name).forEach(function (s) {
      var st = data.stores[s[0]] = { name: s[0], kp: s[1], auto: s[2], indexes: Object.create(null) };
      s[3].forEach(function (x) { st.indexes[x[0]] = { name: x[0], kp: x[1], unique: x[2], multi: x[3] }; });
    });
    return data;
  }
  function open(name, version) {
    name = String(name);
    if (version !== undefined) {
      version = Number(version);
      if (!(version >= 1 && version <= 9007199254740991)) throw new TypeError("Failed to execute 'open' on 'IDBFactory': The version provided must be a positive integer.");
      version = Math.floor(version);
    }
    idb(20);
    var r = req(null, null, IDBOpenDBRequest);
    later(function () {
      var old = idb(0, name), v = version === undefined ? (old || 1) : version;
      r.readyState = 'done';
      if (v < old) {
        r.error = err('The requested version (' + v + ') is less than the existing version (' + old + ').', 'VersionError');
        r.dispatchEvent(new Event('error', { bubbles: true, cancelable: true }));
        return;
      }
      var data = load(name, old), db = conn(name, data);
      r.result = db;
      if (v === old) {
        r.dispatchEvent(new Event('success'));
        return;
      }
      var tx = newTx(db, 'versionchange', []), t = TS.get(tx), d = D.get(db), e = new Event('upgradeneeded');
      d.vtx = tx; t.open = r; r.transaction = tx;
      t.undo.push(function () { data.version = old; });
      idb(6, t.id, name, v);
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
    idb(20);
    var r = req(null, null, IDBOpenDBRequest);
    later(function () {
      var old = idb(2, name), e = new Event('success');
      r.readyState = 'done';
      e.oldVersion = old; e.newVersion = null;
      r.dispatchEvent(e);
    });
    return r;
  }
  def(IDBFactory.prototype, 'open', open);
  def(IDBFactory.prototype, 'deleteDatabase', deleteDatabase);
  def(IDBFactory.prototype, 'cmp', function (a, b) { return cmp(key(a), key(b)); });
  def(IDBFactory.prototype, 'databases', function () { return P.resolve().then(function () { return idb(1); }).then(function (l) { return l.map(function (x) { return { name: x[0], version: x[1] }; }); }); });
  var all = { IDBRequest: IDBRequest, IDBOpenDBRequest: IDBOpenDBRequest, IDBDatabase: IDBDatabase, IDBTransaction: IDBTransaction, IDBObjectStore: IDBObjectStore, IDBIndex: IDBIndex, IDBCursor: IDBCursor, IDBFactory: IDBFactory, IDBKeyRange: IDBKeyRange };
  Object.keys(all).forEach(function (n) {
    Object.defineProperty(all[n].prototype, Symbol.toStringTag, { value: n, configurable: true });
    def(globalThis, n, all[n]);
  });
  def(globalThis, 'indexedDB', new IDBFactory());
})
