use super::*;
use crate::script::load;

#[test]
fn two_realms_run_the_same_code_with_distinct_bindings() {
    let source = format!(
        "/*{}*/var output = new (class extends class {{ word() {{ return 'base'; }} }} {{
             word() {{ return super.word() + '雪'; }}
         }})().word(); function make(x) {{ return () => x + output; }} var saved = make('realm:');",
        " ".repeat(MIN_SOURCE)
    );
    let mut a = load("var first = 0; function firstFn() {}").unwrap();
    let mut b = load("var other = 0; function a() {} function b() {}").unwrap();
    // The first sight is not kept (admission on the second sight), so a third realm compiles it first.
    let mut seed = load("").unwrap();
    for vm in [&mut seed, &mut a, &mut b] {
        let code = vm.realm.script_code(&source, ScriptMode::default()).unwrap();
        vm.realm.run_code(code, false).unwrap();
        assert_eq!(vm.global_text("output"), "base雪");
    }
    assert_eq!((counts().misses, counts().hits), (2, 1));
    b.realm.raw_collect(&[]);
    clear();
    drop(b);
    let code = a.realm.script_code("var checked = saved();", ScriptMode::default()).unwrap();
    a.realm.run_code(code, false).unwrap();
    assert_eq!(a.global_text("checked"), "realm:base雪");
}

#[test]
fn a_hash_collision_cannot_return_the_wrong_source() {
    let atoms = SharedAtoms::default();
    let limits = Limits::default();
    let mode = Mode::Script(ScriptMode::default());
    let source = "var name = 1;";
    let key = (Identity::of(source), mode);
    let code = crate::script::compile_unit(source, mode, limits, &atoms).unwrap();
    let unit = Rc::new(PortableUnit::capture(Rc::from(source), &code));
    let mut cache = Cache::default();
    cache.insert(key, unit.clone(), unit.bytes(), MAX_BYTES);
    assert!(cache.get(key, "var name = 2;").is_none());
    assert!(Rc::ptr_eq(&cache.get(key, source).unwrap(), &unit));
    assert_eq!((cache.counts.misses, cache.counts.hits), (1, 1));
}

#[test]
fn cached_classic_and_module_code_use_separate_entries() {
    use bare_js_api::Engine;

    clear();
    let source = format!("/*{}*/globalThis.output = typeof this;", " ".repeat(MIN_SOURCE));
    for i in 0..3 {
        let mut vm = load("").unwrap();
        let code = vm.realm.script_code(&source, ScriptMode::default()).unwrap();
        vm.realm.run_code(code, false).unwrap();
        assert_eq!(vm.global_text("output"), "object");
        let id = Engine::module_parse(&mut vm.realm, &source, &format!("fixture{i}.js")).unwrap();
        Engine::module_link(&mut vm.realm, id).unwrap();
        Engine::module_evaluate(&mut vm.realm, id).unwrap();
        assert_eq!(vm.global_text("output"), "undefined");
    }
    assert_eq!((counts().misses, counts().hits, counts().entries), (4, 2, 2));
}

#[test]
fn a_hit_rebinds_strings_in_constant_trees() {
    use crate::script::compile_unit;

    clear();
    let source = format!(
        r#"var result = {{'雪': ['\uD800', '{}']}}; var checked = result['雪'][0].charCodeAt(0);"#,
        "b".repeat(2048)
    );
    let mode = ScriptMode::default();
    let atoms = SharedAtoms::default();
    let baseline = compile_unit(&source, Mode::Script(mode), Limits::default(), &atoms).unwrap();
    assert!(!baseline.codes[0].data.is_empty());
    for _ in 0..3 {
        let mut vm = load("").unwrap();
        let code = vm.realm.script_code(&source, mode).unwrap();
        vm.realm.run_code(code, false).unwrap();
        assert_eq!(vm.global_text("checked"), "55296");
    }
    assert_eq!((counts().misses, counts().hits), (2, 1));
}

#[test]
fn byte_and_age_eviction_keep_the_most_recent_entry() {
    let atoms = SharedAtoms::default();
    let limits = Limits::default();
    let mode = Mode::Script(ScriptMode::default());
    let make = |source: &'static str| {
        let code = crate::script::compile_unit(source, mode, limits, &atoms).unwrap();
        let unit = Rc::new(PortableUnit::capture(Rc::from(source), &code));
        ((Identity::of(source), mode), unit)
    };
    let (a, b, c) = ("var x = 1;", "var x = 2;", "var x = 3;");
    let ((ka, ua), (kb, ub), (kc, uc)) = (make(a), make(b), make(c));
    let bytes = ua.bytes();
    assert_eq!((ub.bytes(), uc.bytes()), (bytes, bytes));
    let mut cache = Cache::default();
    cache.insert(ka, ua, bytes, bytes * 2);
    cache.insert(kb, ub, bytes, bytes * 2);
    cache.new_realm();
    assert!(cache.get(ka, a).is_some());
    cache.insert(kc, uc.clone(), bytes, bytes * 2);
    assert!(!cache.entries.contains_key(&kb));
    assert_eq!(cache.counts.bytes, bytes * 2);
    for _ in 0..MAX_IDLE {
        cache.new_realm();
    }
    assert_eq!(cache.entries.len(), 2);
    assert!(cache.get(kc, c).is_some());
    cache.new_realm();
    assert!(!cache.entries.contains_key(&ka));
    assert_eq!(cache.counts.bytes, bytes);
    cache.insert(ka, uc, bytes, bytes - 1);
    assert!(!cache.entries.contains_key(&ka));
    assert_eq!(cache.counts.bytes, bytes);
}
