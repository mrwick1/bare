//! A box with `width: auto`, a definite `height` and an `aspect-ratio` takes its width from the ratio (CSS Sizing 4, 5.1). An
//! `<svg>` with only a `viewBox` gets the ratio from it. The expected widths come from headless Chrome 155.
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

const PAGE: &str = r#"<!doctype html><style>body{margin:0}svg{display:block}.logo{height:20px;width:auto}</style>
<svg id=s class=logo viewBox="0 0 62 90.9"></svg>
<div style="display:flex;align-items:center;height:30px"><svg id=f class=logo viewBox="0 0 62 90.9"></svg><span>x</span></div>
<span><svg id=n style="display:inline;height:20px;width:auto" viewBox="0 0 62 90.9"></svg></span>
<div id=d style="height:30px;aspect-ratio:2"></div>
<div style="display:flex"><div id=e style="height:30px;aspect-ratio:2"></div></div>
<div id=g style="height:30px;aspect-ratio:2;padding:5px;box-sizing:border-box"></div>"#;

#[test]
fn an_auto_width_comes_from_the_height_and_the_ratio() {
    let read = "['s', 'f', 'n', 'd', 'e', 'g'].map(function (id) { return Math.round(document.getElementById(id).getBoundingClientRect().width * 100) / 100 }).join(' ')";
    assert_eq!(js(PAGE, read), "13.64 13.64 13.64 60 60 60");
}

#[test]
fn a_huge_radius_reads_as_a_shorthand() {
    for (sheet, attr) in [("", "style='border-radius:3.40282e38px'"), ("", "style='border-radius:calc(infinity * 1px)'"), (".r{border-radius:3.40282e38px}", "class=r"), (".r{border-radius:calc(infinity * 1px)}", "class=r")] {
        let style = format!("{sheet} {attr}");
        let page = format!("<!doctype html><style>{sheet}</style><button id=e {attr}>x</button>");
        let got = js(&page, "var cs = getComputedStyle(document.getElementById('e')); cs.borderTopLeftRadius + '|' + cs.borderRadius");
        let (long, short) = got.split_once('|').expect(&got);
        assert!(long != "0px" && long == short, "{style}: {got}");
    }
}
