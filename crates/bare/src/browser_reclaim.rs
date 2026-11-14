//! Give the nodes of dead trees back to the arena (`Document::reclaim`).
//!
//! The collector tells which wrappers are dead (`Host::prune`). This module then finds the nodes that the host still names, and frees the trees
//! that have none. It runs between two tasks of the event loop, when no script runs and no host call is on the stack, so no local variable
//! of Rust code holds a `NodeId`. A `NodeId` has no generation: it is safe because each holder of a `NodeId` does one of three things.
//! - It is a root. The module has a `held` function that gives its nodes, and the tree of each stays.
//! - It is weak. The module has a `forget` function that drops its entries for freed nodes, before any node can take the slot.
//! - It is a cache that the new `Document::revision` makes stale (styles, layout, selector results, option lists).
//!
//! A wrapper is a root through `by_node`. An object that names a node (an `Attr`, a `CSSStyleDeclaration`) keeps the wrapper of the node alive
//! (`trace`), and its module gives the node as well (`held`).
//! Not supported, so no node is freed while it is on: custom elements (their holders are not listed).

use crate::browser::*;
use bare_html::dom::{Document, NodeId, Span};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::hash::BuildHasherDefault;

/// A walk costs time in the nodes in use. Free nodes help only the nodes that a script makes next, so a walk waits until this many nodes
/// (and a fourth of the nodes in use after the last walk) were made since the last one. The same pacing as the heap collect (`gc_due`).
const FLOOR: usize = 2048;

/// The state of the reclaim of one realm.
#[derive(Default)]
pub(crate) struct Reclaim {
    /// A collection ran since the last reclaim: wrappers died.
    due: Cell<bool>,
    /// Test mode: reclaim after every task, even when no collection ran.
    always: Cell<bool>,
    /// Test mode: collect after each `run_script` (`BARE_RECLAIM_STRESS`).
    #[cfg(test)]
    each: Cell<bool>,
    /// Nodes that code outside the realm keeps for the life of the document (the refs of a driver, the pictures of a page, the scripts that the loader found): their trees stay.
    pins: RefCell<HashSet<NodeId, BuildHasherDefault<IdHash>>>,
    /// Nodes in use after the last walk, and the growth that makes the next walk worth its cost (`FLOOR`, `set_floor` in tests).
    base: Cell<usize>,
    floor: Cell<usize>,
    /// Reclaims that freed something, and nodes freed.
    runs: Cell<u32>,
    freed: Cell<u64>,
}

impl Reclaim {
    #[cfg(test)]
    pub(crate) fn set_floor(&self, n: usize) {
        self.floor.set(n);
    }

    #[cfg(test)]
    pub(crate) fn set_always(&self, on: bool) {
        self.always.set(on);
    }

    /// Test mode of the whole run: the environment asks for it (`BARE_RECLAIM_STRESS`).
    #[cfg(test)]
    pub(crate) fn each(&self) -> bool {
        self.each.get()
    }

    #[cfg(test)]
    pub(crate) fn set_each(&self, on: bool) {
        self.each.set(on);
    }
}

impl Dom {
    /// Keep the tree of `n` for the life of the document: a holder outside the realm names it.
    pub(crate) fn pin(&self, n: NodeId) {
        self.reclaim.pins.borrow_mut().insert(n);
    }

    /// Wrappers died: the nodes of their trees may be free.
    pub(crate) fn reclaim_wanted(&self) {
        self.reclaim.due.set(true);
    }

    /// Free the trees that nothing names. Call it only when no script runs and no host call is on the stack.
    /// The test for "nothing to do" is inline: the event loop asks after each task.
    #[inline]
    pub(crate) fn reclaim(&self) -> usize {
        let r = &self.reclaim;
        if !(r.due.get() || r.always.get()) {
            return 0;
        }
        self.reclaim_walk()
    }

    #[inline]
    fn reclaim_walk(&self) -> usize {
        let r = &self.reclaim;
        if self.ce.on.get() {
            return 0;
        }
        if !r.always.get() {
            let base = r.base.get();
            let floor = if cfg!(test) { r.floor.get() } else { FLOOR };
            // `due` stays set: the walk comes when enough nodes were made. A busy document borrow (`try_borrow`) means a walk now is not safe either.
            if self.doc.try_borrow().map_or(true, |d| d.live_len() < base + floor.max(base / 4)) {
                return 0;
            }
        }
        self.reclaim_run()
    }

    /// The walk. A function of its own: the early returns above pay for none of its locals.
    #[inline(never)]
    fn reclaim_run(&self) -> usize {
        let r = &self.reclaim;
        r.due.set(false);
        let mut live = Vec::new();
        {
            let st = self.st.borrow();
            live.extend(st.by_node.iter().map(|(&n, _)| n));
            live.extend(st.style_override.map(|s| s.0.0));
            live.extend([st.focus.node, st.focus.sel_owner].into_iter().flatten());
            for e in st.events.values() {
                live.extend(e.path.iter().filter_map(|t| if let Target::Node(n) = t { Some(*n) } else { None }));
            }
        }
        live.extend(self.clicking.borrow().iter().copied());
        live.extend(r.pins.borrow().iter().copied());
        crate::browser_attr::held(self, &mut live);
        crate::browser_computed::held(self, &mut live);
        crate::browser_controls::held(self, &mut live);
        crate::browser_cssom::held(self, &mut live);
        crate::browser_frames::held(self, &mut live);
        crate::browser_img::held(self, &mut live);
        crate::browser_loader::held(self, &mut live);
        crate::browser_media::held(self, &mut live);
        crate::browser_motion::held(self, &mut live);
        crate::browser_mutation::held(self, &mut live);
        crate::browser_nodelist::held(self, &mut live);
        crate::browser_range::held(self, &mut live);
        crate::browser_scheduler::held(self, &mut live);
        crate::browser_scroll::held(self, &mut live);
        crate::browser_style::held(self, &mut live);
        crate::browser_ui::held(self, &mut live);
        crate::browser_walker::held(self, &mut live);
        let Ok(mut doc) = self.doc.try_borrow_mut() else {
            r.due.set(true);
            return 0;
        };
        let freed = doc.reclaim(&live, &mut |f: &mut dyn FnMut(&mut Span)| {
            crate::browser_attr::pins(self, f);
            crate::browser_cssom::pins(self, f);
            crate::browser_style::pins(self, f);
        });
        r.base.set(doc.live_len());
        if freed == 0 {
            return 0;
        }
        let doc: &Document = &doc;
        let gone = |n: NodeId| doc.is_free(n);
        {
            let mut st = self.st.borrow_mut();
            st.inputs.retain(|&n, _| !gone(n));
            st.input_bytes = st.inputs.values().map(|i| i.value.as_ref().map_or(0, String::len)).sum();
        }
        self.dialogs.borrow_mut().retain(|&n, _| !gone(n));
        crate::browser_computed::forget(self, &gone);
        crate::browser_controls::forget(self, &gone);
        crate::browser_loader::forget(self, &gone);
        crate::browser_select::forget(self, &gone);
        r.runs.set(r.runs.get() + 1);
        r.freed.set(r.freed.get() + freed as u64);
        freed
    }
}

impl Browser {
    /// Keep the tree of `n` for the life of the document. A caller outside the realm (a page, a driver) that holds a `NodeId` across
    /// a call that can collect pins it: a freed node gives its slot to a new one.
    pub fn pin(&self, n: NodeId) {
        self.dom.pin(n);
    }

    /// Test mode: free nodes so that a stale `NodeId` panics, and reclaim after every task, not only after a collection.
    #[cfg(test)]
    pub(crate) fn set_reclaim_stress(&self, on: bool) {
        self.dom.doc.borrow_mut().set_poison(on);
        self.dom.reclaim.always.set(on);
    }

    /// How many reclaims freed nodes, and how many nodes they freed.
    pub fn reclaim_stats(&self) -> (u32, u64) {
        (self.dom.reclaim.runs.get(), self.dom.reclaim.freed.get())
    }
}
