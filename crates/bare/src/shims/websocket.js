// `WebSocket` and `CloseEvent` (WebSockets Standard). Made at the first read of either name (`browser_shims.rs`).
// This browser has no WebSocket protocol, so every connection fails, as against a server that refuses it: in a later task the socket
// becomes CLOSED and fires `error`, then `close` with code 1006. The arguments are checked as Chrome checks them, so a page that opens
// a socket runs on and takes its own fallback.
(function () {
  'use strict';
  var S = new WeakMap();
  var TYPES = ['open', 'message', 'error', 'close'];
  function st(o) {
    var s = S.get(o);
    if (!s) throw new TypeError('Illegal invocation');
    return s;
  }
  class CloseEvent extends Event {
    constructor(type, init) {
      super(type, init);
      init = init || {};
      Object.defineProperty(this, 'code', { value: init.code | 0, enumerable: true });
      Object.defineProperty(this, 'reason', { value: init.reason === undefined ? '' : String(init.reason), enumerable: true });
      Object.defineProperty(this, 'wasClean', { value: !!init.wasClean, enumerable: true });
    }
  }
  function fail(o, s) {
    if (s.rt === 3) return;
    s.rt = 3;
    o.dispatchEvent(new Event('error'));
    o.dispatchEvent(new CloseEvent('close', { code: 1006, reason: '', wasClean: false }));
  }
  class WebSocket extends EventTarget {
    constructor(url) {
      if (arguments.length < 1) throw new TypeError("Failed to construct 'WebSocket': 1 argument required, but only 0 present.");
      super();
      var u;
      try { u = new URL(String(url), document.baseURI); } catch (e) { u = null; }
      if (u && (u.protocol === 'http:' || u.protocol === 'https:')) u = new URL('ws' + u.href.slice(4));
      var bad = !u ? 'is invalid' : u.protocol !== 'ws:' && u.protocol !== 'wss:' ? "has a scheme that is not 'ws', 'wss', 'http' or 'https'" : u.hash ? 'contains a fragment identifier' : '';
      if (bad) throw new DOMException("Failed to construct 'WebSocket': The URL '" + url + "' " + bad + '.', 'SyntaxError');
      var s = { url: u.href, rt: 0, bt: 'blob', on: {} }, o = this;
      S.set(this, s);
      console.error("WebSocket connection to '" + s.url + "' failed: this browser has no WebSocket support.");
      setTimeout(function () { fail(o, s); }, 0);
    }
    get url() { return st(this).url; }
    get readyState() { return st(this).rt; }
    get bufferedAmount() { st(this); return 0; }
    get extensions() { st(this); return ''; }
    get protocol() { st(this); return ''; }
    get binaryType() { return st(this).bt; }
    set binaryType(v) { if (v === 'blob' || v === 'arraybuffer') st(this).bt = v; }
    send() {
      if (st(this).rt === 0) throw new DOMException("Failed to execute 'send' on 'WebSocket': Still in CONNECTING state.", 'InvalidStateError');
    }
    close(code) {
      var s = st(this);
      if (code !== undefined && code !== 1000 && !(code >= 3000 && code <= 4999)) {
        throw new DOMException("Failed to execute 'close' on 'WebSocket': The close code must be either 1000, or between 3000 and 4999. " + code + ' is neither.', 'InvalidAccessError');
      }
      if (s.rt < 2) s.rt = 2;
    }
  }
  TYPES.forEach(function (t) {
    Object.defineProperty(WebSocket.prototype, 'on' + t, {
      get: function () { return st(this).on[t] || null; },
      set: function (f) {
        var s = st(this), old = s.on[t];
        if (old) this.removeEventListener(t, old);
        s.on[t] = typeof f === 'function' ? f : null;
        if (s.on[t]) this.addEventListener(t, f);
      },
      enumerable: true, configurable: true
    });
  });
  [['CONNECTING', 0], ['OPEN', 1], ['CLOSING', 2], ['CLOSED', 3]].forEach(function (c) {
    Object.defineProperty(WebSocket, c[0], { value: c[1], enumerable: true });
    Object.defineProperty(WebSocket.prototype, c[0], { value: c[1], enumerable: true });
  });
  Object.defineProperty(WebSocket.prototype, Symbol.toStringTag, { value: 'WebSocket', configurable: true });
  Object.defineProperty(CloseEvent.prototype, Symbol.toStringTag, { value: 'CloseEvent', configurable: true });
  Object.defineProperty(globalThis, 'CloseEvent', { value: CloseEvent, writable: true, configurable: true });
  Object.defineProperty(globalThis, 'WebSocket', { value: WebSocket, writable: true, configurable: true });
})();
