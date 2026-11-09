//! Shadow trees in the cascade: sheets with a scope, `:host`, `::slotted()`, `::part()`, and style through slots.

use crate::cascade::{Origin, RuleSet, Styles, UA_CSS};
use crate::color::Color;
use crate::values::{Keyword, Property, Value};
use bare_html::dom::{Atom, Document, NodeData, NodeId};
use bare_html::tree::parse;

const VIEW: (f32, f32) = (1000.0, 500.0);

fn find(doc: &Document, id: &str) -> NodeId {
    let mut stack = vec![doc.root()];
    while let Some(n) = stack.pop() {
        if doc.attr(n, Atom::ID) == Some(id) {
            return n;
        }
        stack.extend(doc.children(n));
        stack.extend(doc.shadow_root(n).into_iter().flat_map(|r| doc.children(r)));
    }
    panic!("no #{id}")
}

fn copy(doc: &mut Document, frag: &Document, from: NodeId, to: NodeId) {
    for c in frag.children(from).collect::<Vec<_>>() {
        let n = doc.import_node(frag, c).unwrap();
        doc.move_to(n, to, None);
        copy(doc, frag, c, n);
    }
}

/// A page whose host `#h` has the light children `light` and a shadow tree of `shadow`. `doc_css` is the sheet of the document and
/// `shadow_css` the one of the shadow tree. Gives the document and its styles.
fn page(light: &str, shadow: &str, doc_css: &str, shadow_css: &str) -> (Document, Styles) {
    let mut doc = parse(&format!("<body><x-a id=h>{light}</x-a><p id=out>out</p></body>"));
    let host = find(&doc, "h");
    let root = doc.attach_shadow(host, true);
    let frag = parse(&format!("<body>{shadow}</body>"));
    let body = frag.children(frag.children(frag.root()).next().unwrap()).last().unwrap();
    copy(&mut doc, &frag, body, root);
    let mut rules = RuleSet::new(VIEW);
    rules.add(&mut doc, UA_CSS, Origin::UserAgent);
    rules.add(&mut doc, doc_css, Origin::Author);
    rules.add_scoped(&mut doc, shadow_css, Origin::Author, None, root);
    let styles = rules.compute(&doc);
    (doc, styles)
}

fn color(doc: &Document, styles: &Styles, id: &str) -> Option<Value> {
    styles.style(find(doc, id)).map(|s| s.get(Property::TextColor))
}

fn rgb(r: u8, g: u8, b: u8) -> Option<Value> {
    Some(Value::Color(Color { r, g, b, a: 255 }))
}

const RED: Option<Value> = Some(Value::Color(Color { r: 255, g: 0, b: 0, a: 255 }));
const BLUE: Option<Value> = Some(Value::Color(Color { r: 0, g: 0, b: 255, a: 255 }));
const BLACK: Option<Value> = Some(Value::Color(Color::BLACK));

#[test]
fn a_shadow_sheet_reaches_only_its_tree_and_a_document_sheet_does_not_reach_in() {
    let (doc, st) = page("<p id=light>l</p>", "<p id=in>i</p><span id=sp>s</span>", "p { color: red }", "p { color: blue } span { color: blue }");
    assert_eq!(color(&doc, &st, "in"), BLUE, "the rule of the shadow tree applies, and the rule of the document does not");
    assert_eq!(color(&doc, &st, "sp"), BLUE);
    assert_eq!(color(&doc, &st, "out"), RED);
    // The shadow rule does not leak out. The light child is not slotted (no slot), so it has no style at all.
    assert!(st.style(find(&doc, "light")).is_none());
}

#[test]
fn a_document_rule_does_not_reach_into_a_shadow_tree_but_inherits_through_the_host() {
    let (doc, st) = page("", "<p id=in>i</p><i id=em>e</i>", "p { color: red } x-a { color: blue }", "i { display: block }");
    assert_eq!(color(&doc, &st, "in"), BLUE, "color is inherited from the host, not set by the rule for p");
    assert_eq!(color(&doc, &st, "em"), BLUE);
    assert_eq!(st.style(find(&doc, "em")).unwrap().get(Property::Display), Value::Keyword(Keyword::Block));
}

#[test]
fn a_slotted_node_inherits_from_the_slot_not_from_the_host() {
    let (doc, st) = page("<span id=a>a</span>", "<div id=w><slot id=s></slot></div>", "x-a { color: blue }", "#w { color: red }");
    assert_eq!(color(&doc, &st, "a"), RED, "flat tree: the slot is in #w");
    assert_eq!(color(&doc, &st, "s"), RED);
    assert!(st.style(find(&doc, "s")).is_some());
}

#[test]
fn a_document_rule_styles_a_slotted_node() {
    let (doc, st) = page("<span id=a class=c>a</span>", "<slot></slot>", ".c { color: red } span { display: block }", "");
    assert_eq!(color(&doc, &st, "a"), RED);
    assert_eq!(st.style(find(&doc, "a")).unwrap().get(Property::Display), Value::Keyword(Keyword::Block));
}

#[test]
fn host_rules_style_the_host() {
    let (doc, st) = page("", "<p id=in>i</p>", "", ":host { color: red; display: block }");
    assert_eq!(color(&doc, &st, "h"), RED);
    assert_eq!(st.style(find(&doc, "h")).unwrap().get(Property::Display), Value::Keyword(Keyword::Block));
    assert_eq!(color(&doc, &st, "in"), RED, "inherited");
    assert_eq!(color(&doc, &st, "out"), BLACK);
}

#[test]
fn a_host_rule_loses_to_a_document_rule_and_wins_when_important() {
    let (doc, st) = page("", "", "x-a { color: blue }", ":host { color: red }");
    assert_eq!(color(&doc, &st, "h"), BLUE, "the outer context wins for normal declarations, whatever the specificity");
    let (doc, st) = page("", "", "x-a { color: blue }", ":host { color: red !important }");
    assert_eq!(color(&doc, &st, "h"), RED, "the inner context wins for important declarations");
    let (doc, st) = page("", "", "", "x-a { color: red }");
    assert_eq!(color(&doc, &st, "h"), BLACK, "a shadow rule with the name of the host does not match it: the host is outside the tree");
}

#[test]
fn host_with_an_argument_and_host_in_a_selector() {
    let (doc, st) = page("", "<p id=in>i</p><b id=b class=x>b</b>", "", ":host(.on) { color: red } :host(.on) p { color: blue } :host > b { color: red }");
    assert_eq!(color(&doc, &st, "h"), BLACK);
    assert_eq!(color(&doc, &st, "in"), BLACK);
    assert_eq!(color(&doc, &st, "b"), RED, "`:host > b` matches a child of the shadow root");
    let (mut doc, _) = page("", "<p id=in>i</p>", "", "");
    let h = find(&doc, "h");
    doc.set_attr(h, "class", "on");
    let mut rules = RuleSet::new(VIEW);
    rules.add(&mut doc, UA_CSS, Origin::UserAgent);
    let root = doc.shadow_root(h).unwrap();
    rules.add_scoped(&mut doc, ":host(.on) { color: red } :host(.on) p { color: blue } :host(.off) { color: blue }", Origin::Author, None, root);
    let st = rules.compute(&doc);
    assert_eq!(color(&doc, &st, "h"), RED);
    assert_eq!(color(&doc, &st, "in"), BLUE);
}

#[test]
fn host_context_looks_at_the_host_and_its_ancestors() {
    let (doc, st) = page("", "", "", ":host-context(body) { color: red } :host-context(.dark) { color: blue }");
    assert_eq!(color(&doc, &st, "h"), RED);
}

#[test]
fn slotted_rules_style_the_nodes_of_the_slot() {
    let (doc, st) = page("<span id=a class=k>a</span><b id=b>b</b>t", "<slot id=s></slot>", "", "::slotted(span) { color: red } slot::slotted(.k) { display: block } ::slotted(b) { color: blue }");
    assert_eq!(color(&doc, &st, "a"), RED);
    assert_eq!(st.style(find(&doc, "a")).unwrap().get(Property::Display), Value::Keyword(Keyword::Block));
    assert_eq!(color(&doc, &st, "b"), BLUE);
    assert_eq!(color(&doc, &st, "s"), BLACK, "the slot itself is not slotted");
}

#[test]
fn a_document_rule_beats_a_slotted_rule_and_an_important_slotted_rule_wins() {
    let (doc, st) = page("<span id=a>a</span>", "<slot></slot>", "span { color: blue }", "::slotted(span) { color: red }");
    assert_eq!(color(&doc, &st, "a"), BLUE);
    let (doc, st) = page("<span id=a>a</span>", "<slot></slot>", "span { color: blue }", "::slotted(span) { color: red !important }");
    assert_eq!(color(&doc, &st, "a"), RED);
}

#[test]
fn a_named_slot_takes_the_nodes_with_its_name() {
    let (doc, st) = page("<i id=a slot=x>a</i><i id=b>b</i>", "<slot name=x id=sx></slot><slot id=sd></slot>", "", "slot[name=x]::slotted(*) { color: red } ::slotted(i) { display: block }");
    assert_eq!(color(&doc, &st, "a"), RED);
    assert_eq!(color(&doc, &st, "b"), BLACK);
    assert_eq!(st.style(find(&doc, "b")).unwrap().get(Property::Display), Value::Keyword(Keyword::Block));
}

#[test]
fn slot_fallback_is_styled_only_when_nothing_is_taken() {
    let (doc, st) = page("", "<slot><u id=f>f</u></slot>", "", "u { color: red }");
    assert_eq!(color(&doc, &st, "f"), RED);
    let (doc, st) = page("<b id=b>b</b>", "<slot><u id=f>f</u></slot>", "", "u { color: red }");
    assert!(st.style(find(&doc, "f")).is_none());
    assert!(st.style(find(&doc, "b")).is_some());
}

#[test]
fn part_rules_of_the_document_style_the_parts_of_a_shadow_tree() {
    let (doc, st) = page("", "<span id=l part=label>l</span><span id=o>o</span><i id=m part='a b'>m</i>", "x-a::part(label) { color: red } x-a::part(a b) { color: blue } span { color: blue }", "span { color: black }");
    assert_eq!(color(&doc, &st, "l"), RED, "a document rule with ::part beats a rule inside the tree (outer context)");
    assert_eq!(color(&doc, &st, "o"), BLACK);
    assert_eq!(color(&doc, &st, "m"), BLUE, "both names are needed");
}

#[test]
fn part_with_a_pseudo_class_after_it_and_exportparts() {
    let (doc, st) = page("", "<span id=l part=label>l</span>", "x-a::part(label):first-child { color: red }", "");
    assert_eq!(color(&doc, &st, "l"), RED);
    // A part of a nested host is visible from the outer tree only when the nested host exports it.
    let mut doc = parse("<body><x-a id=h1></x-a></body>");
    let h1 = find(&doc, "h1");
    let r1 = doc.attach_shadow(h1, true);
    let h2 = doc.insert_element(r1, None, "x-b", &[("id", "h2"), ("exportparts", "label:outer")]);
    let r2 = doc.attach_shadow(h2, true);
    doc.insert_element(r2, None, "span", &[("id", "s"), ("part", "label")]);
    let mut rules = RuleSet::new(VIEW);
    rules.add(&mut doc, UA_CSS, Origin::UserAgent);
    rules.add(&mut doc, "x-a::part(outer) { color: red }", Origin::Author);
    rules.add_scoped(&mut doc, "x-b::part(label) { color: blue }", Origin::Author, None, r1);
    let st = rules.compute(&doc);
    assert_eq!(color(&doc, &st, "s"), RED);
}

#[test]
fn a_page_with_no_shadow_root_is_unchanged() {
    let mut doc = parse("<body><p id=a>a</p></body>");
    let mut rules = RuleSet::new(VIEW);
    rules.add(&mut doc, UA_CSS, Origin::UserAgent);
    rules.add(&mut doc, "p { color: red } :host { color: blue } p::part(x) { color: blue }", Origin::Author);
    let st = rules.compute(&doc);
    assert_eq!(color(&doc, &st, "a"), RED);
    let _ = NodeData::Document;
}

mod selectors {
    use crate::selector::{Mode, parse_selector_list, parse_selector_list_in};
    use bare_html::dom::Document;

    fn spec(text: &str, mode: Mode) -> Option<(u32, u32, u32)> {
        let s = &parse_selector_list_in(text, &mut Document::new(), mode)?[0];
        Some((s.specificity >> 20, (s.specificity >> 10) & 1023, s.specificity & 1023))
    }

    #[test]
    fn a_script_may_ask_for_them_and_nothing_matches() {
        for text in [":host", ":host(.a)", ":host-context(.a)", "::slotted(b)", "x-a::part(y z)", "x-a::part(y):hover"] {
            assert!(parse_selector_list(text, &mut Document::new()).is_some(), "{text}");
        }
    }

    #[test]
    fn they_count_in_the_specificity() {
        assert_eq!(spec(":host", Mode::Shadow), Some((0, 1, 0)));
        assert_eq!(spec(":host(.a)", Mode::Shadow), Some((0, 2, 0)));
        assert_eq!(spec(":host(#i) p", Mode::Shadow), Some((1, 1, 1)));
        assert_eq!(spec("::slotted(span)", Mode::Shadow), Some((0, 0, 2)));
        assert_eq!(spec("slot::slotted(.k)", Mode::Shadow), Some((0, 1, 2)));
        assert_eq!(spec("x-a::part(l)", Mode::Document), Some((0, 0, 2)));
    }

    #[test]
    fn a_wrong_use_drops_the_rule() {
        for text in [".a :host", ":host.a", ":host#i", "::slotted(a) b", "::slotted()", "::part()", "x::part(a)::slotted(b)", "::slotted(a b)", ":not(::slotted(a))", "x::part(a) y"] {
            assert!(parse_selector_list_in(text, &mut Document::new(), Mode::Shadow).is_none(), "{text}");
        }
    }
}
