//! Tests of the batch 1 built-ins: Array, String, Object, Reflect and the groupBy functions
//! (`eval_array_more.rs`, `eval_string_more.rs`, `eval_object_more.rs`). The Node oracle corpus is
//! `tools/fixtures/js/builtins1-cases.txt`. These tests cover what Node cannot show: explicit gates, limits, work budgets,
//! and that the dense fast path and the generic path give the same result.

use super::*;
use crate::heap::{Heap, HeapLimits};
use crate::parse::{parse_script, Limits};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn show(it: &Interp, v: Value) -> String {
    match v {
        Value::String(_) => format!("string:{}", esc(&it.display(v))),
        Value::Number(_) => format!("number:{}", it.display(v)),
        Value::Bool(_) => format!("boolean:{}", it.display(v)),
        Value::Undefined => "undefined".into(),
        Value::Null => "null".into(),
        Value::Symbol(_) => "symbol".into(),
        Value::BigInt(_) | Value::BigHeap(_) => format!("bigint:{}", it.display(v)),
        Value::Object(_) => it.display(v),
    }
}

/// Run `src`. `Ok` is the shown completion value or `throw:Name:message`. `Err` is a non-JS outcome.
fn eval_with(src: &str, steps: u64, heap: HeapLimits) -> Result<String, Error> {
    let src = src.to_string();
    big(move || {
        let prog = parse_script(&src, Limits::default()).unwrap_or_else(|e| panic!("{src:?}: {e}"));
        let mut it = Interp::new(&prog, Heap::new(heap).unwrap(), EvalLimits { max_steps: steps, ..Default::default() })?;
        match it.run() {
            Ok(v) => Ok(show(&it, v)),
            Err(Error::Throw(v)) => Ok(format!("throw:{}:{}", it.get_str(v, "name"), esc(&it.get_str(v, "message")))),
            Err(e) => Err(e),
        }
    })
}

fn run(src: &str) -> String {
    eval_with(src, 50_000_000, HeapLimits::default()).unwrap_or_else(|e| panic!("{src}: {e:?}"))
}

fn limit(src: &str, steps: u64) -> LimitKind {
    match eval_with(src, steps, HeapLimits::default()) {
        Err(Error::Limit(k)) => k,
        other => panic!("{src}: {other:?}"),
    }
}

fn unsupported_of(src: &str) -> &'static str {
    match eval_with(src, 5_000_000, HeapLimits::default()) {
        Err(Error::Unsupported(u)) => u.what,
        other => panic!("{src}: {other:?}"),
    }
}

#[test]
fn corpus_matches_node() {
    let (cases, expected) = (include_str!("../../../tools/fixtures/js/builtins1-cases.txt"), include_str!("../../../tools/fixtures/js/builtins1-cases.expected"));
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
        let src = lines.join("\n");
        let got = match eval_with(src.trim_end_matches('\n'), 2_000_000_000, HeapLimits::default()) {
            Ok(s) => s,
            Err(Error::Limit(_)) => "limit".into(),
            Err(e) => format!("other:{e:?}"),
        };
        let got = format!("{name}\t{got}");
        if got != w {
            bad.push(format!("want {w}\n got {got}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn gates_are_explicit_unsupported() {
    for (src, what) in [
        ("'a'.normalize('NFKC')", "String.prototype.normalize with NFKC or NFKD"),
        ("'a'.normalize('NFKD')", "String.prototype.normalize with NFKC or NFKD"),
        ("'a'.toLocaleUpperCase('tr')", "toLocaleLowerCase and toLocaleUpperCase for tr, az and lt"),
        ("'A'.toLocaleLowerCase(['az-Latn'])", "toLocaleLowerCase and toLocaleUpperCase for tr, az and lt"),
        ("Reflect.set(new Uint8Array(2), 0, 1, {})", "Reflect.set on an integer index of a byte view with another receiver"),
        ("RegExp.prototype[Symbol.search].call({ exec: function () {} }, 'a')", "RegExp.prototype[Symbol.search] on a non-RegExp object"),
    ] {
        assert_eq!(unsupported_of(src), what, "{src}");
    }
}

#[test]
fn match_all_is_absent_on_purpose() {
    assert_eq!(run("typeof ''.matchAll"), "string:undefined");
}

#[test]
fn string_results_over_the_limit_are_limit_errors_before_any_allocation() {
    for src in ["'ab'.repeat(2 ** 40)", "'a'.padStart(2 ** 40, 'xy')", "'a'.padEnd(2 ** 53, 'xy')", "'abc'.repeat(2 ** 29)"] {
        assert!(matches!(limit(src, 50_000_000), LimitKind::Heap(_)), "{src}");
    }
    // The limit is the heap limit that the caller set, so a small heap fails earlier.
    let small = HeapLimits { max_string_units: 1000, ..HeapLimits::default() };
    assert!(matches!(eval_with("'ab'.repeat(501)", 5_000_000, small), Err(Error::Limit(LimitKind::Heap(Limit::StringLength)))));
    assert_eq!(eval_with("'ab'.repeat(500).length", 5_000_000, small).unwrap(), "number:1000");
}

#[test]
fn scans_are_charged_to_the_step_budget() {
    // lastIndexOf: a needle that matches long prefixes at every position, and a long haystack that never matches.
    assert_eq!(limit("var h = 'a'.repeat(1 << 20); h.lastIndexOf('a'.repeat(1 << 19) + 'b')", 200_000), LimitKind::Steps);
    assert_eq!(limit("'a'.repeat(1 << 22).lastIndexOf('b')", 100_000), LimitKind::Steps);
    // replaceAll with an empty needle makes a match at every unit.
    assert_eq!(limit("'x'.repeat(1 << 20).replaceAll('', 'y')", 10_000), LimitKind::Steps);
    assert_eq!(limit("'\\u00e9'.repeat(1 << 20).normalize('NFD')", 10_000), LimitKind::Steps);
    // A traversal that cannot stop early fails before it starts.
    assert_eq!(limit("Array.prototype.reverse.call({ length: 2 ** 40 })", 100_000), LimitKind::Steps);
    assert_eq!(limit("Array.prototype.lastIndexOf.call({ length: 2 ** 40 }, 1)", 100_000), LimitKind::Steps);
    assert_eq!(limit("Array.prototype.flat.call({ length: 2 ** 40 })", 100_000), LimitKind::Steps);
    assert_eq!(limit("Object.getOwnPropertyDescriptors(new Uint8Array(1 << 20))", 10_000), LimitKind::Steps);
}

#[test]
fn flat_depth_is_bounded() {
    // Infinity depth of a deeply nested array ends in a limit, not a native stack overflow.
    let src = "var a = []; for (var i = 0; i < 100000; i++) a = [a]; a.flat(Infinity).length";
    assert!(matches!(eval_with(src, 50_000_000, HeapLimits::default()), Err(Error::Limit(_))), "{src}");
}

#[test]
fn dense_and_generic_paths_agree() {
    // The same data as a dense array and as an array-like object: every method must give the same answer.
    let src = r#"
var d = [], i;
for (i = 0; i < 600; i++) d.push((i * 7919) % 1009);
var g = { length: d.length };
for (i = 0; i < d.length; i++) g[i] = d[i];
var A = Array.prototype;
function same(x, y) {
  if (x.length !== y.length) return false;
  for (var k = 0; k < x.length; k++) if (x[k] !== y[k]) return false;
  return true;
}
function copy(o) { return A.slice.call(o); }
var r = [];
var d1 = copy(d), g1 = copy(g); d1.reverse(); A.reverse.call(g1); r.push(same(d1, g1));
r.push(same(d.toReversed(), A.toReversed.call(g)));
r.push(same(d.toSorted(), A.toSorted.call(g)));
r.push(same(d.toSorted(function (a, b) { return b - a; }), A.toSorted.call(g, function (a, b) { return b - a; })));
r.push(same(d.with(5, -1), A.with.call(g, 5, -1)));
r.push(same(d.toSpliced(3, 10, 'a', 'b'), A.toSpliced.call(g, 3, 10, 'a', 'b')));
r.push(d.lastIndexOf(d[3]) === A.lastIndexOf.call(g, d[3]));
r.push(d.lastIndexOf(d[3], -100) === A.lastIndexOf.call(g, d[3], -100));
r.push(d.at(-3) === A.at.call(g, -3));
var d2 = copy(d), g2 = copy(g); d2.copyWithin(2, 300, 500); A.copyWithin.call(g2, 2, 300, 500); r.push(same(d2, g2));
r.push(d.reduceRight(function (a, b) { return a * 31 + b & 0xffff; }, 7) === A.reduceRight.call(g, function (a, b) { return a * 31 + b & 0xffff; }, 7));
r.push(d.findLast(function (x) { return x > 1000; }) === A.findLast.call(g, function (x) { return x > 1000; }));
r.push(d.findLastIndex(function (x) { return x < 3; }) === A.findLastIndex.call(g, function (x) { return x < 3; }));
var n = [], m = { length: 40 };
for (i = 0; i < 40; i++) { n.push(i % 3 === 0 ? [i, [i]] : i); m[i] = n[i]; }
r.push(same(n.flat(), A.flat.call(m)) && same(n.flat(2), A.flat.call(m, 2)) && same(n.flatMap(function (x) { return [x]; }), A.flatMap.call(m, function (x) { return [x]; })));
r.join()
"#;
    assert_eq!(run(src), "string:true,true,true,true,true,true,true,true,true,true,true,true,true,true");
}

#[test]
fn holes_are_kept_or_read_as_the_specification_says() {
    let src = r#"
var h = [1, , 3, , 5], g = { length: 5, 0: 1, 2: 3, 4: 5 }, A = Array.prototype;
function keys(x) { return Object.keys(x).join(); }
var r = [];
var h1 = h.slice(), g1 = A.slice.call(g); h1.reverse(); A.reverse.call(g1); r.push(keys(h1) + '/' + keys(g1));
var h2 = h.slice(), g2 = A.slice.call(g); h2.copyWithin(0, 2); A.copyWithin.call(g2, 0, 2); r.push(keys(h2) + '/' + keys(g2));
r.push(keys(h.toReversed()) + '/' + keys(A.toReversed.call(g)));
r.push(keys(h.toSpliced(1, 1)) + '/' + keys(A.toSpliced.call(g, 1, 1)));
r.push(keys(h.with(0, 9)) + '/' + keys(A.with.call(g, 0, 9)));
r.push(keys(h.flat()) + '/' + keys(A.flat.call(g)));
r.join('|')
"#;
    assert_eq!(run(src), "string:0,2,4/0,2,4|0,2,4/0,2,4|0,1,2,3,4/0,1,2,3,4|0,1,2,3/0,1,2,3|0,1,2,3,4/0,1,2,3,4|0,1,2/0,1,2");
}

#[test]
fn frozen_arrays_reject_every_write_and_keep_their_state() {
    let src = r#"
var a = Object.freeze([3, 1, 2]), r = [];
function t(f) { try { f(); r.push('ok'); } catch (e) { r.push(e.name); } }
t(function () { a.push(4); });
t(function () { a.pop(); });
t(function () { a.shift(); });
t(function () { a.unshift(0); });
t(function () { a.splice(0, 1); });
t(function () { a.sort(); });
t(function () { a.reverse(); });
t(function () { a.fill(0); });
t(function () { a.copyWithin(0, 1); });
t(function () { 'use strict'; a[0] = 9; });
t(function () { 'use strict'; a.length = 0; });
t(function () { a[0] = 9; a.length = 0; a[7] = 1; });
r.push(a.join(), a.length, Object.isFrozen(a), a.toSorted().join(), a.with(0, 0).join(), Object.isFrozen(a.toSorted()));
r.join()
"#;
    assert_eq!(run(src), "string:TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,ok,3,1,2,3,true,1,2,3,0,1,2,false");
}

#[test]
fn freezing_a_byte_view_follows_v8() {
    // A view with elements cannot be frozen (TypeError). `seal` of one succeeds in V8 although the specification throws.
    let src = r#"
var r = [];
function t(f) { try { f(); r.push('ok'); } catch (e) { r.push(e.name); } }
t(function () { Object.freeze(new Uint8Array(1)); });
t(function () { Object.freeze(new Uint8Array(0)); });
t(function () { Object.seal(new Uint8Array(2)); });
t(function () { Object.freeze(new Uint8Array(3).subarray(1, 1)); });
r.join()
"#;
    assert_eq!(run(src), "string:TypeError,ok,ok,ok");
}

#[test]
fn group_by_closes_the_iterator_once_and_keeps_group_order() {
    let src = r#"
var log = [];
var it = { [Symbol.iterator]: function () { var i = 0; return {
  next: function () { log.push('n' + i); return { value: i++, done: false }; },
  return: function () { log.push('r'); return {}; } }; } };
try { Object.groupBy(it, function (v) { if (v === 2) throw new Error('x'); return 'k'; }); } catch (e) { log.push(e.message); }
var m = Map.groupBy([5, 6, 7, 8], function (x) { return x % 2 ? 'o' : 'e'; });
log.join() + '|' + Array.from(m.keys()).join() + '|' + m.get('o').join()
"#;
    assert_eq!(run(src), "string:n0,n1,n2,r,x|o,e|5,7");
}

#[test]
fn reflect_and_object_agree_on_the_same_operations() {
    let src = r#"
var r = [];
var o = {}, p = { q: 1 };
r.push(Reflect.setPrototypeOf(o, p) && Object.getPrototypeOf(o) === Reflect.getPrototypeOf(o));
r.push(Reflect.defineProperty(o, 'x', { value: 1 }) && Object.getOwnPropertyDescriptor(o, 'x').value === Reflect.getOwnPropertyDescriptor(o, 'x').value);
r.push(Reflect.has(o, 'q') === ('q' in o));
r.push(Reflect.preventExtensions(o) && !Object.isExtensible(o) && !Reflect.isExtensible(o));
r.push(Reflect.set(o, 'y', 1) === false, o.y === undefined);
r.push(Reflect.deleteProperty(o, 'x') === false);
r.join()
"#;
    assert_eq!(run(src), "string:true,true,true,true,true,true,true");
}
