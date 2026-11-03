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

/// Time the whole layout of a page.
fn lay(page: String, sheet: &str) -> Work {
    let (doc, styles) = styled(&page, sheet);
    Box::new(move || {
        black_box(bare_layout::layout(&doc, &styles, (1280.0, 800.0)));
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
        // Topic 2: text
        case("text-words", 100_000, |n| pieces("hello world ".repeat(n), NORMAL, Transform::None)),
        case("text-one-long-word", 1_000_000, |n| pieces("x".repeat(n), NORMAL, Transform::None)),
        case("text-only-spaces", 1_000_000, |n| pieces(" \n\t".repeat(n), NORMAL, Transform::None)),
        case("text-pre-spaces", 1_000_000, |n| pieces(" ".repeat(n), PRE, Transform::None)),
        case("text-pre-tabs", 1_000_000, |n| pieces("\t\n".repeat(n), PRE, Transform::None)),
        case("text-one-letter-words", 1_000_000, |n| pieces("a ".repeat(n), NORMAL, Transform::None)),
        case("text-accents", 100_000, |n| pieces("caf\u{e9} e\u{301}t\u{e9} ".repeat(n), NORMAL, Transform::None)),
        case("text-chinese", 100_000, |n| pieces("\u{4e2d}\u{6587}\u{5b57} ".repeat(n), NORMAL, Transform::None)),
        // Topic 3: block layout
        case("block-wide", 20_000, |n| lay("<div></div>".repeat(n), "div { height: 1px; margin: 3px }")),
        case("block-deep", 40, |n| lay("<div>".repeat(n), "div { padding: 1px; margin: 1px }")),
        case("block-deep-over-limit", 2_000, |n| lay("<div>".repeat(n), "")),
        case("block-collapse-through", 20_000, |n| lay("<div></div>".repeat(n), "div { margin: 5px 0 }")),
        case("block-nested-collapse", 20, |n| lay(format!("{}x{}", "<div>".repeat(n), "</div>".repeat(n)), "div { margin: 2px 0 }")),
        case("block-percent-calc", 20_000, |n| lay("<div></div>".repeat(n), "div { width: calc(50% - 3px); padding: 1%; margin: 0 auto; max-width: 70%; height: 2px }")),
        case("block-list-items", 20_000, |n| lay(format!("<ol>{}</ol>", "<li><div></div>".repeat(n)), "")),
        // Topic 4: inline layout
        case("inline-paragraph", 20_000, |n| lay(format!("<p>{}</p>", "word ".repeat(n)), "p { width: 300px }")),
        case("inline-one-line-nowrap", 20_000, |n| lay(format!("<p>{}</p>", "word ".repeat(n)), "p { white-space: nowrap }")),
        case("inline-long-word", 200_000, |n| lay(format!("<p>{}</p>", "x".repeat(n)), "p { width: 100px }")),
        case("inline-many-spans", 20_000, |n| lay(format!("<p>{}</p>", "<b>a</b> <i>b</i> ".repeat(n)), "")),
        case("inline-span-unit", 20_000, |n| lay(format!("<p>{}</p>", "<b>a</b>".repeat(n)), "p { width: 100px }")),
        case("inline-deep-spans", 2_000, |n| lay(format!("{}x", "<span>".repeat(n)), "")),
        case("inline-deep-spans-many-lines", 100, |n| lay(format!("{}{}", "<span>".repeat(n), "x ".repeat(n)), "body { width: 20px }")),
        case("inline-breaks", 20_000, |n| lay(format!("<p>{}</p>", "a<br>".repeat(n)), "")),
        case("inline-pre-lines", 20_000, |n| lay(format!("<pre>{}</pre>", "a\tb\n".repeat(n)), "")),
        case("inline-blocks-many", 20_000, |n| lay(format!("<p>{}</p>", "<i>ab cd</i> ".repeat(n)), "i { display: inline-block }")),
        case("inline-blocks-nested", 40, |n| lay(format!("{}x", "<i>a ".repeat(n)), "i { display: inline-block }")),
        case("inline-block-in-inline", 20_000, |n| lay(format!("<span>{}</span>", "a<div>b</div>".repeat(n)), "")),
        case("inline-list", 20_000, |n| lay(format!("<ul>{}</ul>", "<li>item".repeat(n)), "")),
        // Floats
        case("floats-stack", 2_000, |n| lay("<div></div>".repeat(n), "div { float: left; width: 10px; height: 10px } body { width: 300px }")),
        case("floats-one-tall", 20_000, |n| lay(format!("<div></div><p>{}</p>", "word ".repeat(n)), "div { float: left; width: 50px; height: 100000000px } p { width: 300px }")),
        case("floats-with-paragraphs", 2_000, |n| lay("<div></div><p>aaa bbb ccc ddd eee fff ggg</p>".repeat(n), "div { float: left; width: 50px; height: 30px } body { width: 300px }")),
        case("floats-alternate-clear", 2_000, |n| lay("<div></div><p>x</p>".repeat(n), "div { float: right; width: 50px; height: 30px } p { clear: both }")),
        case("floats-in-a-line", 2_000, |n| lay(format!("<p>{}</p>", "aa <i>f</i> bb ".repeat(n)), "i { float: left } p { width: 300px }")),
        case("floats-new-contexts", 2_000, |n| lay("<div></div><section>x</section>".repeat(n), "div { float: left; width: 50px; height: 30px } section { display: flow-root }")),
        // Overflow
        case("overflow-many-auto", 2_000, |n| lay("<div><p>aaa bbb ccc</p></div>".repeat(n), "div { overflow: auto; height: 10px; width: 100px }")),
        case("overflow-nested-auto", 20, |n| lay(format!("{}<p>x</p>", "<div>".repeat(n)), "div { overflow: auto; max-height: 50px; padding-left: 1px } p { height: 300px }")),
        case("overflow-auto-fits", 2_000, |n| lay("<div><p>aaa bbb ccc</p></div>".repeat(n), "div { overflow: auto; width: 100px }")),
        case("overflow-long-log", 20_000, |n| lay(format!("<div>{}</div>", "<p>a line of the log</p>".repeat(n)), "div { overflow: auto; height: 400px }")),
        case("overflow-wide-content", 2_000, |n| lay(format!("<div><p>{}</p></div>", "word ".repeat(n)), "div { overflow-x: auto; width: 100px } p { white-space: nowrap }")),
        // Position
        case("position-many-absolute", 2_000, |n| lay(format!("<div>{}</div>", "<p>x</p><i></i>".repeat(n)), "div { position: relative } i { position: absolute; top: 0; left: 5px; width: 3px; height: 3px }")),
        case("position-absolute-in-each-relative", 2_000, |n| lay("<div><i>a</i>b</div>".repeat(n), "div { position: relative } i { position: absolute; right: 0; bottom: 0 }")),
        case("position-nested-relative", 300, |n| lay(format!("{}<i></i>", "<div>".repeat(n)), "div { position: relative; padding-left: 1px } i { position: absolute }")),
        case("position-fixed-deep", 300, |n| lay(format!("{}<i></i>", "<div>".repeat(n)), "div { position: relative } i { position: fixed; bottom: 0 }")),
        case("position-relative-spans", 2_000, |n| lay(format!("<p>{}</p>", "<span>a</span> b ".repeat(n)), "span { position: relative; top: 1px }")),
        case("position-shrink-to-fit", 2_000, |n| lay("<div><i>some words here</i></div>".repeat(n), "div { position: relative } i { position: absolute; left: 0 }")),
        case("table-many-rows", 20_000, |n| lay(format!("<table>{}</table>", "<tr><td>a cell<td>another cell<td>last".repeat(n)), "")),
        case("table-many-columns", 5_000, |n| lay(format!("<table><tr>{}</table>", "<td>cell text".repeat(n)), "")),
        case("table-many-tables", 2_000, |n| lay("<table><tr><td>a<td>b c<tr><td>d<td>e</table>".repeat(n), "")),
        case("table-fixed-layout", 20_000, |n| lay(format!("<table>{}</table>", "<tr><td>a cell<td>another cell<td>last".repeat(n)), "table { table-layout: fixed; width: 600px }")),
        case("table-colspan-chain", 1_000, |n| lay(format!("<table>{}</table>", (1..=n).map(|i| format!("<tr><td colspan={i}>span of {i}<td>x")).collect::<String>()), "")),
        case("table-rowspan-grid", 5_000, |n| lay(format!("<table>{}</table>", "<tr><td rowspan=3>tall<td>a<td>b<tr><td>c<td>d<tr><td>e<td>f".repeat(n)), "")),
        case("table-nested", 150, |n| lay(format!("{}x{}", "<table><tr><td>".repeat(n), "</table>".repeat(n)), "")),
        case("table-collapse", 10_000, |n| lay(format!("<table>{}</table>", "<tr><td>a cell<td>another cell<td>last".repeat(n)), "table { border-collapse: collapse } td { border: 1px solid }")),
        case("table-percent-columns", 10_000, |n| lay(format!("<table style='width:90%'>{}</table>", "<tr><td style='width:20%'>a<td style='width:30%'>b<td>c".repeat(n)), "")),
        case("text-uppercase", 100_000, |n| pieces("stra\u{df}e ".repeat(n), NORMAL, Transform::Upper)),
    ]
}
