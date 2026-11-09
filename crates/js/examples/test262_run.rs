//! Run test262 tests with the bare-js evaluator. `tools/js/test262_check.sh` calls it for each directory.
//!
//!   test262_run ROOT SKIPFILE LISTFILE [STEPS]
//!
//! ROOT is a checkout of tc39/test262. LISTFILE has one test path (relative to ROOT) on each line. SKIPFILE
//! (`tools/fixtures/js/test262-skip.txt`) has one rule on each line: `NAME` skips a test with that feature, `include:FILE`
//! one that includes that harness file, `text:STRING` one whose source holds STRING. STEPS is the step budget of a test
//! (default 20,000,000): it is the timeout, and it does not depend on the machine.
//! For each test it prints `START path`, then `PASS path`, `FAIL path<TAB>reason` or `SKIP path<TAB>reason`, and flushes.
//! A process that dies (stack overflow, abort) or hangs leaves a START with no result: the script marks that test and goes on.
//! Metadata: `flags` (onlyStrict, noStrict, raw, module, async), `includes`, `features`, `negative`. A test with no flag
//! runs twice, sloppy and strict (`"use strict";` first), and passes when both pass. A `module` test runs once as a module
//! (the harness files run first as classic scripts); the host loads the imported files from the checkout, and `negative`
//! `phase` is checked (`parse`: loading, `resolution`: linking, `runtime`: evaluation). `import()` loads files the same way.
//! The realm gets `print` and a small `$262` (see PRELUDE) as non-enumerable globals. BARE_JS_EXEC is not read yet.

use bare_js::eval::*;
use bare_js::heap::Heap;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// `print` collects the async protocol lines. `$262` has no agent, realm, detach or evalScript (tests that need them are
/// listed in the skip file). The last expression is a function that names the constructor of a thrown value.
const PRELUDE: &str = r#"
Object.defineProperty(globalThis, '__log', { value: [], writable: true, configurable: true });
Object.defineProperty(globalThis, 'print', { value: function print() { __log.push(Array.prototype.join.call(arguments, ' ')); }, writable: true, configurable: true });
Object.defineProperty(globalThis, '$262', { value: { global: globalThis, gc() {}, agent: {} }, writable: true, configurable: true });
(function (e) { try { return e.constructor.name; } catch (_) { return 'unknown'; } });
"#;

#[derive(Default)]
struct Meta {
    includes: Vec<String>,
    flags: Vec<String>,
    features: Vec<String>,
    negative: Option<String>,
    phase: Option<String>,
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

/// The YAML front matter, by hand: `key: [a, b]`, `key:` followed by `- a` lines, and `negative:` with `phase` and `type`.
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
                    if !it.trim().is_empty() { put(&mut m, &key, it); }
                }
                pending = !t.contains(']');
            } else if let Some(i) = t.strip_prefix("- ") {
                put(&mut m, &key, i);
            } else if key == "negative" && let Some(v) = t.strip_prefix("type:") {
                m.negative = Some(v.trim().to_string());
            } else if key == "negative" && let Some(v) = t.strip_prefix("phase:") {
                m.phase = Some(v.trim().to_string());
            }
        } else if let Some((k, v)) = line.split_once(':') {
            key = k.trim().to_string();
            let v = v.trim();
            if v.starts_with('[') {
                for it in v.trim_start_matches('[').trim_end_matches(']').split(',') {
                    if !it.trim().is_empty() { put(&mut m, &key, it); }
                }
                pending = !v.contains(']');
            }
        }
    }
    m
}

/// The modules the host has loaded: file path to module id, and back.
#[derive(Default)]
struct Graph {
    ids: HashMap<PathBuf, u32>,
    paths: HashMap<u32, PathBuf>,
}

/// A failure while the host loads a module: the engine threw (a parse error), or a file is missing.
enum Load {
    Js(Error),
    Missing(String),
}

/// `base`/`spec` without `.` and `..` parts.
fn join(base: &Path, spec: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for c in base.parent().unwrap_or(Path::new("")).join(spec).components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => { out.pop(); }
            c => out.push(c),
        }
    }
    out
}

/// Load the module at `path` and the modules that it imports (the files that exist below the checkout).
fn load(it: &mut Interp, g: &mut Graph, path: &Path) -> Result<u32, Load> {
    if let Some(&m) = g.ids.get(path) {
        return Ok(m);
    }
    let src = std::fs::read_to_string(path).map_err(|e| Load::Missing(format!("{}: {e}", path.display())))?;
    let m = it.module_parse(&src, &path.to_string_lossy()).map_err(Load::Js)?;
    g.ids.insert(path.to_path_buf(), m);
    g.paths.insert(m, path.to_path_buf());
    let mut deps = Vec::new();
    for r in it.module_requests(m) {
        deps.push(load(it, g, &join(path, &r))?);
    }
    it.module_set_requested(m, &deps);
    Ok(m)
}

/// Run the jobs and answer the `import()` calls until nothing is left.
fn pump(it: &mut Interp, g: &mut Graph, script: &Path) -> Result<(), Error> {
    loop {
        it.drain_microtasks()?;
        let reqs = it.take_dynamic_imports();
        if reqs.is_empty() {
            return Ok(());
        }
        for r in reqs {
            let from = r.referrer.and_then(|m| g.paths.get(&m).cloned()).unwrap_or_else(|| script.to_path_buf());
            match load(it, g, &join(&from, &r.specifier)) {
                Ok(m) => it.finish_dynamic_import(r.promise, m)?,
                Err(Load::Js(e)) => it.reject_dynamic_import(r.promise, e)?,
                Err(Load::Missing(msg)) => it.fail_dynamic_import(r.promise, &msg)?,
            }
        }
    }
}

struct Ctx {
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
        for rule in &self.skip {
            let hit = if let Some(f) = rule.strip_prefix("include:") {
                m.includes.iter().any(|i| i == f)
            } else if let Some(s) = rule.strip_prefix("text:") {
                src.contains(s)
            } else {
                m.features.iter().any(|f| f == rule)
            };
            if hit { return Some(rule.clone()); }
        }
        None
    }

    fn exec(&mut self, m: &Meta, path: &str, src: &str, strict: bool) -> Result<(), String> {
        let raw = m.flags.iter().any(|f| f == "raw");
        let is_async = m.flags.iter().any(|f| f == "async");
        let module = m.flags.iter().any(|f| f == "module");
        let mut code = String::new();
        if strict && !module { code.push_str("\"use strict\";\n"); }
        if !raw {
            let mut files = vec!["assert.js".to_string(), "sta.js".to_string()];
            if is_async { files.push("doneprintHandle.js".into()); }
            files.extend(m.includes.iter().cloned());
            for f in files {
                code.push_str(&self.harness(&f)?);
                code.push('\n');
            }
        }
        let limits = EvalLimits { max_steps: self.steps, max_call_depth: 1000, max_traversal_depth: 512, max_statement_frames: 4096, ..Default::default() };
        let mut it = Interp::realm(Heap::default(), limits).map_err(|e| format!("realm: {e:?}"))?;
        let ctor_name = it.run_script(PRELUDE).map_err(|e| format!("prelude: {e:?}"))?;
        let script = Path::new(&self.root).join(path);
        let mut g = Graph::default();
        // `phase` says where a module test failed; a classic test has one phase.
        let mut phase = "runtime";
        let outcome = if module {
            it.run_script(&code).map_err(|e| format!("harness: {e:?}")).and_then(|_| {
                phase = "parse";
                // A syntax error in the test file is `parse`; one in a file that it imports is found while the host resolves it.
                let r = load(&mut it, &mut g, &script).map_err(|l| match l { Load::Js(e) => Ok(e), Load::Missing(msg) => Err(msg) });
                let root = match r {
                    Ok(root) => root,
                    Err(Ok(e)) => {
                        if g.ids.contains_key(&script) { phase = "resolution"; }
                        return Ok(Err(e));
                    }
                    Err(Err(msg)) => return Err(msg),
                };
                phase = "resolution";
                if let Err(e) = it.module_link(root) { return Ok(Err(e)); }
                phase = "runtime";
                let p = match it.module_evaluate(root) { Ok(p) => p, Err(e) => return Ok(Err(e)) };
                if let Err(e) = pump(&mut it, &mut g, &script) { return Ok(Err(e)); }
                Ok(match it.promise_status(bare_js::value::Value::Object(p)) {
                    Some(PromiseStatus::Rejected(v)) => Err(Error::Throw(v)),
                    Some(PromiseStatus::Pending) if !is_async => return Err("module evaluation did not finish".into()),
                    _ => Ok(()),
                })
            })?
        } else {
            code.push_str(src);
            it.run_script(&code).map(|_| ()).and_then(|_| pump(&mut it, &mut g, &script))
        };
        let thrown = match outcome {
            Ok(()) => None,
            Err(Error::Throw(v)) => {
                let name = it.call(ctor_name, bare_js::value::Value::Undefined, &[v]).map(|n| it.display(n)).unwrap_or_else(|_| "unknown".into());
                let msg = if matches!(v, bare_js::value::Value::Object(_)) { it.get_str(v, "message") } else { it.display(v) };
                Some((name, msg))
            }
            Err(Error::Unsupported(u)) => return Err(format!("unsupported: {}", u.what)),
            Err(Error::Limit(l)) => return Err(format!("limit: {l:?}")),
            Err(e) => return Err(format!("internal: {e:?}")),
        };
        match (&m.negative, thrown) {
            (Some(t), Some((name, msg))) if module && m.phase.as_deref().is_some_and(|p| p != phase) => return Err(format!("expected {t} in phase {}, got {name} in {phase}: {msg}", m.phase.as_deref().unwrap_or(""))),
            (Some(t), Some((name, _))) if *t == name => return Ok(()),
            (Some(t), Some((name, msg))) => return Err(format!("expected {t}, got {name}: {msg}")),
            (Some(t), None) => return Err(format!("expected {t}, nothing thrown")),
            (None, Some((name, msg))) => return Err(format!("{name}: {msg}")),
            (None, None) => {}
        }
        if is_async {
            it.drain_microtasks().map_err(|e| format!("drain: {e:?}"))?;
            let log = it.run_script("__log.join('\\n')").map(|v| it.display(v)).map_err(|e| format!("log: {e:?}"))?;
            if log.contains("Test262:AsyncTestFailure") { return Err(log.replace('\n', " ")); }
            if !log.contains("Test262:AsyncTestComplete") { return Err(format!("async test did not finish: {}", log.replace('\n', " "))); }
        }
        Ok(())
    }

    fn run(&mut self, path: &str) -> Result<(), String> {
        let src = std::fs::read_to_string(format!("{}/{path}", self.root)).map_err(|e| format!("read: {e}"))?;
        let m = meta(&src);
        let has = |f: &str| m.flags.iter().any(|x| x == f);
        let modes: &[bool] = if has("module") || has("raw") || has("noStrict") { &[false] } else if has("onlyStrict") { &[true] } else { &[false, true] };
        for &strict in modes {
            self.exec(&m, path, &src, strict).map_err(|e| if modes.len() > 1 { format!("{} {e}", if strict { "strict:" } else { "sloppy:" }) } else { e })?;
        }
        Ok(())
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        eprintln!("usage: test262_run ROOT SKIPFILE LISTFILE [STEPS]");
        std::process::exit(2);
    }
    let skip = std::fs::read_to_string(&a[2]).expect("skip file").lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from).collect();
    let list = std::fs::read_to_string(&a[3]).expect("list file");
    let mut ctx = Ctx { root: a[1].clone(), skip, steps: a.get(4).and_then(|s| s.parse().ok()).unwrap_or(20_000_000), harness: HashMap::new() };
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
