//! Class tests. `tools/fixtures/js/class-cases.txt` is checked against Node (`tools/js/class_check.sh`).
//! The tests here cover what Node cannot show: Unsupported before execution, limits, roots and GC.

use super::*;
use crate::heap::{Heap, HeapLimits};
use crate::parse::{parse_script, Limits};

/// Run on a big native stack (a JS call needs a few KiB; a debug build needs several times more).
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
        Value::Symbol(_) => "symbol".into(),
        Value::BigInt(_) | Value::BigHeap(_) => format!("bigint:{}", it.display(v)),
        Value::Object(_) => it.display(v),
    }
}

fn outcome(it: &mut Interp, r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => show(it, v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", esc(&it.get_str(v, "name")), esc(&it.get_str(v, "message"))),
        Err(Error::Throw(v)) => format!("throw-value:{}", show(it, v)),
        Err(Error::Limit(_)) => "limit".into(),
        Err(e) => format!("other:{e:?}"),
    }
}

fn run_case(src: &str) -> String {
    let prog = parse_script(src, Limits::default()).unwrap_or_else(|e| panic!("{src:?}: {e}"));
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 500_000, ..Default::default() }).expect("setup");
    let r = it.run();
    outcome(&mut it, r)
}

/// Build an interpreter for `src`, run it, and hand it to `f`.
fn with_it(src: &str, limits: EvalLimits, f: impl FnOnce(&mut Interp) + Send + 'static) {
    let src = src.to_string();
    big(move || {
        let prog = parse_script(&src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), limits).unwrap();
        it.run().unwrap();
        f(&mut it);
    });
}

fn global_get(it: &mut Interp, name: &str) -> Value {
    let k = it.heap.key_from_str(name).unwrap();
    match it.heap.get(it.global_object(), k).unwrap() {
        Get::Value(v) => v,
        _ => panic!("accessor"),
    }
}

/// Call the global function `name` with no arguments and show the outcome.
fn probe(it: &mut Interp, name: &str) -> String {
    let f = global_get(it, name);
    let r = it.call(f, Value::Undefined, &[]);
    outcome(it, r)
}

fn limit_of(src: &str, limits: EvalLimits, heap: HeapLimits) -> LimitKind {
    let src = src.to_string();
    big(move || {
        let prog = parse_script(&src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::new(heap).unwrap(), limits).unwrap();
        match it.run() {
            Err(Error::Limit(k)) => k,
            other => panic!("{src}: {:?}", other.map(|v| it.display(v))),
        }
    })
}

#[test]
fn corpus_matches_node() {
    check_corpus(include_str!("../../../tools/fixtures/js/class-cases.txt"), include_str!("../../../tools/fixtures/js/class-cases.expected"));
}

/// `class X extends <builtin>` (`tools/js/subclass_check.sh`).
#[test]
fn subclass_corpus_matches_node() {
    check_corpus(include_str!("../../../tools/fixtures/js/subclass-cases.txt"), include_str!("../../../tools/fixtures/js/subclass-cases.expected"));
}

fn check_corpus(cases: &'static str, expected: &'static str) {
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
            let got = format!("{name}\t{}", run_case(lines.join("\n").trim_end_matches('\n')));
            if got != w {
                bad.push(format!("< {w}\n> {got}"));
            }
        }
        assert!(bad.is_empty(), "{} of {} class cases differ from Node:\n{}", bad.len(), list.len(), bad.join("\n"));
    });
}

/// Parse-only inventory of the class syntax in a directory of scripts (`CLASS_INVENTORY_DIR=dir cargo test
/// -p bare-js class_inventory -- --ignored --nocapture`). Nothing runs.
#[test]
#[ignore]
fn class_inventory() {
    let Ok(dir) = std::env::var("CLASS_INVENTORY_DIR") else { return };
    let mut files: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "js")).collect();
    files.sort();
    for path in files {
        let src = std::fs::read_to_string(&path).unwrap();
        let Ok(prog) = parse_script(&src, Limits::default()) else { println!("{}: parse error", path.display()); continue };
        let mut c: std::collections::BTreeMap<String, usize> = Default::default();
        let mut bump = |k: String| *c.entry(k).or_default() += 1;
        for cl in prog.classes.iter() {
            bump("class".into());
            if cl.superclass.is_some() { bump("class.extends".into()); }
            if cl.name.is_none() { bump("class.anonymous".into()); }
            for e in prog.list(cl.elems) {
                let e = prog.elem(e);
                let kind = match e.kind {
                    ElemKind::Method(f) if prog.func(f).kind != FuncKind::Method => "ctor",
                    ElemKind::Method(_) => "method",
                    ElemKind::Getter(_) => "getter",
                    ElemKind::Setter(_) => "setter",
                    ElemKind::Field(Some(_)) => "field+init",
                    ElemKind::Field(None) => "field",
                };
                let key = match e.key { Key::Name(_) => "name", Key::Str(_) => "str", Key::Num(_) => "num", Key::Computed(_) => "computed", Key::Private(_) => "private", _ => "other" };
                bump(format!("elem.{}{}.{}", if e.is_static { "static." } else { "" }, kind, key));
            }
        }
        for e in prog.exprs.iter() {
            match e.kind {
                ExprKind::Call { callee, .. } if matches!(prog.expr(callee).kind, ExprKind::Super) => bump("super()".into()),
                ExprKind::Member { obj, .. } | ExprKind::Index { obj, .. } if matches!(prog.expr(obj).kind, ExprKind::Super) => bump("super.x".into()),
                ExprKind::PrivateMember { .. } => bump("#x member".into()),
                ExprKind::PrivateIn { .. } => bump("#x in".into()),
                ExprKind::NewTarget => bump("new.target".into()),
                ExprKind::Await(_) => bump("await".into()),
                _ => {}
            }
        }
        for f in prog.funcs.iter() {
            if f.is_async() { bump("async fn".into()); }
        }
        println!("{}: {c:?}", path.file_name().unwrap().to_string_lossy());
    }
}

fn unsupported_what(src: &str) -> String {
    let prog = parse_script(src, Limits::default()).unwrap_or_else(|e| panic!("{src}: {e}"));
    match Interp::new(&prog, Heap::default(), EvalLimits::default()) {
        Err(Error::Unsupported(u)) => {
            assert!(u.span.end > u.span.start, "{src}: span");
            u.what.to_string()
        }
        Err(e) => panic!("{src}: {e:?}"),
        Ok(_) => panic!("{src}: accepted"),
    }
}

#[test]
fn unsupported_class_shapes_are_found_before_anything_runs() {
    assert_eq!(unsupported_what("var o = { m() { return super.x; } }; class A { m() { return o; } }"), "super outside a class element");
    assert_eq!(unsupported_what("class A { m() { return { n() { return super.x; } }; } }"), "super outside a class element");
    assert_eq!(unsupported_what("class A { x = new.target; }"), "new.target in a class field initializer");
    assert_eq!(unsupported_what("class A { static x = () => new.target; }"), "new.target in a class field initializer");
    // Nothing ran: the script would have set a global first.
    let prog = parse_script("var ran = 1; class A { x = new.target; }", Limits::default()).unwrap();
    assert!(Interp::new(&prog, Heap::default(), EvalLimits::default()).is_err());
    // The parser rejects these: they never reach the evaluator.
    for src in ["class A { static { } }", "class A { async *g() {} }", "class A { static async *g() {} }"] {
        let e = parse_script(src, Limits::default()).unwrap_err();
        assert_eq!(e.kind, crate::parse::ErrorKind::Unsupported, "{src}");
    }
}

#[test]
fn class_constructor_calls_from_the_host_are_type_errors() {
    with_it("class A { constructor() { this.x = 1; } } var C = A; var bound = A.bind(null);", EvalLimits::default(), |it| {
        let c = global_get(it, "C");
        let Err(Error::Throw(e)) = it.call(c, Value::Undefined, &[]) else { panic!("class called without new") };
        assert_eq!(it.get_str(e, "message"), "Class constructor A cannot be invoked without 'new'");
        let b = global_get(it, "bound");
        assert!(matches!(it.call(b, Value::Undefined, &[]), Err(Error::Throw(_))));
    });
}

#[test]
fn recursion_through_constructors_hits_the_call_depth_limit() {
    let d = EvalLimits::default();
    assert_eq!(limit_of("class A { constructor() { new A(); } } new A();", d, HeapLimits::default()), LimitKind::CallDepth);
    assert_eq!(limit_of("class A { f = new A(); } new A();", d, HeapLimits::default()), LimitKind::CallDepth);
    assert_eq!(limit_of("class A { static s = new A(); constructor() { new A(); } } ", d, HeapLimits::default()), LimitKind::CallDepth);
    assert_eq!(limit_of("class A { m() { return this.m(); } } new A().m();", d, HeapLimits::default()), LimitKind::CallDepth);
    assert_eq!(limit_of("class A { get g() { return this.g; } } new A().g;", d, HeapLimits::default()), LimitKind::CallDepth);
    assert_eq!(limit_of("class A { #p = 1; get #g() { return this.#g; } m() { return this.#g; } } new A().m();", d, HeapLimits::default()), LimitKind::CallDepth);
    // A `super` call is a call: with the ceilings asked for, the call ceiling (10,000) ends it.
    assert_eq!(limit_of("class A { m() { return this.m(); } } class B extends A { m() { return super.m(); } } new B().m();", EvalLimits { max_call_depth: u32::MAX, max_statement_frames: u32::MAX, ..d }, HeapLimits::default()), LimitKind::CallDepth);
}

#[test]
fn a_deep_derived_chain_is_a_limit_not_a_native_stack_overflow() {
    // C0 <- C1 <- ... <- C300: `new C300()` needs 301 nested constructions. Default derived and explicit.
    let mut implicit = String::from("class C0 {}\n");
    let mut explicit = String::from("class C0 {}\n");
    for i in 1..=300 {
        implicit += &format!("class C{i} extends C{} {{}}\n", i - 1);
        explicit += &format!("class C{i} extends C{} {{ constructor() {{ super(); }} }}\n", i - 1);
    }
    let d = EvalLimits { max_call_depth: 256, max_statement_frames: 768, ..Default::default() };
    assert_eq!(limit_of(&(implicit.clone() + "new C300();"), d, HeapLimits::default()), LimitKind::CallDepth);
    // An explicit `super()` does not start a new call (`in_call`), so its frames add up: the 768-frame guard ends it before the call cap.
    assert_eq!(limit_of(&(explicit.clone() + "new C300();"), d, HeapLimits::default()), LimitKind::TraversalDepth);
    // 60 levels fit.
    let ok = |src: String| run_case(&src);
    let short: String = implicit.lines().take(61).collect::<Vec<_>>().join("\n");
    assert_eq!(ok(short + "\ntypeof new C60()"), "string:object");
    let short: String = explicit.lines().take(61).collect::<Vec<_>>().join("\n");
    assert_eq!(ok(short + "\nnew C60() instanceof C0"), "boolean:true");
}

#[test]
fn nested_classes_are_bounded_by_the_parser_and_the_walk() {
    big(|| {
        // The parser's depth bound (256) comes before the evaluator's 512 preflight frames, so a class nest that
        // parses is also walked. `descend` in `w_class` is the second guard.
        let nest = |n: usize| "class A { m() { return ".repeat(n) + "1" + &"; } }".repeat(n);
        let e = parse_script(&nest(200), Limits::default()).unwrap_err();
        assert!(matches!(e.kind, crate::parse::ErrorKind::Limit(_)), "{e}");
        assert_eq!(run_case(&(nest(60) + " 7")), "number:7");
        let heritage = "class A extends (".repeat(100) + "Object" + &") {}".repeat(100) + " 8";
        assert_eq!(run_case(&heritage), "number:8");
    });
}

#[test]
fn other_limits_are_distinct_for_classes() {
    let d = EvalLimits::default();
    let small = EvalLimits { max_steps: 100_000, ..d };
    assert_eq!(limit_of("class A { a = 1; b = 2; c = 3; } while (true) new A();", small, HeapLimits::default()), LimitKind::Steps);
    assert_eq!(limit_of("class A { a = (() => { while (true) {} })(); } new A();", small, HeapLimits::default()), LimitKind::Steps);
    assert_eq!(limit_of("class A { static a = (() => { for (;;) {} })(); }", small, HeapLimits::default()), LimitKind::Steps);
    assert_eq!(limit_of("class A { [(() => { for (;;) {} })()]() {} }", small, HeapLimits::default()), LimitKind::Steps);
    // The cap counts the intrinsic functions too: allow 100 more than a fresh realm holds.
    let intrinsic = big(|| Interp::realm(Heap::default(), EvalLimits::default()).unwrap().stats().functions);
    assert_eq!(limit_of("var k = []; for (var i = 0; i < 1000; i++) k.push(class { m() {} });", EvalLimits { max_functions: intrinsic + 100, ..d }, HeapLimits::default()), LimitKind::Functions);
    assert_eq!(limit_of("var k = []; for (var i = 0; i < 1000; i++) k.push(class { m() {} });", EvalLimits { max_envs: 50, ..d }, HeapLimits::default()), LimitKind::Envs);
    let intrinsic_objects = big(|| Interp::realm(Heap::default(), EvalLimits::default()).unwrap().heap.stats().live_objects);
    let heap = HeapLimits { max_objects: intrinsic_objects + 100, ..Default::default() };
    assert!(matches!(limit_of("class A { x = 1; } var k = []; for (;;) k.push(new A());", d, heap), LimitKind::Heap(Limit::Objects)));
}

#[test]
fn private_slots_are_bounded_with_their_own_limit() {
    big(|| {
        let prog = parse_script("class A { #a = 1; #b = 2; } var k = []; for (var i = 0; i < 100; i++) k.push(new A());", Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.set_private_slot_limit(10);
        assert!(matches!(it.run(), Err(Error::Limit(LimitKind::PrivateSlots))));
        assert!(it.private_slots() <= 10);
        // Private methods are slots too.
        let prog = parse_script("class A { #m() {} } var k = []; for (var i = 0; i < 100; i++) k.push(new A());", Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.set_private_slot_limit(5);
        assert!(matches!(it.run(), Err(Error::Limit(LimitKind::PrivateSlots))));
    });
}

#[test]
fn collect_keeps_private_values_closures_statics_and_class_scopes() {
    let src = "
        function mk() { var secret = 42; return class { f = secret; static s = { n: 5 }; static #p = { n: 6 }; #v = { n: 7 }; get #g() { return { n: 8 }; } static get() { return A.#p.n + this.s.n; } read() { return this.#v.n + this.#g.n; } }; }
        var A = mk(); var inst = new A();
        function probe1() { return A.get() + ',' + inst.read() + ',' + inst.f + ',' + new A().f; }
        class B { #m() { return 'm'; } static #sm() { return 'sm'; } call() { return this.#m() + B.#sm(); } }
        var b = new B();
        function probe2() { return b.call() + new B().call(); }";
    with_it(src, EvalLimits::default(), |it| {
        let before = probe(it, "probe1");
        assert_eq!(before, "string:11,15,42,42");
        for _ in 0..3 {
            it.collect(&[]).unwrap();
            assert_eq!(probe(it, "probe1"), before);
            assert_eq!(probe(it, "probe2"), "string:msmmsm");
        }
    });
}

#[test]
fn unreachable_instances_and_their_slots_are_freed_even_in_cycles() {
    let src = "
        class A { #self = this; #v = [1, 2]; static has(o) { return #v in o; } }
        var keep = new A();
        for (var i = 0; i < 500; i++) new A();
        0;
        function count() { var n = 0; return A.has(keep); }";
    with_it(src, EvalLimits::default(), |it| {
        assert_eq!(it.private_slots(), 1002);
        let objs = it.heap.stats().live_objects;
        let freed = it.collect(&[]).unwrap();
        assert!(freed.objects >= 1000, "{freed:?} of {objs}");
        // Only `keep` still has slots: #self and #v.
        assert_eq!(it.private_slots(), 2);
        assert_eq!(probe(it, "count"), "boolean:true");
        assert_eq!(it.live_classes(), 1);
    });
}

#[test]
fn a_dead_class_and_its_data_are_dropped_and_stale_handles_never_match() {
    let src = "
        var C = class { #x = 1; static has(o) { return #x in o; } static #s = 1; static f = 2; };
        var inst = new C();
        function ok() { return C.has(inst); }";
    with_it(src, EvalLimits::default(), |it| {
        assert_eq!(it.live_classes(), 1);
        assert_eq!(it.private_slots(), 2); // #x of inst, #s of the class
        let c = global_get(it, "C");
        let k = it.heap.key_from_str("C").unwrap();
        let g = it.global_object();
        it.heap.set(g, k, Value::Null).unwrap();
        let ki = it.heap.key_from_str("inst").unwrap();
        it.heap.set(g, ki, Value::Null).unwrap();
        it.collect(&[]).unwrap();
        assert_eq!(it.live_classes(), 0);
        assert_eq!(it.private_slots(), 0);
        // `c` is stale now. The host must not use it: the heap says so.
        assert!(matches!(it.heap.get_prototype_of(match c { Value::Object(o) => o, _ => unreachable!() }), Err(HeapError::Stale)));
    });
}

#[test]
fn reused_object_slots_do_not_inherit_a_dead_instances_brand() {
    let src = "
        class A { #x = 1; static has(o) { return #x in o; } }
        var made = 0;
        function churn() { for (var i = 0; i < 200; i++) { new A(); } }
        function fresh() { var hits = 0; for (var i = 0; i < 400; i++) { if (A.has({})) hits++; } return hits; }";
    with_it(src, EvalLimits::default(), |it| {
        for _ in 0..3 {
            let _ = probe(it, "churn");
            it.collect(&[]).unwrap();
            assert_eq!(it.private_slots(), 0);
            // Plain objects reuse the freed slots. None may carry a brand.
            assert_eq!(probe(it, "fresh"), "number:0");
        }
    });
}

#[test]
fn roots_cover_class_state() {
    let src = "
        class A { #o = { tag: 1 }; static s = { tag: 2 }; ['k' + 1] = 3; static #p = { tag: 4 }; get #g() { return 5; } }
        var a = new A();";
    with_it(src, EvalLimits::default(), |it| {
        let roots = it.roots();
        // Collect with exactly the roots the interpreter lists: nothing live may be stale afterwards.
        it.heap.collect(&roots).unwrap();
        let k = it.heap.key_from_str("a").unwrap();
        let g = it.global_object();
        assert!(it.heap.get(g, k).is_ok());
        // Interp::collect gives the same answer.
        it.collect(&[]).unwrap();
        assert!(it.private_slots() >= 3);
    });
}

#[test]
fn derived_state_survives_collect_between_host_calls() {
    let src = "
        class A { constructor() { this.a = 1; } }
        class B extends A { #p = 2; constructor() { super(); this.b = this.#p; } sum() { return this.a + this.b + this.#p; } }
        var made = [];
        function build() { made.push(new B()); return made.length; }
        function total() { var t = 0; made.forEach(function(m) { t += m.sum(); }); return t; }";
    with_it(src, EvalLimits::default(), |it| {
        assert_eq!(probe(it, "build"), "number:1");
        it.collect(&[]).unwrap();
        assert_eq!(probe(it, "build"), "number:2");
        it.collect(&[]).unwrap();
        assert_eq!(probe(it, "total"), "number:10");
    });
}

#[test]
fn pending_new_target_never_leaks_between_calls() {
    // `new.target` is passed through a pending slot. A failed call must not leave it for the next function.
    assert_eq!(
        run_case("function F() { return new.target; } function G() { return new.target; } try { new F(); } catch (e) {} var a = G(); typeof a"),
        "string:undefined"
    );
    assert_eq!(
        run_case("class A { constructor() { this.t = typeof new.target; } } function f() { return new.target; } new A(); typeof f()"),
        "string:undefined"
    );
    big(|| {
        let prog = parse_script("class A { constructor() { new.target; } } function f() { return new.target; } var r; try { new A(); } catch (e) { r = e.name; } typeof f()", Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_envs: 1, ..Default::default() }).unwrap();
        // The call environment cannot be made: the run is a limit, and a later call has no stale new.target.
        assert!(matches!(it.run(), Err(Error::Limit(LimitKind::Envs))));
    });
}
