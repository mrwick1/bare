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

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::BuildHasherDefault;

use bare_html::dom::{Atom, Document, Fnv, Mark, NodeData, NodeId};

use crate::at_rules;
use crate::calc::{self, Linear, Node};
use crate::color::Color;
use crate::grid::{self, Grid};
use crate::matching::{compound_match, matches};
use crate::parser::{AtBody, Rule, parse_block, parse_declarations, parse_stylesheet, parse_stylesheet_lazy};
use crate::selector::{Combinator, Mode, Selector, ShadowPseudo, Simple, parse_selector_list_in};
use crate::values::{Declared, Keyword, Length, Property, Side, Value, mirror, parse_declaration};
use crate::var::{self, CustomDecl, VarDecl};
use crate::values::Deferred;

/// A retained CSSOM declaration. The mask selects surviving longhands after parsing the source shorthand.
#[derive(Clone, Debug, PartialEq)]
pub struct InputDeclaration {
    pub name: String,
    pub value: String,
    pub important: bool,
    pub mask: u128,
}

/// Retained blocks in top-level stylesheet rule order. At-rules use `None`.
pub type InputSheet = Vec<Option<Vec<InputDeclaration>>>;

#[cold]
#[inline(never)]
fn input_declarations(input: &[InputDeclaration], side: &mut Side, out: &mut Vec<Declared>, customs: &mut Vec<CustomDecl>) {
    for d in input {
        if d.name.starts_with("--") {
            customs.push(CustomDecl { name: d.name.as_str().into(), text: d.value.as_str().into(), important: d.important });
        } else {
            let from = out.len();
            parse_declaration(&d.name, &d.value, d.important, side, out);
            let mut at = from;
            while at < out.len() {
                if d.mask & (1u128 << out[at].property as usize) == 0 { out.remove(at); } else { at += 1; }
            }
        }
    }
}

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
    /// The block of the rule: a place in `RuleSet::bodies`.
    body: u32,
    /// The layer of the rule: a place in `RuleSet::layers`. 0 is no layer.
    layer: u32,
    origin: Origin,
    /// The hashes of up to 4 tags, ids or classes that the ancestors must have. 0 means no hash.
    ancestors: [u32; 4],
    /// The tree context of the rule against the element that it styles (CSS Scoping): `OWN`, `INNER` (a rule of a shadow tree for its host
    /// or for a node that it slots), `OUTER` (a `::part()` rule of the shadow tree around the host) or `OUTERMOST` (one of the document).
    ctx: u8,
}

/// The block of a rule: its declarations, read when an element first needs them (`RuleSet::ensure`). `a, b { ... }` makes two entries and one body.
struct Body {
    /// The declarations of the rule: a range of `RuleSet::decls`. While `unread`, the place of the text of the block in `RuleSet::sources[source]`.
    decls: Cell<(u32, u32)>,
    /// The custom properties of the rule: a range of `RuleSet::customs`.
    customs: Cell<(u32, u32)>,
    /// How many declarations of picture properties (`background-image`, `mask-size-x`, ...) the rule has. They are in `RuleSet::decls` after
    /// the range `decls`, in their order: the cascade of an element reads them only when one of its rules has some (`picture_pass`).
    pics: Cell<u16>,
    /// `customs` and `pics` are not set yet, and `decls` is the place of the text.
    unread: Cell<bool>,
    /// While `unread`: a declaration of the block may be a `transition-*` or `animation-*` property (`has_motion` reads these blocks).
    motion: bool,
    source: u32,
}

const OWN: u8 = 0;
const INNER: u8 = 1;
const OUTER: u8 = 2;
/// A `::part()` rule of the document: outside every shadow tree.
const OUTERMOST: u8 = 3;

/// A list of rules with more entries than this is split by an ancestor key (`Big`), when at least half of its rules have one.
const SPLIT_FROM: usize = 16;

/// The rules of one tree, found by what the last compound reads. The document, and each shadow tree, has one.
struct Index {
    by_id: Buckets<Box<str>>,
    by_class: Buckets<Box<str>>,
    by_tag: Buckets<Atom>,
    /// The rules with an attribute test and no id or class in the last part, by the name of the attribute: `[data-state=open]`.
    /// An element looks up the names of its own attributes, so a rule for an attribute that it does not have costs nothing.
    by_attr: Buckets<Atom>,
    /// The rules that have no id, no class, no attribute and no tag in the last part: `*`, `:first-child`.
    other: Vec<u32>,
    /// The long lists, split by an ancestor key. Made at the first look-up after a `push`.
    big: std::cell::OnceCell<Option<Box<Bigs>>>,
    /// A list over this length is split. A field so that a test can make small lists split, or never.
    split_from: usize,
    /// The length of the longest list. No list is over `split_from` when this is not, and then `bigs` needs no scan of the lists.
    longest: usize,
}

impl Default for Index {
    fn default() -> Index {
        Index { by_id: Buckets::default(), by_class: Buckets::default(), by_tag: Buckets::default(), by_attr: Buckets::default(), other: Vec::new(), big: std::cell::OnceCell::new(), split_from: SPLIT_FROM, longest: 0 }
    }
}

/// The long lists of an `Index` that have a split. A list that is not here is read whole.
#[derive(Default)]
struct Bigs {
    by_id: HashMap<Box<str>, Big, BuildHasherDefault<Fnv>>,
    by_class: HashMap<Box<str>, Big, BuildHasherDefault<Fnv>>,
    by_tag: HashMap<Atom, Big, BuildHasherDefault<Fnv>>,
    by_attr: HashMap<Atom, Big, BuildHasherDefault<Fnv>>,
    other: Option<Big>,
}

/// One long list, split. A rule goes under the hash of its rarest ancestor (a tag, id or class that an ancestor of the element must have,
/// see `ancestor_hashes`), or in `rest` when it has none. An element needs only `rest` and the groups for the hashes on its path.
/// The three arrays are flat: group `i` is `list[starts[i]..starts[i + 1]]`, under the hash `keys[i]` (sorted, no repeats).
struct Big {
    keys: Box<[u32]>,
    starts: Box<[u32]>,
    list: Box<[u32]>,
    rest: Box<[u32]>,
}

impl Big {
    /// Split `rules`, in their order. `None` when fewer than half of them have a key: the split would not remove much.
    fn split(rules: &[u32], entries: &[Entry]) -> Option<Big> {
        // How many rules each hash could be the key of: the rarest one is the key.
        let hashes: Vec<[u32; 4]> = rules.iter().map(|&r| ancestor_hashes(&entries[r as usize].selector, true)).collect();
        let mut seen: HashMap<u32, u32, BuildHasherDefault<Fnv>> = HashMap::default();
        for h in hashes.iter().flatten().filter(|&&h| h != 0) {
            *seen.entry(*h).or_default() += 1;
        }
        let (mut keyed, mut rest) = (Vec::new(), Vec::new());
        for (&r, hashes) in rules.iter().zip(&hashes) {
            match hashes.iter().take_while(|&&h| h != 0).min_by_key(|h| seen[h]) {
                Some(&h) => keyed.push((h, r)),
                None => rest.push(r),
            }
        }
        if keyed.len() * 2 < rules.len() {
            return None;
        }
        keyed.sort_unstable();
        let (mut keys, mut starts) = (Vec::new(), Vec::new());
        for (i, &(h, _)) in keyed.iter().enumerate() {
            if keys.last() != Some(&h) {
                keys.push(h);
                starts.push(i as u32);
            }
        }
        starts.push(keyed.len() as u32);
        Some(Big { keys: keys.into(), starts: starts.into(), list: keyed.iter().map(|k| k.1).collect(), rest: rest.into() })
    }

    /// Add to `out` the rules that may match an element with this path: `rest`, and the groups of the hashes that the path may have.
    /// Gives the number of look-ups, as work for the step charge.
    #[inline]
    fn collect(&self, filter: &AncestorFilter, out: &mut Vec<u32>) -> u64 {
        out.extend_from_slice(&self.rest);
        let mut group = |i: usize| out.extend_from_slice(&self.list[self.starts[i] as usize..self.starts[i + 1] as usize]);
        // Look up the shorter of the two: the keys in the filter (it may say yes for a hash that is not on the path: the matcher decides),
        // or the hashes of the path in the keys (a binary search for each).
        if self.keys.len() <= filter.path.len() {
            for (i, &k) in self.keys.iter().enumerate() {
                if filter.may_contain(k) {
                    group(i);
                }
            }
            self.keys.len() as u64
        } else {
            for &(h, _) in &filter.path {
                if let Ok(i) = self.keys.binary_search(&h) {
                    group(i);
                }
            }
            filter.path.len() as u64
        }
    }
}

impl Index {
    /// The split lists, made when first needed. `None` when no list is over `split_from`, or none has a split.
    #[inline(always)]
    fn bigs(&self, entries: &[Entry]) -> Option<&Bigs> {
        match self.big.get() {
            Some(b) => b.as_deref(),
            None => self.make_bigs(entries),
        }
    }

    #[cold]
    #[inline(never)]
    fn make_bigs(&self, entries: &[Entry]) -> Option<&Bigs> {
        self.big
            .get_or_init(|| {
                let (from, mut found) = (self.split_from, false);
                if self.longest <= from {
                    return None;
                }
                let mut bigs = Bigs::default();
                let mut make = |list: &Vec<u32>| (list.len() > from).then(|| Big::split(list, entries)).flatten().inspect(|_| found = true);
                for (k, l) in &self.by_id {
                    bigs.by_id.extend(make(l).map(|b| (k.clone(), b)));
                }
                for (k, l) in &self.by_class {
                    bigs.by_class.extend(make(l).map(|b| (k.clone(), b)));
                }
                for (&k, l) in &self.by_tag {
                    bigs.by_tag.extend(make(l).map(|b| (k, b)));
                }
                for (&k, l) in &self.by_attr {
                    bigs.by_attr.extend(make(l).map(|b| (k, b)));
                }
                bigs.other = make(&self.other);
                found.then(|| Box::new(bigs))
            })
            .as_deref()
    }

    #[inline]
    fn push(&mut self, index: u32, selector: &Selector) {
        self.big.take();
        // The bucket: an id is the most specific, then a class, then an attribute, then a tag. An attribute comes before a tag:
        // `input[type=checkbox]` is in the list of `type`, and few elements have a `type`; most elements are a `div`.
        let last = &selector.compounds[0].simples;
        let list = if let Some(Simple::Id(id)) = last.iter().find(|s| matches!(s, Simple::Id(_))) {
            self.by_id.entry(id.clone()).or_default()
        } else if let Some(Simple::Class(class)) = last.iter().find(|s| matches!(s, Simple::Class(_))) {
            self.by_class.entry(class.clone()).or_default()
        } else if let Some(&Simple::Attr { name, .. }) = last.iter().find(|s| matches!(s, Simple::Attr { .. })) {
            // Every attribute test needs the attribute: an element without it never matches.
            self.by_attr.entry(name).or_default()
        } else if let Some(&Simple::Tag(tag)) = last.iter().find(|s| matches!(s, Simple::Tag(_))) {
            self.by_tag.entry(tag).or_default()
        } else {
            &mut self.other
        };
        list.push(index);
        self.longest = self.longest.max(list.len());
    }

    /// Add to `out` the rules that may match the element: the ones in the lists for its id, its classes, its attributes and its tag, and the "other" list.
    /// A split list gives only the rules whose ancestor key may be on the path (`filter`). Gives the look-ups that the splits made, as work.
    #[inline(always)]
    fn collect(&self, doc: &Document, el: NodeId, out: &mut Vec<u32>, entries: &[Entry], filter: &AncestorFilter) -> u64 {
        if filter.track { self.collect_with::<true>(doc, el, out, entries, filter) } else { self.collect_with::<false>(doc, el, out, entries, filter) }
    }

    /// `TRACK` is `filter.track`: false when no index has a split, and then there is nothing to look up.
    #[inline(always)]
    fn collect_with<const TRACK: bool>(&self, doc: &Document, el: NodeId, out: &mut Vec<u32>, entries: &[Entry], filter: &AncestorFilter) -> u64 {
        if TRACK {
            return self.collect_split(doc, el, out, entries, filter);
        }
        self.gather::<false>(doc, el, out, None, filter)
    }

    /// `collect` for a rule set that has a split somewhere. Apart, so that the code of the common case stays as it was.
    #[inline(never)]
    fn collect_split(&self, doc: &Document, el: NodeId, out: &mut Vec<u32>, entries: &[Entry], filter: &AncestorFilter) -> u64 {
        match self.bigs(entries) {
            Some(bigs) => self.gather::<true>(doc, el, out, Some(bigs), filter),
            None => self.gather::<false>(doc, el, out, None, filter),
        }
    }

    /// `collect`. With `SPLIT` false no list is split and it only copies the lists, as before the splits.
    #[inline(always)]
    fn gather<const SPLIT: bool>(&self, doc: &Document, el: NodeId, out: &mut Vec<u32>, bigs: Option<&Bigs>, filter: &AncestorFilter) -> u64 {
        let mut work = 0;
        // A list that is not over `split_from` has no split: no look-up in `bigs`.
        let mut put = |list: &Vec<u32>, big: &dyn Fn(&Bigs) -> Option<&Big>| {
            if SPLIT && list.len() > self.split_from && let Some(b) = bigs.and_then(big) {
                work += b.collect(filter, out);
            } else {
                out.extend_from_slice(list);
            }
        };
        put(&self.other, &|b| b.other.as_ref());
        if let NodeData::Element { name, attrs } = *doc.data(el) {
            if let Some(list) = self.by_tag.get(&name) {
                put(list, &|b| b.by_tag.get(&name));
            }
            if !self.by_attr.is_empty() {
                for a in doc.attrs(attrs) {
                    if let Some(list) = self.by_attr.get(&a.name) {
                        put(list, &|b| b.by_attr.get(&a.name));
                    }
                }
            }
        }
        if let Some(id) = doc.attr(el, Atom::ID)
            && let Some(list) = self.by_id.get(id)
        {
            put(list, &|b| b.by_id.get(id));
        }
        if let Some(classes) = doc.attr(el, Atom::CLASS) {
            for class in classes.split_ascii_whitespace() {
                if let Some(list) = self.by_class.get(class) {
                    put(list, &|b| b.by_class.get(class));
                }
            }
        }
        work
    }
}

/// The rules of one shadow tree (the sheets of its `<style>` elements and of its adopted sheets).
#[derive(Default)]
struct Shadowed {
    /// The rules for the elements of the tree.
    index: Index,
    /// The rules for its host: `:host`, `:host(.a)`, `:host-context(.a)`.
    host: Vec<u32>,
    /// The rules for the nodes that its slots take: `::slotted()`.
    slotted: Vec<u32>,
    /// The `::part()` rules for the parts of the shadow trees of the hosts that this tree holds.
    parts: Vec<u32>,
}

fn slots_set<K>(m: &std::collections::HashSet<K>) -> usize {
    m.capacity() * 8 / 7 * (std::mem::size_of::<K>() + 1)
}

type Buckets<K> = HashMap<K, Vec<u32>, BuildHasherDefault<Fnv>>;
// A map from the name of an id, a class or a tag to the rules for it. The hasher is the fast one of the DOM.

/// All the rules of all the style sheets, and the index to find them.
pub struct RuleSet {
    /// The width and height of the window in pixels. `@media` reads it when a rule is added, `vw` and `vh` read it in `compute`.
    viewport: (f32, f32),
    entries: Vec<Entry>,
    bodies: Vec<Body>,
    /// The text of the style sheets that were added: the blocks that are not read yet are in it.
    sources: Vec<Box<str>>,
    /// The sheet that `add_rules` reads: a place in `sources`.
    source: u32,
    /// The declarations of all rules that are read, one after the other.
    decls: RefCell<Vec<Declared>>,
    /// What a value in `decls` points to: the `calc()` that mix units, and the declarations with `var()`.
    /// It grows when a block is read, and its entries stay where they are: a value is a place in it.
    side: RefCell<Side>,
    /// The layers (`@layer`), in the order that we first saw them. Layer 0 is "no layer", the parent of the top layers.
    layers: Vec<Layer>,
    /// The layer for a parent and a name, to find a layer without a scan of all of them.
    layer_by_name: HashMap<(u32, Box<str>), u32>,
    /// The custom properties of all rules (`--gap: 8px`), one after the other.
    customs: RefCell<Vec<CustomDecl>>,
    /// The rules of the document (and the browser's), found by what the last compound reads.
    index: Index,
    /// The rules of each shadow tree that has any, by `NodeId::index` of its root.
    shadows: HashMap<u32, Shadowed, BuildHasherDefault<Fnv>>,
    /// The `::part()` rules of the document.
    parts: Vec<u32>,
    /// The user action state that some selector reads (`selector::USES_*`).
    user: u8,
    /// Where the selectors read each kind of state (`Scopes`).
    scopes: Scopes,
    /// Where the selectors read each class, id and attribute (`Reach`). Made at the first change, so a page that never
    /// changes does not pay for it.
    reach: std::cell::OnceCell<Reach>,
    /// The rules that the index offered for the elements that `walk` and `restyle` styled, summed over all calls. A caller takes the difference around one call.
    tested: std::cell::Cell<u64>,
    /// The elements that `walk` and `restyle` styled, summed over all calls (one `candidates` call for each).
    styled: std::cell::Cell<u64>,
    /// Does a rule have a logical property of the inline axis, or `float: inline-start | inline-end`? Only then does an `rtl` element need `logical_pass`.
    logical: Cell<bool>,
    /// Does a block that is not read yet have a declaration that may be a `transition` or an `animation`? (`scan::Found::Rule`)
    /// `has_motion` reads those blocks, and then this is `false`.
    motion: Cell<bool>,
    /// The names of the `@keyframes` rules (`has_keyframes`). Their steps are not read: an animation only needs to know that the name exists.
    keyframes: std::collections::HashSet<Box<str>>,
    pending: Option<Box<HashMap<NodeId, Vec<InputDeclaration>>>>,
}

/// The roots of one `restyle` that styled their whole subtree. Out of line: the walk of `restyle_with` is hot, and a set inlined into it makes it slower.
#[derive(Default)]
struct Covered(std::collections::HashSet<usize, BuildHasherDefault<Fnv>>);

impl Covered {
    #[inline(never)]
    fn has(&self, n: NodeId) -> bool {
        !self.0.is_empty() && self.0.contains(&n.index())
    }

    #[inline(never)]
    fn add(&mut self, n: NodeId) {
        self.0.insert(n.index());
    }
}

/// The invalidation sets: for each class, id and attribute name that a selector reads, the widest `Scope` where it reads it.
/// A change of `class` restyles only what the added and removed classes reach (`RuleSet::roots`). Blink and Stylo keep the
/// same sets; theirs also keep the selectors on the right of the name, to restyle fewer descendants.
#[derive(Default)]
struct Reach {
    class: HashMap<Box<str>, Scope, BuildHasherDefault<Fnv>>,
    id: HashMap<Box<str>, Scope, BuildHasherDefault<Fnv>>,
    attr: HashMap<Atom, Scope, BuildHasherDefault<Fnv>>,
    /// `:link` and `:any-link` read `href`.
    link: Scope,
}

impl Reach {
    fn add(&mut self, selector: &Selector) {
        fn simples(me: &mut Reach, list: &[Simple], at: Scope) {
            for s in list {
                let slot = match s {
                    Simple::Id(id) => me.id.entry(id.clone()).or_default(),
                    Simple::Class(class) => me.class.entry(class.clone()).or_default(),
                    Simple::Attr { name, .. } => me.attr.entry(*name).or_default(),
                    Simple::Link => &mut me.link,
                    Simple::Not(args) | Simple::Is(args) => {
                        for a in args {
                            simples(me, a, at);
                        }
                        continue;
                    }
                    Simple::Complex(cx) => {
                        // A compound of the argument left of a combinator is read on an ancestor or an earlier sibling, like one of the selector.
                        for a in &cx.args {
                            let mut at = at;
                            for (i, c) in a.iter().enumerate() {
                                if i > 0 {
                                    at = at.max(match a[i - 1].combinator {
                                        Combinator::Next | Combinator::Later => Scope::Siblings,
                                        _ => Scope::Subtree,
                                    });
                                }
                                simples(me, &c.simples, at);
                            }
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
                at = at.max(match selector.compounds[i - 1].combinator {
                    Combinator::Next | Combinator::Later => Scope::Siblings,
                    _ => Scope::Subtree,
                });
            }
            simples(self, &c.simples, at);
        }
    }
}

/// Which elements a change of the attribute `name` of `el` can move in or out of a form pseudo-class (`form.rs`). `Scope::None`:
/// none. `Own`: `el` only. `Subtree`: `el` and the elements inside it. `None`: others too (the radio group, the default button of a
/// form, the options of a `select`).
fn form_reach(doc: &Document, el: NodeId, name: &str) -> Option<Scope> {
    let is = |tags: &[&str]| tags.iter().any(|t| doc.is_html(el, t));
    Some(match name {
        // `:read-write` of the elements inside reads it.
        "contenteditable" => Scope::Subtree,
        _ if !is(&["button", "fieldset", "input", "optgroup", "option", "progress", "select", "textarea"]) => Scope::None,
        "value" | "placeholder" | "required" | "readonly" => Scope::Own,
        "disabled" if is(&["option", "optgroup"]) => return None,
        "disabled" if is(&["fieldset"]) => Scope::Subtree,
        "disabled" => Scope::Own,
        // The options of a `select`.
        "multiple" | "size" => Scope::Subtree,
        "name" if radio(doc, el) => return None,
        "type" | "checked" | "selected" => return None,
        _ => Scope::None,
    })
}

/// Is `el` a radio button? Its checkedness and its `name` move its group in or out of `:indeterminate`.
fn radio(doc: &Document, el: NodeId) -> bool {
    doc.is_html(el, "input") && doc.find_atom("type").and_then(|t| doc.attr(el, t)).is_some_and(|t| t.trim().eq_ignore_ascii_case("radio"))
}
/// The attributes that presentational hints read (`hints.rs`). A cell reads the table above it, so they reach the subtree.
const HINT_ATTRS: [&str; 11] = ["align", "border", "cellpadding", "cellspacing", "fill", "height", "nowrap", "stroke", "valign", "viewBox", "width"];

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
    pub(crate) fn at(scope: Scope, e: NodeId, doc: &Document, out: &mut Vec<(NodeId, bool)>) {
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
                    Simple::Complex(cx) => {
                        // As in `Reach::add`. A `+` or `~` in the argument also makes the place of a sibling count.
                        for a in &cx.args {
                            let mut at = at;
                            for (i, c) in a.iter().enumerate() {
                                if i > 0 {
                                    let way = match a[i - 1].combinator {
                                        Combinator::Next | Combinator::Later => Scope::Siblings,
                                        _ => Scope::Subtree,
                                    };
                                    at = at.max(way);
                                    if way == Scope::Siblings {
                                        me.structure = Scope::Siblings;
                                    }
                                }
                                simples(me, &c.simples, at);
                            }
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
        RuleSet { viewport, entries: Vec::new(), bodies: Vec::new(), sources: Vec::new(), source: 0, decls: RefCell::default(), side: RefCell::default(), layers: vec![Layer { parent: 0 }], layer_by_name: HashMap::new(), customs: RefCell::default(), index: Index::default(), shadows: HashMap::default(), parts: Vec::new(), user: 0, scopes: Scopes::default(), reach: std::cell::OnceCell::new(), tested: std::cell::Cell::new(0), styled: std::cell::Cell::new(0), logical: Cell::new(false), motion: Cell::new(false), pending: None, keyframes: std::collections::HashSet::new() }
    }

    /// Which user action state (`:hover`, `:active`, the focus pseudo-classes) some rule reads: the bits `selector::USES_*`.
    /// A host that keeps styles must compute them again when one of those states changes.
    /// Does a rule set one of the `transition-*` or `animation-*` properties, or is a `@keyframes` rule here?
    pub fn has_motion(&self) -> bool {
        if self.motion.replace(false) {
            for body in self.bodies.iter().filter(|b| b.unread.get() && b.motion) {
                self.read_lazy(body);
            }
        }
        !self.keyframes.is_empty() || self.decls.borrow().iter().any(|d| crate::motion::is_motion(d.property))
    }

    pub fn has_keyframes(&self, name: &str) -> bool {
        self.keyframes.contains(name)
    }

    pub fn user_flags(&self) -> u8 {
        self.user
    }

    /// Where the selectors read each kind of state (`Scopes`).
    pub fn scopes(&self) -> Scopes {
        self.scopes
    }

    /// The roots of `restyle` (element, whole subtree) for one change of the tree. `false`: restyle the whole page.
    /// A linked node gets its styles. The place of its siblings changes for `:first-child` and `+`, and the parent may stop being
    /// `:empty`. An attribute change restyles what the selectors that read it reach (`Reach`): for `class`, the classes that
    /// were added or removed. A form pseudo-class reads the attributes of other elements (a `fieldset`, the radio group), so with
    /// one, a change of the form state or of an attribute that it reads restyles all.
    pub fn roots(&self, doc: &Document, mark: Mark, out: &mut Vec<(NodeId, bool)>) -> bool {
        let sc = &self.scopes;
        let children = |parent: NodeId, out: &mut Vec<(NodeId, bool)>| {
            if sc.structure != Scope::None {
                out.push((parent, true));
            }
            Scopes::at(sc.empty, parent, doc, out);
        };
        match mark {
            Mark::Link { parent, node } => {
                out.push((node, true));
                children(parent, out);
            }
            Mark::Unlink { parent, .. } => children(parent, out),
            Mark::Text { node } => {
                if let Some(p) = doc.parent(node) {
                    Scopes::at(sc.empty, p, doc, out);
                }
            }
            Mark::Attr { el, name, old } => {
                let (old, new) = (old.map(|s| doc.text(s)), doc.attr(el, name));
                if old == new {
                    // Set to the value that it had: nothing changed.
                    return true;
                }
                let named = |list: &[&str]| list.iter().any(|&n| doc.find_atom(n) == Some(name));
                let r = self.reach.get_or_init(|| {
                    let mut r = Reach::default();
                    for e in &self.entries {
                        r.add(&e.selector);
                    }
                    r
                });
                let mut scope = r.attr.get(&name).copied().unwrap_or_default();
                if sc.form != Scope::None {
                    // The elements whose form state changes, and then what the selectors that read it reach from them.
                    match form_reach(doc, el, doc.atom_str(name)) {
                        Some(Scope::None) => {}
                        Some(Scope::Own) => scope = scope.max(sc.form),
                        // After a `+` or `~`, the siblings of the elements inside are inside too.
                        Some(Scope::Subtree) => scope = scope.max(sc.form.max(Scope::Subtree)),
                        // An element that is not in the document reaches no other element: linking it restyles its subtree.
                        _ => return !Self::connected(doc, el),
                    }
                }
                if named(&["style"]) {
                    scope = scope.max(Scope::Own);
                } else if named(&HINT_ATTRS) {
                    scope = scope.max(Scope::Subtree);
                } else if named(&["href"]) {
                    scope = scope.max(r.link);
                }
                let (old, new) = (old.unwrap_or(""), new.unwrap_or(""));
                if name == Atom::CLASS {
                    // Only the classes that were added or removed.
                    let (old, new) = (|| old.split_ascii_whitespace(), || new.split_ascii_whitespace());
                    for c in old().filter(|c| !new().any(|n| n == *c)).chain(new().filter(|c| !old().any(|o| o == *c))) {
                        scope = scope.max(r.class.get(c).copied().unwrap_or_default());
                    }
                } else if name == Atom::ID && old != new {
                    for id in [old, new] {
                        scope = scope.max(r.id.get(id).copied().unwrap_or_default());
                    }
                }
                Scopes::at(scope, el, doc, out);
            }
            Mark::Ui { el } if sc.form != Scope::None => {
                // The checkedness of a radio button moves its group in or out of `:indeterminate`.
                if radio(doc, el) {
                    return !Self::connected(doc, el);
                }
                Scopes::at(sc.form, el, doc, out);
            }
            Mark::Ui { .. } => {}
            // The size of the picture is in the hints of the element.
            Mark::Image { el } => Scopes::at(Scope::Own, el, doc, out),
        }
        true
    }

    /// Is `el` in the document (also through a shadow root)?
    fn connected(doc: &Document, el: NodeId) -> bool {
        std::iter::successors(Some(el), |&n| doc.parent(n).or_else(|| doc.shadow_host(n))).last() == Some(doc.root())
    }

    /// Was this set built for a window of `viewport` pixels and these sheets (the text of each, in the order of the page, after the
    /// browser's own)? `rule_cache` compares with it before it shares a set.
    pub fn built_from<'a>(&self, viewport: (f32, f32), sheets: impl ExactSizeIterator<Item = &'a str>) -> bool {
        self.viewport == viewport && self.sources.len() == sheets.len() + 1 && self.sources[1..].iter().zip(sheets).all(|(a, b)| **a == *b)
    }

    /// The bytes that the set owns on the heap now, counted from the sizes of its tables: what `rule_cache` weighs a set by. It grows
    /// when blocks are read and lists are split. A hash table counts its slots and one control byte for each. The allocator adds
    /// a little more for each block that it hands out (not counted here).
    pub fn heap_bytes(&self) -> usize {
        use std::mem::size_of;
        fn slots<K, V, S>(m: &HashMap<K, V, S>) -> usize {
            m.capacity() * 8 / 7 * (size_of::<K>() + size_of::<V>() + 1)
        }
        fn lists<K>(m: &Buckets<K>, key: impl Fn(&K) -> usize) -> usize {
            slots(m) + m.iter().map(|(k, v)| key(k) + v.capacity() * 4).sum::<usize>()
        }
        let index = &self.index;
        let bigs = index.big.get().and_then(|b| b.as_deref()).map_or(0, |b| {
            let one = |big: &Big| (big.keys.len() + big.starts.len() + big.list.len() + big.rest.len()) * 4;
            slots(&b.by_id) + slots(&b.by_class) + slots(&b.by_tag) + slots(&b.by_attr)
                + b.by_id.iter().map(|(k, v)| k.len() + one(v)).sum::<usize>()
                + b.by_class.iter().map(|(k, v)| k.len() + one(v)).sum::<usize>()
                + b.by_tag.values().chain(b.by_attr.values()).map(one).sum::<usize>()
                + b.other.as_ref().map_or(0, one)
        });
        let side = self.side.borrow();
        let reach = self.reach.get().map_or(0, |r| slots(&r.class) + slots(&r.id) + slots(&r.attr) + r.class.keys().chain(r.id.keys()).map(|k| k.len()).sum::<usize>());
        self.entries.capacity() * size_of::<Entry>()
            + self.entries.iter().map(|e| e.selector.heap_bytes()).sum::<usize>()
            + self.bodies.capacity() * size_of::<Body>()
            + self.sources.capacity() * size_of::<Box<str>>()
            + self.sources.iter().map(|t| t.len()).sum::<usize>()
            + self.decls.borrow().capacity() * size_of::<Declared>()
            + side.calcs.capacity() * size_of::<Linear>()
            + side.deferred.capacity() * size_of::<Deferred>()
            + side.deferred.iter().map(|entry| match entry {
                Deferred::Variable(v) => v.name.len() + v.text.len(),
                Deferred::Color(expr) => expr.heap_bytes(),
            }).sum::<usize>()
            + side.grids.capacity() * size_of::<Grid>()
            + self.customs.borrow().capacity() * size_of::<CustomDecl>()
            + self.customs.borrow().iter().map(|c| c.name.len() + c.text.len()).sum::<usize>()
            + self.layers.capacity() * size_of::<Layer>()
            + slots(&self.layer_by_name)
            + self.layer_by_name.keys().map(|(_, n)| n.len()).sum::<usize>()
            + lists(&index.by_id, |k| k.len())
            + lists(&index.by_class, |k| k.len())
            + lists(&index.by_tag, |_| 0)
            + lists(&index.by_attr, |_| 0)
            + index.other.capacity() * 4
            + bigs
            + slots(&self.shadows)
            + self.parts.capacity() * 4
            + reach
            + slots_set(&self.keyframes)
            + self.keyframes.iter().map(|k| k.len()).sum::<usize>()
    }

    /// The number of selectors in the set.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Replace retained inline CSSOM state. An empty map disables the extra source lookup.
    pub fn set_inline_inputs(&mut self, inputs: HashMap<NodeId, Vec<InputDeclaration>>) -> bool {
        let changed = self.pending.as_deref().is_some_and(|old| old != &inputs) || self.pending.is_none() && !inputs.is_empty();
        self.pending = (!inputs.is_empty()).then(|| Box::new(inputs));
        changed
    }

    /// Add a published CSSOM sheet with retained blocks for its top-level style rules.
    pub fn add_inputs(&mut self, doc: &mut Document, text: &str, origin: Origin, layer: Option<&str>, scope: NodeId, blocks: &[Option<Vec<InputDeclaration>>]) {
        let layer = layer.map_or(Some(0), |path| self.layer_path(0, path));
        if let Some(layer) = layer {
            let scope = if scope == doc.root() { 0 } else { scope.index() as u32 };
            let rules = self.parse_sheet(text);
            self.add_rules(doc, rules, origin, layer, scope, 0, Some(blocks));
        }
    }

    /// Read a style sheet and add its rules. Add the style sheets in the order of the page: a later rule wins a tie.
    /// The names of the selectors go in the name table of `doc`, so call this before `compute`.
    pub fn add(&mut self, doc: &mut Document, text: &str, origin: Origin) {
        self.add_in_layer(doc, text, origin, None);
    }

    /// The same, for a style sheet of the shadow tree `scope` (a shadow root).
    pub fn add_scoped(&mut self, doc: &mut Document, text: &str, origin: Origin, layer: Option<&str>, scope: NodeId) {
        let layer = layer.map_or(Some(0), |path| self.layer_path(0, path));
        if let Some(layer) = layer {
            let rules = self.parse_sheet(text);
            self.add_rules(doc, rules, origin, layer, scope.index() as u32, 0, None);
        }
    }

    /// The same, for a style sheet that is in a layer: `@import url(a.css) layer(x)`. `layer` is the name (`x`, or `x.y`).
    /// An empty name is a layer with no name.
    pub fn add_in_layer(&mut self, doc: &mut Document, text: &str, origin: Origin, layer: Option<&str>) {
        let layer = layer.map_or(Some(0), |path| self.layer_path(0, path));
        if let Some(layer) = layer {
            let rules = self.parse_sheet(text);
            self.add_rules(doc, rules, origin, layer, 0, 0, None);
        }
    }

    /// Keep a copy of the text of a sheet, and parse it without reading the blocks of its rules (`ensure` reads them).
    fn parse_sheet<'t>(&mut self, text: &'t str) -> Vec<Rule<'t>> {
        self.source = self.sources.len() as u32;
        self.sources.push(text.into());
        parse_stylesheet_lazy(text)
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
    /// `scope` is the root of the shadow tree that the sheet belongs to, or 0 for the document.
    fn add_rules(&mut self, doc: &mut Document, rules: Vec<Rule<'_>>, origin: Origin, layer: u32, scope: u32, depth: usize, blocks: Option<&[Option<Vec<InputDeclaration>>]>) {
        for (rule_index, rule) in rules.into_iter().enumerate() {
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
                                self.add_rules(doc, inner, origin, inner_layer, scope, depth + 1, None);
                            }
                        }
                        _ => {}
                    }
                    continue;
                }
                Rule::At(at) => {
                    if at.name.ends_with("keyframes") && matches!(at.body, AtBody::Rules(_)) {
                        self.keyframes.insert(at.prelude.trim().trim_matches(['"', '\'']).into());
                        continue;
                    }
                    let AtBody::Rules(inner) = at.body else { continue };
                    let yes = match &*at.name {
                        "media" => at_rules::media_matches(at.prelude, self.viewport),
                        "supports" => at_rules::supports_matches(doc, at.prelude),
                        _ => false,
                    };
                    // The parser stops at depth 32, so this is a safety net for the stack.
                    if yes && depth < 32 {
                        self.add_rules(doc, inner, origin, layer, scope, depth + 1, None);
                    }
                    continue;
                }
            };
            // A rule with a selector that is not valid is dropped, with all its selectors (the standard says so).
            let Some(selectors) = parse_selector_list_in(rule.prelude, doc, if scope == 0 { Mode::Document } else { Mode::Shadow }) else { continue };
            let body = if let Some(Some(input)) = blocks.and_then(|b| b.get(rule_index)) {
                self.read_block(|side, decls, customs| input_declarations(input, side, decls, customs))
            } else if let Some((text, motion)) = rule.lazy_block() {
                // Not read now. A block that cannot have a declaration (`a{}`, `a{;;}`, `a{:red}`) makes no entry, as a block that was read.
                if !may_declare(&self.sources[self.source as usize][text.clone()]) {
                    continue;
                }
                self.motion.set(self.motion.get() | motion);
                Some(Body { decls: Cell::new((text.start as u32, text.end as u32)), customs: Cell::default(), pics: Cell::default(), unread: Cell::new(true), motion, source: self.source })
            } else {
                self.read_block(|side, decls, customs| {
                    for d in rule.declarations().iter() {
                        if d.name.starts_with("--") {
                            customs.push(CustomDecl { name: (*d.name).into(), text: d.value.into(), important: d.important });
                        } else {
                            parse_declaration(&d.name, d.value, d.important, side, decls);
                        }
                    }
                })
            };
            let Some(body) = body else { continue };
            self.bodies.push(body);
            let body = self.bodies.len() as u32 - 1;
            for selector in selectors {
                self.push(selector, body, layer, origin, scope);
            }
        }
    }

    fn push(&mut self, selector: Selector, body: u32, layer: u32, origin: Origin, scope: u32) {
        if selector.compounds.iter().any(|c| c.simples.contains(&Simple::Never)) {
            return;
            // `a:hover` never matches. Keep it out of the index: it only costs time.
        }
        self.user |= crate::selector::user_flags(&selector);
        self.scopes.add(&selector);
        self.reach.take();
        let index = self.entries.len() as u32;
        // A rule for the host, for a slotted node or for a part is not offered to the elements of its tree: the cascade looks for it where it applies.
        let host = scope != 0 && selector.compounds[0].simples.iter().any(|s| matches!(s, Simple::Host(_)));
        let mut ctx = match (selector.pseudo.as_deref(), host) {
            (Some(ShadowPseudo::Part { .. }), _) => OUTER,
            (Some(ShadowPseudo::Slotted(_)), _) | (None, true) => INNER,
            (None, false) => OWN,
        };
        if scope == 0 {
            if ctx == OUTER {
                self.parts.push(index);
                ctx = OUTERMOST;
            } else if ctx == OWN {
                self.index.push(index, &selector);
            }
        } else {
            let tree = self.shadows.entry(scope).or_default();
            match (selector.pseudo.as_deref(), host) {
                (Some(ShadowPseudo::Part { .. }), _) => tree.parts.push(index),
                (Some(ShadowPseudo::Slotted(_)), _) => tree.slotted.push(index),
                (None, true) => tree.host.push(index),
                (None, false) => tree.index.push(index, &selector),
            }
        }
        let ancestors = ancestor_hashes(&selector, false);
        self.entries.push(Entry { selector, body, layer, origin, ancestors, ctx });
    }

    /// Read the declarations of a block into the lists of the rule set: `read` adds them. Gives the body, or `None` when the block has no declaration.
    fn read_block(&mut self, read: impl FnOnce(&mut Side, &mut Vec<Declared>, &mut Vec<CustomDecl>)) -> Option<Body> {
        let (decls, customs, side) = (self.decls.get_mut(), self.customs.get_mut(), self.side.get_mut());
        let (ranges, logical) = read_block(decls, customs, side, read);
        *self.logical.get_mut() |= logical;
        ranges.map(|(decls, customs, pics)| Body { decls: Cell::new(decls), customs: Cell::new(customs), pics: Cell::new(pics), unread: Cell::new(false), motion: false, source: 0 })
    }

    /// Read the blocks of the rules that an element matched, when they are not read yet. A block is read once, when a style needs it.
    #[inline(always)]
    fn ensure(&self, matched: &[u32]) {
        for &i in matched {
            let body = &self.bodies[self.entries[i as usize].body as usize];
            if body.unread.get() {
                self.read_lazy(body);
            }
        }
    }

    #[cold]
    #[inline(never)]
    fn read_lazy(&self, body: &Body) {
        let text = &self.sources[body.source as usize][body.decls.get().0 as usize..body.decls.get().1 as usize];
        let list = parse_block(text);
        let (mut decls, mut customs, mut side) = (self.decls.borrow_mut(), self.customs.borrow_mut(), self.side.borrow_mut());
        let (ranges, logical) = read_block(&mut decls, &mut customs, &mut side, |side, decls, customs| {
            for d in &list {
                if d.name.starts_with("--") {
                    customs.push(CustomDecl { name: (*d.name).into(), text: d.value.into(), important: d.important });
                } else {
                    parse_declaration(&d.name, d.value, d.important, side, decls);
                }
            }
        });
        if logical {
            self.logical.set(true);
        }
        let (decls, customs, pics) = ranges.unwrap_or_default();
        body.decls.set(decls);
        body.customs.set(customs);
        body.pics.set(pics);
        body.unread.set(false);
    }

    /// The candidate rules tested so far: one for each (element, rule) that the index offered, and one for each look-up in a split list.
    /// Take the difference around a call to `compute` or `restyle`.
    pub fn tested_candidates(&self) -> u64 {
        self.tested.get()
    }

    /// The elements that `compute` and `restyle` styled so far. Take the difference around a call.
    pub fn styled_elements(&self) -> u64 {
        self.styled.get()
    }

    /// Find the style of every element that is shown. The selectors must come from `add` with this same `doc`.
    pub fn compute(&self, doc: &Document) -> Styles {
        self.walk_any::<false>(doc, None, false)
    }

    /// `compute`, and the styles keep the cache of the cascade, so that `restyle` can update a part of them later.
    pub fn compute_kept(&self, doc: &Document) -> Styles {
        self.walk_any::<false>(doc, None, true)
    }

    /// The same, and the element `want` gets its style too, even inside `display:none` (for `getComputedStyle`: CSSOM gives the
    /// computed values of an element that is not rendered). Only the elements from the root down to `want` are styled inside a
    /// hidden subtree, so the cost of a hidden read is the depth of `want`. Layout asks with `None` and pays nothing: the walk
    /// below changes only at a `display:none` element and on the way from it to `want`.
    /// An element that the walk does not reach (a light child of a shadow host that no slot takes) still has no style.
    pub fn compute_for(&self, doc: &Document, want: Option<NodeId>) -> Styles {
        if want.is_some() {
            self.walk_any::<true>(doc, want, false)
        } else {
            self.walk_any::<false>(doc, None, false)
        }
    }

    /// `walk` with the flags that the rule set decides: retained inputs, and the path of the ancestor filter (only when a list has a split).
    #[inline(always)]
    fn walk_any<const WANT: bool>(&self, doc: &Document, want: Option<NodeId>, keep: bool) -> Styles {
        match (self.pending.is_some(), self.tracks()) {
            (true, true) => self.walk::<WANT, true, true>(doc, want, keep),
            (true, false) => self.walk::<WANT, true, false>(doc, want, keep),
            (false, true) => self.walk::<WANT, false, true>(doc, want, keep),
            (false, false) => self.walk::<WANT, false, false>(doc, want, keep),
        }
    }

    /// Const flags omit hidden-node and retained-input work from the ordinary layout loop. `TRACK`: `AncestorFilter::track`, as a flag of
    /// the code, so that a page with no split list pays no test for it in the loop.
    #[inline(never)]
    fn walk<const WANT: bool, const PENDING: bool, const TRACK: bool>(&self, doc: &Document, want: Option<NodeId>, keep: bool) -> Styles {
        let mut way = Way { want, path: Vec::new(), sorted: Vec::new(), on: false, at: 0 };
        let mut styles = Styles { styles: vec![ComputedStyle::initial()], of_node: vec![NONE; doc.len()], calcs: Vec::new(), grids: Vec::new(), customs: var::Sets::default(), spaced: false, wrapping: false, kept: None };
        // Style 0 is the starting style. It is the parent style of the `html` element.
        let mut cx = Cx { viewport: self.viewport, root_font_size: 16.0, layer_order: self.layer_order(), declared: &self.side, inline: RefCell::default(), computed: RefCell::default(), grids: RefCell::default(), customs: RefCell::default(), merges: RefCell::default() };
        let style_atom = doc.find_atom("style");
        let hints = crate::hints::Hints::new(doc);
        let mut filter = AncestorFilter::new(TRACK);
        let mut memo = Memo::default();
        let (mut candidates, mut matched): (Vec<u32>, Vec<u32>) = (Vec::new(), Vec::new());
        let mut stack = vec![(doc.root(), false, 0u32)];
        // The walk is a loop with a stack, and not a function that calls itself: a deep tree cannot overflow the call stack.
        // `true` means that we leave the node, and we take its hashes out of the filter.
        // It walks the tree of the cascade: a host's children are those of its shadow root, and a slot's are the nodes that it takes (or its own
        // children when it takes none). The third value is the tree of the node: 0 for the document, else its shadow root. In a shadow tree
        // the rules of the document do not match (Stylo, Blink): the browser's rules and the rules of the tree do.
        let shadowed = doc.has_shadow_roots();
        while let Some((node, leaving, scope)) = stack.pop() {
            match doc.data(node) {
                NodeData::Element { .. } if leaving => {
                    filter.remove_element_with::<TRACK>(doc, node);
                    continue;
                }
                NodeData::Element { .. } => {
                    let parent = doc.style_parent(node).map_or(0, |p| styles.of_node[p.index()]);
                    let parent = if parent == NONE { 0 } else { parent };
                    if shadowed {
                        // Outside the hot loop: a page without a shadow root pays one test of a local for this.
                        self.match_shadowed(doc, node, scope, &filter, &mut (&mut candidates, &mut matched));
                    } else {
                        self.candidates_with::<TRACK>(doc, node, &mut candidates, &filter);
                        matched.clear();
                        for &i in &candidates {
                            let e = &self.entries[i as usize];
                            let possible = e.ancestors.iter().take_while(|&&h| h != 0).all(|&h| filter.may_contain(h));
                            if possible && matches(doc, node, &e.selector) {
                                matched.push(i);
                            }
                        }
                    }
                    matched.sort_unstable();
                    // The order of the rules is their place in the list, so the key of the cache and the result do not depend on the buckets.
                    let inline = style_atom.and_then(|a| doc.attr(node, a));
                    let input = if PENDING { self.pending.as_ref().and_then(|all| all.get(&node)).map(Vec::as_slice) } else { None };
                    let hint = hints.as_ref().and_then(|h| h.of(doc, node));
                    let id = self.style_of::<PENDING>(&mut styles, &mut memo, parent, &matched, (inline, hint.as_deref()), &cx, input);
                    styles.of_node[node.index()] = id;
                    let hidden = styles.styles[id as usize].get(Property::Display) == Value::Keyword(Keyword::None);
                    if hidden {
                        // No style for what is inside, and no filter work. `compute_for` goes on only to the node on the way to `want`.
                        if WANT {
                            way.visit(doc, node, id, true, scope, (&styles, &mut cx, &mut filter, &mut stack));
                        }
                        continue;
                    }
                    if WANT && way.on {
                        way.visit(doc, node, id, false, scope, (&styles, &mut cx, &mut filter, &mut stack));
                        continue;
                    }
                    if doc.parent(node) == Some(doc.root()) {
                        // The root element. `rem` is a multiple of its font size.
                        cx.root_font_size = styles.styles[id as usize].px(Property::FontSize);
                    }
                    filter.add_element_with::<TRACK>(doc, node);
                    stack.push((node, true, scope));
                }
                NodeData::Document => {}
                _ => continue,
            }
            // The children, so that the first child comes out of the stack first.
            if shadowed {
                push_children(doc, node, scope, &mut stack);
                continue;
            }
            let mut child = doc.last_child(node);
            while let Some(c) = child {
                stack.push((c, false, 0));
                child = doc.prev_sibling(c);
            }
        }
        let (computed, mut grids, customs) = (cx.computed.take(), cx.grids.take(), cx.customs.take());
        styles.calcs = computed.list;
        styles.grids = std::mem::take(&mut grids.list);
        styles.customs = customs;
        if keep {
            styles.kept = Some(Box::new(StyleKeep { memo, calcs: computed.index, grids, rem: cx.root_font_size, base: styles.styles.len() }));
        }
        styles
    }

    /// Compute again the styles of a part of the page, in `styles` from `compute_kept` of this set and this `doc` (the tree may
    /// have changed since). For each root: `(el, false)` styles `el`, and its subtree only if its style changes; `(el, true)`
    /// styles its whole subtree. The elements whose style number changed go to `changed`, with the old number (`Styles::changed`).
    /// A new node gets its style here, as a part of the subtree of a root. Gives `false` when it cannot (the styles keep no
    /// cache, the font size of the root element changes, or the styles grew too much): then the caller runs
    /// `compute_kept`; no node has a new style then. Equal style numbers mean equal values, so a style that did not change keeps its number.
    pub fn restyle(&self, doc: &Document, styles: &mut Styles, roots: &[(NodeId, bool)], changed: &mut Vec<(NodeId, u32)>) -> bool {
        if self.pending.is_some() { self.restyle_with::<true>(doc, styles, roots, changed) } else { self.restyle_with::<false>(doc, styles, roots, changed) }
    }

    fn restyle_with<const PENDING: bool>(&self, doc: &Document, styles: &mut Styles, roots: &[(NodeId, bool)], changed: &mut Vec<(NodeId, u32)>) -> bool {
        if self.scoped(doc) {
            return false;
        }
        let html = doc.children(doc.root()).find(|&n| matches!(doc.data(n), NodeData::Element { .. }));
        let Some(mut k) = styles.kept.take() else { return false };
        // A style that a restyle makes stays in the list: a page that makes new ones all the time starts again from time to time.
        if styles.styles.len() > 2 * k.base + 4096 {
            return false;
        }
        if styles.of_node.len() < doc.len() {
            styles.of_node.resize(doc.len(), NONE);
        }
        let cx = self.kept_cx(styles, &mut k);
        let style_atom = doc.find_atom("style");
        let hints = crate::hints::Hints::new(doc);
        let mut ok = true;
        // Parents first, a subtree root before a root of the same element. A root inside a subtree that an earlier root styled is skipped, so
        // each element is styled once (`covered`: the roots that styled their whole subtree). Without it, a hover that gives an element and
        // all its ancestors as roots styles the document once for each of them.
        let mut order: Vec<(usize, NodeId, bool)> = roots.iter().map(|&(n, sub)| (std::iter::successors(Some(n), |&p| doc.flat_parent(p)).count(), n, sub)).collect();
        order.sort_unstable_by_key(|r| (r.0, r.1.index(), !r.2));
        order.dedup_by(|a, b| a.1 == b.1);
        let mut covered = Covered::default();
        let (mut candidates, mut matched): (Vec<u32>, Vec<u32>) = (Vec::new(), Vec::new());
        // As in `walk`: the flat tree, and only the browser's rules in a shadow tree. The third value says the node is in one.
        let mut stack: Vec<(NodeId, bool, bool)> = Vec::new();
        let in_shadow = |n: NodeId| doc.has_shadow_roots() && doc.parent(n).is_some_and(|p| doc.shadow_host(p).is_some());
        'roots: for &(_, root, subtree) in &order {
            if !matches!(doc.data(root), NodeData::Element { .. }) {
                continue;
            }
            // Only a connected root that is shown: inside `display: none` all styles are `NONE`, and the hidden element has its own.
            if covered.has(root) {
                continue;
            }
            let mut filter = AncestorFilter::new(self.tracks());
            let mut shadow = in_shadow(root);
            let mut up = doc.flat_parent(root);
            while let Some(a) = up {
                if a == doc.root() {
                    break;
                }
                if covered.has(a) {
                    continue 'roots;
                }
                let s = styles.of_node[a.index()];
                if s == NONE || styles.styles[s as usize].get(Property::Display) == Value::Keyword(Keyword::None) {
                    continue 'roots;
                }
                filter.add_element(doc, a);
                shadow |= in_shadow(a);
                up = doc.flat_parent(a);
            }
            if up.is_none() {
                continue;
            }
            stack.clear();
            stack.push((root, false, shadow));
            while let Some((node, leaving, shadow)) = stack.pop() {
                if !matches!(doc.data(node), NodeData::Element { .. }) {
                    continue;
                }
                if leaving {
                    filter.remove_element(doc, node);
                    continue;
                }
                let parent = doc.flat_parent(node).map_or(0, |p| styles.of_node[p.index()]);
                let parent = if parent == NONE { 0 } else { parent };
                self.candidates(doc, node, &mut candidates, &filter);
                if shadow {
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
                let inline = style_atom.and_then(|a| doc.attr(node, a));
                let input = if PENDING { self.pending.as_ref().and_then(|all| all.get(&node)).map(Vec::as_slice) } else { None };
                let hint = hints.as_ref().and_then(|h| h.of(doc, node));
                let id = self.style_of::<PENDING>(styles, &mut k.memo, parent, &matched, (inline, hint.as_deref()), &cx, input);
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
                        let mut gone: Vec<NodeId> = doc.flat_children(node).collect();
                        while let Some(n) = gone.pop() {
                            if std::mem::replace(&mut styles.of_node[n.index()], NONE) != NONE {
                                gone.extend(doc.flat_children(n));
                            }
                        }
                    }
                    continue;
                }
                if node == root {
                    if !subtree && old == id {
                        continue;
                    }
                    covered.add(root);
                }
                filter.add_element(doc, node);
                stack.push((node, true, shadow));
                let host = doc.shadow_root(node);
                let mut child = doc.last_child(host.unwrap_or(node));
                while let Some(c) = child {
                    stack.push((c, false, shadow || host.is_some()));
                    child = doc.prev_sibling(c);
                }
            }
        }
        Self::put_back(cx, styles, k);
        ok
    }

    /// Does the cascade of `doc` use the rules of a shadow tree, or slots? A partial restyle cannot follow them: a change reaches through the host, a slot
    /// and a part, and `restyle` and `style_hidden` give up (the caller runs the whole cascade).
    fn scoped(&self, doc: &Document) -> bool {
        doc.has_shadow_roots() && (!self.shadows.is_empty() || !self.parts.is_empty() || doc.has_slots())
    }

    /// A `Cx` that adds to the value lists of `styles` and to the indexes of `k`, so that equal values keep their numbers.
    fn kept_cx(&self, styles: &mut Styles, k: &mut StyleKeep) -> Cx<'_> {
        Cx {
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
            customs: RefCell::new(std::mem::take(&mut styles.customs)),
            merges: RefCell::default(),
        }
    }

    /// Give the lists of `kept_cx` back to `styles` and `k`.
    fn put_back(cx: Cx<'_>, styles: &mut Styles, mut k: Box<StyleKeep>) {
        let c = cx.computed.take();
        (styles.calcs, k.calcs) = (c.list, c.index);
        let mut g = cx.grids.take();
        styles.grids = std::mem::take(&mut g.list);
        k.grids = g;
        styles.customs = cx.customs.take();
        styles.kept = Some(k);
    }

    /// The style of `want`, an element inside `display: none`, from `styles` of `compute_kept` (or `restyle`) of this `doc`: the
    /// values that `compute_for(doc, Some(want))` gives. The elements on the way from the hidden ancestor down to `want` get
    /// their styles at the end of the list, and no node points to them, so `styles` still describe the page. The cost is the depth
    /// of `want`, not the size of the page. `None` when it cannot: the styles keep no cache or grew too much, the root element
    /// is hidden (`rem`), or `want` is not on a way that the walk takes (a light child of a shadow host). Then use `compute_for`.
    pub fn style_hidden(&self, doc: &Document, styles: &mut Styles, want: NodeId) -> Option<ComputedStyle> {
        if self.pending.is_some() { self.hidden_with::<true>(doc, styles, want) } else { self.hidden_with::<false>(doc, styles, want) }
    }

    fn hidden_with<const PENDING: bool>(&self, doc: &Document, styles: &mut Styles, want: NodeId) -> Option<ComputedStyle> {
        if self.scoped(doc) {
            return None;
        }
        let mut path: Vec<NodeId> = std::iter::successors(Some(want), |&n| doc.flat_parent(n)).collect();
        if path.pop() != Some(doc.root()) || path.is_empty() {
            return None;
        }
        path.reverse();
        let hidden = |id: u32| styles.styles[id as usize].get(Property::Display) == Value::Keyword(Keyword::None);
        // The first element with no style. The one above it is `display: none`.
        let first = path.iter().position(|n| styles.of_node[n.index()] == NONE)?;
        if first == 0 || hidden(styles.of_node[path[0].index()]) || !hidden(styles.of_node[path[first - 1].index()]) {
            return None;
        }
        let mut k = styles.kept.take()?;
        if styles.styles.len() > 2 * k.base + 4096 {
            styles.kept = Some(k);
            return None;
        }
        let in_shadow = |n: NodeId| doc.has_shadow_roots() && doc.parent(n).is_some_and(|p| doc.shadow_host(p).is_some());
        let mut filter = AncestorFilter::new(self.tracks());
        let mut shadow = false;
        for &a in &path[..first] {
            filter.add_element(doc, a);
            shadow |= in_shadow(a);
        }
        let cx = self.kept_cx(styles, &mut k);
        let style_atom = doc.find_atom("style");
        let hints = crate::hints::Hints::new(doc);
        let (mut candidates, mut matched): (Vec<u32>, Vec<u32>) = (Vec::new(), Vec::new());
        let mut parent = styles.of_node[path[first - 1].index()];
        for &node in &path[first..] {
            shadow |= in_shadow(node);
            self.candidates(doc, node, &mut candidates, &filter);
            if shadow {
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
            let inline = style_atom.and_then(|a| doc.attr(node, a));
            let input = if PENDING { self.pending.as_ref().and_then(|all| all.get(&node)).map(Vec::as_slice) } else { None };
            let hint = hints.as_ref().and_then(|h| h.of(doc, node));
            parent = self.style_of::<PENDING>(styles, &mut k.memo, parent, &matched, (inline, hint.as_deref()), &cx, input);
            filter.add_element(doc, node);
        }
        Self::put_back(cx, styles, k);
        Some(styles.styles[parent as usize].clone())
    }

    /// The rules that match `node` when the document has a shadow root: `matched` is the list (not sorted). `scope` is the tree of `node` (0 for the
    /// document). The rules of the document do not reach into a shadow tree: its elements get the browser's rules and the rules of the
    /// tree. A host also gets the `:host` rules of its tree, a node that a slot takes the `::slotted()` rules of the tree of the slot, and an
    /// element with a `part` the `::part()` rules of the trees around it.
    #[inline(never)]
    fn match_shadowed(&self, doc: &Document, node: NodeId, scope: u32, filter: &AncestorFilter, (candidates, matched): &mut (&mut Vec<u32>, &mut Vec<u32>)) {
        self.candidates(doc, node, candidates, filter);
        if scope != 0 {
            candidates.retain(|&i| self.entries[i as usize].origin == Origin::UserAgent);
            if let Some(tree) = self.shadows.get(&scope) {
                tree.index.collect(doc, node, candidates, &self.entries, filter);
            }
        }
        matched.clear();
        for &i in candidates.iter() {
            let e = &self.entries[i as usize];
            let possible = e.ancestors.iter().take_while(|&&h| h != 0).all(|&h| filter.may_contain(h));
            if possible && matches(doc, node, &e.selector) {
                matched.push(i);
            }
        }
        if self.shadows.is_empty() && self.parts.is_empty() {
            return;
        }
        if let Some(root) = doc.shadow_root(node)
            && let Some(tree) = self.shadows.get(&(root.index() as u32))
        {
            matched.extend(tree.host.iter().copied().filter(|&i| matches(doc, node, &self.entries[i as usize].selector)));
        }
        if let Some(slot) = doc.assigned_slot(node)
            && let Some(tree) = self.shadows.get(&(doc.tree_root(slot).index() as u32))
        {
            for &i in &tree.slotted {
                let e = &self.entries[i as usize];
                if let Some(ShadowPseudo::Slotted(arg)) = e.selector.pseudo.as_deref()
                    && compound_match(doc, node, arg)
                    && matches(doc, slot, &e.selector)
                {
                    matched.push(i);
                }
            }
        }
        if scope != 0 {
            self.match_parts(doc, node, matched);
        }
    }

    /// The `::part()` rules for `node`, an element of a shadow tree. A rule of the tree around the host matches when the host matches the selector
    /// and `node` has all the names. Each host on the way out passes on the parts that its `exportparts` names.
    fn match_parts(&self, doc: &Document, node: NodeId, matched: &mut Vec<u32>) {
        let Some(part) = doc.find_atom("part") else { return };
        let export = doc.find_atom("exportparts");
        let Some(names) = doc.attr(node, part) else { return };
        let mut names: Vec<String> = names.split_ascii_whitespace().map(str::to_string).collect();
        let mut root = doc.tree_root(node);
        while !names.is_empty()
            && let Some(host) = doc.shadow_host(root)
        {
            let outer = doc.tree_root(host);
            let rules = if outer == doc.root() { Some(&self.parts) } else { self.shadows.get(&(outer.index() as u32)).map(|t| &t.parts) };
            for &i in rules.into_iter().flatten() {
                let e = &self.entries[i as usize];
                if let Some(ShadowPseudo::Part { names: want, then }) = e.selector.pseudo.as_deref()
                    && want.iter().all(|w| names.iter().any(|n| n == &**w))
                    && compound_match(doc, node, then)
                    && matches(doc, host, &e.selector)
                {
                    matched.push(i);
                }
            }
            // `exportparts="inner: outer, same"`
            let mut next = Vec::new();
            for item in export.and_then(|e| doc.attr(host, e)).unwrap_or("").split(',') {
                let (inner, outer) = item.split_once(':').map_or((item.trim(), item.trim()), |(a, b)| (a.trim(), b.trim()));
                if !outer.is_empty() && names.iter().any(|n| n == inner) {
                    next.push(outer.to_string());
                }
            }
            names = next;
            root = outer;
        }
    }

    /// Does a list of the index of the document or of a shadow tree have a split? Then the ancestor filter keeps the hashes of its path.
    fn tracks(&self) -> bool {
        self.index.bigs(&self.entries).is_some() || self.shadows.values().any(|s| s.index.bigs(&self.entries).is_some())
    }

    /// The rules of the document that may match the element (`Index::collect`).
    #[inline(always)]
    fn candidates(&self, doc: &Document, el: NodeId, out: &mut Vec<u32>, filter: &AncestorFilter) {
        if filter.track {
            self.candidates_with::<true>(doc, el, out, filter);
        } else {
            self.candidates_with::<false>(doc, el, out, filter);
        }
    }

    #[inline(always)]
    fn candidates_with<const TRACK: bool>(&self, doc: &Document, el: NodeId, out: &mut Vec<u32>, filter: &AncestorFilter) {
        out.clear();
        let lookups = self.index.collect_with::<TRACK>(doc, el, out, &self.entries, filter);
        self.tested.set(self.tested.get() + out.len() as u64 + lookups);
        self.styled.set(self.styled.get() + 1);
    }

    /// The number of the style of an element. It is found in the cache, or it is made.
    #[inline(always)]
    fn style_of<const PENDING: bool>(&self, styles: &mut Styles, memo: &mut Memo, parent: u32, matched: &[u32], (inline, hint): (Option<&str>, Option<&str>), cx: &Cx<'_>, input: Option<&[InputDeclaration]>) -> u32 {
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
        self.ensure(matched);
        let mut style = self.cascade_as::<0, PENDING>(matched, inline, hint, &styles.styles[parent as usize], cx, input);
        if style.2 != 0 {
            self.picture_pass::<PENDING>(matched, (inline, hint), &mut style, &styles.styles[parent as usize], cx, input);
        }
        // Two different rule lists may give the same style (`div div` inside `div`). Then they share it, and the children of both
        // have the same parent style, so they find each other in the cache.
        let hash = style_hash(&style);
        let same = memo.by_hash.get(&hash).and_then(|list| list.iter().copied().find(|&i| styles.styles[i as usize] == style));
        let id = same.unwrap_or_else(|| {
            let id = styles.styles.len() as u32;
            styles.spaced |= style.px(Property::LetterSpacing) != 0.0 || style.px(Property::WordSpacing) != 0.0;
            styles.wrapping |= matches!(style.get(Property::OverflowWrap), Value::Keyword(Keyword::Anywhere | Keyword::BreakWord))
                || style.get(Property::WordBreak) == Value::Keyword(Keyword::BreakAll);
            styles.styles.push(style);
            memo.by_hash.entry(hash).or_default().push(id);
            id
        });
        if inline.is_none() {
            memo.cache.entry(key).or_default().push(CacheEntry { parent, rules: matched.into(), hint: hint.map(Into::into), style: id });
        }
        id
    }

    /// The pass for an element that is `rtl`, or whose `float` is `inline-start`. The declarations of the inline axis go to the other side
    /// (`margin-inline-start` is `margin-right`), and `float: inline-start` goes to the side of the start of the container: the direction
    /// of the parent. This runs `cascade_as` again from the start: the first pass must not hold its style for a second look, because
    /// that makes every call copy the style (1 KB).
    #[cold]
    #[inline(never)]
    fn logical_pass<const PENDING: bool>(&self, matched: &[u32], inline: Option<&str>, hint: Option<&str>, parent: &ComputedStyle, cx: &Cx<'_>, input: Option<&[InputDeclaration]>) -> ComputedStyle {
        let mut style = self.cascade_as::<1, PENDING>(matched, inline, hint, parent, cx, input);
        if matches!(style.0[Property::Direction as usize], Value::Keyword(Keyword::Rtl)) {
            style = self.cascade_as::<2, PENDING>(matched, inline, hint, parent, cx, input);
        }
        if let Value::Keyword(side @ (Keyword::InlineStart | Keyword::InlineEnd)) = style.0[Property::Float as usize] {
            let parent_rtl = parent.get(Property::Direction) == Value::Keyword(Keyword::Rtl);
            style.0[Property::Float as usize] = Value::Keyword(if (side == Keyword::InlineStart) != parent_rtl { Keyword::Left } else { Keyword::Right });
        }
        style
    }

    /// Pick the winning declaration for each property, then compute the values. `PASS` 0 is the first pass, 1 the same without the check, 2 the one for `rtl` (`logical_pass`).
    fn cascade_as<const PASS: u8, const PENDING: bool>(&self, matched: &[u32], inline: Option<&str>, hint: Option<&str>, parent: &ComputedStyle, cx: &Cx<'_>, input: Option<&[InputDeclaration]>) -> ComputedStyle {
        let mut winners = [None::<Value>; Property::CORE];
        let mut keys = [0u128; Property::CORE];
        // The properties that have a winner, one bit each: the loops below visit only these.
        let mut set = 0u128;
        // The properties whose winner is a declaration that pass 2 moved to the other side.
        let mut swapped = 0u128;
        // The key of a declaration: the one with the bigger key wins. The parts, from the most important: the origin and `!important`,
        // the layer, the specificity, and the place in the page. A `style=""` attribute has no layer (`None`): it beats every layer.
        let key = |important: bool, origin: Origin, ctx: u8, layer: Option<u32>, specificity: u32, order: u32| cascade_key(cx, important, origin, ctx, layer, specificity, order);
        // The presentational hints (`hints.rs`): author declarations that any rule beats. Their key is the least of the author origin.
        let mut inline_side = Side::default();
        if let Some(text) = hint {
            let mut out = Vec::new();
            for d in parse_declarations(text) {
                parse_declaration(&d.name, d.value, false, &mut inline_side, &mut out);
            }
            for d in out.iter().filter(|d| !d.property.is_picture()) {
                let p = d.property as usize;
                let (calcs, vars, grids) = cx.declared_len();
                let v = match d.value {
                    Value::Length(Length::Calc(i)) => Value::Length(Length::Calc(i + calcs)),
                    Value::Var(i) => Value::Var(i + vars),
                    Value::Grid(i) => Value::Grid(i + grids),
                    Value::ColorExpr(i) => Value::ColorExpr(i + vars),
                    v => v,
                };
                winners[p] = Some(v);
                keys[p] = 1 << 98;
                set |= 1 << p;
            }
        }
        // The logical declarations of the inline axis wait here, with their keys, until the direction of the element is known.
        let mut apply = |d: &Declared, origin: Origin, ctx: u8, layer: Option<u32>, specificity: u32, order: u32| {
            let key = key(d.important, origin, ctx, layer, specificity, order);
            let p = if PASS == 2 && d.mirror { mirror(d.property) } else { d.property } as usize;
            if winners[p].is_none() || key >= keys[p] {
                winners[p] = Some(d.value);
                keys[p] = key;
                set |= 1 << p;
                if PASS == 2 {
                    swapped = swapped & !(1 << p) | u128::from(d.mirror) << p;
                }
            }
        };
        // The custom properties depend only on the set of the parent and on the rules that have some. Most elements of a page
        // share both (Tailwind puts 40 of them on `*`), so the set is made once for each pair. A `style=""` attribute may add some: no cache.
        let body_of = |i: u32| &self.bodies[self.entries[i as usize].body as usize];
        let merge_key = inline.is_none().then(|| (parent.1, matched.iter().copied().filter(|&i| { let c = body_of(i).customs.get(); c.0 != c.1 }).collect::<Vec<u32>>()));
        let merged = merge_key.as_ref().and_then(|k| cx.merges.borrow().get(k).copied());
        // The custom properties of the `style=""` attribute. They live until the end of the function, because `own` points into them.
        let mut inline_customs = Vec::new();
        // The custom properties that win: name, key, text. `own_at` says where a name is in the list: a scan is slow for 20,000 names.
        // Sized once: a map that grows from empty to the 40 names of Tailwind's `*` rule moves its table 4 times.
        let n_custom = if merged.is_none() { matched.iter().map(|&i| { let c = body_of(i).customs.get(); (c.1 - c.0) as usize }).sum() } else { 0 };
        let mut own: Vec<(&str, u128, &str)> = Vec::with_capacity(n_custom);
        let mut own_at: HashMap<&str, usize, BuildHasherDefault<Fnv>> = HashMap::with_capacity_and_hasher(n_custom, BuildHasherDefault::default());
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
        // Does a rule of the element have a picture declaration? Almost never: the others cost nothing more (`picture_pass`).
        let mut pictures = 0u16;
        // The blocks are read before this (`ensure`), and none is read while these lists are borrowed.
        let (all_decls, all_customs) = (self.decls.borrow(), self.customs.borrow());
        for &i in matched {
            let e = &self.entries[i as usize];
            let body = body_of(i);
            pictures |= body.pics.get();
            let (decls, customs) = (body.decls.get(), body.customs.get());
            for d in &all_decls[decls.0 as usize..decls.1 as usize] {
                apply(d, e.origin, e.ctx, Some(e.layer), e.selector.specificity, i);
            }
            if merged.is_none() {
                for d in &all_customs[customs.0 as usize..customs.1 as usize] {
                    apply_custom(&mut own, &mut own_at, d, key(d.important, e.origin, e.ctx, Some(e.layer), e.selector.specificity, i));
                }
            }
        }
        // What the `style=""` attribute gives.
        let mut inline_decls = Vec::new();
        // The declarations of the attribute before this one are not for pictures (`split_pictures`); the others follow.
        let mut inline_core = 0;
        {
            let mut shared = cx.inline.borrow_mut();
            shared.calcs.clear();
            shared.deferred.clear();
            shared.grids.clear();
        }
        if let Some(text) = inline {
            if PENDING && let Some(input) = input {
                input_declarations(input, &mut inline_side, &mut inline_decls, &mut inline_customs);
            } else {
                for d in parse_declarations(text) {
                    if d.name.starts_with("--") {
                        inline_customs.push(CustomDecl { name: (*d.name).into(), text: d.value.into(), important: d.important });
                    } else {
                        parse_declaration(&d.name, d.value, d.important, &mut inline_side, &mut inline_decls);
                    }
                }
            }
            // What these values point to comes after the ones of the style sheets (see `Cx::linear` and `Cx::var`).
            let (calc_base, var_base, grid_base) = cx.declared_len();
            for d in &mut inline_decls {
                match &mut d.value {
                    Value::Length(Length::Calc(i)) => *i += calc_base,
                    Value::Var(i) => *i += var_base,
                    Value::Grid(i) => *i += grid_base,
                    Value::ColorExpr(i) => *i += var_base,
                    _ => {}
                }
            }
            inline_core = if inline_decls.iter().any(|d| d.property.is_picture()) { split_pictures(&mut inline_decls, 0).0 } else { inline_decls.len() };
            // The specificity of a `style=""` attribute is more than the most that a selector can have (1 << 30, and the most is one less).
            for d in &inline_decls[..inline_core] {
                apply(d, Origin::Author, OWN, None, 1 << 30, u32::MAX >> 1);
            }
            for d in &inline_customs {
                apply_custom(&mut own, &mut own_at, d, key(d.important, Origin::Author, OWN, None, 1 << 30, u32::MAX >> 1));
            }
        }
        *cx.inline.borrow_mut() = inline_side;
        // The custom properties: the ones of the parent, and the ones of this element. Most elements set none, and share the set of the parent.
        let custom = if let Some(m) = merged {
            m
        } else if own.is_empty() {
            parent.1
        } else {
            let mut sets = cx.customs.borrow_mut();
            let own: Vec<(u32, &str)> = own.iter().map(|o| (sets.name(o.0), o.2)).collect();
            sets.merge(parent.1, &own)
        };
        if let Some(k) = merge_key
            && merged.is_none()
        {
            cx.merges.borrow_mut().insert(k, custom);
        }
        // A value with `var()` is read now, with the custom properties of this element.
        let mut rest = set;
        while rest != 0 {
            let p = rest.trailing_zeros() as usize;
            rest &= rest - 1;
            if let Some(Value::Var(i)) = winners[p] {
                // A swapped declaration reads `var()` as the property that its text names, not the one that it landed on.
                let named = if PASS == 2 && swapped >> p & 1 == 1 { mirror(Property::ALL[p]) } else { Property::ALL[p] };
                winners[p] = Some(self.read_var(i, named, custom, cx));
            }
        }
        let mut style = compute(&winners, set, parent, cx);
        style.1 = custom;
        // 1: a rule or the attribute has a picture declaration, and `style_of` asks `picture_pass` for the set. The style is not borrowed here.
        style.2 = u32::from(pictures != 0 || inline_core < inline_decls.len());
        // A logical property of the inline axis (`margin-inline-start`) is its physical property for `ltr`, so the first pass is the same
        // for every element. An `rtl` element, or a `float` that is `inline-start`, gets `logical_pass`.
        if PASS == 0 && (matches!(style.0[Property::Direction as usize], Value::Keyword(Keyword::Rtl)) || matches!(style.0[Property::Float as usize], Value::Keyword(Keyword::InlineStart | Keyword::InlineEnd))) && (self.logical.get() || inline.is_some()) {
            return self.logical_pass::<PENDING>(matched, inline, hint, parent, cx, input);
        }
        style
    }

    /// The number of the set of picture values of an element (`ComputedStyle::2`), put in `style`. It does for the picture properties what
    /// `cascade_as` and `compute` do for the others: the winner of each one by its key, then `inherit`, `initial`, `var()` and the units.
    /// `cascade_as` says that it must run (`style.2` is 1: a rule or a `style=""` declaration has a picture property, `Entry::pics`). For
    /// the others the cascade has no more work than before, and it does not hold its style for this call.
    #[cold]
    #[inline(never)]
    fn picture_pass<const PENDING: bool>(&self, matched: &[u32], (inline, hint): (Option<&str>, Option<&str>), style: &mut ComputedStyle, parent: &ComputedStyle, cx: &Cx<'_>, input: Option<&[InputDeclaration]>) {
        #[cfg(test)]
        PICTURE_PASSES.with(|n| n.set(n.get() + 1));
        let custom = style.1;
        let mut winners = [None::<Value>; Property::COUNT - Property::CORE];
        let mut keys = [0u128; Property::COUNT - Property::CORE];
        let mut apply = |d: &Declared, origin: Origin, ctx: u8, layer: Option<u32>, specificity: u32, order: u32| {
            let (k, key) = (d.property as usize - Property::CORE, cascade_key(cx, d.important, origin, ctx, layer, specificity, order));
            if winners[k].is_none() || key >= keys[k] {
                (winners[k], keys[k]) = (Some(d.value), key);
            }
        };
        let all_decls = self.decls.borrow();
        for &i in matched {
            let e = &self.entries[i as usize];
            let body = &self.bodies[e.body as usize];
            let end = body.decls.get().1 as usize;
            for d in &all_decls[end..end + usize::from(body.pics.get())] {
                apply(d, e.origin, e.ctx, Some(e.layer), e.selector.specificity, i);
            }
        }
        drop(all_decls);
        // The declarations of the attribute again: `cascade_as` left the side of their values in `cx.inline`, the same for this parse.
        if let Some(text) = inline {
            let (mut side, mut decls) = (Side::default(), Vec::new());
            // Match the prefix that cascade_as retained before inline declarations.
            if let Some(hint) = hint {
                let mut ignored = Vec::new();
                for d in parse_declarations(hint) {
                    parse_declaration(&d.name, d.value, false, &mut side, &mut ignored);
                }
            }
            if PENDING && let Some(input) = input {
                input_declarations(input, &mut side, &mut decls, &mut Vec::new());
            } else {
                for d in parse_declarations(text) {
                    if !d.name.starts_with("--") {
                        parse_declaration(&d.name, d.value, d.important, &mut side, &mut decls);
                    }
                }
            }
            let (calc_base, var_base, grid_base) = cx.declared_len();
            for d in decls.iter_mut().filter(|d| d.property.is_picture()) {
                match &mut d.value {
                    Value::Length(Length::Calc(i)) => *i += calc_base,
                    Value::Var(i) => *i += var_base,
                    Value::Grid(i) => *i += grid_base,
                    Value::ColorExpr(i) => *i += var_base,
                    _ => {}
                }
                apply(d, Origin::Author, OWN, None, 1 << 30, u32::MAX >> 1);
            }
        }
        let fs = style.px(Property::FontSize);
        let mut set = picture_start();
        for (k, winner) in winners.iter().enumerate() {
            let Some(mut v) = *winner else { continue };
            let p = Property::ALL[Property::CORE + k];
            if let Value::Var(i) = v {
                v = self.read_var(i, p, custom, cx);
            }
            set[k] = match v {
                Value::Inherit => parent.picture(p),
                Value::Initial | Value::Unset => initial(p),
                // A mix of units is kept in the table of the sets, because the set outlives the styles of this page. A `min()`, `max()` or
                // `clamp()` is not read: the starting value.
                Value::Length(Length::Calc(i)) => {
                    let lin = cx.linear(i);
                    let (x, pct) = (lin.to_px(fs, cx.root_font_size, cx.viewport), lin.pct());
                    match (calc::is_tree(lin), x, pct) {
                        (true, ..) => initial(p),
                        (_, x, 0.0) => Value::Length(Length::Px(x)),
                        (_, 0.0, pct) => Value::Length(Length::Percent(pct)),
                        _ => keep_picture_calc(x, pct).map_or(initial(p), |i| Value::Length(Length::Calc(i))),
                    }
                }
                v => resolve(p, v, &style.0, parent, fs, cx),
            };
        }
        style.2 = keep_pictures(&set);
    }

    /// The value of `property` for the declaration `i` that has `var()`. `unset` when it does not work: a custom property that is not set,
    /// or a text that is not good for the property (the standard calls it "invalid at computed-value time").
    fn read_var(&self, i: u32, property: Property, custom: u32, cx: &Cx<'_>) -> Value {
        let decl = cx.var(i);
        let sets = cx.customs.borrow();
        let text = var::substitute(&decl.text, &mut |name| sets.get(custom, name).map(str::to_string));
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
            *c += (cx.declared.borrow().calcs.len() + shared.calcs.len()) as u32;
            shared.calcs.extend(side.calcs);
        }
        if let Value::ColorExpr(i) = &mut value {
            let mut shared = cx.inline.borrow_mut();
            *i += (cx.declared.borrow().deferred.len() + shared.deferred.len()) as u32;
            shared.deferred.extend(side.deferred);
        }
        if let Value::Grid(g) = &mut value {
            let mut shared = cx.inline.borrow_mut();
            *g += (cx.declared.borrow().grids.len() + shared.grids.len()) as u32;
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
    hash_values(0xcbf2_9ce4_8422_2325_u64 ^ u64::from(style.1) ^ u64::from(style.2) << 32, &style.0)
}

/// `h` mixed with the computed `values`.
#[inline(always)]
pub(crate) fn hash_values(mut h: u64, values: &[Value]) -> u64 {
    for &v in values {
        let x = match v {
            Value::Color(c) => 1 << 40 | u64::from(u32::from_be_bytes([c.r, c.g, c.b, c.a])),
            Value::Length(Length::Px(x)) => 2 << 40 | u64::from(x.to_bits()),
            Value::Length(Length::Percent(x)) => 3 << 40 | u64::from(x.to_bits()),
            Value::Length(Length::Calc(i)) => 7 << 40 | u64::from(i),
            Value::Grid(i) => 8 << 40 | u64::from(i),
            Value::Number(x) => 4 << 40 | u64::from(x.to_bits()),
            Value::Keyword(k) => 5 << 40 | k as u64,
            Value::ColorExpr(i) => 7 << 40 | i as u64,
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
    declared: &'a RefCell<Side>,
    /// What the `style=""` of the element that we compute now points to, and the `calc()` that come from a `var()`.
    /// Their numbers start after the ones of `declared`.
    inline: RefCell<Side>,
    /// The `calc()` values that are done, and are kept in `Styles::calcs`.
    computed: RefCell<Computed>,
    /// The grid lists that are done, and are kept in `Styles::grids`.
    grids: RefCell<grid::Kept>,
    /// The sets of custom properties, each one once.
    customs: RefCell<var::Sets>,
    /// The set of custom properties for a parent set and the rules with custom properties that an element matched.
    merges: RefCell<HashMap<(u32, Vec<u32>), u32, BuildHasherDefault<Fnv>>>,
}

/// The computed `calc()` values, each one once. Two elements with the same value get the same number, so that their styles can be shared.
#[derive(Default)]
struct Computed {
    list: Vec<[f32; 2]>,
    index: HashMap<[u32; 2], u32, BuildHasherDefault<Fnv>>,
}

impl Cx<'_> {
    /// How many `calc()`, `var()` and grid lists the style sheets have: the numbers of the ones of the `style=""` attribute start there.
    fn declared_len(&self) -> (u32, u32, u32) {
        let d = self.declared.borrow();
        (d.calcs.len() as u32, d.deferred.len() as u32, d.grids.len() as u32)
    }

    fn linear(&self, i: u32) -> Linear {
        let declared = self.declared.borrow();
        match declared.calcs.get(i as usize) {
            Some(&l) => l,
            None => self.inline.borrow().calcs[i as usize - declared.calcs.len()],
        }
    }

    /// The tree of a `min()`, `max()` or `clamp()` that starts at entry `i`, or the sum of units as a leaf.
    fn expr(&self, i: u32) -> Node<Linear> {
        let (inline, declared) = (self.inline.borrow(), self.declared.borrow());
        let n = declared.calcs.len();
        let get = |at: usize| if at < n { declared.calcs[at] } else { inline.calcs[at - n] };
        crate::calc::decode(&get, i as usize).0
    }

    /// The grid list `i` of the style sheets or of the `style=""` attribute.
    fn declared_grid(&self, i: u32) -> Grid {
        let declared = self.declared.borrow();
        match declared.grids.get(i as usize) {
            Some(g) => g.clone(),
            None => self.inline.borrow().grids[i as usize - declared.grids.len()].clone(),
        }
    }

    fn color(&self, i: u32, current: Color) -> Color {
        let resolve = |entry: &Deferred| match entry {
            Deferred::Color(expr) => expr.resolve(current),
            Deferred::Variable(_) => unreachable!("color index names a color expression"),
        };
        let declared = self.declared.borrow();
        match declared.deferred.get(i as usize) {
            Some(entry) => resolve(entry),
            None => resolve(&self.inline.borrow().deferred[i as usize - declared.deferred.len()]),
        }
    }

    fn var(&self, i: u32) -> VarDecl {
        let read = |entry: &Deferred| match entry {
            Deferred::Variable(v) => v.clone(),
            Deferred::Color(_) => unreachable!("variable index names a variable declaration"),
        };
        let declared = self.declared.borrow();
        match declared.deferred.get(i as usize) {
            Some(entry) => read(entry),
            None => read(&self.inline.borrow().deferred[i as usize - declared.deferred.len()]),
        }
    }

    /// Keep a `min()`, `max()` or `clamp()` with a percent in it, and give its number: the place of its first pair in `Styles::calcs`.
    fn keep_math(&self, node: Node<[f32; 2]>) -> u32 {
        let mut c = self.computed.borrow_mut();
        let mut flat = Vec::new();
        calc::flatten(&node, &mut flat);
        // The same tree as one that is there: use it. The list holds sums too, and a tree is many pairs.
        let mut at = 0;
        while at < c.list.len() {
            let end = calc::flat_end(&c.list, at);
            if c.list[at][0].is_nan() && calc::flat_same(&c.list, at, &flat, 0) {
                return at as u32;
            }
            at = end;
        }
        let at = c.list.len() as u32;
        c.list.extend(flat);
        at
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

/// The tree of `n`: 0 for the document, else the number of its shadow root.
fn scope_of(doc: &Document, n: NodeId) -> u32 {
    let root = doc.tree_root(n);
    if root == doc.root() { 0 } else { root.index() as u32 }
}

/// Push the children of `node` in the tree of the cascade, last first: its shadow root's if it is a host, what it takes (or its own fallback
/// children) if it is a slot, else its own. `scope` is the tree of `node`.
#[inline(never)]
fn push_children(doc: &Document, node: NodeId, scope: u32, stack: &mut Vec<(NodeId, bool, u32)>) {
    if let Some(root) = doc.shadow_root(node) {
        let mut child = doc.last_child(root);
        while let Some(c) = child {
            stack.push((c, false, root.index() as u32));
            child = doc.prev_sibling(c);
        }
        return;
    }
    if doc.is_slot(node) {
        let taken = doc.assigned_nodes(node);
        if !taken.is_empty() {
            // The nodes are in the tree of the host.
            let outer = doc.shadow_host(doc.tree_root(node)).map_or(0, |host| scope_of(doc, host));
            stack.extend(taken.into_iter().rev().map(|c| (c, false, outer)));
            return;
        }
    }
    let mut child = doc.last_child(node);
    while let Some(c) = child {
        stack.push((c, false, scope));
        child = doc.prev_sibling(c);
    }
}

/// The way from the root to the element that `compute_for` was asked about. It is made at the first `display:none` element.
struct Way {
    want: Option<NodeId>,
    /// The ancestors of `want` in the tree of the cascade, from the root down to `want`. Empty when `want` is not connected, or not made yet.
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
    fn visit(&mut self, doc: &Document, node: NodeId, id: u32, hidden: bool, scope: u32, (styles, cx, filter, stack): (&Styles, &mut Cx<'_>, &mut AncestorFilter, &mut Vec<(NodeId, bool, u32)>)) {
        let Some(next) = self.step(doc, node, hidden) else { return };
        if doc.parent(node) == Some(doc.root()) {
            cx.root_font_size = styles.styles[id as usize].px(Property::FontSize);
        }
        filter.add_element(doc, node);
        stack.push((node, true, scope));
        stack.push((next, false, scope_of(doc, next)));
    }

    #[cold]
    fn step(&mut self, doc: &Document, node: NodeId, hidden: bool) -> Option<NodeId> {
        if !self.on {
            let want = self.want?;
            if self.path.is_empty() {
                let mut up = Some(want);
                while let Some(n) = up {
                    self.path.push(n);
                    up = doc.style_parent(n);
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
    /// The sets of custom properties (`var::Sets`). `ComputedStyle::1` is the number of a set. Set 0 is empty.
    customs: var::Sets,
    /// Does any style have a `letter-spacing` or a `word-spacing` that is not 0? Layout and paint read the two only then.
    spaced: bool,
    /// Conservative hint: some stored style requests additional text break opportunities.
    wrapping: bool,
    /// The cache of the cascade, for `RuleSet::restyle`. Only `RuleSet::compute_kept` keeps it.
    kept: Option<Box<StyleKeep>>,
}

/// What `RuleSet::restyle` needs from the cascade that made the styles: the style cache and the indexes of the value lists, so
/// that the same values get the same numbers.
struct StyleKeep {
    memo: Memo,
    calcs: HashMap<[u32; 2], u32, BuildHasherDefault<Fnv>>,
    grids: grid::Kept,
    /// The font size of the root element, for `rem`.
    rem: f32,
    /// How many styles the full cascade made.
    base: usize,
}

/// A copy of the styles of the elements, for a host that compares two cascades (`bare/src/browser_motion.rs`). It holds the
/// values and the `calc()` numbers, nothing else: a `Value::Grid` in it points to nothing.
pub struct StyleMap {
    styles: Vec<ComputedStyle>,
    of_node: Vec<u32>,
    calcs: Vec<[f32; 2]>,
}

impl StyleMap {
    pub fn style(&self, node: NodeId) -> Option<&ComputedStyle> {
        match self.of_node.get(node.index()) {
            Some(&i) if i != NONE => self.styles.get(i as usize),
            _ => None,
        }
    }

    /// The pixels and the percent of a `Length::Calc`.
    pub fn calc(&self, i: u32) -> (f32, f32) {
        self.calcs.get(i as usize).map_or((0.0, 0.0), |c| (c[0], c[1]))
    }
}

impl Styles {
    /// Copy the styles (`StyleMap`).
    pub fn map(&self) -> StyleMap {
        StyleMap { styles: self.styles.clone(), of_node: self.of_node.clone(), calcs: self.calcs.clone() }
    }

    /// Does any style satisfy `f`? (Each different style is tested once.)
    pub fn any_style(&self, f: impl Fn(&ComputedStyle) -> bool) -> bool {
        self.styles.iter().any(f)
    }

    pub fn wrapping(&self) -> bool { self.wrapping }

    pub fn spaced(&self) -> bool {
        self.spaced
    }

    /// The custom properties of a style, sorted by name, with their text: `--gap`, `8px`.
    pub fn custom_properties(&self, style: &ComputedStyle) -> Vec<(&str, &str)> {
        self.customs.pairs(style.1)
    }

    /// The pixels and the percent of a `Length::Calc` of a computed style: `calc(px + percent of something)`.
    pub fn calc(&self, i: u32) -> (f32, f32) {
        let [px, pct] = self.calcs[i as usize];
        (px, pct)
    }

    /// The pixels of a `Length::Calc` of a computed style when `base` pixels is 100%. A `min()`, `max()` or `clamp()` has NaN pixels in its first entry.
    pub fn calc_at(&self, i: u32, base: f32) -> f32 {
        let [px, pct] = self.calcs[i as usize];
        if px.is_nan() {
            return self.math_at(i, base);
        }
        px + base * pct / 100.0
    }

    /// The pixels of a `min()`, `max()` or `clamp()` of a computed style (a tree with a percent) when `base` pixels is 100%.
    #[cold]
    #[inline(never)]
    pub fn math_at(&self, i: u32, base: f32) -> f32 {
        calc::flat_eval(&self.calcs, i as usize, &|l| l[0] + base * l[1] / 100.0)
    }

    /// The text of a `min()`, `max()` or `clamp()` of a computed style: `min(300px, 50%)`.
    pub fn math_text(&self, i: u32) -> String {
        calc::flat_text(&self.calcs, i as usize)
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
        // The picture properties are all paint only, and a set has the same number when its values are equal.
        let mut out = if self.styles[a as usize].2 == new.styles[b as usize].2 { Change::Same } else { Change::Paint };
        for (i, &p) in Property::ALL[..Property::CORE].iter().enumerate() {
            let (u, v) = (x[i], y[i]);
            let same = match (u, v) {
                (Value::Length(Length::Calc(i)), Value::Length(Length::Calc(j))) => calc::flat_same(&self.calcs, i as usize, &new.calcs, j as usize),
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
                        for &p in &Property::ALL[..Property::CORE] {
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
            TextColor | BorderTopColor | BorderRightColor | BorderBottomColor | BorderLeftColor | TextDecoration | Opacity | ZIndex | Visibility | PointerEvents | Cursor | Fill | Stroke
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
        Value::Length(Length::Calc(i)) if calcs[i as usize][0].is_nan() => calc::flat_text(calcs, i as usize),
        Value::Length(Length::Calc(i)) => format!("calc({}px + {}%)", calcs[i as usize][0], calcs[i as usize][1]),
        Value::Number(x) => format!("{x}"),
        Value::Keyword(k) => k.name().to_string(),
        Value::Grid(i) => format!("{:?}", grids[i as usize]),
        other => format!("{other:?}"),
        // Not reached for a computed value.
    }
}

/// The style of one element: one value for each property before `Property::CORE`, in the order of `Property::ALL`, the number of its set
/// of custom properties, and the number of its set of picture values (0: all at the starting value, nearly every element).
#[derive(Debug, Clone, PartialEq)]
pub struct ComputedStyle([Value; Property::CORE], u32, u32);

const _: () = assert!(std::mem::size_of::<ComputedStyle>() == 8 * Property::CORE + 8, "a style gets one `u32` for the picture set and no more");

impl ComputedStyle {
    /// The style of an element that has no rule at all, and whose parent has none.
    pub fn initial() -> ComputedStyle {
        let mut values = [Value::Initial; Property::CORE];
        for &p in &Property::ALL[..Property::CORE] {
            values[p as usize] = match initial(p) {
                Value::Keyword(Keyword::CurrentColor) => Value::Color(Color::BLACK),
                v => v,
            };
        }
        // The border style is `none`, so the width is 0, as in `compute`.
        for side in 0..4 {
            values[Property::BorderTopWidth as usize + side] = Value::Length(Length::Px(0.0));
        }
        ComputedStyle(values, 0, 0)
    }

    /// The value of a property before `Property::CORE`. A picture property is not here: ask `value` or `picture`.
    pub fn get(&self, property: Property) -> Value {
        self.0[property as usize]
    }

    /// The value of a picture property (`background-image`, `mask-size-x`, ...), read from the set of the style.
    #[inline(always)]
    pub fn picture(&self, property: Property) -> Value {
        picture_value(self.2, property)
    }

    /// The 12 picture values at once, in the order of `Property::ALL[CORE..]`: one lookup, for the code that reads many of them (paint).
    pub fn pictures(&self) -> Pictures {
        picture_values(self.2)
    }

    /// The value of any property.
    pub fn value(&self, property: Property) -> Value {
        if property.is_picture() { self.picture(property) } else { self.get(property) }
    }

    /// The number of the set of picture values: 0 for the starting values, and then `picture` has nothing to look up.
    pub fn picture_set(&self) -> u32 {
        self.2
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

#[cfg(test)]
thread_local! {
    /// How many times `RuleSet::picture_pass` ran on this thread: the tests check that an element with no picture property never runs it.
    static PICTURE_PASSES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The values of the picture properties of one element: `Property::ALL[CORE..]`, in that order.
pub type Pictures = [Value; Property::COUNT - Property::CORE];

/// The sets of picture values, each one once, for the whole process. A style holds the number of its set (`ComputedStyle::2`): the 12
/// values would make each style 96 bytes larger and each compare slower, for a few elements of a page. Set 0 is the starting values: it is
/// not in the list. A `Length::Calc` in a set points to `PictureTable::calcs`, not to `Styles::calcs`: a set is shared by pages.
#[derive(Default)]
struct PictureTable {
    sets: Vec<Pictures>,
    by_hash: HashMap<u64, Vec<u32>, BuildHasherDefault<Fnv>>,
    calcs: Vec<[f32; 2]>,
}

static PICTURES: std::sync::Mutex<Option<PictureTable>> = std::sync::Mutex::new(None);
/// The most sets, and the most `calc()` values, that the table keeps. A page that makes more gets the starting values.
const MAX_PICTURES: usize = 1 << 16;

fn picture_start() -> Pictures {
    std::array::from_fn(|k| initial(Property::ALL[Property::CORE + k]))
}

/// The value of a picture property in the set `id`.
#[cold]
#[inline(never)]
fn picture_value(id: u32, p: Property) -> Value {
    if id == 0 {
        return initial(p);
    }
    let guard = PICTURES.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().map_or_else(|| initial(p), |t| t.sets[id as usize - 1][p as usize - Property::CORE])
}

#[cold]
#[inline(never)]
fn picture_values(id: u32) -> Pictures {
    let guard = PICTURES.lock().unwrap_or_else(|e| e.into_inner());
    match (id, guard.as_ref()) {
        (1.., Some(t)) => t.sets[id as usize - 1],
        _ => picture_start(),
    }
}

/// The pixels and the percent of a `Length::Calc` in the picture values of a style.
pub fn picture_calc(i: u32) -> (f32, f32) {
    let guard = PICTURES.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().and_then(|t| t.calcs.get(i as usize)).map_or((0.0, 0.0), |c| (c[0], c[1]))
}

/// Keep the value `calc(px + pct%)` for a set. `None` when the table is full.
fn keep_picture_calc(px: f32, pct: f32) -> Option<u32> {
    let mut guard = PICTURES.lock().unwrap_or_else(|e| e.into_inner());
    let t = guard.get_or_insert_with(PictureTable::default);
    let at = t.calcs.iter().position(|c| c.map(f32::to_bits) == [px.to_bits(), pct.to_bits()]);
    match at {
        Some(i) => Some(i as u32),
        None if t.calcs.len() < MAX_PICTURES => {
            t.calcs.push([px, pct]);
            Some(t.calcs.len() as u32 - 1)
        }
        None => None,
    }
}

/// The number of the set `set`. 0 when it is the starting values, or the table is full.
fn keep_pictures(set: &Pictures) -> u32 {
    if *set == picture_start() {
        return 0;
    }
    let hash = hash_values(0xcbf2_9ce4_8422_2325, set);
    let mut guard = PICTURES.lock().unwrap_or_else(|e| e.into_inner());
    let t = guard.get_or_insert_with(PictureTable::default);
    if let Some(&id) = t.by_hash.get(&hash).and_then(|list| list.iter().find(|&&i| t.sets[i as usize - 1] == *set)) {
        return id;
    }
    if t.sets.len() >= MAX_PICTURES {
        return 0;
    }
    t.sets.push(*set);
    let id = t.sets.len() as u32;
    t.by_hash.entry(hash).or_default().push(id);
    id
}

/// Can the text of a block have a declaration? A declaration has a name in front of its `:`. This says `false` only when no `:` has one
/// (there is none, or each one is first in its statement), so a block that it rejects has none for sure.
fn may_declare(text: &str) -> bool {
    text.match_indices(':').any(|(at, _)| !matches!(text.as_bytes()[..at].iter().rev().find(|b| !b.is_ascii_whitespace()), None | Some(b';')))
}

/// Add the declarations of one block to `decls`, `customs` and `side` (`read` does it), and put the picture ones last. Gives their places and
/// the number of picture declarations, or `None` when there is no declaration. The flag says that one has the inline axis (`RuleSet::logical`).
#[allow(clippy::type_complexity)]
fn read_block(decls: &mut Vec<Declared>, customs: &mut Vec<CustomDecl>, side: &mut Side, read: impl FnOnce(&mut Side, &mut Vec<Declared>, &mut Vec<CustomDecl>)) -> (Option<((u32, u32), (u32, u32), u16)>, bool) {
    let (start, custom_start) = (decls.len(), customs.len());
    read(side, decls, customs);
    // The picture declarations go after the others (`Body::pics`). One pass tests each declaration, here, not in the cascade.
    let (mut logical, mut picture) = (false, false);
    for d in &decls[start..] {
        logical |= d.mirror || matches!(d.value, Value::Keyword(Keyword::InlineStart | Keyword::InlineEnd));
        picture |= d.property as usize >= Property::CORE;
    }
    let (core_end, pics) = if picture { split_pictures(decls, start) } else { (decls.len(), 0) };
    if decls.len() == start && customs.len() == custom_start {
        return (None, logical);
    }
    (Some(((start as u32, core_end as u32), (custom_start as u32, customs.len() as u32), pics)), logical)
}

/// Move the declarations of picture properties in `decls[start..]` to the end, in their order, and say where the others end and how many
/// they are. A rule that has more than `u16::MAX` of them keeps the first ones.
#[cold]
#[inline(never)]
fn split_pictures(decls: &mut Vec<Declared>, start: usize) -> (usize, u16) {
    let (core, pics): (Vec<Declared>, Vec<Declared>) = decls.drain(start..).partition(|d| !d.property.is_picture());
    let end = start + core.len();
    decls.extend(core);
    decls.extend(pics.into_iter().take(usize::from(u16::MAX)));
    (end, (decls.len() - end) as u16)
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
        WhiteSpace | OverflowWrap | WordBreak | FontStyle | LineHeight | LetterSpacing | WordSpacing => Value::Keyword(K::Normal),
        TextAlign => Value::Keyword(K::Start),
        ListStyleType => Value::Keyword(K::Disc),
        BoxSizing => Value::Keyword(K::ContentBox),
        TextColor => Value::Color(Color::BLACK),
        BackgroundColor => Value::Color(Color::TRANSPARENT),
        BackgroundImage | MaskImage => Value::Keyword(K::None),
        BackgroundSizeX | BackgroundSizeY | MaskSizeX | MaskSizeY => Value::Keyword(K::Auto),
        BackgroundPositionX | BackgroundPositionY | MaskPositionX | MaskPositionY => Value::Length(Length::Percent(0.0)),
        BackgroundRepeat | MaskRepeat => Value::Keyword(K::Repeat),
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
        TransitionProperty | TransitionDuration | TransitionDelay | TransitionTimingFunction | AnimationName | AnimationDuration | AnimationDelay | AnimationIterationCount => Value::Keyword(K::None),
        GridAutoFlow => Value::Number(0.0),
        BorderCollapse => Value::Keyword(K::Separate),
        CaptionSide => Value::Keyword(K::Top),
        VerticalAlign => Value::Keyword(K::Baseline),
        Direction => Value::Keyword(K::Ltr),
        ZIndex => Value::Keyword(K::Auto),
        ObjectFit => Value::Keyword(K::Fill),
        Fill => Value::Color(Color::BLACK),
        Stroke => Value::Keyword(K::None),
        Opacity => Value::Number(1.0),
        Appearance => Value::Keyword(K::None),
        PointerEvents | Cursor | UserSelect => Value::Keyword(K::Auto),
        BorderTopLeftRadius | BorderTopRightRadius | BorderBottomRightRadius | BorderBottomLeftRadius => px(0.0),
        BorderSpacingH | BorderSpacingV => px(0.0),
        TranslateX | TranslateY => px(0.0),
        MarginTop | MarginRight | MarginBottom | MarginLeft | PaddingTop | PaddingRight | PaddingBottom | PaddingLeft => px(0.0),
        BorderTopWidth | BorderRightWidth | BorderBottomWidth | BorderLeftWidth => px(3.0),
        // `medium`. With the style `none` it is 0: `compute` does that.
        BorderTopColor | BorderRightColor | BorderBottomColor | BorderLeftColor => Value::Keyword(K::CurrentColor),
    }
}

/// Does a child take the value of the parent when it has no value of its own?
pub const fn inherited(p: Property) -> bool {
    use Property::*;
    matches!(p, TextColor | FontSize | FontWeight | FontStyle | LineHeight | LetterSpacing | WordSpacing | TextAlign | TextTransform | WhiteSpace | OverflowWrap | WordBreak | ListStyleType | Visibility | PointerEvents | Cursor | Fill | Stroke | BorderCollapse | BorderSpacingH | BorderSpacingV | CaptionSide | Direction)
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
fn start() -> &'static [Value; Property::CORE] {
    static START: std::sync::LazyLock<[Value; Property::CORE]> = std::sync::LazyLock::new(|| {
        let mut values = [Value::Initial; Property::CORE];
        for &p in &Property::ALL[..Property::CORE] {
            values[p as usize] = initial(p);
        }
        values
    });
    &START
}

#[cold]
#[inline(never)]
fn font_size_math(i: u32, parent_size: f32, cx: &Cx<'_>) -> f32 {
    cx.expr(i).eval(&|l: &Linear| l.to_px(parent_size, cx.root_font_size, cx.viewport) + l.pct() * parent_size / 100.0)
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
            if calc::is_tree(lin) {
                return font_size_math(i, parent_size, cx);
            }
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

/// The key of a declaration: the one with the bigger key wins. The parts, from the most important: the origin and `!important`,
/// the layer, the specificity, and the place in the page. A `style=""` attribute has no layer (`None`): it beats every layer.
#[inline(always)]
fn cascade_key(cx: &Cx<'_>, important: bool, origin: Origin, ctx: u8, layer: Option<u32>, specificity: u32, order: u32) -> u128 {
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
        // The tree context comes after the origin and before the layer: the outer context wins for a normal declaration, the inner one for
        // an important one. The tree of the element (`OWN`) is between them, and a `style=""` attribute is in it.
        let outer = match ctx {
            INNER => 0u128,
            OWN => 1,
            OUTER => 2,
            _ => 3,
        };
        let tree = if important { 3 - outer } else { outer };
        rank << 98 | tree << 96 | u128::from(layer) << 64 | u128::from(specificity) << 32 | u128::from(order)
}

/// Make the computed style from the winning declarations and the style of the parent.
#[inline(always)]
fn compute(winners: &[Option<Value>; Property::CORE], mut set: u128, parent: &ComputedStyle, cx: &Cx<'_>) -> ComputedStyle {
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
            // A grid list or a `calc()` of the parent is computed already: its number is from `Styles::grids` or `Styles::calcs`, not
            // from the style sheet.
            Value::Inherit if matches!(parent.0[i], Value::Grid(_) | Value::Length(Length::Calc(_))) => {
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
    ComputedStyle(out, 0, 0)
}

/// A `min()`, `max()` or `clamp()`: the units now, the percent at layout time (unless the property takes the percent now).
#[cold]
#[inline(never)]
fn resolve_math(p: Property, i: u32, fs: f32, cx: &Cx<'_>) -> Value {
    let m = cx.expr(i).map(&|l: &Linear| [l.to_px(fs, cx.root_font_size, cx.viewport), l.pct()]);
    if p == Property::LineHeight || !m.any(&|l| l[1] != 0.0) {
        Value::Length(Length::Px(m.eval(&|l| l[0] + l[1] * fs / 100.0)))
    } else {
        Value::Length(Length::Calc(cx.keep_math(m)))
    }
}

/// Turn a value into its computed form. It does nothing to a value that is computed already (one from the parent).
#[inline(always)]
fn resolve(p: Property, v: Value, out: &[Value; Property::CORE], parent: &ComputedStyle, fs: f32, cx: &Cx<'_>) -> Value {
    let px = |x| Value::Length(Length::Px(x));
    match v {
        Value::ColorExpr(i) => Value::Color(cx.color(i, if p == Property::TextColor { parent.color(p) } else { as_color(out[Property::TextColor as usize]) })),
        // The color of the element. `color: currentcolor` means the color of the parent (the element has no color yet).
        Value::Keyword(Keyword::CurrentColor) if p == Property::TextColor => Value::Color(parent.color(p)),
        Value::Keyword(Keyword::CurrentColor) => Value::Color(as_color(out[Property::TextColor as usize])),
        Value::Length(Length::Percent(x)) if p == Property::LineHeight => px(x * fs / 100.0),
        Value::Length(Length::Calc(i)) => {
            let lin = cx.linear(i);
            if calc::is_tree(lin) {
                return resolve_math(p, i, fs, cx);
            }
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
    /// Keep `path`? Only when some list of the index is split: the split reads it.
    track: bool,
    /// The distinct hashes on the path, each with how many elements of the path have it. The filter alone cannot give the hashes back.
    path: Vec<(u32, u32)>,
}

impl AncestorFilter {
    fn new(track: bool) -> AncestorFilter {
        AncestorFilter { counts: Box::new([0; 4096]), track, path: Vec::new() }
    }

    fn places(h: u32) -> [usize; 2] {
        [(h & 0xfff) as usize, ((h >> 16) & 0xfff) as usize]
    }

    /// Add the hashes of an element (`element_hashes`).
    #[inline(always)]
    fn add_element(&mut self, doc: &Document, el: NodeId) {
        if self.track { self.add_element_with::<true>(doc, el) } else { self.add_element_with::<false>(doc, el) }
    }

    /// `TRACK` is `self.track`, as a flag of the code.
    #[inline(always)]
    fn add_element_with<const TRACK: bool>(&mut self, doc: &Document, el: NodeId) {
        if TRACK {
            element_hashes(doc, el, &mut |h| self.add_tracked(h));
        } else {
            element_hashes(doc, el, &mut |h| self.add(h));
        }
    }

    /// Take out what `add_element` added.
    #[inline(always)]
    fn remove_element(&mut self, doc: &Document, el: NodeId) {
        if self.track { self.remove_element_with::<true>(doc, el) } else { self.remove_element_with::<false>(doc, el) }
    }

    #[inline(always)]
    fn remove_element_with<const TRACK: bool>(&mut self, doc: &Document, el: NodeId) {
        if TRACK {
            element_hashes(doc, el, &mut |h| self.remove_tracked(h));
        } else {
            element_hashes(doc, el, &mut |h| self.remove(h));
        }
    }

    fn add(&mut self, h: u32) {
        for i in Self::places(h) {
            self.counts[i] = self.counts[i].saturating_add(1);
        }
    }

    fn add_tracked(&mut self, h: u32) {
        // The filter never says "no" for a hash that it has, so after a "no" the hash is new, and the scan is for a "maybe".
        match self.may_contain(h).then(|| self.path.iter_mut().rev().find(|p| p.0 == h)).flatten() {
            Some(p) => p.1 += 1,
            None => self.path.push((h, 1)),
        }
        self.add(h);
    }

    fn remove(&mut self, h: u32) {
        for i in Self::places(h) {
            if self.counts[i] != u8::MAX {
                self.counts[i] -= 1;
            }
        }
    }

    fn remove_tracked(&mut self, h: u32) {
        if let Some(i) = self.path.iter().rposition(|p| p.0 == h) {
            self.path[i].1 -= 1;
            if self.path[i].1 == 0 {
                self.path.swap_remove(i);
            }
        }
        self.remove(h);
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

/// The hashes that the ancestors of a matching element must have: from each compound that is an ancestor of the element,
/// one tag, id or class. A compound is an ancestor when a space or `>` comes anywhere on its right. In `.list > :not([hidden]) ~
/// :not([hidden])` the middle compound is a sibling, and `.list` is the parent of both, so an ancestor.
///
/// With `exact`, a compound is an ancestor only when a space or `>` joins it to the compound on its right. The other rule also takes `.a` of
/// `.a ~ .b > .c`, a sibling of the parent: the ancestor filter then rejects the rule for every element, which Chrome does not
/// (to verify: `rust-book-chapters` depends on it). The filter keeps that rule (a style would change); the split lists use `exact`.
fn ancestor_hashes(selector: &Selector, exact: bool) -> [u32; 4] {
    let mut out = [0; 4];
    let mut n = 0;
    let mut up = false;
    for pair in selector.compounds.windows(2) {
        let joined = matches!(pair[0].combinator, Combinator::Descendant | Combinator::Child);
        up |= joined;
        if !(if exact { joined } else { up }) {
            continue;
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
    sources_capped(doc, viewport, usize::MAX, usize::MAX, false).into_iter().map(|(s, _)| s).collect()
}

/// Each source comes with its element. Keep the prefix within `max_sources` and `max_bytes`.
/// Drop later sources before copying their text. With `shadows`, the sources in shadow trees come too: the tree of an
/// element is `Document::tree_root`, and the caller adds its sheet with `RuleSet::add_scoped`.
pub fn sources_capped(doc: &Document, viewport: (f32, f32), max_sources: usize, max_bytes: usize, shadows: bool) -> Vec<(Source, NodeId)> {
    // A name that no node in the page has cannot be on any element, so `None` is a good answer for a name that is not in the table.
    let (style, link, media, rel, href, disabled) = (doc.find_atom("style"), doc.find_atom("link"), doc.find_atom("media"), doc.find_atom("rel"), doc.find_atom("href"), doc.find_atom("disabled"));
    let attr = |el, name: Option<Atom>| name.and_then(|n| doc.attr(el, n));
    let mut out = Vec::new();
    let (mut bytes, mut over) = (0usize, false);
    for sheet in doc.sheet_nodes().iter().filter(|s| shadows || s.light) {
        let el = sheet.node;
        let NodeData::Element { name, .. } = *doc.data(el) else { continue };
        let (is_style, is_link) = (Some(name) == style, Some(name) == link);
        if over || !(is_style || is_link) || !attr(el, media).is_none_or(|m| at_rules::media_matches(m, viewport)) {
            continue;
        }
        if is_style {
            let size: usize = doc.children(el).map(|c| if let NodeData::Text(span) = *doc.data(c) { doc.text(span).len() } else { 0 }).sum();
            bytes = bytes.saturating_add(size);
            over = out.len() >= max_sources || bytes > max_bytes;
            if over {
                continue;
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
            if let (true, false, Some(h)) = (has("stylesheet"), has("alternate") || attr(el, disabled).is_some(), attr(el, href)) {
                bytes = bytes.saturating_add(h.len());
                over = out.len() >= max_sources || bytes > max_bytes;
                if !over {
                    out.push((Source::Link(h.to_string()), el));
                }
            }
        }
    }
    out
}

/// Call `f` for each node of the tree, in the order of the page. A loop with a stack. With `shadows`, the shadow tree of a host comes
/// right after the host, before its light children.
#[cfg(test)]
fn walk(doc: &Document, shadows: bool, mut f: impl FnMut(NodeId)) {
    let mut stack = vec![doc.root()];
    while let Some(node) = stack.pop() {
        f(node);
        let mut child = doc.last_child(node);
        while let Some(c) = child {
            stack.push(c);
            child = doc.prev_sibling(c);
        }
        if shadows && let Some(root) = doc.shadow_root(node) {
            stack.push(root);
        }
    }
}

#[cfg(test)]
mod index_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use bare_html::dom::DAMAGE_FRAME;
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
        walk(doc, false, |n| {
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

    /// The picture properties are one shared set in the style, not 12 slots (`ComputedStyle::2`): a longhand, a shorthand, `inherit`,
    /// `initial` and a `calc()` still work, an element with none has set 0, and equal sets have equal styles.
    #[test]
    fn the_picture_properties_are_one_set() {
        use Property::*;
        let html = "<div id=a><p id=b>b</p><p id=c>c</p><p id=d>d</p></div><div id=e>e</div>";
        let sheet = "#a { background-image: url(a.png); mask: url(m.svg) no-repeat center / 20px; background-position-x: calc(100% - 4px) }
            #b { background-image: inherit; mask-image: inherit; mask-size: inherit }
            #c { background-image: initial; background: url(c.png) }
            #d { background-repeat: repeat-x }";
        let (doc, all) = styles(html, &[sheet]);
        let of = |id| all.style(by_id(&doc, id)).unwrap();
        let url = |u: &str| Value::Number(crate::urls::intern(u) as f32);
        let (a, b, c, d, e) = (of("a"), of("b"), of("c"), of("d"), of("e"));
        assert_eq!(e.picture_set(), 0);
        assert_eq!([e.picture(BackgroundImage), e.picture(MaskRepeat), e.picture(BackgroundSizeX)], [Value::Keyword(Keyword::None), Value::Keyword(Keyword::Repeat), Value::Keyword(Keyword::Auto)]);
        assert_eq!([a.picture(BackgroundImage), a.picture(MaskImage), a.picture(MaskRepeat), a.picture(MaskSizeX), a.picture(MaskPositionX)], [url("a.png"), url("m.svg"), Value::Keyword(Keyword::NoRepeat), px(20.0), Value::Length(Length::Percent(50.0))]);
        // A mix of units is kept outside the page: the pixels and the percent come from `picture_calc`.
        let Value::Length(Length::Calc(i)) = a.picture(BackgroundPositionX) else { panic!("calc") };
        assert_eq!(picture_calc(i), (-4.0, 100.0));
        // `inherit` takes the value of the parent; `initial` and the shorthand reset the others of their kind.
        assert_eq!([b.picture(BackgroundImage), b.picture(MaskImage), b.picture(MaskSizeX), b.picture(MaskRepeat)], [url("a.png"), url("m.svg"), px(20.0), Value::Keyword(Keyword::Repeat)]);
        assert_eq!([c.picture(BackgroundImage), c.picture(MaskImage)], [url("c.png"), Value::Keyword(Keyword::None)]);
        assert_eq!([d.picture(BackgroundRepeat), d.picture(BackgroundImage)], [Value::Keyword(Keyword::RepeatX), Value::Keyword(Keyword::None)]);
        // The same values give the same set, so two elements with one rule share one style.
        let (doc, styles) = styles("<p id=x>x</p><p id=y>y</p>", &["p { mask-image: url(s.svg) }"]);
        assert_eq!(styles.style(by_id(&doc, "x")), styles.style(by_id(&doc, "y")));
    }

    /// Specificity, order, `!important`, `var()`, the first layer of a list and the `style=""` attribute pick the picture winner as they do for
    /// the other properties.
    #[test]
    fn the_picture_winner_follows_the_cascade() {
        use Property::*;
        let html = "<p id=a class=k>a</p><p id=b class=k style='background-image: url(i.png)'>b</p><p id=c class=k style='background-image: url(i.png)'>c</p><p id=d>d</p>";
        let sheet = ":root { --u: url(v.png) } p { background-image: url(p.png) } .k { background-image: url(k.png) } #a { background-image: url(z.png) } #a { background-image: url(y.png) }
            #c { background-image: url(imp.png) !important } #d { background: url(first.png) no-repeat, url(second.png) red; mask-image: var(--u) }";
        let (doc, all) = styles(html, &[sheet]);
        let url = |u: &str| Value::Number(crate::urls::intern(u) as f32);
        let get = |id, p| all.style(by_id(&doc, id)).unwrap().picture(p);
        // The id beats the class and the tag; a later rule of the same weight wins; the attribute beats all but `!important`.
        assert_eq!([get("a", BackgroundImage), get("b", BackgroundImage), get("c", BackgroundImage)], [url("y.png"), url("i.png"), url("imp.png")]);
        // The list uses its first layer; the color is the one of the last layer; `var()` is read with the custom properties of the element.
        assert_eq!([get("d", BackgroundImage), get("d", BackgroundRepeat), get("d", MaskImage)], [url("first.png"), Value::Keyword(Keyword::NoRepeat), url("v.png")]);
        assert_eq!(all.style(by_id(&doc, "d")).unwrap().get(BackgroundColor), Value::Color(Color { r: 255, g: 0, b: 0, a: 255 }));
    }

    /// An element with no picture declaration never runs `picture_pass`, and has set 0. A rule or a `style=""` declaration of a picture
    /// property makes it run for the elements that it matches, and for those only.
    #[test]
    fn an_element_with_no_picture_property_pays_no_picture_work() {
        let passes = || PICTURE_PASSES.with(|n| n.get());
        let at = passes();
        let (_, plain) = styles("<div style='color: red'><p>a</p><p>b</p></div>", &["div { background-color: #eee; margin: 1px } p { color: blue; background-color: red }"]);
        assert_eq!(passes() - at, 0);
        assert!(plain.styles.iter().all(|s| s.2 == 0));
        let at = passes();
        let (doc, rule) = styles("<div><p id=m>a</p><p id=n>b</p></div>", &["#m { mask-image: url(x.svg) } p { color: blue }"]);
        assert_eq!(passes() - at, 1);
        assert_ne!(rule.style(by_id(&doc, "m")).unwrap().2, 0);
        assert_eq!(rule.style(by_id(&doc, "n")).unwrap().2, 0);
        // `background: #fff` resets the image, so it is a picture declaration too. It runs once, for the one element that has it.
        let at = passes();
        let (_, inline) = styles("<div style='background: #fff'><p>a</p></div>", &[]);
        assert_eq!(passes() - at, 1);
        assert!(inline.styles.iter().all(|s| s.2 == 0));
    }

    /// The size of `ComputedStyle` is the values of the other properties and two `u32`; an `Entry` has the place of its `Body` and no
    /// declaration ranges, so a rule with a picture count does not make it grow.
    #[test]
    fn the_picture_set_does_not_grow_the_structures() {
        assert_eq!(std::mem::size_of::<ComputedStyle>(), 8 * Property::CORE + 8);
        assert_eq!(std::mem::size_of::<Entry>(), 72);
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

    /// Logical properties map to the physical ones when the style is computed: the inline axis follows `direction`,
    /// the block axis is top and bottom (horizontal writing mode only).
    #[test]
    fn logical_properties_map_to_physical_by_direction() {
        use Property::*;
        let html = "<div id=a><p id=p>x</p></div><div id=r dir=rtl><p id=q>y</p></div>";
        let sheet = "p { margin-inline: 1px 2px; margin-block: 3px 4px; padding-inline-start: 5px; padding-block-end: 6px; inset-inline-end: 7px; \
            border-inline-start-width: 8px; border-inline-start-style: solid; border-block-start: 9px dotted red; inline-size: 10px; min-block-size: 11px; \
            max-inline-size: 12px; border-start-start-radius: 13px; border-end-end-radius: 14px }";
        let g = |id, p| get(html, &[sheet], id, p);
        assert_eq!([g("p", MarginLeft), g("p", MarginRight), g("p", MarginTop), g("p", MarginBottom)], [px(1.0), px(2.0), px(3.0), px(4.0)]);
        assert_eq!([g("p", PaddingLeft), g("p", PaddingRight), g("p", PaddingBottom)], [px(5.0), px(0.0), px(6.0)]);
        assert_eq!([g("p", Right), g("p", Left)], [px(7.0), Value::Keyword(Keyword::Auto)]);
        assert_eq!([g("p", BorderLeftWidth), g("p", BorderTopWidth), g("p", BorderTopStyle)], [px(8.0), px(9.0), Value::Keyword(Keyword::Dotted)]);
        assert_eq!([g("p", Width), g("p", MinHeight), g("p", MaxWidth)], [px(10.0), px(11.0), px(12.0)]);
        assert_eq!([g("p", BorderTopLeftRadius), g("p", BorderBottomRightRadius)], [px(13.0), px(14.0)]);
        // In an rtl element the inline start is the right side. The block axis does not change.
        assert_eq!([g("q", MarginLeft), g("q", MarginRight), g("q", MarginTop), g("q", MarginBottom)], [px(2.0), px(1.0), px(3.0), px(4.0)]);
        assert_eq!([g("q", PaddingLeft), g("q", PaddingRight), g("q", PaddingBottom)], [px(0.0), px(5.0), px(6.0)]);
        assert_eq!([g("q", Left), g("q", Right)], [px(7.0), Value::Keyword(Keyword::Auto)]);
        assert_eq!([g("q", BorderLeftWidth), g("q", BorderRightWidth), g("q", BorderTopWidth)], [px(0.0), px(8.0), px(9.0)]);
        assert_eq!([g("q", Width), g("q", MinHeight)], [px(10.0), px(11.0)]);
        assert_eq!([g("q", BorderTopRightRadius), g("q", BorderBottomLeftRadius), g("q", BorderTopLeftRadius)], [px(13.0), px(14.0), px(0.0)]);
        // The direction comes from the same cascade: a rule, `inherit`, `var()`.
        let (rule, inherit) = ("#a { direction: rtl } p { margin-inline-start: 5px }", "#a { direction: rtl } p { margin-inline-start: inherit }");
        assert_eq!(get(html, &[rule], "p", MarginRight), px(5.0));
        assert_eq!(get(html, &[rule], "p", MarginLeft), px(0.0));
        assert_eq!(get(html, &["#a { margin-right: 6px } #a { direction: rtl } p { margin-inline-start: inherit }"], "p", MarginRight), px(6.0));
        assert_eq!(get(html, &[inherit], "p", MarginLeft), px(0.0));
        let var = "p { --s: 5px; margin-inline: var(--s) 2px; direction: rtl }";
        assert_eq!([get(html, &[var], "p", MarginRight), get(html, &[var], "p", MarginLeft)], [px(5.0), px(2.0)]);
        // The later declaration of the same strength wins, logical or physical. A bad value drops the declaration.
        assert_eq!(get(html, &["p { margin-left: 1px } p { margin-inline-start: 2px }"], "p", MarginLeft), px(2.0));
        assert_eq!(get(html, &["p { margin-inline-start: 2px } p { margin-left: 1px }"], "p", MarginLeft), px(1.0));
        assert_eq!(get(html, &["p { margin-inline-start: 2px; margin-inline-start: red }"], "p", MarginLeft), px(2.0));
        assert_eq!(get(html, &["p { margin-inline: 1px 2px 3px }"], "p", MarginLeft), px(0.0));
        // In one rule the later declaration wins too, in `ltr` and in `rtl`.
        assert_eq!(get(html, &["p { margin-inline-start: 1px; margin-left: 2px }"], "p", MarginLeft), px(2.0));
        assert_eq!(get(html, &["p { margin-left: 2px; margin-inline-start: 1px }"], "p", MarginLeft), px(1.0));
        assert_eq!(get(html, &["p { margin-inline-start: 1px; margin-right: 2px }"], "q", MarginRight), px(2.0));
        assert_eq!(get(html, &["p { margin-right: 2px; margin-inline-start: 1px }"], "q", MarginRight), px(1.0));
        // A logical `!important` beats a physical rule with more specificity.
        assert_eq!(get(html, &["p { margin-inline-start: 2px !important } #p { margin-left: 1px }"], "p", MarginLeft), px(2.0));
    }

    /// `float: inline-start` follows the direction of the containing block. `text-align: start` stays a keyword for layout.
    /// Tailwind v4 sets `--spacing` in `:root, :host`. A list with `:host` is not dropped: the `:root` part works, and the spacing utilities get a length.
    #[test]
    fn a_rule_with_host_in_its_selector_list_still_matches_the_other_selectors() {
        let html = "<div id=d>x</div>";
        let sheet = ":root, :host { --spacing: .25rem } div { padding-left: calc(var(--spacing) * 6); gap: calc(var(--spacing) * 2) }";
        assert_eq!(get(html, &[sheet], "d", Property::PaddingLeft), px(24.0));
        assert_eq!(get(html, &[sheet], "d", Property::ColumnGap), px(8.0));
    }

    /// A `style=""` attribute may hold a logical property in a page whose sheets have none: the `rtl` pass must still run.
    #[test]
    fn a_logical_property_in_a_style_attribute_follows_the_direction() {
        let html = "<p id=a dir=rtl style='margin-inline-start: 7px'>x</p><p id=b style='margin-inline-start: 7px'>y</p>";
        assert_eq!([get(html, &[""], "a", Property::MarginRight), get(html, &[""], "a", Property::MarginLeft)], [px(7.0), px(0.0)]);
        assert_eq!([get(html, &[""], "b", Property::MarginLeft), get(html, &[""], "b", Property::MarginRight)], [px(7.0), px(0.0)]);
    }

    #[test]
    fn float_inline_start_follows_the_direction_of_the_parent() {
        use Keyword::{Left, Right};
        let html = "<div id=a><p id=p>x</p></div><div id=r dir=rtl><p id=q>y</p></div>";
        let sheet = "p { float: inline-start } #z { float: inline-end }";
        assert_eq!(get(html, &[sheet], "p", Property::Float), Value::Keyword(Left));
        assert_eq!(get(html, &[sheet], "q", Property::Float), Value::Keyword(Right));
        assert_eq!(get(html, &["p { float: inline-end } #a { direction: rtl }"], "p", Property::Float), Value::Keyword(Left));
        assert_eq!(get(html, &["p { float: inline-end }"], "p", Property::Float), Value::Keyword(Right));
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
    fn modern_colors_bind_currentcolor_after_the_text_color() {
        let html = "<div style='color: red'><p id=p>x</p></div>";
        let sheets = ["p { color: color-mix(in srgb, currentColor, blue); background: color-mix(in srgb, currentColor, lime) }"];
        assert_eq!(get(html, &sheets, "p", Property::TextColor), rgb(128, 0, 128));
        assert_eq!(get(html, &sheets, "p", Property::BackgroundColor), rgb(64, 128, 64));
    }

    #[test]
    fn modern_colors_keep_sheet_inline_variable_and_hint_arenas_separate() {
        let sheets = [".unused { color: color-mix(in srgb, currentColor, lime) }", "p { color: red; --mix: color-mix(in srgb, currentColor, blue); background-color: var(--mix) }"];
        assert_eq!(get("<p id=p>x</p>", &sheets, "p", Property::BackgroundColor), rgb(128, 0, 128));
        assert_eq!(get("<p id=p style='background: color-mix(in srgb, currentColor, white)'>x</p>", &sheets, "p", Property::BackgroundColor), rgb(255, 128, 128));
        let html = "<svg style='color: red'><rect id=p fill='color-mix(in srgb, currentColor, blue)' style='stroke: color-mix(in srgb, currentColor, white)'></rect></svg>";
        assert_eq!(get(html, &sheets, "p", Property::Fill), rgb(128, 0, 128));
        assert_eq!(get(html, &sheets, "p", Property::Stroke), rgb(255, 128, 128));
    }

    #[test]
    fn hint_arena_prefix_does_not_shift_inline_picture_variables() {
        let html = "<svg><rect id=p fill='var(--paint)' style='--paint: color-mix(in srgb, currentColor, blue); color: red; --image: url(right.svg); background-image: var(--image)'></rect></svg>";
        let (doc, all) = styles(html, &[]);
        let style = all.style(by_id(&doc, "p")).unwrap();
        assert_eq!(style.color(Property::Fill), Color {r:128,g:0,b:128,a:255});
        assert_eq!(style.picture(Property::BackgroundImage), Value::Number(crate::urls::intern("right.svg") as f32));
    }

    #[test]
    fn modern_colors_enable_the_real_theme_feature_query() {
        let css = ":root { --primary: oklch(48.8% .217 264) } p { color: red } @supports (color: color-mix(in lab, red, red)) { p { color: var(--primary); background: color-mix(in oklab, var(--primary) 50%, transparent) } }";
        let (doc, all) = styles("<p id=p>x</p>", &[css]);
        let style = all.style(by_id(&doc, "p")).unwrap();
        let text = style.color(Property::TextColor);
        let bg = style.color(Property::BackgroundColor);
        assert_ne!(text, Color { r:255, g:0, b:0, a:255 });
        assert_eq!((text.r, text.g, text.b), (bg.r, bg.g, bg.b));
        assert_eq!(bg.a, 128);
    }

    #[test]
    fn wrapping_properties_inherit_and_supports_uses_the_real_parser() {
        let html = "<div style='overflow-wrap: anywhere; word-break: break-all'><p id=p>x</p></div>";
        for (name, expected) in [("overflow-wrap", "anywhere"), ("word-break", "break-all")] {
            let p = Property::from_name(name).expect("wrapping property");
            assert!(inherited(p));
            let Value::Keyword(k) = get(html, &[], "p", p) else { panic!("keyword") };
            assert_eq!(k.name(), expected);
        }
        let css = "p { color: red } @supports (overflow-wrap: anywhere) { p { color: blue } }";
        assert_eq!(get(html, &[css], "p", Property::TextColor), rgb(0, 0, 255));
    }

    #[test]
    fn wrapping_slots_have_the_reviewed_style_memory_cost() {
        assert_eq!(std::mem::size_of::<ComputedStyle>(), 872, "baseline 856 plus two eight-byte values");
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
        assert_eq!(sources(&parse("<link rel=stylesheet disabled href=a.css>"), VIEW), []);
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
    fn min_max_clamp_in_the_cascade() {
        let page = "<div id=a><p id=b>x</p></div>";
        let sheet = "html { font-size: 20px } div { width: min(300px, 80vw); height: max(2em, 10px); font-size: clamp(10px, 1em + 1vw, 40px) }\
            p { width: min(300px, 50%); height: clamp(10px, 50%, 2em); padding-left: calc(min(10px, 50%) + 1em); line-height: max(1em, 25%) }";
        let (doc, styles) = styles(page, &[sheet]);
        let get = |id: &str, p: Property| styles.style(by_id(&doc, id)).unwrap().get(p);
        let px = |x| Value::Length(Length::Px(x));
        // No percent: computed now. VIEW is 1000 wide: 80vw is 800. The font size of `div` is 20px + 1vw.
        assert_eq!(get("a", Property::Width), px(300.0));
        assert_eq!(get("a", Property::FontSize), px(30.0));
        assert_eq!(get("a", Property::Height), px(60.0));
        // A percent: kept until layout.
        let Value::Length(Length::Calc(w)) = get("b", Property::Width) else { panic!("{:?}", get("b", Property::Width)) };
        assert_eq!((styles.math_at(w, 392.0), styles.math_at(w, 1000.0)), (196.0, 300.0));
        let Value::Length(Length::Calc(h)) = get("b", Property::Height) else { panic!() };
        // `2em` is 60px here: `p` inherits the font size of `div`.
        assert_eq!((styles.math_at(h, 10.0), styles.math_at(h, 100.0), styles.math_at(h, 1000.0)), (10.0, 50.0, 60.0));
        let Value::Length(Length::Calc(l)) = get("b", Property::PaddingLeft) else { panic!() };
        assert_eq!((styles.math_at(l, 10.0), styles.math_at(l, 100.0)), (35.0, 40.0));
        assert_eq!(styles.math_text(w), "min(300px, 50%)");
        // `line-height` takes a percent of the font size now.
        assert_eq!(get("b", Property::LineHeight), px(30.0));
    }

    #[test]
    fn inherit_keeps_the_computed_calc_of_the_parent() {
        // The computed `calc()` of the parent is a number of `Styles::calcs`, not of the style sheets: the sheet has no `calc()`.
        let page = "<div id=p style='width:calc(10px + 50%); padding-left:calc(2px + 10%)'><div id=c></div></div>";
        let sheet = "#c { width: inherit; padding-left: inherit }";
        let (doc, styles) = styles(page, &[sheet]);
        let get = |id: &str, p: Property| styles.style(by_id(&doc, id)).unwrap().get(p);
        for p in [Property::Width, Property::PaddingLeft] {
            let Value::Length(Length::Calc(i)) = get("c", p) else { panic!("{:?}", get("c", p)) };
            assert_eq!(get("c", p), get("p", p));
            assert_eq!(styles.calc(i), if p == Property::Width { (10.0, 50.0) } else { (2.0, 10.0) });
        }
    }

    #[test]
    fn min_max_clamp_in_the_style_attribute_and_in_var() {
        let page = "<div id=a style='width:min(300px, 50%); height:clamp(1px, 2vw, 9px)'></div><div id=b style='width: var(--w); margin-left: max(5px, 1%)'></div>";
        let sheet = "#b { --w: min(10px, 20%) } #a { padding-left: min(1em, 50%) }";
        let (doc, styles) = styles(page, &[sheet]);
        let get = |id: &str, p: Property| styles.style(by_id(&doc, id)).unwrap().get(p);
        let at = |v: Value, base: f32| match v {
            Value::Length(Length::Calc(i)) => styles.math_at(i, base),
            other => panic!("{other:?}"),
        };
        assert_eq!(at(get("a", Property::Width), 200.0), 100.0);
        assert_eq!(get("a", Property::Height), Value::Length(Length::Px(9.0)));
        assert_eq!(at(get("a", Property::PaddingLeft), 10.0), 5.0);
        assert_eq!(at(get("b", Property::Width), 20.0), 4.0);
        assert_eq!(at(get("b", Property::Width), 500.0), 10.0);
        assert_eq!(at(get("b", Property::MarginLeft), 100.0), 5.0);
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

    /// Pins how custom properties inherit, so that a change of how the sets are kept cannot change what they hold.
    #[test]
    fn custom_property_sets_inherit_and_are_shared() {
        let page = "<div id=a class=t><section id=b class=t><p id=c class=t>x</p><p id=d class='t o'>y</p></section>\
            <p id=e class=k>z</p><p id=f class=k>z</p><p id=g style='--i: 9px; width: var(--i)'>w</p><p id=h class=bad>v</p></div>";
        let sheet = ":root { --a: 1px; --b: 2px; --c: var(--a) var(--b) } .t { --t: 0 } .o { --a: 5px; --c: var(--a) }\
            .k { --a: initial; --b: inherit; --n: var(--nope, 3px); --l1: var(--l2); --l2: var(--l1); --u: var(--l1, 4px) }\
            .bad { --w: red; width: var(--w); height: var(--w, 6px) }";
        let mut doc = parse(page);
        let mut rules = RuleSet::new(VIEW);
        rules.add(&mut doc, UA_CSS, Origin::UserAgent);
        rules.add(&mut doc, sheet, Origin::Author);
        let styles = rules.compute(&doc);
        let style = |id: &str| styles.style(by_id(&doc, id)).unwrap();
        let set = |id: &str| styles.custom_properties(style(id)).iter().map(|(n, v)| format!("{n}={v}")).collect::<Vec<_>>().join(" ");
        // Inherited from the root, `var()` is replaced once, at the root. A rule that sets the same value again gives the same set.
        assert_eq!(set("a"), "--a=1px --b=2px --c=1px 2px --t=0");
        assert_eq!(style("a").1, style("b").1);
        assert_eq!(style("b").1, style("c").1);
        // An override, and a `var()` that reads the new value of the element.
        assert_eq!(set("d"), "--a=5px --b=2px --c=5px --t=0");
        // `initial` removes, `inherit` takes the parent value, a fallback works, a circle goes, and a `var()` of a circle takes its fallback.
        assert_eq!(set("e"), "--b=2px --c=1px 2px --n=3px --t=0 --u=4px");
        assert_eq!(style("e").1, style("f").1);
        // A `style=""` attribute adds a name that no sheet has.
        assert_eq!(set("g"), "--a=1px --b=2px --c=1px 2px --i=9px --t=0");
        assert_eq!(style("g").get(Property::Width), Value::Length(Length::Px(9.0)));
        // A value that is not good for the property is invalid at computed-value time: `unset`, and the fallback is not used.
        assert_eq!(style("h").get(Property::Width), ComputedStyle::initial().get(Property::Width));
        assert_eq!(style("h").get(Property::Height), ComputedStyle::initial().get(Property::Height));
        // The sets are kept once: the elements share a few.
        let mut numbers: Vec<u32> = ["a", "b", "c", "d", "e", "f", "g", "h"].iter().map(|id| style(id).1).collect();
        numbers.sort_unstable();
        numbers.dedup();
        assert_eq!(numbers.len(), 5);
        // A restyle gives the same sets as a full cascade, and the same numbers for the same sets.
        let mut kept = rules.compute_kept(&doc);
        doc.track_damage(DAMAGE_FRAME);
        let (c, e) = (by_id(&doc, "c"), by_id(&doc, "e"));
        doc.set_attr(c, "class", "k");
        doc.set_attr(e, "class", "t o");
        assert!(rules.restyle(&doc, &mut kept, &[(c, false), (e, false)], &mut Vec::new()));
        let full = rules.compute(&doc);
        for id in ["c", "e"] {
            let pairs = |s: &Styles| s.custom_properties(s.style(by_id(&doc, id)).unwrap()).iter().map(|(n, v)| format!("{n}={v}")).collect::<Vec<_>>();
            assert_eq!(pairs(&kept), pairs(&full), "{id}");
        }
        assert_eq!(kept.style(c).unwrap().1, kept.style(by_id(&doc, "f")).unwrap().1);
        assert_eq!(kept.style(e).unwrap().1, kept.style(by_id(&doc, "d")).unwrap().1);
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
            [data-x]{color:#444}[data-x=y] .c{color:#555}.m{display:none}.n{font-size:2em}\
            #i1 .c{color:#666}#i2{color:#777}#i1 ~ .h{margin-top:7px}[data-state=open] + .f{margin-left:9px}\
            .e:not([data-state]) .c{padding-top:5px}.p .q.r{color:#888}.d:not(.b .d){padding-right:6px}li:is(.g ~ li, .k > *){margin-right:8px}\
            .f:not(.e + .f, :hover > .f){border-left-color:#999}span:where([data-x] span, :focus-within ~ span){margin-bottom:3px}";
        const CLASSES: [&str; 14] = ["a", "b", "c", "d", "e", "f", "g", "h", "k", "q", "m", "n", "p", "r"];
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
                let kids: Vec<_> = doc.children(all[k]).chain(doc.shadow_root(all[k]).into_iter().flat_map(|r| doc.children(r))).collect();
                all.extend(kids);
                k += 1;
            }
            all.retain(|&n| matches!(doc.data(n), NodeData::Element { .. }));
            all
        };
        let chain = |doc: &Document, n: Option<NodeId>| std::iter::successors(n, |&p| doc.parent(p).or_else(|| doc.shadow_host(p))).filter(|&p| matches!(doc.data(p), NodeData::Element { .. })).collect::<Vec<_>>();
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
            if round % 2 == 1 {
                // A shadow tree: only the browser's rules match in it, and it inherits from its host.
                let host = connected(&doc)[4];
                let root = doc.attach_shadow(host, true);
                let el = doc.insert_element(root, None, "div", &[("class", "c")]);
                doc.insert_element(el, None, "span", &[("class", "a")]);
            }
            let mut rules = RuleSet::new(VIEW);
            rules.add(&mut doc, UA_CSS, Origin::UserAgent);
            rules.add(&mut doc, CSS, Origin::Author);
            let sc = rules.scopes();
            let mut st = rules.compute_kept(&doc);
            doc.track_damage(DAMAGE_FRAME);
            let mut chains = [Vec::new(), Vec::new()];
            for step in 0..200 {
                let els = connected(&doc);
                let pick = els[2 + rnd(els.len() - 2)];
                let what = rnd(12);
                match what {
                    8 => {
                        let two = format!("{} {}", CLASSES[rnd(CLASSES.len())], CLASSES[rnd(CLASSES.len())]);
                        doc.set_attr(pick, "class", &two);
                    }
                    9 => doc.set_attr(pick, "id", ["i1", "i2", "i3"][rnd(3)]),
                    10 if rnd(3) == 0 => drop(doc.remove_attr(pick, "data-state")),
                    10 => doc.set_attr(pick, "data-state", ["open", "closed"][rnd(2)]),
                    11 => doc.set_attr(pick, "style", ["color:#abc", "font-size:30px", ""][rnd(3)]),
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
                let mut ok = doc.take_damage(DAMAGE_FRAME).is_some_and(|marks| marks.into_iter().all(|m| rules.roots(&doc, m, &mut roots)));
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
                        let hidden_under = st.style(n).is_none() && std::iter::successors(doc.parent(n), |&p| doc.parent(p).or_else(|| doc.shadow_host(p))).any(listed);
                        assert!(listed(n) || hidden_under, "round {round} step {step}: {n:?} changed and is not listed");
                    }
                }
            }
        }
        assert!(restyled > 1000 && full_runs < 200, "restyled {restyled}, full {full_runs}");
    }

    /// A hover that reads `:hover` on an ancestor gives the hovered element and each ancestor as a subtree root, nested one in the other.
    /// A restyle styles each element once, not once for each root above it.
    #[test]
    fn nested_restyle_roots_style_each_element_once() {
        let leaves: String = (0..40).map(|_| "<span class=c>x</span>").collect();
        let html = format!("<div id=d0><div id=d1><div id=d2><div id=d3><div id=d4><div id=d5>{leaves}</div></div></div></div></div></div>");
        let mut doc = parse(&html);
        let mut rules = RuleSet::new(VIEW);
        rules.add(&mut doc, UA_CSS, Origin::UserAgent);
        rules.add(&mut doc, "div:hover .c { color: #123 }", Origin::Author);
        let mut st = rules.compute_kept(&doc);
        let all = rules.styled_elements();
        let full = {
            rules.compute(&doc);
            rules.styled_elements() - all
        };
        let leaf = doc.children(by_id(&doc, "d5")).next().unwrap();
        doc.set_hover(Some(leaf));
        let chain: Vec<NodeId> = std::iter::successors(Some(leaf), |&p| doc.parent(p)).filter(|&p| matches!(doc.data(p), NodeData::Element { .. })).collect();
        let mut roots = Vec::new();
        Scopes::user_roots(rules.scopes().hover, &[], &chain, &doc, &mut roots);
        assert_eq!(roots.len(), chain.len() + 1, "the leaf twice (an end and a change) and each ancestor: {roots:?}");
        assert!(roots.iter().all(|r| r.1));
        let before = rules.styled_elements();
        let mut changed = Vec::new();
        assert!(rules.restyle(&doc, &mut st, &roots, &mut changed));
        let styled = rules.styled_elements() - before;
        assert_eq!(styled, full, "the root `html` covers the others: one pass of the document, not one for each root");
        let again = rules.compute(&doc);
        for n in std::iter::successors(Some(by_id(&doc, "d0")), |_| None).chain(chain.iter().copied()) {
            assert_eq!(st.style(n), again.style(n));
        }
        assert_eq!(changed.len(), 40, "only the leaves get a new color");
    }

    /// Roots that cover each other, with and without the subtree flag: the result is the full cascade. An ancestor root that is not a
    /// subtree root and keeps its style does not cover the roots below it.
    #[test]
    fn covered_roots_do_not_hide_a_root_that_a_walk_did_not_reach() {
        let html = "<div id=a><div id=b><div id=c class=x><p id=p>t</p></div></div></div>";
        let lists = |[a, b, c, p]: [NodeId; 4]| [vec![(a, false), (c, true)], vec![(c, true), (a, false), (b, false), (p, false)], vec![(a, false), (b, false), (c, false), (p, true)], vec![(b, true), (b, false), (c, false)], vec![(c, true), (c, true)]];
        for i in 0..5 {
            let mut doc = parse(html);
            let mut rules = RuleSet::new(VIEW);
            rules.add(&mut doc, UA_CSS, Origin::UserAgent);
            rules.add(&mut doc, ".x { color: red } .y { margin-top: 3px }", Origin::Author);
            let mut st = rules.compute_kept(&doc);
            let ids = ["a", "b", "c", "p"].map(|i| by_id(&doc, i));
            doc.set_attr(ids[2], "class", "y");
            let roots = lists(ids)[i].clone();
            assert!(rules.restyle(&doc, &mut st, &roots, &mut Vec::new()), "{roots:?}");
            let full = rules.compute(&doc);
            for n in ids {
                assert_eq!(st.style(n), full.style(n), "node {n:?} roots {roots:?}");
            }
        }
    }

    /// A random tree, changes of classes, and the restyle of the changed elements as subtree roots plus random roots (nested and
    /// repeated, each with a random subtree flag): equal to a full cascade.
    #[test]
    fn a_restyle_with_nested_and_repeated_roots_equals_a_full_cascade() {
        const CSS: &str = ".a{color:red}.b .c{margin-top:3px}.a > .d{padding-left:2px}.n{font-size:2em}.m{display:none}.a .m{display:block}.e .f .g{color:blue}";
        const CLASSES: [&str; 8] = ["a", "b", "c", "d", "e", "f", "g", "h"];
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut rnd = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let elements = |doc: &Document| {
            let mut all = Vec::new();
            walk(doc, false, |n| all.push(n));
            all.into_iter().filter(|&n| matches!(doc.data(n), NodeData::Element { .. })).collect::<Vec<_>>()
        };
        let mut restyled = 0;
        for _ in 0..30 {
            let mut doc = parse("<!doctype html><body></body>");
            for _ in 0..60 {
                let els = elements(&doc);
                let parent = els[2 + rnd(els.len() - 2)];
                doc.insert_element(parent, None, "div", &[("class", CLASSES[rnd(CLASSES.len())])]);
            }
            let mut rules = RuleSet::new(VIEW);
            rules.add(&mut doc, UA_CSS, Origin::UserAgent);
            rules.add(&mut doc, CSS, Origin::Author);
            let mut st = rules.compute_kept(&doc);
            for step in 0..20 {
                let els = elements(&doc);
                let mut roots = Vec::new();
                for _ in 0..1 + rnd(3) {
                    let pick = els[2 + rnd(els.len() - 2)];
                    doc.set_attr(pick, "class", [CLASSES[rnd(CLASSES.len())], "n", "m", "a n"][rnd(4)]);
                    roots.push((pick, true));
                }
                for _ in 0..rnd(8) {
                    roots.push((els[rnd(els.len())], rnd(2) == 0));
                }
                let mut changed = Vec::new();
                if rules.restyle(&doc, &mut st, &roots, &mut changed) {
                    restyled += 1;
                    let full = rules.compute(&doc);
                    for &n in &els {
                        assert_eq!(st.style(n), full.style(n), "step {step} node {n:?} roots {roots:?}");
                    }
                } else {
                    st = rules.compute_kept(&doc);
                }
            }
        }
        assert!(restyled > 500, "restyled {restyled}");
    }

    /// A `data-state` toggle on one element restyles that element and the elements that its selectors reach, not the whole
    /// document, also with a form pseudo-class in the sheet (Tailwind has `disabled:`; before, any attribute change restyled all).
    #[test]
    fn a_data_state_toggle_restyles_only_what_it_reaches() {
        let items: String = (0..50).map(|i| format!("<li class=item>{i}</li>")).collect();
        let html = format!("<div><button id=t data-state=closed class='a b'>x<span id=icon></span></button><p id=next>n</p></div><ul>{items}</ul>");
        let css = "input:disabled { opacity: .5 } [data-state=open] { color: red } [data-state=open] > span { margin-left: 2px } \
            .only { color: blue } [data-side=top] ~ p { padding-top: 1px } .item { color: green }";
        let mut doc = parse(&html);
        let mut rules = RuleSet::new(VIEW);
        rules.add(&mut doc, UA_CSS, Origin::UserAgent);
        rules.add(&mut doc, css, Origin::Author);
        let mut st = rules.compute_kept(&doc);
        doc.track_damage(DAMAGE_FRAME);
        let (t, icon, next) = (by_id(&doc, "t"), by_id(&doc, "icon"), by_id(&doc, "next"));
        let step = |doc: &mut Document, st: &mut Styles, change: &dyn Fn(&mut Document)| {
            change(doc);
            let mut roots = Vec::new();
            assert!(doc.take_damage(DAMAGE_FRAME).unwrap().into_iter().all(|m| rules.roots(doc, m, &mut roots)));
            let mut changed = Vec::new();
            assert!(rules.restyle(doc, st, &roots, &mut changed));
            let full = rules.compute(doc);
            assert_eq!(st.dump(doc).lines().filter(|l| !l.starts_with('#')).collect::<Vec<_>>(), full.dump(doc).lines().filter(|l| !l.starts_with('#')).collect::<Vec<_>>());
            let mut changed: Vec<NodeId> = changed.into_iter().map(|c| c.0).collect();
            changed.sort_unstable_by_key(|n| n.index());
            (roots, changed)
        };
        // `[data-state=open] > span` reads it on a parent: the button and its subtree. The list is not styled again.
        assert_eq!(step(&mut doc, &mut st, &|d| d.set_attr(t, "data-state", "open")), (vec![(t, true)], vec![t, icon]));
        assert_eq!(step(&mut doc, &mut st, &|d| d.set_attr(t, "data-state", "closed")), (vec![(t, true)], vec![t, icon]));
        // Set to the same value: nothing.
        assert_eq!(step(&mut doc, &mut st, &|d| d.set_attr(t, "data-state", "closed")), (vec![], vec![]));
        // `~` reads it on an earlier sibling: the element and the siblings after it.
        assert_eq!(step(&mut doc, &mut st, &|d| d.set_attr(t, "data-side", "top")), (vec![(t, true), (next, true)], vec![next]));
        // A class that only the last compound reads: the element, and its subtree only because its style changed (`color` is inherited).
        // A class that no selector reads: nothing.
        assert_eq!(step(&mut doc, &mut st, &|d| d.set_attr(t, "class", "a b only")), (vec![(t, false)], vec![t, icon]));
        assert_eq!(step(&mut doc, &mut st, &|d| d.set_attr(t, "class", "b only c")), (vec![], vec![]));
        // An attribute that no selector reads: nothing.
        assert_eq!(step(&mut doc, &mut st, &|d| d.set_attr(t, "aria-expanded", "true")), (vec![], vec![]));
    }

    /// A class in a complex argument of `:not()` or `:is()` is read on an ancestor or an earlier sibling of the subject.
    #[test]
    fn a_class_in_a_complex_argument_restyles_what_it_reaches() {
        let mut doc = parse("<div id=d><p id=p>x</p></div><i id=s></i><b id=b>y</b>");
        let mut rules = RuleSet::new(VIEW);
        rules.add(&mut doc, "p:not(.x p) { color: red } b:is(.y ~ b) { color: blue }", Origin::Author);
        let mut st = rules.compute_kept(&doc);
        doc.track_damage(DAMAGE_FRAME);
        let (d, p, s, b) = (by_id(&doc, "d"), by_id(&doc, "p"), by_id(&doc, "s"), by_id(&doc, "b"));
        for (el, class, want) in [(d, "x", vec![(d, true)]), (s, "y", vec![(s, true), (b, true)])] {
            doc.set_attr(el, "class", class);
            let mut roots = Vec::new();
            assert!(doc.take_damage(DAMAGE_FRAME).unwrap().into_iter().all(|m| rules.roots(&doc, m, &mut roots)));
            assert_eq!(roots, want, "class {class}");
            assert!(rules.restyle(&doc, &mut st, &roots, &mut Vec::new()));
            let full = rules.compute(&doc);
            assert_eq!(st.style(p), full.style(p));
            assert_eq!(st.style(b), full.style(b));
        }
    }

    /// Random changes of the attributes and the state that the form pseudo-classes read, each followed by a restyle of the roots
    /// that `RuleSet::roots` picks: the values must be those of a full cascade.
    #[test]
    fn form_changes_restyle_like_a_full_cascade() {
        const CSS: &str = "input:disabled{color:red}fieldset:disabled .c{color:blue}:placeholder-shown{margin-top:1px}:read-write{padding-top:2px}\
            input:checked + .c{color:green}:indeterminate{margin-left:3px}:default{padding-left:4px}option:checked{color:#123}\
            :required{font-size:20px}.c:enabled ~ .c{margin-right:5px}[data-state=open]{color:#456}";
        const TAGS: [&str; 9] = ["div", "input", "fieldset", "form", "select", "option", "button", "textarea", "span"];
        const ATTRS: [(&str, &[&str]); 13] = [
            ("disabled", &[""]),
            ("value", &["", "v"]),
            ("placeholder", &["p"]),
            ("readonly", &[""]),
            ("required", &[""]),
            ("type", &["radio", "checkbox", "text", "submit", "button"]),
            ("checked", &[""]),
            ("name", &["a", "b"]),
            ("contenteditable", &["true", "false"]),
            ("selected", &[""]),
            ("multiple", &[""]),
            ("size", &["1", "3"]),
            ("data-state", &["open", "closed"]),
        ];
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut rnd = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let elements = |doc: &Document| {
            let (mut all, mut k) = (vec![doc.root()], 0);
            while k < all.len() {
                let kids: Vec<_> = doc.children(all[k]).collect();
                all.extend(kids);
                k += 1;
            }
            all.retain(|&n| matches!(doc.data(n), NodeData::Element { .. }));
            all
        };
        let (mut restyled, mut full_runs) = (0, 0);
        for round in 0..6 {
            let mut doc = parse("<!doctype html><body></body>");
            for _ in 0..40 {
                let els = elements(&doc);
                let parent = els[2 + rnd(els.len() - 2)];
                doc.insert_element(parent, None, TAGS[rnd(TAGS.len())], &[("class", "c")]);
            }
            let mut rules = RuleSet::new(VIEW);
            rules.add(&mut doc, UA_CSS, Origin::UserAgent);
            rules.add(&mut doc, CSS, Origin::Author);
            let mut st = rules.compute_kept(&doc);
            doc.track_damage(DAMAGE_FRAME);
            for step in 0..300 {
                let els = elements(&doc);
                let pick = els[2 + rnd(els.len() - 2)];
                let (name, values) = ATTRS[rnd(ATTRS.len())];
                match rnd(4) {
                    0 => drop(doc.remove_attr(pick, name)),
                    1 => doc.set_ui(pick, rnd(32) as u8),
                    _ => doc.set_attr(pick, name, values[rnd(values.len())]),
                }
                let mut roots = Vec::new();
                let mut changed = Vec::new();
                let ok = doc.take_damage(DAMAGE_FRAME).is_some_and(|marks| marks.into_iter().all(|m| rules.roots(&doc, m, &mut roots)));
                if !(ok && rules.restyle(&doc, &mut st, &roots, &mut changed)) {
                    full_runs += 1;
                    st = rules.compute_kept(&doc);
                    continue;
                }
                restyled += 1;
                let full = rules.compute(&doc);
                for n in elements(&doc) {
                    assert_eq!(st.style(n), full.style(n), "round {round} step {step} {name} at {pick:?}: node {n:?} roots {roots:?}");
                }
            }
        }
        assert!(restyled > 900, "restyled {restyled}, full {full_runs}");
    }

    #[test]
    fn attribute_rules_match_from_their_bucket() {
        let html = "<div id=a data-state=open></div><input id=b type=checkbox><input id=c type=text><span id=d dir=rtl data-state=closed></span><p id=e hidden></p>";
        let css = "[data-state=open] { width: 1px } input[type=checkbox] { width: 2px } [type] { height: 3px } [dir=rtl][data-state] { width: 4px } [hidden] { width: 5px; display: block }";
        let w = |id: &str| get(html, &[css], id, Property::Width);
        let h = |id: &str| get(html, &[css], id, Property::Height);
        assert_eq!((w("a"), w("b"), w("c"), w("d"), w("e")), (px(1.0), px(2.0), Value::Keyword(Keyword::Auto), px(4.0), px(5.0)));
        assert_eq!((h("a"), h("b"), h("c")), (Value::Keyword(Keyword::Auto), px(3.0), px(3.0)));
    }

    #[test]
    fn an_ancestor_after_a_sibling_combinator_still_filters() {
        let mut doc = parse("");
        let list = crate::selector::parse_selector_list(".list > :not([hidden]) ~ :not([hidden])", &mut doc).expect("a selector");
        assert_eq!(ancestor_hashes(&list[0], false), [name_hash(2, "list"), 0, 0, 0]);
        let html = "<div class=list><b id=a></b><b id=b></b><b id=c hidden></b><b id=d></b></div><div><b id=x></b><b id=y></b></div>";
        let css = ".list > :not([hidden]) ~ :not([hidden]) { margin-top: 7px }";
        let m = |id: &str| get(html, &[css], id, Property::MarginTop);
        assert_eq!((m("a"), m("b"), m("d"), m("y")), (px(0.0), px(7.0), px(7.0), px(0.0)));
    }

    #[test]
    fn custom_property_sets_are_shared_and_stay_right() {
        // The same rules under parents with different sets, and a `style=""` that adds one.
        let html = "<div id=p1 style='--c: 1px'><i id=a></i></div><div id=p2 style='--c: 2px'><i id=b></i></div><div id=p3><i id=c style='--d: 6px'></i></div>";
        let css = "* { --a: 3px; --b: var(--c, 5px) } i { width: var(--b); height: var(--d, 9px) }";
        let w = |id: &str| get(html, &[css], id, Property::Width);
        let h = |id: &str| get(html, &[css], id, Property::Height);
        assert_eq!((w("a"), w("b"), w("c")), (px(1.0), px(2.0), px(5.0)));
        assert_eq!((h("a"), h("c")), (px(9.0), px(6.0)));
    }

    /// A block that is read on first use must still tell at once whether the sheet has motion: the answer of a block that was read.
    #[test]
    fn has_motion_is_the_same_for_a_block_that_is_not_read() {
        let motion = |css: &str| {
            let mut doc = bare_html::tree::parse("");
            let mut rules = RuleSet::new(VIEW);
            rules.add(&mut doc, css, Origin::Author);
            rules.has_motion()
        };
        assert!(motion("a{color:red;transition:opacity 1s}"));
        assert!(motion("a{/* x */ animation-name:y}"));
        assert!(motion("@keyframes k{from{color:red}}"));
        assert!(!motion("a{transition:one two three four five six}"), "not valid: it sets nothing");
        assert!(!motion("a{/* transition: opacity 1s */ color:red}"));
        assert!(!motion("a{-webkit-transition:opacity 1s}"));
        assert!(!motion("a{color:red}"));
    }
}
