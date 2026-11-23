//! `::before` and `::after` boxes (CSS Generated Content): the pseudo nodes of `bare_css::generated::sync` lay out as the first and the last
//! child of their element.

use crate::block::tests::{dump, rect, run};
use crate::fragment::Kind;

/// The text of each text fragment, in tree order.
fn texts(html: &str, css: &str) -> Vec<String> {
    let (doc, layout) = run(html, css);
    layout.fragments.iter().filter_map(|f| match f.kind {
        Kind::Text { start, end } => match doc.data(f.node) {
            bare_html::dom::NodeData::Text(span) => Some(doc.text(*span)[start as usize..end as usize].to_string()),
            _ => None,
        },
        _ => None,
    }).collect()
}

#[test]
fn before_and_after_text_lays_out_around_the_children() {
    let css = "#p::before { content: '[' attr(title) ' ' } #p::after { content: ']' }";
    assert_eq!(texts("<p id=p title=T>mid</p>", css), ["[T ", "mid", "]"]);
    // Seven letters of 9.6 px: the boxes make the content wider.
    assert!((rect("<span id=p title=T style='display:inline-block'>mid</span>", css, "p").w - 7.0 * 9.6).abs() < 0.01);
    // `none`, `normal` and no `content` make no box.
    assert_eq!(texts("<p id=p>mid</p>", "#p::before { content: none } #p::after { color: red }"), ["mid"]);
}

#[test]
fn a_generated_box_takes_its_display_and_size() {
    let block = "#p::before { content: 'x'; display: block }";
    assert_eq!(rect("<div id=p>y</div>", block, "p").h, 44.0);
    let icon = "#p::before { content: ''; display: inline-block; width: 40px; height: 40px }";
    assert_eq!(rect("<span id=p style='display:inline-block'></span>", icon, "p").w, 40.0);
    assert!(dump("<div id=p>y</div>", "#p::after { content: ''; display: block; height: 30px }").contains("::after"), "the box shows in the dump");
    assert_eq!(rect("<div id=p>y</div>", "#p::after { content: ''; display: block; height: 30px }", "p").h, 52.0);
    // A hidden element makes no box for its pseudo-elements.
    assert_eq!(texts("<p id=p style='display:none'>m</p><i>n</i>", "#p::before { content: 'x' }"), ["n"]);
}

