//! Tests of lazy function bodies: the first parse checks a body and drops its nodes, the first call parses the function again.

use super::*;

/// A body long enough to be dropped (`LAZY_MIN_BODY`).
fn pad() -> String {
    "var a = 1; var b = 2; var c = a + b; var d = c * 2; var e = d - a; var f = e + b; var g = f * c; return g + a + b + c + d + e;".to_string()
}

fn lazy(src: &str) -> Program<'static> {
    let (r, _) = parse_script_lazy_used(src, Limits::default());
    r.unwrap_or_else(|e| panic!("{src:?} failed: {e}")).into_owned()
}

fn lazy_err(src: &str) -> Error {
    match parse_script_lazy_used(src, Limits::default()).0 {
        Ok(_) => panic!("{src:?} was accepted"),
        Err(e) => e,
    }
}

#[test]
fn a_long_function_body_is_dropped_and_a_short_one_kept() {
    let long = format!("function f(x) {{ {} }} function g() {{ return 1 }}", pad());
    let p = lazy(&long);
    assert_eq!(p.stats().lazies, 1);
    let eager = parse_script(&long, Limits::default()).unwrap();
    assert!(p.stats().nodes() < eager.stats().nodes());
    let StmtKind::Function(f) = p.stmt(p.list(p.body).next().unwrap()).kind else { panic!() };
    assert!(matches!(p.func(f).body, Body::Lazy(0)));
    let StmtKind::Function(g) = p.stmt(p.list(p.body).nth(1).unwrap()).kind else { panic!() };
    assert!(matches!(p.func(g).body, Body::Block(_)));
}

#[test]
fn an_early_error_in_a_dropped_body_still_fails_the_parse() {
    for body in ["let x; let x;", "var a = ;", "return 1 +;", "break;", "let y = 1; var y = 2;", "if (1) function* g() {}", "a: a: ;", "x = {a = 1};"] {
        let src = format!("function f() {{ {} {body} }}", pad());
        let want = parse_script(&src, Limits::default()).err().unwrap_or_else(|| panic!("{body} is not an error"));
        assert_eq!(lazy_err(&src), want, "{body}");
    }
}

#[test]
fn the_second_parse_gives_the_function_of_the_first() {
    let src = format!("function f(a, b, c) {{ 'use strict'; {} }}", pad());
    let p = lazy(&src);
    let eager = parse_script(&src, Limits::default()).unwrap();
    let StmtKind::Function(ef) = eager.stmt(eager.list(eager.body).next().unwrap()).kind else { panic!() };
    let (sub, fid, used) = p.lazy_function(0, Limits::default()).unwrap();
    assert!(used.is_some_and(|n| n > 0));
    let f = sub.func(fid);
    let e = eager.func(ef);
    assert_eq!((f.span, f.strict, f.simple_params, f.kind, f.mode, f.name), (e.span, e.strict, e.simple_params, e.kind, e.mode, e.name));
    assert_eq!(f.params.len(), e.params.len());
    let (Body::Block(fb), Body::Block(eb)) = (f.body, e.body) else { panic!() };
    assert_eq!(fb.len(), eb.len());
    // The whole function is in the tree, the text of a span is the same text, and a later call gives the same tree.
    assert_eq!(sub.text(f.span), eager.text(e.span));
    let (again, _, used2) = p.lazy_function(0, Limits::default()).unwrap();
    assert!(Rc::ptr_eq(&sub, &again) && used2.is_none());
}

#[test]
fn a_function_in_a_dropped_body_is_lazy_again() {
    let inner = format!("function g() {{ {} }}", pad());
    let src = format!("function f() {{ {inner} {} }}", pad());
    let p = lazy(&src);
    assert_eq!(p.stats().lazies, 1);
    let (sub, _, _) = p.lazy_function(0, Limits::default()).unwrap();
    assert_eq!(sub.stats().lazies, 1);
    let (sub2, fid, _) = sub.lazy_function(0, Limits::default()).unwrap();
    assert_eq!(sub2.func(fid).name.map(|n| sub2.text(n)), Some("g"));
}

#[test]
fn strictness_comes_from_the_code_around_and_from_the_directive() {
    let src = format!("'use strict'; function f() {{ {} }} ", pad());
    let (sub, fid, _) = lazy(&src).lazy_function(0, Limits::default()).unwrap();
    assert!(sub.func(fid).strict);
    // Sloppy code around: a legacy octal literal is fine in the function, as the first parse found.
    let src = format!("function f() {{ var o = 010; {} }}", pad());
    let (sub, fid, _) = lazy(&src).lazy_function(0, Limits::default()).unwrap();
    assert!(!sub.func(fid).strict);
}

#[test]
fn these_functions_are_never_lazy() {
    let p = pad();
    for src in [
        // an arrow, an async function, a generator
        format!("var f = () => {{ {p} }};"),
        format!("async function f() {{ {p} }}"),
        format!("function* f() {{ {p} }}"),
        // the first walk gives a class member its context
        format!("class C {{ m() {{ {p} }} }}"),
        format!("class C {{ constructor() {{ {p} }} }}"),
        // `super` and `new.target`
        format!("var o = {{ m() {{ {p} return super.x; }} }};"),
        format!("function f() {{ {p} return new.target; }}"),
        // a function that starts after `(` or a unary operator is probably called at once
        format!("(function () {{ {p} }})();"),
        format!("!function () {{ {p} }}();"),
    ] {
        let prog = lazy(&src);
        assert_eq!(prog.stats().lazies, 0, "{src}");
    }
    // A method of an object literal and a getter are.
    let src = format!("var o = {{ m() {{ {p} }}, get x() {{ {p} }} }};");
    assert_eq!(lazy(&src).stats().lazies, 2);
}

#[test]
fn the_node_count_of_the_first_parse_still_counts_a_dropped_body() {
    let src = format!("function f() {{ {} }}", pad());
    let (_, lazy_used) = parse_script_lazy_used(&src, Limits::default());
    let (_, eager_used) = parse_script_used(&src, Limits::default());
    assert_eq!(lazy_used, eager_used);
    let tight = Limits { max_nodes: eager_used - 1, ..Limits::default() };
    assert!(matches!(parse_script_lazy_used(&src, tight).0, Err(Error { kind: ErrorKind::Limit(LimitKind::Nodes), .. })));
}

#[test]
fn a_module_function_is_lazy_too() {
    let src = format!("export function f() {{ {} }}", pad());
    let (r, _) = parse_module_lazy_used(&src, Limits::default());
    let p = r.unwrap().into_owned();
    assert_eq!(p.stats().lazies, 1);
    let (sub, fid, _) = p.lazy_function(0, Limits::default()).unwrap();
    assert!(sub.func(fid).strict);
    // A module parses `await` as a keyword in the function again, as the first time.
    let src = format!("export function f() {{ {} var await = 1; }}", pad());
    assert!(parse_module_lazy_used(&src, Limits::default()).0.is_err());
}

/// The nodes that the second parse of the function `idx` of `p` made, counted by the parser.
fn second_parse_nodes(p: &Program<'static>, idx: u32) -> u32 {
    p.lazy_function(idx, Limits::default()).unwrap().2.unwrap()
}

#[test]
fn the_second_parse_skips_the_lazy_functions_inside_the_function() {
    // f holds g, g holds h. The second parse of f and the second parse of g cost the same however long the bodies inside are.
    let nodes = |k: usize| {
        let src = format!("function f() {{ {} function g() {{ {} function h() {{ {} }} }} }}", pad(), pad().repeat(k), pad().repeat(k));
        let p = lazy(&src);
        let n_f = second_parse_nodes(&p, 0);
        let (tf, _, _) = p.lazy_function(0, Limits::default()).unwrap();
        let n_g = second_parse_nodes(&tf, 0);
        let (tg, _, _) = tf.lazy_function(0, Limits::default()).unwrap();
        let n_h = second_parse_nodes(&tg, 0);
        (n_f, n_g, n_h)
    };
    let (small, big) = (nodes(1), nodes(6));
    assert_eq!(small.0, big.0, "the second parse of f does not parse g again");
    // g parses its own body and not the body of h; h parses its own body.
    assert_eq!(big.1 - small.1, 5 * (nodes(2).1 - small.1));
    assert!(big.2 > small.2);
}

/// The tree of the function at each lazy stub of `p` (and, recursively, of the trees below it) has the functions and statements that the
/// full parse `eager` has for the same function.
fn same_as_eager(p: &Program<'static>, eager: &Program<'_>, depth: usize) -> usize {
    let mut seen = 0;
    for i in 0..p.stats().lazies as u32 {
        let (sub, fid, _) = p.lazy_function(i, Limits::default()).unwrap();
        let f = sub.func(fid);
        let ef = eager.funcs.iter().find(|e| e.span.start == f.span.start).unwrap_or_else(|| panic!("no function at {}", f.span.start));
        assert_eq!((f.span, f.strict, f.simple_params, f.kind, f.mode, f.name), (ef.span, ef.strict, ef.simple_params, ef.kind, ef.mode, ef.name));
        let (Body::Block(fb), Body::Block(eb)) = (f.body, ef.body) else { panic!("not a block") };
        let spans = |p: &Program<'_>, l| p.list(l).map(|s| p.stmt(s).span).collect::<Vec<_>>();
        assert_eq!(spans(&sub, fb), spans(eager, eb), "statements of the function at {}", f.span.start);
        seen += 1 + same_as_eager(&sub, eager, depth + 1);
    }
    seen
}

#[test]
fn a_skipped_function_leaves_the_parser_where_the_full_parse_leaves_it() {
    let p = pad();
    let g = format!("function g() {{ {p} }}");
    let h = format!("function h() {{ {p} }}");
    for body in [
        // a declaration, then a regular expression statement (not a division)
        format!("{g} /x/.test('x');"),
        // an expression, then a newline and a division
        format!("var a = function () {{ {p} }}\n/ 2 / 3;"),
        // no semicolon: the next line starts a statement
        format!("{g}\nfoo()"),
        format!("var o = {{ m() {{ {p} }}, get x() {{ {p} }}, set x(v) {{ {p} }}, k: 1 }}; o.k"),
        // a directive in the inner body makes it strict and not the code around it
        format!("function s() {{ 'use strict'; {p} }} var l = 010;"),
        format!("var t = function () {{ 'use strict'; {p} }}; with (o) {{}}"),
        // a function in a block, a label, a function in a function that is kept (it is short)
        format!("if (x) {{ {g} }} lbl: for (;;) {{ break lbl; }}"),
        format!("function k() {{ {h} }} {g}"),
        // two levels
        format!("function g1() {{ {p} function g2() {{ {p} function g3() {{ {p} }} }} }} /y/g;"),
        // a function that uses `super` is kept, its lazy function inside is not
        format!("var q = {{ m() {{ {p} return super.x; }}, n() {{ {p} {g} }} }};"),
    ] {
        let src = format!("function f() {{ {p} {body} }}");
        let eager = parse_script(&src, Limits::default()).unwrap_or_else(|e| panic!("{src}: {e}"));
        let lz = lazy(&src);
        assert!(same_as_eager(&lz, &eager, 0) >= 2, "{src}");
    }
}

#[test]
fn a_skipped_function_in_a_module_and_a_getter_load_with_their_own_tables() {
    let p = pad();
    let src = format!("export function f() {{ {p} var o = {{ get x() {{ {p} function z() {{ {p} }} }} }}; }}");
    let (r, _) = parse_module_lazy_used(&src, Limits::default());
    let prog = r.unwrap().into_owned();
    let (tf, _, _) = prog.lazy_function(0, Limits::default()).unwrap();
    assert_eq!(tf.stats().lazies, 1);
    let (tg, fid, _) = tf.lazy_function(0, Limits::default()).unwrap();
    assert_eq!(tg.func(fid).kind, FuncKind::Getter);
    assert!(tg.func(fid).strict);
    let (tz, zid, _) = tg.lazy_function(0, Limits::default()).unwrap();
    assert_eq!(tz.func(zid).name.map(|n| tz.text(n)), Some("z"));
}

#[test]
fn a_function_in_the_parameters_of_a_sibling_is_skipped_too() {
    // `p` is made before `g` (its start is after the start of `g`): the table of `f` must still find both.
    let nodes = |k: usize| {
        let src = format!(
            "function f() {{ {0} function a() {{ {0} }} function g(q = function p() {{ {1} }}) {{ {0} }} function b() {{ {1} }} function c() {{ {1} }} }}",
            pad(),
            pad().repeat(k)
        );
        second_parse_nodes(&lazy(&src), 0)
    };
    assert_eq!(nodes(1), nodes(6));
}
