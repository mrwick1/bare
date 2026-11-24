// `caches` (Service Workers, "CacheStorage"), in memory only. Made at the first read of `caches`, `CacheStorage` or `Cache` (`browser_shims.rs`).
// An entry is keyed by the URL of its request. The options `ignoreSearch`, `ignoreVary` and `cacheName` are accepted and have no effect.
(function () {
  'use strict';
  var S = new WeakMap();
  function st(o) {
    var s = S.get(o);
    if (!s) throw new TypeError('Illegal invocation');
    return s;
  }
  function url(r) {
    return r instanceof Request ? r.url : String(r);
  }
  function Cache() { throw new TypeError('Illegal constructor'); }
  function make() {
    var c = Object.create(Cache.prototype);
    S.set(c, { map: new Map() });
    return c;
  }
  function put(c, req, resp) {
    var u = url(req);
    if (!(resp instanceof Response)) return Promise.reject(new TypeError("Failed to execute 'put' on 'Cache': parameter 2 is not of type 'Response'."));
    if (!/^https?:/i.test(u)) return Promise.reject(new TypeError("Failed to execute 'put' on 'Cache': Request scheme '" + u.split(':')[0] + "' is unsupported"));
    if (resp.status === 206) return Promise.reject(new TypeError("Failed to execute 'put' on 'Cache': Partial response (status code 206) is unsupported"));
    st(c).map.set(u, { req: req instanceof Request ? req : new Request(u), resp: resp.clone() });
    return Promise.resolve();
  }
  function add(c, req) {
    return fetch(req).then(function (r) {
      if (!r.ok) throw new TypeError("Failed to execute 'add' on 'Cache': Request failed");
      return put(c, req, r);
    });
  }
  function find(c, req) {
    var out = [];
    st(c).map.forEach(function (e, u) { if (req === undefined || u === url(req)) out.push(e); });
    return out;
  }
  var cacheMethods = {
    match: function (req, options) {
      try { var e = find(this, req)[0]; return Promise.resolve(e ? e.resp.clone() : undefined); } catch (x) { return Promise.reject(x); }
    },
    matchAll: function (req, options) {
      try { return Promise.resolve(find(this, req).map(function (e) { return e.resp.clone(); })); } catch (x) { return Promise.reject(x); }
    },
    add: function (req) {
      try { st(this); return add(this, req); } catch (x) { return Promise.reject(x); }
    },
    addAll: function (reqs) {
      try { st(this); return Promise.all(Array.from(reqs, function (r) { return add(this, r); }, this)).then(function () {}); } catch (x) { return Promise.reject(x); }
    },
    put: function (req, resp) {
      try { return put(this, req, resp); } catch (x) { return Promise.reject(x); }
    },
    delete: function (req, options) {
      try { return Promise.resolve(st(this).map.delete(url(req))); } catch (x) { return Promise.reject(x); }
    },
    keys: function (req, options) {
      try { return Promise.resolve(find(this, req).map(function (e) { return e.req; })); } catch (x) { return Promise.reject(x); }
    }
  };
  Object.keys(cacheMethods).forEach(function (k) {
    Object.defineProperty(Cache.prototype, k, { value: cacheMethods[k], writable: true, enumerable: true, configurable: true });
  });
  Object.defineProperty(Cache.prototype, Symbol.toStringTag, { value: 'Cache', configurable: true });

  function CacheStorage() { throw new TypeError('Illegal constructor'); }
  var storageMethods = {
    open: function (name) {
      try {
        var m = st(this).map, n = String(name);
        if (!m.has(n)) m.set(n, make());
        return Promise.resolve(m.get(n));
      } catch (x) { return Promise.reject(x); }
    },
    has: function (name) {
      try { return Promise.resolve(st(this).map.has(String(name))); } catch (x) { return Promise.reject(x); }
    },
    delete: function (name) {
      try { return Promise.resolve(st(this).map.delete(String(name))); } catch (x) { return Promise.reject(x); }
    },
    keys: function () {
      try { return Promise.resolve(Array.from(st(this).map.keys())); } catch (x) { return Promise.reject(x); }
    },
    match: function (req, options) {
      try {
        var out;
        st(this).map.forEach(function (c) { if (!out) out = find(c, req)[0]; });
        return Promise.resolve(out ? out.resp.clone() : undefined);
      } catch (x) { return Promise.reject(x); }
    }
  };
  Object.keys(storageMethods).forEach(function (k) {
    Object.defineProperty(CacheStorage.prototype, k, { value: storageMethods[k], writable: true, enumerable: true, configurable: true });
  });
  Object.defineProperty(CacheStorage.prototype, Symbol.toStringTag, { value: 'CacheStorage', configurable: true });

  var caches = Object.create(CacheStorage.prototype);
  S.set(caches, { map: new Map() });
  Object.defineProperty(globalThis, 'Cache', { value: Cache, writable: true, configurable: true });
  Object.defineProperty(globalThis, 'CacheStorage', { value: CacheStorage, writable: true, configurable: true });
  Object.defineProperty(globalThis, 'caches', { value: caches, writable: true, configurable: true });
})();
