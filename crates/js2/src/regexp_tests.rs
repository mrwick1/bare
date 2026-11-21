// Tests of `regexp.rs`: the tests of the old engine (`crates/js/src/regexp.rs`) over the image, and the budget of the new one.
use super::*;

/// A compiled pattern for the tests: the image.
struct Regex(Vec<u64>);

struct Match {
    spans: Vec<Option<(usize, usize)>>,
}

impl Regex {
    fn new(p: &[u16], f: &str) -> Result<Regex, CompileError> {
        compile(p, f).map(Regex)
    }
    fn from_str(p: &str, f: &str) -> Result<Regex, CompileError> {
        let units: Vec<u16> = p.encode_utf16().take(MAX_PATTERN_UNITS + 1).collect();
        Regex::new(&units, f)
    }
    fn group_count(&self) -> usize {
        image_groups(&self.0)
    }
    fn exec(&self, input: &[u16], li: usize, sc: &mut Scratch) -> Result<Option<Match>, ExecError> {
        let mut fuel = u64::MAX;
        self.exec_fuel(input, li, sc, &mut fuel)
    }
    fn exec_fuel(&self, input: &[u16], li: usize, sc: &mut Scratch, fuel: &mut u64) -> Result<Option<Match>, ExecError> {
        let mut out = vec![0u32; 2 * (self.group_count() + 1)];
        Ok(exec(&self.0, input, li, sc, fuel, &mut out)?.then(|| Match { spans: out.chunks(2).map(|c| (c[0] != UNSET).then_some((c[0] as usize, c[1] as usize))).collect() }))
    }
}


fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}
/// Spans of the first match as "s,e;s,e;-", or "N" for no match.
fn run(p: &str, f: &str, s: &str) -> String {
    run_at(p, f, s, 0)
}
fn run_at(p: &str, f: &str, s: &str, li: usize) -> String {
    let re = Regex::from_str(p, f).unwrap();
    fmt(re.exec(&u(s), li, &mut Scratch::new()).unwrap())
}
fn fmt(m: Option<Match>) -> String {
    match m {
        None => "N".into(),
        Some(m) => m.spans.iter().map(|s| s.map_or("-".into(), |(a, b)| format!("{a},{b}"))).collect::<Vec<_>>().join(";"),
    }
}

// Expected values below were checked with Node 22 (`/p/gd.exec`); see tools/js/regexp_oracle.mjs
// for the machine-checked set.
#[test]
fn lookbehind() {
    assert_eq!(run("(?<=a)b", "", "ab"), "1,2");
    assert_eq!(run("(?<!a)b", "", "cb"), "1,2");
    assert_eq!(run("(?<!a)b", "", "ab"), "N");
    assert_eq!(run("(?<=([ab]+)([bc]+))$", "", "abc"), "3,3;0,1;1,3");
    assert_eq!(run("(?<=([ab])+)c", "", "abc"), "2,3;0,1");
    assert_eq!(run("(?<=a|ba)c", "", "bac"), "2,3");
    assert_eq!(run("(?<=a(?=b))b", "", "ab"), "1,2");
    assert_eq!(run("(?<=\\1(a))b", "", "aab"), "2,3;1,2");
    assert_eq!(run("(?<=😀)x", "u", "😀x"), "2,3");
    assert_eq!(run("(?<=k)x", "iu", "Kx"), "1,2");
    assert_eq!(run("(?<=a)x", "i", "Ax"), "1,2");
}

#[test]
fn quantified_lookahead_follows_annex_b() {
    // Checked with Node 22.
    assert_eq!(run("(?=a)*", "", "b"), "0,0");
    assert_eq!(run("(?=(a))*a", "", "ab"), "0,1;-");
    assert_eq!(run("(?=(a))+a", "", "ab"), "0,1;0,1");
    assert_eq!(run("(?=(a)){2}a", "", "ab"), "0,1;0,1");
    assert_eq!(run("(?=(a))*?a", "", "ab"), "0,1;-");
    assert_eq!(run("x(?!b)+c", "", "xbc"), "N");
    assert_eq!(run("x(?!b)+c", "", "xc"), "0,2");
    assert_eq!(run("(?=a){1,3}", "", "zzz"), "N");
    assert!(Regex::from_str("(?=a)*", "u").is_err());
    assert!(Regex::from_str("(?<=a)*", "").is_err());
}

#[test]
fn basics() {
    assert_eq!(run("abc", "", "xxabcxx"), "2,5");
    assert_eq!(run("a*?", "", "aaa"), "0,0");
    assert_eq!(run("a|ab", "", "abc"), "0,1");
    assert_eq!(run("(z)((a+)?(b+)?(c))*", "", "zaacbbbcac"), "0,10;0,1;8,10;8,9;-;9,10");
    assert_eq!(run("(a*)*", "", "b"), "0,0;-");
    assert_eq!(run("(a*)+", "", "b"), "0,0;0,0");
    assert_eq!(run("(?=(a+))a*b\\1", "", "baaabac"), "3,6;3,4");
    assert_eq!(run("(.*?)a(?!(a+)b\\2c)\\2(.*)", "", "baaabaac"), "0,8;0,2;-;3,8");
}

#[test]
fn target_route_lookahead() {
    let p = "^\\/journeys\\/(?!new\\/?$)[^/]+\\/?$";
    assert_eq!(run(p, "", "/journeys/abc/"), "0,14");
    assert_eq!(run(p, "", "/journeys/new"), "N");
    assert_eq!(run(p, "", "/journeys/new/"), "N");
    assert_eq!(run(p, "", "/journeys/new2"), "0,14");
}

#[test]
fn utf16_units() {
    // Non-BMP: `.` matches one code unit, `^..$` matches a pair, a lone surrogate is a unit.
    assert_eq!(run("^.$", "", "\u{1F600}"), "N");
    assert_eq!(run("^..$", "", "\u{1F600}"), "0,2");
    let re = Regex::new(&[0xD800], "").unwrap();
    let input = [0x61, 0xD800, 0x62];
    let m = re.exec(&input, 0, &mut Scratch::new()).unwrap();
    assert_eq!(fmt(m), "1,2");
    let re = Regex::new(&[0xDE00], "").unwrap();
    let pair = u("\u{1F600}");
    let m = re.exec(&pair, 0, &mut Scratch::new()).unwrap();
    assert_eq!(fmt(m), "1,2"); // no Unicode mode: the low surrogate matches alone
}

#[test]
fn case_folding() {
    assert_eq!(run("abc", "i", "ABC"), "0,3");
    assert_eq!(run("ß", "i", "SS"), "N"); // full uppercase is two units: no fold
    assert_eq!(run("k", "i", "\u{212A}"), "N"); // Kelvin sign maps to ASCII: not folded
    // With `u`: simple case folding, by code point.
    assert_eq!(run("k", "iu", "\u{212A}"), "0,1");
    assert_eq!(run("\\W", "iu", "\u{17F}"), "N"); // U+017F folds to "s": a word character
    assert_eq!(run("(s)\\1", "iu", "S\u{17F}"), "0,2;0,1");
    assert_eq!(run("\u{10400}", "iu", "\u{10428}"), "0,2");
    assert_eq!(run("\u{1C5}", "iu", "\u{1C4}"), "0,1"); // a group of adjacent code points is one range
    assert_eq!(run("σ", "i", "ς"), "0,1");
    assert_eq!(run("[a-z]+", "i", "ABC"), "0,3");
    assert_eq!(run("[^a-z]", "i", "A"), "N");
    assert_eq!(run("(a)\\1", "i", "aA"), "0,2;0,1");
}

#[test]
fn lines_and_anchors() {
    assert_eq!(run("^a", "m", "b\ra"), "2,3");
    assert_eq!(run("a$", "m", "a\u{2028}b"), "0,1");
    assert_eq!(run("^a", "", "b\na"), "N");
    assert_eq!(run(".", "", "\r"), "N");
    assert_eq!(run(".", "s", "\r"), "0,1");
    assert_eq!(run("\\b", "", ""), "N");
    assert_eq!(run("\\B", "", ""), "0,0");
}

#[test]
fn search_start_and_sticky() {
    assert_eq!(run_at("a", "", "ba", 1), "1,2");
    assert_eq!(run_at("a", "y", "ba", 0), "N");
    assert_eq!(run_at("a", "y", "ba", 1), "1,2");
    assert_eq!(run_at("", "", "ab", 2), "2,2");
    assert_eq!(run_at("", "", "ab", 3), "N"); // last_index > len
    assert_eq!(run_at("a", "g", "aa", 1), "1,2"); // g is wrapper state; exec searches from last_index
}

#[test]
fn accelerators_keep_results() {
    // Skipping start positions must not change the answer, with non-ASCII input around.
    assert_eq!(run("b+", "", "ééb\u{1F600}bb"), "2,3");
    assert_eq!(run("é|b", "", "xxb"), "2,3");
    assert_eq!(run("k", "i", "xx\u{212A}K"), "3,4");
    assert_eq!(run("(?=a)", "", "ba"), "1,1");
    assert_eq!(run("^a", "", "ba"), "N");
    assert_eq!(run_at("^a", "", "aa", 1), "N");
    assert_eq!(run_at("^a", "y", "aa", 1), "N");
    assert_eq!(run_at("^a", "m", "a\na", 1), "2,3");
    assert_eq!(run("$", "", "ab"), "2,2");
    assert_eq!(run("x*", "", "ab"), "0,0");
}

#[test]
fn syntax_errors_and_unsupported() {
    for p in ["(", ")", "[", "*", "a**", "a{2,1}", "{1}", "(?x)", "[b-a]", "\\", "^*", "\\b+"] {
        assert!(matches!(Regex::from_str(p, ""), Err(CompileError::Syntax { .. })), "{p}");
    }
    for p in ["]", "}", "{", "a{", "a{,5}", "\\8", "\\c", "[\\d-x]", "\\p{L}", "\\u{3}"] {
        assert!(Regex::from_str(p, "").is_ok(), "{p}");
    }


    assert!(Regex::from_str("(?<n>a)", "").is_ok());
    assert!(Regex::from_str("(?=a)*", "").is_ok());
    assert!(matches!(Regex::from_str("(?=a)*", "u"), Err(CompileError::Syntax { .. })));
    for f in ["v", "dv", "gv"] {
        assert!(Regex::from_str("a", f).is_ok(), "{f}");
    }
    for f in ["gg", "x", "G", "é", "uv"] {
        assert!(matches!(Regex::from_str("a", f), Err(CompileError::InvalidFlags(_))), "{f}");
    }
    assert_eq!(Flags::parse("gimsy").unwrap(), Flags { global: true, ignore_case: true, multiline: true, dot_all: true, sticky: true, unicode: false, unicode_sets: false, has_indices: false });
    assert!(Flags::parse("dg").unwrap().has_indices);
    assert!(Flags::parse("u").unwrap().unicode);
}

/// Whole-string match of `p` (flag `v` plus `extra`) against `s`.
fn vfull(p: &str, extra: &str, s: &str) -> bool {
    let re = Regex::from_str(&format!("^(?:{p})$"), &format!("v{extra}")).unwrap();
    re.exec(&u(s), 0, &mut Scratch::new()).unwrap().is_some()
}

#[test]
fn unicode_sets_flag() {
    let f = Flags::parse("v").unwrap();
    assert!(f.unicode_sets && f.unicode);
    assert!(!Flags::parse("u").unwrap().unicode_sets);
    assert!(matches!(Flags::parse("uv"), Err(CompileError::InvalidFlags(_))));
    // Code point semantics, as with `u`.
    assert!(vfull(".", "", "\u{1F600}"));
}

#[test]
fn unicode_sets_classes() {
    // A plain class and ranges.
    assert!(vfull("[a-c]+", "", "abcba") && !vfull("[a-c]+", "", "abd"));
    assert!(vfull("[^a-c]", "", "d") && !vfull("[^a-c]", "", "b"));
    assert!(vfull("[\\u{1F600}-\\u{1F64F}]", "", "\u{1F610}"));
    // Subtraction.
    assert!(vfull("[\\w--\\d]+", "", "ab_") && !vfull("[\\w--\\d]", "", "5"));
    assert!(vfull("[[a-z]--[aeiou]]+", "", "xyz") && !vfull("[[a-z]--[aeiou]]", "", "e"));
    assert!(vfull("[[a-z]--b--c]+", "", "adz") && !vfull("[[a-z]--b--c]", "", "c"));
    // Intersection.
    assert!(vfull("[\\w&&\\d]+", "", "123") && !vfull("[\\w&&\\d]", "", "a"));
    assert!(vfull("[[a-f]&&[d-z]&&[^e]]+", "", "df") && !vfull("[[a-f]&&[d-z]&&[^e]]", "", "e"));
    // Nested classes and union.
    assert!(vfull("[a[0-9][x-z]]+", "", "a1y") && !vfull("[a[0-9][x-z]]", "", "b"));
    assert!(vfull("[[a-z]--[^a-c]]+", "", "abc") && !vfull("[[a-z]--[^a-c]]", "", "d"));
    assert!(vfull("[^[a-c][x-z]]", "", "m") && !vfull("[^[a-c][x-z]]", "", "y"));
    // Escapes: reserved punctuators and syntax characters.
    assert!(vfull("[\\&\\-\\!\\#\\%\\,\\:\\;\\<\\=\\>\\@\\`\\~]+", "", "&-!#%,:;<=>@`~"));
    assert!(vfull("[\\(\\)\\[\\]\\{\\}\\/\\|]+", "", "()[]{}/|"));
    assert!(vfull("[a\\-z]+", "", "a-z") && !vfull("[a\\-z]", "", "b"));
    // Properties.
    assert!(vfull("[\\p{L}--[a-z]]+", "", "ABé") && !vfull("[\\p{L}--[a-z]]", "", "a"));
    assert!(vfull("[\\p{L}&&\\p{ASCII}]+", "", "az") && !vfull("[\\p{L}&&\\p{ASCII}]", "", "é"));
    // Ignore case.
    assert!(vfull("[a-c]+", "i", "ABC"));
    assert!(vfull("[\\q{ab}x]+", "i", "ABX"));
    // Quantified.
    assert!(vfull("[[a-c]--b]{3}", "", "aca"));
}

#[test]
fn unicode_sets_string_literals() {
    assert!(vfull("[\\q{abc}]", "", "abc") && !vfull("[\\q{abc}]", "", "ab"));
    assert!(vfull("[\\q{abc|d|}x]", "", "abc") && vfull("[\\q{abc|d|}x]", "", "d") && vfull("[\\q{abc|d|}x]", "", "x") && vfull("[\\q{abc|d|}x]", "", ""));
    // The longest string wins over a prefix that is also a member.
    let re = Regex::from_str("[\\q{ab|a}]", "v").unwrap();
    assert_eq!(fmt(re.exec(&u("abc"), 0, &mut Scratch::new()).unwrap()), "0,2");
    assert_eq!(run("[\\q{ab|a}]", "v", "abc"), "0,2");
    // Strings in operations.
    assert!(vfull("[\\q{ab|cd|e}--\\q{cd}]", "", "ab") && !vfull("[\\q{ab|cd|e}--\\q{cd}]", "", "cd"));
    assert!(vfull("[\\q{ab|cd|e}--\\q{cd}]", "", "e"));
    assert!(vfull("[\\q{ab|cd|e}&&\\q{cd|e|f}]", "", "cd") && !vfull("[\\q{ab|cd|e}&&\\q{cd|e|f}]", "", "ab"));
    assert!(vfull("[\\q{ab|cd|e}&&\\q{cd|e|f}]", "", "e"));
    // A one code point string is a class member (also in a subtraction from a range).
    assert!(vfull("[[a-c]--\\q{b}]+", "", "ac") && !vfull("[[a-c]--\\q{b}]", "", "b"));
    assert!(vfull("[\\q{\\u{1F600}}]", "", "\u{1F600}"));
    // Escapes inside \q.
    assert!(vfull("[\\q{a\\|b|c\\}}]", "", "a|b") && vfull("[\\q{a\\|b|c\\}}]", "", "c}"));
    assert!(vfull("[\\q{\\n\\x41}]", "", "\nA"));
}

#[test]
fn unicode_sets_syntax_errors() {
    for p in [
        "[a&&&b]", "[a&&b--c]", "[a--b&&c]", "[ab&&c]", "[a&&bc]", "[a-z&&b]", "[a-z--b]", "[a--]", "[--a]", "[&&a]",
        "[(]", "[)]", "[{]", "[}]", "[/]", "[|]", "[-]", "[a-]", "[[a]", "[a]]", "[!!]", "[##]", "[a^^]",
        "[^\\q{ab}]", "[^\\q{}]", "[\\q{a]", "[\\q{a}", "[\\q]", "[z-a]", "[\\d-z]", "[a-\\d]", "[\\p{RGI_Emoji}]", "[\\k<a>]",
    ] {
        assert!(matches!(Regex::from_str(p, "v"), Err(CompileError::Syntax { .. })), "{p}");
    }
    // Valid in `v`.
    for p in ["[a&b]", "[a!b]", "[^\\q{a|b}]", "[\\q{}]", "[]", "[^]", "[a-z]", "[a\\-b]"] {
        assert!(Regex::from_str(p, "v").is_ok(), "{p}");
    }
    // Without `v` the old syntax stays: `&&` and `--` are plain members.
    for p in ["[a&&b]", "[(]", "[!--b]", "[\\q]"] {
        assert!(Regex::from_str(p, "").is_ok(), "{p}");
    }
    for p in ["[a&&b]", "[(]", "[!--b]"] {
        assert!(Regex::from_str(p, "u").is_ok(), "{p}");
    }
    // A group count scan is not fooled by a `(` inside a nested class.
    assert_eq!(Regex::from_str("[[a]\\(](b)", "v").map(|r| r.group_count()), Ok(1));
}

#[test]
fn unicode_property_escapes() {
    // General_Category, short and long names, in the `u` mode.
    assert_eq!(run("\\p{L}+", "u", "12héllo wörld"), "2,7");
    assert_eq!(run("\\p{Letter}+", "u", "12héllo wörld"), "2,7");
    assert_eq!(run("\\p{Lu}", "u", "abcDef"), "3,4");
    assert_eq!(run("\\p{Uppercase_Letter}", "u", "abcDef"), "3,4");
    assert_eq!(run("\\p{gc=Lu}", "u", "abcDef"), "3,4");
    assert_eq!(run("\\p{General_Category=Ll}+", "u", "ABcdE"), "2,4");
    assert_eq!(run("\\p{N}+", "u", "ab٣4Ⅷ½x"), "2,6");
    assert_eq!(run("\\p{Nd}+", "u", "ab٣4Ⅷ"), "2,4");
    assert_eq!(run("\\p{P}+", "u", "ab!?。c"), "2,5");
    assert_eq!(run("\\p{S}", "u", "ab+c"), "2,3");
    assert_eq!(run("\\p{Z}", "u", "ab c"), "2,3");
    assert_eq!(run("\\p{C}", "u", "ab\u{1}c"), "2,3");
    // Negated, and inside a class.
    assert_eq!(run("\\P{L}+", "u", "ab12-c"), "2,5");
    assert_eq!(run("[\\p{Lu}\\d]+", "u", "abD1eF2g"), "2,4");
    assert_eq!(run("[^\\p{L}]+", "u", "ab12-c"), "2,5");
    assert_eq!(run("[\\P{L}a]+", "u", "b1a-b"), "1,4");
    // Scripts.
    assert_eq!(run("\\p{Script=Greek}+", "u", "abαβγd"), "2,5");
    assert_eq!(run("\\p{sc=Grek}+", "u", "abαβγd"), "2,5");
    assert_eq!(run("\\p{Script=Cyrillic}+", "u", "abжиd"), "2,4");
    assert_eq!(run("\\p{Script=Han}+", "u", "ab字漢d"), "2,4");
    assert_eq!(run("\\p{Script=Hiragana}+", "u", "abあいd"), "2,4");
    assert_eq!(run("\\p{sc=Katakana}+", "u", "abカタd"), "2,4");
    assert_eq!(run("\\p{sc=Arabic}+", "u", "abعربd"), "2,5");
    assert_eq!(run("\\p{sc=Hebrew}+", "u", "abאבd"), "2,4");
    assert_eq!(run("\\p{sc=Devanagari}+", "u", "abअबd"), "2,4");
    assert_eq!(run("\\p{sc=Thai}+", "u", "abกขd"), "2,4");
    assert_eq!(run("\\p{sc=Hangul}+", "u", "ab한글d"), "2,4");
    assert_eq!(run("\\p{sc=Latin}+", "u", "12abé3"), "2,5");
    // Script_Extensions: the prolonged sound mark is Common, and also in Hiragana and Katakana.
    assert_eq!(run("\\p{sc=Hiragana}+", "u", "あー"), "0,1");
    assert_eq!(run("\\p{scx=Hiragana}+", "u", "あー"), "0,2");
    assert_eq!(run("\\p{Script_Extensions=Katakana}+", "u", "カー"), "0,2");
    // Binary properties.
    assert_eq!(run("\\p{Alphabetic}+", "u", "12ab字3"), "2,5");
    assert_eq!(run("\\p{White_Space}", "u", "ab\u{3000}c"), "2,3");
    assert_eq!(run("\\p{Uppercase}", "u", "abCd"), "2,3");
    assert_eq!(run("\\p{Lowercase}", "u", "ABcD"), "2,3");
    assert_eq!(run("\\p{ASCII}+", "u", "éabcé"), "1,4");
    assert_eq!(run("\\P{ASCII}+", "u", "abéé"), "2,4");
    assert_eq!(run("^\\p{Any}+$", "u", "a😀\u{10ffff}"), "0,5");
    assert_eq!(run("\\p{Assigned}", "u", "\u{378}a"), "1,2");
    assert_eq!(run("\\p{Emoji}", "u", "ab😀"), "2,4");
    assert_eq!(run("\\p{ID_Start}+", "u", "1ab字_"), "1,4");
    assert_eq!(run("\\p{ID_Continue}+", "u", "-ab1_-"), "1,5");
    // An astral code point is one match.
    assert_eq!(run("^\\p{So}$", "u", "😀"), "0,2");
    assert_eq!(run("^\\p{L}$", "iu", "A"), "0,1");
    // Without `u`, `\p` is the letter p.
    assert_eq!(run("\\p{L}", "", "p{L}"), "0,4");
    // A name that is not known, or not well made, is a SyntaxError.
    for bad in [
        "\\p{Foo}", "\\p{lu}", "\\p{Script}", "\\p{General_Category}", "\\p{Latin}", "\\p{sc=Foo}", "\\p{sc=latin}", "\\p{L", "\\p", "\\p{}", "\\p{ L}", "\\p{L }", "\\p{sc=}",
        "\\p{=Latin}", "\\p{sc=Latin=x}", "\\P{Foo}", "[\\p{Foo}]", "\\p{Block=Basic_Latin}", "\\p{Alphabetic=Yes}", "\\p{Other_Alphabetic}", "\\p{L-}", "[\\p{L}-z]", "\\pL",
    ] {
        assert!(matches!(Regex::from_str(bad, "u"), Err(CompileError::Syntax { .. })), "{bad}");
    }
}

#[test]
fn compile_bounds() {
    let lim = |p: &str| Regex::from_str(p, "").err();
    assert!(matches!(lim("a{4294967295}"), Some(CompileError::Limit(_))));
    assert!(matches!(lim("(?:){4294967295}"), Some(CompileError::Limit(_))));
    assert!(matches!(lim("((a{300}){300}){300}"), Some(CompileError::Limit(_))));
    assert!(matches!(lim(&"(".repeat(MAX_DEPTH + 1)), Some(CompileError::Limit(_))));
    assert!(Regex::from_str(&format!("{}a{}", "(".repeat(MAX_DEPTH), ")".repeat(MAX_DEPTH)), "").is_ok());
    assert!(matches!(lim(&"a".repeat(MAX_PATTERN_UNITS + 1)), Some(CompileError::Limit(_))));
    assert!(Regex::from_str(&"a".repeat(MAX_PATTERN_UNITS), "").is_ok());
    // Each paren level may add up to four tree levels (group, concatenation, repeat): still fits.
    let nest = format!("{}a{}", "(a".repeat(MAX_DEPTH), ")*".repeat(MAX_DEPTH));
    let re = Regex::from_str(&nest, "").unwrap();
    assert_eq!(fmt(re.exec(&u("aaa"), 0, &mut Scratch::new()).unwrap()).split(';').next(), Some("0,3"));
    let many = "()".repeat(MAX_GROUPS + 1);
    assert!(matches!(lim(&many), Some(CompileError::Limit(_))));
    let classes = "[\\u0000-\\uffff]".repeat(100);
    assert!(matches!(Regex::from_str(&classes, "i").err(), Some(CompileError::Limit(_))));
}

#[test]
fn run_bounds() {
    let mut sc = Scratch::new();
    let evil = Regex::from_str("(a*)*b", "").unwrap();
    let a = vec![b'a' as u16; 40];
    // The cap of one run: a run of this pattern needs more than 50 M steps.
    assert_eq!(evil.exec(&a, 0, &mut sc).err(), Some(ExecError::Steps));
    assert!(sc.steps > MAX_STEPS);
    // The fuel: it ends the run first, and the caller sees `Fuel`, not "no match".
    let mut fuel = 1000;
    assert_eq!(evil.exec_fuel(&a, 0, &mut sc, &mut fuel).err(), Some(ExecError::Fuel));
    assert_eq!(fuel, 0);
    // The backtrack stack.
    let re = Regex::from_str("(?:a|b)*c", "").unwrap();
    assert_eq!(re.exec(&vec![b'a' as u16; 1 << 22], 0, &mut sc).err(), Some(ExecError::Stack));
    // A limit is never "no match": the same pattern with room answers.
    assert!(evil.exec(&a[..10], 0, &mut sc).unwrap().is_none());
    // Hostile pattern shapes do not overflow the Rust stack (matching is iterative).
    let deep = format!("{}a{}", "(?:".repeat(MAX_DEPTH), ")*".repeat(MAX_DEPTH));
    let re = Regex::from_str(&deep, "").unwrap();
    let _ = re.exec(&u("aaaa"), 0, &mut sc);
}

#[test]
fn fuel_is_charged_in_blocks_of_the_steps_of_the_run() {
    let mut sc = Scratch::new();
    let re = Regex::from_str("(?:a|b)*$", "").unwrap();
    let input = vec![b'a' as u16; 100_000];
    let mut fuel = u64::MAX;
    let m = re.exec_fuel(&input, 0, &mut sc, &mut fuel).unwrap().unwrap();
    assert_eq!(m.spans[0], Some((0, 100_000)));
    assert_eq!(u64::MAX - fuel, (sc.steps >> 8) * FUEL_PER_BLOCK);
    // A run that costs less than a block costs no fuel.
    let mut fuel = 5;
    assert!(Regex::from_str("a", "").unwrap().exec_fuel(&u("xxa"), 0, &mut sc, &mut fuel).unwrap().is_some());
    assert_eq!(fuel, 5);
}

#[test]
fn a_one_byte_input_gives_the_same_spans_as_a_two_byte_input() {
    let mut sc = Scratch::new();
    for (p, f, s) in [("(\\w+)\\s(\\w+)", "", "hello world"), ("^(.+?)e$", "", "agree"), ("(?<=a)b", "", "ab"), ("[a-c]+", "i", "xxABCxx"), ("\\bfoo\\b", "", "a foo b")] {
        let re = Regex::from_str(p, f).unwrap();
        let (mut a, mut b) = (vec![0u32; 2 * (re.group_count() + 1)], vec![0u32; 2 * (re.group_count() + 1)]);
        let bytes: Vec<u8> = s.bytes().collect();
        let mut fuel = u64::MAX;
        let r1 = exec(&re.0, &bytes, 0, &mut sc, &mut fuel, &mut a).unwrap();
        let r2 = exec(&re.0, &u(s), 0, &mut sc, &mut fuel, &mut b).unwrap();
        assert_eq!((r1, a), (r2, b), "{p} {s}");
    }
}

#[test]
fn the_image_is_flat_and_small() {
    // The program of a short pattern: header, first set, a few instructions. No pointer, so a `Blob` can hold it.
    let w = compile(&u("^(.+?)e$"), "").unwrap();
    assert!(w.len() < 32, "{}", w.len());
    assert_eq!(image_groups(&w), 1);
    assert_eq!(image_flags(&w), Flags::default());
    assert_eq!(image_flags(&compile(&u("a"), "dgimsy").unwrap()).bits(), 0b1001_1111);
    assert_eq!(Flags::from_bits(Flags::parse("gv").unwrap().bits()), Flags::parse("gv").unwrap());
}

#[test]
fn scratch_is_reusable() {
    let re = Regex::from_str("(a)|b", "").unwrap();
    let mut sc = Scratch::new();
    for _ in 0..3 {
        assert_eq!(fmt(re.exec(&u("b"), 0, &mut sc).unwrap()), "0,1;-");
        assert_eq!(fmt(re.exec(&u("a"), 0, &mut sc).unwrap()), "0,1;0,1");
    }
    assert!(sc.retained_bytes() > 0);
    sc.release();
    assert_eq!(sc.retained_bytes(), 0);
}

#[test]
fn canon_matches_spec_samples() {
    assert_eq!(canon(b'a' as u16), b'A' as u16);
    assert_eq!(canon(0xDF), 0xDF); // ß: uppercase is "SS"
    assert_eq!(canon(0x17F), 0x17F); // ſ would map to ASCII S: kept
    assert_eq!(canon(0x212A), 0x212A);
    assert_eq!(canon(0x3C2), 0x3A3); // ς -> Σ
    assert_eq!(canon(0xD800), 0xD800);
}
#[test]
fn search_work_honors_the_fuel() {
    let re = Regex::from_str("a", "").unwrap();
    let mut sc = Scratch::new();
    // The scan for the first unit is charged: a long input of units that cannot start a match costs fuel.
    let mut fuel = 3;
    assert_eq!(re.exec_fuel(&vec![b'b' as u16; 10_000], 0, &mut sc, &mut fuel).err(), Some(ExecError::Fuel));
}

#[test]
fn backreference_comparisons_consume_budget() {
    let re = Regex::from_str(r"(a{1000})\1", "y").unwrap();
    let input = vec![b'a' as u16; 2000];
    let mut sc = Scratch::new();
    let mut fuel = FUEL_PER_BLOCK * 5;
    assert_eq!(re.exec_fuel(&input, 0, &mut sc, &mut fuel).err(), Some(ExecError::Fuel));
    assert!(re.exec(&input, 0, &mut sc).unwrap().is_some());
}
