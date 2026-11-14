//! The rule sets that this thread built, shared by its documents.
//!
//! A document builds its `RuleSet` from the sheets of its page: the text of each `<style>` and `<link>`, in order, for the size of
//! the window. Two documents with the same sheets and the same window get the same rules. A site that a warm process visits
//! again and again (a test suite) has the same sheets in each document, so the build is the same work each time. This cache keeps
//! the built set as an `Rc<RuleSet>`, and the next document takes the `Rc` instead of building.
//!
//! - Key: a hash of the window size and of the layer and the text of each sheet. A hit also compares the window size, the layers
//!   and the text of every sheet with the ones that built the set (`RuleSet::built_from`), so a hash collision is a miss.
//! - What is shared: everything in the `RuleSet`. It does not change after the build. What it keeps in `Cell`s and `RefCell`s is made
//!   from the rules alone, when a style first needs it, and it only grows (read blocks, the lists that `Index` splits, the
//!   invalidation sets, the counters that a caller reads as a difference around one call). One thread runs one cascade at a time, so
//!   no borrow overlaps. A set holds `Atom`s of the thread, which mean the same in each document of the thread (`bare_html::dom`).
//!   `Rc`, not `Arc`: a set never leaves its thread.
//! - What is not shared: a set with a shadow tree sheet (its rules name the nodes of one document), a set with retained CSSOM inputs
//!   (`RuleSet::add_inputs`, `set_inline_inputs`), and a set that a document grows by `add`. The caller decides this.
//! - Memory: a set that only the cache holds is idle. The idle sets weigh at most [`MAX_IDLE_BYTES`] (`RuleSet::heap_bytes`). When they weigh
//!   more, the set with the lowest priority goes first: the priority is the number of uses plus the priority of the last set that went
//!   (GDSF where the cost of a build is in proportion to the size of the set, so size does not decide, uses and age do). A set that a
//!   document holds is not idle: it is not counted and never goes while the document holds it.
//! - Three more rules drop idle sets early, because an idle set is memory that a page that does not use it pays for:
//!   a page that goes on to another list drops the set that it left (the step: `settle`); a new document ([`new_document`]) drops the sets that
//!   none of the last [`MAX_IDLE`] documents used; and a document that asks for a list that the document before it did not ask for is on
//!   another site, so the idle sets of the earlier documents are for nobody (`Cache::diverged`).

use crate::cascade::RuleSet;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Most bytes in the idle sets. The largest set that a journey of the corpus found in the cache is 2.26 MB; the suite's is 1.37 MB.
pub const MAX_IDLE_BYTES: usize = 2_500_000;

/// The keys that a document may add to the keys that it used (`Cache::diverged` compares with them).
const MAX_KEYS: usize = 64;

/// An entry that the last this many documents did not use goes at the next [`new_document`].
pub const MAX_IDLE: u64 = 2;

/// The layer and the text of one sheet.
pub type Part<'a> = (Option<&'a str>, &'a str);

/// Counts of this thread, for tests and reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Looks that found a set.
    pub hits: u64,
    /// Looks that found none: the caller built a set.
    pub misses: u64,
    pub entries: usize,
}

struct Entry {
    rules: Rc<RuleSet>,
    layers: Box<[Option<Box<str>>]>,
    /// The document count at the last look or insert.
    last: u64,
    /// The inserts and the looks that found the set.
    uses: u64,
    /// The clock of the cache at the last use. The priority of the set is this and `uses`.
    base: u64,
    /// What `RuleSet::heap_bytes` gave when the set was first idle. The walk costs about as much as 2% of a build, so it runs once for each
    /// set. The set grows after that when a style reads blocks, by 3% on the corpus (2.19 to 2.26 MB).
    bytes: Option<usize>,
}

#[derive(Default)]
struct Cache {
    map: HashMap<u64, Entry>,
    docs: u64,
    hits: u64,
    misses: u64,
    /// The key of the set that the current document looked up or kept last.
    recent: Option<u64>,
    /// The priority of the last set that `enforce` dropped.
    clock: u64,
    /// The keys that the last document with a look used, and the keys that this document used so far.
    prev_keys: Vec<u64>,
    cur_keys: Vec<u64>,
    /// The key of the set before it, that the document left: it goes at the next look, when the document no longer holds it.
    step: Option<u64>,
}

impl Cache {
    /// The current document uses the set `k`. The set that it used before is a step of the page (a page that changes its sheets while it
    /// loads gets one set for each list): no page keeps it, and it would be in memory with the next one.
    fn use_key(&mut self, k: u64) {
        self.drop_step();
        if self.recent != Some(k) {
            self.step = self.recent;
            self.recent = Some(k);
        }
    }

    /// Drop the step of the document if nobody holds it.
    fn drop_step(&mut self) {
        if let Some(k) = self.step.take()
            && self.map.get(&k).is_some_and(|e| Rc::strong_count(&e.rules) == 1)
        {
            self.map.remove(&k);
        }
    }

    /// The step goes now if nobody holds it, else it stays the step.
    fn drop_idle_step(&mut self) {
        if self.step.is_some_and(|k| self.map.get(&k).is_some_and(|e| Rc::strong_count(&e.rules) == 1)) {
            self.drop_step();
        }
    }

    /// The document asked for a list that the document before it did not ask for. It is not on the same pages, so the idle sets that it
    /// did not use are for nobody. A page that is on the same site makes the same lists in the same order, and finds the last of them.
    fn diverged(&mut self, k: u64) {
        if !self.prev_keys.contains(&k) {
            let docs = self.docs;
            self.map.retain(|_, e| Rc::strong_count(&e.rules) > 1 || e.last == docs);
        }
    }

    /// Drop idle sets, the lowest priority first, until the idle sets weigh at most [`MAX_IDLE_BYTES`].
    fn enforce(&mut self) {
        self.drop_idle_step();
        let mut total = 0;
        let mut idle = Vec::new();
        for (k, e) in self.map.iter_mut() {
            if Rc::strong_count(&e.rules) == 1 {
                let bytes = *e.bytes.get_or_insert_with(|| e.rules.heap_bytes());
                total += bytes;
                idle.push((e.base + e.uses, e.last, *k, bytes));
            }
        }
        // The lowest priority first, the oldest first among equals.
        idle.sort_unstable();
        for (priority, _, k, bytes) in idle {
            if total <= MAX_IDLE_BYTES {
                break;
            }
            self.map.remove(&k);
            self.clock = priority;
            total -= bytes;
        }
    }
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::default();
}

/// Multiply hash of 8 bytes at a time.
fn fold(mut h: u64, bytes: &[u8]) -> u64 {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let mut chunks = bytes.chunks_exact(8);
    for w in &mut chunks {
        h = (h.rotate_left(5) ^ u64::from_le_bytes(w.try_into().unwrap())).wrapping_mul(K);
    }
    let mut last = [0u8; 8];
    last[..chunks.remainder().len()].copy_from_slice(chunks.remainder());
    (h.rotate_left(5) ^ u64::from_le_bytes(last)).wrapping_mul(K)
}

/// What a look gives for its sheets: the next `insert` of the same sheets takes it, so that the text is hashed once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key(u64);

pub fn key(viewport: (f32, f32), parts: &[Part<'_>]) -> Key {
    let mut h = fold(parts.len() as u64, &viewport.0.to_le_bytes());
    h = fold(h, &viewport.1.to_le_bytes());
    for (layer, text) in parts {
        // The length of the text and a mark for "no layer" keep `("a", "b")` and `(None, "ab")` apart.
        h = fold(h ^ ((text.len() as u64) << 1) ^ u64::from(layer.is_some()), layer.unwrap_or("").as_bytes());
        h = fold(h, text.as_bytes());
    }
    Key(h)
}

/// The set that was built for these sheets and this window size, if the cache has one.
pub fn lookup(viewport: (f32, f32), parts: &[Part<'_>]) -> Option<Rc<RuleSet>> {
    lookup_key(key(viewport, parts), viewport, parts)
}

/// `lookup` for a `key` that the caller made.
pub fn lookup_key(k: Key, viewport: (f32, f32), parts: &[Part<'_>]) -> Option<Rc<RuleSet>> {
    let k = k.0;
    CACHE.with_borrow_mut(|c| {
        let (docs, clock) = (c.docs, c.clock);
        if c.cur_keys.len() < MAX_KEYS {
            c.cur_keys.push(k);
        }
        let found = c.map.get_mut(&k).filter(|e| e.layers.len() == parts.len() && e.layers.iter().zip(parts).all(|(a, b)| a.as_deref() == b.0) && e.rules.built_from(viewport, parts.iter().map(|p| p.1)));
        match found {
            Some(e) => {
                (e.last, e.uses, e.base) = (docs, e.uses + 1, clock);
                let rules = e.rules.clone();
                c.hits += 1;
                c.use_key(k);
                c.enforce();
                Some(rules)
            }
            None => {
                c.misses += 1;
                c.diverged(k);
                c.enforce();
                None
            }
        }
    })
}

/// Keep `rules`, built for these sheets and this window size, for the next documents.
pub fn insert(viewport: (f32, f32), parts: &[Part<'_>], rules: &Rc<RuleSet>) {
    insert_key(key(viewport, parts), parts, rules);
}

/// `insert` for a `key` that the caller made.
pub fn insert_key(k: Key, parts: &[Part<'_>], rules: &Rc<RuleSet>) {
    let k = k.0;
    let layers = parts.iter().map(|p| p.0.map(Box::from)).collect();
    CACHE.with_borrow_mut(|c| {
        let (last, base) = (c.docs, c.clock);
        c.map.insert(k, Entry { rules: rules.clone(), layers, last, uses: 1, base, bytes: None });
        c.use_key(k);
        c.enforce();
    });
}

/// The document made `rules`, the set of its last look, its set: the set that it had before is a step of the page. Drop it now if nobody holds it.
pub fn settle() {
    CACHE.with_borrow_mut(|c| c.drop_idle_step());
}

/// Take `rules` back as an owned set so that the caller can add sheets to it. It works when the cache and the caller are the only
/// holders: the entry goes (the grown set has other sheets). `None` when a document also holds it, or when it is unique and was never kept.
pub fn unshare(rules: Rc<RuleSet>) -> Option<RuleSet> {
    if Rc::strong_count(&rules) == 2 {
        CACHE.with_borrow_mut(|c| {
            if let Some(&k) = c.map.iter().find(|(_, e)| Rc::ptr_eq(&e.rules, &rules)).map(|(k, _)| k) {
                c.map.remove(&k);
            }
        });
    }
    Rc::try_unwrap(rules).ok()
}

/// A new document starts on this thread: drop the entries that the last [`MAX_IDLE`] documents did not use.
pub fn new_document() {
    CACHE.with_borrow_mut(|c| {
        c.drop_step();
        c.recent = None;
        c.docs += 1;
        if !c.cur_keys.is_empty() {
            c.prev_keys = std::mem::take(&mut c.cur_keys);
        }
        let docs = c.docs;
        c.map.retain(|_, e| docs - e.last <= MAX_IDLE);
        c.enforce();
    });
}

/// The counts of this thread.
pub fn counts() -> Counts {
    CACHE.with_borrow(|c| Counts { hits: c.hits, misses: c.misses, entries: c.map.len() })
}

/// Drop every entry. For tests that need a cold cache.
pub fn clear() {
    CACHE.with_borrow_mut(|c| *c = Cache { docs: c.docs, hits: c.hits, misses: c.misses, ..Cache::default() });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cascade::{Origin, UA_CSS};
    use bare_html::dom::Document;

    const VIEW: (f32, f32) = (800.0, 600.0);

    /// A set for these sheets, built as the browser builds it.
    fn built(viewport: (f32, f32), parts: &[Part<'_>]) -> Rc<RuleSet> {
        let mut doc = Document::new();
        let mut rules = RuleSet::new(viewport);
        rules.add(&mut doc, UA_CSS, Origin::UserAgent);
        for (layer, text) in parts {
            rules.add_in_layer(&mut doc, text, Origin::Author, *layer);
        }
        Rc::new(rules)
    }

    fn keep(parts: &[Part<'_>]) -> Rc<RuleSet> {
        let rules = built(VIEW, parts);
        insert(VIEW, parts, &rules);
        rules
    }

    #[test]
    fn the_same_sheets_and_window_give_the_same_set() {
        clear();
        let parts: [Part<'_>; 2] = [(None, "a { color: red }"), (Some("x"), "b { color: blue }")];
        assert!(lookup(VIEW, &parts).is_none());
        let kept = keep(&parts);
        let found = lookup(VIEW, &parts).expect("a hit");
        assert!(Rc::ptr_eq(&kept, &found));
        let c = counts();
        assert_eq!((c.hits, c.misses, c.entries), (1, 1, 1));
    }

    #[test]
    fn a_document_keeps_only_its_last_set_in_the_cache() {
        clear();
        let (a, b, c): ([Part<'_>; 1], [Part<'_>; 1], [Part<'_>; 1]) = ([(None, "a { color: red }")], [(None, "b { color: blue }")], [(None, "c { color: green }")]);
        new_document();
        // A page that changes its sheets twice while it loads: three sets, one after the other. Nothing holds the first two.
        drop(keep(&a));
        drop(keep(&b));
        drop(keep(&c));
        new_document();
        assert_eq!(counts().entries, 1, "the steps of the page are gone, its last set stays");
        assert!(lookup(VIEW, &c).is_some() && lookup(VIEW, &a).is_none() && lookup(VIEW, &b).is_none());
        // A set that the page still holds stays when the page goes on to another list.
        let hold = keep(&a);
        drop(keep(&b));
        assert_eq!(counts().entries, 2, "`c` was the step before `a`; `a` is held and `b` is last");
        new_document();
        assert!(lookup(VIEW, &a).is_some() && Rc::strong_count(&hold) == 2);
    }

    #[test]
    fn another_text_layer_order_count_or_window_is_another_set() {
        clear();
        let parts: [Part<'_>; 2] = [(None, "a { color: red }"), (None, "b { color: blue }")];
        let _held = keep(&parts);
        assert!(lookup((801.0, 600.0), &parts).is_none(), "the window width");
        assert!(lookup((800.0, 601.0), &parts).is_none(), "the window height");
        assert!(lookup(VIEW, &[parts[1], parts[0]]).is_none(), "the order");
        assert!(lookup(VIEW, &parts[..1]).is_none(), "one sheet less");
        assert!(lookup(VIEW, &[parts[0], parts[1], parts[1]]).is_none(), "one sheet more");
        assert!(lookup(VIEW, &[(None, "a { color: red }"), (None, "b { color: green }")]).is_none(), "the text");
        assert!(lookup(VIEW, &[(None, "a { color: red }"), (Some("l"), "b { color: blue }")]).is_none(), "a layer");
        // The same text split in another place is another list.
        assert!(lookup(VIEW, &[(None, "a { color: red }b"), (None, " { color: blue }")]).is_none(), "the join");
        assert!(lookup(VIEW, &parts).is_some());
    }

    #[test]
    fn a_colliding_key_is_checked_against_the_text() {
        clear();
        let parts: [Part<'_>; 1] = [(None, "a { color: red }")];
        let other: [Part<'_>; 1] = [(None, "a { color: blue }")];
        let kept = built(VIEW, &parts);
        // Make the entry sit under the key of another list, as a hash collision would.
        let layers: Box<[Option<Box<str>>]> = Box::new([None]);
        CACHE.with_borrow_mut(|c| {
            c.map.insert(key(VIEW, &other).0, Entry { rules: kept.clone(), layers, last: 0, uses: 1, base: 0, bytes: None });
        });
        assert!(lookup(VIEW, &other).is_none());
        assert!(!kept.built_from(VIEW, other.iter().map(|p| p.1)));
        assert!(kept.built_from(VIEW, parts.iter().map(|p| p.1)));
    }

    #[test]
    fn a_set_that_only_the_cache_and_one_document_hold_can_grow() {
        clear();
        let parts: [Part<'_>; 1] = [(None, "a { color: red }")];
        let mine = keep(&parts);
        assert!(unshare(mine).is_some());
        assert_eq!(counts().entries, 0, "the grown set is another list: the old entry goes");
        assert!(lookup(VIEW, &parts).is_none());
        // Another document holds it too: it cannot grow, and the entry stays.
        let one = keep(&parts);
        let other = lookup(VIEW, &parts).unwrap();
        assert!(unshare(one).is_none());
        assert_eq!(counts().entries, 1);
        assert!(Rc::strong_count(&other) == 2);
        // A set that the cache never held and nobody shares is unique.
        assert!(unshare(built(VIEW, &parts)).is_some());
    }

    #[test]
    fn idle_entries_go_and_the_cache_stays_within_its_budget() {
        clear();
        let a = keep(&[(None, "a { color: red }")]);
        new_document();
        let _b = keep(&[(None, "b { color: red }")]);
        for _ in 0..MAX_IDLE {
            new_document();
        }
        assert_eq!(counts().entries, 1, "a was not used for {MAX_IDLE} documents; b was used one document later");
        assert!(lookup(VIEW, &[(None, "a { color: red }")]).is_none());
        // A document that has the set keeps it alive.
        assert_eq!(Rc::strong_count(&a), 1);
        clear();
    }

    /// A sheet of `n` rules named `tag`: a set of about 400 bytes for each rule.
    fn sheet(tag: &str, n: usize) -> String {
        (0..n).map(|i| format!(".{tag}{i} {{ color: red }}\n")).collect()
    }

    #[test]
    fn idle_sets_weigh_at_most_the_budget_and_the_one_with_fewer_uses_goes() {
        clear();
        let (big_a, big_b) = (sheet("a", 3500), sheet("b", 3500));
        let (pa, pb): ([Part<'_>; 1], [Part<'_>; 1]) = ([(None, &big_a)], [(None, &big_b)]);
        let (a, b) = (built(VIEW, &pa), built(VIEW, &pb));
        let weight = a.heap_bytes();
        assert!(weight < MAX_IDLE_BYTES && weight * 2 > MAX_IDLE_BYTES, "one set fits, two do not: {weight}");
        insert(VIEW, &pa, &a);
        drop(lookup(VIEW, &pa)); // a has two uses
        drop(a);
        new_document();
        // b is held by this test: nothing is idle but a, and a fits.
        insert(VIEW, &pb, &b);
        assert_eq!(counts().entries, 2);
        // b is idle too: the two weigh more than the budget, and b (one use) goes.
        drop(b);
        new_document();
        assert_eq!(counts().entries, 1);
        assert!(lookup(VIEW, &pa).is_some() && lookup(VIEW, &pb).is_none());
    }

    #[test]
    fn a_set_over_the_budget_goes_when_nobody_holds_it() {
        clear();
        let huge = sheet("h", 8000);
        let parts: [Part<'_>; 1] = [(None, &huge)];
        let one = built(VIEW, &parts);
        assert!(one.heap_bytes() > MAX_IDLE_BYTES);
        insert(VIEW, &parts, &one);
        assert_eq!(counts().entries, 1, "a document holds it");
        drop(one);
        new_document();
        assert_eq!(counts().entries, 0);
    }

    #[test]
    fn the_step_of_a_page_goes_when_the_page_has_its_next_set() {
        clear();
        let (a, b) = ([(None, "a { color: red }")], [(None, "b { color: red }")]);
        let first = keep(&a);
        let second = keep(&b);
        assert_eq!(counts().entries, 2, "the page still holds the first set");
        drop(first);
        settle();
        assert_eq!(counts().entries, 1);
        assert!(lookup(VIEW, &a).is_none() && lookup(VIEW, &b).is_some());
        drop(second);
    }

    #[test]
    fn a_page_on_other_lists_than_the_page_before_drops_its_idle_sets() {
        clear();
        let (a, b, c) = ([(None, "a { color: red }")], [(None, "b { color: red }")], [(None, "c { color: red }")]);
        new_document();
        assert!(lookup(VIEW, &a).is_none());
        drop(keep(&a));
        new_document();
        // The next page asks for `a`, the list of the page before: it is on the same site, and the idle sets stay.
        let held_a = lookup(VIEW, &a).unwrap();
        assert!(lookup(VIEW, &b).is_none());
        let held_b = keep(&b);
        assert_eq!(counts().entries, 2);
        drop((held_a, held_b));
        new_document();
        // This page asks for `c`, a list that the page before did not use: its idle sets are for nobody.
        assert!(lookup(VIEW, &c).is_none());
        assert_eq!(counts().entries, 0);
    }
}
