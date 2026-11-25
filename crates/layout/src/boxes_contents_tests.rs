//! `display: contents` (CSS Display 3, 2.5): the element makes no box, and its children take its place in the box
//! tree of its parent.

use crate::block::tests::{run, rect};
use crate::fragment::Kind;

/// The text of each text fragment and its x, in tree order.
fn texts(html: &str, css: &str) -> Vec<(String, f32)> {
    let (doc, layout) = run(html, css);
    layout.fragments.iter().filter_map(|f| match (f.kind, doc.data(f.node)) {
        (Kind::Text { start, end }, bare_html::dom::NodeData::Text(span)) => Some((doc.text(*span)[start as usize..end as usize].to_string(), f.rect.x)),
        _ => None,
    }).collect()
}

/// Whether the element with this `id` has a fragment.
fn boxed(html: &str, css: &str, id: &str) -> bool {
    let (doc, layout) = run(html, css);
    let mut stack = vec![doc.root()];
    while let Some(n) = stack.pop() {
        if doc.attr(n, bare_html::dom::Atom::ID) == Some(id) {
            return layout.fragments.iter().any(|f| f.node == n);
        }
        stack.extend(doc.children(n));
    }
    panic!("no element {id}")
}

#[test]
fn a_contents_element_in_a_block_makes_no_box() {
    let html = "<div><div id=c style='display:contents;padding:20px;border:5px solid'><div id=b style='height:10px'></div></div></div>";
    assert!(!boxed(html, "", "c"));
    let b = rect(html, "", "b");
    // No padding and no border of `c`: `b` is at the content edge of the body.
    assert_eq!((b.x, b.y, b.w), (8.0, 8.0, 784.0));
}

#[test]
fn a_contents_element_in_a_line_gives_its_text_to_the_line() {
    let t = texts("<p>ab<span id=c style='display:contents;padding-left:30px'>cd</span>ef</p>", "");
    let x: Vec<_> = t.iter().map(|(s, x)| (s.as_str(), (x * 10.0).round() / 10.0)).collect();
    assert_eq!(x, [("ab", 8.0), ("cd", 27.2), ("ef", 46.4)]);
    // The children inherit from it: a font of 32 px makes letters of 19.2 px.
    assert_eq!(rect("<p><span style='display:contents;font-size:32px'><span id=i style='display:inline-block'>x</span></span></p>", "", "i").w, 19.2);
}

#[test]
fn the_children_of_a_contents_element_are_flex_items() {
    let html = "<div style='display:flex'><div style='display:contents'><span id=a style='width:50px'>x</span><span id=b>y</span></div></div>";
    // `a` is blockified: it takes its width.
    assert_eq!(rect(html, "", "a").w, 50.0);
    assert_eq!(rect(html, "", "b").x, 58.0);
}

#[test]
fn the_children_of_a_contents_element_are_grid_items() {
    let grid = "<div style='display:grid;grid-template-columns:100px 200px 50px'>";
    let html = format!("{grid}<div style='display:contents'></div><div style='display:contents'><div style='display:contents'><div id=a>x</div></div><div id=b>y</div></div><div id=c>z</div></div>");
    // The empty one makes no item. The nested ones give their children.
    assert_eq!(rect(&html, "", "a").x, 8.0);
    assert_eq!((rect(&html, "", "b").x, rect(&html, "", "b").w), (108.0, 200.0));
    assert_eq!(rect(&html, "", "c").x, 308.0);
}

#[test]
fn the_pseudo_elements_of_a_contents_element_have_boxes() {
    let t = texts("<p><span id=c style='display:contents'>x</span></p>", "#c::before { content: '[' } #c::after { content: ']' }");
    assert_eq!(t.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(), ["[", "x", "]"]);
    let css = "#c::before { content: ''; display: block; height: 5px }";
    let html = "<div style='display:grid;grid-template-columns:100px 200px'><div id=c style='display:contents'><div id=b>y</div></div></div>";
    assert_eq!(rect(html, css, "b").x, 108.0);
}

#[test]
fn an_unusual_element_with_contents_makes_no_box() {
    assert!(!boxed("<img id=i style='display:contents;width:10px;height:10px'>", "", "i"));
    assert!(!boxed("<input id=i style='display:contents'>", "", "i"));
}

#[test]
fn text_in_a_contents_element_makes_its_line_as_tall_as_its_font() {
    // As in Chrome: the same lines as with a plain `span` (WPT `display-contents-line-height.html`).
    let ys = |css: &str| {
        let (_, layout) = run("<div><span>Line 1</span><br><span>Line 2</span></div>", css);
        layout.fragments.iter().filter(|f| matches!(f.kind, Kind::Text { .. })).map(|f| f.rect.y).collect::<Vec<_>>()
    };
    assert_eq!(ys("span { display: contents; font-size: 40px }"), ys("span { font-size: 40px }"));
}

#[test]
fn a_fixed_line_height_of_the_parent_box_holds_for_text_in_a_contents_element() {
    // WPT `display-contents-text-line-height.html`: neither the font nor the `line-height` of the text stretches the line.
    let h = |inner: &str| rect(&format!("<div id=d style='line-height:30px'>a {inner} b</div>"), "", "d").h;
    assert_eq!(h("<span style='display:contents;font-size:40px;line-height:80px'>x</span>"), 30.0);
}
