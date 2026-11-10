//! Tests of DOM focus (`browser_focus.rs`). Real scripts run in a real realm over a real `Document`. Expectations are
//! written by hand from the HTML, DOM and UI Events standards. Nothing here was compared with a real browser.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const PAGE: &str = "<!doctype html><html><head></head><body><div id=wrap><button id=a>a</button><input id=b><button id=c disabled>c</button>\
<div id=d></div><div id=e tabindex=-1></div><a id=f href=x>f</a><a id=g>g</a><div id=h hidden><button id=h1>h</button></div>\
<div id=i inert><button id=i1>i</button></div><input id=j type=hidden><fieldset id=k disabled><legend><button id=k1>x</button></legend><button id=k2>y</button></fieldset>\
<div id=l tabindex=abc></div><div id=m contenteditable=true></div><details id=n><summary id=n1>s</summary><button id=n2>t</button></details>\
<select id=o></select><textarea id=p></textarea><span id=q tabindex=3></span></div></body></html>";

fn browser_with(html: &str, lim: BrowserLimits) -> Browser {
    Browser::new(tree::parse(html), lim, EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn browser() -> Browser {
    browser_with(PAGE, BrowserLimits::default())
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

const SETUP: &str = "var $ = function (s) { return document.getElementById(s); }; var log = [];\
 var rec = function (name) { return function (e) { log.push(name + ':' + e.type + ':' + e.eventPhase + ':' + (e.relatedTarget && e.relatedTarget.id)); }; };\
 var active = function () { return document.activeElement && (document.activeElement.id || document.activeElement.tagName); }; 0";

fn setup() -> Browser {
    let mut b = browser();
    assert_eq!(js(&mut b, SETUP), "0");
    b
}

#[test]
fn active_element_defaults_to_body_and_follows_focus() {
    big(|| {
        let mut b = setup();
        assert_eq!(js(&mut b, "active() + ',' + (document.activeElement === document.body)"), "BODY,true");
        assert_eq!(js(&mut b, "$('a').focus(); active() + ',' + (document.activeElement === $('a'))"), "a,true");
        assert_eq!(js(&mut b, "$('b').focus(); active()"), "b");
        assert_eq!(js(&mut b, "$('b').blur(); active()"), "BODY");
        assert_eq!(js(&mut b, "$('a').blur(); active()"), "BODY");
        assert_eq!(js(&mut b, "typeof HTMLElement.prototype.focus + typeof HTMLElement.prototype.blur + $('a').focus()"), "functionfunctionundefined");
        // The fallback is the body, then the document element, then null.
        assert_eq!(js(&mut b, "document.body.remove(); document.activeElement === document.documentElement"), "true");
        assert_eq!(js(&mut b, "document.documentElement.remove(); String(document.activeElement)"), "null");
    });
}

#[test]
fn event_order_paths_and_related_targets() {
    big(|| {
        let mut b = setup();
        let s = "['focus', 'blur', 'focusin', 'focusout'].forEach(function (t) {\
             window.addEventListener(t, rec('W-cap'), true); document.addEventListener(t, rec('D-cap'), true);\
             document.body.addEventListener(t, rec('body'), false); $('a').addEventListener(t, rec('a'), false); $('b').addEventListener(t, rec('b'), false);\
             $('a').addEventListener(t, rec('a-cap'), true); window.addEventListener(t, rec('W'), false); }); 0";
        assert_eq!(js(&mut b, s), "0");
        // First focus: related target null, focus before focusin, focus does not bubble, focusin does.
        assert_eq!(js(&mut b, "$('a').focus(); log.join()"), "W-cap:focus:1:null,D-cap:focus:1:null,a-cap:focus:2:null,a:focus:2:null,W-cap:focusin:1:null,D-cap:focusin:1:null,a-cap:focusin:2:null,a:focusin:2:null,body:focusin:3:null,W:focusin:3:null");
        // Move: blur, focusout (related = new), focus, focusin (related = old).
        assert_eq!(js(&mut b, "log.length = 0; $('b').focus(); log.join()"), "W-cap:blur:1:b,D-cap:blur:1:b,a-cap:blur:2:b,a:blur:2:b,W-cap:focusout:1:b,D-cap:focusout:1:b,a-cap:focusout:2:b,a:focusout:2:b,body:focusout:3:b,W:focusout:3:b,\
W-cap:focus:1:a,D-cap:focus:1:a,b:focus:2:a,W-cap:focusin:1:a,D-cap:focusin:1:a,b:focusin:2:a,body:focusin:3:a,W:focusin:3:a");
        // blur(): related target null, no window blur beyond the element path.
        assert_eq!(js(&mut b, "log.length = 0; $('b').blur(); log.join()"), "W-cap:blur:1:null,D-cap:blur:1:null,b:blur:2:null,W-cap:focusout:1:null,D-cap:focusout:1:null,b:focusout:2:null,body:focusout:3:null,W:focusout:3:null");
        // Already focused: nothing fires.
        assert_eq!(js(&mut b, "$('a').focus(); log.length = 0; $('a').focus(); log.length"), "0");
        // Flags of each event and the FocusEvent brand.
        let flags = "var seen = {}; ['focus','blur','focusin','focusout'].forEach(function (t) { $('b').addEventListener(t, function (e) { seen[t] = [e.bubbles, e.cancelable, e.composed, e.isTrusted, e instanceof FocusEvent, e.target === $('b'), e.currentTarget === $('b')].join(); e.preventDefault(); seen[t + 'DP'] = e.defaultPrevented; }); }); $('b').focus(); $('a').focus(); JSON.stringify(seen)";
        assert_eq!(js(&mut b, flags), "{\"focus\":\"false,false,true,false,true,true,true\",\"focusDP\":false,\"focusin\":\"true,false,true,false,true,true,true\",\"focusinDP\":false,\"blur\":\"false,false,true,false,true,true,true\",\"blurDP\":false,\"focusout\":\"true,false,true,false,true,true,true\",\"focusoutDP\":false}");
        assert_eq!(js(&mut b, "active()"), "a");
    });
}

#[test]
fn focusability_policy() {
    big(|| {
        let mut b = setup();
        let ids = "a b c d e f g h1 i1 j k1 k2 l m n1 n2 o p q h i".split(' ').collect::<Vec<_>>();
        let probe = format!("['{}'].map(function (id) {{ $(id).focus(); var ok = document.activeElement === $(id); $(id).blur(); return id + (ok ? '+' : '-'); }}).join(' ')", ids.join("','"));
        assert_eq!(
            js(&mut b, &probe),
            "a+ b+ c- d- e+ f+ g- h1- i1- j- k1+ k2- l- m+ n1+ n2- o+ p+ q+ h- i-"
        );
        // tabindex added or removed by script changes the answer; disabled toggles too.
        assert_eq!(js(&mut b, "$('d').setAttribute('tabindex', ' +0'); $('d').focus(); var r = active(); $('d').removeAttribute('tabindex'); $('a').focus(); $('d').focus(); r + ',' + active()"), "d,a");
        assert_eq!(js(&mut b, "$('a').setAttribute('disabled', ''); $('a').focus(); var x = active(); $('a').removeAttribute('disabled'); $('a').focus(); x + ',' + active()"), "BODY,a");
        // details: open makes the content rendered
        assert_eq!(js(&mut b, "$('n').setAttribute('open', ''); $('n2').focus(); active()"), "n2");
        // detached and non-connected nodes: no-op, no events
        assert_eq!(js(&mut b, "$('a').focus(); var x = document.createElement('button'); var n = 0; x.addEventListener('focus', function () { n++; }); x.focus(); var w = document.createElement('div'); w.appendChild(x); x.focus(); n + ',' + active() + ',' + x.isConnected"), "0,a,false");
        // svg and math elements have no focus()
        assert_eq!(js(&mut b, "typeof document.createElementNS('http://www.w3.org/1999/xhtml', 'button').focus"), "function");
    });
}

#[test]
fn focus_options_are_handled_deliberately() {
    big(|| {
        let mut b = setup();
        assert_eq!(js(&mut b, "$('a').focus({preventScroll: true}); active()"), "a");
        assert_eq!(js(&mut b, "$('b').focus({preventScroll: false}); active()"), "b");
        assert_eq!(js(&mut b, "$('a').focus(undefined); $('b').focus(null); $('a').focus({}); active()"), "a");
        assert_eq!(js(&mut b, "$('b').focus(5)"), "throw:TypeError:Failed to execute 'focus' on 'HTMLElement': parameter 1 is not of type 'FocusOptions'.");
        assert_eq!(js(&mut b, "$('b').focus({focusVisible: true})"), "unsupported:focus option focusVisible (no :focus-visible state)");
        assert_eq!(js(&mut b, "active()"), "a");
        assert_eq!(js(&mut b, "HTMLElement.prototype.focus.call({})"), "throw:TypeError:Illegal invocation");
        assert_eq!(js(&mut b, "document.activeElement = 3; active()"), "a");
    });
}

#[test]
fn reentrant_transitions_and_removal() {
    big(|| {
        let mut b = setup();
        // focus() called in blur: the nested focus wins, the outer one does not fire focus on its target.
        let s = "$('a').focus(); $('a').addEventListener('blur', function () { log.push('a-blur-active:' + active()); $('p').focus(); });\
 $('b').addEventListener('focus', function () { log.push('b-focus'); }); $('b').focus(); log.join() + '|' + active()";
        assert_eq!(js(&mut b, s), "a-blur-active:BODY|p");
        let mut b = setup();
        // Target removed in the blur listener: focus ends on the body, no events on the removed target.
        let s = "var bb = $('b'); log.length = 0; $('a').focus(); $('a').addEventListener('blur', function () { bb.remove(); }); $('b').addEventListener('focus', rec('b-focus')); $('b').addEventListener('focusin', rec('b-in'));\
 $('a').focus(); $('b').focus(); log.join() + '|' + active() + '|' + (bb.parentNode === null)";
        assert_eq!(js(&mut b, s), "|BODY|true");
        let mut b = setup();
        // The removed target of focus() never gets focus events.
        assert_eq!(js(&mut b, "log.length = 0; $('a').focus(); $('o').addEventListener('focus', rec('o')); $('a').addEventListener('blur', function () { $('o').remove(); }); $('o').focus(); log.join() + '|' + active()"), "|BODY");
        let mut b = setup();
        // The focused element is removed in its own focus listener: fixup to the body, focusin is skipped.
        let s = "log.length = 0; var q = $('q'); q.addEventListener('focus', function () { q.remove(); log.push('removed'); }); q.addEventListener('focusin', rec('q-in')); q.focus(); log.join() + '|' + active()";
        assert_eq!(js(&mut b, s), "removed|BODY");
        let mut b = setup();
        // Removing the focused element: fixup without blur events.
        let s = "log.length = 0; var t = $('a'); t.addEventListener('blur', rec('a-blur')); t.addEventListener('focusout', rec('a-out')); t.focus(); t.remove(); active() + ',' + log.length";
        assert_eq!(js(&mut b, s), "BODY,0");
        // A removed element can be focused again after it is put back.
        assert_eq!(js(&mut b, "document.getElementById('wrap').appendChild(t); t.focus(); active()"), "a");
        let mut b = setup();
        // Focus listeners that move focus to each other end with a limit, not with a stack overflow.
        let s = "var x = $('a'), y = $('b'); x.addEventListener('focus', function () { y.focus(); }); y.addEventListener('focus', function () { x.focus(); }); x.focus(); 1";
        assert_eq!(js(&mut b, s), format!("limit:{:?}", LimitKind::Host("dom-focus-depth")));
        assert_eq!(b.dom.st.borrow().focus.depth_for_test(), 0);
        // The realm still works.
        assert_eq!(js(&mut b, "typeof document.activeElement"), "object");
    });
}

#[test]
fn listener_errors_cross_script_callbacks_and_reports() {
    big(|| {
        let mut b = setup();
        assert_eq!(js(&mut b, "$('a').addEventListener('blur', function () { throw new Error('boom'); }); $('b').addEventListener('focus', function () { log.push('b-focus'); }); $('a').focus(); $('b').focus(); log.join() + '|' + active()"), "b-focus|b");
        let r = b.take_reports();
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].event_type.as_str(), r[0].target.as_str(), r[0].message.lines().next().unwrap()), ("blur", "button", "Error: boom"));
        assert!(r[0].message.contains("\n    at "), "{r:?}");
        // A function defined by an earlier script and a handleEvent object both run.
        assert_eq!(js(&mut b, "var h = { handleEvent: function (e) { log.push('he:' + e.type); } }; $('p').addEventListener('focusin', h); 0"), "0");
        assert_eq!(js(&mut b, "log.length = 0; $('p').focus(); log.join()"), "he:focusin");
    });
}

#[test]
fn focus_event_constructor_and_synthetic_dispatch() {
    big(|| {
        let mut b = setup();
        assert_eq!(js(&mut b, "var f = new FocusEvent('focus', {relatedTarget: $('b'), bubbles: true}); [f.type, f.bubbles, f.cancelable, f.composed, f.relatedTarget === $('b'), f.isTrusted, f instanceof Event].join()"), "focus,true,false,false,true,false,true");
        assert_eq!(js(&mut b, "new FocusEvent('blur').relatedTarget"), "null");
        assert_eq!(js(&mut b, "new FocusEvent('blur', {relatedTarget: window}).relatedTarget === window"), "true");
        assert_eq!(js(&mut b, "new FocusEvent('blur', {relatedTarget: 5})"), "throw:TypeError:Failed to construct 'FocusEvent': Failed to read the 'relatedTarget' property from 'FocusEventInit': The provided value is not of type 'EventTarget'.");
        assert_eq!(js(&mut b, "new FocusEvent()"), "throw:TypeError:Failed to construct 'FocusEvent': 1 argument required, but only 0 present.");
        assert_eq!(js(&mut b, "FocusEvent('x')"), "throw:TypeError:Failed to construct: please use the 'new' operator");
        assert_eq!(js(&mut b, "Object.getPrototypeOf(FocusEvent.prototype) === Event.prototype && Event.prototype.relatedTarget === undefined"), "true");
        // dispatchEvent of a synthetic focus event runs listeners only: no focus change, no fake trusted action.
        assert_eq!(js(&mut b, "var n = 0; $('b').addEventListener('focus', function (e) { n++; log.push(e.isTrusted + ':' + (e.relatedTarget === $('a'))); }); $('b').dispatchEvent(new FocusEvent('focus', {relatedTarget: $('a')})); $('b').dispatchEvent(new Event('focusin')); n + '|' + log.join() + '|' + active()"), "1|false:true|BODY");
        // A subclass gets its own prototype.
        assert_eq!(js(&mut b, "class MyFocus extends FocusEvent {}; var mf = new MyFocus('x'); [mf instanceof MyFocus, mf instanceof FocusEvent, mf.type].join()"), "true,true,x");
        assert_eq!(js(&mut b, "typeof $('a').focus + typeof FocusEvent + (new Event('x') instanceof FocusEvent)"), "functionfunctionfalse");
    });
}

#[test]
fn saved_events_trace_related_nodes_and_slots_are_reused() {
    big(|| {
        let mut b = setup();
        assert_eq!(js(&mut b, "var saved; $('b').addEventListener('focus', function (e) { saved = e; }); $('a').focus(); $('b').focus(); var keep = saved.relatedTarget; 1"), "1");
        // Remove the related node, drop other references, collect: the saved event still reaches it.
        assert_eq!(js(&mut b, "$('a').remove(); keep = null; 1"), "1");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "saved.relatedTarget.id + ',' + saved.relatedTarget.isConnected + ',' + saved.target.id"), "a,false,b");
        // Rounds of new nodes, focus moves, removal and collection keep the counters bounded.
        let round = "(function () { var w = document.getElementById('wrap'); for (var i = 0; i < 20; i++) { var x = document.createElement('button'); var y = document.createElement('input'); w.appendChild(x); w.appendChild(y); x.focus(); y.focus(); x.remove(); y.remove(); } return active(); })()";
        let base = b.stats();
        for _ in 0..30 {
            assert_eq!(js(&mut b, round), "BODY");
            b.collect(&[]).unwrap();
        }
        let s = b.stats();
        assert!(s.events <= base.events + 4 && s.wrappers <= base.wrappers + 4 && s.listeners == base.listeners, "{base:?} {s:?}");
        assert_eq!(js(&mut b, "saved.relatedTarget.id"), "a");
    });
}

#[test]
fn driver_focus_uses_the_event_engine_and_checkpoints() {
    big(|| {
        let mut b = setup();
        let (a, c, d) = (b.query("#a").unwrap().unwrap(), b.query("#c").unwrap().unwrap(), b.query("#d").unwrap().unwrap());
        let bb = b.query("#b").unwrap().unwrap();
        assert_eq!(js(&mut b, "$('b').addEventListener('blur', function () { log.push('blur'); Promise.resolve().then(function () { log.push('micro'); }); }); $('b').addEventListener('focusout', function () { log.push('out'); }); $('b').addEventListener('focus', function () { throw new Error('x'); }); 0"), "0");
        assert_eq!(b.active_element().unwrap(), None);
        let o = b.focus(bb).unwrap();
        assert!(o.focused && o.reports.len() == 1 && o.reports[0].event_type == "focus", "{o:?}");
        assert_eq!(b.active_element().unwrap(), Some(bb));
        // The checkpoint runs after each listener the driver started: micro lands between blur and focusout.
        assert!(b.focus(a).unwrap().focused);
        assert_eq!(js(&mut b, "log.join()"), "blur,micro,out");
        // From script the microtask waits for the end of the script.
        assert_eq!(js(&mut b, "log.length = 0; $('b').focus(); $('a').focus(); $('b').focus(); log.join()"), "blur,out");
        b.drain().unwrap();
        assert_eq!(js(&mut b, "log.join()"), "blur,out,micro");
        // Ineligible and non-element targets.
        assert!(!b.focus(c).unwrap().focused && !b.focus(d).unwrap().focused);
        assert_eq!(b.active_element().unwrap(), Some(bb));
        let t = b.document().borrow_mut().create_text("t");
        assert!(matches!(b.focus(t), Err(Error::Unsupported(_))));
        // blur() through the driver, then again with nothing focused.
        assert!(!b.blur().unwrap().focused);
        assert_eq!(b.active_element().unwrap(), None);
        assert!(!b.blur().unwrap().focused);
        assert_eq!(js(&mut b, "active()"), "BODY");
        assert!(b.stats().events > 0);
    });
}

#[test]
fn fatal_errors_stop_the_transition_and_the_cap_is_charged() {
    big(|| {
        // max_events: the event that cannot be made ends the call; no later listener runs.
        let mut b = browser_with(PAGE, BrowserLimits { max_events: 3, ..Default::default() });
        assert_eq!(js(&mut b, SETUP), "0");
        assert_eq!(js(&mut b, "$('a').focus(); ['focus', 'focusin', 'blur', 'focusout'].forEach(function (t) { $('b').addEventListener(t, rec('b')); $('a').addEventListener(t, rec('a')); }); log.length = 0; 0"), "0");
        assert_eq!(js(&mut b, "$('b').focus()"), format!("limit:{:?}", LimitKind::Host("dom-events")));
        assert_eq!(js(&mut b, "log.join()"), "a:blur:2:b");
        let bb = b.query("#b").unwrap().unwrap();
        assert!(matches!(b.focus(bb), Err(Error::Limit(_))));
        assert_eq!(js(&mut b, "log.join()"), "a:blur:2:b");
        assert_eq!(b.dom.st.borrow().focus.depth_for_test(), 0);
        // A step limit hit inside a listener ends the transition too.
        let mut b = Browser::new(tree::parse(PAGE), BrowserLimits::default(), EvalLimits { max_steps: 200_000, ..Default::default() }, HeapLimits::default()).unwrap();
        assert_eq!(js(&mut b, SETUP), "0");
        assert_eq!(js(&mut b, "$('a').addEventListener('blur', function () { for (;;) {} }); $('b').addEventListener('focus', function () { log.push('no'); }); $('a').focus(); 0"), "0");
        let r = js(&mut b, "$('b').focus()");
        assert!(r.starts_with("limit:"), "{r}");
        assert_eq!(js(&mut b, "log.length"), "0");
        // The ancestor walk is charged: a deep tree and a tiny budget end in a limit, not in free work.
        let deep = format!("<body>{}<button id=z>z</button>{}", "<div>".repeat(400), "</div>".repeat(400));
        let z = browser_with(&deep, BrowserLimits::default()).query("#z").unwrap().unwrap();
        let mut b = Browser::new(tree::parse(&deep), BrowserLimits::default(), EvalLimits { max_steps: 300, ..Default::default() }, HeapLimits::default()).unwrap();
        assert!(matches!(b.focus(z), Err(Error::Limit(_))));
        let mut b = Browser::new(tree::parse(&deep), BrowserLimits::default(), EvalLimits { max_steps: 100_000, ..Default::default() }, HeapLimits::default()).unwrap();
        assert!(b.focus(z).unwrap().focused);
    });
}

#[test]
fn related_target_requires_a_native_focus_event() {
    big(|| {
        let mut b = setup();
        assert_eq!(js(&mut b, "var relatedGetter = Object.getOwnPropertyDescriptor(FocusEvent.prototype, 'relatedTarget').get; var rejected = 0; [new Event('focus'), new CustomEvent('focus'), Object.create(FocusEvent.prototype)].forEach(function(e) { try { relatedGetter.call(e); } catch(e) { if(e instanceof TypeError) rejected++; } }); rejected"), "3");
        assert_eq!(js(&mut b, "relatedGetter.call(new FocusEvent('focus')) === null"), "true");
    });
}

#[test]
fn a_blur_listener_that_moves_focus_stops_the_old_transition() {
    big(|| {
        let mut b = setup();
        assert_eq!(js(&mut b, "$('a').focus(); $('a').addEventListener('blur', function(){ log.push('blur-a'); $('e').focus(); }); $('a').addEventListener('focusout', function(){ log.push('stale-focusout'); }); $('b').focus(); active() + ':' + log.join()"), "e:blur-a");
    });
}

#[test]
fn active_element_checks_the_document_receiver_before_focus_state() {
    big(|| {
        let mut b = setup();
        assert_eq!(js(&mut b, "$('a').focus(); var activeGetter = Object.getOwnPropertyDescriptor(Document.prototype, 'activeElement').get; var badActive = 0; [{}, $('a'), Object.create(Document.prototype)].forEach(function(o){ try { activeGetter.call(o); } catch(e) { if(e instanceof TypeError) badActive++; } }); badActive"), "3");
    });
}
