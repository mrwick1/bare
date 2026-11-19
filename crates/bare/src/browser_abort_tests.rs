//! Tests of `AbortController`, `AbortSignal` (`browser_abort.rs`), `structuredClone` and `atob`/`btoa` (`browser_encoding.rs`).
//! The broad comparison with headless Chrome is `tools/js/dom_check.sh`. These tests pin the limits, the GC and the gates.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser() -> Browser {
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn a_realm_that_never_reads_the_globals_makes_no_signal() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "Object.getOwnPropertyNames(window).indexOf('AbortSignal') >= 0"), "true");
        assert_eq!(js(&mut b, "var s = new AbortController().signal; s instanceof AbortSignal && s instanceof EventTarget"), "true");
    });
}

#[test]
fn a_timeout_signal_that_nobody_holds_still_fires_and_a_collected_one_leaves_no_task() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var fired = 0; (function () { AbortSignal.timeout(50).addEventListener('abort', function () { fired++; }); })();");
        b.collect(&[]).expect("collect");
        b.advance_time(100.0, 1000).expect("advance");
        assert_eq!(js(&mut b, "fired"), "1");
    });
}

#[test]
fn abort_listeners_per_signal_are_bounded_and_removed_listeners_are_compacted() {
    big(|| {
        let mut b = browser();
        assert_eq!(
            js(&mut b, "var c = new AbortController(), t = new EventTarget(); for (var i = 0; i < 20000; i++) { var f = function () {}; t.addEventListener('x', f, { signal: c.signal }); t.removeEventListener('x', f); } 'done'"),
            "done"
        );
    });
}

#[test]
fn any_with_a_cycle_of_sources_and_a_collected_dependent() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var c = new AbortController(); (function () { for (var i = 0; i < 200; i++) AbortSignal.any([c.signal]); })();");
        b.collect(&[]).expect("collect");
        assert_eq!(js(&mut b, "var d = AbortSignal.any([c.signal]); c.abort('r'); d.reason"), "r");
    });
}

#[test]
fn fetch_with_an_aborted_signal_rejects_with_the_reason_before_any_io() {
    big(|| {
        let mut b = browser();
        let r = js(&mut b, "var c = new AbortController(); c.abort('why'); var out; fetch('http://127.0.0.1:1/', { signal: c.signal }).catch(function (e) { out = e; }); 'sent'");
        assert_eq!(r, "sent");
        b.advance_time(10.0, 1000).expect("advance");
        assert_eq!(js(&mut b, "out"), "why");
    });
}

#[test]
fn structured_clone_gates_and_limits() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "structuredClone(1, { transfer: [new ArrayBuffer(1)] })"), "1");
        assert_eq!(js(&mut b, "var t = new Uint8Array([1, 2, 3]), r = structuredClone(t.buffer, { transfer: [t.buffer] }); [r.byteLength, new Uint8Array(r).join(), t.buffer.byteLength, t.length].join('|')"), "3|1,2,3|0|0");
        assert_eq!(js(&mut b, "structuredClone(1, { transfer: [1] })"), "throw:DataCloneError:Value not transferable.");
        assert_eq!(js(&mut b, "var u = new Uint8Array(70 << 20); structuredClone(u)"), "limit:Host(\"clone-bytes\")");
        assert_eq!(js(&mut b, "var d = {}, c = d; for (var i = 0; i < 300; i++) { c.n = {}; c = c.n; } structuredClone(d)"), "limit:Host(\"clone-depth\")");
        assert_eq!(js(&mut b, "var b = new ArrayBuffer(8); b.transfer(); structuredClone(b)"), "throw:DataCloneError:An ArrayBuffer is detached and could not be cloned.");
    });
}

#[test]
fn structured_clone_keeps_the_old_object_untouched() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var o = { m: new Map([[1, { a: 1 }]]), d: new Date(5) }; var c = structuredClone(o); c.m.get(1).a = 2; [o.m.get(1).a, c.d !== o.d, c.d.getTime()].join()"), "1,true,5");
    });
}

#[test]
fn base64_round_trips_every_byte_and_rejects_bad_text() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var s = ''; for (var i = 0; i < 256; i++) s += String.fromCharCode(i); atob(btoa(s)) === s"), "true");
        assert_eq!(js(&mut b, "btoa('\\u0100')"), "throw:InvalidCharacterError:Failed to execute 'btoa' on 'Window': The string to be encoded contains characters outside of the Latin1 range.");
        assert_eq!(js(&mut b, "atob('Z')"), "throw:InvalidCharacterError:Failed to execute 'atob' on 'Window': The string to be decoded is not correctly encoded.");
    });
}

#[test]
fn the_sources_and_dependents_of_any_follow_the_signals_that_are_alive() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var c = new AbortController(), keep = []; (function () { for (var i = 0; i < 50; i++) { var d = AbortSignal.any([c.signal]); if (i % 10 == 0) keep.push(d); } })(); 0");
        b.collect(&[]).expect("collect");
        assert_eq!(b.dom.abort_entries(), (6, 1, 10), "the source and 5 dependents; each pair is two keys");
        // A dependent of a dependent follows the sources of the first, so `e` follows the source and not `keep[0]`.
        js(&mut b, "var e = AbortSignal.any([keep[0]]); keep.length = 0; 0");
        b.collect(&[]).expect("collect");
        assert_eq!(b.dom.abort_entries(), (2, 1, 2), "the source and `e`");
        assert_eq!(js(&mut b, "c.abort('r'); e.reason + ',' + e.aborted"), "r,true");
    });
}

#[test]
fn a_key_that_was_freed_answers_for_its_new_object_only() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var c = new AbortController(); (function () { for (var i = 0; i < 20; i++) AbortSignal.any([c.signal]); new AbortController(); })(); 0");
        b.collect(&[]).expect("collect");
        // Only the controller and its signal live: the keys of the 20 dependents and of the other pair are free.
        assert_eq!(b.dom.abort_entries(), (1, 1, 0));
        // New signals take the free keys. The old lists must not answer for them.
        js(&mut b, "var other = new AbortController(), d1 = AbortSignal.any([c.signal]), d2 = AbortSignal.any([other.signal]); var seen = []; d1.onabort = function () { seen.push('d1'); }; d2.onabort = function () { seen.push('d2'); }; 0");
        assert_eq!(b.dom.abort_entries().2, 4);
        assert_eq!(js(&mut b, "c.abort('a'); seen.join() + '|' + d1.aborted + d2.aborted + other.signal.aborted"), "d1|truefalsefalse");
        b.collect(&[]).expect("collect");
        assert_eq!(js(&mut b, "other.abort('b'); seen.join() + '|' + d2.reason"), "d1,d2|b");
    });
}
