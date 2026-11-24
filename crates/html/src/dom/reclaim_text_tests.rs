//! Tests of the text that a change replaces: it counts as dead, and a rebuild gives it back.

use super::*;

fn no_pins(_: &mut dyn FnMut(&mut Span)) {}

fn page(doc: &mut Document) -> (NodeId, NodeId) {
    let root = doc.root();
    let html = doc.insert_element(root, None, "html", &[] as &[(&str, &str)]);
    let p = doc.insert_element(html, None, "p", &[("id", "keep")]);
    doc.append_text(p, "hello");
    let t = doc.children(p).next().unwrap();
    (p, t)
}

#[test]
fn replaced_text_and_attribute_values_are_dead() {
    let mut doc = Document::new();
    let (p, t) = page(&mut doc);
    let live = doc.live_text_bytes();
    let big = "x".repeat(1000);
    // React and CSS-in-JS set the same text and attributes again and again.
    for i in 0..2000 {
        doc.set_text(t, &format!("{big}{i}"));
        doc.set_attr(p, "class", &format!("{big}{i}"));
        doc.set_attr(p, "title", "t");
        doc.remove_attr(p, "title");
    }
    let now = live - "hello".len() + doc.text(match doc.data(t) { NodeData::Text(s) => *s, _ => unreachable!() }).len() + doc.attr(p, Atom::CLASS).unwrap().len();
    assert_eq!(doc.live_text_bytes(), now, "only the current values are live");
    assert!(doc.text_bytes() > 2000 * 2000, "the buffer still holds the old values");
    doc.reclaim(&[p], &mut no_pins, Free::Collect);
    assert_eq!(doc.text_bytes(), now, "a rebuild gives the old values back");
    assert_eq!(doc.attr(p, Atom::CLASS).unwrap(), format!("{big}1999"));
}
