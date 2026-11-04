//! Standalone load and oracle driver for crates/js/src/{value,heap}.rs. No crates.
//!
//! Build: rustc --edition=2024 -C opt-level=3 tools/js/heap_load.rs -o <out>
//! Unit tests of both modules: rustc --edition=2024 --test tools/js/heap_load.rs -o <t> && <t>
//!
//! <out> run <workload> <scale: 1|10|100>   one workload in this process, one result line
//! <out> caps                               rejected-string allocation checks
//! <out> all [workload]                     every scale in its own child process, each with
//!                                          `ulimit -v` (address space) and `timeout`
//! <out> oracle <dir> [seed] [cases]        generate Node snippets, run them, compare output
//!
//! Workloads: lookup (property get/set, proto chain, elements), small (many small objects),
//! bigprops (one object with many keys), strings (interner hits and misses), churn (fixed live
//! set, repeated allocate + collect). Each asserts semantics, live counts and checksums.
//! Node runs only the generated snippets in <dir>; it never sees target bundles.

#[allow(dead_code)]
#[path = "../../crates/js/src/number.rs"]
mod number;
#[allow(dead_code)]
#[path = "../../crates/js/src/value.rs"]
mod value;
#[allow(dead_code)]
#[path = "../../crates/js/src/heap.rs"]
mod heap;

use heap::*;
use std::alloc::{GlobalAlloc, Layout, System};
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
    // /proc/self/stat fields 14 and 15 (utime, stime) in clock ticks; 100 Hz on Linux.
    let s = std::fs::read_to_string("/proc/self/stat").unwrap();
    let f: Vec<&str> = s[s.rfind(')').unwrap() + 2..].split(' ').collect();
    (f[11].parse::<f64>().unwrap() + f[12].parse::<f64>().unwrap()) / 100.0
}

fn status_kib(field: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    let l = s.lines().find(|l| l.starts_with(field)).unwrap();
    l.split_whitespace().nth(1).unwrap().parse().unwrap()
}

fn num(h: &Heap, o: ObjId, k: PropertyKey) -> f64 {
    match h.get(o, k).unwrap() {
        Get::Value(Value::Number(x)) => x,
        g => panic!("expected a number, got {g:?}"),
    }
}

fn units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// What a workload reports besides the common counters.
struct Out {
    ops: u64,
    checksum: u64,
    extra: String,
}

fn rss_now() -> u64 {
    status_kib("VmRSS:")
}

// ---------- workloads ----------

/// Property lookup and update: 64 own keys, a 3-deep prototype chain, a 1024-element array.
fn lookup(scale: u64) -> Out {
    let iters = 400_000 * scale;
    let mut h = Heap::default();
    let proto = h.new_object(None).unwrap();
    let pk: Vec<_> = (0..8).map(|i| h.key_from_str(&format!("p{i}")).unwrap()).collect();
    for (i, &k) in pk.iter().enumerate() {
        assert_eq!(h.set(proto, k, Value::Number(100.0 + i as f64)).unwrap(), Set::Done);
    }
    let mid = h.new_object(Some(proto)).unwrap();
    let o = h.new_object(Some(mid)).unwrap();
    let ok: Vec<_> = (0..64).map(|i| h.key_from_str(&format!("k{i}")).unwrap()).collect();
    for &k in &ok {
        h.set(o, k, Value::Number(0.0)).unwrap();
    }
    let arr = h.new_array(None).unwrap();
    for i in 0..1024 {
        h.set(arr, PropertyKey::Index(i), Value::Number(0.0)).unwrap();
    }
    let (mut inherited, mut expect_inherited) = (0f64, 0u64);
    let t = Instant::now();
    for i in 0..iters as usize {
        let k = ok[i & 63];
        let v = num(&h, o, k);
        h.set(o, k, Value::Number(v + 1.0)).unwrap();
        let e = PropertyKey::Index((i & 1023) as u32);
        let v = num(&h, arr, e);
        h.set(arr, e, Value::Number(v + 1.0)).unwrap();
        inherited += num(&h, o, pk[i & 7]);
        expect_inherited += 100 + (i & 7) as u64;
    }
    let loop_s = t.elapsed().as_secs_f64();
    let sum_o: f64 = ok.iter().map(|&k| num(&h, o, k)).sum();
    let sum_a: f64 = (0..1024).map(|i| num(&h, arr, PropertyKey::Index(i))).sum();
    assert_eq!(sum_o, iters as f64);
    assert_eq!(sum_a, iters as f64);
    assert_eq!(inherited as u64, expect_inherited);
    assert_eq!(h.own_keys(o).unwrap().len(), 64);
    assert_eq!(h.own_keys(arr).unwrap().len(), 1025);
    let st = h.stats();
    assert_eq!(st.live_objects, 4);
    Out {
        ops: iters * 5, // 3 gets + 2 sets per iteration
        checksum: expect_inherited,
        extra: format!("loop_ns_per_op={:.1} live_objects={} bytes={}", loop_s * 1e9 / (iters * 5) as f64, st.live_objects, st.bytes),
    }
}

/// Many small objects {x,y,z} held by one array; collect with and without the root.
fn small(scale: u64) -> Out {
    let n = 10_000 * scale as usize;
    let mut h = Heap::default();
    let ks: Vec<_> = ["x", "y", "z"].iter().map(|s| h.key_from_str(s).unwrap()).collect();
    let key_roots: Vec<Value> = ks.iter().map(|k| match k { PropertyKey::String(s) => Value::String(*s), _ => unreachable!() }).collect();
    let base = h.stats();
    let rss0 = rss_now();
    let holder = h.new_array(None).unwrap();
    let t = Instant::now();
    for i in 0..n {
        let o = h.new_object(None).unwrap();
        for (j, &k) in ks.iter().enumerate() {
            h.set(o, k, Value::Number((i * (j + 1)) as f64)).unwrap();
        }
        h.set(holder, PropertyKey::Index(i as u32), Value::Object(o)).unwrap();
    }
    let build_s = t.elapsed().as_secs_f64();
    let rss_built = rss_now();
    let st = h.stats();
    assert_eq!(st.live_objects, n + 1);
    let mut roots = vec![Value::Object(holder)];
    roots.extend(&key_roots);
    let t = Instant::now();
    let c = h.collect(&roots).unwrap();
    let live_gc_s = t.elapsed().as_secs_f64();
    assert_eq!(c, Collected::default()); // everything is reachable: nothing freed
    let mut sum = 0u64;
    for i in 0..n {
        let Get::Value(Value::Object(o)) = h.get(holder, PropertyKey::Index(i as u32)).unwrap() else { panic!() };
        for &k in &ks {
            sum += num(&h, o, k) as u64;
        }
    }
    let expect: u64 = (0..n as u64).map(|i| i * 6).sum();
    assert_eq!(sum, expect);
    let t = Instant::now();
    let c = h.collect(&key_roots).unwrap(); // drop the holder: all objects die
    let dead_gc_s = t.elapsed().as_secs_f64();
    assert_eq!(c.objects, n + 1);
    let after = h.stats();
    assert_eq!(after.live_objects, 0);
    assert_eq!(after.bytes, base.bytes); // accounting returns to the start: no leak
    let rss_freed = rss_now();
    let (cap_built, cap_freed) = (st, after);
    // Rebuild the same set: slots must be reused, capacity must not grow.
    let slots = after.object_slots;
    let holder = h.new_array(None).unwrap();
    for i in 0..n {
        let o = h.new_object(None).unwrap();
        h.set(holder, PropertyKey::Index(i as u32), Value::Object(o)).unwrap();
    }
    assert_eq!(h.stats().object_slots, slots);
    Out {
        ops: (n * 4) as u64,
        checksum: sum,
        extra: format!(
            "n={n} build_ms={:.0} gc_live_ms={:.1} gc_dead_ms={:.1} accounted_bytes_per_obj={} rss_bytes_per_obj={} object_slots={} rss_kib_built={} rss_kib_after_free={} rss_kib_start={} payload_cap_built={} payload_cap_after_free={} meta_cap_after_free={} slab_cap_kib_built={} slab_cap_kib_after_free={}",
            build_s * 1e3, live_gc_s * 1e3, dead_gc_s * 1e3, (st.bytes - base.bytes) / n,
            (rss_built - rss0) * 1024 / n as u64, slots, rss_built, rss_freed, rss0,
            cap_built.object_payload_capacity, cap_freed.object_payload_capacity, cap_freed.object_metadata_capacity,
            cap_built.slab_capacity_bytes / 1024, cap_freed.slab_capacity_bytes / 1024
        ),
    }
}

/// One object with many keys: define, read, delete half, re-add, check order, delete all.
fn bigprops(scale: u64) -> Out {
    let n = 10_000 * scale as usize;
    let mut h = Heap::default();
    let o = h.new_object(None).unwrap();
    let ks: Vec<_> = (0..n).map(|i| h.key_from_str(&format!("prop{i}")).unwrap()).collect();
    let roots: Vec<Value> = ks.iter().map(|k| match k { PropertyKey::String(s) => Value::String(*s), _ => unreachable!() }).collect();
    let empty = h.stats().bytes;
    let t = Instant::now();
    for (i, &k) in ks.iter().enumerate() {
        assert_eq!(h.set(o, k, Value::Number(i as f64)).unwrap(), Set::Done);
    }
    let define_s = t.elapsed().as_secs_f64();
    let st = h.stats();
    let t = Instant::now();
    let mut sum = 0u64;
    for &k in &ks {
        sum += num(&h, o, k) as u64;
    }
    let read_s = t.elapsed().as_secs_f64();
    assert_eq!(sum, (n as u64) * (n as u64 - 1) / 2);
    let t = Instant::now();
    for i in (0..n).step_by(2) {
        assert!(h.delete(o, ks[i]).unwrap());
    }
    let delete_s = t.elapsed().as_secs_f64();
    assert_eq!(h.own_keys(o).unwrap().len(), n / 2);
    for i in (0..n).step_by(2) {
        assert!(!h.has_own_property(o, ks[i]).unwrap());
        assert!(h.has_own_property(o, ks[i + 1]).unwrap());
    }
    for i in (0..n).step_by(2) {
        h.set(o, ks[i], Value::Number(i as f64)).unwrap();
    }
    let keys = h.own_keys(o).unwrap();
    assert_eq!(keys.len(), n);
    // Odd keys kept their place, then the re-added even keys in the order they were added.
    let want: Vec<_> = (1..n).step_by(2).chain((0..n).step_by(2)).map(|i| ks[i]).collect();
    assert_eq!(keys, want);
    // Integer keys on the same object stay in numeric order, in front of the strings.
    for i in [5u32, 3, 4_000_000, 1] {
        h.set(o, PropertyKey::Index(i), Value::Number(0.0)).unwrap();
    }
    let keys = h.own_keys(o).unwrap();
    assert_eq!(&keys[..4], &[PropertyKey::Index(1), PropertyKey::Index(3), PropertyKey::Index(5), PropertyKey::Index(4_000_000)]);
    for i in [5u32, 3, 4_000_000, 1] {
        assert!(h.delete(o, PropertyKey::Index(i)).unwrap());
    }
    for &k in &ks {
        assert!(h.delete(o, k).unwrap());
    }
    assert!(h.own_keys(o).unwrap().is_empty());
    let c = h.collect(&roots).unwrap();
    assert_eq!(c.objects, 1);
    Out {
        ops: (n * 5 + n / 2 * 2) as u64,
        checksum: sum,
        extra: format!(
            "n={n} define_ns_per_key={:.0} read_ns_per_key={:.0} delete_ns_per_key={:.0} accounted_bytes_per_key={}",
            define_s * 1e9 / n as f64, read_s * 1e9 / n as f64, delete_s * 1e9 / (n / 2) as f64, (st.bytes - empty) / n
        ),
    }
}

/// Interner: first sight (miss) and reuse (hit) of D distinct strings, plus handle equality.
fn strings(scale: u64) -> Out {
    let d = 2_000 * scale as usize;
    let hits = 500_000 * scale as usize;
    let mut h = Heap::default();
    let texts: Vec<Vec<u16>> = (0..d).map(|i| units(&format!("string-number-{i}-é😀"))).collect();
    let t = Instant::now();
    let ids: Vec<StrId> = texts.iter().map(|u| h.intern_units(u).unwrap()).collect();
    let miss_s = t.elapsed().as_secs_f64();
    assert_eq!(h.stats().live_strings, d + 1); // + "length"
    // Baseline: the same access pattern over the caller's own buffers, no heap call.
    let t = Instant::now();
    let mut base = 0u64;
    for i in 0..hits {
        base += texts[i.wrapping_mul(7919) % d].len() as u64;
    }
    let base_s = t.elapsed().as_secs_f64();
    assert!(base > 0);
    let t = Instant::now();
    let (mut sum, mut expect) = (0u64, 0u64);
    for i in 0..hits {
        let j = i.wrapping_mul(7919) % d;
        let id = h.intern_units(&texts[j]).unwrap();
        assert_eq!(id, ids[j]);
        sum += h.str_len(id).unwrap() as u64;
        expect += texts[j].len() as u64;
    }
    let hit_s = t.elapsed().as_secs_f64();
    assert_eq!(sum, expect);
    assert_eq!(h.stats().live_strings, d + 1); // reuse made no new string
    // Concatenation, then equality by handle.
    let a = h.intern_str("ab").unwrap();
    let b = h.intern_str("cd").unwrap();
    let abcd = h.intern_str("abcd").unwrap();
    for _ in 0..1000 {
        assert_eq!(h.concat(a, b).unwrap(), abcd);
    }
    // Only a quarter is rooted: the rest must die, and the interner must forget them.
    let keep: Vec<Value> = ids.iter().step_by(4).map(|&s| Value::String(s)).collect();
    let c = h.collect(&keep).unwrap();
    assert_eq!(c.strings, d - keep.len() + 3);
    assert_eq!(h.stats().live_strings, keep.len() + 1);
    for j in (0..d).step_by(4) {
        assert_eq!(h.intern_units(&texts[j]).unwrap(), ids[j]);
    }
    let reborn = h.intern_units(&texts[1]).unwrap();
    assert_ne!(reborn, ids[1]); // the dead string is a new string now
    assert!(h.str_len(ids[1]).is_err());
    Out {
        ops: (d + hits) as u64,
        checksum: sum,
        extra: format!("distinct={d} miss_ns={:.0} hit_ns={:.0} baseline_ns={:.0} live_strings={}", miss_s * 1e9 / d as f64, hit_s * 1e9 / hits as f64, base_s * 1e9 / hits as f64, h.stats().live_strings),
    }
}

/// Fixed live set; each round allocates temporary objects and strings, then collects.
fn churn(scale: u64) -> Out {
    let rounds = 20 * scale as usize;
    let temps = 10_000usize;
    let live = 1_000usize;
    let mut h = Heap::default();
    let kn = h.key_from_str("n").unwrap();
    let ks = h.key_from_str("s").unwrap();
    let kt = h.key_from_str("t").unwrap();
    let mut base_roots: Vec<Value> = [kn, ks, kt].iter().map(|k| match k { PropertyKey::String(s) => Value::String(*s), _ => unreachable!() }).collect();
    let holder = h.new_array(None).unwrap();
    base_roots.push(Value::Object(holder));
    for i in 0..live {
        let o = h.new_object(None).unwrap();
        let s = h.intern_str(&format!("live-{i}")).unwrap();
        h.set(o, kn, Value::Number(i as f64)).unwrap();
        h.set(o, ks, Value::String(s)).unwrap();
        h.set(holder, PropertyKey::Index(i as u32), Value::Object(o)).unwrap();
    }
    let live_sum: u64 = (0..live as u64).sum();
    let (mut first_bytes, mut first_rss, mut max_pause, mut total_pause) = (0, 0, 0f64, 0f64);
    let mut slots_after_first = 0;
    let t = Instant::now();
    for r in 0..rounds {
        for i in 0..temps {
            let o = h.new_object(None).unwrap();
            let s = h.intern_str(&format!("t-{r}-{i}")).unwrap();
            h.set(o, kt, Value::String(s)).unwrap();
            h.set(o, kn, Value::Number(i as f64)).unwrap();
        }
        let p = Instant::now();
        let c = h.collect(&base_roots).unwrap();
        let pause = p.elapsed().as_secs_f64();
        max_pause = max_pause.max(pause);
        total_pause += pause;
        assert_eq!(c.objects, temps);
        assert_eq!(c.strings, temps);
        let st = h.stats();
        assert_eq!((st.live_objects, st.live_strings), (live + 1, live + 4)); // + n, s, t, length -> see below
        if r == 0 {
            first_bytes = st.bytes;
            first_rss = rss_now();
            slots_after_first = st.object_slots;
        }
        assert_eq!(st.bytes, first_bytes); // no drift in accounted live bytes
        assert_eq!(st.object_slots, slots_after_first); // slots are reused, never grow
        if r % 16 == 0 || r == rounds - 1 {
            let mut sum = 0u64;
            for i in 0..live {
                let Get::Value(Value::Object(o)) = h.get(holder, PropertyKey::Index(i as u32)).unwrap() else { panic!() };
                sum += num(&h, o, kn) as u64;
                let Get::Value(Value::String(s)) = h.get(o, PropertyKey::String(match ks { PropertyKey::String(s) => s, _ => unreachable!() })).unwrap() else { panic!() };
                assert_eq!(h.to_string_lossy(s).unwrap(), format!("live-{i}"));
            }
            assert_eq!(sum, live_sum);
        }
    }
    let wall = t.elapsed().as_secs_f64();
    let end = h.stats();
    Out {
        ops: (rounds * temps) as u64,
        checksum: live_sum,
        extra: format!(
            "rounds={rounds} temps_per_round={temps} alloc_ns_per_obj={:.0} gc_avg_us={:.0} gc_max_us={:.0} live_objects={} object_slots={} live_strings={} string_slots={} accounted_bytes={} payload_cap={} meta_cap={} slab_cap_kib={} rss_kib_after_round1={} rss_growth_kib={}",
            (wall - total_pause) * 1e9 / (rounds * temps) as f64, total_pause * 1e6 / rounds as f64, max_pause * 1e6,
            end.live_objects, end.object_slots, end.live_strings, end.string_slots, end.bytes, end.object_payload_capacity, end.object_metadata_capacity, end.slab_capacity_bytes / 1024, first_rss, rss_now() as i64 - first_rss as i64
        ),
    }
}

/// Build many small objects, keep LIVE_PCT percent (env, default 10) scattered through them, and
/// collect twice. Reports RSS and slab capacity around each collection, and that survivors read right.
fn frac(scale: u64) -> Out {
    let pct: usize = std::env::var("LIVE_PCT").ok().map_or(10, |s| s.parse().unwrap());
    let n = 10_000 * scale as usize;
    let mut h = Heap::default();
    let ks: Vec<_> = ["x", "y", "z"].iter().map(|s| h.key_from_str(s).unwrap()).collect();
    let mut roots: Vec<Value> = ks.iter().map(|k| match k { PropertyKey::String(s) => Value::String(*s), _ => unreachable!() }).collect();
    let mut kept = Vec::new();
    for i in 0..n {
        let o = h.new_object(None).unwrap();
        for (j, &k) in ks.iter().enumerate() {
            h.set(o, k, Value::Number((i * (j + 1)) as f64)).unwrap();
        }
        if (i * 7919) % 100 < pct {
            kept.push((o, i));
            roots.push(Value::Object(o));
        }
    }
    let built = (rss_now(), h.stats());
    let t = Instant::now();
    let c1 = h.collect(&roots).unwrap();
    let gc1 = t.elapsed().as_secs_f64();
    let first = (rss_now(), h.stats());
    let t = Instant::now();
    let c2 = h.collect(&roots).unwrap();
    let gc2 = t.elapsed().as_secs_f64();
    let second = (rss_now(), h.stats());
    assert_eq!(c1.objects, n - kept.len());
    assert_eq!(c2.objects, 0);
    let mut sum = 0u64;
    for &(o, i) in &kept {
        assert_eq!(num(&h, o, ks[1]) as usize, i * 2);
        sum += i as u64;
    }
    // Reuse: a rebuild of the dead part must not add slots.
    let slots = second.1.object_slots;
    for _ in 0..n - kept.len() {
        h.new_object(None).unwrap();
    }
    assert_eq!(h.stats().object_slots, slots);
    Out {
        ops: n as u64,
        checksum: sum,
        extra: format!(
            "pct={pct} kept={} gc1_ms={:.1} gc2_ms={:.1} rss_built={} rss_gc1={} rss_gc2={} payload_cap_built={} payload_cap_gc1={} payload_cap_gc2={} slab_cap_kib_gc2={} live_bytes={}",
            kept.len(), gc1 * 1e3, gc2 * 1e3, built.0, first.0, second.0, built.1.object_payload_capacity, first.1.object_payload_capacity,
            second.1.object_payload_capacity, second.1.slab_capacity_bytes / 1024, second.1.bytes
        ),
    }
}

const WORKLOADS: [&str; 6] = ["lookup", "small", "bigprops", "strings", "churn", "frac"];

fn run(w: &str, scale: u64) {
    let (allocs0, rss0, cpu0) = (ALLOCS.load(Relaxed), rss_now(), cpu_secs());
    let t = Instant::now();
    let out = match w {
        "lookup" => lookup(scale),
        "small" => small(scale),
        "bigprops" => bigprops(scale),
        "strings" => strings(scale),
        "churn" => churn(scale),
        "frac" => frac(scale),
        _ => panic!("unknown workload {w}"),
    };
    let wall = t.elapsed().as_secs_f64();
    println!(
        "workload={w} scale={scale}x ops={} wall={:.3}s cpu={:.2}s ns_per_op={:.0} peak_rss_kib={} rss_start_kib={} rust_allocs={} checksum={} {} ok",
        out.ops, wall, cpu_secs() - cpu0, wall * 1e9 / out.ops as f64, status_kib("VmHWM:"), rss0,
        ALLOCS.load(Relaxed) - allocs0, out.checksum, out.extra
    );
}

/// Every scale of every workload in its own process: 4 GiB of address space and 600 s.
fn all(only: Option<&str>) {
    let me = std::env::current_exe().unwrap();
    let mut failed = false;
    for w in WORKLOADS.iter().filter(|w| only.is_none_or(|o| o == **w)) {
        for scale in [1, 10, 100] {
            let cmd = format!("ulimit -v 4194304; exec timeout 600 '{}' run {w} {scale}", me.display());
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

// ---------- Node oracle ----------

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

const KEYS: [&str; 16] = ["0", "1", "2", "3", "7", "1500", "4294967294", "4294967295", "01", "-1", "a", "b", "c", "length", "@s0", "@s1"];

#[derive(Clone, Copy)]
struct Desc {
    v: Option<f64>,
    w: Option<bool>,
    g: Option<bool>, // Some(true) = getter G, Some(false) = get: undefined
    s: Option<bool>,
    e: Option<bool>,
    c: Option<bool>,
}

#[derive(Clone, Copy)]
enum Op {
    Reset,
    Define(usize, usize, Desc),
    Set(usize, usize, f64),
    Delete(usize, usize),
    Get(usize, usize),
    Proto(usize, Option<usize>),
    Prevent(usize),
    Dump(usize),
    Gc, // Rust only: collect with the pool as roots. Node ignores it.
}

fn gen_ops(rng: &mut Rng, seqs: usize) -> Vec<Op> {
    let mut ops = Vec::new();
    for _ in 0..seqs {
        ops.push(Op::Reset);
        for _ in 0..40 {
            let (o, k) = (rng.below(3), rng.below(KEYS.len()));
            let val = rng.below(10) as f64;
            ops.push(match rng.below(20) {
                0..=6 => {
                    let mut d = Desc { v: None, w: None, g: None, s: None, e: None, c: None };
                    match rng.below(4) {
                        0 | 1 => {
                            d.v = rng.chance(80).then_some(val);
                            d.w = rng.chance(50).then(|| rng.chance(50));
                        }
                        2 => {
                            d.g = rng.chance(60).then(|| rng.chance(70));
                            d.s = rng.chance(60).then(|| rng.chance(70));
                            if d.g.is_none() && d.s.is_none() {
                                d.g = Some(true);
                            }
                        }
                        _ => {}
                    }
                    d.e = rng.chance(50).then(|| rng.chance(50));
                    d.c = rng.chance(50).then(|| rng.chance(50));
                    // Sometimes an invalid array length (RangeError) when the key is "length".
                    if o == 2 && KEYS[k] == "length" && rng.chance(25) {
                        d.v = Some([-1.0, 1.5, 4294967296.0][rng.below(3)]);
                    }
                    if o == 2 && KEYS[k] == "length" && (d.g.is_some() || d.s.is_some()) {
                        d.g = None;
                        d.s = None;
                    }
                    Op::Define(o, k, d)
                }
                7..=10 => Op::Set(o, k, if o == 2 && KEYS[k] == "length" { rng.below(8) as f64 } else { val }),
                11..=13 => Op::Delete(o, k),
                14..=15 => Op::Get(o, k),
                16 => Op::Proto(o, if rng.chance(25) { None } else { Some(rng.below(3)) }),
                17 => Op::Prevent(o),
                18 => Op::Gc,
                _ => Op::Dump(rng.below(3)),
            });
        }
        ops.extend([Op::Dump(0), Op::Dump(1), Op::Dump(2)]);
    }
    ops
}

fn js_num(x: f64) -> String {
    number_to_string(x)
}

fn ops_to_js(ops: &[Op]) -> String {
    let flag = |b: Option<bool>| b.map_or("null".to_string(), |b| (b as u8).to_string());
    let items: Vec<String> = ops
        .iter()
        .filter_map(|op| {
            Some(match *op {
                Op::Reset => "[\"reset\"]".into(),
                Op::Define(o, k, d) => format!(
                    "[\"def\",{o},{k},{{\"v\":{},\"w\":{},\"g\":{},\"s\":{},\"e\":{},\"c\":{}}}]",
                    d.v.map_or("null".into(), js_num), flag(d.w), flag(d.g), flag(d.s), flag(d.e), flag(d.c)
                ),
                Op::Set(o, k, v) => format!("[\"set\",{o},{k},{}]", js_num(v)),
                Op::Delete(o, k) => format!("[\"del\",{o},{k}]"),
                Op::Get(o, k) => format!("[\"get\",{o},{k}]"),
                Op::Proto(o, t) => format!("[\"proto\",{o},{}]", t.map_or(-1, |t| t as i32)),
                Op::Prevent(o) => format!("[\"prevent\",{o}]"),
                Op::Dump(o) => format!("[\"dump\",{o}]"),
                Op::Gc => return None,
            })
        })
        .collect();
    let keys: Vec<String> = KEYS.iter().map(|k| format!("{k:?}")).collect();
    format!(
        r#"const KEYS=[{}];
const S0=Symbol('s0'),S1=Symbol('s1');
const G=function(){{return 'G'}},S=function(v){{}};
let P;
const out=[];
const key=i=>KEYS[i]==='@s0'?S0:KEYS[i]==='@s1'?S1:KEYS[i];
const kn=k=>k===S0?'@s0':k===S1?'@s1':k;
const fl=b=>b?1:0;
function dump(i){{
  const o=P[i];
  const ks=Reflect.ownKeys(o).map(k=>{{
    const d=Reflect.getOwnPropertyDescriptor(o,k);
    if('value' in d) return kn(k)+'=D'+String(d.value)+','+fl(d.writable)+fl(d.enumerable)+fl(d.configurable);
    return kn(k)+'=A'+(d.get===G?'G':d.get===undefined?'-':'?')+(d.set===S?'S':d.set===undefined?'-':'?')+','+fl(d.enumerable)+fl(d.configurable);
  }});
  const pr=Reflect.getPrototypeOf(o);
  out.push('dump '+i+' ext='+Reflect.isExtensible(o)+' proto='+(pr===null?'null':P.indexOf(pr))+' ['+ks.join(' ')+']');
}}
function run(op){{
  const [t,a,b,c]=op;
  if(t==='reset'){{P=[Object.setPrototypeOf({{}},null),Object.setPrototypeOf({{}},null),Object.setPrototypeOf([],null)];out.push('reset');return;}}
  try{{
    if(t==='def'){{
      const d={{}};
      if(c.v!==null)d.value=c.v;
      if(c.w!==null)d.writable=!!c.w;
      if(c.g!==null)d.get=c.g?G:undefined;
      if(c.s!==null)d.set=c.s?S:undefined;
      if(c.e!==null)d.enumerable=!!c.e;
      if(c.c!==null)d.configurable=!!c.c;
      out.push('def '+Reflect.defineProperty(P[a],key(b),d));
    }}
    else if(t==='set')out.push('set '+Reflect.set(P[a],key(b),c));
    else if(t==='del')out.push('del '+Reflect.deleteProperty(P[a],key(b)));
    else if(t==='get')out.push('get '+String(Reflect.get(P[a],key(b))));
    else if(t==='proto')out.push('proto '+Reflect.setPrototypeOf(P[a],b<0?null:P[b]));
    else if(t==='prevent')out.push('prevent '+Reflect.preventExtensions(P[a]));
    else if(t==='dump')dump(a);
  }}catch(e){{out.push(t+' throws '+e.name)}}
}}
for(const op of [{}])run(op);
console.log(out.join('\n'));
"#,
        keys.join(","),
        items.join(",\n")
    )
}

/// The same ops on the Rust heap, printed in the same format as the Node program.
fn ops_in_rust(ops: &[Op]) -> Vec<String> {
    struct World {
        h: Heap,
        pool: [ObjId; 3],
        syms: [SymId; 2],
        g: ObjId,
        s: ObjId,
    }
    impl World {
        fn new() -> World {
            let mut h = Heap::default();
            let pool = [h.new_object(None).unwrap(), h.new_object(None).unwrap(), h.new_array(None).unwrap()];
            let d0 = h.intern_str("s0").unwrap();
            let d1 = h.intern_str("s1").unwrap();
            let syms = [h.new_symbol(Some(d0)).unwrap(), h.new_symbol(Some(d1)).unwrap()];
            let (g, s) = (h.new_object(None).unwrap(), h.new_object(None).unwrap());
            World { h, pool, syms, g, s }
        }
        fn key(&mut self, i: usize) -> PropertyKey {
            match KEYS[i] {
                "@s0" => PropertyKey::Symbol(self.syms[0]),
                "@s1" => PropertyKey::Symbol(self.syms[1]),
                s => self.h.key_from_str(s).unwrap(),
            }
        }
        fn name(&self, k: PropertyKey) -> String {
            match k {
                PropertyKey::Index(i) => i.to_string(),
                PropertyKey::String(s) => self.h.to_string_lossy(s).unwrap(),
                PropertyKey::Symbol(s) => format!("@s{}", self.syms.iter().position(|x| *x == s).unwrap()),
            }
        }
        fn fl(b: bool) -> u8 {
            b as u8
        }
        fn dump(&self, i: usize) -> String {
            let o = self.pool[i];
            let ks: Vec<String> = self
                .h
                .own_keys(o)
                .unwrap()
                .into_iter()
                .map(|k| {
                    let body = match self.h.get_own_property(o, k).unwrap().unwrap() {
                        Property::Data { value, writable, enumerable, configurable } => {
                            let v = match value {
                                Value::Number(x) => number_to_string(x),
                                Value::Undefined => "undefined".into(),
                                v => panic!("{v:?}"),
                            };
                            format!("D{v},{}{}{}", Self::fl(writable), Self::fl(enumerable), Self::fl(configurable))
                        }
                        Property::Accessor { get, set, enumerable, configurable } => {
                            let g = match get { Some(x) if x == self.g => 'G', None => '-', _ => '?' };
                            let s = match set { Some(x) if x == self.s => 'S', None => '-', _ => '?' };
                            format!("A{g}{s},{}{}", Self::fl(enumerable), Self::fl(configurable))
                        }
                    };
                    format!("{}={}", self.name(k), body)
                })
                .collect();
            let proto = match self.h.get_prototype_of(o).unwrap() {
                None => "null".to_string(),
                Some(p) => self.pool.iter().position(|x| *x == p).unwrap().to_string(),
            };
            format!("dump {i} ext={} proto={proto} [{}]", self.h.is_extensible(o).unwrap(), ks.join(" "))
        }
    }
    let mut out = Vec::new();
    let mut w = World::new();
    for op in ops {
        match *op {
            Op::Reset => {
                w = World::new();
                out.push("reset".to_string());
            }
            Op::Define(o, k, d) => {
                let key = w.key(k);
                let (g, s) = (w.g, w.s);
                let pd = PropertyDescriptor {
                    value: d.v.map(Value::Number),
                    writable: d.w,
                    get: d.g.map(|x| x.then_some(g)),
                    set: d.s.map(|x| x.then_some(s)),
                    enumerable: d.e,
                    configurable: d.c,
                };
                out.push(match w.h.define_own_property(w.pool[o], key, &pd) {
                    Ok(b) => format!("def {b}"),
                    Err(HeapError::InvalidArrayLength) => "def throws RangeError".into(),
                    Err(e) => panic!("{e:?}"),
                });
            }
            Op::Set(o, k, v) => {
                let key = w.key(k);
                out.push(match w.h.set(w.pool[o], key, Value::Number(v)).unwrap() {
                    Set::Done | Set::Setter(_) => "set true".to_string(), // Node calls S, which does nothing
                    Set::Rejected => "set false".to_string(),
                });
            }
            Op::Delete(o, k) => {
                let key = w.key(k);
                out.push(format!("del {}", w.h.delete(w.pool[o], key).unwrap()));
            }
            Op::Get(o, k) => {
                let key = w.key(k);
                out.push(format!("get {}", match w.h.get(w.pool[o], key).unwrap() {
                    Get::Value(Value::Number(x)) => number_to_string(x),
                    Get::Value(Value::Undefined) => "undefined".into(),
                    Get::Value(v) => panic!("{v:?}"),
                    Get::Accessor { getter: Some(_) } => "G".into(), // Node calls G
                    Get::Accessor { getter: None } => "undefined".into(),
                }));
            }
            Op::Proto(o, t) => {
                let ok = w.h.set_prototype_of(w.pool[o], t.map(|t| w.pool[t])).unwrap();
                out.push(format!("proto {ok}"));
            }
            Op::Prevent(o) => {
                w.h.prevent_extensions(w.pool[o]).unwrap();
                out.push("prevent true".into());
            }
            Op::Dump(o) => out.push(w.dump(o)),
            Op::Gc => {
                let mut roots: Vec<Value> = w.pool.iter().map(|&o| Value::Object(o)).collect();
                roots.extend(w.syms.iter().map(|&s| Value::Symbol(s)));
                roots.extend([Value::Object(w.g), Value::Object(w.s)]);
                w.h.collect(&roots).unwrap();
            }
        }
    }
    out
}

fn bits_hex(x: f64) -> String {
    if x.is_nan() { "nan".into() } else { format!("{:016x}", x.to_bits()) }
}

/// Node source plus the Rust answers for number->string, string->number and primitive equality.
fn conversions(rng: &mut Rng, cases: usize) -> (String, Vec<String>) {
    let mut js = String::new();
    let mut want = Vec::new();
    // number -> string
    let mut hexes = Vec::new();
    for i in 0..cases {
        let x = match i % 5 {
            0 => f64::from_bits(rng.next()),
            1 => (rng.next() % 2_000_000) as f64 / 10f64.powi(rng.below(12) as i32),
            2 => (rng.next() as i64 >> rng.below(60)) as f64,
            3 => (rng.next() % 1000) as f64 * 10f64.powi(rng.below(40) as i32 - 20),
            _ => f64::from_bits(rng.next() & 0x800F_FFFF_FFFF_FFFF), // subnormals
        };
        want.push(format!("n2s {:016x} {}", x.to_bits(), number_to_string(x)));
        hexes.push(format!("\"{:016x}\"", x.to_bits()));
    }
    js += &format!("{{const dv=new DataView(new ArrayBuffer(8));for(const h of [{}]){{dv.setBigUint64(0,BigInt('0x'+h));out.push('n2s '+h+' '+String(dv.getFloat64(0)))}}}}\n", hexes.join(","));
    // string -> number
    let atoms = ["0", "1", "9", "7", ".", "e", "E", "+", "-", "x", "X", "b", "o", "a", "F", "Infinity", " ", "\t", "\u{a0}", "\u{feff}", "_", "n", "00", "5e3", "0x", "0b", "0o", "inf", "NaN", "\u{2028}", "١"];
    let mut lits = Vec::new();
    for i in 0..cases {
        let mut s: Vec<u16> = Vec::new();
        if i % 4 == 0 {
            s.extend(units(["0x", "0b", "0o"][rng.below(3)]));
            for _ in 0..rng.below(40) {
                s.extend(units(&format!("{:x}", rng.below(16))));
            }
        } else {
            for _ in 0..rng.below(7) {
                s.extend(units(atoms[rng.below(atoms.len())]));
            }
        }
        if rng.chance(5) {
            s.push(0xD800);
        }
        let lit: String = s.iter().map(|u| format!("\\u{u:04x}")).collect();
        lits.push(format!("\"{lit}\""));
        want.push(format!("s2n {i} {}", bits_hex(string_to_number(&s))));
    }
    js += &format!("{{const dv=new DataView(new ArrayBuffer(8));[{}].forEach((s,i)=>{{const x=Number(s);let r='nan';if(!Number.isNaN(x)){{dv.setFloat64(0,x);r=dv.getBigUint64(0).toString(16).padStart(16,'0')}}out.push('s2n '+i+' '+r)}})}}\n", lits.join(","));
    (js, want)
}

/// Equality and conversion tables over a pool of primitives, including NaN and -0.
fn equality() -> (String, Vec<String>) {
    let pool = ["undefined", "null", "true", "false", "0", "-0", "NaN", "1", "-1", "Infinity", "\"\"", "\"0\"", "\"1\"", "\"a\"", "\" 1 \"", "\"0x10\"", "\"1e3\"", "\"-0\"", "\"Infinity\""];
    let mut h = Heap::default();
    let vals: Vec<Value> = pool
        .iter()
        .map(|p| match *p {
            "undefined" => Value::Undefined,
            "null" => Value::Null,
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            "NaN" => Value::Number(f64::NAN),
            "-0" => Value::Number(-0.0),
            "Infinity" => Value::Number(f64::INFINITY),
            s if s.starts_with('"') => Value::String(h.intern_str(&s[1..s.len() - 1]).unwrap()),
            s => Value::Number(s.parse().unwrap()),
        })
        .collect();
    let mut want = Vec::new();
    for (i, &a) in vals.iter().enumerate() {
        let sn = h.to_string(a).unwrap();
        want.push(format!("conv {i} bool={} num={} str={}", h.to_boolean(a).unwrap(), {
            let n = h.to_number(a).unwrap();
            if n.is_nan() { "nan".into() } else { number_to_string(n) + if n == 0.0 && n.is_sign_negative() { "neg" } else { "" } }
        }, h.to_string_lossy(sn).unwrap()));
        for (j, &b) in vals.iter().enumerate() {
            want.push(format!("eq {i} {j} {} {} {} {}", h.loose_equals(a, b).unwrap(), a.strict_equals(b), a.same_value(b), a.same_value_zero(b)));
        }
    }
    let js = format!(
        r#"{{const P=[{}];
P.forEach((a,i)=>{{
  const n=Number(a);
  out.push('conv '+i+' bool='+(!!a)+' num='+(Number.isNaN(n)?'nan':String(n)+(Object.is(n,-0)?'neg':''))+' str='+String(a));
  P.forEach((b,j)=>out.push('eq '+i+' '+j+' '+(a==b)+' '+(a===b)+' '+Object.is(a,b)+' '+[a].includes(b)));
}})}}
"#,
        pool.join(",")
    );
    (js, want)
}

fn oracle(dir: &str, seed: u64, cases: usize) {
    std::fs::create_dir_all(dir).unwrap();
    let mut rng = Rng(seed | 1);
    let (cjs, mut want) = conversions(&mut rng, cases);
    let (ejs, ewant) = equality();
    want.extend(ewant);
    let ops = gen_ops(&mut rng, cases / 10 + 1);
    let ojs = ops_to_js(&ops);
    want.extend(ops_in_rust(&ops));
    // The op program prints its own join; run the pieces as one file with a shared `out`.
    let (ohead, otail) = ojs.split_once("for(const op of").unwrap();
    let prog = format!("{ohead}{cjs}{ejs}for(const op of{otail}");
    let path = format!("{dir}/oracle.js");
    std::fs::write(&path, &prog).unwrap();
    let r = std::process::Command::new("node").arg(&path).output().expect("node must be installed");
    assert!(r.status.success(), "node failed: {}", String::from_utf8_lossy(&r.stderr));
    let got: Vec<String> = String::from_utf8(r.stdout).unwrap().lines().map(String::from).collect();
    std::fs::write(format!("{dir}/rust.txt"), want.join("\n") + "\n").unwrap();
    std::fs::write(format!("{dir}/node.txt"), got.join("\n") + "\n").unwrap();
    // Node prints conversions first, then equality, then ops; sort both sides by section.
    let key = |l: &String| l.split(' ').next().unwrap().to_string();
    let mut mismatches = 0;
    let sections = ["n2s", "s2n", "conv", "eq"];
    let by = |lines: &Vec<String>, s: &str| lines.iter().filter(|l| key(l) == s).cloned().collect::<Vec<_>>();
    for s in sections {
        let (a, b) = (by(&want, s), by(&got, s));
        let bad = a.iter().zip(&b).filter(|(x, y)| x != y).count() + a.len().abs_diff(b.len());
        for (x, y) in a.iter().zip(&b).filter(|(x, y)| x != y).take(3) {
            println!("MISMATCH {s}\n  rust: {x}\n  node: {y}");
        }
        println!("section {s}: {} cases, {} mismatches", a.len(), bad);
        mismatches += bad;
    }
    let op_kinds = ["reset", "def", "set", "del", "get", "proto", "prevent", "dump"];
    let ow: Vec<_> = want.iter().filter(|l| op_kinds.contains(&key(l).as_str())).collect();
    let og: Vec<_> = got.iter().filter(|l| op_kinds.contains(&key(l).as_str())).collect();
    let bad = ow.iter().zip(&og).filter(|(x, y)| x != y).count() + ow.len().abs_diff(og.len());
    for (x, y) in ow.iter().zip(&og).filter(|(x, y)| x != y).take(3) {
        println!("MISMATCH ops\n  rust: {x}\n  node: {y}");
    }
    println!("section ops: {} lines, {} mismatches (seed {seed})", ow.len(), bad);
    mismatches += bad;
    if mismatches > 0 {
        std::process::exit(1);
    }
    println!("oracle ok");
}

fn check_rejected_string_allocations() {
    let mut h = Heap::new(HeapLimits { max_bytes: 512, ..Default::default() }).unwrap();
    let units = vec![b'x' as u16; 65536];
    let text = "x".repeat(65536);
    for utf8 in [false, true] {
        let before = ALLOCS.load(Relaxed);
        let result = if utf8 { h.intern_str(&text) } else { h.intern_units(&units) };
        let allocations = ALLOCS.load(Relaxed) - before;
        assert_eq!(result.err(), Some(HeapError::Limit(Limit::Bytes)));
        assert_eq!(allocations, 0, "rejected utf8={utf8} string allocated before its budget check");
    }
    println!("rejected string allocations: 0");
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
        Some("caps") => check_rejected_string_allocations(),
        Some("oracle") if a.len() >= 3 => oracle(&a[2], a.get(3).map_or(12345, |s| s.parse().unwrap()), a.get(4).map_or(2000, |s| s.parse().unwrap())),
        _ => {
            eprintln!("usage: {} run <workload> <1|10|100> | caps | all [workload] | oracle <dir> [seed] [cases]", a[0]);
            std::process::exit(2);
        }
    }
}
