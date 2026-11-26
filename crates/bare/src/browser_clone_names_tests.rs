//! A `DataCloneError` names the value that does not clone, with Chrome's text. V8 makes the text of a script object
//! (`Object::NoSideEffectsToString`): `#<Ctor>`, or `[object Tag]`. Blink makes the text of a platform object: `<Interface> object`.

use crate::browser::*;
use crate::eng_new::NewFault as Error;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser() -> Browser {
    let page = tree::parse("<!doctype html><html><head></head><body><div id=d></div></body></html>");
    let mut b = Browser::new(page, BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.set_document_url("https://a.test/").expect("url");
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) => format!("throw:{}", b.it.display(v)),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn clone_errors_name_the_value() {
    big(|| {
        let mut b = browser();
        // The expected text is the output of Node 22 (V8) for the script objects.
        for (src, want) in [
            ("new Promise(function () {})", "#<Promise>"),
            ("new WeakMap", "#<WeakMap>"),
            ("({ a: [new WeakSet] })", "#<WeakSet>"),
            ("new WeakRef({})", "#<WeakRef>"),
            ("new Proxy({}, {})", "#<Object>"),
            ("new Proxy([], {})", "[object Array]"),
            ("(function* () {})()", "[object Generator]"),
            ("Object(Symbol())", "[object Symbol]"),
            ("(function () { var p = new Promise(function () {}); Object.defineProperty(p, 'constructor', { get: function () { throw 1 } }); return p })()", "[object Promise]"),
            ("document.createTextNode('x')", "Text object"),
            ("matchMedia('all')", "MediaQueryList object"),
        ] {
            let got = js(&mut b, &format!("try {{ structuredClone({src}); 'cloned' }} catch (e) {{ e.name + ':' + e.message }}"));
            assert_eq!(got, format!("DataCloneError:{want} could not be cloned."), "{src}");
        }
        // A proxy trap does not run while the text is made.
        assert_eq!(js(&mut b, "var hit = 0; var t = function () { hit++ }; try { structuredClone(new Proxy(new WeakMap, { get: t, getOwnPropertyDescriptor: t, getPrototypeOf: t })) } catch (e) {} hit"), "0");
        let got = js(&mut b, "try { new MessageChannel().port1.postMessage({ p: new Promise(function () {}) }); 'sent' } catch (e) { e.name + ':' + e.message }");
        assert_eq!(got, "DataCloneError:#<Promise> could not be cloned.");
    });
}
