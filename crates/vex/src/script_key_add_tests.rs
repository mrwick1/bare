use super::tests::check;

#[test]
fn computed_members_select_key_addition_only_for_keys() {
    let (script, _) = crate::compile::compile_all(
        "var o = {}, k = 'k', n = 42; o[k + n] = 7; var a = o[n + k]; var s = k + n;",
        &crate::atoms::SharedAtoms::default(),
        Default::default(),
    )
    .unwrap();
    crate::verify::verify(&script.code).unwrap();
    let text = script.code.disassemble();
    assert_eq!(text.matches("AddKey ").count(), 2, "{text}");
    assert_eq!(text.matches("Add ").count(), 1, "{text}");
}

#[test]
fn computed_addition_preserves_key_kind_and_coercion() {
    check(&[
        ("var a = []; a['1' + 2] = 'x'; a['0' + 2] = 'y'; a[4 + '294967295'] = 'z'; var r = a.length + ':' + a[12] + ':' + a['02'] + ':' + a[4294967295];", "r", "13:x:y:z"),
        ("var o = {}; o['é' + 3] = 1; o[3 + '猫'] = 2; o['long long long long long long long long long long ' + 3] = 3; var r = o['é3'] + o['3猫'] + o['long long long long long long long long long long 3'];", "r", "6"),
        ("var o = {}; o[1 + 2] = 'a'; o['s' + -2147483648] = 'b'; o[2n + 'x'] = 'c'; o['x' + -0] = 'd'; var r = o[3] + o['s-2147483648'] + o['2x'] + o.x0;", "r", "abcd"),
        ("var o = {}, log = ''; function a() { log += 'a'; return o; } function b() { log += 'b'; return {valueOf() { log += 'c'; return 'k'; }}; } function d() { log += 'd'; return 2; } function e() { log += 'e'; return 7; } a()[b() + d()] = e(); var r = log + ':' + o.k2;", "r", "abdce:7"),
        ("var o = {}, log = ''; try { o['k' + Symbol()] = (log = 'rhs'); } catch (e) { log += e.name; } var r = log;", "r", "TypeError"),
        ("var o = {}; var key = 'k'; for (var i = 0; i < 1000; i++) { o[key + (i % 7)] = i; } var r = o[key + 5] + ':' + o[key + 6];", "r", "999:993"),
    ]);
}

#[test]
#[cfg_attr(miri, ignore)]
fn repeated_computed_key_addition_needs_no_new_string_blocks() {
    use crate::interp_rt::Key;
    use crate::value::Value;

    let mut vm = super::run("var o = {}; function write(n) { for (var i = 0; i < n; i++) o['key' + (i % 7)] = i; } write(32);").unwrap();
    let key = Key::Name(vm.realm.intern("write"));
    let write = vm.realm.get_prop(vm.realm.global.value(), key).unwrap();
    let before = vm.realm.heap.grown_bytes();
    vm.realm.call_function(write, Value::UNDEFINED, &[Value::int(8192)]).unwrap();
    assert_eq!(vm.realm.heap.grown_bytes(), before);
}
