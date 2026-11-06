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
        // The document selection is in `browser_range.rs` and is not linked with the one of a text control.
        assert_eq!(js(&mut b, "[typeof window.getSelection, typeof document.getSelection, typeof Selection, typeof Range, typeof HTMLTextAreaElement, 'setRangeText' in t].join()"), "function,function,function,function,undefined,false");
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
        pump(&mut b);
        // not synchronous, not a microtask
        assert_eq!(js(&mut b, "t.setSelectionRange(1, 3); Promise.resolve().then(function () { log.push('micro') }); log.length"), "0");
        b.drain().unwrap();
        assert_eq!(js(&mut b, "log.join()"), "micro");
        assert_eq!(b.sched_stats().tasks, 2, "select and selectionchange");
        let p = pump(&mut b);
        assert_eq!(p.ran, 2);
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
        assert_eq!(pump(&mut b).ran, 2);
        // select() queues one select; select() again changes nothing
        js(&mut b, "log = []; t.select(); t.select()");
        assert_eq!(pump(&mut b).ran, 2);
        assert_eq!(js(&mut b, "log.filter(function (s) { return s.slice(0, 2) === 't:' }).join()"), "t:select:true:false:true:2:0,6,none");
        // the value setter and the driver queue no `select` (HTML has none there); their selectionchange is tested below
        js(&mut b, "log = []; t.value = 'zz'");
        let n = b.query("#t").unwrap().unwrap();
        b.type_text(n, "qqq").unwrap();
        assert_eq!(pump(&mut b).ran, 1, "one coalesced selectionchange, no select");
        assert_eq!(js(&mut b, "log.length"), "0");
        // every change queues its own task; each listener sees the state when it runs
        js(&mut b, "log = []; t.setSelectionRange(0, 1); t.setSelectionRange(1, 2); t.setSelectionRange(0, 2)");
        assert_eq!(b.sched_stats().tasks, 4, "three select tasks and one selectionchange");
        assert_eq!(pump(&mut b).ran, 4);
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
        pump(&mut b);
        assert_eq!(js(&mut b, "t.setSelectionRange(0, 1); t.setSelectionRange(0, 2); t.setSelectionRange(0, 3); S(t)"), "0,3,none");
        // the fourth change is refused, and it does not change the state
        assert_eq!(js(&mut b, "t.setSelectionRange(0, 4)"), "limit:Host(\"select-tasks\")");
        assert_eq!(js(&mut b, "S(t)"), "0,3,none");
        assert_eq!(b.sched_stats().tasks, 4, "three select tasks and one coalesced selectionchange");
        assert_eq!(pump(&mut b).ran, 4);
        assert_eq!(b.sched_stats().tasks, 0);
        assert_eq!(js(&mut b, "t.setSelectionRange(0, 4); S(t)"), "0,4,none");
        assert_eq!(b.sched_stats().tasks, 2);
        // an unchanged selection queues nothing, so it never trips the cap
        assert_eq!(js(&mut b, "for (var i = 0; i < 100; i++) t.setSelectionRange(0, 4); 'ok'"), "ok");
        assert_eq!(b.sched_stats().tasks, 2);
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

// ------------------------------------------------------------------------------------------ selectionchange
// Source: Selection API, "selectionchange event", "Scheduling selectionchange event", "Firing selectionchange event"
// (w3c.github.io/selection-api, read by hand): an input whose selection changes in extent or direction schedules
// the event at itself; `has scheduled selectionchange event` is per input, set when queued, cleared when the task
// starts; the event bubbles and is not cancelable. HTML "set the selection range" queues `select` after it modified the
// selection. Neither text orders the two; the order here (selectionchange first) follows from the steps' order.

fn scans() -> usize {
    crate::browser_selection::scans()
}

fn tasks(b: &Browser) -> usize {
    b.sched_stats().tasks
}

const WATCH: &str = "var log = []; function L(tag) { return function (e) { log.push(tag + ':' + e.type + ':' + e.bubbles + ':' + e.cancelable + ':' + (e.target === t) + ':' + e.eventPhase + ':' + S(t)) } }
    var v = document.getElementById('v'), ro = document.getElementById('ro');";

/// A browser whose `#t` holds 'abcdef' with no task pending.
fn settled(steps: u64, sl: SchedLimits) -> Browser {
    let mut b = make(steps, sl);
    js(&mut b, "t.value = 'abcdef'");
    pump(&mut b);
    b
}

#[test]
fn huge_value_under_a_tiny_budget_is_stopped_before_any_scan() {
    big(|| {
        let mut b = make(5_000, SchedLimits::default());
        let n = b.query("#t").unwrap().unwrap();
        b.type_text(n, &"a".repeat(4 << 20)).unwrap();
        let base = scans();
        for stmt in [
            "t.selectionStart", "t.selectionEnd", "t.selectionDirection", "t.setSelectionRange(1, 2)", "t.selectionStart = 1", "t.selectionEnd = 1",
            "t.selectionDirection = 'forward'", "t.select()", "t.value = 'x'",
        ] {
            let r = js(&mut b, stmt);
            assert!(r.starts_with("limit:"), "{stmt}: {r}");
            assert_eq!(scans(), base, "{stmt} scanned the value before it charged");
        }
        // nothing was written: no task, the same selection (read by the driver, which scans once)
        assert_eq!(tasks(&b), 1, "only the task of the driver's own typing");
        assert_eq!(b.input_selection(n), Some((4 << 20, 4 << 20, "none")));
        assert_eq!(b.input_value(n).unwrap().len(), 4 << 20);
    });
}

#[test]
fn every_operation_scans_a_value_once() {
    big(|| {
        let mut b = settled(3_000_000, SchedLimits::default());
        let count = |b: &mut Browser, stmt: &str| {
            let before = scans();
            let r = js(b, stmt);
            assert!(!r.starts_with("limit") && !r.starts_with("throw"), "{stmt}: {r}");
            scans() - before
        };
        assert_eq!(count(&mut b, "t.selectionStart"), 1);
        assert_eq!(count(&mut b, "t.selectionDirection"), 1);
        assert_eq!(count(&mut b, "t.setSelectionRange(1, 2)"), 1);
        assert_eq!(count(&mut b, "t.selectionStart = 1"), 1);
        assert_eq!(count(&mut b, "t.selectionEnd = 3"), 1);
        assert_eq!(count(&mut b, "t.selectionDirection = 'backward'"), 1);
        assert_eq!(count(&mut b, "t.select()"), 1);
        // a value that differs reads the old selection and measures the new value; an equal value scans nothing
        assert_eq!(count(&mut b, "t.value = 'abcdeg'"), 2);
        assert_eq!(count(&mut b, "t.value = 'abcdeg'"), 0);
        // a not-dirty value follows the attribute
        assert_eq!(count(&mut b, "var u = document.createElement('input'); u.defaultValue = 'abc'"), 2);
        assert_eq!(count(&mut b, "u.value = 'q'; u.defaultValue = 'zzz'"), 2, "only the dirty value change scans; the default of a dirty input does not");
        // no selection, no scan
        assert_eq!(count(&mut b, "var e = document.createElement('input'); e.type = 'email'; e.value = 'a@b'; e.selectionStart; e.defaultValue = 'x'"), 0);
    });
}

#[test]
fn selectionchange_is_a_coalesced_bubbling_task_of_the_input() {
    big(|| {
        let mut b = settled(3_000_000, SchedLimits::default());
        js(&mut b, WATCH);
        js(&mut b, "t.addEventListener('selectionchange', L('t')); document.getElementById('box').addEventListener('selectionchange', L('box')); document.addEventListener('selectionchange', L('doc')); window.addEventListener('selectionchange', L('win'));");
        assert_eq!(js(&mut b, "t.setSelectionRange(1, 3); Promise.resolve().then(function () { log.push('micro') }); log.length"), "0");
        assert_eq!(tasks(&b), 2, "selectionchange and select");
        b.drain().unwrap();
        assert_eq!(js(&mut b, "log.join()"), "micro");
        assert_eq!(pump(&mut b).ran, 2);
        assert_eq!(
            js(&mut b, "log.slice(1).join(' ')"),
            "t:selectionchange:true:false:true:2:1,3,none box:selectionchange:true:false:true:3:1,3,none doc:selectionchange:true:false:true:3:1,3,none win:selectionchange:true:false:true:3:1,3,none"
        );
        // coalesced: four changes (extent and direction) queue one selectionchange and four select tasks
        js(&mut b, "log = []; t.setSelectionRange(0, 1); t.setSelectionRange(0, 2); t.setSelectionRange(0, 3); t.selectionDirection = 'forward'");
        assert_eq!(tasks(&b), 5);
        assert_eq!(pump(&mut b).ran, 5);
        assert_eq!(js(&mut b, "log.filter(function (s) { return s.slice(0, 2) === 't:' }).map(function (s) { return s.split(':').pop() }).join(' ')"), "0,3,forward");
        // the flag is cleared, so the next change queues again
        js(&mut b, "log = []; t.setSelectionRange(2, 3)");
        assert_eq!(tasks(&b), 2);
        pump(&mut b);
        assert_eq!(js(&mut b, "log.filter(function (s) { return s.slice(0, 2) === 't:' }).length"), "1");
        // no change, no task: the same range and direction, the same value
        js(&mut b, "t.setSelectionRange(2, 3); t.selectionStart = 2; t.selectionEnd = 3; t.selectionDirection = 'none'; t.value = t.value");
        assert_eq!(tasks(&b), 0);
    });
}

#[test]
fn selectionchange_is_scheduled_for_each_input_on_its_own() {
    big(|| {
        let mut b = settled(3_000_000, SchedLimits::default());
        js(&mut b, WATCH);
        js(&mut b, "v.value = 'abcdef'; document.addEventListener('selectionchange', function (e) { log.push(e.target.id) })");
        pump(&mut b);
        js(&mut b, "log = []; t.setSelectionRange(0, 1); v.setSelectionRange(0, 1); t.setSelectionRange(0, 2); v.setSelectionRange(0, 2); ro.setSelectionRange(0, 1)");
        assert_eq!(tasks(&b), 3 + 5);
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "t,v,ro");
        // a detached input is its own target
        js(&mut b, "log = []; var d = document.createElement('input'); d.value = 'ab'; d.addEventListener('selectionchange', function (e) { log.push('d:' + (e.target === d)) })");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "d:true");
    });
}

#[test]
fn selectionchange_flag_is_cleared_before_the_event_so_a_listener_can_queue_again() {
    big(|| {
        let mut b = settled(3_000_000, SchedLimits::default());
        js(&mut b, WATCH);
        js(&mut b, "t.addEventListener('selectionchange', function () { log.push('sc:' + S(t)); if (t.selectionEnd < 6) t.selectionEnd = 6 }); t.addEventListener('select', function () { log.push('sel:' + S(t)) });");
        js(&mut b, "t.setSelectionRange(1, 2)");
        assert_eq!(tasks(&b), 2);
        // the listener's own change is queued after the select task that was already there
        assert_eq!(pump(&mut b).ran, 4);
        assert_eq!(js(&mut b, "log.join(' ')"), "sc:1,2,none sel:1,6,none sc:1,6,none sel:1,6,none");
        assert_eq!(tasks(&b), 0);
    });
}

#[test]
fn selectionchange_runs_before_select_of_the_same_change_and_select_alone_when_one_is_pending() {
    big(|| {
        let mut b = settled(3_000_000, SchedLimits::default());
        js(&mut b, WATCH);
        js(&mut b, "t.addEventListener('selectionchange', function () { log.push('sc') }); t.addEventListener('select', function () { log.push('sel') });");
        js(&mut b, "t.setSelectionRange(1, 3)");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "sc,sel");
        js(&mut b, "log = []; t.setSelectionRange(0, 1); t.setSelectionRange(0, 2)");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "sc,sel,sel");
        // a timer between: tasks keep their queue order
        js(&mut b, "log = []; setTimeout(function () { log.push('T') }, 0); t.setSelectionRange(2, 3)");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "T,sc,sel");
    });
}

#[test]
fn value_and_driver_changes_schedule_selectionchange_only_when_the_selection_moves() {
    big(|| {
        let mut b = settled(3_000_000, SchedLimits::default());
        js(&mut b, WATCH);
        js(&mut b, "t.addEventListener('selectionchange', L('t')); t.addEventListener('select', L('sel'));");
        js(&mut b, "t.setSelectionRange(1, 3, 'backward')");
        pump(&mut b);
        js(&mut b, "log = []; t.value = 'zzzz'");
        assert_eq!(tasks(&b), 1, "selectionchange only: the value setter has no select event");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "t:selectionchange:true:false:true:2:4,4,none");
        // the same length with the caret already at the end: the value changed, the selection did not
        js(&mut b, "t.value = 'wxyz'");
        assert_eq!(tasks(&b), 0);
        assert_eq!(js(&mut b, "S(t)"), "4,4,none");
        // an equal value changes nothing
        js(&mut b, "t.setSelectionRange(1, 2); t.value = 'wxyz'");
        pump(&mut b);
        js(&mut b, "t.value = 'wxyz'; t.value = 'wxyz'");
        assert_eq!(tasks(&b), 0);
        // the driver
        js(&mut b, "log = []");
        let n = b.query("#t").unwrap().unwrap();
        b.type_text(n, "qq").unwrap();
        assert_eq!(tasks(&b), 1);
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "t:selectionchange:true:false:true:2:2,2,none");
        b.type_text(n, "rr").unwrap();
        assert_eq!(tasks(&b), 0, "caret was at the end of two units and stays there");
        // a not-dirty input follows its attribute value; a dirty one ignores `defaultValue`
        js(&mut b, "var u = document.createElement('input'); u.defaultValue = 'abcdef'");
        assert_eq!(tasks(&b), 1);
        pump(&mut b);
        js(&mut b, "u.setSelectionRange(4, 6)");
        pump(&mut b);
        js(&mut b, "u.defaultValue = 'ab'");
        assert_eq!(tasks(&b), 1, "the stored range is clamped to the shorter default");
        pump(&mut b);
        js(&mut b, "u.value = 'q'");
        pump(&mut b);
        js(&mut b, "u.defaultValue = 'zzz'");
        assert_eq!(tasks(&b), 0);
        // a type that has no selection queues nothing
        js(&mut b, "var e = document.createElement('input'); e.type = 'email'; e.value = 'a@b'");
        assert_eq!(tasks(&b), 0);
    });
}

#[test]
fn caps_are_checked_before_any_state_is_written() {
    big(|| {
        let sl = SchedLimits { max_selectionchange_tasks: 2, ..Default::default() };
        let mut b = make(3_000_000, sl);
        js(&mut b, WATCH);
        assert_eq!(js(&mut b, "t.value = 'abcdef'; v.value = 'abcdef'; 0"), "0");
        // the third input's value change is refused whole: the value stays
        assert_eq!(js(&mut b, "ro.value = 'zzzz'"), "limit:Host(\"selectionchange-tasks\")");
        assert_eq!(js(&mut b, "ro.value + '|' + S(ro)"), "ro|2,2,none");
        assert_eq!(tasks(&b), 2);
        pump(&mut b);
        js(&mut b, "t.setSelectionRange(0, 1); v.setSelectionRange(0, 1)");
        assert_eq!(tasks(&b), 4);
        assert_eq!(js(&mut b, "ro.setSelectionRange(0, 1)"), "limit:Host(\"selectionchange-tasks\")");
        assert_eq!(js(&mut b, "S(ro)"), "2,2,none");
        assert_eq!(tasks(&b), 4, "no select task was queued for the refused change");
        // an input that already has a pending task coalesces, so the cap does not apply to it
        assert_eq!(js(&mut b, "t.setSelectionRange(0, 2); S(t)"), "0,2,none");
        assert_eq!(tasks(&b), 5);
        pump(&mut b);
        assert_eq!(tasks(&b), 0);
        assert_eq!(js(&mut b, "ro.setSelectionRange(0, 1); S(ro)"), "0,1,none");
        // a refused select task leaves the flag of selectionchange unset and queues nothing
        let mut b = make(3_000_000, SchedLimits { max_select_tasks: 1, ..Default::default() });
        js(&mut b, WATCH);
        js(&mut b, "t.value = 'abcdef'; v.value = 'abcdef'");
        pump(&mut b);
        assert_eq!(js(&mut b, "t.setSelectionRange(0, 1); 0"), "0");
        assert_eq!(js(&mut b, "v.setSelectionRange(0, 1)"), "limit:Host(\"select-tasks\")");
        assert_eq!(js(&mut b, "S(v)"), "6,6,none");
        assert_eq!(tasks(&b), 2);
        pump(&mut b);
        js(&mut b, "v.setSelectionRange(0, 1)");
        assert_eq!(tasks(&b), 2, "the flag was not left set by the refused call");
    });
}

#[test]
fn no_selectionchange_listener_runs_after_a_fatal_pump_error() {
    big(|| {
        let mut b = make(20_000, SchedLimits::default());
        js(&mut b, "t.value = 'abcdef'; var hits = 0; t.addEventListener('selectionchange', function () { hits++ })");
        pump(&mut b);
        js(&mut b, "hits = 0");
        js(&mut b, "setTimeout(function () { for (;;) {} }, 0); t.setSelectionRange(1, 2)");
        assert!(matches!(b.run_scheduled_tasks(100).unwrap_err(), Error::Limit(_)));
        assert!(b.run_scheduled_tasks(100).is_err());
        assert!(b.advance_time(10.0, 100).is_err());
        assert_eq!(b.run_script("hits").map(|v| b.it.display(v)).ok(), Some("0".to_string()));
    });
}

#[test]
fn pending_input_tasks_survive_collection_and_call_listeners_of_other_scripts() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var hits = []; document.addEventListener('selectionchange', function (e) { hits.push('sc:' + e.target.id) }); document.addEventListener('select', function (e) { hits.push('sel:' + e.target.id) });");
        js(&mut b, "(function () { var d = document.createElement('input'); d.id = 'gc'; d.value = 'abcdef'; document.getElementById('box').appendChild(d); })();");
        js(&mut b, "document.getElementById('gc').setSelectionRange(1, 2)");
        let queued = tasks(&b);
        assert_eq!(queued, 2);
        b.collect(&[]).unwrap();
        b.collect(&[]).unwrap();
        assert_eq!(tasks(&b), queued);
        assert_eq!(pump(&mut b).ran, 2);
        assert_eq!(js(&mut b, "hits.join()"), "sc:gc,sel:gc");
        assert_eq!(tasks(&b), 0);
    });
}

#[test]
fn focus_and_blur_neither_move_nor_reset_the_selection_nor_queue_tasks() {
    big(|| {
        let mut b = settled(3_000_000, SchedLimits::default());
        js(&mut b, WATCH);
        js(&mut b, "t.setSelectionRange(1, 4, 'backward')");
        pump(&mut b);
        js(&mut b, "['focus', 'blur', 'focusin', 'focusout'].forEach(function (n) { t.addEventListener(n, function () { log.push(n + ':' + S(t)) }) });");
        js(&mut b, "t.focus(); v.focus(); t.focus(); t.blur()");
        let n = b.query("#t").unwrap().unwrap();
        b.focus(n).unwrap();
        b.blur().unwrap();
        assert_eq!(tasks(&b), 0);
        assert_eq!(js(&mut b, "log.join(' ')"), ["focus", "focusin", "blur", "focusout", "focus", "focusin", "blur", "focusout", "focus", "focusin", "blur", "focusout"].map(|e| format!("{e}:1,4,backward")).join(" "));
        assert_eq!(js(&mut b, "S(t) + '|' + S(v)"), "1,4,backward|3,3,none");
        assert_eq!(b.input_selection(n), Some((1, 4, "backward")));
    });
}

#[test]
fn a_focus_listener_that_selects_queues_after_the_focus_events_and_they_run_in_the_pump() {
    big(|| {
        let mut b = settled(3_000_000, SchedLimits::default());
        js(&mut b, WATCH);
        js(&mut b, "t.addEventListener('focus', function () { log.push('focus'); t.select(); log.push('after') }); t.addEventListener('focusin', function () { log.push('focusin') });
            t.addEventListener('selectionchange', function () { log.push('sc') }); t.addEventListener('select', function () { log.push('sel') });");
        js(&mut b, "t.focus()");
        assert_eq!(js(&mut b, "log.join()"), "focus,after,focusin");
        assert_eq!(tasks(&b), 2);
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "focus,after,focusin,sc,sel");
        // already selected: focusing again changes nothing
        js(&mut b, "t.blur(); log = []; t.focus()");
        assert_eq!(js(&mut b, "log.join()"), "focus,after,focusin");
        assert_eq!(tasks(&b), 0);
        // the driver's focus gives the same order
        js(&mut b, "t.blur(); t.setSelectionRange(1, 2)");
        pump(&mut b);
        js(&mut b, "log = []");
        let n = b.query("#t").unwrap().unwrap();
        b.focus(n).unwrap();
        assert_eq!(js(&mut b, "log.join()"), "focus,after,focusin");
        assert_eq!(tasks(&b), 2);
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "focus,after,focusin,sc,sel");
    });
}
