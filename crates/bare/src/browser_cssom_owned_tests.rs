//! Tests of CSSOM edits of a `<style>` sheet (`browser_cssom.rs`, `Sheet::owned`). An edit changes the rules of the sheet, and the cascade
//! uses them. The text of the element does not change, and a mutation observer gets no record, as in Chrome, Firefox and Ladybird.
use crate::browser::*;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser(lim: BrowserLimits) -> Browser {
    let mut b = Browser::new(tree::parse("<!doctype html><style id=s>p { color: red }</style><p id=p>x</p><b id=b>y</b>"), lim, EvalLimits { max_steps: 200_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src).expect(src);
    b.it.display(r)
}

const OBSERVE: &str = "var s = document.getElementById('s'), p = document.getElementById('p'), b = document.getElementById('b'), n = 0; new MutationObserver(r => { n += r.length }).observe(s, { childList: true, characterData: true, subtree: true });";

#[test]
fn insert_rule_keeps_the_element_text_and_applies_the_rule() {
    big(|| {
        let mut b = browser(BrowserLimits::default());
        js(&mut b, OBSERVE);
        assert_eq!(js(&mut b, "s.sheet.insertRule('#b { color: blue }', 1); s.sheet.cssRules.length + '|' + s.textContent + '|' + getComputedStyle(b).color"), "2|p { color: red }|rgb(0, 0, 255)");
        b.advance_time(10.0, 100).unwrap();
        assert_eq!(js(&mut b, "n"), "0", "no mutation record");
    });
}

#[test]
fn delete_rule_and_rule_edits_keep_the_element_text() {
    big(|| {
        let mut b = browser(BrowserLimits::default());
        js(&mut b, OBSERVE);
        assert_eq!(js(&mut b, "s.sheet.insertRule('#b { color: blue }', 1); s.sheet.cssRules[1].style.color = 'green'; getComputedStyle(b).color"), "rgb(0, 128, 0)");
        assert_eq!(js(&mut b, "s.sheet.cssRules[1].selectorText = '#p'; getComputedStyle(p).color + '|' + getComputedStyle(b).color"), "rgb(0, 128, 0)|rgb(0, 0, 0)");
        assert_eq!(js(&mut b, "s.sheet.deleteRule(1); s.sheet.cssRules.length + '|' + getComputedStyle(p).color + '|' + s.textContent"), "1|rgb(255, 0, 0)|p { color: red }");
        b.advance_time(10.0, 100).unwrap();
        assert_eq!(js(&mut b, "n"), "0", "no mutation record");
    });
}

/// A new element text gives a new sheet from that text: the CSSOM edits are gone, as in Chrome.
#[test]
fn a_new_element_text_replaces_the_edited_rules() {
    big(|| {
        let mut b = browser(BrowserLimits::default());
        js(&mut b, OBSERVE);
        js(&mut b, "s.sheet.insertRule('#b { color: blue }', 1);");
        assert_eq!(js(&mut b, "s.textContent = '#p { color: green }'; s.sheet.cssRules.length + '|' + getComputedStyle(p).color + '|' + getComputedStyle(b).color"), "1|rgb(0, 128, 0)|rgb(0, 0, 0)");
        assert_eq!(js(&mut b, "s.sheet.insertRule('#b { color: blue }', 0); s.sheet.cssRules[0].selectorText + '|' + getComputedStyle(b).color + '|' + s.textContent"), "#b|rgb(0, 0, 255)|#p { color: green }");
    });
}

/// A CSS-in-JS library inserts one rule at a time, and reads each new rule (Fela sets `cssRules[i].score`). The DOM text does not grow.
#[test]
fn many_inserts_in_one_task_add_no_dom_text() {
    big(|| {
        let mut b = browser(BrowserLimits::default());
        let before = b.document().borrow().text_bytes();
        js(&mut b, "var sh = document.getElementById('s').sheet; for (var i = 0; i < 2000; i++) { sh.insertRule('.c' + i + ' { height: ' + i + 'px; min-height: 5.8rem }', sh.cssRules.length); sh.cssRules[sh.cssRules.length - 1].score = i; }");
        let grown = b.document().borrow().text_bytes() - before;
        assert!(grown < 2000 * 64, "{grown} bytes of DOM text: at most the declaration of each rule that a script read, once");
        assert_eq!(js(&mut b, "var e = document.createElement('i'); e.className = 'c1999'; document.body.appendChild(e); sh.cssRules.length + '|' + getComputedStyle(e).height"), "2001|1999px");
    });
}
