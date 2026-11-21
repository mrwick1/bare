//! Tests of `api_script.rs`: the completion value, eval, handler code and the iterator methods.
use super::*;
use crate::script;
use bare_js_api::{Intrinsic, Scope, task};

/// The text of a value: its string conversion (`display` is not built).
fn show(s: &mut Scope<'_, Realm>, v: Local<'_>) -> String {
    match s.to_string(v) {
        Ok(t) => s.as_str(t).map(|t| t.to_string_lossy()).unwrap_or_default(),
        Err(_) => "<error>".into(),
    }
}

fn realm() -> Realm {
    script::load("").unwrap().realm
}

/// `run_script` and show the value or the error.
fn run(r: &mut Realm, src: &str) -> String {
    task(r, |s| match s.run_script(src) {
        Ok(v) => show(s, v),
        Err(Error::Throw(v)) => format!("throw:{}", show(s, v)),
        Err(Error::Abort(a)) => format!("abort:{a:?}"),
    })
    .unwrap()
}

#[test]
fn a_script_gives_the_value_of_its_last_expression_statement() {
    let mut r = realm();
    for (src, want) in [
        ("1; 2", "2"),
        ("var x = 5; x * 2", "10"),
        ("'use strict'", "use strict"),
        ("1; if (true) {}", "undefined"),
        ("1; if (true) { 2 }", "2"),
        ("1; if (false) { 2 } else { 3 }", "3"),
        ("var i = 0; while (i < 3) { i++ } ", "2"),
        ("7; for (var k = 0; k < 2; k++) {}", "undefined"),
        ("7; do { 8 } while (false)", "8"),
        ("1; switch (1) { case 1: 4 }", "4"),
        ("1; try { 2 } catch (e) { 3 }", "2"),
        ("1; try { throw 0 } catch (e) { 3 }", "3"),
        ("l: { 9 }", "9"),
        ("function f() { return 1 } f()", "1"),
        ("let a = 3; a", "3"),
        ("", "undefined"),
        ("var z;", "undefined"),
    ] {
        assert_eq!(run(&mut r, src), want, "{src}");
    }
    assert_eq!(r.stack_len(), 0);
}

#[test]
fn a_script_that_throws_or_does_not_parse_is_a_throw() {
    let mut r = realm();
    assert_eq!(run(&mut r, "1; throw new TypeError('x')"), "throw:TypeError: x");
    assert!(run(&mut r, "1 +").starts_with("throw:SyntaxError"));
}

#[test]
fn eval_global_runs_in_the_global_environment_and_keeps_its_lexical_names() {
    let mut r = realm();
    task(&mut r, |s| {
        {
            let ev = s.eval_function().unwrap();
            let t = s.undefined();
            let eval = |s: &mut Scope<'_, Realm>, src: &str| {
                let x = s.string(src).unwrap();
                match s.call(ev, t, &[x]) {
                    Ok(v) => show(s, v),
                    Err(Error::Throw(v)) => format!("throw:{}", show(s, v)),
                    Err(e) => format!("{e:?}"),
                }
            };
            assert_eq!(eval(s, "var ev1 = 4; ev1 + 1"), "5");
            assert_eq!(eval(s, "typeof ev1"), "number");
            assert_eq!(eval(s, "let ev2 = 1; ev2 + 1"), "2");
            assert_eq!(eval(s, "let ev2 = 1; ev2 + 2"), "3", "a lexical name of eval code is not global");
            assert_eq!(eval(s, "typeof ev2"), "undefined");
            assert_eq!(eval(s, "function evf() { return 8 } evf()"), "8");
            assert_eq!(eval(s, "typeof evf"), "function");
            assert!(eval(s, "1 +").starts_with("throw:SyntaxError"));
            let n = s.number(3.0);
            let r = s.eval_global(n).unwrap();
            assert_eq!(s.as_number(r), Some(3.0), "a value that is not a string comes back");
        }
    })
    .unwrap();
}

#[test]
fn handler_code_looks_names_up_in_the_scope_first() {
    let mut r = realm();
    run(&mut r, "var holder = { v: 1, get g() { return this === holder }, m() { return this === holder }, set w(x) { this.v = x } }; var v = 'global'; var other = { v: 'other' }; 0");
    task(&mut r, |s| {
        {
            let g = s.global();
            let (holder, other) = (s.get_str(g, "holder").unwrap(), s.get_str(g, "other").unwrap());
            let f = s.compile_function("h", "event", "w = 5; return [v, g, m(), typeof nope, typeof m, (() => typeof v)()].join()").unwrap();
            let u = s.undefined();
            let out = s.call_scoped(f, u, &[], &[holder, other]).unwrap();
            assert_eq!(show(s, out), "5,true,true,undefined,function,number");
            let v = s.get_str(holder, "v").unwrap();
            assert_eq!(s.as_number(v), Some(5.0));
            // With no scope the global is used.
            let h = s.compile_function("h", "", "return typeof g + v").unwrap();
            let out = s.call_scoped(h, u, &[], &[]).unwrap();
            assert_eq!(show(s, out), "undefinedglobal");
            // Other scripts never see the scope.
            let p = s.run_script("function plain() { return typeof m } plain").unwrap();
            let out = s.call_scoped(p, u, &[], &[holder]).unwrap();
            assert_eq!(show(s, out), "undefined");
            // A body that closes the function is a SyntaxError and defines nothing.
            for body in ["}); globalThis.bad = 1; (function () {", "}, function () { globalThis.bad = 2", "} } {"] {
                assert!(matches!(s.compile_function("h", "event", body), Err(Error::Throw(_))), "{body}");
            }
            let t = s.run_script("typeof bad").unwrap();
            assert_eq!(show(s, t), "undefined");
            // The function has the name and the length of the attribute.
            let (n, l) = (s.get_str(f, "name").unwrap(), s.get_str(f, "length").unwrap());
            assert_eq!((show(s, n), show(s, l)), ("h".to_string(), "1".to_string()));
        }
    })
    .unwrap();
    assert_eq!(r.stack_len(), 0);
}

#[test]
fn iterators_step_arrays_strings_and_script_iterables() {
    let mut r = realm();
    task(&mut r, |s| {
        {
            let take = |s: &mut Scope<'_, Realm>, src: &str| -> String {
                let v = match s.run_script(src) {
                    Ok(v) => v,
                    Err(Error::Throw(e)) => panic!("{src}: {}", show(s, e)),
                    Err(e) => panic!("{src}: {e:?}"),
                };
                let mut it = match s.iter_open(v) {
                    Ok(Some(it)) => it,
                    Ok(None) => return "none".into(),
                    Err(Error::Throw(e)) => return format!("throw:{}", show(s, e)),
                    Err(e) => panic!("{e:?}"),
                };
                let mut out = Vec::new();
                loop {
                    let mark = s.engine().stack_len();
                    match s.iter_next(&mut it) {
                        Ok(Some(x)) => out.push(show(s, x)),
                        Ok(None) => break,
                        Err(Error::Throw(e)) => return format!("throw:{}", show(s, e)),
                        Err(e) => return format!("{e:?}"),
                    }
                    let _ = mark;
                }
                assert!(s.iter_next(&mut it).unwrap().is_none(), "done for good");
                out.join(",")
            };
            assert_eq!(take(s, "[1, 2, 3]"), "1,2,3");
            assert_eq!(take(s, "'a😀b'"), "a,😀,b");
            assert_eq!(take(s, "({ [Symbol.iterator]() { var i = 0; return { next() { return i < 3 ? { value: i++, done: false } : { done: true } } } } })"), "0,1,2");
            assert_eq!(take(s, "undefined"), "none");
            assert_eq!(take(s, "({})"), "none");
            assert_eq!(take(s, "({ [Symbol.iterator]() { return 1 } })"), "throw:TypeError: the result of Symbol.iterator is not an object");
            assert_eq!(take(s, "({ [Symbol.iterator]() { return { next() { throw 'boom' } } } })"), "throw:boom");
            // iter_fail calls `return` and keeps the error.
            let v = s.run_script("var closed = 0; ({ [Symbol.iterator]() { return { next() { return { value: 1, done: false } }, return() { closed++; return {} } } } })").unwrap();
            let mut it = s.iter_open(v).unwrap().unwrap();
            match s.iter_next(&mut it) {
                Ok(x) => assert!(x.is_some()),
                Err(Error::Throw(e)) => panic!("{}", show(s, e)),
                Err(e) => panic!("{e:?}"),
            }
            let e = s.type_error("stop");
            let e = s.iter_fail(&mut it, e);
            assert!(matches!(e, Error::Throw(_)));
            let c = s.run_script("closed").unwrap();
            assert_eq!(s.as_number(c), Some(1.0));
            assert!(s.iter_next(&mut it).unwrap().is_none());
        }
    })
    .unwrap();
}

#[test]
fn symbols_and_the_iterator_intrinsics() {
    let mut r = realm();
    task(&mut r, |s| {
        {
            let sym = s.run_script("Symbol.iterator").unwrap();
            let i = s.intrinsic(Intrinsic::SymbolIterator);
            assert!(s.engine().raw(sym) == s.engine().raw(i));
            let t = s.intrinsic(Intrinsic::SymbolToStringTag);
            let want = s.run_script("Symbol.toStringTag").unwrap();
            assert!(s.engine().raw(t) == s.engine().raw(want));
            let d = s.run_script("Symbol('why')").unwrap();
            let text = s.symbol_description(d).unwrap().unwrap();
            assert_eq!(s.as_str(text).unwrap().to_string_lossy(), "why");
            let nd = s.run_script("Symbol()").unwrap();
            assert!(s.symbol_description(nd).unwrap().is_none());
            let n = s.number(1.0);
            assert!(s.symbol_description(n).is_err());
            // %IteratorPrototype% gives `this` for `Symbol.iterator`, so an object that inherits from it can be iterated by the host.
            let p = s.intrinsic(Intrinsic::IteratorPrototype);
            let g = s.global();
            s.define_str(g, "ip", p, bare_js_api::Attrs::ALL).unwrap();
            let v = s.run_script("var o = Object.create(ip); var n = 0; o.next = function () { return n++ < 2 ? { value: n, done: false } : { done: true } }; o").unwrap();
            let mut it = s.iter_open(v).unwrap().unwrap();
            let a = s.iter_next(&mut it).unwrap().unwrap();
            assert_eq!(s.as_number(a), Some(1.0));
        }
    })
    .unwrap();
}

/// Indirect eval from a script, under constant collection (a tiny trigger: nearly every op that allocates collects first).
#[test]
fn an_indirect_eval_called_by_a_script_survives_constant_collection() {
    let mut r = realm();
    task(&mut r, |s| {
        let ev = s.eval_function().unwrap();
        let g = s.global();
        s.set_str(g, "ev", ev).unwrap();
    })
    .unwrap();
    r.heap.set_trigger(crate::heap::Trigger { min: 1 << 10, slack: 1 << 10 });
    let src = "var t = []; for (var i = 0; i < 20; i++) t.push({ i: i, s: 'k' + i });
               var a = ev('var q = []; for (var j = 0; j < 50; j++) q.push({ j: j, s: \"v\" + j }); q.length');
               var b = ev('q.map(function (n) { return n.s + t.length; }).join(\"\")');
               [a, t.length, ev('1 + 1'), b.length].join()";
    assert_eq!(run(&mut r, src), "50,20,2,240");
}
