//! The `grid-template` shorthand set through the CSSOM (`style.gridTemplate`, `setProperty`) reaches the cascade, as one set
//! through `cssText` does.
use crate::browser::Browser;
use crate::eng_new::Peek;

fn browser() -> Browser {
    let html = "<html><body><div id=g style='display:grid;width:400px;height:300px'><div id=a style='grid-area:t'>A</div><div id=b style='grid-area:m'>B</div></div></body></html>";
    Browser::new(bare_html::tree::parse(html), Default::default(), Default::default(), Default::default()).unwrap()
}

fn run(b: &mut Browser, source: &str, expected: &str) {
    let v = b.run_script(source).expect(source);
    assert_eq!(b.it.display(v), expected, "{source}");
}

const READ: &str = "var cs = getComputedStyle(g); cs.gridTemplateAreas + '|' + cs.gridTemplateRows + '|' + cs.gridTemplateColumns";

#[test]
fn the_grid_template_property_sets_the_areas() {
    let mut b = browser();
    run(&mut b, "var g = document.getElementById('g'); g.style.gridTemplate = '\"t t\" 40px \"n m\" 1fr / 100px 1fr'; ''", "");
    run(&mut b, READ, "\"t t\" \"n m\"|40px 260px|100px 300px");
}

#[test]
fn set_property_of_grid_template_sets_the_areas() {
    let mut b = browser();
    run(&mut b, "var g = document.getElementById('g'); g.style.setProperty('grid-template', '\"t t\" \"n m\" minmax(0px, 1fr) / auto minmax(0px, 1fr)'); ''", "");
    run(&mut b, "getComputedStyle(g).gridTemplateAreas", "\"t t\" \"n m\"");
}

#[test]
fn css_text_of_grid_template_sets_the_areas() {
    let mut b = browser();
    run(&mut b, "var g = document.getElementById('g'); g.style.cssText = 'display:grid;width:400px;height:300px;grid-template:\"t t\" 40px \"n m\" 1fr / 100px 1fr'; ''", "");
    run(&mut b, READ, "\"t t\" \"n m\"|40px 260px|100px 300px");
}
