//! Tests of the typed arrays, `DataView` and the new `ArrayBuffer` members. The Node-checked corpus is
//! `tools/fixtures/js/typed-cases.txt`. These tests cover what Node cannot show: the gates, the limits, the exact
//! element conversions against a plain model, detached buffers and GC.

use super::binary_tests::{check_corpus, global, limited, run, run_with, with_run};
use super::*;
use crate::heap::{load, store, Elem, HeapLimits, TYPED};

#[test]
fn corpus_matches_node() {
    check_corpus(include_str!("../../../tools/fixtures/js/typed-cases.txt"), include_str!("../../../tools/fixtures/js/typed-cases.expected"));
}

// ---------------------------------------------------------------- element conversion

/// The spec conversion written the plain way: ToIntN / ToUintN by modular arithmetic on the truncated value.
fn model_int(n: f64, bits: u32, signed: bool) -> f64 {
    if !n.is_finite() {
        return 0.0;
    }
    let m = 2f64.powi(bits as i32);
    let mut r = n.trunc().rem_euclid(m);
    if signed && r >= m / 2.0 {
        r -= m;
    }
    r + 0.0
}

fn model(ty: Elem, n: f64) -> f64 {
    match ty {
        Elem::I8 => model_int(n, 8, true),
        Elem::U8 => model_int(n, 8, false),
        Elem::U8C => {
            if n.is_nan() { 0.0 } else { n.clamp(0.0, 255.0).round_ties_even() + 0.0 }
        }
        Elem::I16 => model_int(n, 16, true),
        Elem::U16 => model_int(n, 16, false),
        Elem::I32 => model_int(n, 32, true),
        Elem::U32 => model_int(n, 32, false),
        Elem::F32 => (n as f32) as f64,
        Elem::F64 => n,
        _ => unreachable!(),
    }
}

#[test]
fn element_conversion_matches_the_plain_model_for_every_type() {
    let mut vals = vec![
        0.0, -0.0, 0.5, -0.5, 1.5, 2.5, 3.5, 254.5, 255.5, 0.49999999999999994, 127.9, 128.0, -128.9, 255.9, 256.0, 32767.5, 32768.0, 65535.5, 65536.0, 2147483647.5, 2147483648.0, 4294967295.5,
        4294967296.0, 9007199254740991.0, 9007199254740992.0, 1e20, -1e20, 1e300, f64::MAX, f64::MIN_POSITIVE, 5e-324, 3.4028235677973366e38, 3.4028234663852886e38, 1e-46, f64::INFINITY, f64::NEG_INFINITY, f64::NAN,
    ];
    let mut x: u64 = 0x9E3779B97F4A7C15;
    for _ in 0..4000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let f = f64::from_bits(x);
        vals.push(f);
        vals.push(-((x % 2_000_000_000_000) as f64) / 64.0);
    }
    for &ty in TYPED.iter().filter(|t| !t.is_big()) {
        for &v in &vals {
            let mut b = [0u8; 8];
            store(ty, &mut b[..ty.size()], v);
            let got = load(ty, &b[..ty.size()]);
            let want = model(ty, v);
            let same = (got.is_nan() && want.is_nan()) || (got == want && got.is_sign_negative() == want.is_sign_negative());
            assert!(same, "{ty:?} of {v:e}: got {got:e}, want {want:e}");
        }
    }
}

#[test]
fn nan_is_stored_as_the_canonical_quiet_nan() {
    assert_eq!(run("var b = new Float64Array([NaN]).buffer; new Uint8Array(b).join()"), "string:0,0,0,0,0,0,248,127");
    assert_eq!(run("new Uint8Array(new Float32Array([NaN]).buffer).join()"), "string:0,0,192,127");
}

// ---------------------------------------------------------------- gates

#[test]
fn gates_are_explicit() {
    // The BigInt types hold BigInts (the cases of `tools/fixtures/js/bigint-cases.txt` check them against Node).
    assert_eq!(run("var a = new BigInt64Array(2); a[0] = 5n; a[1] = -1n; a[0] + a[1] + ''"), "string:4");
    assert_eq!(run("new BigUint64Array([2n ** 64n - 1n])[0] + ''"), "string:18446744073709551615");
    assert_eq!(run("try { new BigInt64Array(1)[0] = 1 } catch (e) { e.name + ': ' + e.message }"), "string:TypeError: Cannot convert 1 to a BigInt");
    // What needs no element value works on the BigInt types.
    assert_eq!(run("new BigInt64Array(4).slice(1).length"), "number:3");
    assert_eq!(run("new BigUint64Array(3).sort().byteLength"), "number:24");
    assert_eq!(run("new BigInt64Array(3).reverse().length + new BigInt64Array(3).toReversed().length"), "number:6");
    assert_eq!(run("new BigInt64Array(2).includes(0) + '' + new BigInt64Array(2).indexOf(0)"), "string:false-1");
    assert_eq!(run("var a = new BigInt64Array(2), b = new BigUint64Array(1); new Uint8Array(b.buffer).fill(255); a.set(b, 1); new Uint8Array(a.buffer).join()"), "string:0,0,0,0,0,0,0,0,255,255,255,255,255,255,255,255");
    assert_eq!(run("new Uint8Array(new BigInt64Array(1).buffer, 0, 3).length + new BigInt64Array(new ArrayBuffer(16), 8).length"), "number:4");
    // DataView: the BigInt pairs work.
    assert_eq!(run("var d = new DataView(new ArrayBuffer(8)); d.setBigUint64(0, 2n ** 64n - 1n); d.getBigInt64(0) + ''"), "string:-1");
    assert_eq!(run("new ArrayBuffer(4, {maxByteLength: 8})"), "unsupported:resizable ArrayBuffer");
    assert_eq!(run("new ArrayBuffer(4).resize(2)"), "unsupported:ArrayBuffer.prototype.resize");
    assert_eq!(run("new Int8Array([1]).toLocaleString()"), "string:1");
    assert_eq!(run("new Float64Array([1234.5, -2]).toLocaleString()"), "string:1,234.5,-2");
    assert_eq!(run("new Int8Array(0).toLocaleString()"), "string:");
    // A species or `from` receiver that is not one of the ten intrinsic classes is a gate, never a wrong type.
    let sp = "unsupported:typed array species constructor other than the intrinsic class";
    assert_eq!(run("var u = new Uint8Array(2); u.constructor = {[Symbol.species]: Int8Array}; u.slice(0)"), sp);
    assert_eq!(run("var u = new Uint8Array(2); u.constructor = {[Symbol.species]: Int8Array}; u.subarray(0)"), sp);
    assert_eq!(run("var u = new Uint8Array(2); u.constructor = {[Symbol.species]: Int8Array}; u.map(function (x) { return x })"), sp);
    assert_eq!(run("var u = new Uint8Array(2); u.constructor = {[Symbol.species]: Int8Array}; u.filter(function () { return true })"), sp);
    assert!(run("Uint8Array.from.call(Object, [])").starts_with("unsupported:%TypedArray%.from"));
    assert!(run("Uint8Array.of.call(function () {}, 1)").starts_with("unsupported:%TypedArray%.from"));
    assert!(run("class X extends Uint8Array {}; new X(2)").starts_with("unsupported:subclassing a native constructor"));
    assert_eq!(run("typeof SharedArrayBuffer"), "string:undefined");
}

// ---------------------------------------------------------------- limits

#[test]
fn caps_are_checked_before_allocation() {
    let h = HeapLimits::default;
    assert_eq!(limited(h(), 1000, 600, "new Float64Array(100)"), "limit:Heap(BufferLength)");
    assert_eq!(limited(h(), 1000, 600, "new Int32Array(150).length"), "number:150");
    assert_eq!(limited(h(), 1000, 600, "new Int32Array(151)"), "limit:Heap(BufferLength)");
    assert_eq!(run("new Float64Array(2 ** 40)"), "limit:Heap(BufferLength)");
    assert_eq!(run("new Float64Array(9007199254740991)"), "limit:Heap(BufferLength)");
    assert_eq!(run("new Float64Array(2 ** 53)"), "throw:RangeError:Invalid typed array length: 9007199254740992");
    assert_eq!(limited(h(), 1000, 600, "var a = new Float64Array(70); a.slice(0)"), "limit:Heap(BufferBytes)");
    assert_eq!(limited(h(), 1000, 600, "var a = new Float64Array(70); a.toSorted()"), "limit:Heap(BufferBytes)");
    assert_eq!(limited(h(), 1000, 600, "var a = new Float64Array(70); a.with(0, 1)"), "limit:Heap(BufferBytes)");
    assert_eq!(limited(h(), 1000, 600, "var a = new Float64Array(70); a.map(function (x) { return x })"), "limit:Heap(BufferBytes)");
    assert_eq!(limited(h(), 1000, 600, "var a = new ArrayBuffer(600); a.transfer(601)"), "limit:Heap(BufferLength)");
    // `from` of an endless generator stops at the cap, not by running out of memory.
    assert_eq!(limited(h(), 1 << 20, 800, "Float64Array.from(function* () { for (;;) yield 1; }())"), "limit:Heap(BufferLength)");
    assert_eq!(run("Int8Array.from({length: 1e15})"), "limit:Heap(BufferLength)");
    assert_eq!(run("Uint8Array.of.apply(Uint8Array, new Array(10)).length"), "number:10");
}

#[test]
fn passes_over_elements_consume_steps() {
    let limits = EvalLimits { max_steps: 20_000, ..Default::default() };
    let steps = |src: &str| run_with(src, HeapLimits::default(), limits, |_| {});
    for op in [
        "u.fill(1)", "u.reverse()", "u.toReversed()", "u.sort()", "u.toSorted()", "u.slice(0)", "u.copyWithin(1, 0)", "u.set(u.subarray(1))", "u.includes(9)", "u.lastIndexOf(9)", "u.join()", "u.with(0, 1)",
        "new Float64Array(u)", "Float64Array.from(u)", "new Float32Array(u)",
    ] {
        assert_eq!(steps(&format!("var u = new Int32Array(16777216); {op}")), "limit:Steps", "{op}");
    }
    // A callback is called once for each element, and each call is a step.
    for op in ["u.forEach(function () {})", "u.map(function () { return 1 })", "u.sort(function (a, b) { return a - b })", "u.reduce(function (a, b) { return a + b })", "u.find(function () { return false })"] {
        assert_eq!(steps(&format!("var u = new Int32Array(100000); {op}")), "limit:Steps", "{op}");
    }
    // The element loop of a DataView is the script's own loop: every get and set is a call, counted as such.
    assert_eq!(steps("var d = new DataView(new ArrayBuffer(1000000)); for (var i = 0; i < 1000000; i++) d.setUint8(i, 1);"), "limit:Steps");
}

#[test]
fn a_step_limit_stops_a_comparator_sort_with_the_array_untouched() {
    let limits = EvalLimits { max_steps: 3000, ..Default::default() };
    let r = run_with(
        "var u = new Int32Array(1000); for (var i = 0; i < 1000; i++) u[i] = 1000 - i; var out; try { u.sort(function (a, b) { return a - b; }); } catch (e) { out = 'caught'; } u[0] + ',' + u[999]",
        HeapLimits::default(),
        limits,
        |_| {},
    );
    assert_eq!(r, "limit:Steps");
    // A comparator that throws leaves the array as it was.
    assert_eq!(run("var u = new Int8Array([3, 1, 2]); try { u.sort(function () { throw 1; }); } catch (e) {} u.join()"), "string:3,1,2");
}

// ---------------------------------------------------------------- detach and GC

#[test]
fn transfer_moves_the_bytes_without_a_copy_and_frees_them_at_collect() {
    with_run("var b = new ArrayBuffer(1048576); var v = new Uint8Array(b); v[5] = 9; var c = b.transfer();", |it| {
        let s = it.heap.stats();
        assert_eq!(s.buffer_bytes, 1048576, "one megabyte, not two");
        it.collect(&[]).unwrap();
        assert_eq!(it.heap.stats().buffer_bytes, 1048576);
        let k = it.heap.key_from_str("c").unwrap();
        it.heap.set(it.global_object(), k, Value::Undefined).unwrap();
        it.collect(&[]).unwrap();
        assert_eq!(it.heap.stats().buffer_bytes, 0);
    });
    // The same size moves the bytes: the new buffer has them, a detached view reads none.
    assert_eq!(run("var b = new ArrayBuffer(4), v = new Uint8Array(b); v[1] = 7; var c = b.transfer(); new Uint8Array(c)[1] + ',' + v.length + ',' + v[1]"), "string:7,0,undefined");
    // A grown transfer pads with zeros, a shrunk one cuts.
    assert_eq!(run("var b = new Uint8Array([1, 2, 3]).buffer; new Uint8Array(b.transfer(5)).join()"), "string:1,2,3,0,0");
    assert_eq!(run("var b = new Uint8Array([1, 2, 3]).buffer; new Uint8Array(b.transfer(2)).join()"), "string:1,2");
}

#[test]
fn a_detached_view_is_inert_everywhere() {
    assert_eq!(run("var u = new Float64Array(8); u.buffer.transfer(); u.length + ',' + u.byteLength + ',' + u.byteOffset + ',' + Object.keys(u).length + ',' + (0 in u) + ',' + u[0]"), "string:0,0,0,0,false,undefined");
    assert_eq!(run("var u = new Uint8Array(8); u.buffer.transfer(); u[0] = 5; u[0]"), "undefined");
    assert_eq!(
        run("var u = new Uint8Array(8); u.buffer.transfer(); try { for (var x of u); } catch (e) { e.message }"),
        "string:Cannot perform %TypedArray%.prototype.values on a detached ArrayBuffer"
    );
    assert_eq!(run("var u = new Uint8Array(8); u.buffer.transfer(); new TextDecoder().decode(u).length"), "number:0");
    assert_eq!(run("var u = new Uint8Array(8); u.buffer.transfer(); new TextEncoder().encodeInto('abc', u).written"), "number:0");
    assert_eq!(run("var u = new Uint8Array(8), it = u.values(); it.next(); u.buffer.transfer(); try { it.next(); } catch (e) { e.name + ': ' + e.message }"), "string:TypeError: Cannot perform Array Iterator.prototype.next on a detached ArrayBuffer");
    // A getter-driven detach in the middle of a method: the method stops with the V8 message.
    assert_eq!(
        run("var u = new Int8Array(4); try { u.fill({valueOf: function () { u.buffer.transfer(); return 1; }}); } catch (e) { e.message }"),
        "string:Cannot perform %TypedArray%.prototype.fill on a detached ArrayBuffer"
    );
}

#[test]
fn views_and_buffers_survive_collect_with_their_roots() {
    with_run(
        "var keep = new Float64Array(4); keep[2] = 2.5; var d = new DataView(new ArrayBuffer(16), 4, 8); d.setInt16(0, -2); var s = new Int16Array(d.buffer, 4, 2).subarray(1); var g = Float64Array.from(function* () { yield 1; yield 2; }()); g = null;
         function probe() { return keep[2] + ',' + d.getInt16(0) + ',' + s.length + ',' + s.byteOffset; }",
        |it| {
            it.collect(&[]).unwrap();
            it.collect(&[]).unwrap();
            assert_eq!(it.heap.stats().buffer_bytes, 32 + 16);
            let f = global(it, "probe");
            let v = it.call(f, Value::Undefined, &[]).unwrap();
            assert_eq!(it.display(v), "2.5,-2,1,6");
        },
    );
}

#[test]
fn the_prototypes_stay_roots_after_the_globals_are_deleted() {
    with_run(
        "var T = Object.getPrototypeOf(Int8Array); var P = [Float32Array, DataView, BigUint64Array].map(function (c) { return c.prototype; });
         delete globalThis.Float32Array; delete globalThis.DataView; delete globalThis.BigUint64Array; delete globalThis.Int8Array;
         function probe() { return P[0].BYTES_PER_ELEMENT + ',' + typeof P[1].getInt8 + ',' + P[2].BYTES_PER_ELEMENT + ',' + typeof T.prototype.map; }",
        |it| {
            it.collect(&[]).unwrap();
            it.collect(&[]).unwrap();
            let f = global(it, "probe");
            let v = it.call(f, Value::Undefined, &[]).unwrap();
            assert_eq!(it.display(v), "4,function,8,function");
        },
    );
}

#[test]
fn find_byte_matches_a_plain_scan_at_every_length_and_position() {
    for len in 0..40usize {
        for pos in 0..len {
            for extra in [None, Some(len - 1)] {
                let mut v = vec![0x80u8; len];
                v[pos] = 7;
                if let Some(e) = extra {
                    v[e] = 7;
                }
                assert_eq!(find_byte(&v, 7, false), v.iter().position(|&b| b == 7), "fwd len={len} pos={pos}");
                assert_eq!(find_byte(&v, 7, true), v.iter().rposition(|&b| b == 7), "rev len={len} pos={pos}");
            }
        }
        let v = vec![0xffu8; len];
        assert_eq!(find_byte(&v, 0xfe, false), None);
        assert_eq!(find_byte(&v, 0xff, true), len.checked_sub(1));
    }
}
