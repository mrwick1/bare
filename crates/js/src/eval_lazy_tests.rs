//! Tests of lazy function bodies at run time: a function that the parser checked and dropped runs as it would have run
//! with its body kept, and is parsed once for all realms of the thread.

use super::*;
use crate::heap::Heap;

fn realm_with(l: EvalLimits) -> Interp<'static> {
    Interp::realm(Heap::default(), l).unwrap()
}

fn realm() -> Interp<'static> {
    realm_with(EvalLimits { max_steps: 50_000_000, ..Default::default() })
}

/// Filler that makes a body long enough to be dropped.
const PAD: &str = "var p1 = 1, p2 = 2, p3 = p1 + p2, p4 = p3 * 2, p5 = p4 - p1, p6 = p5 + p2, p7 = p6 * p3;";

/// Run `src` in a realm and show its completion value.
fn run(src: &str) -> String {
    let mut it = realm();
    let r = it.run_script(src);
    match r {
        Ok(v) => it.display(v),
        Err(Error::Throw(v @ Value::Object(_))) => format!("throw:{}", it.get_str(v, "name")),
        Err(Error::Throw(v)) => format!("throw:{}", it.display(v)),
        Err(e) => format!("{e:?}"),
    }
}

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

#[test]
fn a_dropped_function_runs_with_its_closure_and_arguments() {
    big(|| {
        let src = format!(
            "var outer = 10; let blk = 5;
             function add(a, b) {{ {PAD} return a + b + outer + blk + p7 + arguments.length; }}
             function counter() {{ {PAD} var n = 0; return function () {{ {PAD} n += 1; return n + p7; }}; }}
             var c = counter(); c(); var second = c();
             function fact(n) {{ {PAD} return n <= 1 ? 1 : n * fact(n - 1); }}
             function Box(v) {{ {PAD} this.v = v + p1; }}
             Box.prototype.get = function () {{ {PAD} return this.v * 2 + p2; }};
             var b = new Box(4);
             [add(1, 2), second, fact(5), b.v, b.get(), b instanceof Box].join()"
        );
        // p7 is p6 * p3 = ((p5 + p2) * p3) = ((p4 - p1 + 2) * 3) = ((6 - 1 + 2) * 3) = 21
        assert_eq!(run(&src), "41,23,120,5,12,true");
    });
}

#[test]
fn nested_dropped_functions_this_and_strictness_hold() {
    big(|| {
        let src = format!(
            "function outerFn() {{ {PAD}
               function inner() {{ {PAD} return this === undefined ? 'strict' : typeof this; }}
               return inner() + '/' + (function () {{ 'use strict'; {PAD} return this === undefined; }})();
             }}
             var o = {{ m: function () {{ {PAD} return this === o; }} }};
             [outerFn(), o.m(), (0, o.m)()].join()"
        );
        assert_eq!(run(&src), "object/true,true,false");
    });
}

#[test]
fn an_uncalled_function_with_an_early_error_fails_the_script() {
    big(|| {
        let src = format!("function never() {{ {PAD} let x; let x; }} 1");
        assert_eq!(run(&src), "throw:SyntaxError");
    });
}

#[test]
fn a_thrown_error_in_a_dropped_function_reaches_the_caller() {
    big(|| {
        let src = format!("function f() {{ {PAD} throw new RangeError('bad ' + p7); }} try {{ f(); }} catch (e) {{ e.name + ':' + e.message }}");
        assert_eq!(run(&src), "RangeError:bad 21");
    });
}

#[test]
fn two_realms_share_one_parse_of_a_dropped_function() {
    big(|| {
        let src = format!("function f(x) {{ {PAD} return x + p7; }} f(1)");
        let mut a = realm();
        let mut b = realm();
        let (ra, rb) = (a.run_script(&src).unwrap(), b.run_script(&src).unwrap());
        assert_eq!((a.display(ra), b.display(rb)), ("22".into(), "22".into()));
        assert!(a.stats().script_nodes > 0 && b.stats().script_nodes > 0);
    });
}

#[test]
fn a_dropped_function_is_charged_to_the_node_limit_when_it_is_first_called() {
    big(|| {
        let src = format!("function f() {{ {PAD} return 1; }} 1");
        let mut it = realm();
        it.run_script(&src).unwrap();
        let before = it.stats().script_nodes;
        let call = it.run_script("f()");
        assert!(call.is_ok());
        assert!(it.stats().script_nodes > before, "the first call charges the nodes of the body");
        let again = it.stats().script_nodes;
        it.run_script("f()").unwrap();
        assert_eq!(it.stats().script_nodes, again, "a second call parses and charges nothing");
        // A realm with a node limit that the script fits but its body does not: the call fails with the node limit.
        let mut small = realm_with(EvalLimits { max_script_nodes: 200, max_steps: 50_000_000, ..Default::default() });
        let src2 = format!("function g() {{ {PAD} {PAD} {PAD} {PAD} {PAD} return 1; }} 1");
        match small.run_script(&src2) {
            Ok(_) => assert!(matches!(small.run_script("g()"), Err(Error::Limit(LimitKind::ScriptNodes)))),
            Err(Error::Limit(LimitKind::ScriptNodes)) => {}
            Err(e) => panic!("{e:?}"),
        }
    });
}

#[test]
fn a_failed_first_call_leaves_the_realm_usable() {
    big(|| {
        let src = format!("function f() {{ {PAD} return 1; }} function h() {{ {PAD} return 2; }} 1");
        let mut it = realm_with(EvalLimits { max_script_nodes: 1_000_000, max_steps: 50_000_000, ..Default::default() });
        it.run_script(&src).unwrap();
        let v = it.run_script("f() + h()").unwrap();
        assert_eq!(it.display(v), "3");
    });
}
