//! Tests of the pre-parser: the facts of lazy functions, the captures, the skip ranges and the full parse of one function.

use super::*;
use crate::facts::{self, Facts};

fn lazy(src: &str) -> (Program<'_>, Facts) {
    let mut facts = Facts::new();
    let p = parse_script_lazy(src, Limits::default(), &mut facts).unwrap_or_else(|e| panic!("{src:?} failed: {e}"));
    (p, facts)
}

fn lazy_err(src: &str) -> Error {
    match parse_script_lazy(src, Limits::default(), &mut Facts::new()) {
        Ok(_) => panic!("{src:?} was accepted"),
        Err(e) => e,
    }
}

/// The distinct captured names of function `id`, sorted.
fn caps(src: &str, f: &Facts, id: u32) -> Vec<String> {
    let mut v: Vec<String> = f.captures(id).iter().map(|&s| name_in(src, s).into_owned()).collect();
    v.sort();
    v
}

/// The id of the function that starts at the first `needle` in `src`.
fn at(src: &str, f: &Facts, needle: &str) -> u32 {
    let i = src.find(needle).unwrap_or_else(|| panic!("no {needle:?}")) as u32;
    f.find(i).unwrap_or_else(|| panic!("no function at {needle:?} ({i}); starts: {:?}", f.fns.iter().map(|x| x.start).collect::<Vec<_>>()))
}

fn pad() -> String {
    "var a = 1; var b = 2; var c = a + b; var d = c * 2; var e = d - a; var f = e + b; var g = f * c; return g + a + b + c + d + e;".to_string()
}

#[test]
fn a_function_body_is_skipped_and_has_facts() {
    let src = "function f(x) { return x + y; } function g() { return 1 }";
    let (p, f) = lazy(src);
    assert_eq!((f.fns.len(), f.preparsed), (2, 2));
    assert_eq!(p.stats().skipped, 2);
    let StmtKind::Function(fid) = p.stmt(p.list(p.body).next().unwrap()).kind else { panic!() };
    let func = p.func(fid);
    assert!(matches!(func.body, Body::Skipped(0)));
    let e = f.get(0);
    assert_eq!(&src[e.start as usize..e.end as usize], "function f(x) { return x + y; }");
    assert_eq!((e.kind, e.simple, e.length, e.strict), (FuncKind::Normal, true, 1, false));
    assert_eq!(caps(src, &f, 0), ["y"]);
    assert_eq!(caps(src, &f, 1), Vec::<String>::new());
    // The tree of the top level has no node of a body.
    let eager = parse_script(src, Limits::default()).unwrap();
    assert!(p.stats().nodes() < eager.stats().nodes());
}

#[test]
fn an_early_error_in_a_skipped_body_still_fails_the_parse() {
    for body in ["let x; let x;", "var a = ;", "return 1 +;", "break;", "let y = 1; var y = 2;", "if (1) function* g() {}", "a: a: ;", "x = {a = 1};"] {
        let src = format!("function f() {{ {} {body} }}", pad());
        let want = parse_script(&src, Limits::default()).err().unwrap_or_else(|| panic!("{body} is not an error"));
        assert_eq!(lazy_err(&src), want, "{body}");
    }
}

#[test]
fn nested_closures_list_the_names_that_each_level_does_not_bind() {
    let src = "function a(p) { var q; function b(r) { let s; return function c() { return p + q + r + s + t; }; } return b; }";
    let (_, f) = lazy(src);
    let (a, b, c) = (at(src, &f, "function a"), at(src, &f, "function b"), at(src, &f, "function c"));
    assert_eq!(caps(src, &f, c), ["p", "q", "r", "s", "t"]);
    assert_eq!(caps(src, &f, b), ["p", "q", "t"]);
    assert_eq!(caps(src, &f, a), ["t"]);
    // Post-order: a function comes after the functions inside it.
    assert!(c < b && b < a);
    assert_eq!((f.get(a).sub, f.get(b).sub, f.get(c).sub), (0, 0, 0));
    assert_eq!(f.children(Some(a)).collect::<Vec<_>>(), [b]);
    assert_eq!(f.children(Some(b)).collect::<Vec<_>>(), [c]);
    assert_eq!(f.children(None).collect::<Vec<_>>(), [a]);
}

#[test]
fn a_name_is_bound_by_a_declaration_after_the_use() {
    let src = "function f() { function g() { return x + y + z + w; } var x; let y = 1; { var z; } function w() {} }";
    let (_, f) = lazy(src);
    // Hoisting: `var`, `let` (TDZ, still this scope's binding), a `var` in a block, a function declaration.
    assert_eq!(caps(src, &f, at(src, &f, "function g")), ["w", "x", "y", "z"]);
    assert!(caps(src, &f, at(src, &f, "function f")).is_empty());
}

#[test]
fn block_and_catch_and_for_scopes_bind_their_own_names() {
    let src = "function f() { { let a; (function g() { a; })(); } for (let i = 0;;) { () => i; } try {} catch (e) { () => e; } (() => a)(); }";
    let (_, f) = lazy(src);
    assert_eq!(caps(src, &f, at(src, &f, "function f")), ["a"]);
    assert_eq!(caps(src, &f, at(src, &f, "function g")), ["a"]);
}

#[test]
fn a_function_expression_binds_its_own_name_and_a_declaration_does_not() {
    let src = "function f() { var h = function k() { return k + m; }; function d() { return d; } }";
    let (_, f) = lazy(src);
    assert_eq!(caps(src, &f, at(src, &f, "function k")), ["m"]);
    assert_eq!(caps(src, &f, at(src, &f, "function d")), ["d"]);
    // A parameter or a body declaration of the same name shadows the self name: still no capture.
    let src = "var h = function k(k) { return k; };";
    assert!(caps(src, &lazy(src).1, 0).is_empty());
}

#[test]
fn arrows_take_the_names_of_their_head_and_body() {
    let src = "function f(o) { var g = (a, b = a + o, c = () => a + z) => a + b + c + w; var h = x => x + v; var i = async (q) => q + u; var j = async y => y; }";
    let (_, f) = lazy(src);
    let g = at(src, &f, "(a, b");
    assert_eq!(caps(src, &f, g), ["o", "w", "z"]);
    // The arrow in the head of an arrow is a function of the outer arrow; it uses `a` of its parameters and `z` of outside.
    let c = at(src, &f, "() => a + z");
    assert_eq!(caps(src, &f, c), ["a", "z"]);
    assert_eq!(f.children(Some(g)).collect::<Vec<_>>(), [c]);
    assert!(c < g);
    assert_eq!(caps(src, &f, at(src, &f, "x => x")), ["v"]);
    assert_eq!(caps(src, &f, at(src, &f, "async (q)")), ["u"]);
    assert!(caps(src, &f, at(src, &f, "async y")).is_empty());
    assert_eq!(caps(src, &f, at(src, &f, "function f")), ["u", "v", "w", "z"]);
    // Parameter names that look like uses of the head are not captures of the function around it.
    let src = "function f() { (p, q = p) => 1; }";
    assert!(caps(src, &lazy(src).1, at(src, &lazy(src).1, "function f")).is_empty());
}

#[test]
fn a_default_parameter_value_sees_the_parameters_but_not_the_body() {
    let src = "function f(a, b = () => a + c) { var c; return b; }";
    let (_, f) = lazy(src);
    // The default value is in the scope of the parameters. The `c` of the body is another variable: the arrow captures the outer `c`.
    assert_eq!(caps(src, &f, at(src, &f, "() => a")), ["a", "c"]);
    // Resolution by declared names only (as the old scope check does): the body `var c` does bind `c` for the function.
    assert_eq!(caps(src, &f, at(src, &f, "function f")), Vec::<String>::new());
}

#[test]
fn eval_and_with_set_flags_and_the_parent_sees_inner_eval() {
    let src = "function f() { function g() { eval('x'); } function h() { with (o) { (function i() { return v; }); } } function j() { (0, eval)('x'); eval?.('y'); obj.eval('z'); } }";
    let (_, f) = lazy(src);
    let (fl, g, h, i, j) = (at(src, &f, "function f"), at(src, &f, "function g"), at(src, &f, "function h"), at(src, &f, "function i"), at(src, &f, "function j"));
    assert_eq!(f.get(g).flags & facts::EVAL, facts::EVAL);
    assert_eq!(f.get(fl).flags & facts::INNER_EVAL, facts::INNER_EVAL);
    assert_eq!(f.get(fl).flags & facts::EVAL, 0);
    // An indirect eval, an optional call and a method named eval are not direct eval calls.
    assert_eq!(f.get(j).flags & (facts::EVAL | facts::INNER_EVAL), 0);
    assert_eq!(f.get(h).flags & facts::WITH, facts::WITH);
    assert_eq!(f.get(h).flags & facts::IN_WITH, 0);
    assert_eq!(f.get(i).flags & facts::IN_WITH, facts::IN_WITH);
    // A function that is only near the `with` is not in it.
    assert_eq!(f.get(fl).flags & (facts::WITH | facts::IN_WITH), 0);
    // `v` is still a capture of `i` (the object may or may not have it), and `o` a capture of `h`.
    assert_eq!(caps(src, &f, i), ["v"]);
    assert_eq!(caps(src, &f, h), ["o", "v"]);
}

#[test]
fn eval_in_an_arrow_makes_the_function_around_it_inner_eval() {
    let src = "function f() { return () => () => eval('1'); }";
    let (_, f) = lazy(src);
    assert_eq!(f.get(at(src, &f, "function f")).flags & facts::INNER_EVAL, facts::INNER_EVAL);
    assert_eq!(f.get(at(src, &f, "() => () =>")).flags & facts::INNER_EVAL, facts::INNER_EVAL);
    assert_eq!(f.get(at(src, &f, "() => eval")).flags & facts::EVAL, facts::EVAL);
}

#[test]
fn arguments_binds_in_a_function_and_passes_through_an_arrow() {
    let src = "function f() { return () => arguments[0]; } function g() { var arguments; return () => arguments; } function h(arguments) { return arguments; }";
    let (_, f) = lazy(src);
    let (fl, arrow, g, g_arrow, h) = (at(src, &f, "function f"), at(src, &f, "() => arguments[0]"), at(src, &f, "function g"), at(src, &f, "() => arguments;"), at(src, &f, "function h"));
    assert_eq!(caps(src, &f, arrow), ["arguments"]);
    assert_eq!(f.get(arrow).flags & facts::ARGS, facts::ARGS);
    // `f` binds `arguments` itself: not a capture of `f`, and the function uses it.
    assert!(caps(src, &f, fl).is_empty());
    assert_eq!(f.get(fl).flags & facts::ARGS, facts::ARGS);
    // A declaration of the name is the binding; a parameter too: then `arguments` is an ordinary variable.
    assert_eq!(caps(src, &f, g_arrow), ["arguments"]);
    assert_eq!(f.get(g).flags & facts::ARGS, 0);
    assert_eq!(f.get(h).flags & facts::ARGS, 0);
    // A top-level arrow: `arguments` is a capture of nobody.
    let src = "var a = () => arguments;";
    assert_eq!(caps(src, &lazy(src).1, 0), ["arguments"]);
}

#[test]
fn this_new_target_and_super_pass_through_arrows_only() {
    let src = "var o = { m() { return () => this.x + new.target + super.y; }, n() { return function () { return this; }; } };";
    let (_, f) = lazy(src);
    let (m, arrow, n, inner) = (at(src, &f, "() { return () =>"), at(src, &f, "() => this"), at(src, &f, "() { return function"), at(src, &f, "function () { return this"));
    let all = facts::THIS | facts::NEW_TARGET | facts::SUPER;
    assert_eq!(f.get(arrow).flags & all, all);
    assert_eq!(f.get(m).flags & all, all);
    assert_eq!(f.get(inner).flags & all, facts::THIS);
    assert_eq!(f.get(n).flags & all, 0);
}

#[test]
fn class_fields_and_static_blocks_are_functions_for_names() {
    let src = "function f() { var v; class C { a = v + w; static b = this; static { this.c = u; } [k] = 1; m() { return z; } } }";
    let (_, f) = lazy(src);
    let fl = at(src, &f, "function f");
    let a = f.find(src.find("= v + w").unwrap() as u32).unwrap();
    assert_eq!(f.get(a).flags & (facts::INLINE | facts::FIELD_INIT), facts::INLINE | facts::FIELD_INIT);
    assert_eq!(caps(src, &f, a), ["v", "w"]);
    let b = f.find(src.find("= this").unwrap() as u32).unwrap();
    assert_eq!(f.get(b).flags & facts::THIS, facts::THIS);
    assert!(caps(src, &f, b).is_empty());
    let sb = f.find(src.find("static {").unwrap() as u32).unwrap();
    assert_eq!(f.get(sb).flags & (facts::INLINE | facts::STATIC_BLOCK), facts::INLINE | facts::STATIC_BLOCK);
    assert_eq!(caps(src, &f, sb), ["u"]);
    // `this` of a field does not pass to the function around the class. The computed key is evaluated by the function around it.
    assert_eq!(f.get(fl).flags & facts::THIS, 0);
    let m = at(src, &f, "() { return z");
    assert_eq!(caps(src, &f, m), ["z"]);
    assert_eq!(caps(src, &f, fl), ["k", "u", "w", "z"]);
    // Class members are lazy: the method has a skip range.
    assert!(matches!(f.get(m).kind, FuncKind::Method) && f.get(m).flags & facts::INLINE == 0);
}

#[test]
fn a_class_expression_binds_its_name_in_its_body() {
    let src = "var K = class N { m() { return N + M; } };";
    let (_, f) = lazy(src);
    assert_eq!(caps(src, &f, at(src, &f, "() { return N")), ["M", "N"]);
    // The class itself is not a function: no entry holds N; the top level has no captures to report.
}

#[test]
fn labels_and_property_names_are_not_uses() {
    let src = "function f() { l: for (;;) { break l; } return { k: 1, m() {}, [c]: 2 }.k + o.p; }";
    let (_, f) = lazy(src);
    assert_eq!(caps(src, &f, at(src, &f, "function f")), ["c", "o"]);
}

#[test]
fn shorthand_properties_and_destructuring_use_names() {
    let src = "function f() { var o = { a, b }; [x] = [1]; ({ y } = o); var { z } = o; }";
    let (_, f) = lazy(src);
    assert_eq!(caps(src, &f, 0), ["a", "b", "x", "y"]);
}

#[test]
fn the_names_of_a_function_are_listed_once() {
    let src = "function f() { return q + q + q + (() => q + r)() + r; }";
    let (_, f) = lazy(src);
    assert_eq!(caps(src, &f, at(src, &f, "function f")).len(), 2);
    assert_eq!(f.captures(at(src, &f, "function f")).len(), 2);
}

#[test]
fn names_with_escapes_are_compared_by_value() {
    let src = r"function f() { return abc + abc; }";
    let (_, f) = lazy(src);
    assert_eq!(caps(src, &f, 0), ["abc"]);
}

#[test]
fn strictness_comes_from_the_code_around_and_from_the_directive() {
    let src = "'use strict'; function f() { } function g() { 'use strict'; }";
    let (_, f) = lazy(src);
    assert_eq!((f.get(0).strict, f.get(0).strict_out), (true, true));
    let src = "function f() { 'use strict'; } function g() { }";
    let (_, f) = lazy(src);
    assert_eq!((f.get(0).strict, f.get(0).strict_out), (false, true));
    assert_eq!((f.get(1).strict, f.get(1).strict_out), (false, false));
}

#[test]
fn the_second_parse_gives_the_function_of_the_first() {
    let src = format!("function f(a, b, c) {{ 'use strict'; {} }}", pad());
    let (_, mut facts) = lazy(&src);
    let eager = parse_script(&src, Limits::default()).unwrap();
    let StmtKind::Function(ef) = eager.stmt(eager.list(eager.body).next().unwrap()).kind else { panic!() };
    let (sub, fid) = parse_function(&src, &mut facts, 0, Limits::default()).unwrap();
    let (f, e) = (sub.func(fid), eager.func(ef));
    assert_eq!((f.span, f.strict, f.simple_params, f.kind, f.mode, f.name), (e.span, e.strict, e.simple_params, e.kind, e.mode, e.name));
    assert_eq!(f.params.len(), e.params.len());
    let (Body::Block(fb), Body::Block(eb)) = (f.body, e.body) else { panic!() };
    assert_eq!(fb.len(), eb.len());
    assert_eq!(sub.text(f.span), eager.text(e.span));
    assert_eq!(sub.stats().stmts + 1, eager.stats().stmts);
}

#[test]
fn a_full_parse_skips_the_functions_inside_by_their_ranges_and_pre_parses_nothing() {
    let src = "function f() { function g() { function h() { return 1; } return h; } var k = () => 2; return { m() { return 3; }, n: function () { return 4; } }; }";
    let (_, mut facts) = lazy(src);
    let n = facts.fns.len();
    assert_eq!(n, 6);
    assert_eq!(facts.preparsed, 6);
    let before = facts.preparsed;
    let f_id = at(src, &facts, "function f");
    let (sub, fid) = parse_function(src, &mut facts, f_id, Limits::default()).unwrap();
    // The tree of f has its own statements and four skipped children (g, k, m, n), not the body of h.
    assert_eq!(sub.stats().skipped, 4);
    let Body::Block(b) = sub.func(fid).body else { panic!() };
    assert_eq!(b.len(), 3);
    assert_eq!((facts.preparsed, facts.fns.len()), (before, n));
    // Each child is the function of its own entry: parse g, and g's `h` is skipped again.
    let g_id = at(src, &facts, "function g");
    let (gp, gfid) = parse_function(src, &mut facts, g_id, Limits::default()).unwrap();
    assert_eq!(gp.stats().skipped, 1);
    assert_eq!(gp.func(gfid).name.map(|s| gp.text(s)), Some("g"));
    assert_eq!((facts.preparsed, facts.fns.len()), (before, n));
}

#[test]
fn each_function_is_pre_parsed_at_most_once() {
    // Nested to depth 6, with arrows in heads and bodies, methods, accessors and a class: every function is read once, however deep.
    let src = "function a(x = () => 1) { return function b() { return (p) => { class K { m() { return { get g() { return () => function z() {}; } }; } } }; }; }";
    let (_, mut facts) = lazy(src);
    let n = facts.fns.iter().filter(|f| f.flags & facts::INLINE == 0).count();
    assert_eq!(facts.preparsed as usize, n);
    let mut starts: Vec<u32> = facts.fns.iter().map(|f| f.start).collect();
    starts.sort();
    starts.dedup();
    assert_eq!(starts.len(), facts.fns.len());
    // A full parse of each function, in any order, adds no pre-parse and no entry.
    let (c0, l0) = (facts.preparsed, facts.fns.len());
    for id in (0..facts.fns.len() as u32).rev() {
        if facts.get(id).flags & facts::INLINE != 0 {
            continue;
        }
        parse_function(src, &mut facts, id, Limits::default()).unwrap_or_else(|e| panic!("function {id} at {}: {e}", facts.get(id).start));
    }
    assert_eq!((facts.preparsed, facts.fns.len()), (c0, l0));
}

#[test]
fn a_full_parse_of_an_arrow_uses_its_start_and_context() {
    let src = "function f() { var o = { m() { return (a, b = () => a) => a + super.x + new.target + this; } }; }";
    let (_, mut facts) = lazy(src);
    let arrow = at(src, &facts, "(a, b");
    let (p, fid) = parse_function(src, &mut facts, arrow, Limits::default()).unwrap();
    let f = p.func(fid);
    assert_eq!((f.kind, f.params.len()), (FuncKind::Arrow, 2));
    assert!(matches!(f.body, Body::Expr(_)));
    // The default value `() => a` is in the head, skipped with its entry.
    assert_eq!(p.stats().skipped, 1);
    // The method: its `super` and `new.target` are fine (the context comes from its kind).
    let m = at(src, &facts, "() { return (a");
    parse_function(src, &mut facts, m, Limits::default()).unwrap();
}

#[test]
fn a_full_parse_of_a_method_with_a_private_name_does_not_need_the_class() {
    let src = "class C { #x = 1; m() { return this.#x; } static s(o) { return #x in o; } }";
    let (_, mut facts) = lazy(src);
    let m = at(src, &facts, "() { return this");
    parse_function(src, &mut facts, m, Limits::default()).unwrap();
    // A private name outside a class is still an error in the first parse.
    assert_eq!(lazy_err("function f() { return this.#x; }").kind, ErrorKind::Early);
}

#[test]
fn async_functions_generators_and_accessors_are_lazy_too() {
    let src = "async function a() { await 1; } function* g() { yield 1; } var o = { get x() { return 1; }, set x(v) {}, async *ag() { yield await 1; } };";
    let (_, mut facts) = lazy(src);
    assert_eq!(facts.fns.len(), 5);
    for (needle, mode) in [("async function a", FuncMode::Async), ("function* g", FuncMode::Generator), ("() { return 1", FuncMode::Plain)] {
        let id = at(src, &facts, needle);
        assert_eq!(facts.get(id).mode, mode, "{needle}");
        let (p, fid) = parse_function(src, &mut facts, id, Limits::default()).unwrap();
        assert_eq!(p.func(fid).mode, mode, "{needle}");
    }
}

#[test]
fn the_tree_of_a_skipped_body_is_dropped_after_each_statement() {
    // A body of many statements does not grow the arena: the largest statement is the peak.
    let mut body = String::new();
    for i in 0..2000 {
        body.push_str(&format!("var v{i} = [1, 2, 3, {i}];\n"));
    }
    let src = format!("function big() {{ {body} }}");
    let (p, f) = lazy(&src);
    assert!(p.stats().exprs < 10, "the arena kept {} expressions", p.stats().exprs);
    assert_eq!(f.fns.len(), 1);
    let eager = parse_script(&src, Limits::default()).unwrap();
    assert!(eager.stats().exprs > 8000);
}

#[test]
fn the_node_limit_counts_the_live_tree() {
    // Checked nodes are dropped, so a large checked body does not hit the limit that its tree would.
    let mut body = String::new();
    for i in 0..500 {
        body.push_str(&format!("var v{i} = [1, 2, 3, {i}];\n"));
    }
    let src = format!("function big() {{ {body} }}");
    let (_, eager_used) = parse_script_used(&src, Limits::default());
    let small = Limits { max_nodes: eager_used / 4, ..Limits::default() };
    assert!(parse_script(&src, small).is_err());
    assert!(parse_script_lazy(&src, small, &mut Facts::new()).is_ok());
}

#[test]
fn modules_have_lazy_functions_and_a_top_level_that_is_a_function() {
    let src = "import a from 'm'; export function f() { return a + b; } export const g = () => c;";
    let mut facts = Facts::new();
    let p = parse_module_lazy(src, Limits::default(), &mut facts).unwrap();
    assert!(p.module.is_some());
    assert_eq!(facts.fns.len(), 2);
    assert_eq!(caps(src, &facts, at(src, &facts, "function f")), ["a", "b"]);
    assert_eq!(caps(src, &facts, at(src, &facts, "() => c")), ["c"]);
    let f_id = at(src, &facts, "function f");
    let (_, fid) = parse_function(src, &mut facts, f_id, Limits::default()).unwrap();
    assert_eq!(fid.index(), 0);
}

#[test]
fn a_lazy_parse_finds_the_same_errors_as_a_full_parse_on_the_corpus() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/fixtures/js/");
    let cases = std::fs::read_to_string(format!("{dir}parse-cases.txt")).unwrap();
    let mut n = 0;
    for c in cases.split("\n%%\n") {
        let full = parse_script(c, Limits::default());
        let mut facts = Facts::new();
        let lz = parse_script_lazy(c, Limits::default(), &mut facts);
        assert_eq!(full.as_ref().err(), lz.as_ref().err(), "{c:?}");
        if lz.is_ok() {
            // Every function of the text can be parsed in full, and none is parsed twice before.
            let before = (facts.preparsed, facts.fns.len());
            for id in 0..facts.fns.len() as u32 {
                if facts.get(id).flags & facts::INLINE == 0 {
                    parse_function(c, &mut facts, id, Limits::default()).unwrap_or_else(|e| panic!("{c:?} function {id}: {e}"));
                }
            }
            assert_eq!((facts.preparsed, facts.fns.len()), before);
            n += 1;
        }
    }
    assert!(n > 100, "{n}");
}
