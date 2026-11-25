// Tests of `WeakRef` and `FinalizationRegistry` (`bi_weakref.rs`).
use crate::heap::Trigger;
use crate::script::{load, run};

fn ev(src: &str, name: &str) -> String {
    match run(src) {
        Ok(mut vm) => vm.global_text(name),
        Err(e) => panic!("{e}\n--- source:\n{src}"),
    }
}

/// Run `src`, collect once, and read the global `name`.
fn after_gc(src: &str, name: &str) -> (String, crate::gc::Stats) {
    let mut vm = load(src).unwrap_or_else(|e| panic!("{e}\n--- source:\n{src}"));
    vm.realm.heap.set_trigger(Trigger { min: usize::MAX, slack: 0 });
    if let Err(e) = vm.run_main(0) {
        panic!("uncaught {e:?}\n--- source:\n{src}");
    }
    vm.collect_idle();
    (vm.global_text(name), vm.realm.last_gc)
}

#[test]
fn deref_returns_a_live_target() {
    assert_eq!(ev("var o = {a: 1}, w = new WeakRef(o); var r = [w.deref() === o, w.deref().a].join();", "r"), "true,1");
    let (r, _) = after_gc("var o = {}, w = new WeakRef(o); var r = String(w.deref() === o);", "r");
    assert_eq!(r, "true");
}

#[test]
fn deref_is_undefined_after_the_target_dies() {
    let mut vm = load("var w, k = {}, w2 = new WeakRef(k); (function () { w = new WeakRef({ big: [1, 2, 3] }); })(); function chk() { return [w.deref(), w2.deref() === k].join(); }").unwrap();
    vm.realm.heap.set_trigger(Trigger { min: usize::MAX, slack: 0 });
    vm.run_main(0).unwrap();
    vm.collect_idle();
    assert_eq!(vm.realm.last_gc.cleared, 1, "{:?}", vm.realm.last_gc);
    let f = vm.global("chk");
    let g = vm.realm.global.value();
    let r = vm.call(f, g, &[]).unwrap();
    assert_eq!(vm.display(r, false), ",true");
}

#[test]
fn weakref_errors_and_shape() {
    let src = "var r = [typeof WeakRef, WeakRef.length, WeakRef.name, WeakRef.prototype[Symbol.toStringTag], Object.prototype.toString.call(new WeakRef({})),\
        (function () { try { WeakRef({}); } catch (e) { return e.constructor.name; } })(),\
        (function () { try { new WeakRef(1); } catch (e) { return e.constructor.name; } })(),\
        (function () { try { new WeakRef(); } catch (e) { return e.constructor.name; } })(),\
        (function () { try { WeakRef.prototype.deref.call({}); } catch (e) { return e.constructor.name; } })(),\
        new WeakRef({}) instanceof WeakRef].join();";
    assert_eq!(ev(src, "r"), "function,1,WeakRef,WeakRef,[object WeakRef],TypeError,TypeError,TypeError,TypeError,true");
}

#[test]
fn a_subclass_keeps_its_prototype() {
    assert_eq!(ev("class W extends WeakRef {} var o = {}, w = new W(o); var r = [w instanceof W, w.deref() === o].join();", "r"), "true,true");
}

#[test]
fn registry_shape_and_errors() {
    let t = |body: &str| format!("(function () {{ try {{ {body} }} catch (e) {{ return e.constructor.name; }} return 'ok'; }})()");
    let src = format!(
        "var f = new FinalizationRegistry(function () {{}}); var r = [typeof FinalizationRegistry, FinalizationRegistry.length, FinalizationRegistry.prototype[Symbol.toStringTag], \
         Object.prototype.toString.call(f), {}, {}, {}, {}, {}, {}, {}, {}].join();",
        t("FinalizationRegistry(function () {});"),
        t("new FinalizationRegistry(1);"),
        t("f.register(1, 2);"),
        t("var o = {}; f.register(o, o);"),
        t("f.register({}, 1, 5);"),
        t("f.unregister(5);"),
        t("FinalizationRegistry.prototype.register.call({}, {}, 1);"),
        t("f.register({}, undefined, undefined);"),
    );
    assert_eq!(ev(&src, "r"), "function,1,FinalizationRegistry,[object FinalizationRegistry],TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,ok");
}

#[test]
fn register_and_unregister_return_values() {
    let src = "var f = new FinalizationRegistry(function () {}), tok = {}, tok2 = {}, a = {}, b = {};\
        var r = [f.register(a, 1, tok), f.register(b, 2, tok), f.register(a, 3), f.unregister(tok), f.unregister(tok), f.unregister(tok2)].join();";
    assert_eq!(ev(src, "r"), ",,,true,false,false");
}

#[test]
fn registrations_do_not_keep_targets_or_tokens() {
    // 100 targets and 100 tokens, all dropped: nothing stays alive but the registry (its table entries are cleared).
    let (_, st) = after_gc(
        "var f = new FinalizationRegistry(function () {}); (function () { for (var i = 0; i < 100; i++) f.register({ n: i }, { h: i }, { t: i }); })(); var r = 1;",
        "r",
    );
    assert_eq!(st.cleared, 100, "{st:?}");
    // A token that is alive keeps its entry.
    let (_, st) = after_gc("var f = new FinalizationRegistry(function () {}), ks = []; for (var i = 0; i < 20; i++) { ks[i] = {}; f.register({}, 1, ks[i]); } var r = 1;", "r");
    assert_eq!(st.cleared, 0, "{st:?}");
}
