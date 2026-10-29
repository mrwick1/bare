//! Matching: does a selector match an element?
//!
//! The matcher works from the right to the left. It tests the last compound on the element first.
//! If that fails, it stops: most selectors in a big style sheet end at once here, and that is why this is fast.
//! If it works, it goes up the tree for the next compound:
//!
//! ```text
//! ul > li a        element: the `a`
//!   1. `a` matches the element.
//!   2. descendant: check each ancestor for `li`. The first ancestor that is an `li` goes on:
//!   3. child: the parent of that `li` must be an `ul`.
//! ```
//!
//! A descendant (`a b`) or later-sibling (`a ~ b`) step may have more than one place to go. If the first
//! choice fails further on, the matcher tries the next one. (JS: like a regex that backtracks.) The cost is the
//! depth of the tree for each such step, and nothing else is stored. The ancestor filter of the cascade
//! (topic 5) skips most of these walks.
//!
//! Only elements count. A text node or a comment is never a parent, a sibling, or an element for `:first-child`.
//! The parent of the `html` element is the document node, and that is not an element, so `* > html` does not match.
//!
//! Comments compare Rust with JavaScript, because you know JS.

use bare_html::dom::{Atom, Document, NodeData, NodeId};

use crate::selector::{AttrTest, Case, Combinator, Compound, Nth, Selector, Simple};

/// Does `selector` match the element `el`? `el` must be an element of `doc`, and the selector must be
/// parsed for `doc`. For any other node, it gives `false`.
pub fn matches(doc: &Document, el: NodeId, selector: &Selector) -> bool {
    is_element(doc, el) && matches_from(doc, el, &selector.compounds)
    // Without this check, `*` would match a text node, because it has no simple selectors to fail.
}

fn is_element(doc: &Document, id: NodeId) -> bool {
    matches!(doc.data(id), NodeData::Element { .. })
}

/// The parent, if it is an element.
fn parent_element(doc: &Document, id: NodeId) -> Option<NodeId> {
    doc.parent(id).filter(|&p| is_element(doc, p))
}

/// The element before this one in the same parent.
fn prev_element(doc: &Document, mut id: NodeId) -> Option<NodeId> {
    while let Some(prev) = doc.prev_sibling(id) {
        if is_element(doc, prev) {
            return Some(prev);
        }
        id = prev;
    }
    None
}

fn next_element(doc: &Document, mut id: NodeId) -> Option<NodeId> {
    while let Some(next) = doc.next_sibling(id) {
        if is_element(doc, next) {
            return Some(next);
        }
        id = next;
    }
    None
}

/// `compounds[0]` must match `el`, and the rest must match the elements it points to.
fn matches_from(doc: &Document, el: NodeId, compounds: &[Compound]) -> bool {
    let Some((first, rest)) = compounds.split_first() else { return true };
    if !compound_matches(doc, el, &first.simples) {
        return false;
    }
    if rest.is_empty() {
        return true;
    }
    match first.combinator {
        Combinator::Child => parent_element(doc, el).is_some_and(|p| matches_from(doc, p, rest)),
        Combinator::Next => prev_element(doc, el).is_some_and(|p| matches_from(doc, p, rest)),
        Combinator::Descendant => {
            let mut up = parent_element(doc, el);
            while let Some(a) = up {
                if matches_from(doc, a, rest) {
                    return true;
                }
                up = parent_element(doc, a);
            }
            false
        }
        Combinator::Later => {
            let mut before = prev_element(doc, el);
            while let Some(s) = before {
                if matches_from(doc, s, rest) {
                    return true;
                }
                before = prev_element(doc, s);
            }
            false
        }
    }
}

fn compound_matches(doc: &Document, el: NodeId, simples: &[Simple]) -> bool {
    // `all` stops at the first false. The selector parser puts the tag first, and it is the cheapest test.
    simples.iter().all(|s| simple_matches(doc, el, s))
}

fn simple_matches(doc: &Document, el: NodeId, simple: &Simple) -> bool {
    let &NodeData::Element { name, attrs } = doc.data(el) else { return false };
    match simple {
        Simple::Tag(tag) => name == *tag,
        Simple::Id(id) => doc.attr(el, Atom::ID) == Some(id),
        Simple::Class(class) => doc.attr(el, Atom::CLASS).is_some_and(|v| v.split_ascii_whitespace().any(|c| c == &**class)),
        Simple::Attr { name, test } => doc.attrs(attrs).iter().find(|a| a.name == *name).is_some_and(|a| test_value(test, doc.text(a.value))),
        Simple::FirstChild => prev_element(doc, el).is_none(),
        Simple::LastChild => next_element(doc, el).is_none(),
        Simple::OnlyChild => prev_element(doc, el).is_none() && next_element(doc, el).is_none(),
        Simple::Nth(nth) => nth_matches(doc, el, *nth),
        Simple::Root => doc.parent(el) == Some(doc.root()),
        Simple::Empty => doc.children(el).all(|c| match *doc.data(c) {
            NodeData::Element { .. } => false,
            NodeData::Text(span) => doc.text(span).is_empty(),
            _ => true,
            // A comment does not count.
        }),
        Simple::Link => matches!(doc.atom_str(name), "a" | "area") && doc.attrs(attrs).iter().any(|a| doc.atom_str(a.name) == "href"),
        Simple::Never => false,
        Simple::Is(args) => args.iter().any(|simples| compound_matches(doc, el, simples)),
        Simple::Not(args) => !args.iter().any(|simples| compound_matches(doc, el, simples)),
    }
}

/// Is the place of `el` among its sibling elements `a*n + b` for some `n >= 0`?
/// The places come from the document: it counts them for all the elements of a parent in one pass.
fn nth_matches(doc: &Document, el: NodeId, nth: Nth) -> bool {
    let place = i64::from(doc.sibling_place(el, nth.of_type, nth.from_end));
    let (a, b) = (i64::from(nth.a), i64::from(nth.b));
    if a == 0 { place == b } else { (place - b) % a == 0 && (place - b) / a >= 0 }
}

/// The value of the attribute against the test.
fn test_value(test: &AttrTest, value: &str) -> bool {
    match test {
        AttrTest::Exists => true,
        AttrTest::Equals(v, case) => with_case(value, *case, |value| value == &**v),
        AttrTest::Word(v, case) => !v.is_empty() && !v.contains(|c: char| c.is_ascii_whitespace()) && with_case(value, *case, |value| value.split_ascii_whitespace().any(|w| w == &**v)),
        AttrTest::Dash(v, case) => with_case(value, *case, |value| value == &**v || value.strip_prefix(&**v).is_some_and(|rest| rest.starts_with('-'))),
        // An empty value in `^=`, `$=` and `*=` matches nothing. This is the rule of the standard, and it is not what `starts_with("")` does.
        AttrTest::Prefix(v, case) => !v.is_empty() && with_case(value, *case, |value| value.starts_with(&**v)),
        AttrTest::Suffix(v, case) => !v.is_empty() && with_case(value, *case, |value| value.ends_with(&**v)),
        AttrTest::Contains(v, case) => !v.is_empty() && with_case(value, *case, |value| value.contains(&**v)),
    }
}

/// Run `check` on the value. With `Insensitive`, it runs on a lowercase copy (the selector value is lowercase already).
fn with_case(value: &str, case: Case, check: impl Fn(&str) -> bool) -> bool {
    match case {
        Case::Sensitive => check(value),
        Case::Insensitive => check(&value.to_ascii_lowercase()),
        // This allocates, but only for the `i` flag, and that is rare.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selector::parse_selector_list;
    use bare_html::tree::parse;

    /// The elements of the page that match `selector`, as their `id` attributes. Every element in the test
    /// pages has an `id` (or `x` for the ones we do not ask about), so the answer is easy to read.
    fn find(html: &str, selector: &str) -> Vec<String> {
        let mut doc = parse(html);
        let list = parse_selector_list(selector, &mut doc).unwrap_or_else(|| panic!("{selector} did not parse"));
        let mut found = Vec::new();
        let mut stack = vec![doc.root()];
        // Walk the tree in document order. A stack, so no call recursion.
        while let Some(id) = stack.pop() {
            let kids: Vec<_> = doc.children(id).collect();
            stack.extend(kids.into_iter().rev());
            if is_element(&doc, id) && list.iter().any(|s| matches(&doc, id, s)) {
                found.push(doc.attr(id, Atom::ID).unwrap_or("?").to_string());
            }
        }
        found
    }

    const PAGE: &str = r#"<html id=html><body id=body>
        <div id=d1 class="box big"><p id=p1 class=intro>one</p><p id=p2>two</p><span id=s1></span></div>
        <div id=d2 class=box><ul id=u1><li id=l1><a id=a1 href="http://x.org/">x</a></li><li id=l2 class=on>two</li><li id=l3><!-- c --></li></ul></div>
        <p id=p3 lang=en-US data-k="A b">three</p>
    </body></html>"#;

    #[test]
    fn tag_class_id() {
        assert_eq!(find(PAGE, "p"), ["p1", "p2", "p3"]);
        assert_eq!(find(PAGE, "P"), ["p1", "p2", "p3"]);
        assert_eq!(find(PAGE, ".box"), ["d1", "d2"]);
        assert_eq!(find(PAGE, ".big.box"), ["d1"]);
        assert_eq!(find(PAGE, "div.box.big"), ["d1"]);
        assert_eq!(find(PAGE, "#p2"), ["p2"]);
        assert_eq!(find(PAGE, "p#p2.intro"), Vec::<String>::new());
        assert_eq!(find(PAGE, "#nothing"), Vec::<String>::new());
    }

    #[test]
    fn universal() {
        assert_eq!(find(PAGE, "*").len(), 14);
        // `html`, `head` (the tree builder adds it), `body`, and the 11 elements of the page.
        assert_eq!(find(PAGE, "div > *"), ["p1", "p2", "s1", "u1"]);
    }

    #[test]
    fn lists() {
        assert_eq!(find(PAGE, "#p1, #l2, .big"), ["d1", "p1", "l2"]);
    }

    #[test]
    fn descendant_and_child() {
        assert_eq!(find(PAGE, "div p"), ["p1", "p2"]);
        assert_eq!(find(PAGE, "body p"), ["p1", "p2", "p3"]);
        assert_eq!(find(PAGE, "div > p"), ["p1", "p2"]);
        assert_eq!(find(PAGE, "div li"), ["l1", "l2", "l3"]);
        assert_eq!(find(PAGE, "div > li"), Vec::<String>::new());
        assert_eq!(find(PAGE, "ul > li > a"), ["a1"]);
        assert_eq!(find(PAGE, "div ul > li a"), ["a1"]);
        assert_eq!(find(PAGE, "#d2 a"), ["a1"]);
        assert_eq!(find(PAGE, "#d1 a"), Vec::<String>::new());
    }

    #[test]
    fn descendant_tries_the_next_ancestor() {
        // `div div p`: the nearest `div` of the `p` has no `div` above it. The matcher must go on to the next ancestor.
        let html = "<div id=o><div id=m><section><div id=i><p id=x>x</p></div></section></div></div>";
        assert_eq!(find(html, "div div p"), ["x"]);
        assert_eq!(find(html, "#o div > p"), ["x"]);
        assert_eq!(find(html, "#m > div p"), Vec::<String>::new());
        assert_eq!(find(html, "#m section > div > p"), ["x"]);
    }

    #[test]
    fn sibling_combinators() {
        assert_eq!(find(PAGE, "p + p"), ["p2"]);
        assert_eq!(find(PAGE, "p + span"), ["s1"]);
        assert_eq!(find(PAGE, "p ~ span"), ["s1"]);
        assert_eq!(find(PAGE, "p.intro ~ *"), ["p2", "s1"]);
        assert_eq!(find(PAGE, "span + p"), Vec::<String>::new());
        assert_eq!(find(PAGE, "div + div"), ["d2"]);
        assert_eq!(find(PAGE, "div + p"), ["p3"]);
        assert_eq!(find(PAGE, "li.on + li"), ["l3"]);
        assert_eq!(find(PAGE, "li ~ li"), ["l2", "l3"]);
    }

    #[test]
    fn later_sibling_tries_the_next_sibling() {
        // `b ~ c`, with `a` before `b`: `a.x ~ i ~ c`. The nearest `i` before `c` must be tried, then the one before.
        let html = "<a id=a1 class=x></a><i id=i1></i><i id=i2></i><c id=c1></c>";
        assert_eq!(find(html, ".x ~ i ~ c"), ["c1"]);
        assert_eq!(find(html, "i + i ~ c"), ["c1"]);
        assert_eq!(find(html, "c ~ i"), Vec::<String>::new());
    }

    #[test]
    fn text_and_comments_are_not_siblings() {
        let html = "<p id=a></p> text <!-- c --><p id=b></p>";
        assert_eq!(find(html, "p + p"), ["b"]);
        assert_eq!(find(html, "p:first-child"), ["a"]);
    }

    #[test]
    fn attributes() {
        assert_eq!(find(PAGE, "[href]"), ["a1"]);
        assert_eq!(find(PAGE, "a[href]"), ["a1"]);
        assert_eq!(find(PAGE, "[href=\"http://x.org/\"]"), ["a1"]);
        assert_eq!(find(PAGE, "[href=x]"), Vec::<String>::new());
        assert_eq!(find(PAGE, "[class~=big]"), ["d1"]);
        assert_eq!(find(PAGE, "[class~=bi]"), Vec::<String>::new());
        assert_eq!(find(PAGE, "[lang|=en]"), ["p3"]);
        assert_eq!(find(PAGE, "[lang|=e]"), Vec::<String>::new());
        assert_eq!(find(PAGE, "[lang|=en-US]"), ["p3"]);
        assert_eq!(find(PAGE, "[href^=http]"), ["a1"]);
        assert_eq!(find(PAGE, "[href$=\".org/\"]"), ["a1"]);
        assert_eq!(find(PAGE, "[href*=x\\.o]"), ["a1"]);
        assert_eq!(find(PAGE, "[id^=l]"), ["l1", "l2", "l3"]);
        assert_eq!(find(PAGE, "[id=p1]"), ["p1"]);
    }

    #[test]
    fn attribute_case_and_empty_values() {
        assert_eq!(find(PAGE, "[data-k=\"A b\"]"), ["p3"]);
        assert_eq!(find(PAGE, "[data-k=\"a b\"]"), Vec::<String>::new());
        assert_eq!(find(PAGE, "[data-k=\"a b\" i]"), ["p3"]);
        assert_eq!(find(PAGE, "[data-k^=\"A B\" i]"), ["p3"]);
        assert_eq!(find(PAGE, "[DATA-K]"), ["p3"]);
        for empty in ["[lang^='']", "[lang$='']", "[lang*='']", "[lang~='']", "[lang~='a b']"] {
            assert_eq!(find(PAGE, empty), Vec::<String>::new(), "{empty}");
        }
        assert_eq!(find("<p id=e lang=''></p>", "[lang='']"), ["e"]);
    }

    #[test]
    fn class_splits_on_white_space() {
        let html = "<p id=a class=' x\ty\nz '></p><p id=b class='xy'></p>";
        assert_eq!(find(html, ".y"), ["a"]);
        assert_eq!(find(html, ".z"), ["a"]);
        assert_eq!(find(html, ".x"), ["a"]);
    }

    #[test]
    fn structural_pseudo_classes() {
        assert_eq!(find(PAGE, "li:first-child"), ["l1"]);
        assert_eq!(find(PAGE, "li:last-child"), ["l3"]);
        assert_eq!(find(PAGE, "ul:only-child"), ["u1"]);
        assert_eq!(find(PAGE, "p:first-child"), ["p1"]);
        assert_eq!(find(PAGE, "span:last-child"), ["s1"]);
        assert_eq!(find(PAGE, "span:only-child"), Vec::<String>::new());
        assert_eq!(find(PAGE, ":root"), ["html"]);
        assert_eq!(find(PAGE, "* > :root"), Vec::<String>::new());
        assert_eq!(find(PAGE, "li:empty"), ["l3"]);
        assert_eq!(find(PAGE, "span:empty"), ["s1"]);
        assert_eq!(find(PAGE, "p:empty"), Vec::<String>::new());
        assert_eq!(find(PAGE, ":link"), ["a1"]);
        assert_eq!(find(PAGE, "a:hover"), Vec::<String>::new());
        assert_eq!(find(PAGE, "a:visited, a:focus"), Vec::<String>::new());
    }

    #[test]
    fn nth_child_and_type() {
        assert_eq!(find(PAGE, "li:nth-child(2)"), ["l2"]);
        assert_eq!(find(PAGE, "li:nth-child(odd)"), ["l1", "l3"]);
        assert_eq!(find(PAGE, "li:nth-child(even)"), ["l2"]);
        assert_eq!(find(PAGE, "li:nth-child(n+2)"), ["l2", "l3"]);
        assert_eq!(find(PAGE, "li:nth-child(-n+2)"), ["l1", "l2"]);
        assert_eq!(find(PAGE, "li:nth-child(3n)"), ["l3"]);
        assert_eq!(find(PAGE, "li:nth-child(0)"), Vec::<String>::new());
        assert_eq!(find(PAGE, "li:nth-last-child(1)"), ["l3"]);
        assert_eq!(find(PAGE, "li:nth-last-child(2n)"), ["l2"]);
        // In `d1`: p, p, span. The span is the 3rd child, and the first of its type.
        assert_eq!(find(PAGE, "#d1 > :nth-child(3)"), ["s1"]);
        assert_eq!(find(PAGE, "#d1 > p:nth-of-type(2)"), ["p2"]);
        assert_eq!(find(PAGE, "#d1 > :nth-of-type(1)"), ["p1", "s1"]);
        assert_eq!(find(PAGE, "#d1 > p:nth-last-of-type(1)"), ["p2"]);
        assert_eq!(find(PAGE, "p:first-of-type"), ["p1", "p3"]);
        assert_eq!(find(PAGE, "p:last-of-type"), ["p2", "p3"]);
        assert_eq!(find(PAGE, "div:nth-child(2n+1)"), ["d1"]);
    }

    #[test]
    fn not() {
        assert_eq!(find(PAGE, "p:not(.intro)"), ["p2", "p3"]);
        assert_eq!(find(PAGE, "li:not(.on):not(:first-child)"), ["l3"]);
        assert_eq!(find(PAGE, "li:not(.on, :last-child)"), ["l1"]);
        assert_eq!(find(PAGE, "div:not([class~=big])"), ["d2"]);
        assert_eq!(find(PAGE, "li:not(:not(.on))"), ["l2"]);
        assert_eq!(find(PAGE, "p:not(*)"), Vec::<String>::new());
        assert_eq!(find(PAGE, "li:not(a)"), ["l1", "l2", "l3"]);
    }

    #[test]
    fn is_and_where() {
        assert_eq!(find(PAGE, "li:is(.on, :last-child)"), ["l2", "l3"]);
        assert_eq!(find(PAGE, "li:where(.on, :last-child)"), ["l2", "l3"]);
        assert_eq!(find(PAGE, ":is(p, li).on"), ["l2"]);
        assert_eq!(find(PAGE, "li:is(:not(.on))"), ["l1", "l3"]);
        assert_eq!(find(PAGE, "li:is(:hover)"), Vec::<String>::new());
    }

    #[test]
    fn nodes_that_are_not_elements_never_match() {
        let mut doc = parse("<p id=a>text</p>");
        let list = parse_selector_list("*, :not(p), :empty", &mut doc).unwrap();
        assert_eq!(list.len(), 3);
        let p = doc.children(doc.children(doc.children(doc.root()).next().unwrap()).last().unwrap()).next().unwrap();
        // The document, then `html`, then its last child `body`, then the first child of `body`: the `p`.
        let t = doc.children(p).next().unwrap();
        assert!(matches!(doc.data(t), NodeData::Text(_)));
        assert!(!list.iter().any(|s| matches(&doc, t, s)));
        assert!(!list.iter().any(|s| matches(&doc, doc.root(), s)));
    }

    #[test]
    fn deep_trees_do_not_overflow() {
        // 20,000 nested divs, made by hand (the tree builder is slow for a tree this deep, and this test is not about it).
        // The descendant walk is a loop, and the selector has a few compounds, so the stack is small.
        let mut doc = Document::new();
        let mut el = doc.root();
        for _ in 0..20_000 {
            el = doc.insert_element(el, None, "div", &[] as &[(&str, &str)]);
        }
        let p = doc.insert_element(el, None, "p", &[] as &[(&str, &str)]);
        let list = parse_selector_list("div div div p, section p", &mut doc).unwrap();
        assert!(matches(&doc, p, &list[0]));
        assert!(!matches(&doc, p, &list[1]));
    }
}
