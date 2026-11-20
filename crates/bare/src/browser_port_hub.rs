//! The port hub: where a `MessagePort` pair that is split across pages keeps its messages (step 3c of `docs/design-gmail-frames.md`).
//!
//! One hub is shared by all the pages of one top page (`Cfg::hub` in `browser_child.rs`). It holds data only: no JS object, no `ObjId`.
//! A page has its own heap, so a message to a port of another page cannot be queued in that port. It waits here until the owner page takes it.
//!
//! An end is one side of a port pair that is split across pages. It has an id from the hub, an owner (a page number, or `None` while the
//! port is in flight between two pages) and a queue of wires. A wire is a message as data: the heap-free `State`, the transfer list (it
//! holds no `Transfer::Port`) and the byte cost. The owner page maps the id to its port object (`Sched::ends`). A port in a page has `me`
//! (its own end) and `remote` (the end of its peer).
//!
//! Order. The wires of one end leave in the order they came in. A port that moves keeps its undelivered messages: they go back to the
//! front of its queue.
//!
//! Cost. A page that never moves a port to another page never makes an end. The counters `wires` and `bytes` are the only state that
//! the drivers read for each round.
//!
//! Removal. An end goes when its port closes, when the GC drops its port, when its owner page ends, and when a message that carried it is
//! dropped (`release`). A send to a missing end is dropped with no error.
use crate::browser_navigation_state::Transfer;
use bare_js_api::{Cloned, Moved};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};

/// A message for an end, as data.
pub(crate) struct Wire {
    pub cloned: Cloned,
    /// The byte cost in the queue of a port (`16 + cloned.cost()`).
    pub bytes: usize,
}

struct End {
    /// The page that holds the port. `None`: the port is in flight.
    owner: Option<u32>,
    queue: VecDeque<Wire>,
}

#[derive(Default)]
struct Ends {
    pages: u32,
    next: u64,
    ends: HashMap<u64, End>,
}

#[derive(Default)]
pub(crate) struct PortHub {
    /// The wires that wait in all the ends.
    wires: Cell<usize>,
    bytes: Cell<usize>,
    st: RefCell<Ends>,
}

fn ends_of(xf: &[Moved]) -> impl Iterator<Item = u64> + '_ {
    xf.iter().filter_map(|m| match m {
        Moved::Host(b) => match b.downcast_ref::<Transfer>() {
            Some(Transfer::End { me, .. }) => Some(*me),
            _ => None,
        },
        Moved::Buffer(_) => None,
    })
}

impl PortHub {
    /// Does a wire wait?
    #[inline]
    pub(crate) fn busy(&self) -> bool {
        self.wires.get() > 0
    }

    pub(crate) fn wires(&self) -> usize {
        self.wires.get()
    }

    pub(crate) fn bytes(&self) -> usize {
        self.bytes.get()
    }

    /// A number for a new page. The top page is 0 and never asks.
    pub(crate) fn new_page(&self) -> u32 {
        let mut s = self.st.borrow_mut();
        s.pages += 1;
        s.pages
    }

    pub(crate) fn new_end(&self, owner: Option<u32>) -> u64 {
        let mut s = self.st.borrow_mut();
        s.next += 1;
        let id = s.next;
        s.ends.insert(id, End { owner, queue: VecDeque::new() });
        id
    }

    pub(crate) fn has(&self, end: u64) -> bool {
        self.st.borrow().ends.contains_key(&end)
    }

    pub(crate) fn set_owner(&self, end: u64, owner: Option<u32>) {
        if let Some(e) = self.st.borrow_mut().ends.get_mut(&end) {
            e.owner = owner;
        }
    }

    /// Queue a wire. A missing end gives the wire back.
    pub(crate) fn push(&self, end: u64, w: Wire) -> Result<(), Wire> {
        let mut s = self.st.borrow_mut();
        let Some(e) = s.ends.get_mut(&end) else { return Err(w) };
        self.wires.set(self.wires.get() + 1);
        self.bytes.set(self.bytes.get() + w.bytes);
        e.queue.push_back(w);
        Ok(())
    }

    /// Put wires back in front of the queue, in their order (the undelivered messages of a port that moves).
    pub(crate) fn prepend(&self, end: u64, ws: Vec<Wire>) {
        let mut s = self.st.borrow_mut();
        let Some(e) = s.ends.get_mut(&end) else { return };
        for w in ws.into_iter().rev() {
            self.wires.set(self.wires.get() + 1);
            self.bytes.set(self.bytes.get() + w.bytes);
            e.queue.push_front(w);
        }
    }

    pub(crate) fn pop(&self, end: u64) -> Option<Wire> {
        let w = self.st.borrow_mut().ends.get_mut(&end)?.queue.pop_front()?;
        self.wires.set(self.wires.get() - 1);
        self.bytes.set(self.bytes.get() - w.bytes);
        Some(w)
    }

    /// Remove the ends in `todo` and the ends that their queued wires carry.
    fn remove_all(&self, s: &mut Ends, mut todo: Vec<u64>) {
        while let Some(id) = todo.pop() {
            let Some(e) = s.ends.remove(&id) else { continue };
            for w in e.queue {
                self.wires.set(self.wires.get() - 1);
                self.bytes.set(self.bytes.get() - w.bytes);
                todo.extend(ends_of(w.cloned.moved()));
            }
        }
    }

    pub(crate) fn remove_end(&self, end: u64) {
        let mut s = self.st.borrow_mut();
        self.remove_all(&mut s, vec![end]);
    }

    /// The ports of a message that is dropped never arrive: their ends go.
    pub(crate) fn release(&self, xf: &[Moved]) {
        if ends_of(xf).next().is_some() {
            let mut s = self.st.borrow_mut();
            self.remove_all(&mut s, ends_of(xf).collect());
        }
    }

    /// A page ends: its ends go.
    pub(crate) fn drop_page(&self, page: u32) {
        let mut s = self.st.borrow_mut();
        if s.ends.is_empty() {
            return;
        }
        let gone: Vec<u64> = s.ends.iter().filter(|(_, e)| e.owner == Some(page)).map(|(&id, _)| id).collect();
        self.remove_all(&mut s, gone);
    }
}
