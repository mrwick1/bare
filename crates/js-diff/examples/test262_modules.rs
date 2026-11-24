//! test262 modules and `import()` on the old engine or on the new one (`bare-vex`). The module methods are the ones of `bare-js-api`
//! (`Engine`), so the same code loads a graph on both. Only the start of a realm, the jobs and the state of a promise are per engine.
//!
//!   test262_modules ENGINE ROOT SKIPFILE LISTFILE [STEPS]
//!
//! Same arguments, output and skip rules as `test262_run.rs` (a test with the `module` flag is run here). A module test is run as the
//! root module of a graph that the host loads by relative path; `negative.phase` (parse, resolution, runtime) is checked.
use bare_js::eval::{Error as OldError, EvalLimits, Exec, Interp};
use bare_js::heap::Heap;
use bare_js::value::Value as OldValue;
use bare_vex::interp::{Exit, Vm};
use bare_vex::value::Value as NewValue;
use bare_js_api::{Abort, Engine, Error};
use bare_js_diff::HOST;
use std::collections::HashMap;
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Component, Path, PathBuf};

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
            } else if key == "negative" && let Some(v) = t.strip_prefix("phase:") {
                m.phase = Some(v.trim().to_string());
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

/// Why a step failed.
#[derive(Debug)]
enum Fail {
    Throw(String, String),
    Other(String),
}

enum Status {
    Pending,
    Done,
    Rejected(Fail),
}

type Raw<R> = <<R as Rt>::E as Engine>::Raw;

/// One realm of one engine.
trait Rt: Sized {
    type E: Engine;
    /// A realm with the host script, then `code` as the main script (a throw of it is the error).
    fn start(code: &str, steps: u64) -> Result<Self, Fail>;
    fn e(&mut self) -> &mut Self::E;
    /// Run the jobs.
    fn drain(&mut self) -> Result<(), Fail>;
    fn status(&mut self, p: &Raw<Self>) -> Status;
    /// What `print` collected.
    fn log(&mut self) -> String;
    /// A thrown value as the name of its constructor and its message (`__thrown` of the host script).
    fn describe(&mut self, v: Raw<Self>) -> Fail;
    fn fail(&mut self, e: Error<'_>) -> Fail {
        match e {
            Error::Throw(l) => {
                let v = self.e().raw(l);
                self.describe(v)
            }
            Error::Abort(Abort::Unsupported(w)) => Fail::Other(format!("unsupported: {w}")),
            Error::Abort(Abort::Limit(l)) => Fail::Other(format!("limit: {l}")),
        }
    }
}

struct Old(Interp<'static>);

fn old_fail(it: &mut Interp<'static>, e: OldError) -> Fail {
    match e {
        OldError::Throw(v) => old_describe(it, v),
        OldError::Unsupported(u) => Fail::Other(format!("unsupported: {}", u.what)),
        OldError::Limit(l) => Fail::Other(format!("limit: {l:?}")),
        e => Fail::Other(format!("internal: {e:?}")),
    }
}

fn old_describe(it: &mut Interp<'static>, v: OldValue) -> Fail {
    let text = it.run_script("__thrown").and_then(|f| it.call(f, OldValue::Undefined, &[v])).map(|t| it.display(t));
    match text {
        Ok(t) => split(&t),
        Err(_) => Fail::Other("__thrown failed".into()),
    }
}

fn split(t: &str) -> Fail {
    let (n, m) = t.split_once('\u{1}').unwrap_or((t, ""));
    Fail::Throw(n.into(), m.into())
}

impl Rt for Old {
    type E = Interp<'static>;

    fn start(code: &str, steps: u64) -> Result<Self, Fail> {
        let limits = EvalLimits { max_steps: steps, max_call_depth: 1000, max_traversal_depth: 512, max_statement_frames: 4096, exec: Exec::Tree, ..Default::default() };
        let mut it = Interp::realm(Heap::default(), limits).map_err(|e| Fail::Other(format!("realm: {e:?}")))?;
        it.run_script(&format!("{HOST}\nvar __engine = 'old';")).map_err(|e| Fail::Other(format!("host: {e:?}")))?;
        match it.run_script(code) {
            Ok(_) => Ok(Old(it)),
            Err(e) => Err(old_fail(&mut it, e)),
        }
    }

    fn e(&mut self) -> &mut Self::E {
        &mut self.0
    }

    fn drain(&mut self) -> Result<(), Fail> {
        self.0.drain_microtasks().map(|_| ()).map_err(|e| old_fail(&mut self.0, e))
    }

    fn status(&mut self, p: &OldValue) -> Status {
        match self.0.promise_status(*p) {
            Some(bare_js::eval::PromiseStatus::Rejected(v)) => Status::Rejected(old_describe(&mut self.0, v)),
            Some(bare_js::eval::PromiseStatus::Pending) => Status::Pending,
            _ => Status::Done,
        }
    }

    fn log(&mut self) -> String {
        self.0.run_script("__finish").and_then(|f| self.0.call(f, OldValue::Undefined, &[])).map(|t| self.0.display(t)).unwrap_or_default()
    }

    fn describe(&mut self, v: OldValue) -> Fail {
        old_describe(&mut self.0, v)
    }
}

struct New(Vm);

fn new_describe(vm: &mut Vm, v: NewValue) -> Fail {
    let f = vm.global("__thrown");
    match vm.call(f, NewValue::UNDEFINED, &[v]) {
        Ok(t) => split(&vm.display(t, false)),
        Err(_) => Fail::Other("__thrown failed".into()),
    }
}

impl Rt for New {
    type E = bare_vex::interp_rt::Realm;

    fn start(code: &str, steps: u64) -> Result<Self, Fail> {
        let mut vm = bare_vex::script::load_with(&format!("{HOST}\nvar __engine = 'new';"), code).map_err(|e| match e.strip_prefix("SyntaxError: ") {
            Some(m) => Fail::Throw("SyntaxError".into(), m.into()),
            None => Fail::Other(e),
        })?;
        vm.fuel = steps;
        match vm.run_main(0) {
            Ok(_) => Ok(New(vm)),
            Err(Exit::Throw(v)) => Err(new_describe(&mut vm, v)),
            Err(Exit::Fuel) => Err(Fail::Other("limit: the budget ran out".into())),
        }
    }

    fn e(&mut self) -> &mut Self::E {
        &mut self.0.realm
    }

    fn drain(&mut self) -> Result<(), Fail> {
        match self.0.drain_microtasks(&mut |_, _| {}) {
            Ok(_) => Ok(()),
            Err(_) => Err(Fail::Other("limit: drain".into())),
        }
    }

    fn status(&mut self, p: &NewValue) -> Status {
        match bare_vex::interp_rt::Realm::promise_status(&self.0.realm, *p) {
            Some((2, v)) => Status::Rejected(new_describe(&mut self.0, v)),
            Some((0, _)) => Status::Pending,
            _ => Status::Done,
        }
    }

    fn log(&mut self) -> String {
        let f = self.0.global("__finish");
        match self.0.call(f, NewValue::UNDEFINED, &[]) {
            Ok(t) => self.0.display(t, false),
            Err(_) => String::new(),
        }
    }

    fn describe(&mut self, v: NewValue) -> Fail {
        new_describe(&mut self.0, v)
    }
}

#[derive(Default)]
struct Graph {
    ids: HashMap<PathBuf, u32>,
    paths: HashMap<u32, PathBuf>,
}

enum Load<R: Rt> {
    /// A syntax error or a failed resolution of a file. The value is the thrown one, when there is one.
    Js(Fail, Option<Raw<R>>),
    Missing(String),
}

fn join(base: &Path, spec: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for c in base.parent().unwrap_or(Path::new("")).join(spec).components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

fn load<R: Rt>(rt: &mut R, g: &mut Graph, path: &Path) -> Result<u32, Load<R>> {
    if let Some(&m) = g.ids.get(path) {
        return Ok(m);
    }
    let src = std::fs::read_to_string(path).map_err(|e| Load::Missing(format!("{}: {e}", path.display())))?;
    let r = rt.e().module_parse(&src, &path.to_string_lossy());
    let m = match r {
        Ok(m) => m,
        Err(e) => {
            let raw = if let Error::Throw(l) = &e { Some(rt.e().raw(*l)) } else { None };
            return Err(Load::Js(rt.fail(e), raw));
        }
    };
    g.ids.insert(path.to_path_buf(), m);
    g.paths.insert(m, path.to_path_buf());
    let mut deps = Vec::new();
    for r in rt.e().module_requests(m) {
        deps.push(load(rt, g, &join(path, &r))?);
    }
    rt.e().module_set_requested(m, &deps);
    Ok(m)
}

/// Run the jobs, and load what `import()` asks, until nothing is left.
fn pump<R: Rt>(rt: &mut R, g: &mut Graph, script: &Path) -> Result<(), Fail> {
    loop {
        rt.drain()?;
        let reqs = rt.e().take_dynamic_imports();
        if reqs.is_empty() {
            return Ok(());
        }
        for r in reqs {
            let from = r.referrer.and_then(|m| g.paths.get(&m).cloned()).unwrap_or_else(|| script.to_path_buf());
            let res = match load(rt, g, &join(&from, &r.specifier)) {
                Ok(m) => rt.e().finish_dynamic_import(&r.promise, m),
                Err(Load::Js(f, Some(raw))) => {
                    let _ = f;
                    let l = rt.e().raw_local(&raw);
                    rt.e().reject_dynamic_import(&r.promise, l)
                }
                Err(Load::Js(f, None)) => return Err(f),
                Err(Load::Missing(msg)) => rt.e().fail_dynamic_import(&r.promise, &msg),
            };
            if let Err(e) = res {
                return Err(rt.fail(e));
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

    fn skipped(&self, m: &Meta, src: &str) -> Option<String> {
        for rule in &self.skip {
            let hit = if let Some(f) = rule.strip_prefix("include:") {
                m.includes.iter().any(|i| i == f)
            } else if let Some(s) = rule.strip_prefix("text:") {
                src.contains(s)
            } else {
                m.features.iter().any(|f| f == rule)
            };
            if hit {
                return Some(rule.clone());
            }
        }
        None
    }

    fn exec<R: Rt>(&mut self, m: &Meta, path: &str, src: &str, strict: bool) -> Result<(), String> {
        let raw = m.flags.iter().any(|f| f == "raw");
        let is_async = m.flags.iter().any(|f| f == "async");
        let module = m.flags.iter().any(|f| f == "module");
        let mut code = String::new();
        if strict && !module {
            code.push_str("\"use strict\";\n");
        }
        if !raw {
            let mut files = vec!["assert.js".to_string(), "sta.js".to_string()];
            if is_async {
                files.push("doneprintHandle.js".into());
            }
            files.extend(m.includes.iter().cloned());
            for f in files {
                code.push_str(&self.harness(&f)?);
                code.push('\n');
            }
        }
        if !module {
            code.push_str(src);
        }
        let script = Path::new(&self.root).join(path);
        let mut g = Graph::default();
        // `phase` says where a module test failed; a classic test has one phase.
        let mut phase = "runtime";
        let outcome: Result<Option<R>, Fail> = (|| {
            let mut rt = R::start(&code, self.steps)?;
            if module {
                phase = "parse";
                let root = match load(&mut rt, &mut g, &script) {
                    Ok(root) => root,
                    Err(Load::Js(f, _)) => {
                        if g.ids.contains_key(&script) {
                            phase = "resolution";
                        }
                        return Err(f);
                    }
                    Err(Load::Missing(msg)) => return Err(Fail::Other(msg)),
                };
                phase = "resolution";
                if let Err(e) = rt.e().module_link(root) {
                    return Err(rt.fail(e));
                }
                phase = "runtime";
                let p = match rt.e().module_evaluate(root) {
                    Ok(l) => rt.e().raw(l),
                    Err(e) => return Err(rt.fail(e)),
                };
                pump(&mut rt, &mut g, &script)?;
                match rt.status(&p) {
                    Status::Rejected(f) => return Err(f),
                    Status::Pending if !is_async => return Err(Fail::Other("module evaluation did not finish".into())),
                    _ => {}
                }
            } else {
                pump(&mut rt, &mut g, &script)?;
            }
            Ok(Some(rt))
        })();
        let (rt, thrown) = match outcome {
            Ok(rt) => (rt, None),
            Err(Fail::Throw(n, m)) => (None, Some((n, m))),
            Err(Fail::Other(s)) => return Err(s),
        };
        match (&m.negative, thrown) {
            (Some(t), Some((name, msg))) if module && m.phase.as_deref().is_some_and(|p| p != phase) => return Err(format!("expected {t} in phase {}, got {name} in {phase}: {msg}", m.phase.as_deref().unwrap_or(""))),
            (Some(t), Some((name, _))) if *t == name => return Ok(()),
            (Some(t), Some((name, msg))) => return Err(format!("expected {t}, got {name}: {msg}")),
            (Some(t), None) => return Err(format!("expected {t}, nothing thrown")),
            (None, Some((name, msg))) => return Err(format!("{name}: {msg}")),
            (None, None) => {}
        }
        if is_async && let Some(mut rt) = rt {
            let log = rt.log();
            if log.contains("Test262:AsyncTestFailure") {
                return Err(log.replace('\n', " "));
            }
            if !log.contains("Test262:AsyncTestComplete") {
                return Err(format!("async test did not finish: {}", log.replace('\n', " ")));
            }
        }
        Ok(())
    }

    fn run<R: Rt>(&mut self, path: &str) -> Result<(), String> {
        let src = std::fs::read_to_string(format!("{}/{path}", self.root)).map_err(|e| format!("read: {e}"))?;
        let m = meta(&src);
        let has = |f: &str| m.flags.iter().any(|x| x == f);
        let modes: &[bool] = if has("module") || has("raw") || has("noStrict") { &[false] } else if has("onlyStrict") { &[true] } else { &[false, true] };
        for &strict in modes {
            self.exec::<R>(&m, path, &src, strict).map_err(|e| if modes.len() > 1 { format!("{} {e}", if strict { "strict:" } else { "sloppy:" }) } else { e })?;
        }
        Ok(())
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 5 || !matches!(a[1].as_str(), "old" | "new") {
        eprintln!("usage: test262_modules old|new ROOT SKIPFILE LISTFILE [STEPS]");
        std::process::exit(2);
    }
    let old = a[1] == "old";
    let skip = std::fs::read_to_string(&a[3]).expect("skip file").lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("allow:")).map(String::from).collect();
    let list = std::fs::read_to_string(&a[4]).expect("list file");
    let mut ctx = Ctx { root: a[2].clone(), skip, steps: a.get(5).and_then(|s| s.parse().ok()).unwrap_or(20_000_000), harness: HashMap::new() };
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
                    None => match catch_unwind(AssertUnwindSafe(|| if old { ctx.run::<Old>(path) } else { ctx.run::<New>(path) })) {
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
