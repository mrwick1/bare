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
