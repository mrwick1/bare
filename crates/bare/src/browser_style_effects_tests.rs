//! Tests of `backdrop-filter` and `scrollbar-width` (OFS-51): pages test them with `CSS.supports`, read them with
//! `getComputedStyle`, and a row with `scrollbar-width: none` must not give room to a scrollbar.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;

fn js(b: &mut Browser, src: &str) -> String {
    let v = b.run_script(src).expect("script");
    b.it.display(v)
}

fn browser(html: &str) -> Browser {
    Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

#[test]
fn backdrop_filter_is_supported_and_computed() {
    let mut b = browser("<!doctype html><style>#h { backdrop-filter: blur(12px) } #s { -webkit-backdrop-filter: saturate(180%) blur(20px) }</style><div id=h></div><div id=s></div><p id=p></p>");
    let supports = "['blur(2px)', 'saturate(1.8) blur(4px)', 'none', 'blur(red)', 'shiny(2px)'].map(v => CSS.supports('backdrop-filter', v)).join()";
    assert_eq!(js(&mut b, supports), "true,true,true,false,false");
    assert_eq!(js(&mut b, "CSS.supports('-webkit-backdrop-filter', 'blur(2px)')"), "true");
    // A picture property has no camel-case accessor (`Property::LISTED`), so read it by name.
    let computed = "['h', 's', 'p'].map(i => getComputedStyle(document.getElementById(i)).getPropertyValue('backdrop-filter')).join()";
    assert_eq!(js(&mut b, computed), "blur(12px),blur(20px),none");
}

#[test]
fn scrollbar_width_none_takes_no_room() {
    let html = "<!doctype html><style>.row { overflow-x: auto; width: 300px; height: 60px; white-space: nowrap } #n { scrollbar-width: none }</style>\
        <div class=row id=a><span style='display:inline-block;width:900px;height:10px'></span></div>\
        <div class=row id=n><span style='display:inline-block;width:900px;height:10px'></span></div>";
    let mut b = browser(html);
    assert_eq!(js(&mut b, "['none', 'thin', 'auto'].map(v => CSS.supports('scrollbar-width', v)).join()"), "true,true,true");
    // The classic bar takes 15 px; with `none` the box still scrolls, and its whole height is client area.
    assert_eq!(js(&mut b, "var a = document.getElementById('a'), n = document.getElementById('n'); [a.clientHeight, n.clientHeight, n.scrollWidth > n.clientWidth].join()"), "45,60,true");
    assert_eq!(js(&mut b, "getComputedStyle(n).getPropertyValue('scrollbar-width')"), "none");
    // A script that hides the bar later lays the box out again.
    assert_eq!(js(&mut b, "a.style.setProperty('scrollbar-width', 'none'); a.clientHeight"), "60");
}
