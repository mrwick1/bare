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

#[test]
fn a_missing_name_builds_no_list() {
    big(|| {
        let mut b = browser(PAGE);
        // Each round changes a named element, then asks for a name that nobody defines (a feature probe).
        let r = js(&mut b, "var out2 = []; for (var i = 0; i < 5; i++) { var e = document.createElement('p'); e.id = 'n' + i; document.body.appendChild(e); out2.push(typeof window['missing' + i], 'gone' + i in window, typeof document['nothing' + i]); } out2.join()");
        assert_eq!(r, "undefined,false,undefined,undefined,false,undefined,undefined,false,undefined,undefined,false,undefined,undefined,false,undefined");
        // A name that no element ever had is a miss with no walk at all.
        assert_eq!(b.dom.frames.counts(), (0, 0));
        // A name that exists is found by a walk, without a list, and a name that was added and removed is gone.
        assert_eq!(js(&mut b, "[n3 === document.getElementById('n3'), (document.getElementById('n3').remove(), typeof window.n3)].join()"), "true,undefined");
        let (builds, visited) = b.dom.frames.counts();
        assert_eq!(builds, 0);
        assert!(visited > 0);
    });
}

#[test]
fn a_walk_picks_the_same_node_as_the_list() {
    big(|| {
        let page = "<!doctype html><body><div id=k></div><img name=k><iframe name=k id=fr></iframe><a id=k name=k></a>\
<img name=m><a name=m id=m2></a><span id=s></span><svg><rect id=r /></svg><form name=fm id=fid></form><img id=im name=imn><div id=dd></div></body>";
        let mut b = browser(page);
        // Bump the name revision before each ask, so that each ask walks.
        let ask = |b: &mut Browser, q: &str| {
            js(b, "var t = document.createElement('i'); t.id = 'bump' + (window.bumps = (window.bumps | 0) + 1); document.body.appendChild(t);");
            js(b, q)
        };
        // An iframe wins over an element that came first; `name` counts on `a` and `img`, not on `span`.
        assert_eq!(ask(&mut b, "k === document.getElementById('fr').contentWindow"), "true");
        assert_eq!(ask(&mut b, "m === document.querySelector('img[name=m]')"), "true");
        assert_eq!(ask(&mut b, "[m2.tagName, r.tagName, typeof window.imn, im.tagName, fm.tagName, fid.tagName].join()"), "A,rect,object,IMG,FORM,FORM");
        // The document: `name` on form, img, iframe and embed; `id` on iframe, object and img with a name.
        assert_eq!(ask(&mut b, "[document.fm === fid, document.imn === im, document.fr === document.getElementById('fr'), typeof document.m2, typeof document.dd, typeof document.s].join()"), "true,true,true,undefined,undefined,undefined");
        assert_eq!(ask(&mut b, "[typeof window.nope, 'k' in window, 'nope' in window, typeof document.k].join()"), "undefined,true,false,object");
        assert_eq!(b.dom.frames.counts().0, 0);
    });
}

#[test]
fn a_name_asked_often_at_one_revision_builds_the_list_once() {
    big(|| {
        let mut b = browser(PAGE);
        // `id=out` exists: the ever-seen set says maybe, so each ask walks, until the list is built.
        assert_eq!(js(&mut b, "var c = 0; for (var i = 0; i < 100; i++) { document.body.lastChild.id = 'x'; if (typeof window['out'] !== 'object') c++; } [c, typeof f].join()"), "0,object");
        assert_eq!(b.dom.frames.counts().0, 0);
        // The same name again costs nothing (the last answer stays); other names walk until the list is built, once.
        assert_eq!(js(&mut b, "var c = 0, ns = ['out', 'f', 'q', 'pic']; for (var i = 0; i < 100; i++) { if (typeof window[ns[i % 4]] !== 'object') c++; } [c, typeof f].join()"), "0,object");
        assert_eq!(b.dom.frames.counts().0, 1);
    });
}

#[test]
fn enumeration_and_an_index_still_see_the_names() {
    big(|| {
        let mut b = browser(PAGE);
        js(&mut b, "var e = document.createElement('p'); e.id = 'late'; document.body.appendChild(e); typeof window.zzz");
        // The names sit on the object behind `Window.prototype` (the `WindowProperties` object), so `for in` and its keys show them.
        let r = js(&mut b, "var o = Object.getPrototypeOf(Object.getPrototypeOf(window)), k = Object.getOwnPropertyNames(o); var dk = []; for (var x in window) { if (x == 'late' || x == 'f') dk.push(x) } [k.indexOf('late') >= 0, k.indexOf('login') >= 0, k.indexOf('nope') >= 0, dk.sort().join('+')].join()");
        assert_eq!(r, "true,true,false,f+late");
        let mut b = browser("<body><iframe name=a></iframe><iframe id=b></iframe></body>");
        js(&mut b, "var e = document.createElement('p'); e.id = 'x'; document.body.appendChild(e);");
        assert_eq!(js(&mut b, "[frames.length, frames[1] === frames.b, frames[0] === window.a, typeof frames[2]].join()"), "2,true,true,undefined");
        assert_eq!(js(&mut b, "document.body.firstChild.remove(); [frames.length, frames[0] === frames.b].join()"), "1,true");
    });
}
