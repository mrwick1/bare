//! The split lists of the rule index (`Big`): the rules that an element tests are found another way, and the rules that match must not change.
//! The tests build random trees and random sheets, and compare the matched lists with the lists of every rule tested one by one (`matches`).

use super::*;
use bare_html::tree::parse;

const VIEW: (f32, f32) = (1000.0, 500.0);
/// The lengths over which a list is split: never, always, and a few small ones that split some lists and not others.
const FROM: [usize; 5] = [usize::MAX, 0, 1, 2, 5];

struct Rng(u64);

impl Rng {
    fn next(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }

    fn pick<'a>(&mut self, list: &[&'a str]) -> &'a str {
        list[self.next(list.len())]
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.next(100) < percent
    }
}

const TAGS: [&str; 6] = ["div", "span", "p", "a", "ul", "li"];
const CLASSES: [&str; 5] = ["a", "b", "c", "d", "e"];

fn compound(r: &mut Rng) -> String {
    let mut s = String::new();
    if r.chance(50) {
        s += r.pick(&TAGS);
    }
    for _ in 0..r.next(3) {
        s += &format!(".{}", r.pick(&CLASSES));
    }
    if r.chance(12) {
        s += &format!("#i{}", r.next(3));
    }
    if r.chance(8) {
        s += r.pick(&[":first-child", ":last-child", ":nth-child(2)", "[title]", "[title=x]"]);
    }
    if r.chance(8) {
        s += &format!(":not(.{})", r.pick(&CLASSES));
    }
    if r.chance(8) {
        s += &format!(":is(.{}, .{})", r.pick(&CLASSES), r.pick(&CLASSES));
    }
    if r.chance(5) {
        s += &format!(":is({} > .{}, p)", r.pick(&TAGS), r.pick(&CLASSES));
    }
    if r.chance(3) {
        s += &format!(":has(.{})", r.pick(&CLASSES));
    }
    if s.is_empty() { "*".into() } else { s }
}

fn selector(r: &mut Rng) -> String {
    let mut s = compound(r);
    for _ in 0..r.next(5) {
        let comb = match r.next(100) {
            0..40 => " ",
            40..70 => " > ",
            70..85 => " + ",
            _ => " ~ ",
        };
        s = format!("{}{comb}{s}", compound(r));
    }
    s
}

/// `n` rules. Rule `i` sets `margin-left` to `i + 1` pixels, and one more property by `i % 3`, so that the winner shows which rules matched.
fn sheet(r: &mut Rng, n: usize, host: bool) -> String {
    (0..n)
        .map(|i| {
            let mut sel = selector(r);
            if r.chance(10) {
                sel = format!("{sel}, {}", selector(r));
            }
            if host && r.chance(10) {
                sel = ":host(.a)".into();
            }
            let other = ["padding-top", "padding-right", "padding-bottom"][i % 3];
            format!("{sel} {{ margin-left: {}px; {other}: {}px }}\n", i + 1, 1 + i / 3)
        })
        .collect()
}

fn element(r: &mut Rng, depth: usize, out: &mut String) {
    let tag = r.pick(&TAGS);
    out.push_str(&format!("<{tag}"));
    let classes: Vec<&str> = (0..r.next(4)).map(|_| r.pick(&CLASSES)).collect();
    if !classes.is_empty() {
        out.push_str(&format!(" class='{}'", classes.join(" ")));
    }
    if r.chance(15) {
        out.push_str(&format!(" id=i{}", r.next(3)));
    }
    if r.chance(10) {
        out.push_str(&format!(" title={}", r.pick(&["x", "y"])));
    }
    out.push('>');
    if depth < 6 {
        for _ in 0..r.next(4) {
            if r.chance(10) {
                out.push_str("text");
            } else {
                element(r, depth + 1, out);
            }
        }
    }
    out.push_str(&format!("</{tag}>"));
}

fn page(r: &mut Rng) -> String {
    let mut html = String::new();
    for _ in 0..1 + r.next(4) {
        element(r, 0, &mut html);
    }
    html
}

/// The elements of the tree in order, the shadow trees too.
fn all(doc: &Document) -> Vec<NodeId> {
    let (mut out, mut stack) = (Vec::new(), vec![doc.root()]);
    while let Some(n) = stack.pop() {
        if matches!(doc.data(n), NodeData::Element { .. }) {
            out.push(n);
        }
        let kids: Vec<NodeId> = doc.shadow_root(n).into_iter().chain(Some(n)).flat_map(|p| doc.children(p)).collect();
        stack.extend(kids.into_iter().rev());
    }
    out
}

/// Make the lists over `from` split, in the document and in every shadow tree.
fn split_from(rules: &mut RuleSet, from: usize) {
    for index in std::iter::once(&mut rules.index).chain(rules.shadows.values_mut().map(|t| &mut t.index)) {
        index.split_from = from;
        index.big.take();
    }
}

fn rules_of(doc: &mut Document, sheets: &[&str], from: usize) -> RuleSet {
    let mut rules = RuleSet::new(VIEW);
    for s in sheets {
        rules.add(doc, s, Origin::Author);
    }
    split_from(&mut rules, from);
    rules
}

/// The rules that the cascade finds for each element: the candidates of the index, the ancestor filter, then `matches`. As `walk` does
/// for a page without shadow trees. It checks each element on the way:
/// - the list is the one that every rule of the document gives with the filter and `matches` (the lists before the split);
/// - every rule that matches, with no filter, is among the candidates (a key is never an element that is not an ancestor).
fn found(rules: &RuleSet, doc: &Document) -> Vec<(NodeId, Vec<u32>)> {
    let mut filter = AncestorFilter::new(rules.tracks());
    let (mut out, mut candidates) = (Vec::new(), Vec::new());
    let mut stack = vec![(doc.root(), false)];
    while let Some((node, leaving)) = stack.pop() {
        if !matches!(doc.data(node), NodeData::Element { .. }) {
            stack.extend(doc.children(node).collect::<Vec<_>>().into_iter().rev().map(|c| (c, false)));
            continue;
        }
        if leaving {
            filter.remove_element(doc, node);
            continue;
        }
        rules.candidates(doc, node, &mut candidates, &filter);
        // A rule is offered once, however many ancestors have its key (an element that repeats a class is the only way to get one twice).
        let tokens: Vec<&str> = doc.attr(node, Atom::CLASS).unwrap_or("").split_ascii_whitespace().collect();
        let (mut once, mut distinct) = (candidates.clone(), tokens.clone());
        once.sort_unstable();
        once.dedup();
        distinct.sort_unstable();
        distinct.dedup();
        assert!(once.len() == candidates.len() || distinct.len() < tokens.len(), "element {} is offered a rule twice", node.index());
        let possible = |i: u32| rules.entries[i as usize].ancestors.iter().take_while(|&&h| h != 0).all(|&h| filter.may_contain(h));
        let mut matched: Vec<u32> = candidates.iter().copied().filter(|&i| possible(i) && matches(doc, node, &rules.entries[i as usize].selector)).collect();
        // An element with a class twice (`class="a a"`) is offered the list of that class twice, in the index as it was before the split too.
        // The rule is the same, so the lists are compared without the repeats.
        matched.sort_unstable();
        matched.dedup();
        let all = || (0..rules.entries.len() as u32).filter(|&i| rules.entries[i as usize].ctx == OWN);
        let before: Vec<u32> = all().filter(|&i| possible(i) && matches(doc, node, &rules.entries[i as usize].selector)).collect();
        assert_eq!(matched, before, "element {}", node.index());
        for i in all().filter(|&i| matches(doc, node, &rules.entries[i as usize].selector)) {
            assert!(candidates.contains(&i), "rule {i} matches element {} and is not offered", node.index());
        }
        out.push((node, matched));
        filter.add_element(doc, node);
        stack.push((node, true));
        stack.extend(doc.children(node).collect::<Vec<_>>().into_iter().rev().map(|c| (c, false)));
    }
    out
}

fn check_flat(html: &str, sheet: &str, label: &str) {
    let mut doc = parse(html);
    for from in FROM {
        let rules = rules_of(&mut doc, &[sheet], from);
        found(&rules, &doc);
    }
    let _ = label;
}

#[test]
fn random_trees_and_sheets_match_the_same_rules_with_every_split() {
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    let mut split = 0;
    for i in 0..400 {
        let html = page(&mut r);
        let rules = 1 + r.next(60);
        let css = sheet(&mut r, rules, false);
        check_flat(&html, &css, &format!("case {i}"));
        let set = rules_of(&mut parse(&html), &[&css], 2);
        split += usize::from(set.index.bigs(&set.entries).is_some());
    }
    assert!(split > 100, "only {split} of 400 sheets have a split list: the test does not test the split");
}

#[test]
fn a_sibling_of_an_ancestor_is_not_a_key() {
    // `.a` is before the parent of `p`, not above it: it must not be the key of the rule, or the rule would not be offered to `p`.
    check_flat("<div class=a></div><div class=b><p class=c id=p>x</p></div>", ".a ~ .b > .c { margin-left: 1px } .a ~ .b .c, .a + .b p { margin-left: 2px }", "sibling of the parent");
    // The ancestor filter still takes `.a` as an ancestor, so these rules do not match (as before the split: `found` compares with that list).
    let mut doc = parse("<div class=a></div><div class=b><p class=c id=p>x</p></div>");
    let rules = rules_of(&mut doc, &[".a ~ .b > .c { margin-left: 1px } .a + .b p { margin-left: 3px } .b > .c { margin-left: 4px }"], 0);
    assert!(found(&rules, &doc).iter().any(|(_, m)| m == &[2]));
}

#[test]
fn deep_paths_and_repeated_classes_match_once() {
    // 200 nested divs, all with the classes `a` and `k`, and every tenth with `b`: the same hash is on the path 200 times.
    let mut html = String::new();
    for i in 0..200 {
        html += &format!("<div class='a k{}'>", if i % 10 == 0 { " b" } else { "" });
    }
    html += "<p class=c id=p>x</p><span class=c>y</span>";
    html += &"</div>".repeat(200);
    let rules: String = (0..40).map(|i| format!(".{} .c {{ margin-left: {i}px }} .b > .k > .c {{ margin-left: {i}px }} ", ["a", "b", "k", "d"][i % 4])).collect();
    check_flat(&html, &rules, "deep");
    let mut doc = parse(&html);
    let rules = rules_of(&mut doc, &[&rules], 1);
    found(&rules, &doc);
}

#[test]
fn a_filter_with_a_saturated_counter_still_finds_the_path() {
    // More than 255 elements with the same class: the counter of the filter stays at 255, the path keeps its own counts.
    let mut html = String::new();
    for _ in 0..300 {
        html += "<div class=a>";
    }
    html += "<p class=c>x</p>";
    html += &"</div>".repeat(300);
    html += "<p class=c>y</p>";
    check_flat(&html, &".a .c { margin-left: 1px } div > .c { margin-left: 2px } .z .c { margin-left: 3px }".repeat(1), "saturated");
}

#[test]
fn shadow_trees_give_the_same_styles_with_every_split() {
    let mut r = Rng(0xdead_beef_1234_5678);
    for i in 0..150 {
        let (light, shadow) = (page(&mut r), format!("{}<slot></slot>", page(&mut r)));
        let (n, m) = (1 + r.next(40), 1 + r.next(40));
        let (doc_css, shadow_css) = (sheet(&mut r, n, false), sheet(&mut r, m, true));
        let mut reference: Option<Vec<Option<ComputedStyle>>> = None;
        for from in FROM {
            let mut doc = parse(&format!("<body><x-a class=a>{light}</x-a><p id=out>out</p></body>"));
            let host = all(&doc).into_iter().find(|&n| doc.atom_str(match doc.data(n) { NodeData::Element { name, .. } => *name, _ => unreachable!() }) == "x-a").unwrap();
            let root = doc.attach_shadow(host, true);
            let frag = parse(&format!("<body>{shadow}</body>"));
            let body = frag.children(frag.children(frag.root()).next().unwrap()).last().unwrap();
            let mut stack = vec![(body, root)];
            while let Some((from_node, to)) = stack.pop() {
                for c in frag.children(from_node).collect::<Vec<_>>() {
                    let n = doc.import_node(&frag, c).unwrap();
                    doc.move_to(n, to, None);
                    stack.push((c, n));
                }
            }
            let mut rules = RuleSet::new(VIEW);
            rules.add(&mut doc, UA_CSS, Origin::UserAgent);
            rules.add(&mut doc, &doc_css, Origin::Author);
            rules.add_scoped(&mut doc, &shadow_css, Origin::Author, None, root);
            split_from(&mut rules, from);
            let styles = rules.compute(&doc);
            let now: Vec<Option<ComputedStyle>> = all(&doc).into_iter().map(|n| styles.style(n).cloned()).collect();
            match &reference {
                None => reference = Some(now),
                Some(first) => assert!(*first == now, "case {i}: split from {from} gives other styles\n{light}\n{shadow}\n{doc_css}\n{shadow_css}"),
            }
        }
    }
}

#[test]
fn a_restyle_gives_the_styles_of_a_new_compute_with_every_split() {
    let mut r = Rng(0x0123_4567_89ab_cdef);
    for i in 0..150 {
        let html = page(&mut r);
        let n = 1 + r.next(50);
        let css = sheet(&mut r, n, false);
        let mut doc = parse(&html);
        let mut sets: Vec<RuleSet> = FROM.iter().map(|&from| rules_of(&mut doc, &[&css], from)).collect();
        let mut kept: Vec<Styles> = sets.iter().map(|s| s.compute_kept(&doc)).collect();
        let elements = all(&doc);
        for _ in 0..1 + r.next(4) {
            let el = elements[r.next(elements.len())];
            doc.set_attr(el, "class", &format!("{} {}", r.pick(&CLASSES), r.pick(&CLASSES)));
        }
        let html_el = doc.children(doc.root()).find(|&n| matches!(doc.data(n), NodeData::Element { .. })).unwrap();
        let fresh = sets[0].compute(&doc);
        let want: Vec<Option<ComputedStyle>> = elements.iter().map(|&n| fresh.style(n).cloned()).collect();
        for (k, (set, styles)) in sets.iter_mut().zip(&mut kept).enumerate() {
            if set.restyle(&doc, styles, &[(html_el, true)], &mut Vec::new()) {
                let got: Vec<Option<ComputedStyle>> = elements.iter().map(|&n| styles.style(n).cloned()).collect();
                assert!(got == want, "case {i}: restyle with split from {} gives other styles\n{html}\n{css}", FROM[k]);
            }
        }
    }
}

fn with_ua(doc: &mut Document, css: &str, from: usize) -> RuleSet {
    let mut rules = RuleSet::new(VIEW);
    rules.add(doc, UA_CSS, Origin::UserAgent);
    rules.add(doc, css, Origin::Author);
    split_from(&mut rules, from);
    rules
}

#[test]
fn rules_built_for_one_document_give_another_document_the_matches_and_styles_of_its_own_rules() {
    // `rule_cache` shares one set between documents. The set holds atoms, so it must not depend on the names that its document had.
    let mut r = Rng(0x5eed_0001_5eed_0002);
    for i in 0..200 {
        let n = 1 + r.next(50);
        let css = sheet(&mut r, n, false);
        // The builder interns other names first, in another order, so that its numbers differ from the numbers of the others.
        let mut builder = parse(&page(&mut r));
        for name in ["zz-first", "li", "p", "title", "x-custom", "span"] {
            builder.intern(name);
        }
        let from = FROM[r.next(FROM.len())];
        let shared = with_ua(&mut builder, &css, from);
        for _ in 0..3 {
            let html = page(&mut r);
            let mut doc = parse(&html);
            let own = with_ua(&mut doc, &css, from);
            assert_eq!(found(&shared, &doc), found(&own, &doc), "case {i}\n{html}\n{css}");
            let (a, b) = (shared.compute(&doc), own.compute(&doc));
            for el in all(&doc) {
                assert!(a.style(el) == b.style(el), "case {i}: element {} has another style\n{html}\n{css}", el.index());
            }
        }
    }
}
