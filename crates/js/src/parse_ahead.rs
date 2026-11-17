//! Parse a script on a worker thread when its text arrives, so the page thread finds the tree ready.
//!
//! A page waits for the network between the arrival of a script and the turn that runs it (Gmail: the big scripts arrive 0.6 to 0.7 s
//! before the document ends). The loader gives each such text to [`Sender::submit`] from the thread that downloaded it. One worker thread
//! runs the same parse that `parse_cache::parse` runs. When the page thread parses the same text (same bytes and [`Kind`]), the cache
//! asks [`take`] first.
//!
//! - Result: a pure function of the text and the kind. It does not depend on when or where the parse ran. A syntax error is stored as
//!   the result and reaches the realm at `run_script` or `module_parse`, as before. Nothing is reported by the worker.
//! - Order: the worker only fills a slot. It never runs code. Scripts run in the order that the loader chooses.
//! - Slot states: `Queued` (the page thread may cancel it and parse itself), `Running` (the page thread waits on the slot), `Done`
//!   (the page thread takes the tree), `Gone`. One slot per (hash, kind). A slot is taken at most once.
//! - Memory: the worker makes the one `Rc<str>` copy of the text that the tree keeps (the same copy that `Program::into_owned` makes on
//!   the page thread). The tree is the only owner of it when it is handed over. The registry holds a result only until the page thread
//!   takes it or the page ends ([`Ahead::cancel`], also on drop): then queued slots are skipped and finished trees are dropped.
//! - `Send`: a `Program` holds `Rc` (not `Send`). Every `Rc` of a tree is owned by that tree (the source, the marks, the lazy function
//!   lists), and a handle that the worker or the submitting thread made is dropped before the hand-over. So no count is touched by two
//!   threads at once. The hand-over goes through a mutex (a release and an acquire). That is the whole argument for [`Hand`].
//! - Virtual clock: the worker does not use time or the clock of `bare_net`. The page thread waits in real time on a condition variable,
//!   and no virtual time passes. A run gets the same tree whether the worker or the page thread parsed it, so the page code sees the same
//!   steps and the same errors. Only the instructions of the page thread change.

use crate::ast::Program;
use crate::parse::{Error, Limits};
use crate::parse_cache::{self, Kind, LAZY_MIN_SOURCE};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// A shorter text parses faster on the page thread than a hand-over costs.
pub const MIN_AHEAD: usize = LAZY_MIN_SOURCE;

/// How long the worker waits for a job before its thread ends.
const PARK: Duration = Duration::from_secs(2);

/// A value that moves to another thread with all its `Rc` handles (see the module comment).
struct Hand<T>(T);
// SAFETY: see the module comment. The value and every `Rc` inside it have no owner outside it when it moves.
unsafe impl<T> Send for Hand<T> {}

type Tree = Result<Rc<Program<'static>>, Error>;
/// The result, the count of nodes made, and for an error the text it belongs to (the byte compare of the cache).
type Out = (Tree, u32, Option<Rc<str>>);

enum St {
    Queued(Hand<Rc<str>>),
    Running,
    Done(Hand<Out>),
    Gone,
}

struct Slot {
    st: Mutex<St>,
    ready: Condvar,
}

#[derive(Default)]
struct Inner {
    slots: HashMap<(u64, Kind), Arc<Slot>>,
    queue: VecDeque<(Arc<Slot>, Kind)>,
    /// A worker thread runs (or is about to start).
    thread: bool,
}

#[derive(Default)]
struct Shared {
    closed: AtomicBool,
    inner: Mutex<Inner>,
    wake: Condvar,
    /// Parses that the worker began. For tests and reports.
    began: AtomicUsize,
    /// Results that the page thread took.
    taken: AtomicUsize,
}

/// The owner of the parse worker of one page. Dropping it ends the work (see [`Ahead::cancel`]).
#[derive(Default)]
pub struct Ahead(Arc<Shared>);

/// A handle for the threads that download texts.
#[derive(Clone)]
pub struct Sender(Arc<Shared>);

thread_local! {
    /// The worker of the page that runs on this thread.
    static HERE: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
}

impl Ahead {
    pub fn new() -> Ahead {
        Ahead::default()
    }

    pub fn sender(&self) -> Sender {
        Sender(self.0.clone())
    }

    /// The parse cache of this thread asks this worker from now on (until the next `attach` or the end of the worker).
    pub fn attach(&self) {
        HERE.with_borrow_mut(|h| *h = Some(self.0.clone()));
    }

    /// The page is done: slots that have not begun are skipped, finished trees are dropped, the thread ends. A parse that runs is not
    /// interrupted (the parser has no check for it): the worker drops its tree when it ends.
    pub fn cancel(&self) {
        self.0.closed.store(true, Ordering::Release);
        let (slots, queue) = {
            let mut i = self.0.inner.lock().unwrap();
            (std::mem::take(&mut i.slots), std::mem::take(&mut i.queue))
        };
        for (_, s) in slots {
            *s.st.lock().unwrap() = St::Gone;
        }
        drop(queue);
        self.0.wake.notify_all();
    }

    /// Parses that the worker began.
    pub fn began(&self) -> usize {
        self.0.began.load(Ordering::Relaxed)
    }

    /// Results that the page thread took from the worker.
    pub fn taken(&self) -> usize {
        self.0.taken.load(Ordering::Relaxed)
    }

    /// Is the worker thread running?
    pub fn running(&self) -> bool {
        self.0.inner.lock().unwrap().thread
    }
}

impl Drop for Ahead {
    fn drop(&mut self) {
        self.cancel();
        HERE.with_borrow_mut(|h| {
            if h.as_ref().is_some_and(|s| Arc::ptr_eq(s, &self.0)) {
                *h = None;
            }
        });
    }
}

impl Sender {
    /// Parse `source` as `kind` on the worker. Nothing happens for a short or a too long text, for a text that is queued already, or after
    /// [`Ahead::cancel`]. Call it from any thread.
    pub fn submit(&self, kind: Kind, source: &str) {
        let sh = &self.0;
        if source.len() < MIN_AHEAD || source.len() > Limits::default().max_source || sh.closed.load(Ordering::Acquire) {
            return;
        }
        let key = (parse_cache::hash(source.as_bytes()), kind);
        // A text that a cache has already (an earlier document of the session ran it) is not parsed again.
        if parse_cache::known(key) {
            return;
        }
        let slot = Arc::new(Slot { st: Mutex::new(St::Queued(Hand(Rc::from(source)))), ready: Condvar::new() });
        let mut i = sh.inner.lock().unwrap();
        if sh.closed.load(Ordering::Acquire) || i.slots.contains_key(&key) {
            return;
        }
        i.slots.insert(key, slot.clone());
        i.queue.push_back((slot, kind));
        if !i.thread {
            let me = self.0.clone();
            let stack = if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 };
            i.thread = std::thread::Builder::new().stack_size(stack).spawn(move || work(&me)).is_ok();
        }
        drop(i);
        sh.wake.notify_one();
    }
}

fn work(sh: &Arc<Shared>) {
    loop {
        let (slot, kind) = {
            let mut i = sh.inner.lock().unwrap();
            loop {
                if sh.closed.load(Ordering::Acquire) {
                    i.thread = false;
                    return;
                }
                if let Some(j) = i.queue.pop_front() {
                    break j;
                }
                let (g, t) = sh.wake.wait_timeout(i, PARK).unwrap();
                i = g;
                if t.timed_out() && i.queue.is_empty() {
                    i.thread = false;
                    return;
                }
            }
        };
        let src = {
            let mut st = slot.st.lock().unwrap();
            match std::mem::replace(&mut *st, St::Running) {
                St::Queued(Hand(src)) => src,
                other => {
                    *st = other;
                    continue;
                }
            }
        };
        sh.began.fetch_add(1, Ordering::Relaxed);
        let out = parse(kind, src);
        let mut st = slot.st.lock().unwrap();
        *st = if sh.closed.load(Ordering::Acquire) { St::Gone } else { St::Done(Hand(out)) };
        drop(st);
        slot.ready.notify_all();
    }
}

/// The parse that `parse_cache::parse` runs for a text of the default limits, with the text kept as the one `Rc<str>` of the tree.
fn parse(kind: Kind, src: Rc<str>) -> Out {
    // The tree borrows `view` while it is made. `with_shared_source` then swaps the borrow for `src`, the owner that it keeps.
    let view = src.clone();
    let (r, used) = parse_cache::parse_raw(kind, &view, Limits::default());
    let out = match r {
        Ok(p) => (Ok(Rc::new(p.with_shared_source(src))), used, None),
        Err(e) => (Err(e), used, Some(src)),
    };
    drop(view);
    debug_assert!(matches!(&out.0, Ok(p) if matches!(&p.source, crate::ast::Src::Shared(s) if Rc::strong_count(s) == 1)) || out.0.is_err());
    out
}

/// Results that the page thread of this thread took from its worker so far (for tests and reports). 0 with no worker attached.
pub fn taken_here() -> usize {
    HERE.with_borrow(|h| h.as_ref().map_or(0, |s| s.taken.load(Ordering::Relaxed)))
}

/// The result of the parse of `source` as `kind` (`hash`: `parse_cache::hash` of its bytes), when the worker of this thread has it. It
/// waits when the worker runs this text. It gives `None` (the caller parses) when there is no such slot, when the worker has not begun
/// (the slot is cancelled: the same text is never parsed twice), after the end of the page, or when the bytes differ.
pub(crate) fn take(kind: Kind, hash: u64, source: &str) -> Option<(Tree, u32)> {
    let sh = HERE.with_borrow(|h| h.clone())?;
    if sh.closed.load(Ordering::Acquire) {
        return None;
    }
    let slot = sh.inner.lock().unwrap().slots.remove(&(hash, kind))?;
    let mut st = slot.st.lock().unwrap();
    loop {
        match &*st {
            St::Queued(_) | St::Gone => {
                *st = St::Gone;
                return None;
            }
            St::Running => st = slot.ready.wait(st).unwrap(),
            St::Done(_) => break,
        }
    }
    let St::Done(Hand((r, used, text))) = std::mem::replace(&mut *st, St::Gone) else { unreachable!() };
    drop(st);
    let same = match (&r, &text) {
        (Ok(p), _) => *p.source == *source,
        (Err(_), Some(t)) => **t == *source,
        (Err(_), None) => false,
    };
    if !same {
        return None;
    }
    sh.taken.fetch_add(1, Ordering::Relaxed);
    Some((r, used))
}

#[cfg(test)]
impl Ahead {
    fn inner_empty(&self) -> bool {
        let i = self.0.inner.lock().unwrap();
        i.queue.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{EvalLimits, Interp};
    use crate::value::Value;

    /// A script of `MIN_AHEAD` bytes or more that ends with `tail`.
    fn big(tail: &str) -> String {
        format!("/*{}*/\n{tail}", "x".repeat(MIN_AHEAD))
    }

    /// A text that takes a few ms to parse: a long run of statements.
    fn long(tail: &str) -> String {
        format!("{}\n{tail}", "var a = [1, 2, 3, {b: 4}]; a.length;\n".repeat(60_000))
    }

    fn realm() -> Interp<'static> {
        Interp::realm(crate::heap::Heap::default(), EvalLimits::default()).unwrap()
    }

    fn wait_for(f: impl Fn() -> bool) {
        let t = std::time::Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(30), "timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn run(src: &str, ahead: Option<&Ahead>) -> Result<Value, String> {
        parse_cache::clear();
        let mut it = realm();
        if let Some(a) = ahead {
            a.sender().submit(Kind::ClassicLazy, src);
            a.attach();
            wait_for(|| a.began() == 1);
        }
        it.run_script(src).map_err(|e| format!("{e:?}"))
    }

    #[test]
    fn a_script_gives_the_same_result_with_and_without_the_worker() {
        let src = big("var f = function () { return 1 + 2; }; f() + 40");
        let a = Ahead::new();
        let (with, without) = (run(&src, Some(&a)), run(&src, None));
        assert!(matches!(with, Ok(Value::Number(n)) if n == 43.0), "{with:?}");
        assert_eq!(format!("{with:?}"), format!("{without:?}"));
        assert_eq!(a.taken(), 1);
        HERE.with_borrow_mut(|h| *h = None);
    }

    #[test]
    fn a_syntax_error_is_the_same_error_at_run_script_not_at_submit() {
        let src = big("var = 1;");
        let a = Ahead::new();
        a.sender().submit(Kind::ClassicLazy, &src);
        a.attach();
        wait_for(|| a.began() == 1);
        // Nothing was thrown yet. The realm gets the error at `run_script`, as without the worker.
        parse_cache::clear();
        let mut it = realm();
        let with = it.run_script(&src);
        assert!(matches!(with, Err(crate::eval::Error::Throw(_))));
        assert_eq!(a.taken(), 1);
        let msg = |it: &mut Interp<'_>, r: Result<Value, crate::eval::Error>| match r {
            Err(crate::eval::Error::Throw(v)) => it.display(v),
            other => panic!("{other:?}"),
        };
        let with = msg(&mut it, with);
        parse_cache::clear();
        let mut it = realm();
        let r = it.run_script(&src);
        assert_eq!(with, msg(&mut it, r));
        HERE.with_borrow_mut(|h| *h = None);
    }

    #[test]
    fn a_lazy_function_of_a_tree_from_the_worker_runs() {
        let body = "var s = 0; for (var i = 0; i < 10; i++) { s += i; } return s + 1000000 + 2000000 + 3000000 + 4000000 + 5000000 + 6000000;";
        let src = big(&format!("function f() {{ {body} }} f()"));
        let a = Ahead::new();
        let r = run(&src, Some(&a));
        assert!(matches!(r, Ok(Value::Number(n)) if n == 21_000_045.0), "{r:?}");
        assert_eq!(a.taken(), 1);
        HERE.with_borrow_mut(|h| *h = None);
    }

    #[test]
    fn a_module_from_the_worker_parses() {
        let src = big("export const x = 1;");
        let a = Ahead::new();
        parse_cache::clear();
        a.sender().submit(Kind::ModuleLazy, &src);
        a.attach();
        wait_for(|| a.began() == 1);
        let mut it = realm();
        assert!(it.module_parse(&src, "https://a.test/m.js").is_ok());
        assert_eq!(a.taken(), 1);
        HERE.with_borrow_mut(|h| *h = None);
    }

    #[test]
    fn a_text_that_the_worker_has_not_begun_is_parsed_by_the_page_thread_once() {
        let (a1, a2) = (big("1 + 1"), big("2 + 2"));
        let a = Ahead::new();
        parse_cache::clear();
        // The first job is long, so the second still waits in the queue when the page thread asks for it.
        let first = long("1");
        a.sender().submit(Kind::ClassicLazy, &first);
        wait_for(|| a.began() == 1);
        a.sender().submit(Kind::ClassicLazy, &a2);
        a.attach();
        let mut it = realm();
        assert!(matches!(it.run_script(&a2), Ok(Value::Number(n)) if n == 4.0));
        assert_eq!(a.taken(), 0);
        // The worker skips the cancelled slot: only the first job ran.
        wait_for(|| a.taken() == 0 && a.inner_empty());
        assert_eq!(a.began(), 1);
        let _ = a1;
        HERE.with_borrow_mut(|h| *h = None);
    }

    #[test]
    fn a_script_that_is_needed_before_its_parse_ends_waits_for_it() {
        let src = big("40 + 2");
        let sh = Arc::new(Shared::default());
        let slot = Arc::new(Slot { st: Mutex::new(St::Running), ready: Condvar::new() });
        sh.inner.lock().unwrap().slots.insert((parse_cache::hash(src.as_bytes()), Kind::ClassicLazy), slot.clone());
        HERE.with_borrow_mut(|h| *h = Some(sh.clone()));
        let s = src.clone();
        let finisher = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let out = parse(Kind::ClassicLazy, Rc::from(&*s));
            *slot.st.lock().unwrap() = St::Done(Hand(out));
            slot.ready.notify_all();
        });
        let t = std::time::Instant::now();
        parse_cache::clear();
        let mut it = realm();
        let r = it.run_script(&src);
        assert!(t.elapsed() >= Duration::from_millis(90), "did not wait");
        assert!(matches!(r, Ok(Value::Number(n)) if n == 42.0));
        assert_eq!(sh.taken.load(Ordering::Relaxed), 1);
        finisher.join().unwrap();
        HERE.with_borrow_mut(|h| *h = None);
    }

    #[test]
    fn the_tree_from_the_worker_is_the_only_owner_of_its_text() {
        let src = big("1 + 7");
        let a = Ahead::new();
        a.sender().submit(Kind::ClassicLazy, &src);
        a.attach();
        wait_for(|| a.began() == 1);
        let sh = a.0.clone();
        let slot = loop {
            let i = sh.inner.lock().unwrap();
            let s = i.slots.values().next().cloned().unwrap();
            drop(i);
            if matches!(&*s.st.lock().unwrap(), St::Done(_)) {
                break s;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        drop(slot);
        let (r, _) = take(Kind::ClassicLazy, parse_cache::hash(src.as_bytes()), &src).unwrap();
        let p = r.unwrap();
        assert!(matches!(&p.source, crate::ast::Src::Shared(s) if Rc::strong_count(s) == 1));
        HERE.with_borrow_mut(|h| *h = None);
    }

    #[test]
    fn leaving_the_page_drops_the_queued_parses_and_the_finished_trees() {
        let a = Ahead::new();
        parse_cache::clear();
        let texts: Vec<String> = (0..4).map(|i| long(&format!("{i}"))).collect();
        for t in &texts {
            a.sender().submit(Kind::ClassicLazy, t);
        }
        wait_for(|| a.began() >= 1);
        a.attach();
        let sh = a.0.clone();
        drop(a);
        // The thread ends after the parse that runs. The three that were queued are never parsed.
        wait_for(|| !sh.inner.lock().unwrap().thread);
        assert!(sh.began.load(Ordering::Relaxed) <= 2, "began {}", sh.began.load(Ordering::Relaxed));
        let i = sh.inner.lock().unwrap();
        assert!(i.slots.is_empty() && i.queue.is_empty());
        drop(i);
        // The page thread parses for itself now.
        let mut it = realm();
        assert!(matches!(it.run_script(&texts[3]), Ok(Value::Number(n)) if n == 3.0));
        assert_eq!(sh.taken.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn the_same_text_is_queued_once_and_a_short_text_never() {
        let a = Ahead::new();
        let s = a.sender();
        let src = big("1");
        s.submit(Kind::ClassicLazy, &src);
        s.submit(Kind::ClassicLazy, &src);
        s.submit(Kind::ClassicLazy, "var a = 1;");
        wait_for(|| a.began() >= 1);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(a.began(), 1);
    }

    #[test]
    fn a_text_that_a_cache_holds_is_not_parsed_ahead() {
        let src = big("3 + 9");
        parse_cache::clear();
        let mut it = realm();
        assert!(matches!(it.run_script(&src), Ok(Value::Number(n)) if n == 12.0));
        let a = Ahead::new();
        a.sender().submit(Kind::ClassicLazy, &src);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!((a.began(), a.running()), (0, false));
        // After the cache lets go of it, it is parsed ahead again.
        parse_cache::clear();
        a.sender().submit(Kind::ClassicLazy, &src);
        wait_for(|| a.began() == 1);
    }

    #[test]
    fn a_node_room_that_is_too_small_gives_the_error_of_a_fresh_parse() {
        let src = big("var a = [1, 2, 3];");
        let a = Ahead::new();
        parse_cache::clear();
        a.sender().submit(Kind::ClassicLazy, &src);
        a.attach();
        wait_for(|| a.began() == 1);
        let l = Limits { max_nodes: 3, ..Limits::default() };
        let want = crate::parse::parse_script(&src, l).map(|p| p.stats()).map_err(|e| e.kind);
        let got = parse_cache::parse(Kind::ClassicLazy, &src, l).map(|p| p.stats()).map_err(|e| e.kind);
        assert_eq!(got, want);
        HERE.with_borrow_mut(|h| *h = None);
    }
}
