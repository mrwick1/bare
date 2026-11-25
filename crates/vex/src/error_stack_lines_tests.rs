// Tests of the line and column of a stack frame (`error_stack::Lines`): the column counts UTF-16 units, as in V8, also after text that is
// not ASCII, and the line index of a script gives the same answer on each read.
use crate::script::run;

fn frame(src: &str) -> String {
    match run(src) {
        Ok(mut vm) => vm.global_text("r"),
        Err(e) => panic!("{e}\n--- source:\n{src}"),
    }
}

#[test]
fn the_column_counts_utf16_units_after_text_that_is_not_ascii() {
    // `var s = '` is 9 units, `é` 1, the emoji 2, `'; var e = ` 11: `new` is at column 24.
    let src = "var s = 'é😀'; var e = new Error('t');\nvar r = e.stack.split('\\n')[1].replace(/^\\s+/, '');";
    assert_eq!(frame(src), "at <anonymous>:1:24");
}

#[test]
fn the_line_counts_after_lines_that_are_not_ascii() {
    let src = "var a = 'ü';\nvar b = '日本';\nfunction f() { return new Error('t'); }\nvar r = f().stack.split('\\n').slice(1, 3).map(function (l) { return l.trim() }).join('|');";
    assert_eq!(frame(src), "at f (<anonymous>:3:23)|at <anonymous>:4:9");
}

#[test]
fn many_reads_in_one_long_line_give_the_same_frames() {
    let pad = "x".repeat(50_000);
    let src = format!("var p = '{pad}'; function f(i) {{ return new Error('t' + i); }} var s = {{}}; for (var i = 0; i < 50; i++) s[f(i).stack.split('\\n')[1]] = 1; var r = Object.keys(s).join('|');");
    // 50 errors, one distinct frame: the column of `new` in the one line.
    assert_eq!(frame(&src), format!("    at f (<anonymous>:1:{})", src.find("new Error").unwrap() + 1));
}

#[test]
fn the_column_after_a_long_line_that_is_not_ascii_counts_every_unit() {
    // Each checkpoint of the index is about 1 KiB apart: the lines cross many of them, with 1-, 2-, 3- and 4-byte characters.
    let src: String = (0..40).map(|i| format!("{}\n", "aé日😀".repeat(100 + i * 37))).collect();
    let lines = crate::error_stack::Lines::new(&src);
    for (off, _) in src.char_indices().step_by(97) {
        let start = src[..off].rfind('\n').map_or(0, |n| n + 1);
        let line = src[..off].matches('\n').count() + 1;
        assert_eq!(lines.get(&src, off), (line, src[start..off].encode_utf16().count() + 1), "byte {off}");
    }
}
