//! Tests of data literals: the text is accepted or refused, and the values equal the values of the syntax tree.

use super::*;
use crate::ast::ExprKind;
use crate::eval::{Error, EvalLimits, Interp};
use crate::heap::Heap;
use crate::parse::{parse_script, parse_script_lazy_used, Limits};

fn scan_str(s: &str) -> Option<usize> {
    scan(s, 0)
}

#[test]
fn json_values_are_data_and_the_scan_stops_at_the_close() {
    for s in ["[1]", "[]", "{}", "[ 1 , 2 ]", "{\"a\":1}", "{ \"a\" : [ true , false , null ] }", "[-0,0.5,1e5,1E+5,1e-5,-1.5e10]", "[\"a\\n\\u00e9\\\"\"]", "[[[[1]]]]", "[\"é😀\"]", "{\"\":1}"] {
        assert_eq!(scan_str(s), Some(s.len()), "{s}");
        assert_eq!(scan_str(&format!("{s}.length")), Some(s.len()), "{s}.length");
        assert_eq!(scan_str(&format!("{s}\n+ x")), Some(s.len()), "{s} newline");
    }
}

#[test]
fn text_that_json_does_not_accept_is_not_data() {
    for s in [
        "[1,]", "[1,,2]", "[,]", "{a:1}", "{'a':1}", "[+1]", "[01]", "[1.]", "[.5]", "[0x10]", "[1_0]", "[1n]", "[\"a\\x41\"]", "[\"a\\0\"]", "[\"a\tb\"]", "[- 1]", "[undefined]",
        "[NaN]", "[Infinity]", "[x]", "[1 2]", "{\"a\" 1}", "{\"a\":1,}", "{\"a\":1 \"b\":2}", "[1", "{\"a\":", "[\"a", "[/* c */ 1]", "[1 // c\n]", "[...a]", "[`a`]", "[1,\u{a0}2]",
        "{\"__proto__\":1}", "{\"a\":1,\"__proto__\":{}}", "{\"__pro\\u0074o__\":1}", "{\"\\u005f_proto__\":1}", "[{\"x\":[{\"__proto__\":0}]}]", "{[\"a\"]:1}", "{\"a\"}",
        "[truex]", "[1a]", "[1.5.5]", "[nul]", "{1:2}",
    ] {
        assert_eq!(scan_str(s), None, "{s}");
    }
    // Text after a literal that does not belong to it is not an error of the scan.
    assert_eq!(scan_str("{\"a\":1}}"), Some(7));
    assert_eq!(scan_str("[1]]"), Some(3));
    // A deep literal is left to the tree, which has its own depth limit.
    let deep = |n: usize| format!("{}1{}", "[".repeat(n), "]".repeat(n));
    assert!(scan_str(&deep(64)).is_some());
    assert!(scan_str(&deep(65)).is_none());
}

/// A literal of at least `MIN_BYTES` that holds `items` and numbers that fill it up.
fn array(items: &[&str]) -> String {
    let mut s = format!("[{}", items.join(","));
    let mut i = 0;
    while s.len() < MIN_BYTES {
        s.push_str(&format!(",{i}"));
        i += 1;
    }
    s.push(']');
    s
}

fn object(items: &[&str]) -> String {
    let mut s = format!("{{{}", items.join(","));
    let mut i = 0;
    while s.len() < MIN_BYTES {
        s.push_str(&format!(",\"filler{i}\":{i}"));
        i += 1;
    }
    s.push('}');
    s
}

const TRICKY_VALUES: &[&str] = &[
    "-0", "0", "1e400", "-1e400", "1.5e-7", "123456789012345678901234567890", "0.1", "9007199254740993", "5e-324", "1.7976931348623157e308", "1e-400", "0.30000000000000004", "-123",
    "\"a\\\"b\\\\c\\/d\\b\\f\\n\\r\\t\"", "\"\\u00e9\\ud83d\\ude00\\ud800\"", "\"é😀\u{2028}\"", "\"\"", "\"__proto__\"", "true", "false", "null", "[]", "{}", "[[1,[2,[3]]]]",
    "{\"a\":{\"b\":[{}]}}", "{\"x\":\"__proto__\"}", "[ 1 ,\n\t2 ]",
];

const TRICKY_KEYS: &[&str] = &[
    "\"b\":1", "\"a\":2", "\"2\":3", "\"1\":4", "\"01\":5", "\"4294967295\":6", "\"4294967294\":7", "\"-1\":8", "\"1.5\":9", "\"constructor\":10", "\"length\":11", "\"toString\":12", "\"a\":13", "\"\":14",
    "\"é\":15", "\"\\u0061b\":16", "\"__proto__x\":17", "\"x\\\"y\":18", "\"__proto\":19", "\"0\":20", "\"hasOwnProperty\":{\"__proto__x\":[1]}",
];

/// Shows a value with its prototype, key order, attributes and the sign of zero.
const DUMP: &str = "function dump(v) {
  if (v === null) return 'null';
  switch (typeof v) {
    case 'number': return Object.is(v, -0) ? '-0' : String(v);
    case 'string': return JSON.stringify(v) + '#' + v.length;
    case 'boolean': return String(v);
    case 'object': {
      var arr = Array.isArray(v);
      var head = Object.getPrototypeOf(v) === (arr ? Array.prototype : Object.prototype) ? '' : '!proto';
      var d = Reflect.ownKeys(v).map(function (k) {
        var x = Object.getOwnPropertyDescriptor(v, k);
        return String(k) + (x.writable && x.enumerable && x.configurable ? '' : '?') + '=' + dump(x.value);
      });
      return head + (arr ? 'A' : 'O') + '{' + d.join(';') + '}';
    }
    default: return typeof v;
  }
}
";

fn padded(src: &str) -> String {
    format!("/*{}*/{src}", " ".repeat(crate::parse_cache::LAZY_MIN_SOURCE))
}

fn run(src: &str) -> String {
    let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }).unwrap();
    match it.run_script(src) {
        Ok(v) => it.display(v),
        Err(Error::Throw(v)) => format!("throw:{}", it.display(v)),
        Err(e) => format!("{e:?}"),
    }
}

/// The data nodes of the tree that a realm makes of `src` (the lazy kind: only a source of `LAZY_MIN_SOURCE` bytes or more has data nodes).
fn data_nodes(src: &str) -> usize {
    let p = crate::parse_cache::parse(crate::parse_cache::Kind::ClassicLazy, src, Limits::default()).unwrap();
    p.exprs.iter().filter(|e| matches!(e.kind, ExprKind::Data)).count()
}

/// The result of the program on the tree (short source) and on the text (padded source). Both must agree.
fn same(program: &str, data: usize) -> String {
    let src = format!("{DUMP}{program}");
    let big = padded(&src);
    if data != usize::MAX {
        assert_eq!(data_nodes(&big), data, "data nodes in {program:.60}");
    }
    assert_eq!(data_nodes(&src), 0);
    let (tree, text) = (run(&src), run(&big));
    assert_eq!(tree, text, "{program:.80}");
    tree
}

#[test]
fn an_array_of_values_equals_its_tree() {
    let lit = array(TRICKY_VALUES);
    same(&format!("dump({lit})"), 1);
    // Each value alone, at the front and at the end.
    for v in TRICKY_VALUES {
        same(&format!("dump({})", array(&[v])), 1);
        same(&format!("dump({})", array(&[v, v, v])), 1);
    }
}

#[test]
fn an_object_of_values_equals_its_tree() {
    let r = same(&format!("dump({})", object(TRICKY_KEYS)), 1);
    // The tree gives integer keys first in order, then the others as they came, a repeated key at its first place.
    assert!(r.starts_with("O{0=20;1=4;2=3;4294967294=7;b=1;a=13;"), "{r}");
    for k in TRICKY_KEYS {
        same(&format!("dump({})", object(&[k])), 1);
    }
    let nested: Vec<String> = TRICKY_VALUES.iter().enumerate().map(|(i, v)| format!("\"k{i}\":{v}")).collect();
    same(&format!("dump({})", object(&nested.iter().map(String::as_str).collect::<Vec<_>>())), 1);
}

#[test]
fn a_proto_key_keeps_its_tree() {
    for k in ["\"__proto__\":null", "\"__proto__\":[1]", "\"__pro\\u0074o__\":{\"z\":1}", "\"__proto__\":1"] {
        let lit = object(&["\"a\":1", k]);
        let r = same(&format!("dump({lit})"), 0);
        assert!(r.contains("a=1"), "{r}");
    }
    // An inner literal without the key is data even when the outer one is not.
    let inner = array(&["1"]);
    let lit = object(&["\"__proto__\":null", &format!("\"in\":{inner}")]);
    same(&format!("dump({lit})"), 1);
}

#[test]
fn a_literal_inside_an_expression_goes_on_with_the_tokens_after_it() {
    let (a, o) = (array(&["7"]), object(&["\"k\":8"]));
    assert_eq!(same(&format!("{a}.length + {o}.k"), 2), "1031.0".replace("1031.0", &(a.matches(',').count() + 1 + 8).to_string()));
    same(&format!("var x = {a}\nvar y = 2; x.length + y"), 1);
    same(&format!("var f = function (p, q) {{ return p.length + q; }}; f({a}, {o}.k) + [{o}][0].k"), 3);
    same(&format!("{a} ? 1 : 2"), 1);
    // The outer array holds data only, so it is one node whose text holds the others.
    same(&format!("dump([{a}, {o}, {a}])"), 1);
    same(&format!("dump({{ \"x\": {a}, y: {o} }})"), 2);
    same(&format!("var [p, q] = {a}; p + q"), 1);
    same(&format!("var {{ k, filler0 }} = {o}; k + filler0"), 1);
    same(&format!("`${{{a}.length}}` + (() => ({o}).k)()"), 2);
    // A dropped arrow has no node of its literal in the first tree: the node is in the tree of the second parse.
    same(&format!("var h = () => ({o}).k; h()"), 0);
}

#[test]
fn a_literal_is_never_a_target_or_a_parameter() {
    let (a, o) = (array(&["7"]), object(&["\"k\":8"]));
    for src in [format!("{a} = 1"), format!("({a}) => 1"), format!("({o}) => 1"), format!("({a} = 1) => 1"), format!("[{a}] = 1"), format!("for ({a} of []) ;"), format!("{a}++"), format!("({o} = 1)")] {
        let big = padded(&src);
        let (tree, text) = (parse_script(&src, Limits::default()).err(), parse_script_lazy_used(&big, Limits::default()).0.err());
        assert!(tree.is_some() && text.is_some(), "{src:.50}");
        assert_eq!((tree.unwrap().kind, tree.map(|e| e.msg)), (text.unwrap().kind, text.map(|e| e.msg)), "{src:.50}");
    }
}

#[test]
fn a_literal_in_a_function_is_a_new_value_for_each_call() {
    let a = array(&["1"]);
    let src = format!("function f() {{ return {a}; }} var x = f(), y = f(); x[0] = 99; [x === y, y[0], x.length === y.length].join()");
    // The literal sits in a function body that is dropped: its node is in the tree of the second parse.
    assert_eq!(same(&src, usize::MAX), "false,1,true");
    let g = format!("function* g() {{ var a = {a}; yield a.length; a.push(5); return a.length; }} var it = g(), r = [it.next().value, it.next().value]; r.join()");
    same(&g, usize::MAX);
}

#[test]
fn a_literal_that_is_not_json_stays_a_tree_with_the_same_values() {
    let tail: String = (0..300).map(|i| format!(",{i}")).collect();
    for lit in [format!("[1, x{tail}]"), format!("[1{tail},]"), format!("[1,,2{tail}]"), format!("[+1{tail}]"), format!("['a'{tail}]"), format!("{{a: 1{}}}", (0..300).map(|i| format!(",k{i}: {i}")).collect::<String>())] {
        let src = format!("var x = 5; var n = {lit}.length");
        assert_eq!(data_nodes(&padded(&src)), 0, "{lit:.30}");
    }
    // A literal with code inside has data inside: the part that is long enough and plain.
    let inner = array(&["3"]);
    let src = format!("var x = 5; var r = [x, {inner}, x]; r.length + r[1].length");
    assert_eq!(data_nodes(&padded(&src)), 1);
    assert_eq!(run(&padded(&src)), run(&src));
}

#[test]
fn a_short_literal_and_a_small_source_stay_trees() {
    assert_eq!(data_nodes(&padded("var x = [1,2,3,4]; x.length")), 0);
    let lit = array(&["1"]);
    assert_eq!(data_nodes(&format!("var x = {lit};")), 0, "a source under the lazy threshold is eager");
    assert_eq!(data_nodes(&padded(&format!("var x = {lit};"))), 1);
    // The tree of the literal is one node, not one for each value.
    let src = padded(&format!("var x = {lit};"));
    let p = crate::parse_cache::parse(crate::parse_cache::Kind::ClassicLazy, &src, Limits::default()).unwrap();
    assert!(p.stats().nodes() < 10, "{:?}", p.stats());
}

#[test]
fn heap_limits_stop_a_literal_like_a_tree() {
    let lit = array(&["1"]);
    let src = padded(&format!("var x = {lit}; x.length"));
    let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
    it.run_script(&src).unwrap();
    let tiny = crate::heap::HeapLimits { max_bytes: 200_000, ..Default::default() };
    let mut h = Heap::new(tiny).unwrap();
    let o = h.new_object(None).unwrap();
    let a = h.new_array(None).unwrap();
    let mut seen = 0;
    for n in [50usize, 100_000] {
        let lit = format!("[{}]", (0..n).map(|i| i.to_string()).collect::<Vec<_>>().join(","));
        match build(&mut h, o, a, &lit) {
            Ok(_) => seen += 1,
            Err(e) => assert!(matches!(e, crate::heap::HeapError::Limit(_)), "{e:?}"),
        }
    }
    assert_eq!(seen, 1);
}

#[test]
fn the_bytecode_modes_agree_on_a_data_literal() {
    let a = array(&["1", "\"é\""]);
    let o = object(&["\"k\":8"]);
    let src = padded(&format!("{DUMP} function f() {{ var a = {a}; var o = {o}; return dump(a) + dump(o); }} f() + f()"));
    let want = run(&src);
    for mode in [crate::eval::Exec::Auto, crate::eval::Exec::Byte] {
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }).unwrap();
        it.set_exec(mode);
        let v = it.run_script(&src).unwrap();
        assert_eq!(it.display(v), want);
    }
}
