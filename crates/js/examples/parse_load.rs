//! Load test for the parser. One case at one scale per process, so that the peak memory belongs to that run.
//!
//!   parse_load CASE SCALE      CASE: stmts | chain | bodies | malformed | deep    SCALE: 1, 10, 100
//!
//! The valid cases assert the exact node counts and a walk over the tree, then print the time (best and median of
//! 5 parses), the peak resident memory, the allocations of one parse and the tree size. The invalid cases assert
//! the exact error and print no time: an error is never a timing. A failed assertion exits with status 1.
//! `tools/js/parse_load.sh` runs the matrix with a time and memory cap.

use bare_js::ast::*;
use bare_js::parse::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            ALLOCS.fetch_add(1, Relaxed);
            BYTES.fetch_add(l.size(), Relaxed);
            let live = LIVE.fetch_add(l.size(), Relaxed) + l.size();
            PEAK.fetch_max(live, Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            ALLOCS.fetch_add(1, Relaxed);
            if new > l.size() {
                BYTES.fetch_add(new - l.size(), Relaxed);
                let live = LIVE.fetch_add(new - l.size(), Relaxed) + new - l.size();
                PEAK.fetch_max(live, Relaxed);
            } else {
                LIVE.fetch_sub(l.size() - new, Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

fn die(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1)
}

fn check(cond: bool, what: &str) {
    if !cond {
        die(what.to_string());
    }
}

fn status_kib(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok()).unwrap_or(0)
}

// ------------------------------------------------------------------ sources

fn stmts_src(n: usize) -> String {
    let mut s = String::with_capacity(n * 16);
    for i in 0..n {
        s.push_str(&format!("var v{i} = {i} + 1;\n"));
    }
    s
}

/// (source, binary terms, member links, call links)
fn chain_src(t: usize) -> String {
    let mut s = String::from("t = v0");
    for i in 1..t {
        s.push_str(&format!(" + v{i}"));
    }
    s.push_str(";\no");
    s.push_str(&".p".repeat(t));
    s.push_str(";\nf");
    s.push_str(&"()".repeat(t));
    s.push_str(";\n");
    s
}

const UNIT: &str = "\
function f_I(a, b = 1) { var s = 0; for (var i = 0; i < a; i++) { if (i % 2) s += i; else s -= b; } return s + I; }
class C_I extends B { x = 1; static y = I; #z = 2; constructor(p) { super(); this.p = p; } get g() { return this.#z; } m(q) { return q ? q * 2 : -q; } }
";

fn bodies_src(n: usize) -> String {
    let mut s = String::with_capacity(n * UNIT.len());
    for i in 0..n {
        s.push_str(&UNIT.replace('I', &i.to_string()));
    }
    s
}

// ------------------------------------------------------------------ checks

fn stat_fields(s: &Stats) -> [usize; 14] {
    [s.exprs, s.stmts, s.pats, s.funcs, s.classes, s.props, s.pat_props, s.templates, s.quasis, s.decls, s.cases, s.catches, s.elems, s.list_items]
}

fn checksum(s: &Stats, extra: u64) -> u64 {
    let mut h = 0xcbf29ce484222325u64 ^ extra;
    for v in stat_fields(s) {
        h = (h ^ v as u64).wrapping_mul(0x100000001b3);
    }
    h
}

/// Check the tree of the `stmts` case and return a checksum of the numbers in it.
fn walk_stmts(p: &Program, n: usize) -> u64 {
    let mut sum = 0u64;
    let mut count = 0;
    for (i, id) in p.list(p.body).enumerate() {
        let StmtKind::Var { kind: VarKind::Var, decls } = p.stmt(id).kind else { die(format!("statement {i} is not a var")) };
        check(decls.len() == 1, "one declarator");
        let d = p.decl(p.list(decls).next().unwrap());
        check(matches!(p.pat(d.target).kind, PatKind::Ident) && p.text(p.pat(d.target).span) == format!("v{i}"), "binding name");
        let ExprKind::Binary { op: BinaryOp::Add, left, right } = p.expr(d.init.unwrap()).kind else { die("init is not an addition".into()) };
        let (ExprKind::Number(lb), ExprKind::Number(rb)) = (p.expr(left).kind, p.expr(right).kind) else { die("operands are not numbers".into()) };
        check(p.number(p.expr(left).span, lb) == Some(i as f64), "left number");
        check(p.number(p.expr(right).span, rb) == Some(1.0), "right number");
        sum += i as u64 + 1;
        count += 1;
    }
    check(count == n, "statement count");
    sum
}

/// Length of the left spine that starts at `e`, for binary, member and call chains. The spines are long, so no recursion.
fn spine(p: &Program, mut e: ExprId) -> usize {
    let mut n = 0;
    loop {
        match p.expr(e).kind {
            ExprKind::Binary { left, .. } => e = left,
            ExprKind::Member { obj, .. } => e = obj,
            ExprKind::Call { callee, .. } => e = callee,
            ExprKind::Assign { value, .. } => {
                n += 1;
                e = value;
                continue;
            }
            _ => return n,
        }
        n += 1;
    }
}

fn walk_chain(p: &Program, t: usize) -> u64 {
    let mut stmts = p.list(p.body);
    let mut total = 0;
    let expect = [t - 1 + 1, t, t]; // assign + binaries; member links; call links
    for (i, want) in expect.iter().enumerate() {
        let StmtKind::Expr(e) = p.stmt(stmts.next().unwrap()).kind else { die("not an expression statement".into()) };
        let got = spine(p, e);
        check(got == *want, &format!("spine {i}: {got} != {want}"));
        total += got as u64;
    }
    check(stmts.next().is_none(), "three statements");
    total
}

fn walk_bodies(p: &Program, n: usize) -> u64 {
    let (mut funcs, mut classes, mut elems, mut methods) = (0, 0, 0, 0u64);
    for id in p.list(p.body) {
        match p.stmt(id).kind {
            StmtKind::Function(f) => {
                funcs += 1;
                check(p.func(f).params.len() == 2 && !p.func(f).simple_params, "function parameters");
            }
            StmtKind::Class(c) => {
                classes += 1;
                let c = p.class(c);
                check(c.superclass.is_some(), "superclass");
                for e in p.list(c.elems) {
                    elems += 1;
                    if let ElemKind::Method(f) | ElemKind::Getter(f) | ElemKind::Setter(f) = p.elem(e).kind {
                        methods += 1;
                        if p.func(f).kind == FuncKind::DerivedConstructor {
                            check(p.func(f).params.len() == 1, "constructor parameters");
                        }
                    }
                }
            }
            k => die(format!("unexpected statement {k:?}")),
        }
    }
    check(funcs == n && classes == n, "function and class count");
    check(elems == 6 * n && methods == 3 * n as u64, "class element count");
    methods
}

// ------------------------------------------------------------------ main

fn time_valid(src: &str, n: usize, case: &str, scale: usize, extra: impl Fn(&Program) -> u64, unit: Option<Stats>) {
    // The first parse is the counted one: allocations and tree size. The others only time.
    let (a0, b0) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed));
    PEAK.store(LIVE.load(Relaxed), Relaxed);
    let live0 = LIVE.load(Relaxed);
    let t = Instant::now();
    let p = parse_script(src, Limits::default()).unwrap_or_else(|e| die(format!("{case}: {e}")));
    let first = t.elapsed();
    let (allocs, bytes, peak) = (ALLOCS.load(Relaxed) - a0, BYTES.load(Relaxed) - b0, PEAK.load(Relaxed) - live0);
    let st = p.stats();
    let sum = extra(&p);
    if let Some(u) = unit {
        // Linear in the number of units: every count is `scale * units` times the count of one unit.
        let (got, one) = (stat_fields(&st), stat_fields(&u));
        for (i, (g, o)) in got.iter().zip(one).enumerate() {
            check(*g == o * n, &format!("field {i}: {g} != {n} * {o}"));
        }
    }
    drop(p);
    let mut times = vec![first];
    for _ in 0..4 {
        let t = Instant::now();
        let p = parse_script(src, Limits::default()).unwrap_or_else(|e| die(format!("{case}: {e}")));
        times.push(t.elapsed());
        drop(p);
    }
    times.sort();
    println!(
        "case={case} scale={scale} src_bytes={} nodes={} list_items={} tree_bytes={} best_us={} median_us={} mb_per_s={:.1} allocs={allocs} alloc_bytes={bytes} peak_live_bytes={peak} rss_kib={} checksum={:016x}",
        src.len(), st.nodes(), st.list_items, st.bytes, times[0].as_micros(), times[2].as_micros(),
        src.len() as f64 / times[0].as_secs_f64() / 1e6, status_kib("VmHWM:"), checksum(&st, sum)
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (Some(case), Some(scale)) = (a.get(1), a.get(2).and_then(|s| s.parse::<usize>().ok())) else {
        die("usage: parse_load CASE SCALE".into())
    };
    check(scale >= 1, "scale");
    match case.as_str() {
        "stmts" => {
            let n = 2000 * scale;
            let src = stmts_src(n);
            let unit_stats = parse_script(&stmts_src(1), Limits::default()).unwrap().stats();
            check(unit_stats.exprs == 3 && unit_stats.stmts == 1 && unit_stats.pats == 1 && unit_stats.decls == 1, "unit counts");
            time_valid(&src, n, case, scale, |p| walk_stmts(p, n), Some(unit_stats));
        }
        "chain" => {
            let t = 10_000 * scale;
            let src = chain_src(t);
            // 3 statements; exprs: assign + (2t - 1) binary tree + (t + 1) member + (t + 1) call + 1 unreachable node:
            // the assignment target was parsed as an expression first, then converted to a pattern.
            let l = Limits { max_list: u32::MAX, ..Limits::default() };
            let p = parse_script(&src, l).unwrap_or_else(|e| die(format!("{e}")));
            let st = p.stats();
            check(st.stmts == 3 && st.exprs == 1 + (2 * t - 1) + (t + 1) + (t + 1) + 1 && st.pats == 1, "chain counts");
            drop(p);
            time_valid(&src, t, case, scale, |p| walk_chain(p, t), None);
        }
        "bodies" => {
            let n = 200 * scale;
            let src = bodies_src(n);
            let u = parse_script(&bodies_src(1), Limits::default()).unwrap().stats();
            check(u.funcs == 4 && u.classes == 1 && u.elems == 6 && u.stmts > 8, "unit counts");
            time_valid(&src, n, case, scale, |p| walk_bodies(p, n), Some(u));
        }
        "malformed" => {
            // Valid source, then one bad token. The error must name that token. Truncation must fail at the end.
            let n = 2000 * scale;
            let mut src = stmts_src(n);
            let at = src.len() + 4;
            src.push_str("var = ;\n");
            let e = parse_script(&src, Limits::default()).err().unwrap_or_else(|| die("malformed source accepted".into()));
            check(e.kind == ErrorKind::Syntax && e.start as usize == at, &format!("error {e}, want Syntax at {at}"));
            let body = bodies_src(200 * scale);
            let cut = &body[..body.len() - 3]; // drops `}\n` of the last class and one more byte
            let e = parse_script(cut, Limits::default()).err().unwrap_or_else(|| die("truncated source accepted".into()));
            check(e.kind == ErrorKind::Syntax && e.start as usize >= cut.len() - 1, &format!("truncation error {e}"));
            let l = e.location(cut);
            check(l.line as usize == 400 * scale, "truncation line");
            println!("case={case} scale={scale} errors_ok=2 src_bytes={} rss_kib={}", src.len() + cut.len(), status_kib("VmHWM:"));
        }
        "deep" => {
            let n = 5_000 * scale;
            let kinds: Vec<(&str, String)> = vec![
                ("parens", format!("{}1{}", "(".repeat(n), ")".repeat(n))),
                ("blocks", format!("{}{}", "{".repeat(n), "}".repeat(n))),
                ("arrows", format!("{}1", "a => ".repeat(n))),
                ("classes", format!("{}{}", "class A { m(){".repeat(n), "}}".repeat(n))),
            ];
            // A 2 MiB thread, like the parser's own tests.
            let h = std::thread::Builder::new().stack_size(2 << 20).spawn(move || {
                for (name, src) in &kinds {
                    let e = parse_script(src, Limits::default()).err().unwrap_or_else(|| die(format!("{name} accepted")));
                    check(e.kind == ErrorKind::Limit(LimitKind::Depth), &format!("{name}: {e}"));
                }
                kinds.iter().map(|k| k.1.len()).sum::<usize>()
            });
            let total = h.unwrap().join().unwrap_or_else(|_| die("deep input crashed the thread".into()));
            println!("case={case} scale={scale} errors_ok=4 src_bytes={total} rss_kib={}", status_kib("VmHWM:"));
        }
        _ => die(format!("unknown case {case}")),
    }
}
