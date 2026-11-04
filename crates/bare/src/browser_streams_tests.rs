//! Tests of the script-made default `ReadableStream` (`browser_streams.rs`). Real scripts run in a real realm. The pure
//! semantics also have a Node oracle (`tools/js/streams_oracle.mjs`); these tests cover what Node cannot: limits, collection
//! while reads are pending, fatal errors, host-only gates and intrinsic Promise functions. Each test runs on a big stack.

use crate::browser::*;
use crate::browser_streams::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn mk() -> Browser {
    Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 20_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn mk_with(l: StreamLimits) -> Browser {
    let mut b = mk();
    b.configure_streams(l);
    b
}

fn show(b: &mut Browser, r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Throw(v)) => format!("throw-value:{}", b.it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src);
    show(b, r)
}

/// Run a script, then the microtasks. Gives `out.join('|')`.
fn go(b: &mut Browser, src: &str) -> String {
    let r = js(b, src);
    assert!(!r.starts_with("throw") && !r.starts_with("limit") && !r.starts_with("unsupported"), "{r}");
    b.drain().unwrap();
    js(b, "out.join('|')")
}

fn lim(name: &'static str) -> String {
    format!("limit:{:?}", LimitKind::Host(name))
}

#[test]
fn a_flight_shaped_source_feeds_a_reader_loop_with_asynchronous_callbacks() {
    big(|| {
        let mut b = mk();
        // The source starts asynchronously (a promise), enqueues TextEncoder chunks from promise jobs in several turns,
        // and closes. The consumer is the `reader.read().then(progress)` loop. The rows are split in the middle of a row.
        let src = r#"
var out = [], enc = new TextEncoder(), dec = new TextDecoder(), parts = ['0:["$","di', 'v",null,{}]\n1:I[', '"c",[],"d"]\n', '2:"tail"\n'];
var rs = new ReadableStream({
  start(c) {
    var i = 0;
    return Promise.resolve().then(function next() {
      if (i < parts.length) { c.enqueue(enc.encode(parts[i++])); return Promise.resolve().then(next); }
      c.close();
    });
  }
});
var reader = rs.getReader(), text = '', chunks = 0;
function progress(x) {
  if (x.done) { out.push('end:' + chunks + ':' + text.length); out.push(text); return; }
  chunks++;
  text += dec.decode(x.value, { stream: true });
  return reader.read().then(progress);
}
reader.read().then(progress);
"#;
        let r = go(&mut b, src);
        assert_eq!(r, "end:4:47|0:[\"$\",\"div\",null,{}]\n1:I[\"c\",[],\"d\"]\n2:\"tail\"\n");
        assert_eq!(b.stream_stats().pending_reads, 0);
        assert_eq!(b.stream_stats().queued_chunks, 0);
    });
}

#[test]
fn collection_while_reads_are_pending_keeps_the_chain_and_the_chunks() {
    big(|| {
        let mut b = mk();
        // Only the controller is kept (by a global). The stream, the reader and the read promises hang on it.
        js(&mut b, r#"var out = [], ctl;
(function () {
  var rs = new ReadableStream({ start(c) { ctl = c; } });
  var r = rs.getReader();
  r.read().then(function (x) { out.push('a:' + x.value.n); return r.read(); }).then(function (x) { out.push('b:' + x.value.n); });
  r.read().then(function (x) { out.push('c:' + x.value.n); });
})();"#);
        assert_eq!(b.stream_stats().pending_reads, 2);
        for _ in 0..3 {
            b.collect(&[]).unwrap();
        }
        assert_eq!((b.stream_stats().streams, b.stream_stats().readers, b.stream_stats().pending_reads), (1, 1, 2));
        // Chunks that sit in the queue (not fulfilled yet) also live through a collection.
        js(&mut b, "ctl.enqueue({ n: 1 }); ctl.enqueue({ n: 2 }); ctl.enqueue({ n: 3 }); ctl.enqueue({ n: 4 });");
        assert_eq!(b.stream_stats().queued_chunks, 2);
        b.collect(&[]).unwrap();
        b.collect(&[]).unwrap();
        b.drain().unwrap();
        assert_eq!(js(&mut b, "out.join('|')"), "a:1|c:2|b:3");
        assert_eq!(b.stream_stats().queued_chunks, 1);
        js(&mut b, "ctl.close(); var rd = null;");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "typeof ctl.enqueue"), "function");
    });
}

#[test]
fn a_queue_of_chunks_survives_collection_and_unreachable_streams_are_dropped() {
    big(|| {
        let mut b = mk();
        js(&mut b, r#"var out = [], keep;
keep = new ReadableStream({ start(c) { for (var i = 0; i < 5; i++) c.enqueue({ i: i, s: 'chunk' + i, u: new Uint8Array(100) }); c.close(); } }, { highWaterMark: 10 });
var keep2 = new ReadableStream({ start(c) { for (var i = 0; i < 5; i++) c.enqueue(new Uint8Array(100)); } }, { highWaterMark: 10 });
for (var k = 0; k < 20; k++) new ReadableStream({ start(c) { c.enqueue(new Uint8Array(1000)); } }, { highWaterMark: 4 });
"#);
        assert_eq!(b.stream_stats().streams, 22);
        assert_eq!(b.stream_stats().queued_chunks, 30);
        assert_eq!(b.stream_stats().queued_bytes, 20 * 1000 + 5 * 100);
        // The start reactions of the new streams are still queued jobs, and they hold their controllers.
        b.collect(&[]).unwrap();
        assert_eq!(b.stream_stats().streams, 22, "queued start jobs keep their streams");
        b.drain().unwrap();
        js(&mut b, "0");
        b.collect(&[]).unwrap();
        let s = b.stream_stats();
        assert_eq!((s.streams, s.queued_chunks, s.queued_bytes), (2, 10, 500), "{s:?}");
        let r = go(&mut b, r#"var r = keep.getReader(), got = [];
function step() { return r.read().then(function (x) { if (x.done) { out.push(got.join(',')); return; } got.push(x.value.s + ':' + x.value.u.length); return step(); }); }
step();"#);
        assert_eq!(r, "chunk0:100,chunk1:100,chunk2:100,chunk3:100,chunk4:100");
        assert_eq!(b.stream_stats().queued_bytes, 500, "the other stream still holds its chunks");
        js(&mut b, "keep2 = null");
        b.collect(&[]).unwrap();
        assert_eq!(b.stream_stats().queued_bytes, 0);
    });
}

#[test]
fn the_pull_cycle_runs_once_at_a_time_and_follows_backpressure() {
    big(|| {
        let mut b = mk();
        // The pull is asynchronous. It must never run twice at the same time, and the queue never grows past the mark
        // by more than the one chunk that a pull adds.
        let r = go(&mut b, r#"var out = [], n = 0, active = 0, maxActive = 0, maxQueue = 0, pending = [], ctl;
var rs = new ReadableStream({
  start(c) { ctl = c; },
  pull(c) { n++; active++; maxActive = Math.max(maxActive, active);
    return Promise.resolve().then(function () { c.enqueue(n); maxQueue = Math.max(maxQueue, 2 - c.desiredSize); active--; }); }
}, { highWaterMark: 2 });
var r = rs.getReader(), total = 0;
function step() { return r.read().then(function (x) { total += x.value; return n < 50 ? step() : undefined; }); }
step().then(function () { out.push('maxActive:' + maxActive, 'maxQueue<=3:' + (maxQueue <= 3), 'total>0:' + (total > 0)); });"#);
        assert_eq!(r, "maxActive:1|maxQueue<=3:true|total>0:true");
    });
}

#[test]
fn promise_machinery_survives_deleting_or_replacing_globals() {
    big(|| {
        let mut b = mk();
        // The script keeps its own handles first, then breaks the globals the stream layer could have used.
        let r = go(&mut b, r#"var out = [], P = Promise, resolved = Promise.resolve.bind(Promise);
var boom = new Error('x');
var rs = new ReadableStream({
  start(c) { return resolved().then(function () { c.enqueue('s'); }); },
  pull(c) { c.enqueue('p'); return resolved(); },
  cancel(r) { out.push('cancel:' + r); }
}, { highWaterMark: 0 });
var r = rs.getReader();
var thenFn = Promise.prototype.then;
Promise.prototype.then = function () { out.push('SPOOF-then'); throw boom; };
Function.prototype.bind = function () { out.push('SPOOF-bind'); throw boom; };
Function.prototype.call = function () { out.push('SPOOF-call'); throw boom; };
delete Promise.resolve; delete Promise.prototype.then;
delete globalThis.Promise;
var first = r.read();
Object.defineProperty(Object.prototype, 'then', { value: undefined, configurable: true, writable: true });
P.prototype.constructor = P;
first.then = undefined;
var ok = Object.getPrototypeOf(first) === P.prototype;
out.push('proto:' + ok);
function T(p, f) { p.then = thenFn; return p.then(f); }
// No handler returns a promise: assimilating one would read the deleted `then`, as in any engine.
T(first, function (x) { out.push('first:' + x.value); T(r.read(), function (x) { out.push('second:' + x.value); T(r.cancel('end'), function () { out.push('cancelled'); }); }); });"#);
        assert!(!r.contains("SPOOF"), "{r}");
        assert_eq!(r, "proto:true|first:s|second:p|cancel:end|cancelled", "{r}");
    });
}

#[test]
fn explicit_gates_are_unsupported_and_nothing_is_ignored() {
    big(|| {
        let mut b = mk();
        for (src, what) in [
            ("new ReadableStream({ type: 'bytes' })", "ReadableStream type: \"bytes\" (byte streams and BYOB)"),
            ("new ReadableStream({ autoAllocateChunkSize: 10 })", "ReadableStream autoAllocateChunkSize (byte streams)"),
            ("new ReadableStream({}, { size: function () { return 1; } })", "ReadableStream strategy.size (custom queuing strategy)"),
            ("new ReadableStream({}).getReader({ mode: 'byob' })", "ReadableStream BYOB reader"),
            ("new ReadableStream({}).getReader().closed", "ReadableStreamDefaultReader.closed"),
            ("new ReadableStream({}).tee()", "ReadableStream tee, pipeTo, pipeThrough and async iteration"),
            ("new ReadableStream({}).pipeTo({})", "ReadableStream tee, pipeTo, pipeThrough and async iteration"),
            ("new ReadableStream({}).pipeThrough({})", "ReadableStream tee, pipeTo, pipeThrough and async iteration"),
        ] {
            let mut b2 = mk();
            assert_eq!(js(&mut b2, src), format!("unsupported:{what}"), "{src}");
        }
        // A size that is not callable is a TypeError (as in the standard), not a gate.
        assert!(js(&mut b, "new ReadableStream({}, { size: 3 })").starts_with("throw:TypeError"));
        // A stream as the body of a Response is still a gate (see browser_fetch.rs).
        assert!(js(&mut b, "new Response(new ReadableStream({}))").starts_with("unsupported:"));
        // The controller and the reader cannot be built by script.
        assert_eq!(js(&mut b, "typeof ReadableStreamDefaultController"), "function");
        assert!(js(&mut b, "new ReadableStreamDefaultController()").starts_with("throw:TypeError"));
        assert!(js(&mut b, "new ReadableStreamDefaultReader(new ReadableStream({}))").starts_with("throw:TypeError"));
    });
}

#[test]
fn brand_checks_and_response_bodies_coexist() {
    big(|| {
        let mut b = mk();
        let r = go(&mut b, r#"var out = [];
function t(label, f) { try { f(); out.push(label + ':ok'); } catch (e) { out.push(label + ':' + e.name); } }
var rs = new ReadableStream({}), rd = rs.getReader(), ctl;
new ReadableStream({ start(c) { ctl = c; } });
var P = ReadableStream.prototype, R = ReadableStreamDefaultReader.prototype, C = ReadableStreamDefaultController.prototype;
t('locked-this-obj', function () { Object.getOwnPropertyDescriptor(P, 'locked').get.call({}); });
t('getReader-this-reader', function () { P.getReader.call(rd); });
R.read.call(rs).then(function () { out.push('no'); }, function (e) { out.push('read-this-stream:' + e.name); });
t('enqueue-this-stream', function () { C.enqueue.call(rs, 1); });
t('enqueue-this-undefined', function () { C.enqueue.call(undefined, 1); });
t('desired-this-reader', function () { Object.getOwnPropertyDescriptor(C, 'desiredSize').get.call(rd); });
var body = new Response('abc').body, br = body.getReader();
out.push('resp-locked:' + body.locked, 'stream-locked:' + rs.locked);
br.read().then(function (x) { out.push('resp:' + x.value.length + x.done); return br.read(); }).then(function (x) { out.push('resp-done:' + x.done); });
// A custom stream's method on a response body, and the reverse, is a brand error, not a mixup.
t('mine-on-resp', function () { P.getReader.call(new Response('x').body); });
var cr = new ReadableStream({ start(c) { c.enqueue('z'); } }).getReader();
cr.read().then(function (x) { out.push('custom:' + x.value); });"#);
        assert!(r.starts_with("locked-this-obj:TypeError|getReader-this-reader:TypeError|enqueue-this-stream:TypeError|enqueue-this-undefined:TypeError|desired-this-reader:TypeError|"), "{r}");
        assert!(r.contains("resp-locked:true|stream-locked:true"), "{r}");
        assert!(r.contains("mine-on-resp:ok") && r.contains("read-this-stream:TypeError"), "{r}");
        assert!(r.ends_with("resp:3false|custom:z|resp-done:true"), "{r}");
    });
}

#[test]
fn throwing_callbacks_follow_the_standard_and_leave_no_entries() {
    big(|| {
        let mut b = mk();
        let r = go(&mut b, r#"var out = [];
for (var i = 0; i < 50; i++) { try { new ReadableStream({ start(c) { c.enqueue(new Uint8Array(1000)); throw 'x' + i; } }); } catch (e) { if (i === 49) out.push(e); } }
var a = new ReadableStream({ pull() { throw new RangeError('pull'); } });
a.getReader().read().then(function () { out.push('no'); }, function (e) { out.push('pull:' + e.name); });
var c = new ReadableStream({ cancel() { throw new SyntaxError('cancel'); } });
c.cancel().then(function () { out.push('no'); }, function (e) { out.push('cancel:' + e.name); });"#);
        assert_eq!(r, "x49|cancel:SyntaxError|pull:RangeError");
        // Unreachable failed constructions are pruned; an escaped controller would keep its state.
        b.collect(&[]).unwrap();
        let s = b.stream_stats();
        assert_eq!((s.streams, s.queued_bytes), (2, 0), "{s:?}");
    });
}

#[test]
fn reentrant_calls_from_callbacks_do_not_break_the_state() {
    big(|| {
        let mut b = mk();
        let r = go(&mut b, r#"var out = [], ctl, rd, n = 0;
var rs = new ReadableStream({
  start(c) { ctl = c; c.enqueue('s1'); c.enqueue('s2'); },
  pull(c) {
    n++;
    if (n === 1) { rd.read().then(function (x) { out.push('inner:' + x.value); }); }
    if (n === 2) { try { c.close(); c.enqueue('late'); } catch (e) { out.push('enq:' + e.name); } }
    if (n === 3) { rd.releaseLock(); }
    c.enqueue('p' + n);
  },
  cancel() { out.push('cancel'); }
}, { highWaterMark: 2 });
rd = rs.getReader();
rd.read().then(function (x) { out.push('outer:' + x.value); });
rd.read().then(function (x) { out.push('outer2:' + x.value); });
Promise.resolve().then(function () {}).then(function () {}).then(function () {}).then(function () { out.push('n:' + n); });"#);
        assert!(r.contains("outer:s1") && r.contains("outer2:s2"), "{r}");
        // A pull that closes and then enqueues sees a TypeError, and enqueue-after-close inside the pull throws once.
        assert!(!r.contains("late"), "{r}");
        let _ = b.stream_stats();
        // A pull that cancels its own stream, a cancel that errors it, and an error inside start.
        let r2 = go(&mut b, r#"var out = [], rs, n = 0;
rs = new ReadableStream({ pull(c) { n++; rs.cancel('self').then(function () { out.push('cancelled'); }); }, cancel(r) { out.push('src:' + r); } }, { highWaterMark: 1 });
rs2 = new ReadableStream({ start(c) { c.error('E'); c.enqueue; } });
rs2.getReader().read().then(function () { out.push('no'); }, function (e) { out.push('err:' + e); });
Promise.resolve().then(function () {}).then(function () {}).then(function () { out.push('pulls:' + n); });"#);
        assert_eq!(r2, "src:self|err:E|cancelled|pulls:1");
    });
}

#[test]
fn limits_end_the_call_fatally_and_no_script_callback_runs_after() {
    big(|| {
        let tiny = StreamLimits { max_streams: 3, max_readers: 2, max_queue_chunks: 4, max_queued_bytes: 300, max_pending_reads: 3, max_callbacks: 6 };
        // Queue chunk cap, and it is not catchable by the script.
        let mut b = mk_with(tiny);
        let r = js(&mut b, "var calls = 0, caught = 'no'; var rs; try { rs = new ReadableStream({ start(c) { calls++; for (var i = 0; i < 10; i++) c.enqueue(i); } }, { highWaterMark: 100 }); } catch (e) { caught = 'yes'; } caught");
        assert_eq!(r, lim("stream-queue"));
        assert_eq!(js(&mut b, "caught"), "no");
        // Fatal for good: the same error from every entry point, and no callback runs.
        assert_eq!(js(&mut b, "new ReadableStream({ start() { calls++; } })"), lim("stream-queue"));
        assert_eq!(js(&mut b, "calls"), "1");
        // Bytes cap.
        let mut b = mk_with(tiny);
        assert_eq!(js(&mut b, "new ReadableStream({ start(c) { c.enqueue(new Uint8Array(200)); c.enqueue(new Uint8Array(101)); } }, { highWaterMark: 9 })"), lim("stream-bytes"));
        // The bytes cap counts typed-array bytes of all streams, and a read gives them back.
        let mut b = mk_with(tiny);
        assert_eq!(js(&mut b, "var out = [], r = new ReadableStream({ start(c) { c.enqueue(new Uint8Array(250)); } }, { highWaterMark: 9 }).getReader(); r.read(); 1"), "1");
        b.drain().unwrap();
        assert_eq!(b.stream_stats().queued_bytes, 0);
        assert_eq!(js(&mut b, "new ReadableStream({ start(c) { c.enqueue(new Uint8Array(250)); } }, { highWaterMark: 9 }); 2"), "2");
        // Pending read cap.
        let mut b = mk_with(tiny);
        assert_eq!(js(&mut b, "var r = new ReadableStream({}).getReader(); r.read(); r.read(); r.read(); 1"), "1");
        assert_eq!(js(&mut b, "r.read()"), lim("stream-reads"));
        assert_eq!(js(&mut b, "r.read()"), lim("stream-reads"));
        // Streams cap and readers cap.
        let mut b = mk_with(tiny);
        assert_eq!(js(&mut b, "new ReadableStream({}); new ReadableStream({}); new ReadableStream({}); 1"), "1");
        assert_eq!(js(&mut b, "new ReadableStream({})"), lim("stream-objects"));
        let mut b = mk_with(tiny);
        assert_eq!(js(&mut b, "var a = new ReadableStream({}), b2 = new ReadableStream({}), c3 = new ReadableStream({}); a.getReader(); b2.getReader(); 1"), "1");
        assert_eq!(js(&mut b, "c3.getReader()"), lim("stream-readers"));
        // A collection gives room again for the streams that nobody holds.
        let mut b = mk_with(tiny);
        js(&mut b, "for (var i = 0; i < 3; i++) new ReadableStream({});");
        b.drain().unwrap();
        js(&mut b, "0");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "for (var i = 0; i < 3; i++) new ReadableStream({}); 'ok'"), "ok");
        // Callback cap: 6 calls of start/pull/cancel.
        let mut b = mk_with(StreamLimits { max_streams: 100, ..tiny });
        let r = js(&mut b, "var n = 0; for (var i = 0; i < 10; i++) new ReadableStream({ start() { n++; } }); n");
        assert_eq!(r, lim("stream-callbacks"));
        assert_eq!(js(&mut b, "n"), "6");
        // A limit from a pull callback in a job is fatal too: later pulls do not run.
        let mut b = mk_with(StreamLimits { max_callbacks: 3, ..tiny });
        js(&mut b, "var pulls = 0, r = new ReadableStream({ pull(c) { pulls++; c.enqueue(1); } }, { highWaterMark: 0 }).getReader(); function loop() { return r.read().then(loop); } loop();");
        let e = b.drain();
        assert!(matches!(e, Err(Error::Limit(LimitKind::Host("stream-callbacks")))), "{e:?}");
        assert_eq!(js(&mut b, "pulls"), "3");
        assert!(matches!(b.drain(), Err(Error::Limit(_))), "the failed checkpoint is sticky");
        assert_eq!(js(&mut b, "pulls"), "3");
    });
}

#[test]
fn a_step_budget_error_inside_a_callback_is_fatal_and_charged() {
    big(|| {
        // The cost of chunks is charged: 1 step and 1 step per 64 bytes, so big chunks end a call with a small budget.
        let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 2_000, ..Default::default() }, HeapLimits::default()).unwrap();
        let r = js(&mut b, "var c0; new ReadableStream({ start(c) { c0 = c; } }); for (var i = 0; i < 100; i++) c0.enqueue(new Uint8Array(64 * 1024)); 1");
        assert!(r.starts_with("limit:"), "{r}");
        // A limit inside a user callback marks the layer failed.
        let mut b = mk();
        let r = js(&mut b, "var calls = 0; new ReadableStream({ start() { (function f() { f(); })(); } })");
        assert!(r.starts_with("limit:"), "{r}");
        assert!(js(&mut b, "calls++; new ReadableStream({ start() { calls++; } })").starts_with("limit:"));
        assert_eq!(js(&mut b, "calls"), "1");
    });
}

#[test]
fn deep_recursion_through_stream_callbacks_hits_the_call_limit_not_the_stack() {
    big(|| {
        let mut b = mk();
        let r = js(&mut b, "var depth = 0; function mkr() { depth++; new ReadableStream({ start() { mkr(); } }); } mkr();");
        assert!(r.starts_with("limit:"), "{r}");
        assert!(js(&mut b, "depth").parse::<u32>().unwrap() > 10);
    });
}

#[test]
fn object_chunks_keep_their_identity_and_pull_may_be_a_getter() {
    big(|| {
        let mut b = mk();
        let r = go(&mut b, r#"var out = [], o = { a: 1 }, src = {};
Object.defineProperty(src, 'start', { get() { out.push('get-start'); return function (c) { c.enqueue(o); c.close(); }; } });
Object.defineProperty(src, 'pull', { get() { out.push('get-pull'); return undefined; } });
Object.defineProperty(src, 'cancel', { get() { out.push('get-cancel'); return undefined; } });
Object.defineProperty(src, 'type', { get() { out.push('get-type'); return undefined; } });
Object.defineProperty(src, 'autoAllocateChunkSize', { get() { out.push('get-auto'); return undefined; } });
var rs = new ReadableStream(src), r = rs.getReader();
r.read().then(function (x) { out.push('same:' + (x.value === o)); });"#);
        assert_eq!(r, "get-auto|get-cancel|get-pull|get-start|get-type|same:true");
    });
}

#[test]
fn null_dictionaries_and_invalid_reader_modes_follow_conversion_rules() {
    big(|| {
        let mut b = mk();
        assert_eq!(js(&mut b, "var s = new ReadableStream({}, null); var r = s.getReader(null); s.locked"), "true");
        assert_eq!(js(&mut b, "try { new ReadableStream().getReader({ mode: 'other' }); } catch (e) { e.name; }"), "TypeError");
        assert_eq!(js(&mut b, "try { new ReadableStream().getReader(42); } catch (e) { e.name; }"), "TypeError");
    });
}

#[test]
fn an_escaped_controller_keeps_its_state_after_start_throws() {
    big(|| {
        let mut b = mk();
        assert_eq!(js(&mut b, "var ctl; try { new ReadableStream({ start(c) { ctl = c; throw 1; } }); } catch (e) {} ctl.enqueue('kept'); ctl.desiredSize"), "0");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "ctl.close(); ctl.desiredSize"), "0");
        assert_eq!(b.stream_stats().queued_chunks, 1);
        assert_eq!(js(&mut b, "ctl = null; 0"), "0");
        b.collect(&[]).unwrap();
        assert_eq!(b.stream_stats().streams, 0);
    });
}

#[test]
fn start_result_assimilation_does_not_read_constructor_synchronously() {
    big(|| {
        let mut b = mk();
        let result = go(&mut b, "var out = [], ctl; var p = Promise.resolve(); Object.defineProperty(p, 'constructor', { get() { out.push('constructor'); throw 17; } }); var s = new ReadableStream({ start(c) { ctl = c; return p; } }); out.push('initial:' + ctl.desiredSize); s.getReader().read().catch(function (e) { out.push('error:' + e); });");
        assert_eq!(result, "initial:1|constructor|error:17");
    });
}

#[test]
fn unsupported_pipe_operations_poison_the_custom_stream_layer() {
    big(|| {
        let mut b = mk();
        assert_eq!(js(&mut b, "var ctl, s = new ReadableStream({ start(c) { ctl = c; } }); 0"), "0");
        let first = js(&mut b, "s.tee()");
        assert!(first.starts_with("unsupported:"));
        assert_eq!(js(&mut b, "ctl.enqueue('must not queue')"), first);
        assert_eq!(b.stream_stats().queued_chunks, 0);
    });
}

#[test]
fn timer_sources_and_aggregate_reads_keep_weak_controller_edges_through_collection() {
    big(|| {
        let mut b = mk();
        b.use_virtual_clock();
        assert_eq!(js(&mut b, "var key = {}, w = new WeakMap(), reader; (function () { var s = new ReadableStream({ start(c) { w.set(key, c); } }); reader = s.getReader(); })(); Promise.all([reader.read(), reader.read()]).then(function (items) { document.body.textContent = items[0].value + items[1].value; }); reader = null; setTimeout(function () { var c = w.get(key); c.enqueue(4); c.enqueue(6); c.close(); key = null; }, 0); 0"), "0");
        b.collect(&[]).unwrap();
        assert_eq!(b.stream_stats().pending_reads, 2);
        assert_eq!(b.run_scheduled_tasks(1).unwrap().ran, 1);
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "document.body.textContent"), "10");
        assert_eq!(b.it.weak_tracked(), (1, 0));
        assert_eq!(b.it.agg_stats(), Default::default());
        assert_eq!(b.stream_stats().streams, 0);
    });
}

#[test]
fn byob_gate_does_not_replace_receiver_brand_validation() {
    big(|| {
        let mut b = mk();
        assert_eq!(js(&mut b, "try { ReadableStream.prototype.getReader.call({}, { mode: 'byob' }); } catch (e) { e.name; }"), "TypeError");
        assert_eq!(js(&mut b, "new Response('x').body.getReader(null) instanceof ReadableStreamDefaultReader"), "true");
    });
}
