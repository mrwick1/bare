//! The DOM bridge on js2 (`Browser<New>`, S0.4 PR 6c-2). A failing case here is a line of the phase 2 queue.

use crate::browser::*;
use crate::eng::Ops;
use crate::eng_new::New;
use bare_html::tree;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const PAGE: &str = "<!doctype html><html><head><title>t</title></head><body><div id=a class='x y'><p>hi</p><span id=s>one</span></div></body></html>";

fn browser() -> Browser<New> {
    Browser::<New>::make(tree::parse(PAGE), BrowserLimits::default(), Default::default(), Default::default()).expect("browser")
}

fn js(b: &mut Browser<New>, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display_o(v),
        Err(e) => format!("fault:{e:?}"),
    }
}

#[test]
fn a_script_reads_the_document() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "1 + 2"), "3");
        assert_eq!(js(&mut b, "document.getElementById('a').tagName"), "DIV");
        assert_eq!(js(&mut b, "document.querySelectorAll('p, span').length"), "2");
    })
}

#[test]
fn a_script_function_has_a_prototype_in_a_host_realm() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "function C() { this.v = 7 } C.prototype.get = function () { return this.v }; new C().get()"), "7");
    })
}

#[test]
fn the_host_freezes_an_array_and_constructs_a_class() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "Object.isFrozen(navigator.languages)"), "true");
        assert_eq!(js(&mut b, "class A extends HTMLElement {} customElements.define('x-a', A); document.createElement('x-a') instanceof A"), "true");
    })
}

#[test]
fn assignment_adopts_a_constructed_stylesheet() {
    big(|| {
        let mut b = browser();
        assert_eq!(
            js(&mut b, "var sheet = new CSSStyleSheet(); sheet.replaceSync('#a { color: red }'); document.adoptedStyleSheets = [sheet]; [getComputedStyle(document.getElementById('a')).color, Object.hasOwn(document, 'adoptedStyleSheets')].join('|')"),
            "rgb(255, 0, 0)|false",
        );
    });
}

#[test]
fn assignment_updates_the_dom_through_inherited_setters() {
    big(|| {
        let mut b = browser();
        assert_eq!(
            js(&mut b, "var a = document.getElementById('a'); a.className = 'changed'; document['title'] = 'updated'; [a.getAttribute('class'), document.querySelector('title').textContent, Object.hasOwn(a, 'className'), Object.hasOwn(document, 'title')].join()"),
            "changed,updated,false,false",
        );
        assert_eq!(
            js(&mut b, "document.dir = 'rtl'; document.designMode = 'on'; [document.documentElement.getAttribute('dir'), document.designMode, Object.hasOwn(document, 'dir'), Object.hasOwn(document, 'designMode')].join()"),
            "rtl,off,false,false",
        );
    });
}

#[test]
fn assignment_registers_a_lazy_event_handler() {
    big(|| {
        let mut b = browser();
        assert_eq!(
            js(&mut b, "var hits = 0; document.onchange = function () { hits++; }; document.dispatchEvent(new Event('change')); [hits, Object.hasOwn(document, 'onchange')].join()"),
            "1,false",
        );
    });
}

#[test]
fn assignment_adopts_a_constructed_stylesheet_in_a_shadow_root() {
    big(|| {
        let mut b = browser();
        assert_eq!(
            js(&mut b, "var root = document.getElementById('a').attachShadow({ mode: 'open' }); var sheet = new CSSStyleSheet(); root.adoptedStyleSheets = [sheet]; [root.adoptedStyleSheets[0] === sheet, Object.hasOwn(root, 'adoptedStyleSheets')].join()"),
            "true,false",
        );
    });
}
