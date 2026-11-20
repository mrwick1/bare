// Tests of `parse_ahead.rs`. The job is a fake parse: it counts its runs, and it can block until the test opens a gate, so a test knows
// when a job is queued, running or done without a sleep. Many tests run threads: the CI runs them as they are.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Instant;

#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    cv: Condvar,
}

impl Gate {
    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

/// A fake parse. `Out` is the length of the text and the mode.
struct Fake {
    runs: AtomicUsize,
    /// The count of runs that began, with a signal for `wait_runs`.
    began: (Mutex<usize>, Condvar),
    /// Each run blocks until the gate is open.
    gate: Option<Arc<Gate>>,
    cached: Mutex<Vec<(Identity, u8)>>,
    panic_on: Option<u8>,
}

impl Fake {
    fn new(gate: Option<Arc<Gate>>) -> Fake {
        Fake { runs: AtomicUsize::new(0), began: (Mutex::new(0), Condvar::new()), gate, cached: Mutex::new(Vec::new()), panic_on: None }
    }
}

impl Job for Arc<Fake> {
    type Mode = u8;
    type Out = (usize, u8);
    const PARK: Duration = Duration::from_millis(20);

    fn cached(&self, mode: u8, id: Identity) -> bool {
        self.cached.lock().unwrap().contains(&(id, mode))
    }

    fn run(&self, mode: u8, id: Identity, text: &Arc<str>) -> (usize, u8) {
        self.runs.fetch_add(1, Ordering::SeqCst);
        *self.began.0.lock().unwrap() += 1;
        self.began.1.notify_all();
        if let Some(g) = &self.gate {
            let mut o = g.open.lock().unwrap();
            while !*o {
                o = g.cv.wait(o).unwrap();
            }
        }
        assert_eq!(id, Identity::of(text), "the identity is carried, not changed");
        if self.panic_on == Some(mode) {
            panic!("fake parse error");
        }
        (text.len(), mode)
    }
}

impl Fake {
    fn runs(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }

    /// Wait until `n` runs have begun.
    fn wait_runs(&self, n: usize) {
        let t = Instant::now();
        let mut b = self.began.0.lock().unwrap();
        while *b < n {
            let (g, _) = self.began.1.wait_timeout(b, Duration::from_millis(50)).unwrap();
            b = g;
            assert!(t.elapsed() < Duration::from_secs(30), "timed out");
        }
    }
}

fn wait_for(f: impl Fn() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(30), "timed out");
        thread::sleep(Duration::from_millis(1));
    }
}

/// A text of `n` bytes. `tag` makes it differ from the others.
fn text(tag: usize, n: usize) -> String {
    let mut s = format!("//{tag}\n");
    s.push_str(&"x".repeat(n - s.len()));
    s
}

fn ahead(job: &Arc<Fake>) -> Ahead<Arc<Fake>> {
    Ahead::new(job.clone(), 16, 1 << 20)
}

fn submit(a: &Ahead<Arc<Fake>>, mode: u8, t: &str) -> Option<Pending<Arc<Fake>>> {
    a.sender().submit(mode, Identity::of(t), t)
}

#[test]
fn the_identity_is_of_the_bytes_and_the_length() {
    let (a, b) = (text(1, 100), text(2, 100));
    assert_eq!(Identity::of(&a), Identity::of(&a.clone()));
    assert_ne!(Identity::of(&a), Identity::of(&b));
    assert_ne!(Identity::of(&a), Identity::of(&text(1, 101)));
    assert_eq!(Identity::of(&a).len(), 100);
    // Every length of the tail (the 8-byte steps and the rest) gives its own value.
    let ids: std::collections::HashSet<_> = (1..40).map(|n| Identity::of(&"a".repeat(n))).collect();
    assert_eq!(ids.len(), 39);
}

#[test]
fn a_submit_gives_the_result_with_the_identity_and_the_text() {
    let job = Arc::new(Fake::new(None));
    let a = ahead(&job);
    let t = text(1, 200);
    let p = submit(&a, 3, &t).unwrap();
    let r = p.wait().unwrap();
    assert_eq!((r.out, r.id), ((200, 3), Identity::of(&t)));
    assert!(r.is(&t) && !r.is(&text(2, 200)));
    assert_eq!(p.id(), r.id);
    assert_eq!(job.runs(), 1);
}

#[test]
fn a_short_text_a_long_text_and_a_cached_key_start_nothing() {
    let job = Arc::new(Fake::new(None));
    let a = Ahead::new(job.clone(), 100, 1000);
    let s = a.sender();
    for t in [text(1, 99), text(2, 1001)] {
        assert!(s.submit(0, Identity::of(&t), &t).is_none());
    }
    let t = text(3, 500);
    job.cached.lock().unwrap().push((Identity::of(&t), 0));
    assert!(s.submit(0, Identity::of(&t), &t).is_none());
    // The same text in another mode is not cached.
    assert!(s.submit(1, Identity::of(&t), &t).unwrap().wait().is_some());
    assert_eq!((job.runs(), a.stats().claims), (1, 1));
}

#[test]
fn a_duplicate_submit_while_running_gets_the_same_entry_and_the_text_is_copied_once() {
    let gate = Arc::new(Gate::default());
    let job = Arc::new(Fake::new(Some(gate.clone())));
    let a = ahead(&job);
    let t = text(1, 5000);
    let p1 = submit(&a, 0, &t).unwrap();
    job.wait_runs(1);
    let dups: Vec<_> = (0..10).map(|_| submit(&a, 0, &t).unwrap()).collect();
    assert_eq!((a.stats().claims, a.stats().entries), (1, 1));
    gate.open();
    let r1 = p1.wait().unwrap();
    for d in &dups {
        assert!(Arc::ptr_eq(&r1, &d.wait().unwrap()));
    }
    assert_eq!((job.runs(), a.stats().claims, a.stats().published), (1, 1, 1));
}

#[test]
fn a_duplicate_submit_while_queued_gets_the_same_entry() {
    let gate = Arc::new(Gate::default());
    let job = Arc::new(Fake::new(Some(gate.clone())));
    let a = ahead(&job);
    let (first, t) = (text(1, 300), text(2, 300));
    let _p0 = submit(&a, 0, &first).unwrap();
    job.wait_runs(1);
    // The worker is busy: `t` stays queued.
    let (p1, p2) = (submit(&a, 0, &t).unwrap(), submit(&a, 0, &t).unwrap());
    assert_eq!((a.stats().queued, a.stats().claims), (1, 2));
    gate.open();
    assert!(Arc::ptr_eq(&p1.wait().unwrap(), &p2.wait().unwrap()));
    assert_eq!(job.runs(), 2);
}

#[test]
fn the_same_text_in_another_mode_is_another_entry() {
    let job = Arc::new(Fake::new(None));
    let a = ahead(&job);
    let t = text(1, 300);
    let (p0, p1, p0b) = (submit(&a, 0, &t).unwrap(), submit(&a, 1, &t).unwrap(), submit(&a, 0, &t).unwrap());
    let (r0, r1) = (p0.wait().unwrap(), p1.wait().unwrap());
    assert_eq!((r0.out.1, r1.out.1), (0, 1));
    assert_eq!(r0.id, r1.id);
    assert!(Arc::ptr_eq(&r0, &p0b.wait().unwrap()));
    assert_eq!((job.runs(), a.stats().claims), (2, 2));
}

#[test]
fn a_submit_after_the_publish_finds_the_result_while_a_handle_is_held() {
    let job = Arc::new(Fake::new(None));
    let a = ahead(&job);
    let t = text(1, 300);
    let p1 = submit(&a, 0, &t).unwrap();
    let r1 = p1.wait().unwrap();
    wait_for(|| a.stats().published == 1);
    let p2 = submit(&a, 0, &t).unwrap();
    assert!(Arc::ptr_eq(&r1, &p2.take().unwrap()));
    assert_eq!((job.runs(), a.stats().claims), (1, 1));
    // The entry leaves with the last handle. The cache of the caller serves the later duplicates (`Job::cached`).
    drop((p1, p2));
    assert_eq!(a.stats().entries, 0);
    job.cached.lock().unwrap().push((Identity::of(&t), 0));
    assert!(submit(&a, 0, &t).is_none());
    assert_eq!(job.runs(), 1);
}

#[test]
fn many_threads_that_submit_the_same_source_cause_one_parse() {
    for round in 0..20 {
        let job = Arc::new(Fake::new(None));
        let a = ahead(&job);
        let t = Arc::new(text(round, 20_000));
        let id = Identity::of(&t);
        let start = Arc::new(std::sync::Barrier::new(16));
        let hs: Vec<_> = (0..16)
            .map(|i| {
                let (s, t, start) = (a.sender(), t.clone(), start.clone());
                thread::spawn(move || {
                    start.wait();
                    // Two modes, and a few submits from each thread.
                    let ps: Vec<_> = (0..5).map(|k| s.submit(((i + k) % 2) as u8, id, &t).unwrap()).collect();
                    let rs: Vec<_> = ps.iter().map(|p| p.wait().unwrap()).collect();
                    // The handles stay until every thread is done: an entry leaves with its last waiter.
                    (ps, rs)
                })
            })
            .collect();
        let (handles, all): (Vec<_>, Vec<_>) = hs.into_iter().map(|h| h.join().unwrap()).unzip();
        let all: Vec<_> = all.into_iter().flatten().collect();
        assert_eq!(all.len(), 80);
        // One parse for each mode, and one result for each mode.
        assert_eq!((job.runs(), a.stats().claims), (2, 2), "round {round}");
        for r in &all {
            let mode0 = all.iter().find(|x| x.out.1 == r.out.1).unwrap();
            assert!(Arc::ptr_eq(r, mode0));
        }
        drop((all, handles));
        assert_eq!(a.stats().entries, 0);
    }
}

#[test]
fn the_last_waiter_that_goes_drops_the_queued_job() {
    let gate = Arc::new(Gate::default());
    let job = Arc::new(Fake::new(Some(gate.clone())));
    let a = ahead(&job);
    let (first, t, last) = (text(1, 300), text(2, 300), text(3, 300));
    let p0 = submit(&a, 0, &first).unwrap();
    job.wait_runs(1);
    let (p1, p2) = (submit(&a, 0, &t).unwrap(), submit(&a, 0, &t).unwrap());
    let p1b = p1.clone();
    drop(p1);
    drop(p2);
    // One waiter is left: the job stays queued.
    assert_eq!((a.stats().queued, a.stats().entries), (1, 2));
    drop(p1b);
    assert_eq!((a.stats().queued, a.stats().entries), (0, 1));
    let pl = submit(&a, 0, &last).unwrap();
    gate.open();
    assert!(p0.wait().is_some() && pl.wait().is_some());
    // Order: first, then last. The dropped text never ran.
    assert_eq!(job.runs(), 2);
    drop((p0, pl));
    assert_eq!(a.stats().entries, 0);
    assert_eq!(a.stats().began, 2);
}

#[test]
fn a_take_of_queued_work_cancels_it_and_the_worker_skips_it() {
    let gate = Arc::new(Gate::default());
    let job = Arc::new(Fake::new(Some(gate.clone())));
    let a = ahead(&job);
    let (first, t) = (text(1, 300), text(2, 300));
    let p0 = submit(&a, 0, &first).unwrap();
    job.wait_runs(1);
    let (p1, p2) = (submit(&a, 0, &t).unwrap(), submit(&a, 0, &t).unwrap());
    // The page thread wants it now and the worker has not begun: it parses itself, and nobody else gets a result either.
    assert!(p1.take().is_none());
    assert_eq!((a.stats().queued, a.stats().entries), (0, 1));
    assert!(p2.take().is_none() && p2.wait().is_none());
    gate.open();
    assert!(p0.wait().is_some());
    assert_eq!(job.runs(), 1);
    // A new submit of the same key is a new claim, with a new entry that the old handles do not touch.
    let p3 = submit(&a, 0, &t).unwrap();
    drop((p1, p2));
    assert_eq!(p3.wait().unwrap().out, (300, 0));
    assert_eq!(job.runs(), 2);
}

#[test]
fn a_take_of_queued_work_wakes_a_waiter_of_the_same_entry() {
    let gate = Arc::new(Gate::default());
    let job = Arc::new(Fake::new(Some(gate.clone())));
    let a = ahead(&job);
    let p0 = submit(&a, 0, &text(1, 300)).unwrap();
    job.wait_runs(1);
    let t = text(2, 300);
    let (p1, p2) = (submit(&a, 0, &t).unwrap(), submit(&a, 0, &t).unwrap());
    let waiter = thread::spawn(move || p2.wait().is_none());
    // The waiter blocks in `wait` on the queued entry (it may not have begun yet: both orders end the same way).
    assert!(p1.take().is_none());
    assert!(waiter.join().unwrap());
    gate.open();
    assert!(p0.wait().is_some());
}

#[test]
fn a_take_of_running_work_waits_for_it() {
    let gate = Arc::new(Gate::default());
    let job = Arc::new(Fake::new(Some(gate.clone())));
    let a = ahead(&job);
    let t = text(1, 300);
    let p = submit(&a, 0, &t).unwrap();
    job.wait_runs(1);
    let opener = thread::spawn({
        let gate = gate.clone();
        move || {
            thread::sleep(Duration::from_millis(50));
            gate.open();
        }
    });
    let at = Instant::now();
    assert_eq!(p.take().unwrap().out, (300, 0));
    assert!(at.elapsed() >= Duration::from_millis(40), "did not wait");
    opener.join().unwrap();
}

#[test]
fn the_last_waiter_that_goes_makes_a_running_result_unpublished() {
    let gate = Arc::new(Gate::default());
    let job = Arc::new(Fake::new(Some(gate.clone())));
    let a = ahead(&job);
    let t = text(1, 300);
    let p = submit(&a, 0, &t).unwrap();
    job.wait_runs(1);
    drop(p);
    // The job runs on, and its entry stays for now.
    assert_eq!(a.stats().entries, 1);
    gate.open();
    wait_for(|| a.stats().unpublished == 1);
    assert_eq!((a.stats().entries, a.stats().published, job.runs()), (0, 0, 1));
}

#[test]
fn a_waiter_that_comes_back_to_a_running_job_gets_its_result() {
    let gate = Arc::new(Gate::default());
    let job = Arc::new(Fake::new(Some(gate.clone())));
    let a = ahead(&job);
    let t = text(1, 300);
    let p0 = submit(&a, 0, &t).unwrap();
    job.wait_runs(1);
    drop(p0);
    let p = submit(&a, 0, &t).unwrap();
    gate.open();
    assert_eq!(p.wait().unwrap().out, (300, 0));
    assert_eq!((job.runs(), a.stats().claims, a.stats().published), (1, 1, 1));
}

#[test]
fn the_end_of_the_page_drops_queued_work_and_unpublishes_the_running_job() {
    let gate = Arc::new(Gate::default());
    let job = Arc::new(Fake::new(Some(gate.clone())));
    let a = ahead(&job);
    let ps: Vec<_> = (0..6).map(|i| submit(&a, 0, &text(i, 300)).unwrap()).collect();
    job.wait_runs(1);
    assert_eq!((a.stats().queued, a.stats().entries), (5, 6));
    let waiter = {
        let p = ps[0].clone();
        thread::spawn(move || p.wait().is_none())
    };
    a.cancel();
    assert_eq!((a.stats().queued, a.stats().entries), (0, 0));
    // A blocked waiter wakes with `None`, and a later submit starts nothing.
    assert!(waiter.join().unwrap());
    assert!(submit(&a, 0, &text(99, 300)).is_none());
    gate.open();
    wait_for(|| !a.stats().thread);
    // Only the job that ran was ever begun, and its result went nowhere.
    assert_eq!((job.runs(), a.stats().published, a.stats().unpublished), (1, 0, 1));
    assert!(ps.iter().all(|p| p.take().is_none()));
    drop(ps);
    assert_eq!(a.stats().entries, 0);
}

#[test]
fn dropping_the_owner_ends_the_work_and_leaves_no_entry() {
    let gate = Arc::new(Gate::default());
    let job = Arc::new(Fake::new(Some(gate.clone())));
    let a = ahead(&job);
    let s = a.sender();
    let p = submit(&a, 0, &text(1, 300)).unwrap();
    job.wait_runs(1);
    drop(a);
    assert!(s.submit(0, Identity::of(&text(2, 300)), &text(2, 300)).is_none());
    gate.open();
    assert!(p.wait().is_none());
    // The worker has dropped its result; the table is empty.
    wait_for(|| !p.sh.lock().thread);
    let g = p.sh.lock();
    assert!(g.map.is_empty() && g.queue.is_empty());
}

#[test]
fn no_entry_is_left_after_a_mix_of_cancels() {
    let gate = Arc::new(Gate::default());
    let job = Arc::new(Fake::new(Some(gate.clone())));
    let a = ahead(&job);
    let mut keep = Vec::new();
    // A `take` of the job that runs waits for it, so the gate opens from another thread while the loop runs.
    let opener = thread::spawn({
        let gate = gate.clone();
        move || {
            thread::sleep(Duration::from_millis(3));
            gate.open();
        }
    });
    for i in 0..40 {
        let t = text(i, 400);
        let p = submit(&a, (i % 3) as u8, &t).unwrap();
        match i % 4 {
            0 => drop(p),
            1 => {
                let _ = p.take();
            }
            2 => keep.push(p),
            _ => {
                let q = p.clone();
                drop(p);
                keep.push(q);
            }
        }
    }
    opener.join().unwrap();
    for p in &keep {
        let _ = p.wait();
    }
    drop(keep);
    wait_for(|| {
        let s = a.stats();
        s.entries == 0 && s.queued == 0
    });
}

#[test]
fn a_job_that_panics_gives_no_result_and_the_worker_goes_on() {
    let mut fake = Fake::new(None);
    fake.panic_on = Some(9);
    let job = Arc::new(fake);
    let a = ahead(&job);
    let bad = submit(&a, 9, &text(1, 300)).unwrap();
    let good = submit(&a, 0, &text(2, 300)).unwrap();
    assert!(bad.wait().is_none());
    assert_eq!(good.wait().unwrap().out, (300, 0));
    assert_eq!((a.stats().published, a.stats().unpublished), (1, 1));
    drop((bad, good));
    assert_eq!(a.stats().entries, 0);
}

#[test]
fn the_worker_thread_ends_when_idle_and_starts_again() {
    let job = Arc::new(Fake::new(None));
    let a = ahead(&job);
    assert!(!a.stats().thread);
    let p = submit(&a, 0, &text(1, 300)).unwrap();
    assert!(p.wait().is_some());
    wait_for(|| !a.stats().thread);
    let q = submit(&a, 0, &text(2, 300)).unwrap();
    assert!(q.wait().is_some());
    assert_eq!(job.runs(), 2);
}

#[test]
fn threads_that_submit_take_and_drop_at_once_leave_no_entry() {
    // The copy of a big text takes a while: other threads join, take and drop the entry while it is `Claimed`.
    let job = Arc::new(Fake::new(None));
    let a = Ahead::new(job.clone(), 16, 64 << 20);
    let t = Arc::new(text(1, 4 << 20));
    let id = Identity::of(&t);
    let hs: Vec<_> = (0..8)
        .map(|n| {
            let (s, t) = (a.sender(), t.clone());
            thread::spawn(move || {
                for i in 0..60 {
                    let Some(p) = s.submit((i % 2) as u8, id, &t) else { continue };
                    match (i + n) % 3 {
                        0 => assert!(p.take().is_none_or(|r| r.is(&t))),
                        1 => drop(p),
                        _ => assert!(p.wait().is_none_or(|r| r.is(&t))),
                    }
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    wait_for(|| {
        let s = a.stats();
        s.entries == 0 && s.queued == 0
    });
    // Each claim made one copy and at most one parse: a parse needs a claim.
    assert!(job.runs() <= a.stats().claims);
}
