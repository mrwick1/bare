//! Hit tests of `::before` and `::after` boxes: a point on one hits its element, as in Chrome.

use bare_css::cascade::{Origin, RuleSet, UA_CSS};
use bare_html::dom::{Atom, NodeId};
use bare_html::tree;

use crate::hit::{element_of, hit_test};

#[test]
fn a_point_on_a_generated_box_hits_its_element() {
    let mut doc = tree::parse("<div id=d style='width:200px'>x</div>");
    let mut rules = RuleSet::new((300.0, 400.0));
    rules.add(&mut doc, UA_CSS, Origin::UserAgent);
    rules.add(&mut doc, "body { margin: 0 } #d::before { content: ''; display: block; height: 50px }", Origin::Author);
    let styles = rules.compute(&doc);
    bare_css::generated::sync(&mut doc, &styles);
    let layout = bare_layout::layout(&doc, &styles, (300.0, 400.0));
    let d = doc.children(doc.root()).flat_map(|h| doc.children(h)).flat_map(|b| doc.children(b)).find(|&n| doc.attr(n, Atom::ID) == Some("d")).expect("the div");
    assert_eq!(hit_test(&doc, &styles, &layout, 10.0, 20.0), Some(d));
    let before: NodeId = doc.pseudo(d, 0).expect("a ::before node");
    assert_eq!(element_of(&doc, before), Some(d));
    let text = doc.flat_children(before).next().expect("its text");
    assert_eq!(element_of(&doc, text), Some(d));
}
