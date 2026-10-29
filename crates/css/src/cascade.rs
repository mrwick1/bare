//! The cascade: find the style of every element.
//!
//! The steps for one element:
//! 1. Find the rules that match it (`RuleSet::compute` and `matching.rs`).
//! 2. For each property, pick the declaration that wins (the cascade).
//! 3. Turn the winners into computed values: `2em` into pixels, `inherit` into the value of the parent,
//!    no value at all into the inherited value or the starting value (inheritance).
//!
//! ## Which declaration wins
//!
//! Each declaration gets a key, and the biggest key wins:
//! 1. Importance and origin: the browser's own style sheet (UA), then the page's (author), then `!important` of the author,
//!    then `!important` of the UA. (`!important` turns the order around, so a page cannot take away what the browser needs.)
//! 2. The layer (`@layer`): a later layer wins, and no layer beats every layer. `!important` turns this order around too.
//!    The `style=""` attribute beats every layer.
//! 3. Specificity (`#a` beats `.a` beats `a`). The `style=""` attribute beats every selector.
//! 4. Order: the later rule wins.
//!
//! The four go in one `u128`: 2 bits, 32 bits, 31 bits and 31 bits (with some bits not used). We compare one number for each declaration, and we never sort.
//!
//! ## Speed and memory
//!
//! - Rule index: each rule is in one list, by the last part of its selector: an id, a class, a tag, or "other".
//!   An element only tests the rules in the lists of its own id, classes and tag, not all the rules
//!   of the style sheet. For a style sheet of 100,000 rules, this is the difference between 100,000 tests and about 10.
//! - Ancestor filter: a Bloom filter (a small table of counters) of the tags, ids and classes of all the ancestors.
//!   `.a .b p` needs an ancestor with the class `a`. If the filter says "no ancestor has `a`", the rule is out at once,
//!   and we do not walk up the tree. The filter may say "maybe" for an `a` that is not there (then we walk and find out),
//!   but it never says "no" when there is an `a`.
//! - Shared styles: two elements with the same parent style and the same matching rules have the same style, so they share one
//!   `ComputedStyle`. A page with 100,000 elements has a few hundred different styles. An element costs 4 bytes (a number in a table).
//!   Chrome does the same. This also skips all the work of the cascade for the second element and the next ones.
//!   An element with a `style=""` attribute does not use the cache.
//! - Elements with `display: none` (like `head`) do not get a style for themselves or for the elements inside. Layout skips them.
//!
//! ## What a computed style is
//!
//! An array with one `Value` for each property (8 bytes each). A computed value is never `inherit`, `initial` or `unset`.
//! A length is `Px` or `Percent` (layout needs the size of the container for a percent). `em`, `rem`, `vw`, `vh` are `Px`.
//! `currentcolor` is the color. `font-weight` is a number, and `line-height` is a number, `normal`, or `Px`.
//! A border has a width of 0 when its style is `none` or `hidden`.
//!
//! Inherited properties (a child gets the value of the parent when it has none of its own): `color`, `font-size`, `font-weight`,
//! `font-style`, `line-height`, `text-align`, `text-transform`, `white-space`, `list-style-type`, `visibility`.
//! The others start at their starting value.
//!
//! Left out:
//! - `@media`, `@supports`, `@import` and other at-rules: the rules inside are not used. (A style sheet for `print` would apply, too,
//!   if the app gave it. The app skips `<style media>` and `<link media>` that do not say `all` or `screen`.)
//! - `:visited`, `:hover`: they never match, see `selector.rs`.
//! - `larger` and `smaller` are 1.2 and 1/1.2 of the parent size. Chrome uses a table of sizes for the keywords.
//! - The parent of a `display: none` element does not change anything: the style is not shared with it.
//!
//! Comments compare Rust with JavaScript, because you know JS.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::BuildHasherDefault;

use bare_html::dom::{Atom, Document, Fnv, NodeData, NodeId};

use crate::at_rules;
use crate::calc::Linear;
use crate::color::Color;
use crate::matching::matches;
use crate::parser::{AtBody, Rule, parse_declarations, parse_stylesheet};
use crate::selector::{Combinator, Selector, Simple, parse_selector_list};
use crate::values::{Declared, Keyword, Length, Property, Side, Value, parse_declaration};
use crate::var::{self, CustomDecl, VarDecl};

/// The style sheet of the browser itself.
pub const UA_CSS: &str = include_str!("ua.css");

/// Where a style sheet comes from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Origin {
    /// The browser. `UA_CSS`.
    UserAgent,
    /// The page: `<style>`, `<link rel=stylesheet>`, and the `style=""` attributes.
    Author,
}

/// A layer: a name inside a parent layer. `@layer a.b` is the layer `b` with the parent `a`.
struct Layer {
    parent: u32,
}

/// One selector of one rule. `a, b { ... }` makes two of them, and they share the declarations.
struct Entry {
    selector: Selector,
    /// The declarations of the rule: a range of `RuleSet::decls`.
    decls: (u32, u32),
    /// The custom properties of the rule: a range of `RuleSet::customs`.
    customs: (u32, u32),
    /// The layer of the rule: a place in `RuleSet::layers`. 0 is no layer.
    layer: u32,
    origin: Origin,
    /// The hashes of up to 4 tags, ids or classes that the ancestors must have. 0 means no hash.
    ancestors: [u32; 4],
}

type Buckets<K> = HashMap<K, Vec<u32>, BuildHasherDefault<Fnv>>;
// A map from the name of an id, a class or a tag to the rules for it. The hasher is the fast one of the DOM.

/// All the rules of all the style sheets, and the index to find them.
pub struct RuleSet {
    /// The width and height of the window in pixels. `@media` reads it when a rule is added, `vw` and `vh` read it in `compute`.
    viewport: (f32, f32),
    entries: Vec<Entry>,
    /// The declarations of all rules, one after the other.
    decls: Vec<Declared>,
    /// What a value in `decls` points to: the `calc()` that mix units, and the declarations with `var()`.
    side: Side,
    /// The layers (`@layer`), in the order that we first saw them. Layer 0 is "no layer", the parent of the top layers.
    layers: Vec<Layer>,
    /// The layer for a parent and a name, to find a layer without a scan of all of them.
    layer_by_name: HashMap<(u32, Box<str>), u32>,
    /// The custom properties of all rules (`--gap: 8px`), one after the other.
    customs: Vec<CustomDecl>,
    by_id: Buckets<Box<str>>,
    by_class: Buckets<Box<str>>,
    by_tag: Buckets<Atom>,
    /// The rules that have no id, no class and no tag in the last part: `*`, `[href]`, `:first-child`.
    other: Vec<u32>,
}

impl RuleSet {
    pub fn new(viewport: (f32, f32)) -> RuleSet {
        RuleSet { viewport, entries: Vec::new(), decls: Vec::new(), side: Side::default(), layers: vec![Layer { parent: 0 }], layer_by_name: HashMap::new(), customs: Vec::new(), by_id: Buckets::default(), by_class: Buckets::default(), by_tag: Buckets::default(), other: Vec::new() }
    }

    /// The number of selectors in the set.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Read a style sheet and add its rules. Add the style sheets in the order of the page: a later rule wins a tie.
    /// The names of the selectors go in the name table of `doc`, so call this before `compute`.
    pub fn add(&mut self, doc: &mut Document, text: &str, origin: Origin) {
        self.add_in_layer(doc, text, origin, None);
    }

    /// The same, for a style sheet that is in a layer: `@import url(a.css) layer(x)`. `layer` is the name (`x`, or `x.y`).
    /// An empty name is a layer with no name.
    pub fn add_in_layer(&mut self, doc: &mut Document, text: &str, origin: Origin, layer: Option<&str>) {
        let layer = layer.map_or(Some(0), |path| self.layer_path(0, path));
        if let Some(layer) = layer {
            self.add_rules(doc, parse_stylesheet(text), origin, layer, 0);
        }
    }

    /// The layer `path` (`a` or `a.b.c`) inside the layer `parent`. It is made when it is new. `None` when a name is not good.
    fn layer_path(&mut self, mut parent: u32, path: &str) -> Option<u32> {
        if path.trim().is_empty() {
            // A layer with no name (`@layer { ... }`): each one is a new layer.
            return Some(self.new_layer(parent));
        }
        // Deeper than 32 parts is not good: it makes a path that is slow to handle.
        for (depth, name) in path.trim().split('.').enumerate() {
            if depth >= 32 || name.is_empty() || name.contains(|c: char| c.is_whitespace() || c == ',') {
                return None;
            }
            parent = match self.layer_by_name.get(&(parent, Box::from(name))) {
                Some(&i) => i,
                None => {
                    let i = self.new_layer(parent);
                    self.layer_by_name.insert((parent, name.into()), i);
                    i
                }
            };
        }
        Some(parent)
    }

    fn new_layer(&mut self, parent: u32) -> u32 {
        self.layers.push(Layer { parent });
        self.layers.len() as u32 - 1
    }

    /// The place of each layer in the order of the cascade: the later one wins. Inside a layer, the sub-layers come first,
    /// and then the rules of the layer itself, so they win. Layer 0 (no layer) comes last of all, so it wins.
    fn layer_order(&self) -> Vec<u32> {
        let mut children = vec![Vec::new(); self.layers.len()];
        for (i, l) in self.layers.iter().enumerate().skip(1) {
            children[l.parent as usize].push(i);
        }
        let mut order = vec![0; self.layers.len()];
        let (mut next, mut stack) = (0, vec![(0usize, 0usize)]);
        // A loop with a stack, not calls that call themselves: `@layer a.a.a.a...` can be very deep.
        while let Some((node, child)) = stack.pop() {
            match children[node].get(child) {
                Some(&c) => {
                    stack.push((node, child + 1));
                    stack.push((c, 0));
                }
                None => {
                    order[node] = next;
                    next += 1;
                }
            }
        }
        order
    }

    /// Add the rules. A `@media` or `@supports` block that says yes adds the rules inside it, and so does a `@layer` block (in its layer). Other at-rules add nothing:
    /// `@import` is for the app (see `imports`), and `@container` waits for layout.
    fn add_rules(&mut self, doc: &mut Document, rules: Vec<Rule<'_>>, origin: Origin, layer: u32, depth: usize) {
        for rule in rules {
            let rule = match rule {
                Rule::Style(rule) => rule,
                Rule::At(at) if &*at.name == "layer" => {
                    match at.body {
                        // `@layer a, b;` only says the order of the layers.
                        AtBody::None => {
                            for name in at.prelude.split(',') {
                                self.layer_path(layer, name);
                            }
                        }
                        AtBody::Rules(inner) if depth < 32 => {
                            if let Some(inner_layer) = self.layer_path(layer, at.prelude) {
                                self.add_rules(doc, inner, origin, inner_layer, depth + 1);
                            }
                        }
                        _ => {}
                    }
                    continue;
                }
                Rule::At(at) => {
                    let AtBody::Rules(inner) = at.body else { continue };
                    let yes = match &*at.name {
                        "media" => at_rules::media_matches(at.prelude, self.viewport),
                        "supports" => at_rules::supports_matches(doc, at.prelude),
                        _ => false,
                    };
                    // The parser stops at depth 32, so this is a safety net for the stack.
                    if yes && depth < 32 {
                        self.add_rules(doc, inner, origin, layer, depth + 1);
                    }
                    continue;
                }
            };
            // A rule with a selector that is not valid is dropped, with all its selectors (the standard says so).
            let Some(selectors) = parse_selector_list(rule.prelude, doc) else { continue };
            let (start, custom_start) = (self.decls.len(), self.customs.len());
            for d in &rule.declarations {
                if d.name.starts_with("--") {
                    self.customs.push(CustomDecl { name: (*d.name).into(), text: d.value.into(), important: d.important });
                } else {
                    parse_declaration(&d.name, d.value, d.important, &mut self.side, &mut self.decls);
                }
            }
            if self.decls.len() == start && self.customs.len() == custom_start {
                continue;
            }
            let decls = (start as u32, self.decls.len() as u32);
            let customs = (custom_start as u32, self.customs.len() as u32);
            for selector in selectors {
                self.push(selector, decls, customs, layer, origin);
            }
        }
    }

    fn push(&mut self, selector: Selector, decls: (u32, u32), customs: (u32, u32), layer: u32, origin: Origin) {
        if selector.compounds.iter().any(|c| c.simples.contains(&Simple::Never)) {
            return;
            // `a:hover` never matches. Keep it out of the index: it only costs time.
        }
        let index = self.entries.len() as u32;
        // The bucket: an id is the most specific, then a class, then a tag.
        let last = &selector.compounds[0].simples;
        if let Some(Simple::Id(id)) = last.iter().find(|s| matches!(s, Simple::Id(_))) {
            self.by_id.entry(id.clone()).or_default().push(index);
        } else if let Some(Simple::Class(class)) = last.iter().find(|s| matches!(s, Simple::Class(_))) {
            self.by_class.entry(class.clone()).or_default().push(index);
        } else if let Some(&Simple::Tag(tag)) = last.iter().find(|s| matches!(s, Simple::Tag(_))) {
            self.by_tag.entry(tag).or_default().push(index);
        } else {
            self.other.push(index);
        }
        let ancestors = ancestor_hashes(&selector);
        self.entries.push(Entry { selector, decls, customs, layer, origin, ancestors });
    }

    /// Find the style of every element that is shown. The selectors must come from `add` with this same `doc`.
    pub fn compute(&self, doc: &Document) -> Styles {
        let mut styles = Styles { styles: vec![ComputedStyle::initial()], of_node: vec![NONE; doc.len()], calcs: Vec::new(), customs: Vec::new() };
        // Style 0 is the starting style. It is the parent style of the `html` element.
        let mut cx = Cx { viewport: self.viewport, root_font_size: 16.0, layer_order: self.layer_order(), declared: &self.side, inline: RefCell::default(), computed: RefCell::default(), customs: RefCell::default() };
        let style_atom = doc.find_atom("style");
        let mut filter = AncestorFilter::new();
        let mut memo = Memo::default();
        let (mut candidates, mut matched): (Vec<u32>, Vec<u32>) = (Vec::new(), Vec::new());
        let mut stack = vec![(doc.root(), false)];
        // The walk is a loop with a stack, and not a function that calls itself: a deep tree cannot overflow the call stack.
        // `true` means that we leave the node, and we take its hashes out of the filter.
        while let Some((node, leaving)) = stack.pop() {
            match doc.data(node) {
                NodeData::Element { .. } if leaving => {
                    element_hashes(doc, node, &mut |h| filter.remove(h));
                    continue;
                }
                NodeData::Element { .. } => {
                    let parent = doc.parent(node).map_or(0, |p| styles.of_node[p.index()]);
                    let parent = if parent == NONE { 0 } else { parent };
                    self.candidates(doc, node, &mut candidates);
                    matched.clear();
                    for &i in &candidates {
                        let e = &self.entries[i as usize];
                        let possible = e.ancestors.iter().take_while(|&&h| h != 0).all(|&h| filter.may_contain(h));
                        if possible && matches(doc, node, &e.selector) {
                            matched.push(i);
                        }
                    }
                    matched.sort_unstable();
                    // The order of the rules is their place in the list, so the key of the cache and the result do not depend on the buckets.
                    let inline = style_atom.and_then(|a| doc.attr(node, a));
                    let id = self.style_of(&mut styles, &mut memo, parent, &matched, inline, &cx);
                    styles.of_node[node.index()] = id;
                    if styles.styles[id as usize].get(Property::Display) == Value::Keyword(Keyword::None) {
                        continue;
                        // No style for what is inside, and no filter work.
                    }
                    if doc.parent(node) == Some(doc.root()) {
                        // The root element. `rem` is a multiple of its font size.
                        cx.root_font_size = styles.styles[id as usize].px(Property::FontSize);
                    }
                    element_hashes(doc, node, &mut |h| filter.add(h));
                    stack.push((node, true));
                }
                NodeData::Document => {}
                _ => continue,
            }
            // The children, so that the first child comes out of the stack first.
            let mut child = doc.last_child(node);
            while let Some(c) = child {
                stack.push((c, false));
                child = doc.prev_sibling(c);
            }
        }
        styles.calcs = cx.computed.take().list;
        styles.customs = cx.customs.take().list;
        styles
    }

    /// The rules that may match the element: the ones in the lists for its id, its classes and its tag, and the "other" list.
    fn candidates(&self, doc: &Document, el: NodeId, out: &mut Vec<u32>) {
        out.clear();
        out.extend_from_slice(&self.other);
        if let NodeData::Element { name, .. } = *doc.data(el)
            && let Some(list) = self.by_tag.get(&name)
        {
            out.extend_from_slice(list);
        }
        if let Some(id) = doc.attr(el, Atom::ID)
            && let Some(list) = self.by_id.get(id)
        {
            out.extend_from_slice(list);
        }
        if let Some(classes) = doc.attr(el, Atom::CLASS) {
            for class in classes.split_ascii_whitespace() {
                if let Some(list) = self.by_class.get(class) {
                    out.extend_from_slice(list);
                }
            }
        }
    }

    /// The number of the style of an element. It is found in the cache, or it is made.
    fn style_of(&self, styles: &mut Styles, memo: &mut Memo, parent: u32, matched: &[u32], inline: Option<&str>, cx: &Cx<'_>) -> u32 {
        let key = matched.iter().fold(u64::from(parent), |h, &i| (h ^ u64::from(i)).wrapping_mul(0x0100_0000_01b3));
        if inline.is_none() {
            let found = memo.cache.get(&key).and_then(|list| list.iter().find(|c| c.parent == parent && *c.rules == *matched));
            if let Some(found) = found {
                return found.style;
            }
        }
        let style = self.cascade(matched, inline, &styles.styles[parent as usize], cx);
        // Two different rule lists may give the same style (`div div` inside `div`). Then they share it, and the children of both
        // have the same parent style, so they find each other in the cache.
        let hash = style_hash(&style);
        let same = memo.by_hash.get(&hash).and_then(|list| list.iter().copied().find(|&i| styles.styles[i as usize] == style));
        let id = same.unwrap_or_else(|| {
            let id = styles.styles.len() as u32;
            styles.styles.push(style);
            memo.by_hash.entry(hash).or_default().push(id);
            id
        });
        if inline.is_none() {
            memo.cache.entry(key).or_default().push(CacheEntry { parent, rules: matched.into(), style: id });
        }
        id
    }

    /// Pick the winning declaration for each property, then compute the values.
    fn cascade(&self, matched: &[u32], inline: Option<&str>, parent: &ComputedStyle, cx: &Cx<'_>) -> ComputedStyle {
        let mut winners = [None::<Value>; Property::COUNT];
        let mut keys = [0u128; Property::COUNT];
        // The key of a declaration: the one with the bigger key wins. The parts, from the most important: the origin and `!important`,
        // the layer, the specificity, and the place in the page. A `style=""` attribute has no layer (`None`): it beats every layer.
        let key = |important: bool, origin: Origin, layer: Option<u32>, specificity: u32, order: u32| {
            let rank = match (important, origin) {
                (false, Origin::UserAgent) => 0u128,
                (false, Origin::Author) => 1,
                (true, Origin::Author) => 2,
                (true, Origin::UserAgent) => 3,
            };
            // `!important` turns the order of the layers around: the earlier layer wins, and no layer is the last.
            let layer = match layer {
                None => u32::MAX,
                Some(l) if important => u32::MAX - 1 - cx.layer_order[l as usize],
                Some(l) => cx.layer_order[l as usize],
            };
            rank << 96 | u128::from(layer) << 64 | u128::from(specificity) << 32 | u128::from(order)
        };
        let mut apply = |d: &Declared, origin: Origin, layer: Option<u32>, specificity: u32, order: u32| {
            let key = key(d.important, origin, layer, specificity, order);
            let p = d.property as usize;
            if winners[p].is_none() || key >= keys[p] {
                winners[p] = Some(d.value);
                keys[p] = key;
            }
        };
        // The custom properties of the `style=""` attribute. They live until the end of the function, because `own` points into them.
        let mut inline_customs = Vec::new();
        // The custom properties that win: name, key, text. `own_at` says where a name is in the list: a scan is slow for 20,000 names.
        let mut own: Vec<(&str, u128, &str)> = Vec::new();
        let mut own_at: HashMap<&str, usize, BuildHasherDefault<Fnv>> = HashMap::default();
        fn apply_custom<'a>(own: &mut Vec<(&'a str, u128, &'a str)>, own_at: &mut HashMap<&'a str, usize, BuildHasherDefault<Fnv>>, d: &'a CustomDecl, key: u128) {
            match own_at.get(&*d.name) {
                Some(&i) if key >= own[i].1 => own[i] = (&d.name, key, &d.text),
                Some(_) => {}
                None => {
                    own_at.insert(&d.name, own.len());
                    own.push((&d.name, key, &d.text));
                }
            }
        }
        for &i in matched {
            let e = &self.entries[i as usize];
            for d in &self.decls[e.decls.0 as usize..e.decls.1 as usize] {
                apply(d, e.origin, Some(e.layer), e.selector.specificity, i);
            }
            for d in &self.customs[e.customs.0 as usize..e.customs.1 as usize] {
                apply_custom(&mut own, &mut own_at, d, key(d.important, e.origin, Some(e.layer), e.selector.specificity, i));
            }
        }
        // What the `style=""` attribute gives.
        let mut inline_side = Side::default();
        let mut inline_decls = Vec::new();
        {
            let mut shared = cx.inline.borrow_mut();
            shared.calcs.clear();
            shared.vars.clear();
        }
        if let Some(text) = inline {
            for d in parse_declarations(text) {
                if d.name.starts_with("--") {
                    inline_customs.push(CustomDecl { name: (*d.name).into(), text: d.value.into(), important: d.important });
                } else {
                    parse_declaration(&d.name, d.value, d.important, &mut inline_side, &mut inline_decls);
                }
            }
            // What these values point to comes after the ones of the style sheets (see `Cx::linear` and `Cx::var`).
            let (calc_base, var_base) = (cx.declared.calcs.len() as u32, cx.declared.vars.len() as u32);
            for d in &mut inline_decls {
                match &mut d.value {
                    Value::Length(Length::Calc(i)) => *i += calc_base,
                    Value::Var(i) => *i += var_base,
                    _ => {}
                }
            }
            *cx.inline.borrow_mut() = inline_side;
            // The specificity of a `style=""` attribute is more than the most that a selector can have (1 << 30, and the most is one less).
            for d in &inline_decls {
                apply(d, Origin::Author, None, 1 << 30, u32::MAX >> 1);
            }
            for d in &inline_customs {
                apply_custom(&mut own, &mut own_at, d, key(d.important, Origin::Author, None, 1 << 30, u32::MAX >> 1));
            }
        }
        // The custom properties: the ones of the parent, and the ones of this element. Most elements set none, and share the set of the parent.
        let custom = if own.is_empty() {
            parent.1
        } else {
            let merged = {
                let sets = cx.customs.borrow();
                let own: Vec<(&str, &str)> = own.iter().map(|o| (o.0, o.2)).collect();
                var::merge(&sets.list[parent.1 as usize], &own)
            };
            cx.customs.borrow_mut().keep(merged)
        };
        // A value with `var()` is read now, with the custom properties of this element.
        for p in 0..Property::COUNT {
            if let Some(Value::Var(i)) = winners[p] {
                winners[p] = Some(self.read_var(i, Property::ALL[p], custom, cx));
            }
        }
        let mut style = compute(&winners, parent, cx);
        style.1 = custom;
        style
    }

    /// The value of `property` for the declaration `i` that has `var()`. `unset` when it does not work: a custom property that is not set,
    /// or a text that is not good for the property (the standard calls it "invalid at computed-value time").
    fn read_var(&self, i: u32, property: Property, custom: u32, cx: &Cx<'_>) -> Value {
        let decl = cx.var(i);
        let sets = cx.customs.borrow();
        let set = &sets.list[custom as usize];
        let text = var::substitute(&decl.text, &mut |name| set.binary_search_by(|(n, _)| (**n).cmp(name)).ok().map(|i| set[i].1.to_string()));
        drop(sets);
        let (mut side, mut out) = (Side::default(), Vec::new());
        let Some(text) = text else { return Value::Unset };
        if !parse_declaration(&decl.name, &text, false, &mut side, &mut out) {
            return Value::Unset;
        }
        let Some(found) = out.iter().find(|d| d.property == property) else { return Value::Unset };
        let mut value = found.value;
        if let Value::Length(Length::Calc(c)) = &mut value {
            // A mix of units: keep it with the others of this element, after the ones that are there.
            let mut shared = cx.inline.borrow_mut();
            *c += (cx.declared.calcs.len() + shared.calcs.len()) as u32;
            shared.calcs.extend(side.calcs);
        }
        match value {
            Value::Var(_) => Value::Unset,
            v => v,
        }
    }
}

/// What one element in the cache is. Two elements with the same parent style and the same rules have the same style.
struct CacheEntry {
    parent: u32,
    rules: Box<[u32]>,
    style: u32,
}

/// A map from a hash of (parent style, rules) to the entries with that hash. A list, because two different keys may have the same hash,
/// and we compare the real keys before we take an entry.
type Cache = HashMap<u64, Vec<CacheEntry>, BuildHasherDefault<Fnv>>;

/// What we remember while we compute the styles of one document.
#[derive(Default)]
struct Memo {
    /// From the parent style and the matched rules to the style.
    cache: Cache,
    /// From the hash of a style to the numbers of the styles with that hash: to find a style that we made already.
    by_hash: HashMap<u64, Vec<u32>, BuildHasherDefault<Fnv>>,
}

/// A hash of the values of a style.
fn style_hash(style: &ComputedStyle) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64 ^ u64::from(style.1);
    for &v in &style.0 {
        let x = match v {
            Value::Color(c) => 1 << 40 | u64::from(u32::from_be_bytes([c.r, c.g, c.b, c.a])),
            Value::Length(Length::Px(x)) => 2 << 40 | u64::from(x.to_bits()),
            Value::Length(Length::Percent(x)) => 3 << 40 | u64::from(x.to_bits()),
            Value::Length(Length::Calc(i)) => 7 << 40 | u64::from(i),
            Value::Number(x) => 4 << 40 | u64::from(x.to_bits()),
            Value::Keyword(k) => 5 << 40 | k as u64,
            _ => 6 << 40,
            // A computed value is none of the others. (A length is `Px` or `Percent`.)
        };
        h = (h ^ x).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// What the computation of the values needs to know about the page.
struct Cx<'a> {
    viewport: (f32, f32),
    /// The font size of the `html` element, for `rem`. 16 until we know it.
    root_font_size: f32,
    /// The `calc()` of the style sheets (`RuleSet::calcs`).
    /// The place of each layer in the order of the cascade (`RuleSet::layer_order`).
    layer_order: Vec<u32>,
    declared: &'a Side,
    /// What the `style=""` of the element that we compute now points to, and the `calc()` that come from a `var()`.
    /// Their numbers start after the ones of `declared`.
    inline: RefCell<Side>,
    /// The `calc()` values that are done, and are kept in `Styles::calcs`.
    computed: RefCell<Computed>,
    /// The sets of custom properties, each one once.
    customs: RefCell<Customs>,
}

/// The sets of custom properties. Set 0 is the empty one. Elements with the same set have the same number, so that their styles can be shared.
struct Customs {
    list: Vec<var::Set>,
    index: HashMap<var::Set, u32, BuildHasherDefault<Fnv>>,
}

impl Default for Customs {
    fn default() -> Customs {
        let mut index = HashMap::default();
        index.insert(Vec::new(), 0);
        Customs { list: vec![Vec::new()], index }
    }
}

impl Customs {
    fn keep(&mut self, set: var::Set) -> u32 {
        if let Some(&i) = self.index.get(&set) {
            return i;
        }
        self.list.push(set.clone());
        self.index.insert(set, self.list.len() as u32 - 1);
        self.list.len() as u32 - 1
    }
}

/// The computed `calc()` values, each one once. Two elements with the same value get the same number, so that their styles can be shared.
#[derive(Default)]
struct Computed {
    list: Vec<[f32; 2]>,
    index: HashMap<[u32; 2], u32, BuildHasherDefault<Fnv>>,
}

impl Cx<'_> {
    fn linear(&self, i: u32) -> Linear {
        match self.declared.calcs.get(i as usize) {
            Some(&l) => l,
            None => self.inline.borrow().calcs[i as usize - self.declared.calcs.len()],
        }
    }

    fn var(&self, i: u32) -> VarDecl {
        match self.declared.vars.get(i as usize) {
            Some(v) => v.clone(),
            None => self.inline.borrow().vars[i as usize - self.declared.vars.len()].clone(),
        }
    }

    /// Keep the value `calc(px + pct%)` and give its number.
    fn keep(&self, px: f32, pct: f32) -> u32 {
        let mut c = self.computed.borrow_mut();
        let c = &mut *c;
        *c.index.entry([px.to_bits(), pct.to_bits()]).or_insert_with(|| {
            c.list.push([px, pct]);
            c.list.len() as u32 - 1
        })
    }
}

const NONE: u32 = u32::MAX;

/// The styles of a document.
pub struct Styles {
    /// The different styles. Many elements share one.
    styles: Vec<ComputedStyle>,
    /// For each node (by `NodeId::index`): the number of its style in `styles`, or `NONE`.
    of_node: Vec<u32>,
    /// The `calc()` values that mix units, after the cascade: pixels and percent. A `Length::Calc` in a style points here.
    calcs: Vec<[f32; 2]>,
    /// The sets of custom properties. `ComputedStyle::1` is a place in this list. Set 0 is empty.
    customs: Vec<var::Set>,
}

impl Styles {
    /// The custom properties of a style, sorted by name, with their text: `--gap`, `8px`.
    pub fn custom_properties(&self, style: &ComputedStyle) -> &[(Box<str>, Box<str>)] {
        &self.customs[style.1 as usize]
    }

    /// The pixels and the percent of a `Length::Calc` of a computed style: `calc(px + percent of something)`.
    pub fn calc(&self, i: u32) -> (f32, f32) {
        let [px, pct] = self.calcs[i as usize];
        (px, pct)
    }

    /// The style of an element. `None` for a node that is not an element, and for an element that is not shown
    /// (`display: none`, or inside one).
    pub fn style(&self, node: NodeId) -> Option<&ComputedStyle> {
        match self.of_node[node.index()] {
            NONE => None,
            i => Some(&self.styles[i as usize]),
        }
    }

    /// How many different styles there are. Style 0 (the starting style) is not counted.
    pub fn unique(&self) -> usize {
        self.styles.len() - 1
    }

    /// The tree of elements, one line for each, with the properties that are not at the starting value.
    /// An element that is not shown has `(not shown)`.
    pub fn dump(&self, doc: &Document) -> String {
        let initial = ComputedStyle::initial();
        let mut out = String::new();
        let mut stack = vec![(doc.root(), 0usize)];
        let mut shown = 0;
        while let Some((node, depth)) = stack.pop() {
            if let &NodeData::Element { name, .. } = doc.data(node) {
                out.push_str(&"  ".repeat(depth - 1));
                out.push_str(doc.atom_str(name));
                match self.style(node) {
                    None => out.push_str("  (not shown)"),
                    Some(style) => {
                        shown += 1;
                        let mut sep = "  ";
                        for &p in Property::ALL {
                            // The color of a border that is not drawn tells nothing.
                            let border_color = (p as usize).checked_sub(Property::BorderTopColor as usize).filter(|&side| side < 4);
                            let hidden = border_color.is_some_and(|side| matches!(style.0[Property::BorderTopStyle as usize + side], Value::Keyword(Keyword::None | Keyword::Hidden)));
                            if !hidden && style.get(p) != initial.get(p) {
                                out.push_str(&format!("{sep}{}: {}", p.name(), show(style.get(p), &self.calcs)));
                                sep = "; ";
                            }
                        }
                    }
                }
                out.push('\n');
            }
            let mut child = doc.last_child(node);
            while let Some(c) = child {
                stack.push((c, depth + 1));
                child = doc.prev_sibling(c);
            }
        }
        out.push_str(&format!("# {} different styles for {shown} elements with a style\n", self.unique()));
        out
    }
}

/// A computed value as text.
fn show(v: Value, calcs: &[[f32; 2]]) -> String {
    match v {
        Value::Color(c) if c.a == 255 => format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b),
        Value::Color(c) => format!("#{:02x}{:02x}{:02x}{:02x}", c.r, c.g, c.b, c.a),
        Value::Length(Length::Px(x)) => format!("{x}px"),
        Value::Length(Length::Percent(x)) => format!("{x}%"),
        Value::Length(Length::Calc(i)) => format!("calc({}px + {}%)", calcs[i as usize][0], calcs[i as usize][1]),
        Value::Number(x) => format!("{x}"),
        Value::Keyword(k) => k.name().to_string(),
        other => format!("{other:?}"),
        // Not reached for a computed value.
    }
}

/// The style of one element: one value for each property, in the order of `Property::ALL`.
#[derive(Debug, Clone, PartialEq)]
pub struct ComputedStyle([Value; Property::COUNT], u32);

impl ComputedStyle {
    /// The style of an element that has no rule at all, and whose parent has none.
    pub fn initial() -> ComputedStyle {
        let mut values = [Value::Initial; Property::COUNT];
        for &p in Property::ALL {
            values[p as usize] = match initial(p) {
                Value::Keyword(Keyword::CurrentColor) => Value::Color(Color::BLACK),
                v => v,
            };
        }
        // The border style is `none`, so the width is 0, as in `compute`.
        for side in 0..4 {
            values[Property::BorderTopWidth as usize + side] = Value::Length(Length::Px(0.0));
        }
        ComputedStyle(values, 0)
    }

    pub fn get(&self, property: Property) -> Value {
        self.0[property as usize]
    }

    /// The value of a property that is a length in pixels. 0 if it is not (a percent, or `auto`): ask `get` first for those.
    pub fn px(&self, property: Property) -> f32 {
        as_px(self.get(property))
    }

    /// The value of a property that is a color. Black if it is not one.
    pub fn color(&self, property: Property) -> Color {
        as_color(self.get(property))
    }
}

fn as_px(v: Value) -> f32 {
    match v {
        Value::Length(Length::Px(x)) => x,
        _ => 0.0,
    }
}

fn as_color(v: Value) -> Color {
    match v {
        Value::Color(c) => c,
        _ => Color::BLACK,
    }
}

/// The starting value of a property. `currentcolor` for the border colors: `resolve` turns it into the color of the element.
fn initial(p: Property) -> Value {
    use Keyword as K;
    use Property::*;
    let px = |v| Value::Length(Length::Px(v));
    match p {
        Display => Value::Keyword(K::Inline),
        Float | Clear | MaxWidth | MaxHeight | BorderTopStyle | BorderRightStyle | BorderBottomStyle | BorderLeftStyle | TextDecoration | TextTransform => Value::Keyword(K::None),
        Visibility => Value::Keyword(K::Visible),
        WhiteSpace | FontStyle | LineHeight => Value::Keyword(K::Normal),
        TextAlign => Value::Keyword(K::Start),
        ListStyleType => Value::Keyword(K::Disc),
        BoxSizing => Value::Keyword(K::ContentBox),
        TextColor => Value::Color(Color::BLACK),
        BackgroundColor => Value::Color(Color::TRANSPARENT),
        FontSize => px(16.0),
        FontWeight => Value::Number(400.0),
        Width | Height => Value::Keyword(K::Auto),
        MinWidth | MinHeight | MarginTop | MarginRight | MarginBottom | MarginLeft | PaddingTop | PaddingRight | PaddingBottom | PaddingLeft => px(0.0),
        BorderTopWidth | BorderRightWidth | BorderBottomWidth | BorderLeftWidth => px(3.0),
        // `medium`. With the style `none` it is 0: `compute` does that.
        BorderTopColor | BorderRightColor | BorderBottomColor | BorderLeftColor => Value::Keyword(K::CurrentColor),
    }
}

/// Does a child take the value of the parent when it has no value of its own?
fn inherited(p: Property) -> bool {
    use Property::*;
    matches!(p, TextColor | FontSize | FontWeight | FontStyle | LineHeight | TextAlign | TextTransform | WhiteSpace | ListStyleType | Visibility)
}

/// The font size of the element, in pixels. It is first, because `em` in the other properties needs it.
fn font_size(winner: Option<Value>, parent_size: f32, cx: &Cx<'_>) -> f32 {
    match winner {
        None | Some(Value::Inherit | Value::Unset) => parent_size,
        Some(Value::Initial) => 16.0,
        Some(Value::Keyword(Keyword::Larger)) => parent_size * 1.2,
        Some(Value::Keyword(Keyword::Smaller)) => parent_size / 1.2,
        // `em` and `%` in `font-size` mean the font size of the parent. Not of this element: it would go in a circle.
        Some(Value::Length(Length::Em(x))) => x * parent_size,
        Some(Value::Length(Length::Percent(x))) => x * parent_size / 100.0,
        Some(Value::Length(Length::Calc(i))) => {
            let lin = cx.linear(i);
            lin.to_px(parent_size, cx.root_font_size, cx.viewport) + lin.pct() * parent_size / 100.0
        }
        Some(Value::Length(l)) => length_to_px(l, parent_size, cx),
        _ => parent_size,
    }
}

/// A length in pixels, for a length that is not a percent. `em` is a multiple of `font_size`.
fn length_to_px(l: Length, font_size: f32, cx: &Cx<'_>) -> f32 {
    let (w, h) = cx.viewport;
    match l {
        Length::Px(x) => x,
        Length::Em(x) => x * font_size,
        Length::Rem(x) => x * cx.root_font_size,
        Length::Vw(x) => x * w / 100.0,
        Length::Vh(x) => x * h / 100.0,
        Length::Vmin(x) => x * w.min(h) / 100.0,
        Length::Vmax(x) => x * w.max(h) / 100.0,
        Length::Percent(x) => x,
        // Callers keep a percent as a percent, and they do the `calc()` themselves. This is not reached for them.
        Length::Calc(_) => 0.0,
    }
}

/// Make the computed style from the winning declarations and the style of the parent.
fn compute(winners: &[Option<Value>; Property::COUNT], parent: &ComputedStyle, cx: &Cx<'_>) -> ComputedStyle {
    use Property::*;
    let fs = font_size(winners[FontSize as usize], parent.px(FontSize), cx);
    let mut out = [Value::Initial; Property::COUNT];
    out[FontSize as usize] = Value::Length(Length::Px(fs));
    for &p in Property::ALL {
        if p == FontSize {
            continue;
        }
        let i = p as usize;
        let raw = match winners[i] {
            Some(Value::Inherit) => parent.0[i],
            Some(Value::Initial) => initial(p),
            None | Some(Value::Unset) if inherited(p) => parent.0[i],
            None | Some(Value::Unset) => initial(p),
            Some(v) => v,
        };
        out[i] = resolve(p, raw, &out, parent, fs, cx);
    }
    // A border with no style has no width.
    for side in 0..4 {
        if matches!(out[BorderTopStyle as usize + side], Value::Keyword(Keyword::None | Keyword::Hidden)) {
            out[BorderTopWidth as usize + side] = Value::Length(Length::Px(0.0));
        }
    }
    ComputedStyle(out, 0)
}

/// Turn a value into its computed form. It does nothing to a value that is computed already (one from the parent).
fn resolve(p: Property, v: Value, out: &[Value; Property::COUNT], parent: &ComputedStyle, fs: f32, cx: &Cx<'_>) -> Value {
    let px = |x| Value::Length(Length::Px(x));
    match v {
        // The color of the element. `color: currentcolor` means the color of the parent (the element has no color yet).
        Value::Keyword(Keyword::CurrentColor) if p == Property::TextColor => Value::Color(parent.color(p)),
        Value::Keyword(Keyword::CurrentColor) => Value::Color(as_color(out[Property::TextColor as usize])),
        Value::Length(Length::Percent(x)) if p == Property::LineHeight => px(x * fs / 100.0),
        Value::Length(Length::Calc(i)) => {
            let lin = cx.linear(i);
            let (x, pct) = (lin.to_px(fs, cx.root_font_size, cx.viewport), lin.pct());
            if p == Property::LineHeight {
                px(x + pct * fs / 100.0)
            } else if pct == 0.0 {
                px(x)
            } else if x == 0.0 {
                Value::Length(Length::Percent(pct))
            } else {
                Value::Length(Length::Calc(cx.keep(x, pct)))
            }
        }
        Value::Length(l @ Length::Percent(_)) => Value::Length(l),
        Value::Length(l) => px(length_to_px(l, fs, cx)),
        Value::Keyword(Keyword::Bolder) => Value::Number(match parent.number_or_400(p) {
            w if w < 350.0 => 400.0,
            w if w < 550.0 => 700.0,
            _ => 900.0,
        }),
        Value::Keyword(Keyword::Lighter) => Value::Number(match parent.number_or_400(p) {
            w if w < 550.0 => 100.0,
            w if w < 750.0 => 400.0,
            _ => 700.0,
        }),
        _ => v,
    }
}

impl ComputedStyle {
    /// The number of a property, for the weight of the parent.
    fn number_or_400(&self, p: Property) -> f32 {
        match self.get(p) {
            Value::Number(x) => x,
            _ => 400.0,
        }
    }
}

/// A Bloom filter of counters: it tells if a hash may be in the set, and it can take hashes out.
/// 4096 counters of one byte. A hash has two places. A counter that reaches 255 stays there (it would be wrong to count down from it).
struct AncestorFilter {
    counts: Box<[u8; 4096]>,
}

impl AncestorFilter {
    fn new() -> AncestorFilter {
        AncestorFilter { counts: Box::new([0; 4096]) }
    }

    fn places(h: u32) -> [usize; 2] {
        [(h & 0xfff) as usize, ((h >> 16) & 0xfff) as usize]
    }

    fn add(&mut self, h: u32) {
        for i in Self::places(h) {
            self.counts[i] = self.counts[i].saturating_add(1);
        }
    }

    fn remove(&mut self, h: u32) {
        for i in Self::places(h) {
            if self.counts[i] != u8::MAX {
                self.counts[i] -= 1;
            }
        }
    }

    fn may_contain(&self, h: u32) -> bool {
        Self::places(h).iter().all(|&i| self.counts[i] != 0)
    }
}

/// A hash of a name, never 0. `kind` makes the hash of the class `a` and the hash of the id `a` different.
fn name_hash(kind: u32, name: &str) -> u32 {
    let mut h = 0x811c_9dc5 ^ kind;
    for &b in name.as_bytes() {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    mix(h)
}

fn tag_hash(tag: Atom) -> u32 {
    mix((0x811c_9dc5 ^ (tag.index() + 1)).wrapping_mul(0x0100_0193))
}

/// Spread the bits, so that the two places of the filter (the low bits and the middle bits) both depend on all of them.
fn mix(mut h: u32) -> u32 {
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h.max(1)
}

/// Give the hashes of the tag, the id and the classes of an element.
fn element_hashes(doc: &Document, el: NodeId, f: &mut dyn FnMut(u32)) {
    if let &NodeData::Element { name, .. } = doc.data(el) {
        f(tag_hash(name));
    }
    if let Some(id) = doc.attr(el, Atom::ID) {
        f(name_hash(1, id));
    }
    if let Some(classes) = doc.attr(el, Atom::CLASS) {
        for class in classes.split_ascii_whitespace() {
            f(name_hash(2, class));
        }
    }
}

/// The hashes that the ancestors of a matching element must have: from each compound that is an ancestor of the element
/// (joined by a space or `>`, all the way from the right), one tag, id or class. The compounds after a `+` or `~` are not
/// ancestors: they are siblings of an ancestor or of the element.
fn ancestor_hashes(selector: &Selector) -> [u32; 4] {
    let mut out = [0; 4];
    let mut n = 0;
    for pair in selector.compounds.windows(2) {
        if !matches!(pair[0].combinator, Combinator::Descendant | Combinator::Child) {
            break;
        }
        let hash = pair[1].simples.iter().find_map(|s| match s {
            Simple::Tag(t) => Some(tag_hash(*t)),
            Simple::Id(id) => Some(name_hash(1, id)),
            Simple::Class(class) => Some(name_hash(2, class)),
            _ => None,
        });
        if let Some(h) = hash
            && n < out.len()
        {
            out[n] = h;
            n += 1;
        }
    }
    out
}

/// The addresses of the `@import` rules at the start of a style sheet that match the window (see `at_rules::imports`).
pub fn imports(doc: &mut Document, text: &str, viewport: (f32, f32)) -> Vec<at_rules::Import> {
    at_rules::imports(doc, &parse_stylesheet(text), viewport)
}

/// What a page asks us to read for style, in the order of the page.
#[derive(Debug, PartialEq)]
pub enum Source {
    /// The text of a `<style>` element.
    Inline(String),
    /// The `href` of a `<link rel=stylesheet>`. The app must fetch it, and resolve it against the address of the page.
    Link(String),
}

/// The `<style>` and `<link rel=stylesheet>` elements, in order. One with a `media` that does not match a window of `viewport` pixels
/// is left out, and so is an alternate style sheet (`rel="alternate stylesheet"`): a browser does not use it unless the user asks.
/// An empty `media` is good.
pub fn sources(doc: &Document, viewport: (f32, f32)) -> Vec<Source> {
    // A name that no node in the page has cannot be on any element, so `None` is a good answer for a name that is not in the table.
    let (style, link, media, rel, href) = (doc.find_atom("style"), doc.find_atom("link"), doc.find_atom("media"), doc.find_atom("rel"), doc.find_atom("href"));
    let attr = |el, name: Option<Atom>| name.and_then(|n| doc.attr(el, n));
    let mut out = Vec::new();
    walk(doc, &mut |el| {
        let NodeData::Element { name, .. } = *doc.data(el) else { return };
        let (is_style, is_link) = (Some(name) == style, Some(name) == link);
        if !(is_style || is_link) || !attr(el, media).is_none_or(|m| at_rules::media_matches(m, viewport)) {
            return;
        }
        if is_style {
            let mut text = String::new();
            for c in doc.children(el) {
                if let NodeData::Text(span) = *doc.data(c) {
                    text.push_str(doc.text(span));
                }
            }
            out.push(Source::Inline(text));
        } else if let Some(rel) = attr(el, rel) {
            let has = |word: &str| rel.split_ascii_whitespace().any(|w| w.eq_ignore_ascii_case(word));
            if let (true, false, Some(h)) = (has("stylesheet"), has("alternate"), attr(el, href)) {
                out.push(Source::Link(h.to_string()));
            }
        }
    });
    out
}

/// Call `f` for each node of the tree, in the order of the page. A loop with a stack.
fn walk(doc: &Document, f: &mut dyn FnMut(NodeId)) {
    let mut stack = vec![doc.root()];
    while let Some(node) = stack.pop() {
        f(node);
        let mut child = doc.last_child(node);
        while let Some(c) = child {
            stack.push(c);
            child = doc.prev_sibling(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bare_html::tree::parse;

    const VIEW: (f32, f32) = (1000.0, 500.0);

    /// The styles of a page with the UA sheet and the author sheets.
    fn styles(html: &str, sheets: &[&str]) -> (Document, Styles) {
        let mut doc = parse(html);
        let mut rules = RuleSet::new(VIEW);
        rules.add(&mut doc, UA_CSS, Origin::UserAgent);
        for s in sheets {
            rules.add(&mut doc, s, Origin::Author);
        }
        let styles = rules.compute(&doc);
        (doc, styles)
    }

    /// The first element with this `id`.
    fn by_id(doc: &Document, id: &str) -> NodeId {
        let mut found = None;
        walk(doc, &mut |n| {
            if found.is_none() && doc.attr(n, Atom::ID) == Some(id) {
                found = Some(n);
            }
        });
        found.unwrap_or_else(|| panic!("no element with id {id}"))
    }

    fn get(html: &str, sheets: &[&str], id: &str, p: Property) -> Value {
        let (doc, styles) = styles(html, sheets);
        styles.style(by_id(&doc, id)).unwrap_or_else(|| panic!("{id} has no style")).get(p)
    }

    fn px(v: f32) -> Value {
        Value::Length(Length::Px(v))
    }

    fn rgb(r: u8, g: u8, b: u8) -> Value {
        Value::Color(Color { r, g, b, a: 255 })
    }

    #[test]
    fn the_browser_style_sheet() {
        let html = "<h1 id=h>x</h1><p id=p>y</p><b id=b>z</b><ul id=u><li id=l>w</ul>";
        assert_eq!(get(html, &[], "h", Property::Display), Value::Keyword(Keyword::Block));
        assert_eq!(get(html, &[], "h", Property::FontSize), px(32.0));
        assert_eq!(get(html, &[], "h", Property::FontWeight), Value::Number(700.0));
        assert_eq!(get(html, &[], "h", Property::MarginTop), px(21.44));
        assert_eq!(get(html, &[], "p", Property::MarginTop), px(16.0));
        assert_eq!(get(html, &[], "p", Property::MarginLeft), px(0.0));
        assert_eq!(get(html, &[], "b", Property::Display), Value::Keyword(Keyword::Inline));
        assert_eq!(get(html, &[], "b", Property::FontWeight), Value::Number(700.0));
        assert_eq!(get(html, &[], "l", Property::Display), Value::Keyword(Keyword::ListItem));
        assert_eq!(get(html, &[], "u", Property::PaddingLeft), px(40.0));
    }

    #[test]
    fn body_has_a_margin_and_head_is_not_shown() {
        let (doc, styles) = styles("<title>t</title><p id=p>x", &[]);
        let root = doc.children(doc.root()).next().unwrap();
        let kids: Vec<_> = doc.children(root).collect();
        // `head`, then `body`. `head` has a style, and it says `display: none`.
        assert_eq!(styles.style(kids[0]).unwrap().get(Property::Display), Value::Keyword(Keyword::None));
        assert_eq!(styles.style(kids[1]).unwrap().get(Property::MarginTop), px(8.0));
        let title = doc.children(kids[0]).next().unwrap();
        assert_eq!(styles.style(title), None, "an element inside `display: none` has no style");
    }

    #[test]
    fn text_nodes_have_no_style() {
        let (doc, styles) = styles("<p id=p>x</p>", &[]);
        let text = doc.children(by_id(&doc, "p")).next().unwrap();
        assert_eq!(styles.style(text), None);
        assert_eq!(styles.style(doc.root()), None);
    }

    #[test]
    fn author_beats_the_browser() {
        let html = "<p id=p>x</p>";
        assert_eq!(get(html, &["p { margin: 0 }"], "p", Property::MarginTop), px(0.0));
        assert_eq!(get(html, &["* { margin: 3px }"], "p", Property::MarginTop), px(3.0));
        // Even with a selector that has less specificity than the one in the UA sheet: `*` against `p`.
    }

    #[test]
    fn specificity_order_and_importance() {
        let html = "<div id=d class=c><p id=p class=k>x</p></div>";
        let p = |sheet: &str| get(html, &[sheet], "p", Property::TextColor);
        assert_eq!(p("p { color: red } .k { color: blue }"), rgb(0, 0, 255));
        assert_eq!(p(".k { color: blue } p { color: red }"), rgb(0, 0, 255));
        assert_eq!(p("#p { color: lime } .k { color: blue }"), rgb(0, 255, 0));
        assert_eq!(p("div p { color: red } p { color: blue }"), rgb(255, 0, 0));
        assert_eq!(p("p { color: red } p { color: blue }"), rgb(0, 0, 255));
        assert_eq!(p("p { color: red !important } #p { color: blue }"), rgb(255, 0, 0));
        assert_eq!(p("#p { color: red !important } #p { color: blue !important }"), rgb(0, 0, 255));
        assert_eq!(p("p { color: red; color: blue }"), rgb(0, 0, 255));
        assert_eq!(p("p { color: red; color: nonsense }"), rgb(255, 0, 0));
        // Two sheets: the later one wins a tie.
        assert_eq!(get(html, &["p { color: red }", "p { color: blue }"], "p", Property::TextColor), rgb(0, 0, 255));
    }

    #[test]
    fn a_style_attribute_beats_the_selectors() {
        let html = "<p id=p style='color: blue; margin: 5px'>x</p>";
        assert_eq!(get(html, &["#p#p#p { color: red }"], "p", Property::TextColor), rgb(0, 0, 255));
        assert_eq!(get(html, &[], "p", Property::MarginTop), px(5.0));
        // `!important` in a sheet beats a normal declaration in the attribute.
        assert_eq!(get(html, &["p { color: red !important }"], "p", Property::TextColor), rgb(255, 0, 0));
        // A mistake in the attribute loses only that declaration.
        assert_eq!(get("<p id=p style='oops; color: blue; width: x'>", &[], "p", Property::TextColor), rgb(0, 0, 255));
    }

    #[test]
    fn important_of_the_browser_beats_the_page() {
        let sheet = "p { display: inline !important }";
        let mut doc = parse("<p id=p>x</p>");
        let mut rules = RuleSet::new(VIEW);
        rules.add(&mut doc, "p { display: block !important }", Origin::UserAgent);
        rules.add(&mut doc, sheet, Origin::Author);
        let styles = rules.compute(&doc);
        assert_eq!(styles.style(by_id(&doc, "p")).unwrap().get(Property::Display), Value::Keyword(Keyword::Block));
    }

    #[test]
    fn inheritance() {
        let html = "<div id=d><p id=p><b id=b>x</b></p></div>";
        let sheet = "div { color: red; font-size: 20px; margin-left: 10px; background-color: blue; font-weight: 600 }";
        assert_eq!(get(html, &[sheet], "b", Property::TextColor), rgb(255, 0, 0));
        assert_eq!(get(html, &[sheet], "p", Property::FontSize), px(20.0));
        assert_eq!(get(html, &[sheet], "p", Property::FontWeight), Value::Number(600.0));
        // Not inherited: the margin and the background.
        assert_eq!(get(html, &[sheet], "p", Property::MarginLeft), px(0.0));
        assert_eq!(get(html, &[sheet], "p", Property::BackgroundColor), Value::Color(Color::TRANSPARENT));
        // `b` has its own weight in the browser sheet, and keeps the color of its ancestor.
        assert_eq!(get(html, &[sheet], "b", Property::FontWeight), Value::Number(700.0));
    }

    #[test]
    fn inherit_initial_unset() {
        let html = "<div id=d><p id=p>x</p></div>";
        let s = |css: &str, p| get(html, &[css], "p", p);
        assert_eq!(s("div { margin-left: 7px } p { margin-left: inherit }", Property::MarginLeft), px(7.0));
        assert_eq!(s("div { color: red } p { color: initial }", Property::TextColor), rgb(0, 0, 0));
        assert_eq!(s("div { color: red } p { color: unset }", Property::TextColor), rgb(255, 0, 0));
        assert_eq!(s("div { margin-left: 7px } p { margin-left: unset }", Property::MarginLeft), px(0.0));
        assert_eq!(s("p { margin: inherit }", Property::MarginTop), px(0.0));
        assert_eq!(s("p { display: initial }", Property::Display), Value::Keyword(Keyword::Inline));
    }

    #[test]
    fn em_rem_and_percent() {
        let html = "<div id=d><p id=p>x</p></div>";
        let s = |css: &str, p| get(html, &[css], "p", p);
        // `em` in `font-size` is of the parent. In other properties, it is of the element.
        assert_eq!(s("div { font-size: 10px } p { font-size: 2em }", Property::FontSize), px(20.0));
        assert_eq!(s("div { font-size: 10px } p { font-size: 150% }", Property::FontSize), px(15.0));
        assert_eq!(s("div { font-size: 10px } p { font-size: 20px; margin-left: 2em }", Property::MarginLeft), px(40.0));
        assert_eq!(s("div { font-size: 10px } p { margin-left: 2em }", Property::MarginLeft), px(20.0));
        assert_eq!(s("html { font-size: 20px } p { margin-left: 2rem }", Property::MarginLeft), px(40.0));
        assert_eq!(s("html { font-size: 2rem } p { margin-left: 1rem }", Property::MarginLeft), px(32.0));
        assert_eq!(s("p { width: 50vw; height: 10vh }", Property::Width), px(500.0));
        assert_eq!(s("p { height: 10vh }", Property::Height), px(50.0));
        assert_eq!(s("p { width: 6vmin }", Property::Width), px(30.0));
        assert_eq!(s("p { width: 6vmax }", Property::Width), px(60.0));
        assert_eq!(s("p { width: 50% }", Property::Width), Value::Length(Length::Percent(50.0)));
        assert_eq!(s("p { margin-left: auto }", Property::MarginLeft), Value::Keyword(Keyword::Auto));
        assert_eq!(s("div { font-size: 10px } p { font-size: larger }", Property::FontSize), px(12.0));
        assert_eq!(s("div { font-size: 12px } p { font-size: smaller }", Property::FontSize), px(10.0));
        assert_eq!(s("p { font-size: 1.5em }", Property::FontSize), px(24.0));
    }

    #[test]
    fn line_height_and_font_weight() {
        let html = "<div id=d><p id=p>x</p></div>";
        let s = |css: &str, p| get(html, &[css], "p", p);
        assert_eq!(s("div { line-height: 1.5 }", Property::LineHeight), Value::Number(1.5));
        assert_eq!(s("div { font-size: 10px; line-height: 150% } p { font-size: 20px }", Property::LineHeight), px(15.0));
        assert_eq!(s("div { line-height: 2em; font-size: 10px } p { font-size: 40px }", Property::LineHeight), px(20.0));
        assert_eq!(s("p { line-height: normal }", Property::LineHeight), Value::Keyword(Keyword::Normal));
        assert_eq!(s("div { font-weight: 300 } p { font-weight: bolder }", Property::FontWeight), Value::Number(400.0));
        assert_eq!(s("div { font-weight: 400 } p { font-weight: bolder }", Property::FontWeight), Value::Number(700.0));
        assert_eq!(s("div { font-weight: 700 } p { font-weight: bolder }", Property::FontWeight), Value::Number(900.0));
        assert_eq!(s("div { font-weight: 700 } p { font-weight: lighter }", Property::FontWeight), Value::Number(400.0));
        assert_eq!(s("div { font-weight: 400 } p { font-weight: lighter }", Property::FontWeight), Value::Number(100.0));
    }

    #[test]
    fn currentcolor_and_borders() {
        let html = "<p id=p>x</p>";
        let s = |css: &str, p| get(html, &[css], "p", p);
        assert_eq!(s("p { color: red; border: 2px solid }", Property::BorderTopColor), rgb(255, 0, 0));
        assert_eq!(s("p { color: red; border: 2px solid }", Property::BorderLeftWidth), px(2.0));
        assert_eq!(s("p { border-top-color: blue; color: red; border-top-style: solid }", Property::BorderTopColor), rgb(0, 0, 255));
        // The color is not yet known when the border color comes first in the rule: it is still the color of the element.
        assert_eq!(s("p { border-top-color: currentcolor; color: red }", Property::BorderTopColor), rgb(255, 0, 0));
        assert_eq!(s("p { background-color: currentcolor; color: lime }", Property::BackgroundColor), rgb(0, 255, 0));
        // No style, no width.
        assert_eq!(s("p { border-width: 5px }", Property::BorderTopWidth), px(0.0));
        assert_eq!(s("p { border: 5px hidden red }", Property::BorderTopWidth), px(0.0));
        assert_eq!(s("p { border: 5px dotted red }", Property::BorderTopWidth), px(5.0));
        assert_eq!(s("p { border: medium dotted red }", Property::BorderTopWidth), px(3.0));
    }

    #[test]
    fn selectors_with_context() {
        let html = "<div id=a class=x><ul><li id=l1>1<li id=l2>2<li id=l3>3</ul><p id=p>t</div>";
        let color = |css: &str, id| get(html, &[css], id, Property::TextColor);
        assert_eq!(color(".x li { color: red }", "l2"), rgb(255, 0, 0));
        assert_eq!(color(".x > li { color: red }", "l2"), rgb(0, 0, 0));
        assert_eq!(color("ul > li:first-child { color: red }", "l1"), rgb(255, 0, 0));
        assert_eq!(color("ul > li:first-child { color: red }", "l2"), rgb(0, 0, 0));
        assert_eq!(color("li + li { color: red }", "l3"), rgb(255, 0, 0));
        assert_eq!(color("li:nth-child(odd) { color: red }", "l3"), rgb(255, 0, 0));
        assert_eq!(color("ul ~ p { color: red }", "p"), rgb(255, 0, 0));
        assert_eq!(color("#a p, #a li { color: red }", "l1"), rgb(255, 0, 0));
        assert_eq!(color("p:not(.x) { color: red }", "p"), rgb(255, 0, 0));
        assert_eq!(color("li:hover { color: red }", "l1"), rgb(0, 0, 0));
        // A broken selector drops the rule, and the rules after it work.
        assert_eq!(color("li::before, p { color: red } li { color: blue }", "p"), rgb(0, 0, 0));
        assert_eq!(color("li::before, p { color: red } li { color: blue }", "l1"), rgb(0, 0, 255));
    }

    #[test]
    fn a_selector_list_and_the_rules_of_all_buckets() {
        // One rule for each kind of bucket: id, class, tag, other.
        let html = "<p id=p class=c>x</p>";
        let sheet = "#p { margin-top: 1px } .c { margin-right: 2px } p { margin-bottom: 3px } [id] { margin-left: 4px }";
        for (p, v) in [(Property::MarginTop, 1.0), (Property::MarginRight, 2.0), (Property::MarginBottom, 3.0), (Property::MarginLeft, 4.0)] {
            assert_eq!(get(html, &[sheet], "p", p), px(v));
        }
    }

    #[test]
    fn display_none_hides_the_inside() {
        let (doc, styles) = styles("<div id=d style='display:none'><p id=p>x</p></div><div id=e><p id=q>y</p></div>", &[]);
        assert!(styles.style(by_id(&doc, "d")).is_some(), "the element itself has a style: it says `none`");
        assert_eq!(styles.style(by_id(&doc, "p")), None);
        assert!(styles.style(by_id(&doc, "q")).is_some());
        assert!(styles.dump(&doc).contains("(not shown)"));
    }

    #[test]
    fn elements_share_styles() {
        let list: String = (0..1000).map(|i| format!("<li class=c id=i{i}>x")).collect();
        let (doc, styles) = styles(&format!("<ul>{list}</ul>"), &[".c { color: red }"]);
        // `html`, `head`, `body`, `ul` and one style for all the `li`. (`head` has a style: `display: none`.)
        assert!(styles.unique() <= 5, "{} styles", styles.unique());
        let a = styles.style(by_id(&doc, "i3")).unwrap();
        let b = styles.style(by_id(&doc, "i900")).unwrap();
        assert!(std::ptr::eq(a, b), "the same style object");
    }

    #[test]
    fn sharing_does_not_mix_up_styles() {
        // Same rules, different parents: different inherited values.
        let html = "<div class=a><p id=p1>x</p></div><div class=b><p id=p2>y</p></div>";
        let sheet = ".a { color: red } .b { color: blue }";
        assert_eq!(get(html, &[sheet], "p1", Property::TextColor), rgb(255, 0, 0));
        assert_eq!(get(html, &[sheet], "p2", Property::TextColor), rgb(0, 0, 255));
        // An attribute makes an element different from the others with the same rules.
        let html = "<p class=c id=p1>x</p><p class=c id=p2 style='color: red'>y</p><p class=c id=p3>z</p>";
        assert_eq!(get(html, &[".c { margin-left: 1px }"], "p1", Property::TextColor), rgb(0, 0, 0));
        assert_eq!(get(html, &[".c { margin-left: 1px }"], "p2", Property::TextColor), rgb(255, 0, 0));
        assert_eq!(get(html, &[".c { margin-left: 1px }"], "p3", Property::TextColor), rgb(0, 0, 0));
    }

    #[test]
    fn the_ancestor_filter_never_hides_a_match() {
        // Many ancestors with many classes, so the filter has many counters in use, and then we leave and enter again.
        let mut html = String::new();
        for i in 0..300 {
            html.push_str(&format!("<div class='k{i} m{}'>", i * 7));
        }
        html.push_str("<p id=p>x</p>");
        html.push_str(&"</div>".repeat(300));
        html.push_str("<p id=q>y</p>");
        // `.k150 .m1050` needs a descendant with `m1050`, and `m1050` is on the same `div` as `k150`, so that rule does not match.
        for (sheet, expected) in [(".k0 p", true), (".k299 p", true), (".k150 .m1050 > div > p", false), (".k150 .m1057 p", true), (".m1 p", false), ("div.k298 > div > p", true)] {
            let (doc, s) = styles(&html, &[&format!("{sheet} {{ color: red }}")]);
            let red = |id| s.style(by_id(&doc, id)).unwrap().get(Property::TextColor) == rgb(255, 0, 0);
            assert_eq!(red("p"), expected, "{sheet}");
            assert!(!red("q"), "{sheet}: `q` is outside the divs");
        }
    }

    #[test]
    fn a_deep_tree_does_not_overflow() {
        let mut doc = Document::new();
        let mut el = doc.root();
        for _ in 0..50_000 {
            el = doc.insert_element(el, None, "div", &[] as &[(&str, &str)]);
        }
        let mut rules = RuleSet::new(VIEW);
        rules.add(&mut doc, "div div div { color: red }", Origin::Author);
        let styles = rules.compute(&doc);
        assert_eq!(styles.style(el).unwrap().get(Property::TextColor), rgb(255, 0, 0));
        assert_eq!(styles.unique(), 2, "all the divs share one style, and `html`... no `html` here: the first `div` is the root");
    }

    #[test]
    fn sources_of_style() {
        let html = "<link rel=stylesheet href=a.css><style>p{}</style><link rel=icon href=i.png><link rel='Alternate Stylesheet' href=b.css>\
                    <link rel=stylesheet href=print.css media=print><style media='screen and (min-width: 1px)'>q{}</style>\
                    <style media=print>r{}</style><link rel=stylesheet href=c.css media=''><p>x</p>";
        assert_eq!(
            sources(&parse(html), VIEW),
            [
                Source::Link("a.css".into()),
                Source::Inline("p{}".into()),
                Source::Inline("q{}".into()),
                Source::Link("c.css".into()),
            ]
        );
        assert_eq!(sources(&parse("<p>x</p>"), VIEW), []);
        assert_eq!(sources(&parse("<link rel=stylesheet href=a.css>"), VIEW), [Source::Link("a.css".into())]);
    }

    #[test]
    fn dump_shows_what_is_not_the_starting_value() {
        let (doc, styles) = styles("<p id=p style='color: red'>x</p>", &[]);
        let dump = styles.dump(&doc);
        assert!(dump.contains("p  display: block; color: #ff0000; margin-top: 16px; margin-bottom: 16px"), "{dump}");
        assert!(dump.contains("head  display: none\n"), "{dump}");
        assert!(dump.contains("# "), "{dump}");
    }

    #[test]
    fn random_pages_and_sheets_do_not_panic() {
        let words = ["div", "p", "a", "b", ".x", ".y", "#i", "{", "}", ";", ":", "color", "red", "margin", "1px", "2em", "inherit", ">", "+", "~", ",", " ", "li:first-child", "!important", "font-size", "50%", "(", ")"];
        let tags = ["<div class=x>", "<p id=i>", "<a class='x y'>", "<b style='color:red'>", "</div>", "</p>", "<li>", "text"];
        let mut seed = 0x1234_5678_9abc_def1_u64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for _ in 0..300 {
            let html: String = (0..next(30)).map(|_| tags[next(tags.len())]).collect();
            let sheet: String = (0..next(60)).map(|_| words[next(words.len())]).collect::<Vec<_>>().join(" ");
            let (doc, s) = styles(&html, &[&sheet]);
            let _ = s.dump(&doc);
        }
    }

    #[test]
    fn at_rules_apply_when_they_match() {
        let color = |sheet: &str| get("<p id=p>x</p>", &[sheet], "p", Property::TextColor);
        let red = Value::Color(Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        });
        let black = color("");
        for (sheet, want) in [
            (
                "@media screen and (min-width: 600px) { p { color: red } }",
                true,
            ),
            ("@media (min-width: 2000px) { p { color: red } }", false),
            ("@media print { p { color: red } }", false),
            ("@media not print { p { color: red } }", true),
            ("@supports (display: grid) { p { color: red } }", true),
            ("@supports (display: nonsense) { p { color: red } }", false),
            (
                "@supports not (display: nonsense) { p { color: red } }",
                true,
            ),
            ("@layer base { p { color: red } }", true),
            ("@layer a, b;", false),
            (
                "@media screen { @supports (color: red) { @media (min-width: 1px) { p { color: red } } } }",
                true,
            ),
            (
                "@media screen { @supports (color: nonsense) { p { color: red } } }",
                false,
            ),
            ("@container (min-width: 1px) { p { color: red } }", false),
            (
                "@font-face { color: red } @keyframes k { from { color: red } }",
                false,
            ),
        ] {
            assert_eq!(color(sheet) == red, want, "{sheet}");
            assert!(want || color(sheet) == black, "{sheet}");
        }
    }

    #[test]
    fn rules_in_media_keep_the_order_of_the_page() {
        let sheet =
            "p { color: blue } @media (min-width: 1px) { p { color: red } } p { color: lime }";
        assert_eq!(
            get("<p id=p>x</p>", &[sheet], "p", Property::TextColor),
            Value::Color(Color {
                r: 0,
                g: 255,
                b: 0,
                a: 255
            })
        );
        let sheet = "@media (min-width: 1px) { p { color: red } } p { color: blue }";
        assert_eq!(
            get("<p id=p>x</p>", &[sheet], "p", Property::TextColor),
            Value::Color(Color {
                r: 0,
                g: 0,
                b: 255,
                a: 255
            })
        );
    }

    #[test]
    fn calc_in_the_cascade() {
        let page = "<div id=a><p id=b>x</p></div>";
        let sheet = "html { font-size: 20px } div { font-size: calc(1em + 2px); width: calc(100% - 10px); height: calc(2em + 10vw); margin-left: calc(1rem * 2) }\
            p { font-size: calc(50% + 1px); line-height: calc(100% + 2px); padding-left: calc(1em + 1em); width: calc(100% - 10px) }";
        let (doc, styles) = styles(page, &[sheet]);
        let get = |id: &str, p: Property| styles.style(by_id(&doc, id)).unwrap().get(p);
        let px = |x| Value::Length(Length::Px(x));
        // font-size: `em` and `%` are about the parent.
        assert_eq!(get("a", Property::FontSize), px(22.0));
        assert_eq!(get("b", Property::FontSize), px(12.0));
        // A mix of percent and pixels waits for layout. Everything else is pixels.
        let Value::Length(Length::Calc(i)) = get("a", Property::Width) else { panic!("{:?}", get("a", Property::Width)) };
        assert_eq!(styles.calc(i), (-10.0, 100.0));
        assert_eq!(get("a", Property::Height), px(44.0 + 100.0));
        assert_eq!(get("a", Property::MarginLeft), px(40.0));
        assert_eq!(get("b", Property::LineHeight), px(14.0));
        assert_eq!(get("b", Property::PaddingLeft), px(24.0));
        // Two elements with the same value share the same number.
        assert_eq!(get("a", Property::Width), get("b", Property::Width));
    }

    #[test]
    fn calc_in_the_style_attribute() {
        let (doc, styles) = styles("<p id=a style='width: calc(100% - 1px)'>x</p><p id=b style='width: calc(10% - 2px)'>y</p>", &["p { width: calc(1% + 1px) }"]);
        let width = |id: &str| match styles.style(by_id(&doc, id)).unwrap().get(Property::Width) {
            Value::Length(Length::Calc(i)) => styles.calc(i),
            v => panic!("{v:?}"),
        };
        assert_eq!(width("a"), (-1.0, 100.0));
        assert_eq!(width("b"), (-2.0, 10.0));
    }

    #[test]
    fn var_reads_custom_properties() {
        let page = "<div id=a><p id=b>x</p><p id=c class=other>y</p></div>";
        let sheet = ":root { --main: red; --gap: 4px } div { color: var(--main); margin: var(--gap) calc(var(--gap) * 2) }\
            p { padding-left: var(--gap); border-top: var(--w, 3px) solid var(--main) } .other { --main: blue; --gap: 10px }";
        let (doc, styles) = styles(page, &[sheet]);
        let get = |id: &str, p: Property| styles.style(by_id(&doc, id)).unwrap().get(p);
        let px = |x| Value::Length(Length::Px(x));
        assert_eq!(get("a", Property::TextColor), rgb(255, 0, 0));
        // `margin` with `var()` gives the four sides, and `calc()` can be in it.
        assert_eq!(get("a", Property::MarginTop), px(4.0));
        assert_eq!(get("a", Property::MarginLeft), px(8.0));
        // Custom properties are inherited, and a child may set its own.
        assert_eq!(get("b", Property::PaddingLeft), px(4.0));
        assert_eq!(get("c", Property::PaddingLeft), px(10.0));
        // The fallback, and `var()` in a shorthand.
        assert_eq!(get("b", Property::BorderTopWidth), px(3.0));
        assert_eq!(get("b", Property::BorderTopColor), rgb(255, 0, 0));
        assert_eq!(get("c", Property::BorderTopColor), rgb(0, 0, 255));
        // `color` is inherited: the child has the color of `div`, which used the custom property of `div`.
        assert_eq!(get("c", Property::TextColor), rgb(255, 0, 0));
        // The sets are shared: `b` has the set of `div`, and `div` has the set of the root.
        let set = |id: &str| styles.custom_properties(styles.style(by_id(&doc, id)).unwrap()).to_vec();
        assert_eq!(set("a"), set("b"));
        assert_eq!(set("c").iter().map(|(n, v)| (&**n, &**v)).collect::<Vec<_>>(), [("--gap", "10px"), ("--main", "blue")]);
    }

    #[test]
    fn var_that_does_not_work_is_unset() {
        let page = "<div id=a><p id=b>x</p></div>";
        let sheet = "div { color: red; width: 5px; --loop1: var(--loop2); --loop2: var(--loop1); --bad: 12 } \
            p { color: var(--nope); width: var(--loop1); height: var(--bad); line-height: var(--bad); margin-top: var(--bad, 7px) }";
        let (doc, styles) = styles(page, &[sheet]);
        let get = |p: Property| styles.style(by_id(&doc, "b")).unwrap().get(p);
        // `color` is inherited: `unset` gives the color of the parent.
        assert_eq!(get(Property::TextColor), rgb(255, 0, 0));
        // `width` is not inherited: `unset` gives the starting value.
        assert_eq!(get(Property::Width), ComputedStyle::initial().get(Property::Width));
        // A circle is not set, `12` is not a good height, and `12` is a good line height.
        assert_eq!(get(Property::Height), ComputedStyle::initial().get(Property::Height));
        assert_eq!(get(Property::LineHeight), Value::Number(12.0));
        // The fallback is used only when the property is not set. `--bad` is set.
        assert_eq!(get(Property::MarginTop), ComputedStyle::initial().get(Property::MarginTop));
    }

    #[test]
    fn var_follows_the_cascade() {
        let page = "<p id=a class=x style='--c: lime'>x</p><p id=b class=x>y</p>";
        let sheet = ".x { --c: red; color: var(--c) } p#a { --c: blue !important } p { --c: black }\
            #b { color: var(--c) !important; color: green }";
        let (doc, styles) = styles(page, &[sheet]);
        let color = |id: &str| styles.style(by_id(&doc, id)).unwrap().get(Property::TextColor);
        // `!important` beats the `style` attribute. The specificity decides between `.x` and `p`.
        assert_eq!(color("a"), rgb(0, 0, 255));
        assert_eq!(color("b"), rgb(255, 0, 0));
    }

    #[test]
    fn var_in_the_style_attribute() {
        let (doc, styles) = styles("<p id=a style='--w: 7px; width: var(--w); height: calc(100% - var(--w))'>x</p>", &[]);
        let get = |p: Property| styles.style(by_id(&doc, "a")).unwrap().get(p);
        assert_eq!(get(Property::Width), Value::Length(Length::Px(7.0)));
        let Value::Length(Length::Calc(i)) = get(Property::Height) else { panic!() };
        assert_eq!(styles.calc(i), (-7.0, 100.0));
    }

    /// The color of `<p id=p class=c>` with these style sheets.
    fn layered(sheet: &str) -> Value {
        get("<p id=p class=c style='--x: 1'>x</p>", &[sheet], "p", Property::TextColor)
    }

    #[test]
    fn layers_beat_specificity_and_the_last_layer_wins() {
        let red = rgb(255, 0, 0);
        let blue = rgb(0, 0, 255);
        let black = Value::Color(Color::BLACK);
        // A later layer wins, also against a more specific selector.
        assert_eq!(layered("@layer a, b; @layer a { #p { color: red } } @layer b { p { color: blue } }"), blue);
        assert_eq!(layered("@layer a { p { color: red } } @layer b { p { color: blue } }"), blue);
        // The order of the statement wins over the order of the blocks.
        assert_eq!(layered("@layer b, a; @layer a { p { color: red } } @layer b { p { color: blue } }"), red);
        // A layer that is opened again keeps its place.
        assert_eq!(layered("@layer a { p { color: red } } @layer b { p { color: blue } } @layer a { p { color: lime } }"), blue);
        // No layer beats every layer, also with less specificity.
        assert_eq!(layered("p { color: red } @layer a { #p { color: blue } }"), red);
        assert_eq!(layered("@layer a { #p { color: blue } } p { color: red }"), red);
        // Inside a layer, specificity and order work as always.
        assert_eq!(layered("@layer a { #p { color: red } p { color: blue } }"), red);
        // `@layer` in `@media`, and a layer with no name.
        assert_eq!(layered("@layer a { p { color: red } } @media screen { @layer b { p { color: blue } } }"), blue);
        assert_eq!(layered("@layer { p { color: red } } @layer { p { color: blue } }"), blue);
        assert_eq!(layered("@layer a { p { color: red } } @layer a, b; "), red);
        // Not a good name: the block is dropped, and a list in a block too.
        assert_eq!(layered("@layer a b { p { color: red } }"), black);
        assert_eq!(layered("@layer a, b { p { color: red } }"), black);
        assert_eq!(layered("@layer a..b { p { color: red } }"), black);
    }

    #[test]
    fn nested_layers() {
        let (red, blue, lime) = (rgb(255, 0, 0), rgb(0, 0, 255), rgb(0, 255, 0));
        // The rules of a layer win over its own sub-layers. A sub-layer is in the order of its parent.
        assert_eq!(layered("@layer a { @layer x { p { color: red } } p { color: blue } }"), blue);
        assert_eq!(layered("@layer a { p { color: blue } @layer x { p { color: red } } }"), blue);
        assert_eq!(layered("@layer a.x { p { color: red } } @layer b { p { color: blue } }"), blue);
        assert_eq!(layered("@layer b { p { color: blue } } @layer a.x { p { color: red } }"), red);
        assert_eq!(layered("@layer a { @layer x, y; @layer y { p { color: lime } } @layer x { p { color: red } } }"), lime);
        // A name with more than 32 parts is not good. It is not slow, and it does not overflow the stack.
        let deep = |n| format!("@layer {} {{ p {{ color: red }} }}", vec!["a"; n].join("."));
        assert_eq!(layered(&deep(32)), red);
        assert_eq!(layered(&deep(100_000)), Value::Color(Color::BLACK));
    }

    #[test]
    fn important_turns_the_layers_around() {
        let (red, blue, lime) = (rgb(255, 0, 0), rgb(0, 0, 255), rgb(0, 255, 0));
        // `!important`: the earlier layer wins, and it beats a later one and no layer.
        assert_eq!(layered("@layer a { p { color: red !important } } @layer b { p { color: blue !important } }"), red);
        assert_eq!(layered("@layer a { p { color: red !important } } p { color: blue !important }"), red);
        // Important beats normal, in any layer.
        assert_eq!(layered("@layer a { p { color: red !important } } @layer b { p { color: blue } } p { color: lime }"), red);
        // An attribute `style=""` beats the layers, for normal and for important.
        let page = "<p id=p style='color: lime'>x</p>";
        assert_eq!(get(page, &["@layer a { p { color: red } }"], "p", Property::TextColor), lime);
        assert_eq!(get(page, &["p { color: red }"], "p", Property::TextColor), lime);
        let page = "<p id=p style='color: lime !important'>x</p>";
        assert_eq!(get(page, &["@layer a { p { color: red !important } }"], "p", Property::TextColor), lime);
        let _ = blue;
    }

    #[test]
    fn layers_work_for_custom_properties_and_imports() {
        let (red, blue) = (rgb(255, 0, 0), rgb(0, 0, 255));
        assert_eq!(layered("@layer a { p { --c: red } } @layer b { p { --c: blue } } p { color: var(--c) }"), blue);
        assert_eq!(layered("@layer b, a; @layer a { p { --c: red } } @layer b { p { --c: blue } } p { color: var(--c) }"), red);
        let mut doc = parse("<p id=p>x</p>");
        let mut rules = RuleSet::new(VIEW);
        rules.add_in_layer(&mut doc, "p { color: red }", Origin::Author, Some("lib"));
        rules.add(&mut doc, "@layer lib { p { color: lime } } p { color: blue }", Origin::Author);
        let styles = rules.compute(&doc);
        // The layer `lib` is the one of the first sheet, and the rule with no layer wins.
        assert_eq!(styles.style(by_id(&doc, "p")).unwrap().get(Property::TextColor), blue);
        let _ = red;
    }
}
