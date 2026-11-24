//! Tests of the `dom-text` cap on a text list that holds dead text (`Dom::room`, `browser_reclaim.rs`).

use crate::browser::*;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

const PAGE: &str = "<!doctype html><body><p id=a class=x>hi</p></body>";

fn browser(cap: usize) -> Browser {
    let doc = tree::parse(PAGE);
    let max_text_bytes = doc.text_bytes() + cap;
    let mut b = Browser::new(doc, BrowserLimits { max_text_bytes, ..Default::default() }, EvalLimits::default(), HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src).expect(src);
    b.it.display(r)
}

/// Each task replaces the text of a style element and an attribute. The old text is dead: the list is rebuilt between two tasks, and the cap is not hit.
#[test]
fn rewrites_in_many_tasks_stay_under_the_cap() {
    let mut b = browser(1 << 20);
    js(&mut b, "var st = document.head.appendChild(document.createElement('style')), a = document.getElementById('a'), n = 0; function step(i) { st.textContent = '.c' + i + ' { height: 1px; }' + ' '.repeat(100_000); a.className = 'x'.repeat(100_000) + i; n++; } for (var i = 0; i < 100; i++) setTimeout(step.bind(null, i), i);");
    b.advance_time(200.0, 1000).expect("no limit: the dead text goes at each rebuild");
    assert_eq!(js(&mut b, "n + ',' + st.textContent.length"), "100,100021");
    let cap = b.dom.lim.max_text_bytes;
    assert!(b.stats().text_bytes <= 2 * cap, "{} bytes, cap {cap}", b.stats().text_bytes);
}

/// One task that rewrites text with no end hits the cap: the dead text of a task stays until the task ends.
#[test]
fn rewrites_in_one_task_hit_the_cap() {
    let mut b = browser(20_000);
    let r = b.run_script("var a = document.getElementById('a'); try { for (var i = 0; i < 1000; i++) a.className = 'x'.repeat(3000) + i; 'ok' } catch (e) { 'caught' }");
    assert!(matches!(&r, Err(e) if format!("{e:?}").contains("dom-text")), "{:?}", r.map(|v| b.it.display(v)));
    let cap = b.dom.lim.max_text_bytes;
    assert!(b.stats().text_bytes <= 2 * cap, "{} bytes, cap {cap}", b.stats().text_bytes);
}
