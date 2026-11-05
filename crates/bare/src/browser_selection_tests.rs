//! Tests of the `<input>` selection foundation (`browser_selection.rs`). Real scripts run in a real realm.
//! Expectations come from the HTML Standard text "APIs for the text control selections" and WebIDL conversions,
//! read by hand. NOT verified against any browser. Node has no DOM, so there is no oracle here.
//! Each test runs on a big stack.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const PAGE: &str = "<body><div id=box><input id=t value=hello><input id=ro readonly value=ro><input id=dis disabled value=dd><input id=v value=abc></div></body>";

fn make(steps: u64, sl: SchedLimits) -> Browser {
    let mut b = Browser::new(tree::parse(PAGE), BrowserLimits::default(), EvalLimits { max_steps: steps, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b.set_sched_limits(sl);
    b.run_script("function S(e){return String(e.selectionStart)+','+String(e.selectionEnd)+','+String(e.selectionDirection)} var t=document.getElementById('t');").unwrap();
    b
}

fn browser() -> Browser {
    make(3_000_000, SchedLimits::default())
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

fn pump(b: &mut Browser) -> Pump {
    b.run_scheduled_tasks(1_000_000).expect("pump")
}

const SUPPORTED: [&str; 5] = ["text", "search", "tel", "url", "password"];
const UNSUPPORTED: [&str; 17] = [
    "email", "number", "date", "month", "week", "time", "datetime-local", "range", "color", "checkbox", "radio", "file", "submit", "reset", "image", "button", "hidden",
];

#[test]
fn accessors_live_on_the_input_prototype_only() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var P = HTMLInputElement.prototype; ['selectionStart','selectionEnd','selectionDirection'].map(function (n) { var d = Object.getOwnPropertyDescriptor(P, n); return typeof d.get + typeof d.set + d.enumerable + d.configurable }).join()"), "functionfunctiontruetrue,functionfunctiontruetrue,functionfunctiontruetrue");
        assert_eq!(js(&mut b, "[typeof P.setSelectionRange, P.setSelectionRange.length, typeof P.select, P.select.length].join()"), "function,2,function,0");
        assert_eq!(js(&mut b, "'selectionStart' in t && 'setSelectionRange' in t && 'select' in t"), "true");
        assert_eq!(js(&mut b, "var d = document.getElementById('box'); ['selectionStart','selectionEnd','selectionDirection','setSelectionRange','select'].map(function (n) { return n in d }).join()"), "false,false,false,false,false");
        assert_eq!(js(&mut b, "Object.getOwnPropertyDescriptor(P, 'selectionStart').get.call(d)"), "throw:TypeError:Illegal invocation");
        assert_eq!(js(&mut b, "Object.getOwnPropertyDescriptor(P, 'selectionEnd').set.call(d, 1)"), "throw:TypeError:Illegal invocation");
        assert_eq!(js(&mut b, "P.setSelectionRange.call(d, 0, 1)"), "throw:TypeError:Illegal invocation");
        assert_eq!(js(&mut b, "P.select.call(d)"), "throw:TypeError:Illegal invocation");
        assert_eq!(js(&mut b, "P.select.call({})"), "throw:TypeError:Illegal invocation");
        // no fake global selection API
        assert_eq!(js(&mut b, "[typeof window.getSelection, typeof document.getSelection, typeof Selection, typeof Range, typeof HTMLTextAreaElement, 'setRangeText' in t].join()"), "undefined,undefined,undefined,undefined,undefined,false");
    });
}

#[test]
fn supported_types_hold_a_selection() {
    big(|| {
        let mut b = browser();
        for ty in SUPPORTED.iter().copied().chain(["foo", "TEXT", "Password", ""]) {
            let r = js(&mut b, &format!("var e = document.createElement('input'); e.setAttribute('type', '{ty}'); e.value = 'abcdef'; e.setSelectionRange(1, 3, 'forward'); S(e) + '|' + (e.selectionStart = 2, e.selectionEnd = 5, S(e)) + '|' + (e.select(), S(e))"));
            assert_eq!(r, "1,3,forward|2,5,forward|0,6,none", "type {ty}");
        }
    });
}

#[test]
fn unsupported_types_do_not_pretend_to_select() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = []; document.addEventListener('select', function (e) { log.push(e.type) });");
        for ty in UNSUPPORTED {
            let r = js(&mut b, &format!("var e = document.createElement('input'); e.type = '{ty}'; document.getElementById('box').appendChild(e); [S(e), e.type].join('|')"));
            assert_eq!(r, format!("null,null,null|{ty}"), "getters for {ty}");
            for stmt in ["e.setSelectionRange(0, 1)", "e.selectionStart = 0", "e.selectionEnd = 0", "e.selectionDirection = 'forward'"] {
                assert_eq!(js(&mut b, stmt), "throw:InvalidStateError:The input element's type ('".to_string() + ty + "') does not support selection.", "{stmt} on {ty}");
            }
            assert_eq!(js(&mut b, "e.select()"), "undefined", "select() on {ty} returns quietly");
            assert_eq!(js(&mut b, "e.remove()"), "undefined");
        }
        assert_eq!(pump(&mut b).ran, 0);
        assert_eq!(js(&mut b, "log.length"), "0");
        // switching the type switches the answers at once; the value is kept
        assert_eq!(js(&mut b, "var e = document.createElement('input'); e.value = 'abcd'; e.setSelectionRange(1, 2); e.type = 'email'; var a = S(e); e.type = 'text'; a + '|' + S(e)"), "null,null,null|1,2,none");
        // email must not hold text selection even when a value is set
        assert_eq!(js(&mut b, "e.type = 'email'; e.value = 'a@b.c'; S(e)"), "null,null,null");
    });
}

#[test]
fn initial_and_value_driven_selection() {
    big(|| {
        let mut b = browser();
        // choice (spec leaves the initial selection open): the caret sits at the end of the value
        assert_eq!(js(&mut b, "S(document.getElementById('v')) + '|' + S(document.createElement('input'))"), "3,3,none|0,0,none");
        assert_eq!(js(&mut b, "t.value = 'xy'; S(t)"), "2,2,none");
        // a new value that differs moves the caret to the end and clears the direction
        assert_eq!(js(&mut b, "t.value = 'abcdef'; t.setSelectionRange(1, 3, 'backward'); t.value = 'abcdef'; S(t)"), "1,3,backward");
        assert_eq!(js(&mut b, "t.value = 'abcdeg'; S(t)"), "6,6,none");
        assert_eq!(js(&mut b, "t.setSelectionRange(1, 3, 'forward'); t.value = null; S(t)"), "0,0,none");
        assert_eq!(js(&mut b, "t.value = 'abc'; t.select(); t.value = 'a'; S(t)"), "1,1,none");
        // a selection past a shorter value is clamped when read
        assert_eq!(js(&mut b, "t.value = 'abcdef'; t.setSelectionRange(4, 6); t.defaultValue = 'zz'; t.value"), "abcdef");
        // not-dirty value follows the attribute: the selection is read inside the new value
        assert_eq!(js(&mut b, "var u = document.createElement('input'); u.defaultValue = 'abcdef'; u.setSelectionRange(4, 6); u.defaultValue = 'ab'; S(u)"), "2,2,none");
        // driver typing: caret at the end, native state agrees
        let n = b.query("#t").unwrap().unwrap();
        b.type_text(n, "é😀x").unwrap();
        assert_eq!(js(&mut b, "S(t)"), "4,4,none");
        assert_eq!(b.input_selection(n), Some((4, 4, "none")));
        let ro = b.query("#box").unwrap().unwrap();
        assert_eq!(b.input_selection(ro), None);
    });
}

#[test]
fn index_conversion_follows_webidl_unsigned_long() {
    big(|| {
        let mut b = browser();
        js(&mut b, "t.value = 'abcdef'");
        let cases = [
            ("1.9, 4.9", "1,4"),
            ("-1, 3", "3,3"),
            ("NaN, NaN", "0,0"),
            ("Infinity, 4", "0,4"),
            ("2, Infinity", "0,0"),
            ("'2', '5'", "2,5"),
            ("undefined, 3", "0,3"),
            ("null, null", "0,0"),
            ("true, 2", "1,2"),
            ("4294967297, 4294967299", "1,3"),
            ("0, 4294967295", "0,6"),
            ("0, 99", "0,6"),
            ("-0.5, 2", "0,2"),
            ("4, 2", "2,2"),
            ("6, 6", "6,6"),
            ("7, 9", "6,6"),
        ];
        for (args, want) in cases {
            let r = js(&mut b, &format!("t.setSelectionRange({args}); [t.selectionStart, t.selectionEnd].join()"));
            assert_eq!(r, want, "setSelectionRange({args})");
        }
        // required arguments (WebIDL): fewer than two is a TypeError and changes nothing
        js(&mut b, "t.setSelectionRange(1, 2)");
        assert_eq!(js(&mut b, "t.setSelectionRange()"), "throw:TypeError:Failed to execute 'setSelectionRange': 2 arguments required, but only 0 present.");
        assert_eq!(js(&mut b, "t.setSelectionRange(0)"), "throw:TypeError:Failed to execute 'setSelectionRange': 2 arguments required, but only 1 present.");
        assert_eq!(js(&mut b, "S(t)"), "1,2,none");
        // reversed range collapses at end (spec: both ends before the character with offset end)
        assert_eq!(js(&mut b, "t.setSelectionRange(5, 3, 'backward'); S(t)"), "3,3,backward");
    });
}

#[test]
fn direction_normalisation() {
    big(|| {
        let mut b = browser();
        js(&mut b, "t.value = 'abcdef'");
        let cases = [
            ("'forward'", "forward"),
            ("'backward'", "backward"),
            ("'none'", "none"),
            ("'FORWARD'", "none"),
            ("' forward'", "none"),
            ("'sideways'", "none"),
            ("undefined", "none"),
            ("null", "none"),
            ("{toString: function () { return 'backward' }}", "backward"),
            ("5", "none"),
        ];
        for (d, want) in cases {
            assert_eq!(js(&mut b, &format!("t.setSelectionRange(1, 3, {d}); t.selectionDirection")), want, "direction {d}");
        }
        assert_eq!(js(&mut b, "t.setSelectionRange(1, 3, 'backward'); t.setSelectionRange(1, 3); t.selectionDirection"), "none");
        assert_eq!(js(&mut b, "t.setSelectionRange(1, 3); t.selectionDirection = 'backward'; S(t)"), "1,3,backward");
        assert_eq!(js(&mut b, "t.selectionDirection = 'x'; S(t)"), "1,3,none");
        assert_eq!(js(&mut b, "t.selectionDirection = 'forward'; t.selectionDirection = null; S(t)"), "1,3,none");
        assert_eq!(js(&mut b, "t.setSelectionRange(1, 3, Symbol('s'))"), "throw:TypeError:Cannot convert a Symbol value to a string");
    });
}

#[test]
fn start_and_end_setters_follow_the_spec_steps() {
    big(|| {
        let mut b = browser();
        js(&mut b, "t.value = 'abcdef'; t.setSelectionRange(1, 3, 'backward')");
        // selectionStart: end is raised to the new start; direction is kept
        assert_eq!(js(&mut b, "t.selectionStart = 2; S(t)"), "2,3,backward");
        assert_eq!(js(&mut b, "t.selectionStart = 5; S(t)"), "5,5,backward");
        assert_eq!(js(&mut b, "t.selectionStart = 100; S(t)"), "6,6,backward");
        // selectionEnd: an end before the start collapses both
        assert_eq!(js(&mut b, "t.setSelectionRange(1, 4, 'forward'); t.selectionEnd = 2; S(t)"), "1,2,forward");
        assert_eq!(js(&mut b, "t.selectionEnd = 0; S(t)"), "0,0,forward");
        assert_eq!(js(&mut b, "t.setSelectionRange(3, 5); t.selectionEnd = 1; S(t)"), "1,1,none");
        // null converts to null, which the algorithm reads as 0
        assert_eq!(js(&mut b, "t.setSelectionRange(2, 4); t.selectionStart = null; S(t)"), "0,4,none");
        assert_eq!(js(&mut b, "t.selectionEnd = undefined; S(t)"), "0,0,none");
        assert_eq!(js(&mut b, "t.selectionEnd = -1; S(t)"), "0,6,none");
        assert_eq!(js(&mut b, "t.selectionStart = 2.7; S(t)"), "2,6,none");
        // select() covers everything and drops the direction
        assert_eq!(js(&mut b, "t.setSelectionRange(1, 2, 'backward'); t.select(); S(t)"), "0,6,none");
        assert_eq!(js(&mut b, "t.value = ''; t.select(); S(t)"), "0,0,none");
    });
}

#[test]
fn utf16_offsets_with_surrogate_pairs() {
    big(|| {
        let mut b = browser();
        js(&mut b, "t.value = '\\u{1F600}a\\u{1F600}'");
        assert_eq!(js(&mut b, "t.value.length"), "5");
        assert_eq!(js(&mut b, "t.select(); S(t)"), "0,5,none");
        // an offset inside a pair is a valid UTF-16 offset
        assert_eq!(js(&mut b, "t.setSelectionRange(1, 2); S(t)"), "1,2,none");
        assert_eq!(js(&mut b, "t.setSelectionRange(0, 99); S(t)"), "0,5,none");
        assert_eq!(js(&mut b, "t.value = '\\u{1F600}'; S(t)"), "2,2,none");
        // multi-byte (2- and 3-byte) characters count 1 unit
        assert_eq!(js(&mut b, "t.value = '\\u00e9\\u20ac'; t.select(); S(t)"), "0,2,none");
        let n = b.query("#t").unwrap().unwrap();
        assert_eq!(b.input_selection(n), Some((0, 2, "none")));
    });
}

#[test]
fn coercion_runs_js_and_state_is_rechecked_afterwards() {
    big(|| {
        let mut b = browser();
        js(&mut b, "t.value = 'abcdef'; var log = [];");
        // arguments convert in order: start, end, direction
        js(&mut b, "t.setSelectionRange({valueOf: function () { log.push('s'); return 1 }}, {valueOf: function () { log.push('e'); return 3 }}, {toString: function () { log.push('d'); return 'forward' }})");
        assert_eq!(js(&mut b, "log.join() + '|' + S(t)"), "s,e,d|1,3,forward");
        // the value shrinks while converting: the range is clamped to the new value
        assert_eq!(js(&mut b, "t.setSelectionRange({valueOf: function () { t.value = 'ab'; return 1 }}, 5); S(t)"), "1,2,none");
        // selectionStart setter reads the current end after converting
        assert_eq!(js(&mut b, "t.value = 'abcdef'; t.setSelectionRange(0, 2); t.selectionStart = {valueOf: function () { t.setSelectionRange(0, 5); return 3 }}; S(t)"), "3,5,none");
        // the type changes while converting: the InvalidStateError is decided afterwards
        assert_eq!(js(&mut b, "t.setSelectionRange({valueOf: function () { t.type = 'email'; return 1 }}, 2)"), "throw:InvalidStateError:The input element's type ('email') does not support selection.");
        assert_eq!(js(&mut b, "t.type = 'text'; t.setSelectionRange(1, 2); t.setSelectionRange({valueOf: function () { t.type = 'text'; return 0 }}, 4); S(t)"), "0,4,none");
        // conversion side effects of an invalid-type element still happen before the throw
        assert_eq!(js(&mut b, "t.type = 'number'; log = []; try { t.setSelectionRange({valueOf: function () { log.push('x'); return 0 }}, 1) } catch (e) { log.push(e.name) } log.join()"), "x,InvalidStateError");
        assert_eq!(js(&mut b, "t.type = 'text'; t.setSelectionRange(1, 2); S(t)"), "1,2,none");
        // a throw in a conversion changes nothing
        assert_eq!(js(&mut b, "try { t.setSelectionRange(0, {valueOf: function () { throw 7 }}) } catch (e) { log.push('c' + e) } S(t)"), "1,2,none");
        assert_eq!(js(&mut b, "t.selectionStart = {valueOf: function () { throw new RangeError('r') }}"), "throw:RangeError:r");
        assert_eq!(js(&mut b, "t.setSelectionRange(Symbol('s'), 1)"), "throw:TypeError:Cannot convert a Symbol value to a number");
        assert_eq!(js(&mut b, "S(t)"), "1,2,none");
        // an element removed while converting still takes the selection
        assert_eq!(js(&mut b, "t.setSelectionRange({valueOf: function () { t.remove(); return 0 }}, 3); S(t) + '|' + t.isConnected"), "0,3,none|false");
    });
}

#[test]
fn readonly_disabled_detached_and_clones() {
    big(|| {
        let mut b = browser();
        // HTML does not restrict the selection of a readonly or disabled input
        assert_eq!(js(&mut b, "var ro = document.getElementById('ro'), dis = document.getElementById('dis'); ro.setSelectionRange(0, 1); dis.select(); S(ro) + '|' + S(dis)"), "0,1,none|0,2,none");
        // detached: it works, and the state is kept after the input is attached
        assert_eq!(js(&mut b, "var d = document.createElement('input'); d.value = 'hello'; d.setSelectionRange(1, 4, 'backward'); var before = S(d); document.getElementById('box').appendChild(d); before + '|' + S(d) + '|' + d.remove() + '|' + S(d)"), "1,4,backward|1,4,backward|undefined|1,4,backward");
        // clone: value is copied, the selection is not (it restarts at the end)
        assert_eq!(js(&mut b, "t.value = 'abcdef'; t.setSelectionRange(1, 3, 'forward'); var c = t.cloneNode(false); c.value + '|' + S(c) + '|' + S(t)"), "abcdef|6,6,none|1,3,forward");
        assert_eq!(js(&mut b, "c.setSelectionRange(0, 1); S(c) + '|' + S(t)"), "0,1,none|1,3,forward");
        // a deep clone of a tree restarts every input
        assert_eq!(js(&mut b, "var bx = document.getElementById('box'); document.getElementById('v').setSelectionRange(0, 2); var bc = bx.cloneNode(true); [].map.call(bc.children, S).join(' ')"), "6,6,none 2,2,none 2,2,none 3,3,none");
    });
}

#[test]
fn state_survives_wrapper_collection_and_never_leaks_to_new_elements() {
    big(|| {
        let mut b = browser();
        js(&mut b, "(function () { var d = document.createElement('input'); d.id = 'keep'; d.value = 'abcdef'; d.setSelectionRange(2, 4, 'backward'); document.getElementById('box').appendChild(d); })();");
        let keep = b.query("#keep").unwrap().unwrap();
        // many throw-away inputs with a selection each, then a collection
        js(&mut b, "for (var i = 0; i < 3000; i++) { var x = document.createElement('input'); x.value = 'abcdefgh'; x.setSelectionRange(i % 8, 8, 'forward'); } x = null;");
        b.collect(&[]).unwrap();
        b.collect(&[]).unwrap();
        // fresh elements made after the collection start clean, whatever wrapper slots they get
        assert_eq!(js(&mut b, "var fresh = []; for (var i = 0; i < 50; i++) fresh.push(S(document.createElement('input'))); fresh.filter(function (s) { return s !== '0,0,none' }).length"), "0");
        // the collected wrapper of an attached input is made again over the same native state
        assert_eq!(js(&mut b, "S(document.getElementById('keep'))"), "2,4,backward");
        assert_eq!(b.input_selection(keep), Some((2, 4, "backward")));
        js(&mut b, "document.getElementById('keep').remove()");
        b.collect(&[]).unwrap();
        assert_eq!(b.input_selection(keep), Some((2, 4, "backward")), "native node and state live on in the arena");
    });
}

#[test]
fn select_event_is_a_queued_task_fired_at_the_input_when_the_selection_changed() {
    big(|| {
        let mut b = browser();
        js(&mut b, "t.value = 'abcdef'; var log = []; function L(tag) { return function (e) { log.push(tag + ':' + e.type + ':' + e.bubbles + ':' + e.cancelable + ':' + (e.target === t) + ':' + e.eventPhase + ':' + S(t)) } }
            t.addEventListener('select', L('t')); document.addEventListener('select', L('doc')); window.addEventListener('select', L('win'));
            document.getElementById('box').addEventListener('select', L('box'));");
        // not synchronous, not a microtask
        assert_eq!(js(&mut b, "t.setSelectionRange(1, 3); Promise.resolve().then(function () { log.push('micro') }); log.length"), "0");
        b.drain().unwrap();
        assert_eq!(js(&mut b, "log.join()"), "micro");
        assert_eq!(b.sched_stats().tasks, 1);
        let p = pump(&mut b);
        assert_eq!(p.ran, 1);
        assert_eq!(
            js(&mut b, "log.slice(1).join(' ')"),
            "t:select:true:false:true:2:1,3,none box:select:true:false:true:3:1,3,none doc:select:true:false:true:3:1,3,none win:select:true:false:true:3:1,3,none"
        );
        assert_eq!(b.sched_stats().tasks, 0);
        // no change, no event: the same range, the same direction
        js(&mut b, "log = []; t.setSelectionRange(1, 3); t.selectionStart = 1; t.selectionEnd = 3; t.selectionDirection = 'none'; t.value = 'abcdef'");
        assert_eq!(pump(&mut b).ran, 0);
        // a direction change alone is a change
        js(&mut b, "t.selectionDirection = 'forward'");
        assert_eq!(pump(&mut b).ran, 1);
        // select() queues one; select() again changes nothing
        js(&mut b, "log = []; t.select(); t.select()");
        assert_eq!(pump(&mut b).ran, 1);
        assert_eq!(js(&mut b, "log.filter(function (s) { return s.slice(0, 2) === 't:' }).join()"), "t:select:true:false:true:2:0,6,none");
        // the value setter and the driver queue nothing
        js(&mut b, "log = []; t.value = 'zz'");
        let n = b.query("#t").unwrap().unwrap();
        b.type_text(n, "qq").unwrap();
        assert_eq!(pump(&mut b).ran, 0);
        // every change queues its own task; each listener sees the state when it runs
        js(&mut b, "log = []; t.setSelectionRange(0, 1); t.setSelectionRange(1, 2); t.setSelectionRange(0, 2)");
        assert_eq!(b.sched_stats().tasks, 3);
        assert_eq!(pump(&mut b).ran, 3);
        assert_eq!(js(&mut b, "log.filter(function (s) { return s.slice(0, 2) === 't:' }).map(function (s) { return s.split(':').pop() }).join(' ')"), "0,2,none 0,2,none 0,2,none");
    });
}

#[test]
fn select_tasks_keep_queue_order_with_other_tasks_and_reentrant_changes() {
    big(|| {
        let mut b = browser();
        js(&mut b, "t.value = 'abcdef'; var log = []; t.addEventListener('select', function () { log.push('sel:' + S(t)); if (t.selectionEnd < 6) t.selectionEnd = 6 });");
        js(&mut b, "setTimeout(function () { log.push('T1') }, 0); t.setSelectionRange(1, 2); setTimeout(function () { log.push('T2') }, 0); t.select()");
        pump(&mut b);
        // order of queueing at equal due time; the listener's own change is queued after and runs in the same pump
        assert_eq!(js(&mut b, "log.join()"), "T1,sel:0,6,none,T2,sel:0,6,none");
        // a throwing listener is reported and later select tasks still run
        js(&mut b, "log = []; t.addEventListener('select', function () { throw new Error('boom') }); t.setSelectionRange(0, 1); t.setSelectionRange(0, 2)");
        let p = pump(&mut b);
        assert!(p.ran >= 2);
        let r = b.take_reports();
        assert!(r.iter().all(|x| x.event_type == "select" && x.target == "input") && r.len() >= 2, "{r:?}");
        // a later timer scheduled in the future is not run by select tasks
        js(&mut b, "log = []; setTimeout(function () { log.push('late') }, 50); t.setSelectionRange(2, 3)");
        pump(&mut b);
        assert!(!js(&mut b, "log.join()").contains("late"));
        // virtual-time pump runs select tasks at the current time
        js(&mut b, "log = []; t.setSelectionRange(3, 4)");
        b.advance_time(0.0, 10).unwrap();
        assert!(js(&mut b, "log.join()").contains("sel:"));
    });
}

#[test]
fn select_task_cap_is_checked_before_queueing_and_frees_after_the_pump() {
    big(|| {
        let mut b = make(3_000_000, SchedLimits { max_select_tasks: 3, ..Default::default() });
        js(&mut b, "t.value = 'abcdef'");
        assert_eq!(js(&mut b, "t.setSelectionRange(0, 1); t.setSelectionRange(0, 2); t.setSelectionRange(0, 3); S(t)"), "0,3,none");
        // the fourth change is refused, and it does not change the state
        assert_eq!(js(&mut b, "t.setSelectionRange(0, 4)"), "limit:Host(\"select-tasks\")");
        assert_eq!(js(&mut b, "S(t)"), "0,3,none");
        assert_eq!(b.sched_stats().tasks, 3);
        assert_eq!(pump(&mut b).ran, 3);
        assert_eq!(b.sched_stats().tasks, 0);
        assert_eq!(js(&mut b, "t.setSelectionRange(0, 4); S(t)"), "0,4,none");
        assert_eq!(b.sched_stats().tasks, 1);
        // an unchanged selection queues nothing, so it never trips the cap
        assert_eq!(js(&mut b, "for (var i = 0; i < 100; i++) t.setSelectionRange(0, 4); 'ok'"), "ok");
        assert_eq!(b.sched_stats().tasks, 1);
    });
}

#[test]
fn no_select_listener_runs_after_a_fatal_pump_error() {
    big(|| {
        let mut b = make(20_000, SchedLimits::default());
        js(&mut b, "t.value = 'abcdef'; var hits = 0; t.addEventListener('select', function () { hits++ });");
        js(&mut b, "setTimeout(function () { for (;;) {} }, 0); t.setSelectionRange(1, 2)");
        let e = b.run_scheduled_tasks(100).unwrap_err();
        assert!(matches!(e, Error::Limit(_)), "{e:?}");
        assert!(b.sched_stats().halted);
        // the select task was queued after the failing timer and never runs
        assert!(b.run_scheduled_tasks(100).is_err());
        assert!(b.advance_time(10.0, 100).is_err());
        let n = b.query("#t").unwrap().unwrap();
        assert_eq!(b.input_selection(n), Some((1, 2, "none")));
    });
}

#[test]
fn select_listener_budget_is_per_task() {
    big(|| {
        let mut b = browser();
        js(&mut b, "t.value = 'abcdef'; var n = 0; t.addEventListener('select', function () { n++; for (var i = 0; i < 1000; i++); });
            for (var k = 0; k < 200; k++) t.setSelectionRange(k % 5, 5 + (k % 2));");
        let p = pump(&mut b);
        assert!(p.ran >= 100, "{p:?}");
        assert_eq!(js(&mut b, "n >= 100"), "true");
    });
}
