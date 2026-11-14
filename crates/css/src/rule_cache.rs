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
//! - Memory: at most [`MAX_RULES`] rules of sets that only the cache holds, plus the sets that documents hold. The oldest entries go first.
//!   A new document ([`new_document`]) drops the entries that none of the last [`MAX_IDLE`] documents used. A document that still has
//!   such a set keeps its own `Rc`.

use crate::cascade::RuleSet;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

/// Most rules in the sets that the cache keeps. A set that is larger is not kept.
pub const MAX_RULES: usize = 1 << 16;

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
    pub rules: usize,
}

struct Entry {
    rules: Rc<RuleSet>,
    layers: Box<[Option<Box<str>>]>,
    cost: usize,
    /// The document count at the last look or insert.
    last: u64,
}

#[derive(Default)]
struct Cache {
    map: HashMap<u64, Entry>,
    order: VecDeque<u64>,
    cost: usize,
    docs: u64,
    hits: u64,
    misses: u64,
    /// The key of the set that the current document looked up or kept last.
    recent: Option<u64>,
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
            && let Some(old) = self.map.remove(&k)
        {
            self.cost -= old.cost;
            self.order.retain(|o| *o != k);
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
        let docs = c.docs;
        let found = c.map.get_mut(&k).filter(|e| e.layers.len() == parts.len() && e.layers.iter().zip(parts).all(|(a, b)| a.as_deref() == b.0) && e.rules.built_from(viewport, parts.iter().map(|p| p.1)));
        match found {
            Some(e) => {
                e.last = docs;
                let rules = e.rules.clone();
                c.hits += 1;
                c.use_key(k);
                Some(rules)
            }
            None => {
                c.misses += 1;
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
    let cost = rules.len() + 1;
    if cost > MAX_RULES {
        return;
    }
    let k = k.0;
    let layers = parts.iter().map(|p| p.0.map(Box::from)).collect();
    CACHE.with_borrow_mut(|c| {
        if let Some(old) = c.map.remove(&k) {
            c.cost -= old.cost;
        }
        while c.cost + cost > MAX_RULES {
            let Some(oldest) = c.order.pop_front() else { break };
            if let Some(old) = c.map.remove(&oldest) {
                c.cost -= old.cost;
            }
        }
        c.map.insert(k, Entry { rules: rules.clone(), layers, cost, last: c.docs });
        c.order.push_back(k);
        c.cost += cost;
        c.use_key(k);
    });
}

/// Take `rules` back as an owned set so that the caller can add sheets to it. It works when the cache and the caller are the only
/// holders: the entry goes (the grown set has other sheets). `None` when a document also holds it, or when it is unique and was never kept.
pub fn unshare(rules: Rc<RuleSet>) -> Option<RuleSet> {
    if Rc::strong_count(&rules) == 2 {
        CACHE.with_borrow_mut(|c| {
            if let Some(&k) = c.map.iter().find(|(_, e)| Rc::ptr_eq(&e.rules, &rules)).map(|(k, _)| k) {
                let old = c.map.remove(&k).expect("found");
                c.cost -= old.cost;
                c.order.retain(|o| *o != k);
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
        let docs = c.docs;
        let before = c.map.len();
        let mut freed = 0;
        c.map.retain(|_, e| {
            let keep = docs - e.last <= MAX_IDLE;
            if !keep {
                freed += e.cost;
            }
            keep
        });
        if c.map.len() != before {
            c.cost -= freed;
            c.order.retain(|k| c.map.contains_key(k));
        }
    });
}

/// The counts of this thread.
pub fn counts() -> Counts {
    CACHE.with_borrow(|c| Counts { hits: c.hits, misses: c.misses, entries: c.map.len(), rules: c.cost })
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
            c.map.insert(key(VIEW, &other).0, Entry { rules: kept.clone(), layers, cost: 1, last: 0 });
            c.cost += 1;
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
        // Sets over the budget are not kept, and old ones go to make room.
        let big: String = (0..MAX_RULES).map(|i| format!(".c{i} {{ color: red }}\n")).collect();
        let parts: [Part<'_>; 1] = [(None, &big)];
        let one = built(VIEW, &parts);
        assert!(one.len() >= MAX_RULES);
        insert(VIEW, &parts, &one);
        assert_eq!(counts().entries, 0);
        let half: String = (0..MAX_RULES / 2 + 10).map(|i| format!(".c{i} {{ color: red }}\n")).collect();
        let (p1, p2): ([Part<'_>; 1], [Part<'_>; 1]) = ([(None, &half)], [(Some("l"), &half)]);
        let (s1, s2) = (built(VIEW, &p1), built(VIEW, &p2));
        insert(VIEW, &p1, &s1);
        insert(VIEW, &p2, &s2);
        let c = counts();
        assert_eq!(c.entries, 1, "the second set made room by dropping the first");
        assert!(c.rules <= MAX_RULES);
        assert!(lookup(VIEW, &p1).is_none() && lookup(VIEW, &p2).is_some());
    }
}
