//! A virtual clock for benchmarks that replay recorded replies (`replay.rs`), so that a journey does the same work in every run.
//!
//! The problem. Replies come from a tape, but a page still races the real clock. A reply is ready when a worker thread has read the tape
//! and decoded the body; a step of the driver ends when its timers and its sleeps say so. Which of the two comes first changes with the
//! load of the machine. One example: the search index of the Rust book loads while the driver types 9 keys. A run in which it arrives
//! after key 3 searches 6 times, another one searches once. Instructions differ by a factor of 4.
//!
//! The design (it is the idea of the "virtual time budget" of Chrome, `Emulation.setVirtualTimePolicy`, with one change: the replies
//! have a time of their own, and Chrome's policy waits for them without one).
//! - `BARE_CLOCK=virtual` and a tape that reads (`BARE_NET_REPLAY` or `BARE_NET_FILL`): the clock is on. Nothing else turns it on.
//! - Time is a number of milliseconds that only this module moves. `performance.now`, `Date.now`, timers, animation frames and idle
//!   callbacks of the page read it (`browser_scheduler.rs`). Nothing sleeps for it.
//! - A request that a worker runs is a `parallel::Task`. It gets a due time when it starts: now + `REPLY_MS`. The page sees its result
//!   (`Task::done`) only when the clock has reached the due time. Real time that the worker takes does not matter.
//! - The clock moves in three cases. (1) The page or the driver is idle and waits: it jumps to the next event, a timer of the page or
//!   the due time of a task. (2) A task of the scheduler runs, or an action of the driver runs: a fixed cost (`TASK_MS`, `ACTION_MS`).
//!   (3) Page code reads the time: a cost of `READ_MS`, so that a busy loop `while (Date.now() < t)` ends.
//! - Moving to a time `t` first waits, in real time, for every task that is due at `t` or before. So a task that is due has a result.
//!   This is the only place where real and virtual time meet.
//!
//! What is not virtual: the time limits of a step, a script and a load (they stop a hang, they do not make a result), and the TLS and
//! cookie clocks. The clock is one for the process: one journey runs in one process.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

/// The time that a reply takes, for every request.
pub const REPLY_MS: f64 = 5.0;
/// The time that a task of the page scheduler takes.
pub const TASK_MS: f64 = 0.1;
/// The time that a step of the driver takes: the round trip of an input event.
pub const ACTION_MS: f64 = 1.0;
/// The time that a read of `Date.now()` or `performance.now()` takes.
pub const READ_MS: f64 = 0.001;
/// The start of the clock for `Date.now()`: 2026-11-13T00:00:00Z, the day of the first recording. `BARE_CLOCK_EPOCH=<ms>` changes it.
const EPOCH_MS: f64 = 1_794_528_000_000.0;

/// What `advance_to` waits for: the end of a job on a worker thread.
pub(crate) trait Ended: Send + Sync {
    fn wait_ended(&self);
}

struct State {
    now: f64,
    seq: u64,
    /// Tasks that are not due yet, by (due time, order of start). Every key is later than `now`.
    pending: BTreeMap<(u64, u64), Weak<dyn Ended>>,
}

static STATE: Mutex<State> = Mutex::new(State { now: 0.0, seq: 0, pending: BTreeMap::new() });

/// Is the clock on? It is, for the whole process, when `BARE_CLOCK=virtual` is set and the tape of the process answers requests.
pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("BARE_CLOCK").is_ok_and(|v| v == "virtual") && crate::replay::Tape::from_env().is_some_and(|t| t.reads()))
}

/// Milliseconds since the start of the process on the virtual clock.
pub fn now() -> f64 {
    STATE.lock().unwrap().now
}

/// Now as an `Instant`, for the code that waits until a time (`Page::nap`, the limits of a step): the start of the process plus the virtual time.
/// With the clock off it is `Instant::now()`. Do not compare it with an `Instant` from another source, or take `elapsed()` of it.
pub fn instant() -> Instant {
    if !on() {
        return Instant::now();
    }
    base() + Duration::from_secs_f64(now() / 1000.0)
}

/// The time of `t`, an `Instant` from `instant()`, on the virtual clock in ms.
pub fn ms_of(t: Instant) -> f64 {
    t.saturating_duration_since(base()).as_secs_f64() * 1000.0
}

/// The time since `t`, an `Instant` from `instant()`.
pub fn since(t: Instant) -> Duration {
    instant().saturating_duration_since(t)
}

fn base() -> Instant {
    static BASE: OnceLock<Instant> = OnceLock::new();
    *BASE.get_or_init(Instant::now)
}

/// `Date.now()`: the start of the clock plus the virtual time, in whole milliseconds.
pub fn epoch_ms() -> f64 {
    static EPOCH: OnceLock<f64> = OnceLock::new();
    let base = *EPOCH.get_or_init(|| std::env::var("BARE_CLOCK_EPOCH").ok().and_then(|v| v.parse().ok()).unwrap_or(EPOCH_MS));
    (base + now()).floor()
}

/// Give a task that starts now its due time, and take note of it. `None` when the clock is off.
pub(crate) fn start(task: Weak<dyn Ended>) -> Option<f64> {
    if !on() {
        return None;
    }
    let mut s = STATE.lock().unwrap();
    let due = s.now + REPLY_MS;
    s.seq += 1;
    let key = (due.to_bits(), s.seq);
    s.pending.insert(key, task);
    Some(due)
}

/// Move the clock to `t` (it never goes back). First wait for every task that is due at `t` or before.
pub fn advance_to(t: f64) {
    loop {
        let next = {
            let mut s = STATE.lock().unwrap();
            let Some((&key, _)) = s.pending.first_key_value() else { break };
            if f64::from_bits(key.0) > t {
                break;
            }
            s.pending.remove(&key)
        };
        // The lock is free while the page thread waits: a worker may start a request meanwhile (it does not touch the clock).
        if let Some(task) = next.and_then(|w| w.upgrade()) {
            task.wait_ended();
        }
    }
    let mut s = STATE.lock().unwrap();
    s.now = s.now.max(t);
}

/// `advance_to(now + d)`.
pub fn advance_by(d: f64) {
    advance_to(now() + d);
}

/// The due time of the first task that the page cannot see yet.
pub fn next_task_due() -> Option<f64> {
    let mut s = STATE.lock().unwrap();
    while let Some((&key, task)) = s.pending.first_key_value() {
        if task.strong_count() > 0 {
            return Some(f64::from_bits(key.0));
        }
        s.pending.remove(&key);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Job(Mutex<bool>, std::sync::Condvar);
    impl Ended for Job {
        fn wait_ended(&self) {
            let mut done = self.0.lock().unwrap();
            while !*done {
                done = self.1.wait(done).unwrap();
            }
        }
    }

    // One test: the clock is global, and the tests of a crate run together.
    #[test]
    fn the_clock_moves_by_events_and_waits_for_the_tasks_that_are_due() {
        let job = Arc::new(Job(Mutex::new(false), std::sync::Condvar::new()));
        let weak: Weak<dyn Ended> = Arc::downgrade(&(job.clone() as Arc<dyn Ended>));
        let t0 = now();
        // The clock is off in a test (no environment): `start` gives no due time, so this test adds the task by hand.
        let due = t0 + REPLY_MS;
        STATE.lock().unwrap().pending.insert((due.to_bits(), u64::MAX), weak);
        assert_eq!(next_task_due(), Some(due));
        // A move to a time before the due time does not wait.
        advance_to(t0 + 1.0);
        assert_eq!(now(), t0 + 1.0);
        // A move to the due time waits for the job, which ends on another thread.
        let ender = job.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            *ender.0.lock().unwrap() = true;
            ender.1.notify_all();
        });
        advance_to(due);
        assert!(*job.0.lock().unwrap(), "the move returned before the job ended");
        thread.join().unwrap();
        assert_eq!((now(), next_task_due()), (due, None));
        // Time does not go back.
        advance_to(t0);
        assert_eq!(now(), due);
        advance_by(0.5);
        assert_eq!(now(), due + 0.5);
    }
}
