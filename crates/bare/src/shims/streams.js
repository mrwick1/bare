// `WritableStream`, `TransformStream`, `ReadableStream.prototype.pipeTo` and `pipeThrough`, `TextDecoderStream` and `TextEncoderStream`
// (Streams Standard, Encoding Standard). Made at the first read of one of these names, or at the first call of `pipeTo` or
// `pipeThrough` (`browser_shims.rs`). The readable side is the host `ReadableStream` (`browser_streams.rs`), so its limits hold for
// every readable queue here. The writable queue lives in the realm heap, so the heap limits bound it.
// The algorithms follow the standard, step by step, for default streams. The state of each object is in a WeakMap, so a script can
// not reach it, and a class can extend each constructor.
// Deviations: `pipeTo` sees an error of the source only when it reads, because the host reader has no `closed` promise. An abort of
// the writable side of a TransformStream errors its readable side at once (the older text of the standard); `transformer.cancel` runs.
(function () {
  'use strict';
  var P = Promise, RS = ReadableStream, TE = TextEncoder, TD = TextDecoder;
  var WS = new WeakMap(), WC = new WeakMap(), WW = new WeakMap(), TS = new WeakMap(), TC = new WeakMap(), TX = new WeakMap();
  var CLOSE = {};
  function noop() {}
  function defer() {
    var d = {};
    d.p = new P(function (res, rej) { d.res = res; d.rej = rej; });
    return d;
  }
  function settled() { var d = defer(); d.res(); return d; }
  function failed(e) { var d = defer(); d.rej(e); d.p.then(noop, noop); return d; }
  function rejected(e) { return P.reject(e); }
  function brand(map, o, what) {
    var s = map.get(o);
    if (!s) throw new TypeError('Illegal invocation: this is not a ' + what);
    return s;
  }
  // Read a member of the sink or the transformer: undefined, or a function.
  function method(obj, name, ctor) {
    if (obj === undefined) return undefined;
    var f = obj[name];
    if (f === undefined) return undefined;
    if (typeof f !== 'function') throw new TypeError("Failed to construct '" + ctor + "': The " + name + ' member is not a function');
    return f;
  }
  // Call a member. A throw becomes a rejected promise, as the standard says.
  function promiseCall(f, obj, args) {
    if (!f) return P.resolve();
    try { return P.resolve(f.apply(obj, args)); } catch (e) { return P.reject(e); }
  }
  function strategy(s, dflt, ctor) {
    var q = { hwm: dflt, size: undefined };
    if (s === undefined || s === null) return q;
    if (typeof s !== 'object' && typeof s !== 'function') throw new TypeError("Failed to construct '" + ctor + "': The queuing strategy is not an object");
    var h = s.highWaterMark;
    q.size = s.size;
    if (h !== undefined) {
      q.hwm = Number(h);
      if (q.hwm !== q.hwm || q.hwm < 0) throw new RangeError("Failed to construct '" + ctor + "': Invalid highWaterMark");
    }
    if (q.size !== undefined && typeof q.size !== 'function') throw new TypeError("Failed to construct '" + ctor + "': The size member is not a function");
    return q;
  }

  // ------------------------------------------------------------------------------------------------ WritableStream

  function closeQueuedOrInFlight(s) { return s.closeReq !== null || s.inFlightClose !== null; }
  function inFlight(s) { return s.inFlightWrite !== null || s.inFlightClose !== null; }
  function backpressureOf(c) { return c.hwm - c.total <= 0; }
  function writerOf(s) { return s.writer === undefined ? undefined : WW.get(s.writer); }

  function wsAbort(s, reason) {
    if (s.state === 'closed' || s.state === 'errored') return P.resolve();
    s.ctl.ac.abort(reason);
    if (s.state === 'closed' || s.state === 'errored') return P.resolve();
    if (s.pendingAbort) return s.pendingAbort.d.p;
    var was = s.state === 'erroring';
    if (was) reason = undefined;
    var d = defer();
    s.pendingAbort = { d: d, reason: reason, was: was };
    if (!was) startErroring(s, reason);
    return d.p;
  }
  function wsClose(s) {
    if (s.state === 'closed' || s.state === 'errored') return rejected(new TypeError('The stream is closed or errored'));
    var d = defer();
    s.closeReq = d;
    var w = writerOf(s);
    if (w && s.backpressure && s.state === 'writable') w.ready.res();
    s.ctl.queue.push({ v: CLOSE, size: 0 });
    advance(s.ctl);
    return d.p;
  }
  function dealWithRejection(s, e) {
    if (s.state === 'writable') startErroring(s, e);
    else finishErroring(s);
  }
  function startErroring(s, reason) {
    s.state = 'erroring';
    s.err = reason;
    var w = writerOf(s);
    if (w) readyRejected(w, reason);
    if (!inFlight(s) && s.ctl.started) finishErroring(s);
  }
  function finishErroring(s) {
    s.state = 'errored';
    var c = s.ctl;
    c.queue = [];
    c.total = 0;
    var reqs = s.writeReqs;
    s.writeReqs = [];
    reqs.forEach(function (d) { d.rej(s.err); });
    var a = s.pendingAbort;
    if (!a) return rejectCloseAndClosed(s);
    s.pendingAbort = null;
    if (a.was) { a.d.rej(s.err); return rejectCloseAndClosed(s); }
    var p = promiseCall(c.abort, c.sink, [a.reason]);
    clearSink(c);
    p.then(function () { a.d.res(); rejectCloseAndClosed(s); }, function (r) { a.d.rej(r); rejectCloseAndClosed(s); });
  }
  function rejectCloseAndClosed(s) {
    if (s.closeReq) { s.closeReq.rej(s.err); s.closeReq = null; }
    var w = writerOf(s);
    if (w) { w.closed.rej(s.err); w.closed.p.then(noop, noop); }
  }
  function finishClose(s) {
    s.inFlightClose.res();
    s.inFlightClose = null;
    if (s.state === 'erroring') {
      s.err = undefined;
      if (s.pendingAbort) { s.pendingAbort.d.res(); s.pendingAbort = null; }
    }
    s.state = 'closed';
    var w = writerOf(s);
    if (w) w.closed.res();
  }
  function finishCloseError(s, e) {
    s.inFlightClose.rej(e);
    s.inFlightClose = null;
    if (s.pendingAbort) { s.pendingAbort.d.rej(e); s.pendingAbort = null; }
    dealWithRejection(s, e);
  }
  function updateBackpressure(s, bp) {
    var w = writerOf(s);
    if (w && bp !== s.backpressure) {
      if (bp) w.ready = defer();
      else w.ready.res();
    }
    s.backpressure = bp;
  }
  // A settled promise ignores `rej`, so a settled `ready` or `closed` is replaced by a new rejected one. Both are marked as handled,
  // as the standard says: no "unhandled rejection" is reported for them.
  function readyRejected(w, e) { w.ready.p.then(noop, noop); w.ready.rej(e); w.ready = failed(e); }
  function closedRejected(w, e) { w.closed.p.then(noop, noop); w.closed.rej(e); w.closed = failed(e); }
  function clearSink(c) { c.sink = c.write = c.close = c.abort = c.size = undefined; }
  function errorIfNeeded(c, e) {
    if (c.s.state === 'writable') { clearSink(c); startErroring(c.s, e); }
  }
  function advance(c) {
    var s = c.s;
    if (!c.started || s.inFlightWrite !== null) return;
    if (s.state === 'erroring') return finishErroring(s);
    if (c.queue.length === 0) return;
    var v = c.queue[0].v;
    if (v === CLOSE) {
      s.inFlightClose = s.closeReq;
      s.closeReq = null;
      c.queue.shift();
      var p = promiseCall(c.close, c.sink, []);
      clearSink(c);
      p.then(function () { finishClose(s); }, function (e) { finishCloseError(s, e); });
      return;
    }
    s.inFlightWrite = s.writeReqs.shift();
    promiseCall(c.write, c.sink, [v, c.self]).then(function () {
      s.inFlightWrite.res();
      s.inFlightWrite = null;
      var e = c.queue.shift();
      c.total = Math.max(0, c.total - e.size);
      if (!closeQueuedOrInFlight(s) && s.state === 'writable') updateBackpressure(s, backpressureOf(c));
      advance(c);
    }, function (e) {
      if (s.state === 'writable') clearSink(c);
      s.inFlightWrite.rej(e);
      s.inFlightWrite = null;
      dealWithRejection(s, e);
    });
  }
  // `SetUpWritableStreamDefaultController`. `start` throws out of the constructor.
  function setupWritable(stream, sink, write, close, abort, start, q) {
    var s = { state: 'writable', err: undefined, writer: undefined, ctl: null, inFlightWrite: null, closeReq: null, inFlightClose: null, pendingAbort: null, writeReqs: [], backpressure: false };
    WS.set(stream, s);
    var self = Object.create(WritableStreamDefaultController.prototype);
    var c = { self: self, s: s, sink: sink, write: write, close: close, abort: abort, queue: [], total: 0, started: false, hwm: q.hwm, size: q.size, ac: new AbortController() };
    WC.set(self, c);
    s.ctl = c;
    updateBackpressure(s, backpressureOf(c));
    var r = start(self);
    P.resolve(r).then(function () { c.started = true; advance(c); }, function (e) { c.started = true; dealWithRejection(s, e); });
  }

  class WritableStream {
    constructor(sink, strat) {
      if (sink === null) sink = undefined;
      if (sink !== undefined && typeof sink !== 'object' && typeof sink !== 'function') throw new TypeError("Failed to construct 'WritableStream': The underlying sink is not an object");
      // Dictionary members are read in alphabetical order, as WebIDL does.
      var abort = method(sink, 'abort', 'WritableStream'), close = method(sink, 'close', 'WritableStream'), start = method(sink, 'start', 'WritableStream');
      if (sink !== undefined && sink.type !== undefined) throw new RangeError("Failed to construct 'WritableStream': Invalid type is specified");
      var write = method(sink, 'write', 'WritableStream');
      var q = strategy(strat, 1, 'WritableStream');
      setupWritable(this, sink, write, close, abort, function (c) { return start ? start.call(sink, c) : undefined; }, q);
    }
    get locked() { return brand(WS, this, 'WritableStream').writer !== undefined; }
    abort(reason) {
      var s = WS.get(this);
      if (!s) return rejected(new TypeError('Illegal invocation'));
      if (s.writer !== undefined) return rejected(new TypeError('Cannot abort a locked stream'));
      return wsAbort(s, reason);
    }
    close() {
      var s = WS.get(this);
      if (!s) return rejected(new TypeError('Illegal invocation'));
      if (s.writer !== undefined) return rejected(new TypeError('Cannot close a locked stream'));
      if (closeQueuedOrInFlight(s)) return rejected(new TypeError('Cannot close an already-closing stream'));
      return wsClose(s);
    }
    getWriter() { brand(WS, this, 'WritableStream'); return new WritableStreamDefaultWriter(this); }
  }

  class WritableStreamDefaultWriter {
    constructor(stream) {
      var s = brand(WS, stream, 'WritableStream');
      if (s.writer !== undefined) throw new TypeError('This stream has already been locked for exclusive writing by another writer');
      var w = { stream: stream, ready: null, closed: defer() };
      if (s.state === 'writable') w.ready = !closeQueuedOrInFlight(s) && s.backpressure ? defer() : settled();
      else if (s.state === 'erroring') w.ready = failed(s.err);
      else if (s.state === 'closed') { w.ready = settled(); w.closed.res(); }
      else { w.ready = failed(s.err); w.closed = failed(s.err); }
      WW.set(this, w);
      s.writer = this;
    }
    get closed() {
      var w = WW.get(this);
      return w ? w.closed.p : rejected(new TypeError('Illegal invocation'));
    }
    get ready() {
      var w = WW.get(this);
      return w ? w.ready.p : rejected(new TypeError('Illegal invocation'));
    }
    get desiredSize() {
      var w = brand(WW, this, 'WritableStreamDefaultWriter');
      if (!w.stream) throw new TypeError('This writer is not attached to a stream');
      var s = WS.get(w.stream);
      if (s.state === 'errored' || s.state === 'erroring') return null;
      if (s.state === 'closed') return 0;
      return s.ctl.hwm - s.ctl.total;
    }
    abort(reason) {
      var w = WW.get(this);
      if (!w || !w.stream) return rejected(new TypeError('This writer is not attached to a stream'));
      return wsAbort(WS.get(w.stream), reason);
    }
    close() {
      var w = WW.get(this);
      if (!w || !w.stream) return rejected(new TypeError('This writer is not attached to a stream'));
      var s = WS.get(w.stream);
      if (closeQueuedOrInFlight(s)) return rejected(new TypeError('Cannot close an already-closing stream'));
      return wsClose(s);
    }
    releaseLock() {
      var w = brand(WW, this, 'WritableStreamDefaultWriter');
      if (!w.stream) return;
      var e = new TypeError('Writer was released');
      readyRejected(w, e);
      closedRejected(w, e);
      WS.get(w.stream).writer = undefined;
      w.stream = undefined;
    }
    write(chunk) {
      var w = WW.get(this);
      if (!w || !w.stream) return rejected(new TypeError('This writer is not attached to a stream'));
      return writerWrite(w, chunk);
    }
  }
  function writerWrite(w, chunk) {
    var s = WS.get(w.stream), c = s.ctl, size = 1;
    if (c.size) {
      try { size = c.size.call(undefined, chunk); } catch (e) { errorIfNeeded(c, e); size = 1; }
    }
    if (s.state === 'errored') return rejected(s.err);
    if (closeQueuedOrInFlight(s) || s.state === 'closed') return rejected(new TypeError('The stream is closing or closed and cannot be written to'));
    if (s.state === 'erroring') return rejected(s.err);
    var d = defer();
    s.writeReqs.push(d);
    size = Number(size);
    if (size !== size || size < 0 || size === Infinity) {
      errorIfNeeded(c, new RangeError('The return value of a queuing strategy size function must be a finite, non-NaN, non-negative number'));
      return d.p;
    }
    c.queue.push({ v: chunk, size: size });
    c.total += size;
    if (!closeQueuedOrInFlight(s) && s.state === 'writable') updateBackpressure(s, backpressureOf(c));
    advance(c);
    return d.p;
  }

  class WritableStreamDefaultController {
    constructor() { throw new TypeError('Illegal constructor'); }
    get signal() { return brand(WC, this, 'WritableStreamDefaultController').ac.signal; }
    error(e) {
      var c = brand(WC, this, 'WritableStreamDefaultController');
      if (c.s.state === 'writable') errorIfNeeded(c, e);
    }
  }

  // ------------------------------------------------------------------------------------------------ TransformStream

  function setBackpressure(t, bp) {
    if (t.bpChange) t.bpChange.res();
    t.bpChange = defer();
    t.backpressure = bp;
  }
  // `TransformStreamError`. `rDone` is true once the readable side is closed or errored; `rErrored` says which.
  function tsError(t, e) {
    if (!t.rDone) {
      t.rDone = t.rErrored = true;
      t.rErr = e;
      t.rctl.error(e);
    }
    errorWritableAndUnblock(t, e);
  }
  function errorWritableAndUnblock(t, e) {
    t.transform = t.flush = t.cancel = undefined;
    errorIfNeeded(WS.get(t.writable).ctl, e);
    if (t.backpressure) setBackpressure(t, false);
  }
  function tcEnqueue(t, chunk) {
    if (t.rDone) throw new TypeError('Invalid state: the readable side is closed or errored');
    try {
      t.rctl.enqueue(chunk);
    } catch (e) {
      errorWritableAndUnblock(t, e);
      throw e;
    }
    var bp = !(t.rctl.desiredSize > 0);
    if (bp !== t.backpressure) setBackpressure(t, true);
  }
  function performTransform(t, chunk) {
    var p = t.transform ? promiseCall(t.transform, t.transformer, [chunk, t.self]) : promiseCall(function () { tcEnqueue(t, chunk); }, undefined, []);
    return p.then(undefined, function (r) { tsError(t, r); throw r; });
  }

  class TransformStream {
    constructor(transformer, wStrat, rStrat) {
      if (transformer === null) transformer = undefined;
      if (transformer !== undefined && typeof transformer !== 'object' && typeof transformer !== 'function') throw new TypeError("Failed to construct 'TransformStream': The transformer is not an object");
      var cancel = method(transformer, 'cancel', 'TransformStream'), flush = method(transformer, 'flush', 'TransformStream');
      if (transformer !== undefined && transformer.readableType !== undefined) throw new RangeError("Failed to construct 'TransformStream': Invalid readableType is specified");
      var start = method(transformer, 'start', 'TransformStream'), transform = method(transformer, 'transform', 'TransformStream');
      if (transformer !== undefined && transformer.writableType !== undefined) throw new RangeError("Failed to construct 'TransformStream': Invalid writableType is specified");
      var rq = strategy(rStrat, 0, 'TransformStream'), wq = strategy(wStrat, 1, 'TransformStream');
      var startD = defer(), self = Object.create(TransformStreamDefaultController.prototype), ws = Object.create(WritableStream.prototype);
      var t = { self: self, transformer: transformer, transform: transform, flush: flush, cancel: cancel, rctl: null, readable: null, writable: ws, backpressure: undefined, bpChange: null, rDone: false, rErrored: false, rErr: undefined };
      setupWritable(ws, undefined, function (chunk) {
        if (!t.backpressure) return performTransform(t, chunk);
        return t.bpChange.p.then(function () {
          var s = WS.get(ws);
          if (s.state === 'erroring') throw s.err;
          return performTransform(t, chunk);
        });
      }, function () {
        var p = promiseCall(t.flush, t.transformer, [self]);
        t.transform = t.flush = t.cancel = undefined;
        return p.then(function () {
          if (t.rErrored) throw t.rErr;
          if (!t.rDone) { t.rDone = true; t.rctl.close(); }
        }, function (r) { tsError(t, r); throw t.rErr; });
      }, function (reason) {
        var p = promiseCall(t.cancel, t.transformer, [reason]);
        tsError(t, reason);
        return p;
      }, function () { return startD.p; }, wq);
      // The host stream reads `size` and refuses a function (`Unsupported`), so it gets only what it can keep.
      t.readable = new RS({
        start: function (c) { t.rctl = c; return startD.p; },
        pull: function () { setBackpressure(t, false); return t.bpChange.p; },
        cancel: function (reason) {
          var p = promiseCall(t.cancel, t.transformer, [reason]);
          t.rDone = true;
          errorWritableAndUnblock(t, reason);
          return p;
        }
      }, { highWaterMark: rq.hwm, size: rq.size });
      setBackpressure(t, true);
      TC.set(self, t);
      TS.set(this, t);
      startD.res(start ? start.call(transformer, self) : undefined);
    }
    get readable() { return brand(TS, this, 'TransformStream').readable; }
    get writable() { return brand(TS, this, 'TransformStream').writable; }
  }

  class TransformStreamDefaultController {
    constructor() { throw new TypeError('Illegal constructor'); }
    get desiredSize() { return brand(TC, this, 'TransformStreamDefaultController').rctl.desiredSize; }
    enqueue(chunk) { tcEnqueue(brand(TC, this, 'TransformStreamDefaultController'), chunk); }
    error(e) { tsError(brand(TC, this, 'TransformStreamDefaultController'), e); }
    terminate() {
      var t = brand(TC, this, 'TransformStreamDefaultController');
      if (!t.rDone) { t.rDone = true; t.rctl.close(); }
      errorWritableAndUnblock(t, new TypeError('TransformStream terminated'));
    }
  }

  // ------------------------------------------------------------------------------------------------ pipes

  // `ReadableStreamPipeTo` for a default source and destination.
  function pipe(source, s, opts) {
    var preventAbort = !!opts.preventAbort, preventCancel = !!opts.preventCancel, preventClose = !!opts.preventClose, signal = opts.signal;
    if (signal !== undefined && !(signal instanceof AbortSignal)) return rejected(new TypeError("Failed to execute 'pipeTo' on 'ReadableStream': member signal is not of type AbortSignal."));
    if (source.locked) return rejected(new TypeError('Cannot pipe a locked stream'));
    if (s.writer !== undefined) return rejected(new TypeError('Cannot pipe to a locked stream'));
    var reader = source.getReader(), writer = new WritableStreamDefaultWriter(s.self), w = WW.get(writer);
    var done = defer(), down = false, lastWrite = P.resolve(), onAbort;
    function finalize(isErr, e) {
      writer.releaseLock();
      reader.releaseLock();
      if (onAbort) signal.removeEventListener('abort', onAbort);
      if (isErr) done.rej(e); else done.res();
    }
    function shutdown(action, isErr, e) {
      if (down) return;
      down = true;
      // Every chunk that was read is written first, unless the destination is closing or broken.
      var wait = s.state === 'writable' && !closeQueuedOrInFlight(s) ? lastWrite.then(noop, noop) : P.resolve();
      wait.then(function () {
        if (!action) return finalize(isErr, e);
        action().then(function () { finalize(isErr, e); }, function (x) { finalize(true, x); });
      });
    }
    function cancelSource(e) { return function () { return reader.cancel(e); }; }
    function destErrored(e) { shutdown(preventCancel ? null : cancelSource(e), true, e); }
    function step() {
      if (down) return;
      if (s.state === 'errored' || s.state === 'erroring') return destErrored(s.err);
      if (closeQueuedOrInFlight(s) || s.state === 'closed') {
        var e = new TypeError('The destination writable stream closed before all data could be piped to it.');
        return shutdown(preventCancel ? null : cancelSource(e), true, e);
      }
      w.ready.p.then(function () {
        if (down) return;
        reader.read().then(function (r) {
          if (down) return;
          if (r.done) {
            return shutdown(preventClose ? null : function () {
              if (closeQueuedOrInFlight(s) || s.state === 'closed') return P.resolve();
              if (s.state === 'errored') return P.reject(s.err);
              return wsClose(s);
            }, false);
          }
          lastWrite = writerWrite(w, r.value);
          lastWrite.then(noop, noop);
          step();
        }, function (e) {
          shutdown(preventAbort ? null : function () { return wsAbort(s, e); }, true, e);
        });
      }, step);
    }
    w.closed.p.then(noop, function (e) { if (w.stream) destErrored(e); });
    if (signal) {
      onAbort = function () {
        var e = signal.reason, acts = [];
        if (!preventAbort) acts.push(function () { return s.state === 'writable' ? wsAbort(s, e) : P.resolve(); });
        if (!preventCancel) acts.push(cancelSource(e));
        shutdown(function () { return P.all(acts.map(function (a) { return a(); })); }, true, e);
      };
      if (signal.aborted) { onAbort(); return done.p; }
      signal.addEventListener('abort', onAbort);
    }
    step();
    return done.p;
  }
  function options(o) {
    if (o === undefined || o === null) return {};
    if (typeof o !== 'object' && typeof o !== 'function') throw new TypeError('StreamPipeOptions must be a dictionary');
    return o;
  }

  var proto = RS.prototype;
  var pipes = {
    pipeTo: function (dest, opts) {
      if (!(this instanceof RS)) return rejected(new TypeError('Illegal invocation'));
      var s = WS.get(dest);
      if (!s) return rejected(new TypeError("Failed to execute 'pipeTo' on 'ReadableStream': parameter 1 is not of type 'WritableStream'."));
      try { opts = options(opts); } catch (e) { return rejected(e); }
      s.self = dest;
      return pipe(this, s, opts);
    },
    pipeThrough: function (pair, opts) {
      if (!(this instanceof RS)) throw new TypeError('Illegal invocation');
      var writable = pair === undefined || pair === null ? undefined : pair.writable, readable = pair === undefined || pair === null ? undefined : pair.readable;
      var s = WS.get(writable);
      if (!s) throw new TypeError("Failed to execute 'pipeThrough' on 'ReadableStream': Failed to read the 'writable' property from 'ReadableWritablePair'.");
      if (!(readable instanceof RS)) throw new TypeError("Failed to execute 'pipeThrough' on 'ReadableStream': Failed to read the 'readable' property from 'ReadableWritablePair'.");
      opts = options(opts);
      if (this.locked) throw new TypeError('Cannot pipe a locked stream');
      if (s.writer !== undefined) throw new TypeError('Cannot pipe to a locked stream');
      s.self = writable;
      pipe(this, s, opts).then(noop, noop);
      return readable;
    }
  };
  Object.keys(pipes).forEach(function (n) {
    Object.defineProperty(proto, n, { value: pipes[n], writable: true, enumerable: true, configurable: true });
  });

  // ------------------------------------------------------------------------------------------------ text streams

  class TextDecoderStream {
    constructor(label, opts) {
      var dec = new TD(label === undefined ? 'utf-8' : label, opts);
      var ts = new TransformStream({
        transform: function (chunk, c) {
          if (!ArrayBuffer.isView(chunk) && !(chunk instanceof ArrayBuffer)) throw new TypeError("Failed to execute 'decode' on 'TextDecoder': The provided value is not of type '(ArrayBuffer or ArrayBufferView)'");
          var s = dec.decode(chunk, { stream: true });
          if (s !== '') c.enqueue(s);
        },
        flush: function (c) {
          var s = dec.decode();
          if (s !== '') c.enqueue(s);
        }
      });
      TX.set(this, { dec: dec, ts: ts });
    }
    get encoding() { return brand(TX, this, 'TextDecoderStream').dec.encoding; }
    get fatal() { return brand(TX, this, 'TextDecoderStream').dec.fatal; }
    get ignoreBOM() { return brand(TX, this, 'TextDecoderStream').dec.ignoreBOM; }
    get readable() { return brand(TX, this, 'TextDecoderStream').ts.readable; }
    get writable() { return brand(TX, this, 'TextDecoderStream').ts.writable; }
  }

  class TextEncoderStream {
    constructor() {
      var enc = new TE(), lead = '';
      var ts = new TransformStream({
        transform: function (chunk, c) {
          var s = lead + String(chunk);
          lead = '';
          // A lead surrogate at the end waits for the next chunk, as the Encoding Standard says.
          var last = s.charCodeAt(s.length - 1);
          if (last >= 0xD800 && last <= 0xDBFF) { lead = s.charAt(s.length - 1); s = s.slice(0, -1); }
          if (s !== '') c.enqueue(enc.encode(s));
        },
        flush: function (c) {
          if (lead !== '') c.enqueue(new Uint8Array([0xEF, 0xBF, 0xBD]));
        }
      });
      TX.set(this, { ts: ts });
    }
    get encoding() { brand(TX, this, 'TextEncoderStream'); return 'utf-8'; }
    get readable() { return brand(TX, this, 'TextEncoderStream').ts.readable; }
    get writable() { return brand(TX, this, 'TextEncoderStream').ts.writable; }
  }

  [WritableStream, WritableStreamDefaultWriter, WritableStreamDefaultController, TransformStream, TransformStreamDefaultController, TextDecoderStream, TextEncoderStream].forEach(function (C) {
    Object.defineProperty(C.prototype, Symbol.toStringTag, { value: C.name, configurable: true });
    Object.defineProperty(globalThis, C.name, { value: C, writable: true, configurable: true });
  });
})();
