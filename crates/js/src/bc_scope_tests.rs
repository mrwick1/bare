//! The scope data of the code generator against the tree walker (risk R7 of `docs/engine-plan.md`). Each corpus case runs on the
//! tree walker twice: as it is, and with every function in an environment and every site resolved at its first run. Then every
//! identifier site is compared (`bc_scope_check`).

use super::*;
use crate::heap::Heap;
use crate::parse::{parse_script, Limits};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(256 << 20).spawn(f).unwrap().join().unwrap()
}

/// Make every function keep its environment (no frame slots) and every site resolve at its first run, so the walker learns the
/// index of each binding that the run reads.
fn force_env(it: &mut Interp) {
    for s in &mut it.scopes {
        s.fr = FR_ENV;
    }
    for s in &mut it.slots.sites {
        if *s & UNRES != 0 {
            *s |= SEEN;
        }
    }
}

fn add(a: &mut ScopeCheck, b: ScopeCheck) {
    a.sites += b.sites;
    a.indexed += b.indexed;
    a.global_or_dynamic += b.global_or_dynamic;
    a.hops_only += b.hops_only;
    a.unresolved += b.unresolved;
    a.frame += b.frame;
    a.static_checked += b.static_checked;
    a.mismatches.extend(b.mismatches);
}

/// Run one source on the tree walker (with `force`, see `force_env`) and check its sites. `None` if it does not run here.
fn check(src: &str, force: bool) -> Option<ScopeCheck> {
    let prog = parse_script(src, Limits::default()).ok()?;
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 300_000, ..Default::default() }).ok()?;
    if force {
        force_env(&mut it);
    }
    let _ = it.run();
    let _ = it.drain_microtasks();
    Some(it.bc_scope_check())
}

fn corpus(text: &str, total: &mut ScopeCheck) {
    let mut cases: Vec<(String, Vec<&str>)> = Vec::new();
    for line in text.lines() {
        if let Some(n) = line.strip_prefix("=== ") {
            cases.push((n.trim().to_string(), Vec::new()));
        } else if let Some(c) = cases.last_mut() {
            c.1.push(line);
        }
    }
    for (name, lines) in cases {
        let src = lines.join("\n");
        for force in [false, true] {
            if let Some(mut r) = check(&src, force) {
                for m in &mut r.mismatches {
                    *m = format!("{name} (force {force}): {m}");
                }
                add(total, r);
            }
        }
    }
}

#[test]
fn every_site_of_the_corpora_matches_the_walker() {
    let r = big(|| {
        let mut total = ScopeCheck::default();
        for text in [
            include_str!("../../../tools/fixtures/js/eval-cases.txt"),
            include_str!("../../../tools/fixtures/js/statements-cases.txt"),
            include_str!("../../../tools/fixtures/js/class-cases.txt"),
            include_str!("../../../tools/fixtures/js/subclass-cases.txt"),
            include_str!("../../../tools/fixtures/js/generator-cases.txt"),
            include_str!("../../../tools/fixtures/js/iterator-cases.txt"),
            include_str!("../../../tools/fixtures/js/async-cases.txt"),
            include_str!("../../../tools/fixtures/js/tagged-cases.txt"),
            include_str!("../../../tools/fixtures/js/arrobj-cases.txt"),
        ] {
            corpus(text, &mut total);
        }
        total
    });
    eprintln!("{r:?}");
    assert!(r.mismatches.is_empty(), "{} mismatches:\n{}", r.mismatches.len(), r.mismatches.join("\n"));
    assert!(r.indexed > 3000 && r.static_checked > 20000 && r.frame > 1000, "too few sites checked: {r:?}");
}

/// The cases that the corpora may not hold, each in a function that runs twice (resolved) and with `force_env`.
#[test]
fn layout_corner_cases_match_the_walker() {
    let srcs = [
        // A parameter, a var and a function declaration with the same name keep the first place.
        "function f(a, b) { var a; var c = a + b; function a() {} return [a, b, c] } f(1, 2); f(3, 4)",
        // A named function expression: its own name first, then a parameter that shadows it.
        "var g = function h(h) { return h } ; g(1); g(2); var k = function m(x) { var m; return [m, x] }; k(1); k(2)",
        // `arguments`, rest and destructured parameters with defaults that read earlier parameters.
        "function f(a, {b, c: [d = a] = []} = {}, ...r) { var e = arguments.length; return [a, b, d, r, e] } f(1); f(1, {b: 2, c: [3]}, 4, 5)",
        // Blocks, switch, loop heads and catch, with closures that keep them.
        "function f(n) { var out = []; for (let i = 0; i < n; i++) { let j = i * 2; const k = () => i + j; out.push(k) } \
         switch (n) { case 1: let a = 1; out.push(() => a); break; default: const b = 2; class C {} out.push(() => b + C.name) } \
         try { throw n } catch ({x = n, y}) { out.push(() => x + y) } \
         for (const [p, q] of [[1, 2]]) out.push(() => p + q); for (let key in {z: 1}) out.push(() => key); \
         { function inner() { return n } out.push(inner) } return out.map(f => f()) } f(1); f(2)",
        // Classes: a site inside a method resolves in the method, one that leaves the class is dynamic.
        "let outer = 1; class A { constructor(x) { this.x = x + outer } m(y) { let z = y; return () => z + this.x + new.target } } \
         class B extends A { constructor(q) { super(q); var w = q; this.w = w } } new B(1); new B(2); new A(1).m(2)(); new A(1).m(3)()",
        // Generators and async functions.
        "function* g(a) { let b = a; yield b; for (const c of [1]) yield c + a } [...g(1)]; [...g(2)]; \
         async function h(a) { let b = await a; return b + a } h(1); h(2)",
        // A top-level `let` and `class` read from functions (environment 0).
        "let top = 1; const two = 2; class K {} function r() { return top + two + K.name } r(); r()",
    ];
    big(move || {
        for src in srcs {
            for force in [false, true] {
                let r = check(src, force).unwrap_or_else(|| panic!("does not run: {src}"));
                assert!(r.mismatches.is_empty(), "{src}\n{}", r.mismatches.join("\n"));
                assert!(r.indexed > 0 || !force, "nothing resolved: {src}");
            }
        }
    });
}

/// Names of the bindings of the scope of the first function named `name`.
fn binds_of(it: &mut Interp, name: &str) -> Vec<(String, u8, BindKind)> {
    let n = it.intern_name(name);
    let fid = (0..it.prog.stats().funcs).map(FuncId::from_index).find(|f| it.func_name[f.index()] == n).unwrap();
    let fs = it.bc_fn_scope(fid).unwrap();
    let mut b = Vec::new();
    it.bc_binds(fs.scope, &mut b);
    b.into_iter().map(|b| (it.names[b.name as usize].clone(), b.flags, b.kind)).collect()
}

#[test]
fn function_layout_and_flags() {
    big(|| {
        let src = "var o = { f: function f(f, a) { var a, v; let l; const c = 1; function d() {} class K {} return arguments } }; \
                   function g() { return () => this } class A { constructor() { new.target } } class B extends A { constructor() { super() } }";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let fid_of = |it: &mut Interp, name: &str| {
            let n = it.intern_name(name);
            (0..it.prog.stats().funcs).map(FuncId::from_index).find(|f| it.func_name[f.index()] == n).unwrap()
        };
        let b = binds_of(&mut it, "f");
        let got: Vec<(&str, u8, BindKind)> = b.iter().map(|(n, f, k)| (n.as_str(), *f, *k)).collect();
        let d = fid_of(&mut it, "d");
        assert_eq!(
            got,
            [
                ("f", INIT | MUT, BindKind::Param),
                ("a", INIT | MUT, BindKind::Param),
                ("arguments", INIT | MUT, BindKind::Arguments),
                ("v", INIT | MUT, BindKind::Var),
                ("l", MUT, BindKind::Let),
                ("c", 0, BindKind::Const),
                ("d", INIT | MUT, BindKind::Func(d)),
                ("K", MUT, BindKind::Class),
            ]
        );
        let f = fid_of(&mut it, "f");
        let fl = it.bc_fn_scope(f).unwrap().flags;
        assert_eq!(fl & (BF_ARGS | BF_SELF_NAME | BF_INNER | BF_IN_CLASS), BF_ARGS | BF_SELF_NAME | BF_INNER);
        let g = fid_of(&mut it, "g");
        assert_eq!(it.bc_fn_scope(g).unwrap().flags & (BF_THIS | BF_INNER), BF_INNER, "the arrow names `this`, not g");
        // The constructors of A and B: `%nt` first, and `%this` and `%fn` before it in a derived constructor.
        let ctors: Vec<FuncId> = (0..it.prog.stats().funcs).map(FuncId::from_index).filter(|&f| matches!(it.prog.func(f).kind, FuncKind::Constructor | FuncKind::DerivedConstructor)).collect();
        let mut names = Vec::new();
        for c in ctors {
            let fs = it.bc_fn_scope(c).unwrap();
            assert_ne!(fs.flags & BF_IN_CLASS, 0);
            let mut b = Vec::new();
            it.bc_binds(fs.scope, &mut b);
            names.push(b.iter().map(|b| it.names[b.name as usize].clone()).collect::<Vec<_>>().join(","));
        }
        assert_eq!(names, ["%nt", "%this,%fn,%nt"]);
    });
}

#[test]
fn the_walk_numbers_scopes_as_the_first_walk() {
    big(|| {
        let src = "for (let i of [1]) { let a; (function () { { let b; try {} catch (e) { let c } } })() } \
                   switch (1) { case 1: let s; } class Q { m() { for (const x in {}) {} } }";
        let prog = parse_script(src, Limits::default()).unwrap();
        let it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let root = it.bc_root().unwrap();
        let w = it.bc_walk(None, root);
        assert_eq!(w.bad, None);
        assert_eq!(w.stmts.len(), 3, "for-of head, its block, the switch");
        assert_eq!(w.funcs.len(), 2, "the function expression and the method");
        for (f, s) in w.funcs.clone() {
            let inner = it.bc_walk(Some(f), s);
            assert_eq!(inner.bad, None);
            assert_eq!(inner.stmts.len(), if it.prog.func(f).kind == FuncKind::Method { 1 } else { 3 });
        }
        assert_eq!(it.bc_scope_end(root) as usize, it.sc_all.len());
    });
}

#[test]
fn script_names_in_environment_zero_follow_earlier_scripts() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        it.run_script("let a = 1; let b = 2;").unwrap();
        it.run_script("let c = 3; function f() { return c + a } f(); f();").unwrap();
        let r = it.bc_scope_check();
        assert!(r.mismatches.is_empty(), "{}", r.mismatches.join("\n"));
        // `c` is the third binding of environment 0, `a` (a name of the first script) is global for the second script.
        let n = it.intern_name("c");
        it.select(2);
        let root = it.bc_root().unwrap();
        assert_eq!(it.bc_resolve(n, root), Site::Env { hops: 0, index: 2 });
        let a = it.intern_name("a");
        assert_eq!(it.bc_resolve(a, root), Site::Global);
    });
}
