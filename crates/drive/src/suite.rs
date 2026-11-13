//! A suite of related tests that share a login, run by K warm worker processes (`docs/design-suite.md`, step 7).
//!
//! `bare-drive --suite DIR [--workers K|auto]`. DIR holds `setup.steps` (the login; its last step is `save-state "${STATE}"`) and one
//! `NAME.steps` for each test. A shared test starts with `open URL state="${STATE}"`: it starts from the session that the setup saved.
//! The runner runs the setup once, in the first worker. A test is `own` when it needs its own login: the lint finds `sign out`,
//! `password` or `delete` in its steps (a comment does not count), and a line `# own` or `# shared` in the file decides by hand. The
//! runner replaces the first `open ... state=` line of an own test with the steps of the setup (without `save-state`), so the test logs
//! in itself. A test with no `state=` line runs as it is.
//!
//! Workers: each is a `bare-drive --suite-worker` process. It reads one test name for each line on stdin, runs the test in a new
//! `Session` that shares the one network client of the process (connections, reply cache) and the thread-local parse caches, and writes
//! one JSON line. Each `open` has its own storage and cookie jar, so a test starts clean. A test that fails ends its worker (the next
//! test gets a new process, as Playwright does), and so does a test that runs past `--test-timeout`. The coordinator gives the next
//! test to the first worker that is free (a queue, not a split in advance).
//!
//! How many workers (`pick_workers`): the smallest of the cores minus 4 (a fixture server, the coordinator and the other users of a
//! shared machine), half of the available memory divided by the peak RSS of the first worker after the setup, and the number of tests.

use crate::run::{Options, Redactor, Session};
use crate::steps;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Class {
    Shared,
    Own,
}

pub struct Test {
    pub name: String,
    pub text: String,
    pub class: Class,
    /// Why the test is own: the word that the lint found, or the mark.
    pub why: String,
}

pub struct Suite {
    pub setup: Option<String>,
    pub tests: Vec<Test>,
}

/// The words in a step that mean the test changes the account or ends the session, so it cannot share a login.
const OWN_WORDS: [&str; 3] = ["sign out", "password", "delete"];

/// Shared or own, and why. A line `# own` or `# shared` decides. Else a step with one of `OWN_WORDS` makes it own (a text rule, not a proof).
pub fn lint(text: &str) -> (Class, String) {
    let marks: Vec<&str> = text.lines().map(str::trim).filter_map(|l| l.strip_prefix('#')).map(str::trim).collect();
    if marks.contains(&"own") {
        return (Class::Own, "marked own".into());
    }
    if marks.contains(&"shared") {
        return (Class::Shared, "marked shared".into());
    }
    let steps = text.lines().map(str::trim).filter(|l| !l.starts_with('#')).collect::<Vec<_>>().join("\n").to_ascii_lowercase();
    match OWN_WORDS.iter().find(|w| steps.contains(**w)) {
        Some(w) => (Class::Own, format!("a step says {w}")),
        None => (Class::Shared, String::new()),
    }
}

/// The first step line of a test (not blank, not a comment): its index, when it is an `open` that starts from a saved session.
fn state_open(text: &str) -> Option<usize> {
    let (i, l) = text.lines().enumerate().map(|(i, l)| (i, l.trim())).find(|(_, l)| !l.is_empty() && !l.starts_with('#'))?;
    (l.starts_with("open ") && l.contains("state=")).then_some(i)
}

/// The test with its own login: the first `open ... state=` line becomes the steps of the setup, without `save-state`. A test with no
/// such line is returned as it is.
pub fn compose(test: &str, setup: &str) -> String {
    let Some(at) = state_open(test) else { return test.to_string() };
    let login: Vec<&str> = setup.lines().filter(|l| !l.trim().starts_with("save-state")).collect();
    let mut out: Vec<&str> = test.lines().collect();
    out.splice(at..=at, login);
    out.join("\n") + "\n"
}

pub fn load(dir: &Path) -> Result<Suite, String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "steps")).collect();
    files.sort();
    let (mut setup, mut tests) = (None, Vec::new());
    for p in files {
        let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        if name == "setup" {
            setup = Some(text);
        } else {
            let (class, why) = lint(&text);
            tests.push(Test { name, text, class, why });
        }
    }
    if tests.is_empty() {
        return Err(format!("{}: no test (NAME.steps)", dir.display()));
    }
    Ok(Suite { setup, tests })
}

/// What a test runs: its own text, or with its own login (`login_each`: every test; else the own tests).
fn text_of(t: &Test, setup: Option<&str>, login_each: bool) -> String {
    match setup {
        Some(s) if login_each || t.class == Class::Own => compose(&t.text, s),
        _ => t.text.clone(),
    }
}

/// One step file for each test, each with its own login (the cold run: a new process for each test).
pub fn emit_cold(dir: &Path, into: &Path) -> Result<usize, String> {
    let suite = load(dir)?;
    let setup = suite.setup.as_deref().ok_or("no setup.steps")?;
    std::fs::create_dir_all(into).map_err(|e| format!("{}: {e}", into.display()))?;
    for t in &suite.tests {
        std::fs::write(into.join(format!("{}.steps", t.name)), compose(&t.text, setup)).map_err(|e| e.to_string())?;
    }
    Ok(suite.tests.len())
}

/// The number of workers and the rule's numbers, as text. The peak RSS of a worker is a guess before the first worker has run.
pub fn pick_workers(cores: usize, avail_kib: u64, peak_kib: u64, tests: usize) -> (usize, String) {
    let k_cpu = cores.saturating_sub(4).max(1);
    // No data on the memory (a system without /proc/meminfo): no memory limit.
    let k_mem = if avail_kib == 0 { usize::MAX } else { (((avail_kib / 2) / peak_kib.max(1)).max(1)) as usize };
    let k = k_cpu.min(k_mem).min(tests).max(1);
    (k, format!("min(cores {cores} - 4 = {k_cpu}, half of {} MiB available / {} MiB peak = {}, tests {tests}) = {k}", avail_kib >> 10, peak_kib >> 10, if k_mem == usize::MAX { "no limit".into() } else { k_mem.to_string() }))
}

/// The peak RSS of a worker before one has run, for the rule above (KiB).
const GUESS_PEAK_KIB: u64 = 64 << 10;

/// The worker: one test name for each line on stdin, one JSON line for each test on stdout. Exits 1 after a failed test.
pub fn worker(dir: &Path, opt: &Options, vars: &HashMap<String, String>, red: &Redactor, login_each: bool, peak: fn() -> u64) -> i32 {
    let suite = match load(dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bare-drive: {e}");
            return 2;
        }
    };
    let net = bare_net::http::Client::new();
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let name = line.trim();
        let text = if name == "setup" { suite.setup.clone() } else { suite.tests.iter().find(|t| t.name == name).map(|t| text_of(t, suite.setup.as_deref(), login_each)) };
        let mut red = red.clone();
        let (mut ok, mut ms, mut n, mut failed) = (false, 0.0, 0, json!({"line": 0, "kind": "suite", "message": format!("no test {name}")}));
        if let Some(text) = text {
            match steps::parse(&text, vars) {
                Err(e) => failed = json!({"line": 0, "kind": "steps", "message": red.clean(&e)}),
                Ok(list) => {
                    let o = Options { size: opt.size, timeout: opt.timeout, out: opt.out.as_ref().map(|d| d.join(name)), window: false, progress: false };
                    if let Some(d) = &o.out {
                        let _ = std::fs::create_dir_all(d);
                    }
                    let mut s = Session::with_client(&o, net.fork());
                    for step in &list {
                        if !s.step(step, &o, &mut red) {
                            break;
                        }
                    }
                    let report = s.finish();
                    if let Some(d) = &o.out {
                        let _ = std::fs::write(d.join("result.json"), report.json(&red));
                    }
                    (ok, ms, n) = (report.ok, report.ms, report.steps.len());
                    if let Some((line, f, _)) = &report.failure {
                        failed = json!({"line": line, "kind": f.kind, "message": red.clean(&f.message)});
                    }
                }
            }
        }
        let mut out = json!({"name": name, "ok": ok, "ms": ms, "steps": n, "peak_kib": peak()});
        if !ok {
            out["failure"] = failed;
        }
        let mut so = std::io::stdout().lock();
        if writeln!(so, "{out}").and_then(|_| so.flush()).is_err() || !ok {
            return if ok { 0 } else { 1 };
        }
    }
    0
}

pub struct Config {
    pub dir: PathBuf,
    pub out: PathBuf,
    /// `None`: choose by the rule of `pick_workers`.
    pub workers: Option<usize>,
    pub test_limit: Duration,
    /// Count the instructions of each worker (`perf stat`).
    pub perf: bool,
    pub login_each: bool,
    pub only: Option<String>,
    /// The options that a worker needs as they came (`--var`, `--secrets`, `--size`, `--timeout`).
    pub args: Vec<String>,
    pub cores: usize,
    pub avail_kib: u64,
    pub quiet: bool,
}

struct Worker {
    id: usize,
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
    perf: Option<PathBuf>,
    tests: usize,
    peak_kib: u64,
}

fn spawn(cfg: &Config, id: usize) -> Result<Worker, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let perf = cfg.perf.then(|| cfg.out.join(format!("perf.{id}")));
    let mut c;
    if let Some(p) = &perf {
        c = Command::new("perf");
        c.args(["stat", "-x,", "-e", "instructions:u", "-o"]).arg(p).arg("--").arg(&exe);
    } else {
        c = Command::new(&exe);
    }
    c.arg("--suite-worker").arg(&cfg.dir).arg("--quiet").args(&cfg.args);
    if cfg.login_each {
        c.arg("--login-each");
    }
    let mut child = c.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().map_err(|e| format!("worker: {e}"))?;
    let (stdin, stdout) = (child.stdin.take().ok_or("worker stdin")?, child.stdout.take().ok_or("worker stdout")?);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for l in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(l).is_err() {
                break;
            }
        }
    });
    Ok(Worker { id, child, stdin, rx, perf, tests: 0, peak_kib: 0 })
}

/// Give a worker a test and wait for its line. `Err` is a worker that died or ran past the limit (it is killed).
fn exchange(w: &mut Worker, name: &str, limit: Duration) -> Result<Value, String> {
    writeln!(w.stdin, "{name}").and_then(|_| w.stdin.flush()).map_err(|e| format!("worker {} is gone: {e}", w.id))?;
    match w.rx.recv_timeout(limit) {
        Ok(l) => {
            let v: Value = serde_json::from_str(&l).map_err(|e| format!("bad line from worker {}: {e}", w.id))?;
            w.tests += 1;
            w.peak_kib = w.peak_kib.max(v["peak_kib"].as_u64().unwrap_or(0));
            Ok(v)
        }
        Err(RecvTimeoutError::Timeout) => {
            let _ = w.child.kill();
            Err(format!("the test ran past {} s; the worker was killed", limit.as_secs()))
        }
        Err(RecvTimeoutError::Disconnected) => Err(format!("worker {} died", w.id)),
    }
}

/// End a worker, wait for it and read its instruction count.
fn retire(mut w: Worker, why: &str) -> Value {
    drop(w.stdin);
    let _ = w.child.wait();
    let instructions = w.perf.and_then(|p| {
        let text = std::fs::read_to_string(&p).ok()?;
        let _ = std::fs::remove_file(&p);
        text.lines().find(|l| l.contains("instructions"))?.split(',').next()?.trim().parse::<u64>().ok()
    });
    json!({"worker": w.id, "tests": w.tests, "peak_kib": w.peak_kib, "instructions": instructions, "end": why})
}

struct Shared<'a> {
    cfg: &'a Config,
    tests: &'a [&'a Test],
    queue: Mutex<VecDeque<usize>>,
    results: Mutex<Vec<(usize, Value)>>,
    stats: Mutex<Vec<Value>>,
    next_id: AtomicUsize,
}

/// One worker thread: take a test, run it, and start a new process after a failure.
fn pull(sh: &Shared, mut w: Option<Worker>) {
    let mut error = None;
    loop {
        let Some(i) = sh.queue.lock().unwrap().pop_front() else { break };
        let t = sh.tests[i];
        if w.is_none() {
            match spawn(sh.cfg, sh.next_id.fetch_add(1, Ordering::SeqCst)) {
                Ok(n) => w = Some(n),
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        }
        let Some(wk) = w.as_mut() else { break };
        let id = wk.id;
        let (mut v, end) = match exchange(wk, &t.name, sh.cfg.test_limit) {
            Ok(v) if v["ok"] == true => (v, None),
            Ok(v) => (v, Some("a test failed")),
            Err(e) => (json!({"name": t.name, "ok": false, "ms": 0.0, "steps": 0, "failure": {"line": 0, "kind": "worker", "message": e}}), Some("died or timed out")),
        };
        v["worker"] = json!(id);
        v["class"] = json!(if sh.cfg.login_each || t.class == Class::Own { "own" } else { "shared" });
        sh.results.lock().unwrap().push((i, v));
        if let Some(why) = end
            && let Some(old) = w.take()
        {
            let s = retire(old, why);
            sh.stats.lock().unwrap().push(s);
        }
    }
    if let Some(old) = w.take() {
        let s = retire(old, "no more tests");
        sh.stats.lock().unwrap().push(s);
    }
    if let Some(e) = error {
        // A worker that could not start: the tests it would have run stay in the queue for the others; none left means they are lost.
        eprintln!("bare-drive: {e}");
    }
}

/// Run the suite. Exit code: 0 all passed, 1 a test failed, 2 a bad suite.
pub fn run(cfg: &Config) -> i32 {
    let t0 = Instant::now();
    let suite = match load(&cfg.dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bare-drive: {e}");
            return 2;
        }
    };
    let tests: Vec<&Test> = suite.tests.iter().filter(|t| cfg.only.as_ref().is_none_or(|o| t.name.contains(o.as_str()))).collect();
    let shared = tests.iter().any(|t| t.class == Class::Shared && state_open(&t.text).is_some()) && !cfg.login_each;
    if shared && suite.setup.is_none() {
        eprintln!("bare-drive: a test starts from a saved login, and {} has no setup.steps", cfg.dir.display());
        return 2;
    }
    if tests.is_empty() {
        eprintln!("bare-drive: no test matches --only");
        return 2;
    }
    let _ = std::fs::create_dir_all(&cfg.out);
    let _ = std::fs::remove_file(cfg.out.join("state.json"));
    let mut first = match spawn(cfg, 0) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("bare-drive: {e}");
            return 2;
        }
    };
    // The setup runs in the first worker, which then keeps the caches that it warmed.
    let mut setup = Value::Null;
    if shared || (suite.setup.is_some() && !cfg.login_each && tests.iter().any(|t| t.class == Class::Own)) {
        let r = exchange(&mut first, "setup", cfg.test_limit).unwrap_or_else(|e| json!({"name": "setup", "ok": false, "ms": 0.0, "failure": {"line": 0, "kind": "worker", "message": e}}));
        if r["ok"] != true {
            let s = retire(first, "the setup failed");
            return finish(cfg, &suite, &tests, vec![], vec![s], r, 0, "the setup failed: no test ran".into(), t0);
        }
        setup = r;
    }
    let (k, rule) = match cfg.workers {
        Some(k) => (k.clamp(1, tests.len()), format!("--workers {k}")),
        None => pick_workers(cfg.cores, cfg.avail_kib, if first.peak_kib > 0 { first.peak_kib } else { GUESS_PEAK_KIB }, tests.len()),
    };
    let sh = Shared { cfg, tests: &tests, queue: Mutex::new((0..tests.len()).collect()), results: Mutex::new(vec![]), stats: Mutex::new(vec![]), next_id: AtomicUsize::new(1) };
    let sh = &sh;
    std::thread::scope(|sc| {
        let mut first = Some(first);
        for _ in 0..k {
            let w = first.take();
            sc.spawn(move || pull(sh, w));
        }
    });
    let results = std::mem::take(&mut *sh.results.lock().unwrap());
    let stats = std::mem::take(&mut *sh.stats.lock().unwrap());
    finish(cfg, &suite, &tests, results, stats, setup, k, rule, t0)
}

#[allow(clippy::too_many_arguments)]
fn finish(cfg: &Config, _suite: &Suite, tests: &[&Test], mut results: Vec<(usize, Value)>, mut stats: Vec<Value>, setup: Value, k: usize, rule: String, t0: Instant) -> i32 {
    let _ = std::fs::remove_file(cfg.out.join("state.json"));
    results.sort_by_key(|(i, _)| *i);
    stats.sort_by_key(|s| s["worker"].as_u64());
    let ran: Vec<&Value> = results.iter().map(|(_, v)| v).collect();
    let passed = ran.iter().filter(|v| v["ok"] == true).count();
    // A test that never ran (the setup failed, or no worker could start) is a failure.
    let failed = tests.len() - passed;
    let instr: Option<u64> = stats.iter().map(|s| s["instructions"].as_u64()).sum();
    let owns = tests.iter().filter(|t| cfg.login_each || t.class == Class::Own).count();
    let mut rows: Vec<Value> = results.iter().map(|(i, v)| json!({"test": tests[*i].name, "class": v["class"], "why": tests[*i].why, "ok": v["ok"], "ms": v["ms"], "worker": v["worker"], "failure": v.get("failure")})).collect();
    for (i, t) in tests.iter().enumerate() {
        if !results.iter().any(|(j, _)| *j == i) {
            rows.push(json!({"test": t.name, "class": null, "why": t.why, "ok": false, "ms": 0.0, "worker": null, "failure": {"kind": "not run", "message": "the test did not run"}}));
        }
    }
    let ok = failed == 0 && (setup.is_null() || setup["ok"] == true);
    let report = json!({
        "ok": ok,
        "tests": tests.len(), "passed": passed, "failed": failed, "workers": k, "workers_rule": rule,
        "wall_ms": t0.elapsed().as_secs_f64() * 1000.0, "setup": setup, "instructions": instr,
        "logins_expected": (!setup.is_null()) as usize + owns,
        "results": rows, "worker_stats": stats,
    });
    let text = report.to_string();
    let _ = std::fs::write(cfg.out.join("suite.json"), format!("{text}\n"));
    println!("{text}");
    if !cfg.quiet {
        eprintln!("{:<5} {:<22} {:>8}  {:>6} {}", "", "test", "ms", "worker", "login");
        for r in &rows {
            let line = format!("{:<5} {:<22} {:>8.0}  {:>6} {}", if r["ok"] == true { "ok" } else { "FAIL" }, r["test"].as_str().unwrap_or(""), r["ms"].as_f64().unwrap_or(0.0), r["worker"].as_u64().map_or("-".into(), |w| w.to_string()), r["class"].as_str().unwrap_or(""));
            eprintln!("{line}");
            if let Some(m) = r["failure"]["message"].as_str() {
                eprintln!("      {}:{} {m}", r["failure"]["kind"].as_str().unwrap_or(""), r["failure"]["line"]);
            }
        }
        let peaks: Vec<String> = stats.iter().map(|s| format!("w{}:{}M", s["worker"], s["peak_kib"].as_u64().unwrap_or(0) >> 10)).collect();
        eprintln!("{passed}/{} passed, {k} workers ({rule}), wall {:.0} ms, instructions {}, peak RSS {}", tests.len(), t0.elapsed().as_secs_f64() * 1000.0, instr.map_or("n/a".into(), |i| format!("{:.0} M", i as f64 / 1e6)), peaks.join(" "));
    }
    !ok as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETUP: &str = "# login\nopen ${BASE}/login\ntype css=\"input[name=password]\" \"x\"\nclick role=button name=\"Sign in\"\nsave-state \"${STATE}\"\n";

    #[test]
    fn the_lint_reads_steps_and_not_comments_and_a_mark_decides() {
        let read = "# a test about the password page\nopen ${BASE}/home state=\"${STATE}\"\nclick role=link name=\"Page 1\"\n";
        assert_eq!(lint(read).0, Class::Shared);
        assert_eq!(lint("open ${BASE}/home state=\"${STATE}\"\nclick role=button name=\"Sign out\"\n"), (Class::Own, "a step says sign out".into()));
        assert_eq!(lint("open x\ntype css=\"input[name=password]\" \"a\"\n").0, Class::Own);
        assert_eq!(lint("open x\nclick role=button name=\"Delete\"\n").0, Class::Own);
        assert_eq!(lint("# own\nopen x state=\"s\"\n"), (Class::Own, "marked own".into()));
        assert_eq!(lint("# shared\nopen x state=\"s\"\nclick role=button name=\"Sign out\"\n"), (Class::Shared, "marked shared".into()));
    }

    #[test]
    fn an_own_test_gets_the_setup_in_place_of_its_state_line() {
        let t = "# t\nopen ${BASE}/home state=\"${STATE}\"\nexpect url~\"/home\"\n";
        let c = compose(t, SETUP);
        assert_eq!(c, "# t\n# login\nopen ${BASE}/login\ntype css=\"input[name=password]\" \"x\"\nclick role=button name=\"Sign in\"\nexpect url~\"/home\"\n");
        assert!(!c.contains("save-state") && !c.contains("state="));
        // No state line: unchanged.
        let own = "open ${BASE}/login\nexpect url~\"/login\"\n";
        assert_eq!(compose(own, SETUP), own);
    }

    #[test]
    fn workers_follow_the_smallest_limit() {
        // 16 cores, plenty of memory: cores - 4.
        assert_eq!(pick_workers(16, 12 << 20, 25 << 10, 40).0, 12);
        // Memory limits: 100 MiB available, half is 50 MiB, 25 MiB each.
        assert_eq!(pick_workers(16, 100 << 10, 25 << 10, 40).0, 2);
        // Few tests, few cores, no data.
        assert_eq!(pick_workers(16, 12 << 20, 25 << 10, 3).0, 3);
        assert_eq!(pick_workers(2, 12 << 20, 25 << 10, 40).0, 1);
        // Memory unknown: no memory limit.
        assert_eq!(pick_workers(16, 0, 0, 40).0, 12);
        // A worker bigger than the memory: one.
        assert_eq!(pick_workers(16, 10 << 10, 25 << 10, 40).0, 1);
    }
}
