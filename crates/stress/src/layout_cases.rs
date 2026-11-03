//! The scaling cases for layout. Each one builds a page of size `n`, with its DOM and styles (not timed), and times layout.

use crate::Case;
use bare_css::cascade::{Origin, RuleSet, Styles, UA_CSS};
use bare_html::dom::{Document, NodeId};
use bare_html::tree;
use bare_layout::boxes::{Child, Kind, Tree};
use bare_layout::text::{self, Flow, Transform, WhiteSpace};
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

/// Time the cut of one text into pieces, with their widths.
fn pieces(text: String, ws: WhiteSpace, transform: Transform) -> Work {
    Box::new(move || {
        let mut flow = Flow::new();
        black_box(text::pieces(&text, ws, transform, &mut flow).count());
    })
}

const NORMAL: WhiteSpace = WhiteSpace { collapse: true, newlines: false, wrap: true };
const PRE: WhiteSpace = WhiteSpace { collapse: false, newlines: true, wrap: false };

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
        // Topic 2: text
        case("text-words", 100_000, |n| pieces("hello world ".repeat(n), NORMAL, Transform::None)),
        case("text-one-long-word", 1_000_000, |n| pieces("x".repeat(n), NORMAL, Transform::None)),
        case("text-only-spaces", 1_000_000, |n| pieces(" \n\t".repeat(n), NORMAL, Transform::None)),
        case("text-pre-spaces", 1_000_000, |n| pieces(" ".repeat(n), PRE, Transform::None)),
        case("text-pre-tabs", 1_000_000, |n| pieces("\t\n".repeat(n), PRE, Transform::None)),
        case("text-one-letter-words", 1_000_000, |n| pieces("a ".repeat(n), NORMAL, Transform::None)),
        case("text-accents", 100_000, |n| pieces("caf\u{e9} e\u{301}t\u{e9} ".repeat(n), NORMAL, Transform::None)),
        case("text-chinese", 100_000, |n| pieces("\u{4e2d}\u{6587}\u{5b57} ".repeat(n), NORMAL, Transform::None)),
        case("text-uppercase", 100_000, |n| pieces("stra\u{df}e ".repeat(n), NORMAL, Transform::Upper)),
    ]
}
