//! Tests of `browser_element`: `hidden`, `inert`, `tabIndex`, `click()`, `offsetParent`, `offsetTop`, `offsetLeft` and `innerText`. The Chrome cases
//! first (46 of them), then the gates, which Chrome has no need of.

use crate::browser::*;
use crate::browser_topic_tests::topic_test;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

topic_test!(chrome_cases, "element", 46);

fn page(body: &str) -> Browser {
    let mut b = Browser::new(tree::parse(&format!("<!doctype html><body>{body}</body>")), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b
}

fn unsupported(body: &str, src: &str) -> bool {
    matches!(page(body).run_script(src), Err(Error::Unsupported(_)))
}

#[test]
fn click_gates_default_actions_that_are_not_built() {
    for html in ["<input id=x type=checkbox>", "<input id=x type=radio>", "<a id=x href=y></a>", "<label id=x></label>", "<details><summary id=x>s</summary></details>", "<a href=y><b id=x></b></a>"] {
        assert!(unsupported(html, "document.getElementById('x').click()"), "{html}");
    }
    // A link with no href has no default action; a plain element has none.
    assert!(!unsupported("<a id=x></a>", "document.getElementById('x').click()"));
}

#[test]
fn click_gate_comes_before_any_event() {
    let mut b = page("<input id=x type=checkbox>");
    b.run_script("var n = 0; document.addEventListener('click', function () { n++ })").unwrap();
    assert!(matches!(b.run_script("document.getElementById('x').click()"), Err(Error::Unsupported(_))));
    assert!(matches!(b.run_script("n"), Ok(Value::Number(n)) if n == 0.0));
}

#[test]
fn inner_text_gates_select_and_textarea() {
    assert!(unsupported("<div id=x><select><option>a</option></select></div>", "document.getElementById('x').innerText"));
    assert!(unsupported("<div id=x><textarea>t</textarea></div>", "document.getElementById('x').innerText"));
}

#[test]
fn offset_gates_a_box_that_wraps() {
    // An inline box with a border over two lines has two fragments: "the first box" is not known.
    let html = "<div style='width:30px'><span id=x style='border:1px solid'>aaa bbb ccc ddd</span></div>";
    assert!(unsupported(html, "document.getElementById('x').offsetTop"));
}
