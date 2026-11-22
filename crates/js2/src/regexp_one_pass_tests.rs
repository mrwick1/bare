use super::CACHE;
use crate::interp_rt::Realm;
use crate::regexp::PARSES;
use crate::script::{load, run};

#[test]
fn syntax_errors_precede_matching() {
    for script in [
        r"function unused() { /\a/u; }",
        r"function unused() { /a{2,1}/v; }",
        r"function unused() { /(?<n>a)(?<n>b)/; }",
        r"function unused() { /a/uv; }",
    ] {
        assert!(
            load(script)
                .err()
                .is_some_and(|e| e.starts_with("SyntaxError:")),
            "{script}"
        );
    }
    let mut vm = run(r"
        var errors = 0;
        for (var [src, flags] of [['\\a', 'u'], ['[a&&]', 'v'], ['(?<n>a)(?<n>b)', ''], ['a', 'uv']]) {
            try { new RegExp(src, flags); }
            catch (e) { if (e instanceof SyntaxError) errors++; }
        }
    ").unwrap();
    assert_eq!(vm.global_text("errors"), "4");
}

#[test]
fn first_match_reuses_the_parse_for_literals_and_constructors() {
    PARSES.set(0);
    let mut vm = run(r"
        var a = /(?<word>[a-z])\k<word>./disuy;
        var b = new RegExp('[a&&[a-z]]', 'v');
        var m = a.exec('AA\n');
        var ok = m.groups.word === 'A' && m.indices.groups.word[1] === 1 &&
            a.lastIndex === 3 && b.test('a') && !b.test('b');
    ")
    .unwrap();
    assert_eq!(vm.global_text("ok"), "true");
    assert_eq!(PARSES.get(), 2);
}

#[test]
fn a_live_wrapper_reuses_its_parse_after_cache_eviction_and_collection() {
    PARSES.set(0);
    let mut vm = run(&format!(
        "
        var first = /a+/y;
        for (var i = 0; i < {CACHE}; i++) new RegExp('b' + i);
    "
    ))
    .unwrap();
    vm.collect_idle();
    let first = Realm::obj(vm.global("first")).unwrap();
    assert!(vm.realm.program(first).is_ok());
    assert_eq!(PARSES.get(), CACHE + 1);
}
