// Tests of `interp.rs` and `interp_rt.rs`. There is no compiler yet (S2.5): every program is written with `Asm`, then checked by
// `verify::verify`. The root maps are computed here by a liveness analysis (what a compiler computes), so a program that lets the
// interpreter collect inside a call is a test of the real contract: a register that is dead is not in the map.
use super::*;
use crate::heap::Trigger;
use crate::interp_rt::{FRESH, MEGA, MONO, POLY, SEEN};

mod prelude {
    pub use crate::atoms::SharedAtoms;
    pub use crate::bytecode::{Asm, Code, Handler, Ins, Label, Op, Recipes, decode};
    pub use crate::roots::RootMaps;
    pub use crate::string::{Chars, Str};
    pub use crate::verify::verify;
    pub use std::collections::BTreeSet;
    pub use std::sync::Arc;
}
#[path = "asm_util.rs"]
mod asm_util;
use asm_util::F;

const MIRI: bool = cfg!(miri);

fn n(i: i32) -> Value {
    Value::int(i)
}

fn vm_with(shared: &Arc<SharedAtoms>, codes: Vec<Code>) -> Vm {
    Vm::new(Program { codes }, shared.clone())
}

fn run_main(sh: &Arc<SharedAtoms>, codes: Vec<Code>) -> (Vm, Value) {
    let mut vm = vm_with(sh, codes);
    let v = vm.run_main(0).unwrap_or_else(|e| panic!("uncaught {e:?}"));
    (vm, v)
}

fn shared() -> Arc<SharedAtoms> {
    Arc::new(SharedAtoms::default())
}

fn text(vm: &Vm, v: Value) -> String {
    vm.realm.text(Realm::str_of(v).unwrap_or_else(|| panic!("{v:?} is not a string")))
}

fn texts(vm: &Vm, v: Value) -> Vec<String> {
    elements(v).into_iter().map(|e| text(vm, e)).collect()
}

fn prop(vm: &mut Vm, o: Value, name: &str) -> Value {
    let k = vm.realm.intern(name);
    vm.realm.get_prop(o, Key::Name(k)).expect("a property")
}

/// An object of the realm with the integer properties `props`, added in order (so the same list gives the same shape).
fn mk(vm: &mut Vm, props: &[(&str, i32)]) -> Value {
    let o = Obj::new(&mut vm.realm.heap, &vm.realm.shapes, vm.realm.s_object).expect("memory");
    for (k, v) in props {
        vm.realm.set_own(o, k, Value::int(*v));
    }
    o.value()
}

/// The text of the error that a program throws out of `run_main`.
fn run_err(sh: &Arc<SharedAtoms>, codes: Vec<Code>) -> String {
    let mut vm = vm_with(sh, codes);
    match vm.run_main(0) {
        Err(Exit::Throw(e)) => vm.realm.error_text(e),
        other => panic!("expected an uncaught error, got {other:?}"),
    }
}

fn elements(v: Value) -> Vec<Value> {
    let o = Realm::obj(v).expect("an array");
    (0..o.len()).map(|i| o.get_element(i).unwrap_or(Value::HOLE)).collect()
}

fn f64_of(v: Value) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("{v:?} is not a number"))
}

#[test]
fn arithmetic_runs_on_integers_and_doubles() {
    let sh = shared();
    let mut f = F::new(&sh, 12, 0);
    f.li(0, 7);
    f.li(1, 5);
    f.e(Op::Add, 2, 0, 1);
    f.e(Op::Sub, 3, 0, 1);
    f.e(Op::Mul, 4, 0, 1);
    f.e(Op::Div, 5, 0, 1);
    f.e(Op::Mod, 6, 0, 1);
    f.li(7, 2147483647);
    f.e(Op::AddI, 8, 7, 1);
    f.e(Op::Exp, 9, 0, 1);
    f.e(Op::Div, 10, 4, 1);
    f.e(Op::NewArrLit, 11, 2, 9);
    f.e(Op::Ret, 11, 0, 0);
    let (_vm, v) = run_main(&sh, vec![f.build()]);
    let v = elements(v);
    assert_eq!(&v[..3], &[n(12), n(2), n(35)]);
    assert_eq!(f64_of(v[3]), 1.4);
    assert_eq!(v[4], n(2));
    assert_eq!(v[5], n(2147483647));
    assert_eq!(f64_of(v[6]), 2147483648.0, "an integer overflow is a double");
    // Miri changes the result of `powf` by a few ulps on purpose.
    assert!((f64_of(v[7]) - 16807.0).abs() <= if MIRI { 1e-9 } else { 0.0 });
    assert_eq!(v[8], n(7), "a division with an integer result gives an integer");
}

#[test]
fn arithmetic_edge_cases_follow_javascript() {
    let sh = shared();
    let mut f = F::new(&sh, 20, 0);
    f.li(0, 0);
    f.li(1, -5);
    f.e(Op::Mul, 2, 0, 1); // 0 * -5 = -0
    f.e(Op::Mod, 3, 1, 1); // -5 % -5 = -0
    f.li(4, 7);
    f.e(Op::Mod, 5, 4, 0); // 7 % 0 = NaN
    f.e(Op::Div, 6, 4, 0); // 7 / 0 = Infinity
    f.e(Op::Neg, 7, 0, 0); // -0
    f.li(8, 1);
    f.e(Op::UShrI, 9, 1, 0); // -5 >>> 0 = 4294967291
    f.e(Op::BitNot, 10, 4, 0); // ~7 = -8
    f.e(Op::ShlI, 11, 8, 31); // 1 << 31 = -2147483648
    f.e(Op::NewArrLit, 12, 2, 10);
    f.e(Op::Ret, 12, 0, 0);
    let (_vm, v) = run_main(&sh, vec![f.build()]);
    let v = elements(v);
    assert!(f64_of(v[0]) == 0.0 && f64_of(v[0]).is_sign_negative());
    assert!(f64_of(v[1]) == 0.0 && f64_of(v[1]).is_sign_negative());
    assert_eq!(v[2], n(7));
    assert!(f64_of(v[3]).is_nan());
    assert_eq!(f64_of(v[4]), f64::INFINITY);
    assert!(f64_of(v[5]) == 0.0 && f64_of(v[5]).is_sign_negative());
    assert_eq!(f64_of(v[7]), 4294967291.0);
    assert_eq!(v[8], n(-8));
    assert_eq!(v[9], n(i32::MIN));
}

#[test]
fn compares_follow_javascript_for_numbers_strings_and_nan() {
    let sh = shared();
    let mut f = F::new(&sh, 20, 0);
    f.numbers.push(f64::NAN);
    let (a, b, one) = (f.atom("a"), f.atom("b"), f.atom("1"));
    f.li(0, 3);
    f.li(1, 4);
    f.e(Op::LoadN, 2, 0, 0);
    f.e(Op::LoadAtom, 3, a, 0);
    f.e(Op::LoadAtom, 4, b, 0);
    f.e(Op::LoadAtom, 5, one, 0);
    f.li(6, 1);
    f.e(Op::Lt, 10, 0, 1);
    f.e(Op::Ge, 11, 0, 1);
    f.e(Op::Lt, 12, 2, 0);
    f.e(Op::Ge, 13, 2, 0);
    f.e(Op::Lt, 14, 3, 4);
    f.e(Op::StrictEq, 15, 3, 3);
    f.e(Op::Eq, 16, 6, 5);
    let (s1, s2) = (f.label(), f.label());
    f.e(Op::LoadTrue, 17, 0, 0);
    f.j(Op::JnLt, 2, 0, s1); // NaN < 3 is false, so this jumps
    f.e(Op::LoadFalse, 17, 0, 0);
    f.bind(s1);
    f.e(Op::LoadFalse, 18, 0, 0);
    f.j(Op::JLt, 2, 0, s2); // and this does not
    f.e(Op::LoadTrue, 18, 0, 0);
    f.bind(s2);
    f.e(Op::NewArrLit, 19, 10, 9);
    f.e(Op::Ret, 19, 0, 0);
    let (_vm, v) = run_main(&sh, vec![f.build()]);
    let t = Value::TRUE;
    let fl = Value::FALSE;
    assert_eq!(elements(v), [t, fl, fl, fl, t, t, t, t, t]);
}

#[test]
fn a_loop_runs_to_its_end_and_has_one_loop_header() {
    let sh = shared();
    let mut f = F::new(&sh, 4, 0);
    let (head, done) = (f.label(), f.label());
    f.li(0, 0);
    f.li(1, 1);
    f.li(2, 100);
    f.bind(head);
    f.j(Op::JGt, 1, 2, done);
    f.e(Op::Add, 0, 0, 1);
    f.e(Op::AddI, 1, 1, 1);
    f.j(Op::JmpBack, 0, 0, head);
    f.bind(done);
    f.e(Op::Ret, 0, 0, 0);
    let code = f.build();
    assert_eq!(code.loops.len(), 1);
    assert!(code.loops[0].head < code.loops[0].end);
    let (_vm, v) = run_main(&sh, vec![code]);
    assert_eq!(v, n(5050));
}

#[test]
fn a_loop_stops_when_the_fuel_is_gone() {
    let sh = shared();
    let mut f = F::new(&sh, 1, 0);
    let head = f.label();
    f.li(0, 0);
    f.bind(head);
    f.e(Op::AddI, 0, 0, 1);
    f.j(Op::JmpBack, 0, 0, head);
    let mut vm = vm_with(&sh, vec![f.build()]);
    vm.fuel = 1000;
    assert_eq!(vm.run_main(0), Err(Exit::Fuel));
    assert_eq!(vm.fuel, 0);
}

#[test]
fn compare_jumps_with_an_immediate_and_a_constant_test_the_register() {
    let sh = shared();
    let mut f = F::new(&sh, 8, 0);
    let (hi, lo, a, b) = (f.label(), f.label(), f.label(), f.label());
    let s = f.atom("s");
    f.li(0, 5);
    f.e(Op::LoadAtom, 1, s, 0);
    f.li(2, 0);
    f.ji(Op::JGtI, 0, 4, hi);
    f.li(2, 1); // not run: 5 > 4
    f.bind(hi);
    f.ji(Op::JLtI, 0, 4, lo);
    f.e(Op::AddI, 2, 2, 10); // run: 5 < 4 is false
    f.bind(lo);
    f.ji(Op::JSEqA, 1, s as i32, a);
    f.e(Op::AddI, 2, 2, 100); // not run: r1 is "s"
    f.bind(a);
    f.e(Op::LoadTrue, 3, 0, 0);
    f.ji(Op::JSEqB, 3, 1, b);
    f.e(Op::AddI, 2, 2, 100); // not run
    f.bind(b);
    f.e(Op::Ret, 2, 0, 0);
    let (_vm, v) = run_main(&sh, vec![f.build()]);
    assert_eq!(v, n(10));
}

#[test]
fn a_wide_instruction_runs_like_a_narrow_one() {
    let sh = shared();
    let mut f = F::new(&sh, 300, 0);
    f.li(290, 100000); // the value needs the wide form
    f.e(Op::Mov, 291, 290, 0);
    f.e(Op::Add, 292, 290, 291); // all three operands need the wide form
    f.e(Op::Ret, 292, 0, 0);
    let code = f.build();
    assert!(code.disassemble().contains("Add.w"), "{}", code.disassemble());
    let (_vm, v) = run_main(&sh, vec![code]);
    assert_eq!(v, n(200000));
}

// 40,000 words: the liveness pass of `asm_util::roots` takes over 10 minutes of Miri time.
#[test]
#[cfg_attr(miri, ignore)]
fn a_long_jump_uses_the_wide_offset() {
    let sh = shared();
    let mut f = F::new(&sh, 4, 0);
    let end = f.label();
    f.e(Op::LoadTrue, 0, 0, 0);
    f.li(1, 1);
    f.j(Op::JmpIf, 0, 0, end);
    for _ in 0..40000 {
        f.e(Op::Nop, 0, 0, 0);
    }
    f.li(1, 2);
    f.bind(end);
    f.e(Op::Ret, 1, 0, 0);
    let code = f.build();
    assert!(code.disassemble().contains("JmpIf.w"));
    let (_vm, v) = run_main(&sh, vec![code]);
    assert_eq!(v, n(1));
}

// ---- calls ----

/// `fib(n)` by the global `fib`; the main function installs it and calls it with `n`.
fn fib_program(sh: &Arc<SharedAtoms>, n: i32) -> Vec<Code> {
    let mut m = F::new(sh, 4, 0);
    let site = m.site("fib");
    m.funcs = vec![1];
    m.e(Op::Closure, 0, 0, 0);
    m.e(Op::SetGlobal, 0, site, 0);
    m.e(Op::LoadUndef, 1, 0, 0);
    m.li(2, n);
    m.e(Op::Call, 3, 0, 1);
    m.e(Op::Ret, 3, 0, 0);
    let mut f = F::new(sh, 6, 1);
    let base = f.label();
    f.ji(Op::JLtI, 0, 2, base);
    for k in [1, 2] {
        let s = f.site("fib");
        f.e(Op::GetGlobal, 1, s, 0);
        f.e(Op::LoadUndef, 2, 0, 0);
        f.e(Op::SubI, 3, 0, k);
        f.e(Op::Call, if k == 1 { 4 } else { 5 }, 1, 1);
    }
    f.e(Op::Add, 4, 4, 5);
    f.e(Op::Ret, 4, 0, 0);
    f.bind(base);
    f.e(Op::Ret, 0, 0, 0);
    vec![m.build(), f.build()]
}

#[test]
fn a_recursive_function_calls_and_returns_with_frame_records() {
    let sh = shared();
    let (arg, want) = if MIRI { (10, 55) } else { (20, 6765) };
    let (vm, v) = run_main(&sh, fib_program(&sh, arg));
    assert_eq!(v, n(want));
    // The cache of the global read is a hit after the second use: one entry, and the state stays MONO.
    assert_eq!(vm.fbs[1].caches[0].state, MONO);
    assert_eq!(vm.fbs[1].entries(0), 1);
}

#[test]
fn a_call_gives_missing_arguments_undefined_and_drops_extra_ones() {
    let sh = shared();
    let mut m = F::new(&sh, 8, 0);
    m.funcs = vec![1];
    m.e(Op::Closure, 0, 0, 0);
    m.li(1, 5);
    m.li(2, 6);
    m.li(3, 7);
    m.e(Op::CallU, 5, 0, 1); // second parameter is missing
    m.e(Op::CallU, 6, 0, 3); // two extra arguments
    m.e(Op::NewArrLit, 7, 5, 2);
    m.e(Op::Ret, 7, 0, 0);
    let mut g = F::new(&sh, 2, 2);
    g.e(Op::Ret, 1, 0, 0);
    let (_vm, v) = run_main(&sh, vec![m.build(), g.build()]);
    assert_eq!(elements(v), [Value::UNDEFINED, n(6)]);
}

#[test]
fn call_passes_this_and_new_makes_an_object_from_the_prototype() {
    let sh = shared();
    let mut m = F::new(&sh, 12, 0);
    m.funcs = vec![1, 2];
    m.e(Op::Closure, 0, 0, 0); // returns `this`
    m.e(Op::NewObject, 1, 0, 0);
    m.li(2, 9);
    m.set(1, 2, "tag");
    m.e(Op::Call, 3, 0, 0); // this = r1
    m.get(4, 3, "tag");
    m.e(Op::Closure, 5, 1, 0); // a constructor: this.x = 5
    m.e(Op::New, 6, 5, 0);
    m.get(7, 6, "x");
    m.e(Op::LoadHole, 8, 0, 0);
    m.e(Op::NewArrLit, 9, 4, 4);
    m.e(Op::Ret, 9, 0, 0);
    let mut g = F::new(&sh, 1, 0);
    g.e(Op::LoadThis, 0, 0, 0);
    g.e(Op::Ret, 0, 0, 0);
    let mut c = F::new(&sh, 2, 0);
    c.e(Op::LoadThis, 0, 0, 0);
    c.li(1, 5);
    c.set(0, 1, "x");
    c.li(1, 99); // a primitive result of a constructor is ignored
    c.e(Op::Ret, 1, 0, 0);
    let (_vm, v) = run_main(&sh, vec![m.build(), g.build(), c.build()]);
    let v = elements(v);
    assert_eq!(v[0], n(9));
    assert_eq!(v[3], n(5));
}

#[test]
fn an_error_for_a_call_of_a_value_that_is_no_function() {
    let sh = shared();
    let mut f = F::new(&sh, 3, 0);
    f.li(0, 5);
    f.e(Op::LoadUndef, 1, 0, 0);
    f.e(Op::Call, 2, 0, 0);
    f.e(Op::Ret, 2, 0, 0);
    assert_eq!(run_err(&sh, vec![f.build()]), "TypeError: not a function");
}

#[test]
fn runaway_recursion_throws_a_range_error() {
    if MIRI {
        return;
    }
    let sh = shared();
    let mut m = F::new(&sh, 3, 0);
    let site = m.site("f");
    m.funcs = vec![1];
    m.e(Op::Closure, 0, 0, 0);
    m.e(Op::SetGlobal, 0, site, 0);
    m.e(Op::LoadUndef, 1, 0, 0);
    m.e(Op::Call, 2, 0, 0);
    m.e(Op::Ret, 2, 0, 0);
    let mut f = F::new(&sh, 3, 0);
    let s = f.site("f");
    f.e(Op::GetGlobal, 0, s, 0);
    f.e(Op::LoadUndef, 1, 0, 0);
    f.e(Op::Call, 2, 0, 0);
    f.e(Op::Ret, 2, 0, 0);
    assert_eq!(run_err(&sh, vec![m.build(), f.build()]), "RangeError: maximum call stack size exceeded");
}

// ---- closures and contexts ----

#[test]
fn a_closure_keeps_and_changes_a_context_slot() {
    let sh = shared();
    let mut m = F::new(&sh, 7, 0);
    m.funcs = vec![1];
    m.li(0, 10);
    m.e(Op::PushCtx, 0, 1, 0);
    m.e(Op::InitCtx, 0, 0, 0);
    m.e(Op::Closure, 1, 0, 0);
    m.e(Op::PopCtx, 0, 0, 0);
    m.e(Op::LoadUndef, 2, 0, 0);
    for r in [3, 4, 5] {
        m.e(Op::Call, r, 1, 0);
    }
    m.e(Op::NewArrLit, 6, 3, 3);
    m.e(Op::Ret, 6, 0, 0);
    let mut c = F::new(&sh, 1, 0);
    c.e(Op::GetCtx, 0, 0, 0);
    c.e(Op::AddI, 0, 0, 1);
    c.e(Op::SetCtx, 0, 0, 0);
    c.e(Op::Ret, 0, 0, 0);
    let (_vm, v) = run_main(&sh, vec![m.build(), c.build()]);
    assert_eq!(elements(v), [n(11), n(12), n(13)]);
}

#[test]
fn copy_ctx_gives_each_loop_iteration_its_own_variable() {
    let sh = shared();
    let mut m = F::new(&sh, 10, 0);
    m.funcs = vec![1];
    let (head, done) = (m.label(), m.label());
    m.e(Op::NewArray, 0, 0, 0);
    m.e(Op::PushCtx, 0, 1, 0);
    m.li(1, 0);
    m.e(Op::InitCtx, 1, 0, 0);
    m.bind(head);
    m.e(Op::GetCtx, 2, 0, 0);
    m.ji(Op::JGeI, 2, 3, done);
    m.e(Op::Closure, 3, 0, 0);
    m.e(Op::ArrayPush, 0, 3, 0);
    m.e(Op::CopyCtx, 0, 0, 0);
    m.e(Op::GetCtx, 2, 0, 0);
    m.e(Op::AddI, 2, 2, 1);
    m.e(Op::SetCtx, 2, 0, 0);
    m.j(Op::JmpBack, 0, 0, head);
    m.bind(done);
    m.e(Op::PopCtx, 0, 0, 0);
    m.e(Op::LoadUndef, 5, 0, 0);
    for i in 0..3 {
        m.e(Op::GetElemI, 4, 0, i);
        m.e(Op::Call, 6 + i, 4, 0);
    }
    m.e(Op::NewArrLit, 9, 6, 3);
    m.e(Op::Ret, 9, 0, 0);
    let mut c = F::new(&sh, 1, 0);
    c.e(Op::GetCtx, 0, 0, 0);
    c.e(Op::Ret, 0, 0, 0);
    let (_vm, v) = run_main(&sh, vec![m.build(), c.build()]);
    assert_eq!(elements(v), [n(0), n(1), n(2)]);
}

#[test]
fn a_context_variable_in_its_dead_zone_throws_a_reference_error() {
    let sh = shared();
    let mut f = F::new(&sh, 3, 0);
    let x = f.atom("x");
    f.e(Op::PushCtx, 0, 1, 0);
    f.ex(Op::GetCtxTdz, 0, 0, 0, x);
    f.e(Op::Ret, 0, 0, 0);
    assert_eq!(run_err(&sh, vec![f.build()]), "ReferenceError: cannot access 'x' before initialization");
    let mut g = F::new(&sh, 1, 0);
    let y = g.atom("y");
    g.e(Op::LoadHole, 0, 0, 0);
    g.e(Op::CheckInit, 0, y, 0);
    g.e(Op::Ret, 0, 0, 0);
    assert_eq!(run_err(&sh, vec![g.build()]), "ReferenceError: cannot access 'y' before initialization");
}

// ---- properties and caches ----

/// A function `(o) => o.x`; the cache of its one site is `vm.fbs[0].caches[0]`.
fn getter(sh: &Arc<SharedAtoms>, name: &str) -> Vec<Code> {
    let mut f = F::new(sh, 2, 1);
    f.get(1, 0, name);
    f.e(Op::Ret, 1, 0, 0);
    vec![f.build()]
}

fn call1(vm: &mut Vm, f: Value, arg: Value) -> Value {
    vm.call(f, Value::UNDEFINED, &[arg]).unwrap_or_else(|e| panic!("{e:?}"))
}

#[test]
fn a_get_field_site_goes_from_fresh_to_mono_poly_and_mega() {
    let sh = shared();
    let mut vm = vm_with(&sh, getter(&sh, "x"));
    let f = vm.function(0);
    let st = |vm: &Vm| (vm.fbs[0].caches[0].state, vm.fbs[0].entries(0));
    assert!(!vm.fbs[0].ready, "a feedback vector is made at the first run of its code");
    let o = mk(&mut vm, &[("x", 1)]);
    assert_eq!(call1(&mut vm, f, o), n(1));
    assert_eq!(st(&vm), (SEEN, 0), "the first use makes no entry");
    let o = mk(&mut vm, &[("x", 2)]);
    assert_eq!(call1(&mut vm, f, o), n(2));
    assert_eq!(st(&vm), (MONO, 1), "the second use of the site fills it");
    let shape = vm.fbs[0].caches[0].shape;
    for i in 3..6 {
        let o = mk(&mut vm, &[("x", i)]);
        assert_eq!(call1(&mut vm, f, o), n(i));
    }
    assert_eq!(st(&vm), (MONO, 1), "a hit does not change the record");
    assert_eq!(vm.fbs[0].caches[0].shape, shape);
    let o = mk(&mut vm, &[("y", 0), ("x", 9)]);
    assert_eq!(call1(&mut vm, f, o), n(9));
    assert_eq!(st(&vm), (POLY, 2));
    for (k, want) in [("a", 3), ("b", 4)] {
        let o = mk(&mut vm, &[(k, 0), ("x", want)]);
        assert_eq!(call1(&mut vm, f, o), n(want));
        assert_eq!(call1(&mut vm, f, o), n(want), "a pool entry hits");
    }
    assert_eq!(st(&vm), (POLY, 4));
    let o = mk(&mut vm, &[("c", 0), ("x", 5)]);
    assert_eq!(call1(&mut vm, f, o), n(5));
    assert_eq!(st(&vm), (MEGA, 0));
    assert_eq!(vm.fbs[0].caches[0].shape, 0, "no object has shape 0");
    let o = mk(&mut vm, &[("x", 6)]);
    assert_eq!(call1(&mut vm, f, o), n(6), "a megamorphic site still reads");
    assert_eq!(st(&vm).0, MEGA);
}

#[test]
fn a_get_field_of_an_inherited_name_makes_a_prototype_entry_and_a_missing_name_makes_none() {
    let sh = shared();
    let mut vm = vm_with(&sh, getter(&sh, "inherited"));
    let f = vm.function(0);
    let proto = vm.realm.object_proto;
    vm.realm.set_own(proto, "inherited", n(5));
    let o = mk(&mut vm, &[("x", 1)]);
    for _ in 0..4 {
        assert_eq!(call1(&mut vm, f, o), n(5));
    }
    // The first read makes the site `SEEN`, the second makes the entry (a prototype entry: the record has shape 0).
    assert_eq!((vm.fbs[0].caches[0].state, vm.fbs[0].entries(0), vm.fbs[0].protos.len()), (MONO, 1, 1));
    assert_eq!(vm.fbs[0].caches[0].shape, 0, "the own-property hit never matches a prototype entry");
    let mut vm = vm_with(&sh, getter(&sh, "missing"));
    let f = vm.function(0);
    let o = mk(&mut vm, &[("x", 1)]);
    for _ in 0..4 {
        assert_eq!(call1(&mut vm, f, o), Value::UNDEFINED);
    }
    assert_eq!((vm.fbs[0].caches[0].state, vm.fbs[0].entries(0)), (SEEN, 0));
}

#[test]
fn get_field_on_primitives_and_on_undefined() {
    let sh = shared();
    let mut vm = vm_with(&sh, getter(&sh, "length"));
    let f = vm.function(0);
    let s = vm.realm.string("abc");
    assert_eq!(call1(&mut vm, f, s), n(3));
    let a = Obj::array(&mut vm.realm.heap, &vm.realm.shapes, vm.realm.s_array, &[n(1), n(2)]).unwrap().value();
    assert_eq!(call1(&mut vm, f, a), n(2));
    assert_eq!(call1(&mut vm, f, n(5)), Value::UNDEFINED);
    match vm.call(f, Value::UNDEFINED, &[Value::UNDEFINED]) {
        Err(Exit::Throw(e)) => assert_eq!(vm.realm.error_text(e), "TypeError: cannot read properties of undefined (reading 'length')"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_set_field_that_adds_a_property_is_a_cache_hit_with_a_new_shape() {
    let sh = shared();
    let mut f = F::new(&sh, 2, 2);
    f.set(0, 1, "z");
    f.e(Op::RetUndef, 0, 0, 0);
    let mut vm = vm_with(&sh, vec![f.build()]);
    let fun = vm.function(0);
    let put = |vm: &mut Vm, o: Value, v: i32| {
        vm.call(fun, Value::UNDEFINED, &[o, n(v)]).unwrap_or_else(|e| panic!("{e:?}"));
    };
    let a1 = mk(&mut vm, &[("x", 1)]);
    put(&mut vm, a1, 5);
    assert_eq!(prop(&mut vm, a1, "z"), n(5));
    assert_eq!(vm.fbs[0].caches[0].state, SEEN, "the first add makes no entry");
    let a2 = mk(&mut vm, &[("x", 2)]);
    let before = Realm::obj(a2).unwrap().shape(&vm.realm.shapes);
    put(&mut vm, a2, 6);
    let c = vm.fbs[0].caches[0];
    assert_eq!((c.state, c.shape), (MONO, before.raw()));
    assert_ne!(c.to, 0, "the entry has the shape after the add");
    assert_eq!(Realm::obj(a2).unwrap().shape(&vm.realm.shapes).raw(), c.to);
    let a3 = mk(&mut vm, &[("x", 3)]);
    put(&mut vm, a3, 7);
    assert_eq!(vm.fbs[0].caches[0].state, MONO, "the third add is a hit");
    assert_eq!(prop(&mut vm, a3, "z"), n(7));
    assert_eq!(Realm::obj(a3).unwrap().shape(&vm.realm.shapes).raw(), c.to);
    assert_eq!(prop(&mut vm, a3, "x"), n(3));
    // An object that has `z` now: another shape, so a second entry (a plain write).
    put(&mut vm, a3, 8);
    assert_eq!((vm.fbs[0].caches[0].state, vm.fbs[0].entries(0)), (POLY, 2));
    assert_eq!(prop(&mut vm, a3, "z"), n(8));
    put(&mut vm, a2, 9);
    assert_eq!(prop(&mut vm, a2, "z"), n(9));
    assert_eq!(vm.fbs[0].entries(0), 2, "a pool entry hits for both kinds");
}

#[test]
fn set_field_variants_store_constants() {
    let sh = shared();
    let mut f = F::new(&sh, 8, 0);
    let a = f.atom("str");
    f.e(Op::NewObject, 0, 0, 0);
    for (i, name) in ["u", "nl", "fl", "tr"].iter().enumerate() {
        let s = f.site(name);
        f.e(Op::SetFieldK, 0, i as u32, s);
    }
    let s = f.site("neg");
    f.e(Op::SetFieldI, 0, -7i32 as u32, s);
    let s = f.site("str");
    f.e(Op::SetFieldA, 0, a, s);
    f.e(Op::Ret, 0, 0, 0);
    let (mut vm, o) = run_main(&sh, vec![f.build()]);
    assert_eq!(prop(&mut vm, o, "u"), Value::UNDEFINED);
    assert_eq!(prop(&mut vm, o, "nl"), Value::NULL);
    assert_eq!(prop(&mut vm, o, "fl"), Value::FALSE);
    assert_eq!(prop(&mut vm, o, "tr"), Value::TRUE);
    assert_eq!(prop(&mut vm, o, "neg"), n(-7));
    let s = prop(&mut vm, o, "str");
    assert_eq!(text(&vm, s), "str");
}

#[test]
fn object_literals_share_one_shape_and_their_reads_hit() {
    let sh = shared();
    let mut f = F::new(&sh, 12, 0);
    let (ka, kb) = (f.atom("a"), f.atom("b"));
    f.recipes.push(&[ka, kb]);
    let (head, done) = (f.label(), f.label());
    f.li(0, 0); // sum
    f.li(1, 0); // i
    f.bind(head);
    f.ji(Op::JGeI, 1, 1000, done);
    f.e(Op::Mov, 4, 1, 0);
    f.li(5, 1);
    f.e(Op::NewObjLit, 2, 4, 0);
    f.get(6, 2, "a");
    f.get(7, 2, "b");
    f.e(Op::Add, 0, 0, 6);
    f.e(Op::Add, 0, 0, 7);
    f.e(Op::AddI, 1, 1, 1);
    f.j(Op::JmpBack, 0, 0, head);
    f.bind(done);
    f.e(Op::Ret, 0, 0, 0);
    let (vm, v) = run_main(&sh, vec![f.build()]);
    assert_eq!(v, n(499500 + 1000));
    for site in 0..2 {
        assert_eq!((vm.fbs[0].caches[site].state, vm.fbs[0].entries(site)), (MONO, 1));
    }
}

#[test]
fn globals_are_cached_and_a_missing_one_throws() {
    let sh = shared();
    let mut f = F::new(&sh, 4, 0);
    let (set, get, get2, ty) = (f.site("g"), f.site("g"), f.site("g"), f.site("nope"));
    f.li(0, 5);
    f.e(Op::SetGlobal, 0, set, 0);
    f.e(Op::GetGlobal, 1, get, 0);
    f.e(Op::GetGlobal, 2, get2, 0);
    f.e(Op::TypeofGlobal, 3, ty, 0);
    f.e(Op::NewArrLit, 0, 1, 3);
    f.e(Op::Ret, 0, 0, 0);
    let (vm, v) = run_main(&sh, vec![f.build()]);
    let v = elements(v);
    assert_eq!((v[0], v[1]), (n(5), n(5)));
    assert_eq!(text(&vm, v[2]), "undefined");
    let mut g = F::new(&sh, 1, 0);
    let s = g.site("nope");
    g.e(Op::GetGlobal, 0, s, 0);
    g.e(Op::Ret, 0, 0, 0);
    assert_eq!(run_err(&sh, vec![g.build()]), "ReferenceError: nope is not defined");
}

#[test]
fn get_ctx_field_reads_a_field_of_a_context_value() {
    let sh = shared();
    let mut f = F::new(&sh, 4, 0);
    let (ka, s) = (f.atom("k"), f.site("k"));
    f.recipes.push(&[ka]);
    f.li(1, 41);
    f.e(Op::NewObjLit, 0, 1, 0);
    f.e(Op::PushCtx, 0, 1, 0);
    f.e(Op::InitCtx, 0, 0, 0);
    f.ex(Op::GetCtxField, 2, 0, 0, s);
    f.e(Op::AddI, 2, 2, 1);
    f.e(Op::Ret, 2, 0, 0);
    let (_vm, v) = run_main(&sh, vec![f.build()]);
    assert_eq!(v, n(42));
}

// ---- elements ----

#[test]
fn elements_read_write_grow_and_truncate() {
    let sh = shared();
    let mut f = F::new(&sh, 20, 0);
    let head = f.label();
    let done = f.label();
    f.e(Op::NewArray, 0, 0, 0);
    f.li(1, 0);
    f.bind(head);
    f.ji(Op::JGeI, 1, 5, done);
    f.e(Op::Mul, 2, 1, 1);
    f.e(Op::ArrayPush, 0, 2, 0);
    f.e(Op::AddI, 1, 1, 1);
    f.j(Op::JmpBack, 0, 0, head);
    f.bind(done);
    f.li(3, 1);
    f.li(4, 100);
    f.e(Op::SetElem, 0, 3, 4); // a[1] = 100
    f.e(Op::GetElemI, 10, 0, 1);
    f.e(Op::GetElemI, 11, 0, 4);
    f.e(Op::GetElemI, 12, 0, 9); // out of range
    f.li(5, 7);
    f.e(Op::SetElem, 0, 5, 4); // a[7] = 100: holes between
    f.get(13, 0, "length");
    f.li(6, 2);
    f.set(0, 6, "length"); // a.length = 2
    f.get(14, 0, "length");
    f.e(Op::GetElemI, 15, 0, 3);
    f.e(Op::NewArrLit, 16, 10, 6);
    f.e(Op::Ret, 16, 0, 0);
    let (_vm, v) = run_main(&sh, vec![f.build()]);
    assert_eq!(elements(v), [n(100), n(16), Value::UNDEFINED, n(8), n(2), Value::UNDEFINED]);
}

#[test]
fn a_plain_object_takes_integer_and_string_keys() {
    let sh = shared();
    let mut f = F::new(&sh, 10, 0);
    let k = f.atom("name");
    f.e(Op::NewObject, 0, 0, 0);
    f.li(1, 3);
    f.li(2, 7);
    f.e(Op::SetElem, 0, 1, 2); // o[3] = 7
    f.e(Op::LoadAtom, 3, k, 0);
    f.e(Op::SetElem, 0, 3, 1); // o["name"] = 3
    f.e(Op::GetElem, 4, 0, 1);
    f.e(Op::GetElem, 5, 0, 3);
    f.e(Op::In, 6, 3, 0);
    f.e(Op::In, 7, 2, 0); // 7 in o: no
    f.e(Op::Delete, 8, 0, 3);
    f.e(Op::In, 9, 3, 0);
    f.e(Op::NewArrLit, 1, 4, 6);
    f.e(Op::Ret, 1, 0, 0);
    let (_vm, v) = run_main(&sh, vec![f.build()]);
    assert_eq!(elements(v), [n(7), n(3), Value::TRUE, Value::FALSE, Value::TRUE, Value::FALSE]);
}

#[test]
fn for_of_walks_an_array_and_for_in_walks_the_own_keys() {
    let sh = shared();
    let mut f = F::new(&sh, 20, 0);
    let (ka, kb, empty) = (f.atom("a"), f.atom("b"), f.atom(""));
    f.recipes.push(&[ka, kb]);
    let (h1, d1, h2, d2) = (f.label(), f.label(), f.label(), f.label());
    f.li(10, 10);
    f.li(11, 20);
    f.li(12, 30);
    f.e(Op::NewArrLit, 0, 10, 3);
    f.e(Op::IterOpen, 1, 0, 0);
    f.li(5, 0);
    f.bind(h1);
    f.ji(Op::IterNext, 4, 1, d1);
    f.e(Op::Add, 5, 5, 4);
    f.j(Op::JmpBack, 0, 0, h1);
    f.bind(d1);
    f.e(Op::NewObjLit, 6, 10, 0);
    f.e(Op::ForInOpen, 1, 6, 0);
    f.e(Op::LoadAtom, 7, empty, 0);
    f.bind(h2);
    f.ji(Op::IterNext, 4, 1, d2);
    f.e(Op::Add, 7, 7, 4);
    f.j(Op::JmpBack, 0, 0, h2);
    f.bind(d2);
    f.e(Op::Mov, 13, 5, 0);
    f.e(Op::Mov, 14, 7, 0);
    f.e(Op::NewArrLit, 15, 13, 2);
    f.e(Op::Ret, 15, 0, 0);
    let (vm, v) = run_main(&sh, vec![f.build()]);
    let v = elements(v);
    assert_eq!(v[0], n(60));
    assert_eq!(text(&vm, v[1]), "ab");
}

// ---- strings and conversions ----

#[test]
fn strings_join_convert_and_have_a_type() {
    let sh = shared();
    let mut f = F::new(&sh, 24, 0);
    let (ab, cd, x, y) = (f.atom("ab"), f.atom("cd"), f.atom("x"), f.atom("y"));
    f.e(Op::LoadAtom, 0, ab, 0);
    f.e(Op::LoadAtom, 1, cd, 0);
    f.li(3, 5);
    f.e(Op::LoadAtom, 6, x, 0);
    f.li(7, 1);
    f.e(Op::LoadAtom, 8, y, 0);
    f.e(Op::Add, 10, 0, 1);
    f.e(Op::Add, 11, 0, 3);
    f.e(Op::Concat, 12, 6, 3);
    f.e(Op::Typeof, 13, 0, 0);
    f.e(Op::Typeof, 14, 3, 0);
    f.e(Op::ToStr, 15, 3, 0);
    f.e(Op::Typeof, 16, 1, 0);
    f.e(Op::NewArrLit, 20, 10, 6);
    f.e(Op::Ret, 20, 0, 0);
    let (vm, v) = run_main(&sh, vec![f.build()]);
    assert_eq!(texts(&vm, v), ["abcd", "ab5", "x1y", "string", "number", "5"]);
}

// ---- exceptions ----

#[test]
fn a_handler_in_the_same_function_gets_the_value() {
    let sh = shared();
    let mut f = F::new(&sh, 3, 0);
    let h = f.label();
    f.li(0, 42);
    let t = f.e(Op::Throw, 0, 0, 0);
    f.bind(h);
    f.e(Op::Ret, 1, 0, 0);
    f.handlers.push((t, t + 1, h, 1, 0));
    let (_vm, v) = run_main(&sh, vec![f.build()]);
    assert_eq!(v, n(42));
}

#[test]
fn an_exception_unwinds_the_frames_to_the_handler_of_a_caller() {
    let sh = shared();
    let mut m = F::new(&sh, 6, 0);
    let h = m.label();
    m.funcs = vec![1];
    m.e(Op::Closure, 0, 0, 0);
    m.e(Op::LoadUndef, 1, 0, 0);
    m.li(4, 3);
    let c = m.e(Op::Call, 2, 0, 0);
    m.e(Op::Ret, 2, 0, 0);
    m.bind(h);
    m.e(Op::Add, 3, 3, 4);
    m.e(Op::Ret, 3, 0, 0);
    m.handlers.push((c, c + 1, h, 3, 0));
    let mut g = F::new(&sh, 4, 0);
    g.funcs = vec![2];
    g.e(Op::Closure, 0, 0, 0);
    g.e(Op::LoadUndef, 1, 0, 0);
    g.e(Op::Call, 2, 0, 0);
    g.e(Op::Ret, 2, 0, 0);
    let mut t = F::new(&sh, 1, 0);
    t.li(0, 7);
    t.e(Op::Throw, 0, 0, 0);
    let (_vm, v) = run_main(&sh, vec![m.build(), g.build(), t.build()]);
    assert_eq!(v, n(10), "the caller's register r4 is still there");
}

#[test]
fn an_uncaught_exception_leaves_the_vm_and_it_can_run_again() {
    let sh = shared();
    let mut f = F::new(&sh, 1, 0);
    f.li(0, 9);
    f.e(Op::Throw, 0, 0, 0);
    let mut vm = vm_with(&sh, vec![f.build()]);
    assert_eq!(vm.run_main(0), Err(Exit::Throw(n(9))));
    assert_eq!(vm.run_main(0), Err(Exit::Throw(n(9))));
}

#[test]
fn a_handler_pops_the_contexts_that_the_try_block_pushed() {
    let sh = shared();
    let mut f = F::new(&sh, 3, 0);
    let h = f.label();
    f.li(0, 111);
    f.e(Op::PushCtx, 0, 1, 0);
    f.e(Op::InitCtx, 0, 0, 0);
    let s = f.e(Op::PushCtx, 0, 1, 0);
    f.li(1, 222);
    f.e(Op::InitCtx, 1, 0, 0);
    let t = f.e(Op::Throw, 1, 0, 0);
    f.bind(h);
    f.e(Op::GetCtx, 2, 0, 0);
    f.e(Op::Ret, 2, 0, 0);
    f.handlers.push((s, t + 1, h, 1, 1));
    let (_vm, v) = run_main(&sh, vec![f.build()]);
    assert_eq!(v, n(111));
}

#[test]
fn a_thrown_error_is_an_object_with_a_name_and_a_message() {
    let sh = shared();
    let mut f = F::new(&sh, 3, 0);
    let h = f.label();
    f.e(Op::LoadUndef, 0, 0, 0);
    let g = f.get(1, 0, "x");
    f.e(Op::Ret, 1, 0, 0);
    f.bind(h);
    f.e(Op::Ret, 2, 0, 0);
    f.handlers.push((g, g + 1, h, 2, 0));
    let (vm, v) = run_main(&sh, vec![f.build()]);
    assert_eq!(vm.realm.error_text(v), "TypeError: cannot read properties of undefined (reading 'x')");
}

// ---- collection ----

/// `churn(n)`: make `n` garbage objects.
fn churn(sh: &Arc<SharedAtoms>) -> Code {
    let mut f = F::new(sh, 4, 1);
    let (head, done) = (f.label(), f.label());
    f.li(1, 0);
    f.bind(head);
    f.j(Op::JGe, 1, 0, done);
    f.e(Op::NewObject, 2, 0, 0);
    f.li(3, 1);
    f.set(2, 3, "k");
    f.e(Op::AddI, 1, 1, 1);
    f.j(Op::JmpBack, 0, 0, head);
    f.bind(done);
    f.e(Op::RetUndef, 0, 0, 0);
    f.build()
}

fn small_heap(vm: &mut Vm) {
    let k = if MIRI { 4 << 10 } else { 32 << 10 };
    vm.realm.heap.set_trigger(Trigger { min: k, slack: k });
}

/// The main function of the collection tests: `live` is read after the call, `dead` is not (and `pin` keeps it in the maps).
fn gc_program(sh: &Arc<SharedAtoms>, rounds: i32, pin: &[usize]) -> Vec<Code> {
    let mut m = F::new(sh, 9, 0);
    m.funcs = vec![1];
    m.pin = pin.to_vec();
    m.e(Op::NewObject, 0, 0, 0);
    m.li(1, 77);
    m.set(0, 1, "x");
    m.e(Op::NewObject, 2, 0, 0);
    m.li(3, 5);
    m.set(2, 3, "d");
    m.e(Op::Closure, 4, 0, 0);
    m.e(Op::LoadUndef, 5, 0, 0);
    m.li(6, rounds);
    m.e(Op::Call, 7, 4, 1);
    m.get(8, 0, "x");
    m.e(Op::Ret, 8, 0, 0);
    vec![m.build(), churn(sh)]
}

#[test]
fn a_collection_inside_a_call_keeps_the_live_registers_of_every_frame() {
    let sh = shared();
    let rounds = if MIRI { 300 } else { 100_000 };
    let mut vm = vm_with(&sh, gc_program(&sh, rounds, &[]));
    small_heap(&mut vm);
    assert_eq!(vm.run_main(0), Ok(n(77)));
    assert!(vm.realm.collections > 1, "the churn collected {} times", vm.realm.collections);
}

#[test]
fn a_dead_register_is_not_a_root_and_a_pinned_one_is() {
    let sh = shared();
    let live = |pin: &[usize]| {
        let mut vm = vm_with(&sh, gc_program(&sh, if MIRI { 300 } else { 20_000 }, pin));
        small_heap(&mut vm);
        assert_eq!(vm.run_main(0), Ok(n(77)));
        assert!(vm.realm.collections > 0);
        vm.realm.last_gc.live_bytes
    };
    let (dead, pinned) = (live(&[]), live(&[2]));
    assert!(pinned > dead, "r2 pinned keeps one more object alive: {pinned} against {dead}");
}

#[test]
fn closures_and_their_contexts_survive_collections() {
    let sh = shared();
    let mut m = F::new(&sh, 12, 0);
    m.funcs = vec![1, 2];
    m.li(0, 10);
    m.e(Op::PushCtx, 0, 1, 0);
    m.e(Op::InitCtx, 0, 0, 0);
    m.e(Op::Closure, 1, 0, 0);
    m.e(Op::PopCtx, 0, 0, 0);
    m.e(Op::Closure, 8, 1, 0);
    m.e(Op::LoadUndef, 2, 0, 0);
    m.e(Op::Call, 3, 1, 0);
    m.e(Op::LoadUndef, 9, 0, 0);
    m.li(10, if MIRI { 300 } else { 60_000 });
    m.e(Op::Call, 11, 8, 1);
    m.e(Op::Call, 4, 1, 0);
    m.e(Op::NewArrLit, 5, 3, 2);
    m.e(Op::Ret, 5, 0, 0);
    let mut c = F::new(&sh, 1, 0);
    c.e(Op::GetCtx, 0, 0, 0);
    c.e(Op::AddI, 0, 0, 1);
    c.e(Op::SetCtx, 0, 0, 0);
    c.e(Op::Ret, 0, 0, 0);
    let mut vm = vm_with(&sh, vec![m.build(), c.build(), churn(&sh)]);
    small_heap(&mut vm);
    let v = vm.run_main(0).unwrap();
    assert!(vm.realm.collections > 1);
    assert_eq!(elements(v), [n(11), n(12)]);
}

#[test]
fn a_cache_is_cleared_when_a_collection_frees_a_shape() {
    let sh = shared();
    let mut vm = vm_with(&sh, getter(&sh, "x"));
    let f = vm.function(0);
    let g = vm.realm.global;
    vm.realm.set_own(g, "getter", f); // a root: nothing else keeps the function cell
    for i in 0..3 {
        let o = mk(&mut vm, &[("x", i)]);
        call1(&mut vm, f, o);
    }
    assert_eq!(vm.fbs[0].caches[0].state, MONO);
    // Shapes that nothing keeps: the collection frees them and bumps the epoch.
    for i in 0..50 {
        mk(&mut vm, &[(&format!("tmp{i}"), 0), ("x", 1)]);
    }
    vm.collect_idle();
    assert_eq!(vm.fbs[0].caches[0].state, FRESH, "the cache would name a shape id that may be reused");
    let f = prop(&mut vm, g.value(), "getter");
    let o = mk(&mut vm, &[("x", 4)]);
    assert_eq!(call1(&mut vm, f, o), n(4));
}

#[test]
fn every_program_of_these_tests_is_verified_and_the_op_list_is_covered() {
    // `step!` has an arm for each op (the match is exhaustive: a new op does not compile until it has one), so this only checks
    // that the table that the loop indexes (`Op::ALL`) is in the order of the numbers.
    for (i, op) in Op::ALL.iter().enumerate() {
        assert_eq!(*op as usize, i);
    }
}
