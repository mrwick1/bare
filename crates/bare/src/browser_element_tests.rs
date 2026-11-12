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
fn offset_of_a_box_that_wraps_is_its_first_box() {
    // An inline box with a border over two lines has two fragments, one for each line. CSSOM View: the offsets come from the border
    // edge of the first box. A 10px font: a letter is 6px wide.
    let css = "<style>body { margin: 0; font-size: 10px }</style>";
    let body = "<div style='width: 50px'>xxx <span id=x style='border: 1px solid'>aaaa bbbbb</span></div>\
                <div style='width: 50px'>xx<span id=p style='position: relative; border: 2px solid'>aa <b id=c>bbbbb</b></span></div>";
    let mut b = page(&format!("{css}{body}"));
    let mut js = |src: &str| match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(e) => format!("{e:?}"),
    };
    let src = "var x = document.getElementById('x'), r = x.getClientRects();\
               [r.length, r[0].top < r[1].top, x.offsetLeft, x.offsetTop, Math.round(r[0].left), Math.round(r[0].top), x.offsetParent.tagName].join()";
    // two rects; the offsets are the first rect (after "xxx ", 24px; the border sits 1px above the line), not the union of both lines
    assert_eq!(js(src), "2,true,24,-1,24,-1,BODY");
    // an offset parent over two lines: from the padding edge of its first box (after "xx" and its 2px border)
    let src = "var c = document.getElementById('c'), p = document.getElementById('p'), r = p.getClientRects(), k = c.getBoundingClientRect();\
               [r.length, c.offsetParent.id, c.offsetLeft == Math.round(k.left - r[0].left - 2), c.offsetTop == Math.round(k.top - r[0].top - 2)].join()";
    assert_eq!(js(src), "2,p,true,true");
}

#[test]
fn offset_of_an_inline_box_with_nothing_to_paint_is_its_first_line() {
    // A 10px font: a letter is 6px wide and a line is 14px high. Layout makes no fragment for these spans (no background, no border).
    let css = "<style>body { margin: 0; font-size: 10px; height: 2000px }</style>";
    let body = "<div style='padding: 5px'>xx<span id=a>aaaa</span></div>\
                <div style='width: 50px'>xxx <span id=w>aaaa bbbbb</span></div>\
                <div>xx<span id=e></span></div>\
                <div style='position: relative; border: 2px solid; padding: 3px'>y<span id=p>t</span><br id=br></div>\
                <div>xx<span style='position: relative'>ab<span id=q>c</span></span></div>";
    let mut b = page(&format!("{css}{body}"));
    let off = |b: &mut Browser, id: &str| {
        let src = format!("var x = document.getElementById('{id}'); [x.offsetLeft, x.offsetTop, x.offsetParent && x.offsetParent.tagName].join()");
        match b.run_script(&src) {
            Ok(v) => b.it.display(v),
            Err(e) => format!("{e:?}"),
        }
    };
    // after "xx" (12px) in a padding of 5px
    assert_eq!(off(&mut b, "a"), "17,5,BODY");
    // the first line: after "xxx " (24px), in the second div (24px down); not the union of both lines
    assert_eq!(off(&mut b, "w"), "24,24,BODY");
    // an empty inline: its place on the line
    assert_eq!(off(&mut b, "e"), "12,52,BODY");
    // relative to the padding edge of a positioned parent, after "y" (6px)
    assert_eq!(off(&mut b, "p"), "9,3,DIV");
    // a `br` is not a box (Chrome: `LayoutBR` is a text): 0, but it has an offset parent
    assert_eq!(off(&mut b, "br"), "0,0,DIV");
    // an offset parent with nothing to paint: from its first line, after "ab" (12px)
    assert_eq!(off(&mut b, "q"), "12,0,SPAN");
    // the offsets do not move with the page
    b.run_script("scrollTo(0, 100)").unwrap();
    assert_eq!(off(&mut b, "w"), "24,24,BODY");
    assert_eq!(off(&mut b, "q"), "12,0,SPAN");
}
