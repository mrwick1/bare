// `caches` (Service Workers, "CacheStorage" and "Cache"). Made at the first read of `caches`, `CacheStorage` or `Cache`
// (`browser_shims.rs`). The entries live in the store of the session (`cache_store.rs`), shared by the pages and workers of an
// origin and kept in the profile. The source gives the install function; it takes `cs(op, ...)` (`browser_caches.rs`).
// A request crosses as `url, method, header lines`; a response as `status, statusText, header lines, bytes, url`.
(function (cs) {
  "use strict";
  var OP = {
    open: 0,
    has: 1,
    del: 2,
    names: 3,
    put: 4,
    match: 5,
    delEntries: 6,
  };
  var S = new WeakMap();
  function st(o) {
    var s = S.get(o);
    if (!s) throw new TypeError("Illegal invocation");
    return s;
  }
  function lines(h) {
    var out = [];
    h.forEach(function (v, k) {
      out.push(k + ":" + v);
    });
    return out.join("\n");
  }
  function headers(t) {
    var h = new Headers();
    if (t)
      t.split("\n").forEach(function (l) {
        var i = l.indexOf(":");
        h.append(l.slice(0, i), l.slice(i + 1));
      });
    return h;
  }
  // A request or a URL: `{url, method, headers}`. A relative URL resolves against the base URL.
  function req(r) {
    if (!(r instanceof Request)) r = new Request(String(r));
    return { url: r.url, method: r.method, headers: lines(r.headers) };
  }
  function opts(o) {
    o = o || {};
    return [!!o.ignoreSearch, !!o.ignoreMethod, !!o.ignoreVary];
  }
  function found(name, r, o, first) {
    var q =
        r === undefined ? { url: null, method: "GET", headers: "" } : req(r),
      p = opts(o);
    return cs(
      OP.match,
      name,
      q.url,
      q.method,
      q.headers,
      p[0],
      p[1],
      p[2],
      first,
    );
  }
  // A new `Response` for a stored entry. A status that `Response` refuses comes back as 200.
  function response(e) {
    var status = e[0] >= 200 && e[0] <= 599 ? e[0] : 200;
    var empty = status === 204 || status === 205 || status === 304;
    var r = new Response(empty ? null : e[3], {
      status: status,
      statusText: e[1],
      headers: headers(e[2]),
    });
    if (e[4])
      Object.defineProperty(r, "url", { value: e[4], configurable: true });
    return r;
  }
  function request(e) {
    return new Request(e[5], { headers: headers(e[6]) });
  }
  function fail(m) {
    return Promise.reject(
      new TypeError("Failed to execute 'put' on 'Cache': " + m),
    );
  }
  function put(c, r, resp) {
    var q = req(r);
    if (!/^https?:/i.test(q.url))
      return fail(
        "Request scheme '" + q.url.split(":")[0] + "' is unsupported",
      );
    if (q.method !== "GET")
      return fail("Request method '" + q.method + "' is unsupported");
    if (!(resp instanceof Response))
      return fail("parameter 2 is not of type 'Response'.");
    if (resp.status === 206)
      return fail("Partial response (status code 206) is unsupported");
    var vary = resp.headers.get("vary");
    if (
      vary &&
      vary.split(",").some(function (v) {
        return v.trim() === "*";
      })
    )
      return fail("Vary header contains *");
    if (resp.bodyUsed) return fail("Response body is already used");
    return resp.arrayBuffer().then(function (b) {
      cs(
        OP.put,
        c.name,
        q.url,
        q.method,
        q.headers,
        resp.status,
        resp.statusText,
        lines(resp.headers),
        new Uint8Array(b),
        resp.url,
      );
    });
  }
  function add(c, r) {
    return fetch(r).then(function (resp) {
      if (!resp.ok)
        throw new TypeError(
          "Failed to execute 'add' on 'Cache': Request failed",
        );
      return put(c, r, resp);
    });
  }

  function Cache() {
    throw new TypeError("Illegal constructor");
  }
  function make(name) {
    var c = Object.create(Cache.prototype);
    S.set(c, { name: name });
    return c;
  }
  var cacheMethods = {
    match: function (r, o) {
      try {
        var l = found(st(this).name, r, o, true);
        return Promise.resolve(l.length ? response(l[0]) : undefined);
      } catch (x) {
        return Promise.reject(x);
      }
    },
    matchAll: function (r, o) {
      try {
        return Promise.resolve(found(st(this).name, r, o, false).map(response));
      } catch (x) {
        return Promise.reject(x);
      }
    },
    add: function (r) {
      try {
        return add(st(this), r);
      } catch (x) {
        return Promise.reject(x);
      }
    },
    addAll: function (rs) {
      try {
        var c = st(this);
        return Promise.all(
          Array.from(rs, function (r) {
            return add(c, r);
          }),
        ).then(function () {});
      } catch (x) {
        return Promise.reject(x);
      }
    },
    put: function (r, resp) {
      try {
        return put(st(this), r, resp);
      } catch (x) {
        return Promise.reject(x);
      }
    },
    delete: function (r, o) {
      try {
        var q = req(r),
          p = opts(o);
        return Promise.resolve(
          cs(
            OP.delEntries,
            st(this).name,
            q.url,
            q.method,
            q.headers,
            p[0],
            p[1],
            p[2],
          ),
        );
      } catch (x) {
        return Promise.reject(x);
      }
    },
    keys: function (r, o) {
      try {
        return Promise.resolve(found(st(this).name, r, o, false).map(request));
      } catch (x) {
        return Promise.reject(x);
      }
    },
  };
  Object.keys(cacheMethods).forEach(function (k) {
    Object.defineProperty(Cache.prototype, k, {
      value: cacheMethods[k],
      writable: true,
      enumerable: true,
      configurable: true,
    });
  });
  Object.defineProperty(Cache.prototype, Symbol.toStringTag, {
    value: "Cache",
    configurable: true,
  });

  function CacheStorage() {
    throw new TypeError("Illegal constructor");
  }
  var storageMethods = {
    open: function (name) {
      try {
        st(this);
        var n = String(name);
        cs(OP.open, n);
        return Promise.resolve(make(n));
      } catch (x) {
        return Promise.reject(x);
      }
    },
    has: function (name) {
      try {
        st(this);
        return Promise.resolve(cs(OP.has, String(name)));
      } catch (x) {
        return Promise.reject(x);
      }
    },
    delete: function (name) {
      try {
        st(this);
        return Promise.resolve(cs(OP.del, String(name)));
      } catch (x) {
        return Promise.reject(x);
      }
    },
    keys: function () {
      try {
        st(this);
        return Promise.resolve(cs(OP.names));
      } catch (x) {
        return Promise.reject(x);
      }
    },
    match: function (r, o) {
      try {
        st(this);
        var name = null;
        if (o && o.cacheName !== undefined) {
          name = String(o.cacheName);
          if (!cs(OP.has, name)) return Promise.resolve(undefined);
        }
        var l = found(name, r, o, true);
        return Promise.resolve(l.length ? response(l[0]) : undefined);
      } catch (x) {
        return Promise.reject(x);
      }
    },
  };
  Object.keys(storageMethods).forEach(function (k) {
    Object.defineProperty(CacheStorage.prototype, k, {
      value: storageMethods[k],
      writable: true,
      enumerable: true,
      configurable: true,
    });
  });
  Object.defineProperty(CacheStorage.prototype, Symbol.toStringTag, {
    value: "CacheStorage",
    configurable: true,
  });

  var caches = Object.create(CacheStorage.prototype);
  S.set(caches, {});
  Object.defineProperty(globalThis, "Cache", {
    value: Cache,
    writable: true,
    configurable: true,
  });
  Object.defineProperty(globalThis, "CacheStorage", {
    value: CacheStorage,
    writable: true,
    configurable: true,
  });
  Object.defineProperty(globalThis, "caches", {
    value: caches,
    writable: true,
    configurable: true,
  });
});
