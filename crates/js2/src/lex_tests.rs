// Tests of `lex.rs`: the tests of the old lexer (`crates/js/src/lex.rs`), moved with it.
use super::*;
use Kind::{BigInt, Eof, Ident, Number, PrivateName, RegExp, Template};

#[test]
fn html_like_comments_only_when_asked() {
    // Checked with Node 22: `x = 1 <!-- 2` is `x = 1`; `-->` is a comment only at the start of a line or of the source.
    let texts = |src: &str, on: bool| {
        let mut l = Lexer::new(src).unwrap();
        l.set_html_comments(on);
        let mut v = Vec::new();
        loop {
            let t = l.next(Goal::Div).unwrap();
            if t.kind == Eof {
                return v.join(" ");
            }
            v.push(t.text(src).to_string());
        }
    };
    assert_eq!(texts("a <!-- b c\nd", true), "a d");
    assert_eq!(texts("    <!-- note -->\nif (x) y", true), "if ( x ) y");
    assert_eq!(texts("a\n  --> b\nc", true), "a c");
    assert_eq!(texts("--> b\nc", true), "c");
    assert_eq!(texts("a /*\n*/ --> b\nc", true), "a c");
    assert_eq!(texts("a --> b", true), "a -- > b");
    assert_eq!(texts("a <!-- b", false), "a < ! -- b");
}

/// All tokens with the `Div` goal, up to and without `Eof`.
fn lex(src: &str) -> Result<Vec<Token>, Error> {
    let mut l = Lexer::new(src)?;
    let mut v = Vec::new();
    loop {
        let t = l.next(Goal::Div)?;
        if t.kind == Eof {
            return Ok(v);
        }
        v.push(t);
    }
}

fn texts(src: &str) -> Vec<&str> {
    lex(src).unwrap().iter().map(|t| t.text(src)).collect()
}

fn err(src: &str) -> ErrorKind {
    lex(src).unwrap_err().kind
}

fn string(src: &str) -> Vec<u16> {
    let t = lex(src).unwrap()[0];
    let mut out = Vec::new();
    decode_string(src, &t, &mut out).unwrap();
    out
}

fn u16s(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

#[test]
fn malformed_public_token_spans_do_not_panic() {
    let string = Token { kind: Kind::String { legacy_octal: false }, start: 0, end: 3, newline_before: false, escapes: false };
    assert_eq!(decode_string("'é'", &string, &mut vec![]).unwrap_err().kind, ErrorKind::BadToken);
    let huge = Token { start: u32::MAX, end: u32::MAX, ..string };
    assert_eq!(decode_string("", &huge, &mut vec![]).unwrap_err().kind, ErrorKind::BadToken);
    let template = Token { kind: Kind::Template { part: TemplatePart::Tail, bad_escape: false }, ..string };
    assert_eq!(decode_template("}é`", &template, false, &mut vec![]).unwrap_err().kind, ErrorKind::BadToken);
    let number = Token { kind: Kind::Number(NumberBase::Hex), end: 1, ..string };
    assert_eq!(number_value("x", &number), None);
    assert_eq!(number_value("0b2", &Token { kind: Kind::Number(NumberBase::Binary), end: 3, ..number }), None);
}

#[test]
fn maximal_munch() {
    let src = ">>>= >>> >> >= > **= ** * ?? ??= ?. ? ... . === == = !== != ! => <<= << <= < &&= && & ||= || | ^= ^ ~ ++ -- += -= %= /= : ; , ( ) [ ] { }";
    let t = lex(src).unwrap();
    let got: Vec<&str> = t.iter().map(|t| match t.kind { Kind::Punct(p) => p.as_str(), k => panic!("{k:?}") }).collect();
    assert_eq!(got.join(" "), src);
    assert_eq!(texts("a+++b"), ["a", "++", "+", "b"]);
    assert_eq!(texts("a>>>>b"), [("a"), ">>>", ">", "b"]);
    assert_eq!(texts("x=>y"), ["x", "=>", "y"]);
}

#[test]
fn optional_chain_before_digit_is_conditional() {
    assert_eq!(texts("a?.5:1"), ["a", "?", ".5", ":", "1"]);
    assert_eq!(texts("a?.b"), ["a", "?.", "b"]);
    assert_eq!(texts("a?.[0]"), ["a", "?.", "[", "0", "]"]);
}

#[test]
fn keywords_and_names() {
    let t = lex("if ifx $a _b a1 let async of await yield static null this").unwrap();
    assert_eq!(t[0].kind, Kind::Keyword(Keyword::If));
    assert!(t[1..9].iter().all(|t| t.kind == Ident));
    assert_eq!(t[10 + 1].kind, Kind::Keyword(Keyword::Null));
    assert_eq!(t[12].kind, Kind::Keyword(Keyword::This));
    // Escaped: a name, not a keyword, and flagged.
    let src = "\\u0069f";
    let t = lex(src).unwrap()[0];
    assert_eq!((t.kind, t.escapes), (Ident, true));
    assert_eq!(ident_name(src, &t).unwrap(), "if");
    let src = "a\\u{62}c";
    assert_eq!(ident_name(src, &lex(src).unwrap()[0]).unwrap(), "abc");
    assert!(matches!(ident_name("abc", &lex("abc").unwrap()[0]).unwrap(), Cow::Borrowed(_)));
}

#[test]
fn private_names() {
    let src = "this.#x in #\\u0079";
    let t = lex(src).unwrap();
    assert_eq!(t[2].kind, PrivateName);
    assert_eq!(t[2].text(src), "#x");
    assert_eq!(ident_name(src, &t[2]).unwrap(), "x");
    assert_eq!(ident_name(src, &t[4]).unwrap(), "y");
    assert_eq!(err("#"), ErrorKind::InvalidPrivateName);
    assert_eq!(err("# x"), ErrorKind::InvalidPrivateName);
    assert_eq!(err("#1"), ErrorKind::InvalidPrivateName);
    assert_eq!(err("#€"), ErrorKind::NonAscii);
}

#[test]
fn non_ascii_names_follow_id_start_and_id_continue() {
    let name = |src: &str| {
        let t = lex(src).unwrap();
        assert_eq!(t.len(), 1, "{src}");
        assert_eq!(t[0].kind, Kind::Ident, "{src}");
        ident_name(src, &t[0]).unwrap().into_owned()
    };
    assert_eq!(name("é"), "é");
    assert_eq!(name("a\u{b7}"), "a\u{b7}"); // the middle dot continues a name
    assert_eq!(err("\u{b7}"), ErrorKind::NonAscii);
    assert_eq!(err("\u{d7}"), ErrorKind::NonAscii); // the multiplication sign is not a letter
    assert_eq!(err("\u{b1}"), ErrorKind::NonAscii);
    assert_eq!(name("日本"), "日本");
    assert_eq!(name("a\u{3b2}b"), "aβb");
    assert_eq!(name("\\u00e9"), "é");
    assert_eq!(name("a\\u{3b2}"), "aβ");
    assert_eq!(name("x\u{200c}y"), "x\u{200c}y"); // ZWNJ continues a name
    assert_eq!(name("a\u{0301}"), "a\u{0301}"); // a combining mark continues a name
    assert_eq!(err("\u{0301}"), ErrorKind::NonAscii); // but cannot start one
    assert_eq!(err("\\u{1F600}"), ErrorKind::NonAscii); // an emoji is not a letter
    assert_eq!(err("€"), ErrorKind::NonAscii);
    assert_eq!(err("a€"), ErrorKind::NonAscii);
    assert_eq!(lex("#é").unwrap()[0].kind, Kind::PrivateName);
    assert_eq!(lex("({À:'A'})").unwrap().len(), 7);
    assert_eq!(err("\\u0031"), ErrorKind::InvalidEscape); // a digit cannot start a name
    assert_eq!(err("a\\x41"), ErrorKind::InvalidEscape);
    assert_eq!(err("a\\u12"), ErrorKind::InvalidEscape);
    assert_eq!(err("@"), ErrorKind::UnexpectedChar);
    assert_eq!(err("a\u{0}"), ErrorKind::UnexpectedChar);
    // But non-ASCII is fine in strings, comments, templates, regexps and as white space.
    assert_eq!(lex("'é' /*日本*/ `ü` // ö").unwrap().len(), 2);
    assert_eq!(texts("a\u{a0}b\u{feff}c\u{3000}d\u{2003}e\u{202f}f"), ["a", "b", "c", "d", "e", "f"]);
}

#[test]
fn numbers_valid() {
    let cases: &[(&str, NumberBase, f64)] = &[
        ("0", NumberBase::Decimal, 0.0),
        ("12", NumberBase::Decimal, 12.0),
        ("1.5", NumberBase::Decimal, 1.5),
        ("5.", NumberBase::Decimal, 5.0),
        (".5", NumberBase::Decimal, 0.5),
        ("5.e3", NumberBase::Decimal, 5000.0),
        ("1e3", NumberBase::Decimal, 1000.0),
        ("1E-2", NumberBase::Decimal, 0.01),
        ("2.5e+2", NumberBase::Decimal, 250.0),
        ("1_000_000", NumberBase::Decimal, 1e6),
        ("1_0.2_5e1_0", NumberBase::Decimal, 10.25e10),
        ("0x1F", NumberBase::Hex, 31.0),
        ("0XfF_ff", NumberBase::Hex, 65535.0),
        ("0b101", NumberBase::Binary, 5.0),
        ("0B1_1", NumberBase::Binary, 3.0),
        ("0o17", NumberBase::Octal, 15.0),
        ("017", NumberBase::LegacyOctal, 15.0),
        ("00", NumberBase::LegacyOctal, 0.0),
        ("089", NumberBase::NonOctalDecimal, 89.0),
        ("08.5", NumberBase::NonOctalDecimal, 8.5),
        ("0.5", NumberBase::Decimal, 0.5),
        ("1e400", NumberBase::Decimal, f64::INFINITY),
        ("0xFFFFFFFFFFFFFFFF", NumberBase::Hex, 18446744073709551616.0),
        // 2^53 + 1 is a tie: rounds to even (2^53). With a lower bit set, it rounds up.
        ("0x20000000000001", NumberBase::Hex, 9007199254740992.0),
        ("0x20000000000003", NumberBase::Hex, 9007199254740996.0),
        // Digits past 64 bits only act as a sticky bit: an exact tie stays even, a tie plus a far 1 rounds up.
        ("0x200000000000010000000000000001", NumberBase::Hex, 1.6615349947311452e35),
        ("0x2000000000000100000000000000000000", NumberBase::Hex, 1.0889035741470031e40),
        ("0x20000000000001000000000000000000001", NumberBase::Hex, 1.7422457186352053e41),
        ("0x1000000000000000000000000000000000000000000000000000000000", NumberBase::Hex, 2f64.powi(228)),
    ];
    for &(s, base, v) in cases {
        let t = lex(s).unwrap();
        assert_eq!(t.len(), 1, "{s}");
        assert_eq!(t[0].kind, Number(base), "{s}");
        assert_eq!(number_value(s, &t[0]), Some(v), "{s}");
    }
    assert_eq!(texts("1..toString"), ["1.", ".", "toString"]);
    assert_eq!(texts("1.5.toFixed"), ["1.5", ".", "toFixed"]);
    assert_eq!(texts("1+2"), ["1", "+", "2"]);
    assert_eq!(texts("a.5"), ["a", ".5"]);
    assert_eq!(texts("0.0.1"), ["0.0", ".1"]);
    assert_eq!(texts("07.5"), ["07", ".5"]);
}

/// Long decimal literals (over a million units): Rust's float parser gives infinity for a
/// cancelling exponent. Each expected value is the one Node gives for the same text.
#[test]
fn long_decimal_literals() {
    let m = 1_000_000;
    let z = "0".repeat(m);
    let val = |s: &str| {
        let t = lex(s).unwrap();
        assert_eq!(t.len(), 1, "{s:.40}");
        number_value(s, &t[0]).unwrap()
    };
    assert_eq!(val(&format!("{}.5e-{m}", "9".repeat(m))), 1.0);
    assert_eq!(val(&format!("1{z}e-{m}")), 1.0);
    assert_eq!(val(&format!("0.{z}1e{m}")), 0.1);
    assert_eq!(val(&format!("1{z}e-{}", m - 300)), 1e300);
    assert_eq!(val(&format!("4{z}e-{}", m + 324)), f64::from_bits(1));
    assert_eq!(val(&format!("2{z}e-{}", m + 324)), 0.0);
    assert_eq!(val(&format!("1{z}e-{}", m - 309)), f64::INFINITY);
    assert_eq!(val(&format!("17976931348623157{z}e-{}", m - 292)), f64::MAX);
    // Separators are stripped before the value is taken.
    let digits = "9".repeat(m).as_bytes().chunks(1000).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join("_");
    assert_eq!(val(&format!("{digits}.5e-{m}")), 1.0);
    // 1 + 2^-53 is exactly halfway between 1 and 1 + 2^-52: ties to even gives 1. A dropped
    // tail above the tie rounds up, even when it is far past the kept digits.
    let half = "1.00000000000000011102230246251565404236316680908203125";
    assert_eq!(val(&format!("{half}{z}")), 1.0);
    assert_eq!(val(&format!("{half}{}", "0".repeat(2000))), 1.0);
    assert_eq!(val(&format!("{half}{z}1")), 1.0 + f64::EPSILON);
    assert_eq!(val(&format!("{half}{}1", "0".repeat(2000))), 1.0 + f64::EPSILON);
}

#[test]
fn bigint_scans() {
    for (s, base) in [("10n", NumberBase::Decimal), ("0xFn", NumberBase::Hex), ("0b1n", NumberBase::Binary), ("0o7n", NumberBase::Octal), ("0n", NumberBase::Decimal)] {
        let t = lex(s).unwrap();
        assert_eq!(t[0].kind, BigInt(base), "{s}");
        assert_eq!(number_value(s, &t[0]), None);
    }
    for s in ["1.5n", "1e3n", ".5n", "07n", "089n", "1nn", "1n_"] {
        assert_eq!(err(s), ErrorKind::InvalidNumber, "{s}");
    }
}

#[test]
fn numbers_invalid() {
    for s in [
        "0x", "0b", "0o", "0xg", "0b2", "0o8", "1e", "1e+", "1e-x", "3in", "1_", "1__0", "0_1", "1_.5", "1._5", "1e_5",
        "0x_1", "0x1_", "1.5_", "08_1", "07_1", "1a", "0x1g", "5\\u0061", "0b1_", "1_e5", "00_1",
    ] {
        assert_eq!(err(s), ErrorKind::InvalidNumber, "{s}");
    }
    // The offset points at the bad character.
    assert_eq!(lex("1__0").unwrap_err().offset, 1);
    assert_eq!(lex("12abc").unwrap_err().offset, 2);
    assert_eq!(lex("0x").unwrap_err().offset, 0);
}

#[test]
fn strings_decode_utf16() {
    assert_eq!(string("'abc'"), u16s("abc"));
    assert_eq!(string("\"\""), Vec::<u16>::new());
    assert_eq!(string(r#""a\nb\t\r\b\f\v\0\\\'\"\/""#), u16s("a\nb\t\r\u{8}\u{c}\u{b}\0\\'\"/"));
    assert_eq!(string("'\\x41\\u0042\\u{43}\\u{1F600}'"), u16s("ABC😀"));
    assert_eq!(string("'😀'"), [0xD83D, 0xDE00]); // a real emoji: two code units
    assert_eq!(string("'\\uD83D\\uDE00'"), [0xD83D, 0xDE00]); // an escaped pair: same two units
    assert_eq!(string(r"'\uD800'"), [0xD800]); // a lone high surrogate
    assert_eq!(string(r"'a\uDC00b'"), [0x61, 0xDC00, 0x62]); // a lone low surrogate
    assert_eq!(string(r"'\u{D800}'"), [0xD800]);
    assert_eq!(string(r"'\u{10FFFF}'"), [0xDBFF, 0xDFFF]);
    assert_eq!(string(r"'\u{0000041}'"), [0x41]);
    assert_eq!(string("'a\\\nb'"), u16s("ab")); // line continuation
    assert_eq!(string("'a\\\r\nb'"), u16s("ab"));
    assert_eq!(string("'a\\\u{2028}b'"), u16s("ab"));
    assert_eq!(string("'a\u{2028}b\u{2029}'"), u16s("a\u{2028}b\u{2029}")); // legal raw in a string
    assert_eq!(string(r"'\é'"), u16s("é"));
    assert_eq!(string(r"'\😀'"), [0xD83D, 0xDE00]);
    assert_eq!(string("'日本語'"), u16s("日本語"));
    // Legacy octal.
    assert_eq!(string(r"'\101\7\08\1\377\400'"), [0x41, 7, 0, b'8' as u16, 1, 0xFF, 0x20, b'0' as u16]);
    assert_eq!(string(r"'\8\9'"), u16s("89"));
}

#[test]
fn string_flags_and_raw_spans() {
    let src = r#"'a' "b\n" 'c\1' '\0' '\08'"#;
    let t = lex(src).unwrap();
    assert_eq!((t[0].kind, t[0].escapes), (Kind::String { legacy_octal: false }, false));
    assert_eq!((t[1].kind, t[1].escapes), (Kind::String { legacy_octal: false }, true));
    assert_eq!(t[2].kind, Kind::String { legacy_octal: true });
    assert_eq!(t[3].kind, Kind::String { legacy_octal: false }); // \0 alone is fine
    assert_eq!(t[4].kind, Kind::String { legacy_octal: true }); // \0 before a digit is octal
    assert_eq!(t[1].text(src), "\"b\\n\"");
}

#[test]
fn strings_invalid() {
    for s in [r"'\x4'", r"'\xg0'", r"'\u12'", r"'\u{}'", r"'\u{110000}'", r"'\u{12'", r"'\uXYZW'", r"'\u{1g}'"] {
        assert_eq!(err(s), ErrorKind::InvalidEscape, "{s}");
    }
    assert_eq!(lex(r"'ab\x4'").unwrap_err().offset, 3); // at the backslash
    for s in ["'abc", "\"abc", "'abc\n'", "'abc\r'", "'abc\\", "'a\\'", "'\\", "'"] {
        assert_eq!(err(s), ErrorKind::UnterminatedString, "{s}");
    }
    assert_eq!(lex("x = 'abc\n'").unwrap_err().offset, 4); // at the opening quote
}

#[test]
fn decode_appends_and_checks_token() {
    let src = "'ab'";
    let t = lex(src).unwrap()[0];
    let mut out = vec![1u16];
    decode_string(src, &t, &mut out).unwrap();
    assert_eq!(out, [1, 0x61, 0x62]);
    let fake = Token { kind: Kind::String { legacy_octal: false }, start: 0, end: 99, newline_before: false, escapes: false };
    assert_eq!(decode_string(src, &fake, &mut out).unwrap_err().kind, ErrorKind::BadToken);
    let fake = Token { end: 1, ..fake };
    assert_eq!(decode_string(src, &fake, &mut out).unwrap_err().kind, ErrorKind::BadToken);
    let num = lex("1").unwrap()[0];
    assert_eq!(decode_string("1", &num, &mut out).unwrap_err().kind, ErrorKind::BadToken);
    assert_eq!(number_value("'a'", &t), None);
}

#[test]
fn comments_and_newline_metadata() {
    let nl = |src: &str| lex(src).unwrap().iter().map(|t| t.newline_before).collect::<Vec<_>>();
    assert_eq!(nl("a\nb"), [false, true]);
    assert_eq!(nl("a\rb"), [false, true]);
    assert_eq!(nl("a\r\nb"), [false, true]);
    assert_eq!(nl("a\u{2028}b"), [false, true]);
    assert_eq!(nl("a\u{2029}b"), [false, true]);
    assert_eq!(nl("a b"), [false, false]);
    assert_eq!(nl("a /* x */ b"), [false, false]);
    assert_eq!(nl("a /* x\n */ b"), [false, true]); // a newline inside a block comment counts
    assert_eq!(nl("a /* \u{2028} */ b"), [false, true]);
    assert_eq!(nl("a // x\nb"), [false, true]);
    assert_eq!(nl("a // x \u{2029} b"), [false, true]); // U+2029 ends a line comment
    assert_eq!(nl("a // x b"), [false]);
    // 0xE2 starts many characters (euro, check mark). They are not line terminators.
    assert_eq!(nl("a // \u{20ac}\u{2713} b"), [false]);
    assert_eq!(nl("a /* \u{20ac}\u{2713} */ b"), [false, false]);
    assert_eq!(nl("a // \u{20ac}\nb"), [false, true]);
    assert_eq!(nl("a /* ** * / \u{20ac}\u{2028} * */ b"), [false, true]);
    assert_eq!(nl("a /* * ** **/ b"), [false, false]);
    assert_eq!(nl("\n a"), [true]); // the first token too
    assert_eq!(nl("a\n\n/**/ \t\u{b}\u{c} b"), [false, true]);
    assert_eq!(texts("a/**/b/***/c/* * / */d"), ["a", "b", "c", "d"]);
    assert_eq!(texts("a//b\nc"), ["a", "c"]);
    assert_eq!(texts("a//b\r\nc"), ["a", "c"]);
    assert_eq!(texts("a/*/ b */c"), ["a", "c"]);
    // Hashbang: only at offset 0.
    assert_eq!(texts("#!/usr/bin/env node\nfoo"), ["foo"]);
    assert_eq!(err("\n#!x"), ErrorKind::InvalidPrivateName);
    assert_eq!(err(" #!x"), ErrorKind::InvalidPrivateName);
    // Errors.
    assert_eq!(lex("a /* x").unwrap_err(), Error { kind: ErrorKind::UnterminatedComment, offset: 2 });
    assert_eq!(err("/*"), ErrorKind::UnterminatedComment);
    assert_eq!(err("/*/"), ErrorKind::UnterminatedComment);
    assert_eq!(err("/* *"), ErrorKind::UnterminatedComment);
}

#[test]
fn slash_goal() {
    let src = "a / b /= c";
    let mut l = Lexer::new(src).unwrap();
    let kinds: Vec<_> = (0..5).map(|_| l.next(Goal::Div).unwrap().kind).collect();
    assert_eq!(kinds[1], Kind::Punct(Punct::Slash));
    assert_eq!(kinds[3], Kind::Punct(Punct::SlashAssign));
    // The same bytes with the regexp goal.
    let src = "/ b /= c/gi";
    let mut l = Lexer::new(src).unwrap();
    let t = l.next(Goal::RegExp).unwrap();
    assert_eq!((t.kind, t.text(src)), (RegExp, "/ b /"));
    assert_eq!(l.next(Goal::Div).unwrap().kind, Kind::Punct(Punct::Assign));
    let src = "/a/ /b/g";
    let mut l = Lexer::new(src).unwrap();
    assert_eq!(l.next(Goal::RegExp).unwrap().text(src), "/a/");
    assert_eq!(l.next(Goal::RegExp).unwrap().text(src), "/b/g");
    // Comments win over regexps in every goal.
    let src = "// c\n/*x*/ /y/";
    let mut l = Lexer::new(src).unwrap();
    assert_eq!(l.next(Goal::RegExp).unwrap().text(src), "/y/");
    // Rescan: the parser read `/=` and now knows it starts a regexp. newline_before is kept.
    let src = "x\n/=a=/.test(y)";
    let mut l = Lexer::new(src).unwrap();
    l.next(Goal::Div).unwrap();
    let slash = l.next(Goal::Div).unwrap();
    assert_eq!((slash.kind, slash.newline_before), (Kind::Punct(Punct::SlashAssign), true));
    let re = l.rescan(&slash, Goal::RegExp).unwrap();
    assert_eq!((re.kind, re.text(src), re.newline_before), (RegExp, "/=a=/", true));
    assert_eq!(l.next(Goal::Div).unwrap().kind, Kind::Punct(Punct::Dot));
}

#[test]
fn regexp_boundaries() {
    let ok = |src: &str| {
        let mut l = Lexer::new(src).unwrap();
        let t = l.next(Goal::RegExp).unwrap();
        assert_eq!(t.kind, RegExp, "{src}");
        assert_eq!(l.next(Goal::Div).unwrap().kind, Eof, "{src}");
        regexp_parts(src, &t).map(|(p, f)| (p.to_string(), f.to_string())).unwrap()
    };
    assert_eq!(ok("/a/"), ("a".into(), "".into()));
    assert_eq!(ok(r"/a\/b/gi"), (r"a\/b".into(), "gi".into()));
    assert_eq!(ok("/[/]/"), ("[/]".into(), "".into())); // a slash in a class
    assert_eq!(ok(r"/[\]/]+/u"), (r"[\]/]+".into(), "u".into()));
    assert_eq!(ok(r"/^\/journeys\/(?!new\/?$)[^/]+\/?$/"), (r"^\/journeys\/(?!new\/?$)[^/]+\/?$".into(), "".into()));
    assert_eq!(ok("/(/"), ("(".into(), "".into())); // the pattern is not checked
    assert_eq!(ok("/a/dgimsuy"), ("a".into(), "dgimsuy".into()));
    assert_eq!(ok("/日本/"), ("日本".into(), "".into()));
    assert_eq!(ok(r"/\\/"), (r"\\".into(), "".into()));
    assert_eq!(ok("/[[]/"), ("[[]".into(), "".into()));
    assert_eq!(ok("/a/\u{a0}"), ("a".into(), "".into()));
    let rl = |s: &str| Lexer::new(s).unwrap().next(Goal::RegExp).unwrap_err().kind;
    for s in ["/a", "/a\nb/", "/a\\\n/", "/[/]", "/a\u{2028}/", "/\\", "/a\\"] {
        assert_eq!(rl(s), ErrorKind::UnterminatedRegExp, "{s:?}");
    }
    assert_eq!(rl("/a/gg"), ErrorKind::DuplicateRegExpFlag);
    assert_eq!(rl("/a/gig"), ErrorKind::DuplicateRegExpFlag);
    assert_eq!(rl("/a/vv"), ErrorKind::DuplicateRegExpFlag);
    assert_eq!(rl("/a/x"), ErrorKind::InvalidRegExpFlag);
    assert_eq!(rl("/a/g1"), ErrorKind::InvalidRegExpFlag);
    assert_eq!(rl("/a/\\u0067"), ErrorKind::InvalidRegExpFlag);
}

#[test]
fn regexp_flag_followed_by_unsupported_char() {
    let src = "/a/g€";
    let mut l = Lexer::new(src).unwrap();
    assert_eq!(l.next(Goal::RegExp).unwrap().text(src), "/a/g");
    assert_eq!(l.next(Goal::Div).unwrap_err().kind, ErrorKind::NonAscii);
}

/// Scan a whole template tree the way a parser does: the caller keeps the `${` count.
fn template_parts(src: &str) -> Vec<(TemplatePart, std::string::String)> {
    let mut l = Lexer::new(src).unwrap();
    let mut out = Vec::new();
    let mut depth = 0; // open `${`
    let mut braces = Vec::new(); // `{` depth inside each `${`
    loop {
        let t = l.next(Goal::Div).unwrap();
        match t.kind {
            Eof => return out,
            Template { part, .. } => {
                let mut v = Vec::new();
                decode_template(src, &t, false, &mut v).unwrap();
                out.push((part, std::string::String::from_utf16(&v).unwrap()));
                match part {
                    TemplatePart::Head => {
                        depth += 1;
                        braces.push(0);
                    }
                    TemplatePart::Tail => {
                        depth -= 1;
                        braces.pop();
                    }
                    _ => {}
                }
            }
            Kind::Punct(Punct::LBrace) if depth > 0 => *braces.last_mut().unwrap() += 1,
            Kind::Punct(Punct::RBrace) if depth > 0 => {
                if *braces.last().unwrap() == 0 {
                    // This `}` closes `${`: the parser rescans it as a template continuation.
                    let t = l.rescan(&t, Goal::TemplateTail).unwrap();
                    let Template { part, .. } = t.kind else { panic!() };
                    let mut v = Vec::new();
                    decode_template(src, &t, false, &mut v).unwrap();
                    out.push((part, std::string::String::from_utf16(&v).unwrap()));
                    if part == TemplatePart::Tail {
                        depth -= 1;
                        braces.pop();
                    }
                } else {
                    *braces.last_mut().unwrap() -= 1;
                }
            }
            _ => {}
        }
    }
}

#[test]
fn templates_nested_by_caller() {
    use TemplatePart::*;
    assert_eq!(template_parts("`abc`"), [(NoSubstitution, "abc".into())]);
    assert_eq!(template_parts("`${a}`"), [(Head, "".into()), (Tail, "".into())]);
    assert_eq!(
        template_parts("`a${b}c${d}e`"),
        [(Head, "a".into()), (Middle, "c".into()), (Tail, "e".into())]
    );
    // Braces, objects and a nested template inside `${ }`.
    assert_eq!(
        template_parts("`a${ {x:{}}.x }b${ `c${d}e` }f`"),
        [(Head, "a".into()), (Middle, "b".into()), (Head, "c".into()), (Tail, "e".into()), (Tail, "f".into())]
    );
    assert_eq!(template_parts("`$ {a} $$ \\${x} \\` $`"), [(NoSubstitution, "$ {a} $$ ${x} ` $".into())]);
}

#[test]
fn template_goal_direct() {
    let src = "a }mid${ b }end`";
    let mut l = Lexer::new(src).unwrap();
    l.next(Goal::Div).unwrap();
    let t = l.next(Goal::TemplateTail).unwrap();
    assert_eq!((t.kind, t.text(src)), (Template { part: TemplatePart::Middle, bad_escape: false }, "}mid${"));
    l.next(Goal::Div).unwrap();
    let t = l.next(Goal::TemplateTail).unwrap();
    assert_eq!((t.kind, t.text(src)), (Template { part: TemplatePart::Tail, bad_escape: false }, "}end`"));
    // Not a `}`: an error, and it stays an error.
    let mut l = Lexer::new("a b").unwrap();
    l.next(Goal::Div).unwrap();
    let e = l.next(Goal::TemplateTail).unwrap_err();
    assert_eq!((e.kind, e.offset), (ErrorKind::ExpectedTemplateContinuation, 2));
    assert_eq!(l.next(Goal::Div).unwrap_err(), e);
    // Unterminated.
    assert_eq!(err("`abc"), ErrorKind::UnterminatedTemplate);
    assert_eq!(err("`a\\"), ErrorKind::UnterminatedTemplate);
    let mut l = Lexer::new("}abc").unwrap();
    assert_eq!(l.next(Goal::TemplateTail).unwrap_err().kind, ErrorKind::UnterminatedTemplate);
}

#[test]
fn template_values() {
    let one = |src: &str, raw: bool| {
        let t = Lexer::new(src).unwrap().next(Goal::Div).unwrap();
        let mut v = Vec::new();
        decode_template(src, &t, raw, &mut v).map(|_| v)
    };
    assert_eq!(one("`a\\nb`", false).unwrap(), u16s("a\nb"));
    assert_eq!(one("`a\\nb`", true).unwrap(), u16s("a\\nb")); // raw keeps the backslash
    assert_eq!(one("`a\r\nb\rc`", false).unwrap(), u16s("a\nb\nc")); // CRLF and CR read as LF
    assert_eq!(one("`a\r\nb\rc`", true).unwrap(), u16s("a\nb\nc"));
    assert_eq!(one("`a\\\r\nb`", false).unwrap(), u16s("ab"));
    assert_eq!(one("`a\\\r\nb`", true).unwrap(), u16s("a\\\nb"));
    assert_eq!(one("`\\uD800\\u{1F600}😀`", false).unwrap(), [0xD800, 0xD83D, 0xDE00, 0xD83D, 0xDE00]);
    assert_eq!(one("`\\0`", false).unwrap(), [0]);
    // Bad escapes: the token is fine, the cooked value is not, the raw value is.
    for s in ["`\\01`", "`\\1`", "`\\8`", "`\\xg`", "`\\u12`", "`\\u{110000}`", "`\\u{`"] {
        let t = lex(s).unwrap()[0];
        assert_eq!(t.kind, Template { part: TemplatePart::NoSubstitution, bad_escape: true }, "{s}");
        assert_eq!(one(s, false).unwrap_err().kind, ErrorKind::InvalidEscape, "{s}");
        assert!(one(s, true).is_ok(), "{s}");
    }
    // The text after a bad escape is read normally: this closes the template at the last backtick.
    assert_eq!(lex("`\\u{110000}` x").unwrap().len(), 2);
    let src = "`\\xg ${a}`";
    let t = Lexer::new(src).unwrap().next(Goal::Div).unwrap();
    assert_eq!(t.kind, Template { part: TemplatePart::Head, bad_escape: true });
    assert_eq!(one("`${a}`", false).unwrap(), Vec::<u16>::new());
    // Head, middle and tail strip their own delimiters.
    let src = "`a${x}b${y}c`";
    let mut l = Lexer::new(src).unwrap();
    let h = l.next(Goal::Div).unwrap();
    let x = l.next(Goal::Div).unwrap();
    let rb = l.next(Goal::Div).unwrap();
    assert_eq!(x.text(src), "x");
    let m = l.rescan(&rb, Goal::TemplateTail).unwrap();
    let mut v = Vec::new();
    decode_template(src, &h, true, &mut v).unwrap();
    decode_template(src, &m, true, &mut v).unwrap();
    assert_eq!(v, u16s("ab"));
    let nonstr = lex("1").unwrap()[0];
    assert_eq!(decode_template("1", &nonstr, false, &mut v).unwrap_err().kind, ErrorKind::BadToken);
}

#[test]
fn rescan_rejects_foreign_tokens() {
    let mut l = Lexer::new("ab").unwrap();
    let t = Token { kind: Eof, start: 9, end: 9, newline_before: false, escapes: false };
    assert_eq!(l.rescan(&t, Goal::Div).unwrap_err().kind, ErrorKind::BadToken);
    let mut l = Lexer::new("é").unwrap();
    let t = Token { start: 1, ..t };
    assert_eq!(l.rescan(&t, Goal::Div).unwrap_err().kind, ErrorKind::BadToken);
}

#[test]
fn eof_and_errors_are_sticky() {
    let mut l = Lexer::new("  ").unwrap();
    let t = l.next(Goal::Div).unwrap();
    assert_eq!((t.kind, t.start, t.end), (Eof, 2, 2));
    assert_eq!(l.next(Goal::Div).unwrap().kind, Eof);
    let mut l = Lexer::new("a @ b").unwrap();
    l.next(Goal::Div).unwrap();
    let e = l.next(Goal::Div).unwrap_err();
    assert_eq!(e, Error { kind: ErrorKind::UnexpectedChar, offset: 2 });
    assert_eq!(l.next(Goal::Div).unwrap_err(), e);
}

#[test]
fn source_cap_is_exact() {
    let big = " ".repeat(MAX_SOURCE_LEN);
    let mut l = Lexer::new(&big).unwrap();
    let t = l.next(Goal::Div).unwrap();
    assert_eq!((t.kind, t.start), (Eof, MAX_SOURCE_LEN as u32));
    let over = " ".repeat(MAX_SOURCE_LEN + 1);
    let e = Lexer::new(&over).err().unwrap();
    assert_eq!(e.kind, ErrorKind::SourceTooLarge);
    assert!(MAX_SOURCE_LEN < u32::MAX as usize);
}

#[test]
fn locations_count_utf16() {
    let src = "ab\n'😀' @";
    let e = lex(src).unwrap_err();
    assert_eq!(e.offset, 10); // bytes: a b \n ' (4 bytes) ' space
    assert_eq!(e.location(src), Location { line: 2, column: 5 }); // quote 1, emoji 2, quote 1, space 1
    assert_eq!(location("a\r\nb", 3), Location { line: 2, column: 0 });
    assert_eq!(location("a\rb", 2), Location { line: 2, column: 0 });
    assert_eq!(location("a\u{2028}bc", 6), Location { line: 2, column: 2 });
    assert_eq!(location("é", 1), Location { line: 1, column: 0 }); // inside a character: clamped back
    assert_eq!(location("ab", 99), Location { line: 1, column: 2 });
    assert_eq!(utf16_offset("a😀b", 5), 3);
    assert_eq!(utf16_offset("a😀b", 1), 1);
}

#[test]
fn tokens_are_small() {
    assert!(std::mem::size_of::<Token>() <= 12);
}

/// Random mixes of syntax characters: no panic, every token inside the source and moving forward,
/// a failed lexer stays failed. A fixed seed, so a failure repeats.
#[test]
fn fuzz_terminates_and_stays_in_bounds() {
    const PARTS: &[&str] = &[
        "a", "if", "1", "0x", "1_0", ".", "..", "...", "'", "\"", "`", "${", "}", "{", "/", "/*", "*/", "//", "\\", "\\u", "\\u{", "é", "😀", "\n", "\r\n", "\u{2028}", " ", "#", "#!", "?.", "?", "=", ">", ">>>", "e+", "n", "[", "]", "(", ")", "$", "_", "\0", "\u{a0}",
    ];
    let mut seed = 0x9E3779B97F4A7C15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..20000 {
        let n = next() % 12;
        let src: std::string::String = (0..n).map(|_| PARTS[(next() % PARTS.len() as u64) as usize]).collect();
        let mut l = Lexer::new(&src).unwrap();
        let mut last = 0u32;
        for step in 0..src.len() + 2 {
            let goal = [Goal::Div, Goal::RegExp, Goal::TemplateTail][(next() % 3) as usize];
            match l.next(goal) {
                Ok(t) => {
                    assert!(t.start <= t.end && t.end as usize <= src.len(), "{src:?}");
                    assert!(src.is_char_boundary(t.start as usize) && src.is_char_boundary(t.end as usize), "{src:?}");
                    assert!(t.start >= last, "{src:?}");
                    assert!(t.kind == Eof || t.end > t.start, "{src:?}");
                    last = t.end;
                    if t.kind == Eof {
                        break;
                    }
                    let _ = decode_string(&src, &t, &mut Vec::new());
                    let _ = decode_template(&src, &t, false, &mut Vec::new());
                    let _ = ident_name(&src, &t);
                    let _ = number_value(&src, &t);
                    let _ = regexp_parts(&src, &t);
                }
                Err(e) => {
                    assert!(e.offset as usize <= src.len(), "{src:?}");
                    assert_eq!(l.next(goal).unwrap_err(), e);
                    let _ = e.location(&src);
                    break;
                }
            }
            assert!(step <= src.len(), "no progress on {src:?}");
        }
    }
}
#[test]
fn forged_long_decimal_tokens_do_not_panic_or_parse_junk() {
    for suffix in ["e!", "..1", "x", "e", "e+"] {
        let source = "1".repeat(2000) + suffix;
        let token = Token { kind: Kind::Number(NumberBase::Decimal), start: 0, end: source.len() as u32, newline_before: false, escapes: false };
        assert!(std::panic::catch_unwind(|| number_value(&source, &token)).unwrap().is_none());
    }
}


#[test]
fn marked_locations_match_the_full_scan() {
    // CR, CRLF split at many offsets, LS, PS, 2-, 3- and 4-byte characters, across several marks
    let mut src = String::new();
    for i in 0..3000 {
        src.push_str(["ab\r\n", "é\r", "x\u{2028}", "😀\n", "€", "\u{2029}q", "lo"][i % 7]);
    }
    // the character-by-character scan that the byte scan replaced
    let reference = |byte: usize| {
        let mut end = byte.min(src.len());
        while !src.is_char_boundary(end) {
            end -= 1;
        }
        let (mut line, mut column, mut prev_cr) = (1u32, 0u32, false);
        for ch in src[..end].chars() {
            match ch {
                '\n' if prev_cr => {}
                '\n' | '\r' | '\u{2028}' | '\u{2029}' => (line, column) = (line + 1, 0),
                _ => column += ch.len_utf16() as u32,
            }
            prev_cr = ch == '\r';
        }
        Location { line, column }
    };
    let marks = location_marks(&src);
    assert_eq!(marks.len(), src.len().div_ceil(MARK_STEP));
    for b in 0..=src.len() + 3 {
        let want = reference(b);
        assert_eq!(location_in(&src, &marks, b), want, "byte {b}");
        assert_eq!(location(&src, b), want, "byte {b}");
    }
    assert_eq!(location_in("", &location_marks(""), 5), Location { line: 1, column: 0 });
}

/// Long runs of plain text (the block path of the scan) with line breaks, E2 bytes and wide characters at random places, so
/// that they fall at every position of a block and across the edges of blocks and marks. Each offset is checked.
#[test]
fn marked_locations_match_the_reference_on_random_text() {
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    for round in 0..6 {
        let mut src = String::new();
        while src.len() < 2 * MARK_STEP + 100 {
            let run = match round % 3 {
                0 => next(200),
                1 => next(40),
                _ => next(4),
            };
            for _ in 0..run {
                src.push(['a', 'b', ' ', ';', '\t', '\0', '\u{b}', '\u{c}'][next(8) as usize]);
            }
            src.push_str(["\n", "\r", "\r\n", "\u{2028}", "\u{2029}", "\u{2027}", "\u{20ac}", "\u{e9}", "\u{30a2}", "\u{1f600}", "\n\n", "\n\r"][next(12) as usize]);
        }
        let reference = |byte: usize| {
            let mut end = byte.min(src.len());
            while !src.is_char_boundary(end) {
                end -= 1;
            }
            let (mut line, mut column, mut prev_cr) = (1u32, 0u32, false);
            for ch in src[..end].chars() {
                match ch {
                    '\n' if prev_cr => {}
                    '\n' | '\r' | '\u{2028}' | '\u{2029}' => (line, column) = (line + 1, 0),
                    _ => column += ch.len_utf16() as u32,
                }
                prev_cr = ch == '\r';
            }
            Location { line, column }
        };
        let marks = location_marks(&src);
        for b in 0..=src.len() {
            assert_eq!(location_in(&src, &marks, b), reference(b), "round {round} byte {b}");
        }
    }
}
