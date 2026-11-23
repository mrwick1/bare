// `WebSocket` and `CloseEvent` (WebSockets Standard). Made at the first read of either name (`browser_shims.rs`). The source gives a
// function; the host calls it with `ws(op, ...)`, the network side (`browser_websocket.rs`). The host calls back `cb(kind, ...)` of a
// socket in a task for each event: 0 open, 1 text, 2 binary (an ArrayBuffer), 3 close, 4 failed. The arguments are checked as
// Chrome checks them.
(function () {
  'use strict';
  return function (ws) {
    var S = new WeakMap();
    var TYPES = ['open', 'message', 'error', 'close'];
    var TOKEN = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;
    function st(o) {
      var s = S.get(o);
      if (!s) throw new TypeError('Illegal invocation');
      return s;
    }
    function utf8(t) { return new TextEncoder().encode(t).length; }
    class CloseEvent extends Event {
      constructor(type, init) {
        super(type, init);
        init = init || {};
        Object.defineProperty(this, 'code', { value: init.code | 0, enumerable: true });
        Object.defineProperty(this, 'reason', { value: init.reason === undefined ? '' : String(init.reason), enumerable: true });
        Object.defineProperty(this, 'wasClean', { value: !!init.wasClean, enumerable: true });
      }
    }
    function closed(o, s, code, reason, clean, error) {
      if (s.rt === 3) return;
      s.rt = 3;
      if (error) o.dispatchEvent(new Event('error'));
      o.dispatchEvent(new CloseEvent('close', { code: code, reason: reason, wasClean: clean }));
    }
    function on(o, s) {
      return function (kind, a, b, c) {
        if (kind === 0) {
          if (s.rt !== 0) return;
          s.rt = 1;
          s.proto = a;
          s.ext = b;
          o.dispatchEvent(new Event('open'));
        } else if (kind === 1 || kind === 2) {
          if (s.rt !== 1) return;
          // `MessageEvent` has no constructor here: an `Event` with its fields, as `shims/eventsource.js` does.
          var e = new Event('message');
          var data = kind === 1 ? a : s.bt === 'blob' ? new Blob([a]) : a;
          [['data', data], ['origin', s.origin], ['lastEventId', ''], ['source', null], ['ports', Object.freeze([])]].forEach(function (f) {
            Object.defineProperty(e, f[0], { value: f[1], enumerable: true, configurable: true });
          });
          o.dispatchEvent(e);
        } else if (kind === 3) {
          closed(o, s, a, b, c, false);
        } else {
          closed(o, s, 1006, '', false, true);
        }
      };
    }
    class WebSocket extends EventTarget {
      constructor(url, protocols) {
        if (arguments.length < 1) throw new TypeError("Failed to construct 'WebSocket': 1 argument required, but only 0 present.");
        super();
        var u;
        try { u = new URL(String(url), document.baseURI); } catch (e) { u = null; }
        if (u && (u.protocol === 'http:' || u.protocol === 'https:')) u = new URL('ws' + u.href.slice(4));
        var bad = !u ? 'is invalid' : u.protocol !== 'ws:' && u.protocol !== 'wss:' ? "has a scheme that is not 'ws', 'wss', 'http' or 'https'" : u.hash ? 'contains a fragment identifier' : '';
        if (bad) throw new DOMException("Failed to construct 'WebSocket': The URL '" + url + "' " + bad + '.', 'SyntaxError');
        var list = protocols === undefined ? [] : typeof protocols === 'string' ? [protocols] : Array.from(protocols, String);
        list.forEach(function (p, i) {
          if (!TOKEN.test(p)) throw new DOMException("Failed to construct 'WebSocket': The subprotocol '" + p + "' is invalid.", 'SyntaxError');
          if (list.indexOf(p) !== i) throw new DOMException("Failed to construct 'WebSocket': The subprotocol '" + p + "' is duplicated.", 'SyntaxError');
        });
        var local = /^(localhost|127\.[0-9.]+|\[::1\])$/.test(u.hostname) || /\.localhost$/.test(u.hostname);
        if (u.protocol === 'ws:' && location.protocol === 'https:' && !local) {
          throw new DOMException("Failed to construct 'WebSocket': An insecure WebSocket connection may not be initiated from a page loaded over HTTPS.", 'SecurityError');
        }
        var s = { url: u.href, origin: u.protocol + '//' + u.host, rt: 0, bt: 'blob', on: {}, proto: '', ext: '', id: -1, q: null, pend: 0, late: 0 };
        S.set(this, s);
        s.id = ws(0, s.url, list.join(','), on(this, s));
      }
      get url() { return st(this).url; }
      get readyState() { return st(this).rt; }
      get bufferedAmount() { var s = st(this); return ws(3, s.id) + s.pend + s.late; }
      get extensions() { return st(this).ext; }
      get protocol() { return st(this).proto; }
      get binaryType() { return st(this).bt; }
      set binaryType(v) { if (v === 'blob' || v === 'arraybuffer') st(this).bt = v; }
      send(data) {
        var s = st(this);
        if (arguments.length < 1) throw new TypeError("Failed to execute 'send' on 'WebSocket': 1 argument required, but only 0 present.");
        if (s.rt === 0) throw new DOMException("Failed to execute 'send' on 'WebSocket': Still in CONNECTING state.", 'InvalidStateError');
        var blob = data instanceof Blob;
        var bytes = blob ? null : ArrayBuffer.isView(data) ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength) : data instanceof ArrayBuffer ? new Uint8Array(data) : String(data);
        if (s.rt !== 1) {
          s.late += blob ? data.size : typeof bytes === 'string' ? utf8(bytes) : bytes.length;
          return;
        }
        if (blob) {
          // The bytes of a Blob come later: what is sent after it waits for them, so the order stays.
          var size = data.size, p = data.arrayBuffer();
          s.pend += size;
          s.q = (s.q || Promise.resolve()).then(function () { return p; }).then(function (b) {
            s.pend -= size;
            ws(1, s.id, new Uint8Array(b));
          });
        } else if (s.q) {
          s.q = s.q.then(function () { ws(1, s.id, bytes); });
        } else {
          ws(1, s.id, bytes);
        }
      }
      close(code, reason) {
        var s = st(this);
        if (code !== undefined && code !== 1000 && !(code >= 3000 && code <= 4999)) {
          throw new DOMException("Failed to execute 'close' on 'WebSocket': The close code must be either 1000, or between 3000 and 4999. " + code + ' is neither.', 'InvalidAccessError');
        }
        reason = reason === undefined ? '' : String(reason);
        if (utf8(reason) > 123) throw new DOMException("Failed to execute 'close' on 'WebSocket': The close reason must not be greater than 123 UTF-8 bytes.", 'SyntaxError');
        if (s.rt >= 2) return;
        if (s.rt === 0) {
          s.rt = 2;
          ws(2, s.id, -2, '');
          console.error("WebSocket connection to '" + s.url + "' failed: WebSocket is closed before the connection is established.");
          var o = this;
          setTimeout(function () { closed(o, s, 1006, '', false, true); }, 0);
          return;
        }
        s.rt = 2;
        var c = code === undefined ? -1 : code;
        if (s.q) s.q = s.q.then(function () { ws(2, s.id, c, reason); });
        else ws(2, s.id, c, reason);
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
  };
})()
