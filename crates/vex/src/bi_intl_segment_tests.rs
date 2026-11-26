use crate::script::run;

fn check(source: &str, expected: &str) {
    let mut vm = run(source).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(vm.global_text("r"), expected);
}

#[test]
fn segment_splits_graphemes_and_iterates() {
    check(
        r#"
        var g = new Intl.Segmenter(void 0, {granularity: 'grapheme'});
        var parts = Array.from(g.segment('aé👩‍💻'));
        var r = [parts.length, parts[1].segment, parts[1].index, parts[2].index, parts[0].input, 'isWordLike' in parts[0]].join('|');
        "#,
        "3|e\u{301}|1|3|ae\u{301}👩\u{200d}💻|false",
    );
}

#[test]
fn segment_gives_words_and_containing() {
    check(
        r#"
        var s = new Intl.Segmenter('en', {granularity: 'word'}).segment('Hi, you');
        var words = [];
        for (var x of s) if (x.isWordLike) words.push(x.segment);
        var c = s.containing(5);
        var r = [words.join(','), c.segment, c.index, s.containing(99) === undefined, s.containing(-1) === undefined].join('|');
        "#,
        "Hi,you|you|4|true|true",
    );
}

#[test]
fn segment_needs_a_segmenter() {
    check(
        r#"
        var r;
        try { Intl.Segmenter.prototype.segment.call({}, 'x'); r = 'no throw'; } catch (e) { r = e.constructor.name; }
        "#,
        "TypeError",
    );
}
