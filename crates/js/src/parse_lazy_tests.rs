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
        // an async function, an async arrow, a generator
        format!("async function f() {{ {p} }}"),
        format!("var f = async () => {{ {p} }};"),
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

// ---- arrow functions

/// An expression of 128 bytes or more.
fn long_expr() -> String {
    (1..=40).map(|i| i.to_string()).collect::<Vec<_>>().join(" + ")
}

/// Statements that need no `return`.
fn pad_stmts() -> String {
    pad().replace("return ", "")
}

#[test]
fn a_long_arrow_body_is_dropped_and_a_short_one_kept() {
    let (p, e) = (pad_stmts(), long_expr());
    let src = format!("var a = x => {{ {p} return x; }}; var b = (x, y) => x + y; var c = (x, y) => {{ {p} return x + y; }}; var d = () => {e};");
    let prog = lazy(&src);
    // a, c and d; not b.
    assert_eq!(prog.stats().lazies, 3, "{:?}", prog.stats());
    let eager = parse_script(&src, Limits::default()).unwrap();
    assert!(prog.stats().nodes() < eager.stats().nodes());
}

#[test]
fn an_arrow_that_takes_something_from_the_code_around_it_is_eager() {
    let p = pad_stmts();
    // The function around is called at once, so it is eager and the count is the one of the arrow.
    for src in [
        // the `arguments` of the function around, also from an arrow inside the arrow
        format!("(function () {{ return () => {{ {p} return arguments[0]; }}; }})();"),
        format!("(function () {{ return () => {{ {p} return () => arguments; }}; }})();"),
        format!("(function () {{ return () => {{ {p} return {{ arguments }}; }}; }})();"),
        format!("(function () {{ return () => {{ {p} return \\u0061rguments; }}; }})();"),
        // `new.target` and `super`
        format!("(function () {{ return () => {{ {p} return new.target; }}; }})();"),
        format!("var o = {{ m() {{ return () => {{ {p} return super.x; }}; }} }};"),
        // parameters that run in the code around
        format!("var f = (a = 1) => {{ {p} }};"),
        format!("var f = ({{ a }}) => {{ {p} }};"),
        format!("var f = (...a) => {{ {p} }};"),
        // an async arrow, an arrow in a class, an arrow that starts a call, an arrow in the head of a `for`
        format!("var f = async x => {{ {p} }};"),
        format!("class C {{ m() {{ return () => {{ {p} }}; }} }}"),
        format!("class C {{ f = () => {{ {p} }}; }}"),
        format!("(() => {{ {p} }})();"),
        format!("for (var f = x => {{ {p} }}; false;) ;"),
    ] {
        assert_eq!(lazy(&src).stats().lazies, 0, "{src}");
    }
    // `arguments` of a function inside the arrow is its own.
    let src = format!("var f = () => {{ {p} return function () {{ return arguments; }}; }};");
    assert_eq!(lazy(&src).stats().lazies, 1);
    // Without the fact the same arrow is lazy.
    let src = format!("(function () {{ return () => {{ {p} return 1; }}; }})();");
    assert_eq!(lazy(&src).stats().lazies, 1);
    // The arrow inside an eager arrow is lazy.
    let src = format!("var f = (g) => (h) => {{ {p} return g + h; }};");
    assert_eq!(lazy(&src).stats().lazies, 1);
}

#[test]
fn an_early_error_in_a_dropped_arrow_body_still_fails_the_parse() {
    for body in ["let x; let x;", "var a = ;", "return 1 +;", "break;", "let y = 1; var y = 2;", "x = {a = 1};", "a: a: ;", "yield: 1; await: 2; async: 3; let z; z = new.target;"] {
        let src = format!("var f = (u, v) => {{ {} {body} }};", pad_stmts());
        let Some(want) = parse_script(&src, Limits::default()).err() else { continue };
        assert_eq!(lazy_err(&src), want, "{body}");
    }
    // A duplicate parameter and a body that does not end.
    for src in [format!("var f = (a, a) => {{ {} }};", pad()), format!("var f = a => {{ {} ", pad())] {
        assert!(parse_script(&src, Limits::default()).is_err() && parse_script_lazy_used(&src, Limits::default()).0.is_err());
    }
}

#[test]
fn the_second_parse_of_an_arrow_gives_the_arrow_of_the_first() {
    let (p, e) = (pad_stmts(), long_expr());
    for src in [
        format!("var f = (a, b) => {{ 'use strict'; {p} return a + b; }};"),
        format!("var f = a => {{ {p} return a; }};"),
        format!("var f = (a, b) => {e};"),
        format!("x = (y, z) => {e}, 5;"),
        format!("var g = k => k ? (m => {e}) : null;"),
    ] {
        let prog = lazy(&src);
        assert_eq!(prog.stats().lazies, 1, "{src}");
        let eager = parse_script(&src, Limits::default()).unwrap();
        let (sub, fid, used) = prog.lazy_function(0, Limits::default()).unwrap();
        let at = prog.lazies()[0].start;
        let ef = eager.funcs.iter().find(|f| f.kind == FuncKind::Arrow && f.span.start == at).unwrap();
        assert!(used.is_some_and(|n| n > 0));
        let f = sub.func(fid);
        assert_eq!((f.span, f.strict, f.simple_params, f.kind, f.mode, f.name), (ef.span, ef.strict, ef.simple_params, ef.kind, ef.mode, ef.name), "{src}");
        assert_eq!(f.params.len(), ef.params.len());
        assert_eq!(sub.text(f.span), eager.text(ef.span));
        assert!(matches!((f.body, ef.body), (Body::Block(_), Body::Block(_)) | (Body::Expr(_), Body::Expr(_))), "{src}");
        let (again, _, used2) = prog.lazy_function(0, Limits::default()).unwrap();
        assert!(Rc::ptr_eq(&sub, &again) && used2.is_none());
    }
}

#[test]
fn an_arrow_inside_a_dropped_arrow_is_lazy_again() {
    let p = pad_stmts();
    let src = format!("var f = a => {{ {p} var g = b => {{ {p} {p} }}; return g; }};");
    let prog = lazy(&src);
    assert_eq!(prog.stats().lazies, 1);
    let (sub, _, _) = prog.lazy_function(0, Limits::default()).unwrap();
    assert_eq!(sub.stats().lazies, 1);
}

#[test]
fn an_arrow_that_starts_a_call_is_eager_with_or_without_parentheses() {
    let p = pad_stmts();
    for src in [format!("var r = (() => {{ {p} return 1; }})();"), format!("var r = (x => {{ {p} return x; }})(1);"), format!("!(() => {{ {p} }})();")] {
        assert_eq!(lazy(&src).stats().lazies, 0, "{src}");
    }
}
