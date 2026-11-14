//! The rule set is shared between documents of one thread (`bare_css::rule_cache`). Each test reads the styles that a document gives with
//! a shared set, and with a set of its own, and checks that a change in one document reaches no other.

use crate::browser::*;
use crate::browser_computed::SheetPart;
use bare_css::rule_cache;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;

const BLUE: &str = "rgb(0, 0, 255)";
const RED: &str = "rgb(255, 0, 0)";
const BLACK: &str = "rgb(0, 0, 0)";

const PAGE: &str = "<!doctype html><html><head><style>#a { color: red; --m: 3px } .m { color: green } @media (min-width: 500px) { #a { color: blue } }</style></head><body><div id=a class=m></div></body></html>";

fn at(page: &str) -> Browser {
    let mut b = Browser::new(tree::parse(page), BrowserLimits::default(), EvalLimits { max_steps: 20_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.set_viewport(1000.0, 800.0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, bare_js::value::Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("error:{e:?}"),
    }
}

fn color(b: &mut Browser) -> String {
    js(b, "getComputedStyle(document.getElementById('a')).color")
}

#[test]
fn two_documents_with_the_same_sheets_get_the_same_rule_set() {
    rule_cache::clear();
    let (mut a, mut b) = (at(PAGE), at(PAGE));
    assert_eq!(color(&mut a), BLUE);
    assert_eq!(rule_cache::counts().misses, 1);
    assert_eq!(color(&mut b), BLUE);
    let c = rule_cache::counts();
    assert_eq!((c.misses, c.hits), (1, 1), "the second document built nothing");
    assert!(a.dom.rules_id().is_some() && a.dom.rules_id() == b.dom.rules_id());
    // A third document in the same state, after the first two are gone, still finds it.
    drop((a, b));
    let mut c = at(PAGE);
    assert_eq!(color(&mut c), BLUE);
    assert_eq!(rule_cache::counts().hits, 2);
}

#[test]
fn a_changed_viewport_gives_its_own_rule_set() {
    rule_cache::clear();
    let (mut wide, mut narrow) = (at(PAGE), at(PAGE));
    narrow.set_viewport(300.0, 800.0).unwrap();
    assert_eq!(color(&mut wide), BLUE);
    assert_eq!(color(&mut narrow), RED);
    assert_ne!(wide.dom.rules_id(), narrow.dom.rules_id(), "the `@media` rule is in one set and not in the other");
    // Another document at either size shares with the first one of that size.
    let mut again = at(PAGE);
    assert_eq!(color(&mut again), BLUE);
    assert_eq!(again.dom.rules_id(), wide.dom.rules_id());
    // A document that is resized reads the set of that size, built by another document.
    wide.set_viewport(300.0, 800.0).unwrap();
    assert_eq!(color(&mut wide), RED);
    assert_eq!(wide.dom.rules_id(), narrow.dom.rules_id());
    assert_eq!(color(&mut again), BLUE);
}

#[test]
fn a_cssom_edit_reaches_only_its_document() {
    rule_cache::clear();
    let (mut a, mut b) = (at(PAGE), at(PAGE));
    assert_eq!((color(&mut a), color(&mut b)), (BLUE.into(), BLUE.into()));
    let shared = a.dom.rules_id();
    assert_eq!(js(&mut b, "document.styleSheets[0].insertRule('.m#a { color: black }', 0)"), "0");
    assert_eq!(color(&mut b), BLACK);
    assert_eq!(color(&mut a), BLUE);
    assert_eq!(a.dom.rules_id(), shared);
    assert_ne!(b.dom.rules_id(), shared);
    // A new document with the sheets of the page has the old rules, not the edited ones.
    let mut c = at(PAGE);
    assert_eq!(color(&mut c), BLUE);
    assert_eq!(c.dom.rules_id(), shared);
    // The edit is undone for its document by a delete. The sheet text is written again by CSSOM, so it may be another list than the first.
    assert_eq!(js(&mut b, "document.styleSheets[0].deleteRule(0); 0"), "0");
    assert_eq!(color(&mut b), BLUE);
    assert_eq!((color(&mut a), a.dom.rules_id()), (BLUE.into(), shared));
}

const LATER: &str = ".m#a { color: black }";

#[test]
fn an_inserted_style_does_not_change_the_set_that_other_documents_hold() {
    rule_cache::clear();
    let (mut a, mut b) = (at(PAGE), at(PAGE));
    assert_eq!((color(&mut a), color(&mut b)), (BLUE.into(), BLUE.into()));
    let shared = a.dom.rules_id();
    js(&mut b, &format!("var s = document.createElement('style'); s.textContent = '{LATER}'; document.head.appendChild(s); 0"));
    assert_eq!(color(&mut b), BLACK);
    assert_eq!(color(&mut a), BLUE);
    assert_eq!(a.dom.rules_id(), shared, "the set of the first document did not grow");
    assert_ne!(b.dom.rules_id(), shared);
    assert_eq!(color(&mut b), BLACK);
}

#[test]
fn a_set_that_only_one_document_holds_grows_and_is_found_under_its_new_sheets() {
    rule_cache::clear();
    let mut a = at(PAGE);
    assert_eq!(color(&mut a), BLUE);
    js(&mut a, &format!("var s = document.createElement('style'); s.textContent = '{LATER}'; document.head.appendChild(s); 0"));
    assert_eq!(color(&mut a), BLACK);
    let grown = a.dom.rules_id();
    // The old list is not in the cache any more: a new document with it builds a set, and it is the old rules.
    let before = rule_cache::counts();
    let mut old = at(PAGE);
    assert_eq!(color(&mut old), BLUE);
    assert_eq!(rule_cache::counts().misses, before.misses + 1);
    assert_ne!(old.dom.rules_id(), grown);
    // A document with the new list finds the grown set.
    let both = PAGE.replace("</style>", &format!("</style><style>{LATER}</style>"));
    let mut new = at(&both);
    assert_eq!(color(&mut new), BLACK);
    assert_eq!(new.dom.rules_id(), grown);
}

const LINKED: &str = "<!doctype html><html><head><link rel=stylesheet href='a.css'><style>.m { color: green }</style></head><body><div id=a class=m></div></body></html>";

fn part(text: &str) -> SheetPart {
    SheetPart { text: text.into(), layer: None }
}

#[test]
fn a_late_link_gives_a_rule_set_of_its_own() {
    rule_cache::clear();
    let (mut a, mut b) = (at(LINKED), at(LINKED));
    for d in [&mut a, &mut b] {
        d.set_external_stylesheet("a.css", vec![part("#a { color: red }")]).unwrap();
        d.set_pending_sheets(true);
    }
    assert_eq!((color(&mut a), color(&mut b)), (RED.into(), RED.into()));
    let shared = a.dom.rules_id();
    assert_eq!(b.dom.rules_id(), shared);
    // A link that a script adds is still loading: no rules yet, so the same list and the same set.
    js(&mut b, "var l = document.createElement('link'); l.rel = 'stylesheet'; l.setAttribute('href', 'late.css'); document.head.appendChild(l); 0");
    assert_eq!(color(&mut b), RED);
    assert_eq!(b.dom.rules_id(), shared);
    // It loads: the list changes. The first document keeps its set.
    b.set_external_stylesheet("late.css", vec![part(".m#a { color: black }")]).unwrap();
    assert_eq!(color(&mut b), BLACK);
    assert_eq!(color(&mut a), RED);
    assert_eq!(a.dom.rules_id(), shared);
    assert_ne!(b.dom.rules_id(), shared);
    // A sheet with another text under the same address is another list.
    let mut c = at(LINKED);
    c.set_external_stylesheet("a.css", vec![part("#a { color: blue }")]).unwrap();
    assert_eq!(color(&mut c), BLUE);
    assert_ne!(c.dom.rules_id(), shared);
}

#[test]
fn a_layered_part_is_another_list_than_the_same_text_with_no_layer() {
    rule_cache::clear();
    let (mut plain, mut layered) = (at(LINKED), at(LINKED));
    plain.set_external_stylesheet("a.css", vec![part("#a { color: red }")]).unwrap();
    layered.set_external_stylesheet("a.css", vec![SheetPart { text: "#a { color: red }".into(), layer: Some("x".into()) }]).unwrap();
    // The unlayered inline `.m` beats a layered `#a`; it loses to an unlayered one.
    assert_eq!(color(&mut plain), RED);
    assert_eq!(color(&mut layered), "rgb(0, 128, 0)");
    assert_ne!(plain.dom.rules_id(), layered.dom.rules_id());
}

#[test]
fn a_shadow_tree_sheet_is_not_shared() {
    rule_cache::clear();
    let page = "<!doctype html><html><head><style>#a { color: red }</style></head><body><div id=a></div></body></html>";
    let (mut a, mut b) = (at(page), at(page));
    for d in [&mut a, &mut b] {
        js(d, "document.getElementById('a').attachShadow({mode:'open'}).innerHTML = '<style>:host { color: blue } p { color: black }</style><p>x</p>'; 0");
    }
    // The host rule of a shadow tree has less weight than the page rule. A style of the page for the same host wins.
    assert_eq!((color(&mut a), color(&mut b)), (RED.into(), RED.into()));
    assert_ne!(a.dom.rules_id(), b.dom.rules_id(), "each document has its own set for a list with a shadow tree sheet");
    assert_eq!(js(&mut a, "getComputedStyle(document.getElementById('a').shadowRoot.querySelector('p')).color"), BLACK);
    // A document without the shadow tree still shares the page sheet with others.
    let (mut c, mut d) = (at(page), at(page));
    assert_eq!((color(&mut c), color(&mut d)), (RED.into(), RED.into()));
    assert_eq!(c.dom.rules_id(), d.dom.rules_id());
}

#[test]
fn retained_inline_inputs_give_a_document_rules_of_its_own() {
    rule_cache::clear();
    let (mut a, mut b) = (at(PAGE), at(PAGE));
    assert_eq!((color(&mut a), color(&mut b)), (BLUE.into(), BLUE.into()));
    let shared = a.dom.rules_id();
    // A shorthand with `var()` stays as written until the cascade (`has_pending`): its value comes from the page rule.
    js(&mut b, "var st = document.getElementById('a').style; st.margin = 'var(--m) 2px'; st.marginTop = '1px'; 0");
    assert!(b.dom.style.has_pending.get() && !a.dom.style.has_pending.get(), "a longhand next to a shorthand with `var()` is kept as input");
    assert_eq!(js(&mut b, "getComputedStyle(document.getElementById('a')).marginRight"), "2px");
    assert_ne!(b.dom.rules_id(), shared, "the inputs are written into the rules of this document only");
    assert_eq!(js(&mut a, "getComputedStyle(document.getElementById('a')).marginTop"), "0px");
    assert_eq!(a.dom.rules_id(), shared);
    // The set that the first two documents had is still the one that a new document gets.
    let mut c = at(PAGE);
    assert_eq!(color(&mut c), BLUE);
    assert_eq!(c.dom.rules_id(), shared);
    assert_eq!(js(&mut c, "getComputedStyle(document.getElementById('a')).marginTop"), "0px");
}

/// A document reads the same styles with the cache and without it (a cold cache for each read).
#[test]
fn the_styles_are_the_same_with_and_without_the_cache() {
    const READ: &str = "var o = []; var all = document.querySelectorAll('*'); for (var i = 0; i < all.length; i++) { var c = getComputedStyle(all[i]); o.push(all[i].tagName + ':' + c.color + ',' + c.display + ',' + c.marginTop + ',' + c.fontSize + ',' + c.width); } o.join(';')";
    let pages = [
        PAGE,
        LINKED,
        "<!doctype html><html><head><style>ul > li:first-child { color: red } li + li { color: blue } .a .b { margin-top: 4px } [title=x] { display: flex } p::first-line { color: red }</style></head><body><ul><li class=a title=x>1</li><li class=b>2</li></ul><div class=a><p class=b>t</p></div></body></html>",
        "<!doctype html><html><head><style>@media (max-width: 600px) { p { display: none } } @supports (display: grid) { div { display: grid } } @layer base { div { color: red } } div { color: blue }</style></head><body><div><p>x</p></div></body></html>",
    ];
    for (i, page) in pages.iter().enumerate() {
        let make = || {
            let mut b = at(page);
            if page.contains("a.css") {
                b.set_external_stylesheet("a.css", vec![part("#a { color: red }")]).unwrap();
            }
            b
        };
        rule_cache::clear();
        let cold = js(&mut make(), READ);
        // Another document builds the set first, and this one reads it from the cache.
        let mut first = make();
        js(&mut first, READ);
        let hits = rule_cache::counts().hits;
        let warm = js(&mut make(), READ);
        assert_eq!(rule_cache::counts().hits, hits + 1, "page {i} reads the cached set");
        assert_eq!(warm, cold, "page {i}");
        assert!(!cold.starts_with("error"), "page {i}: {cold}");
    }
}

#[test]
fn an_image_keeps_its_size_hint_when_the_rules_come_from_the_cache() {
    rule_cache::clear();
    let page = "<!doctype html><div>aa<span id=s>bb<img id=im width=30 height=40 style=\"display:inline-block\">cc</span>dd</div>";
    let size = "var s=getComputedStyle(document.getElementById('im')); s.width+' '+s.height";
    let first = js(&mut at(page), size);
    assert_eq!(first, "30px 40px");
    assert_eq!(js(&mut at(page), size), first, "the second document read its rules from the cache");
    assert_eq!(rule_cache::counts().hits, 1);
}
