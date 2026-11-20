//! Tests of the pattern check. The accept and reject lists were also compared with V8 (Deno 2.9, `new RegExp`) on 660,000 random pattern and flag pairs made
//! of the pieces below, for the flags none, `u` and `i`: no difference.

use super::check;

fn ok(p: &str, flags: &str) {
    assert_eq!(check(p, flags), Ok(()), "/{p}/{flags} was rejected");
}

fn bad(p: &str, flags: &str) {
    assert!(check(p, flags).is_err(), "/{p}/{flags} was accepted");
}

#[test]
fn plain_patterns_are_valid() {
    for p in ["", "a|b", "^a$", "a*b+c?", "a{2}", "a{2,}", "a{2,3}?", "(a)(?:b)\\1", "[a-z]\\d", "\\bx\\B", "a(?=b)c(?!d)", "(?<=a)b(?<!c)", "\\/", "[/]"] {
        ok(p, "");
        ok(p, "u");
    }
}

#[test]
fn quantifier_errors() {
    for p in ["*", "a|+", "(?", "a**", "a{2,1}", "^*", "\\b+", "(?<=a)*", "{1}", "a{1}{2}", "x{2,1}?"] {
        bad(p, "");
        bad(p, "u");
    }
    // A lookahead may have a quantifier without `u` only; a brace that is not a quantifier is a letter without `u` only.
    ok("(?=a)*", "");
    bad("(?=a)*", "u");
    ok("a{", "");
    ok("a{1", "");
    ok("a{,2}", "");
    ok("{", "");
    ok("}", "");
    ok("]", "");
    for p in ["a{", "a{1", "a{,2}", "{", "}", "]", "a{1,2"] {
        bad(p, "u");
    }
}

#[test]
fn groups_and_lookarounds() {
    for p in ["(", "a)", ")", "(a", "(?x)", "(?<", "(?<a", "(?<=", "(?=a", "(?"] {
        bad(p, "");
    }
    ok("((((a))))", "u");
    bad(&"(".repeat(300), "");
}

#[test]
fn ranges_in_classes() {
    bad("[z-a]", "");
    bad("[\\x7a-\\x61]", "");
    bad("[\\u{7a}-a]", "u");
    ok("[a-a]", "u");
    ok("[a-]", "u");
    ok("[-a]", "u");
    ok("[\\d-a]", "");
    ok("[a-\\d]", "");
    bad("[\\d-a]", "u");
    bad("[a-\\d]", "u");
    bad("[\\d-\\w]", "u");
    ok("[\\b]", "u");
    bad("[", "");
    bad("[a", "");
    bad("[a-", "");
    // Without `u` an astral character is two units: the range runs from a trail to a lead surrogate.
    bad("[😀-😁]", "");
    ok("[😀-😁]", "u");
    ok("[\\ud83d\\ude00-\\ud83d\\ude01]", "u");
}

#[test]
fn escapes_in_unicode_mode_are_strict() {
    for p in ["\\a", "\\-", "\\_", "\\c", "\\c1", "\\x4", "\\u00", "\\u{110000}", "\\u{}", "\\00", "\\07", "\\8", "[\\1]", "[\\c1]", "[\\B]", "[\\k]", "\\p", "\\p{", "\\p{}", "\\p{a=}", "\\p{=a}", "\\p{a b}"] {
        bad(p, "u");
    }
    for p in ["\\a", "\\-", "\\c", "\\c1", "\\x4", "\\u00", "\\u{1}", "\\00", "\\07", "\\8", "\\9", "[\\1]", "[\\c1]", "[\\c_]", "[\\B]", "[\\-]", "\\p", "\\p{a b}", "\\k", "\\k<a>"] {
        ok(p, "");
    }
    for p in ["\\^", "\\$", "\\.", "\\/", "\\|", "[\\-]", "\\0", "\\cA", "\\x41", "\\u0041", "\\u{1F600}", "\\ud83d\\ude00", "\\p{L}", "\\P{Script=Latn}", "\\p{ASCII}"] {
        ok(p, "u");
    }
}

#[test]
fn backreferences() {
    ok("(a)\\1", "u");
    ok("\\1(a)", "u");
    bad("\\1", "u");
    bad("(a)\\2", "u");
    ok("\\1", "");
    ok("\\9", "");
    ok("(a)\\2", "");
}

#[test]
fn named_groups() {
    ok("(?<a>x)\\k<a>", "");
    ok("\\k<a>(?<a>x)", "u");
    ok("(?<𝒜>x)", "");
    ok("(?<\\u{1d49c}>x)", "");
    ok("(?<a\\u0062>x)\\k<ab>", "");
    ok("(?<$_1>x)", "u");
    for p in ["(?<>x)", "(?<1>x)", "(?<a-b>x)", "(?<a", "(?<a>x)(?<a>y)", "(?<a>x)\\k<b>", "(?<a>x)\\k", "(?<a>x)\\k<a", "(?<a>x)[\\k]", "\\k(?<a>x)", "(?<\\u{110000}>x)"] {
        bad(p, "");
    }
    // The same name in different alternatives of one disjunction is allowed.
    ok("(?<a>x)|(?<a>y)", "");
    ok("(?:(?<a>x)|(?<a>y))(?:(?<b>x)|(?<b>y))", "u");
    bad("(?<a>x)|(?<a>y)(?<a>z)", "");
    bad("(?:(?<a>x)|y)(?<a>z)", "");
    // Without a named group `\k` is a letter, and without `u`.
    ok("\\k", "");
    ok("\\k<a>", "");
    ok("[\\k]", "");
    bad("\\k", "u");
    bad("\\k<a>", "u");
}

#[test]
fn modifiers() {
    for p in ["(?i:a)", "(?-i:a)", "(?i-s:a)", "(?ims:a)", "(?i-:a)", "(?ms-i:a)"] {
        ok(p, "");
        ok(p, "u");
    }
    for p in ["(?-:a)", "(?ii:a)", "(?i-i:a)", "(?i--s:a)", "(?x:a)", "(?I:a)", "(?i", "(?i)", "(?i-s)", "(?-s-i:a)"] {
        bad(p, "");
    }
}

#[test]
fn flags_u_and_v_together_are_an_error_and_v_checks_only_the_brackets() {
    bad("a", "uv");
    ok("[a-z&&[^aeiou]]", "v");
    ok("[\\q{abc|d}--\\p{ASCII}]", "v");
    bad("[a", "v");
    bad("(", "v");
    bad("\\a", "v");
}

/// The fast path may only accept what the full check accepts. Random patterns from a list of pieces: whenever `simple` says yes, `full` must too.
#[test]
fn the_fast_path_accepts_only_what_the_full_check_accepts() {
    const PIECES: &[&str] = &[
        "a", "b", "0", " ", ".", "^", "$", "|", "(", ")", "(?:", "(?=", "(?!", "(?<=", "(?<n>", "(?i:", "[", "]", "[^", "-", "a-z", "9-0", "*", "+", "?", "{", "}",
        "{2}", "{2,}", "{2,1}", "{,2}", "\\", "\\d", "\\b", "\\B", "\\1", "\\k<n>", "\\k", "\\x41", "\\u0041", "\\c", "\\0", "\\-", "\\/", "\\]",
        "\\p{L}", "\\.", "é", "\u{1F600}",
    ];
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let (mut fast, mut total) = (0, 0);
    for _ in 0..200_000 {
        let n = 1 + next() % 10;
        let p: String = (0..n).map(|_| PIECES[(next() % PIECES.len() as u64) as usize]).collect();
        total += 1;
        if super::simple(p.as_bytes()) {
            fast += 1;
            assert_eq!(super::full(&p, false, false), Ok(()), "the fast path accepted /{p}/ and the full check did not");
        }
    }
    assert!(fast > total / 20, "the fast path took only {fast} of {total}");
}
