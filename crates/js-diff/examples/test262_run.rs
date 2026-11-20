//! Run test262 tests on the old engine or on the new one (`bare-js2`). The same tests, harness, includes, metadata and checks as
//! `crates/js/examples/test262_run.rs`; `tools/js/test262_js2.sh` runs it for both engines and compares them.
//!
//!   test262_run ENGINE ROOT SKIPFILE LISTFILE [STEPS]
//!
//! ENGINE is `old` or `new`. ROOT is a checkout of tc39/test262. LISTFILE has one test path (relative to ROOT) on each line.
//! SKIPFILE (`tools/fixtures/js/test262-js2-skip.txt`) has one rule on each line: `NAME` skips a test with that feature,
//! `include:FILE` one that includes that harness file, `text:STRING` one whose source holds STRING, and `allow:NAME` (once or more)
//! skips every test that has a feature that no `allow:` line names. A test with a `module` or `async` flag is always skipped: neither
//! engine runs them in this runner. STEPS is the budget of a test (default 20,000,000).
//! For each test it prints `START path`, then `PASS path`, `FAIL path<TAB>reason` or `SKIP path<TAB>reason`, and flushes. A process that
//! dies or hangs leaves a START with no result: the script marks that test and goes on.
//! A test with no flag runs twice, sloppy and strict (`"use strict";` first), and passes when both pass. A `raw` test has no harness.
//! The realm gets `print` and a small `$262` from `src/host.js`. BARE_JS_EXEC (tree, auto, byte) picks the mode of the old engine.

use bare_js_diff::{Engine, Out, run};
use std::collections::HashMap;
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};

#[derive(Default)]
struct Meta {
    includes: Vec<String>,
    flags: Vec<String>,
    features: Vec<String>,
    negative: Option<String>,
}

fn put(m: &mut Meta, key: &str, item: &str) {
    let item = item.trim().to_string();
    match key {
        "includes" => m.includes.push(item),
        "flags" => m.flags.push(item),
        "features" => m.features.push(item),
        _ => {}
    }
}

/// The YAML front matter, by hand: `key: [a, b]`, `key:` followed by `- a` lines, and `negative:` with `type`.
fn meta(src: &str) -> Meta {
    let mut m = Meta::default();
    let Some(a) = src.find("/*---") else { return m };
    let Some(b) = src[a..].find("---*/") else { return m };
    let (mut key, mut pending) = (String::new(), false);
    for line in src[a + 5..a + b].lines() {
        let t = line.trim();
        if pending || line.starts_with(char::is_whitespace) {
            if pending || t.starts_with('[') {
                for it in t.trim_start_matches('[').trim_end_matches(']').split(',') {
                    if !it.trim().is_empty() {
                        put(&mut m, &key, it);
                    }
                }
                pending = !t.contains(']');
            } else if let Some(i) = t.strip_prefix("- ") {
                put(&mut m, &key, i);
            } else if key == "negative" && let Some(v) = t.strip_prefix("type:") {
                m.negative = Some(v.trim().to_string());
            }
        } else if let Some((k, v)) = line.split_once(':') {
            key = k.trim().to_string();
            let v = v.trim();
            if v.starts_with('[') {
                for it in v.trim_start_matches('[').trim_end_matches(']').split(',') {
                    if !it.trim().is_empty() {
                        put(&mut m, &key, it);
                    }
                }
                pending = !v.contains(']');
            }
        }
    }
    m
}

struct Ctx {
    engine: Engine,
    root: String,
    skip: Vec<String>,
    steps: u64,
    harness: HashMap<String, String>,
}

impl Ctx {
    fn harness(&mut self, name: &str) -> Result<String, String> {
        if !self.harness.contains_key(name) {
            let text = std::fs::read_to_string(format!("{}/harness/{name}", self.root)).map_err(|e| format!("harness {name}: {e}"))?;
            self.harness.insert(name.to_string(), text);
        }
        Ok(self.harness[name].clone())
    }

    /// Why this test is not run, if it is not.
    fn skipped(&self, m: &Meta, src: &str) -> Option<String> {
        if let Some(f) = m.flags.iter().find(|f| *f == "module" || *f == "async") {
            return Some(format!("flag:{f}"));
        }
        for rule in &self.skip {
            let hit = if let Some(f) = rule.strip_prefix("include:") {
                m.includes.iter().any(|i| i == f)
            } else if let Some(s) = rule.strip_prefix("text:") {
                src.contains(s)
            } else if rule.starts_with("allow:") {
                false
            } else {
                m.features.iter().any(|f| f == rule)
            };
            if hit {
                return Some(rule.clone());
            }
        }
        let allowed = |f: &String| self.skip.iter().any(|r| r.strip_prefix("allow:") == Some(f));
        if self.skip.iter().any(|r| r.starts_with("allow:"))
            && let Some(f) = m.features.iter().find(|f| !allowed(f))
        {
            return Some(format!("feature:{f}"));
        }
        None
    }

    fn exec(&mut self, m: &Meta, src: &str, strict: bool) -> Result<(), String> {
        let mut code = String::new();
        if strict {
            code.push_str("\"use strict\";\n");
        }
        if !m.flags.iter().any(|f| f == "raw") {
            let mut files = vec!["assert.js".to_string(), "sta.js".to_string()];
            files.extend(m.includes.iter().cloned());
            for f in files {
                code.push_str(&self.harness(&f)?);
                code.push('\n');
            }
        }
        code.push_str(src);
        let thrown = match run(self.engine, &code, self.steps) {
            Out::Done(_) => None,
            Out::Threw { name, message } => Some((name, message)),
            Out::Unsupported(what) => return Err(format!("unsupported: {what}")),
            Out::Limit(l) => return Err(format!("limit: {l}")),
            Out::Internal(e) => return Err(format!("internal: {e}")),
        };
        match (&m.negative, thrown) {
            (Some(t), Some((name, _))) if *t == name => Ok(()),
            (Some(t), Some((name, msg))) => Err(format!("expected {t}, got {name}: {msg}")),
            (Some(t), None) => Err(format!("expected {t}, nothing thrown")),
            (None, Some((name, msg))) => Err(format!("{name}: {msg}")),
            (None, None) => Ok(()),
        }
    }

    fn run(&mut self, path: &str) -> Result<(), String> {
        let src = std::fs::read_to_string(format!("{}/{path}", self.root)).map_err(|e| format!("read: {e}"))?;
        let m = meta(&src);
        let has = |f: &str| m.flags.iter().any(|x| x == f);
        let modes: &[bool] = if has("raw") || has("noStrict") { &[false] } else if has("onlyStrict") { &[true] } else { &[false, true] };
        for &strict in modes {
            self.exec(&m, &src, strict).map_err(|e| if modes.len() > 1 { format!("{} {e}", if strict { "strict:" } else { "sloppy:" }) } else { e })?;
        }
        Ok(())
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let engine = a.get(1).and_then(|e| Engine::from_name(e));
    if a.len() < 5 || engine.is_none() {
        eprintln!("usage: test262_run old|new ROOT SKIPFILE LISTFILE [STEPS]");
        std::process::exit(2);
    }
    let skip = std::fs::read_to_string(&a[3]).expect("skip file").lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from).collect();
    let list = std::fs::read_to_string(&a[4]).expect("list file");
    let mut ctx = Ctx { engine: engine.unwrap(), root: a[2].clone(), skip, steps: a.get(5).and_then(|s| s.parse().ok()).unwrap_or(20_000_000), harness: HashMap::new() };
    std::panic::set_hook(Box::new(|_| {}));
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let out = std::io::stdout();
            for path in list.lines().filter(|l| !l.is_empty()) {
                let mut o = out.lock();
                writeln!(o, "START {path}").ok();
                o.flush().ok();
                drop(o);
                let skipped = std::fs::read_to_string(format!("{}/{path}", ctx.root)).ok().and_then(|s| ctx.skipped(&meta(&s), &s));
                let line = match skipped {
                    Some(why) => format!("SKIP {path}\t{why}"),
                    None => match catch_unwind(AssertUnwindSafe(|| ctx.run(path))) {
                        Ok(Ok(())) => format!("PASS {path}"),
                        Ok(Err(why)) => format!("FAIL {path}\t{}", why.replace(['\n', '\t'], " ").chars().take(200).collect::<String>()),
                        Err(_) => format!("FAIL {path}\tpanic"),
                    },
                };
                let mut o = out.lock();
                writeln!(o, "{line}").ok();
                o.flush().ok();
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
