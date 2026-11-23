//! The `::before` and `::after` nodes (`pseudo.rs`): their place in the flat tree, and their life in the arena.

use super::*;
use crate::dom::reclaim::Free;

fn no_pins(_: &mut dyn FnMut(&mut Span)) {}

fn text_in(doc: &Document, n: NodeId) -> String {
    doc.flat_children(n).map(|t| match doc.data(t) {
        NodeData::Text(span) => doc.text(*span).to_string(),
        _ => String::new(),
    }).collect()
}

#[test]
fn pseudo_nodes_are_first_and_last_in_the_flat_tree_only() {
    let mut doc = Document::new();
    let p = doc.insert_element(doc.root(), None, "p", &[] as &[(&str, &str)]);
    doc.append_text(p, "mid");
    let mid = doc.children(p).next().expect("the text");
    let revision = doc.revision();
    let before = doc.show_pseudo(p, 0, "a");
    let after = doc.show_pseudo(p, 1, "z");
    assert_eq!(doc.revision(), revision, "no change record");
    assert_eq!(doc.flat_children(p).collect::<Vec<_>>(), [before, mid, after]);
    assert_eq!((doc.flat_next(before), doc.flat_next(mid), doc.flat_next(after)), (Some(mid), Some(after), None));
    assert_eq!((doc.flat_parent(before), doc.flat_parent(after)), (Some(p), Some(p)));
    assert_eq!(doc.children(p).collect::<Vec<_>>(), [mid], "the DOM does not see them");
    assert_eq!((doc.element_name(before), text_in(&doc, before)), ("::before", "a".to_string()));
    // Hidden, then shown again: the same node, with the new text.
    doc.hide_pseudo(p, 0);
    assert_eq!(doc.flat_children(p).collect::<Vec<_>>(), [mid, after]);
    assert_eq!(doc.show_pseudo(p, 0, "b"), before);
    assert_eq!(text_in(&doc, before), "b");
    // An element with no children: the `::before` node comes before the `::after` node.
    let empty = doc.insert_element(doc.root(), None, "i", &[] as &[(&str, &str)]);
    let (b, a) = (doc.show_pseudo(empty, 0, ""), doc.show_pseudo(empty, 1, ""));
    assert_eq!(doc.flat_children(empty).collect::<Vec<_>>(), [b, a]);
    assert_eq!(doc.flat_next(b), Some(a));
}

#[test]
fn pseudo_nodes_live_and_die_with_their_element() {
    let mut doc = Document::new();
    let keep = doc.insert_element(doc.root(), None, "p", &[] as &[(&str, &str)]);
    let held = doc.show_pseudo(keep, 1, "x");
    let gone = doc.create_element("div");
    let pseudo = doc.show_pseudo(gone, 0, "y");
    assert_eq!(doc.reclaim(&[keep, pseudo], &mut no_pins, Free::Now), 0, "a pseudo node keeps its element");
    assert_eq!(doc.reclaim(&[keep], &mut no_pins, Free::Now), 2, "the element and its pseudo node");
    assert!(doc.is_free(gone) && doc.is_free(pseudo) && !doc.is_free(held));
    assert_eq!(doc.pseudo(keep, 1), Some(held));
    assert_eq!(doc.pseudo_host(held), Some((keep, 1)));
}
