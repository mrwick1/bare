//! Tests of `WritableStream`, `TransformStream`, `pipeTo`, `pipeThrough`, `TextDecoderStream` and `TextEncoderStream`
//! (`shims/streams.js`). Real scripts run in a real realm, on a big stack.

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

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Throw(v)) => format!("throw-value:{}", b.it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

/// Run a script, then the microtasks. Gives `out.join('|')`.
fn go(b: &mut Browser, src: &str) -> String {
    let r = js(b, src);
    assert!(!r.starts_with("throw") && !r.starts_with("limit") && !r.starts_with("unsupported"), "{r}");
    b.drain().unwrap();
    js(b, "out.join('|')")
}

#[test]
fn the_globals_exist_before_any_read_and_a_class_can_extend_them() {
    big(|| {
        let mut b = mk();
        // The shape of `assistant-stream`: a class extends TransformStream at the top level of a module.
        assert_eq!(js(&mut b, "['WritableStream' in window, typeof TransformStream, typeof TextDecoderStream, typeof TextEncoderStream].join()"), "true,function,function,function");
        let r = go(&mut b, r#"var out = [];
class Upper extends TransformStream {
  constructor() { super({ transform(c, ctl) { ctl.enqueue(String(c).toUpperCase()); }, flush(ctl) { ctl.enqueue('!'); } }); }
}
var t = new Upper();
out.push(t instanceof TransformStream, t.readable instanceof ReadableStream, t.writable instanceof WritableStream, Object.prototype.toString.call(t));
var src = new ReadableStream({ start(c) { c.enqueue('a'); c.enqueue('b'); c.close(); } });
src.pipeThrough(t).pipeThrough(new TransformStream()).pipeTo(new WritableStream({
  write(x) { out.push('w:' + x); }, close() { out.push('closed'); }
})).then(function () { out.push('done:' + src.locked); });
"#);
        assert_eq!(r, "true|true|true|[object TransformStream]|w:A|w:B|w:!|closed|done:false");
    });
}

#[test]
fn a_writer_follows_backpressure_and_the_queue_strategy() {
    big(|| {
        let mut b = mk();
        let r = go(&mut b, r#"var out = [], done = [];
var ws = new WritableStream({
  write(x) { out.push('w:' + x); return new Promise(function (r) { done.push(r); }); },
  close() { out.push('close'); }
}, { highWaterMark: 4, size: function (c) { return c.length; } });
var w = ws.getWriter();
out.push('locked:' + ws.locked, 'ds:' + w.desiredSize);
w.write('ab'); w.write('cde');
out.push('ds:' + w.desiredSize);
w.ready.then(function () { out.push('ready'); });
try { ws.getWriter(); } catch (e) { out.push(e.name); }
function step() { if (done.length) { done.shift()(); } }
Promise.resolve().then(function () { step(); return Promise.resolve(); }).then(function () { return Promise.resolve(); }).then(function () { step(); });
w.close().then(function () { out.push('closed:' + w.desiredSize); w.releaseLock(); out.push('locked:' + ws.locked); });
"#);
        assert_eq!(r, "locked:true|ds:4|ds:-1|TypeError|w:ab|ready|w:cde|close|closed:0|locked:false");
    });
}

#[test]
fn errors_and_aborts_reach_both_sides() {
    big(|| {
        let mut b = mk();
        let r = go(&mut b, r#"var out = [];
// The writer's `ready` and `closed` are marked as handled: only the control rejection is reported.
addEventListener('unhandledrejection', function (e) { out.push('unhandled:' + e.reason); });
Promise.reject('control');
var t = new TransformStream({ transform() { throw new Error('boom'); } });
var w = t.writable.getWriter();
w.write(1).catch(function (e) { out.push('write:' + e.message); });
t.readable.getReader().read().catch(function (e) { out.push('read:' + e.message); });
var ws = new WritableStream({ abort(r) { out.push('abort:' + r); } });
ws.abort('why').then(function () { out.push('aborted'); });
var ac = new AbortController();
new ReadableStream({ cancel(r) { out.push('cancel:' + r); } }).pipeTo(new WritableStream(), { signal: ac.signal }).catch(function (e) { out.push('pipe:' + e); });
ac.abort('stop');
"#);
        // Node gives the same lines; the order of lines from separate streams differs by a microtask, so it is not checked.
        let mut lines: Vec<&str> = r.split('|').collect();
        lines.sort();
        assert_eq!(lines.join("|"), "abort:why|aborted|cancel:stop|pipe:stop|read:boom|unhandled:control|write:boom");
    });
}

#[test]
fn text_streams_decode_a_response_body_and_keep_split_characters() {
    big(|| {
        let mut b = mk();
        let r = go(&mut b, r#"var out = [];
function all(rs) {
  var r = rs.getReader(), s = '';
  return r.read().then(function next(x) { if (x.done) return s; s += x.value; return r.read().then(next); });
}
all(new Response('héllo').body.pipeThrough(new TextDecoderStream())).then(function (s) { out.push('body:' + s); });
var src = new ReadableStream({ start(c) { c.enqueue('aé'); c.enqueue('\ud83d'); c.enqueue('\ude00b'); c.close(); } });
all(src.pipeThrough(new TextEncoderStream()).pipeThrough(new TextDecoderStream('utf-8'))).then(function (s) { out.push('round:' + s + ':' + s.length); });
var d = new TextDecoderStream();
out.push(d.encoding, d.fatal, new TextEncoderStream().encoding);
"#);
        assert_eq!(r, "utf-8|false|utf-8|body:h\u{e9}llo|round:a\u{e9}\u{1F600}b:5");
    });
}

#[test]
fn bad_arguments_are_type_errors() {
    big(|| {
        let mut b = mk();
        // pipeTo is called before any stream name is read: the first call makes the shim.
        let r = go(&mut b, "var out = []; new ReadableStream({}).pipeTo({}).catch(function (e) { out.push(e.name); });");
        assert_eq!(r, "TypeError");
        assert!(js(&mut b, "new ReadableStream({}).pipeThrough({})").starts_with("throw:TypeError"));
        assert!(js(&mut b, "new WritableStream({ write: 1 })").starts_with("throw:TypeError"));
        assert!(js(&mut b, "new WritableStream({}, { highWaterMark: -1 })").starts_with("throw:RangeError"));
        assert!(js(&mut b, "new TransformStream({ readableType: 'bytes' })").starts_with("throw:RangeError"));
        assert!(js(&mut b, "new WritableStreamDefaultController()").starts_with("throw:TypeError"));
        assert!(js(&mut b, "WritableStream.prototype.getWriter.call({})").starts_with("throw:TypeError"));
        // A size function on the readable side is the gate of the host stream.
        assert_eq!(js(&mut b, "new TransformStream({}, undefined, { size: function () { return 1; } })"), "unsupported:ReadableStream strategy.size (custom queuing strategy)");
    });
}

#[test]
fn the_readable_side_keeps_the_host_limits() {
    big(|| {
        let mut b = mk();
        b.configure_streams(StreamLimits { max_queue_chunks: 8, ..Default::default() });
        let r = js(&mut b, "new TransformStream({ start(c) { for (var i = 0; i < 100; i++) c.enqueue(i); } })");
        assert_eq!(r, format!("limit:{:?}", LimitKind::Host("stream-queue")));
    });
}
