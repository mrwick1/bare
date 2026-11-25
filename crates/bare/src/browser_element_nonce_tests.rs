//! Tests of `nonce` on `HTMLElement` and `SVGElement` (HTML Standard, "nonce"). WhatsApp Web compares `script.nonce` of each server
//! payload with the page nonce, and drops every payload when it is `undefined`.

use crate::eng_new::Peek;
use crate::browser::*;
use bare_html::tree;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;

fn eval(body: &str, src: &str) -> String {
    let mut b = Browser::new(tree::parse(&format!("<!doctype html><body>{body}</body>")), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    let v = b.run_script(src).unwrap();
    b.it.display(v)
}

#[test]
fn nonce_reads_the_attribute() {
    assert_eq!(eval("<script id=x type=application/json nonce=abc>{}</script>", "document.getElementById('x').nonce"), "abc");
    assert_eq!(eval("<div id=x></div>", "JSON.stringify(document.getElementById('x').nonce)"), "\"\"");
    assert_eq!(eval("<svg><g id=x nonce=n1></g></svg>", "document.getElementById('x').nonce"), "n1");
}

#[test]
fn nonce_setter_writes_the_attribute() {
    assert_eq!(eval("<style id=x></style>", "var e = document.getElementById('x'); e.nonce = 'z'; e.getAttribute('nonce') + e.nonce"), "zz");
}
