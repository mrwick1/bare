//! `::before` and `::after` boxes as scripts see them: they size their element, a point on them hits the element, and the DOM and
//! `innerText` do not hold them.

use crate::eng_new::Peek;
use crate::browser::*;
use bare_html::tree;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(e) => format!("error:{e:?}"),
    }
}

const PAGE: &str = "<!doctype html><html><head><style>body { margin: 0; font: 16px monospace } #d { width: 200px }
    .on::before { content: 'tag'; display: block; height: 50px } #d::after { content: attr(title) }</style></head>
    <body><div id=d title=T>x</div></body></html>";

#[test]
fn generated_boxes_size_their_element_and_hit_it() {
    let mut b = Browser::new(tree::parse(PAGE), BrowserLimits::default(), EvalLimits { max_steps: 20_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    js(&mut b, "var d = document.getElementById('d');");
    let h = js(&mut b, "d.getBoundingClientRect().height");
    // A restyle adds the `::before` box: 50 px more.
    assert_eq!(js(&mut b, "d.className = 'on'; d.getBoundingClientRect().height - 50"), h);
    assert_eq!(js(&mut b, "document.elementFromPoint(10, 20) === d"), "true");
    assert_eq!(js(&mut b, "[d.childNodes.length, d.innerText, d.textContent, document.querySelectorAll('*').length].join('|')"), "1|x|x|5");
    // And a restyle takes it away.
    assert_eq!(js(&mut b, "d.className = ''; d.getBoundingClientRect().height"), h);
}
