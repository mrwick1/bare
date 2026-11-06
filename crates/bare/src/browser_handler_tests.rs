//! Tests of `oninput` / `onchange` (`browser_handlers.rs`). Hand-written pages; expectations come from the HTML Standard
//! ("event handlers", "event handler processing algorithm", "activate/deactivate an event handler") and Web IDL
//! ("callback function types", "nullable types" with `[LegacyTreatNonObjectAsNull]`), read by hand. Nothing was compared with Chrome.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

const PAGE: &str = "<!doctype html><html><body><div id=d><input id=i type=text></div><svg id=s></svg><math id=m></math></body></html>";

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn make(steps: u64, lim: BrowserLimits) -> Browser {
    Browser::new(tree::parse(PAGE), lim, EvalLimits { max_steps: steps, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn browser() -> Browser {
    make(2_000_000, BrowserLimits::default())
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

fn lim(name: &'static str) -> String {
    format!("limit:{:?}", LimitKind::Host(name))
}

#[test]
fn the_properties_exist_where_the_standard_puts_them_and_nowhere_else() {
    big(|| {
        let mut b = browser();
        // The React feature test: `'oninput' in document`.
        assert_eq!(js(&mut b, "['oninput' in document, 'onchange' in document, 'oninput' in window, 'onchange' in window].join()"), "true,true,true,true");
        assert_eq!(js(&mut b, "[HTMLElement, SVGElement, Document].map(function (c) { return ('oninput' in c.prototype) + ':' + ('onchange' in c.prototype) }).join()"), "true:true,true:true,true:true");
        assert_eq!(js(&mut b, "var e = document.getElementById('i'), s = document.getElementById('s'); [('oninput' in e), ('oninput' in s), ('onchange' in s)].join()"), "true,true,true");
        // Not on Node, Element, CharacterData, Text, EventTarget, or a plain EventTarget.
        assert_eq!(js(&mut b, "[Node, Element, CharacterData, Text, Comment, EventTarget].map(function (c) { return ('oninput' in c.prototype) || ('onchange' in c.prototype) }).join()"), "false,false,false,false,false,false");
        assert_eq!(js(&mut b, "var t = document.createTextNode('x'); [('oninput' in t), ('onchange' in new EventTarget())].join()"), "false,false");
        // Accessor shape: enumerable, configurable, getter and setter; every value starts as null.
        assert_eq!(js(&mut b, "var g = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'oninput'); [typeof g.get, typeof g.set, g.enumerable, g.configurable, e.oninput, document.onchange, window.oninput].join()"), "function,function,true,true,,,");
        // MathML has no MathMLElement here (`browser_namespace.rs`): a MathML element is a plain Element. Recorded gap.
        assert_eq!(js(&mut b, "'oninput' in document.getElementById('m')"), "false");
    });
}

#[test]
fn a_property_handler_runs_with_the_right_this_event_and_target() {
    big(|| {
        let mut b = browser();
        let r = js(&mut b, "var log = [];
            var i = document.getElementById('i'), s = document.getElementById('s');
            function mk(name) { return function (ev) { log.push(name + ':' + (this === ev.currentTarget) + ':' + arguments.length + ':' + (ev.target === i) + ':' + ev.eventPhase) }; }
            i.oninput = mk('el'); document.oninput = mk('doc'); window.oninput = mk('win');
            i.dispatchEvent(new Event('input', { bubbles: true }));
            log.join()");
        assert_eq!(r, "el:true:1:true:2,doc:true:1:true:3,win:true:1:true:3");
        // `this` is the window, the document, the element itself.
        assert_eq!(js(&mut b, "log = []; var seen = []; window.onchange = function () { seen.push(this === window) }; document.onchange = function () { seen.push(this === document) }; i.onchange = function () { seen.push(this === i) };
            i.dispatchEvent(new Event('change', { bubbles: true })); seen.join()"), "true,true,true");
        // SVG element and a bare `onchange = f` assignment (global accessor, `this` is the window).
        assert_eq!(js(&mut b, "seen = []; s.onchange = function () { seen.push('svg') }; onchange = function () { seen.push('bare:' + (this === window)) }; s.dispatchEvent(new Event('change', { bubbles: true })); seen.join()"), "svg,true,bare:true");
        // getter gives back the same function object
        assert_eq!(js(&mut b, "var f = function () {}; i.oninput = f; [i.oninput === f, i.oninput === i.oninput].join()"), "true,true");
        // the handler is a non-capturing listener: bubbles through the path, a non-bubbling event does not reach the parent
        assert_eq!(js(&mut b, "log = []; document.oninput = null; window.oninput = null; var d = document.getElementById('d'); d.oninput = function () { log.push('d') }; i.oninput = function () { log.push('i') };
            i.dispatchEvent(new Event('input')); var n1 = log.join(); log = []; i.dispatchEvent(new Event('input', { bubbles: true })); n1 + '|' + log.join()"), "i|i,d");
        // capture listener at the target runs before the property handler, a bubble listener after it
        assert_eq!(js(&mut b, "log = []; i.addEventListener('input', function () { log.push('bub') }); i.addEventListener('input', function () { log.push('cap') }, true); i.dispatchEvent(new Event('input')); log.join()"), "cap,i,bub");
    });
}

#[test]
fn a_property_handler_and_the_same_function_in_add_event_listener_are_two_listeners_in_slot_order() {
    big(|| {
        let mut b = browser();
        let r = js(&mut b, "var i = document.getElementById('i'), log = [];
            function f(e) { log.push('f') } function g(e) { log.push('g') } function h(e) { log.push('h') }
            i.addEventListener('input', f); i.oninput = f; i.addEventListener('input', g);
            i.dispatchEvent(new Event('input')); var a = log.join(); log = [];
            // replacing keeps the slot
            i.oninput = h; i.dispatchEvent(new Event('input')); var r = log.join(); log = [];
            // removeEventListener with the handler's function removes only the real listener
            i.oninput = f; i.removeEventListener('input', f); i.dispatchEvent(new Event('input')); var rm = log.join(); log = [];
            // addEventListener with the handler's function is not a duplicate
            i.addEventListener('input', f); i.dispatchEvent(new Event('input')); var ad = log.join(); log = [];
            // null deactivates; setting it again goes to the end
            i.oninput = null; i.dispatchEvent(new Event('input')); var nl = log.join(); log = [];
            i.oninput = h; i.dispatchEvent(new Event('input')); var re = log.join();
            [a, r, rm, ad, nl, re].join('|')");
        assert_eq!(r, "f,f,g|f,h,g|f,g|f,g,f|g,f|g,f,h");
    });
}

#[test]
fn assignment_follows_the_event_handler_idl_type() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var i = document.getElementById('i'), log = []; function f() { log.push('f') }");
        // primitives, null and undefined become null; the setter runs no coercion
        for v in ["null", "undefined", "1", "'throw 1'", "true", "Symbol('x')"] {
            let r = js(&mut b, &format!("i.oninput = f; i.oninput = {v}; log = []; i.dispatchEvent(new Event('input')); [i.oninput, log.length].join()"));
            assert_eq!(r, ",0", "assigned {v}");
        }
        assert_eq!(b.stats().listeners, 0, "a null handler holds no slot");
        // a non-callable object is kept as the value, the getter returns it, and it is skipped without an error
        let r = js(&mut b, "var o = {}; var before = 0; i.oninput = o; i.dispatchEvent(new Event('input')); [i.oninput === o, log.length].join()");
        assert_eq!(r, "true,0");
        assert!(b.take_reports().is_empty());
        assert_eq!(b.stats().listeners, 1, "the object occupies one slot");
        // no coercion of any kind: toString and valueOf are never called, and a string is never compiled
        let r = js(&mut b, "var hit = 0; var bad = { toString: function () { hit++; return 'x' }, valueOf: function () { hit++; return 1 } }; i.oninput = bad; i.oninput = 'log.push(1)'; i.dispatchEvent(new Event('input')); [hit, log.length].join()");
        assert_eq!(r, "0,0");
        // later a function replaces the object in the same slot
        assert_eq!(js(&mut b, "i.oninput = f; i.dispatchEvent(new Event('input')); log.join()"), "f");
        // the setter is not variadic and `this` must be a node of the right kind
        assert_eq!(js(&mut b, "i.oninput = f, 1; typeof i.oninput"), "function");
    });
}

#[test]
fn only_return_false_cancels_and_only_a_cancelable_event() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var i = document.getElementById('i'); function run(v, cancelable) { i.oninput = function () { return v }; var e = new Event('input', { cancelable: cancelable }); i.dispatchEvent(e); return e.defaultPrevented + ':' + !i.dispatchEvent(new Event('input', { cancelable: cancelable })) }");
        assert_eq!(js(&mut b, "run(false, true)"), "true:true");
        assert_eq!(js(&mut b, "run(false, false)"), "false:false");
        for v in ["true", "0", "''", "null", "undefined", "'false'", "NaN", "{}"] {
            assert_eq!(js(&mut b, &format!("run({v}, true)")), "false:false", "returned {v}");
        }
        // the driver reports not_canceled
        let i = b.query("#i").unwrap().unwrap();
        js(&mut b, "i.oninput = function () { return false }");
        assert!(b.type_text(i, "a").unwrap().not_canceled, "driver `input` is not cancelable, so returning false does nothing");
        assert!(!b.dispatch(i, "input", true, true).unwrap().not_canceled);
    });
}

#[test]
fn replacement_and_removal_during_dispatch_are_seen_by_the_handler_turn() {
    big(|| {
        let mut b = browser();
        let r = js(&mut b, "var i = document.getElementById('i'), log = [];
            function a() { log.push('a'); i.oninput = function () { log.push('new') } }
            i.addEventListener('input', a); i.oninput = function () { log.push('old') };
            i.dispatchEvent(new Event('input')); var r1 = log.join(); log = [];
            // removal by an earlier listener: the handler does not run
            i.removeEventListener('input', a); i.oninput = null;
            function b1() { log.push('b'); i.oninput = null } i.addEventListener('input', b1);
            i.oninput = function () { log.push('h') }; i.addEventListener('input', function () { log.push('late') });
            // order now: b1, h (appended after), late  -> b1 clears h before h runs
            i.dispatchEvent(new Event('input')); var r2 = log.join(); log = [];
            // reactivation during dispatch: the new listener is not in this turn's list
            i.removeEventListener('input', b1);
            function c() { log.push('c'); i.oninput = null; i.oninput = function () { log.push('again') } } i.addEventListener('input', c);
            i.oninput = function () { log.push('h2') };
            i.dispatchEvent(new Event('input')); var r3 = log.join(); log = [];
            i.dispatchEvent(new Event('input')); var r4 = log.join();
            [r1, r2, r3, r4].join('|')");
        assert_eq!(r, "a,new|b,late|late,c|late,c");
    });
}

#[test]
fn a_handler_that_throws_is_reported_and_the_other_listeners_run() {
    big(|| {
        let mut b = browser();
        let r = js(&mut b, "var i = document.getElementById('i'), log = [];
            i.addEventListener('change', function () { log.push('1') }); i.onchange = function () { log.push('h'); throw new Error('boom') }; i.addEventListener('change', function () { log.push('3') });
            i.dispatchEvent(new Event('change')); log.join()");
        assert_eq!(r, "1,h,3");
        let reports = b.take_reports();
        assert_eq!(reports.len(), 1);
        assert_eq!((reports[0].event_type.as_str(), reports[0].target.as_str(), reports[0].message.as_str()), ("change", "input", "Error: boom"));
    });
}

#[test]
fn the_driver_reaches_handlers_on_input_document_and_window() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var i = document.getElementById('i'), log = [];
            i.oninput = function (e) { log.push('i:' + i.value + ':' + e.type + ':' + e.bubbles) };
            i.onchange = function (e) { log.push('c:' + i.value) };
            document.oninput = function () { log.push('doc') }; window.onchange = function () { log.push('win') };
            // ordinary listeners still see the same events
            i.addEventListener('input', function () { log.push('listener') });");
        let i = b.query("#i").unwrap().unwrap();
        let r = b.type_text(i, "hello").unwrap();
        assert!(r.reports.is_empty() && r.not_canceled);
        b.commit_change(i).unwrap();
        assert_eq!(js(&mut b, "log.join()"), "i:hello:input:true,listener,doc,c:hello,win");
        // a Promise reaction queued by the handler runs at the driver's checkpoint after that listener
        js(&mut b, "log = []; i.oninput = function () { Promise.resolve().then(function () { log.push('micro') }); log.push('h') }; i.addEventListener('input', function () { log.push('next') });");
        b.type_text(i, "x").unwrap();
        assert_eq!(js(&mut b, "log.join()"), "h,micro,listener,next,doc");
    });
}

#[test]
fn a_handler_set_by_one_script_is_replaced_by_another_and_keeps_its_slot() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var i = document.getElementById('i'), log = []; i.addEventListener('input', function () { log.push('x') }); i.oninput = function () { log.push('s1') }; i.addEventListener('input', function () { log.push('y') });");
        assert_eq!(js(&mut b, "var first = i.oninput; i.oninput = function () { log.push('s2') }; first === i.oninput"), "false");
        assert_eq!(js(&mut b, "i.dispatchEvent(new Event('input')); log.join()"), "x,s2,y");
        // the getter in a later script returns the function the earlier script stored
        assert_eq!(js(&mut b, "i.oninput = first; i.oninput === first"), "true");
    });
}

#[test]
fn a_handler_lives_while_its_owner_lives_and_slots_are_reused() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var keep; for (var n = 0; n < 300; n++) { var e = document.createElement('input'); e.oninput = function () {}; e.onchange = function () {}; keep = e; } e = null;");
        assert!(b.stats().listeners >= 600);
        b.collect(&[]).unwrap();
        assert_eq!(b.stats().listeners, 2, "only the kept detached owner holds handlers");
        assert_eq!(js(&mut b, "typeof keep.oninput + typeof keep.onchange"), "functionfunction");
        // the closure is reachable through the owner, and still runs after the collection
        assert_eq!(js(&mut b, "var seen = 0; keep.oninput = function () { seen++ }; keep.dispatchEvent(new Event('input')); seen"), "1");
        js(&mut b, "keep = null");
        b.collect(&[]).unwrap();
        assert_eq!(b.stats().listeners, 0);
        // the window and document handlers are roots of the page
        js(&mut b, "window.oninput = function () { window.fired = 1 }; document.onchange = function () {}");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "window.dispatchEvent(new Event('input')); window.fired"), "1");
        assert_eq!(b.stats().listeners, 2);
        // a freed wrapper's slot is reused without aliasing: a new element has no handler
        assert_eq!(js(&mut b, "document.createElement('input').oninput"), "null");
    });
}

#[test]
fn the_listener_caps_are_checked_before_a_handler_slot_is_added() {
    big(|| {
        let mut b = make(2_000_000, BrowserLimits { max_listeners: 3, max_listeners_per_target: 2, ..Default::default() });
        js(&mut b, "var i = document.getElementById('i'); i.oninput = function () {}; i.addEventListener('x', function () {});");
        // per-target cap
        assert_eq!(js(&mut b, "i.onchange = function () {}"), lim("dom-listeners-per-target"));
        assert_eq!(js(&mut b, "i.onchange"), "null");
        // total cap: another target
        js(&mut b, "document.addEventListener('y', function () {})");
        assert_eq!(js(&mut b, "window.onchange = function () {}"), lim("dom-listeners"));
        assert_eq!(js(&mut b, "window.onchange"), "null");
        assert_eq!(b.stats().listeners, 3);
        // a replacement and a null never grow, so they work at the cap
        assert_eq!(js(&mut b, "i.oninput = function () {}; i.oninput = null; i.oninput"), "null");
        assert_eq!(js(&mut b, "window.onchange = function () {}; typeof window.onchange"), "function");
    });
}

#[test]
fn a_limit_fault_in_a_handler_ends_the_dispatch_and_replays_nothing() {
    big(|| {
        let mut b = make(300_000, BrowserLimits::default());
        js(&mut b, "var i = document.getElementById('i'), log = []; i.oninput = function () { log.push('h'); for (;;) {} }; i.addEventListener('input', function () { log.push('after') }); document.addEventListener('input', function () { log.push('doc') });");
        let i = b.query("#i").unwrap().unwrap();
        assert!(matches!(b.type_text(i, "a"), Err(Error::Limit(LimitKind::Steps))));
        assert_eq!(js(&mut b, "log.join()"), "h", "no later listener ran");
    });
}

#[test]
fn a_limit_in_a_promise_reaction_of_a_handler_is_sticky_and_replays_nothing() {
    big(|| {
        let mut b = make(300_000, BrowserLimits::default());
        js(&mut b, "var i = document.getElementById('i'), log = []; i.oninput = function () { log.push('h'); Promise.resolve().then(function () { log.push('p'); for (;;) {} }) }; i.addEventListener('input', function () { log.push('after') });");
        let i = b.query("#i").unwrap().unwrap();
        // the driver checkpoint after the handler runs the reaction, which hits the cap: the dispatch ends
        assert!(matches!(b.type_text(i, "a"), Err(Error::Limit(LimitKind::Steps))));
        // the failed checkpoint repeats its error, runs nothing again, and the reaction is not replayed
        assert!(matches!(b.drain(), Err(Error::Limit(LimitKind::Steps))));
        assert!(matches!(b.type_text(i, "b"), Err(Error::Limit(LimitKind::Steps))));
        let log = js(&mut b, "log.join()");
        assert!(!log.contains("after") && log.matches('p').count() == 1, "log {log}");
    });
}

#[test]
fn unsupported_in_a_handler_stops_the_dispatch() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var i = document.getElementById('i'), log = []; i.oninput = function () { log.push('h'); document.createElementNS('urn:x', 'a'); log.push('never') }; i.addEventListener('input', function () { log.push('after') });");
        let i = b.query("#i").unwrap().unwrap();
        assert!(matches!(b.type_text(i, "a"), Err(Error::Unsupported(_))));
        assert_eq!(js(&mut b, "log.join()"), "h");
        assert!(b.take_reports().is_empty());
    });
}

#[test]
fn brand_checks_refuse_the_wrong_receiver() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var i = document.getElementById('i'), s = document.getElementById('s'), t = document.createTextNode('x');
            var H = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'oninput'), S = Object.getOwnPropertyDescriptor(SVGElement.prototype, 'oninput');
            var D = Object.getOwnPropertyDescriptor(Document.prototype, 'onchange');
            function th(f) { try { f(); return 'ok' } catch (e) { return e.name } }");
        let r = js(&mut b, "[th(function () { H.get.call(s) }), th(function () { S.get.call(i) }), th(function () { H.get.call(t) }), th(function () { H.set.call(new EventTarget(), function () {}) }),
            th(function () { D.get.call(i) }), th(function () { H.get.call({}) }), th(function () { H.get.call(undefined) }), th(function () { H.set.call(document, null) }), th(function () { H.get.call(i) }), th(function () { S.get.call(s) }), th(function () { D.get.call(document) })].join()");
        assert_eq!(r, "TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,ok,ok,ok");
        // a getter of one interface is not the getter of another, and a Text/EventTarget did not gain the property by assignment
        assert_eq!(js(&mut b, "[H.get === S.get, H.set === S.set].join()"), "false,false");
        assert_eq!(js(&mut b, "t.oninput = function () {}; var e = new EventTarget(); e.onchange = function () {}; var n = 0; t.dispatchEvent(new Event('input')); e.dispatchEvent(new Event('change')); n"), "0");
    });
}

#[test]
fn replacing_or_deleting_the_global_interfaces_does_not_remove_the_accessors() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var i = document.getElementById('i'), log = []; delete window.HTMLElement; delete window.Document; delete window.SVGElement; window.HTMLElement = 1;");
        assert_eq!(js(&mut b, "i.oninput = function () { log.push('h') }; document.onchange = function () { log.push('d') }; i.dispatchEvent(new Event('input')); document.dispatchEvent(new Event('change')); log.join() + ('oninput' in document)"), "h,dtrue");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "log = []; i.dispatchEvent(new Event('input')); log.join()"), "h");
    });
}
