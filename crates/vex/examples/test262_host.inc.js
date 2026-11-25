// Run in a separate script so the test keeps its directive prologue.
(function (g) {
  // The tests change the built-ins (`Array.prototype.join` has a test that deletes it), so the host keeps its own.
  var join = Array.prototype.join, define = Object.defineProperty;
  function append(a, v) { define(a, a.length, { value: v, writable: true, enumerable: true, configurable: true }); }
  function hide(name, value) { Object.defineProperty(g, name, { value: value, writable: true, configurable: true }); }
  var timers = null, now = Date.now;
  hide('setTimeout', function (callback, delay) {
    var time = now() + Math.max(0, Math.min(Number(delay) || 0, 10000));
    var timer = { callback: callback, time: time, next: null };
    if (timers === null || time < timers.time) {
      timer.next = timers;
      timers = timer;
    } else {
      var previous = timers;
      while (previous.next !== null && previous.next.time <= time) previous = previous.next;
      timer.next = previous.next;
      previous.next = timer;
    }
  });
  hide('__runTimer', function () {
    if (timers === null) return -1;
    var delay = timers.time - now();
    if (delay > 0) return delay;
    var callback = timers.callback;
    timers = timers.next;
    callback();
    return true;
  });
  hide('__log', []);
  hide('print', function print() { append(g.__log, join.call(arguments, ' ')); });
  var call = Function.prototype.call;
  var detached = call.bind(Object.getOwnPropertyDescriptor(ArrayBuffer.prototype, 'detached').get);
  var transfer = call.bind(ArrayBuffer.prototype.transferToFixedLength);
  function detachArrayBuffer(buffer) {
    if (!detached(buffer)) transfer(buffer, 0);
  }
  hide('$262', { global: g, gc: function () {}, agent: {}, detachArrayBuffer: detachArrayBuffer });
  hide('__finish', function () { return join.call(g.__log, '\n'); });
  hide('__thrown', function (e) {
    var n = 'unknown', m = '', k = ['TypeError', 'RangeError', 'ReferenceError', 'SyntaxError', 'EvalError', 'URIError', 'Test262Error', 'Error'];
    try {
      for (var i = 0; i < k.length; i++) {
        if (typeof g[k[i]] === 'function' && e instanceof g[k[i]]) { n = k[i]; break; }
      }
      if (n === 'unknown') n = e.constructor.name;
    } catch (_) {}
    try { m = (typeof e === 'object' && e !== null) ? String(e.message) : String(e); } catch (_) {}
    return n + '\u0001' + m;
  });
})(globalThis);
