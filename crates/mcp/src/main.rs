//! `bare-mcp [--workspace N]`: an MCP server over stdio for end-to-end tests in `bare`.
//!
//! An agent (Claude Code, Codex) starts this program and talks JSON-RPC 2.0 to it: one JSON object on each line of stdin and stdout.
//! The tools use the test driver (`bare-drive`), so each click and key goes through the same hit testing and input path as a user.
//!
//! - `run_steps`: run a whole step file and give its report.
//! - `ensure_session`, `save_state`: a logged-in live page from a saved session (in memory by default), or a new login when it is no longer valid.
//! - `open`, `step`, `close`: a live session. `open` loads a page, each `step` call runs more step lines on the same page, and `close`
//!   ends it. Each call gives the report of its own steps only, so the server keeps no old records.
//!
//! Headless by default. With `window: true` the page shows in a Wayland window. `--workspace N` puts that window on Hyprland workspace
//! N without taking the focus.
//!
//! Only JSON goes to stdout: the protocol owns it. Progress and errors go to stderr. JS: like a Node script that reads `readline`
//! lines from `process.stdin` and writes `JSON.stringify(reply)` lines to `process.stdout`.

use bare_drive::{DownloadState, Downloads};
use bare_drive::locate::{Locator, Text};
use bare_drive::run::{Event, Options, Redactor, Report, Session, StorageState};
use bare_drive::snap::{self, Mode};
use bare_drive::steps;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::time::Duration;

/// A live session: the page, its options and its secrets, kept between tool calls.
struct Live {
    session: Session,
    opt: Options,
    red: Redactor,
    vars: HashMap<String, String>,
    /// Step lines run so far: each new call numbers its lines after them, so the line numbers in reports stay unique.
    lines: usize,
    /// The files that the session saved from downloads (see `bare::downloads`).
    dl: Downloads,
    /// The downloads that an `act` receipt already named.
    told: std::collections::HashSet<usize>,
}

struct Server {
    workspace: Option<u32>,
    /// Live sessions by name (`session`, default `main`): one page each, side by side.
    live: HashMap<String, Live>,
    /// Saved sessions by name: cookies and storage, in this process only. A file is written only when a call gives `file`.
    saved: HashMap<String, StorageState>,
    /// A counter for the capture folders of this process.
    runs: u32,
}

fn main() {
    let mut workspace = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--workspace" => match args.next().and_then(|n| n.parse().ok()) {
                Some(n) => workspace = Some(n),
                None => return eprintln!("bare-mcp: --workspace N needs a number"),
            },
            _ => return eprintln!("usage: bare-mcp [--workspace N]"),
        }
    }
    bare_net::parallel::limit_malloc_arenas(false);
    let mut server = Server { workspace, live: HashMap::new(), saved: HashMap::new(), runs: 0 };
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => server.handle(&msg),
            Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}})),
        };
        // A notification (no `id`) gets no reply.
        if let Some(r) = reply {
            if writeln!(out, "{r}").and_then(|_| out.flush()).is_err() {
                break;
            }
        }
    }
}

impl Server {
    fn handle(&mut self, msg: &Value) -> Option<Value> {
        let id = msg.get("id")?.clone();
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let result = match msg.get("method").and_then(Value::as_str).unwrap_or("") {
            "initialize" => Ok(json!({
                // Answer with the version that the client asked for: this server uses only the tools part, which has not changed.
                "protocolVersion": params.get("protocolVersion").cloned().unwrap_or(json!("2025-06-18")),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "bare-mcp", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Call ensure_session before a test to get a logged-in live page: it restores a saved session or logs in again. When this server fails or behaves wrong, file an issue: a GitHub issue on mrwick1/bare with the label bare-mcp. Give the tool call, the result and the expected result.",
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => Ok(self.call(&params)),
            m => Err(json!({"code": -32601, "message": format!("unknown method {m}")})),
        };
        Some(match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
        })
    }

    /// Run a tool. A tool error is a result with `isError`, not a protocol error, so the agent sees the message.
    fn call(&mut self, params: &Value) -> Value {
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let done = match params.get("name").and_then(Value::as_str).unwrap_or("") {
            "run_steps" => self.run_steps(&args),
            "open" => self.open(&args),
            "step" => self.step(&args),
            "save_state" => self.save_state(&args),
            "ensure_session" => self.ensure_session(&args),
            "close" => self.close(&args),
            "snapshot" => self.snapshot(&args),
            "find" => self.find(&args),
            "read" => self.read(&args),
            "links" => self.links(&args),
            "act" => self.act(&args),
            "downloads" => self.downloads(&args),
            "eval" => self.eval(&args),
            "console" => self.console(&args),
            "network" => self.network(&args),
            "diagnose" => self.diagnose(&args),
            "sessions" => Ok(self.sessions()),
            "storage" => self.storage(&args),
            "screenshot" => return self.screenshot(&args).unwrap_or_else(|e| json!({"content": [{"type": "text", "text": e}], "isError": true})),
            n => Err(format!("unknown tool {n}")),
        };
        match done {
            Ok(text) => json!({"content": [{"type": "text", "text": text}]}),
            Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
        }
    }

    fn run_steps(&mut self, args: &Value) -> Result<String, String> {
        let src = args.get("steps").and_then(Value::as_str).ok_or("steps (the text of a step file) is required")?;
        let (opt, mut red, vars) = self.options(args)?;
        let list = steps::parse(src, &vars).map_err(|e| red.clean(&e))?;
        self.place_window(&opt)?;
        let report = bare_drive::run::run(&list, &opt, &mut red);
        Ok(with_out(report.json(&red), &opt, true))
    }

    fn open(&mut self, args: &Value) -> Result<String, String> {
        let url = args.get("url").and_then(Value::as_str).ok_or("url is required")?;
        let state = match args.get("state").and_then(Value::as_str) {
            Some(n) => Some(self.saved.get(n).cloned().ok_or(format!("no saved session named {n}"))?),
            None => None,
        };
        if args.get("snapshot").and_then(Value::as_bool) == Some(true) {
            let mut a = args.clone();
            a["compact"] = json!(true);
            let (rep, ok) = self.start(&a, url, state)?;
            if !ok {
                return Err(rep);
            }
            let mark = self.arm(args)?;
            let snap = self.snapshot(args)?;
            let errs = self.script_errors(args, mark, &rep)?;
            return Ok(rep + &errs + "\n" + &snap);
        }
        let (rep, ok) = self.start(args, url, state)?;
        self.arm(args)?;
        if ok { Ok(rep) } else { Err(rep) }
    }

    /// Start a live session on URL, from a saved session or with an empty profile. One live session at a time: this ends the old one
    /// (and closes its window). Gives the report of the open step and whether it passed.
    fn start(&mut self, args: &Value, url: &str, state: Option<StorageState>) -> Result<(String, bool), String> {
        let name = session_name(args);
        let _ = self.close(args);
        let (opt, red, vars) = self.options(args)?;
        self.place_window(&opt)?;
        let mut session = Session::new(&opt);
        session.ready_wait = true;
        let dir = args.get("downloads_dir").and_then(Value::as_str).map_or_else(|| std::env::temp_dir().join(format!("bare-mcp-{}-downloads", std::process::id())), std::path::PathBuf::from);
        let dl = Downloads::new(dir);
        session.set_downloads(dl.clone());
        if let Some(st) = state {
            session.preload(st);
        }
        self.live.insert(name, Live { session, opt, red, vars, lines: 0, dl, told: Default::default() });
        let (mut rep, ok) = self.run_live(args, &format!("open {url}"))?;
        // A redirect to another address: the agent must see where the page really is.
        let end = self.live_mut(args)?.session.url().unwrap_or_default();
        let bare = |u: &str| u.split('#').next().unwrap_or("").trim_end_matches('/').to_string();
        if ok && bare(&end) != bare(url) {
            let to = self.live_mut(args)?.red.clean(&bare_drive::run::safe_url(&end));
            if args.get("compact").and_then(Value::as_bool) == Some(true) {
                rep.push_str(&format!("\nredirected: {to}"));
            } else {
                rep.insert_str(rep.len() - 1, &format!(",\"redirected\":{}", json!(to)));
            }
        }
        Ok((rep, ok))
    }

    /// Save the live session under a name, and in a file when asked. Gives counts only: the values never leave this process.
    fn save_state(&mut self, args: &Value) -> Result<String, String> {
        let name = args.get("name").and_then(Value::as_str).ok_or("name is required")?;
        self.store(args, name, args.get("file").and_then(Value::as_str))
    }

    fn store(&mut self, args: &Value, name: &str, file: Option<&str>) -> Result<String, String> {
        let live = self.live_mut(args)?;
        let st = live.session.export_state().ok_or("the page has no session")?;
        if let Some(f) = file {
            bare_drive::state::write(f, &st).map_err(|e| live.red.clean(&e))?;
        }
        let origins: std::collections::HashSet<&String> = st.local.iter().chain(&st.session).map(|(o, _)| o).collect();
        let reply = json!({"saved": name, "cookies": st.cookies.len(), "origins": origins.len()}).to_string();
        self.saved.insert(name.to_string(), st);
        Ok(reply)
    }

    /// Like `cy.session`: restore the saved session and run `check`; when there is none or the check fails, log in again with an empty
    /// profile, run `check` and save. The live session stays open on the checked page.
    fn ensure_session(&mut self, args: &Value) -> Result<String, String> {
        let get = |k: &str| args.get(k).and_then(Value::as_str).ok_or(format!("{k} is required"));
        let (name, url, login, check) = (get("name")?, get("url")?, get("login")?, get("check")?);
        let file = args.get("file").and_then(Value::as_str);
        let found = match self.saved.get(name) {
            Some(st) => Some(st.clone()),
            None => match file.filter(|f| std::path::Path::new(f).exists()) {
                Some(f) => Some(bare_drive::state::read(f)?),
                None => None,
            },
        };
        let had = found.is_some();
        if found.is_some() {
            self.start(args, url, found)?;
            let (rep, ok) = self.run_live(args, check)?;
            if ok {
                return Ok(with_session(rep, "restored"));
            }
        }
        // The open of a profile with no state, then the login and the check; a failure gives the report of the failing step.
        let (r, ok) = self.start(args, url, None)?;
        if !ok {
            return Err(r);
        }
        let mut rep = String::new();
        for src in [login, check] {
            let (r, ok) = self.run_live(args, src)?;
            if !ok {
                return Err(r);
            }
            rep = r;
        }
        self.store(args, name, file)?;
        Ok(with_session(rep, if had { "recreated" } else { "created" }))
    }

    fn step(&mut self, args: &Value) -> Result<String, String> {
        let src = args.get("steps").and_then(Value::as_str).ok_or("steps (one or more step lines) is required")?;
        Ok(self.run_live(args, src)?.0)
    }

    fn close(&mut self, args: &Value) -> Result<String, String> {
        let name = session_name(args);
        let live = self.live.remove(&name).ok_or(format!("no live session {name}: call open first"))?;
        Ok(with_out(live.session.finish().json(&live.red), &live.opt, true))
    }

    /// Run step lines on the live session until one fails, and give the report of these lines.
    fn run_live(&mut self, args: &Value, src: &str) -> Result<(String, bool), String> {
        let live = self.live_mut(args)?;
        let mut list = steps::parse(src, &live.vars).map_err(|e| live.red.clean(&e))?;
        for s in &mut list {
            s.line += live.lines;
        }
        live.lines += src.lines().count();
        for s in &list {
            if !live.session.step(s, &live.opt, &mut live.red) {
                break;
            }
        }
        let ok = live.session.rep.failure.is_none();
        if args.get("compact").and_then(Value::as_bool) == Some(true) {
            return Ok((compact(&live.session.drain(), &live.red), ok));
        }
        Ok((with_out(live.session.drain().json(&live.red), &live.opt, false), ok))
    }

    fn live_mut(&mut self, args: &Value) -> Result<&mut Live, String> {
        let name = session_name(args);
        self.live.get_mut(&name).ok_or(format!("no live session {name}: call open first"))
    }

    fn sessions(&mut self) -> String {
        if self.live.is_empty() {
            return "no live sessions".into();
        }
        let mut names: Vec<&String> = self.live.keys().collect();
        names.sort();
        let names: Vec<String> = names.into_iter().cloned().collect();
        names.into_iter().map(|n| { let u = self.live.get_mut(&n).and_then(|l| l.session.url()).unwrap_or_default(); format!("{n}: {}", bare_drive::run::safe_url(&u)) }).collect::<Vec<_>>().join("\n")
    }

    fn max(args: &Value, d: usize) -> usize {
        args.get("max_chars").and_then(Value::as_u64).map_or(d, |n| n as usize)
    }

    fn snapshot(&mut self, args: &Value) -> Result<String, String> {
        let out = self.snapshot_diff(args, false)?;
        self.with_late(args, out)
    }

    /// Put the lines of `late` before the reply of a read tool.
    fn with_late(&mut self, args: &Value, out: String) -> Result<String, String> {
        let late = self.late(args)?;
        Ok(if late.is_empty() { out } else { format!("{}\n{out}", &late[1..]) })
    }

    /// Answer the dialogs that come between tool calls with dismiss, and record them and the tabs (`late` reports them). Gives the
    /// mark of the console history of the session.
    fn arm(&mut self, args: &Value) -> Result<u64, String> {
        let live = self.live_mut(args)?;
        live.session.set_dialog(false, None);
        Ok(live.session.hist.mark())
    }

    /// The dialogs and the tabs since the last reply, one line each, each after a newline. A tab becomes a session. Arms again.
    fn late(&mut self, args: &Value) -> Result<String, String> {
        let events = self.live_mut(args)?.session.events();
        self.arm(args)?;
        let mut out = String::new();
        for e in events {
            match e {
                Event::Dialog { kind, message, accepted, text } => {
                    let answer = if accepted { "accepted" } else { "dismissed" };
                    out.push_str(&format!("\ndialog: {kind} {message:?} -> {answer}{}", text.map(|t| format!(" {t:?}")).unwrap_or_default()));
                }
                Event::Tab(url) => match self.open_tab(&session_name(args), &url) {
                    Ok(name) => out.push_str(&format!("\nopened tab: {name} {}", bare_drive::run::safe_url(&url))),
                    Err(e) => out.push_str(&format!("\ntab not opened: {e}")),
                },
            }
        }
        Ok(self.live_mut(args)?.red.clean(&out))
    }

    /// Lines `script error: ...` for the uncaught errors that the console history got after `mark`, each after a newline. Leaves out
    /// the lines that `have` holds, and stops at 3 script error lines in all.
    fn script_errors(&mut self, args: &Value, mark: u64, have: &str) -> Result<String, String> {
        let live = self.live_mut(args)?;
        let mut room = 3usize.saturating_sub(have.matches("\nscript error: ").count() + usize::from(have.starts_with("script error: ")));
        let mut out = String::new();
        for line in live.session.hist.logs_since(mark).filter_map(script_error) {
            if room == 0 {
                break;
            }
            if !have.lines().chain(out.lines()).any(|l| l == line) {
                out.push('\n');
                out.push_str(&line);
                room -= 1;
            }
        }
        Ok(live.red.clean(&out))
    }

    /// With `diff`: only what changed since the last snapshot, and its parents.
    fn snapshot_diff(&mut self, args: &Value, diff: bool) -> Result<String, String> {
        let mode = match args.get("mode").and_then(Value::as_str) {
            Some("all") => Mode::All,
            _ => Mode::Interactive,
        };
        let root = ref_arg(args, "root")?;
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let max = Self::max(args, 12_000);
        let live = self.live_mut(args)?;
        let out = live.session.with_refs(move |p, refs| snap::snapshot(p, refs, mode, root, offset, max, diff))??;
        Ok(live.red.clean(&out))
    }

    fn find(&mut self, args: &Value) -> Result<String, String> {
        let get = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        let exact = args.get("exact").and_then(Value::as_bool) == Some(true);
        let t = |v: String| if exact { Text::Is(v) } else { Text::Has(v) };
        let loc = if let Some(r) = get("role") {
            Locator::Role(r.to_ascii_lowercase(), get("name").map(t))
        } else if let Some(v) = get("label") {
            Locator::Label(t(v))
        } else if let Some(v) = get("text") {
            Locator::Text(t(v))
        } else if let Some(v) = get("placeholder") {
            Locator::Placeholder(t(v))
        } else if let Some(v) = get("css") {
            Locator::Css(v)
        } else {
            return Err("give one of role (with name), label, text, placeholder or css".into());
        };
        let max = Self::max(args, 6_000);
        let live = self.live_mut(args)?;
        let out = live.session.with_refs(move |p, refs| snap::find(p, refs, &loc, max))??;
        let out = live.red.clean(&out);
        self.with_late(args, out)
    }

    fn read(&mut self, args: &Value) -> Result<String, String> {
        let root = ref_arg(args, "ref")?;
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let max = Self::max(args, 8_000);
        let live = self.live_mut(args)?;
        let out = live.session.with_refs(move |p, refs| snap::read(p, refs, root, offset, max))??;
        let out = live.red.clean(&out);
        self.with_late(args, out)
    }

    fn links(&mut self, args: &Value) -> Result<String, String> {
        let max = Self::max(args, 8_000);
        let live = self.live_mut(args)?;
        let out = live.session.with_refs(move |p, refs| snap::links(p, refs, max))??;
        let out = live.red.clean(&out);
        self.with_late(args, out)
    }

    fn eval(&mut self, args: &Value) -> Result<String, String> {
        let code = args.get("js").and_then(Value::as_str).ok_or("js is required")?.to_string();
        let live = self.live_mut(args)?;
        let out = live.session.eval(code);
        out.map(|o| live.red.clean(&o)).map_err(|e| live.red.clean(&e))
    }

    /// The console entries of the live page: since the last call of this tool unless `since_last` is false.
    fn console(&mut self, args: &Value) -> Result<String, String> {
        let level = args.get("level").and_then(Value::as_str).unwrap_or("error");
        if !matches!(level, "error" | "warn" | "all") {
            return Err(format!("level: {level} is not error, warn or all"));
        }
        let (since, max) = (args.get("since_last").and_then(Value::as_bool).unwrap_or(true), args.get("max").and_then(Value::as_u64).unwrap_or(50) as usize);
        let live = self.live_mut(args)?;
        live.session.collect()?;
        let v = live.session.hist.console(level, since, max);
        Ok(live.red.clean(&if v.is_empty() { "no console entries".to_string() } else { v.join("\n") }))
    }

    /// The network records of the live page: the failed ones, since the last call of this tool, unless the arguments say otherwise.
    fn network(&mut self, args: &Value) -> Result<String, String> {
        let flag = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(true);
        let max = args.get("max").and_then(Value::as_u64).unwrap_or(50) as usize;
        let contains = args.get("url_contains").and_then(Value::as_str).map(str::to_string);
        let live = self.live_mut(args)?;
        live.session.collect()?;
        let v = live.session.hist.network(flag("failed_only"), contains.as_deref(), flag("since_last"), max);
        Ok(live.red.clean(&if v.is_empty() { "no requests".to_string() } else { v.join("\n") }))
    }

    /// A report of what is wrong with the page, to paste into a bare issue.
    fn diagnose(&mut self, args: &Value) -> Result<String, String> {
        let live = self.live_mut(args)?;
        let red = live.red.clone();
        live.session.diagnose(&red)
    }

    /// One action on an element or the page, as a step line; gives a short receipt and, unless `snapshot` is false, the new snapshot.
    fn act(&mut self, args: &Value) -> Result<String, String> {
        let op = args.get("op").and_then(Value::as_str).unwrap_or("click");
        let target = match (args.get("ref").and_then(Value::as_str), args.get("loc").and_then(Value::as_str)) {
            (Some(r), _) => format!("ref={}", r.trim()),
            (None, Some(l)) => l.to_string(),
            (None, None) => String::new(),
        };
        let text = args.get("text").and_then(Value::as_str).unwrap_or("");
        let q = |t: &str| format!("{t:?}");
        let need = |what: &str| if target.is_empty() { Err(format!("op {what} needs ref (or loc)")) } else { Ok(()) };
        let line = match op {
            "click" => need("click").map(|_| format!("click {target}"))?,
            "type" | "fill" => need("type").map(|_| format!("type {target} {}", q(text)))?,
            "select" => need("select").and_then(|_| {
                let opts: Vec<&str> = args.get("options").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                let opts = if opts.is_empty() && !text.is_empty() { vec![text] } else { opts };
                if opts.is_empty() { return Err("op select needs text (the option label or value) or options".to_string()) }
                Ok(format!("select {target} {}", opts.iter().map(|o| q(o)).collect::<Vec<_>>().join(" ")))
            })?,
            "check" | "uncheck" | "hover" | "dblclick" => need(op).map(|_| format!("{op} {target}"))?,
            "upload" => need("upload").and_then(|_| {
                let files: Vec<&str> = args.get("files").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                if files.is_empty() { return Err("op upload needs files (an array of local paths)".to_string()) }
                Ok(format!("upload {target} {}", files.iter().map(|f| q(f)).collect::<Vec<_>>().join(" ")))
            })?,
            "press" => format!("press {}{}", if target.is_empty() { String::new() } else { format!("{target} ") }, args.get("key").and_then(Value::as_str).unwrap_or(if text.is_empty() { "Enter" } else { text })),
            "scroll" => {
                let dy = args.get("dy").and_then(Value::as_f64).unwrap_or(600.0);
                if target.is_empty() { format!("scroll page {dy}") } else { format!("scroll {target} {dy}") }
            }
            "goto" => format!("goto {}", args.get("url").and_then(Value::as_str).ok_or("op goto needs url")?),
            "reload" => "reload".into(),
            "back" => "back".into(),
            "forward" => "forward".into(),
            "wait" => need("wait").map(|_| format!("wait {target} {}", if text.is_empty() { "visible" } else { text }))?,
            o => return Err(format!("unknown op {o}: click, type, select, check, uncheck, hover, dblclick, upload, press, scroll, goto, reload, back, forward, wait")),
        };
        let accept = match args.get("dialog").and_then(Value::as_str) {
            None | Some("accept") => true,
            Some("dismiss") => false,
            Some(_) => return Err("dialog must be accept or dismiss".into()),
        };
        let mut a = args.clone();
        a["compact"] = json!(true);
        let live = self.live_mut(args)?;
        live.session.set_dialog(accept, args.get("prompt_text").and_then(Value::as_str).map(str::to_string));
        let before = live.session.url();
        let (receipt, ok) = self.run_live(&a, &line)?;
        let mark = self.live_mut(args)?.session.hist.mark();
        let mut out = receipt;
        let snap = if ok && args.get("snapshot").and_then(Value::as_bool) != Some(false) {
            Some(self.snapshot_diff(args, args.get("diff").and_then(Value::as_bool) != Some(false))?)
        } else {
            // No snapshot waits for the page, so let a started navigation finish before the URL check (#382).
            self.live_mut(args)?.session.with_refs(|p, _| snap::settle(p))?;
            None
        };
        // A download that started in the action ends first. The page needs ticks for that: a link starts its worker in a tick.
        out.push_str(&self.finish_downloads(args)?);
        // After the snapshot, which waits for the page: a click starts a navigation that ends later.
        let after = self.live_mut(args)?.session.url();
        if before.is_some() && after != before {
            out.push_str(&format!("\nnavigated: {}", bare_drive::run::safe_url(&after.unwrap_or_default())));
        }
        // The errors of the snapshot and the download wait; the receipt has those of the step.
        out.push_str(&self.script_errors(args, mark, &out)?);
        out.push_str(&self.late(args)?);
        let out = self.live_mut(args)?.red.clean(&out);
        let mut out = out;
        if let Some(s) = snap {
            out.push('\n');
            out.push_str(&s);
        }
        if ok { Ok(out) } else { Err(out) }
    }

    /// Wait up to the step time limit for the downloads that run, and for a page load that may be one. Then say what finished since the last
    /// call: `downloaded: PATH (BYTES bytes, MIME)`, or the failure.
    fn finish_downloads(&mut self, args: &Value) -> Result<String, String> {
        let live = self.live_mut(args)?;
        let end = std::time::Instant::now() + live.opt.timeout;
        while std::time::Instant::now() < end {
            let loading = live.session.with_refs(|p, _| {
                p.tick();
                p.loading()
            })?;
            if !loading && live.dl.running() == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut out = String::new();
        for (i, d) in live.dl.list().into_iter().enumerate() {
            if d.state == DownloadState::Running || !live.told.insert(i) {
                continue;
            }
            out.push('\n');
            out.push_str(&match &d.state {
                DownloadState::Failed(why) => format!("download failed: {}: {why}", bare_drive::run::safe_url(&d.url)),
                _ => format!("downloaded: {} ({} bytes, {})", d.path.display(), d.bytes, d.mime),
            });
        }
        Ok(live.red.clean(&out))
    }

    /// The files that the session saved.
    fn downloads(&mut self, args: &Value) -> Result<String, String> {
        let live = self.live_mut(args)?;
        let list = live.dl.list();
        if list.is_empty() {
            return Ok(format!("no downloads (folder: {})", live.dl.dir().display()));
        }
        let lines: Vec<String> = list
            .iter()
            .map(|d| {
                let url = bare_drive::run::safe_url(&d.url);
                match &d.state {
                    DownloadState::Running => format!("running: {url}"),
                    DownloadState::Failed(why) => format!("failed: {url}: {why}"),
                    DownloadState::Done => format!("{} ({} bytes, {}) from {url}", d.path.display(), d.bytes, d.mime),
                }
            })
            .collect();
        Ok(live.red.clean(&lines.join("\n")))
    }

    /// A new live session `BASE~N` for a tab that the page of session `from` opened: the same size, secrets and variables, and a copy of
    /// its cookies and storage. The load starts at once. Gives the name.
    fn open_tab(&mut self, from: &str, url: &str) -> Result<String, String> {
        let base = from.split('~').next().unwrap_or(from);
        let name = (2..).map(|n| format!("{base}~{n}")).find(|n| !self.live.contains_key(n)).expect("a free name");
        let l = self.live.get_mut(from).ok_or("the opener is gone")?;
        let (size, timeout, window, red, vars, state) = (l.opt.size, l.opt.timeout, l.opt.window, l.red.clone(), l.vars.clone(), l.session.export_state());
        let (mut opt, _, _) = self.options(&json!({"width": size.0, "height": size.1, "timeout_ms": timeout.as_millis() as u64, "window": window}))?;
        opt.progress = false;
        self.place_window(&opt)?;
        let mut session = Session::new(&opt);
        if let Some(st) = state {
            session.preload(st);
        }
        session.ready_wait = true;
        let dl = Downloads::new(std::env::temp_dir().join(format!("bare-mcp-{}-downloads", std::process::id())));
        session.set_downloads(dl.clone());
        self.live.insert(name.clone(), Live { session, opt, red, vars, lines: 0, dl, told: Default::default() });
        match self.run_live(&json!({"session": name, "compact": true}), &format!("open {url}")) {
            Ok((_, true)) => {
                self.arm(&json!({"session": name}))?;
                Ok(name)
            }
            Ok((r, false)) | Err(r) => {
                self.live.remove(&name);
                Err(r)
            }
        }
    }

    /// Read, set or clear the cookies, `localStorage` or `sessionStorage` of a session. Values go through the redactor.
    fn storage(&mut self, args: &Value) -> Result<String, String> {
        let get = |k: &str| args.get(k).and_then(Value::as_str);
        let (area, key, action) = (get("area").unwrap_or("local"), get("key"), get("action").unwrap_or("get"));
        if !["local", "session", "cookies"].contains(&area) {
            return Err("area must be local, session or cookies".into());
        }
        if !["get", "set", "clear"].contains(&action) {
            return Err("action must be get, set or clear".into());
        }
        let cap = |v: &str| if v.chars().count() > 300 { format!("{}...", v.chars().take(300).collect::<String>()) } else { v.to_string() };
        let live = self.live_mut(args)?;
        let out = match (area, action) {
            ("cookies", "get") => {
                let st = live.session.export_state().ok_or("the page has no session")?;
                let list: Vec<String> = st.cookies.iter().filter(|c| key.is_none_or(|k| k == c.name)).map(|c| format!("{}={}  {}{}{}{}", c.name, cap(&c.value), c.domain, c.path, if c.http_only { "; httponly" } else { "" }, if c.secure { "; secure" } else { "" })).collect();
                if list.is_empty() { "no cookies".into() } else { list.join("\n") }
            }
            ("cookies", "set") => {
                let (k, v) = (key.ok_or("set needs key")?, get("value").ok_or("set needs value")?);
                if k.is_empty() || k.contains(|c: char| c.is_whitespace() || c == '=' || c == '"') {
                    return Err("cookie key must be a name without spaces, = or quotes".into());
                }
                let (rep, ok) = self.run_live(&json!({"session": session_name(args), "compact": true}), &format!("cookie set {k} {v:?}"))?;
                if !ok {
                    return Err(rep);
                }
                format!("cookie {k} set")
            }
            ("cookies", _) => format!("removed {} cookies", live.session.clear_cookies(key.map(str::to_string))?),
            (_, "get") => {
                let js = format!("(() => {{ const s = window.{area}Storage, o = []; for (let i = 0; i < s.length; i++) {{ const k = s.key(i); o.push([k, s.getItem(k)]); }} return JSON.stringify(o); }})()");
                let raw = live.session.eval(js)?;
                let raw = serde_json::from_str::<String>(&raw).unwrap_or(raw);
                let pairs: Vec<(String, String)> = serde_json::from_str(&raw).map_err(|e| format!("storage read: {e}"))?;
                let list: Vec<String> = pairs.iter().filter(|(k, _)| key.is_none_or(|w| w == k)).map(|(k, v)| format!("{k}={}", cap(v))).collect();
                if list.is_empty() { "empty".into() } else { list.join("\n") }
            }
            (_, "set") => {
                let (k, v) = (key.ok_or("set needs key")?, get("value").ok_or("set needs value")?);
                live.session.eval(format!("window.{area}Storage.setItem({}, {})", json!(k), json!(v)))?;
                format!("{area} {k} set")
            }
            (_, _) => {
                live.session.eval(match key {
                    Some(k) => format!("window.{area}Storage.removeItem({})", json!(k)),
                    None => format!("window.{area}Storage.clear()"),
                })?;
                format!("{area} cleared")
            }
        };
        Ok(self.live_mut(args)?.red.clean(&out))
    }

    /// A PNG of the page or of one element: its path, and the picture itself when `inline` is true.
    fn screenshot(&mut self, args: &Value) -> Result<Value, String> {
        let r = ref_arg(args, "ref")?;
        let full = args.get("full_page").and_then(Value::as_bool) == Some(true);
        let inline = args.get("inline").and_then(Value::as_bool) == Some(true);
        self.runs += 1;
        let dir = std::env::temp_dir().join(format!("bare-mcp-{}-shots", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = match args.get("path").and_then(Value::as_str) {
            Some(p) => std::path::PathBuf::from(p),
            None => dir.join(format!("shot-{}.png", self.runs)),
        };
        let live = self.live_mut(args)?;
        let red = live.red.clone();
        let said = live.session.screenshot(path.clone(), r, full, &red)?;
        let said = self.with_late(args, said)?;
        let mut content = vec![json!({"type": "text", "text": said})];
        if inline {
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            content.push(json!({"type": "image", "data": base64(&bytes), "mimeType": "image/png"}));
        }
        Ok(json!({"content": content}))
    }

    /// The driver options of a call, its secrets and its variables. Each call gets its own capture folder.
    fn options(&mut self, args: &Value) -> Result<(Options, Redactor, HashMap<String, String>), String> {
        let num = |k: &str| args.get(k).and_then(Value::as_u64);
        let size = (num("width").unwrap_or(1280) as u32, num("height").unwrap_or(800) as u32);
        if size.0 == 0 || size.1 == 0 {
            return Err("width and height must be over 0".into());
        }
        self.runs += 1;
        let out = std::env::temp_dir().join(format!("bare-mcp-{}-{}", std::process::id(), self.runs));
        std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
        let opt = Options {
            size,
            timeout: Duration::from_millis(num("timeout_ms").unwrap_or(10_000)),
            out: Some(out),
            window: args.get("window").and_then(Value::as_bool).unwrap_or(false),
            progress: false,
        };
        let mut vars = HashMap::new();
        if let Some(v) = args.get("vars").and_then(Value::as_object) {
            for (k, v) in v {
                vars.insert(k.clone(), v.as_str().ok_or(format!("vars.{k} must be a string"))?.to_string());
            }
        }
        // Secrets come from a file, as for `bare-drive --secrets`, so their values are never in the arguments that the agent logs.
        let mut red = Redactor::default();
        if let Some(path) = args.get("secrets_file").and_then(Value::as_str) {
            let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
            for line in text.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')) {
                let (k, v) = line.split_once('=').ok_or(format!("{path}: a line is not NAME=value"))?;
                red.add(v);
                vars.insert(k.trim().to_string(), v.to_string());
            }
        }
        Ok((opt, red, vars))
    }

    /// Before a window opens: ask Hyprland to put the driven window on the workspace, without the focus. The rule lives in the running
    /// compositor only (a config reload drops it), so it is set again before each window.
    fn place_window(&self, opt: &Options) -> Result<(), String> {
        let (true, Some(n)) = (opt.window, self.workspace) else { return Ok(()) };
        // The Lua config of Hyprland 0.56 (`hyprctl keyword` refuses it). A named rule replaces the rule of the same name, so the rules do
        // not pile up. `silent`: the window does not take the focus or move the view.
        let rule = format!(r#"hl.window_rule({{ name = "bare-mcp", match = {{ class = "^(bare)$", title = "^(bare \\(driven\\))$" }}, workspace = "{n} silent" }})"#);
        let out = std::process::Command::new("hyprctl").args(["eval", &rule]).output().map_err(|e| format!("hyprctl: {e}"))?;
        let said = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() || !said.trim().eq_ignore_ascii_case("ok") {
            return Err(format!("hyprctl did not set the window rule: {}", said.trim()));
        }
        Ok(())
    }
}

/// Add the capture folder to a report, so the agent can read the pictures of `shot` steps and failures. At the `end` of a run an empty
/// folder is removed (`remove_dir` fails on a folder with files), so a server that runs for days leaves only folders with pictures.
fn with_out(mut json: String, opt: &Options, end: bool) -> String {
    if let Some(dir) = &opt.out
        && !(end && std::fs::remove_dir(dir).is_ok())
    {
        json.insert_str(json.len() - 1, &format!(",\"out\":{}", Value::from(dir.display().to_string())));
    }
    json
}

/// The session name of a call: `session`, default `main`.
fn session_name(args: &Value) -> String {
    args.get("session").and_then(Value::as_str).unwrap_or("main").to_string()
}

/// A ref argument: `e7` (or `7`).
fn ref_arg(args: &Value, k: &str) -> Result<Option<u32>, String> {
    match args.get(k).and_then(Value::as_str) {
        None => Ok(None),
        Some(r) => r.trim().trim_start_matches('e').parse().map(Some).map_err(|_| format!("{k}: not a ref: {r} (use e7)")),
    }
}

/// A short receipt of step lines for an agent: ok or the failure with a hint, then new console errors and failed requests.
fn compact(rep: &Report, red: &Redactor) -> String {
    let ms: f64 = rep.steps.iter().map(|s| s.ms).sum();
    let mut out = match &rep.failure {
        None => {
            let notes: Vec<&str> = rep.steps.iter().filter_map(|s| s.note.as_deref()).collect();
            format!("ok ({ms:.0} ms){}", if notes.is_empty() { String::new() } else { format!(": {}", notes.join("; ")) })
        }
        Some((_, f, _)) => {
            let hint = match f.kind {
                "not_found" => "take a snapshot: the element may not exist yet, or has another name",
                "stale_ref" | "detached" => "take a new snapshot and use its refs",
                "hidden" => "open the menu, tab or section that holds it, or scroll",
                "covered" => "close the dialog or overlay first (press Escape), or act on the covering element",
                "disabled" => "fill the required fields first, or wait",
                "duplicate" => "use a ref from snapshot or find",
                _ => "",
            };
            let script = rep.logs.iter().find(|(_, l)| bare_drive::diag::uncaught(l)).filter(|_| matches!(f.kind, "not_found" | "hidden" | "page"));
            let hint = match script {
                Some((_, l)) => format!("\nhint: the page script failed: {}", l.text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect::<String>()),
                None if hint.is_empty() => String::new(),
                None => format!("\nhint: {hint}"),
            };
            format!("FAILED {}: {}{hint}", f.kind, f.message)
        }
    };
    let mut scripts: Vec<String> = Vec::new();
    for s in rep.logs.iter().filter_map(|(_, l)| script_error(l)) {
        if scripts.len() < 3 && !scripts.contains(&s) {
            scripts.push(s);
        }
    }
    let errs: Vec<String> = rep.logs.iter().filter(|(_, l)| matches!(l.level, "error" | "warn") && script_error(l).is_none()).take(5).map(|(_, l)| format!("console {}: {}", l.level, l.text.chars().take(200).collect::<String>())).collect();
    let bad: Vec<String> = rep.net.iter().filter(|(_, r)| r.error.is_some() || r.status >= 400).take(5).map(|(_, r)| format!("request {}: {}", r.error.clone().unwrap_or_else(|| r.status.to_string()), bare_drive::run::safe_url(&r.url))).collect();
    for l in scripts.iter().chain(&errs).chain(&bad) {
        out.push('\n');
        out.push_str(l);
    }
    red.clean(&out)
}

/// An uncaught exception or unhandled rejection of a page script as one line of at most about 200 characters:
/// `script error: MESSAGE (SOURCE:LINE)` when the text names its first stack frame. Other entries give `None`.
fn script_error(l: &bare_drive::diag::Log) -> Option<String> {
    let t = match l.level {
        "exception" => l.text.strip_prefix("listener of ").and_then(|t| t.split_once(": ")).map_or(l.text.as_str(), |(_, m)| m),
        "error" => l.text.strip_prefix("Uncaught ")?,
        _ => return None,
    };
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    // The first stack frame with a line, `at f (src:line:col)` or `at src:line:col`, becomes `(src:line)`; else the first frame.
    let (msg, at) = match t.split_once(" at ") {
        Some((m, frames)) => {
            let places: Vec<&str> = frames.split(" at ").map(|f| f.rsplit_once('(').map_or(f, |(_, p)| p).trim_end_matches(')')).collect();
            let place = places.iter().find_map(|p| p.rsplit_once(':').filter(|(a, c)| c.parse::<u32>().is_ok() && a.rsplit_once(':').is_some_and(|(_, n)| n.parse::<u32>().is_ok())).map(|(a, _)| a)).unwrap_or(places[0]);
            (m.to_string(), format!(" ({place})"))
        }
        None => (t, String::new()),
    };
    let msg: String = msg.chars().take(200usize.saturating_sub(at.chars().count().min(100))).collect();
    let at: String = at.chars().take(100).collect();
    Some(format!("script error: {msg}{at}"))
}

/// Standard base64, for an image in a tool result.
fn base64(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            s.push(if i <= c.len() { A[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    s
}

/// Add the way the session was made to a report, as `with_out` adds `out`.
fn with_session(mut json: String, how: &str) -> String {
    json.insert_str(json.len() - 1, &format!(",\"session\":\"{how}\""));
    json
}

fn tools() -> Value {
    let common = json!({
        "vars": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Values for ${NAME} in the steps."},
        "secrets_file": {"type": "string", "description": "A file of NAME=value lines. Each value is a variable and is [secret] in all output."},
        "window": {"type": "boolean", "description": "Show the page in a Wayland window. Default: headless."},
        "width": {"type": "integer", "description": "Page width in pixels. Default 1280."},
        "height": {"type": "integer", "description": "Page height in pixels. Default 800."},
        "timeout_ms": {"type": "integer", "description": "Time limit of each step. Default 10000."},
        "session": {"type": "string", "description": "Live session name. Default main. Sessions run side by side."},
        "downloads_dir": {"type": "string", "description": "open: the folder for files that the page downloads. Default: a temp folder of this server."},
    });
    let with = |extra: Value, required: &[&str]| {
        let mut p = common.clone();
        p.as_object_mut().expect("object").extend(extra.as_object().expect("object").clone());
        json!({"type": "object", "properties": p, "required": required})
    };
    #[allow(non_snake_case)]
    let S = json!({"type": "string", "description": "Live session name. Default main."});
    let syntax = "Step lines (# starts a comment): open URL, goto URL, reload, click LOC, type LOC \"text\" (\"\" clears), \
        select LOC \"label or value\" [\"more\" ...], check LOC, uncheck LOC, hover LOC, dblclick LOC, upload LOC \"path\" [\"path2\" ...], \
        press [LOC] KEY (Enter, Tab, Shift+Tab, Control+a), scroll LOC|page DY, \
        expect LOC visible|hidden|enabled|disabled|checked|unchecked|focused|count N|text=\"x\"|value~\"x\", \
        expect url=\"x\"|url~\"x\", expect title=\"x\"|title~\"x\", expect js \"expression\", expect cookie NAME [absent|value=\"x\"], \
        cookie set NAME VALUE, shot NAME (a PNG in the out folder), timeout MS. \
        LOC: ref=e7 (from snapshot), css=\"...\", role=NAME [name=\"...\"|name~\"...\"], label=\"...\", text=\"...\", placeholder=\"...\"; it must match one element. \
        A step waits for its condition until its time limit; there are no fixed sleeps.";
    json!([
        {
            "name": "run_steps",
            "description": format!("Run a whole end-to-end journey in the bare browser and return a JSON report: each step, the failure with a capture, console logs and network requests. The first step must be open. {syntax}"),
            "inputSchema": with(json!({"steps": {"type": "string", "description": "The step file text, one step per line. # starts a comment."}}), &["steps"]),
        },
        {
            "name": "open",
            "description": "Start a live session: load URL in the bare browser. A new open ends the old session. Returns the report of the open step.",
            "inputSchema": with(json!({"url": {"type": "string"}, "state": {"type": "string", "description": "Start from the session that save_state or ensure_session saved under this name, in this server."}, "snapshot": {"type": "boolean", "description": "Return a short receipt and the page snapshot instead of the JSON report."}}), &["url"]),
        },
        {
            "name": "step",
            "description": format!("Run one or more step lines on the live session page, until one fails. Returns the report of these steps only. The session stays open after a failure. {syntax}"),
            "inputSchema": {"type": "object", "properties": {"steps": {"type": "string"}, "session": S, "compact": {"type": "boolean", "description": "A short receipt instead of the JSON report."}}, "required": ["steps"]},
        },
        {
            "name": "save_state",
            "description": "Save the cookies, localStorage and sessionStorage of the live session in memory under a name, and in a file (mode 0600) when file is given. Returns counts only, never values. Open it again with open state=name.",
            "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "session": S, "file": {"type": "string", "description": "Also write the session to this file. It holds live credentials."}}, "required": ["name"]},
        },
        {
            "name": "ensure_session",
            "description": "Get a logged-in live page, like cy.session. Restores the session saved under name (from memory, else from file), opens url and runs the check steps. If there is none or the check fails, opens url with an empty profile, runs the login steps, runs the check, and saves the session. The live page stays open: go on with step. Returns the report of the check with session: restored, created or recreated. login and check are step lines (see step); use ${NAME} with secrets_file for the credentials.",
            "inputSchema": with(json!({
                "name": {"type": "string"},
                "url": {"type": "string", "description": "The page to open, where the check and the login steps start."},
                "login": {"type": "string", "description": "Step lines that log in (without open)."},
                "check": {"type": "string", "description": "Step lines that pass only when the session is valid, such as expect on a logged-in element."},
                "file": {"type": "string", "description": "Also read the session from this file when it is not in memory, and write it there when saved. It holds live credentials."},
            }), &["name", "url", "login", "check"]),
        },
        {
            "name": "close",
            "description": "End the live session, close its window, and return the cookie names and flags (never values).",
            "inputSchema": {"type": "object", "properties": {"session": S}},
        },
        {
            "name": "snapshot",
            "description": "Dialogs and tabs that the page opened after the last reply (from a timer, say) show first, one line each: dialog: confirm \"...\" -> dismissed (such a dialog is always dismissed), or opened tab: main~2 URL. This holds for snapshot, act, find, read, links and screenshot. Read the live page as text: the accessibility outline, one element per line, indented by nesting: [e7] role \"name\" = \"value\" (states). Act on an element with its ref (act ref=e7, or ref=e7 in step lines). * marks an element new or changed since the last snapshot. A link line shows its href only when it has no name: links lists them all. Refs stay valid until the page navigates. (offscreen) marks a control outside the window: act click scrolls it into view. An iframe shows as one line: its content is not loaded, so it is not in the snapshot. Use this instead of a screenshot.",
            "inputSchema": {"type": "object", "properties": {"session": S, "mode": {"type": "string", "enum": ["interactive", "all"], "description": "interactive (default): controls, headings, landmarks, dialogs, alerts. all: also the text between them."}, "root": {"type": "string", "description": "A ref: the outline of that element only."}, "offset": {"type": "integer", "description": "Skip this many outline lines (the cut line gives the next offset)."}, "max_chars": {"type": "integer", "description": "Default 12000. A longer outline keeps 80% from the top and 20% from the end, and a line says which refs were cut."}}},
        },
        {
            "name": "act",
            "description": "Act on the live page like a user and get a short receipt (ok, or the failure with a hint, up to 3 script error lines for uncaught errors, new console errors and failed requests, navigation, dialogs and the answer given, tabs the page opened: \"opened tab: main~2 URL\" is a new session that you read with session=main~2 and end with close), then what changed on the page (only the lines marked *, with the lines that hold them; the whole outline after a navigation or with diff false). Ops: click, type (clears the field, then types text; \\n is Enter), press (key: Enter, Escape, Tab, Control+a; with ref it acts on that element, and Enter in a form field submits the form), scroll (dy pixels, over ref or the page), goto (url), reload, back, forward (the history), wait (ref until text state: visible, hidden, enabled, checked, text~\"x\"), select (text = the option label or value; options = several for a multiple select; fires input and change), check and uncheck (a checkbox or radio; clicks only when the state differs), hover (pointer over the element: :hover and mouseenter), dblclick, upload (files = local paths for an <input type=file>, at most 100 MiB each; a hidden input needs loc css=\"input[type=file]\"). The action waits for the element up to timeout_ms.",
            "inputSchema": {"type": "object", "properties": {"session": S, "op": {"type": "string", "enum": ["click", "type", "select", "check", "uncheck", "hover", "dblclick", "upload", "press", "scroll", "goto", "reload", "back", "forward", "wait"]}, "options": {"type": "array", "items": {"type": "string"}, "description": "For select: several options of a multiple select."}, "files": {"type": "array", "items": {"type": "string"}, "description": "For upload: local file paths."}, "dialog": {"type": "string", "enum": ["accept", "dismiss"], "description": "How to answer alert, confirm and prompt dialogs that the action opens. Default accept. The receipt lists each one."}, "prompt_text": {"type": "string", "description": "The answer of an accepted prompt. Default: the default value of the prompt."}, "ref": {"type": "string", "description": "e7 from snapshot or find."}, "loc": {"type": "string", "description": "A step locator instead of ref: role=button name=\"Save\", label=\"Email\", text=\"x\", css=\"...\"."}, "text": {"type": "string"}, "key": {"type": "string"}, "dy": {"type": "number"}, "url": {"type": "string"}, "snapshot": {"type": "boolean", "description": "Default true: add the changes of the page after the action."}, "diff": {"type": "boolean", "description": "Default true. False: add the whole outline instead of the changes."}, "mode": {"type": "string", "enum": ["interactive", "all"]}, "max_chars": {"type": "integer"}}, "required": ["op"]},
        },
        {
            "name": "find",
            "description": "Elements that match, as snapshot lines with refs, without a full snapshot. Give role (with name), label, text, placeholder or css. Text matches a part, in any case, unless exact.",
            "inputSchema": {"type": "object", "properties": {"session": S, "role": {"type": "string"}, "name": {"type": "string"}, "label": {"type": "string"}, "text": {"type": "string"}, "placeholder": {"type": "string"}, "css": {"type": "string"}, "exact": {"type": "boolean"}, "max_chars": {"type": "integer"}}},
        },
        {
            "name": "read",
            "description": "The rendered text of the page or of one element (ref), one line per block, in pages of max_chars (default 8000) from offset.",
            "inputSchema": {"type": "object", "properties": {"session": S, "ref": {"type": "string"}, "offset": {"type": "integer"}, "max_chars": {"type": "integer"}}},
        },
        {
            "name": "links",
            "description": "The links of the page: ref, text and URL (a path for a link on the origin of the page, with tracking parameters cut).",
            "inputSchema": {"type": "object", "properties": {"session": S, "max_chars": {"type": "integer"}}},
        },
        {
            "name": "screenshot",
            "description": "A PNG of the live page window, of the whole page (full_page true), or of the whole box of one element (ref), however tall: up to 8000 px, and the reply says when that cut it. Returns its path; with inline true also the image. Waits for pictures to load. Refused when a secret shows on the page. Prefer snapshot to read a page.",
            "inputSchema": {"type": "object", "properties": {"session": S, "ref": {"type": "string"}, "path": {"type": "string", "description": "Where to write the PNG. Default: a temp folder."}, "inline": {"type": "boolean"}, "full_page": {"type": "boolean", "description": "Paint the whole page, not only the window. Ignored with ref."}}},
        },
        {
            "name": "eval",
            "description": "Run JavaScript in the live page and return its value as text. JSON.stringify an object. A Promise is awaited up to the time limit. Top-level await works: an expression gives its value; statements need a return. A thrown error shows as Name: message.",
            "inputSchema": {"type": "object", "properties": {"session": S, "js": {"type": "string"}}, "required": ["js"]},
        },
        {
            "name": "console",
            "description": "The console of the live page, one short line per entry: level and text. Defaults: errors only, entries since the last console call. level: error (errors and uncaught exceptions), warn (and warnings) or all. since_last false: the last 500 entries. max: the newest entries to show, default 50.",
            "inputSchema": {"type": "object", "properties": {"session": S, "level": {"type": "string", "enum": ["error", "warn", "all"]}, "since_last": {"type": "boolean"}, "max": {"type": "integer"}}},
        },
        {
            "name": "network",
            "description": "The requests of the live page, one line per request: failure or status, kind (by URL), URL, size, time. Defaults: failed requests only (an error or HTTP 400 and over), since the last network call. failed_only false: all requests. url_contains: keep URLs with this text. since_last false: the last 500 records. max: the newest records to show, default 50.",
            "inputSchema": {"type": "object", "properties": {"session": S, "failed_only": {"type": "boolean"}, "url_contains": {"type": "string"}, "since_last": {"type": "boolean"}, "max": {"type": "integer"}}},
        },
        {
            "name": "diagnose",
            "description": "A report of what is wrong with the live page, to paste into a bare issue: the document load, uncaught exceptions, missing engine features, failed requests by kind, an empty-render check, the page size, and a verdict: site problem, bare gap, network or unclear, with the reason. Call it after a step failed.",
            "inputSchema": {"type": "object", "properties": {"session": S}},
        },
        {
            "name": "downloads",
            "description": "The files that the live session saved from downloads (a response with Content-Disposition: attachment or a type the page cannot show, or a link with a download attribute, also a blob: or data: URL made by a script): path, size, type and source URL, or running or failed. A download does not replace the page; act names each one that ends in its receipt.",
            "inputSchema": {"type": "object", "properties": {"session": S}},
        },
        {
            "name": "sessions",
            "description": "The live sessions and their URLs.",
            "inputSchema": {"type": "object", "properties": {}},
        },
        {
            "name": "storage",
            "description": "Read, set or clear the cookies, localStorage or sessionStorage of a session (area: local, session, cookies; action: get, set, clear; key; value). Without key, get lists all and clear removes all. Known secrets show as [secret].",
            "inputSchema": {"type": "object", "properties": {"session": S, "area": {"type": "string", "enum": ["local", "session", "cookies"], "description": "Default local."}, "action": {"type": "string", "enum": ["get", "set", "clear"], "description": "Default get."}, "key": {"type": "string"}, "value": {"type": "string"}}},
        },
    ])
}
