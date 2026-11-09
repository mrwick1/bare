// `Animation` and `Element.prototype.animate`: no timeline, no effect. An animation that plays ends at once (one timer), so
// `finished` resolves and `onfinish` runs. Made at the first read of `Animation` or `animate` (`browser_shims.rs`).
(function () {
  'use strict';
  var S = new WeakMap(), P = Promise;
  // `new DOMException(...)` is not a constructor here, so the error inherits from the prototype.
  var codes = { AbortError: 20, NotFoundError: 8, InvalidStateError: 11, InvalidAccessError: 15 };
  function err(msg, name) {
    var e = Object.create(DOMException.prototype);
    Object.defineProperties(e, { message: { value: msg, writable: true, configurable: true }, name: { value: name, writable: true, configurable: true }, code: { value: codes[name] || 0, configurable: true } });
    return e;
  }
  function st(a) {
    var s = S.get(a);
    if (!s) throw new TypeError('Illegal invocation');
    return s;
  }
  function fresh(s) {
    s.done = new P(function (res, rej) { s.res = res; s.rej = rej; });
    s.done.catch(function () {});
  }
  function fire(a, s, type) {
    P.resolve().then(function () {
      var e = { type: type, target: a, currentTarget: a, currentTime: null, timelineTime: null };
      if (typeof a['on' + type] === 'function') a['on' + type](e);
      (s.ls[type] || []).slice().forEach(function (f) { typeof f === 'function' ? f.call(a, e) : f.handleEvent(e); });
    });
  }
  function end(a, s) {
    s.st = 'finished';
    s.res(a);
    fire(a, s, 'finish');
  }
  function run(a, s) {
    var t = ++s.t;
    setTimeout(function () { if (s.t === t && s.st === 'running') end(a, s); }, 0);
  }
  function init(a, effect, play) {
    var s = { st: 'idle', t: 0, rate: 1, id: '', effect: effect, ls: {} };
    fresh(s);
    S.set(a, s);
    if (play) {
      s.st = 'running';
      run(a, s);
    }
  }
  function Animation(effect, timeline) {
    if (!(this instanceof Animation)) throw new TypeError("Failed to construct 'Animation': Please use the 'new' operator, this DOM object constructor cannot be called as a function.");
    init(this, effect === undefined ? null : effect, false);
  }
  var proto = Animation.prototype, none = function () { st(this); };
  var methods = {
    play: function () {
      var s = st(this);
      if (s.st === 'finished') fresh(s);
      if (s.st !== 'running') {
        s.st = 'running';
        run(this, s);
      }
    },
    pause: function () {
      var s = st(this);
      if (s.st === 'running') {
        s.st = 'paused';
        s.t++;
      }
    },
    cancel: function () {
      var s = st(this);
      if (s.st === 'idle') return;
      if (s.st !== 'finished') s.rej(err('The user aborted a request.', 'AbortError'));
      s.st = 'idle';
      s.t++;
      fresh(s);
      fire(this, s, 'cancel');
    },
    finish: function () {
      var s = st(this);
      if (s.st !== 'finished') {
        s.t++;
        end(this, s);
      }
    },
    reverse: none,
    persist: none,
    commitStyles: none,
    updatePlaybackRate: function (r) { st(this).rate = +r || 1; },
    addEventListener: function (type, f) { var l = st(this).ls; (l[type] || (l[type] = [])).push(f); },
    removeEventListener: function (type, f) { var l = st(this).ls; l[type] = (l[type] || []).filter(function (x) { return x !== f; }); }
  };
  Object.keys(methods).forEach(function (n) {
    Object.defineProperty(proto, n, { value: methods[n], writable: true, enumerable: true, configurable: true });
  });
  var getters = {
    playState: function () { return st(this).st; },
    finished: function () { return st(this).done; },
    ready: function () { return P.resolve(this); },
    effect: function () { return st(this).effect; },
    timeline: function () { st(this); return null; },
    currentTime: function () { return st(this).st === 'idle' ? null : 0; },
    startTime: function () { st(this); return null; },
    pending: function () { st(this); return false; },
    replaceState: function () { st(this); return 'active'; }
  };
  Object.keys(getters).forEach(function (n) {
    Object.defineProperty(proto, n, { get: getters[n], enumerable: true, configurable: true });
  });
  Object.defineProperty(proto, 'playbackRate', { get: function () { return st(this).rate; }, set: function (r) { st(this).rate = +r || 1; }, enumerable: true, configurable: true });
  Object.defineProperty(proto, 'id', { get: function () { return st(this).id; }, set: function (v) { st(this).id = String(v); }, enumerable: true, configurable: true });
  ['onfinish', 'oncancel', 'onremove'].forEach(function (n) {
    Object.defineProperty(proto, n, { value: null, writable: true, enumerable: true, configurable: true });
  });
  Object.defineProperty(proto, Symbol.toStringTag, { value: 'Animation', configurable: true });
  Object.defineProperty(globalThis, 'Animation', { value: Animation, writable: true, configurable: true });
  Object.defineProperty(Element.prototype, 'animate', {
    value: function animate(keyframes, options) {
      if (arguments.length < 1) throw new TypeError("Failed to execute 'animate' on 'Element': 1 argument required, but only 0 present.");
      var a = Object.create(proto);
      init(a, null, true);
      return a;
    },
    writable: true, enumerable: true, configurable: true
  });
})();
