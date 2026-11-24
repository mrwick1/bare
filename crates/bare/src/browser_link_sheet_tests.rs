//! Tests of `HTMLLinkElement.sheet` for a supplied external style sheet (CSSOM "associated CSS style sheet").

use crate::browser::{Browser, BrowserLimits};
use crate::browser_computed::SheetPart;
use crate::eng_new::Peek;

fn run(b: &mut Browser, source: &str) -> String {
    match b.run_script(source) {
        Ok(v) => b.it.display(v),
        Err(crate::eng_new::NewFault::Throw(v)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn a_parsed_link_with_a_loaded_sheet_has_it() {
    let html = "<html><head><link id=l rel=stylesheet href='/a.css?v=1'></head><body><p>x</p></body></html>";
    let mut b = Browser::new(bare_html::tree::parse(html), BrowserLimits::default(), Default::default(), Default::default()).unwrap();
    b.set_external_stylesheet("/a.css?v=1", vec![SheetPart { text: "p { color: red }".into(), layer: None }]).unwrap();
    b.set_pending_sheets(true);
    // Teams waits for `load` when `sheet` is null; the `load` already ran, so the wait never ends.
    // The first read of `sheet` defines the member; it must give the sheet too.
    assert_eq!(run(&mut b, "String(document.getElementById('l').sheet)"), "[object CSSStyleSheet]");
    assert_eq!(run(&mut b, "var l = document.getElementById('l'); [getComputedStyle(document.querySelector('p')).color, document.styleSheets.length, l.sheet === document.styleSheets[0], l.sheet.href].join()"), "rgb(255, 0, 0),1,true,/a.css?v=1");
}
