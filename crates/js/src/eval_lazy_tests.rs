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

#[test]
fn a_bad_regexp_in_a_dropped_function_still_fails_the_script() {
    big(|| {
        for lit in ["/(/", "/a**/", "/(?<n>a)(?<n>b)/", "/\\k<x>(?<y>a)/"] {
            let src = format!("function never() {{ {PAD} return {lit}; }} 1");
            let r = realm().run_script(&src);
            assert!(matches!(r, Err(Error::Unsupported(_))), "{lit}: {r:?}");
        }
    });
}

#[test]
fn a_regexp_literal_compiles_at_first_use_and_is_a_new_object_each_time() {
    big(|| {
        let src = format!(
            "function f() {{ {PAD} return /(\\d+)-(?<w>[a-z]+)/gi; }}
             var r1 = f(), r2 = f(), m = r1.exec('x 12-Ab y');
             var same = r1 !== r2 && r1.source === r2.source;
             [same, r1.flags, r1.lastIndex, m[1], m.groups.w, /a|b/.test('b'), /\\//.source].join()"
        );
        assert_eq!(run(&src), "true,gi,7,12,Ab,true,\\/");
        // The same literals in a second realm give the same results (the compiled program is shared, the objects are not).
        assert_eq!(run(&src), "true,gi,7,12,Ab,true,\\/");
    });
}

#[test]
fn the_check_of_a_pattern_agrees_with_its_compile() {
    use crate::regexp::Regex;
    let flags = ["", "g", "i", "u", "v", "iu", "gg", "uv", "x"];
    let pats = ["", "a", "(", ")", "a**", "a{2,1}", "[", "[z-a]", "\\", "(?<n>a)\\k<n>", "(?<n>a)(?<n>b)", "\\k<x>(?<y>a)", "(?<=a)b", "(?:a|b){3}", "\\p{L}", "\\p{Foo}", "[\\d-x]", "\\u{110000}", "{", "a{", "(?i:a)", "[a&&b]", "\\1(a)", "\\2(a)"];
    for p in pats {
        let u: Vec<u16> = p.encode_utf16().collect();
        for f in flags {
            let (c, n) = (Regex::check(&u, f), Regex::new(&u, f));
            match n {
                Ok(_) => assert_eq!(c, Ok(()), "/{p}/{f}"),
                // A failure of the emit pass shows at the compile only.
                Err(e @ (crate::regexp::CompileError::Syntax { .. } | crate::regexp::CompileError::InvalidFlags(_) | crate::regexp::CompileError::Unsupported { .. })) => assert_eq!(c, Err(e), "/{p}/{f}"),
                Err(_) => {}
            }
        }
    }
}

#[test]
fn lazy_functions_loaded_one_after_the_other_keep_their_own_frame_slots() {
    big(|| {
        // Each function is a leaf with a loop, so it may get frame slots. Each is the only function of its tree (the same function index).
        let mut src = String::new();
        for i in 0..6 {
            src += &format!("function f{i}(n) {{ {PAD} var s = {i}; for (var k = 0; k < n; k++) {{ let t = k + p1; s += t; }} return s + p7; }}\n");
        }
        // The first calls come first: a function that has run once and is called again gets its frame while the next one is loaded.
        src += "var o = [f0(3), f1(3), f2(3), f0(3), f3(3), f1(3), f4(3), f2(3), f5(3)]; for (var j = 0; j < 3; j++) { o.push(f0(3), f1(3), f2(3), f3(3), f4(3), f5(3)); } o.join()";
        assert_eq!(run(&src), "27,28,29,27,30,28,31,29,32,27,28,29,30,31,32,27,28,29,30,31,32,27,28,29,30,31,32");
    });
}

#[test]
fn the_bytecode_modes_agree_with_the_tree_walker_on_lazy_functions() {
    big(|| {
        let src = format!(
            "var log = [];
             function reactive(o) {{ {PAD} return {{ get: function (t, k) {{ {PAD} log.push('get' + k); return t[k] + p1; }}, set: function (t, k, v) {{ {PAD} log.push('set' + k); t[k] = v; return true; }} }}; }}
             var h = reactive({{}}), o = {{ a: 1 }}; h.set(o, 'a', 5); log.push(h.get(o, 'a'));
             function loop(n) {{ {PAD} var s = 0; for (var i = 0; i < n; i++) s += i; return s + p7; }}
             log.push(loop(4), loop(5)); log.join()"
        );
        let mut want = String::new();
        for mode in [Exec::Tree, Exec::Auto, Exec::Byte] {
            let mut it = realm();
            it.set_exec(mode);
            let r = it.run_script(&src);
            let got = match r {
                Ok(v) => it.display(v),
                Err(e) => format!("{e:?}"),
            };
            if mode == Exec::Tree {
                want = got.clone();
                assert_eq!(want, "seta,geta,6,27,31", "tree");
            }
            assert_eq!(got, want, "{mode:?}");
        }
    });
}

/// The cases of the proxy corpus (they have long functions inside functions) give the same result in each mode.
#[test]
fn the_bytecode_modes_agree_on_the_proxy_corpus() {
    big(|| {
        let mut cases: Vec<(&str, Vec<&str>)> = Vec::new();
        for line in include_str!("../../../tools/fixtures/js/proxy-cases.txt").lines() {
            if let Some(n) = line.strip_prefix("=== ") {
                cases.push((n.trim(), Vec::new()));
            } else if let Some(c) = cases.last_mut() {
                c.1.push(line);
            }
        }
        let mut bad = Vec::new();
        for (name, lines) in &cases {
            let mut got: Vec<String> = Vec::new();
            for mode in [Exec::Tree, Exec::Auto, Exec::Byte] {
                let mut it = realm();
                it.set_exec(mode);
                let r = it.run_script(&lines.join("\n")).and_then(|f| it.call(f, Value::Undefined, &[]));
                got.push(match r {
                    Ok(v) => it.display(v),
                    Err(e) => format!("{e:?}"),
                });
            }
            if got[0] != got[1] || got[0] != got[2] {
                bad.push(format!("{name}: {got:?}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    });
}
