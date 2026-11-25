//! `DOMParser.parseFromString` takes `application/xhtml+xml`, one of the five types of the HTML Standard. DOMPurify with
//! `PARSER_MEDIA_TYPE: "application/xhtml+xml"` (Teams messages) parses with it. A `TypeError` sent DOMPurify to
//! `createDocument`, which bare does not have, and the Teams message pane showed an error.
use crate::browser::*;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

#[test]
fn an_xhtml_string_parses_into_a_document() {
    let mut b = Browser::new(tree::parse("<!doctype html><p>x</p>"), BrowserLimits::default(), EvalLimits { max_steps: 2_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    let src = "try { var d = new DOMParser().parseFromString('<html xmlns=\"http://www.w3.org/1999/xhtml\"><head></head><body><p>a<b>b</b></p></body></html>', 'application/xhtml+xml'); \
        [d.documentElement.nodeName.toLowerCase(), d.body.textContent].join('|') } catch (e) { e.name + ': ' + e.message }";
    let r = b.run_script(src).expect(src);
    assert_eq!(b.it.display(r), "html|ab");
}
