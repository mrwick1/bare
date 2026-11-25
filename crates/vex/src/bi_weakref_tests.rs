// Tests of `WeakRef` and `FinalizationRegistry` (`bi_weakref.rs`). A "job" is one call from the host; `end_job` is the end of the microtask
// checkpoint, where the KeepDuringJob list is cleared. `gc` ends the job and collects with the stack empty.
use crate::heap::Trigger;
use crate::interp::Vm;
use crate::script::{eval_program, load, run};
use crate::value::Value;

/// A program, run once, with no collection on its own.
fn start(src: &str) -> Vm {
    let mut vm = load(src).unwrap_or_else(|e| panic!("{e}\n--- source:\n{src}"));
    vm.realm.heap.set_trigger(Trigger { min: usize::MAX, slack: 0 });
    if let Err(e) = vm.run_main(0) {
        panic!("uncaught {e:?}\n--- source:\n{src}");
    }
    vm
}

/// Call the global function `name` as one job and show its result.
fn job(vm: &mut Vm, name: &str) -> String {
    let f = vm.global(name);
    let r = vm.call(f, Value::UNDEFINED, &[]).unwrap_or_else(|e| panic!("{name} threw {e:?}"));
    vm.display(r, false)
}

fn end_job(vm: &mut Vm) {
    vm.drain_microtasks(&mut |_, _| {}).expect("drain");
}

fn gc(vm: &mut Vm) {
    end_job(vm);
    vm.collect_idle();
}

/// Run the cleanup callbacks that are due; the number that ran.
fn cleanup(vm: &mut Vm) -> u32 {
    vm.run_finalizers(&mut |_, _| panic!("a cleanup callback threw")).expect("cleanup")
}

fn kept(vm: &Vm) -> usize {
    vm.realm.wr.as_ref().map_or(0, |w| w.kept.len())
}

fn eval(src: &str) -> String {
    let mut vm = run(&eval_program(src)).unwrap_or_else(|e| panic!("{e}\n--- source:\n{src}"));
    vm.global_text("__result")
}

#[test]
fn a_ref_gives_its_target_until_the_target_is_collected() {
    let mut vm = start("var w, keep = {}, wk; function mk() { w = new WeakRef({ n: 1 }); wk = new WeakRef(keep); } function look() { return typeof w.deref() + ',' + typeof wk.deref(); }");
    job(&mut vm, "mk");
    end_job(&mut vm);
    assert_eq!(job(&mut vm, "look"), "object,object");
    gc(&mut vm);
    assert_eq!(job(&mut vm, "look"), "undefined,object");
}

#[test]
fn a_target_lives_to_the_end_of_the_job_that_made_or_read_the_ref() {
    // The job allocates enough to collect many times (the trigger is small): the target is only in the ref.
    let src = "var w, r = ''; function mk() { w = new WeakRef({ n: 1 }); } function churn() { var a; for (var i = 0; i < 20000; i++) a = { i: i }; } function once() { mk(); churn(); r += typeof w.deref(); } function again() { churn(); r += ',' + typeof w.deref(); churn(); r += ',' + typeof w.deref(); }";
    let mut vm = load(src).expect("load");
    vm.realm.heap.set_trigger(Trigger { min: 4 << 10, slack: 4 << 10 });
    vm.run_main(0).expect("main");
    job(&mut vm, "once");
    assert!(vm.realm.collections > 2, "the job collected");
    assert_eq!(kept(&vm), 1);
    end_job(&mut vm);
    assert_eq!(kept(&vm), 0);
    // A deref in a new job keeps the target for that job only, and by then the target of this ref is gone when a collection ran between.
    gc(&mut vm);
    job(&mut vm, "again");
    assert_eq!(vm.global_text("r"), "object,undefined,undefined");
}

#[test]
fn a_deref_keeps_the_target_once_for_each_target() {
    let mut vm = start("var o = {}, w = new WeakRef(o); function spin() { for (var i = 0; i < 1000; i++) w.deref(); }");
    job(&mut vm, "spin");
    assert_eq!(kept(&vm), 1);
    end_job(&mut vm);
    assert_eq!(kept(&vm), 0);
}

#[test]
fn holdings_arrive_after_the_target_dies_in_the_order_of_registration() {
    let mut vm = start("var log = []; var f = new FinalizationRegistry(function (h) { log.push(h); }); function mk() { f.register({}, 'a'); f.register({}, 'b'); f.register(keep, 'c'); } var keep = {};");
    job(&mut vm, "mk");
    gc(&mut vm);
    assert!(vm.realm.finalizers_due());
    assert_eq!(vm.global_text("log"), "[]");
    assert_eq!(cleanup(&mut vm), 2);
    assert_eq!(vm.global_text("log"), r#"["a","b"]"#);
    assert!(!vm.realm.finalizers_due());
    assert_eq!(cleanup(&mut vm), 0);
    gc(&mut vm);
    assert_eq!(cleanup(&mut vm), 0);
    assert_eq!(vm.global_text("log"), r#"["a","b"]"#);
}

#[test]
fn unregister_removes_the_cells_of_a_token() {
    let src = "var log = [], t1 = {}, t2 = {}, r = []; var f = new FinalizationRegistry(function (h) { log.push(h); });
        function mk() { f.register({}, 'a', t1); f.register({}, 'b', t1); f.register({}, 'c', t2); f.register({}, 'd'); }
        function un() { r.push(f.unregister(t1), f.unregister(t1), f.unregister({})); }";
    let mut vm = start(src);
    job(&mut vm, "mk");
    job(&mut vm, "un");
    assert_eq!(vm.global_text("r"), "[true,false,false]");
    gc(&mut vm);
    assert_eq!(cleanup(&mut vm), 2);
    assert_eq!(vm.global_text("log"), r#"["c","d"]"#);
}

#[test]
fn unregister_after_the_target_died_still_removes_the_cell() {
    let src = "var log = [], t = {}, r; var f = new FinalizationRegistry(function (h) { log.push(h); }); function mk() { f.register({}, 'a', t); } function un() { r = f.unregister(t); }";
    let mut vm = start(src);
    job(&mut vm, "mk");
    gc(&mut vm);
    assert!(vm.realm.finalizers_due());
    job(&mut vm, "un");
    assert_eq!(vm.global_text("r"), "true");
    assert!(!vm.realm.finalizers_due());
    assert_eq!(cleanup(&mut vm), 0);
    assert_eq!(vm.global_text("log"), "[]");
}

#[test]
fn a_dead_registry_calls_nothing() {
    let mut vm = start("var called = 0; function mk() { var f = new FinalizationRegistry(function () { called++; }); f.register({}, 1); f.register({}, 2); }");
    job(&mut vm, "mk");
    gc(&mut vm);
    assert!(!vm.realm.finalizers_due());
    assert_eq!(cleanup(&mut vm), 0);
    assert_eq!(vm.global_text("called"), "0");
    // The slot of the registry is free: a new registry works.
    let mut vm = start("var log = []; var f = new FinalizationRegistry(function (h) { log.push(h); }); function mk() { f.register({}, 1); }");
    job(&mut vm, "mk");
    gc(&mut vm);
    assert_eq!(cleanup(&mut vm), 1);
}

#[test]
fn a_registry_that_dies_while_a_cleanup_waits_calls_nothing() {
    let mut vm = start("var called = 0, f = new FinalizationRegistry(function () { called++; }); function mk() { f.register({}, 1); } function drop() { f = null; }");
    job(&mut vm, "mk");
    gc(&mut vm);
    assert!(vm.realm.finalizers_due());
    job(&mut vm, "drop");
    gc(&mut vm);
    assert!(!vm.realm.finalizers_due());
    assert_eq!(cleanup(&mut vm), 0);
    assert_eq!(vm.global_text("called"), "0");
}

#[test]
fn a_held_value_that_names_its_target_keeps_it_alive() {
    let mut vm = start("var log = [], f = new FinalizationRegistry(function (h) { log.push('x'); }); function mk() { var o = {}; f.register(o, { back: o }); }");
    job(&mut vm, "mk");
    gc(&mut vm);
    assert!(!vm.realm.finalizers_due());
    assert_eq!(cleanup(&mut vm), 0);
}

#[test]
fn holdings_and_callbacks_live_with_the_registry_and_a_chain_of_registries_follows() {
    // g is only reachable through the holdings of f; its own target dies, so g's callback must still be called with its held value.
    let src = "var log = [], f = new FinalizationRegistry(function (h) { log.push('f' + h); });
        function mk() { var g = new FinalizationRegistry(function (h) { log.push('g' + h.n); }); g.register({}, { n: 7 }); f.register({}, g); }";
    let mut vm = start(src);
    job(&mut vm, "mk");
    gc(&mut vm);
    assert_eq!(cleanup(&mut vm), 2);
    assert_eq!(vm.global_text("log"), r#"["f[object FinalizationRegistry]","g7"]"#);
}

#[test]
fn a_cleanup_that_throws_is_reported_and_the_rest_run() {
    let mut vm = start("var log = []; var f = new FinalizationRegistry(function (h) { log.push(h); if (h == 'a') throw new Error('boom'); }); function mk() { f.register({}, 'a'); f.register({}, 'b'); }");
    job(&mut vm, "mk");
    gc(&mut vm);
    let mut thrown = Vec::new();
    let n = vm.run_finalizers(&mut |_, v| thrown.push(v)).expect("cleanup");
    assert_eq!((n, thrown.len()), (2, 1));
    assert_eq!(vm.global_text("log"), r#"["a","b"]"#);
}

#[test]
fn a_cleanup_can_allocate_and_register_again() {
    let src = "var log = [], f = new FinalizationRegistry(function (h) { log.push(h); if (h < 3) { f.register({}, h + 1); var a = []; for (var i = 0; i < 20000; i++) a.push({ i: i }); } }); function mk() { f.register({}, 1); }";
    let mut vm = load(src).expect("load");
    vm.realm.heap.set_trigger(Trigger { min: 4 << 10, slack: 4 << 10 });
    vm.run_main(0).expect("main");
    job(&mut vm, "mk");
    for _ in 0..3 {
        gc(&mut vm);
        cleanup(&mut vm);
    }
    assert_eq!(vm.global_text("log"), "[1,2,3]");
}

#[test]
fn a_token_that_died_matches_nothing_and_the_cell_stays() {
    let src = "var log = [], r = []; var f = new FinalizationRegistry(function (h) { log.push(h); }); var live = {}; function mk() { f.register(live, 'x', {}); } function un() { r.push(f.unregister({})); }";
    let mut vm = start(src);
    job(&mut vm, "mk");
    gc(&mut vm);
    job(&mut vm, "un");
    assert_eq!(vm.global_text("r"), "[false]");
    assert!(!vm.realm.finalizers_due());
}

#[test]
fn the_oracle_cases_match_node() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/fixtures/js/");
    let cases = std::fs::read_to_string(format!("{dir}iterator-cases.txt")).unwrap();
    let expected = std::fs::read_to_string(format!("{dir}iterator-cases.expected")).unwrap();
    let mut seen = 0;
    for part in cases.split("\n=== ").map(|p| p.strip_prefix("=== ").unwrap_or(p)).skip(0) {
        let (name, src) = part.split_once('\n').unwrap();
        if !(name.starts_with("weakref") || name.starts_with("finalization")) {
            continue;
        }
        let want = expected.lines().find_map(|l| l.strip_prefix(name).and_then(|r| r.strip_prefix('\t'))).unwrap_or_else(|| panic!("no expected line for {name}"));
        let want = want.split_once(':').unwrap().1;
        assert_eq!(eval(src), want, "case {name}");
        seen += 1;
    }
    assert_eq!(seen, 5);
}

#[test]
fn a_subclass_and_the_brand_checks() {
    assert_eq!(eval("class R extends WeakRef {} var o = {}; var r = new R(o); [r.deref() === o, r instanceof WeakRef, r instanceof R].join()"), "true,true,true");
    assert_eq!(eval("class F extends FinalizationRegistry {} var f = new F(function () {}); [f instanceof FinalizationRegistry, f.register({}, 1), f.unregister({})].join()"), "true,,false");
    assert_eq!(eval("var t = WeakRef.prototype.deref; function f() { try { t.call(new FinalizationRegistry(function () {})); return 'no'; } catch (e) { return e instanceof TypeError; } } f()"), "true");
}
