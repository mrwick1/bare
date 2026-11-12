// `FileReader` (File API, "The FileReader API"), over `Blob.prototype.arrayBuffer` and `TextDecoder`. Made at the first read of `FileReader` (`browser_shims.rs`).
// A read sets LOADING and returns. In a later task it fires `loadstart`, reads the bytes, fires one `progress`, then sets DONE and the
// result, and fires `load` and `loadend`. A failed read sets `error`, then fires `error` and `loadend`. `abort` ends a read at once: the
// result is dropped, and `abort` and `loadend` fire. `abort` on an idle reader clears the result and fires nothing, as the standard says.
(function () {
  'use strict';
  var S = new WeakMap();
  var TYPES = ['loadstart', 'progress', 'load', 'abort', 'error', 'loadend'];
  function st(o) {
    var s = S.get(o);
    if (!s) throw new TypeError('Illegal invocation');
    return s;
  }
  function fire(o, type, init) {
    o.dispatchEvent(typeof ProgressEvent === 'function' ? new ProgressEvent(type, init) : new Event(type));
  }
  // The result for the kind of read. An unknown encoding label reads as UTF-8, as the standard says.
  function convert(buf, blob, kind, enc) {
    if (kind === 'buffer') return buf;
    var bytes = new Uint8Array(buf), s = '', i;
    if (kind === 'text') {
      var dec;
      try { dec = new TextDecoder(enc); } catch (e) { dec = new TextDecoder(); }
      return dec.decode(bytes);
    }
    for (i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
    if (kind === 'binary') return s;
    return 'data:' + (blob.type || 'application/octet-stream') + ';base64,' + btoa(s);
  }
  function read(o, s, blob, kind, enc) {
    if (!(blob instanceof Blob)) throw new TypeError("Failed to execute the read on 'FileReader': parameter 1 is not of type 'Blob'.");
    if (s.state === 1) throw new DOMException("The object is already busy reading Blobs.", 'InvalidStateError');
    s.state = 1; s.result = null; s.error = null;
    var token = s.token = {};
    setTimeout(function () {
      if (s.token !== token) return;
      fire(o, 'loadstart');
      blob.arrayBuffer().then(function (buf) {
        if (s.token !== token) return;
        fire(o, 'progress', { lengthComputable: true, loaded: buf.byteLength, total: buf.byteLength });
        if (s.token !== token) return;
        s.token = null; s.state = 2; s.result = convert(buf, blob, kind, enc);
        fire(o, 'load');
        fire(o, 'loadend');
      }, function () {
        if (s.token !== token) return;
        s.token = null; s.state = 2; s.error = new DOMException('The file could not be read.', 'NotReadableError');
        fire(o, 'error');
        fire(o, 'loadend');
      });
    }, 0);
  }
  class FileReader extends EventTarget {
    constructor() {
      super();
      S.set(this, { state: 0, result: null, error: null, token: null, on: {} });
    }
    get readyState() { return st(this).state; }
    get result() { return st(this).result; }
    get error() { return st(this).error; }
    readAsArrayBuffer(blob) { read(this, st(this), blob, 'buffer'); }
    readAsText(blob, enc) { read(this, st(this), blob, 'text', enc); }
    readAsDataURL(blob) { read(this, st(this), blob, 'url'); }
    readAsBinaryString(blob) { read(this, st(this), blob, 'binary'); }
    abort() {
      var s = st(this);
      if (s.state !== 1) { s.result = null; return; }
      s.state = 2; s.result = null; s.token = null;
      fire(this, 'abort');
      fire(this, 'loadend');
    }
  }
  TYPES.forEach(function (t) {
    Object.defineProperty(FileReader.prototype, 'on' + t, {
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
  [['EMPTY', 0], ['LOADING', 1], ['DONE', 2]].forEach(function (c) {
    Object.defineProperty(FileReader, c[0], { value: c[1], enumerable: true });
    Object.defineProperty(FileReader.prototype, c[0], { value: c[1], enumerable: true });
  });
  Object.defineProperty(FileReader.prototype, Symbol.toStringTag, { value: 'FileReader', configurable: true });
  Object.defineProperty(globalThis, 'FileReader', { value: FileReader, writable: true, configurable: true });
})();
