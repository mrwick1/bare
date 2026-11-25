//! Tests of the `CSS` namespace (`browser_css_ns.rs`). Real scripts run in a real realm.

use crate::eng_new::Peek;
use crate::browser::*;
use bare_html::tree;
use crate::eng_new::NewFault as Error;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn mk() -> Browser {
    Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 20_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if crate::eng_new::is_object(v) => format!("throw:{}", b.it.get_str(v, "name")),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn css_is_a_namespace_with_supports_and_escape() {
    big(|| {
        let mut b = mk();
        assert_eq!(js(&mut b, "typeof CSS + ',' + typeof CSS.supports + ',' + typeof CSS.escape + ',' + typeof CSS.registerProperty"), "object,function,function,function");
        assert_eq!(js(&mut b, "var d = Object.getOwnPropertyDescriptor(window, 'CSS'); [d.writable, d.enumerable, d.configurable, Object.prototype.toString.call(CSS), CSS === window.CSS, CSS.supports.length, CSS.escape.length].join()"), "true,false,true,[object CSS],true,1,1");
        assert_eq!(js(&mut b, "Object.keys(window).indexOf('CSS') < 0"), "true");
        // a write before the first read replaces the accessor
        let mut b = mk();
        assert_eq!(js(&mut b, "window.CSS = 5; CSS"), "5");
    });
}

#[test]
fn supports_answers_like_an_at_supports_prelude() {
    big(|| {
        let mut b = mk();
        let r = js(
            &mut b,
            "['(display: grid)', 'display: grid', '(display:flex) and (color: red)', 'not (display: nonesuch)', '(display: nonesuch)', '(nonesuch: 1)', 'selector(a > b)', 'selector(a >)', '(--x: anything)', '(color: red) or (nonesuch: 1)', '', 'garbage', '(display: grid', 'color'].map(function (t) { return CSS.supports(t); }).join()",
        );
        assert_eq!(r, "true,true,true,true,false,false,true,false,true,true,false,false,false,false");
        assert_eq!(js(&mut b, "[CSS.supports('display', 'grid'), CSS.supports('DISPLAY', 'grid'), CSS.supports('display', 'nonesuch'), CSS.supports('nonesuch', '1'), CSS.supports('--x', 'y'), CSS.supports('width', 'calc(1px + 2px)')].join()"), "true,true,false,false,true,true");
        // the property must be an identifier and the value cannot close the group: neither widens the condition
        assert_eq!(js(&mut b, "[CSS.supports('color: red) and (display', 'grid'), CSS.supports('display', 'grid) or (display: grid'), CSS.supports('display', 'grid)'), CSS.supports(' ', 'red')].join()"), "false,false,false,false");
        assert_eq!(js(&mut b, "try { CSS.supports() } catch (e) { e.name }"), "TypeError");
        assert_eq!(js(&mut b, "CSS.supports({ toString: function () { return '(display: grid)'; } })"), "true");
    });
}

#[test]
fn supports_text_has_no_length_cap() {
    big(|| {
        let mut b = mk();
        assert_eq!(js(&mut b, "CSS.supports('(display: grid)' + ' '.repeat(1 << 16))"), "true");
    });
}

#[test]
fn escape_serializes_an_identifier() {
    big(|| {
        let mut b = mk();
        let cases = [
            ("", ""),
            ("a", "a"),
            ("-", "\\-"),
            ("--", "--"),
            ("-a", "-a"),
            ("_a-B9", "_a-B9"),
            ("1a", "\\31 a"),
            ("-1a", "-\\31 a"),
            ("a1", "a1"),
            ("--1", "--1"),
            ("a b", "a\\ b"),
            ("a.b#c:d", "a\\.b\\#c\\:d"),
            ("a\u{1}b", "a\\1 b"),
            ("a\u{7f}", "a\\7f "),
            ("\u{1f}", "\\1f "),
            ("é☃😀", "é☃😀"),
            ("a/b[c]", "a\\/b\\[c\\]"),
            ("\0", "\u{FFFD}"),
        ];
        for (input, want) in cases {
            let src = format!("CSS.escape({:?})", input.replace('\0', "\\u0000"));
            let got = js(&mut b, &src.replace("\\\\u0000", "\\u0000"));
            assert_eq!(got, want, "{input:?}");
        }
        assert_eq!(js(&mut b, "CSS.escape(12) + CSS.escape(null) + CSS.escape(undefined)"), "\\31 2nullundefined");
        assert_eq!(js(&mut b, "try { CSS.escape() } catch (e) { e.name }"), "TypeError");
        // the result is usable in a selector
        assert_eq!(js(&mut b, "var e = document.createElement('div'); e.id = '1a.b'; document.body.appendChild(e); document.querySelector('#' + CSS.escape('1a.b')) === e"), "true");
    });
}

#[test]
fn register_property_gives_the_initial_value_and_restyles() {
    big(|| {
        let mut b = Browser::new(tree::parse("<body><div id=a><p id=p>x</p></div></body>"), BrowserLimits::default(), EvalLimits { max_steps: 20_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
        assert_eq!(js(&mut b, "var s = document.createElement('style'); s.textContent = 'p { color: var(--c, blue) } #a { --c: green }'; document.head.appendChild(s); getComputedStyle(document.getElementById('p')).color"), "rgb(0, 128, 0)");
        // Registered with inherits: false, the child no longer takes the value of the parent: the initial value shows.
        assert_eq!(js(&mut b, "CSS.registerProperty({ name: '--c', syntax: '*', inherits: false, initialValue: 'red' }); getComputedStyle(document.getElementById('p')).color"), "rgb(255, 0, 0)");
        assert_eq!(js(&mut b, "CSS.registerProperty({ name: '--d', syntax: '*', inherits: false, initialValue: 'red' }); s.textContent = 'p { color: var(--d) } #a { --d: green }'; getComputedStyle(document.getElementById('p')).color"), "rgb(255, 0, 0)");
        assert_eq!(js(&mut b, "try { CSS.registerProperty({ name: '--d', syntax: '*', inherits: false }) } catch (e) { e.name }"), "InvalidModificationError");
        assert_eq!(js(&mut b, "['x', '--', '--a b', ''].map(function (n) { try { CSS.registerProperty({ name: n, syntax: '*', inherits: false }) } catch (e) { return e.name } }).join()"), "SyntaxError,SyntaxError,SyntaxError,SyntaxError");
        assert_eq!(js(&mut b, "try { CSS.registerProperty({ name: '--e', syntax: '<length>', inherits: false }) } catch (e) { e.name }"), "SyntaxError");
        assert_eq!(js(&mut b, "try { CSS.registerProperty() } catch (e) { e.name }"), "TypeError");
    });
}
