//! `env()`: the values of the browser, like `env(safe-area-inset-left, 0px)`.

use crate::cascade::{Origin, RuleSet, UA_CSS};
use crate::values::{Declared, Length, Property, Side, Value, parse_declaration};
use bare_html::dom::{Atom, Document, NodeId};
use bare_html::tree::parse;

fn read(name: &str, value: &str) -> Option<Vec<Declared>> {
    let mut out = Vec::new();
    parse_declaration(name, value, false, &mut Side::default(), &mut out).then_some(out)
}

#[test]
fn a_safe_area_inset_is_zero_and_the_declaration_is_kept() {
    assert_eq!(read("padding-left", "max(16px, env(safe-area-inset-left, 0px))"), read("padding-left", "max(16px, 0px)"));
    assert_eq!(read("padding-top", "env(safe-area-inset-top)"), read("padding-top", "0px"));
    assert_eq!(read("padding", "ENV(safe-area-inset-bottom) 4px"), read("padding", "0px 4px"), "the name of the function has no case");
    assert_eq!(read("margin-bottom", "calc(8px + env(keyboard-inset-height, 50px))"), read("margin-bottom", "calc(8px + 0px)"));
}

#[test]
fn an_unknown_name_takes_the_fallback() {
    assert_eq!(read("padding-right", "env(no-such-thing, 7px)"), read("padding-right", "7px"));
    assert_eq!(read("padding-right", "env(no-such-thing, env(safe-area-inset-right))"), read("padding-right", "0px"), "a fallback may hold env()");
    assert_eq!(read("padding-right", "env(no-such-thing)"), None, "no value and no fallback");
    assert_eq!(read("padding-right", "env(safe-area-inset-right"), None, "not closed");
    assert_eq!(read("padding-right", "env()"), None);
}

#[test]
fn env_text_in_a_string_or_a_longer_name_is_not_env() {
    assert_eq!(read("content", "\"env(x)\""), read("content", "\"env(x)\""));
    assert_eq!(read("padding-left", "myenv(safe-area-inset-left)"), None);
}

fn find(doc: &Document, id: &str) -> NodeId {
    let mut stack = vec![doc.root()];
    while let Some(n) = stack.pop() {
        if doc.attr(n, Atom::ID) == Some(id) {
            return n;
        }
        stack.extend(doc.children(n));
    }
    panic!("no #{id}")
}

#[test]
fn env_works_with_var_on_both_sides() {
    let mut doc = parse("<div id=a></div><div id=b></div>");
    let mut rules = RuleSet::new((1000.0, 500.0));
    rules.add(&mut doc, UA_CSS, Origin::UserAgent);
    rules.add(&mut doc, "div { --gap: 5px; --edge: env(safe-area-inset-left, 3px) } #a { padding-left: env(no-such-thing, var(--gap)) } #b { padding-left: var(--edge) }", Origin::Author);
    let st = rules.compute(&doc);
    let pad = |id| st.style(find(&doc, id)).unwrap().get(Property::PaddingLeft);
    assert_eq!(pad("a"), Value::Length(Length::Px(5.0)), "a var() in the fallback of env()");
    assert_eq!(pad("b"), Value::Length(Length::Px(0.0)), "env() in a custom property");
}
