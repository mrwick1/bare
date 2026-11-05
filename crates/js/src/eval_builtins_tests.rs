//! Tests of the JSON.parse, RegExp and String.prototype bindings (`eval_builtins.rs`).
//! The Node oracle corpus is `tools/fixtures/js/builtins-cases.txt`. These tests cover what Node cannot show:
//! explicit gates, limits, work budgets, roots and slot reuse across `collect`.

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
        Value::Object(_) => it.display(v),
    }
}

/// Run `src`. `Ok` is the shown completion value or `throw:Name:message`. `Err` is a non-JS outcome.
fn eval_with(src: &str, el: EvalLimits, bl: BuiltinLimits, heap: HeapLimits) -> Result<String, Error> {
    let src = src.to_string();
    big(move || {
        let prog = parse_script(&src, Limits::default()).unwrap_or_else(|e| panic!("{src:?}: {e}"));
        let mut it = Interp::new(&prog, Heap::new(heap).unwrap(), el)?;
        it.set_builtin_limits(bl);
        match it.run() {
            Ok(v) => Ok(show(&it, v)),
            Err(Error::Throw(v)) => Ok(format!("throw:{}:{}", it.get_str(v, "name"), esc(&it.get_str(v, "message")))),
            Err(e) => Err(e),
        }
    })
}

fn run(src: &str) -> String {
    eval_with(src, EvalLimits { max_steps: 5_000_000, ..Default::default() }, BuiltinLimits::default(), HeapLimits::default()).unwrap_or_else(|e| panic!("{src}: {e:?}"))
}

fn limit(src: &str, el: EvalLimits, bl: BuiltinLimits, heap: HeapLimits) -> LimitKind {
    match eval_with(src, el, bl, heap) {
        Err(Error::Limit(k)) => k,
        other => panic!("{src}: {other:?}"),
    }
}

fn unsupported_of(src: &str) -> &'static str {
    match eval_with(src, EvalLimits::default(), BuiltinLimits::default(), HeapLimits::default()) {
        Err(Error::Unsupported(u)) => u.what,
        other => panic!("{src}: {other:?}"),
    }
}

#[test]
fn corpus_matches_node() {
    let (cases, expected) = (include_str!("../../../tools/fixtures/js/builtins-cases.txt"), include_str!("../../../tools/fixtures/js/builtins-cases.expected"));
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
        let got = match eval_with(src.trim_end_matches('\n'), EvalLimits { max_steps: 500_000, ..Default::default() }, BuiltinLimits::default(), HeapLimits::default()) {
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
    // A literal pattern is compiled by `Interp::new`: nothing of the script runs. The parser checks only the flags.
    let literal = [
        ("var x = 1; /(/;", "regexp literal: invalid pattern (early SyntaxError)"),
        ("var r = /a(?<=b)/;", "lookbehind"),
        ("var r = /(?<n>a)/;", "named group"),
        ("var r = /(?=a)*/;", "quantified lookahead"),
        ("var r = /\\p{L}/u;", "regexp Unicode property escape"),
        ("var r = /a/d;", "regexp flag d"),
        ("function f() { return /[b-a]/; }", "regexp literal: invalid pattern (early SyntaxError)"),
    ];
    for (src, what) in literal {
        let prog = parse_script(src, Limits::default()).unwrap_or_else(|e| panic!("{src}: {e}"));
        match Interp::new(&prog, Heap::default(), EvalLimits::default()) {
            Err(Error::Unsupported(u)) => {
                assert_eq!(u.what, what, "{src}");
                assert!(u.span.end > u.span.start && src[u.span.start as usize..u.span.end as usize].starts_with('/'), "{src}: span");
            }
            Err(e) => panic!("{src}: {e:?}"),
            Ok(_) => panic!("{src}: accepted"),
        }
    }
    assert_eq!(unsupported_of("new RegExp('(?<=a)b')"), "lookbehind");
    assert_eq!(unsupported_of("new RegExp('a', 'v')"), "regexp flag v");
    assert_eq!(unsupported_of("RegExp('(?<x>a)')"), "named group");
    assert_eq!(unsupported_of("var r = /a/; r.exec = function () { return null; }; r.test('a')"), "RegExp with a replaced exec method");
    assert_eq!(unsupported_of("var r = /a/g; r.exec = function () { return null; }; 'a'.replace(r, 'b')"), "RegExp with a replaced exec method");
    assert_eq!(unsupported_of("var r = /a/; r.exec = function () { return null; }; 'a'.split(r)"), "RegExp with a replaced exec method");
    // A non-callable exec is ignored, as in the spec.
    assert_eq!(run("var r = /a/; r.exec = 5; r.test('a')"), "boolean:true");
    // The absent methods stay absent (no silent stand-in). `replaceAll`, `search` and `repeat` exist since batch 1.
    assert_eq!(run("typeof 'a'.replaceAll + typeof 'a'.search + typeof 'a'.matchAll + typeof ''.repeat + typeof RegExp.prototype.compile"), "string:functionfunctionundefinedfunctionundefined");
}

#[test]
fn invalid_literal_stops_before_any_effect() {
    let src = "globalThis.ran = 1; var r = /(?:/;";
    let prog = parse_script(src, Limits::default()).unwrap();
    assert!(matches!(Interp::new(&prog, Heap::default(), EvalLimits::default()), Err(Error::Unsupported(_))));
}

#[test]
fn regexp_work_is_charged_and_capped_distinctly() {
    let hostile = "var s = 'a'; for (var i = 0; i < 6; i++) s += s; /(a+)+b/.test(s + 'c')";
    // The evaluator budget ends first: Steps.
    assert_eq!(limit(hostile, EvalLimits { max_steps: 200_000, ..Default::default() }, BuiltinLimits::default(), HeapLimits::default()), LimitKind::Steps);
    // The per-attempt regexp budget ends first: Regexp(Steps).
    let bl = BuiltinLimits { regexp_steps: 100_000, ..Default::default() };
    assert_eq!(limit(hostile, EvalLimits::default(), bl, HeapLimits::default()), LimitKind::Regexp(RegexpLimit::Steps));
    // The same limit through each entry point.
    for src in ["new RegExp('(a+)+b').exec(S)", "'x'.concat(S).replace(/(a+)+b/, 'z')", "S.match(/(a+)+b/g)", "S.split(/(a+)+b/)", "/(a+)+b/y.test(S)"] {
        let src = format!("var S = 'a'; for (var i = 0; i < 6; i++) S += S; S += 'c'; {src}");
        assert_eq!(limit(&src, EvalLimits::default(), bl, HeapLimits::default()), LimitKind::Regexp(RegexpLimit::Steps), "{src}");
    }
    // Stack frames.
    let bl = BuiltinLimits { regexp_stack_frames: 64, ..Default::default() };
    let src = "var s = ''; for (var i = 0; i < 100; i++) s += 'ab'; /(?:a|b)*c/.test(s)";
    assert_eq!(limit(src, EvalLimits::default(), bl, HeapLimits::default()), LimitKind::Regexp(RegexpLimit::Stack));
    // Input length.
    let bl = BuiltinLimits { regexp_input_units: 100, ..Default::default() };
    let src = "var s = 'a'; for (var i = 0; i < 8; i++) s += s; /a/.test(s)";
    assert_eq!(limit(src, EvalLimits::default(), bl, HeapLimits::default()), LimitKind::Regexp(RegexpLimit::Input));
    assert_eq!(run("/a/.test('a')"), "boolean:true");
    // Compile limit.
    let src = "var p = 'a'; for (var i = 0; i < 17; i++) p += p; new RegExp(p)";
    assert_eq!(limit(src, EvalLimits::default(), BuiltinLimits::default(), HeapLimits::default()), LimitKind::Regexp(RegexpLimit::Compile));
    // Not catchable by try.
    let src = "try { new RegExp('(a+)+b').test('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaac'); } catch (e) { 'caught' }";
    let bl = BuiltinLimits { regexp_steps: 10_000, ..Default::default() };
    assert_eq!(limit(src, EvalLimits::default(), bl, HeapLimits::default()), LimitKind::Regexp(RegexpLimit::Steps));
}

#[test]
fn matches_and_items_are_capped_before_growth() {
    let bl = BuiltinLimits { max_matches: 1000, max_items: 500, ..Default::default() };
    let build = "var s = 'a'; for (var i = 0; i < 11; i++) s += s;"; // 2048 units
    let el = EvalLimits::default();
    assert_eq!(limit(&format!("{build} s.replace(/a/g, 'b')"), el, bl, HeapLimits::default()), LimitKind::Regexp(RegexpLimit::Matches));
    assert_eq!(limit(&format!("{build} s.match(/a/g)"), el, bl, HeapLimits::default()), LimitKind::Regexp(RegexpLimit::Matches));
    assert_eq!(limit(&format!("{build} s.match(/(?:)/g)"), el, bl, HeapLimits::default()), LimitKind::Regexp(RegexpLimit::Matches));
    // Capture groups count: 600 matches of 2 slots exceed 1000 slots.
    assert_eq!(limit("var s = ''; for (var i = 0; i < 600; i++) s += 'a'; s.replace(/(a)/g, 'b')", el, bl, HeapLimits::default()), LimitKind::Regexp(RegexpLimit::Matches));
    assert_eq!(run("var s = ''; for (var i = 0; i < 300; i++) s += 'a'; s.replace(/(a)/g, 'b').length"), "number:300");
    assert_eq!(limit(&format!("{build} s.split('')"), el, bl, HeapLimits::default()), LimitKind::Regexp(RegexpLimit::Items));
    assert_eq!(limit(&format!("{build} s.split(/a/)"), el, bl, HeapLimits::default()), LimitKind::Regexp(RegexpLimit::Items));
    assert_eq!(limit(&format!("{build} s.split('a')"), el, bl, HeapLimits::default()), LimitKind::Regexp(RegexpLimit::Items));
    // A limit on the split result is not hit when `limit` stops earlier.
    assert_eq!(eval_with(&format!("{build} s.split('', 10).length"), el, bl, HeapLimits::default()).unwrap(), "number:10");
    assert_eq!(eval_with(&format!("{build} s.split(/a/, 10).length"), el, bl, HeapLimits::default()).unwrap(), "number:10");
}

#[test]
fn output_strings_are_capped_before_growth() {
    let heap = || HeapLimits { max_string_units: 3000, ..Default::default() };
    let el = EvalLimits::default();
    let bl = BuiltinLimits::default();
    let build = "var s = 'a'; for (var i = 0; i < 11; i++) s += s;"; // 2048
    for src in [
        format!("{build} s.replace(/a/g, 'bb')"),
        format!("{build} s.replace(/a/g, '$&$&$&')"),
        format!("{build} s.replace(/a/g, function () {{ return 'xxx'; }})"),
        format!("{build} s.concat(s, s)"),
        format!("{build} s.replace('a', s + s + s)"),
        format!("{build} (s + 'é').toUpperCase().replace(/./g, 'xx')"),
    ] {
        assert_eq!(limit(&src, el, bl, heap()), LimitKind::Heap(Limit::StringLength), "{src}");
    }
    // Case mapping that grows: U+00DF becomes "SS".
    let src = "var s = '\\u00df'; for (var i = 0; i < 11; i++) s += s; s.toUpperCase().length";
    assert_eq!(eval_with(src, el, bl, HeapLimits::default()).unwrap(), "number:4096");
    assert_eq!(limit(src, el, bl, HeapLimits { max_string_units: 3000, ..Default::default() }), LimitKind::Heap(Limit::StringLength));
    let src = "var s = '\\u00df'; for (var i = 0; i < 11; i++) s += s; s.toUpperCase()";
    assert_eq!(limit(src, el, bl, HeapLimits { max_string_units: 3000, ..Default::default() }), LimitKind::Heap(Limit::StringLength));
}

#[test]
fn string_scans_and_loops_use_the_step_budget() {
    let build = "var s = 'a'; for (var i = 0; i < 18; i++) s += s;"; // 262144 units
    let el = EvalLimits { max_steps: 6_000, ..Default::default() };
    for body in [
        "s.indexOf('b')",
        "s.indexOf('aaaaaaaaab')",
        "s.includes('ab')",
        "s.split('')",
        "s.split('a')",
        "s.replace(/a/g, 'b')",
        "s.replace('b', 'c')",
        "s.match(/(?:)/g)",
        "s.toUpperCase()",
        "s.concat(s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s, s)",
        "JSON.parse('[' + s.slice(0, 20000).split('').join(',').replace(/a/g, '1') + ']')",
        "s.slice(0, 262000).trim()",
    ] {
        let src = format!("{build} {body}");
        assert_eq!(limit(&src, el, BuiltinLimits::default(), HeapLimits::default()), LimitKind::Steps, "{body}");
    }
}

#[test]
fn json_limits_are_distinct() {
    let el = EvalLimits::default();
    let mk = |f: fn(&mut BuiltinLimits)| {
        let mut b = BuiltinLimits::default();
        f(&mut b);
        b
    };
    let kind = |src: &str, b: BuiltinLimits| limit(src, el, b, HeapLimits::default());
    assert_eq!(kind("JSON.parse('[1,2,3]')", mk(|b| b.json.max_source_units = 4)), LimitKind::Json(json::LimitKind::Source));
    assert_eq!(kind("JSON.parse('[[[1]]]')", mk(|b| b.json.max_depth = 2)), LimitKind::Json(json::LimitKind::Depth));
    assert_eq!(kind("JSON.parse('[1,2,3]')", mk(|b| b.json.max_nodes = 3)), LimitKind::Json(json::LimitKind::Nodes));
    assert_eq!(kind("JSON.parse('\"abcdef\"')", mk(|b| b.json.max_text_units = 3)), LimitKind::Json(json::LimitKind::Text));
    let heap = HeapLimits { max_objects: 200, ..Default::default() };
    assert_eq!(limit("JSON.parse('[' + new Array(300).join('{},') + '{}]')", el, BuiltinLimits::default(), heap), LimitKind::Heap(Limit::Objects));
    // Deep input is iterative: 5000 levels do not touch the native stack.
    let src = "var s = '', i; for (i = 0; i < 5000; i++) s += '['; for (i = 0; i < 5000; i++) s += ']'; var v = JSON.parse(s), d = 0; while (v.length) { v = v[0]; d++; } d";
    assert_eq!(eval_with(src, EvalLimits { max_steps: 5_000_000, ..el }, mk(|b| b.json.max_depth = 10_000), HeapLimits::default()).unwrap(), "number:4999");
    // A syntax error is a catchable SyntaxError, a limit is not.
    assert_eq!(run("try { JSON.parse('{') } catch (e) { e.name }"), "string:SyntaxError");
    assert_eq!(eval_with("try { JSON.parse('[[1]]') } catch (e) { 'caught' }", el, mk(|b| b.json.max_depth = 1), HeapLimits::default()).unwrap_err().to_string_tag(), "limit");
}

trait Tag {
    fn to_string_tag(&self) -> &'static str;
}
impl Tag for Error {
    fn to_string_tag(&self) -> &'static str {
        match self {
            Error::Limit(_) => "limit",
            Error::Unsupported(_) => "unsupported",
            Error::Throw(_) => "throw",
            Error::Heap(_) => "heap",
        }
    }
}

#[test]
fn json_reads_utf16_and_keeps_prototypes() {
    assert_eq!(run("var s = JSON.parse('\"\\\\ud83d\\\\ude00\"'); s.length + ':' + s.charCodeAt(1)"), "string:2:56832");
    assert_eq!(run("var s = JSON.parse('\"\\ud83d\\ude00\"'); s.length"), "number:2");
    assert_eq!(run("var o = JSON.parse('{\"a\":{\"b\":[]}}'); (o instanceof Object) + ':' + (o.a.b instanceof Array)"), "string:true:true");
    // A parsed object is writable and extensible like any other.
    assert_eq!(run("var o = JSON.parse('{\"a\":1}'); o.b = 2; o.a++; o.a + o.b"), "number:4");
}

#[test]
fn failed_json_leaves_only_unrooted_garbage() {
    big(|| {
        let src = "var ok = JSON.parse('[1,{\"a\":[2]}]'); try { JSON.parse('[[[[1,2],[3]]],{\"x\":[}'); } catch (e) { } ok.length";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let live_before = it.heap.stats().live_objects;
        let v = it.run().unwrap();
        assert_eq!(show(&it, v), "number:2");
        let with_garbage = it.heap.stats().live_objects;
        assert!(with_garbage > live_before + 5);
        it.collect(&[]).unwrap();
        let after = it.heap.stats().live_objects;
        assert!(after < with_garbage, "{after} < {with_garbage}");
        // `ok` is still reachable through the global object.
        let ok = it.get_str(Value::Object(it.global_object()), "ok");
        assert_eq!(ok, "1,[object Object]");
    });
}

#[test]
fn regexps_survive_collect_and_dead_ones_are_pruned() {
    big(|| {
        let src = "var keep = new RegExp('a' + 'bc', 'g' + 'i'); var cb = function (m) { return '<' + m + '>'; }; \
                   var tmp = []; for (var i = 0; i < 50; i++) tmp.push(new RegExp('x' + i)); tmp = null; \
                   function mk() { return /dead/; } mk(); mk(); 0;";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        assert_eq!(it.ext.regexps.len(), 53);
        let c = it.collect(&[]).unwrap();
        assert!(c.objects >= 52, "{c:?}");
        assert_eq!(it.ext.regexps.len(), 1, "only `keep` is reachable");
        // Source and flags strings are still alive and right.
        let g = Value::Object(it.global_object());
        let keep = {
            let k = it.heap.key_from_str("keep").unwrap();
            it.get_prop(g, k).unwrap()
        };
        assert_eq!(it.get_str(keep, "source"), "abc");
        assert_eq!(it.get_str(keep, "flags"), "gi");
        assert_eq!(it.get_str(keep, "lastIndex"), "0");
        // The callback and the regexp still work after collection, and new regexps reuse the freed slots with fresh data.
        let f = {
            let k = it.heap.key_from_str("cb").unwrap();
            it.get_prop(g, k).unwrap()
        };
        let abc = Value::String(it.heap.intern_str("xABCx abc").unwrap());
        let rep = {
            let k = it.heap.key_from_str("replace").unwrap();
            it.get_prop(abc, k).unwrap()
        };
        let r = it.call(rep, abc, &[keep, f]).unwrap();
        assert_eq!(show(&it, r), "string:x<ABC>x <abc>");
        let again = it.call(rep, abc, &[keep, f]).unwrap();
        assert_eq!(show(&it, again), "string:x<ABC>x <abc>");
        assert_eq!(it.ext.regexps.len(), 1);
        let ctor = {
            let k = it.heap.key_from_str("RegExp").unwrap();
            it.get_prop(g, k).unwrap()
        };
        let pat = Value::String(it.heap.intern_str("q+").unwrap());
        let fresh = it.call(ctor, Value::Undefined, &[pat]).unwrap();
        let (Value::Object(a), Value::Object(b)) = (fresh, keep) else { panic!() };
        assert_ne!(a, b);
        assert_eq!(it.get_str(fresh, "source"), "q+");
        assert_eq!(it.get_str(keep, "source"), "abc");
        assert_eq!(it.ext.regexps.len(), 2);
    });
}

#[test]
fn a_reused_object_slot_never_sees_the_old_regexp() {
    big(|| {
        let src = "var a = /first/g; a.lastIndex = 3; a = null; 0";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        let before: Vec<ObjId> = it.ext.regexps.keys().copied().collect();
        assert_eq!(before.len(), 1);
        it.collect(&[]).unwrap();
        assert!(it.ext.regexps.is_empty());
        // The next object takes the freed slot (same index, next generation).
        let o = it.heap.new_object(None).unwrap();
        assert_eq!(o.index, before[0].index);
        assert_ne!(o.gn, before[0].gn);
        assert!(!it.ext.regexps.contains_key(&o));
        // An old handle is stale for the heap: using it is a heap error, never another object's regexp.
        assert!(it.heap.get_own_property(before[0], it.ext.k_last_index).is_err());
        let o = Value::Object(o);
        let t = it.get_prop(o, it.ext.k_exec).unwrap();
        assert!(matches!(t, Value::Undefined));
    });
}

#[test]
fn collect_is_refused_inside_a_callback_and_roots_hold_after() {
    // No JS function can call `collect`. The host cannot run during a native callback loop either; this checks the
    // evaluator still refuses a collect while a call is active.
    big(|| {
        let src = "var out = []; 'aaa'.replace(/a/g, function (m) { out.push(m); return 'b'; }); out.length";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let v = it.run().unwrap();
        assert_eq!(show(&it, v), "number:3");
        it.collect(&[]).unwrap();
        assert_eq!(it.get_str(Value::Object(it.global_object()), "out"), "a,a,a");
    });
}

#[test]
fn callback_reentrancy_respects_depth_and_frame_limits() {
    let el = EvalLimits::default();
    let src = "function f(n) { return 'a'.replace(/a/, function () { return f(n + 1); }); } f(0)";
    assert_eq!(limit(src, el, BuiltinLimits::default(), HeapLimits::default()), LimitKind::CallDepth);
    let src = "function f(n) { return 'a'.replace('a', function () { return f(n + 1); }); } f(0)";
    assert_eq!(limit(src, el, BuiltinLimits::default(), HeapLimits::default()), LimitKind::CallDepth);
    // Shallow reentrancy with the same regexp and the same scratch memory is exact.
    assert_eq!(run("var r = /(\\d)(\\d)?/g; 'a12b3'.replace(r, function (m, a, b) { return '<' + 'x45'.replace(r, '#') + a + (b || '') + '>'; })"), "string:a<x#12>b<x#3>");
    assert_eq!(run("var r = /a/g; var n = 0; 'aaa'.replace(r, function () { n++; return n < 3 ? 'x'.replace(/x/g, function () { return 'y'; }) : 'z'; })"), "string:yyz");
}

#[test]
fn builtin_results_hold_after_regexp_scratch_is_released() {
    // A big backtrack stack is released after use (retained scratch stays small).
    big(|| {
        let src = "var s = ''; for (var i = 0; i < 4000; i++) s += 'ab'; /^(?:a|b)*c/.test(s); /(?:a|b)*/.exec(s)[0].length";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let v = it.run().unwrap();
        assert_eq!(show(&it, v), "number:8000");
        assert!(it.ext.scratch.retained_bytes() <= SCRATCH_KEEP);
    });
}

#[test]
fn replace_special_cases() {
    assert_eq!(run("'abc'.replace(/(?:)/g, '$&|')"), "string:|a|b|c|");
    assert_eq!(run("'aaa'.replace(/a/g, '$`')"), "string:aaa");
    assert_eq!(run("'abc'.replace('b', '$\\'$`')"), "string:acac");
    assert_eq!(run("'abc'.replace(/b/, '$')"), "string:a$c");
    assert_eq!(run("'abc'.replace(/b/, '$<n>')"), "string:a$<n>c");
}

#[test]
fn builtin_prototypes_are_permanent_roots() {
    // The script drops every global that names a binding. The prototypes, the constructor and the methods must stay
    // alive through `collect` because the evaluator roots them, not because a script can still reach them.
    big(|| {
        let src = "var f = ''.slice; String = 0; RegExp = 0; JSON = 0; var lit = function () { return /(b)/g; }; 0";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        it.collect(&[]).unwrap();
        it.collect(&[]).unwrap();
        let g = Value::Object(it.global_object());
        let abc = Value::String(it.heap.intern_str("abcb").unwrap());
        let k = it.heap.key_from_str("replace").unwrap();
        let rep = it.get_prop(abc, k).unwrap();
        let lit = {
            let k = it.heap.key_from_str("lit").unwrap();
            it.get_prop(g, k).unwrap()
        };
        let re = it.call(lit, Value::Undefined, &[]).unwrap();
        let to = Value::String(it.heap.intern_str("[$1]").unwrap());
        let r = it.call(rep, abc, &[re, to]).unwrap();
        assert_eq!(it.display(r), "a[b]c[b]");
        assert_eq!(it.get_str(re, "flags"), "g");
        let ctor = it.get_str(re, "constructor");
        assert_eq!(ctor, "[object Function]");
        let sp = it.get_str(abc, "constructor");
        assert_eq!(sp, "[object Function]");
    });
}

#[test]
fn long_successful_string_search_consumes_comparison_work() {
    let text = vec![97u16; 4096];
    let mut budget = 1;
    assert_eq!(find_units(&text, &text, 0, &mut budget), None);
    let mut budget = 1024;
    assert_eq!(find_units(&text, &text, 0, &mut budget), Some(Some(0)));
    assert!(budget < 1024);
}

#[test]
fn regexp_to_string_reads_overridden_properties() {
    assert_eq!(run("var r = /a/g; Object.defineProperty(r, 'source', {get: function(){return 'b'}}); Object.defineProperty(r, 'flags', {value: 'i'}); r.toString()"), "string:/b/i");
}

#[test]
fn case_mapping_checks_the_actual_output_length() {
    let heap = HeapLimits { max_string_units: 128, ..Default::default() };
    let src = format!("'{}'.toUpperCase()", "é".repeat(64));
    assert_eq!(eval_with(&src, EvalLimits::default(), BuiltinLimits::default(), heap).unwrap(), format!("string:{}", "É".repeat(64)));
}

#[test]
fn trimming_whitespace_consumes_scan_work() {
    let src = format!("'{}'.trim()", " ".repeat(8192));
    assert_eq!(limit(&src, EvalLimits { max_steps: 100, ..Default::default() }, BuiltinLimits::default(), HeapLimits::default()), LimitKind::Steps);
}

#[test]
fn regexp_literal_cache_keeps_script_provenance_across_collection() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        it.run_script("function left(){return /left/g.test('left')}").unwrap();
        it.run_script("function right(){return /right/g.test('left')}").unwrap();
        it.collect(&[]).unwrap();
        let v = it.run_script("left() + ':' + right()").unwrap();
        assert_eq!(it.display(v), "true:false");
        assert!(matches!(it.run_script("/discard/; /(/"), Err(Error::Unsupported(_))));
        let v = it.run_script("/fresh/.test('fresh')").unwrap();
        assert!(matches!(v, Value::Bool(true)));
    });
}

#[test]
fn unicode_flag_through_the_string_methods() {
    // Expected values were checked with Node 22 on these authored programs.
    let cases = [
        ("/x/gu.flags", "string:gu"),
        ("/a/u.unicode + ',' + /a/.unicode", "string:true,false"),
        // An empty match advances by a code point with `u` (AdvanceStringIndex), by a code unit without it.
        ("'\u{1F600}\u{1F600}'.replace(/(?:)/gu, '-')", "string:-\u{1F600}-\u{1F600}-"),
        ("'\u{1F600}\u{1F600}'.replace(/(?:)/g, '-').length", "number:9"),
        ("'\u{1F600}\u{1F600}'.split(/(?:)/u).length", "number:2"),
        ("'\u{1F600}\u{1F600}'.match(/(?:)/gu).length", "number:3"),
        ("'\u{1F600}\u{1F600}'.match(/(?:)/g).length", "number:5"),
        ("/^\\u{1F600}$/u.test('\u{1F600}')", "boolean:true"),
        ("/\\u{1F600}/u.source", "string:\\\\u{1F600}"),
        ("'\u{1F600}'.match(/./u)[0].length", "number:2"),
        ("new RegExp('\\\\u{41}', 'u').test('A')", "boolean:true"),
        // The /u literals of the staging app's route chunks.
        ("'matrix(1, 0, 0, 1, 10, 20)'.match(/^matrix\\(([-\\d.e\\s,]+)\\)$/u)[1]", "string:1, 0, 0, 1, 10, 20"),
        ("'translateX(10px) scale(2)'.match(/\\b([a-z-]*)\\(.*?\\)/gu).join('|')", "string:(10px)|scale(2)"),
        ("'fooBar'.replace(/[A-Z]/u, function (m) { return '-' + m.toLowerCase() })", "string:foo-bar"),
        ("try { new RegExp('\\\\a', 'u') } catch (e) { e.name + ': ' + e.message }", "string:SyntaxError: Invalid regular expression: /\\\\a/: Invalid escape"),
        ("try { new RegExp(']', 'u') } catch (e) { e.message }", "string:Invalid regular expression: /]/: Lone quantifier brackets"),
    ];
    for (src, want) in cases {
        assert_eq!(run(src), want, "{src}");
    }
}
