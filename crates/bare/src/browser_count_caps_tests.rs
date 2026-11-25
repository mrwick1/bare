//! A page may make as many nodes, attributes, names, wrappers, listeners and results as it needs: each test passes the old default cap
//! (`docs/internal/caps-audit.md`). The JS heap and the process bound them, as in Chrome.

use crate::browser::*;
use crate::eng_new::Peek;
use bare_html::tree;

fn browser() -> Browser {
    Browser::new(tree::parse("<!doctype html><body></body>"), BrowserLimits::default(), Default::default(), Default::default()).expect("browser")
}

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src).expect(src);
    b.it.display(r)
}

/// Old caps: 2^20 nodes and 2^21 attribute slots.
#[test]
fn a_page_passes_the_old_node_and_attribute_caps() {
    let mut b = browser();
    let n = (1 << 20) + 1;
    assert_eq!(js(&mut b, &format!("document.body.innerHTML = '<i a=1 b=2></i>'.repeat({n}); document.body.childElementCount")), n.to_string());
}

/// Old caps: 2^16 elements in one result list.
#[test]
fn a_query_passes_the_old_result_cap() {
    let mut b = browser();
    let n = (1 << 16) + 1;
    assert_eq!(js(&mut b, &format!("document.body.innerHTML = '<i></i>'.repeat({n}); [document.querySelectorAll('i').length, document.getElementsByTagName('i').length, document.body.childNodes.length].join()")), format!("{n},{n},{n}"));
}

/// Old caps: 2^16 live wrappers and 2^16 listeners.
#[test]
fn a_page_passes_the_old_wrapper_and_listener_caps() {
    let mut b = browser();
    let n = (1 << 16) + 1;
    assert_eq!(js(&mut b, &format!("var keep = [], hits = 0; function f() {{ hits++ }} for (var i = 0; i < {n}; i++) {{ var t = new EventTarget(); t.addEventListener('x', f); keep.push(t) }} keep[{n} - 1].dispatchEvent(new Event('x')); keep.length + ',' + hits")), format!("{n},1"));
}

/// Old cap: 2^16 names (tags and attributes) in the name table.
#[test]
fn a_page_passes_the_old_name_cap() {
    let mut b = browser();
    let n = (1 << 16) + 1;
    assert_eq!(js(&mut b, &format!("for (var i = 0; i < {n}; i++) document.createElement('i').setAttribute('n' + i, ''); 'ok'")), "ok");
}

/// Old cap: 16 MiB for one string between JS and the document.
#[test]
fn a_string_passes_the_old_string_cap() {
    let mut b = browser();
    let n = (16 << 20) + 1;
    assert_eq!(js(&mut b, &format!("document.createTextNode('x'.repeat({n})).data.length")), n.to_string());
}
