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
//! `font-style`, `line-height`, `text-align`, `text-transform`, `white-space`, `list-style-type`, `visibility`, `direction`.
//! The others start at their starting value.
//!
//! Left out:
//! - `@media`, `@supports`, `@import` and other at-rules: the rules inside are not used. (A style sheet for `print` would apply, too,
//!   if the app gave it. The app skips `<style media>` and `<link media>` that do not say `all` or `screen`.)
//! - `:visited`: it never matches, see `selector.rs`. `:hover`, `:active` and the focus pseudo-classes match the state in `Document::user`.
//! - `larger` and `smaller` are 1.2 and 1/1.2 of the parent size. Chrome uses a table of sizes for the keywords.
//! - The parent of a `display: none` element does not change anything: the style is not shared with it.
//!
//! Comments compare Rust with JavaScript, because you know JS.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::BuildHasherDefault;

use bare_html::dom::{Atom, Document, Fnv, Mark, NodeData, NodeId};

use crate::at_rules;
use crate::calc::Linear;
use crate::color::Color;
use crate::grid::{self, Grid};
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
    /// The user action state that some selector reads (`selector::USES_*`).
    user: u8,
    /// Where the selectors read each kind of state (`Scopes`).
    scopes: Scopes,
}

/// Where a selector reads a state, seen from the element that it styles. The order is the cost of a change of the state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// No selector reads it.
    #[default]
    None,
    /// Only on the element itself (the last compound): a change restyles the element, and its subtree only if its style changes.
    Own,
    /// On an ancestor too (a compound to the left, after a space or `>`): a change restyles the subtree of the element.
    Subtree,
    /// On a sibling of the element or of an ancestor (after a `+` or `~`): a change restyles the subtree of the parent.
    Siblings,
}

/// The widest `Scope` of each kind of state, over all the selectors of a `RuleSet`. A host that restyles a part of the page
/// (`RuleSet::restyle`) picks the elements to restyle with it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Scopes {
    pub hover: Scope,
    pub active: Scope,
    pub focus: Scope,
    /// `:checked` and the other form pseudo-classes. Some read other elements (a `fieldset` above, the radio group).
    pub form: Scope,
    /// Ids, classes and attributes (`:link` reads `href`).
    pub attr: Scope,
    /// The place among the siblings: `:first-child`, `:nth-child()`, and any `+` or `~`.
    pub structure: Scope,
    /// `:empty`: the children of the element.
    pub empty: Scope,
}

impl Scopes {
    /// The roots of `RuleSet::restyle` (element, whole subtree) for one change of the tree. `false`: restyle the whole page.
    /// A linked node gets its styles. The place of its siblings changes for `:first-child` and `+`, and the parent may stop being
    /// `:empty`. A form pseudo-class reads the attributes of other elements (a `fieldset`, the radio group), so with one, an
    /// attribute or form change restyles all.
    pub fn roots(&self, doc: &Document, mark: Mark, out: &mut Vec<(NodeId, bool)>) -> bool {
        let children = |parent: NodeId, out: &mut Vec<(NodeId, bool)>| {
            if self.structure != Scope::None {
                out.push((parent, true));
            }
            Self::at(self.empty, parent, doc, out);
        };
        match mark {
            Mark::Link { parent, node } => {
                out.push((node, true));
                children(parent, out);
            }
            Mark::Unlink { parent, .. } => children(parent, out),
            Mark::Text { node } => {
                if let Some(p) = doc.parent(node) {
                    Self::at(self.empty, p, doc, out);
                }
            }
            Mark::Attr { el, .. } => {
                if self.form != Scope::None {
                    return false;
                }
                // The presentational hints of a cell read the table above it: the subtree, also with no selector on attributes.
                Self::at(self.attr.max(Scope::Subtree), el, doc, out);
            }
            Mark::Ui { .. } => return self.form == Scope::None,
        }
        true
    }

    /// The roots for a user state (`:hover`, `:active`, focus) read in `scope` that moved from the element `old[0]` to `new[0]`.
    /// `old` and `new` are the element and its ancestors (`:hover`, `:active` and `:focus-within` match them): the elements
    /// in one of them and not in the other change, and so do the two elements themselves (`:focus-visible` can change alone).
    pub fn user_roots(scope: Scope, old: &[NodeId], new: &[NodeId], doc: &Document, out: &mut Vec<(NodeId, bool)>) {
        if scope == Scope::None {
            return;
        }
        let ends = old.first().into_iter().chain(new.first());
        let diff = old.iter().filter(|n| !new.contains(n)).chain(new.iter().filter(|n| !old.contains(n)));
        for &e in ends.chain(diff) {
            Self::at(scope, e, doc, out);
        }
    }

    /// The roots for a change of the state of `e` that the selectors read in `scope`. After a `+` or `~`, the state of `e`
    /// reaches only the siblings after it, and their subtrees.
    fn at(scope: Scope, e: NodeId, doc: &Document, out: &mut Vec<(NodeId, bool)>) {
        match scope {
            Scope::None => {}
            Scope::Own => out.push((e, false)),
            Scope::Subtree => out.push((e, true)),
            Scope::Siblings => out.extend(std::iter::successors(Some(e), |&n| doc.next_sibling(n)).map(|n| (n, true))),
        }
    }

    fn add(&mut self, selector: &Selector) {
        use crate::selector::User;
        fn simples(me: &mut Scopes, list: &[Simple], at: Scope) {
            for s in list {
                let slot = match s {
                    Simple::User(User::Hover) => &mut me.hover,
                    Simple::User(User::Active) => &mut me.active,
                    Simple::User(_) => &mut me.focus,
                    Simple::Form(_) => &mut me.form,
                    Simple::Id(_) | Simple::Class(_) | Simple::Attr { .. } | Simple::Link => &mut me.attr,
                    Simple::FirstChild | Simple::LastChild | Simple::OnlyChild | Simple::Nth(_) => &mut me.structure,
                    Simple::Empty => &mut me.empty,
                    Simple::Not(args) | Simple::Is(args) => {
                        for a in args {
                            simples(me, a, at);
                        }
                        continue;
                    }
                    _ => continue,
                };
                *slot = (*slot).max(at);
            }
        }
        let mut at = Scope::Own;
        for (i, c) in selector.compounds.iter().enumerate() {
            if i > 0 {
                let way = match selector.compounds[i - 1].combinator {
                    Combinator::Next | Combinator::Later => Scope::Siblings,
                    _ => Scope::Subtree,
                };
                at = at.max(way);
                if way == Scope::Siblings {
                    self.structure = Scope::Siblings;
                }
            }
            simples(self, &c.simples, at);
        }
    }
}

impl RuleSet {
    /// The width and height of the window that the rules were made for.
    pub fn viewport(&self) -> (f32, f32) {
        self.viewport
    }

    pub fn new(viewport: (f32, f32)) -> RuleSet {
        RuleSet { viewport, entries: Vec::new(), decls: Vec::new(), side: Side::default(), layers: vec![Layer { parent: 0 }], layer_by_name: HashMap::new(), customs: Vec::new(), by_id: Buckets::default(), by_class: Buckets::default(), by_tag: Buckets::default(), other: Vec::new(), user: 0, scopes: Scopes::default() }
    }

    /// Which user action state (`:hover`, `:active`, the focus pseudo-classes) some rule reads: the bits `selector::USES_*`.
    /// A host that keeps styles must compute them again when one of those states changes.
    pub fn user_flags(&self) -> u8 {
        self.user
    }

    /// Where the selectors read each kind of state (`Scopes`).
    pub fn scopes(&self) -> Scopes {
        self.scopes
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
        self.user |= crate::selector::user_flags(&selector);
        self.scopes.add(&selector);
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
        self.walk::<false>(doc, None, false)
    }

    /// `compute`, and the styles keep the cache of the cascade, so that `restyle` can update a part of them later.
    pub fn compute_kept(&self, doc: &Document) -> Styles {
        self.walk::<false>(doc, None, true)
    }

    /// The same, and the element `want` gets its style too, even inside `display:none` (for `getComputedStyle`: CSSOM gives the
    /// computed values of an element that is not rendered). Only the elements from the root down to `want` are styled inside a
    /// hidden subtree, so the cost of a hidden read is the depth of `want`. Layout asks with `None` and pays nothing: the walk
    /// below changes only at a `display:none` element and on the way from it to `want`.
    /// An element that the walk does not reach (a light child of a shadow host: no slots) still has no style.
    pub fn compute_for(&self, doc: &Document, want: Option<NodeId>) -> Styles {
        if want.is_some() { self.walk::<true>(doc, want, false) } else { self.walk::<false>(doc, None, false) }
    }

    /// `WANT` is a const so that the walk for layout (`false`) has none of the code for `want`: it is the same machine code as before.
    #[inline(always)]
    fn walk<const WANT: bool>(&self, doc: &Document, want: Option<NodeId>, keep: bool) -> Styles {
        let mut way = Way { want, path: Vec::new(), sorted: Vec::new(), on: false, at: 0 };
        let mut styles = Styles { styles: vec![ComputedStyle::initial()], of_node: vec![NONE; doc.len()], calcs: Vec::new(), grids: Vec::new(), customs: Vec::new(), spaced: false, kept: None };
        // Style 0 is the starting style. It is the parent style of the `html` element.
        let mut cx = Cx { viewport: self.viewport, root_font_size: 16.0, layer_order: self.layer_order(), declared: &self.side, inline: RefCell::default(), computed: RefCell::default(), grids: RefCell::default(), customs: RefCell::default() };
        let style_atom = doc.find_atom("style");
        let hints = crate::hints::Hints::new(doc);
        let mut filter = AncestorFilter::new();
        let mut memo = Memo::default();
        let (mut candidates, mut matched): (Vec<u32>, Vec<u32>) = (Vec::new(), Vec::new());
        let mut stack = vec![(doc.root(), false, false)];
        // The walk is a loop with a stack, and not a function that calls itself: a deep tree cannot overflow the call stack.
        // `true` means that we leave the node, and we take its hashes out of the filter.
        // It walks the flat tree: a host's children are those of its shadow root. The third value says the node is in a shadow tree.
        // There, only the browser's rules match: the page's rules do not reach into a shadow tree (Stylo, Blink), and the
        // shadow tree's own sheets are not built (the geometry module refuses a shadow tree that has one).
        while let Some((node, leaving, shadow)) = stack.pop() {
            match doc.data(node) {
                NodeData::Element { .. } if leaving => {
                    element_hashes(doc, node, &mut |h| filter.remove(h));
                    continue;
                }
                NodeData::Element { .. } => {
                    let parent = doc.flat_parent(node).map_or(0, |p| styles.of_node[p.index()]);
                    let parent = if parent == NONE { 0 } else { parent };
                    self.candidates(doc, node, &mut candidates);
                    if shadow {
                        // Outside the hot loop: a light-tree element pays nothing for this.
                        candidates.retain(|&i| self.entries[i as usize].origin == Origin::UserAgent);
                    }
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
                    let hint = hints.as_ref().and_then(|h| h.of(doc, node));
                    let id = self.style_of(&mut styles, &mut memo, parent, &matched, (inline, hint.as_deref()), &cx);
                    styles.of_node[node.index()] = id;
                    let hidden = styles.styles[id as usize].get(Property::Display) == Value::Keyword(Keyword::None);
                    if hidden {
                        // No style for what is inside, and no filter work. `compute_for` goes on only to the node on the way to `want`.
                        if WANT {
                            way.visit(doc, node, id, true, shadow, (&styles, &mut cx, &mut filter, &mut stack));
                        }
                        continue;
                    }
                    if WANT && way.on {
                        way.visit(doc, node, id, false, shadow, (&styles, &mut cx, &mut filter, &mut stack));
                        continue;
                    }
                    if doc.parent(node) == Some(doc.root()) {
                        // The root element. `rem` is a multiple of its font size.
                        cx.root_font_size = styles.styles[id as usize].px(Property::FontSize);
                    }
                    element_hashes(doc, node, &mut |h| filter.add(h));
                    stack.push((node, true, shadow));
                }
                NodeData::Document => {}
                _ => continue,
            }
            // The children, so that the first child comes out of the stack first.
            let root = doc.shadow_root(node);
            let mut child = doc.last_child(root.unwrap_or(node));
            while let Some(c) = child {
                stack.push((c, false, shadow || root.is_some()));
                child = doc.prev_sibling(c);
            }
        }
        let (computed, mut grids, customs) = (cx.computed.take(), cx.grids.take(), cx.customs.take());
        styles.calcs = computed.list;
        styles.grids = std::mem::take(&mut grids.list);
        styles.customs = customs.list;
        if keep {
            styles.kept = Some(Box::new(StyleKeep { memo, calcs: computed.index, grids, customs: customs.index, rem: cx.root_font_size, base: styles.styles.len() }));
        }
        styles
    }

    /// Compute again the styles of a part of the page, in `styles` from `compute_kept` of this set and this `doc` (the tree may
    /// have changed since). For each root: `(el, false)` styles `el`, and its subtree only if its style changes; `(el, true)`
    /// styles its whole subtree. The elements whose style number changed go to `changed`, with the old number (`Styles::changed`).
    /// A new node gets its style here, as a part of the subtree of a root. Gives `false` when it cannot (the styles keep no
    /// cache, the font size of the root element changes, the page has a shadow root, or the styles grew too much): then the caller runs
    /// `compute_kept`; no node has a new style then. Equal style numbers mean equal values, so a style that did not change keeps its number.
    pub fn restyle(&self, doc: &Document, styles: &mut Styles, roots: &[(NodeId, bool)], changed: &mut Vec<(NodeId, u32)>) -> bool {
        let html = doc.children(doc.root()).find(|&n| matches!(doc.data(n), NodeData::Element { .. }));
        let Some(mut k) = styles.kept.take() else { return false };
        // A style that a restyle makes stays in the list: a page that makes new ones all the time starts again from time to time.
        if styles.styles.len() > 2 * k.base + 4096 || doc.has_shadow_roots() {
            return false;
        }
        if styles.of_node.len() < doc.len() {
            styles.of_node.resize(doc.len(), NONE);
        }
        let cx = Cx {
            viewport: self.viewport,
            root_font_size: k.rem,
            layer_order: self.layer_order(),
            declared: &self.side,
            inline: RefCell::default(),
            computed: RefCell::new(Computed { list: std::mem::take(&mut styles.calcs), index: std::mem::take(&mut k.calcs) }),
            grids: RefCell::new({
                let mut g = std::mem::take(&mut k.grids);
                g.list = std::mem::take(&mut styles.grids);
                g
            }),
            customs: RefCell::new(Customs { list: std::mem::take(&mut styles.customs), index: std::mem::take(&mut k.customs) }),
        };
        let style_atom = doc.find_atom("style");
        let hints = crate::hints::Hints::new(doc);
        let mut ok = true;
        // Parents first: a root inside a subtree that is styled again by an earlier root costs a little more, and is still right.
        let mut order: Vec<(usize, NodeId, bool)> = roots.iter().map(|&(n, sub)| (std::iter::successors(Some(n), |&p| doc.flat_parent(p)).count(), n, sub)).collect();
        order.sort_unstable_by_key(|r| (r.0, r.1.index()));
        let (mut candidates, mut matched): (Vec<u32>, Vec<u32>) = (Vec::new(), Vec::new());
        let mut stack: Vec<(NodeId, bool)> = Vec::new();
        'roots: for &(_, root, subtree) in &order {
            if !matches!(doc.data(root), NodeData::Element { .. }) {
                continue;
            }
            // Only a connected root that is shown: inside `display: none` all styles are `NONE`, and the hidden element has its own.
            let mut filter = AncestorFilter::new();
            let mut up = doc.flat_parent(root);
            while let Some(a) = up {
                if a == doc.root() {
                    break;
                }
                let s = styles.of_node[a.index()];
                if s == NONE || styles.styles[s as usize].get(Property::Display) == Value::Keyword(Keyword::None) {
                    continue 'roots;
                }
                element_hashes(doc, a, &mut |h| filter.add(h));
                up = doc.flat_parent(a);
            }
            if up.is_none() {
                continue;
            }
            stack.clear();
            stack.push((root, false));
            while let Some((node, leaving)) = stack.pop() {
                if !matches!(doc.data(node), NodeData::Element { .. }) {
                    continue;
                }
                if leaving {
                    element_hashes(doc, node, &mut |h| filter.remove(h));
                    continue;
                }
                let parent = doc.flat_parent(node).map_or(0, |p| styles.of_node[p.index()]);
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
                let inline = style_atom.and_then(|a| doc.attr(node, a));
                let hint = hints.as_ref().and_then(|h| h.of(doc, node));
                let id = self.style_of(styles, &mut k.memo, parent, &matched, (inline, hint.as_deref()), &cx);
                if Some(node) == html && styles.styles[id as usize].px(Property::FontSize) != k.rem {
                    // `rem` changes: the cache of the cascade does not see it. The root element is the first root, so no node has a new style yet.
                    ok = false;
                    break 'roots;
                }
                let old = std::mem::replace(&mut styles.of_node[node.index()], id);
                if old != id {
                    changed.push((node, old));
                }
                if styles.styles[id as usize].get(Property::Display) == Value::Keyword(Keyword::None) {
                    // Nothing inside has a style now.
                    if old != NONE && styles.styles[old as usize].get(Property::Display) != Value::Keyword(Keyword::None) {
                        let mut gone: Vec<NodeId> = doc.children(node).collect();
                        while let Some(n) = gone.pop() {
                            if std::mem::replace(&mut styles.of_node[n.index()], NONE) != NONE {
                                gone.extend(doc.children(n));
                            }
                        }
                    }
                    continue;
                }
                if node == root && !subtree && old == id {
                    continue;
                }
                element_hashes(doc, node, &mut |h| filter.add(h));
                stack.push((node, true));
                let mut child = doc.last_child(node);
                while let Some(c) = child {
                    stack.push((c, false));
                    child = doc.prev_sibling(c);
                }
            }
        }
        styles.calcs = std::mem::take(&mut cx.computed.borrow_mut().list);
        k.calcs = std::mem::take(&mut cx.computed.borrow_mut().index);
        let mut g = cx.grids.take();
        styles.grids = std::mem::take(&mut g.list);
        k.grids = g;
        let c = cx.customs.take();
        (styles.customs, k.customs) = (c.list, c.index);
        styles.kept = Some(k);
        ok
    }

    /// The rules that may match the element: the ones in the lists for its id, its classes and its tag, and the "other" list.
    #[inline(always)]
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
    #[inline(always)]
    fn style_of(&self, styles: &mut Styles, memo: &mut Memo, parent: u32, matched: &[u32], (inline, hint): (Option<&str>, Option<&str>), cx: &Cx<'_>) -> u32 {
        let mut key = matched.iter().fold(u64::from(parent), |h, &i| (h ^ u64::from(i)).wrapping_mul(0x0100_0000_01b3));
        // The hint is part of the key: the cells of one table share their hint, and so their style.
        if let Some(h) = hint {
            key = h.bytes().fold(key, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
        }
        if inline.is_none() {
            let found = memo.cache.get(&key).and_then(|list| list.iter().find(|c| c.parent == parent && *c.rules == *matched && c.hint.as_deref() == hint));
            if let Some(found) = found {
                return found.style;
            }
        }
        let style = self.cascade(matched, inline, hint, &styles.styles[parent as usize], cx);
        // Two different rule lists may give the same style (`div div` inside `div`). Then they share it, and the children of both
        // have the same parent style, so they find each other in the cache.
        let hash = style_hash(&style);
        let same = memo.by_hash.get(&hash).and_then(|list| list.iter().copied().find(|&i| styles.styles[i as usize] == style));
        let id = same.unwrap_or_else(|| {
            let id = styles.styles.len() as u32;
            styles.spaced |= style.px(Property::LetterSpacing) != 0.0 || style.px(Property::WordSpacing) != 0.0;
            styles.styles.push(style);
            memo.by_hash.entry(hash).or_default().push(id);
            id
        });
        if inline.is_none() {
            memo.cache.entry(key).or_default().push(CacheEntry { parent, rules: matched.into(), hint: hint.map(Into::into), style: id });
        }
        id
    }

    /// Pick the winning declaration for each property, then compute the values.
    fn cascade(&self, matched: &[u32], inline: Option<&str>, hint: Option<&str>, parent: &ComputedStyle, cx: &Cx<'_>) -> ComputedStyle {
        let mut winners = [None::<Value>; Property::COUNT];
        let mut keys = [0u128; Property::COUNT];
        // The properties that have a winner, one bit each: the loops below visit only these.
        let mut set = 0u128;
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
        // The presentational hints (`hints.rs`): author declarations that any rule beats. Their key is the least of the author origin.
        if let Some(text) = hint {
            let mut out = Vec::new();
            for d in parse_declarations(text) {
                parse_declaration(&d.name, d.value, false, &mut Side::default(), &mut out);
            }
            for d in &out {
                let p = d.property as usize;
                winners[p] = Some(d.value);
                keys[p] = 1 << 96;
                set |= 1 << p;
            }
        }
        let mut apply = |d: &Declared, origin: Origin, layer: Option<u32>, specificity: u32, order: u32| {
            let key = key(d.important, origin, layer, specificity, order);
            let p = d.property as usize;
            if winners[p].is_none() || key >= keys[p] {
                winners[p] = Some(d.value);
                keys[p] = key;
                set |= 1 << p;
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
            shared.grids.clear();
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
            let (calc_base, var_base, grid_base) = (cx.declared.calcs.len() as u32, cx.declared.vars.len() as u32, cx.declared.grids.len() as u32);
            for d in &mut inline_decls {
                match &mut d.value {
                    Value::Length(Length::Calc(i)) => *i += calc_base,
                    Value::Var(i) => *i += var_base,
                    Value::Grid(i) => *i += grid_base,
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
        let mut rest = set;
        while rest != 0 {
            let p = rest.trailing_zeros() as usize;
            rest &= rest - 1;
            if let Some(Value::Var(i)) = winners[p] {
                winners[p] = Some(self.read_var(i, Property::ALL[p], custom, cx));
            }
        }
        let mut style = compute(&winners, set, parent, cx);
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
        if let Value::Grid(g) = &mut value {
            let mut shared = cx.inline.borrow_mut();
            *g += (cx.declared.grids.len() + shared.grids.len()) as u32;
            shared.grids.extend(side.grids);
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
    hint: Option<Box<str>>,
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
            Value::Grid(i) => 8 << 40 | u64::from(i),
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
    /// The grid lists that are done, and are kept in `Styles::grids`.
    grids: RefCell<grid::Kept>,
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

    /// The grid list `i` of the style sheets or of the `style=""` attribute.
    fn declared_grid(&self, i: u32) -> Grid {
        match self.declared.grids.get(i as usize) {
            Some(g) => g.clone(),
            None => self.inline.borrow().grids[i as usize - self.declared.grids.len()].clone(),
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

/// The way from the root to the element that `compute_for` was asked about. It is made at the first `display:none` element.
struct Way {
    want: Option<NodeId>,
    /// The flat ancestors of `want` from the root down to `want`. Empty when `want` is not connected, or not made yet.
    path: Vec<NodeId>,
    /// The places (`NodeId::index`) of `path`, sorted: is a node on the path?
    sorted: Vec<u32>,
    /// The walk is on the way from a `display:none` element to `want`: only the nodes of `path` are walked.
    on: bool,
    /// The place of the node that is walked now in `path`, while `on`.
    at: usize,
}

impl Way {
    /// A `display:none` element, or a node on the way from one to `want`, got its style. `None`: nothing is below it to style.
    /// `Some(next)`: style `next`, the next node on the way, and nothing else below this node.
    /// A `display:none` element, or a node on the way to `want`, was styled: go on to the next node on the way, if there is one.
    #[cold]
    #[inline(never)]
    fn visit(&mut self, doc: &Document, node: NodeId, id: u32, hidden: bool, shadow: bool, (styles, cx, filter, stack): (&Styles, &mut Cx<'_>, &mut AncestorFilter, &mut Vec<(NodeId, bool, bool)>)) {
        let Some(next) = self.step(doc, node, hidden) else { return };
        if doc.parent(node) == Some(doc.root()) {
            cx.root_font_size = styles.styles[id as usize].px(Property::FontSize);
        }
        element_hashes(doc, node, &mut |h| filter.add(h));
        stack.push((node, true, shadow));
        stack.push((next, false, shadow || doc.shadow_root(node).is_some()));
    }

    #[cold]
    fn step(&mut self, doc: &Document, node: NodeId, hidden: bool) -> Option<NodeId> {
        if !self.on {
            let want = self.want?;
            if self.path.is_empty() {
                let mut up = Some(want);
                while let Some(n) = up {
                    self.path.push(n);
                    up = doc.flat_parent(n);
                }
                self.path.reverse();
                if self.path[0] != doc.root() {
                    self.path.clear();
                    self.path.push(want);
                    // Not connected: no node of the walk is on this path.
                    self.sorted.clear();
                } else {
                    self.sorted = self.path.iter().map(|n| n.index() as u32).collect();
                    self.sorted.sort_unstable();
                }
            }
            debug_assert!(hidden);
            if self.sorted.binary_search(&(node.index() as u32)).is_err() {
                return None;
            }
            self.at = self.path.iter().position(|&n| n == node)?;
            self.on = true;
        }
        if self.at + 1 == self.path.len() {
            // `want` itself: its children are not styled.
            self.on = false;
            return None;
        }
        self.at += 1;
        Some(self.path[self.at])
    }
}

/// The styles of a document.
pub struct Styles {
    /// The different styles. Many elements share one.
    styles: Vec<ComputedStyle>,
    /// For each node (by `NodeId::index`): the number of its style in `styles`, or `NONE`.
    of_node: Vec<u32>,
    /// The `calc()` values that mix units, after the cascade: pixels and percent. A `Length::Calc` in a style points here.
    calcs: Vec<[f32; 2]>,
    /// The grid lists (`grid.rs`), after the cascade. A `Value::Grid` in a style points here.
    grids: Vec<Grid>,
    /// The sets of custom properties. `ComputedStyle::1` is a place in this list. Set 0 is empty.
    customs: Vec<var::Set>,
    /// Does any style have a `letter-spacing` or a `word-spacing` that is not 0? Layout and paint read the two only then.
    spaced: bool,
    /// The cache of the cascade, for `RuleSet::restyle`. Only `RuleSet::compute_kept` keeps it.
    kept: Option<Box<StyleKeep>>,
}

/// What `RuleSet::restyle` needs from the cascade that made the styles: the style cache and the indexes of the value lists, so
/// that the same values get the same numbers.
struct StyleKeep {
    memo: Memo,
    calcs: HashMap<[u32; 2], u32, BuildHasherDefault<Fnv>>,
    grids: grid::Kept,
    customs: HashMap<var::Set, u32, BuildHasherDefault<Fnv>>,
    /// The font size of the root element, for `rem`.
    rem: f32,
    /// How many styles the full cascade made.
    base: usize,
}

impl Styles {
    pub fn spaced(&self) -> bool {
        self.spaced
    }

    /// The custom properties of a style, sorted by name, with their text: `--gap`, `8px`.
    pub fn custom_properties(&self, style: &ComputedStyle) -> &[(Box<str>, Box<str>)] {
        &self.customs[style.1 as usize]
    }

    /// The pixels and the percent of a `Length::Calc` of a computed style: `calc(px + percent of something)`.
    pub fn calc(&self, i: u32) -> (f32, f32) {
        let [px, pct] = self.calcs[i as usize];
        (px, pct)
    }

    /// The grid list that a `Value::Grid` of a computed style points to.
    pub fn grid(&self, i: u32) -> &Grid {
        &self.grids[i as usize]
    }

    /// The style of an element. `None` for a node that is not an element, and for an element that is not shown
    /// (`display: none`, or inside one), unless it was asked for with `RuleSet::compute_for`.
    pub fn style(&self, node: NodeId) -> Option<&ComputedStyle> {
        match self.of_node[node.index()] {
            NONE => None,
            i => Some(&self.styles[i as usize]),
        }
    }

    /// The different styles, without the order of the nodes. A question about the whole page ("does any style use
    /// `z-index`?") costs one look at each style, not one at each node.
    pub fn each(&self) -> impl Iterator<Item = &ComputedStyle> {
        self.styles.iter()
    }

    /// How many nodes the styles were made for: the length of the document then.
    pub fn nodes(&self) -> usize {
        self.of_node.len()
    }

    /// How many different styles there are. Style 0 (the starting style) is not counted.
    pub fn unique(&self) -> usize {
        self.styles.len() - 1
    }

    /// How the cascade `new` of the same tree differs from this one, for a host that keeps a layout. `Same`: every node has the same
    /// values. `Paint`: only values that layout does not read differ (`Change::paint_only`). `Layout`: any other difference. The
    /// custom property sets are not compared: layout and paint do not read them.
    pub fn change(&self, new: &Styles) -> Change {
        if self.of_node.len() != new.of_node.len() {
            return Change::Layout;
        }
        // The style of `new` that each style of `self` was last found equal to: most nodes share styles, so most need no compare.
        let mut seen = vec![NONE; self.styles.len()];
        let mut out = Change::Same;
        for (&a, &b) in self.of_node.iter().zip(&new.of_node) {
            if a == NONE || b == NONE {
                if a != b {
                    return Change::Layout;
                }
                continue;
            }
            if seen[a as usize] == b {
                continue;
            }
            match self.differ(a, new, b) {
                Change::Layout => return Change::Layout,
                Change::Paint => out = Change::Paint,
                Change::Same => {}
            }
            seen[a as usize] = b;
        }
        out
    }

    /// How the style `old` of these styles differs from the style that `node` has now (after `RuleSet::restyle`). A node that has no
    /// style before or after changes its layout.
    pub fn change_of(&self, old: u32, node: NodeId) -> Change {
        let new = self.of_node.get(node.index()).copied().unwrap_or(NONE);
        match (old, new) {
            _ if old == new => Change::Same,
            (NONE, _) | (_, NONE) => Change::Layout,
            _ => self.differ(old, self, new),
        }
    }

    /// How the style `b` of `new` differs from the style `a` of these styles.
    #[inline(always)]
    fn differ(&self, a: u32, new: &Styles, b: u32) -> Change {
        let (x, y) = (&self.styles[a as usize].0, &new.styles[b as usize].0);
        let mut out = Change::Same;
        for (i, &p) in Property::ALL.iter().enumerate() {
            let (u, v) = (x[i], y[i]);
            let same = match (u, v) {
                (Value::Length(Length::Calc(i)), Value::Length(Length::Calc(j))) => self.calcs[i as usize] == new.calcs[j as usize],
                (Value::Grid(i), Value::Grid(j)) => self.grids[i as usize] == new.grids[j as usize],
                _ => u == v,
            };
            if !same {
                if !Change::paint_only(p, u, v, y[Property::Display as usize]) {
                    return Change::Layout;
                }
                out = Change::Paint;
            }
        }
        out
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
                                out.push_str(&format!("{sep}{}: {}", p.name(), show(style.get(p), &self.calcs, &self.grids)));
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

/// How two cascades of one tree differ (`Styles::change`). The order is the cost: a later one needs more work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    Same,
    Paint,
    Layout,
}

impl Change {
    /// Layout does not read `p` when it goes from `a` to `b` on an element with `display`: colors, decoration, opacity, `z-index`,
    /// `visibility`, `pointer-events`, SVG paint and corner radii. Inline layout reads whether the background of a `display: inline` box is transparent.
    pub fn paint_only(p: Property, a: Value, b: Value, display: Value) -> bool {
        use Property::*;
        match p {
            TextColor | BorderTopColor | BorderRightColor | BorderBottomColor | BorderLeftColor | TextDecoration | Opacity | ZIndex | Visibility | PointerEvents | Fill | Stroke
            | BorderTopLeftRadius | BorderTopRightRadius | BorderBottomRightRadius | BorderBottomLeftRadius => true,
            BackgroundColor => display != Value::Keyword(Keyword::Inline) || matches!((a, b), (Value::Color(x), Value::Color(y)) if (x.a == 0) == (y.a == 0)),
            _ => false,
        }
    }
}

/// A computed value as text.
fn show(v: Value, calcs: &[[f32; 2]], grids: &[Grid]) -> String {
    match v {
        Value::Color(c) if c.a == 255 => format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b),
        Value::Color(c) => format!("#{:02x}{:02x}{:02x}{:02x}", c.r, c.g, c.b, c.a),
        Value::Length(Length::Px(x)) => format!("{x}px"),
        Value::Length(Length::Percent(x)) => format!("{x}%"),
        Value::Length(Length::Calc(i)) => format!("calc({}px + {}%)", calcs[i as usize][0], calcs[i as usize][1]),
        Value::Number(x) => format!("{x}"),
        Value::Keyword(k) => k.name().to_string(),
        Value::Grid(i) => format!("{:?}", grids[i as usize]),
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
        Visibility | OverflowX | OverflowY => Value::Keyword(K::Visible),
        WhiteSpace | FontStyle | LineHeight | LetterSpacing | WordSpacing => Value::Keyword(K::Normal),
        TextAlign => Value::Keyword(K::Start),
        ListStyleType => Value::Keyword(K::Disc),
        BoxSizing => Value::Keyword(K::ContentBox),
        TextColor => Value::Color(Color::BLACK),
        BackgroundColor => Value::Color(Color::TRANSPARENT),
        FontSize => px(16.0),
        FontWeight => Value::Number(400.0),
        // `min-width: auto` is 0, except on a flex item: there it is the "automatic minimum size" (`flex.rs`).
        Width | Height | MinWidth | MinHeight | Top | Right | Bottom | Left | AspectRatio => Value::Keyword(K::Auto),
        Position => Value::Keyword(K::Static),
        FlexDirection => Value::Keyword(K::Row),
        FlexWrap => Value::Keyword(K::Nowrap),
        FlexGrow | Order => Value::Number(0.0),
        FlexShrink => Value::Number(1.0),
        FlexBasis => Value::Keyword(K::Auto),
        JustifyContent | AlignContent | AlignItems | JustifyItems | RowGap | ColumnGap => Value::Keyword(K::Normal),
        AlignSelf | JustifySelf | TableLayout | GridAutoColumns | GridAutoRows | GridRowStart | GridRowEnd | GridColumnStart | GridColumnEnd => Value::Keyword(K::Auto),
        GridTemplateColumns | GridTemplateRows | GridTemplateAreas => Value::Keyword(K::None),
        GridAutoFlow => Value::Number(0.0),
        BorderCollapse => Value::Keyword(K::Separate),
        CaptionSide => Value::Keyword(K::Top),
        VerticalAlign => Value::Keyword(K::Baseline),
        Direction => Value::Keyword(K::Ltr),
        ZIndex => Value::Keyword(K::Auto),
        Fill => Value::Color(Color::BLACK),
        Stroke => Value::Keyword(K::None),
        Opacity => Value::Number(1.0),
        Appearance => Value::Keyword(K::None),
        PointerEvents => Value::Keyword(K::Auto),
        BorderTopLeftRadius | BorderTopRightRadius | BorderBottomRightRadius | BorderBottomLeftRadius => px(0.0),
        BorderSpacingH | BorderSpacingV => px(0.0),
        MarginTop | MarginRight | MarginBottom | MarginLeft | PaddingTop | PaddingRight | PaddingBottom | PaddingLeft => px(0.0),
        BorderTopWidth | BorderRightWidth | BorderBottomWidth | BorderLeftWidth => px(3.0),
        // `medium`. With the style `none` it is 0: `compute` does that.
        BorderTopColor | BorderRightColor | BorderBottomColor | BorderLeftColor => Value::Keyword(K::CurrentColor),
    }
}

/// Does a child take the value of the parent when it has no value of its own?
const fn inherited(p: Property) -> bool {
    use Property::*;
    matches!(p, TextColor | FontSize | FontWeight | FontStyle | LineHeight | LetterSpacing | WordSpacing | TextAlign | TextTransform | WhiteSpace | ListStyleType | Visibility | PointerEvents | Fill | Stroke | BorderCollapse | BorderSpacingH | BorderSpacingV | CaptionSide | Direction)
}

/// The properties that `inherited` names, as bits.
const INHERITED: u128 = {
    let (mut bits, mut i) = (0, 0);
    while i < Property::COUNT {
        if inherited(Property::ALL[i]) {
            bits |= 1 << i;
        }
        i += 1;
    }
    bits
};

/// The initial value of each property.
fn start() -> &'static [Value; Property::COUNT] {
    static START: std::sync::LazyLock<[Value; Property::COUNT]> = std::sync::LazyLock::new(|| {
        let mut values = [Value::Initial; Property::COUNT];
        for &p in Property::ALL {
            values[p as usize] = initial(p);
        }
        values
    });
    &START
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
fn compute(winners: &[Option<Value>; Property::COUNT], mut set: u128, parent: &ComputedStyle, cx: &Cx<'_>) -> ComputedStyle {
    use Property::*;
    let fs = font_size(winners[FontSize as usize], parent.px(FontSize), cx);
    // A property with no winner keeps the initial value, or the one of the parent: only the others need a look.
    let mut out = *start();
    let mut inherit = INHERITED;
    while inherit != 0 {
        let i = inherit.trailing_zeros() as usize;
        inherit &= inherit - 1;
        out[i] = parent.0[i];
    }
    out[FontSize as usize] = Value::Length(Length::Px(fs));
    while set != 0 {
        let i = set.trailing_zeros() as usize;
        set &= set - 1;
        let (p, Some(v)) = (Property::ALL[i], winners[i]) else { continue };
        if p == FontSize {
            continue;
        }
        let raw = match v {
            // A grid list of the parent is computed already: its number is from `Styles::grids`, not from the style sheet.
            Value::Inherit if matches!(parent.0[i], Value::Grid(_)) => {
                out[i] = parent.0[i];
                continue;
            }
            Value::Inherit => parent.0[i],
            Value::Initial => initial(p),
            Value::Unset if inherited(p) => parent.0[i],
            Value::Unset => initial(p),
            v => v,
        };
        out[i] = resolve(p, raw, &out, parent, fs, cx);
    }
    // The initial border color is `currentcolor`: the color of the element.
    for p in [BorderTopColor, BorderRightColor, BorderBottomColor, BorderLeftColor] {
        if winners[p as usize].is_none() {
            out[p as usize] = Value::Color(as_color(out[TextColor as usize]));
        }
    }
    // A border with no style has no width. Chrome gives the others a whole number of pixels: under 1 px is 1 px, and
    // above goes down (`StyleBuilderConverter::ConvertBorderWidth`). The computed value has that width, so layout and
    // paint both use it, and `0.4px` does not move the boxes under it.
    for side in 0..4 {
        let w = BorderTopWidth as usize + side;
        if matches!(out[BorderTopStyle as usize + side], Value::Keyword(Keyword::None | Keyword::Hidden)) {
            out[w] = Value::Length(Length::Px(0.0));
        } else if let Value::Length(Length::Px(x)) = out[w]
            && x > 0.0
        {
            out[w] = Value::Length(Length::Px(if x < 1.0 { 1.0 } else { x.floor() }));
        }
    }
    // `visible` and `clip` do not scroll. With the other side a scroll container, they become `auto` and `hidden` (CSS
    // Overflow 3, 3.1): a box scrolls on both sides or on none.
    let (x, y) = (out[OverflowX as usize], out[OverflowY as usize]);
    let stays = |v| matches!(v, Value::Keyword(Keyword::Visible | Keyword::Clip));
    if stays(x) != stays(y) {
        for i in [OverflowX as usize, OverflowY as usize] {
            out[i] = match out[i] {
                Value::Keyword(Keyword::Visible) => Value::Keyword(Keyword::Auto),
                Value::Keyword(Keyword::Clip) => Value::Keyword(Keyword::Hidden),
                v => v,
            };
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
        Value::Grid(i) => {
            let mut grid = cx.declared_grid(i);
            grid.map_lengths(&mut |l| match l {
                Length::Percent(_) => l,
                l => Length::Px(length_to_px(l, fs, cx)),
            });
            Value::Grid(cx.grids.borrow_mut().keep(grid))
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
    sources_capped(doc, viewport, usize::MAX, usize::MAX).unwrap_or_default().into_iter().map(|(s, _)| s).collect()
}

/// The same, but each source comes with its element, and the result is `None` before more than `max_sources` sources, or more than
/// `max_bytes` bytes of `<style>` text and `href`, are copied.
pub fn sources_capped(doc: &Document, viewport: (f32, f32), max_sources: usize, max_bytes: usize) -> Option<Vec<(Source, NodeId)>> {
    // A name that no node in the page has cannot be on any element, so `None` is a good answer for a name that is not in the table.
    let (style, link, media, rel, href) = (doc.find_atom("style"), doc.find_atom("link"), doc.find_atom("media"), doc.find_atom("rel"), doc.find_atom("href"));
    let attr = |el, name: Option<Atom>| name.and_then(|n| doc.attr(el, n));
    let mut out = Vec::new();
    let (mut bytes, mut over) = (0usize, false);
    walk(doc, &mut |el| {
        let NodeData::Element { name, .. } = *doc.data(el) else { return };
        let (is_style, is_link) = (Some(name) == style, Some(name) == link);
        if over || !(is_style || is_link) || !attr(el, media).is_none_or(|m| at_rules::media_matches(m, viewport)) {
            return;
        }
        if is_style {
            let size: usize = doc.children(el).map(|c| if let NodeData::Text(span) = *doc.data(c) { doc.text(span).len() } else { 0 }).sum();
            bytes = bytes.saturating_add(size);
            over = out.len() >= max_sources || bytes > max_bytes;
            if over {
                return;
            }
            let mut text = String::with_capacity(size);
            for c in doc.children(el) {
                if let NodeData::Text(span) = *doc.data(c) {
                    text.push_str(doc.text(span));
                }
            }
            out.push((Source::Inline(text), el));
        } else if let Some(rel) = attr(el, rel) {
            let has = |word: &str| rel.split_ascii_whitespace().any(|w| w.eq_ignore_ascii_case(word));
            if let (true, false, Some(h)) = (has("stylesheet"), has("alternate"), attr(el, href)) {
                bytes = bytes.saturating_add(h.len());
                over = out.len() >= max_sources || bytes > max_bytes;
                if !over {
                    out.push((Source::Link(h.to_string()), el));
                }
            }
        }
    });
    (!over).then_some(out)
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

    /// `Styles::change` after a hover: no matching rule is `Same`, a color is `Paint`, a size is `Layout`, and the background of a
    /// `display: inline` box is `Layout` only when it goes to or from transparent (inline layout makes a fragment for a painted box).
    #[test]
    fn a_hover_change_is_same_paint_or_layout() {
        let html = "<div id=a>a <span id=s>s</span></div><div id=b>b</div>";
        let change = |sheet: &str, id: &str| {
            let (mut doc, before) = styles(html, &[sheet]);
            let n = by_id(&doc, id);
            doc.set_hover(Some(n));
            let mut rules = RuleSet::new(VIEW);
            rules.add(&mut doc, UA_CSS, Origin::UserAgent);
            rules.add(&mut doc, sheet, Origin::Author);
            assert_eq!(rules.user_flags(), if sheet.contains(":hover") { crate::selector::USES_HOVER } else { 0 });
            before.change(&rules.compute(&doc))
        };
        assert_eq!(change("#a:hover { color: red }", "b"), Change::Same);
        assert_eq!(change("#a:hover { color: red; background: #eee; border-color: blue }", "a"), Change::Paint);
        assert_eq!(change("#a:hover { opacity: 0.5; z-index: 2; visibility: hidden }", "a"), Change::Paint);
        assert_eq!(change("#a:hover { padding-left: 1px }", "a"), Change::Layout);
        assert_eq!(change("#a:hover > span { font-weight: bold }", "a"), Change::Layout);
        assert_eq!(change("span:hover { background: #eee }", "s"), Change::Layout);
        assert_eq!(change("span { background: #fff } span:hover { background: #eee }", "s"), Change::Paint);
        assert_eq!(change("div { color: red }", "a"), Change::Same);
    }

    #[test]
    fn presentational_hints_lose_to_rules_and_beat_the_browser() {
        let html = "<table id=t cellspacing=7 width=300><tr><td id=a>x<td id=b>y</table><table><tr><td id=c>z</table>";
        // The browser's `border-spacing: 2px` loses to the hint. A rule of the page beats the hint, even with no specificity.
        assert_eq!(get(html, &[], "t", Property::BorderSpacingH), px(7.0));
        assert_eq!(get(html, &["table { border-spacing: 1px }"], "t", Property::BorderSpacingH), px(1.0));
        assert_eq!(get(html, &[], "t", Property::Width), px(300.0));
        // The cell gets its padding from the table: the same style for the two cells, and the 1px of the browser for the other table.
        let html = "<table cellpadding=5><tr><td id=a>x<td id=b>y</table><table><tr><td id=c>z</table>";
        assert_eq!(get(html, &[], "a", Property::PaddingLeft), px(5.0));
        assert_eq!(get(html, &[], "b", Property::PaddingTop), px(5.0));
        assert_eq!(get(html, &[], "c", Property::PaddingLeft), px(1.0));
        assert_eq!(get(html, &["td { padding: 0 }"], "a", Property::PaddingLeft), px(0.0));
    }

    #[test]
    fn direction_starts_ltr_inherits_and_follows_the_cascade() {
        use Keyword::{Ltr, Rtl};
        let k = |w| Value::Keyword(w);
        let html = "<html id=h><body id=b><div id=d><p id=p>x</p></div><span id=s dir=rtl><i id=i></i></span><input id=t type=tel></body></html>";
        let g = |sheets: &[&str], id| get(html, sheets, id, Property::Direction);
        // Starting value ltr, inherited by a child.
        assert_eq!(g(&[], "p"), k(Ltr));
        assert_eq!(g(&["div { direction: rtl }"], "d"), k(Rtl));
        assert_eq!(g(&["div { direction: rtl }"], "p"), k(Rtl));
        // `initial`, `inherit` and `unset` go through the engine.
        assert_eq!(g(&["div { direction: rtl } p { direction: initial }"], "p"), k(Ltr));
        assert_eq!(g(&["div { direction: rtl } p { direction: ltr } p { direction: inherit }"], "p"), k(Rtl));
        assert_eq!(g(&["div { direction: rtl } p { direction: unset }"], "p"), k(Rtl));
        assert_eq!(g(&["body { --x: rtl } div { direction: var(--x) }"], "d"), k(Rtl));
        // A value that is not a direction is dropped: the older one stays.
        assert_eq!(g(&["div { direction: rtl; direction: auto; direction: 1px }"], "d"), k(Rtl));
        // The `dir` attribute is a rule of the browser sheet: it is inherited, and any rule of the page beats it.
        assert_eq!(g(&[], "s"), k(Rtl));
        assert_eq!(g(&[], "i"), k(Rtl));
        assert_eq!(g(&["span { direction: ltr }"], "s"), k(Ltr));
        assert_eq!(g(&["i { direction: ltr }"], "i"), k(Ltr));
        assert_eq!(get("<div dir=RTL id=d></div>", &[], "d", Property::Direction), k(Rtl));
        assert_eq!(get("<div dir=rtl id=d style='direction: ltr'></div>", &[], "d", Property::Direction), k(Ltr));
        // A tel input is ltr by itself, inside an rtl page too.
        assert_eq!(get("<div dir=rtl><input id=t type=tel></div>", &[], "t", Property::Direction), k(Ltr));
        assert_eq!(get("<div dir=rtl><input id=t type=tel dir=rtl></div>", &[], "t", Property::Direction), k(Rtl));
        assert_eq!(get("<div dir=rtl><input id=t type=text></div>", &[], "t", Property::Direction), k(Rtl));
        // `dir=auto` has no rule of the browser here (it needs the text): the value is inherited.
        assert_eq!(get("<div dir=rtl><p dir=auto id=p></div>", &[], "p", Property::Direction), k(Rtl));
    }

    #[test]
    fn table_properties_inherit_or_not() {
        let html = "<table id=t><tr id=r><td id=c><table id=n></table></td></tr></table>";
        let css = "#t { border-collapse: collapse; border-spacing: 5px 7px; caption-side: bottom; table-layout: fixed; vertical-align: top }";
        let g = |id, p| get(html, &[css], id, p);
        // A cell inherits them. (The UA sheet gives each table `border-spacing: 2px`, so the table inside does not.)
        assert_eq!(g("c", Property::BorderCollapse), Value::Keyword(Keyword::Collapse));
        assert_eq!(g("n", Property::BorderCollapse), Value::Keyword(Keyword::Collapse));
        assert_eq!(g("c", Property::BorderSpacingH), px(5.0));
        assert_eq!(g("c", Property::BorderSpacingV), px(7.0));
        assert_eq!(g("n", Property::BorderSpacingH), px(2.0));
        assert_eq!(g("c", Property::CaptionSide), Value::Keyword(Keyword::Bottom));
        // `table-layout` is not inherited. `vertical-align` is not either, but a cell inherits it by the UA sheet.
        assert_eq!(g("n", Property::TableLayout), Value::Keyword(Keyword::Auto));
        assert_eq!(g("t", Property::VerticalAlign), Value::Keyword(Keyword::Top));
        assert_eq!(g("r", Property::VerticalAlign), Value::Keyword(Keyword::Middle));
        assert_eq!(get("<table><tbody><tr><td id=c>", &[], "c", Property::VerticalAlign), Value::Keyword(Keyword::Middle));
        assert_eq!(get("<table id=t>", &[], "t", Property::BorderSpacingH), px(2.0));
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
        let form = "<input id=h type=HIDDEN><input id=t type=text>";
        assert_eq!(get(form, &[], "h", Property::Display), Value::Keyword(Keyword::None));
        assert_eq!(get(form, &[], "t", Property::Display), Value::Keyword(Keyword::InlineBlock));
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
    fn overflow_scrolls_on_both_sides_or_on_none() {
        let k = Value::Keyword;
        let html = "<div id=d>x</div>";
        assert_eq!(get(html, &[], "d", Property::OverflowX), k(Keyword::Visible));
        assert_eq!(get(html, &["div { overflow-x: hidden }"], "d", Property::OverflowY), k(Keyword::Auto));
        assert_eq!(get(html, &["div { overflow: clip scroll }"], "d", Property::OverflowX), k(Keyword::Hidden));
        assert_eq!(get(html, &["div { overflow: clip visible }"], "d", Property::OverflowX), k(Keyword::Clip));
        assert_eq!(get(html, &["div { overflow: clip visible }"], "d", Property::OverflowY), k(Keyword::Visible));
    }

    #[test]
    fn pointer_events_is_inherited_and_a_child_can_turn_it_on_again() {
        let k = Value::Keyword;
        let html = "<div id=p><div id=c><i id=g></i></div></div>";
        assert_eq!(get(html, &[], "p", Property::PointerEvents), k(Keyword::Auto));
        let sheet = ["#p { pointer-events: none } #c { pointer-events: all }"];
        assert_eq!(get(html, &sheet, "p", Property::PointerEvents), k(Keyword::None));
        assert_eq!(get(html, &sheet, "c", Property::PointerEvents), k(Keyword::All));
        assert_eq!(get(html, &sheet, "g", Property::PointerEvents), k(Keyword::All), "inherited");
        assert_eq!(get(html, &["#p { pointer-events: none } #c { pointer-events: bogus }"], "g", Property::PointerEvents), k(Keyword::None), "a bad value is dropped");
        assert_eq!(get(html, &["#p { pointer-events: none } #c { pointer-events: visiblePainted }"], "c", Property::PointerEvents), k(Keyword::VisiblePainted));
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
    fn letter_and_word_spacing_inherit_as_pixels() {
        let html = "<div id=d><p id=p>x</p></div>";
        let s = |css: &str, p| get(html, &[css], "p", p);
        assert_eq!(s("div { letter-spacing: 0.5em; font-size: 10px } p { font-size: 40px }", Property::LetterSpacing), px(5.0));
        assert_eq!(s("div { word-spacing: -2px }", Property::WordSpacing), px(-2.0));
        assert_eq!(s("div { letter-spacing: 2px } p { letter-spacing: normal }", Property::LetterSpacing), Value::Keyword(Keyword::Normal));
        assert_eq!(s("p { color: red }", Property::LetterSpacing), Value::Keyword(Keyword::Normal));
        assert_eq!(s("div { letter-spacing: 5% }", Property::LetterSpacing), Value::Keyword(Keyword::Normal), "a percent is not valid");
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
        // A pseudo-element never matches, but the other selectors of the list work, as in Chrome.
        assert_eq!(color("li::before, p { color: red } li { color: blue }", "p"), rgb(255, 0, 0));
        assert_eq!(color("li::before, p { color: red } li { color: blue }", "l1"), rgb(0, 0, 255));
        // A broken selector drops the rule, and the rules after it work.
        assert_eq!(color("li::x, p { color: red } li { color: blue }", "p"), rgb(0, 0, 0));
        assert_eq!(color("li::x, p { color: red } li { color: blue }", "l1"), rgb(0, 0, 255));
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
    fn grid_lists_in_the_cascade() {
        use crate::grid::{Bound, Grid, Part};
        let page = "<div id=a><p id=b>x</p></div><div id=c style='grid-template-columns: 2em 1fr'></div><div id=d></div>";
        let sheet = "html { font-size: 20px } div { grid-template-columns: 10em 50% minmax(1rem, 2rem) } p { grid-template-columns: inherit } #d { --w: 3em; grid-template-rows: var(--w) auto }";
        let (doc, styles) = styles(page, &[sheet]);
        let list = |id: &str, p: Property| match styles.style(by_id(&doc, id)).unwrap().get(p) {
            Value::Grid(i) => match styles.grid(i) {
                Grid::Tracks(t) => t
                    .iter()
                    .map(|part| match part {
                        Part::Track(t) => (t.min, t.max),
                        _ => panic!(),
                    })
                    .collect::<Vec<_>>(),
                g => panic!("{g:?}"),
            },
            v => panic!("{v:?}"),
        };
        let px = |x| Bound::Len(Length::Px(x));
        // `em` is the font size of the element: 10em of 20px. `rem` is the font size of `html`.
        assert_eq!(list("a", Property::GridTemplateColumns), [(px(200.0), px(200.0)), (Bound::Len(Length::Percent(50.0)), Bound::Len(Length::Percent(50.0))), (px(20.0), px(40.0))]);
        // `inherit` takes the computed list of the parent.
        assert_eq!(list("b", Property::GridTemplateColumns), list("a", Property::GridTemplateColumns));
        // The `style=""` attribute, and a `var()`.
        assert_eq!(list("c", Property::GridTemplateColumns), [(px(40.0), px(40.0)), (Bound::Auto, Bound::Fr(1.0))]);
        assert_eq!(list("d", Property::GridTemplateRows), [(px(60.0), px(60.0)), (Bound::Auto, Bound::Auto)]);
        // Elements with the same list share its number.
        let number = |id: &str| styles.style(by_id(&doc, id)).unwrap().get(Property::GridTemplateColumns);
        assert_eq!(number("a"), number("b"));
        assert_eq!(styles.style(by_id(&doc, "a")).unwrap().get(Property::GridAutoFlow), Value::Number(0.0));
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

    /// Random changes of the tree and of the user state, each followed by a restyle of the roots that `Scopes` picks: the values
    /// must be those of a full cascade, and each element whose values changed must be in the changed list (or be hidden now
    /// under an element in it).
    #[test]
    fn a_restyle_of_the_picked_roots_equals_a_full_cascade() {
        const CSS: &str = ".a{color:red}.b .c{margin-top:3px}.a > .d{padding-left:2px}.e + .f{color:blue}.g ~ .h{font-size:20px}\
            :hover{color:green}.a:hover .c{color:#123}div:hover > span{margin-left:1px}.e:hover + .f{color:#321}\
            .k:focus-within{background-color:#eee}:focus .c{font-size:12px}:focus{border-top-color:#9a9}\
            li:first-child{color:#111}li:nth-child(2n){color:#222}li:last-child{margin-left:4px}:empty{padding-top:1px}.q:empty + div{color:#333}\
            [data-x]{color:#444}[data-x=y] .c{color:#555}.m{display:none}.n{font-size:2em}";
        const CLASSES: [&str; 12] = ["a", "b", "c", "d", "e", "f", "g", "h", "k", "q", "m", "n"];
        const TAGS: [&str; 4] = ["div", "span", "li", "ul"];
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut rnd = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let connected = |doc: &Document| {
            let (mut all, mut k) = (vec![doc.root()], 0);
            while k < all.len() {
                let kids: Vec<_> = doc.children(all[k]).collect();
                all.extend(kids);
                k += 1;
            }
            all.retain(|&n| matches!(doc.data(n), NodeData::Element { .. }));
            all
        };
        let chain = |doc: &Document, n: Option<NodeId>| std::iter::successors(n, |&p| doc.parent(p)).filter(|&p| matches!(doc.data(p), NodeData::Element { .. })).collect::<Vec<_>>();
        let (mut restyled, mut full_runs) = (0, 0);
        for round in 0..8 {
            let mut doc = parse("<!doctype html><body></body>");
            let body = connected(&doc)[2];
            for _ in 0..40 {
                let els = connected(&doc);
                let parent = els[2 + rnd(els.len() - 2)];
                let c = CLASSES[rnd(CLASSES.len())];
                let el = doc.insert_element(parent, None, TAGS[rnd(TAGS.len())], &[("class", c)]);
                if rnd(2) == 0 {
                    doc.append_text(el, "x");
                }
            }
            let mut rules = RuleSet::new(VIEW);
            rules.add(&mut doc, UA_CSS, Origin::UserAgent);
            rules.add(&mut doc, CSS, Origin::Author);
            let sc = rules.scopes();
            let mut st = rules.compute_kept(&doc);
            doc.track_damage();
            let mut chains = [Vec::new(), Vec::new()];
            for step in 0..200 {
                let els = connected(&doc);
                let pick = els[2 + rnd(els.len() - 2)];
                let what = rnd(8);
                match what {
                    0 => doc.set_attr(pick, "class", CLASSES[rnd(CLASSES.len())]),
                    1 => doc.set_attr(pick, "data-x", ["y", "z"][rnd(2)]),
                    2 => {
                        let el = doc.insert_element(pick, doc.children(pick).next().filter(|_| rnd(2) == 0), TAGS[rnd(TAGS.len())], &[("class", CLASSES[rnd(CLASSES.len())])]);
                        doc.append_text(el, "y");
                    }
                    3 if pick != body => doc.detach(pick),
                    4 => match doc.children(pick).find(|&c| matches!(doc.data(c), NodeData::Text(_))) {
                        Some(t) => doc.set_text(t, ["", "z"][rnd(2)]),
                        None => doc.append_text(pick, "t"),
                    },
                    5 => {
                        doc.set_hover(Some(pick));
                    }
                    6 => {
                        doc.set_focus((rnd(3) > 0).then_some(pick), rnd(2) == 0);
                    }
                    _ => {
                        doc.set_hover(None);
                    }
                }
                let mut roots = Vec::new();
                let mut ok = doc.take_damage().is_some_and(|marks| marks.into_iter().all(|m| sc.roots(&doc, m, &mut roots)));
                let new_chains = [chain(&doc, doc.user().hover), chain(&doc, doc.user().focus)];
                Scopes::user_roots(sc.hover, &chains[0], &new_chains[0], &doc, &mut roots);
                Scopes::user_roots(sc.focus, &chains[1], &new_chains[1], &doc, &mut roots);
                chains = new_chains;
                let old: Vec<(NodeId, Option<ComputedStyle>)> = connected(&doc).into_iter().filter(|n| n.index() < st.nodes()).map(|n| (n, st.style(n).cloned())).collect();
                let mut changed = Vec::new();
                ok = ok && rules.restyle(&doc, &mut st, &roots, &mut changed);
                if !ok {
                    full_runs += 1;
                    st = rules.compute_kept(&doc);
                    continue;
                }
                restyled += 1;
                let full = rules.compute(&doc);
                for n in connected(&doc) {
                    assert_eq!(st.style(n), full.style(n), "round {round} step {step} change {what} at {pick:?}: node {n:?} roots {roots:?}\n{}", full.dump(&doc));
                }
                for (n, before) in old {
                    if before.as_ref() != st.style(n) {
                        let listed = |e: NodeId| changed.iter().any(|c| c.0 == e);
                        let hidden_under = st.style(n).is_none() && std::iter::successors(doc.parent(n), |&p| doc.parent(p)).any(listed);
                        assert!(listed(n) || hidden_under, "round {round} step {step}: {n:?} changed and is not listed");
                    }
                }
            }
        }
        assert!(restyled > 1000 && full_runs < 200, "restyled {restyled}, full {full_runs}");
    }
}
