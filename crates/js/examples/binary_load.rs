//! Load test for byte buffers, typed arrays, DataView and the text codecs. One case at one scale per process, so the peak
//! memory belongs to that run.
//!
//!   binary_load CASE SCALE    CASE: copy | scan | scanarr | codec | stream | indexof | f64from | i32sort | dataview    SCALE: 1, 10, 100
//!
//! The host builds the input bytes and hands them over without a copy. The script works on them. The host then
//! checks the result against the same computation done in Rust. A wrong result exits with status 1. It prints
//! wall time, CPU time and peak resident memory of the process (from /proc), the managed-heap allocations,
//! buffer bytes and steps. `tools/js/binary_load.sh` runs the matrix with caps.
//!
//!   copy    SCALE x 512 KiB: slice, set, subarray and a typed-array copy; checks the bytes against a Rust copy
//!   scan    SCALE*100000 element reads from a script loop; checks the sum
//!   codec   SCALE x 512 KiB of mixed text: decode, then encode; checks the text length and the round trip
//!   stream  SCALE x 512 KiB of mixed text in 1000-byte chunks that split code points: checks the total length
//!   indexof SCALE x 2 MiB: hit scans over every 4096th byte (255), a miss (252) over the whole view, and a search
//!           from the tail; checks the count, the sum of the hit positions and the three positions against the formula
//!   f64from SCALE*20000 values from a generator through `Float64Array.from` with a map function; checks every element
//!   i32sort SCALE*100000 random int32 values: slice, fill, sort, toReversed, set; checks the bytes against a Rust sort
//!   dataview SCALE*100000 loop turns of `getUint32` (little endian), `setUint32` (big endian) and `getInt16`; checks the sum
//!           and the bytes of the buffer against a Rust model

use bare_js::eval::*;
use bare_js::heap::Heap;
use bare_js::parse::{parse_script, Limits};
use bare_js::value::Value;
use std::time::Instant;

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

/// A deterministic mix of 1, 2, 3 and 4 byte code points, `bytes` long, valid UTF-8.
fn mixed_text(bytes: usize) -> Vec<u8> {
    let unit = "ab\u{e9}\u{20ac}\u{1f600}cd\u{4e2d}".as_bytes();
    let mut v = Vec::with_capacity(bytes + 4);
    while v.len() + unit.len() <= bytes {
        v.extend_from_slice(unit);
    }
    v.resize(bytes, b'x');
    v
}

fn set_global(it: &mut Interp, name: &str, v: Value) {
    let k = it.heap.key_from_str(name).unwrap_or_else(|e| die(format!("key {e:?}")));
    it.heap.set(it.global_object(), k, v).unwrap_or_else(|e| die(format!("set {e:?}")));
}

fn run_case(case: String, scale: usize) {
    let mib = scale << 19; // 512 KiB per scale unit: 50 MiB at 100x, so copy (4 buffers) fits the 256 MiB buffer cap
    // (input bytes, script, checker over the result and the input)
    let (input, src): (Vec<u8>, String) = match case.as_str() {
        "copy" => ((0..mib).map(|i| (i.wrapping_mul(31) >> 3) as u8).collect(),
            "var a = data.slice(0); var b = new Uint8Array(a); var c = new Uint8Array(data.length); c.set(b.subarray(1)); c.set(a.subarray(0, 1), data.length - 1); c".into()),
        "scan" => ((0..scale * 100_000).map(|i| (i % 251) as u8).collect(),
            "var s = 0; for (var i = 0; i < data.length; i++) s += data[i]; s".into()),
        // The same loop over a plain Array: the baseline for the cost of an element read.
        "scanarr" => ((0..scale * 100_000).map(|i| (i % 251) as u8).collect(),
            "var a = []; for (var i = 0; i < data.length; i++) a.push(data[i]); var s = 0; for (var i = 0; i < a.length; i++) s += a[i]; s".into()),
        // Byte 255 is at every 4096th index; `i % 251` never reaches 251 or more, so 252 is a miss over all the bytes.
        "indexof" => ((0..scale << 21).map(|i| if i % 4096 == 17 { 255 } else { (i % 251) as u8 }).collect(),
            "var n = 0, s = 0, k = 0; while ((k = data.indexOf(255, k)) !== -1) { n++; s += k; k++; } [n, s, data.indexOf(255), data.indexOf(252), data.indexOf(255, data.length - 4079)].join('|')".into()),
        "f64from" => (vec![0; scale * 20_000],
            "function* g(n) { for (var i = 0; i < n; i++) yield i; } Float64Array.from(g(data.length), function (x, k) { return x * 0.5 + k; })".into()),
        "i32sort" => ((0..scale * 400_000).map(|i| (((i as u64).wrapping_mul(0x9E3779B97F4A7C15) >> 29) ^ (i as u64 >> 2)) as u8).collect(),
            "var a = new Int32Array(data.buffer, 0, data.length >> 2); var t = a.slice(0); t.sort(); var r = t.toReversed(); var z = new Int32Array(a.length); z.fill(7); r.set(z.subarray(0, 100)); r.reverse(); r.sort(); r".into()),
        "dataview" => ((0..scale * 400_000).map(|i| (i.wrapping_mul(131) >> 2) as u8).collect(),
            "var d = new DataView(data.buffer); var s = 0; for (var i = 0; i < data.length; i += 4) { d.setUint32(i, d.getUint32(i, true) + 1, false); s += d.getInt16(i, true); } s".into()),
        "codec" => (mixed_text(mib), "var t = new TextDecoder().decode(data); var e = new TextEncoder().encode(t); [t.length, e]".into()),
        "stream" => (mixed_text(mib), "var d = new TextDecoder(); var n = 0; for (var i = 0; i < data.length; i += 1000) n += d.decode(data.subarray(i, i + 1000), {stream: true}).length; n + d.decode().length".into()),
        _ => die(format!("unknown case {case}")),
    };
    let keep = input.clone();
    let t0 = Instant::now();
    let prog = parse_script(&src, Limits::default()).unwrap_or_else(|e| die(format!("parse {e}")));
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 2_000_000_000, ..Default::default() }).unwrap_or_else(|e| die(format!("setup {e:?}")));
    let data = it.new_uint8_array(input).unwrap_or_else(|e| die(format!("host buffer {e:?}")));
    set_global(&mut it, "data", data);
    let t_setup = t0.elapsed();
    let t1 = Instant::now();
    let result = it.run().unwrap_or_else(|e| die(format!("run {e:?}")));
    let t_run = t1.elapsed();
    match case.as_str() {
        "copy" => {
            let mut want = keep[1..].to_vec();
            want.push(keep[0]);
            if it.bytes_of(result) != Some(&want[..]) {
                die(format!("copy x{scale}: bytes differ"));
            }
        }
        "scan" | "scanarr" => {
            let want: f64 = keep.iter().map(|&b| b as f64).sum();
            if !matches!(result, Value::Number(x) if x == want) {
                die(format!("scan x{scale}: sum {} != {want}", it.display(result)));
            }
        }
        "codec" => {
            let text = std::str::from_utf8(&keep).unwrap();
            let units = text.encode_utf16().count();
            let Value::Object(arr) = result else { die("codec: not an array".into()) };
            let get = |it: &mut Interp, i: u32| match it.heap.get(arr, bare_js::value::PropertyKey::Index(i)) {
                Ok(bare_js::heap::Get::Value(v)) => v,
                other => die(format!("codec get {other:?}")),
            };
            let (len, enc) = (get(&mut it, 0), get(&mut it, 1));
            if !matches!(len, Value::Number(x) if x == units as f64) {
                die(format!("codec x{scale}: length {} != {units}", it.display(len)));
            }
            if it.bytes_of(enc) != Some(&keep[..]) {
                die(format!("codec x{scale}: round trip differs"));
            }
        }
        "f64from" => {
            let want: Vec<u8> = (0..keep.len()).flat_map(|k| (k as f64 * 0.5 + k as f64).to_le_bytes()).collect();
            if it.bytes_of(result) != Some(&want[..]) {
                die(format!("f64from x{scale}: bytes differ"));
            }
        }
        "i32sort" => {
            // The script: the first 100 values of the reversed sort become 7, then everything is sorted again.
            let mut w: Vec<i32> = keep.chunks_exact(4).map(|c| i32::from_le_bytes(c.try_into().unwrap())).collect();
            w.sort_unstable();
            w.reverse();
            for x in w.iter_mut().take(100) {
                *x = 7;
            }
            w.sort_unstable();
            let want: Vec<u8> = w.iter().flat_map(|x| x.to_le_bytes()).collect();
            if it.bytes_of(result) != Some(&want[..]) {
                die(format!("i32sort x{scale}: bytes differ"));
            }
        }
        "dataview" => {
            let mut b = keep.clone();
            let mut sum = 0f64;
            for i in (0..b.len()).step_by(4) {
                let v = u32::from_le_bytes(b[i..i + 4].try_into().unwrap()).wrapping_add(1);
                b[i..i + 4].copy_from_slice(&v.to_be_bytes());
                sum += i16::from_le_bytes([b[i], b[i + 1]]) as f64;
            }
            if !matches!(result, Value::Number(x) if x == sum) || it.bytes_of(data) != Some(&b[..]) {
                die(format!("dataview x{scale}: result {} != {sum} or bytes differ", it.display(result)));
            }
        }
        "indexof" => {
            // The expected text comes from the formula, not from the buffer: the hits are at 17 + 4096 * j.
            let len = (scale << 21) as u64;
            let hits: Vec<u64> = (17..len).step_by(4096).collect();
            let want = format!("{}|{}|17|-1|{}", hits.len(), hits.iter().sum::<u64>(), len - 4079);
            let got = it.display(result);
            if got != want {
                die(format!("indexof x{scale}: {got} != {want}"));
            }
        }
        _ => {
            let units = std::str::from_utf8(&keep).unwrap().encode_utf16().count();
            if !matches!(result, Value::Number(x) if x == units as f64) {
                die(format!("stream x{scale}: total {} != {units}", it.display(result)));
            }
        }
    }
    let before = it.heap.stats();
    let t2 = Instant::now();
    let freed = it.collect(&[]).unwrap_or_else(|e| die(format!("collect {e:?}")));
    let t_collect = t2.elapsed();
    let after = it.heap.stats();
    println!(
        "{case:7} x{scale:<3} checksum=ok bytes={:<9} run={:>8.1}ms collect={:>6.1}ms setup={:.1}ms | cpu(whole process)={:>8.1}ms peakRSS={}MiB | heap objs alloc={} strs alloc={} buffer_bytes_before={} after={} freed_objs={} | steps={}",
        keep.len(),
        t_run.as_secs_f64() * 1e3,
        t_collect.as_secs_f64() * 1e3,
        t_setup.as_secs_f64() * 1e3,
        cpu_ms(),
        proc_kib("VmHWM:") / 1024,
        before.total_object_allocs,
        before.total_string_allocs,
        before.buffer_bytes,
        after.buffer_bytes,
        freed.objects,
        it.stats().steps,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: binary_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    std::thread::Builder::new().stack_size(if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 }).spawn(move || run_case(name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
