// `DataTransfer`, `DataTransferItem`, `DataTransferItemList` and `ClipboardEvent` (HTML Standard, "Drag and drop" data store; Clipboard API
// and events). Made at the first read of one of the names (`browser_shims.rs`). The value of the file is the helper that the host calls
// for the copy, cut and paste keys (`browser_clipboard.rs`): it makes the events and reads the data back.
// Gaps: `files` is an array with `item()`, not a `FileList`. `effectAllowed` and `dropEffect` are only stored. There is no drag image.
(function () {
  'use strict';
  var S = new WeakMap();
  var E = new WeakMap();
  var L = new WeakMap();
  var I = new WeakMap();
  function illegal() { return new TypeError('Illegal invocation'); }
  function st(o) { var s = S.get(o); if (!s) throw illegal(); return s; }
  // The type of a format name: lower case, and the two legacy names.
  function norm(f) {
    f = String(f).toLowerCase();
    return f === 'text' ? 'text/plain' : f === 'url' ? 'text/uri-list' : f;
  }
  function refresh(s) {
    var l = s.list;
    var n = s.items.length;
    for (var i = 0; i < n; i++) l[i] = s.items[i].wrap;
    for (var j = n; j in l; j++) delete l[j];
    s.files = s.items.filter(function (x) { return x.file; }).map(function (x) { return x.file; });
    s.files.item = function (i) { return this[i] || null; };
  }
  function wrapItem(s, rec) {
    var it = Object.create(DataTransferItem.prototype);
    I.set(it, rec);
    rec.wrap = it;
    return it;
  }
  function DataTransfer() {
    if (!new.target) throw new TypeError("Failed to construct 'DataTransfer': Please use the 'new' operator, this DOM object constructor cannot be called as a function.");
    var s = { items: [], files: [], mode: 'rw', drop: 'none', allowed: 'all', list: null };
    S.set(this, s);
    var list = Object.create(DataTransferItemList.prototype);
    L.set(list, s);
    s.list = list;
    refresh(s);
  }
  function DataTransferItemList() { throw new TypeError('Illegal constructor'); }
  function DataTransferItem() { throw new TypeError('Illegal constructor'); }
  var dtp = DataTransfer.prototype;
  Object.defineProperties(dtp, {
    dropEffect: { get: function () { return st(this).drop; }, set: function (v) { v = String(v); if (v === 'none' || v === 'copy' || v === 'link' || v === 'move') st(this).drop = v; }, enumerable: true, configurable: true },
    effectAllowed: { get: function () { return st(this).allowed; }, set: function (v) { v = String(v); if (st(this).mode === 'rw' && /^(none|copy|copyLink|copyMove|link|linkMove|move|all|uninitialized)$/.test(v)) st(this).allowed = v; }, enumerable: true, configurable: true },
    items: { get: function () { return st(this).list; }, enumerable: true, configurable: true },
    types: {
      get: function () {
        var s = st(this), t = [];
        s.items.forEach(function (x) { if (!x.file) t.push(x.type); });
        if (s.files.length) t.push('Files');
        return Object.freeze(t);
      }, enumerable: true, configurable: true
    },
    files: { get: function () { return st(this).files; }, enumerable: true, configurable: true }
  });
  function method(o, name, len, fn) {
    Object.defineProperty(o, name, { value: fn, writable: true, enumerable: true, configurable: true });
    Object.defineProperty(fn, 'length', { value: len });
    Object.defineProperty(fn, 'name', { value: name });
  }
  method(dtp, 'getData', 1, function getData(format) {
    var s = st(this);
    if (arguments.length < 1) throw new TypeError("Failed to execute 'getData' on 'DataTransfer': 1 argument required, but only 0 present.");
    if (s.mode === 'wo') return '';
    var t = norm(format);
    for (var i = 0; i < s.items.length; i++) if (!s.items[i].file && s.items[i].type === t) return s.items[i].data;
    return '';
  });
  method(dtp, 'setData', 2, function setData(format, data) {
    var s = st(this);
    if (arguments.length < 2) throw new TypeError("Failed to execute 'setData' on 'DataTransfer': 2 arguments required, but only " + arguments.length + ' present.');
    if (s.mode !== 'rw') return;
    var t = norm(format);
    data = String(data);
    for (var i = 0; i < s.items.length; i++) {
      if (!s.items[i].file && s.items[i].type === t) { s.items[i].data = data; return; }
    }
    var rec = { type: t, data: data, file: null };
    s.items.push(rec);
    wrapItem(s, rec);
    refresh(s);
  });
  method(dtp, 'clearData', 0, function clearData(format) {
    var s = st(this);
    if (s.mode !== 'rw') return;
    var t = format === undefined ? null : norm(format);
    s.items = s.items.filter(function (x) { return x.file || (t !== null && x.type !== t); });
    refresh(s);
  });
  method(dtp, 'setDragImage', 3, function setDragImage() { st(this); });
  Object.defineProperty(dtp, Symbol.toStringTag, { value: 'DataTransfer', configurable: true });

  var lp = DataTransferItemList.prototype;
  function ls(o) { var s = L.get(o); if (!s) throw illegal(); return s; }
  Object.defineProperty(lp, 'length', { get: function () { return ls(this).items.length; }, enumerable: true, configurable: true });
  method(lp, 'add', 1, function add(data, type) {
    var s = ls(this);
    if (s.mode !== 'rw') return null;
    var rec;
    if (typeof File === 'function' && data instanceof File) {
      rec = { type: data.type, data: '', file: data };
    } else {
      if (arguments.length < 2) throw new TypeError("Failed to execute 'add' on 'DataTransferItemList': parameter 1 is not of type 'File'.");
      var t = norm(type);
      for (var i = 0; i < s.items.length; i++) {
        if (!s.items[i].file && s.items[i].type === t) throw new DOMException("Failed to execute 'add' on 'DataTransferItemList': An item already exists for type '" + t + "'.", 'NotSupportedError');
      }
      rec = { type: t, data: String(data), file: null };
    }
    s.items.push(rec);
    var w = wrapItem(s, rec);
    refresh(s);
    return w;
  });
  method(lp, 'remove', 1, function remove(index) {
    var s = ls(this);
    if (s.mode !== 'rw') throw new DOMException("Failed to execute 'remove' on 'DataTransferItemList': The list is not writable.", 'InvalidStateError');
    index = Number(index) >>> 0;
    if (index < s.items.length) { s.items.splice(index, 1); refresh(s); }
  });
  method(lp, 'clear', 0, function clear() {
    var s = ls(this);
    if (s.mode !== 'rw') return;
    s.items = [];
    refresh(s);
  });
  method(lp, Symbol.iterator, 0, function values() {
    var s = ls(this), i = 0;
    return { next: function () { return i < s.items.length ? { value: s.items[i++].wrap, done: false } : { value: undefined, done: true }; }, [Symbol.iterator]: function () { return this; } };
  });
  Object.defineProperty(lp, Symbol.toStringTag, { value: 'DataTransferItemList', configurable: true });

  var ip = DataTransferItem.prototype;
  function is(o) { var r = I.get(o); if (!r) throw illegal(); return r; }
  Object.defineProperties(ip, {
    kind: { get: function () { return is(this).file ? 'file' : 'string'; }, enumerable: true, configurable: true },
    type: { get: function () { return is(this).type; }, enumerable: true, configurable: true }
  });
  method(ip, 'getAsString', 1, function getAsString(cb) {
    var r = is(this);
    if (typeof cb !== 'function' || r.file) return;
    setTimeout(function () { cb(r.data); }, 0);
  });
  method(ip, 'getAsFile', 0, function getAsFile() { return is(this).file; });
  method(ip, 'webkitGetAsEntry', 0, function webkitGetAsEntry() { is(this); return null; });
  Object.defineProperty(ip, Symbol.toStringTag, { value: 'DataTransferItem', configurable: true });

  class ClipboardEvent extends Event {
    constructor(type, init) {
      if (arguments.length < 1) throw new TypeError("Failed to construct 'ClipboardEvent': 1 argument required, but only 0 present.");
      super(type, init);
      var d = init == null ? null : init.clipboardData;
      if (d != null && !S.has(d)) throw new TypeError("Failed to construct 'ClipboardEvent': Failed to read the 'clipboardData' property from 'ClipboardEventInit': Failed to convert value to 'DataTransfer'.");
      E.set(this, d == null ? null : d);
    }
    get clipboardData() { if (!E.has(this)) throw illegal(); return E.get(this); }
  }
  Object.defineProperty(ClipboardEvent.prototype, Symbol.toStringTag, { value: 'ClipboardEvent', configurable: true });

  [['DataTransfer', DataTransfer], ['DataTransferItem', DataTransferItem], ['DataTransferItemList', DataTransferItemList], ['ClipboardEvent', ClipboardEvent]].forEach(function (p) {
    Object.defineProperty(globalThis, p[0], { value: p[1], writable: true, enumerable: false, configurable: true });
  });

  var EXT = { 'image/png': 'png', 'image/jpeg': 'jpg', 'image/webp': 'webp', 'image/gif': 'gif' };
  // What the host calls. `copy`: a `copy` or `cut` event with an empty writable store. `paste`: a `paste` event whose store is read-only
  // and holds the text (when not `null`) and the image (`bin` is its bytes as a string of code units below 256).
  return {
    copy: function (type) {
      var dt = new DataTransfer();
      return new ClipboardEvent(type, { bubbles: true, cancelable: true, composed: true, clipboardData: dt });
    },
    paste: function (text, bin, mime) {
      var dt = new DataTransfer();
      if (text !== null) dt.items.add(text, 'text/plain');
      if (bin !== null) {
        var bytes = new Uint8Array(bin.length);
        for (var i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
        dt.items.add(new File([bytes], 'image.' + (EXT[mime] || 'png'), { type: mime }));
      }
      S.get(dt).mode = 'ro';
      return new ClipboardEvent('paste', { bubbles: true, cancelable: true, composed: true, clipboardData: dt });
    },
    // The string items of the store of `e`, as `[type, data, type, data, ...]`.
    read: function (e) {
      var out = [];
      S.get(e.clipboardData).items.forEach(function (x) { if (!x.file) out.push(x.type, x.data); });
      return out;
    }
  };
})()
