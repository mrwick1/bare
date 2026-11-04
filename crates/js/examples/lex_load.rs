//! Load harness for the lexer (S2). One case per process, in release mode:
//!
//!   cargo run --release -p bare-js --example lex_load -- <case> <scale> [runs] [decode]
//!
//! Cases (scale 1, 10 or 100; the source is scale x 512 KiB, so 100x is 50 MiB, under the 64 MiB cap):
//!   small      many short tokens: names, operators, numbers, strings, `?.`
//!   repeat     the same literals again and again: regexp, hex and decimal numbers, #private, escaped name, a template with
//!              two `${}` (the harness plays the parser: it gives the regexp and template goals)
//!   comments   2 KiB of `//` and `/* */` comments (with line breaks) between tokens
//!   strings    4 KiB plain strings and 1.5 KiB strings full of escapes. With `decode` the values are decoded too.
//!   malformed  many short bad sources (one error each), then a big valid source with a bad string at its end
//!
//! The source and the expected token list are built by construction (not by the lexer). The timed loop only lexes. Any wrong
//! token count, span checksum, error kind or error offset exits with 1: a failure is never a timing. Scanning must not
//! allocate: the harness checks that the timed loop makes 0 allocations (with `decode`, only the one reused buffer grows).
//! The last line is `key=value`. RSS is `VmHWM` of the whole process, source included.

use bare_js::lex::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.realloc(p, l, new) }
    }
}
#[global_allocator]
static A: Counting = Counting;

fn fail(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1)
}

fn status_kib(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok()).unwrap_or(0)
}

/// One expected token: the text before it, its class letter (as in `lex_check`), its goal, its text.
struct Tok {
    sep: String,
    class: u8,
    goal: Goal,
    text: String,
}

fn t(sep: &str, class: u8, goal: Goal, text: impl Into<String>) -> Tok {
    Tok { sep: sep.into(), class, goal, text: text.into() }
}

fn class_of(k: Kind) -> u8 {
    match k {
        Kind::Ident | Kind::Keyword(_) => b'i',
        Kind::PrivateName => b'p',
        Kind::Number(_) | Kind::BigInt(_) => b'n',
        Kind::String { .. } => b's',
        Kind::RegExp => b'r',
        Kind::Template { .. } => b't',
        Kind::Punct(_) => b'x',
        Kind::Eof => b'e',
    }
}

fn mix(h: u64, class: u8, start: u32, end: u32) -> u64 {
    [class as u64, start as u64, end as u64].iter().fold(h, |h, &x| (h ^ x).wrapping_mul(0x100000001b3))
}
const FNV0: u64 = 0xcbf29ce484222325;

fn unit(case: &str) -> Vec<Tok> {
    use Goal::*;
    let bs = '\\';
    match case {
        "small" => vec![
            t("", b'i', Div, "a"), t("", b'x', Div, "="), t(" ", b'i', Div, "b"), t(" ", b'x', Div, "+"), t(" ", b'n', Div, "12"),
            t("", b'x', Div, ";"), t("\n", b'i', Div, "f"), t("", b'x', Div, "("), t("", b'i', Div, "x"), t("", b'x', Div, ","),
            t(" ", b's', Div, "\"s\""), t("", b'x', Div, ")"), t("", b'x', Div, "?."), t("", b'i', Div, "y"), t("", b'x', Div, ";"),
            t("\n", b'i', Div, "if"), t(" ", b'x', Div, "("), t("", b'i', Div, "q"), t(" ", b'x', Div, ">>>="), t(" ", b'n', Div, "1"), t("", b'x', Div, ")"),
        ],
        "repeat" => vec![
            t("", b'r', RegExp, "/ab+c[/]d/gi"), t(" ", b'x', Div, "+"), t("", b'n', Div, "0x1F_FF"), t("", b'x', Div, ","),
            t(" ", b'n', Div, "1_000.5e3"), t(" ", b'n', Div, ".5"), t(" ", b'p', Div, "#priv"), t(" ", b'i', Div, format!("{bs}u0061bc")),
            t(" ", b't', Div, "`t${"), t("", b'i', Div, "x"), t("", b't', TemplateTail, "}u${"), t("", b'i', Div, "y"),
            t("", b't', TemplateTail, "}v`"), t("\n", b'x', Div, ";"),
        ],
        "comments" => vec![
            t(&format!("//{}\n/*{}\n*/ ", "c".repeat(2000), "m".repeat(2000)), b'i', Div, "x"),
            t("", b'x', Div, ";"),
        ],
        "strings" => vec![
            t("", b's', Div, format!("'{}'", "s".repeat(4096))),
            t("\n", b's', Div, format!("\"{}\"", "a\\n\\u00e9\\x41\\u{1F600}q".repeat(70))),
            t("", b'x', Div, ";"),
        ],
        _ => fail(format!("unknown case {case}")),
    }
}

struct Run {
    tokens: u64,
    hash: u64,
    units: u64,
}

/// Lex `src` as `n` copies of `spec`. Only the lexer is timed work. Returns what it saw.
fn lex_all(src: &str, spec: &[Tok], decode: bool, buf: &mut Vec<u16>) -> Run {
    let mut l = Lexer::new(src).unwrap_or_else(|e| fail(format!("{e:?}")));
    let (mut tokens, mut hash, mut units) = (0, FNV0, 0u64);
    'outer: loop {
        for tk in spec {
            let t = l.next(tk.goal).unwrap_or_else(|e| fail(format!("token {tokens}: {:?} at {:?}", e.kind, e.location(src))));
            if t.kind == Kind::Eof {
                break 'outer;
            }
            let c = class_of(t.kind);
            if c != tk.class {
                fail(format!("token {tokens}: class {} want {}", c as char, tk.class as char));
            }
            hash = mix(hash, c, t.start, t.end);
            tokens += 1;
            if decode {
                buf.clear();
                let _ = match t.kind {
                    Kind::String { .. } => decode_string(src, &t, buf),
                    _ => Ok(()),
                };
                units += buf.len() as u64;
            }
        }
    }
    Run { tokens, hash, units }
}

/// The same walk with no checks and no hash: the cost of the lexer and the goal lookup only.
fn lex_only(src: &str, spec: &[Tok]) -> u64 {
    let mut l = Lexer::new(src).unwrap();
    let mut tokens = 0;
    loop {
        for tk in spec {
            match l.next(tk.goal) {
                Ok(t) if t.kind == Kind::Eof => return tokens,
                Ok(_) => tokens += 1,
                Err(_) => return u64::MAX,
            }
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (Some(case), Some(scale)) = (a.first(), a.get(1).and_then(|s| s.parse::<usize>().ok())) else {
        eprintln!("usage: lex_load small|repeat|comments|strings|malformed <scale> [runs] [decode]");
        std::process::exit(2)
    };
    let runs: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(5);
    let decode = a.get(3).is_some_and(|s| s == "decode");
    let target = scale * (512 << 10);
    if target > MAX_SOURCE_LEN {
        fail("scale is over the source cap".into());
    }
    if case == "malformed" {
        return malformed(scale, runs);
    }

    let spec = unit(case);
    let unit_src: String = spec.iter().map(|t| format!("{}{}", t.sep, t.text)).collect();
    let n = target / unit_src.len();
    let src = unit_src.repeat(n);
    // Expected tokens, from the spec and the arithmetic of the offsets (not from the lexer).
    let (mut want_hash, mut off) = (FNV0, 0u32);
    for _ in 0..n {
        for tk in &spec {
            let start = off + tk.sep.len() as u32;
            off = start + tk.text.len() as u32;
            want_hash = mix(want_hash, tk.class, start, off);
        }
    }
    let want_tokens = (n * spec.len()) as u64;
    // Each plain string is 4096 units. Each escaped string is 70 x (a, LF, e-acute, A, 2 units for the emoji, q) = 490.
    let want_units: u64 = if decode { 4096 + 490 } else { 0 } * n as u64;
    let mut buf: Vec<u16> = Vec::with_capacity(8192);
    let mut ms = Vec::new();
    let mut allocs = 0;
    for run in 0..runs {
        let before = ALLOCS.load(Relaxed);
        let start = Instant::now();
        let r = lex_all(&src, &spec, decode, &mut buf);
        let took = start.elapsed().as_secs_f64() * 1000.0;
        allocs = ALLOCS.load(Relaxed) - before;
        ms.push(took);
        if r.tokens != want_tokens || r.hash != want_hash || r.units != want_units {
            fail(format!("run {run}: {} tokens (want {want_tokens}), hash {:016x} (want {want_hash:016x}), {} units (want {want_units})", r.tokens, r.hash, r.units));
        }
        if allocs != 0 {
            fail(format!("run {run}: {allocs} allocations while lexing"));
        }
    }
    // Lexing alone, without the per-token checks of the runs above.
    let mut only = Vec::new();
    for _ in 0..runs {
        let start = Instant::now();
        let tokens = lex_only(&src, &spec);
        only.push(start.elapsed().as_secs_f64() * 1000.0);
        if tokens != want_tokens {
            fail(format!("lex_only: {tokens} tokens (want {want_tokens})"));
        }
    }
    only.sort_by(|a, b| a.partial_cmp(b).unwrap());
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = ms[ms.len() / 2];
    println!(
        "case={case} scale={scale} decode={decode} source_bytes={} tokens={want_tokens} bytes_per_token={:.1} token_struct={} min_ms={:.2} med_ms={med:.2} lex_only_min_ms={:.2} mb_per_s={:.0} ns_per_token={:.1} allocs={allocs} rss_hwm_kib={} ok=true",
        src.len(), src.len() as f64 / want_tokens as f64, std::mem::size_of::<Token>(), ms[0], only[0], src.len() as f64 / 1e6 / (med / 1000.0), med * 1e6 / want_tokens as f64, status_kib("VmHWM:")
    );
}

/// Bad sources: each must fail with this kind at this offset. Then one big valid source with a bad string at its end.
fn malformed(scale: usize, runs: usize) {
    use ErrorKind::*;
    let cases: [(&str, Goal, ErrorKind, u32); 10] = [
        ("'abc", Goal::Div, UnterminatedString, 0),
        ("a /* x", Goal::Div, UnterminatedComment, 2),
        ("0x", Goal::Div, InvalidNumber, 0),
        ("1__0", Goal::Div, InvalidNumber, 1),
        ("x = '\\u{110000}'", Goal::Div, InvalidEscape, 5),
        ("/a/gg", Goal::RegExp, DuplicateRegExpFlag, 4),
        ("`abc", Goal::Div, UnterminatedTemplate, 0),
        ("@", Goal::Div, UnexpectedChar, 0),
        ("\u{e9}", Goal::Div, NonAscii, 0),
        ("#", Goal::Div, InvalidPrivateName, 0),
    ];
    let target = scale * (512 << 10);
    let n = target / cases.iter().map(|c| c.0.len()).sum::<usize>();
    let spec = unit("small");
    let unit_src: String = spec.iter().map(|t| format!("{}{}", t.sep, t.text)).collect();
    let reps = target / unit_src.len();
    let body = unit_src.repeat(reps);
    let want_tokens = reps * spec.len() + 1; // and the `;` before the bad string
    let big = format!("{body};'oops\n");
    let want_off = body.len() as u32 + 1;
    let (mut ms, mut allocs) = (Vec::new(), 0);
    for _ in 0..runs {
        let before = ALLOCS.load(Relaxed);
        let start = Instant::now();
        let mut errors = 0usize;
        for _ in 0..n {
            for &(src, goal, kind, off) in &cases {
                let mut l = Lexer::new(src).unwrap();
                let e = loop {
                    match l.next(goal) {
                        Ok(t) if t.kind == Kind::Eof => fail(format!("{src:?} lexed with no error")),
                        Ok(_) => {}
                        Err(e) => break e,
                    }
                };
                if e.kind != kind || e.offset != off {
                    fail(format!("{src:?}: {:?} at {} (want {kind:?} at {off})", e.kind, e.offset));
                }
                errors += 1;
            }
        }
        let mut l = Lexer::new(&big).unwrap();
        let mut i = 0;
        let e = loop {
            match l.next(spec[i % spec.len()].goal) {
                Ok(t) if t.kind == Kind::Eof => fail("big source lexed with no error".into()),
                Ok(_) => i += 1,
                Err(e) => break e,
            }
        };
        let took = start.elapsed().as_secs_f64() * 1000.0;
        allocs = ALLOCS.load(Relaxed) - before;
        ms.push(took);
        if e.kind != UnterminatedString || e.offset != want_off || errors != n * cases.len() || i != want_tokens {
            fail(format!("big source: {:?} at {} (want at {want_off}); {i} tokens (want {want_tokens}); {errors} errors", e.kind, e.offset));
        }
        if allocs != 0 {
            fail(format!("{allocs} allocations while lexing"));
        }
    }
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "case=malformed scale={scale} bad_sources={} big_source_bytes={} min_ms={:.2} med_ms={:.2} allocs={allocs} rss_hwm_kib={} ok=true",
        n * cases.len() + 1, big.len(), ms[0], ms[ms.len() / 2], status_kib("VmHWM:")
    );
}
