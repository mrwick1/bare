//! Tests of `scrollbar-width: none` on a box with `overflow: scroll` (the scroll areas of Base UI, as in T3 Code): the box has no bar
//! and the bar takes no room, as in Chrome.

use crate::eng_new::Peek;
use crate::browser::*;
use bare_html::tree;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;

#[test]
fn scroll_box_with_no_bars_gives_the_whole_box_to_the_content() {
    let html = "<!doctype html><style>.s { overflow: scroll; width: 100px; height: 50px } .n { scrollbar-width: none }</style>\
        <div class=s id=a><div style='height:200px'></div></div><div class='s n' id=n><div style='height:200px'></div></div>";
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    let v = b.run_script("var a = document.getElementById('a'), n = document.getElementById('n'); [a.clientWidth, a.clientHeight, n.clientWidth, n.clientHeight].join()").expect("script");
    assert_eq!(b.it.display(v), "85,35,100,50");
}
