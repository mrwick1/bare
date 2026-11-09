//! Tests of `window.console` and its bounded sink (`browser_console.rs`). Real scripts run in a real realm. All inputs are
//! public throwaway strings. No test prints a logged value: failures show counts and fixed words only.

use crate::browser::*;
use crate::browser_console::*;
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
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}", b.it.get_str(v, "name")),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(e) => format!("other:{e:?}"),
    }
}

fn lim(name: &'static str) -> String {
    format!("limit:{:?}", LimitKind::Host(name))
}

/// Every record as `L:text` (`L`, `W`, `E`), through the formatter.
fn lines(b: &mut Browser) -> Vec<String> {
    b.take_console_records()
        .into_iter()
        .map(|r| {
            let c = match r.level { ConsoleLevel::Log => 'L', ConsoleLevel::Warn => 'W', ConsoleLevel::Error => 'E' };
            format!("{c}:{}", b.format_console_record(&r).unwrap())
        })
        .collect()
}

#[test]
fn console_exists_with_the_observed_methods_and_descriptors() {
    big(|| {
        let mut b = mk();
        assert_eq!(js(&mut b, "typeof window.console + ',' + typeof console.error + ',' + typeof console.warn + ',' + typeof console.log"), "object,function,function,function");
        let d = "var d = Object.getOwnPropertyDescriptor(window, 'console'); d.writable + ',' + d.enumerable + ',' + d.configurable";
        assert_eq!(js(&mut b, d), "true,false,true");
        let m = "['log','warn','error'].map(function (k) { var d = Object.getOwnPropertyDescriptor(console, k); return [d.writable, d.enumerable, d.configurable, console[k].length, console[k].name].join(); }).join('|')";
        assert_eq!(js(&mut b, m), "true,true,true,0,log|true,true,true,0,warn|true,true,true,0,error");
        assert_eq!(js(&mut b, "Object.prototype.toString.call(console)"), "[object console]");
        assert_eq!(js(&mut b, "console.log(1) === undefined && console.warn() === undefined && console.error('a', 'b') === undefined"), "true");
        assert_eq!(js(&mut b, "try { new console.log(); 'constructed' } catch (e) { e.name }"), "TypeError");
        assert_eq!(js(&mut b, "typeof console.info + typeof console.debug + typeof console.trace"), "undefinedundefinedundefined");
    });
}

#[test]
fn the_reduced_bootstrap_shapes_work() {
    big(|| {
        let mut b = mk();
        // The module-top shape of the app source.
        assert_eq!(js(&mut b, "let _ = window.console.error; _('first'); typeof _"), "function");
        // Saved, bound and patched like a framework does.
        let src = r#"
var orig = console.error, seen = 0;
console.error = function () { seen++; if (arguments[0] === 'skip') return; return orig.apply(console, arguments); };
console.error('skip'); console.error('keep', 1);
var bound = console.warn.bind(console, 'pre'); bound('x');
var viaCall = console.log.call(null, 'c'); Function.prototype.apply.call(console.log, undefined, ['r']);
orig.call({ other: 1 }, 'recv'); orig('bare');
seen"#;
        assert_eq!(js(&mut b, src), "2");
        assert_eq!(lines(&mut b), ["E:first", "E:keep 1", "W:pre x", "L:c", "L:r", "E:recv", "E:bare"]);
    });
}

#[test]
fn deleting_or_replacing_console_does_not_stop_saved_methods() {
    big(|| {
        let mut b = mk();
        js(&mut b, "var save = console.log; var other = { log: function () { return 'mine'; } };");
        assert_eq!(js(&mut b, "delete window.console; typeof console"), "undefined");
        assert_eq!(js(&mut b, "try { console.log('x'); 'ok' } catch (e) { e.name }"), "ReferenceError");
        js(&mut b, "save('after delete'); window.console = other; console.log('x'); save('again'); globalThis.console = undefined;");
        assert_eq!(lines(&mut b), ["L:after delete", "L:again"]);
        // A deleted console and its methods are garbage; the kept records still work after a collection.
        js(&mut b, "save = null; save2 = 1");
        b.collect(&[]).unwrap();
    });
}

#[test]
fn a_logged_object_is_never_read_or_converted() {
    big(|| {
        let mut b = mk();
        let setup = r#"
var hits = 0;
var o = { toString() { hits++; return 'S'; }, valueOf() { hits++; return 1; }, get g() { hits++; return 1; }, [Symbol.toPrimitive]() { hits++; return 'p'; } };
var p = Object.create({ get inherited() { hits++; return 1; } }); Object.defineProperty(p, 'own', { get() { hits++; return 1; }, enumerable: true });
var arr = [1]; Object.defineProperty(arr, 'length', { value: 1 }); Object.defineProperty(arr, '0', { get() { hits++; return 1; } });
var cyc = { name: 'c' }; cyc.self = cyc; cyc.list = [cyc, [cyc]];
var f = function () { hits++; };
console.log(o, p, cyc, f, 'plain', arr); console.error('%s|%d|%i|%f|%o|%O|%c|%%', o, o, p, cyc, f, o, o, 'tail');
console.warn(o, '%s', o);
hits"#;
        assert_eq!(js(&mut b, setup), "0");
        let recs = b.take_console_records();
        assert_eq!(recs.len(), 3);
        // The record keeps the identity of the arguments.
        assert_eq!(js(&mut b, "hits"), "0");
        for r in &recs {
            b.format_console_record(r).unwrap();
        }
        assert_eq!(js(&mut b, "hits"), "0");
        let texts: Vec<String> = recs.iter().map(|r| b.format_console_record(r).unwrap()).collect();
        assert_eq!(texts[0], "[object] [object] [object] [function] plain [object]");
        assert_eq!(texts[1], "[object]|NaN|NaN|NaN|[function]|[object]||% tail");
        assert_eq!(texts[2], "[object] %s [object]");
    });
}

#[test]
fn an_error_prints_its_stack_or_message_without_running_page_code() {
    big(|| {
        let mut b = mk();
        let setup = r#"
var hits = 0;
var e = new TypeError('bad thing');
var noStack = { message: 'only message' };
var trap = { get stack() { hits++; return 'x'; }, get message() { hits++; return 'y'; } };
var numStack = { stack: 5 };
console.error(e); console.error(noStack); console.warn(trap); console.log(numStack); console.error('%s!', e);
hits"#;
        assert_eq!(js(&mut b, setup), "0");
        let l = lines(&mut b);
        assert!(l[0].starts_with("E:TypeError: bad thing"), "{}", l[0]);
        assert_eq!(l[1], "E:only message");
        assert_eq!(l[2], "W:[object]");
        assert_eq!(l[3], "L:[object]");
        assert!(l[4].starts_with("E:TypeError: bad thing") && l[4].ends_with('!'), "{}", l[4]);
        assert_eq!(js(&mut b, "hits"), "0");
    });
}

#[test]
fn records_keep_raw_arguments_levels_and_sequence() {
    big(|| {
        let mut b = mk();
        js(&mut b, "var obj = { k: 7 }; console.log(obj, 'two', 3, null, undefined, true); console.warn(); console.error(obj);");
        let r = b.take_console_records();
        assert_eq!(r.iter().map(|r| (r.seq, r.level, r.args.len())).collect::<Vec<_>>(), [(0, ConsoleLevel::Log, 6), (1, ConsoleLevel::Warn, 0), (2, ConsoleLevel::Error, 1)]);
        assert_eq!(b.it.get_str(r[0].args[0], "k"), "7");
        assert!(r[0].args[0].strict_equals(r[2].args[0]));
        assert_eq!(b.console_stats(), ConsoleStats { records: 0, values: 0, calls: 3, dropped: 0 });
        assert!(b.take_console_records().is_empty());
    });
}

#[test]
fn debug_output_shows_counts_never_arguments() {
    big(|| {
        let mut b = mk();
        js(&mut b, "console.log('TOKEN-LIKE-abc123', { secret: 'TOKEN-LIKE-def456' }); console.error(new Error('TOKEN-LIKE-ghi789'));");
        let s = format!("{:?} {:?}", b.console_stats(), b.take_console_records());
        assert!(!s.contains("TOKEN") && !s.contains("abc123") && !s.contains("secret"), "{s}");
        assert!(s.contains("args: 2") && s.contains("args: 1"), "{s}");
    });
}

#[test]
fn the_oldest_record_is_dropped_and_counted() {
    big(|| {
        let mut b = mk();
        b.set_console_limits(ConsoleLimits { max_records: 3, ..Default::default() });
        js(&mut b, "for (var i = 0; i < 10; i++) console.log(i, 'a');");
        assert_eq!(b.console_stats(), ConsoleStats { records: 3, values: 6, calls: 10, dropped: 7 });
        assert_eq!(b.take_console_records().iter().map(|r| r.seq).collect::<Vec<_>>(), [7, 8, 9]);
        // A smaller cap drops now.
        js(&mut b, "console.log(1); console.log(2); console.log(3);");
        b.set_console_limits(ConsoleLimits { max_records: 1, ..Default::default() });
        assert_eq!(b.console_stats(), ConsoleStats { records: 1, values: 1, calls: 13, dropped: 9 });
        // Zero keeps nothing and counts everything.
        b.set_console_limits(ConsoleLimits { max_records: 0, ..Default::default() });
        js(&mut b, "console.log(1, 2); console.error(3);");
        assert_eq!(b.console_stats(), ConsoleStats { records: 0, values: 0, calls: 15, dropped: 12 });
    });
}

#[test]
fn too_many_arguments_is_a_fatal_limit_checked_before_capture() {
    big(|| {
        let mut b = mk();
        b.set_console_limits(ConsoleLimits { max_args: 4, ..Default::default() });
        assert_eq!(js(&mut b, "console.log(1, 2, 3, 4); 'ok'"), "ok");
        assert_eq!(js(&mut b, "var five = [1, 2, 3, 4, 5]; console.log.apply(console, five)"), lim("console-args"));
        assert_eq!(js(&mut b, "try { console.error.apply(console, five); 'no' } catch (e) { 'caught' }"), lim("console-args"));
        let s = b.console_stats();
        assert_eq!((s.records, s.calls, s.dropped), (1, 1, 0));
    });
}

#[test]
fn each_call_charges_steps() {
    big(|| {
        let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 5_000, ..Default::default() }, HeapLimits::default()).unwrap();
        let r = js(&mut b, "var a = []; for (var j = 0; j < 60; j++) a.push(j); for (var i = 0; i < 100000; i++) console.log.apply(console, a);");
        assert_eq!(r, format!("limit:{:?}", LimitKind::Steps));
        assert!(b.console_stats().calls > 0);
    });
}

#[test]
fn kept_values_are_roots_and_a_taken_record_is_the_callers() {
    big(|| {
        let mut b = mk();
        js(&mut b, "console.log({ k: 11 }, 'str' + 'ing', Symbol('sy')); (function () { console.warn({ k: 22 }); })();");
        b.collect(&[]).unwrap();
        // Still there after a collection (rooted by the sink).
        let r = b.take_console_records();
        assert_eq!(b.it.get_str(r[0].args[0], "k"), "11");
        assert_eq!(b.format_console_record(&r[0]).unwrap(), "[object] string Symbol(sy)");
        let keep = r[0].args[0];
        // Taken: not rooted any more. The caller's `extra` keeps what it names.
        b.collect(&[keep]).unwrap();
        assert_eq!(b.it.get_str(keep, "k"), "11");
        // The other object was freed. Allocate again, so that its slot can be reused. The old value stays refused.
        let stale = r[1].args[0];
        js(&mut b, "var junk = []; for (var i = 0; i < 200; i++) junk.push({ k: 99 });");
        assert_eq!(b.it.get_str(stale, "k"), "?");
        // A collection with a stale root is an error, not a crash.
        assert!(b.collect(&[stale]).is_err());
    });
}

#[test]
fn drained_records_leave_nothing_behind_across_many_collections() {
    big(|| {
        let mut b = mk();
        let mut total = 0;
        for round in 0..20 {
            js(&mut b, &format!("for (var i = 0; i < 50; i++) console.log({{ round: {round}, i: i }}, [i]);"));
            b.collect(&[]).unwrap();
            total += b.take_console_records().len();
            let c = b.collect(&[]).unwrap();
            let _ = c;
        }
        assert_eq!(total, 1000);
        let before = b.it.heap.stats();
        for _ in 0..3 {
            js(&mut b, "for (var i = 0; i < 50; i++) console.log({ i: i }, [i]);");
            b.take_console_records();
            b.collect(&[]).unwrap();
        }
        assert_eq!(b.it.heap.stats().live_objects, before.live_objects);
    });
}

#[test]
fn callbacks_from_another_script_and_microtasks_record() {
    big(|| {
        let mut b = mk();
        b.use_virtual_clock_at(1000.0).unwrap();
        js(&mut b, "function fromA(tag) { console.log('A', tag); return console.warn; }");
        js(&mut b, "var w = fromA(1); w('B'); queueMicrotask(function () { fromA('micro'); }); setTimeout(fromA, 5, 'timer'); Promise.resolve().then(fromA.bind(null, 'then'));");
        b.drain().unwrap();
        b.advance_time(10.0, 100).unwrap();
        assert_eq!(lines(&mut b), ["L:A 1", "W:B", "L:A micro", "L:A then", "L:A timer"]);
    });
}

#[test]
fn a_throwing_wrapper_around_console_still_records_before_it_throws() {
    big(|| {
        let mut b = mk();
        let r = js(&mut b, "var o = console.error; console.error = function () { o.apply(this, arguments); throw new RangeError('x'); }; try { console.error('a'); } catch (e) { e.name }");
        assert_eq!(r, "RangeError");
        assert_eq!(lines(&mut b), ["E:a"]);
    });
}

#[test]
fn formatter_follows_the_standard_with_the_documented_gates() {
    big(|| {
        let mut b = mk();
        let src = r#"
console.log('%s and %s', 'one', 2);
console.log('%d|%i|%f|%d', '42.9px', 42.9, '3.5e2x', 'abc');
console.log('%d|%i|%f', Infinity, -7.5, -Infinity);
console.log('%%|%s', 'x', 'extra', 9);
console.log('no spec', 'a', 1);
console.log(1, '%s', 'x');
console.log('%s %s', 'only');
console.log('100%', 'a'); console.log('%');
console.log('%c styled %s', 'color:red', 'text');
console.log('%o %O', 'str', 'str2');
console.log('%x %s', 'q');
console.log('%s', Symbol('d'), '%d');
console.log('%d', Symbol('d'));
console.log('%s', -0, 0, NaN, null, undefined, false);
console.log('only one %s');
console.log('');
console.log();
"#;
        js(&mut b, src);
        assert_eq!(
            lines(&mut b),
            [
                "L:one and 2",
                "L:42|42|350|NaN",
                "L:NaN|-7|-Infinity",
                "L:%|x extra 9",
                "L:no spec a 1",
                "L:1 %s x",
                "L:only %s",
                "L:100% a",
                "L:%",
                "L: styled text",
                "L:\"str\" \"str2\"",
                "L:%x q",
                "L:Symbol(d) %d",
                "L:NaN",
                "L:0 0 NaN null undefined false",
                "L:only one %s",
                "L:",
                "L:",
            ]
        );
    });
}

/// The expected lines come from Node 22.18 (`console.log` of the same calls).
#[test]
fn a_bigint_prints_with_an_n() {
    big(|| {
        let mut b = mk();
        let src = r#"
console.log(1n, -5n, 2n ** 70n);
console.log('%s|%d|%i|%f|%o|%O|%c|', 1n, 2n, 3n, 4n, 5n, 6n, 7n);
console.log('%d %i %f', 2n ** 70n, -(2n ** 70n), 2n ** 70n);
console.log('a', 1n, 'b');
"#;
        js(&mut b, src);
        assert_eq!(
            lines(&mut b),
            ["L:1n -5n 1180591620717411303424n", "L:1n|2n|3n|4|5n|6n||", "L:1180591620717411303424n -1180591620717411303424n 1.1805916207174113e+21", "L:a 1n b"]
        );
    });
}

#[test]
fn formatted_output_is_bounded_before_it_grows() {
    big(|| {
        let mut b = mk();
        b.set_console_limits(ConsoleLimits { max_format_bytes: 16, ..Default::default() });
        js(&mut b, "function rep(s, n) { var r = ''; for (var i = 0; i < n; i++) r += s; return r; } console.log('%s', rep('x', 64)); console.log('ab', 'cd'); console.log(rep('x', 1000), 1);");
        let r = b.take_console_records();
        assert!(matches!(b.format_console_record(&r[0]), Err(Error::Limit(LimitKind::Host("console-format")))));
        assert_eq!(b.format_console_record(&r[1]).unwrap(), "ab cd");
        assert!(matches!(b.format_console_record(&r[2]), Err(Error::Limit(LimitKind::Host("console-format")))));
    });
}

#[test]
fn console_does_not_leak_into_other_globals() {
    big(|| {
        let mut b = mk();
        assert_eq!(js(&mut b, "Object.keys(console).join()"), "log,warn,error");
        assert_eq!(js(&mut b, "Object.keys(window).indexOf('console') < 0"), "true");
        assert_eq!(js(&mut b, "console === window.console && console === self.console && console === globalThis.console"), "true");
        assert_eq!(js(&mut b, "typeof window.chrome + typeof window.opera"), "undefinedundefined");
    });
}

#[test]
fn numeric_format_inputs_are_bounded_and_scans_consume_steps() {
    big(|| {
        let mut b = mk();
        b.set_console_limits(ConsoleLimits { max_format_bytes: 16, ..Default::default() });
        let fmt = Value::String(b.it.heap.intern_str("%d").unwrap());
        let text = Value::String(b.it.heap.intern_str(&"0".repeat(64)).unwrap());
        let r = ConsoleRecord { seq: 0, level: ConsoleLevel::Log, args: vec![fmt, text] };
        assert!(matches!(b.format_console_record(&r), Err(Error::Limit(LimitKind::Host("console-format")))));
    });
    big(|| {
        let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 10, ..Default::default() }, HeapLimits::default()).unwrap();
        let fmt = Value::String(b.it.heap.intern_str("%d").unwrap());
        let text = Value::String(b.it.heap.intern_str(&"0".repeat(8192)).unwrap());
        let r = ConsoleRecord { seq: 0, level: ConsoleLevel::Log, args: vec![fmt, text] };
        assert!(matches!(b.format_console_record(&r), Err(Error::Limit(LimitKind::Steps))));
    });
}

#[test]
fn formatting_symbol_descriptions_consumes_steps_before_copying() {
    big(|| {
        let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 10, ..Default::default() }, HeapLimits::default()).unwrap();
        let text = b.it.heap.intern_str(&"x".repeat(8192)).unwrap();
        let sym = Value::Symbol(b.it.heap.new_symbol(Some(text)).unwrap());
        let r = ConsoleRecord { seq: 0, level: ConsoleLevel::Log, args: vec![sym] };
        assert!(matches!(b.format_console_record(&r), Err(Error::Limit(LimitKind::Steps))));
    });
}

#[test]
fn integer_formatting_uses_decimal_string_prefixes_and_js_whitespace() {
    big(|| {
        let mut b = mk();
        js(&mut b, "console.log('%i|%i|%i|%i', 1e21, 1e-7, '\\uFEFF42px', '\\u008542px');");
        assert_eq!(lines(&mut b), ["L:1|1|42|NaN"]);
    });
}

#[test]
fn console_roots_participate_in_weak_stream_and_aggregate_collection() {
    big(|| {
        let mut b = mk();
        assert_eq!(js(&mut b, "var key = {}, wm = new WeakMap(), all; (function () { var s = new ReadableStream({ start(c) { wm.set(key, c); } }); var r = s.getReader(); all = Promise.all([r.read()]); })(); console.log(key); key = null; 0"), "0");
        b.drain().unwrap();
        b.collect(&[]).unwrap();
        assert_eq!(b.it.weak_tracked(), (1, 1));
        assert_eq!(b.stream_stats().streams, 1);
        assert_eq!(b.it.agg_stats().records, 1);
        let records = b.take_console_records();
        assert_eq!(records.len(), 1);
        b.collect(&[]).unwrap();
        assert_eq!(b.it.weak_tracked(), (1, 0));
        assert_eq!(b.stream_stats().streams, 0);
        assert_eq!(b.it.agg_stats().records, 0);
    });
}
