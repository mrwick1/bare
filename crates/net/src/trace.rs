//! Timing of each request. Off unless the environment variable `BARE_NET_TRACE` is set.
//!
//! When it is off, a request costs one read of a `OnceLock` and no clock read. A caller prints the records with `dump`.
//! One record per request: the wait before it started, DNS, TCP connect, TLS handshake, time to the first byte (the
//! head of the reply), body time, and whether the connection was reused. The times are in microseconds.
//! Two counters give what the main thread did with nothing to run: `blocked` (inside a network call) and `slept`.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("BARE_NET_TRACE").is_some())
}

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// One request. Fields are in microseconds. `at` is the start, from the first trace call of the process.
#[derive(Default, Clone, Debug)]
pub struct Rec {
    pub url: String,
    pub at: u64,
    pub wait: u64,
    pub dns: u64,
    pub tcp: u64,
    pub tls: u64,
    pub ttfb: u64,
    pub body: u64,
    pub bytes: u64,
    pub reused: bool,
    pub status: u16,
}

thread_local! {
    static WANTED: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// The next request of this thread was wanted at `t`: the time between `t` and its start is its queue wait.
pub fn wanted(t: Instant) {
    if on() {
        WANTED.with(|w| w.set(Some(t)));
    }
}

/// A clock for one phase. `lap` gives the time since the last lap, and is zero when tracing is off.
pub struct Clock(Option<Instant>);

impl Clock {
    pub fn new() -> Clock {
        Clock(on().then(Instant::now))
    }
    pub fn lap(&mut self) -> u64 {
        match &mut self.0 {
            Some(t) => {
                let now = Instant::now();
                let us = now.duration_since(*t).as_micros() as u64;
                *t = now;
                us
            }
            None => 0,
        }
    }
}

impl Default for Clock {
    fn default() -> Self {
        Clock::new()
    }
}

/// A record that starts now (None when tracing is off). It takes the wait of `wanted`.
pub fn begin(url: &str) -> Option<Box<Rec>> {
    if !on() {
        return None;
    }
    let now = Instant::now();
    let wait = WANTED.with(|w| w.take()).or(*PAGE.lock().unwrap()).map_or(0, |t| now.saturating_duration_since(t).as_micros() as u64);
    Some(Box::new(Rec { url: url.to_string(), at: now.duration_since(epoch()).as_micros() as u64, wait, ..Rec::default() }))
}

static PAGE: Mutex<Option<Instant>> = Mutex::new(None);

/// A page starts now: a request with no `wanted` time counts its wait from here (a script that the parser found).
pub fn page_start() {
    if on() {
        *PAGE.lock().unwrap() = Some(Instant::now());
    }
}

static MARKS: Mutex<Vec<(u64, u64, String)>> = Mutex::new(Vec::new());

/// A named point on the timeline, with the time that the step before it took (`us`). For page code: scripts, parsing.
pub fn mark(name: impl FnOnce() -> String, us: u64) {
    if on() {
        MARKS.lock().unwrap().push((Instant::now().duration_since(epoch()).as_micros() as u64, us, name()));
    }
}

static RECS: Mutex<Vec<Rec>> = Mutex::new(Vec::new());
static BLOCKED: AtomicU64 = AtomicU64::new(0);
static SLEPT: AtomicU64 = AtomicU64::new(0);

pub fn push(rec: Rec) {
    RECS.lock().unwrap().push(rec);
}

/// Run `f` and count its time as blocked in the network. Call it on the thread that would otherwise run page code.
pub fn blocked<T>(f: impl FnOnce() -> T) -> T {
    if !on() {
        return f();
    }
    let t = Instant::now();
    let out = f();
    BLOCKED.fetch_add(t.elapsed().as_micros() as u64, Ordering::Relaxed);
    out
}

/// Count a sleep of the main thread (it waited for a timer).
pub fn slept(d: Duration) {
    if on() {
        SLEPT.fetch_add(d.as_micros() as u64, Ordering::Relaxed);
    }
}

/// Print the records and the two counters to stderr, one tab separated line each, and clear them. Does nothing when off.
pub fn dump() {
    if !on() {
        return;
    }
    let mut recs = std::mem::take(&mut *RECS.lock().unwrap());
    recs.sort_by_key(|r| r.at);
    eprintln!("net\tat_us\twait_us\tdns_us\ttcp_us\ttls_us\tttfb_us\tbody_us\tbytes\treused\tstatus\turl");
    for r in recs {
        eprintln!("net\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}", r.at, r.wait, r.dns, r.tcp, r.tls, r.ttfb, r.body, r.bytes, r.reused as u8, r.status, r.url);
    }
    for (at, us, name) in std::mem::take(&mut *MARKS.lock().unwrap()) {
        eprintln!("mark\t{at}\t{us}\t{name}");
    }
    eprintln!("blocked_us\t{}\nslept_us\t{}", BLOCKED.swap(0, Ordering::Relaxed), SLEPT.swap(0, Ordering::Relaxed));
}
