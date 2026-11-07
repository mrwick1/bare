//! A side table of the objects that have some data (the function table of the evaluator).
//!
//! JS has `Map` and `WeakMap`: a hash of the key, then a probe. This table is indexed by the slot of the handle instead. A read is two
//! loads and one compare, with no hash. `at[slot]` holds the generation of the handle and the place of its value in `dense` (plus one,
//! 0 is "no entry"). The generation check means an entry of a dead object never answers for the new object of the same slot.
//! `dense` holds the values with no gaps, so the memory follows the entries and a pass over all of them is a plain slice walk.
//! This is a sparse set. `at` has 8 bytes for each slot up to the highest slot with an entry, which is what the heap spends for the
//! metadata of one slot too.

use crate::value::ObjId;

pub(crate) struct ObjTab<V> {
    /// By slot: (generation, position in `dense` + 1). Position 0: the slot has no entry.
    at: Vec<(u32, u32)>,
    dense: Vec<(ObjId, V)>,
}

impl<V> Default for ObjTab<V> {
    fn default() -> Self {
        ObjTab { at: Vec::new(), dense: Vec::new() }
    }
}

impl<V> ObjTab<V> {
    pub fn len(&self) -> usize {
        self.dense.len()
    }
    /// The number of entries the values vector can hold without growing.
    pub fn capacity(&self) -> usize {
        self.dense.capacity()
    }
    /// Bytes that the table holds.
    pub fn bytes(&self) -> usize {
        self.dense.capacity() * size_of::<(ObjId, V)>() + self.at.capacity() * size_of::<(u32, u32)>()
    }
    /// The place of the value of `o` in `dense`.
    #[inline(always)]
    fn pos(&self, o: &ObjId) -> Option<usize> {
        match self.at.get(o.index as usize) {
            Some(&(gn, p)) if p != 0 && gn == o.gn => Some(p as usize - 1),
            _ => None,
        }
    }
    #[inline(always)]
    pub fn contains_key(&self, o: &ObjId) -> bool {
        self.pos(o).is_some()
    }
    #[inline(always)]
    pub fn get(&self, o: &ObjId) -> Option<&V> {
        Some(&self.dense[self.pos(o)?].1)
    }
    pub fn get_mut(&mut self, o: &ObjId) -> Option<&mut V> {
        let p = self.pos(o)?;
        Some(&mut self.dense[p].1)
    }
    /// The value of `o` is `v` from now on. The old value comes back if `o` had one.
    pub fn insert(&mut self, o: ObjId, v: V) -> Option<V> {
        let s = o.index as usize;
        if self.at.len() <= s {
            self.at.resize(s + 1, (0, 0));
        }
        let (gn, p) = self.at[s];
        if p != 0 {
            // The slot has an entry. It is the same object, or a dead object that nothing pruned yet: replace it either way.
            let old = std::mem::replace(&mut self.dense[p as usize - 1], (o, v));
            self.at[s].0 = o.gn;
            return (gn == o.gn).then_some(old.1);
        }
        self.dense.push((o, v));
        self.at[s] = (o.gn, self.dense.len() as u32);
        None
    }
    /// The entries, in no fixed order.
    pub fn iter(&self) -> impl Iterator<Item = (&ObjId, &V)> {
        self.dense.iter().map(|(o, v)| (o, v))
    }
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.dense.iter().map(|(_, v)| v)
    }
    /// Keep the entries for which `keep` is true. The last entry moves into the place of a removed one, so the order changes.
    pub fn retain(&mut self, mut keep: impl FnMut(&ObjId, &mut V) -> bool) {
        let mut i = 0;
        while i < self.dense.len() {
            let (o, v) = &mut self.dense[i];
            if keep(o, v) {
                i += 1;
                continue;
            }
            self.at[o.index as usize].1 = 0;
            self.dense.swap_remove(i);
            if let Some((m, _)) = self.dense.get(i) {
                self.at[m.index as usize].1 = i as u32 + 1;
            }
        }
        while self.at.last().is_some_and(|e| e.1 == 0) {
            self.at.pop();
        }
        self.at.shrink_to(self.at.len() + self.at.len() / 4);
    }
    /// Give back capacity of `dense` down to `keep` entries (`retain` trims `at`).
    pub fn shrink_to(&mut self, keep: usize) {
        self.dense.shrink_to(keep);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(index: u32, gn: u32) -> ObjId {
        ObjId { index, gn }
    }

    #[test]
    fn insert_get_replace() {
        let mut t = ObjTab::default();
        assert!(t.get(&id(3, 0)).is_none() && !t.contains_key(&id(3, 0)));
        assert_eq!(t.insert(id(3, 0), "a"), None);
        assert_eq!(t.insert(id(9, 2), "b"), None);
        assert_eq!((t.get(&id(3, 0)), t.get(&id(9, 2)), t.len()), (Some(&"a"), Some(&"b"), 2));
        assert_eq!(t.insert(id(3, 0), "c"), Some("a"));
        assert_eq!((t.get(&id(3, 0)), t.len()), (Some(&"c"), 2));
        *t.get_mut(&id(9, 2)).unwrap() = "d";
        assert_eq!(t.get(&id(9, 2)), Some(&"d"));
        assert!(t.get(&id(4, 0)).is_none() && t.get(&id(100, 0)).is_none());
    }

    #[test]
    fn a_reused_slot_never_answers_for_the_old_handle() {
        let mut t = ObjTab::default();
        t.insert(id(5, 0), 1);
        assert!(t.get(&id(5, 1)).is_none() && !t.contains_key(&id(5, 1)));
        // a new object in the same slot, the dead one was not pruned: the entry is replaced and the old value is not returned
        assert_eq!(t.insert(id(5, 1), 2), None);
        assert_eq!((t.len(), t.get(&id(5, 1)), t.get(&id(5, 0))), (1, Some(&2), None));
    }

    #[test]
    fn retain_keeps_the_index_in_step() {
        let mut t = ObjTab::default();
        for i in 0..100u32 {
            t.insert(id(i * 3, i % 4), i);
        }
        t.retain(|_, v| *v % 3 != 0);
        assert_eq!(t.len(), 66);
        for i in 0..100u32 {
            assert_eq!(t.get(&id(i * 3, i % 4)).copied(), (i % 3 != 0).then_some(i), "entry {i}");
        }
        let mut seen: Vec<u32> = t.values().copied().collect();
        seen.sort();
        assert_eq!(seen, (0..100).filter(|i| i % 3 != 0).collect::<Vec<_>>());
        assert_eq!(t.iter().count(), 66);
        // a removed entry can be made again, and the table drops the empty end of `at`
        t.retain(|_, _| false);
        assert!(t.len() == 0 && t.at.is_empty() && t.get(&id(3, 1)).is_none());
        t.insert(id(3, 1), 7);
        assert_eq!(t.get(&id(3, 1)), Some(&7));
    }

    #[test]
    fn matches_a_hash_map_on_random_operations() {
        let mut t = ObjTab::default();
        let mut m = std::collections::HashMap::new();
        let mut x = 0x9e3779b97f4a7c15u64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for step in 0..20_000u32 {
            let k = id((next() % 64) as u32, (next() % 3) as u32);
            match next() % 4 {
                0 | 1 => {
                    // the map holds one handle for each slot, as the table does: a new generation replaces the old
                    m.retain(|o: &ObjId, _| o.index != k.index);
                    m.insert(k, step);
                    t.insert(k, step);
                }
                2 => assert_eq!(t.get(&k), m.get(&k)),
                _ => {
                    let g = next() % 5;
                    m.retain(|_, v| *v % 5 != g as u32);
                    t.retain(|_, v| *v % 5 != g as u32);
                }
            }
            assert_eq!(t.len(), m.len());
        }
        for (o, v) in &m {
            assert_eq!(t.get(o), Some(v));
        }
    }
}
