//! Tests of the BigInt primitive. The corpus (`tools/fixtures/js/bigint-cases.txt`) is checked against what Node gave
//! (`bigint-cases.expected`, made by `tools/js/eval_oracle.mjs`; `tools/js/bigint_check.sh` re-runs Node and the matrix cases).
//! The tests below cover what Node cannot show: limits, GC, interning and the gates.

use super::*;
use crate::heap::{Heap, HeapLimits};
use crate::parse::{parse_script, Limits};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn show(it: &mut Interp, r: Result<Value, Error>) -> String {
    match r {
        Ok(Value::String(s)) => format!("string:{}", esc(&it.display(Value::String(s)))),
        Ok(v @ Value::Number(_)) => format!("number:{}", it.display(v)),
        Ok(v @ Value::Bool(_)) => format!("boolean:{}", it.display(v)),
        Ok(v @ (Value::BigInt(_) | Value::BigHeap(_))) => format!("bigint:{}", it.display(v)),
        Ok(Value::Undefined) => "undefined".into(),
        Ok(Value::Null) => "null".into(),
        Ok(v) => it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", esc(&it.get_str(v, "name")), esc(&it.get_str(v, "message"))),
        Err(Error::Throw(v)) => format!("throw-value:{}", it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

fn out(it: &mut Interp, src: &str) -> String {
    let r = it.run_script(src);
    show(it, r)
}

fn run(src: &str) -> String {
    let src = src.to_string();
    big(move || {
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 200_000_000, ..Default::default() }).unwrap();
        out(&mut it, &src)
    })
}

/// The corpus without the matrix cases (too slow for a debug build; `bigint_check.sh` runs them).
#[test]
fn corpus_matches_node() {
    let (cases, expected) = (include_str!("../../../tools/fixtures/js/bigint-cases.txt"), include_str!("../../../tools/fixtures/js/bigint-cases.expected"));
    big(move || {
        let mut list: Vec<(&str, Vec<&str>)> = Vec::new();
        for line in cases.lines() {
            if let Some(n) = line.strip_prefix("=== ") {
                list.push((n.trim(), Vec::new()));
            } else if let Some(c) = list.last_mut() {
                c.1.push(line);
            }
        }
        let want: Vec<&str> = expected.lines().collect();
        assert_eq!(list.len(), want.len());
        let mut bad = Vec::new();
        for ((name, lines), w) in list.iter().zip(want) {
            if name.ends_with("_matrix") {
                continue;
            }
            let src = lines.join("\n");
            let prog = parse_script(src.trim_end_matches('\n'), Limits::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
            let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }).expect("setup");
            let r = it.run();
            let got = format!("{name}\t{}", show(&mut it, r));
            if got != w {
                bad.push(format!("node: {w}\nbare: {got}"));
            }
        }
        assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad.join("\n"));
    });
}

/// One value is one heap entry, so handles of equal values are equal and a second computation makes no new entry.
#[test]
fn equal_values_share_one_heap_entry() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        assert_eq!(out(&mut it, "var a = 2n ** 64n; var b = 18446744073709551615n + 1n; var c = BigInt('18446744073709551616'); a === b && b === c"), "boolean:true");
        // 2^64 once, and the literal 2^64 - 1 (it is above i64::MAX too).
        assert_eq!(it.heap.stats().live_bigints, 2);
        assert_eq!(out(&mut it, "var d = -(2n ** 64n); d !== a"), "boolean:true");
        assert_eq!(it.heap.stats().live_bigints, 3);
        // The inline form never reaches the heap: all of these fit in i64.
        out(&mut it, "var s = 0n; for (var i = 0n; i < 1000n; i++) s += i * i;");
        assert_eq!(it.heap.stats().live_bigints, 3);
    });
}

/// A heap BigInt is traced from its roots and collected when nothing holds it. The table is rebuilt, so a later lookup still works.
#[test]
fn heap_bigints_are_collected_and_survivors_still_match() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }).unwrap();
        out(&mut it, "var keep = 3n ** 100n; var m = new Map([[2n ** 70n, 'k']]); var o = { v: 5n ** 90n }; var arr = []; for (var i = 0n; i < 200n; i++) arr.push(2n ** 64n + i);");
        assert_eq!(it.heap.stats().live_bigints, 203);
        out(&mut it, "arr = null;");
        it.collect(&[]).unwrap();
        assert_eq!(it.heap.stats().live_bigints, 3);
        assert_eq!(out(&mut it, "[keep === 3n ** 100n, m.get(2n ** 70n), o.v === 5n ** 90n, (2n ** 64n + 7n) === (2n ** 64n + 7n)].join()"), "string:true,k,true,true");
        out(&mut it, "keep = m = o = null;");
        it.collect(&[]).unwrap();
        it.collect(&[]).unwrap();
        // The three held values are gone; only what the last expression made can remain (nothing, after the collect).
        assert_eq!(it.heap.stats().live_bigints, 0);
        assert_eq!(out(&mut it, "(3n ** 100n) % 1000n"), "bigint:1");
    });
}

/// A BigInt a host holds (here `extra`) survives a collect.
#[test]
fn a_held_result_survives_collect() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        let v = it.run_script("10n ** 40n").unwrap();
        it.run_script("0").unwrap();
        it.collect(&[v]).unwrap();
        assert_eq!(it.heap.stats().live_bigints, 1);
        assert_eq!(it.display(v), "10000000000000000000000000000000000000000");
        it.collect(&[]).unwrap();
        assert_eq!(it.heap.stats().live_bigints, 0);
    });
}

/// A result with too many limbs is a fatal `Limit`, checked before the work. The cap is `max_big_limbs`.
#[test]
fn limb_cap_is_a_limit_checked_before_allocation() {
    big(|| {
        let heap = Heap::new(HeapLimits { max_big_limbs: 4, ..Default::default() }).unwrap();
        let mut it = Interp::realm(heap, EvalLimits::default()).unwrap();
        assert_eq!(out(&mut it, "1n << 191n"), "bigint:3138550867693340381917894711603833208051177722232017256448");
        assert_eq!(out(&mut it, "(1n << 191n) >> 100n"), "bigint:2475880078570760549798248448");
        assert_eq!(out(&mut it, "1n << 256n"), "limit:Heap(BigLimbs)");
        // A size is refused from an upper bound (limbs of the operands, bits of the base times the exponent), so a result near
        // the cap can be refused though it would fit: 1n << 255n and 2n ** 255n are 4 limbs, and both are refused.
        assert_eq!(out(&mut it, "1n << 255n"), "limit:Heap(BigLimbs)");
        assert_eq!(out(&mut it, "2n ** 100n"), "bigint:1267650600228229401496703205376");
        assert_eq!(out(&mut it, "2n ** 255n"), "limit:Heap(BigLimbs)");
        let heap = Heap::new(HeapLimits { max_big_limbs: 4, ..Default::default() }).unwrap();
        let mut it = Interp::realm(heap, EvalLimits::default()).unwrap();
        assert_eq!(out(&mut it, "1n << 256n"), "limit:Heap(BigLimbs)");
        assert_eq!(out(&mut it, "(1n << 128n) * (1n << 128n)"), "limit:Heap(BigLimbs)");
        assert_eq!(out(&mut it, "BigInt('1' + '0'.repeat(80))"), "limit:Heap(BigLimbs)");
        assert_eq!(out(&mut it, "BigInt('0x1' + '0'.repeat(64))"), "limit:Heap(BigLimbs)");
        assert_eq!(out(&mut it, "BigInt.asUintN(300, -1n)"), "limit:Heap(BigLimbs)");
        // A huge shift or power is refused at once, not computed.
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        assert_eq!(out(&mut it, "1n << 1000000000000n"), "limit:Heap(BigLimbs)");
        assert_eq!(out(&mut it, "7n ** 1000000000n"), "limit:Heap(BigLimbs)");
        assert_eq!(out(&mut it, "(2n ** 64n) << (2n ** 80n)"), "limit:Heap(BigLimbs)");
        // A right shift by a huge count is cheap: the value is gone.
        assert_eq!(out(&mut it, "(2n ** 64n) >> (2n ** 80n)"), "bigint:0");
        assert_eq!(out(&mut it, "-(2n ** 64n) >> (2n ** 80n)"), "bigint:-1");
    });
}

/// `toString` over the string cap is a `Limit`, not a huge allocation.
#[test]
fn to_string_respects_the_string_cap() {
    big(|| {
        let heap = Heap::new(HeapLimits { max_string_units: 100, ..Default::default() }).unwrap();
        let mut it = Interp::realm(heap, EvalLimits::default()).unwrap();
        assert_eq!(out(&mut it, "(10n ** 50n).toString().length"), "number:51");
        assert_eq!(out(&mut it, "(10n ** 200n).toString()"), "limit:Heap(StringLength)");
        assert_eq!(out(&mut it, "(2n ** 400n).toString(2)"), "limit:Heap(StringLength)");
    });
}

/// The work of a big multiplication counts as steps, so `max_steps` stops a loop of them.
#[test]
fn big_work_is_charged_as_steps() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 20_000, ..Default::default() }).unwrap();
        assert_eq!(out(&mut it, "var a = 3n ** 20000n; var b = 0n; for (var i = 0; i < 100000; i++) b = a * a;"), "limit:Steps");
    });
}

/// What stays behind a gate: the wrapper object. Each gate says what it needs.
#[test]
fn the_wrapper_object_is_a_gate() {
    for src in ["Object(1n)", "new Object(1n)", "Object(2n ** 64n)"] {
        let r = run(src);
        assert!(r.starts_with("unsupported:") && r.contains("wrapper"), "{src}: {r}");
    }
    // A primitive keeps working through its prototype without a wrapper.
    assert_eq!(run("(5n).toString(2) + (2n ** 64n).toString(16)"), "string:10110000000000000000");
}

/// `toLocaleString` stays a gate until `Intl.NumberFormat` lands: up to 3 digits it equals `toString`.
#[test]
fn to_locale_string_gate() {
    assert_eq!(run("(999n).toLocaleString() + (-5n).toLocaleString()"), "string:999-5");
    let r = run("(1000n).toLocaleString()");
    assert!(r.starts_with("unsupported:") && r.contains("Intl.NumberFormat"), "{r}");
    let r = run("(5n).toLocaleString('de')");
    assert!(r.starts_with("unsupported:"), "{r}");
}

#[test]
fn a_symbol_and_a_bigint_do_not_mix() {
    assert_eq!(run("try { 1n + Symbol() } catch (e) { e.name + ':' + e.message }"), "string:TypeError:Cannot convert a Symbol value to a number");
}
