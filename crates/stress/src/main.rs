//! Load tests for modules 1 to 3.
//!
//! - `scale`: run each case with 1x, 10x and 100x input. Each run is a new process, so a crash,
//!   a hang or a memory blowup in one case does not stop the others. Time that grows faster than
//!   the input (an exponent over 1.5) is flagged.
//! - `fuzz`: random and broken input into the parsers. A panic, a hang or a memory blowup saves
//!   the input to `crash-*.txt`.
//!
//! Usage: `cargo run --release -p bare-stress -- scale [filter]`
//!        `cargo run --release -p bare-stress -- fuzz <html|css|url> [iterations] [seed]`
//!
//! Comments compare Rust with JavaScript, because you know JS.

mod cases;
mod fuzz;
mod net_cases;

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};

/// A counting allocator, like the one in `bare-bench`. It also stops the process when the
/// program holds too much memory: a loop that never ends and keeps allocating must not freeze the machine.
struct Counting;
pub static LIVE: AtomicUsize = AtomicUsize::new(0);
pub static PEAK: AtomicUsize = AtomicUsize::new(0);

/// More than 2 GB alive is a blowup. Exit code 3 tells the parent.
const MEMORY_LIMIT: usize = 2 << 30;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        grow(l.size());
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        LIVE.fetch_sub(l.size(), Relaxed);
        grow(new);
        unsafe { System.realloc(p, l, new) }
    }
}

fn grow(n: usize) {
    let now = LIVE.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(now, Relaxed);
    if now > MEMORY_LIMIT {
        fuzz::save_current_input();
        eprintln!("memory limit: more than {} MB alive", MEMORY_LIMIT >> 20);
        std::process::exit(3);
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// A case: a name, the starting size, and a function that builds the input (not timed) and
/// returns the work (timed).
pub struct Case {
    pub name: &'static str,
    pub base: usize,
    pub build: fn(usize) -> Box<dyn FnOnce()>,
    /// Run only the base size (for cases where the size means nothing).
    pub single: bool,
}

/// How long one run may take before the parent kills it.
const RUN_LIMIT: Duration = Duration::from_secs(20);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("scale") => scale(args.get(1).map(String::as_str).unwrap_or("")),
        Some("run") => run_one(&args[1], args[2].parse().unwrap()),
        Some("fuzz") => fuzz::run(&args[1..]),
        _ => eprintln!("usage: scale [filter] | fuzz <html|css|url> [iterations] [seed]"),
    }
}

fn all_cases() -> Vec<Case> {
    let mut all = cases::all();
    all.extend(net_cases::all());
    all
}

/// The child process: build, time, and print `ms peak_bytes`.
fn run_one(name: &str, n: usize) {
    let case = all_cases().into_iter().find(|c| c.name == name).expect("no such case");
    let work = (case.build)(n);
    let start = LIVE.load(Relaxed);
    PEAK.store(start, Relaxed);
    let t = Instant::now();
    work();
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    println!("{ms} {}", PEAK.load(Relaxed) - start);
}

/// One run of a case in a new process.
enum Outcome {
    Done { ms: f64, peak: f64 },
    Timeout,
    Memory,
    Crash(String),
}

fn spawn(name: &str, n: usize) -> Outcome {
    let exe = std::env::current_exe().unwrap();
    let mut child = Command::new(exe).args(["run", name, &n.to_string()]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let (mut out, mut err) = (String::new(), String::new());
            child.stdout.take().unwrap().read_to_string(&mut out).ok();
            child.stderr.take().unwrap().read_to_string(&mut err).ok();
            if status.success() {
                let mut parts = out.split_whitespace().map(|p| p.parse::<f64>().unwrap_or(0.0));
                return Outcome::Done { ms: parts.next().unwrap_or(0.0), peak: parts.next().unwrap_or(0.0) / 1e6 };
            }
            if status.code() == Some(3) {
                return Outcome::Memory;
            }
            let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
            let how = match status.code() {
                Some(c) => format!("exit {c}"),
                None => "killed by a signal (stack overflow?)".to_string(),
            };
            return Outcome::Crash(format!("{how}: {last}"));
        }
        if start.elapsed() > RUN_LIMIT {
            child.kill().ok();
            child.wait().ok();
            return Outcome::Timeout;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The parent: run each case at 1x, 10x and 100x, print a table and flag what grows too fast.
fn scale(filter: &str) {
    println!("| case | n | ms | peak MB | note |");
    println!("| --- | ---: | ---: | ---: | --- |");
    let mut flagged = Vec::new();
    for case in all_cases().into_iter().filter(|c| c.name.contains(filter)) {
        let sizes: &[usize] = if case.single { &[1] } else { &[1, 10, 100] };
        let mut last: Option<(f64, f64)> = None;
        for &k in sizes {
            let n = case.base * k;
            match spawn(case.name, n) {
                Outcome::Done { ms, peak } => {
                    let mut note = String::new();
                    if let Some((pms, ppeak)) = last {
                        // The exponent: 10x input and 10x time is 1.0. 100x time is 2.0 (quadratic).
                        if pms > 5.0 {
                            let e = (ms / pms).log10();
                            note += &format!("time ^{e:.1}");
                            if e > 1.5 {
                                note += " SLOW GROWTH";
                                flagged.push(format!("{} time ^{e:.1} at n={n}", case.name));
                            }
                        }
                        if ppeak > 5.0 {
                            let e = (peak / ppeak).log10();
                            note += &format!(" mem ^{e:.1}");
                            if e > 1.5 {
                                note += " MEMORY GROWTH";
                                flagged.push(format!("{} memory ^{e:.1} at n={n}", case.name));
                            }
                        }
                    }
                    println!("| {} | {n} | {ms:.1} | {peak:.1} | {note} |", case.name);
                    last = Some((ms, peak));
                    if ms > 3000.0 {
                        break; // the next size would take 10x as long
                    }
                }
                Outcome::Timeout => {
                    println!("| {} | {n} | > {} s | | TIMEOUT |", case.name, RUN_LIMIT.as_secs());
                    flagged.push(format!("{} timeout at n={n}", case.name));
                    break;
                }
                Outcome::Memory => {
                    println!("| {} | {n} | | > {} MB | MEMORY LIMIT |", case.name, MEMORY_LIMIT >> 20);
                    flagged.push(format!("{} memory limit at n={n}", case.name));
                    break;
                }
                Outcome::Crash(why) => {
                    println!("| {} | {n} | | | CRASH {why} |", case.name);
                    flagged.push(format!("{} crash at n={n}", case.name));
                    break;
                }
            }
        }
    }
    println!("\nFlagged: {}", flagged.len());
    for f in flagged {
        println!("- {f}");
    }
}
