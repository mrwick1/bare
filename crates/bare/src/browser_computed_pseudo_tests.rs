//! `getComputedStyle(el, '::before')` and `'::after'` read the style that the rules of the pseudo-element give (`RuleSet::style_pseudo`).

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(e) => format!("error:{e:?}"),
    }
}

const PAGE: &str = "<!doctype html><html><head><style>#e { color: rgb(255, 0, 0); font-size: 20px; display: block }
    #e::before { content: 'x'; color: blue; width: 40px } #e:after { content: \"q\\\"\" attr(title) }
    #n::before { color: green; content: none } .on::after { content: 'on'; display: inline-block }</style></head>
    <body><div id=e title=T>t</div><div id=n>n</div><p id=p>p</p></body></html>";

#[test]
fn a_pseudo_element_reads_the_values_of_its_rules() {
    let mut b = Browser::new(tree::parse(PAGE), BrowserLimits::default(), EvalLimits { max_steps: 20_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    js(&mut b, "var e = document.getElementById('e'), n = document.getElementById('n'), p = document.getElementById('p');");
    let row = |b: &mut Browser, el: &str, pe: &str| js(b, &format!("var cs = getComputedStyle({el}, '{pe}'); [cs.content, cs.getPropertyValue('content'), cs.display, cs.color, cs.fontSize, cs.width].join('|')"));
    for pe in ["::before", ":before", "::BEFORE"] {
        assert_eq!(row(&mut b, "e", pe), "\"x\"|\"x\"|inline|rgb(0, 0, 255)|20px|40px", "{pe}");
    }
    assert_eq!(row(&mut b, "e", "::after"), "\"q\\\"\" attr(title)|\"q\\\"\" attr(title)|inline|rgb(255, 0, 0)|20px|auto");
    // A rule with `content: none` still gives its values.
    assert_eq!(row(&mut b, "n", "::before"), "none|none|inline|rgb(0, 128, 0)|16px|auto");
    // No rule: the inherited values of the element, the rest initial, and `content: normal`.
    assert_eq!(row(&mut b, "p", "::after"), "normal|normal|inline|rgb(0, 0, 0)|16px|auto");
    // Each read is live.
    js(&mut b, "var cs = getComputedStyle(p, '::after');");
    assert_eq!(js(&mut b, "p.className = 'on'; cs.content + '|' + cs.display"), "\"on\"|inline-block");
    // `::first-line` has no rules here: as before.
    assert_eq!(row(&mut b, "e", "::first-line"), "normal|normal|inline|rgb(255, 0, 0)|20px|auto");
}
