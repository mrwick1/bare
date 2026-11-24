//! Parse-ahead (plan step S2.3, design 6.11 item 16 and 6.10 item 19): parse a script on a worker thread when its text arrives, so the
//! page thread finds the result ready. The module is generic over the job. It does not know the parser: [`Job::run`] is the parse step.
//!
//! - Identity: [`Identity`] is the hash and the length of the text. The caller computes it once (`Identity::of`), passes it to
//!   [`Sender::submit`], and finds it again in [`Published::id`]. The compile step and the shared-code lookup use that value and do not
//!   hash the text again. A collision (same hash and length, other bytes) is caught by [`Published::is`], which compares the bytes.
//! - Claim: the table is keyed by (identity, mode). A submit looks at the table first. If it finds the entry (queued, running or done),
//!   it only counts one more waiter and returns a handle for that entry: it copies no text and allocates no job. A submit that finds no
//!   entry claims the key (a small slot in the table) and copies the text after that, outside the lock. So a text is copied and parsed
//!   at most once for each live entry, also with many threads.
//! - Entry states: `Claimed` (the text is not copied yet), `Queued` (holds the one copy of the text), `Running`, `Done` (holds the
//!   result). One thread, the worker, runs jobs and publishes. It starts at the first job and ends after [`Job::PARK`] with no work.
//! - Waiters: each [`Pending`] is one waiter. When the last one goes (drop, or [`Pending::take`] on queued work), a queued entry leaves the
//!   table and the queue, and the worker never sees it. A running entry stays and may get a waiter again. If it has none when the job
//!   ends, the result is dropped (unpublished) and the entry leaves. A done entry leaves with its last waiter: later duplicates are for
//!   the cache of the caller ([`Job::cached`]), which is asked before the claim.
//! - Page end: [`Ahead::cancel`] (also on drop) empties the table and the queue. A running job is not interrupted. Its result is dropped
//!   when it ends. Every blocked waiter wakes and gets `None`.
//! - Memory: one slot in the table, one queue key, and the one copy of the text for each queued job (about 64 bytes and the text).
//!   Results stay in the table only while a waiter holds a handle or until the page ends.
//! - Thread safety: this module has no `unsafe`. `Job::Out` must be `Send + Sync`.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// The identity of a source text: a hash of its bytes and its length. Same text, same identity. Two texts may share one, so a reader
/// of a result compares the bytes ([`Published::is`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Identity {
    hash: u64,
    len: usize,
}

impl Identity {
    /// Multiply hash of 8 bytes at a time (the hash of `parse_cache` in the old engine), and the length. It reads the text once.
    pub fn of(text: &str) -> Identity {
        const K: u64 = 0x517c_c1b7_2722_0a95;
        let b = text.as_bytes();
        let mut h = b.len() as u64;
        let mut c = b.chunks_exact(8);
        for w in &mut c {
            h = (h.rotate_left(5) ^ u64::from_le_bytes(w.try_into().unwrap())).wrapping_mul(K);
        }
        let mut t = [0u8; 8];
        t[..c.remainder().len()].copy_from_slice(c.remainder());
        Identity { hash: (h.rotate_left(5) ^ u64::from_le_bytes(t)).wrapping_mul(K), len: b.len() }
    }

    pub fn hash(self) -> u64 {
        self.hash
    }

    pub fn len(self) -> usize {
        self.len
    }

    pub fn is_empty(self) -> bool {
        self.len == 0
    }
}

/// The parse step. The worker calls `run` on its own thread, once for each claimed (identity, mode).
pub trait Job: Send + Sync + 'static {
    /// The parse mode (script or module, lazy or not). The same text in two modes is two entries.
    type Mode: Copy + Eq + Hash + Send + 'static;
    /// The result (the bytecode and the lazy data). It is shared by every waiter.
    type Out: Send + Sync + 'static;

    /// How long the worker waits for a job before its thread ends.
    const PARK: Duration = Duration::from_secs(2);
    /// The stack of the worker thread (the parser recurses).
    const STACK: usize = 8 << 20;

    /// Does a cache of the caller hold this (identity, mode)? Then no submit is made. It runs with no lock of this module held.
    fn cached(&self, _mode: Self::Mode, _id: Identity) -> bool {
        false
    }

    /// The parse. `text` is the one copy of the source: the result may keep a clone of it.
    fn run(&self, mode: Self::Mode, id: Identity, text: &Arc<str>) -> Self::Out;
}

/// A result that the worker published.
pub struct Published<T> {
    /// The identity that the submit carried.
    pub id: Identity,
    /// The text that was parsed (the one copy).
    pub text: Arc<str>,
    pub out: T,
}

impl<T> Published<T> {
    /// Are these the bytes that were parsed? (The check for a hash collision.)
    pub fn is(&self, text: &str) -> bool {
        *self.text == *text
    }
}

type Key<J> = (Identity, <J as Job>::Mode);

enum St<T> {
    /// A submit has claimed the key. It copies the text now.
    Claimed,
    Queued(Arc<str>),
    Running,
    Done(Arc<Published<T>>),
}

struct Slot<T> {
    /// Tells this entry from a later entry of the same key.
    era: u64,
    waiters: usize,
    st: St<T>,
}

struct Inner<J: Job> {
    map: HashMap<Key<J>, Slot<J::Out>>,
    queue: VecDeque<Key<J>>,
    next_era: u64,
    /// The worker thread runs (or is about to start).
    thread: bool,
    closed: bool,
    stats: Stats,
}

/// Counts, for tests and reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Keys that a submit claimed (each copies the text once).
    pub claims: usize,
    /// Jobs that the worker began.
    pub began: usize,
    /// Results that the worker put in the table.
    pub published: usize,
    /// Results that the worker dropped: no waiter was left, the page ended, or the job panicked.
    pub unpublished: usize,
    /// Entries in the table now.
    pub entries: usize,
    /// Jobs in the queue now.
    pub queued: usize,
    /// Does the worker thread run?
    pub thread: bool,
}

struct Shared<J: Job> {
    job: J,
    min: usize,
    max: usize,
    inner: Mutex<Inner<J>>,
    /// The worker waits here for a job.
    wake: Condvar,
    /// A waiter waits here for a result.
    done: Condvar,
}

impl<J: Job> Shared<J> {
    fn lock(&self) -> MutexGuard<'_, Inner<J>> {
        self.inner.lock().unwrap()
    }
}

impl<J: Job> Inner<J> {
    /// Join the entry of `key`, if there is one: one more waiter.
    fn join(&mut self, key: Key<J>) -> Option<u64> {
        let s = self.map.get_mut(&key)?;
        s.waiters += 1;
        Some(s.era)
    }

    /// Remove the entry (and its queue key). The caller drops the slot with the lock released: it may hold a text or a result.
    fn remove(&mut self, key: &Key<J>) -> Option<Slot<J::Out>> {
        let s = self.map.remove(key)?;
        if matches!(s.st, St::Queued(_)) {
            self.queue.retain(|k| k != key);
        }
        Some(s)
    }
}

/// The owner of the parse worker of one page. Dropping it ends the work (see [`Ahead::cancel`]).
pub struct Ahead<J: Job>(Arc<Shared<J>>);

/// A handle for the threads that download texts.
pub struct Sender<J: Job>(Arc<Shared<J>>);

impl<J: Job> Clone for Sender<J> {
    fn clone(&self) -> Self {
        Sender(self.0.clone())
    }
}

impl<J: Job> Ahead<J> {
    /// A text shorter than `min` or longer than `max` bytes is never submitted (a short text parses faster on the page thread than a
    /// hand-over costs).
    pub fn new(job: J, min: usize, max: usize) -> Ahead<J> {
        let inner = Inner { map: HashMap::new(), queue: VecDeque::new(), next_era: 0, thread: false, closed: false, stats: Stats::default() };
        Ahead(Arc::new(Shared { job, min, max, inner: Mutex::new(inner), wake: Condvar::new(), done: Condvar::new() }))
    }

    pub fn sender(&self) -> Sender<J> {
        Sender(self.0.clone())
    }

    /// The page is done: queued work is dropped, results are dropped, every waiter gets `None`, and no later submit is taken. A job that
    /// runs is not interrupted: the worker drops its result when it ends, then its thread ends.
    pub fn cancel(&self) {
        let (map, queue) = {
            let mut g = self.0.lock();
            g.closed = true;
            (std::mem::take(&mut g.map), std::mem::take(&mut g.queue))
        };
        drop((map, queue));
        self.0.wake.notify_all();
        self.0.done.notify_all();
    }

    pub fn stats(&self) -> Stats {
        let g = self.0.lock();
        Stats { entries: g.map.len(), queued: g.queue.len(), thread: g.thread, ..g.stats }
    }
}

impl<J: Job> Drop for Ahead<J> {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl<J: Job> Sender<J> {
    /// Parse `text` in `mode` on the worker. `id` is `Identity::of(text)`: the caller computes it once and it is not checked here.
    /// It gives `None`, and starts nothing, for a text that is too short or too long, for a key that [`Job::cached`] knows, and after
    /// [`Ahead::cancel`]. A key that is queued, running or done gives a handle for that entry, with no copy and no second parse.
    /// Call it from any thread. Keep the handle until the result is needed: when the last handle goes, queued work is dropped.
    pub fn submit(&self, mode: J::Mode, id: Identity, text: &str) -> Option<Pending<J>> {
        let sh = &self.0;
        debug_assert_eq!(id.len, text.len());
        if text.len() < sh.min || text.len() > sh.max {
            return None;
        }
        let key = (id, mode);
        let mut g = sh.lock();
        if g.closed {
            return None;
        }
        if let Some(era) = g.join(key) {
            return Some(Pending { sh: sh.clone(), key, era });
        }
        drop(g);
        if sh.job.cached(mode, id) {
            return None;
        }
        let mut g = sh.lock();
        if g.closed {
            return None;
        }
        if let Some(era) = g.join(key) {
            return Some(Pending { sh: sh.clone(), key, era });
        }
        // Claim the key. The copy of the text comes after this, with the lock released.
        let era = g.next_era;
        g.next_era += 1;
        g.stats.claims += 1;
        g.map.insert(key, Slot { era, waiters: 1, st: St::Claimed });
        drop(g);
        let pending = Pending { sh: sh.clone(), key, era };
        let copy: Arc<str> = Arc::from(text);
        let mut g = sh.lock();
        // A waiter took the entry (or the page ended) while the text was copied: the copy is dropped.
        if !matches!(g.map.get_mut(&key), Some(s) if s.era == era && matches!(s.st, St::Claimed)) {
            drop(g);
            return Some(pending);
        }
        g.map.get_mut(&key).unwrap().st = St::Queued(copy);
        g.queue.push_back(key);
        if !g.thread {
            let me = sh.clone();
            g.thread = std::thread::Builder::new().stack_size(J::STACK).spawn(move || work(&me)).is_ok();
        }
        drop(g);
        sh.wake.notify_one();
        Some(pending)
    }
}

/// One waiter for the result of one claimed entry. Send it to the thread that needs the result.
pub struct Pending<J: Job> {
    sh: Arc<Shared<J>>,
    key: Key<J>,
    era: u64,
}

impl<J: Job> Pending<J> {
    /// The identity of the entry.
    pub fn id(&self) -> Identity {
        self.key.0
    }

    /// The result, for the page thread that needs it now. A published result is returned. A running job is waited for. Queued work is
    /// not: the entry is dropped (also for the other waiters, who get `None`), and the caller parses itself. `None` also after the
    /// end of the page and after a job that panicked. Check the bytes with [`Published::is`].
    pub fn take(&self) -> Option<Arc<Published<J::Out>>> {
        let mut g = self.sh.lock();
        loop {
            let s = g.map.get(&self.key).filter(|s| s.era == self.era)?;
            match &s.st {
                St::Done(p) => return Some(p.clone()),
                St::Running => g = self.sh.done.wait(g).unwrap(),
                St::Claimed | St::Queued(_) => {
                    let gone = g.remove(&self.key);
                    drop(g);
                    drop(gone);
                    // Another waiter may block in `wait` on this entry.
                    self.sh.done.notify_all();
                    return None;
                }
            }
        }
    }

    /// The result, waiting also for queued work (the worker may be busy with other jobs first). `None` after the end of the page, after a
    /// job that panicked, and after a [`Pending::take`] that dropped the entry.
    pub fn wait(&self) -> Option<Arc<Published<J::Out>>> {
        let mut g = self.sh.lock();
        loop {
            let s = g.map.get(&self.key).filter(|s| s.era == self.era)?;
            if let St::Done(p) = &s.st {
                return Some(p.clone());
            }
            g = self.sh.done.wait(g).unwrap();
        }
    }
}

impl<J: Job> Clone for Pending<J> {
    /// Another waiter for the same entry.
    fn clone(&self) -> Self {
        if let Some(s) = self.sh.lock().map.get_mut(&self.key).filter(|s| s.era == self.era) {
            s.waiters += 1;
        }
        Pending { sh: self.sh.clone(), key: self.key, era: self.era }
    }
}

impl<J: Job> Drop for Pending<J> {
    fn drop(&mut self) {
        let mut g = self.sh.lock();
        let Some(s) = g.map.get_mut(&self.key).filter(|s| s.era == self.era) else { return };
        s.waiters -= 1;
        // A running job is kept: a new waiter may come, and the worker drops the result if none does.
        if s.waiters == 0 && !matches!(s.st, St::Running) {
            let gone = g.remove(&self.key);
            drop(g);
            drop(gone);
        }
    }
}

/// The next job: its key, entry and text. `None` ends the thread.
fn next<J: Job>(sh: &Shared<J>) -> Option<(Key<J>, u64, Arc<str>)> {
    let mut g = sh.lock();
    loop {
        if g.closed {
            g.thread = false;
            return None;
        }
        let Some(key) = g.queue.pop_front() else {
            let (ng, t) = sh.wake.wait_timeout(g, J::PARK).unwrap();
            g = ng;
            if t.timed_out() && g.queue.is_empty() {
                g.thread = false;
                return None;
            }
            continue;
        };
        let Some(s) = g.map.get_mut(&key) else { continue };
        let St::Queued(text) = std::mem::replace(&mut s.st, St::Running) else {
            unreachable!("a key in the queue names a queued entry")
        };
        let era = s.era;
        g.stats.began += 1;
        return Some((key, era, text));
    }
}

fn work<J: Job>(sh: &Shared<J>) {
    while let Some((key, era, text)) = next(sh) {
        let out = catch_unwind(AssertUnwindSafe(|| sh.job.run(key.1, key.0, &text)));
        // What the table does not keep is dropped after the lock is released.
        let mut junk = None;
        let mut g = sh.lock();
        match g.map.get_mut(&key).filter(|s| s.era == era) {
            Some(s) if s.waiters > 0 && out.is_ok() => {
                let out = out.ok().unwrap();
                s.st = St::Done(Arc::new(Published { id: key.0, text, out }));
                g.stats.published += 1;
            }
            Some(_) => {
                junk = g.map.remove(&key);
                g.stats.unpublished += 1;
            }
            None => g.stats.unpublished += 1,
        }
        drop(g);
        drop(junk);
        sh.done.notify_all();
    }
}

#[cfg(test)]
#[path = "parse_ahead_tests.rs"]
mod tests;
