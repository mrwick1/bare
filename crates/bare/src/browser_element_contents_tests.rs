//! An element with `display: contents` seen from script: its computed `display`, no rectangles, `checkVisibility`, and hit
//! tests that find its children.
use crate::browser::Browser;
use crate::eng_new::Peek;

fn browser() -> Browser {
    let html = "<html><body style='margin:0'><div id=g style='display:grid;grid-template-columns:100px 200px'><div id=c style='display:contents'><div id=a style='height:50px'>A</div><div id=b style='height:50px'>B</div></div></div></body></html>";
    Browser::new(bare_html::tree::parse(html), Default::default(), Default::default(), Default::default()).unwrap()
}

fn run(b: &mut Browser, source: &str, expected: &str) {
    let v = b.run_script(source).expect(source);
    assert_eq!(b.it.display(v), expected, "{source}");
}

#[test]
fn the_computed_display_is_contents_but_block_on_the_root() {
    let mut b = browser();
    run(&mut b, "getComputedStyle(c).display", "contents");
    run(&mut b, "document.documentElement.style.display = 'contents'; getComputedStyle(document.documentElement).display", "block");
}

#[test]
fn a_contents_element_has_no_rectangles() {
    let mut b = browser();
    run(&mut b, "var r = c.getBoundingClientRect(); [r.x, r.y, r.width, r.height, c.getClientRects().length].join()", "0,0,0,0,0");
    run(&mut b, "var r = b.getBoundingClientRect(); [r.x, r.width].join()", "100,200");
    run(&mut b, "[c.checkVisibility(), b.checkVisibility()].join()", "false,true");
}

#[test]
fn a_hit_test_finds_the_child_of_a_contents_element() {
    let mut b = browser();
    run(&mut b, "document.elementFromPoint(150, 10).id", "b");
}
