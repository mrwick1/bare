//! Tests of two heap errors that stopped target.com (`page stopped: Heap`). A page must never cause a heap error.
//! 1. `NeedsHost`: one write to `window` asked the host more often than the evaluator allows. The frames object on the chain of
//!    `window` asked again each time its prepaid count ran out (`browser_frames.rs`).
//! 2. `Stale`: the prototype of `HTMLUnknownElement` was not a root while the custom element registry was unused, so a
//!    collect freed it and the next unknown element got a dead prototype (`browser_custom.rs`).

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser(html: &str) -> Browser {
    Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    }
}

/// A page with an iframe (named access on) and more than 32 nodes (the old prepaid count applied).
fn frames_page() -> String {
    format!("<!doctype html><body><iframe name=fr id=fi></iframe>{}</body>", "<div></div>".repeat(40))
}

#[test]
fn writes_of_on_names_to_window_with_an_iframe_never_stop_at_the_heap() {
    big(|| {
        let mut b = browser(&frames_page());
        // Each write meets the frames object, `EventTarget.prototype` and the own lazy names of `window`: up to three asks.
        assert_eq!(js(&mut b, "for (var i = 0; i < 40; i++) window['onzz' + i] = i; window.onzz0 + ',' + window.onzz39"), "0,39");
        assert_eq!(js(&mut b, "var n = 0; for (var i = 0; i < 40; i++) { n += typeof window['nope' + i] === 'undefined'; window['onclick'] = null; } n"), "40");
        assert_eq!(js(&mut b, "for (var i = 0; i < 20; i++) document['onyy' + i] = i; document.onyy19"), "19");
    });
}

#[test]
fn named_iframe_access_follows_the_tree() {
    big(|| {
        let mut b = browser(&frames_page());
        let f = "document.getElementById('fi')";
        assert_eq!(js(&mut b, &format!("[window.fr === {f}.contentWindow, window[0] === window.fr, window.fi === window.fr, document.fr === {f}, 'fr' in window, window.length].join()")), "true,true,true,true,true,1");
        // A new iframe, a new name and a removed iframe show at once.
        js(&mut b, "var g = document.createElement('iframe'); g.name = 'late'; document.body.appendChild(g);");
        assert_eq!(js(&mut b, "[window.late === g.contentWindow, window[1] === window.late, window.length].join()"), "true,true,2");
        js(&mut b, "g.name = 'later';");
        assert_eq!(js(&mut b, "[typeof window.late, window.later === g.contentWindow, Object.keys(Object.getPrototypeOf(window)).join(' ')].join()"), "undefined,true,0 fr fi 1 later");
        js(&mut b, "g.remove();");
        assert_eq!(js(&mut b, "[typeof window.later, window.length, 'later' in window].join()"), "undefined,1,false");
    });
}

#[test]
fn cached_wrapper_prototypes_survive_a_collect() {
    big(|| {
        let mut b = browser("<!doctype html><body></body>");
        // One element of each kind that `Dom::wrap` gives a cached prototype. The wrappers die at the collect; the prototypes must not.
        let make = "['walrus', 'div', 'canvas', 'table', 'iframe', 'link', 'style', 'a', 'area', 'template', 'img', 'slot', 'audio', 'video', \
            'input', 'form', 'button', 'label', 'script', 'body', 'frameset', 'select', 'option', 'optgroup', 'textarea', 'x-foo']\
            .map(function (t) { return Object.prototype.toString.call(document.createElement(t)) })\
            .concat(['svg', 'rect', 'text'].map(function (t) { return Object.prototype.toString.call(document.createElementNS('http://www.w3.org/2000/svg', t)) }))\
            .concat([Object.prototype.toString.call(document.createElementNS('http://www.w3.org/1998/Math/MathML', 'math'))]).join()";
        let first = js(&mut b, make);
        assert!(first.starts_with("[object HTMLUnknownElement],[object HTMLElement]") && !first.contains("other:"), "{first}");
        b.collect(&[]).expect("collect");
        assert_eq!(js(&mut b, make), first);
        b.collect(&[]).expect("collect");
        assert_eq!(js(&mut b, "var u = document.createElement('walrus'); [u instanceof HTMLUnknownElement, Object.getPrototypeOf(u) === HTMLUnknownElement.prototype].join()"), "true,true");
    });
}
