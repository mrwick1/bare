//! The html5lib tree-construction tests for `<template>`: `tools/fixtures/html/template.dat` (MIT license: `html5lib-tests-LICENSE`).
//!
//! The html5lib project moved these tests to web-platform-tests in June 2026. The file is the last copy in its own repository
//! (commit 224991e^, 112 tests). A test is a page or a fragment with a context, and the tree that every browser must make.
//! `Document::dump_with_contents` writes a tree in the same text format, so the check is a text compare.
//!
//! `tools/fixtures/html/table-fragments.dat` holds the 91 fragment tests of html5lib-tests whose context is a table element (`table`, `thead`,
//! `tbody`, `tfoot`, `tr`, `td`, `caption`, `colgroup`), in file order: 66 from `tests_innerHTML_1.dat`, 8 from `math.dat`, 8 from `svg.dat` and 9 from
//! `tests6.dat`. The source is the copy in the Go module `golang.org/x/net` v0.62.0 (`html/testdata/html5lib-tests/tree-construction`).
//!
//! `tools/fixtures/html/foreign-fragments.dat` holds the 63 fragment tests of html5lib-tests whose context is an SVG or MathML element (`svg path`,
//! `math mi`), all from `foreign-fragment.dat`, in file order. The source is the same Go module.

use super::*;
use crate::dom::Ns;

const TEMPLATE_DAT: &str = include_str!("../../../../tools/fixtures/html/template.dat");
const TABLE_FRAGMENTS_DAT: &str = include_str!("../../../../tools/fixtures/html/table-fragments.dat");
const FOREIGN_FRAGMENTS_DAT: &str = include_str!("../../../../tools/fixtures/html/foreign-fragments.dat");

/// One test of a `.dat` file.
struct Case {
    data: String,
    context: Option<String>,
    scripting: bool,
    expected: String,
}

/// Cut a `.dat` file into tests. A test starts at a `#data` line. The `#errors` list is not used: only the tree is checked.
fn cases(dat: &str) -> Vec<Case> {
    let lines: Vec<&str> = dat.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i] != "#data" {
            i += 1;
            continue;
        }
        let end = (i + 1..lines.len()).find(|&j| lines[j] == "#data").unwrap_or(lines.len());
        let mut section = "";
        let (mut data, mut expected) = (Vec::new(), Vec::new());
        let (mut context, mut scripting) = (None, true);
        for &line in &lines[i + 1..end] {
            match line {
                "#errors" | "#new-errors" | "#document-fragment" | "#document" | "#script-on" | "#script-off" => {
                    section = line;
                    scripting &= line != "#script-off";
                }
                _ => match section {
                    "" => data.push(line),
                    "#document-fragment" => context = Some(line.to_string()),
                    "#document" => expected.push(line),
                    _ => {}
                },
            }
        }
        // The blank line before the next `#data` belongs to the file, not to the tree.
        while expected.last() == Some(&"") {
            expected.pop();
        }
        // The data keeps its inner newlines. The newline at its end is not part of it.
        out.push(Case { data: data.join("\n"), context, scripting, expected: expected.iter().map(|l| format!("{l}\n")).collect() });
        i = end;
    }
    out
}

/// The tree that we make for a case, in the html5lib text format.
fn tree_of(case: &Case) -> String {
    match &case.context {
        None => parse(&case.data).dump_with_contents(),
        Some(context) => {
            let lim = FragmentLimits { max_source: 1 << 20, max_nodes: 1 << 16, max_text_bytes: 1 << 20, max_attr_slots: 1 << 16, max_names: 1 << 12, max_depth: 512 };
            // A foreign context is written `svg path` or `math mi`.
            let (ns, context) = match context.split_once(' ') {
                Some(("svg", name)) => (Ns::Svg, name),
                Some(("math", name)) => (Ns::MathMl, name),
                _ => (Ns::Html, context.as_str()),
            };
            match parse_fragment(&case.data, context, ns, false, case.scripting, &lim, &mut |_| true) {
                // The root `html` line goes, and the children move one level up.
                Ok(f) => f.doc.dump_with_contents().lines().skip(1).map(|l| format!("| {}\n", &l[4..])).collect(),
                Err(e) => format!("{e:?}"),
            }
        }
    }
}

/// The tests that fail, as (number, text of the failure).
fn failures(dat: &str) -> Vec<(usize, String)> {
    let all = cases(dat);
    assert!(!all.is_empty());
    let mut bad = Vec::new();
    for (n, case) in all.iter().enumerate() {
        let got = tree_of(case);
        if got != case.expected {
            bad.push((n + 1, format!("#{} {:?} ({:?})\n--- expected\n{}--- got\n{}", n + 1, case.data, case.context, case.expected, got)));
        }
    }
    bad
}

/// The tests that fail on purpose, because the tree builder has no frameset modes and no frameset-ok flag (see the header of `tree.rs`).
/// 40 needs the "in frameset" mode. 42 and 43 need the flag: `<template>` sets it to "not ok", and then a `<frameset>` start tag is ignored.
/// The check fails when one of these passes, so that the list does not go stale.
const FRAMESET: [usize; 3] = [40, 42, 43];

#[test]
fn the_runner_reads_all_112_template_tests() {
    assert_eq!(cases(TEMPLATE_DAT).len(), 112);
    assert_eq!(cases(TEMPLATE_DAT).iter().filter(|c| c.context.is_some()).count(), 1);
}

#[test]
fn html5lib_template_dat() {
    let bad = failures(TEMPLATE_DAT);
    let unexpected: Vec<_> = bad.iter().filter(|(n, _)| !FRAMESET.contains(n)).map(|(_, text)| text.as_str()).collect();
    assert!(unexpected.is_empty(), "{} of 112 tests fail:\n{}", unexpected.len(), unexpected.join("\n"));
    let numbers: Vec<_> = bad.iter().map(|(n, _)| *n).collect();
    assert_eq!(numbers, FRAMESET, "a frameset test passes now: take it off the list");
}

#[test]
fn html5lib_table_context_fragments() {
    assert_eq!(cases(TABLE_FRAGMENTS_DAT).len(), 91);
    let bad = failures(TABLE_FRAGMENTS_DAT);
    let text: Vec<_> = bad.iter().map(|(_, text)| text.as_str()).collect();
    assert!(bad.is_empty(), "{} of 91 tests fail:\n{}", bad.len(), text.join("\n"));
}

#[test]
fn html5lib_foreign_context_fragments() {
    // 52 is `<frameset>` in a fragment: the fragment parser refuses that start tag on purpose (see the header of `fragment.rs`).
    assert_eq!(cases(FOREIGN_FRAGMENTS_DAT).len(), 63);
    let bad = failures(FOREIGN_FRAGMENTS_DAT);
    let unexpected: Vec<_> = bad.iter().filter(|(n, _)| *n != 52).map(|(_, text)| text.as_str()).collect();
    assert!(unexpected.is_empty(), "{} of 63 tests fail:\n{}", unexpected.len(), unexpected.join("\n"));
    assert_eq!(bad.iter().map(|(n, _)| *n).collect::<Vec<_>>(), [52], "the frameset test passes now: take it off");
}
