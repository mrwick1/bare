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

/// The real memory of this process in MB, from the kernel (`VmRSS`). Linux only.
fn rss_mb() -> f64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.trim().trim_end_matches(" kB").parse::<f64>().ok());
    kb.unwrap_or(0.0) / 1024.0
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
    if std::env::args().nth(1).as_deref() == Some("parse") {
        // Only the tree build, 40 times: a long run for `perf record`.
        let text = text.replace('\r', "");
        for _ in 0..40 {
            std::hint::black_box(bare_html::tree::parse(&text));
        }
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("rss") {
        // A fresh process, so that memory freed by earlier steps does not hide the cost.
        // The kernel's view of the DOM: how much the process grows when we build one tree.
        let bytes = text.into_bytes();
        let decoded = bare_html::encoding::decode(&bytes, Some("text/html; charset=utf-8"));
        let before = rss_mb();
        let doc = bare_html::tree::parse(&decoded);
        println!(
            "RSS grew by {:.1} MB for the DOM ({:.0} bytes for each of {} nodes)",
            rss_mb() - before,
            (rss_mb() - before) * 1048576.0 / 400_006.0,
            400_006
        );
        drop(doc);
        return;
    }
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

    // A style sheet of 100,000 rules, 4 properties each.
    let sheet: String = (0..100_000)
        .map(|i| {
            format!(
                ".c{} .d{}{{color:#{:06x};margin:{}px 2em;padding:{}.5%;font-size:1.{}rem}}\n",
                i % 100,
                i % 7,
                i * 37 % 0xffffff,
                i % 5,
                i % 3,
                i % 9
            )
        })
        .collect();
    println!("css: {} bytes", sheet.len());
    let tokens = step("css: tokenize", || {
        bare_css::tokenizer::tokenize(&sheet).count()
    });
    println!("css: {tokens} tokens");
    step("css: parse", || {
        bare_css::parser::parse_stylesheet(&sheet).len()
    });
    // The values of all declarations. `margin` has 4 longhands, so this makes 700,000 items of 12 bytes.
    step("css: parse + values", || {
        let mut out = Vec::new();
        for rule in bare_css::parser::parse_stylesheet(&sheet) {
            if let bare_css::parser::Rule::Style(rule) = rule {
                for d in &rule.declarations {
                    bare_css::values::parse_declaration(&d.name, d.value, d.important, &mut Default::default(), &mut out);
                }
            }
        }
        out.len()
    });

    let decoded = step("html: decode bytes", || {
        bare_html::encoding::decode(&bytes, Some("text/html; charset=utf-8"))
    });
    let tokens = step("html: tokenize", || {
        bare_html::tokenizer::tokenize(&decoded).count()
    });
    let mut doc = step("html: tree (with tokens)", || {
        bare_html::tree::parse(&decoded)
    });

    // The cascade. A sheet of 10,000 rules: 5,000 that use the classes of the page (a rule with 3 parts, so the
    // ancestor filter and the matcher have work to do. Each of them matches some of the spans), and 5,000 for classes that the page does not have.
    let author: String = (0..10_000)
        .map(|i| {
            if i % 2 == 0 {
                format!("body .c{}.d{} > .e{} {{ color: #{:06x}; margin-left: {}px }}\n", i / 2 % 100, i / 2 % 7, i / 2 % 13, i * 37 % 0xffffff, i % 9)
            } else {
                format!(".f{i} p {{ margin-top: 1px }}\n")
            }
        })
        .collect();
    let rules = step("css: add 10,000 rules", || {
        let mut rules = bare_css::cascade::RuleSet::new((1280.0, 800.0));
        rules.add(&mut doc, bare_css::cascade::UA_CSS, bare_css::cascade::Origin::UserAgent);
        rules.add(&mut doc, &author, bare_css::cascade::Origin::Author);
        rules
    });
    let styles = step("css: styles of 200,000 elements", || rules.compute(&doc));
    println!("{} rules, {} different styles", rules.len(), styles.unique());

    // A deep tree: 20,000 nested divs. The time must grow with the depth, not with its square.
    let deep = format!("{}x{}", "<div>".repeat(20_000), "</div>".repeat(20_000));
    step("html: tree, 20,000 nested divs", || bare_html::tree::parse(&deep));
    // Formatting tags in the wrong place: the adoption agency algorithm and the list of active formatting elements.
    let misnested = format!("{}x{}", "<b><div>".repeat(20_000), "</b>".repeat(20_000));
    step("html: tree, 20,000 misnested <b><div>", || bare_html::tree::parse(&misnested));
    let reopened = "<p><b><i>x</p>y".repeat(20_000);
    step("html: tree, 20,000 reopened <b><i>", || bare_html::tree::parse(&reopened));

    let mut nodes = 0;
    let mut stack = vec![doc.root()];
    while let Some(id) = stack.pop() {
        nodes += 1;
        stack.extend(doc.children(id));
    }
    println!("\n{tokens} tokens, {nodes} DOM nodes");
}
