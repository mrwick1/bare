//! Tests of `Interp::inspect` (`eval_inspect.rs`): values as short text for tools.

use super::*;
use crate::heap::{Heap, HeapLimits};
use crate::parse::{parse_script, Limits};

/// Run `src` and inspect its value with `cap`. No host: no value is a node.
fn ins(src: &str, cap: usize) -> String {
    let src = src.to_string();
    // A thread with a big stack, as the other evaluator tests use.
    std::thread::Builder::new().stack_size(64 << 20).spawn(move || {
        let prog = parse_script(&src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::new(HeapLimits::default()).unwrap(), EvalLimits::default()).unwrap();
        let v = it.run().unwrap();
        it.inspect(v, cap, &|_| None)
    }).unwrap().join().unwrap()
}

#[test]
fn primitives_show_as_display_does() {
    assert_eq!(ins("'a \"b\"'", 100), "a \"b\"");
    assert_eq!(ins("undefined", 100), "undefined");
    assert_eq!(ins("-0", 100), "-0");
    assert_eq!(ins("1.5", 100), "1.5");
}

#[test]
fn objects_are_compact_json_in_key_order() {
    assert_eq!(ins("({z: 1, a: 'x\\n\"', 2: true, u: undefined, s: Symbol()})", 100), r#"{"2":true,"z":1,"a":"x\n\""}"#);
    assert_eq!(ins("[undefined, NaN, -0, [], {}]", 100), "[null,null,0,[],{}]");
    assert_eq!(ins("Object.defineProperty({a: 1}, 'h', {value: 2, enumerable: false})", 100), r#"{"a":1}"#);
}

#[test]
fn functions_dates_maps_and_sets() {
    assert_eq!(ins("(function named() {})", 100), "function named()");
    assert_eq!(ins("[() => 1, new Date(NaN)]", 100), r#"["function ()","Invalid Date"]"#);
    assert_eq!(ins("var m = new Map([[1, 'a']]); m.set(m, 2); m", 100), r#"[[1,"a"],["[Circular]",2]]"#);
    assert_eq!(ins("var s = new Set([1, 2, 3]); s.delete(2); s", 100), "[1,3]");
}

#[test]
fn a_getter_that_throws_and_deep_nesting_do_not_stop_the_walk() {
    assert_eq!(ins("({get a() { throw 1 }, b: 2})", 100), r#"{"a":"[threw]","b":2}"#);
    let deep = ins("var o = {}; for (var i = 0; i < 100; i++) o = {o: o}; o", 10_000);
    assert!(deep.ends_with(&format!("\"…\"{}", "}".repeat(64))), "{deep}");
}

#[test]
fn the_cap_cuts_and_gives_the_full_length() {
    assert_eq!(ins("['abc', 'def']", 5), r#"["abc… (cut: 5 of 13 chars)"#);
    assert_eq!(ins("['é', 'é']", 4), r#"["é"… (cut: 4 of 9 chars)"#);
    let big = ins("Array.from({length: 200000}, () => 0)", 10);
    assert!(big.ends_with("… (cut: 10 of more than 200000 chars)"), "{big}");
}
