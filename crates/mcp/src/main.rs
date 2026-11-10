//! `bare-mcp [--workspace N]`: an MCP server over stdio for end-to-end tests in `bare`.
//!
//! An agent (Claude Code, Codex) starts this program and talks JSON-RPC 2.0 to it: one JSON object on each line of stdin and stdout.
//! The tools use the test driver (`bare-drive`), so each click and key goes through the same hit testing and input path as a user.
//!
//! - `run_steps`: run a whole step file and give its report.
//! - `open`, `step`, `close`: a live session. `open` loads a page, each `step` call runs more step lines on the same page, and `close`
//!   ends it. Each call gives the report of its own steps only, so the server keeps no old records.
//!
//! Headless by default. With `window: true` the page shows in a Wayland window. `--workspace N` puts that window on Hyprland workspace
//! N without taking the focus.
//!
//! Only JSON goes to stdout: the protocol owns it. Progress and errors go to stderr. JS: like a Node script that reads `readline`
//! lines from `process.stdin` and writes `JSON.stringify(reply)` lines to `process.stdout`.

use bare_drive::run::{Options, Redactor, Session};
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
}

struct Server {
    workspace: Option<u32>,
    live: Option<Live>,
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
    let mut server = Server { workspace, live: None, runs: 0 };
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
            "close" => self.close(),
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
        // One live session at a time: a new `open` ends the old one (and closes its window).
        let _ = self.close();
        let (opt, red, vars) = self.options(args)?;
        self.place_window(&opt)?;
        let session = Session::new(&opt);
        self.live = Some(Live { session, opt, red, vars, lines: 0 });
        self.run_live(&format!("open {url}"))
    }

    fn step(&mut self, args: &Value) -> Result<String, String> {
        let src = args.get("steps").and_then(Value::as_str).ok_or("steps (one or more step lines) is required")?;
        self.run_live(src)
    }

    fn close(&mut self) -> Result<String, String> {
        let live = self.live.take().ok_or("no live session: call open first")?;
        Ok(with_out(live.session.finish().json(&live.red), &live.opt, true))
    }

    /// Run step lines on the live session until one fails, and give the report of these lines.
    fn run_live(&mut self, src: &str) -> Result<String, String> {
        let live = self.live.as_mut().ok_or("no live session: call open first")?;
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
        Ok(with_out(live.session.drain().json(&live.red), &live.opt, false))
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

fn tools() -> Value {
    let common = json!({
        "vars": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Values for ${NAME} in the steps."},
        "secrets_file": {"type": "string", "description": "A file of NAME=value lines. Each value is a variable and is [secret] in all output."},
        "window": {"type": "boolean", "description": "Show the page in a Wayland window. Default: headless."},
        "width": {"type": "integer", "description": "Page width in pixels. Default 1280."},
        "height": {"type": "integer", "description": "Page height in pixels. Default 800."},
        "timeout_ms": {"type": "integer", "description": "Time limit of each step. Default 10000."},
    });
    let with = |extra: Value, required: &[&str]| {
        let mut p = common.clone();
        p.as_object_mut().expect("object").extend(extra.as_object().expect("object").clone());
        json!({"type": "object", "properties": p, "required": required})
    };
    let syntax = "Step lines (# starts a comment): open URL, goto URL, reload, click LOC, type LOC \"text\" (\"\" clears), \
        press KEY (Enter, Tab, Shift+Tab, Control+a), scroll LOC|page DY, \
        expect LOC visible|hidden|enabled|disabled|checked|unchecked|focused|count N|text=\"x\"|value~\"x\", \
        expect url=\"x\"|url~\"x\", expect title=\"x\"|title~\"x\", expect js \"expression\", expect cookie NAME [absent|value=\"x\"], \
        cookie set NAME VALUE, shot NAME (a PNG in the out folder), timeout MS. \
        LOC: css=\"...\", role=NAME [name=\"...\"|name~\"...\"], label=\"...\", text=\"...\", placeholder=\"...\"; it must match one element. \
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
            "inputSchema": with(json!({"url": {"type": "string"}}), &["url"]),
        },
        {
            "name": "step",
            "description": format!("Run one or more step lines on the live session page, until one fails. Returns the report of these steps only. The session stays open after a failure. {syntax}"),
            "inputSchema": {"type": "object", "properties": {"steps": {"type": "string"}}, "required": ["steps"]},
        },
        {
            "name": "close",
            "description": "End the live session, close its window, and return the cookie names and flags (never values).",
            "inputSchema": {"type": "object", "properties": {}},
        },
    ])
}
