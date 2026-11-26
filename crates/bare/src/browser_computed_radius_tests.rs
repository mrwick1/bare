//! `getComputedStyle(x).borderRadius`: the shorthand from its four corners, as Chrome writes it.

use crate::browser::*;
use crate::eng_new::NewFault as Error;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

fn js(page: &str, src: &str) -> String {
    let mut b = Browser::new(tree::parse(page), BrowserLimits::default(), EvalLimits { max_steps: 20_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if crate::eng_new::is_object(v) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("error:{e:?}"),
    }
}

#[test]
fn border_radius_shorthand_reads_as_chrome_writes_it() {
    let read = "var cs = getComputedStyle(document.getElementById('e')); cs.borderTopLeftRadius + '|' + cs.borderRadius";
    for (style, want) in [
        ("", "0px|0px"),
        ("border-radius:8px", "8px|8px"),
        ("border-radius:8px 8px 0 0", "8px|8px 8px 0px 0px"),
        ("border-radius:4px 8px", "4px|4px 8px"),
        ("border-radius:1px 2px 3px", "1px|1px 2px 3px"),
        ("border-radius:1px 2px 3px 2px", "1px|1px 2px 3px"),
        ("border-radius:50%", "50%|50%"),
    ] {
        let page = format!("<!doctype html><div id=e style='{style}'>x</div>");
        assert_eq!(js(&page, read), want, "{style}");
    }
}

#[test]
fn inline_border_radius_reads_from_its_corners() {
    let page = "<!doctype html><div id=e style='border-top-left-radius:2px;border-top-right-radius:2px;border-bottom-right-radius:2px;border-bottom-left-radius:2px'>x</div>";
    assert_eq!(js(page, "document.getElementById('e').style.borderRadius"), "2px");
}
