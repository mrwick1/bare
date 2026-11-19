// `speechSynthesis` and the `SpeechSynthesis*` interfaces (Web Speech API). Made at the first read of one of the names (`browser_shims.rs`).
// This browser has no voices and no audio, as headless Chrome: `getVoices()` gives [], and `speak()` fires `error` with
// "synthesis-unavailable" on the utterance in a later task. A page that reads the voices or speaks runs on.
(function () {
  'use strict';
  var S = new WeakMap();
  function st(o) {
    var s = S.get(o);
    if (!s) throw new TypeError('Illegal invocation');
    return s;
  }
  function handlers(proto, types) {
    types.forEach(function (t) {
      Object.defineProperty(proto, 'on' + t, {
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
  }
  function ro(o, k, v) { Object.defineProperty(o, k, { value: v, enumerable: true }); }
  class SpeechSynthesisEvent extends Event {
    constructor(type, init) {
      if (arguments.length < 2) throw new TypeError("Failed to construct 'SpeechSynthesisEvent': 2 arguments required, but only " + arguments.length + ' present.');
      super(type, init);
      ro(this, 'utterance', init.utterance);
      ro(this, 'charIndex', init.charIndex >>> 0);
      ro(this, 'charLength', init.charLength >>> 0);
      ro(this, 'elapsedTime', +init.elapsedTime || 0);
      ro(this, 'name', init.name === undefined ? '' : String(init.name));
    }
  }
  class SpeechSynthesisErrorEvent extends SpeechSynthesisEvent {
    constructor(type, init) {
      super(type, init);
      ro(this, 'error', init.error);
    }
  }
  class SpeechSynthesisUtterance extends EventTarget {
    constructor(text) {
      super();
      S.set(this, { on: {} });
      this.text = text === undefined ? '' : String(text);
      this.lang = '';
      this.voice = null;
      this.volume = 1;
      this.rate = 1;
      this.pitch = 1;
    }
  }
  handlers(SpeechSynthesisUtterance.prototype, ['start', 'end', 'error', 'pause', 'resume', 'mark', 'boundary']);
  var made = false;
  class SpeechSynthesis extends EventTarget {
    constructor() {
      if (made) throw new TypeError('Illegal constructor');
      made = true;
      super();
      S.set(this, { on: {} });
    }
    get pending() { st(this); return false; }
    get speaking() { st(this); return false; }
    get paused() { st(this); return false; }
    getVoices() { st(this); return []; }
    speak(u) {
      st(this);
      if (!(u instanceof SpeechSynthesisUtterance)) throw new TypeError("Failed to execute 'speak' on 'SpeechSynthesis': parameter 1 is not of type 'SpeechSynthesisUtterance'.");
      setTimeout(function () { u.dispatchEvent(new SpeechSynthesisErrorEvent('error', { utterance: u, error: 'synthesis-unavailable' })); }, 0);
    }
    cancel() { st(this); }
    pause() { st(this); }
    resume() { st(this); }
  }
  handlers(SpeechSynthesis.prototype, ['voiceschanged']);
  var synth = new SpeechSynthesis();
  [[SpeechSynthesis, 'SpeechSynthesis'], [SpeechSynthesisUtterance, 'SpeechSynthesisUtterance'], [SpeechSynthesisEvent, 'SpeechSynthesisEvent'], [SpeechSynthesisErrorEvent, 'SpeechSynthesisErrorEvent']].forEach(function (c) {
    Object.defineProperty(c[0].prototype, Symbol.toStringTag, { value: c[1], configurable: true });
    Object.defineProperty(globalThis, c[1], { value: c[0], writable: true, configurable: true });
  });
  Object.defineProperty(globalThis, 'speechSynthesis', { get: function () { return synth; }, enumerable: true, configurable: true });
})();
