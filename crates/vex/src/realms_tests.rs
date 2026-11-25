use crate::script;

const HOST: &str = include_str!("../examples/test262_host.inc.js");

#[test]
fn foreign_calls_and_throws_restore_the_caller_realm() {
    let mut vm = script::load_with(HOST, r#"
        var foreign = $262.createRealm();
        var other = foreign.global;
        var saved = globalThis;
        var read = foreign.evalScript('let captured = 13; (() => [globalThis, captured])');
        var fail = foreign.evalScript('(() => { throw new TypeError("foreign"); })');
        var result = read();
        var matches;
        try { fail(); } catch (error) { matches = error instanceof other.TypeError && !(error instanceof TypeError); }
        var ok = result[0] === other && result[1] === 13 && result instanceof other.Array &&
            matches && globalThis === saved && foreign.evalScript('captured') === 13 &&
            new other.Map() instanceof other.Map && !(new other.Map() instanceof Map) &&
            other.Symbol.iterator === Symbol.iterator && Symbol.for('x') === other.Symbol.for('x');
    "#).unwrap();
    vm.realm.install_test262_realms(HOST).unwrap();
    vm.run_main(0).unwrap();
    assert_eq!(vm.global("ok").as_bool(), Some(true));
    vm.collect_idle();
    let foreign = vm.global("foreign");
    let result = vm.realm.get_named(foreign, "evalScript").unwrap();
    let source = vm.realm.string("captured");
    let answer = vm.realm.call_function(result, crate::value::Value::UNDEFINED, &[source]).unwrap();
    assert_eq!(answer.as_int(), Some(13));
}

#[test]
fn new_target_follows_bound_and_proxy_targets_to_the_intrinsic() {
    let mut vm = script::load_with(HOST, r#"
        var other = $262.createRealm().global;
        var foreign = new other.Function('return globalThis');
        var bound = foreign.bind(null);
        var target = new Proxy(bound, {get(o, k) { return k === 'prototype' ? null : Reflect.get(o, k); }});
        var buffer = Reflect.construct(ArrayBuffer, [2], target);
        var view = Reflect.construct(DataView, [buffer], target);
        var ok = bound() === other && Object.getPrototypeOf(buffer) === other.ArrayBuffer.prototype &&
            Object.getPrototypeOf(view) === other.DataView.prototype;
    "#).unwrap();
    vm.realm.install_test262_realms(HOST).unwrap();
    vm.run_main(0).unwrap();
    assert_eq!(vm.global("ok").as_bool(), Some(true));
}
