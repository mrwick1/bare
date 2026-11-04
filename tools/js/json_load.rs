//! Standalone load, cap and oracle driver for crates/js/src/json.rs. No crates.
//!
//! Build: rustc --edition=2024 -C opt-level=3 tools/js/json_load.rs -o <out>
//! Unit tests of the three modules: rustc --edition=2024 --test tools/js/json_load.rs -o <t> && <t>
//!
//! <out> run <workload> <scale: 1|10|100>   one workload in this process, one result line
//! <out> caps                               malformed input, resource caps, adversarial nesting
//! <out> all [workload]                     every scale in its own child process, each with
//!                                          `ulimit -v` (2 GiB address space) and `timeout 45`
//! <out> oracle <dir> [seed] [cases]        generate small JSON texts, run them through Node's
//!                                          JSON.parse, compare accept/reject and every value
//!
//! Workloads: records (array of small objects), numbers (mixed number syntax), strings (escapes,
//! short and long), wide (one object with many keys, one long array), deep (nesting).
//! Each asserts a checksum computed from the generator, and peak RSS <= 1 GiB.
//! Node runs only the generated cases in <dir>; it never sees target bundles.

#[allow(dead_code)]
#[path = "../../crates/js/src/value.rs"]
mod value;
#[allow(dead_code)]
#[path = "../../crates/js/src/heap.rs"]
mod heap;
#[allow(dead_code)]
#[path = "../../crates/js/src/json.rs"]
mod json;

use heap::*;
use json::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;
use value::*;

struct Counting;
static ALLOCS: AtomicU64 = AtomicU64::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static A: Counting = Counting;

fn cpu_secs() -> f64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap();
    let f: Vec<&str> = s[s.rfind(')').unwrap() + 2..].split(' ').collect();
    (f[11].parse::<f64>().unwrap() + f[12].parse::<f64>().unwrap()) / 100.0
}

fn status_kib(field: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    let l = s.lines().find(|l| l.starts_with(field)).unwrap();
    l.split_whitespace().nth(1).unwrap().parse().unwrap()
}

fn units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, pct: usize) -> bool {
        self.below(100) < pct
    }
}

/// What a walk over a parsed value finds. Iterative, so deep results cannot overflow the stack.
#[derive(Default, Debug, PartialEq)]
struct Tally {
    nodes: u64,
    numbers: u64,
    num_sum: f64,
    strings: u64,
    text_units: u64,
    keys: u64,
}

fn walk(h: &Heap, root: Value) -> Tally {
    let mut t = Tally::default();
    let mut todo = vec![root];
    while let Some(v) = todo.pop() {
        t.nodes += 1;
        match v {
            Value::Number(n) => {
                t.numbers += 1;
                t.num_sum += n;
            }
            Value::String(s) => {
                t.strings += 1;
                t.text_units += h.str_len(s).unwrap() as u64;
            }
            Value::Object(o) => {
                for k in h.own_keys(o).unwrap() {
                    if h.is_array(o).unwrap() && matches!(k, PropertyKey::String(_)) {
                        continue; // `length`
                    }
                    if let PropertyKey::String(s) = k {
                        t.text_units += h.str_len(s).unwrap() as u64;
                    }
                    if !h.is_array(o).unwrap() {
                        t.keys += 1;
                    }
                    let Some(Property::Data { value, .. }) = h.get_own_property(o, k).unwrap() else { panic!("not data") };
                    todo.push(value);
                }
            }
            _ => {}
        }
    }
    t
}

// ---------- workloads ----------

struct Out {
    ops: u64,
    checksum: String,
    extra: String,
}

struct Case {
    text: Vec<u16>,
    want: Tally,
}

/// Array of `n` records {"id":i,"name":"item i","tags":["a","b"],"v":i/1000,"ok":bool,"n":null}.
fn records(n: u64) -> Case {
    let mut s = String::from("[");
    let mut w = Tally { nodes: 1, ..Default::default() };
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        let v = i as f64 / 1000.0;
        write!(s, "{{\"id\":{i},\"name\":\"item {i}\",\"tags\":[\"a\",\"b\"],\"v\":{v},\"ok\":{},\"n\":null}}", i % 2 == 0).unwrap();
        // object + id + name + tags(+2 strings) + v + ok + n
        w.nodes += 1 + 1 + 1 + 3 + 1 + 1 + 1;
        w.numbers += 2;
        w.num_sum += i as f64 + v;
        w.strings += 3;
        w.text_units += format!("item {i}").len() as u64 + 2;
        w.keys += 6;
        w.text_units += ["id", "name", "tags", "v", "ok", "n"].iter().map(|k| k.len() as u64).sum::<u64>();
    }
    s.push(']');
    Case { text: units(&s), want: w }
}

/// Numbers: small ints, big ints, fractions, exponents, negative, -0, many digits.
fn numbers(n: u64) -> Case {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut s = String::from("[");
    let mut sum = 0.0;
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        let t = match rng.below(6) {
            0 => format!("{}", rng.below(1000)),
            1 => format!("-{}", rng.next() >> 11),
            2 => format!("{}.{}", rng.below(100000), rng.below(1000000)),
            3 => format!("{}e{}", rng.below(1000), rng.below(40)),
            4 => format!("{}.{}E-{}", rng.below(10), rng.next(), rng.below(20)),
            _ => "-0".to_string(),
        };
        sum += t.parse::<f64>().unwrap(); // Rust's own parser: same rounding; checks plumbing and speed only
        s += &t;
    }
    s.push(']');
    Case { text: units(&s), want: Tally { nodes: n + 1, numbers: n, num_sum: sum, ..Default::default() } }
}

/// Strings: every third has escapes, every 50th is 4 KiB, keys repeat (interner hits).
fn strings(n: u64) -> Case {
    let mut s = String::from("[");
    let mut w = Tally { nodes: 1, ..Default::default() };
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        let (text, len) = if i % 50 == 0 {
            let b = format!("long{i}-").repeat(600);
            let l = b.len();
            (b, l)
        } else if i % 3 == 0 {
            (format!("a\\n\\u00e9\\\"{i}\\uD83D\\uDE00"), 1 + 1 + 1 + 1 + i.to_string().len() + 2)
        } else {
            let b = format!("short string number {i}");
            let l = b.len();
            (b, l)
        };
        write!(s, "\"{text}\"").unwrap();
        w.nodes += 1;
        w.strings += 1;
        w.text_units += len as u64;
    }
    s.push(']');
    Case { text: units(&s), want: w }
}

/// `n` keys on one object plus an array of `4n` small numbers.
fn wide(n: u64) -> Case {
    let mut s = String::from("{\"o\":{");
    let mut w = Tally { nodes: 3, keys: n + 2, ..Default::default() };
    w.text_units += 2; // "o" and "a"
    let mut sum = 0.0;
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        write!(s, "\"key{i}\":{i}").unwrap();
        w.nodes += 1;
        w.numbers += 1;
        sum += i as f64;
        w.text_units += format!("key{i}").len() as u64;
    }
    s += "},\"a\":[";
    for i in 0..4 * n {
        if i > 0 {
            s.push(',');
        }
        write!(s, "{}", i % 10).unwrap();
        w.nodes += 1;
        w.numbers += 1;
        sum += (i % 10) as f64;
    }
    s += "]}";
    w.keys = n + 2;
    w.num_sum = sum;
    Case { text: units(&s), want: w }
}

/// Nesting `d` deep as arrays and as objects, one leaf each.
fn deep(d: u64) -> Case {
    let d = d as usize;
    let s = "[".repeat(d) + "1" + &"]".repeat(d) + "";
    let o = "{\"k\":".repeat(d) + "2" + &"}".repeat(d);
    let text = units(&format!("[{s},{o}]"));
    Case {
        text,
        want: Tally { nodes: 1 + 2 * d as u64 + 2, numbers: 2, num_sum: 3.0, keys: d as u64, text_units: d as u64, ..Default::default() },
    }
}

fn run_case(w: &str, scale: u64) -> Out {
    let n = 5_000 * scale;
    let case = match w {
        "records" => records(n),
        "numbers" => numbers(20 * n),
        "strings" => strings(n),
        "wide" => wide(n),
        "deep" => deep(1_000 * scale),
        _ => panic!("unknown workload {w}"),
    };
    let src_units = case.text.len();
    let mut h = Heap::default();
    let lim = JsonLimits { max_depth: 200_000, ..JsonLimits::default() };
    let (a0, t0, c0) = (ALLOCS.load(Relaxed), Instant::now(), cpu_secs());
    let v = parse(&mut h, &case.text, &JsonOptions::default(), &lim).unwrap_or_else(|e| panic!("{w} {scale}x: {e:?}"));
    let wall = t0.elapsed().as_secs_f64();
    let parse_allocs = ALLOCS.load(Relaxed) - a0;
    let parse_cpu = cpu_secs() - c0;
    let got = walk(&h, v);
    assert_eq!(got.nodes, case.want.nodes, "nodes");
    assert_eq!(got.numbers, case.want.numbers, "numbers");
    assert_eq!(got.strings, case.want.strings, "strings");
    assert_eq!(got.text_units, case.want.text_units, "text");
    assert_eq!(got.keys, case.want.keys, "keys");
    let tol = 1e-9 * case.want.num_sum.abs().max(1.0);
    assert!((got.num_sum - case.want.num_sum).abs() <= tol, "sum {} vs {}", got.num_sum, case.want.num_sum);
    let st = h.stats();
    Out {
        ops: got.nodes,
        checksum: format!("{}", got.nodes ^ got.text_units),
        extra: format!(
            "src_mib={:.1} parse_wall={wall:.3}s parse_cpu={parse_cpu:.2}s mib_per_s={:.0} ns_per_node={:.0} parse_allocs={parse_allocs} allocs_per_node={:.2} heap_bytes={} live_objects={} live_strings={}",
            src_units as f64 * 2.0 / 1048576.0, src_units as f64 * 2.0 / 1048576.0 / wall, wall * 1e9 / got.nodes as f64,
            parse_allocs as f64 / got.nodes as f64, st.bytes, st.live_objects, st.live_strings
        ),
    }
}

const WORKLOADS: [&str; 5] = ["records", "numbers", "strings", "wide", "deep"];

fn run(w: &str, scale: u64) {
    let (rss0, cpu0) = (status_kib("VmRSS:"), cpu_secs());
    let t = Instant::now();
    let out = run_case(w, scale);
    let peak = status_kib("VmHWM:");
    assert!(peak <= 1 << 20, "peak RSS {peak} KiB exceeds 1 GiB");
    println!(
        "workload={w} scale={scale}x nodes={} total_wall={:.3}s total_cpu={:.2}s peak_rss_kib={peak} rss_start_kib={rss0} checksum={} {} ok",
        out.ops, t.elapsed().as_secs_f64(), cpu_secs() - cpu0, out.checksum, out.extra
    );
}

fn all(only: Option<&str>) {
    let me = std::env::current_exe().unwrap();
    let mut failed = false;
    for w in WORKLOADS.iter().filter(|w| only.is_none_or(|o| o == **w)) {
        for scale in [1, 10, 100] {
            let cmd = format!("ulimit -v 2097152; exec timeout 45 '{}' run {w} {scale}", me.display());
            let o = std::process::Command::new("sh").args(["-c", &cmd]).output().unwrap();
            if o.status.success() {
                print!("{}", String::from_utf8_lossy(&o.stdout));
            } else {
                failed = true;
                println!("workload={w} scale={scale}x FAILED status={:?}\n{}", o.status.code(), String::from_utf8_lossy(&o.stderr));
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
}

// ---------- caps and adversarial input ----------

fn timed<T>(name: &str, f: impl FnOnce() -> T) -> T {
    let (t, c, a) = (Instant::now(), cpu_secs(), ALLOCS.load(Relaxed));
    let r = f();
    println!("  {name}: wall={:.3}s cpu={:.2}s allocs={} peak_rss_kib={}", t.elapsed().as_secs_f64(), cpu_secs() - c, ALLOCS.load(Relaxed) - a, status_kib("VmHWM:"));
    r
}

fn caps() {
    let o = JsonOptions::default();
    let kind = |r: Result<Value, Error>| r.unwrap_err().kind;
    println!("caps (default limits unless noted):");
    // 100M-deep flood: depth error at 1024, only 1024 objects made, no stack growth.
    let flood = units(&"[".repeat(100_000_000));
    let mut h = Heap::default();
    let r = timed("flood 100M '[' (source limit 128M)", || parse(&mut h, &flood, &o, &JsonLimits { max_source_units: 1 << 28, ..Default::default() }));
    assert_eq!(r.unwrap_err(), Error { kind: ErrorKind::Limit(LimitKind::Depth), offset: 1024 });
    assert_eq!(h.stats().live_objects, 1024);
    drop(flood);
    // Same flood with the depth knob at usize::MAX: the hard cap holds.
    let flood = units(&"[".repeat(HARD_MAX_DEPTH + 10));
    let mut h = Heap::default();
    let r = timed("flood with max_depth=usize::MAX", || parse(&mut h, &flood, &o, &JsonLimits { max_depth: usize::MAX, ..Default::default() }));
    assert_eq!(r.unwrap_err(), Error { kind: ErrorKind::Limit(LimitKind::Depth), offset: HARD_MAX_DEPTH });
    drop(flood);
    // Source cap rejects before any scan.
    let mut h = Heap::default();
    let big = vec![b' ' as u16; 1000];
    let r = parse(&mut h, &big, &o, &JsonLimits { max_source_units: 999, ..Default::default() });
    assert_eq!(r.unwrap_err(), Error { kind: ErrorKind::Limit(LimitKind::Source), offset: 999 });
    assert_eq!(h.stats().total_string_allocs, 1); // only the permanent "length"
    // Node bomb: 10M zeros under a 1M node cap; nothing allocated beyond the array.
    let mut s = String::from("[");
    s += &"0,".repeat(10_000_000);
    s += "0]";
    let t = units(&s);
    let mut h = Heap::default();
    let r = timed("10M elements, max_nodes=1M", || parse(&mut h, &t, &o, &JsonLimits { max_nodes: 1 << 20, ..Default::default() }));
    assert_eq!(kind(r), ErrorKind::Limit(LimitKind::Nodes));
    // Text bomb: 8 MiB string under a 1 MiB text cap: no intern allocation.
    let t = units(&format!("\"{}\"", "x".repeat(8 << 20)));
    let mut h = Heap::default();
    let before = h.stats().total_string_allocs;
    let r = timed("8M-unit string, max_text=1M", || parse(&mut h, &t, &o, &JsonLimits { max_text_units: 1 << 20, ..Default::default() }));
    assert_eq!(kind(r), ErrorKind::Limit(LimitKind::Text));
    assert_eq!(h.stats().total_string_allocs, before);
    // Escaped text bomb: the scratch buffer stops at the cap.
    let t = units(&format!("\"{}\"", "\\n".repeat(4 << 20)));
    let r = timed("4M escapes, max_text=1M", || parse(&mut h, &t, &o, &JsonLimits { max_text_units: 1 << 20, ..Default::default() }));
    assert_eq!(kind(r), ErrorKind::Limit(LimitKind::Text));
    // Number with 10M digits: linear, correct rounding at the ends.
    let t = units(&format!("1{}", "0".repeat(10_000_000)));
    let mut h = Heap::default();
    let r = timed("10M-digit integer", || parse(&mut h, &t, &o, &JsonLimits::default())).unwrap();
    assert!(matches!(r, Value::Number(n) if n == f64::INFINITY));
    let t = units(&format!("0.{}1", "0".repeat(10_000_000)));
    let r = timed("0.000...1 with 10M zeros", || parse(&mut h, &t, &o, &JsonLimits::default())).unwrap();
    assert!(matches!(r, Value::Number(n) if n == 0.0));
    let t = units(&format!("{}.5e-{}", "9".repeat(5_000_000), 5_000_000));
    let r = timed("5M nines with exponent cancelling them", || parse(&mut h, &t, &o, &JsonLimits::default())).unwrap();
    assert!(matches!(r, Value::Number(n) if n == 1.0), "{r:?}");
    // Duplicate-key bomb: 1M repeats of one key keep one property.
    let mut s = String::from("{");
    s += &"\"a\":1,".repeat(1_000_000);
    s += "\"a\":2}";
    let mut h = Heap::default();
    let r = timed("1M duplicate keys", || parse(&mut h, &units(&s), &o, &JsonLimits::default())).unwrap();
    let Value::Object(ob) = r else { panic!() };
    assert_eq!(h.own_keys(ob).unwrap().len(), 1);
    assert_eq!(h.stats().live_strings, 2); // "length" and "a"
    // Heap byte limit comes back as a heap error, with the partial result reclaimable.
    let mut h = Heap::new(HeapLimits { max_bytes: 200_000, ..Default::default() }).unwrap();
    let t = units(&format!("[{}0]", "{\"a\":[1,2,3]},".repeat(100_000)));
    let r = parse(&mut h, &t, &o, &JsonLimits::default());
    assert!(matches!(r.unwrap_err().kind, ErrorKind::Heap(HeapError::Limit(_))));
    h.collect(&[]).unwrap();
    assert_eq!(h.stats().live_objects, 0);
    // Truncation at every prefix of a document is a syntax error, never a panic.
    let doc = r#" {"a":[1,-2.5e+3,"x\u00e9\n",true,false,null,{"b":{}}],"c":"\ud800"} "#;
    let d = units(doc);
    let mut h = Heap::default();
    for i in 0..d.len() - 1 { // the last unit is only a space: that prefix is valid
        let r = parse(&mut h, &d[..i], &o, &JsonLimits::default());
        assert!(matches!(r, Err(Error { kind: ErrorKind::Syntax(_), .. })), "prefix {i}");
    }
    assert!(parse(&mut h, &d, &o, &JsonLimits::default()).is_ok());
    println!("caps ok");
}

// ---------- Node oracle ----------

const POOL: [&str; 40] = [
    "0", "-0", "1", "-1", "10", "0.5", "1e400", "-1e400", "1e-400", "5e-324", "2.4703282292062327e-324", "2.4703282292062328e-324",
    "2.2250738585072011e-308", "2.2250738585072014e-308", "1.7976931348623157e308", "1.7976931348623158e308",
    "1.7976931348623159e308", "9007199254740993", "9007199254740992", "9007199254740995", "0.1", "0.30000000000000004",
    "123456789012345678901234567890", "1e99999999999999999999", "0e99999999999999999999", "1e-99999999999999999999",
    "4.35", "100000000000000000000000", "0.000001", "1E5", "1e+5", "1.0", "-0.0", "-0e1", "18446744073709551616",
    "3.141592653589793238462643383279", "7.0e-10", "123e-20", "0.0000000000000000000000000000001", "1.5e300",
];

fn rand_number(rng: &mut Rng) -> String {
    if rng.chance(40) {
        return POOL[rng.below(POOL.len())].to_string();
    }
    let digits = |rng: &mut Rng, n: usize| (0..n).map(|_| char::from(b'0' + rng.below(10) as u8)).collect::<String>();
    let mut s = String::new();
    if rng.chance(30) {
        s.push('-');
    }
    if rng.chance(15) {
        s.push('0');
    } else {
        s.push(char::from(b'1' + rng.below(9) as u8));
        let n = if rng.chance(20) { rng.below(40) } else { rng.below(12) };
        s += &digits(rng, n);
    }
    if rng.chance(50) {
        s.push('.');
        let n = 1 + if rng.chance(30) { rng.below(40) } else { rng.below(10) };
        s += &digits(rng, n);
    }
    if rng.chance(35) {
        s.push(if rng.chance(50) { 'e' } else { 'E' });
        match rng.below(3) {
            0 => s.push('+'),
            1 => s.push('-'),
            _ => {}
        }
        let e = if rng.chance(10) { rng.below(100000) } else { rng.below(340) };
        write!(s, "{e}").unwrap();
    }
    s
}

fn rand_string(rng: &mut Rng) -> String {
    const PIECES: [&str; 22] = [
        "a", "Z", " ", "é", "😀", "\\\"", "\\\\", "\\/", "\\b", "\\f", "\\n", "\\r", "\\t", "\\u0041", "\\u00E9", "\\ud83d\\ude00",
        "\\ud800", "\\udc00", "\\udbff\\udfff", "\\u0000", "\\u2028", "__proto__",
    ];
    let mut s = String::from("\"");
    for _ in 0..rng.below(8) {
        s += PIECES[rng.below(PIECES.len())];
    }
    s.push('"');
    s
}

fn rand_ws(rng: &mut Rng) -> &'static str {
    ["", "", "", " ", "\n", "\t", "\r\n", "  "][rng.below(8)]
}

fn gen_value(rng: &mut Rng, depth: usize) -> String {
    let k = if depth >= 6 { rng.below(5) } else { rng.below(8) };
    match k {
        0 => ["null", "true", "false"][rng.below(3)].to_string(),
        1 | 2 => rand_number(rng),
        3 | 4 => rand_string(rng),
        5 | 6 => {
            let n = rng.below(5);
            let items: Vec<String> = (0..n).map(|_| format!("{}{}{}", rand_ws(rng), gen_value(rng, depth + 1), rand_ws(rng))).collect();
            format!("[{}]", items.join(","))
        }
        _ => {
            const KEYS: [&str; 12] = ["\"a\"", "\"b\"", "\"__proto__\"", "\"0\"", "\"1\"", "\"01\"", "\"4294967295\"", "\"4294967294\"", "\"\"", "\"length\"", "\"constructor\"", "\"toString\""];
            let n = rng.below(6);
            let items: Vec<String> = (0..n)
                .map(|_| {
                    let key = if rng.chance(70) { KEYS[rng.below(KEYS.len())].to_string() } else { rand_string(rng) };
                    format!("{}{}{}:{}{}{}", rand_ws(rng), key, rand_ws(rng), rand_ws(rng), gen_value(rng, depth + 1), rand_ws(rng))
                })
                .collect();
            format!("{{{}}}", items.join(","))
        }
    }
}

/// One random edit of valid text, to get mostly-invalid input near the grammar.
fn mutate(rng: &mut Rng, text: &str) -> Vec<u16> {
    let mut u = units(text);
    if u.is_empty() {
        return u;
    }
    const INS: [u16; 24] = [
        b'[' as u16, b']' as u16, b'{' as u16, b'}' as u16, b',' as u16, b':' as u16, b'"' as u16, b'\\' as u16, b' ' as u16,
        b'-' as u16, b'+' as u16, b'.' as u16, b'e' as u16, b'0' as u16, b't' as u16, b'u' as u16, b'\'' as u16, 0x0A, 0x00,
        0x1F, 0xA0, 0xFEFF, 0xD800, 0x2028,
    ];
    for _ in 0..1 + rng.below(2) {
        if u.is_empty() {
            break;
        }
        let i = rng.below(u.len());
        match rng.below(4) {
            0 => {
                u.remove(i);
            }
            1 => u.insert(i, INS[rng.below(INS.len())]),
            2 => u[i] = INS[rng.below(INS.len())],
            _ => u.truncate(i),
        }
    }
    u
}

fn fixed_cases() -> Vec<Vec<u16>> {
    let mut v: Vec<Vec<u16>> = [
        "", " ", "null", "nul", "[]", "{}", "[1,]", "{\"a\":1,}", "01", "-", "1.", ".5", "+1", "1e", "NaN", "Infinity", "-Infinity",
        "\"\\u12\"", "\"\\x41\"", "[\"a\nb\"]", "'a'", "{a:1}", "[1 2]", "1 2", "// c\n1", "/*c*/1", "\u{feff}1", "\u{a0}1", "\u{2028}1",
        "\"\u{2028}\"", "\"\u{7f}\"", "{\"__proto__\":null}", "{\"__proto__\":{\"x\":1},\"x\":2}", "{\"a\":1,\"a\":2,\"b\":3,\"a\":4}",
        "{\"b\":1,\"2\":1,\"a\":1,\"1\":1}", "[[[[[[[[[[1]]]]]]]]]]", "{\"a\":{\"a\":{\"a\":{\"a\":{}}}}}", "\t\r\n 1 \t\r\n", "\u{b}1", "\u{c}1",
        "-0", "-0.0", "[-0,0]", "1E400", "[1e-400,-1e-400]", "tru", "truee", "falsee", "nulll", "[true,false,null]", "\"\\ud83d\\ude00\"",
        "\"\\ud83d\"", "\"\\ude00\\ud83d\"", "{\"\\u0061\":1}", "{\"a\\u0000b\":1}", "[1,2,3,4,5,6,7,8,9,10,11]",
    ]
    .iter()
    .map(|s| units(s))
    .collect();
    v.push(vec![b'"' as u16, 0xD800, b'"' as u16]); // raw lone surrogate inside a string
    v.push(vec![b'"' as u16, 0xDC00, 0xD800, b'"' as u16]);
    v.push(vec![0xD800]);
    // Long number tokens take the normalizing path: ties, sticky tails, 800-digit boundary, exponent clamps.
    for z in [790usize, 799, 800, 801, 802, 1100, 3000] {
        let zs = "0".repeat(z);
        for t in [
            format!("9007199254740993.{zs}"), format!("9007199254740993.{zs}1"), format!("9007199254740995.{zs}1"),
            format!("0.{zs}1e{z}"), format!("1{zs}e-{z}"), format!("-0.{zs}e5"), format!("1{zs}"), format!("0.{zs}1"),
            format!("1.7976931348623158{zs}e308"), format!("1.7976931348623159{zs}e308"), format!("2.4703282292062327{zs}e-324"),
            format!("2.4703282292062327{zs}1e-324"), format!("12345{zs}6789e-{}", z + 4), format!("{zs}5e-{}", z + 1),
        ] {
            v.push(units(&t));
        }
    }
    let mut rng = Rng(77);
    for _ in 0..300 {
        let n = 900 + rng.below(1500);
        let d: String = (0..n).map(|_| char::from(b'0' + rng.below(10) as u8)).collect();
        let e = rng.below(2000) as i64 - 1000 - n as i64 + 300;
        v.push(units(&format!("{}{d}e{e}", if rng.chance(50) { "-" } else { "" })));
        v.push(units(&format!("0.{d}e{}", rng.below(2000) as i64 - 1000)));
    }
    let deep = |n: usize| units(&("[".repeat(n) + &"]".repeat(n)));
    v.push(deep(300));
    v.push(units(&("{\"a\":".repeat(300) + "1" + &"}".repeat(300))));
    v
}

/// A JS string literal for the text: ASCII printable stays, the rest becomes \uXXXX.
fn js_literal(t: &[u16]) -> String {
    let mut s = String::from("\"");
    for &c in t {
        match c {
            0x22 => s += "\\\"",
            0x5C => s += "\\\\",
            0x20..=0x7E => s.push(c as u8 as char),
            _ => write!(s, "\\u{c:04x}").unwrap(),
        }
    }
    s.push('"');
    s
}

/// JSON.stringify's quoting of one string (well-formed): what Node prints for strings and keys.
fn quote(u: &[u16]) -> String {
    let mut s = String::from("\"");
    for r in char::decode_utf16(u.iter().copied()) {
        match r {
            Ok('"') => s += "\\\"",
            Ok('\\') => s += "\\\\",
            Ok('\u{8}') => s += "\\b",
            Ok('\u{c}') => s += "\\f",
            Ok('\n') => s += "\\n",
            Ok('\r') => s += "\\r",
            Ok('\t') => s += "\\t",
            Ok(c) if (c as u32) < 0x20 => write!(s, "\\u{:04x}", c as u32).unwrap(),
            Ok(c) => s.push(c),
            Err(e) => write!(s, "\\u{:04x}", e.unpaired_surrogate()).unwrap(),
        }
    }
    s.push('"');
    s
}

fn ser(h: &Heap, v: Value, out: &mut String) {
    match v {
        Value::Null => out.push('n'),
        Value::Bool(true) => out.push('t'),
        Value::Bool(false) => out.push('f'),
        Value::Number(n) => write!(out, "d{:x}", n.to_bits()).unwrap(),
        Value::String(s) => {
            out.push('s');
            out.push_str(&quote(h.str_units(s).unwrap()));
        }
        Value::Object(o) => {
            let arr = h.is_array(o).unwrap();
            out.push(if arr { '[' } else { '{' });
            let mut first = true;
            for k in h.own_keys(o).unwrap() {
                if arr && matches!(k, PropertyKey::String(_)) {
                    continue;
                }
                if !first {
                    out.push(',');
                }
                first = false;
                if !arr {
                    match k {
                        PropertyKey::Index(i) => write!(out, "\"{i}\"").unwrap(),
                        PropertyKey::String(s) => out.push_str(&quote(h.str_units(s).unwrap())),
                        PropertyKey::Symbol(_) => unreachable!(),
                    }
                    out.push(':');
                }
                let Some(Property::Data { value, writable: true, enumerable: true, configurable: true }) = h.get_own_property(o, k).unwrap() else {
                    panic!("property is not a plain data property")
                };
                ser(h, value, out);
            }
            out.push(if arr { ']' } else { '}' });
        }
        _ => unreachable!(),
    }
}

const NODE_JS: &str = r#"
const fs = require('fs');
const f = new Float64Array(1), b = new BigUint64Array(f.buffer);
function ser(v) {
  if (v === null) return 'n';
  if (v === true) return 't';
  if (v === false) return 'f';
  if (typeof v === 'number') { f[0] = v; return 'd' + b[0].toString(16); }
  if (typeof v === 'string') return 's' + JSON.stringify(v);
  const keys = Reflect.ownKeys(v);
  const part = k => {
    const d = Object.getOwnPropertyDescriptor(v, k);
    if (!(d.writable && d.enumerable && d.configurable && 'value' in d)) throw new Error('not plain data');
    return ser(d.value);
  };
  if (Array.isArray(v)) {
    if (keys.length !== v.length + 1) throw new Error('array extras');
    return '[' + keys.filter(k => k !== 'length').map(part).join(',') + ']';
  }
  return '{' + keys.map(k => JSON.stringify(k) + ':' + part(k)).join(',') + '}';
}
const lines = fs.readFileSync(process.argv[2], 'utf8').split('\n').filter(l => l.length);
const out = [];
for (const l of lines) {
  const text = JSON.parse(l);
  try { out.push(ser(JSON.parse(text))); } catch (e) { out.push('ERR'); }
}
process.stdout.write(out.join('\n') + '\n');
"#;

fn oracle(dir: &str, seed: u64, cases: usize) {
    std::fs::create_dir_all(dir).unwrap();
    let mut rng = Rng(seed | 1);
    let mut inputs = fixed_cases();
    for i in 0..cases {
        let text = gen_value(&mut rng, 0);
        let wrapped = format!("{}{}{}", rand_ws(&mut rng), text, rand_ws(&mut rng));
        if i % 3 == 0 {
            inputs.push(mutate(&mut rng, &wrapped));
        } else {
            inputs.push(units(&wrapped));
        }
    }
    let mut cases_txt = String::new();
    for t in &inputs {
        cases_txt += &js_literal(t);
        cases_txt.push('\n');
    }
    std::fs::write(format!("{dir}/cases.txt"), &cases_txt).unwrap();
    std::fs::write(format!("{dir}/oracle.js"), NODE_JS).unwrap();
    let r = std::process::Command::new("node").arg(format!("{dir}/oracle.js")).arg(format!("{dir}/cases.txt")).output().expect("node must be installed");
    assert!(r.status.success(), "node failed: {}", String::from_utf8_lossy(&r.stderr));
    let node: Vec<String> = String::from_utf8(r.stdout).unwrap().lines().map(String::from).collect();
    assert_eq!(node.len(), inputs.len());
    let (mut bad, mut accepted, mut rejected) = (0, 0, 0);
    let mut rust_lines = Vec::new();
    for (t, want) in inputs.iter().zip(&node) {
        let mut h = Heap::default();
        let mut got = String::new();
        match parse(&mut h, t, &JsonOptions::default(), &JsonLimits::default()) {
            Ok(v) => ser(&h, v, &mut got),
            Err(e) => {
                assert!(matches!(e.kind, ErrorKind::Syntax(_)), "unexpected non-syntax error {e:?}");
                assert!(e.offset <= t.len());
                got.push_str("ERR");
            }
        }
        if got == "ERR" { rejected += 1 } else { accepted += 1 }
        if &got != want {
            bad += 1;
            if bad <= 5 {
                println!("MISMATCH input={}\n  rust: {got}\n  node: {want}", js_literal(t));
            }
        }
        rust_lines.push(got);
    }
    std::fs::write(format!("{dir}/rust.txt"), rust_lines.join("\n") + "\n").unwrap();
    std::fs::write(format!("{dir}/node.txt"), node.join("\n") + "\n").unwrap();
    println!("oracle: {} cases ({accepted} accepted, {rejected} rejected), {bad} mismatches (seed {seed})", inputs.len());
    if bad > 0 {
        std::process::exit(1);
    }
    println!("oracle ok");
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("run") if a.len() == 4 => {
            let scale: u64 = a[3].parse().unwrap();
            assert!(matches!(scale, 1 | 10 | 100), "scale must be 1, 10 or 100");
            run(&a[2], scale);
        }
        Some("all") => all(a.get(2).map(String::as_str)),
        Some("caps") => caps(),
        Some("oracle") if a.len() >= 3 => oracle(&a[2], a.get(3).map_or(12345, |s| s.parse().unwrap()), a.get(4).map_or(3000, |s| s.parse().unwrap())),
        _ => {
            eprintln!("usage: {} run <workload> <1|10|100> | caps | all [workload] | oracle <dir> [seed] [cases]", a[0]);
            std::process::exit(2);
        }
    }
}
