// `EventSource` (HTML Standard, "Server-sent events"), on top of `XMLHttpRequest`. Made at the first read of `EventSource` (`browser_shims.rs`).
// The request asks for `text/event-stream`, so the host reads its body as it comes (`queue_xhr_stream` in `browser_fetch.rs`): each chunk
// is a `readystatechange` in state 3, and the events in it fire then. When the stream ends, the connection is made again after the retry
// time. A last event with no blank line after it is dropped, as the standard says.
(function () {
  'use strict';
  var S = new WeakMap();
  var TYPES = ['open', 'message', 'error'];
  function st(o) {
    var s = S.get(o);
    if (!s) throw new TypeError('Illegal invocation');
    return s;
  }
  function fire(o, type, extra) {
    var e = new Event(type);
    if (extra) Object.keys(extra).forEach(function (k) { Object.defineProperty(e, k, { value: extra[k], enumerable: true, configurable: true }); });
    o.dispatchEvent(e);
  }
  function message(o, s, data, type, origin) {
    // The standard: the last event ID is set before the event is dispatched, and an empty data buffer gives no event.
    if (data === null) return;
    fire(o, type, { data: data, origin: origin, lastEventId: s.id, source: null, ports: [] });
  }
  function fail(o, s) {
    s.rt = 2;
    fire(o, 'error');
  }
  function reconnect(o, s) {
    if (s.rt === 2) return;
    s.rt = 0;
    fire(o, 'error');
    if (s.rt === 2) return;
    s.timer = setTimeout(function () { s.timer = 0; connect(o, s); }, s.retry);
  }
  // Parse the complete lines of `s.buf`; keep the rest for the next chunk. The data and the type of the event that is being read stay in
  // `s` across chunks. A `\r` at the end waits: a `\n` may follow in the next chunk.
  function parse(o, s) {
    var re = /\r\n|\n|\r(?!$)/g, at = 0, m;
    while (s.rt !== 2 && (m = re.exec(s.buf))) {
      line(o, s, s.buf.slice(at, m.index));
      at = re.lastIndex;
    }
    s.buf = s.buf.slice(at);
  }
  function line(o, s, l) {
    if (l === '') {
      if (s.pendingId !== null) { s.id = s.pendingId; s.pendingId = null; }
      message(o, s, s.data === null ? null : s.data.replace(/\n$/, ''), s.type || 'message', s.origin);
      s.data = null;
      s.type = '';
    } else if (l.charAt(0) !== ':') {
      var c = l.indexOf(':'), f = c < 0 ? l : l.slice(0, c), v = c < 0 ? '' : l.slice(c + 1);
      if (v.charAt(0) === ' ') v = v.slice(1);
      if (f === 'data') s.data = (s.data === null ? '' : s.data) + v + '\n';
      else if (f === 'event') s.type = v;
      else if (f === 'id') { if (v.indexOf('\0') < 0) s.pendingId = v; }
      else if (f === 'retry' && /^[0-9]+$/.test(v)) s.retry = parseInt(v, 10);
    }
  }
  function connect(o, s) {
    var x = new XMLHttpRequest();
    s.x = x;
    x.open('GET', s.url);
    x.withCredentials = s.cred;
    x.setRequestHeader('Accept', 'text/event-stream');
    x.setRequestHeader('Cache-Control', 'no-cache');
    if (s.id !== '') x.setRequestHeader('Last-Event-ID', s.id);
    s.seen = 0; s.buf = ''; s.data = null; s.type = ''; s.pendingId = null;
    x.onerror = function () { if (s.x === x) reconnect(o, s); };
    x.onreadystatechange = function () {
      if (s.x !== x || s.rt === 2) return;
      if (x.readyState === 2) {
        var ct = (x.getResponseHeader('content-type') || '').split(';')[0].trim().toLowerCase(), st = x.status;
        if (st === 200 && ct === 'text/event-stream') {
          s.rt = 1;
          fire(o, 'open');
        } else {
          s.x = null;
          x.abort();
          if (st === 200 || st === 204 || (st >= 300 && st < 600 && st !== 502 && st !== 503 && st !== 504)) fail(o, s);
          else reconnect(o, s);
        }
      } else if (x.readyState >= 3 && s.rt === 1) {
        var text = x.responseText;
        if (s.seen === 0 && text.charCodeAt(0) === 0xFEFF) s.seen = 1;
        s.buf += text.slice(s.seen);
        s.seen = text.length;
        parse(o, s);
      }
    };
    x.onload = function () { if (s.x === x && s.rt !== 2) reconnect(o, s); };
    x.send();
  }
  class EventSource extends EventTarget {
    constructor(url, init) {
      if (arguments.length < 1) throw new TypeError("Failed to construct 'EventSource': 1 argument required, but only 0 present.");
      super();
      var u;
      try { u = new URL(String(url), document.baseURI); } catch (e) { u = null; }
      if (!u) throw new DOMException("Failed to construct 'EventSource': Cannot open an EventSource to '" + url + "'. The URL is invalid.", 'SyntaxError');
      var s = { url: u.href, origin: u.origin, cred: !!(init && init.withCredentials), rt: 0, id: '', pendingId: null, retry: 3000, timer: 0, x: null, on: {} };
      S.set(this, s);
      connect(this, s);
    }
    get url() { return st(this).url; }
    get withCredentials() { return st(this).cred; }
    get readyState() { return st(this).rt; }
    close() {
      var s = st(this);
      s.rt = 2;
      if (s.timer) clearTimeout(s.timer);
      s.timer = 0;
      if (s.x) s.x.abort();
    }
  }
  TYPES.forEach(function (t) {
    Object.defineProperty(EventSource.prototype, 'on' + t, {
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
  [['CONNECTING', 0], ['OPEN', 1], ['CLOSED', 2]].forEach(function (c) {
    Object.defineProperty(EventSource, c[0], { value: c[1], enumerable: true });
    Object.defineProperty(EventSource.prototype, c[0], { value: c[1], enumerable: true });
  });
  Object.defineProperty(EventSource.prototype, Symbol.toStringTag, { value: 'EventSource', configurable: true });
  Object.defineProperty(globalThis, 'EventSource', { value: EventSource, writable: true, configurable: true });
})();
