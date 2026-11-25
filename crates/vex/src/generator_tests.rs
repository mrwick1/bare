// Tests of `generator.rs` (plan step G1). There is no `function*` in the compiler yet (G2): every generator is hand-built with `Asm`, then
// checked by `verify::verify`, as in `interp_tests.rs`.
use super::*;
use crate::heap::Trigger;
use crate::interp::{Exit, Program, Vm};
use crate::interp_rt::Key;

mod prelude {
    pub use crate::atoms::SharedAtoms;
    pub use crate::bytecode::{Asm, Code, GK_GEN, Handler, Ins, Label, NO_REG, Op, Recipes, decode};
    pub use crate::roots::RootMaps;
    pub use crate::string::{Chars, Str};
    pub use crate::verify::verify;
    pub use std::collections::BTreeSet;
    pub use std::sync::Arc;
}
#[path = "asm_util.rs"]
mod asm_util;
use asm_util::F;
use prelude::*;

const MIRI: bool = cfg!(miri);

fn n(i: i32) -> Value {
    Value::int(i)
}

fn shared() -> Arc<SharedAtoms> {
    Arc::new(SharedAtoms::default())
}

fn prop(vm: &mut Vm, o: Value, name: &str) -> Value {
    let k = vm.realm.intern(name);
    vm.realm.get_prop(o, Key::Name(k)).expect("a property")
}

/// `function* (n) { var i = 0, acc = 100; for (; i < n; i++) { sent = yield i; acc += sent; } return acc; }`. A `throw(e)` throws `e` at the
/// yield, a `return(v)` returns `v`. Three registers stay live across the yield.
fn gen_acc(sh: &Arc<SharedAtoms>) -> Code {
    gen_acc_regs(sh, 8)
}

fn gen_acc_regs(sh: &Arc<SharedAtoms>, nregs: u16) -> Code {
    let mut g = F::new(sh, nregs, 1);
    let (head, cont, thr, done) = (g.label(), g.label(), g.label(), g.label());
    g.gen_init(&[0]);
    g.li(1, 0);
    g.li(2, 100);
    g.bind(head);
    g.j(Op::JGe, 1, 0, done);
    g.yield_(1, 3, 4, &[0, 1, 2]);
    g.ji(Op::JSEqI, 4, MODE_NEXT, cont);
    g.ji(Op::JSEqI, 4, MODE_THROW, thr);
    g.e(Op::RetGen, 3, 0, 0);
    g.bind(thr);
    g.e(Op::Throw, 3, 0, 0);
    g.bind(cont);
    g.e(Op::Add, 2, 2, 3);
    g.e(Op::Inc, 1, 1, 0);
    g.j(Op::JmpBack, 0, 0, head);
    g.bind(done);
    g.e(Op::RetGen, 2, 0, 0);
    g.build()
}

/// `function* () { try { yield 1; } catch (e) { return e; } return 0; }`
fn gen_catch(sh: &Arc<SharedAtoms>) -> Code {
    let mut g = F::new(sh, 6, 0);
    let (cont, thr, handler) = (g.label(), g.label(), g.label());
    g.gen_init(&[]);
    g.li(0, 1);
    let y = g.yield_(0, 1, 2, &[]);
    g.ji(Op::JSEqI, 2, MODE_NEXT, cont);
    g.ji(Op::JSEqI, 2, MODE_THROW, thr);
    g.e(Op::RetGen, 1, 0, 0);
    g.bind(thr);
    g.e(Op::Throw, 1, 0, 0);
    g.bind(cont);
    g.li(3, 0);
    g.e(Op::RetGen, 3, 0, 0);
    g.bind(handler);
    g.e(Op::RetGen, 4, 0, 0);
    g.handlers.push((y, y + 5, handler, 4, 0));
    g.build()
}

/// The main function that makes a generator from the code `f` (index 1) with the argument `arg` and returns it.
fn make(sh: &Arc<SharedAtoms>, arg: i32) -> Code {
    let mut m = F::new(sh, 6, 0);
    m.funcs = vec![1];
    m.e(Op::Closure, 0, 0, 0);
    m.li(1, arg);
    m.e(Op::CallU, 2, 0, 1);
    m.e(Op::Ret, 2, 0, 0);
    m.build()
}

fn host(codes: Vec<Code>, sh: &Arc<SharedAtoms>) -> (Vm, Value) {
    let mut vm = Vm::new(Program { codes }, sh.clone());
    let g = vm.run_main(0).unwrap_or_else(|e| panic!("uncaught {e:?}"));
    vm.realm.consts.push(g);
    (vm, g)
}

fn step(vm: &mut Vm, g: Value, mode: i32, v: Value) -> (Value, bool) {
    vm.resume(g, mode, v).unwrap_or_else(|e| panic!("uncaught {e:?}"))
}

#[test]
fn next_runs_the_body_in_steps_and_sends_values_in() {
    let sh = shared();
    let (mut vm, g) = host(vec![make(&sh, 3), gen_acc(&sh)], &sh);
    // The first `next` starts the body: its value is not seen.
    assert_eq!(step(&mut vm, g, MODE_NEXT, n(999)), (n(0), false));
    assert_eq!(step(&mut vm, g, MODE_NEXT, n(5)), (n(1), false));
    assert_eq!(step(&mut vm, g, MODE_NEXT, n(7)), (n(2), false));
    // The loop ends and the body returns `acc`: 100 + 5 + 7 + 1.
    assert_eq!(step(&mut vm, g, MODE_NEXT, n(1)), (n(113), true));
    assert!(vm.realm.gen_done(g));
    // A finished generator answers with no value; `return` gives its argument back.
    assert_eq!(step(&mut vm, g, MODE_NEXT, n(1)), (Value::UNDEFINED, true));
    assert_eq!(step(&mut vm, g, MODE_RETURN, n(4)), (n(4), true));
    assert_eq!(vm.resume(g, MODE_THROW, n(6)), Err(Exit::Throw(n(6))));
}

#[test]
fn return_ends_a_suspended_generator_and_a_new_one_without_running_it() {
    let sh = shared();
    let (mut vm, g) = host(vec![make(&sh, 3), gen_acc(&sh)], &sh);
    assert_eq!(step(&mut vm, g, MODE_NEXT, Value::UNDEFINED), (n(0), false));
    assert_eq!(step(&mut vm, g, MODE_RETURN, n(42)), (n(42), true));
    assert_eq!(step(&mut vm, g, MODE_NEXT, n(1)), (Value::UNDEFINED, true));
    // A generator that never started: `return` and `throw` end it at once.
    let (mut vm, g) = host(vec![make(&sh, 3), gen_acc(&sh)], &sh);
    assert_eq!(step(&mut vm, g, MODE_RETURN, n(8)), (n(8), true));
    assert_eq!(step(&mut vm, g, MODE_NEXT, Value::UNDEFINED), (Value::UNDEFINED, true));
    let (mut vm, g) = host(vec![make(&sh, 3), gen_acc(&sh)], &sh);
    assert_eq!(vm.resume(g, MODE_THROW, n(9)), Err(Exit::Throw(n(9))));
    assert_eq!(step(&mut vm, g, MODE_NEXT, Value::UNDEFINED), (Value::UNDEFINED, true));
}

#[test]
fn an_exception_out_of_the_body_ends_the_generator() {
    let sh = shared();
    let (mut vm, g) = host(vec![make(&sh, 3), gen_acc(&sh)], &sh);
    assert_eq!(step(&mut vm, g, MODE_NEXT, Value::UNDEFINED), (n(0), false));
    // `throw` makes the body throw at the yield; nothing catches it.
    assert_eq!(vm.resume(g, MODE_THROW, n(77)), Err(Exit::Throw(n(77))));
    assert!(vm.realm.gen_done(g));
    assert_eq!(step(&mut vm, g, MODE_NEXT, Value::UNDEFINED), (Value::UNDEFINED, true));
    assert!(vm.stack.iter().all(|v| !v.is_hole()), "the run left the stack in a state that the next run can use");
}

#[test]
fn a_handler_around_a_yield_catches_what_throw_sends() {
    let sh = shared();
    let (mut vm, g) = host(vec![make(&sh, 0), gen_catch(&sh)], &sh);
    assert_eq!(step(&mut vm, g, MODE_NEXT, Value::UNDEFINED), (n(1), false));
    assert_eq!(step(&mut vm, g, MODE_THROW, n(31)), (n(31), true));
    // The other paths: `next` falls out of the try and returns 0.
    let (mut vm, g) = host(vec![make(&sh, 0), gen_catch(&sh)], &sh);
    assert_eq!(step(&mut vm, g, MODE_NEXT, Value::UNDEFINED), (n(1), false));
    assert_eq!(step(&mut vm, g, MODE_NEXT, Value::UNDEFINED), (n(0), true));
}

#[test]
fn two_generators_of_one_function_keep_their_own_state() {
    let sh = shared();
    let mut m = F::new(&sh, 8, 0);
    m.funcs = vec![1];
    m.e(Op::Closure, 0, 0, 0);
    m.li(1, 3);
    m.e(Op::CallU, 2, 0, 1);
    m.e(Op::Mov, 4, 0, 0);
    m.li(5, 3);
    m.e(Op::CallU, 3, 4, 1);
    m.e(Op::NewArrLit, 6, 2, 2);
    m.e(Op::Ret, 6, 0, 0);
    let (mut vm, arr) = host(vec![m.build(), gen_acc(&sh)], &sh);
    let (a, b) = (Realm::obj(arr).unwrap().get_element(0).unwrap(), Realm::obj(arr).unwrap().get_element(1).unwrap());
    assert_ne!(a, b);
    assert_eq!(step(&mut vm, a, MODE_NEXT, Value::UNDEFINED), (n(0), false));
    assert_eq!(step(&mut vm, a, MODE_NEXT, n(1)), (n(1), false));
    assert_eq!(step(&mut vm, b, MODE_NEXT, Value::UNDEFINED), (n(0), false));
    assert_eq!(step(&mut vm, a, MODE_NEXT, n(2)), (n(2), false));
    assert_eq!(step(&mut vm, b, MODE_NEXT, n(10)), (n(1), false));
    assert_eq!(step(&mut vm, a, MODE_NEXT, n(3)), (n(106), true));
    assert_eq!(step(&mut vm, b, MODE_NEXT, n(20)), (n(2), false));
}

/// The main function: `var g = f(n); var s = 0; for (i = 0; i < n; i++) s += g.next(i).value; return s;` for `gen_acc`; the values that it
/// yields are 0, 1, 2, ...
fn main_next(sh: &Arc<SharedAtoms>, n: i32) -> Code {
    let mut m = F::new(sh, 13, 0);
    let (head, done) = (m.label(), m.label());
    m.funcs = vec![1];
    m.e(Op::Closure, 0, 0, 0);
    m.e(Op::Mov, 3, 0, 0);
    m.li(4, n);
    m.e(Op::CallU, 5, 3, 1);
    m.li(6, 0);
    m.li(7, 0);
    m.get(2, 5, "next");
    m.bind(head);
    m.ji(Op::JnLtI, 7, n, done);
    m.e(Op::Mov, 8, 2, 0);
    m.e(Op::Mov, 9, 5, 0);
    m.e(Op::Mov, 10, 7, 0);
    m.e(Op::Call, 11, 8, 1);
    m.get(12, 11, "value");
    m.e(Op::Add, 6, 6, 12);
    m.e(Op::Inc, 7, 7, 0);
    m.j(Op::JmpBack, 0, 0, head);
    m.bind(done);
    m.e(Op::Ret, 6, 0, 0);
    m.build()
}

#[test]
fn next_from_a_script_gives_a_result_object() {
    let sh = shared();
    let mut vm = Vm::new(Program { codes: vec![main_next(&sh, 10), gen_acc(&sh)] }, sh.clone());
    assert_eq!(vm.run_main(0), Ok(n(45)));
    // The result object: `{ value, done }`, from a main that calls `next` once more than the generator yields.
    let mut m = F::new(&sh, 10, 0);
    m.funcs = vec![1];
    m.e(Op::Closure, 0, 0, 0);
    m.li(1, 0);
    m.e(Op::CallU, 2, 0, 1);
    m.get(3, 2, "next");
    m.e(Op::Mov, 4, 3, 0);
    m.e(Op::Mov, 5, 2, 0);
    m.e(Op::Call, 6, 4, 0);
    m.e(Op::Ret, 6, 0, 0);
    let mut vm = Vm::new(Program { codes: vec![m.build(), gen_acc(&sh)] }, sh.clone());
    let r = vm.run_main(0).unwrap();
    assert_eq!(prop(&mut vm, r, "value"), n(100));
    assert_eq!(prop(&mut vm, r, "done"), Value::TRUE);
}

#[test]
fn a_generator_that_calls_next_on_itself_throws_a_type_error() {
    let sh = shared();
    // `function* () { return this.g.next(); }` called as a method of `o`, and then `o.g = g; g.next()`.
    let mut g = F::new(&sh, 6, 0);
    g.gen_init(&[]);
    g.e(Op::LoadThis, 0, 0, 0);
    g.get(1, 0, "g");
    g.get(2, 1, "next");
    g.e(Op::Mov, 3, 1, 0);
    g.e(Op::Call, 4, 2, 0);
    g.e(Op::RetGen, 4, 0, 0);
    let mut m = F::new(&sh, 10, 0);
    m.funcs = vec![1];
    m.e(Op::NewObject, 0, 0, 0);
    m.e(Op::Closure, 1, 0, 0);
    m.e(Op::Mov, 2, 0, 0);
    m.e(Op::Call, 3, 1, 0);
    m.set(0, 3, "g");
    m.get(4, 3, "next");
    m.e(Op::Mov, 5, 3, 0);
    m.e(Op::Call, 6, 4, 0);
    m.e(Op::Ret, 6, 0, 0);
    let mut vm = Vm::new(Program { codes: vec![m.build(), g.build()] }, sh.clone());
    let Err(Exit::Throw(e)) = vm.run_main(0) else { panic!("no exception") };
    assert_eq!(vm.realm.error_text(e), "TypeError: the generator is already running");
    // The exception left the running frame: the generator is done.
}

#[test]
fn next_on_something_that_is_not_a_generator_throws() {
    let sh = shared();
    let (mut vm, g) = host(vec![make(&sh, 1), gen_acc(&sh)], &sh);
    let next = prop(&mut vm, g, "next");
    let o = Obj::new(&mut vm.realm.heap, &vm.realm.shapes, vm.realm.state.s_object).unwrap().value();
    for this in [Value::UNDEFINED, n(1), o] {
        let e = vm.realm.call_function(next, this, &[]).expect_err("a TypeError");
        assert!(vm.realm.error_text(e).starts_with("TypeError"), "{}", vm.realm.error_text(e));
    }
}

#[test]
fn the_prototype_of_a_generator_has_next_return_and_throw() {
    let sh = shared();
    let (mut vm, g) = host(vec![make(&sh, 1), gen_acc(&sh)], &sh);
    for name in ["next", "return", "throw"] {
        let f = prop(&mut vm, g, name);
        assert!(vm.realm.native_id(f).is_some(), "{name}");
    }
}

// ---- a nested run on a pooled machine ----

/// `valueOf` of the object: `return this.g.next().value;`
fn value_of(sh: &Arc<SharedAtoms>) -> Code {
    let mut f = F::new(sh, 6, 0);
    f.e(Op::LoadThis, 0, 0, 0);
    f.get(1, 0, "g");
    f.get(2, 1, "next");
    f.e(Op::Mov, 3, 1, 0);
    f.e(Op::Call, 4, 2, 0);
    f.get(5, 4, "value");
    f.e(Op::Ret, 5, 0, 0);
    f.build()
}

/// The generator starts on the machine of the host, is resumed by a `valueOf` that a `+` runs on a pooled machine (it yields there), and is
/// resumed again on the first machine. `value_of` is code 2.
fn main_pooled(sh: &Arc<SharedAtoms>) -> Code {
    let mut m = F::new(sh, 16, 0);
    m.funcs = vec![1, 2];
    m.e(Op::Closure, 0, 0, 0);
    m.li(1, 5);
    m.e(Op::CallU, 2, 0, 1);
    m.get(3, 2, "next");
    m.e(Op::Mov, 4, 3, 0);
    m.e(Op::Mov, 5, 2, 0);
    m.li(6, 0);
    m.e(Op::Call, 7, 4, 1);
    m.get(8, 7, "value");
    m.e(Op::NewObject, 9, 0, 0);
    m.set(9, 2, "g");
    m.e(Op::Closure, 10, 1, 0);
    m.set(9, 10, "valueOf");
    m.li(11, 1);
    m.e(Op::Add, 12, 9, 11);
    m.e(Op::Mov, 4, 3, 0);
    m.e(Op::Mov, 5, 2, 0);
    m.li(6, 0);
    m.e(Op::Call, 13, 4, 1);
    m.get(14, 13, "value");
    // 100 * (first value) + 10 * (value in the nested run + 1) + (last value)
    m.li(15, 100);
    m.e(Op::Mul, 8, 8, 15);
    m.li(15, 10);
    m.e(Op::Mul, 12, 12, 15);
    m.e(Op::Add, 8, 8, 12);
    m.e(Op::Add, 8, 8, 14);
    m.e(Op::Ret, 8, 0, 0);
    m.build()
}

#[test]
fn a_generator_that_suspended_on_one_machine_resumes_on_another() {
    let sh = shared();
    let mut vm = Vm::new(Program { codes: vec![main_pooled(&sh), gen_acc(&sh), value_of(&sh)] }, sh.clone());
    // 0 on the first machine, 1 on the pooled one (1 + 1 = 2, times 10), 2 on the first machine again.
    assert_eq!(vm.run_main(0).map(|v| v.as_f64()), Ok(Some(22.0)));
    assert!(!vm.realm.pool.is_empty(), "the `valueOf` ran on a pooled machine");
    assert_eq!(vm.realm.nest, 0);
}

#[test]
fn a_native_that_calls_next_runs_the_generator_on_a_pooled_machine_and_the_host_goes_on() {
    let sh = shared();
    let (mut vm, g) = host(vec![make(&sh, 4), gen_acc(&sh)], &sh);
    assert_eq!(step(&mut vm, g, MODE_NEXT, Value::UNDEFINED), (n(0), false));
    let next = prop(&mut vm, g, "next");
    // `call_function` runs `next` on a pooled machine: the frame is pushed on that machine's stack.
    let r = vm.realm.call_function(next, g, &[n(3)]).unwrap();
    assert_eq!(prop(&mut vm, r, "value"), n(1));
    assert_eq!(prop(&mut vm, r, "done"), Value::FALSE);
    assert!(!vm.realm.pool.is_empty());
    // The host's own stack resumes it again: acc was 103 and goes on from there.
    assert_eq!(step(&mut vm, g, MODE_NEXT, n(4)), (n(2), false));
    assert_eq!(step(&mut vm, g, MODE_NEXT, n(5)), (n(3), false));
    assert_eq!(step(&mut vm, g, MODE_NEXT, n(6)), (n(118), true));
}

#[test]
fn a_resume_that_grows_the_stack_of_a_pooled_machine_makes_the_register_pointer_again() {
    // The frame of the generator needs more words than the pooled machine has when the `valueOf` calls `next`: `gen_enter` grows the stack
    // (the loop's `regs` points into the old buffer) and the loop makes `regs` again, as for a call. A stale pointer reads and writes freed memory.
    let sh = shared();
    for regs in [8, 600, 5000] {
        let mut vm = Vm::new(Program { codes: vec![main_pooled(&sh), gen_acc_regs(&sh, regs), value_of(&sh)] }, sh.clone());
        assert_eq!(vm.run_main(0).map(|v| v.as_f64()), Ok(Some(22.0)), "{regs} registers");
        let big = vm.realm.pool.iter().map(|m| m.stack.len()).max().unwrap();
        assert!(regs < 600 || big > regs as usize, "the pooled machine grew to {big} words for a frame of {regs}");
    }
}

// ---- collection ----

fn small_heap(vm: &mut Vm) {
    // A collection at every gate: the heap is always past the limit.
    vm.realm.heap.set_trigger(Trigger { min: 1, slack: 1 });
}

/// `function* (n) { var i = 0, last = {}; for (; i < n; i++) { var o = { i }; last.last = i; yield o; } return last.last; }`: a fresh object is
/// yielded, and `last` (a heap object in a saved register) must survive each suspension.
fn gen_objects(sh: &Arc<SharedAtoms>) -> Code {
    let mut g = F::new(sh, 8, 1);
    let (head, done) = (g.label(), g.label());
    g.gen_init(&[0]);
    g.li(1, 0);
    g.e(Op::NewObject, 2, 0, 0);
    g.bind(head);
    g.j(Op::JGe, 1, 0, done);
    g.e(Op::NewObject, 5, 0, 0);
    g.set(5, 1, "i");
    g.set(2, 1, "last");
    g.yield_(5, 6, 7, &[0, 1, 2]);
    g.e(Op::Inc, 1, 1, 0);
    g.j(Op::JmpBack, 0, 0, head);
    g.bind(done);
    g.get(3, 2, "last");
    g.e(Op::RetGen, 3, 0, 0);
    g.build()
}

#[test]
fn a_collection_at_every_suspend_and_resume_keeps_the_saved_frame() {
    let sh = shared();
    let rounds = if MIRI { 6 } else { 40 };
    let (mut vm, g) = host(vec![make(&sh, rounds), gen_objects(&sh)], &sh);
    small_heap(&mut vm);
    let before = vm.realm.collections;
    for i in 0..rounds {
        let (o, done) = step(&mut vm, g, MODE_NEXT, Value::UNDEFINED);
        assert!(!done);
        assert_eq!(prop(&mut vm, o, "i"), n(i));
        // The generator is suspended: only its activation holds the frame. Collect, then resume.
        vm.collect_idle();
    }
    assert_eq!(step(&mut vm, g, MODE_NEXT, Value::UNDEFINED), (n(rounds - 1), true));
    assert!(vm.realm.collections >= before + 2 * rounds as u32, "{} collections for {rounds} rounds", vm.realm.collections - before);
}

#[test]
fn a_collection_at_every_gate_of_a_script_loop_keeps_the_saved_frame() {
    let sh = shared();
    // `main_next` reads `.value` as a number: use a main that reads `.value.i`.
    let mut m = F::new(&sh, 14, 0);
    let (head, done) = (m.label(), m.label());
    let rounds = if MIRI { 6 } else { 40 };
    m.funcs = vec![1];
    m.e(Op::Closure, 0, 0, 0);
    m.e(Op::Mov, 3, 0, 0);
    m.li(4, rounds);
    m.e(Op::CallU, 5, 3, 1);
    m.li(6, 0);
    m.li(7, 0);
    m.get(2, 5, "next");
    m.bind(head);
    m.ji(Op::JnLtI, 7, rounds, done);
    m.e(Op::Mov, 8, 2, 0);
    m.e(Op::Mov, 9, 5, 0);
    m.e(Op::Call, 10, 8, 0);
    m.get(11, 10, "value");
    m.get(12, 11, "i");
    m.e(Op::Add, 6, 6, 12);
    m.e(Op::Inc, 7, 7, 0);
    m.j(Op::JmpBack, 0, 0, head);
    m.bind(done);
    m.e(Op::Ret, 6, 0, 0);
    let mut vm = Vm::new(Program { codes: vec![m.build(), gen_objects(&sh)] }, sh.clone());
    small_heap(&mut vm);
    assert_eq!(vm.run_main(0), Ok(n(rounds * (rounds - 1) / 2)));
    assert!(vm.realm.collections > rounds as u32, "{} collections", vm.realm.collections);
}

#[test]
fn a_collection_while_a_generator_runs_on_a_pooled_machine_is_held_until_it_returns() {
    // The nested run holds the collector (`call_function`), and the outer gate collects after it: the generator frame that was pushed on
    // the pooled machine is gone by then, and the saved record is what the activation holds.
    let sh = shared();
    let mut vm = Vm::new(Program { codes: vec![main_pooled(&sh), gen_acc(&sh), value_of(&sh)] }, sh.clone());
    small_heap(&mut vm);
    assert_eq!(vm.run_main(0).map(|v| v.as_f64()), Ok(Some(22.0)));
    assert!(vm.realm.collections > 0);
}

// ---- the compile of a function that waits for its first call ----

#[test]
fn a_generator_that_waits_as_a_stub_is_compiled_at_its_first_call() {
    use crate::bytecode::encode;
    use crate::codes::Codes;
    use crate::interp::{LazyCompile, code_flags};
    struct Hand(Option<Code>);
    impl LazyCompile for Hand {
        fn compile(&mut self, codes: &Codes, ci: usize) -> Result<(), String> {
            let mut c = self.0.take().ok_or("compiled already")?;
            c.flags = code_flags(&c);
            codes.set(ci, c);
            Ok(())
        }
    }
    let sh = shared();
    // The stub of a function with one plain parameter: the arguments are in the registers when it runs (`script.rs`, `stub`).
    let mut stub = Code::new(1, 1);
    stub.fn_len = 1;
    let mut w = Vec::new();
    encode(&mut w, &Ins::new(Op::Lazy, 0, 0, 0)).unwrap();
    stub.words = w;
    let mut vm = Vm::new(Program { codes: vec![main_next(&sh, 6), stub] }, sh.clone());
    vm.realm.lazy = Some(Box::new(Hand(Some(gen_acc(&sh)))));
    assert_eq!(vm.run_main(0), Ok(n(15)));
    assert!(vm.realm.prog[1].gen_kind != crate::bytecode::GK_NONE, "the stub was replaced by the generator code");
}

// ---- the verifier ----

fn bad(c: &Code, what: &str) {
    let e = verify(c).expect_err("the verifier accepted bad code");
    assert!(e.contains(what), "wanted {what:?}, got {e:?}");
}

#[test]
fn a_register_that_is_read_after_a_yield_must_be_in_the_save_list() {
    let sh = shared();
    let mut g = F::new(&sh, 6, 1);
    g.gen_init(&[0]);
    g.li(1, 5);
    g.yield_(0, 3, 4, &[0]);
    g.e(Op::Add, 2, 1, 1);
    g.e(Op::RetGen, 2, 0, 0);
    bad(&g.build_unchecked(), "r1");
    // With r1 in the list it is accepted.
    let mut g = F::new(&sh, 6, 1);
    g.gen_init(&[0]);
    g.li(1, 5);
    g.yield_(0, 3, 4, &[0, 1]);
    g.e(Op::Add, 2, 1, 1);
    g.e(Op::RetGen, 2, 0, 0);
    verify(&g.build_unchecked()).unwrap();
}

#[test]
fn the_sent_value_and_the_mode_are_written_by_the_resume_and_nothing_else_is() {
    let sh = shared();
    // r3 (sent) and r4 (mode) may be read after the yield; r2 (written before, not in the list) may not.
    let mut g = F::new(&sh, 6, 1);
    g.gen_init(&[0]);
    g.li(2, 5);
    g.yield_(0, 3, 4, &[0]);
    g.e(Op::Add, 5, 3, 4);
    g.e(Op::RetGen, 5, 0, 0);
    verify(&g.build_unchecked()).unwrap();
    let mut g = F::new(&sh, 6, 1);
    g.gen_init(&[0]);
    g.li(2, 5);
    g.yield_(0, 3, 4, &[0]);
    g.e(Op::RetGen, 2, 0, 0);
    bad(&g.build_unchecked(), "r2");
}

#[test]
fn a_save_list_names_only_registers_that_every_path_has_written() {
    let sh = shared();
    let mut g = F::new(&sh, 6, 1);
    g.gen_init(&[0]);
    g.yield_(0, 3, 4, &[0, 2]);
    g.e(Op::RetGen, 0, 0, 0);
    bad(&g.build_unchecked(), "r2");
}

#[test]
fn a_handler_around_a_yield_gets_only_the_registers_of_the_save_list() {
    let sh = shared();
    for (listed, ok) in [(&[0u32, 1][..], true), (&[0u32][..], false)] {
        let mut g = F::new(&sh, 6, 1);
        let handler = g.label();
        g.gen_init(&[0]);
        g.li(1, 5);
        let y = g.yield_(0, 3, 4, listed);
        g.e(Op::RetGen, 3, 0, 0);
        g.bind(handler);
        // The handler reads r1, which was written before the yield and is in the list only the first time.
        g.e(Op::RetGen, 1, 0, 0);
        g.handlers.push((y, y + 2, handler, 5, 0));
        let c = g.build_unchecked();
        if ok {
            verify(&c).unwrap();
        } else {
            bad(&c, "r1");
        }
    }
}

#[test]
fn a_save_list_that_is_not_in_the_root_map_is_rejected() {
    let sh = shared();
    let mut g = F::new(&sh, 6, 1);
    g.gen_init(&[0]);
    g.li(1, 5);
    g.yield_(0, 3, 4, &[0, 1]);
    g.e(Op::RetGen, 1, 0, 0);
    let mut c = g.build_unchecked();
    verify(&c).unwrap();
    // A map that leaves r1 out: a collection before the copy would free what r1 holds.
    c.roots = RootMaps::new(c.nregs as usize);
    let mut pc = 0;
    while pc < c.words.len() {
        let (i, len) = decode(&c.words, pc).unwrap();
        if i.op.is_safepoint() {
            c.roots.add(pc as u32, [0usize]);
        }
        pc += len;
    }
    bad(&c, "root map");
}

#[test]
fn the_save_list_is_checked_for_its_pool_range_and_length() {
    let sh = shared();
    let good = || {
        let mut g = F::new(&sh, 6, 1);
        g.gen_init(&[0]);
        g.yield_(0, 3, 4, &[0]);
        g.e(Op::RetGen, 0, 0, 0);
        g.build_unchecked()
    };
    verify(&good()).unwrap();
    // The second list starts at 4 (the first one has sent, mode, len and 1 register).
    let mut c = good();
    c.tables[4 + 2] = 9;
    bad(&c, "save list");
    let mut c = good();
    c.tables[4] = 6;
    bad(&c, "register out of range");
    let mut c = good();
    c.tables[4 + 1] = c.tables[4];
    bad(&c, "twice");
    let mut c = good();
    c.gen_regs = 0;
    bad(&c, "gen_regs");
}

#[test]
fn the_ops_of_a_generator_are_only_for_generator_code() {
    let sh = shared();
    let mut g = F::new(&sh, 6, 1);
    g.gen_init(&[0]);
    g.e(Op::RetGen, 0, 0, 0);
    let mut c = g.build_unchecked();
    verify(&c).unwrap();
    c.gen_kind = crate::bytecode::GK_NONE;
    bad(&c, "invalid suspension metadata");
    // With no save registers either, the verifier reaches the ops.
    c.gen_regs = 0;
    bad(&c, "not one");
    // A generator that a script can construct, and the ops that a generator may not use.
    let mut c = {
        let mut g = F::new(&sh, 6, 1);
        g.gen_init(&[0]);
        g.e(Op::RetGen, 0, 0, 0);
        g.build_unchecked()
    };
    c.noctor = false;
    bad(&c, "construct");
    for op in [Op::Ret, Op::RetUndef, Op::LoadNewTarget] {
        let mut g = F::new(&sh, 6, 1);
        g.gen_init(&[0]);
        g.e(op, 0, 0, 0);
        g.e(Op::RetGen, 0, 0, 0);
        bad(&g.build_unchecked(), "generator code");
    }
}
