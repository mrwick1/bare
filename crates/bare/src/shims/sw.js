// Service workers (`browser_sw.rs`, design S2 of `docs/internal/design-service-workers.md`). The objects of a realm live here; the
// registrations, the jobs and the running workers live in the session (`sw_hub.rs`). The source gives a function. The host calls it
// with `sw(op, ...)` (the native ops, numbered as `OP`) and `inWorker` (true in a service worker global). It gives the helper that the
// host calls: `sync(item)` in a page, `dispatch(id, kind, regSnap, ...)` in a service worker.
// A worker snapshot is `[id, scriptURL, state]`. A registration snapshot is `[id, scope, updateViaCache, installing, waiting, active]`.
(function (sw, inWorker) {
  "use strict";
  var OP = {
    client: 0,
    register: 1,
    regs: 2,
    unregister: 3,
    post: 4,
    update: 5,
    done: 10,
    skip: 11,
    claim: 12,
    clients: 13,
    clientPost: 14,
    trust: 16,
    focus: 17,
    show: 20,
    notes: 21,
    fetch: 30,
    net: 31,
    fetchReply: 32,
  };
  var UVC = ["imports", "all", "none"];
  var TOKEN = {};
  var S = new WeakMap();
  function st(o) {
    var s = S.get(o);
    if (!s) throw new TypeError("Illegal invocation");
    return s;
  }
  function tag(C, name) {
    Object.defineProperty(C.prototype, Symbol.toStringTag, {
      value: name,
      configurable: true,
    });
  }
  function define(name, value) {
    Object.defineProperty(self, name, {
      value: value,
      writable: true,
      enumerable: false,
      configurable: true,
    });
  }
  // An `on<type>` attribute of `proto`: a setter adds one listener that calls the current handler.
  function handler(proto, type) {
    var H = new WeakMap();
    Object.defineProperty(proto, "on" + type, {
      get: function () {
        var h = H.get(this);
        return h ? h.f : null;
      },
      set: function (f) {
        var h = H.get(this);
        if (typeof f !== "function" && (typeof f !== "object" || f === null))
          f = null;
        if (!h) {
          if (f === null) return;
          h = { f: f };
          H.set(this, h);
          this.addEventListener(type, function (e) {
            var g = h.f;
            if (typeof g === "function") return g.call(this, e);
          });
          return;
        }
        h.f = f;
      },
      enumerable: true,
      configurable: true,
    });
  }
  // Headers cross as `name:value` lines.
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
  function reject(name, message) {
    return Promise.reject(new DOMException(message, name));
  }

  // ---------------------------------------------------------------------------------------------- ServiceWorker

  class ServiceWorker extends EventTarget {
    constructor() {
      if (arguments[0] !== TOKEN) throw new TypeError("Illegal constructor");
      super();
    }
    get scriptURL() {
      return st(this).url;
    }
    get state() {
      return st(this).state;
    }
    postMessage(message, transfer) {
      var s = st(this);
      if (arguments.length < 1)
        throw new TypeError(
          "Failed to execute 'postMessage' on 'ServiceWorker': 1 argument required, but only 0 present.",
        );
      if (s.state === "redundant")
        throw new DOMException(
          "Failed to execute 'postMessage' on 'ServiceWorker': The ServiceWorker is in redundant state.",
          "InvalidStateError",
        );
      sw(OP.post, s.id, message, transfer);
    }
  }
  tag(ServiceWorker, "ServiceWorker");
  handler(ServiceWorker.prototype, "statechange");
  handler(ServiceWorker.prototype, "error");
  var workers = new Map();
  function worker(w) {
    if (!w) return null;
    var o = workers.get(w[0]);
    if (!o) {
      o = new ServiceWorker(TOKEN);
      S.set(o, { id: w[0], url: w[1], state: w[2] });
      workers.set(w[0], o);
    }
    return o;
  }
  // A worker of this realm moved to `state`: `statechange` fires at it. Ready resolves on the way.
  function setState(id, state) {
    var o = workers.get(id);
    if (!o) return;
    var s = S.get(o);
    if (s.state === state) return;
    s.state = state;
    if (state === "redundant") workers.delete(id);
    o.dispatchEvent(new Event("statechange"));
  }

  // ---------------------------------------------------------------------------------------------- ServiceWorkerRegistration

  class ServiceWorkerRegistration extends EventTarget {
    constructor() {
      if (arguments[0] !== TOKEN) throw new TypeError("Illegal constructor");
      super();
    }
    get installing() {
      return st(this).installing;
    }
    get waiting() {
      return st(this).waiting;
    }
    get active() {
      return st(this).active;
    }
    get scope() {
      return st(this).scope;
    }
    get updateViaCache() {
      return st(this).uvc;
    }
    get navigationPreload() {
      st(this);
      return preload;
    }
    update() {
      var s;
      try {
        s = st(this);
      } catch (e) {
        return Promise.reject(e);
      }
      if (!s.installing && !s.waiting && !s.active)
        return reject(
          "InvalidStateError",
          "Failed to update a ServiceWorker for scope ('" +
            s.scope +
            "'): The registration has no worker.",
        );
      if (inWorker && s.installing === me)
        return reject(
          "InvalidStateError",
          "Failed to update a ServiceWorker for scope ('" +
            s.scope +
            "'): The worker is installing.",
        );
      return job(sw(OP.update, s.id));
    }
    unregister() {
      var s;
      try {
        s = st(this);
      } catch (e) {
        return Promise.reject(e);
      }
      return Promise.resolve(sw(OP.unregister, s.id));
    }
    showNotification(title, options) {
      try {
        var s = st(this);
        if (arguments.length < 1)
          throw new TypeError(
            "Failed to execute 'showNotification' on 'ServiceWorkerRegistration': 1 argument required, but only 0 present.",
          );
        sw(OP.show, s.id, title, options);
        return Promise.resolve();
      } catch (e) {
        return Promise.reject(e);
      }
    }
    getNotifications(filter) {
      try {
        var s = st(this);
        var t = "";
        if (filter !== undefined && filter !== null) {
          if (typeof filter !== "object" && typeof filter !== "function")
            throw new TypeError(
              "Failed to execute 'getNotifications' on 'ServiceWorkerRegistration': The provided value is not of type 'GetNotificationOptions'.",
            );
          if (filter.tag !== undefined) t = String(filter.tag);
        }
        return Promise.resolve(sw(OP.notes, s.id, t));
      } catch (e) {
        return Promise.reject(e);
      }
    }
  }
  tag(ServiceWorkerRegistration, "ServiceWorkerRegistration");
  handler(ServiceWorkerRegistration.prototype, "updatefound");
  // `navigationPreload` is a manager that never preloads.
  var preload = {
    enable: function () {
      return Promise.resolve();
    },
    disable: function () {
      return Promise.resolve();
    },
    setHeaderValue: function () {
      return Promise.resolve();
    },
    getState: function () {
      return Promise.resolve({ enabled: false, headerValue: "true" });
    },
  };
  var regs = new Map();
  function registration(r) {
    var o = regs.get(r[0]);
    if (!o) {
      o = new ServiceWorkerRegistration(TOKEN);
      S.set(o, {
        id: r[0],
        scope: r[1],
        uvc: UVC[r[2]],
        installing: null,
        waiting: null,
        active: null,
      });
      regs.set(r[0], o);
    }
    var s = S.get(o);
    s.uvc = UVC[r[2]];
    s.installing = worker(r[3]);
    s.waiting = worker(r[4]);
    s.active = worker(r[5]);
    return o;
  }

  // ---------------------------------------------------------------------------------------------- jobs

  var jobs = new Map();
  // A promise of the job `id` (a number), or of the value that the op gave.
  function job(id) {
    if (typeof id !== "number") return Promise.resolve(id);
    return new Promise(function (resolve, rejectIt) {
      jobs.set(id, { resolve: resolve, reject: rejectIt });
    });
  }
  function settle(id, snap, name, message) {
    var j = jobs.get(id);
    if (!j) return;
    jobs.delete(id);
    if (snap) j.resolve(registration(snap));
    else if (name === "TypeError") j.reject(new TypeError(message));
    else j.reject(new DOMException(message, name));
  }

  // ---------------------------------------------------------------------------------------------- the page side

  function page() {
    var ready = {};
    ready.p = new Promise(function (r) {
      ready.resolve = r;
    });
    var first = sw(OP.client);
    var ctrl = worker(first);
    function readyCheck(o) {
      if (!ready.done && S.get(o).active && sw(OP.regs, 1) === S.get(o).id) {
        ready.done = true;
        ready.resolve(o);
      }
    }
    class ServiceWorkerContainer extends EventTarget {
      constructor() {
        if (arguments[0] !== TOKEN)
          throw new TypeError("Illegal constructor");
        super();
      }
      get controller() {
        st(this);
        return ctrl;
      }
      get ready() {
        st(this);
        return ready.p;
      }
      register(url, options) {
        try {
          st(this);
          if (arguments.length < 1)
            throw new TypeError(
              "Failed to execute 'register' on 'ServiceWorkerContainer': 1 argument required, but only 0 present.",
            );
          url = String(url);
          var scope,
            uvc = 0,
            type = 0;
          if (options !== undefined && options !== null) {
            if (typeof options !== "object" && typeof options !== "function")
              throw new TypeError(
                "Failed to execute 'register' on 'ServiceWorkerContainer': The provided value is not of type 'RegistrationOptions'.",
              );
            if (options.scope !== undefined) scope = String(options.scope);
            if (options.type !== undefined) {
              type = ["classic", "module"].indexOf(String(options.type));
              if (type < 0)
                throw new TypeError(
                  "Failed to execute 'register' on 'ServiceWorkerContainer': Failed to read the 'type' property from 'RegistrationOptions': The provided value '" +
                    String(options.type) +
                    "' is not a valid enum value of type WorkerType.",
                );
            }
            if (options.updateViaCache !== undefined) {
              uvc = UVC.indexOf(String(options.updateViaCache));
              if (uvc < 0)
                throw new TypeError(
                  "Failed to execute 'register' on 'ServiceWorkerContainer': Failed to read the 'updateViaCache' property from 'RegistrationOptions': The provided value '" +
                    String(options.updateViaCache) +
                    "' is not a valid enum value of type ServiceWorkerUpdateViaCache.",
                );
            }
          }
          return job(sw(OP.register, url, scope, uvc, type));
        } catch (e) {
          return Promise.reject(e);
        }
      }
      getRegistration(url) {
        try {
          st(this);
          var r = sw(OP.regs, 2, url === undefined ? "" : String(url));
          return Promise.resolve(r ? registration(r) : undefined);
        } catch (e) {
          return Promise.reject(e);
        }
      }
      getRegistrations() {
        try {
          st(this);
          return Promise.resolve(sw(OP.regs, 0).map(registration));
        } catch (e) {
          return Promise.reject(e);
        }
      }
      startMessages() {
        st(this);
      }
    }
    tag(ServiceWorkerContainer, "ServiceWorkerContainer");
    handler(ServiceWorkerContainer.prototype, "controllerchange");
    handler(ServiceWorkerContainer.prototype, "message");
    handler(ServiceWorkerContainer.prototype, "messageerror");
    var container = new ServiceWorkerContainer(TOKEN);
    S.set(container, {});
    define("ServiceWorkerContainer", ServiceWorkerContainer);
    define("ServiceWorkerRegistration", ServiceWorkerRegistration);
    define("ServiceWorker", ServiceWorker);
    // A page loaded under an active worker: its registration is ready.
    if (ctrl)
      sw(OP.regs, 0).forEach(function (r) {
        readyCheck(registration(r));
      });
    // A `fetch()` of a controlled page: the worker gets a `fetch` event. With no reply it goes to the network.
    var pend = new Map();
    function pageFetch(input, init) {
      var r;
      try {
        r = new Request(input, init);
      } catch (e) {
        return Promise.reject(e);
      }
      var get = r.method === "GET" || r.method === "HEAD";
      return (get ? Promise.resolve(null) : r.arrayBuffer()).then(function (b) {
        b = b && new Uint8Array(b);
        var again = {
          method: r.method,
          headers: r.headers,
          body: b,
          mode: r.mode,
          credentials: r.credentials,
          redirect: r.redirect,
          signal: r.signal,
        };
        var id = sw(OP.fetch, r.url, r.method, lines(r.headers), b, r.mode, r.credentials, r.redirect);
        if (!id) return sw(OP.net, r.url, again);
        return new Promise(function (resolve, rejectIt) {
          pend.set(id, { resolve: resolve, reject: rejectIt, url: r.url, init: again });
        });
      });
    }
    function fetched(id, kind, status, text, h, body, url) {
      var p = pend.get(id);
      if (!p) return;
      pend.delete(id);
      if (kind === "fallback") p.resolve(sw(OP.net, p.url, p.init));
      else if (kind === "error") p.reject(new TypeError("Failed to fetch"));
      else {
        var ok = status >= 200 && status <= 599 ? status : 200;
        var empty = ok === 204 || ok === 205 || ok === 304;
        var resp = new Response(empty ? null : body, { status: ok, statusText: text, headers: headers(h) });
        // A response with no URL takes the request URL (Fetch, "main fetch").
        Object.defineProperty(resp, "url", { value: url || p.url, configurable: true });
        p.resolve(resp);
      }
    }
    return {
      container: container,
      fetch: pageFetch,
      // One change from the session. The host runs the microtasks after each one.
      sync: function (kind, a, b, c, d) {
        if (kind === "job") settle(a, b, c, d);
        else if (kind === "reg") {
          var o = registration(a);
          if (b) o.dispatchEvent(new Event("updatefound"));
          readyCheck(o);
        } else if (kind === "state") setState(a, b);
        else if (kind === "ctrl") {
          ctrl = worker(a);
          container.dispatchEvent(new Event("controllerchange"));
        } else if (kind === "msg") {
          return [container, worker(a)];
        } else if (kind === "fetched") fetched(a, b, c, d, arguments[5], arguments[6], arguments[7]);
      },
    };
  }

  // ---------------------------------------------------------------------------------------------- the worker side

  var me = null;
  function workerSide() {
    var E = new WeakMap();
    class ExtendableEvent extends Event {
      constructor(type, init) {
        super(type, init);
        E.set(this, { pending: 0, active: false, ok: true });
      }
      waitUntil(p) {
        var s = E.get(this);
        if (!s) throw new TypeError("Illegal invocation");
        if (!this.isTrusted)
          throw new DOMException(
            "Failed to execute 'waitUntil' on 'ExtendableEvent': The event handler is null.",
            "InvalidStateError",
          );
        if (!s.active && s.pending === 0)
          throw new DOMException(
            "Failed to execute 'waitUntil' on 'ExtendableEvent': The event handler is already finished and no extend lifetime promises are outstanding.",
            "InvalidStateError",
          );
        s.pending++;
        Promise.resolve(p).then(
          function () {
            end(s);
          },
          function () {
            s.ok = false;
            end(s);
          },
        );
      }
    }
    function end(s) {
      if (--s.pending === 0 && !s.active && s.id !== undefined) finish(s);
    }
    // The event ended: tell the host. A `notificationclick` lets a window take the focus until then.
    function finish(s) {
      if (s.click) {
        s.click = false;
        interact--;
      }
      sw(OP.done, s.id, s.ok);
    }
    var interact = 0;
    class ExtendableMessageEvent extends ExtendableEvent {
      constructor(type, init) {
        super(type, init);
        init = init || {};
        var ports = init.ports === undefined ? [] : Array.from(init.ports);
        Object.freeze(ports);
        Object.defineProperty(this, "data", {
          value: init.data === undefined ? null : init.data,
          enumerable: true,
        });
        Object.defineProperty(this, "origin", {
          value: init.origin === undefined ? "" : String(init.origin),
          enumerable: true,
        });
        Object.defineProperty(this, "lastEventId", {
          value:
            init.lastEventId === undefined ? "" : String(init.lastEventId),
          enumerable: true,
        });
        Object.defineProperty(this, "source", {
          value: init.source === undefined ? null : init.source,
          enumerable: true,
        });
        Object.defineProperty(this, "ports", {
          value: ports,
          enumerable: true,
        });
      }
    }
    var N = new WeakMap();
    class NotificationEvent extends ExtendableEvent {
      constructor(type, init) {
        if (arguments.length < 2)
          throw new TypeError(
            "Failed to construct 'NotificationEvent': 2 arguments required, but only " +
              arguments.length +
              " present.",
          );
        if (!(init && init.notification instanceof Notification))
          throw new TypeError(
            "Failed to construct 'NotificationEvent': Failed to read the 'notification' property from 'NotificationEventInit': Required member is undefined.",
          );
        super(type, init);
        N.set(this, {
          n: init.notification,
          a: init.action === undefined ? "" : String(init.action),
        });
      }
      get notification() {
        var s = N.get(this);
        if (!s) throw new TypeError("Illegal invocation");
        return s.n;
      }
      get action() {
        var s = N.get(this);
        if (!s) throw new TypeError("Illegal invocation");
        return s.a;
      }
    }
    tag(ExtendableEvent, "ExtendableEvent");
    tag(ExtendableMessageEvent, "ExtendableMessageEvent");
    tag(NotificationEvent, "NotificationEvent");
    var F = new WeakMap();
    class FetchEvent extends ExtendableEvent {
      constructor(type, init) {
        if (!(init && init.request instanceof Request))
          throw new TypeError(
            "Failed to construct 'FetchEvent': Failed to read the 'request' property from 'FetchEventInit': Required member is undefined.",
          );
        super(type, init);
        var done;
        F.set(this, {
          request: init.request,
          clientId: init.clientId === undefined ? "" : String(init.clientId),
          resultingClientId: init.resultingClientId === undefined ? "" : String(init.resultingClientId),
          handled: new Promise(function (r) {
            done = r;
          }),
          done: done,
          responded: false,
        });
      }
      get request() {
        return fs(this).request;
      }
      get clientId() {
        return fs(this).clientId;
      }
      get resultingClientId() {
        return fs(this).resultingClientId;
      }
      get replacesClientId() {
        fs(this);
        return "";
      }
      get handled() {
        return fs(this).handled;
      }
      get preloadResponse() {
        fs(this);
        return Promise.resolve(undefined);
      }
      respondWith(p) {
        var f = fs(this);
        var s = E.get(this);
        if (f.responded)
          throw new DOMException(
            "Failed to execute 'respondWith' on 'FetchEvent': The event has already been responded to.",
            "InvalidStateError",
          );
        if (!s.active)
          throw new DOMException(
            "Failed to execute 'respondWith' on 'FetchEvent': The event handler is already finished.",
            "InvalidStateError",
          );
        f.responded = true;
        this.stopImmediatePropagation();
        var id = f.id;
        this.waitUntil(
          Promise.resolve(p)
            .then(function (r) {
              if (!(r instanceof Response)) throw new TypeError("not a Response");
              return r.arrayBuffer().then(function (b) {
                sw(OP.fetchReply, id, 2, r.status, r.statusText, lines(r.headers), new Uint8Array(b), r.url);
              });
            })
            .then(
              function () {
                f.done();
              },
              function () {
                sw(OP.fetchReply, id, 1);
                f.done();
              },
            ),
        );
      }
    }
    function fs(o) {
      var f = F.get(o);
      if (!f) throw new TypeError("Illegal invocation");
      return f;
    }
    tag(FetchEvent, "FetchEvent");

    class Client {
      constructor() {
        throw new TypeError("Illegal constructor");
      }
      get id() {
        return st(this).id;
      }
      get url() {
        return st(this).url;
      }
      get type() {
        return st(this).type;
      }
      get frameType() {
        return st(this).frame;
      }
      postMessage(message, transfer) {
        var s = st(this);
        if (arguments.length < 1)
          throw new TypeError(
            "Failed to execute 'postMessage' on 'Client': 1 argument required, but only 0 present.",
          );
        sw(OP.clientPost, s.id, message, transfer);
      }
    }
    class WindowClient extends Client {
      get visibilityState() {
        return st(this).visibility;
      }
      get focused() {
        return st(this).focused;
      }
      get ancestorOrigins() {
        st(this);
        return Object.freeze([]);
      }
      focus() {
        var s;
        try {
          s = st(this);
        } catch (e) {
          return Promise.reject(e);
        }
        if (interact < 1 || !sw(OP.focus, s.id))
          return reject(
            "InvalidAccessError",
            "Failed to execute 'focus' on 'WindowClient': Not allowed to focus a window.",
          );
        s.focused = true;
        s.visibility = "visible";
        return Promise.resolve(this);
      }
      navigate() {
        try {
          st(this);
        } catch (e) {
          return Promise.reject(e);
        }
        return Promise.reject(
          new TypeError(
            "Failed to execute 'navigate' on 'WindowClient': Cannot navigate.",
          ),
        );
      }
    }
    tag(Client, "Client");
    tag(WindowClient, "WindowClient");
    // `[uuid, url, type, frameType, visibilityState, focused]`.
    function client(c) {
      var o = Object.create(
        (c[2] === "window" ? WindowClient : Client).prototype,
      );
      S.set(o, {
        id: c[0],
        url: c[1],
        type: c[2],
        frame: c[3],
        visibility: c[4],
        focused: c[5],
      });
      return o;
    }
    class Clients {
      constructor() {
        throw new TypeError("Illegal constructor");
      }
      get(id) {
        try {
          st(this);
        } catch (e) {
          return Promise.reject(e);
        }
        var c = sw(OP.clients, true, "all").find(function (c) {
          return c[0] === String(id);
        });
        return Promise.resolve(c ? client(c) : undefined);
      }
      matchAll(options) {
        try {
          st(this);
        } catch (e) {
          return Promise.reject(e);
        }
        options = options || {};
        var type =
          options.type === undefined ? "window" : String(options.type);
        if (["window", "worker", "sharedworker", "all"].indexOf(type) < 0)
          return Promise.reject(
            new TypeError(
              "Failed to execute 'matchAll' on 'Clients': Failed to read the 'type' property from 'ClientQueryOptions': The provided value '" +
                type +
                "' is not a valid enum value of type ClientType.",
            ),
          );
        return Promise.resolve(
          sw(OP.clients, !!options.includeUncontrolled, type).map(client),
        );
      }
      openWindow() {
        try {
          st(this);
        } catch (e) {
          return Promise.reject(e);
        }
        return reject(
          "InvalidAccessError",
          "Failed to execute 'openWindow' on 'Clients': Not allowed to open a window.",
        );
      }
      claim() {
        try {
          st(this);
        } catch (e) {
          return Promise.reject(e);
        }
        if (!sw(OP.claim))
          return reject(
            "InvalidStateError",
            "Failed to execute 'claim' on 'Clients': Only the active worker can claim clients.",
          );
        return Promise.resolve();
      }
    }
    tag(Clients, "Clients");
    var clients = Object.create(Clients.prototype);
    S.set(clients, {});
    var reg = null;
    Object.defineProperty(self, "registration", {
      get: function () {
        return reg;
      },
      enumerable: true,
      configurable: true,
    });
    Object.defineProperty(self, "clients", {
      get: function () {
        return clients;
      },
      enumerable: true,
      configurable: true,
    });
    Object.defineProperty(self, "serviceWorker", {
      get: function () {
        return me;
      },
      enumerable: true,
      configurable: true,
    });
    define("skipWaiting", function skipWaiting() {
      sw(OP.skip);
      return Promise.resolve();
    });
    var proto = Object.getPrototypeOf(self);
    [
      "install",
      "activate",
      "fetch",
      "message",
      "messageerror",
      "notificationclick",
      "notificationclose",
      "push",
      "pushsubscriptionchange",
      "sync",
      "periodicsync",
      "backgroundfetchsuccess",
      "backgroundfetchfail",
      "backgroundfetchabort",
      "backgroundfetchclick",
      "paymentrequest",
      "canmakepayment",
      "contentdelete",
    ].forEach(function (t) {
      handler(proto, t);
    });
    define("ServiceWorker", ServiceWorker);
    define("ServiceWorkerRegistration", ServiceWorkerRegistration);
    define("ExtendableEvent", ExtendableEvent);
    define("ExtendableMessageEvent", ExtendableMessageEvent);
    define("NotificationEvent", NotificationEvent);
    define("FetchEvent", FetchEvent);
    define("Client", Client);
    define("WindowClient", WindowClient);
    define("Clients", Clients);
    // Fire `ev` as a trusted event; `id` gets its result when its `waitUntil` promises settle.
    function fire(id, ev) {
      var s = E.get(ev);
      s.id = id;
      s.active = true;
      if (ev.type === "notificationclick") {
        s.click = true;
        interact++;
      }
      sw(OP.trust, ev);
      self.dispatchEvent(ev);
      s.active = false;
      if (s.pending === 0) finish(s);
    }
    return {
      // `r`: the registration snapshot, `w`: this worker. The rest depends on `kind`.
      dispatch: function (id, kind, r, w, a, b, c) {
        me = worker(w);
        reg = registration(r);
        if (kind === "install" || kind === "activate")
          fire(id, new ExtendableEvent(kind));
        else if (kind === "message")
          fire(
            id,
            new ExtendableMessageEvent("message", {
              data: a,
              ports: b,
              source: client(c),
              origin: new URL(c[1]).origin,
            }),
          );
        else if (kind === "fetch") {
          var req;
          try {
            req = new Request(a[0], {
              method: a[1],
              headers: headers(a[2]),
              body: a[3],
              mode: a[4] === "navigate" ? "same-origin" : a[4],
              credentials: a[5],
              redirect: a[6],
            });
          } catch (e) {
            // The event ends with no reply: the page goes to the network.
            sw(OP.done, id, true);
            return;
          }
          // A navigation: its request has the mode "navigate", which `new Request` refuses.
          if (a[4] === "navigate") Object.defineProperty(req, "mode", { value: "navigate", configurable: true });
          var ev = new FetchEvent("fetch", { request: req, clientId: b });
          F.get(ev).id = a[7];
          fire(id, ev);
          // No `respondWith`: the page goes to the network.
          if (!F.get(ev).responded) sw(OP.fetchReply, a[7], 0);
        } else if (kind === "notificationclick" || kind === "notificationclose")
          fire(id, new NotificationEvent(kind, { notification: a, action: b }));
        else if (kind === "state") setState(a, b);
      },
    };
  }

  return inWorker ? workerSide() : page();
});
