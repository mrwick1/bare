//! The `with` statement (`eval_with.rs`). The corpus is checked against what Node gave (`with-cases.expected`, made by
//! `tools/js/eval_oracle.mjs`; `tools/js/with_check.sh` re-runs Node). The tests below cover what Node cannot show: which sites
//! are dynamic, the gates, the collector and the size of the interpreter.

use super::*;
use crate::heap::Heap;
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
        _ => it.display(v),
    }
}

/// The corpus format: a limit prints `limit`, a throw `throw:NAME:MESSAGE`.
fn outcome(src: &str) -> String {
    let prog = parse_script(src, Limits::default()).unwrap_or_else(|e| panic!("{src:?}: {e}"));
    let mut it = match Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 500_000, ..Default::default() }) {
        Ok(it) => it,
        Err(Error::Unsupported(u)) => return format!("unsupported:{}", u.what),
        Err(e) => panic!("{src:?}: setup {e:?}"),
    };
    match it.run() {
        Ok(v) => show(&it, v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", esc(&it.get_str(v, "name")), esc(&it.get_str(v, "message"))),
        Err(Error::Throw(v)) => format!("throw-value:{}", show(&it, v)),
        Err(Error::Limit(_)) => "limit".into(),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn corpus_matches_node() {
    let (cases, expected) = (include_str!("../../../tools/fixtures/js/with-cases.txt"), include_str!("../../../tools/fixtures/js/with-cases.expected"));
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
            let got = format!("{name}\t{}", outcome(lines.join("\n").trim_end_matches('\n')));
            if got != w {
                bad.push(format!("node: {w}\nbare: {got}"));
            }
        }
        assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad.join("\n"));
    });
}

/// Run `src` and give the slot of each identifier read named `name`, in source order, after the run.
fn slots_of(src: &str, name: &str) -> Vec<u32> {
    let prog = parse_script(src, Limits::default()).unwrap();
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 100_000, ..Default::default() }).unwrap();
    it.run().unwrap();
    (0..it.prog.stats().exprs)
        .filter(|&i| matches!(it.prog.expr(ExprId::from_index(i)).kind, ExprKind::Ident) && it.names[it.cc.expr_name[i] as usize] == name)
        .map(|i| it.cc.slots.sites[i])
        .collect()
}

#[test]
fn a_site_in_the_body_is_dynamic_and_a_site_outside_stays_resolved() {
    // `x` is read before, inside and after the `with`. The function has no frame (it holds a `with`), so the outside sites are
    // environment sites (hops 0). The site inside the body, and the one in a closure made there, ask the full search.
    let src = "function f(o) { var x = 1; var a = x; with (o) { a = x; var g = function () { return x; }; g(); } return x + a; } f({}); f({}); f({});";
    let s = slots_of(src, "x");
    assert_eq!(s.len(), 4);
    assert_ne!(s[0] & LOCAL, 0, "before the with: resolved");
    assert_eq!(s[1], 0, "in the body: dynamic");
    assert_eq!(s[2], 0, "in a closure made in the body: dynamic");
    assert_ne!(s[3] & LOCAL, 0, "after the with: resolved");
    // A name that a block in the body declares is resolved: no object environment lies between the site and its block.
    let s = slots_of("function f(o) { with (o) { { let k = 1; k = k + 1; } } } f({}); f({}); f({});", "k");
    assert!(s.iter().all(|&x| x & LOCAL != 0), "{s:?}");
}

#[test]
fn a_function_with_a_with_keeps_its_environment() {
    let prog = parse_script("function f(o) { var x = 1; with (o) { x = 2; } return x; } f({}); f({});", Limits::default()).unwrap();
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
    it.run().unwrap();
    assert!(it.cc.scopes.iter().all(|f| f.fr != FR_ON), "no function has a frame");
}

#[test]
fn with_in_a_generator_or_an_async_function_is_gated() {
    for src in ["function* g() { with ({}) {} }", "async function f() { with ({}) {} }", "var o = { async *m() { with ({}) {} } }", "var f = async () => { { with ({}) {} } }"] {
        assert_eq!(outcome(src), "unsupported:with statement in a generator or an async function", "{src}");
    }
    // An arrow or a function inside a generator is plain code.
    assert_eq!(outcome("function* g() { var f = function () { with ({ a: 1 }) { return a; } }; yield f(); } g().next().value"), "number:1");
}

#[test]
fn a_collection_keeps_the_object_of_a_with_that_a_closure_holds() {
    big(|| {
        let prog = parse_script("var f; with ({ p: { v: 41 } }) { f = function () { return p.v + 1; }; } var junk = []; for (var i = 0; i < 200; i++) junk.push({ i });", Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        it.collect(&[]).unwrap();
        assert_eq!(it.run_script("f()").map(|v| it.display(v)).unwrap(), "42");
    });
}

#[test]
fn a_with_that_ends_frees_its_environment_unless_a_closure_holds_it() {
    big(|| {
        let prog = parse_script("var n = 0; for (var i = 0; i < 50; i++) { with ({ k: i }) { n += k; } } n", Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let v = it.run().unwrap();
        assert_eq!(it.display(v), "1225");
        assert!(it.stats().live_envs < 10, "{}", it.stats().live_envs);
    });
}

#[test]
fn the_interpreter_does_not_grow() {
    // `with_any` takes a byte that was padding.
    assert_eq!(std::mem::size_of::<Env>(), 56);
}

#[test]
fn strict_code_in_a_with_throws_when_the_property_is_gone_before_the_read_or_write() {
    // ECMA-262 9.1.1.2.6 and 9.1.1.2.5 (S is true). V8 does not throw here, so Node cannot be the oracle; test262 has the same cases.
    let env = "var env = { b: 0, get [Symbol.unscopables]() { delete env.b; return null; } }; var r; ";
    assert_eq!(outcome(&format!("{env} with (env) {{ try {{ (function () {{ 'use strict'; return b; }})(); r = 'no'; }} catch (e) {{ r = e.name; }} }} r")), "string:ReferenceError");
    assert_eq!(outcome(&format!("{env} with (env) {{ try {{ (function () {{ 'use strict'; b = 1; }})(); r = 'no'; }} catch (e) {{ r = e.name; }} }} r + ',' + ('b' in env)")), "string:ReferenceError,false");
}

#[test]
fn a_name_is_asked_twice_for_a_compound_assignment() {
    // Deviation, recorded in `eval_with.rs`: `x += 1` resolves `x` twice, so a proxy sees two `has` calls for it (ECMA-262 one).
    let src = "var log = []; var p = new Proxy({ x: 1 }, { has(t, k) { if (k === 'x') log.push('has'); return k in t; } }); with (p) { x += 1; } log.length";
    assert_eq!(outcome(src), "number:4");
}
