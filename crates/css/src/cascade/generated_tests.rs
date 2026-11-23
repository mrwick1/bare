//! The styles of the `::before` and `::after` boxes (`Styles::generated`): a full cascade, and a restyle after a change.

use super::*;
use crate::selector::{AFTER, BEFORE};
use bare_html::dom::DAMAGE_FRAME;
use bare_html::tree::parse;

const VIEW: (f32, f32) = (1000.0, 500.0);

const CSS: &str = "#a::before { content: 'B'; color: rgb(255, 0, 0) } #a::after { content: 'A' } p { color: rgb(0, 0, 255) }
    #b::before { content: ''; display: inline-block; width: 40px } #c::before { color: red } #d::before { content: none }
    #e::before { content: 'x'; display: none } .on::after { content: attr(title) } #f:BEFORE { content: 'old' }";

fn rules(doc: &mut Document) -> RuleSet {
    let mut rules = RuleSet::new(VIEW);
    rules.add(doc, UA_CSS, Origin::UserAgent);
    rules.add(doc, CSS, Origin::Author);
    rules
}

fn id(doc: &Document, name: &str) -> NodeId {
    let mut found = None;
    walk(doc, false, |n| {
        if found.is_none() && doc.attr(n, Atom::ID) == Some(name) {
            found = Some(n);
        }
    });
    found.unwrap()
}

#[test]
fn before_and_after_get_a_style_when_their_content_makes_a_box() {
    let mut doc = parse("<p id=a>x</p><div id=b></div><div id=c></div><div id=d></div><div id=e></div><div id=f></div><div id=g></div>");
    let st = rules(&mut doc).compute(&doc);
    let (a, b) = (id(&doc, "a"), id(&doc, "b"));
    let before = st.generated(a, BEFORE).expect("#a::before makes a box");
    assert_eq!(before.color(Property::TextColor), Color { r: 255, g: 0, b: 0, a: 255 });
    assert_eq!(crate::content::serialize(before.picture(Property::Content)), "\"B\"");
    // `::after` inherits from its element: the blue of `p`.
    assert_eq!(st.generated(a, AFTER).unwrap().color(Property::TextColor), Color { r: 0, g: 0, b: 255, a: 255 });
    let boxed = st.generated(b, BEFORE).unwrap();
    assert_eq!((boxed.get(Property::Display), boxed.get(Property::Width)), (Value::Keyword(Keyword::InlineBlock), Value::Length(Length::Px(40.0))));
    // No `content`, `content: none`, `display: none`, no rule: no box.
    for n in ["c", "d", "e", "g"] {
        assert!(st.generated(id(&doc, n), BEFORE).is_none() && st.generated(id(&doc, n), AFTER).is_none(), "{n}");
    }
    assert!(st.generated(id(&doc, "f"), BEFORE).is_some(), "the old one-colon form");
    // The element itself does not take the rules of its pseudo-elements.
    assert_eq!(st.style(a).unwrap().color(Property::TextColor), Color { r: 0, g: 0, b: 255, a: 255 });
}

#[test]
fn a_restyle_adds_and_removes_the_boxes() {
    let mut doc = parse("<div id=t title=hi>x</div><p id=a>y</p>");
    let rules = rules(&mut doc);
    let mut st = rules.compute_kept(&doc);
    doc.track_damage(DAMAGE_FRAME);
    let t = id(&doc, "t");
    assert!(st.generated(t, AFTER).is_none());
    let mut step = |doc: &mut Document, change: &dyn Fn(&mut Document)| {
        change(doc);
        let mut roots = Vec::new();
        assert!(doc.take_damage(DAMAGE_FRAME).unwrap().into_iter().all(|m| rules.roots(doc, m, &mut roots)));
        let mut changed = Vec::new();
        assert!(rules.restyle(doc, &mut st, &roots, &mut changed));
        (st.generated(t, AFTER).is_some(), changed.iter().any(|c| c.0 == t))
    };
    // The class adds the box, though the style of the element stays the same: the element is in the changed list.
    assert_eq!(step(&mut doc, &|d| d.set_attr(t, "class", "on")), (true, true));
    assert_eq!(step(&mut doc, &|d| d.set_attr(t, "class", "")), (false, true));
    assert_eq!(step(&mut doc, &|d| d.set_attr(t, "class", "on")), (true, true));
    // Hidden: no box.
    assert_eq!(step(&mut doc, &|d| d.set_attr(t, "style", "display: none")), (false, true));
}
