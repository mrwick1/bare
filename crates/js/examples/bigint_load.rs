//! Load test for BigInt. One case at one scale per process, so the peak memory belongs to that run.
//!
//!   bigint_load CASE SCALE    CASE: small | heapmix | mul | divmod | tostring | i64sort | u64sort    SCALE: 1, 10, 100
//!
//! The host checks every result against a formula or a Rust model that does not use the engine's BigInt code. A wrong result
//! exits with status 1. It prints wall time, CPU time and peak resident memory of the process (from /proc), the heap
//! entries and the steps. `tools/js/bigint_load.sh` runs the matrix with caps.
//!
//!   small    SCALE*100000 loop turns of `acc += BigInt(i) * 3n % 1000003n`: all values are inline (no heap entry); checks the sum
//!   heapmix  SCALE*20000 turns of a 64-bit LCG in BigInt (`& 0xFFFF_FFFF_FFFF_FFFFn`): values above `i64` use the heap; checks
//!            the last state against Rust `u64` arithmetic
//!   mul      `a = 10^d / 7`, `b = 10^d / 13` with d = 1000*SCALE digits, then `a * b`: checks the digit count and the leading
//!            digits against the formula (the product is about 10^(2d) / 91), that `p / b - a` is 0, and `q * b + r === p`
//!   divmod   `q = a / b`, `r = a % b` with a of 2d digits and b of d digits: checks `q * b + r === a` and `0 <= r < b`
//!   tostring `7^k` (about 1000*SCALE digits), toString, parse back: checks the digit count, the last digit and the round trip
//!   i64sort  BigInt64Array of SCALE*50000 values written in a loop, then `sort()`: checks the ends, the middle and the sum
//!   u64sort  BigUint64Array of SCALE*50000 values above `i64::MAX`, then `fill` and `sort()` of the halves: the same checks

use bare_js::eval::*;
use bare_js::heap::Heap;
use bare_js::parse::{parse_script, Limits};
use bare_js::value::Value;
use std::time::Instant;
#[path = "common/exec.rs"]
mod exec;

fn die(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1)
}

fn proc_kib(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(key)).and_then(|r| r.split_whitespace().next()).and_then(|n| n.parse().ok()).unwrap_or(0)
}

fn cpu_ms() -> f64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let f: Vec<&str> = s.rsplit_once(')').map_or("", |x| x.1).split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|n| n.parse::<f64>().ok()).unwrap_or(0.0);
    (t(11) + t(12)) * 10.0
}

fn run_case(case: String, scale: usize) {
    let d = 1000 * scale;
    let n = scale * 50_000;
    let (src, want): (String, String) = match case.as_str() {
        "small" => {
            let mut acc: u128 = 0;
            for i in 0..scale as u128 * 100_000 {
                acc += (i * 3) % 1_000_003;
            }
            (format!("var acc = 0n; for (var i = 0; i < {}; i++) acc += BigInt(i) * 3n % 1000003n; acc.toString()", scale * 100_000), acc.to_string())
        }
        "heapmix" => {
            let mut x: u64 = 12345;
            for _ in 0..scale * 20_000 {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            }
            (format!("var x = 12345n; for (var i = 0; i < {}; i++) x = (x * 6364136223846793005n + 1442695040888963407n) & 0xFFFFFFFFFFFFFFFFn; x.toString()", scale * 20_000), x.to_string())
        }
        "mul" => {
            // 10^(2d) / 91 = 1.0989010989...e(2d-2): the product of the two floors is a little less, so 2d - 1 digits, and the
            // leading digits "1098901098901098901" are stable (the loss is about 10^d, far below the 20th digit).
            let src = format!(
                "var d = {d}; var T = 10n ** BigInt(d); var a = T / 7n, b = T / 13n; var p = a * b; var s = p.toString(); \
                 var q = p / b, r = p % b; s.length + ':' + s.slice(0, 19) + ':' + (q - a) + ':' + (r < b) + ':' + (q * b + r === p)"
            );
            (src, format!("{}:1098901098901098901:0:true:true", 2 * d - 1))
        }
        "divmod" => (
            format!("var d = {d}; var T = 10n ** BigInt(d); var b = T / 7n + 12345n; var a = (T * T) / 11n; var q = a / b, r = a % b; (r >= 0n && r < b) + ':' + (q * b + r === a) + ':' + q.toString().length"),
            // a ~ 10^(2d) / 11 = 9.09e(2d-2), b ~ 10^d / 7 = 1.43e(d-1): q ~ 6.36e(d-1) has d digits.
            format!("true:true:{d}"),
        ),
        "tostring" => {
            let k = (d as f64 * 1.1832) as u64;
            let digits = (k as f64 * 7f64.log10()).floor() as usize + 1;
            let last = [1, 7, 9, 3][(k % 4) as usize];
            (format!("var p = 7n ** {k}n; var s = p.toString(); s.length + ':' + s[s.length - 1] + ':' + (BigInt(s) === p)"), format!("{digits}:{last}:true"))
        }
        "i64sort" | "u64sort" => {
            let big = case == "u64sort";
            // The same values in Rust and in the script.
            let val = |i: usize| -> i128 { (i as i128 * 2654435761) % 1_000_003 - 500_000 };
            let mut v: Vec<i128> = (0..n).map(|i| if big { (1i128 << 63) + (val(i) + 500_000) * 7919 } else { val(i) }).collect();
            v.sort_unstable();
            let sum: i128 = v.iter().sum();
            let want = format!("{}:{}:{}:{}", v[0], v[n - 1], v[n >> 1], sum);
            let (ctor, form) = if big { ("BigUint64Array", "(2n ** 63n) + (BigInt((i * 2654435761) % 1000003) - 500000n + 500000n) * 7919n") } else { ("BigInt64Array", "BigInt((i * 2654435761) % 1000003) - 500000n") };
            // `i * 2654435761` stays below 2^53 for i < 3.3 million, so the Number product is exact.
            (format!("var n = {n}; var a = new {ctor}(n); for (var i = 0; i < n; i++) a[i] = {form}; a.sort(); var s = 0n; for (var i = 0; i < n; i++) s += a[i]; a[0] + ':' + a[n - 1] + ':' + a[n >> 1] + ':' + s"), want)
        }
        _ => die(format!("unknown case {case}")),
    };
    let t0 = Instant::now();
    let prog = parse_script(&src, Limits::default()).unwrap_or_else(|e| die(format!("parse {e}")));
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 4_000_000_000, ..Default::default() }).unwrap_or_else(|e| die(format!("setup {e:?}")));
    exec::set(&mut it);
    let t_setup = t0.elapsed();
    let t1 = Instant::now();
    let result = it.run().unwrap_or_else(|e| die(format!("run {e:?}")));
    let t_run = t1.elapsed();
    let got = it.display(result);
    if !matches!(result, Value::String(_)) || got != want {
        let cut = |s: &str| s.chars().take(80).collect::<String>();
        die(format!("{case} x{scale}: got {} != want {}", cut(&got), cut(&want)));
    }
    let before = it.heap.stats();
    let t2 = Instant::now();
    let freed = it.collect(&[]).unwrap_or_else(|e| die(format!("collect {e:?}")));
    let t_collect = t2.elapsed();
    let after = it.heap.stats();
    println!(
        "{case:8} x{scale:<3} checksum=ok run={:>9.1}ms collect={:>6.1}ms setup={:.1}ms | cpu(whole process)={:>9.1}ms peakRSS={}MiB | bigints_live_before={} after={} freed_objs={} heap_bytes={} | steps={}",
        t_run.as_secs_f64() * 1e3,
        t_collect.as_secs_f64() * 1e3,
        t_setup.as_secs_f64() * 1e3,
        cpu_ms(),
        proc_kib("VmHWM:") / 1024,
        before.live_bigints,
        after.live_bigints,
        freed.objects,
        before.bytes,
        it.stats().steps,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: bigint_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    std::thread::Builder::new().stack_size(if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 }).spawn(move || run_case(name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
