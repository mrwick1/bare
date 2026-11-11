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
use bare_drive::run::{Options, Redactor, Report, Session, StorageState};
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
            "sessions" => Ok(self.sessions()),
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
            return if ok { Ok(rep + "\n" + &self.snapshot(args)?) } else { Err(rep) };
        }
        Ok(self.start(args, url, state)?.0)
    }

    /// Start a live session on URL, from a saved session or with an empty profile. One live session at a time: this ends the old one
    /// (and closes its window). Gives the report of the open step and whether it passed.
    fn start(&mut self, args: &Value, url: &str, state: Option<StorageState>) -> Result<(String, bool), String> {
        let name = session_name(args);
        let _ = self.close(args);
        let (opt, red, vars) = self.options(args)?;
        self.place_window(&opt)?;
        let mut session = Session::new(&opt);
        let dir = args.get("downloads_dir").and_then(Value::as_str).map_or_else(|| std::env::temp_dir().join(format!("bare-mcp-{}-downloads", std::process::id())), std::path::PathBuf::from);
        let dl = Downloads::new(dir);
        session.set_downloads(dl.clone());
        if let Some(st) = state {
            session.preload(st);
        }
        self.live.insert(name, Live { session, opt, red, vars, lines: 0, dl, told: Default::default() });
        self.run_live(args, &format!("open {url}"))
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
        let mode = match args.get("mode").and_then(Value::as_str) {
            Some("all") => Mode::All,
            _ => Mode::Interactive,
        };
        let root = ref_arg(args, "root")?;
        let max = Self::max(args, 12_000);
        let live = self.live_mut(args)?;
        let out = live.session.with_refs(move |p, refs| snap::snapshot(p, refs, mode, root, max))??;
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
        Ok(live.red.clean(&out))
    }

    fn read(&mut self, args: &Value) -> Result<String, String> {
        let root = ref_arg(args, "ref")?;
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let max = Self::max(args, 8_000);
        let live = self.live_mut(args)?;
        let out = live.session.with_refs(move |p, refs| snap::read(p, refs, root, offset, max))??;
        Ok(live.red.clean(&out))
    }

    fn links(&mut self, args: &Value) -> Result<String, String> {
        let max = Self::max(args, 8_000);
        let live = self.live_mut(args)?;
        let out = live.session.with_refs(move |p, refs| snap::links(p, refs, max))??;
        Ok(live.red.clean(&out))
    }

    fn eval(&mut self, args: &Value) -> Result<String, String> {
        let code = args.get("js").and_then(Value::as_str).ok_or("js is required")?.to_string();
        let live = self.live_mut(args)?;
        let out = live.session.eval(code);
        out.map(|o| live.red.clean(&o)).map_err(|e| live.red.clean(&e))
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
            "press" => format!("press {}", args.get("key").and_then(Value::as_str).unwrap_or(if text.is_empty() { "Enter" } else { text })),
            "scroll" => {
                let dy = args.get("dy").and_then(Value::as_f64).unwrap_or(600.0);
                if target.is_empty() { format!("scroll page {dy}") } else { format!("scroll {target} {dy}") }
            }
            "goto" => format!("goto {}", args.get("url").and_then(Value::as_str).ok_or("op goto needs url")?),
            "reload" => "reload".into(),
            "wait" => need("wait").map(|_| format!("wait {target} {}", if text.is_empty() { "visible" } else { text }))?,
            o => return Err(format!("unknown op {o}: click, type, press, scroll, goto, reload, wait")),
        };
        let mut a = args.clone();
        a["compact"] = json!(true);
        let before = self.live_mut(args)?.session.url();
        let (receipt, ok) = self.run_live(&a, &line)?;
        let mut out = receipt;
        let snap = if ok && args.get("snapshot").and_then(Value::as_bool) != Some(false) { Some(self.snapshot(args)?) } else { None };
        // A download that started in the action ends first. The page needs ticks for that: a link starts its worker in a tick.
        out.push_str(&self.finish_downloads(args)?);
        // After the snapshot, which waits for the page: a click starts a navigation that ends later.
        let after = self.live_mut(args)?.session.url();
        if after != before {
            out.push_str(&format!("\nnavigated: {}", bare_drive::run::safe_url(&after.unwrap_or_default())));
        }
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

    /// A PNG of the page or of one element: its path, and the picture itself when `inline` is true.
    fn screenshot(&mut self, args: &Value) -> Result<Value, String> {
        let r = ref_arg(args, "ref")?;
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
        let said = live.session.screenshot(path.clone(), r, &red)?;
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
            format!("FAILED {}: {}{}", f.kind, f.message, if hint.is_empty() { String::new() } else { format!("\nhint: {hint}") })
        }
    };
    let errs: Vec<String> = rep.logs.iter().filter(|(_, l)| matches!(l.level, "error" | "warn")).take(5).map(|(_, l)| format!("console {}: {}", l.level, l.text.chars().take(200).collect::<String>())).collect();
    let bad: Vec<String> = rep.net.iter().filter(|(_, r)| r.error.is_some() || r.status >= 400).take(5).map(|(_, r)| format!("request {}: {}", r.error.clone().unwrap_or_else(|| r.status.to_string()), bare_drive::run::safe_url(&r.url))).collect();
    for l in errs.iter().chain(&bad) {
        out.push('\n');
        out.push_str(l);
    }
    red.clean(&out)
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
        press KEY (Enter, Tab, Shift+Tab, Control+a), scroll LOC|page DY, \
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
            "description": "Read the live page as text: the accessibility outline, one element per line, indented by nesting: [e7] role \"name\" = \"value\" (states). Act on an element with its ref (act ref=e7, or ref=e7 in step lines). * marks an element new since the last snapshot. Refs stay valid until the page navigates. Use this instead of a screenshot.",
            "inputSchema": {"type": "object", "properties": {"session": S, "mode": {"type": "string", "enum": ["interactive", "all"], "description": "interactive (default): controls, headings, landmarks, dialogs, alerts. all: also the text between them."}, "root": {"type": "string", "description": "A ref: the outline of that element only."}, "max_chars": {"type": "integer", "description": "Default 12000. A longer outline keeps 80% from the top and 20% from the end."}}},
        },
        {
            "name": "act",
            "description": "Act on the live page like a user and get a short receipt (ok, or the failure with a hint, new console errors and failed requests, navigation), then the new snapshot. Ops: click, type (clears the field, then types text; \\n is Enter), press (key: Enter, Escape, Tab, Control+a), scroll (dy pixels, over ref or the page), goto (url), reload, wait (ref until text state: visible, hidden, enabled, checked, text~\"x\"). The action waits for the element up to timeout_ms.",
            "inputSchema": {"type": "object", "properties": {"session": S, "op": {"type": "string", "enum": ["click", "type", "press", "scroll", "goto", "reload", "wait"]}, "ref": {"type": "string", "description": "e7 from snapshot or find."}, "loc": {"type": "string", "description": "A step locator instead of ref: role=button name=\"Save\", label=\"Email\", text=\"x\", css=\"...\"."}, "text": {"type": "string"}, "key": {"type": "string"}, "dy": {"type": "number"}, "url": {"type": "string"}, "snapshot": {"type": "boolean", "description": "Default true: add the snapshot after the action."}, "mode": {"type": "string", "enum": ["interactive", "all"]}, "max_chars": {"type": "integer"}}, "required": ["op"]},
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
            "description": "The links of the page: ref, text and absolute URL.",
            "inputSchema": {"type": "object", "properties": {"session": S, "max_chars": {"type": "integer"}}},
        },
        {
            "name": "screenshot",
            "description": "A PNG of the live page, or of one element (ref). Returns its path; with inline true also the image. Waits for pictures to load. Refused when a secret shows on the page. Prefer snapshot to read a page.",
            "inputSchema": {"type": "object", "properties": {"session": S, "ref": {"type": "string"}, "path": {"type": "string", "description": "Where to write the PNG. Default: a temp folder."}, "inline": {"type": "boolean"}}},
        },
        {
            "name": "eval",
            "description": "Run JavaScript in the live page and return its value as text. JSON.stringify an object.",
            "inputSchema": {"type": "object", "properties": {"session": S, "js": {"type": "string"}}, "required": ["js"]},
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
    ])
}
