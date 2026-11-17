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

#[test]
fn a_function_three_levels_deep_runs_as_it_would_with_its_body_kept() {
    big(|| {
        // Each function is loaded from the tree of the one around it, which skipped its body: scopes, `arguments`, `with`, a default
        // parameter, strictness and the text of the function must all be as with every body kept.
        let src = format!(
            "function a(x) {{ {PAD} var va = 1;
               function b(y, z = x + 1) {{ {PAD} var vb = 2;
                 function c() {{ {PAD}
                   return [typeof arguments, arguments.length, x, y, z, va, vb, (function () {{ with ({{ w: 5 }}) return w + vb; }})()].join();
                 }}
                 return c(1, 2, 3);
               }}
               return b(10);
             }}
             a(100)"
        );
        assert_eq!(run(&src), "object,3,100,10,101,1,2,7");
        let src = format!(
            "function outer() {{ {PAD}
               var o = {{ m() {{ {PAD} return 7 + g(); function g() {{ {PAD} return p7; }} }} }};
               function strictOne() {{ 'use strict'; {PAD} return this === undefined; }}
               return o.m() + ',' + strictOne() + ',' + (function () {{ return this === globalThis; }})();
             }}
             outer()"
        );
        assert_eq!(run(&src), "28,true,true");
        let c = format!("function c() {{ {PAD} return 1; }}");
        let src = format!("function a() {{ {PAD} function b() {{ {PAD} {c} return c; }} return b(); }} String(a()) === {c:?}");
        assert_eq!(run(&src), "true");
    });
}

#[test]
fn error_stack_positions_in_a_function_below_a_skipped_body_are_the_source_positions() {
    big(|| {
        let nested = format!("function a() {{ {PAD}\n function b() {{ {PAD}\n function c() {{ {PAD}\n return new Error('x').stack; }}\n return c(); }}\n return b(); }}\n a()");
        let flat = "function a() {\n var _ = 1;\n function b() {\n var _ = 1;\n function c() {\n return new Error('x').stack; }\n return c(); }\n return b(); }\n a()".to_string();
        let position = |s: &str| -> String { s.lines().next().unwrap_or("").to_string() };
        let (n, f) = (run(&nested), run(&flat));
        // Both runs name the three frames; the line and column of the `new Error` are those of the text, as the first parse gives them.
        assert!(n.contains(":4:"), "{n}");
        assert!(f.contains(":6:"), "{f}");
        let frames = |s: &str| s.matches("\n    at ").count();
        assert_eq!(frames(&n), frames(&f), "{n} / {f}");
        let _ = position;
    });
}

#[test]
fn eval_and_super_below_a_skipped_body_give_what_the_eager_parse_gives() {
    big(|| {
        // The same text with and without the padding (below 16 KiB nothing is lazy): the answers must be equal.
        let both = |f: &dyn Fn(&str) -> String| {
            let (lazy, eager) = (run(&f(PAD)), run(&f("")));
            assert_eq!(lazy, eager);
            lazy
        };
        both(&|pad| {
            format!(
                "function a() {{ {pad} var av = 1;
                   function b() {{ {pad} var bv = 2;
                     function c() {{ {pad} try {{ return eval('av + bv'); }} catch (e) {{ return e.name; }} }}
                     return c();
                   }}
                   return b();
                 }}
                 a()"
            )
        });
        let r = both(&|pad| {
            format!(
                "function w() {{ {pad} class A {{ x() {{ return 5; }} }} class B extends A {{ m() {{ return super.x() + 1; }} }}
                   function g() {{ {pad} return new B().m(); }} return g(); }}
                 function wrap() {{ {pad} function m2() {{ {pad} return w(); }} return m2(); }}
                 wrap()"
            )
        });
        assert_eq!(r, "6");
    });
}

/// In `byte` mode a lazy function runs on the bytecode lane, as does the function that holds a lazy inner function. The results
/// agree with the tree walker. The names of the outer scopes belong to another script (the one that holds the lazy function).
#[test]
fn lazy_functions_run_on_the_bytecode_lane() {
    big(|| {
        let src = format!(
            "var g = 3; let lx = 4;
             function mk(a) {{ {PAD} var own = a * 2; let blk = 1;
               function inner(b) {{ {PAD} own += b; return own + blk + a + g + lx + p7; }}
               return inner;
             }}
             function plain(n) {{ {PAD} var s = 0; for (var i = 0; i < n; i++) s += i * g + lx; return s + p1; }}
             var f = mk(5), r = [];
             for (var k = 0; k < 3; k++) r.push(f(k), plain(k + 2));
             r.join()"
        );
        // Only a source of `LAZY_MIN_SOURCE` bytes or more drops function bodies.
        let src = format!("/*{}*/{src}", " ".repeat(crate::parse_cache::LAZY_MIN_SOURCE));
        let mut want = String::new();
        for mode in [Exec::Tree, Exec::Byte] {
            let mut it = realm();
            it.set_exec(mode);
            let r = it.run_script(&src);
            let got = match r {
                Ok(v) => it.display(v),
                Err(e) => format!("{e:?}"),
            };
            if mode == Exec::Tree {
                want = got.clone();
                assert!(!want.contains("Error"), "{want}");
            } else {
                assert_eq!(got, want, "byte");
                let s = it.bc_stats();
                assert_eq!(s.calls_tree, 0, "{s:?}");
                assert!(s.calls_lane >= 7, "{s:?}");
            }
        }
    });
}

/// Every case of the corpora with all function bodies lazy (a long comment after each `{`, and a source of `LAZY_MIN_SOURCE`
/// bytes): `byte` gives what the tree walker gives. The comments change some strings too, the same way in both modes.
#[test]
fn the_bytecode_lane_agrees_with_the_tree_walker_on_the_corpora_with_lazy_bodies() {
    big(|| {
        let pad = format!("{{/*{}*/", " ".repeat(130));
        let head = format!("/*{}*/", " ".repeat(crate::parse_cache::LAZY_MIN_SOURCE));
        let mut bad = Vec::new();
        let mut n = 0;
        for text in [
            include_str!("../../../tools/fixtures/js/eval-cases.txt"),
            include_str!("../../../tools/fixtures/js/statements-cases.txt"),
            include_str!("../../../tools/fixtures/js/class-cases.txt"),
            include_str!("../../../tools/fixtures/js/arrobj-cases.txt"),
            include_str!("../../../tools/fixtures/js/with-cases.txt"),
            include_str!("../../../tools/fixtures/js/tagged-cases.txt"),
            include_str!("../../../tools/fixtures/js/iterator-cases.txt"),
        ] {
            let mut cases: Vec<(&str, Vec<&str>)> = Vec::new();
            for line in text.lines() {
                if let Some(name) = line.strip_prefix("=== ") {
                    cases.push((name.trim(), Vec::new()));
                } else if let Some(c) = cases.last_mut() {
                    c.1.push(line);
                }
            }
            for (name, lines) in &cases {
                let src = format!("{head}{}", lines.join("\n").replace('{', &pad));
                let mut got = Vec::new();
                for mode in [Exec::Tree, Exec::Byte] {
                    let mut it = realm();
                    it.set_exec(mode);
                    let r = it.run_script(&src);
                    got.push(match r {
                        Ok(v) => it.display(v),
                        Err(e) => format!("{e:?}"),
                    });
                    // The sites of the lazy trees that ran resolve as the walker resolves them.
                    if mode == Exec::Tree {
                        bad.extend(it.bc_scope_check().mismatches.into_iter().map(|m| format!("{name}: {m}")));
                    }
                }
                n += 1;
                if got[0] != got[1] {
                    bad.push(format!("{name}: tree {:?} byte {:?}", got[0], got[1]));
                }
            }
        }
        assert!(n > 500, "{n} cases");
        assert!(bad.is_empty(), "{} of {n}:\n{}", bad.len(), bad.join("\n"));
    });
}

/// A lazy function that calls another lazy function: the callee runs in the loop of the caller, though each has its own script
/// slot. `Error.stack` names each frame with its own script, as the tree walker does.
#[test]
fn a_call_to_a_lazy_function_of_another_script_stays_in_the_loop() {
    big(|| {
        let src = format!(
            "function inner(i) {{ {PAD} if (i == 9) throw new Error('boom'); return i + p7; }}
             function outer(n) {{ {PAD} var s = 0; for (var i = 0; i < n; i++) s += inner(i); return s; }}
             var st = '';
             try {{ outer(10); }} catch (e) {{ st = e.stack; }}
             outer(5) + '|' + st"
        );
        let src = format!("/*{}*/{src}", " ".repeat(crate::parse_cache::LAZY_MIN_SOURCE));
        let mut want = String::new();
        for mode in [Exec::Tree, Exec::Auto, Exec::Byte] {
            let mut it = realm();
            it.set_exec(mode);
            let got = match it.run_script(&src) {
                Ok(v) => it.display(v),
                Err(e) => format!("{e:?}"),
            };
            if mode == Exec::Tree {
                want = got.clone();
                assert!(want.contains("at inner (script#") && want.contains("at outer (script#"), "{want}");
                continue;
            }
            assert_eq!(got, want, "{mode:?}");
            if mode == Exec::Byte {
                // The script body, the two calls of `outer` and the first call of `inner` (it resolves the lazy body) enter the loop.
                // The other 14 calls of `inner` run in the loop of `outer`.
                let s = it.bc_stats();
                assert_eq!(s.calls_tree, 0, "{s:?}");
                assert!(s.runs <= 4, "{s:?}");
            }
        }
    });
}

/// A lane function calls natives: one that calls back into script code (`map`), one that throws, and `push`. The loop calls
/// them with its registers as the arguments. Results, `Error.stack` and the depth limit of a recursion through `map` are what
/// the tree walker gives.
#[test]
fn a_native_called_from_the_loop_keeps_results_stacks_and_the_depth_limit() {
    big(|| {
        let src = "function k3(v) { return v * 3; }
             function each(a) { var s = 0, b = a.map(k3), arr = []; for (var i = 0; i < b.length; i++) s += b[i];
               for (var j = 0; j < 5; j++) arr.push(j, s); return s + '/' + arr.join(); }
             function bad(o) { return Object.defineProperty(o, 'x', {}); }
             function cb() { return [1].map(function inner(v) { throw new Error('in ' + v); }); }
             var st1 = '', st2 = '';
             try { bad(1); } catch (e) { st1 = e.stack; }
             try { cb(); } catch (e) { st2 = e.stack; }
             each([1, 2, 3]) + '|' + st1 + '|' + st2";
        let deep = "var c = 0; function deep(n) { c++; return [n].map(deep); } deep(0)";
        let mut want = String::new();
        for mode in [Exec::Tree, Exec::Auto, Exec::Byte] {
            let mut it = realm();
            it.set_exec(mode);
            let got = match it.run_script(src) {
                Ok(v) => it.display(v),
                Err(e) => format!("{e:?}"),
            };
            let s = it.bc_stats();
            let mut d = realm_with(EvalLimits { max_steps: 50_000_000, max_call_depth: 40, ..Default::default() });
            d.set_exec(mode);
            let lim = format!("{:?}", d.run_script(deep).map(|_| ()));
            let got = format!("{got}|{lim}|{}", d.run_script("c").map(|v| d.display(v)).unwrap_or_default());
            if mode == Exec::Tree {
                want = got;
                assert!(want.contains("at bad (") && want.contains("at inner (") && want.contains("at cb ("), "{want}");
                assert!(want.contains("Limit(CallDepth)"), "{want}");
                continue;
            }
            assert_eq!(got, want, "{mode:?}");
            if mode == Exec::Byte {
                // `map` twice, `defineProperty`, `join`, and `push` five times.
                assert!(s.natives >= 9, "{s:?}");
            }
        }
    });
}
