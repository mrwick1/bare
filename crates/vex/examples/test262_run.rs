//! Run a list of classic Test262 tests through Vex. See tools/js/test262_js2.sh.

use bare_vex::interp::Exit;
use bare_vex::value::Value;
use std::collections::HashMap;
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// How a run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Out {
    /// The script ran. The text is what `print` collected.
    Done(String),
    /// The script threw (a syntax error is a throw of `SyntaxError`). `name` is the name of the constructor.
    Threw { name: String, message: String },
    /// The engine refuses a construct (a feature that is not built).
    Unsupported(String),
    /// The step budget ran out, or another limit.
    Limit(String),
    /// A panic or an error of the engine itself.
    Internal(String),
}

fn threw(text: &str) -> Out {
    let (name, message) = text.split_once('\u{1}').unwrap_or((text, ""));
    Out::Threw { name: name.into(), message: message.into() }
}

fn run(code: &str, steps: u64) -> Out {
    let mut vm = match bare_vex::script::load_with(include_str!("test262_host.inc.js"), code) {
        Ok(vm) => vm,
        Err(e) if e.starts_with("not supported yet") => return Out::Unsupported(e),
        Err(e) if e.starts_with("SyntaxError: ") => return Out::Threw { name: "SyntaxError".into(), message: e["SyntaxError: ".len()..].into() },
        Err(e) => return Out::Internal(e),
    };
    vm.fuel = steps;
    let end = |vm: &mut bare_vex::interp::Vm, name: &str, args: &[Value]| -> Result<String, String> {
        let f = vm.global(name);
        let v = vm.call(f, Value::UNDEFINED, args).map_err(|e| format!("{name} {}", if matches!(e, Exit::Fuel) { "ran out of budget" } else { "threw" }))?;
        Ok(vm.display(v, false))
    };
    // The host runs the jobs when the script ends: the stack is empty.
    match vm.run_main(0).and_then(|v| vm.drain_microtasks(&mut |_, _| {}).map(|_| v).map_err(|_| Exit::Fuel)) {
        Ok(_) => end(&mut vm, "__finish", &[]).map_or_else(Out::Internal, Out::Done),
        // A function that holds an unbuilt construct fails at its first call (lazy compile): an Error of that text is "unsupported" too.
        Err(Exit::Throw(e)) => end(&mut vm, "__thrown", &[e]).map_or_else(Out::Internal, |t| match threw(&t) {
            Out::Threw { name, message } if name == "Error" && message.starts_with("not supported yet") => Out::Unsupported(message),
            o => o,
        }),
        Err(Exit::Fuel) => Out::Limit("the budget ran out".into()),
    }
}

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
    fn skipped(&self, path: &str, m: &Meta, src: &str) -> Option<String> {
        if m.flags.iter().any(|f| f == "module") {
            return Some("flag:module".into());
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
        let allowed = |f: &String| self.skip.iter().filter_map(|r| r.strip_prefix("allow:")).any(|rule| {
            match rule.split_once(' ') {
                Some((feature, prefix)) => feature == f && path.starts_with(prefix),
                None => rule == f,
            }
        });
        if self.skip.iter().any(|r| r.starts_with("allow:"))
            && let Some(f) = m.features.iter().find(|f| !allowed(f))
        {
            return Some(format!("feature:{f}"));
        }
        None
    }

    fn exec(&mut self, m: &Meta, src: &str, strict: bool) -> Result<(), String> {
        let is_async = m.flags.iter().any(|f| f == "async");
        let mut code = String::new();
        if strict {
            code.push_str("\"use strict\";\n");
        }
        if !m.flags.iter().any(|f| f == "raw") {
            let mut files = vec!["assert.js".to_string(), "sta.js".to_string()];
            files.extend(m.includes.iter().cloned());
            if is_async && !files.iter().any(|f| f == "doneprintHandle.js") {
                files.push("doneprintHandle.js".into());
            }
            for f in files {
                code.push_str(&self.harness(&f)?);
                code.push('\n');
            }
        }
        code.push_str(src);
        let thrown = match run(&code, self.steps) {
            Out::Done(log) if is_async && m.negative.is_none() => {
                if let Some(i) = log.find("Test262:AsyncTestFailure:") {
                    return Err(log[i + "Test262:AsyncTestFailure:".len()..].lines().next().unwrap_or("").to_string());
                }
                if !log.contains("Test262:AsyncTestComplete") {
                    return Err("$DONE was not called".into());
                }
                None
            }
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
    if !(4..=5).contains(&a.len()) {
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
                let skipped = std::fs::read_to_string(format!("{}/{path}", ctx.root)).ok().and_then(|s| ctx.skipped(path, &meta(&s), &s));
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
