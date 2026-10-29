//! Benchmarks: time and memory of each part of the browser, on one big generated page.
//!
//! Memory comes from a counting allocator. It wraps the system allocator and counts the bytes
//! that are alive. "peak" is the most bytes alive at once during the step, above the start.
//! "kept" is what is still alive when the step returns its result (for example the DOM).
//! This counts heap bytes that the program asked for. It does not count the kernel's pages,
//! so it is lower than the RSS that `top` shows. Chrome numbers in docs/resources.md are RSS.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

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
}

#[global_allocator]
static ALLOC: Counting = Counting;

const RUNS: usize = 5;

/// Run `f` five times. Print the median time, and the peak and kept memory of the last run.
fn step<T>(name: &str, mut f: impl FnMut() -> T) -> T {
    let mut times = Vec::new();
    let mut out = None;
    for _ in 0..RUNS {
        drop(out.take()); // free the last result, so each run starts from the same memory
        let start = LIVE.load(Relaxed);
        PEAK.store(start, Relaxed);
        let t = Instant::now();
        out = Some(f());
        times.push(t.elapsed());
        let (peak, kept) = (PEAK.load(Relaxed) - start, LIVE.load(Relaxed) - start);
        if times.len() == RUNS {
            times.sort();
            println!(
                "| {name:<22} | {:>8.1} | {:>9.1} | {:>9.1} |",
                times[RUNS / 2].as_secs_f64() * 1000.0,
                peak as f64 / 1e6,
                kept as f64 / 1e6
            );
        }
    }
    out.unwrap()
}

fn page(n: usize) -> String {
    let mut s = String::from("<!doctype html><meta charset=utf-8><body>");
    for i in 0..n {
        s += &format!(
            "<div class=\"c{} d{}\"><span class=\"e{}\">x</span></div>\n",
            i % 100,
            i % 7,
            i % 13
        );
    }
    s
}

fn main() {
    let text = page(100_000);
    let bytes = text.clone().into_bytes();
    println!(
        "page: {} bytes, 100000 divs and 100000 spans\n",
        bytes.len()
    );
    println!("| step                   |   ms     | peak MB   | kept MB   |");
    println!("| ---------------------- | -------- | --------- | --------- |");

    let urls: Vec<String> = (0..100_000)
        .map(|i| format!("http://example.com/a/b/c?q={i}#x"))
        .collect();
    let base = bare_url::Url::parse("http://example.com/a/b/").unwrap();
    step("url: parse x100000", || {
        urls.iter()
            .filter(|u| bare_url::Url::parse(u).is_ok())
            .count()
    });
    step("url: join x100000", || {
        (0..100_000)
            .filter(|i| base.join(&format!("../x{i}.css")).is_ok())
            .count()
    });

    let decoded = step("html: decode bytes", || {
        bare_html::encoding::decode(&bytes, Some("text/html; charset=utf-8"))
    });
    let tokens = step("html: tokenize", || {
        bare_html::tokenizer::tokenize(&decoded).count()
    });
    let doc = step("html: tree (with tokens)", || {
        bare_html::tree::parse(&decoded)
    });

    let mut nodes = 0;
    let mut stack = vec![doc.root()];
    while let Some(id) = stack.pop() {
        nodes += 1;
        stack.extend(doc.children(id));
    }
    println!("\n{tokens} tokens, {nodes} DOM nodes");
}
