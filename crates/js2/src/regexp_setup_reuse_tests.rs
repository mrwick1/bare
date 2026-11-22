use super::*;
use crate::regexp::PARSES;
use crate::script::{load, run};

#[test]
fn literal_checks_and_compiles_once_per_resident_entry() {
    PARSES.set(0);
    CACHE_HITS.set((0, 0));
    let mut vm = run("
        function make() { return /[a-z]+/g; }
        var ok = true, previous;
        for (var i = 0; i < 20; i++) {
            var r = make();
            ok = ok && r !== previous && r.hasOwnProperty('lastIndex') &&
                r.lastIndex === 0 && r.test('ab') && r.lastIndex === 2;
            previous = r;
        }
    ").unwrap();
    assert_eq!(vm.global_text("ok"), "true");
    assert_eq!(PARSES.get(), 1);
    assert_eq!(CACHE_HITS.get(), (19, 0));
}

#[test]
fn equal_source_content_and_program_survive_collection() {
    let mut vm = load("").unwrap();
    PARSES.set(0);
    CACHE_HITS.set((0, 0));
    let src = Str::from_str(&mut vm.realm.heap, "[a-z]+").unwrap();
    let first = vm.realm.new_regexp(None, src, "gi").unwrap();
    let first = vm.realm.program(Realm::obj(first).unwrap()).unwrap().value();
    vm.collect_idle();
    let other = Str::from_str(&mut vm.realm.heap, "[a-z]+").unwrap();
    assert_ne!(src, other);
    let second = vm.realm.new_regexp(None, other, "ig").unwrap();
    let second = vm.realm.program(Realm::obj(second).unwrap()).unwrap().value();
    assert_eq!(PARSES.get(), 1);
    assert_eq!(first, second);
    assert_eq!(CACHE_HITS.get(), (0, 1));
}

#[test]
fn unused_entries_survive_collection_and_stay_bounded() {
    let mut vm = load("").unwrap();
    for i in 0..CACHE + 1 {
        let src = Str::from_str(&mut vm.realm.heap, &format!("pattern{i}")).unwrap();
        vm.realm.new_regexp(None, src, "").unwrap();
    }
    vm.collect_idle();
    PARSES.set(0);
    let src = Str::from_str(&mut vm.realm.heap, "pattern1").unwrap();
    vm.realm.new_regexp(None, src, "").unwrap();
    assert_eq!(PARSES.get(), 0);
    let src = Str::from_str(&mut vm.realm.heap, "pattern0").unwrap();
    vm.realm.new_regexp(None, src, "").unwrap();
    assert_eq!(PARSES.get(), 1);
    assert_eq!(vm.realm.rx().cache.len(), CACHE);
}

#[test]
fn syntax_errors_and_emit_limits_keep_their_timing() {
    let mut vm = run(r"
        var log = '';
        new RegExp('\\a');
        try { new RegExp('\\a', 'u'); } catch (e) { log += e.name + ' '; }
        try { new RegExp('\\a', 'gg'); } catch (e) { log += e.name + ' '; }
        var big = /a{4294967295}/;
        log += 'created ';
        try { big.test('a'); } catch (e) { log += e.name; }
    ").unwrap();
    assert_eq!(vm.global_text("log"), "SyntaxError SyntaxError created SyntaxError");
    assert!(load("var r = /a{2,1}/").is_err());

    // The early literal checker accepts this pattern. The full runtime check enforces its length limit.
    let source = format!("var kind; try {{ /{}/; }} catch (e) {{ kind = e.name; }}",
        "a".repeat(regexp::MAX_PATTERN_UNITS + 1));
    let mut vm = run(&source).unwrap();
    assert_eq!(vm.global_text("kind"), "SyntaxError");
}

#[test]
fn different_flags_use_different_programs() {
    PARSES.set(0);
    let mut vm = run("
        var a = new RegExp('[a-z]+', 'i');
        var b = new RegExp('[a-z]+');
        var ok = a.test('ABC') && !b.test('ABC') && b.test('abc');
    ").unwrap();
    assert_eq!(vm.global_text("ok"), "true");
    assert_eq!(PARSES.get(), 2);
}
