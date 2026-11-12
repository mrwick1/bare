//! Named access on `Window` and named properties of `Document` (HTML Standard, "named access on the Window object" and "dom-document-nameditem").
//! Expectations are written by hand from the standard. A full `HTMLCollection` for several matches is not built: the first match is the value.

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

const PAGE: &str = "<!doctype html><body><form id=f name=login><input id=q name=user></form><div id=out></div><div id=out></div>\
<img name=logo id=pic><a name=top2></a><span name=nope></span><embed name=em><svg><rect id=r /></svg></body>";

#[test]
fn ids_and_names_resolve_on_window() {
    big(|| {
        let mut b = browser(PAGE);
        let g = "function g(i) { return document.getElementById(i) }";
        js(&mut b, g);
        assert_eq!(js(&mut b, "JSON.stringify([typeof window.out, typeof f, 'out' in window, typeof document.f])"), "[\"object\",\"object\",true,\"undefined\"]");
        // The first element in tree order wins.
        assert_eq!(js(&mut b, "[out === document.querySelector('#out'), window.out === out, f === g('f'), q === g('q'), r === g('r')].join()"), "true,true,true,true,true");
        // `name` counts on a, embed, form, iframe, img and object only.
        assert_eq!(js(&mut b, "[login === f, logo === pic, top2.tagName, em.tagName, typeof window.nope, typeof window.user].join()"), "true,true,A,EMBED,undefined,undefined");
        assert_eq!(js(&mut b, "nope"), "throw:ReferenceError:nope is not defined");
    });
}

#[test]
fn document_names_cover_forms_images_and_embeds() {
    big(|| {
        let mut b = browser(PAGE);
        assert_eq!(js(&mut b, "[document.login === document.getElementById('f'), document.logo === document.getElementById('pic'), document.pic === document.logo, document.em.tagName, typeof document.out, typeof document.top2, 'login' in document].join()"), "true,true,true,EMBED,undefined,undefined,true");
    });
}

#[test]
fn declared_globals_and_window_properties_win() {
    big(|| {
        let mut b = browser("<!doctype html><body><div id=v></div><div id=fn></div><div id=lx></div><div id=name></div><div id=top></div><div id=screen></div><div id=open></div><div id=hidden></div><div id=options></div><div id=addEventListener></div></body>");
        js(&mut b, "var v = 1; function fn() { return 2 } let lx = 3;");
        assert_eq!(js(&mut b, "[v, fn(), lx, window.v, typeof window.fn].join()"), "1,2,3,1,function");
        assert_eq!(js(&mut b, "[typeof name, top === window, typeof screen.width, typeof open, typeof window.open, typeof alert].join()"), "string,true,number,function,function,function");
        // An `on` prefix that is no handler name is still a name.
        assert_eq!(js(&mut b, "options.tagName"), "DIV");
        // A sloppy assignment makes an own property of window that hides the element.
        assert_eq!(js(&mut b, "options = 5; [options, window.options].join()"), "5,5");
    });
}

#[test]
fn named_access_is_live() {
    big(|| {
        let mut b = browser("<!doctype html><body><div id=a></div></body>");
        assert_eq!(js(&mut b, "typeof late"), "undefined");
        js(&mut b, "var d = document.createElement('p'); d.id = 'late'; document.body.appendChild(d);");
        assert_eq!(js(&mut b, "late === d"), "true");
        js(&mut b, "d.id = 'later';");
        assert_eq!(js(&mut b, "[typeof window.late, later === d].join()"), "undefined,true");
        js(&mut b, "d.remove(); document.getElementById('a').remove();");
        assert_eq!(js(&mut b, "[typeof window.later, 'a' in window, typeof a].join()"), "undefined,false,undefined");
        // A form added later shows on document too.
        js(&mut b, "var fm = document.createElement('form'); fm.setAttribute('name', 'nf'); document.body.appendChild(fm);");
        assert_eq!(js(&mut b, "[document.nf === fm, nf === fm].join()"), "true,true");
    });
}

#[test]
fn a_wrapper_dropped_by_a_collect_comes_back() {
    big(|| {
        let mut b = browser("<!doctype html><body><div id=z></div></body>");
        assert_eq!(js(&mut b, "z.tagName"), "DIV");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "z === document.getElementById('z')"), "true");
    });
}
