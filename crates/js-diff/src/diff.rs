//! The differential runner: the same script on both engines, and a comparison of what each one printed and the value it left in a
//! global. It exits non-zero on a difference (`examples/diff_run.rs`, `tests/diff.rs`).
//!
//! The corpus has three parts: the `eval_load` programs (`crates/js2/examples/data/eval_*.js`, the same text as the old engine's
//! `eval_load`), the programs of `crates/js2/src/script_tests.rs` (the cases that fit on one line: `("source", "global", "expected")`),
//! and the files of `tools/fixtures/js/diff/`. The expected texts of the script tests are not used: the old engine is the reference.
use crate::{Engine, Out, run};
use std::path::{Path, PathBuf};

/// A program, and the global whose value is compared (`""`: only what `print` collected).
pub struct Case {
    pub name: String,
    pub src: String,
    pub global: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Same,
    /// One engine refuses a construct that it does not build. This is not a difference.
    Skipped(String),
    Differs,
}

pub struct Row {
    pub name: String,
    /// The first 120 characters of the program, for the report.
    pub head: String,
    pub old: Out,
    pub new: Out,
    pub verdict: Verdict,
}

/// The step budget of a program: both engines stop a loop that does not end. The corpus is far below it.
pub const STEPS: u64 = 500_000_000;

/// Compare two results. A thrown value counts by the name of its constructor only: the texts of the messages differ between engines.
pub fn judge(old: &Out, new: &Out) -> Verdict {
    match (old, new) {
        (Out::Unsupported(w), _) => Verdict::Skipped(format!("old engine: {w}")),
        (_, Out::Unsupported(w)) => Verdict::Skipped(format!("new engine: {w}")),
        (Out::Threw { name: a, .. }, Out::Threw { name: b, .. }) if a == b => Verdict::Same,
        (a, b) if a == b => Verdict::Same,
        _ => Verdict::Differs,
    }
}

pub fn run_case(c: &Case) -> Row {
    let src = if c.global.is_empty() { c.src.clone() } else { format!("{}\nvar __name = {:?};", c.src, c.global) };
    let (old, new) = (run(Engine::Old, &src, STEPS), run(Engine::New, &src, STEPS));
    Row { name: c.name.clone(), head: c.src.chars().take(120).collect(), verdict: judge(&old, &new), old, new }
}

/// The root of the repository.
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A Rust string literal at the start of `s`: its text and the rest. Only the escapes that the tests use.
fn literal(s: &str) -> Option<(String, &str)> {
    let mut it = s.strip_prefix('"')?.char_indices();
    let mut out = String::new();
    while let Some((i, c)) = it.next() {
        match c {
            '"' => return Some((out, &s[i + 2..])),
            '\\' => match it.next()?.1 {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '0' => out.push('\0'),
                c @ ('\\' | '"' | '\'') => out.push(c),
                'u' => {
                    let mut hex = String::new();
                    it.next()?;
                    for (_, c) in it.by_ref() {
                        if c == '}' {
                            break;
                        }
                        hex.push(c);
                    }
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                _ => return None,
            },
            c => out.push(c),
        }
    }
    None
}

/// The one-line cases of `script_tests.rs`: `("source", "global", "expected"),`.
pub fn script_tests(text: &str) -> Vec<Case> {
    let mut v = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let Some((src, rest)) = literal(line.trim_start().strip_prefix('(').unwrap_or("")) else { continue };
        let Some((global, rest)) = rest.strip_prefix(", ").and_then(literal) else { continue };
        let Some((_, rest)) = rest.strip_prefix(", ").and_then(literal) else { continue };
        if rest.starts_with(')') {
            v.push(Case { name: format!("script_tests:{}", n + 1), src, global });
        }
    }
    v
}

/// All the cases, in a fixed order.
pub fn corpus(root: &Path) -> Vec<Case> {
    let mut v = Vec::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join("crates/js2/examples/data")).expect("eval_load data").map(|e| e.unwrap().path()).collect();
    files.sort();
    for f in files.iter().filter(|f| f.file_name().unwrap().to_string_lossy().starts_with("eval_")) {
        let src = bare_js2::script::eval_program(&std::fs::read_to_string(f).unwrap());
        v.push(Case { name: format!("eval_load:{}", f.file_stem().unwrap().to_string_lossy().trim_start_matches("eval_")), src, global: "__result".into() });
    }
    v.extend(script_tests(&std::fs::read_to_string(root.join("crates/js2/src/script_tests.rs")).expect("script_tests.rs")));
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join("tools/fixtures/js/diff")).expect("corpus").map(|e| e.unwrap().path()).collect();
    files.sort();
    for f in files.iter().filter(|f| f.extension().is_some_and(|e| e == "js")) {
        v.push(Case { name: format!("corpus:{}", f.file_stem().unwrap().to_string_lossy()), src: std::fs::read_to_string(f).unwrap(), global: String::new() });
    }
    v
}

/// `tools/fixtures/js/diff-known.txt`: `NAME<TAB>reason`. A known difference does not fail the run. A name that no longer differs does.
pub fn known(root: &Path) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(root.join("tools/fixtures/js/diff-known.txt")).unwrap_or_default();
    text.lines().filter(|l| !l.is_empty() && !l.starts_with('#')).map(|l| l.split_once('\t').map_or((l.to_string(), String::new()), |(a, b)| (a.to_string(), b.to_string()))).collect()
}

pub struct Report {
    pub rows: Vec<Row>,
    /// Differences that are not in the known list.
    pub new_diffs: Vec<String>,
    /// Names in the known list that no longer differ (or are not in the corpus).
    pub stale: Vec<String>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.new_diffs.is_empty() && self.stale.is_empty()
    }
}

pub fn run_all(cases: &[Case], known: &[(String, String)]) -> Report {
    let rows: Vec<Row> = cases.iter().map(run_case).collect();
    let new_diffs = rows.iter().filter(|r| r.verdict == Verdict::Differs && !known.iter().any(|k| k.0 == r.name)).map(|r| r.name.clone()).collect();
    let stale = known.iter().filter(|k| !rows.iter().any(|r| r.name == k.0 && r.verdict == Verdict::Differs)).map(|k| k.0.clone()).collect();
    Report { rows, new_diffs, stale }
}

fn short(o: &Out) -> String {
    let t = format!("{o:?}");
    if t.chars().count() > 160 { format!("{}...", t.chars().take(160).collect::<String>()) } else { t }
}

/// The text of a report: the counts, and each difference with the two results.
pub fn print(r: &Report, verbose: bool) {
    let n = |v: fn(&Verdict) -> bool| r.rows.iter().filter(|x| v(&x.verdict)).count();
    for x in &r.rows {
        match &x.verdict {
            Verdict::Differs => {
                let tag = if r.new_diffs.contains(&x.name) { "DIFF" } else { "known" };
                println!("{tag} {}  {:?}\n  old: {}\n  new: {}", x.name, x.head, short(&x.old), short(&x.new));
                if let (Out::Done(a), Out::Done(b)) = (&x.old, &x.new) {
                    // Every output line that differs, so that a corpus file with a known difference shows a second one.
                    let (a, b): (Vec<_>, Vec<_>) = (a.lines().collect(), b.lines().collect());
                    for i in (0..a.len().max(b.len())).filter(|&i| a.get(i) != b.get(i)) {
                        let cut = |l: Option<&&str>| l.unwrap_or(&"").chars().take(80).collect::<String>();
                        println!("    line {}: old {:?} / new {:?}", i + 1, cut(a.get(i)), cut(b.get(i)));
                    }
                }
            }
            Verdict::Skipped(why) if verbose => println!("skip {}: {why}", x.name),
            _ => {}
        }
    }
    for s in &r.stale {
        println!("STALE {s}: in diff-known.txt, but it does not differ now; remove it");
    }
    println!(
        "{} programs: {} same, {} skipped (an engine refuses a construct), {} differ ({} new, {} known), {} stale",
        r.rows.len(),
        n(|v| *v == Verdict::Same),
        n(|v| matches!(v, Verdict::Skipped(_))),
        n(|v| *v == Verdict::Differs),
        r.new_diffs.len(),
        n(|v| *v == Verdict::Differs) - r.new_diffs.len(),
        r.stale.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_cases_are_read_with_their_escapes() {
        let t = "    (\"var r = \\\"a\\\" + 1;\\n\", \"r\", \"a1\"),\n    (\"multi\n  line\", \"r\", \"x\"),\n    (\"var q = 2;\", \"q\", \"2\"),\n";
        let v = script_tests(t);
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].src.as_str(), v[0].global.as_str(), v[0].name.as_str()), ("var r = \"a\" + 1;\n", "r", "script_tests:1"));
        assert_eq!(v[1].name, "script_tests:4");
    }

    #[test]
    fn a_thrown_value_counts_by_its_name() {
        let t = |n: &str, m: &str| Out::Threw { name: n.into(), message: m.into() };
        assert_eq!(judge(&t("TypeError", "a"), &t("TypeError", "b")), Verdict::Same);
        assert_eq!(judge(&t("TypeError", "a"), &t("RangeError", "a")), Verdict::Differs);
        assert_eq!(judge(&Out::Done("1".into()), &Out::Unsupported("x".into())), Verdict::Skipped("new engine: x".into()));
    }
}
