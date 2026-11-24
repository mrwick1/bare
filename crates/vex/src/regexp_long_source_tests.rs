use crate::script::run;

#[test]
fn long_alternation_compiles_and_matches() {
    // About 90,000 units: an alternation of 4,174 escaped words, as a page builds to match a word list.
    let mut vm = run(r"
        var names = [];
        for (var i = 0; i < 4174; i++) names.push('Name\\.' + i + ' \\(x\\)-tool');
        var src = '(^|[^\\p{L}\\p{N}_-])(' + names.join('|') + ')(?=$|[^\\p{L}\\p{N}_-])';
        var re = new RegExp(src, 'iu');
        var ok = src.length > 65536 && re.test(' name.4173 (x)-tool ') && !re.test('name.4174 (x)-tool')
            && new RegExp(names.join('|'), 'i').test('NAME.4000 (X)-TOOL');
    ").unwrap();
    assert_eq!(vm.global_text("ok"), "true");
}
