//! The scaling cases for layout. Each one builds a page of size `n`, with its DOM and styles (not timed), and times layout.

use crate::Case;
use bare_css::cascade::{Origin, RuleSet, Styles, UA_CSS};
use bare_html::dom::{Document, NodeId};
use bare_html::tree;
use bare_layout::boxes::{Child, Kind, Tree};
use std::hint::black_box;

type Work = Box<dyn FnOnce()>;

fn case(name: &'static str, base: usize, build: fn(usize) -> Work) -> Case {
    Case { name, base, build, single: false }
}

/// The DOM and the styles of a page, with the browser sheet and one author sheet.
fn styled(page: &str, sheet: &str) -> (Document, Styles) {
    let mut doc = tree::parse(page);
    let mut rules = RuleSet::new((1280.0, 800.0));
    rules.add(&mut doc, UA_CSS, Origin::UserAgent);
    rules.add(&mut doc, sheet, Origin::Author);
    let styles = rules.compute(&doc);
    (doc, styles)
}

/// Visit every box of the box tree (`boxes.rs`), and count them.
fn count_boxes(tree: &Tree) -> usize {
    let Some(root) = tree.root() else { return 0 };
    let mut count = 0;
    let mut blocks = vec![root];
    let mut inline: Vec<NodeId> = Vec::new();
    while let Some(block) = blocks.pop() {
        count += 1;
        for child in tree.children(block) {
            match child {
                Child::Block(b) => blocks.push(b),
                Child::Lines(first, last) => {
                    let mut n = Some(first);
                    while let Some(node) = n {
                        inline.push(node);
                        n = if node == last { None } else { tree.doc.next_sibling(node) };
                    }
                }
            }
        }
        while let Some(node) = inline.pop() {
            count += 1;
            match tree.kind(node) {
                Kind::Inline => inline.extend(tree.doc.children(node)),
                Kind::Atomic | Kind::Block | Kind::ListItem => blocks.push(node),
                _ => {}
            }
        }
    }
    count
}

/// Time the walk over the box tree of a page.
fn boxes(page: String, sheet: &str) -> Work {
    let (doc, styles) = styled(&page, sheet);
    Box::new(move || {
        black_box(count_boxes(&Tree::new(&doc, &styles)));
    })
}

pub fn all() -> Vec<Case> {
    vec![
        // Topic 1: the box tree
        case("boxes-wide-blocks", 20_000, |n| boxes("<div>x</div>".repeat(n), "")),
        case("boxes-deep-blocks", 2_000, |n| boxes("<div>".repeat(n), "")),
        case("boxes-deep-inline", 2_000, |n| boxes("<span>".repeat(n), "")),
        case("boxes-spaces-between", 20_000, |n| boxes("<p>x</p>\n   ".repeat(n), "")),
        case("boxes-one-long-run", 20_000, |n| boxes(format!("<div>{}<p>end</p></div>", "<i>x</i> ".repeat(n)), "")),
        case("boxes-mixed-runs", 20_000, |n| boxes(format!("<div>{}</div>", "a<p>b</p>".repeat(n)), "")),
        case("boxes-display-none", 20_000, |n| boxes("<div style=display:none><p>x</p></div>".repeat(n), "")),
        case("boxes-list-items", 20_000, |n| boxes(format!("<ol>{}</ol>", "<li>x".repeat(n)), "")),
        case("boxes-inline-blocks", 20_000, |n| boxes("<i>x</i>".repeat(n), "i { display: inline-block }")),
    ]
}
