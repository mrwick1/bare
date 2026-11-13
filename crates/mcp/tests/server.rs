//! `bare-mcp` against the fixture app of the driver (`crates/drive/tests/common`), offline and headless: the staging journey through
//! `run_steps`, the same journey one line at a time through a live session, and the protocol errors. No secret may leak into a reply.

#[path = "../../drive/tests/common/mod.rs"]
mod common;

use common::{EMAIL, PASSWORD};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    id: u64,
    /// Values that no reply may hold, besides the fixture secrets: the values of a saved state file.
    forbid: Vec<String>,
}

impl Server {
    fn start() -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_bare-mcp")).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let (stdin, stdout) = (child.stdin.take().unwrap(), BufReader::new(child.stdout.take().unwrap()));
        let mut s = Server { child, stdin, stdout, id: 0, forbid: Vec::new() };
        let init = s.ask("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
        assert_eq!(init["result"]["serverInfo"]["name"], "bare-mcp");
        assert!(init["result"]["instructions"].as_str().unwrap().contains("label bare-mcp"));
        writeln!(s.stdin, "{}", json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).unwrap();
        s
    }

    fn ask(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        writeln!(self.stdin, "{}", json!({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params})).unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        for s in [EMAIL, PASSWORD, "app-token-"] {
            assert!(!line.contains(s), "a secret leaked: {s} in {line}");
        }
        for s in &self.forbid {
            assert!(!line.contains(s.as_str()), "a saved value leaked in {line}");
        }
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], self.id, "{line}");
        reply
    }

    /// Call a tool. Gives the report (the JSON in the text) and whether the call was an error.
    fn tool(&mut self, name: &str, args: Value) -> (Value, bool) {
        let r = self.ask("tools/call", json!({"name": name, "arguments": args}));
        let text = r["result"]["content"][0]["text"].as_str().unwrap().to_string();
        let error = r["result"]["isError"] == true;
        (serde_json::from_str(&text).unwrap_or(Value::String(text)), error)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The staging step file and the arguments for the fixture app: `BASE`, `QUERY` and a secrets file.
fn journey(name: &str) -> (String, Value) {
    let dir = std::env::temp_dir().join(format!("bare-mcp-test-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let secrets = dir.join("secrets");
    std::fs::write(&secrets, format!("EMAIL={EMAIL}\nPASSWORD={PASSWORD}\n")).unwrap();
    let steps = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/drive/staging-journey.steps")).unwrap();
    let args = json!({"vars": {"BASE": common::serve_app(), "QUERY": "Beta"}, "secrets_file": secrets.to_str().unwrap()});
    (steps, args)
}

#[test]
fn tools_list_names_the_tools() {
    let mut s = Server::start();
    let r = s.ask("tools/list", json!({}));
    let names: Vec<&str> = r["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["run_steps", "open", "step", "save_state", "ensure_session", "close", "snapshot", "act", "find", "read", "links", "screenshot", "eval", "console", "network", "diagnose", "downloads", "sessions", "storage"]);
}

#[test]
fn run_steps_passes_the_staging_journey_on_the_fixture_app() {
    let mut s = Server::start();
    let (steps, mut args) = journey("run");
    args["steps"] = json!(steps);
    let (rep, error) = s.tool("run_steps", args);
    assert!(!error, "{rep}");
    assert_eq!(rep["ok"], true, "{rep}");
    assert_eq!(rep["steps"].as_array().unwrap().len(), 27);
    // The `shot` steps write PNGs into the out folder of the call.
    let out = rep["out"].as_str().unwrap();
    assert!(std::path::Path::new(out).join("overview.png").is_file(), "{rep}");
}

#[test]
fn a_live_session_runs_the_journey_one_line_at_a_time() {
    let mut s = Server::start();
    let (steps, mut args) = journey("live");
    let mut lines = steps.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#'));
    assert_eq!(lines.next(), Some("timeout 30000"));
    assert_eq!(lines.next(), Some("open ${BASE}/"));
    args["url"] = json!(format!("{}/", args["vars"]["BASE"].as_str().unwrap()));
    args["timeout_ms"] = json!(30000);
    args["report"] = json!(true);
    let (rep, error) = s.tool("open", args);
    assert!(!error && rep["ok"] == true, "{rep}");
    let mut last = 1;
    for line in lines {
        let (rep, error) = s.tool("step", json!({"steps": line}));
        assert!(!error && rep["ok"] == true, "{line}: {rep}");
        // Each call gives only its own step, numbered after the steps before it.
        let got = rep["steps"].as_array().unwrap();
        assert_eq!(got.len(), 1, "{rep}");
        assert_eq!(got[0]["line"], last + 1);
        last += 1;
    }
    let (rep, error) = s.tool("close", json!({}));
    assert!(!error && rep["steps"].as_array().unwrap().is_empty(), "{rep}");
    let (rep, error) = s.tool("step", json!({"steps": "reload"}));
    assert!(error, "{rep}");
}

#[test]
fn a_failed_step_keeps_the_session_open() {
    let mut s = Server::start();
    let (_, mut args) = journey("fail");
    args["url"] = json!(format!("{}/", args["vars"]["BASE"].as_str().unwrap()));
    s.tool("open", args);
    let (rep, error) = s.tool("step", json!({"steps": "timeout 300\nexpect css=\"#nothing-here\" visible\nexpect url~\"/login\""}));
    assert!(!error, "{rep}");
    assert_eq!(rep["ok"], false);
    assert_eq!(rep["failure"]["line"], 3, "{rep}");
    assert_eq!(rep["failure"]["kind"], "not_found", "{rep}");
    // The line after the failure did not run; the next call runs on the same page.
    assert_eq!(rep["steps"].as_array().unwrap().len(), 2);
    let (rep, _) = s.tool("step", json!({"steps": "expect url~\"/login\""}));
    assert_eq!(rep["ok"], true, "{rep}");
}

#[test]
fn bad_calls_are_errors_and_the_server_goes_on() {
    let mut s = Server::start();
    assert!(s.tool("step", json!({"steps": "reload"})).1);
    assert!(s.tool("run_steps", json!({})).1);
    assert!(s.tool("run_steps", json!({"steps": "fly away"})).1);
    assert!(s.tool("nope", json!({})).1);
    assert_eq!(s.ask("nope/method", json!({}))["error"]["code"], -32601);
    writeln!(s.stdin, "not json").unwrap();
    let mut line = String::new();
    s.stdout.read_line(&mut line).unwrap();
    assert!(line.contains("-32700"), "{line}");
    assert_eq!(s.ask("ping", json!({}))["result"], json!({}));
}

#[test]
fn a_run_without_pictures_leaves_no_folder() {
    let mut s = Server::start();
    let (_, mut args) = journey("nopics");
    args["steps"] = json!("open ${BASE}/\nexpect url~\"/login\"");
    let (rep, error) = s.tool("run_steps", args);
    assert!(!error && rep["ok"] == true, "{rep}");
    assert!(rep.get("out").is_none(), "{rep}");
}

const LOGIN: &str = "type css=\"input[name=email]\" \"${EMAIL}\"\ntype css=\"input[name=password]\" \"${PASSWORD}\"\nclick css=\"form button[type=submit]\"";
const CHECK: &str = "expect role=link name=\"Agents\" visible";

fn ensure_args(name: &str, test: &str) -> Value {
    let (_, mut args) = journey(test);
    args["url"] = json!(format!("{}/", args["vars"]["BASE"].as_str().unwrap()));
    args["name"] = json!(name);
    args["login"] = json!(LOGIN);
    args["check"] = json!(CHECK);
    args["timeout_ms"] = json!(2000);
    args
}

fn state_file(test: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("bare-mcp-test-{test}-{}", std::process::id())).join("state.json")
}

#[test]
fn ensure_session_logs_in_once_and_then_restores_from_memory() {
    let mut s = Server::start();
    let args = ensure_args("main", "ensure");
    let (rep, error) = s.tool("ensure_session", args.clone());
    assert!(!error && rep["ok"] == true && rep["session"] == "created", "{rep}");
    // The live page is logged in.
    let (rep, _) = s.tool("step", json!({"steps": "expect js \"sessionStorage.getItem('token') !== null\""}));
    assert_eq!(rep["ok"], true, "{rep}");
    let (rep, error) = s.tool("ensure_session", args);
    assert!(!error && rep["ok"] == true && rep["session"] == "restored", "{rep}");
    // Only the check ran: no `type` step.
    let kinds: Vec<String> = rep["steps"].as_array().unwrap().iter().map(|x| x.to_string()).collect();
    assert!(kinds.iter().all(|k| !k.contains("type")), "{rep}");
    let (rep, _) = s.tool("step", json!({"steps": "expect js \"sessionStorage.getItem('token') !== null\""}));
    assert_eq!(rep["ok"], true, "{rep}");
}

#[test]
fn ensure_session_logs_in_again_when_a_saved_session_is_not_valid() {
    let file = state_file("stale");
    let mut a = Server::start();
    let mut args = ensure_args("x", "stale");
    args["file"] = json!(file.to_str().unwrap());
    // A session that is not logged in: the login page only.
    let (rep, _) = a.tool("open", json!({"url": args["url"], "vars": args["vars"], "report": true}));
    assert_eq!(rep["ok"], true, "{rep}");
    let (rep, error) = a.tool("save_state", json!({"name": "x", "file": file.to_str().unwrap()}));
    assert!(!error && rep["saved"] == "x", "{rep}");
    drop(a);
    let mut s = Server::start();
    let (rep, error) = s.tool("ensure_session", args.clone());
    assert!(!error && rep["ok"] == true && rep["session"] == "recreated", "{rep}");
    // The login is saved in the file now: a new server restores it.
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("app-token-"), "the file has the login");
    drop(s);
    let mut s = Server::start();
    let (rep, error) = s.tool("ensure_session", args);
    assert!(!error && rep["session"] == "restored", "{rep}");
}

#[test]
fn ensure_session_with_a_wrong_login_is_an_error_with_the_failing_step() {
    let mut s = Server::start();
    let mut args = ensure_args("bad", "wrong");
    args["login"] = json!("type css=\"input[name=email]\" \"nobody@example.test\"\nclick css=\"form button[type=submit]\"");
    let (rep, error) = s.tool("ensure_session", args);
    assert!(error && rep["ok"] == false, "{rep}");
    // The wrong login saved nothing.
    let (rep, error) = s.tool("open", json!({"url": "http://127.0.0.1:1/", "state": "bad"}));
    assert!(error && rep.as_str().unwrap().contains("bad"), "{rep}");
}

#[test]
fn save_state_writes_a_private_file_and_open_restores_a_name() {
    let file = state_file("save");
    let mut s = Server::start();
    let args = ensure_args("n", "save");
    let (rep, _) = s.tool("ensure_session", args.clone());
    assert_eq!(rep["session"], "created", "{rep}");
    let (rep, error) = s.tool("save_state", json!({"name": "n2", "file": file.to_str().unwrap()}));
    assert!(!error && rep["saved"] == "n2" && rep["origins"] == 1, "{rep}");
    #[cfg(unix)]
    assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&file).unwrap().permissions()) & 0o777, 0o600);
    // No reply from now on may hold a value of the file.
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let token = saved.to_string().split("app-token-").nth(1).map(|t| format!("app-token-{}", &t[..1])).expect("a token in the file");
    s.forbid.push(token);
    let (rep, error) = s.tool("open", json!({"url": args["url"], "vars": args["vars"], "state": "n2", "report": true}));
    assert!(!error && rep["ok"] == true, "{rep}");
    let (rep, _) = s.tool("step", json!({"steps": CHECK}));
    assert_eq!(rep["ok"], true, "{rep}");
    let (rep, error) = s.tool("open", json!({"url": args["url"], "state": "nope"}));
    assert!(error && rep.as_str().unwrap().contains("nope"), "{rep}");
}

#[test]
fn open_reads_a_named_session_from_its_file_in_a_new_server() {
    let file = state_file("openfile");
    let mut s = Server::start();
    let mut args = ensure_args("f", "openfile");
    args["file"] = json!(file.to_str().unwrap());
    let (rep, _) = s.tool("ensure_session", args.clone());
    assert_eq!(rep["session"], "created", "{rep}");
    drop(s);
    // A new server has no session in memory: `open` reads it from `file`.
    let mut s = Server::start();
    let (rep, error) = s.tool("open", json!({"url": args["url"], "vars": args["vars"], "state": "f", "file": file.to_str().unwrap(), "report": true}));
    assert!(!error && rep["ok"] == true, "{rep}");
    let (rep, _) = s.tool("step", json!({"steps": CHECK}));
    assert_eq!(rep["ok"], true, "{rep}");
    // No file and no session in memory: still an error that names the session.
    let (rep, error) = s.tool("open", json!({"url": args["url"], "state": "g", "file": "/nonexistent/state.json"}));
    assert!(error && rep.as_str().unwrap().contains("g"), "{rep}");
}

/// A picture of the page right after `open` must show the page: the same picture as a run that waits for the page with `expect`.
#[test]
fn a_shot_right_after_open_shows_the_loaded_page() {
    let mut s = Server::start();
    let base = common::serve_app();
    let url = format!("{base}/");
    let (rep, error) = s.tool("run_steps", json!({"steps": format!("open {url}\nexpect url~\"/login\"\nshot want"), "timeout_ms": 30000}));
    assert!(!error && rep["ok"] == true, "{rep}");
    let want = std::fs::read(format!("{}/want.png", rep["out"].as_str().unwrap())).unwrap();
    let (rep, error) = s.tool("open", json!({"url": url, "timeout_ms": 30000, "report": true}));
    assert!(!error && rep["ok"] == true, "{rep}");
    let (rep, error) = s.tool("step", json!({"steps": "shot got"}));
    assert!(!error && rep["ok"] == true, "{rep}");
    let got = std::fs::read(format!("{}/got.png", rep["out"].as_str().unwrap())).unwrap();
    assert!(got == want, "the picture after open is not the picture of the loaded page ({} and {} bytes)", got.len(), want.len());
}

/// A page that opens an `EventSource` that the server never ends, then sends a POST. Gives the base URL.
fn event_stream_app() -> String {
    use std::io::Read;
    const PAGE: &str = "<!doctype html><body><p id=a>start</p><script>
const es = new EventSource('/sse');
es.onmessage = () => document.getElementById('a').textContent += ' msg';
(async () => { await new Promise(r => setTimeout(r, 200)); const t = await (await fetch('/x', {method: 'POST', body: '{}'})).text();
  const p = document.createElement('p'); p.textContent = 'fetch ' + t; document.body.append(p); })();
</script></body>";
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let (mut head, mut b) = (Vec::new(), [0u8; 1]);
                while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
                    head.push(b[0]);
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let len = head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
                let _ = c.read_exact(&mut vec![0; len]);
                if head.starts_with("GET /sse ") {
                    let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n");
                    for _ in 0..300 {
                        if c.write_all(b"9\r\ndata: x\n\n\r\n").and_then(|_| c.flush()).is_err() {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    return;
                }
                let (ty, body) = if head.starts_with("POST /x ") { ("text/plain", "done") } else { ("text/html", PAGE) };
                let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// An open `EventSource` stream delivers its events while it stays open, and a later request does not wait for its end (bare#375).
#[test]
fn an_open_event_stream_delivers_events_and_does_not_block_a_fetch() {
    let base = event_stream_app();
    let mut s = Server::start();
    let steps = format!("open {base}/\ntimeout 10000\nexpect text=\"fetch done\" visible\nwait js \"document.getElementById('a').textContent.startsWith('start msg msg')\"");
    let (rep, error) = s.tool("run_steps", json!({"steps": steps, "timeout_ms": 30000}));
    assert!(!error && rep["ok"] == true, "{rep}");
    // The stream stays open and does not hold `open`: `network` names it as a stream, not as a fetch.
    let (rep, error) = s.tool("open", json!({"url": format!("{base}/"), "timeout_ms": 10000}));
    assert!(!error && !rep.as_str().unwrap().contains("not ready"), "{rep}");
    let (net, _) = s.tool("network", json!({}));
    assert!(net.as_str().unwrap().contains("pending stream GET"), "{net}");
}

/// The ref of the first snapshot line that holds `what`.
fn ref_of(snap: &str, what: &str) -> String {
    let line = snap.lines().find(|l| l.contains(what)).unwrap_or_else(|| panic!("no {what} in {snap}"));
    line[line.find("[e").unwrap() + 1..line.find(']').unwrap()].to_string()
}

#[test]
fn an_agent_logs_in_with_snapshot_refs_and_no_step_lines() {
    let mut s = Server::start();
    let (_, mut args) = journey("agent");
    args["url"] = json!(format!("{}/", args["vars"]["BASE"].as_str().unwrap()));
    args["timeout_ms"] = json!(30000);
    args["snapshot"] = json!(true);
    let (snap, error) = s.tool("open", args);
    let snap = snap.as_str().unwrap().to_string();
    assert!(!error && snap.starts_with("ok"), "{snap}");
    // The secrets go in as ${NAME}: the agent never sees them, and the password field shows a mask.
    let (r, error) = s.tool("act", json!({"op": "type", "ref": ref_of(&snap, "textbox \"Email"), "text": "${EMAIL}", "snapshot": false}));
    assert!(!error && r.as_str().unwrap().starts_with("ok"), "{r}");
    let (r, error) = s.tool("act", json!({"op": "type", "ref": ref_of(&snap, "(password"), "text": "${PASSWORD}"}));
    let r = r.as_str().unwrap().to_string();
    assert!(!error && r.contains("= \"••••\""), "{r}");
    // Without a snapshot, the receipt still reports the navigation (#382).
    let (r, error) = s.tool("act", json!({"op": "click", "ref": ref_of(&snap, "button"), "snapshot": false}));
    let r = r.as_str().unwrap().to_string();
    assert!(!error && r.contains("navigated:"), "{r}");
    let (r, _) = s.tool("snapshot", json!({}));
    let r = r.as_str().unwrap().to_string();
    assert!(r.contains("link \"Agents\""), "{r}");
    // The login button of the old snapshot left the document (the app changed its route without a new document).
    let (r, error) = s.tool("act", json!({"op": "click", "ref": ref_of(&snap, "button")}));
    assert!(error && r.as_str().unwrap().contains("FAILED detached"), "{r}");
    let (r, _) = s.tool("find", json!({"role": "link", "name": "agents"}));
    assert!(r.as_str().unwrap().starts_with("1 match"), "{r}");
    // A limit that the header fills is an error that names a limit that works (#398).
    let (r, error) = s.tool("read", json!({"max_chars": 20}));
    assert!(error && r.as_str().unwrap().contains("pass at least"), "{r}");
    let (r, _) = s.tool("eval", json!({"js": "1 + 1"}));
    assert_eq!(r, json!(2));
    // A second session runs beside the first.
    let base = journey("agent-two").1["vars"]["BASE"].as_str().unwrap().to_string();
    let (r, error) = s.tool("open", json!({"url": format!("{base}/"), "session": "two", "snapshot": true}));
    assert!(!error, "{r}");
    let (r, _) = s.tool("sessions", json!({}));
    assert!(r.as_str().unwrap().contains("main: ") && r.as_str().unwrap().contains("two: "), "{r}");
}

/// A site with two pages that fail: `/site` throws, fetches a 404 and a port that nobody listens on, and loads a missing script and
/// stylesheet; `/gap` calls an API that the engine does not have. Gives the base URL.
fn broken_site() -> String {
    use std::io::Read;
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let site = format!(
        "<!doctype html><link rel=stylesheet href=/missing.css><script src=/missing.js></script><body><button id=b>Break</button><p>Hello</p><script>
document.getElementById('b').onclick = () => {{ throw new Error('click failed'); }};
fetch('/nope').catch(() => {{}});
fetch('http://127.0.0.1:{dead}/x').catch(() => {{}});
console.warn('careful');
null.boom;
</script></body>"
    );
    let gap = "<!doctype html><body><p>Hello</p><script>navigator.noSuchEngineApi();</script></body>".to_string();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            let (site, gap) = (site.clone(), gap.clone());
            std::thread::spawn(move || {
                let (mut head, mut b) = (Vec::new(), [0u8; 1]);
                while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
                    head.push(b[0]);
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let (status, ty, body) = if head.starts_with("GET /site ") {
                    (200, "text/html", site)
                } else if head.starts_with("GET /gap ") {
                    (200, "text/html", gap)
                } else {
                    (404, "text/plain", "not found".to_string())
                };
                let _ = write!(c, "HTTP/1.1 {status} X\r\nContent-Type: {ty}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

#[test]
fn console_and_network_report_what_failed_and_forget_what_they_gave() {
    let base = broken_site();
    let mut s = Server::start();
    let (r, error) = s.tool("open", json!({"url": format!("{base}/site"), "timeout_ms": 20000}));
    assert!(!error, "{r}");
    s.tool("read", json!({}));
    let (r, _) = s.tool("console", json!({}));
    let r = r.as_str().unwrap().to_string();
    assert!(r.contains("boom") && !r.contains("careful"), "{r}");
    // Nothing is new now.
    let (r, _) = s.tool("console", json!({}));
    assert_eq!(r, json!("no console entries"));
    let (r, _) = s.tool("console", json!({"level": "all", "since_last": false}));
    assert!(r.as_str().unwrap().contains("careful") && r.as_str().unwrap().contains("boom"), "{r}");
    let (r, _) = s.tool("network", json!({}));
    let r = r.as_str().unwrap().to_string();
    assert!(r.contains("404 fetch") && r.contains("/nope"), "{r}");
    assert!(r.contains("script") && r.contains("/missing.js") && r.contains("stylesheet") && r.contains("/missing.css"), "{r}");
    assert!(r.lines().count() >= 4, "the unreachable port is a line too: {r}");
    assert!(!r.contains("/site"), "the document passed, so it is not a failed request: {r}");
    let (r, _) = s.tool("network", json!({}));
    assert_eq!(r, json!("no requests"));
    let (r, _) = s.tool("network", json!({"failed_only": false, "since_last": false, "url_contains": "/site"}));
    assert!(r.as_str().unwrap().starts_with("200 fetch"), "{r}");
    // An entry that arrives between two calls shows in the next one.
    s.tool("act", json!({"op": "click", "loc": "css=\"#b\"", "snapshot": false}));
    let (r, _) = s.tool("console", json!({}));
    assert!(r.as_str().unwrap().contains("click failed"), "{r}");
}

#[test]
fn diagnose_tells_a_site_problem_from_a_bare_gap() {
    let base = broken_site();
    let mut s = Server::start();
    s.tool("open", json!({"url": format!("{base}/site"), "timeout_ms": 20000}));
    let (r, error) = s.tool("diagnose", json!({}));
    let r = r.as_str().unwrap().to_string();
    assert!(!error && r.starts_with("document: ") && r.contains("HTTP 200"), "{r}");
    assert!(r.contains("uncaught:") && r.contains("failed script:") && r.contains("failed stylesheet:") && r.contains("failed fetch:"), "{r}");
    assert!(r.contains("DOM nodes"), "{r}");
    assert!(r.lines().last().unwrap().starts_with("verdict: site problem"), "{r}");
    s.tool("open", json!({"url": format!("{base}/gap"), "timeout_ms": 20000}));
    let (r, _) = s.tool("diagnose", json!({}));
    let r = r.as_str().unwrap().to_string();
    assert!(r.contains("missing feature:") && r.lines().last().unwrap().starts_with("verdict: bare gap"), "{r}");
    // A page that does not exist: the document answered 404. The live session keeps the error page, so the verdict names the status.
    s.tool("open", json!({"url": format!("{base}/none"), "timeout_ms": 20000}));
    let (r, _) = s.tool("diagnose", json!({}));
    assert!(r.as_str().unwrap().contains("HTTP 404") && r.as_str().unwrap().contains("verdict: site problem"), "{r}");
}

#[test]
fn a_failed_step_names_the_script_error_that_may_explain_it() {
    let base = broken_site();
    let mut s = Server::start();
    s.tool("open", json!({"url": format!("{base}/site"), "timeout_ms": 20000}));
    // The load logs go to the history of this read, not to the report of the step.
    s.tool("read", json!({}));
    let (r, _) = s.tool("step", json!({"steps": "timeout 300\nclick css=\"#b\"\nclick text=\"Missing\"", "compact": true}));
    let r = r.as_str().unwrap().to_string();
    assert!(r.contains("FAILED not_found") && r.contains("hint: the page script failed: ") && r.contains("click failed"), "{r}");
    // With no exception in the call, the usual hint stays.
    let (r, _) = s.tool("step", json!({"steps": "click text=\"Missing\"", "compact": true}));
    assert!(r.as_str().unwrap().contains("hint: take a snapshot"), "{r}");
}

/// A live page goes on between calls, as a browser tab does (#471): its timers run while the agent does not call.
#[test]
fn a_live_page_runs_its_timers_between_calls() {
    let mut s = Server::start();
    let page = one_page("<!doctype html><title>t</title><script>var c = 0; setInterval(function () { c++ }, 50)</script>".to_string());
    let (r, error) = s.tool("open", json!({"url": page}));
    assert!(!error, "{r}");
    let count = |s: &mut Server| s.tool("eval", json!({"js": "c"})).0.as_f64().unwrap();
    let before = count(&mut s);
    std::thread::sleep(std::time::Duration::from_millis(1000));
    let after = count(&mut s);
    assert!(after - before >= 10.0, "the timer ran {} times in 1 s between calls", after - before);
}

/// Serves one page at every path. Gives the base URL.
fn one_page(html: String) -> String {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            let html = html.clone();
            std::thread::spawn(move || {
                let (mut head, mut b) = (Vec::new(), [0u8; 1]);
                while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
                    head.push(b[0]);
                }
                let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}", html.len());
            });
        }
    });
    format!("http://127.0.0.1:{port}/")
}

fn snap_of(html: &str, args: Value) -> String {
    let mut s = Server::start();
    let mut a = json!({"url": one_page(html.to_string()), "snapshot": true, "timeout_ms": 30000});
    a.as_object_mut().unwrap().extend(args.as_object().unwrap().clone());
    let (r, error) = s.tool("open", a);
    assert!(!error, "{r}");
    r.as_str().unwrap().to_string()
}

/// A slotted light child shows once, where its `<slot>` is, in the order of the flat tree.
#[test]
fn a_slotted_child_shows_once_at_its_slot() {
    let snap = snap_of(
        "<body><div id=h><button>Middle</button></div><script>\
         document.getElementById('h').attachShadow({mode:'open'}).innerHTML='<button>First</button><slot></slot><button>Last</button>';</script>",
        json!({}),
    );
    let names: Vec<&str> = snap.lines().filter(|l| l.contains("button")).collect();
    assert_eq!(names.len(), 3, "{snap}");
    assert!(names[0].contains("First") && names[1].contains("Middle") && names[2].contains("Last"), "{snap}");
}

/// A pointer subtree becomes one `clickable`, named by its text; its children are not clickable on their own.
#[test]
fn a_pointer_subtree_is_one_clickable() {
    let snap = snap_of(
        "<style>.p{cursor:pointer}.q{cursor:default}</style><body><div class=p><span>Open menu</span><b>now</b></div><div>plain</div>\
         <div class=p><div class=q>inner</div></div>",
        json!({}),
    );
    let c: Vec<&str> = snap.lines().filter(|l| l.contains("clickable")).collect();
    assert_eq!(c.len(), 2, "{snap}");
    assert!(c[0].contains("Open menu now"), "{snap}");
}

/// A control below the fold says so; one in view does not.
#[test]
fn a_control_outside_the_viewport_is_marked_offscreen() {
    let snap = snap_of("<body><button>Near</button><div style='height:3000px'></div><button>Far</button>", json!({}));
    let near = snap.lines().find(|l| l.contains("Near")).unwrap();
    let far = snap.lines().find(|l| l.contains("Far")).unwrap();
    assert!(!near.contains("offscreen") && far.contains("(offscreen)"), "{snap}");
}

/// An iframe shows as a line with its title or source. Its content is not in the snapshot: bare does not load child documents.
#[test]
fn an_iframe_shows_as_a_line() {
    let snap = snap_of("<body><iframe title=\"Card form\" src=\"/inner\" width=300 height=100></iframe>", json!({}));
    assert!(snap.lines().any(|l| l.contains("iframe \"Card form\"")), "{snap}");
}

/// A page of about 5,000 elements: the snapshot (with the flat tree, `cursor` and offscreen work) stays under 30 ms once laid out. Best of 5.
#[test]
fn a_snapshot_of_5000_elements_takes_under_30_ms() {
    let rows: String = (0..830)
        .map(|i| format!("<div><span>Item {i}</span><a href=\"/x{i}\">link {i}</a><button>Go {i}</button><div class=p><b>pick {i}</b></div></div>"))
        .collect();
    let mut s = Server::start();
    let (r, error) = s.tool("open", json!({"url": one_page(format!("<style>.p{{cursor:pointer}}</style><body>{rows}")), "timeout_ms": 30000}));
    assert!(!error, "{r}");
    let mut best = u128::MAX;
    for _ in 0..5 {
        let t = std::time::Instant::now();
        let (r, error) = s.tool("snapshot", json!({"max_chars": 2000000}));
        best = best.min(t.elapsed().as_millis());
        assert!(!error && r.as_str().unwrap().contains("clickable \"pick 829\""), "{r}");
    }
    assert!(best < 30, "best snapshot time {best} ms");
}

#[test]
fn an_agent_fills_a_form_with_select_check_hover_dblclick_and_upload() {
    let mut s = Server::start();
    let base = common::serve();
    let (r, error) = s.tool("open", json!({"url": format!("{base}/forms"), "snapshot": true}));
    let snap = r.as_str().unwrap().to_string();
    assert!(!error && snap.contains("combobox \"Country\" = \"Kenya\"") && snap.contains("listbox \"Fruit\""), "{snap}");
    // This test reads the whole outline after each action, not the changes.
    let act = |s: &mut Server, mut args: Value| {
        args["diff"] = json!(false);
        let (r, error) = s.tool("act", args);
        (r.as_str().unwrap().to_string(), error)
    };
    // select: by label, then by value; input and change fire once for each change, none for the same pick again.
    let (r, error) = act(&mut s, json!({"op": "select", "ref": ref_of(&snap, "combobox"), "text": "Tanzania", "mode": "all"}));
    assert!(!error && r.contains("= \"Tanzania\"") && r.contains("events: country:input country:change"), "{r}");
    let (r, error) = act(&mut s, json!({"op": "select", "loc": "css=\"#country\"", "text": "tz", "mode": "all"}));
    assert!(!error && r.contains("events: country:input country:change\""), "{r}");
    let (r, error) = act(&mut s, json!({"op": "select", "loc": "css=\"#country\"", "text": "Narnia"}));
    assert!(error && r.contains("FAILED no_option") && r.contains("Kenya"), "{r}");
    // A multiple select takes several options.
    let (r, error) = act(&mut s, json!({"op": "select", "loc": "css=\"#fruit\"", "options": ["Apple", "c"]}));
    assert!(!error && r.contains("= \"Apple, Cherry\""), "{r}");
    let (r, error) = act(&mut s, json!({"op": "select", "loc": "css=\"#country\"", "options": ["Kenya", "Tanzania"]}));
    assert!(error && r.contains("one option"), "{r}");
    // check and uncheck click only when the state differs.
    let (r, error) = act(&mut s, json!({"op": "check", "loc": "label=\"Agree\""}));
    assert!(!error && r.contains("checkbox \"Agree\" (checked"), "{r}");
    let (r, _) = act(&mut s, json!({"op": "check", "loc": "label=\"Agree\""}));
    assert!(r.contains("(checked"), "{r}");
    let (r, error) = act(&mut s, json!({"op": "uncheck", "loc": "label=\"Agree\""}));
    assert!(!error && r.contains("checkbox \"Agree\"") && !r.contains("checkbox \"Agree\" (checked"), "{r}");
    let (r, error) = act(&mut s, json!({"op": "check", "loc": "css=\"#dbl\""}));
    assert!(error && r.contains("FAILED not_checkable"), "{r}");
    // hover: the :hover rule shows the menu, and mouseenter fires.
    let r = s.tool("read", json!({})).0.as_str().unwrap().to_string();
    assert!(!r.contains("Menu is open") && r.contains("cold"), "{r}");
    let (r, error) = act(&mut s, json!({"op": "hover", "loc": "text=\"Hover target\"", "mode": "all"}));
    assert!(!error && r.contains("Menu is open") && r.contains("hot"), "{r}");
    // dblclick fires dblclick, and a click alone does not.
    let (_, _) = act(&mut s, json!({"op": "click", "loc": "css=\"#dbl\""}));
    assert_eq!(s.tool("eval", json!({"js": "document.getElementById('dbls').textContent"})).0, json!("single"));
    let (r, error) = act(&mut s, json!({"op": "dblclick", "loc": "css=\"#dbl\"", "mode": "all"}));
    assert!(!error && r.contains("double"), "{r}");
    // upload: a plain form posts it as multipart, and a fetch with FormData sends the same file.
    let dir = std::env::temp_dir().join(format!("bare-mcp-test-upload-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("notes.txt");
    std::fs::write(&file, "twelve bytes").unwrap();
    let file = file.to_str().unwrap().to_string();
    let (r, error) = act(&mut s, json!({"op": "upload", "loc": "css=\"input[type=file]\"", "files": [file.clone(), file.clone()]}));
    assert!(error && r.contains("FAILED one_file"), "{r}");
    let (r, error) = act(&mut s, json!({"op": "upload", "loc": "css=\"input[type=file]\"", "files": [dir.join("missing.txt").to_str().unwrap()]}));
    assert!(error && r.contains("FAILED bad_file"), "{r}");
    // A file over 100 MiB is refused with a clear error (a sparse file: no disk is used).
    let big = dir.join("big.bin");
    std::fs::File::create(&big).unwrap().set_len((100 << 20) + 1).unwrap();
    let (r, error) = act(&mut s, json!({"op": "upload", "loc": "css=\"input[type=file]\"", "files": [big.to_str().unwrap()]}));
    assert!(error && r.contains("FAILED bad_file") && r.contains("over the 100 MiB limit"), "{r}");
    let (r, error) = act(&mut s, json!({"op": "upload", "loc": "css=\"input[type=file]\"", "files": [file.clone()], "snapshot": false}));
    assert!(!error && r.starts_with("ok"), "{r}");
    assert_eq!(s.tool("eval", json!({"js": "var f = document.querySelector('input[type=file]').files; f.length + ' ' + f[0].name + ' ' + f[0].size + ' ' + f[0].type"})).0, json!("1 notes.txt 12 text/plain"));
    let (r, error) = act(&mut s, json!({"op": "click", "loc": "css=\"#go\""}));
    assert!(!error, "{r}");
    let (r, _) = act(&mut s, json!({"op": "wait", "loc": "text~\"fetched uploaded\"", "snapshot": false}));
    assert!(r.starts_with("ok"), "{r}");
    assert_eq!(s.tool("eval", json!({"js": "document.getElementById('fetched').textContent"})).0, json!("fetched uploaded: notes.txt 12"));
    let (r, error) = act(&mut s, json!({"op": "click", "loc": "role=button name=\"Send file\"", "mode": "all"}));
    assert!(!error && r.contains("navigated:") && r.contains("uploaded: notes.txt 12"), "{r}");
    // The same actions are step lines.
    let (r, error) = s.tool("run_steps", json!({"steps": format!("open {base}/forms\nselect css=\"#country\" \"Tanzania\"\ncheck label=\"Agree\"\nexpect label=\"Agree\" checked\nuncheck label=\"Agree\"\nhover text=\"Hover target\"\ndblclick css=\"#dbl\"\nexpect css=\"#dbls\" text=\"double\"\nupload css=\"input[type=file]\" \"{file}\"\n")}));
    assert!(!error && r["ok"] == true, "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Click each link or button of the downloads page and check the file that the receipt names: its bytes, and that the page did not change.
#[test]
fn a_download_goes_to_a_file_and_the_page_stays() {
    let mut s = Server::start();
    let base = common::serve();
    let dir = std::env::temp_dir().join(format!("bare-mcp-test-downloads-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let url = format!("{base}/downloads");
    let (snap, error) = s.tool("open", json!({"url": url, "snapshot": true, "downloads_dir": dir.to_str().unwrap(), "timeout_ms": 20000}));
    let snap = snap.as_str().unwrap().to_string();
    assert!(!error && snap.starts_with("ok"), "{snap}");
    let bytes = |name: &str| std::fs::read(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
    let click = |s: &mut Server, what: &str| {
        let (r, error) = s.tool("act", json!({"op": "click", "ref": ref_of(&snap, what), "snapshot": false}));
        let r = r.as_str().unwrap().to_string();
        assert!(!error && !r.contains("navigated:"), "{r}");
        r
    };
    let r = click(&mut s, "\"Report\"");
    assert!(r.contains(&format!("downloaded: {}/report.csv ({} bytes, text/csv)", dir.display(), common::PAYLOAD_CSV.len())), "{r}");
    assert_eq!(bytes("report.csv"), common::PAYLOAD_CSV);
    let r = click(&mut s, "\"Invoice\"");
    assert!(r.contains("invoice.pdf (") && r.contains("application/pdf)"), "{r}");
    assert_eq!(bytes("invoice.pdf"), common::PAYLOAD_PDF);
    let r = click(&mut s, "\"Notes\"");
    assert!(r.contains("my notes.txt (11 bytes, text/plain)"), "{r}");
    assert_eq!(bytes("my notes.txt"), b"plain notes");
    // A script makes a Blob and clicks a link: the name loses its path; a second download of a name never replaces the first.
    let r = click(&mut s, "\"Export\"");
    assert!(r.contains("export.csv (8 bytes, text/csv)"), "{r}");
    assert_eq!(bytes("export.csv"), b"x,y\n1,2\n");
    let r = click(&mut s, "\"Report\"");
    assert!(r.contains("report (1).csv"), "{r}");
    assert_eq!(bytes("report (1).csv"), common::PAYLOAD_CSV);
    // A download that fails is named, and leaves no file.
    let r = click(&mut s, "\"Missing\"");
    assert!(r.contains("download failed: ") && r.contains("/files/missing.zip: status 404"), "{r}");
    assert!(!dir.join("missing.zip").exists());
    let (r, _) = s.tool("eval", json!({"js": "location.href"}));
    assert_eq!(r, json!(url));
    let (list, error) = s.tool("downloads", json!({}));
    let list = list.as_str().unwrap();
    assert!(!error && list.lines().count() == 6 && list.contains("failed: ") && list.contains("invoice.pdf (") && list.contains("from blob:"), "{list}");
    assert!(list.lines().next().unwrap().starts_with("#1 "), "{list}");
    // An ended download takes no operation; an unknown op and a missing id are named.
    let (r, error) = s.tool("downloads", json!({"op": "pause", "id": 1}));
    assert!(error && r.as_str().unwrap().contains("has ended"), "{r}");
    let (r, error) = s.tool("downloads", json!({"op": "pause"}));
    assert!(error && r.as_str().unwrap().contains("give id"), "{r}");
    let (r, error) = s.tool("downloads", json!({"op": "stop", "id": 1}));
    assert!(error && r.as_str().unwrap().contains("unknown op"), "{r}");
}

/// A server of one page. `/slow.png` never answers for 5 s, so a picture stays on its way. Gives the base URL.
fn page_server(page: String) -> String {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            let page = page.clone();
            std::thread::spawn(move || {
                let (mut head, mut b) = (Vec::new(), [0u8; 1]);
                while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
                    head.push(b[0]);
                }
                if String::from_utf8_lossy(&head).starts_with("GET /slow.png") {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    return;
                }
                let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len());
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// A snapshot over budget says what it cut, with the counts and the refs, and `offset` reaches the cut part.
#[test]
fn a_cut_snapshot_says_what_was_cut_and_offset_reaches_it() {
    let rows: String = (0..400).map(|i| format!("<li><a href='/p{i}'>link number {i}</a></li>")).collect();
    let base = page_server(format!("<!doctype html><title>big</title><h1>Big</h1><ul>{rows}</ul>"));
    let mut s = Server::start();
    let (r, error) = s.tool("open", json!({"url": format!("{base}/"), "snapshot": false, "timeout_ms": 30000}));
    assert!(!error, "{r}");
    let (r, _) = s.tool("snapshot", json!({"max_chars": 2000}));
    let r = r.as_str().unwrap().to_string();
    let cut = r.lines().find(|l| l.contains("lines cut")).unwrap_or_else(|| panic!("no cut line in {r}"));
    assert!(cut.contains("lines cut (e") && cut.contains("–e") && cut.contains("offset="), "{cut}");
    let offset: usize = cut.split("offset=").nth(1).unwrap().split(' ').next().unwrap().parse().unwrap();
    // The first cut ref is valid and the page after the offset starts with it.
    let first = cut.split("(e").nth(1).unwrap().split('–').next().unwrap();
    let (p2, error) = s.tool("snapshot", json!({"max_chars": 2000, "offset": offset}));
    let p2 = p2.as_str().unwrap();
    assert!(!error && p2.contains(&format!("[e{first}]")), "{first} not in {p2}");
}

/// A read on a settled page does not wait for a picture that is on its way (it waited the 300 ms of the cap).
#[test]
fn a_read_does_not_wait_for_a_stalled_picture() {
    let base = page_server("<!doctype html><title>t</title><h1>Hi</h1><p>static</p><img src='/slow.png' width=10 height=10>".into());
    let mut s = Server::start();
    let (r, error) = s.tool("open", json!({"url": format!("{base}/"), "snapshot": false, "timeout_ms": 30000}));
    assert!(!error, "{r}");
    s.tool("read", json!({}));
    for tool in ["read", "links", "snapshot"] {
        let t = std::time::Instant::now();
        let (r, error) = s.tool(tool, json!({}));
        let ms = t.elapsed().as_millis();
        assert!(!error && ms < 100, "{tool} took {ms} ms: {r}");
    }
}

/// A page server with a slow route: `/slow` answers after 700 ms.
fn naming_app() -> String {
    use std::io::Read;
    const PAGES: &[(&str, &str)] = &[
        // A search box with no size comes first in the tree, and the visible one second.
        ("/rank", "<!doctype html><body><input type=search aria-label=Zero style='width:0;height:0;padding:0;border:0'><input type=search aria-label=Main>"),
        // A script gives the box the role `combobox` after a reply, as a typeahead widget does.
        ("/flip", "<!doctype html><body><input type=search id=m aria-label=Flip><script>fetch('/slow').then(() => document.getElementById('m').setAttribute('role', 'combobox'))</script>"),
        // The login page of a news site: the labels sit in the cell before the field.
        ("/login", "<!doctype html><body><form><table><tr><td>username:</td><td><input type=text name=acct></td></tr><tr><td>password:</td><td><input type=password name=pw></td></tr></table>\
            <p>Nickname <input name=nick></p><p><b>Age</b> <input name=age></p><div><input name=bare_name></div><input name=holder placeholder=Holder><input name=named aria-label=Named></form>"),
        // An info box: the word "Developer" is in a header cell, in a paragraph and in a link.
        ("/scope", "<!doctype html><body><table class=info><tr><th>Developer</th><td>A team</td></tr><tr><th>Typing</th><td>Static</td></tr></table>\
            <p>Developer Ann wrote it.</p><a href=/scope>Developers</a>"),
        // Links whose names hold a count, and a timer that never ends, as a big site has.
        ("/near", "<!doctype html><body><a href=/near>Issues (5k+)</a> <a href=/near>Issues closed</a> <a href=/near>Pull requests</a><script>setInterval(() => {}, 50)</script>"),
        // A to-do list with one item, and nothing that runs.
        ("/todo", "<!doctype html><body><ul><li><input type=checkbox aria-label=\"Toggle Buy milk\"> Buy milk</li></ul><input type=checkbox aria-label=\"Toggle all\">"),
    ];
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let (mut head, mut b) = (Vec::new(), [0u8; 1]);
                while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
                    head.push(b[0]);
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                if path == "/slow" {
                    std::thread::sleep(std::time::Duration::from_millis(700));
                }
                let body = PAGES.iter().find(|(p, _)| *p == path).map_or("ok", |(_, b)| b);
                let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

#[test]
fn find_lists_the_visible_match_first() {
    let base = naming_app();
    let mut s = Server::start();
    s.tool("open", json!({"url": format!("{base}/rank"), "snapshot": false}));
    let (r, _) = s.tool("find", json!({"role": "searchbox"}));
    let lines: Vec<&str> = r.as_str().unwrap().lines().collect();
    assert!(lines[0].starts_with("2 match"), "{r}");
    assert!(lines[1].contains("\"Main\""), "the visible box is first: {r}");
}

#[test]
fn a_role_that_a_script_sets_after_a_reply_is_the_same_in_every_call() {
    let base = naming_app();
    let mut s = Server::start();
    s.tool("open", json!({"url": format!("{base}/flip"), "snapshot": false}));
    // The first call must not see the role from before the script ran.
    let (r, _) = s.tool("find", json!({"role": "searchbox"}));
    assert!(r.as_str().unwrap().starts_with("0 match"), "{r}");
    let (r, _) = s.tool("find", json!({"role": "combobox"}));
    assert!(r.as_str().unwrap().contains("combobox \"Flip\""), "{r}");
}

#[test]
fn unnamed_form_fields_take_a_name_from_what_sits_before_them() {
    let base = naming_app();
    let mut s = Server::start();
    let (r, _) = s.tool("open", json!({"url": format!("{base}/login"), "snapshot": true}));
    let r = r.as_str().unwrap();
    for want in ["textbox \"username:\"", "textbox \"password:\" (password)", "textbox \"Nickname\"", "textbox \"Age\"", "textbox \"bare_name\"", "textbox \"Holder\"", "textbox \"Named\""] {
        assert!(r.contains(want), "missing {want} in {r}");
    }
    // The fallback names a locator too.
    let (r, _) = s.tool("find", json!({"role": "textbox", "name": "username"}));
    assert!(r.as_str().unwrap().starts_with("1 match"), "{r}");
}

/// A page server for one fixture page: `/` gives `page`, every other path (a form submission) gives a page that says `results`.
fn focus_page_server(page: &'static str) -> String {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let (mut head, mut b) = (Vec::new(), [0u8; 1]);
                while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
                    head.push(b[0]);
                }
                let root = String::from_utf8_lossy(&head).starts_with("GET / ");
                let body = if root { page } else { "<!doctype html><title>r</title><p>results page</p>" };
                let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            });
        }
    });
    format!("http://127.0.0.1:{port}/")
}

/// Two search forms, as Wikipedia has: the header input `a` and the page input `b`. The script of `a` moves the focus to `b` at the first
/// key, and puts the page of `b` before it.
const TWO_SEARCH: &str = "<!doctype html><body>
<form id=fa action=/s><input id=a name=q type=search aria-label=\"Header search\"></form>
<form id=fb action=/s><input id=b name=q type=search aria-label=\"Page search\"></form>
<form id=fc action=/s><input id=c name=q type=search aria-label=\"Quiet search\"></form>
<input id=d aria-label=\"Not a form\">
<div style=\"display:none\"><input id=h aria-label=\"Hidden search\"></div>
<script>
document.getElementById('a').addEventListener('keydown', () => document.getElementById('b').focus());
</script></body>";

#[test]
fn typing_into_an_input_that_hands_the_focus_away_fails_and_never_drops_the_first_key() {
    let mut s = Server::start();
    let (snap, _) = s.tool("open", json!({"url": focus_page_server(TWO_SEARCH), "snapshot": true, "timeout_ms": 20000}));
    let snap = snap.as_str().unwrap().to_string();
    let (r, error) = s.tool("act", json!({"op": "type", "ref": ref_of(&snap, "Header search"), "text": "bare browser", "snapshot": false}));
    let r = r.as_str().unwrap().to_string();
    let (v, _) = s.tool("eval", json!({"js": "['a', 'b'].map(i => document.getElementById(i).value).join('|')"}));
    // Either all the text is in `a`, or the receipt says that the type failed. The text is never cut or in another box without a failure.
    assert!(v == json!("bare browser|") || (error && r.contains("FAILED") && r.contains("focus")), "{r} {v}");
    assert!(!(v == json!("|are browser") && !error), "{r} {v}");
}

#[test]
fn typing_into_a_quiet_input_puts_all_the_text_in_it() {
    let mut s = Server::start();
    let (snap, _) = s.tool("open", json!({"url": focus_page_server(TWO_SEARCH), "snapshot": true, "timeout_ms": 20000}));
    let snap = snap.as_str().unwrap().to_string();
    let (r, error) = s.tool("act", json!({"op": "type", "ref": ref_of(&snap, "Quiet search"), "text": "bare browser", "snapshot": false}));
    assert!(!error, "{r}");
    let (v, _) = s.tool("eval", json!({"js": "document.getElementById('c').value"}));
    assert_eq!(v, json!("bare browser"));
}

#[test]
fn press_enter_on_a_ref_submits_the_form_of_that_element() {
    let mut s = Server::start();
    let (snap, _) = s.tool("open", json!({"url": focus_page_server(TWO_SEARCH), "snapshot": true, "timeout_ms": 20000}));
    let snap = snap.as_str().unwrap().to_string();
    let (_, error) = s.tool("act", json!({"op": "type", "ref": ref_of(&snap, "Quiet search"), "text": "x", "snapshot": false}));
    assert!(!error);
    // The focus is on `c`; press on `b` must act on `b`, not on the focused element: `b` is empty but its form still submits.
    let (r, error) = s.tool("act", json!({"op": "press", "ref": ref_of(&snap, "Page search"), "key": "Enter"}));
    let r = r.as_str().unwrap().to_string();
    assert!(!error && r.contains("navigated:") && r.contains("/s?q="), "{r}");
}

#[test]
fn press_that_changes_nothing_says_so() {
    let mut s = Server::start();
    let (snap, _) = s.tool("open", json!({"url": focus_page_server(TWO_SEARCH), "snapshot": true, "timeout_ms": 20000}));
    let snap = snap.as_str().unwrap().to_string();
    let (r, error) = s.tool("act", json!({"op": "press", "ref": ref_of(&snap, "Not a form"), "key": "Enter", "snapshot": false}));
    let r = r.as_str().unwrap().to_string();
    assert!(!error && r.contains("no effect"), "{r}");
}

#[test]
fn press_that_types_in_a_contenteditable_does_not_say_no_effect() {
    let mut s = Server::start();
    let page = "<!doctype html><body><div id=e contenteditable=true aria-label=\"Editor\">a </div></body>";
    let (snap, _) = s.tool("open", json!({"url": focus_page_server(page), "snapshot": true, "timeout_ms": 20000}));
    let snap = snap.as_str().unwrap().to_string();
    let r = ref_of(&snap, "Editor");
    for key in ["Control+b", "x", "Control+b", "y"] {
        let (out, error) = s.tool("act", json!({"op": "press", "ref": r, "key": key, "snapshot": false}));
        let out = out.as_str().unwrap().to_string();
        assert!(!error, "{out}");
        // Control+b alone changes no document node: only the typed keys must not say it.
        if key.len() == 1 {
            assert!(!out.contains("no effect"), "{key}: {out}");
        }
    }
    let (v, _) = s.tool("eval", json!({"js": "document.getElementById('e').innerHTML"}));
    assert_eq!(v, json!("a <b>x</b>y"));
}

#[test]
fn acting_on_a_hidden_element_says_it_is_hidden() {
    let mut s = Server::start();
    let (snap, _) = s.tool("open", json!({"url": focus_page_server(TWO_SEARCH), "snapshot": true, "timeout_ms": 20000}));
    let r = ref_of(snap.as_str().unwrap(), "Not a form");
    let (_, _) = s.tool("eval", json!({"js": "document.getElementById('d').style.display = 'none'"}));
    for args in [json!({"op": "press", "key": "Enter"}), json!({"op": "type", "text": "x"}), json!({"op": "click"})] {
        let mut args = args;
        args["ref"] = json!(r);
        args["snapshot"] = json!(false);
        let (out, error) = s.tool("act", args.clone());
        assert!(error && out.as_str().unwrap().contains("hidden"), "{args} {out}");
    }
}

/// Pages for the open tests: a page that fills its body after a slow request, one whose request never ends, an error page, a redirect.
fn open_pages(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    let path = target.split('?').next().unwrap_or("/");
    let page = |script: &str| (200, "text/html", format!("<!doctype html><title>Slow</title><body><script>{script}</script></body>").into_bytes());
    match path {
        "/slow" => page("fetch('/data?delay=400').then(r => r.text()).then(t => { document.body.textContent = t; })"),
        "/never" => page("fetch('/data?delay=5000').then(r => r.text()).then(t => { document.body.textContent = t; })"),
        "/data" => {
            let ms = target.split("delay=").nth(1).and_then(|d| d.parse().ok()).unwrap_or(0);
            std::thread::sleep(std::time::Duration::from_millis(ms));
            (200, "text/plain", b"rendered late".to_vec())
        }
        "/blocked" => (403, "text/html", b"<!doctype html><title>Access denied</title><body><h1>Blocked</h1><p>Try later</p></body>".to_vec()),
        "/old" => (302, "text/plain", b"/login?reason=guest&token=abc123&dest=/r/rust".to_vec()),
        "/login" => (200, "text/html", b"<!doctype html><title>Login</title><body><p>Sign in please</p></body>".to_vec()),
        "/empty" => (200, "text/html", b"<!doctype html><title>Empty</title><body></body>".to_vec()),
        _ => (404, "text/plain", b"no".to_vec()),
    }
}

#[test]
fn open_waits_for_a_page_that_fills_its_body_after_a_request() {
    let base = common::serve_route(open_pages);
    let mut s = Server::start();
    let (rep, error) = s.tool("open", json!({"url": format!("{base}/slow")}));
    assert!(!error, "{rep}");
    assert!(!rep.to_string().contains("not ready"), "{rep}");
    let (text, _) = s.tool("eval", json!({"js": "document.body.textContent"}));
    assert!(text.to_string().contains("rendered late"), "open returned before the page rendered: {text}");
}

#[test]
fn open_says_not_ready_when_the_time_limit_passes_on_an_empty_page() {
    let base = common::serve_route(open_pages);
    let mut s = Server::start();
    let (rep, _) = s.tool("open", json!({"url": format!("{base}/never"), "timeout_ms": 600, "snapshot": true}));
    let r = rep.as_str().unwrap();
    assert!(r.contains("not ready: body empty after") && r.contains("1 request pending"), "{r}");
}

#[test]
fn open_does_not_wait_for_a_page_that_is_empty_and_idle() {
    let base = common::serve_route(open_pages);
    let mut s = Server::start();
    let t = std::time::Instant::now();
    let (rep, _) = s.tool("open", json!({"url": format!("{base}/empty"), "snapshot": true}));
    assert!(t.elapsed().as_millis() < 3000, "{rep}");
    assert!(rep.as_str().unwrap().contains("not ready: body empty"), "{rep}");
}

#[test]
fn open_of_an_error_page_gives_the_status_and_keeps_the_page() {
    let base = common::serve_route(open_pages);
    let mut s = Server::start();
    let (rep, _) = s.tool("open", json!({"url": format!("{base}/blocked"), "snapshot": true}));
    let r = rep.as_str().unwrap();
    assert!(r.contains("status 403") && r.contains(&format!("{base}/blocked")) && r.contains("Access denied") && r.contains("Try later"), "{r}");
    let (rep, error) = s.tool("snapshot", json!({}));
    assert!(!error && rep.as_str().unwrap().contains("Blocked"), "{rep}");
}

#[test]
fn open_shows_the_redirect_and_full_urls_with_secret_values_hidden() {
    let base = common::serve_route(open_pages);
    let mut s = Server::start();
    let (rep, _) = s.tool("open", json!({"url": format!("{base}/old"), "snapshot": true}));
    let r = rep.as_str().unwrap();
    assert!(r.contains(&format!("redirected: {base}/login?reason=guest&token=…&dest=/r/rust")), "{r}");
    // `act` shows the full URL of a navigation too, with the secret values hidden.
    let (rep, _) = s.tool("act", json!({"op": "goto", "url": format!("{base}/login?a=1&session=zz9"), "snapshot": false}));
    let r = rep.as_str().unwrap();
    assert!(r.contains(&format!("navigated: {base}/login?a=1&session=…")) && !r.contains("zz9"), "{r}");
}

#[test]
fn open_of_a_file_url_fails_at_once_with_the_reason() {
    let mut s = Server::start();
    let (rep, error) = s.tool("open", json!({"url": "file:///etc/hostname", "snapshot": true}));
    assert!(error && rep.as_str().unwrap().contains("not an http(s) URL"), "{rep}");
}

/// A server of fixed pages: `(path, html)`. Gives its origin.
fn pages(list: &'static [(&str, &str)]) -> String {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let (mut head, mut b) = (Vec::new(), [0u8; 1]);
                while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
                    head.push(b[0]);
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let path = head.split(' ').nth(1).unwrap_or("/").to_string();
                let (code, body) = match list.iter().find(|(p, _)| *p == path) {
                    Some((_, h)) => (200, *h),
                    None => (404, "not found"),
                };
                let _ = write!(c, "HTTP/1.1 {code} X\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

const DIALOGS: &str = "<!doctype html><body><p id=out>none</p>
<button onclick=\"alert('Hi'); document.getElementById('out').textContent='alerted'\">Alert</button>
<button onclick=\"document.getElementById('out').textContent='confirm=' + confirm('Delete item?')\">Confirm</button>
<button onclick=\"document.getElementById('out').textContent='prompt=' + prompt('Name?', 'dflt')\">Prompt</button></body>";

/// `alert`, `confirm` and `prompt` never hang: the act receipt names each dialog and its answer, and `dialog` and `prompt_text` choose it.
#[test]
fn dialogs_show_in_the_receipt_and_the_agent_chooses_the_answer() {
    let base = pages(&[("/", DIALOGS)]);
    let mut s = Server::start();
    s.tool("open", json!({"url": format!("{base}/")}));
    let act = |s: &mut Server, name: &str, extra: Value| {
        let mut a = json!({"op": "click", "loc": format!("role=button name=\"{name}\"")});
        a.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let (r, e) = s.tool("act", a);
        assert!(!e, "{r}");
        let (page, _) = s.tool("read", json!({}));
        format!("{}\n{}", r.as_str().unwrap(), page.as_str().unwrap())
    };
    let r = act(&mut s, "Alert", json!({}));
    assert!(r.contains("dialog: alert \"Hi\" -> accepted") && r.contains("alerted"), "{r}");
    let r = act(&mut s, "Confirm", json!({"dialog": "dismiss"}));
    assert!(r.contains("dialog: confirm \"Delete item?\" -> dismissed") && r.contains("confirm=false"), "{r}");
    let r = act(&mut s, "Confirm", json!({"dialog": "accept"}));
    assert!(r.contains("dialog: confirm \"Delete item?\" -> accepted") && r.contains("confirm=true"), "{r}");
    let r = act(&mut s, "Prompt", json!({"prompt_text": "bob"}));
    assert!(r.contains("dialog: prompt \"Name?\" -> accepted \"bob\"") && r.contains("prompt=bob"), "{r}");
    let r = act(&mut s, "Prompt", json!({}));
    assert!(r.contains("prompt=dflt"), "{r}");
    let r = act(&mut s, "Prompt", json!({"dialog": "dismiss"}));
    assert!(r.contains("-> dismissed") && r.contains("prompt=null"), "{r}");
    let (r, _) = s.tool("act", json!({"op": "click", "loc": "role=button name=\"Alert\"", "dialog": "later"}));
    assert!(r.as_str().unwrap().contains("dialog must be accept or dismiss"), "{r}");
}

const TABS: &str = "<!doctype html><body><a href=/b target=_blank>Blank link</a>
<button onclick=\"window.open('/b')\">Open window</button><p>first page</p></body>";

/// `target=_blank` and `window.open` open a new named session; the receipt names it, `sessions` lists it, `close` ends it.
#[test]
fn new_tabs_are_named_sessions() {
    let base = pages(&[("/", TABS), ("/b", "<!doctype html><body><p>second page</p></body>")]);
    let mut s = Server::start();
    s.tool("open", json!({"url": format!("{base}/")}));
    let (r, e) = s.tool("act", json!({"op": "click", "loc": "text=\"Blank link\"", "snapshot": false}));
    assert!(!e, "{r}");
    assert!(r.as_str().unwrap().contains(&format!("opened tab: main~2 {base}/b")), "{r}");
    assert!(!r.as_str().unwrap().contains("navigated"), "the opener stays: {r}");
    let (r, _) = s.tool("act", json!({"op": "click", "loc": "role=button name=\"Open window\"", "snapshot": false}));
    assert!(r.as_str().unwrap().contains(&format!("opened tab: main~3 {base}/b")), "{r}");
    let (r, _) = s.tool("sessions", json!({}));
    assert!(r.as_str().unwrap().contains("main~2:") && r.as_str().unwrap().contains("main~3:") && r.as_str().unwrap().contains("main: "), "{r}");
    let (r, e) = s.tool("act", json!({"op": "wait", "loc": "text=\"second page\"", "session": "main~2", "snapshot": false}));
    assert!(!e, "{r}");
    let (r, _) = s.tool("read", json!({"session": "main~2"}));
    assert!(r.as_str().unwrap().contains("second page"), "{r}");
    let (r, _) = s.tool("read", json!({}));
    assert!(r.as_str().unwrap().contains("first page"), "{r}");
    let (_, e) = s.tool("close", json!({"session": "main~2"}));
    assert!(!e);
    let (r, _) = s.tool("sessions", json!({}));
    assert!(!r.as_str().unwrap().contains("main~2:"), "{r}");
}

/// `storage` reads, writes and clears cookies, `localStorage` and `sessionStorage`; a secret value shows as `[secret]`.
#[test]
fn storage_gets_sets_and_clears_with_secrets_redacted() {
    let base = pages(&[("/", "<!doctype html><body>x</body>")]);
    let dir = std::env::temp_dir().join(format!("bare-mcp-test-storage-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let secrets = dir.join("secrets");
    std::fs::write(&secrets, "TOKEN=tok-9f8e7d6c\n").unwrap();
    let mut s = Server::start();
    s.tool("open", json!({"url": format!("{base}/"), "secrets_file": secrets.to_str().unwrap()}));
    let call = |s: &mut Server, a: Value| {
        let (r, e) = s.tool("storage", a);
        assert!(!e, "{r}");
        r.as_str().unwrap().to_string()
    };
    for area in ["local", "session", "cookies"] {
        call(&mut s, json!({"area": area, "action": "set", "key": "k", "value": "v1"}));
        call(&mut s, json!({"area": area, "action": "set", "key": "auth", "value": "tok-9f8e7d6c"}));
        let r = call(&mut s, json!({"area": area}));
        assert!(r.contains("k=v1") && r.contains("auth=[secret]") && !r.contains("tok-9f8e"), "{area}: {r}");
        let r = call(&mut s, json!({"area": area, "key": "k"}));
        assert!(r.lines().next().unwrap().starts_with("k=v1") && r.lines().count() == 1, "{area}: {r}");
        call(&mut s, json!({"area": area, "action": "clear", "key": "k"}));
        let r = call(&mut s, json!({"area": area}));
        assert!(!r.contains("k=v1") && r.contains("auth="), "{area}: {r}");
        call(&mut s, json!({"area": area, "action": "clear"}));
        assert!(!call(&mut s, json!({"area": area})).contains("auth="), "{area}");
    }
    let (r, e) = s.tool("storage", json!({"area": "disk"}));
    assert!(e && r.as_str().unwrap().contains("area must be"), "{r}");
}

/// `back` and `forward` walk the history of the session.
#[test]
fn back_and_forward_walk_the_history() {
    let base = pages(&[("/", "<!doctype html><body><a href=/b>Next</a><p>page one</p></body>"), ("/b", "<!doctype html><body><p>page two</p></body>")]);
    let mut s = Server::start();
    s.tool("open", json!({"url": format!("{base}/")}));
    let (r, e) = s.tool("act", json!({"op": "click", "loc": "text=\"Next\""}));
    assert!(!e, "{r}");
    let (r, e) = s.tool("act", json!({"op": "wait", "loc": "text=\"page two\""}));
    assert!(!e, "{r}");
    let (r, e) = s.tool("act", json!({"op": "back"}));
    assert!(!e, "{r}");
    let (r, e) = s.tool("act", json!({"op": "wait", "loc": "text=\"page one\""}));
    assert!(!e, "{r}");
    let (r, e) = s.tool("act", json!({"op": "forward"}));
    assert!(!e, "{r}");
    let (r, e) = s.tool("act", json!({"op": "wait", "loc": "text=\"page two\""}));
    assert!(!e, "{r}");
}

/// Serve one HTML page on 127.0.0.1 for every path. Gives the base URL.
fn html_app(page: &'static str) -> String {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let (mut head, mut b) = (Vec::new(), [0u8; 1]);
                while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
                    head.push(b[0]);
                }
                let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len());
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// Thirty named links, a wrapper with a click handler, a nameless link and a button that adds a new button.
const TOKENS_PAGE: &str = "<!doctype html><body><h1>Tokens</h1>
<div id=wrap onclick='1'><span>Wrapper text that repeats what its children say</span><a href='/w?utm_source=x&id=7'>Inside</a></div>
<a href='/blank'><img src='data:,' width=1 height=1></a>
<ul id=list></ul><div id=out><button id=add onclick=\"var b=document.createElement('button');b.textContent='Added';document.getElementById('out').append(b)\">Add</button></div>
<script>for(let i=0;i<30;i++){const li=document.createElement('li'),a=document.createElement('a');a.href='/p/'+i+'?utm_medium=m&keep='+i+'&fbclid=zz';a.textContent='Link '+i;li.append(a);document.getElementById('list').append(li)}</script></body>";

fn opened(s: &mut Server, base: &str) -> String {
    let (snap, error) = s.tool("open", json!({"url": format!("{base}/"), "snapshot": true, "timeout_ms": 30000}));
    assert!(!error, "{snap}");
    snap.as_str().unwrap().to_string()
}

#[test]
fn act_gives_only_what_changed_and_its_parents() {
    let mut s = Server::start();
    let snap = opened(&mut s, &html_app(TOKENS_PAGE));
    assert!(snap.contains("link \"Link 29\""), "{snap}");
    let (r, error) = s.tool("act", json!({"op": "click", "ref": ref_of(&snap, "button \"Add\"")}));
    let r = r.as_str().unwrap().to_string();
    assert!(!error && r.starts_with("ok"), "{r}");
    assert!(r.contains("*") && r.contains("button \"Added\""), "{r}");
    // The parent of the new button is there for context; the unchanged links are not.
    assert!(r.contains("button \"Add\""), "{r}");
    assert!(!r.contains("Link 5") && !r.contains("Tokens"), "{r}");
    assert!(r.len() * 3 < snap.len(), "{} chars of diff for {} of snapshot", r.len(), snap.len());
    // A ref of the full snapshot still works on the diff's page, and the new button has a ref of its own.
    let (r2, error) = s.tool("act", json!({"op": "click", "ref": ref_of(&r, "button \"Added\"")}));
    assert!(!error, "{r2}");
    // `diff: false` asks for the whole outline.
    let (r, _) = s.tool("act", json!({"op": "scroll", "diff": false}));
    assert!(r.as_str().unwrap().contains("link \"Link 29\""), "{r}");
    // Nothing changed: say so, in a few characters.
    let (r, _) = s.tool("act", json!({"op": "scroll"}));
    let r = r.as_str().unwrap();
    assert!(r.contains("no change") && r.len() < 80, "{r}");
}

#[test]
fn a_snapshot_marks_changed_lines_too() {
    let mut s = Server::start();
    let snap = opened(&mut s, &html_app(TOKENS_PAGE));
    s.tool("act", json!({"op": "click", "ref": ref_of(&snap, "button \"Add\""), "snapshot": false}));
    let (r, _) = s.tool("snapshot", json!({}));
    let r = r.as_str().unwrap();
    assert!(r.lines().any(|l| l.trim_start().starts_with("*[e") && l.contains("\"Added\"")), "{r}");
    assert!(r.lines().any(|l| l.trim_start().starts_with("*[e") && l.contains("\"Add\"") && l.contains("focused")), "{r}");
    assert!(!r.lines().any(|l| l.contains("\"Link 3\"") && l.contains('*')), "{r}");
}

#[test]
fn a_snapshot_keeps_each_control_without_the_href_the_links_tool_has() {
    let mut s = Server::start();
    let snap = opened(&mut s, &html_app(TOKENS_PAGE));
    for i in 0..30 {
        assert!(snap.contains(&format!("link \"Link {i}\"")), "link {i} missing: {snap}");
    }
    assert!(!snap.contains("utm_") && !snap.contains("/p/3"), "{snap}");
    // A link with no name shows where it goes, so it can be told apart.
    assert!(snap.lines().any(|l| l.contains("link") && l.contains("href=/blank")), "{snap}");
    // The wrapper keeps its ref but not a copy of the text of its children.
    let wrap = snap.lines().find(|l| l.contains("clickable")).unwrap();
    assert!(!wrap.contains("Wrapper text"), "{wrap}");
    assert!(snap.contains("link \"Inside\""), "{snap}");
}

#[test]
fn links_drop_tracking_params_and_the_origin_of_the_page() {
    let mut s = Server::start();
    let base = html_app(TOKENS_PAGE);
    opened(&mut s, &base);
    let (r, _) = s.tool("links", json!({}));
    let r = r.as_str().unwrap();
    assert!(r.contains("\"Link 3\" -> /p/3?keep=3"), "{r}");
    let lines = r.split_once("link(s)\n").unwrap().1;
    assert!(!lines.contains("utm_") && !lines.contains("fbclid") && !lines.contains(&base), "{r}");
    assert!(r.contains("\"Inside\" -> /w?id=7"), "{r}");
    assert!(r.contains("-> /blank"), "{r}");
}

/// The height of a PNG, from its header.
fn png_size(path: &str) -> (u32, u32) {
    let b = std::fs::read(path).unwrap();
    (u32::from_be_bytes(b[16..20].try_into().unwrap()), u32::from_be_bytes(b[20..24].try_into().unwrap()))
}

/// A page with a 3000 px tall element below the fold. Gives the base URL.
fn tall_page() -> String {
    use std::io::Read;
    const PAGE: &str = "<!doctype html><body style='margin:0'><div style='height:900px'>top</div>\
<main aria-label='tall' style='height:3000px;width:200px;background:#0a0'>tall</main></body>";
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let (mut head, mut b) = (Vec::new(), [0u8; 1]);
                while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
                    head.push(b[0]);
                }
                let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{PAGE}", PAGE.len());
            });
        }
    });
    format!("http://127.0.0.1:{port}/")
}

/// An element taller than the window gives a picture of its whole box, and `full_page` one of the whole page.
#[test]
fn a_shot_of_a_tall_element_is_not_cut_at_the_window() {
    let mut s = Server::start();
    let (rep, error) = s.tool("open", json!({"url": tall_page(), "timeout_ms": 30000, "report": true}));
    assert!(!error && rep["ok"] == true, "{rep}");
    let (snap, _) = s.tool("snapshot", json!({}));
    let line = snap.as_str().unwrap().lines().find(|l| l.contains("\"tall\"")).unwrap_or_else(|| panic!("{snap}"));
    let r = line.split(['[', ']']).nth(1).unwrap().to_string();
    let (said, error) = s.tool("screenshot", json!({"ref": r}));
    assert!(!error, "{said}");
    let path = said.as_str().unwrap().split(" (").next().unwrap().to_string();
    assert_eq!(png_size(&path), (200, 3000), "{said}");
    let (said, error) = s.tool("screenshot", json!({"full_page": true}));
    assert!(!error, "{said}");
    let path = said.as_str().unwrap().split(" (").next().unwrap().to_string();
    assert_eq!(png_size(&path), (1280, 3900), "{said}");
}

#[test]
fn a_snapshot_leaves_out_what_an_adopted_sheet_hides() {
    let mut s = Server::start();
    let (snap, error) = s.tool("open", json!({"url": format!("{}/adopted", common::serve()), "timeout_ms": 30000, "snapshot": true}));
    let snap = snap.as_str().unwrap().to_string();
    assert!(!error && snap.contains("Shown button"), "{snap}");
    assert!(!snap.contains("Hidden button"), "{snap}");
    let (r, error) = s.tool("eval", json!({"js": "var r = document.querySelector('x-card').shadowRoot; getComputedStyle(r.getElementById('seen')).color + ' ' + getComputedStyle(r.getElementById('gone')).display"}));
    assert!(!error && r.to_string().contains("rgb(0, 0, 255) none"), "{r}");
}

fn eval_pages(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    match target {
        "/report.csv" => (200, "text/csv", b"a,b\n1,2\n".to_vec()),
        _ => (200, "text/html", b"<!doctype html><title>eval</title><p>page</p>".to_vec()),
    }
}

#[test]
fn eval_waits_for_a_promise_and_runs_top_level_await() {
    let base = common::serve_route(eval_pages);
    let mut s = Server::start();
    let (r, error) = s.tool("open", json!({"url": format!("{base}/"), "timeout_ms": 1500}));
    assert!(!error, "{r}");
    // A promise: eval waits for it and gives the fulfilled value.
    assert_eq!(s.tool("eval", json!({"js": "fetch('/report.csv').then(r => r.text())"})), (json!("a,b\n1,2\n"), false));
    // Top-level await, as an expression and as statements with a return.
    assert_eq!(s.tool("eval", json!({"js": "await (await fetch('/report.csv')).text()"})), (json!("a,b\n1,2\n"), false));
    assert_eq!(s.tool("eval", json!({"js": "const t = await (await fetch('/report.csv')).text(); return t.length"})), (json!(8), false));
    // A rejected promise and a thrown error show the name and the message.
    let (r, error) = s.tool("eval", json!({"js": "Promise.reject(new TypeError('nope'))"}));
    assert!(error && r.as_str().unwrap().contains("rejected: TypeError: nope"), "{r}");
    let (r, error) = s.tool("eval", json!({"js": "throw new RangeError('bad')"}));
    assert!(error && r.as_str().unwrap().contains("RangeError: bad") && !r.as_str().unwrap().contains("ObjId"), "{r}");
    let (r, error) = s.tool("eval", json!({"js": "throw 'plain'"}));
    assert!(error && r.as_str().unwrap().contains("plain") && !r.as_str().unwrap().contains("ObjId"), "{r}");
    // A promise that never settles ends at the time limit.
    let (r, error) = s.tool("eval", json!({"js": "new Promise(() => {})"}));
    assert!(error && r.as_str().unwrap().contains("pending"), "{r}");
}

fn eval_json_pages(_: &str, _: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    (200, "text/html", b"<!doctype html><title>eval</title><div id=out class='a b'>x</div><p>one</p><p>two</p>".to_vec())
}

#[test]
fn eval_prints_values_as_compact_json() {
    let base = common::serve_route(eval_json_pages);
    let mut s = Server::start();
    let (r, error) = s.tool("open", json!({"url": format!("{base}/"), "timeout_ms": 1500}));
    assert!(!error, "{r}");
    let text = |s: &mut Server, js: &str| {
        let (r, error) = s.tool("eval", json!({"js": js}));
        assert!(!error, "{js}: {r}");
        match r {
            Value::String(t) => t,
            v => v.to_string(),
        }
    };
    // Arrays and plain objects are compact JSON; strings, numbers, booleans and null stay as before.
    assert_eq!(text(&mut s, "[typeof window, 1, \"a\"]"), r#"["object",1,"a"]"#);
    assert_eq!(text(&mut s, "({a: 1, b: [true, null], c: {d: 'x'}, u: undefined})"), r#"{"a":1,"b":[true,null],"c":{"d":"x"}}"#);
    assert_eq!(text(&mut s, "'plain text'"), "plain text");
    assert_eq!(text(&mut s, "undefined"), "undefined");
    assert_eq!(text(&mut s, "null"), "null");
    // Functions, DOM nodes, node lists, Map, Set and Date.
    assert_eq!(text(&mut s, "function hello(a) {}; hello"), "function hello()");
    assert_eq!(text(&mut s, "document.getElementById('out')"), "<div#out.a.b>");
    assert_eq!(text(&mut s, "document.querySelectorAll('p')"), r#"["<p>","<p>"]"#);
    assert_eq!(text(&mut s, "document.getElementsByTagName('p')"), r#"["<p>","<p>"]"#);
    assert_eq!(text(&mut s, "[...document.querySelectorAll('div,p')].slice(0, 2)"), r#"["<div#out.a.b>","<p>"]"#);
    assert_eq!(text(&mut s, "new Map([['k', 1], [2, [3]]])"), r#"[["k",1],[2,[3]]]"#);
    assert_eq!(text(&mut s, "new Set(['a', 'a', 'b'])"), r#"["a","b"]"#);
    assert_eq!(text(&mut s, "new Date(0)"), "1970-01-01T00:00:00.000Z");
    assert_eq!(text(&mut s, "({f: function g() {}, when: new Date(0)})"), r#"{"f":"function g()","when":"1970-01-01T00:00:00.000Z"}"#);
    // A cycle does not hang.
    assert_eq!(text(&mut s, "var o = {name: 'o'}; o.self = o; o"), r#"{"name":"o","self":"[Circular]"}"#);
    // A long value is cut, with its full length.
    let long = text(&mut s, "Array.from({length: 2000}, (_, i) => i)");
    assert!(long.starts_with("[0,1,2,") && long.ends_with("… (cut: 4000 of 8891 chars)"), "{}", &long[long.len() - 60..]);
    assert_eq!(long.chars().count(), 4000 + "… (cut: 4000 of 8891 chars)".chars().count());
}

const LATE: &str = "<!doctype html><body><p id=out>none</p>
<button onclick=\"setTimeout(function(){document.getElementById('out').textContent='confirm=' + confirm('Sure?')}, 50)\">Later</button>
<button onclick=\"setTimeout(function(){window.open('/b')}, 50)\">Tab later</button>
<button onclick=\"throw new Error('boom')\">Throw</button>
<button onclick=\"Promise.reject(new Error('nope'))\">Reject</button>
<button onclick=\"document.getElementById('out').textContent='quiet'\">Quiet</button></body>";

/// Lets the 50 ms timers of `LATE` fire while no call runs.
fn idle() {
    std::thread::sleep(std::time::Duration::from_millis(200));
}

/// A dialog that a timer opens after `act` returned shows in the next reply, once, with the answer it got (dismiss).
#[test]
fn a_late_dialog_shows_in_the_next_reply() {
    let base = pages(&[("/", LATE)]);
    let mut s = Server::start();
    s.tool("open", json!({"url": format!("{base}/")}));
    let (r, e) = s.tool("act", json!({"op": "click", "loc": "role=button name=\"Later\"", "snapshot": false}));
    assert!(!e && !r.as_str().unwrap().contains("dialog:"), "the timer has not fired yet: {r}");
    idle();
    let (r, _) = s.tool("snapshot", json!({}));
    assert!(r.as_str().unwrap().contains("dialog: confirm \"Sure?\" -> dismissed"), "{r}");
    let (r, _) = s.tool("read", json!({}));
    assert!(r.as_str().unwrap().contains("confirm=false") && !r.as_str().unwrap().contains("dialog:"), "told once: {r}");
}

/// A tab that a timer opens after `act` returned becomes a session, named in the next reply.
#[test]
fn a_late_tab_shows_in_the_next_reply() {
    let base = pages(&[("/", LATE), ("/b", "<!doctype html><body><p>second page</p></body>")]);
    let mut s = Server::start();
    s.tool("open", json!({"url": format!("{base}/")}));
    let (r, _) = s.tool("act", json!({"op": "click", "loc": "role=button name=\"Tab later\"", "snapshot": false}));
    assert!(!r.as_str().unwrap().contains("tab"), "{r}");
    idle();
    let (r, _) = s.tool("find", json!({"role": "button", "name": "Quiet"}));
    assert!(r.as_str().unwrap().contains(&format!("opened tab: main~2 {base}/b")), "{r}");
    let (r, _) = s.tool("read", json!({"session": "main~2"}));
    assert!(r.as_str().unwrap().contains("second page"), "{r}");
}

/// An uncaught error or rejection in an action shows as a short script error line; a quiet action gives no extra line.
#[test]
fn script_errors_show_in_the_act_and_open_replies() {
    let base = pages(&[("/", LATE), ("/bad", "<!doctype html><body><p>bad</p><script>null.f()</script></body>")]);
    let mut s = Server::start();
    s.tool("open", json!({"url": format!("{base}/")}));
    let (r, _) = s.tool("act", json!({"op": "click", "loc": "role=button name=\"Throw\""}));
    let r = r.as_str().unwrap();
    assert!(r.lines().any(|l| l.starts_with("script error: ") && l.contains("boom") && l.len() <= 230), "{r}");
    let (r, _) = s.tool("act", json!({"op": "click", "loc": "role=button name=\"Reject\""}));
    let r = r.as_str().unwrap();
    assert!(r.lines().any(|l| l.starts_with("script error: ") && l.contains("nope")), "{r}");
    assert_eq!(r.matches("nope").count(), 1, "one line for one error: {r}");
    let (r, _) = s.tool("act", json!({"op": "click", "loc": "role=button name=\"Quiet\""}));
    let r = r.as_str().unwrap();
    assert!(r.starts_with("ok (") && !r.contains("script error") && !r.contains("dialog") && !r.contains("tab"), "{r}");
    let (r, _) = s.tool("open", json!({"url": format!("{base}/bad"), "snapshot": true}));
    assert!(r.as_str().unwrap().lines().any(|l| l.starts_with("script error: ")), "{r}");
}

/// A styled `noscript` (crates.io sets `display: grid`) stays out of the snapshot and of `read`: scripts run.
#[test]
fn a_styled_noscript_is_not_in_the_snapshot_or_the_read() {
    let html = "<!doctype html><style>noscript{display:grid}</style><body><noscript>Enable JavaScript please</noscript><h1>Crate</h1><button>Copy</button>";
    let mut s = Server::start();
    let (snap, error) = s.tool("open", json!({"url": one_page(html.to_string()), "snapshot": true, "timeout_ms": 30000}));
    let snap = snap.as_str().unwrap().to_string();
    assert!(!error && snap.contains("Copy") && !snap.contains("Enable JavaScript"), "{snap}");
    let (r, error) = s.tool("read", json!({}));
    let r = r.to_string();
    assert!(!error && r.contains("Crate") && !r.contains("Enable JavaScript"), "{r}");
}

/// `focus({ focusVisible })` and an unknown option do not stop the page: the app goes on and renders after it.
#[test]
fn focus_with_focus_visible_does_not_stop_the_page() {
    let html = "<!doctype html><body><input id=i><script>\
        var i = document.getElementById('i'); i.focus({focusVisible: true}); i.focus({focusVisible: false, preventScroll: true, other: 1});\
        var b = document.createElement('button'); b.textContent = 'After focus'; document.body.appendChild(b);</script>";
    let snap = snap_of(html, json!({}));
    assert!(snap.contains("After focus") && !snap.contains("stopped"), "{snap}");
}

/// A button under a full-page cover. `script` decides when the cover goes.
fn covered_page(script: &str) -> String {
    one_page(format!(
        "<!doctype html><title>Cover</title><body>\
         <button id=go onclick=\"this.textContent='Clicked'\">Go</button>\
         <div id=cover class=\"cookie banner\" role=dialog aria-label=\"Cookie consent\" style=\"position:fixed;inset:0;background:rgba(0,0,0,.4)\"><p>We use cookies</p><button>Accept all</button></div>\
         <script>{script}</script></body>"
    ))
}

#[test]
fn a_click_waits_for_a_spinner_that_goes_away() {
    // A timer removes the spinner; a script fades a backdrop out frame by frame and then removes it. (bare has no CSS transitions, so a
    // fade by `transition` and `transitionend` never ends.)
    let fade = "var c = document.getElementById('cover'), t0 = performance.now(); \
                requestAnimationFrame(function f(t) { var o = 1 - (t - t0) / 300; if (o > 0) { c.style.opacity = o; requestAnimationFrame(f) } else c.remove() })";
    for script in ["setTimeout(() => document.getElementById('cover').remove(), 300)", fade] {
        let mut s = Server::start();
        let (snap, _) = s.tool("open", json!({"url": covered_page(script), "snapshot": true, "timeout_ms": 20000}));
        let go = ref_of(snap.as_str().unwrap(), "\"Go\"");
        let (r, error) = s.tool("act", json!({"op": "click", "ref": go}));
        let r = r.as_str().unwrap().to_string();
        assert!(!error && r.contains("Clicked"), "{script}: {r}");
    }
}

/// A cover that stays fails within the cover wait (not the step time limit), also on a page whose timers never stop, and names the cover
/// (not the text under the pointer) by its role and name and by its tag, id and classes.
#[test]
fn a_click_under_a_cookie_banner_fails_fast_and_names_the_banner() {
    for script in ["", "setInterval(() => {}, 50)"] {
        let mut s = Server::start();
        let (snap, _) = s.tool("open", json!({"url": covered_page(script), "snapshot": true, "timeout_ms": 20000}));
        let go = ref_of(snap.as_str().unwrap(), "\"Go\"");
        let t = std::time::Instant::now();
        let (r, error) = s.tool("act", json!({"op": "click", "ref": go}));
        let ms = t.elapsed().as_millis();
        let r = r.as_str().unwrap().to_string();
        assert!(error && r.contains("FAILED covered"), "{r}");
        assert!(r.contains("div#cover.cookie.banner") && r.contains("dialog \"Cookie consent\""), "{r}");
        assert!(ms < 1500, "{script:?}: {ms} ms: {r}");
    }
}

const WINDOW_PAGE: &str = "<!doctype html><title>Window</title><body><h2 id=out>none</h2>
<button onclick=\"document.getElementById('out').textContent='changed'\">Change</button>
<button onclick=\"document.getElementById('out').textContent='confirm=' + confirm('Delete item?')\">Confirm</button>
<a href=/b target=_blank>Blank link</a></body>";

/// The agent tools on a page in a real window (`window: true`): the page runs on the window thread, so each tool goes through
/// `Remote::call`. Run it on a display of its own (a nested compositor), not on a desktop in use.
#[test]
#[ignore = "needs a Wayland display: it opens real windows"]
fn the_agent_tools_work_in_a_window() {
    agent_tools(true);
}

/// The checks of `the_agent_tools_work_in_a_window`, headless.
#[test]
fn the_agent_tools_work_headless() {
    agent_tools(false);
}

fn agent_tools(window: bool) {
    let base = pages(&[("/", WINDOW_PAGE), ("/b", "<!doctype html><body><p>second page</p></body>")]);
    let mut s = Server::start();
    let (snap, e) = s.tool("open", json!({"url": format!("{base}/"), "window": window, "snapshot": true, "timeout_ms": 20000}));
    let snap = snap.as_str().unwrap().to_string();
    assert!(!e && snap.contains("\"Change\""), "{snap}");
    // act gives the diff: only the changed line, marked *.
    let (r, e) = s.tool("act", json!({"op": "click", "ref": ref_of(&snap, "\"Change\"")}));
    let r = r.as_str().unwrap().to_string();
    assert!(!e && r.lines().any(|l| l.contains('*') && l.contains("changed")) && !r.contains("\"Confirm\""), "{r}");
    let (r, e) = s.tool("find", json!({"text": "Confirm"}));
    assert!(!e && r.as_str().unwrap().contains("Confirm"), "{r}");
    let (r, e) = s.tool("read", json!({}));
    assert!(!e && r.as_str().unwrap().contains("changed"), "{r}");
    let (r, e) = s.tool("screenshot", json!({}));
    let shot = r.as_str().unwrap();
    assert!(!e && shot.contains(".png ("), "{shot}");
    let path = shot.split(" (").next().unwrap();
    assert!(std::fs::metadata(path).unwrap().len() > 100, "{path}");
    // Dialogs: the policy is per thread, so it must be set on the window thread.
    let (r, e) = s.tool("act", json!({"op": "click", "ref": ref_of(&snap, "\"Confirm\""), "dialog": "dismiss"}));
    let r = r.as_str().unwrap().to_string();
    assert!(!e && r.contains("dialog: confirm \"Delete item?\" -> dismissed") && r.contains("confirm=false"), "{r}");
    // A tab is a new session in a window of its own.
    let (r, e) = s.tool("act", json!({"op": "click", "ref": ref_of(&snap, "Blank link"), "snapshot": false}));
    assert!(!e && r.as_str().unwrap().contains(&format!("opened tab: main~2 {base}/b")), "{r}");
    let (r, e) = s.tool("act", json!({"op": "wait", "loc": "text=\"second page\"", "session": "main~2", "snapshot": false}));
    assert!(!e, "{r}");
    let (r, e) = s.tool("close", json!({"session": "main~2"}));
    assert!(!e, "{r}");
    // Downloads.
    let dir = std::env::temp_dir().join(format!("bare-mcp-test-window-downloads-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (snap, e) = s.tool("open", json!({"url": format!("{}/downloads", common::serve()), "session": "dl", "window": window, "snapshot": true, "downloads_dir": dir.to_str().unwrap(), "timeout_ms": 20000}));
    let snap = snap.as_str().unwrap().to_string();
    assert!(!e, "{snap}");
    let (r, e) = s.tool("act", json!({"op": "click", "ref": ref_of(&snap, "\"Report\""), "session": "dl", "snapshot": false}));
    assert!(!e && r.as_str().unwrap().contains("report.csv ("), "{r}");
    assert_eq!(std::fs::read(dir.join("report.csv")).unwrap(), common::PAYLOAD_CSV);
    // A spinner that a timer removes, and a banner that stays.
    let (snap, _) = s.tool("open", json!({"url": covered_page("setTimeout(() => document.getElementById('cover').remove(), 300)"), "session": "cover", "window": window, "snapshot": true, "timeout_ms": 20000}));
    let (r, e) = s.tool("act", json!({"op": "click", "ref": ref_of(snap.as_str().unwrap(), "\"Go\""), "session": "cover"}));
    assert!(!e && r.as_str().unwrap().contains("Clicked"), "{r}");
    let (snap, _) = s.tool("open", json!({"url": covered_page("setInterval(() => {}, 50)"), "session": "cover", "window": window, "snapshot": true, "timeout_ms": 20000}));
    let t = std::time::Instant::now();
    let (r, e) = s.tool("act", json!({"op": "click", "ref": ref_of(snap.as_str().unwrap(), "\"Go\""), "session": "cover"}));
    let r = r.as_str().unwrap().to_string();
    assert!(e && r.contains("dialog \"Cookie consent\"") && t.elapsed().as_millis() < 1500, "{} ms: {r}", t.elapsed().as_millis());
}

/// The pages of a site that navigates in four ways: a plain link to a slow page, Turbo-like links that fetch the next page and replace
/// the body (the address changes before or after the fetch), and a button that changes the address, then fetches and renders. The
/// fetch takes longer than the 2 s that a read waits for a reply, as GitHub's does.
fn nav_pages(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    let path = target.split('?').next().unwrap_or("/");
    let wait = |ms| std::thread::sleep(std::time::Duration::from_millis(ms));
    let html = |s: &str| (200, "text/html", s.as_bytes().to_vec());
    match path {
        "/" => html("<!doctype html><title>Home</title><body><h1>Home page</h1>
<a href=/plain>Plain</a> <a href=/turbo id=t>Turbo</a> <a href=/late id=l>Late</a> <button id=r>Route</button>
<a href=/styled>Styled</a> <a href=/stuck>Stuck</a>
<script>
function load(after) { return fetch('/frag').then(r => r.text()).then(t => { after(); document.body.innerHTML = t; }); }
document.getElementById('t').onclick = e => { e.preventDefault(); history.pushState({}, '', '/turbo'); load(() => {}); };
document.getElementById('l').onclick = e => { e.preventDefault(); load(() => history.pushState({}, '', '/late')); };
document.getElementById('r').onclick = () => { history.pushState({}, '', '/route'); fetch('/frag').then(r => r.text()).then(() => { document.body.innerHTML = '<h1>Route page</h1>'; }); };
</script></body>"),
        "/frag" => {
            wait(2300);
            html("<h1>Issues page</h1><a href=/>Home</a>")
        }
        "/plain" => {
            wait(200);
            html("<!doctype html><title>Plain</title><body><h1>Plain page</h1><a href=/>Home</a></body>")
        }
        "/styled" => html("<!doctype html><title>Styled</title><link rel=stylesheet href=/slow.css?ms=800><body><h1>Styled page</h1><a href=/plain>Plain</a></body>"),
        "/stuck" => html("<!doctype html><title>Stuck</title><link rel=stylesheet href=/slow.css?ms=13000><body><h1>Stuck page</h1></body>"),
        "/slow.css" => {
            wait(target.split("ms=").nth(1).and_then(|m| m.parse().ok()).unwrap_or(800));
            (200, "text/css", b"h1 { color: red }".to_vec())
        }
        _ => (404, "text/plain", b"no".to_vec()),
    }
}

/// Each act that starts a navigation names the new URL, and the outline after it is of the new page, with or without a snapshot.
#[test]
fn an_act_that_navigates_says_where_and_shows_the_new_page() {
    let base = common::serve_route(nav_pages);
    let mut s = Server::start();
    for (what, path, heading) in [("link name=\"Plain\"", "/plain", "Plain page"), ("link name=\"Turbo\"", "/turbo", "Issues page"), ("link name=\"Late\"", "/late", "Issues page"), ("button name=\"Route\"", "/route", "Route page")] {
        for snapshot in [true, false] {
            let (r, e) = s.tool("open", json!({"url": format!("{base}/"), "snapshot": true}));
            assert!(!e, "{r}");
            let (r, e) = s.tool("act", json!({"op": "click", "loc": format!("role={what}"), "snapshot": snapshot}));
            let r = r.as_str().unwrap().to_string();
            assert!(!e && r.contains(&format!("navigated: {base}{path}")), "{what} snapshot={snapshot}: {r}");
            assert!(!r.contains("not ready"), "{what} snapshot={snapshot}: {r}");
            if snapshot {
                assert!(r.contains(heading) && !r.contains("Home page"), "{what}: the outline is not of the new page: {r}");
            }
            let (r, _) = s.tool("eval", json!({"js": "location.pathname + ' ' + document.querySelector('h1').textContent"}));
            assert!(r.as_str().unwrap().contains(&format!("{path} {heading}")), "{what} snapshot={snapshot}: {r}");
        }
    }
}

/// `back` and `forward` over two documents name the URL they reach, and show that page.
#[test]
fn back_and_forward_say_where_they_went() {
    let base = common::serve_route(nav_pages);
    let mut s = Server::start();
    for snapshot in [true, false] {
        s.tool("open", json!({"url": format!("{base}/"), "snapshot": true}));
        let (r, e) = s.tool("act", json!({"op": "click", "loc": "role=link name=\"Plain\"", "snapshot": snapshot}));
        assert!(!e && r.as_str().unwrap().contains(&format!("navigated: {base}/plain")), "{r}");
        let (r, e) = s.tool("act", json!({"op": "back", "snapshot": snapshot}));
        let r = r.as_str().unwrap().to_string();
        assert!(!e && r.contains(&format!("navigated: {base}/")) && !r.contains("/plain"), "back snapshot={snapshot}: {r}");
        if snapshot {
            assert!(r.contains("Home page"), "{r}");
        }
        let (r, e) = s.tool("act", json!({"op": "forward", "snapshot": snapshot}));
        let r = r.as_str().unwrap().to_string();
        assert!(!e && r.contains(&format!("navigated: {base}/plain")), "forward snapshot={snapshot}: {r}");
        if snapshot {
            assert!(r.contains("Plain page") && !r.contains("Home page"), "{r}");
        }
    }
}

/// The snapshot after a navigation waits for the first layout (a slow style sheet). A layout that does not come in time does not make
/// the act a failure: the click ran, and the receipt says so.
#[test]
fn an_act_waits_for_the_first_layout() {
    let base = common::serve_route(nav_pages);
    let mut s = Server::start();
    let (r, e) = s.tool("open", json!({"url": format!("{base}/"), "snapshot": true}));
    assert!(!e, "{r}");
    let (r, e) = s.tool("act", json!({"op": "click", "loc": "role=link name=\"Styled\""}));
    let r = r.as_str().unwrap().to_string();
    assert!(!e && r.contains("Styled page"), "{r}");
    let (r, e) = s.tool("open", json!({"url": format!("{base}/"), "snapshot": true, "timeout_ms": 1000}));
    assert!(!e, "{r}");
    let (r, e) = s.tool("act", json!({"op": "click", "loc": "role=link name=\"Stuck\""}));
    let r = r.as_str().unwrap().to_string();
    assert!(!e && r.contains(&format!("navigated: {base}/stuck")) && r.contains("no snapshot: ") && r.contains("no layout yet"), "{r}");
}

/// A tab that the last action opened is a session in `sessions`, also when no reply has named it yet (a `step` call).
#[test]
fn sessions_lists_a_tab_that_the_last_action_opened() {
    let base = pages(&[("/", TABS), ("/b", "<!doctype html><body><p>second page</p></body>")]);
    let mut s = Server::start();
    s.tool("open", json!({"url": format!("{base}/")}));
    let (r, e) = s.tool("step", json!({"steps": "click text=\"Blank link\"", "compact": true}));
    assert!(!e, "{r}");
    let (r, _) = s.tool("sessions", json!({}));
    assert!(r.as_str().unwrap().contains(&format!("main~2: {base}/b")), "{r}");
}

#[test]
fn find_with_css_and_text_keeps_the_matches_inside_the_css_scope() {
    let base = naming_app();
    let mut s = Server::start();
    s.tool("open", json!({"url": format!("{base}/scope"), "snapshot": false}));
    let (r, error) = s.tool("find", json!({"css": "table.info th", "text": "Developer"}));
    let r = r.as_str().unwrap();
    assert!(!error && r.starts_with("1 match"), "{r}");
    assert!(r.contains("\"Developer\"") && !r.contains("Ann") && !r.contains("Developers"), "{r}");
}

#[test]
fn an_unknown_argument_names_the_closest_valid_key() {
    let base = naming_app();
    let mut s = Server::start();
    s.tool("open", json!({"url": format!("{base}/near"), "snapshot": false}));
    let (r, error) = s.tool("find", json!({"role": "link", "name~": "Issues"}));
    assert!(error, "{r}");
    assert_eq!(r.as_str().unwrap(), "unknown argument \"name~\": did you mean \"name\"?");
    // A key that one op does not use: a prompt answer is prompt_text, not text.
    let (r, error) = s.tool("act", json!({"op": "click", "loc": "text=\"Pull requests\"", "dialog": "accept", "text": "yes"}));
    assert!(error, "{r}");
    assert!(r.as_str().unwrap().contains("did you mean \"prompt_text\"?"), "{r}");
    // A tool without a session checks its keys too.
    let (r, error) = s.tool("sessions", json!({"verbose": true}));
    assert!(error && r.as_str().unwrap().starts_with("unknown argument \"verbose\""), "{r}");
}

#[test]
fn an_exact_name_with_near_names_fails_at_once_and_lists_them() {
    let base = naming_app();
    let mut s = Server::start();
    s.tool("open", json!({"url": format!("{base}/near"), "snapshot": false, "timeout_ms": 20000}));
    let t = std::time::Instant::now();
    let (r, error) = s.tool("act", json!({"op": "click", "loc": "role=link name=\"Issues\"", "snapshot": false}));
    let ms = t.elapsed().as_millis();
    let r = r.as_str().unwrap();
    assert!(error && r.starts_with("FAILED not_found"), "{r}");
    assert!(ms < 2000, "took {ms} ms: {r}");
    assert!(r.contains("\"Issues (5k+)\"") && r.contains("\"Issues closed\"") && !r.contains("Pull requests"), "{r}");
}

#[test]
fn a_miss_on_an_idle_page_speaks_of_the_locator() {
    let base = naming_app();
    let mut s = Server::start();
    s.tool("open", json!({"url": format!("{base}/todo"), "snapshot": false}));
    let (r, error) = s.tool("act", json!({"op": "click", "loc": "role=checkbox name~\"Walk the dog\"", "snapshot": false}));
    let r = r.as_str().unwrap();
    assert!(error && r.starts_with("FAILED not_found"), "{r}");
    assert!(!r.contains("nothing can change it"), "{r}");
    assert!(r.contains("2 element(s) with role=checkbox"), "the miss says what has the role: {r}");
    // An exact name with no near name says the same.
    let (r, _) = s.tool("act", json!({"op": "click", "loc": "role=checkbox name=\"Toggle every\"", "snapshot": false}));
    assert!(r.as_str().unwrap().contains("2 element(s) with role=checkbox, none with that name"), "{r}");
}

#[test]
fn refs_from_find_work_without_a_snapshot() {
    let base = naming_app();
    let mut s = Server::start();
    s.tool("open", json!({"url": format!("{base}/scope"), "snapshot": false}));
    // A ref before any snapshot or find: the error says so, and not that the page navigated.
    let (r, error) = s.tool("act", json!({"op": "click", "ref": "e3", "snapshot": false}));
    assert!(error && r.as_str().unwrap().contains("no snapshot or find gave refs on this page"), "{r}");
    let (r, _) = s.tool("find", json!({"role": "link", "name": "Developers"}));
    let r = r.as_str().unwrap().to_string();
    let (a, error) = s.tool("act", json!({"op": "click", "ref": ref_of(r.split_once('\n').unwrap().1, "Developers"), "snapshot": false}));
    assert!(!error, "{a}");
}

#[test]
fn find_with_no_match_on_an_idle_page_returns_at_once() {
    let base = naming_app();
    let mut s = Server::start();
    s.tool("open", json!({"url": format!("{base}/near"), "snapshot": false}));
    s.tool("find", json!({"css": "a"}));
    let t = std::time::Instant::now();
    let (r, _) = s.tool("find", json!({"css": "table.none"}));
    let ms = t.elapsed().as_millis();
    assert!(r.as_str().unwrap().starts_with("0 match") && ms < 100, "took {ms} ms: {r}");
}

/// A paragraph that a click changes, and a button that changes 20 long paragraphs.
const TEXT_CHANGES: &str = "<!doctype html><body><h1>Text</h1><p id=out>You clicked: Cancel</p>
<button onclick=\"document.getElementById('out').textContent = confirm('Sure?') ? 'You clicked: Ok' : 'You clicked: Cancel'\">Confirm</button>
<button onclick=\"for (const p of document.querySelectorAll('.many')) p.textContent = 'changed ' + p.id + ' ' + 'x'.repeat(400)\">Many</button>
<script>for (let i = 0; i < 20; i++) { const p = document.createElement('p'); p.className = 'many'; p.id = 'p' + i; p.textContent = 'old ' + i; document.body.append(p) }</script></body>";

/// #398: a text change that the user sees is in the act diff, in a few short lines.
#[test]
fn act_shows_text_that_changed() {
    let mut s = Server::start();
    let snap = opened(&mut s, &pages(&[("/", TEXT_CHANGES)]));
    assert!(!snap.contains("You clicked"), "interactive snapshot has no plain text: {snap}");
    let (r, error) = s.tool("act", json!({"op": "click", "loc": "role=button name=\"Confirm\"", "dialog": "accept"}));
    let r = r.as_str().unwrap().to_string();
    assert!(!error && r.contains("text: \"You clicked: Ok\""), "{r}");
    assert!(!r.contains("no change") && !r.contains("Cancel\""), "{r}");
    // The same click again changes nothing.
    let (r, _) = s.tool("act", json!({"op": "click", "loc": "role=button name=\"Confirm\"", "dialog": "accept"}));
    assert!(r.as_str().unwrap().contains("no change"), "{r}");
    // Many changed lines: 5 at most, each cut at 200 characters, and a line that says how many more.
    let (r, _) = s.tool("act", json!({"op": "click", "loc": "role=button name=\"Many\""}));
    let r = r.as_str().unwrap().to_string();
    let text: Vec<&str> = r.lines().filter(|l| l.starts_with("text: ")).collect();
    assert_eq!(text.len(), 5, "{r}");
    assert!(text.iter().all(|l| l.chars().count() <= 212), "{r}");
    assert!(r.contains("15 more changed text lines"), "{r}");
    assert!(r.len() < 1500, "{} chars: {r}", r.len());
}

/// #398: `open` gives a short receipt by default, and the JSON step report only with `report: true`.
#[test]
fn open_gives_a_short_receipt_unless_the_report_is_asked() {
    let base = pages(&[("/", TEXT_CHANGES)]);
    let mut s = Server::start();
    let (r, error) = s.tool("open", json!({"url": format!("{base}/")}));
    let r = r.as_str().unwrap_or_else(|| panic!("not text: {r}")).to_string();
    assert!(!error && r.starts_with("ok (") && r.len() < 100, "{r}");
    let (r, error) = s.tool("open", json!({"url": format!("{base}/"), "report": true}));
    assert!(!error && r["ok"] == true && r["steps"].is_array(), "{r}");
    let (r, error) = s.tool("open", json!({"url": format!("{base}/missing")}));
    assert!(error && r.as_str().unwrap().starts_with("FAILED"), "{r}");
}

/// #398: `read` and `links` stay within `max_chars`, header included.
#[test]
fn read_and_links_stay_within_max_chars() {
    let rows: String = (0..200).map(|i| format!("<p>Paragraph {i} with some words in it.</p><a href='/page/{i}'>link number {i}</a>")).collect();
    let base = page_server(format!("<!doctype html><title>A long title for the page</title><body>{rows}"));
    let mut s = Server::start();
    s.tool("open", json!({"url": format!("{base}/")}));
    for max in [300, 1500] {
        let (r, error) = s.tool("read", json!({"max_chars": max}));
        let r = r.as_str().unwrap();
        assert!(!error && r.chars().count() <= max && r.contains("read again with offset="), "{} chars: {r}", r.chars().count());
        let (r, error) = s.tool("links", json!({"max_chars": max}));
        let r = r.as_str().unwrap();
        assert!(!error && r.chars().count() <= max && r.contains("lines cut"), "{} chars: {r}", r.chars().count());
    }
    // The next page of read starts where the cut line says.
    let (r, _) = s.tool("read", json!({"max_chars": 300}));
    let next: usize = r.as_str().unwrap().split("offset=").nth(1).unwrap().trim().parse().unwrap();
    let (r, _) = s.tool("read", json!({"max_chars": 300, "offset": next}));
    assert!(r.as_str().unwrap().contains(&format!("chars {next}-")), "{r}");
}

/// A form whose submit handler writes through `id` globals (bare#392).
const NAMED_PAGE: &str = "<!doctype html><body><form id=f name=login onsubmit=\"event.preventDefault(); out.textContent = 'sent ' + q.value\">
<input id=q aria-label=Query value=hi><button>Go</button></form><div id=out>none</div></body>";

#[test]
fn id_globals_and_document_names_resolve_in_handlers_and_eval() {
    let mut s = Server::start();
    let snap = opened(&mut s, &html_app(NAMED_PAGE));
    let (r, error) = s.tool("eval", json!({"js": "JSON.stringify([typeof window.out, typeof f, 'out' in window, typeof document.login])"}));
    assert!(!error && r == json!(["object", "object", true, "object"]), "{r}");
    let (r, error) = s.tool("act", json!({"op": "click", "ref": ref_of(&snap, "button \"Go\"")}));
    assert!(!error, "{r}");
    let (r, error) = s.tool("eval", json!({"js": "out.textContent"}));
    assert!(!error && r == json!("sent hi"), "{r}");
}

/// Pages for the pending tests (bare #406): a fetch and an XHR that answer late, and a picture that never comes. Each page has text, so
/// only the request can hold `open`.
fn pending_pages(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    let path = target.split('?').next().unwrap_or("/");
    let page = |inner: &str| (200, "text/html", format!("<!doctype html><title>Pending</title><body><p>Hello</p>{inner}</body>").into_bytes());
    match path {
        "/fetch" => page("<script>fetch('/api?delay=8000')</script>"),
        "/xhr" => page("<script>var x = new XMLHttpRequest(); x.open('GET', '/api?delay=5000'); x.send()</script>"),
        "/img" => page("<img src='/hang.png' alt='late'>"),
        "/api" | "/hang.png" => {
            let ms = target.split("delay=").nth(1).and_then(|d| d.parse().ok()).unwrap_or(20000);
            std::thread::sleep(std::time::Duration::from_millis(ms));
            (200, "text/plain", b"late".to_vec())
        }
        _ => (404, "text/plain", b"no".to_vec()),
    }
}

#[test]
fn not_ready_names_the_pending_request_and_network_and_diagnose_list_it() {
    let base = common::serve_route(pending_pages);
    let mut s = Server::start();
    let t = std::time::Instant::now();
    let (rep, _) = s.tool("open", json!({"url": format!("{base}/fetch"), "timeout_ms": 600}));
    let r = rep.as_str().unwrap();
    assert!(r.contains(&format!("not ready: still loading after")) && r.contains(&format!("1 request pending: fetch GET {base}/api?delay=8000 (")), "{r}");
    let (net, _) = s.tool("network", json!({}));
    let net = net.as_str().unwrap().to_string();
    assert!(net.contains(&format!("pending fetch GET {base}/api?delay=8000")), "{net}");
    // A pending request stays new: the next call shows it again.
    let (again, _) = s.tool("network", json!({}));
    assert!(again.as_str().unwrap().contains("pending fetch GET"), "{again}");
    let (d, _) = s.tool("diagnose", json!({}));
    assert!(!d.as_str().unwrap().contains("pending"), "under 5 s is no finding: {d}");
    std::thread::sleep(std::time::Duration::from_millis(5500u64.saturating_sub(t.elapsed().as_millis() as u64)));
    let (d, _) = s.tool("diagnose", json!({}));
    assert!(d.as_str().unwrap().contains(&format!("pending fetch GET {base}/api?delay=8000 (")), "{d}");
    // Once it ends, it shows once more, with its status, and no longer as pending.
    std::thread::sleep(std::time::Duration::from_millis(9000u64.saturating_sub(t.elapsed().as_millis() as u64)));
    let (net, _) = s.tool("network", json!({"failed_only": false}));
    let net = net.as_str().unwrap().to_string();
    assert!(net.contains("200 fetch") && !net.contains("pending"), "{net}");
}

#[test]
fn not_ready_names_a_pending_xhr() {
    let base = common::serve_route(pending_pages);
    let mut s = Server::start();
    let (rep, _) = s.tool("open", json!({"url": format!("{base}/xhr"), "timeout_ms": 600}));
    let r = rep.as_str().unwrap();
    assert!(r.contains(&format!("1 request pending: xhr GET {base}/api?delay=5000 (")), "{r}");
}

#[test]
fn a_picture_that_never_comes_does_not_hold_open_and_network_lists_it() {
    let base = common::serve_route(pending_pages);
    let mut s = Server::start();
    let t = std::time::Instant::now();
    let (rep, _) = s.tool("open", json!({"url": format!("{base}/img"), "timeout_ms": 3000}));
    assert!(!rep.as_str().unwrap().contains("not ready"), "{rep}");
    assert!(t.elapsed().as_millis() < 2000, "open waited for the picture: {:?}", t.elapsed());
    let (net, _) = s.tool("network", json!({}));
    assert!(net.as_str().unwrap().contains(&format!("pending image GET {base}/hang.png")), "{net}");
}

const FILTER_PAGE: &str = "<!doctype html><body><nav aria-label=Top><a href='/home'>Home</a><a href='https://example.org/docs'>Docs</a></nav>
<main><a href='https://github.com/tokio-rs/tokio'>Repository</a><a href='/about'>About GitHub</a><a href='https://example.org/blog'>Blog</a></main></body>";

#[test]
fn links_filter_by_text_root_and_origin() {
    let mut s = Server::start();
    let base = html_app(FILTER_PAGE);
    let snap = opened(&mut s, &base);
    let count = |r: &Value| r.as_str().unwrap().lines().filter(|l| l.starts_with("[e")).count();
    // `text` matches the text or the URL, in any case.
    let (r, error) = s.tool("links", json!({"text": "GITHUB.com/tokio"}));
    assert!(!error && count(&r) == 1 && r.as_str().unwrap().contains("\"Repository\" -> https://github.com/tokio-rs/tokio"), "{r}");
    let (r, _) = s.tool("links", json!({"text": "github"}));
    assert!(count(&r) == 2 && r.as_str().unwrap().contains(&format!("\"About GitHub\" -> {base}/about")), "{r}");
    // `root` keeps the links inside an element, or the link itself.
    let nav = ref_of(&snap, "navigation \"Top\"");
    let (r, error) = s.tool("links", json!({"root": nav}));
    assert!(!error && count(&r) == 2 && r.as_str().unwrap().contains("\"Home\""), "{r}");
    let blog = ref_of(&snap, "link \"Blog\"");
    let (r, _) = s.tool("links", json!({"root": blog}));
    assert!(count(&r) == 1 && r.as_str().unwrap().contains("\"Blog\""), "{r}");
    // `external` keeps the links to another origin; the filters combine.
    let (r, _) = s.tool("links", json!({"external": true}));
    assert_eq!(count(&r), 3, "{r}");
    let main = ref_of(&snap, "main");
    let (r, _) = s.tool("links", json!({"external": true, "root": main, "text": "example"}));
    assert!(count(&r) == 1 && r.as_str().unwrap().contains("\"Blog\""), "{r}");
    // No match: one short line says so.
    let (r, error) = s.tool("links", json!({"text": "nothing here"}));
    assert!(!error && r.as_str().unwrap().ends_with("no link matches the filters"), "{r}");
    // A ref of an older page fails as in the other tools.
    s.tool("act", json!({"op": "goto", "url": format!("{base}/again"), "snapshot": false}));
    let (r, error) = s.tool("links", json!({"root": nav}));
    assert!(error && r.as_str().unwrap().contains("older page"), "{r}");
}

/// A list of issues as GitHub renders it: each title is a link inside a heading.
const HEADING_LINKS_PAGE: &str = "<!doctype html><body><ul>\
<li><h3><a href='/issues/101' tabindex='-1'><span><span>First issue</span></span></a></h3><span>opened today</span></li>\
<li><h3><a href='/issues/102' tabindex='-1'><span><span>Second <code>issue</code></span></span></a></h3></li>\
<li><h3>Plain heading</h3></li>\
<li><h2><a href='#install'><svg></svg></a>Install</h2></li></ul></body>";

#[test]
fn a_link_inside_a_heading_shows_in_snapshot_and_links() {
    let mut s = Server::start();
    let snap = opened(&mut s, &html_app(HEADING_LINKS_PAGE));
    for t in ["First issue", "Second issue"] {
        assert!(snap.lines().any(|l| l.contains(&format!("link \"{t}\"")) && l.contains("[e")), "{t}: {snap}");
    }
    assert!(snap.contains("heading \"Plain heading\""), "{snap}");
    // An anchor with no name (a `#` icon of a docs page) keeps the heading a leaf.
    assert!(snap.contains("heading \"Install\"") && !snap.contains("href=#install"), "{snap}");
    let (r, _) = s.tool("links", json!({"text": "/issues/"}));
    let r = r.as_str().unwrap();
    assert!(r.contains("\"First issue\" -> http") && r.contains("/issues/101\n") && r.contains("\"Second issue\" -> http") && r.contains("/issues/102"), "{r}");
}

/// A top navigation, a paragraph link, a footer link, and a header in an article (not a banner).
const LANDMARK_LINKS_PAGE: &str = "<!doctype html><body><nav aria-label=Top><a href='/docs/release-notes.html'>Release notes</a></nav>\
<p><a href='/about'>Plain link</a></p><footer><a href='/legal'>Legal</a></footer>\
<article><header><a href='/inside'>Inside article</a></header></article></body>";

#[test]
fn links_name_the_nearest_landmark_of_each_link() {
    let mut s = Server::start();
    opened(&mut s, &html_app(LANDMARK_LINKS_PAGE));
    let (r, error) = s.tool("links", json!({}));
    let r = r.as_str().unwrap();
    assert!(!error, "{r}");
    let line = |t: &str| r.lines().find(|l| l.contains(t)).unwrap_or_else(|| panic!("no line for {t}: {r}")).to_string();
    assert!(line("Release notes").ends_with("-> /docs/release-notes.html (navigation \"Top\")"), "{r}");
    assert!(line("Legal").ends_with("-> /legal (contentinfo)"), "{r}");
    assert!(line("Plain link").ends_with("-> /about") && !line("Plain link").contains('('), "{r}");
    assert!(line("Inside article").ends_with("-> /inside"), "{r}");
}

/// A plain page with no timer and a text file to fetch.
fn quiet_page(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    match target {
        "/data.txt" => (200, "text/plain", b"fixture text".to_vec()),
        _ => (200, "text/html", b"<!DOCTYPE html><title>Quiet</title><p>quiet</p>".to_vec()),
    }
}

#[test]
fn eval_runs_the_microtasks_of_its_script_at_once() {
    let base = common::serve_route(quiet_page);
    let mut s = Server::start();
    let (r, error) = s.tool("open", json!({"url": format!("{base}/")}));
    assert!(!error, "{r}");
    // Each case gives its value; a promise that only microtasks settle comes back in far less than the time limit.
    for (js, want, quick) in [
        ("Promise.resolve(1).then(x => x + 1)", json!(2), true),
        ("(async () => { await null; return 5 })()", json!(5), true),
        ("Promise.resolve(1).then(x => x + 1).then(x => x * 10).then(x => 'n' + x)", json!("n20"), true),
        ("new Promise(r => setTimeout(() => r(7), 50))", json!(7), true),
        ("(async () => { const r = await fetch('/data.txt'); return await r.text() })()", json!("fixture text"), false),
    ] {
        let t = std::time::Instant::now();
        let (r, error) = s.tool("eval", json!({"js": js}));
        let ms = t.elapsed().as_millis();
        assert!(!error && r == want, "{js}: {r}");
        assert!(!quick || ms < 1000, "{js}: {ms} ms");
    }
    let t = std::time::Instant::now();
    let (r, error) = s.tool("eval", json!({"js": "Promise.resolve().then(() => { throw new Error('boom') })"}));
    assert!(error && r.as_str().unwrap().contains("rejected") && r.as_str().unwrap().contains("boom"), "{r}");
    assert!(t.elapsed().as_millis() < 1000);
}

/// A list page with a client-side router, as GitHub has it: a click on a title hands the navigation to the router with a hidden link
/// and `click()`. The router cancels that click, calls `checkVisibility` (the sheet has `display: contents`), fetches the data and
/// moves with `pushState`.
fn router_pages(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    const LIST: &str = "<!doctype html><title>Issues</title><style>.wrap { display: contents }</style>\
<div class=wrap><h3><a href='/issues/7' tabindex='-1'><span>Issue seven</span></a></h3></div><div id=bar></div><script>\
document.addEventListener('click', function (e) {\
  var a = e.target.closest('a'); if (!a || a.hidden) return; e.preventDefault();\
  var h = document.createElement('a'); h.href = a.getAttribute('href'); h.hidden = true; document.body.appendChild(h); h.click(); h.remove();\
});\
document.addEventListener('click', function (e) {\
  var a = e.target.closest('a'); if (!a || !a.hidden) return; e.preventDefault();\
  document.getElementById('bar').checkVisibility();\
  var to = a.getAttribute('href');\
  fetch(to + '.json').then(function (r) { return r.json() }).then(function (d) {\
    history.pushState({}, '', to); document.title = d.title; document.body.innerHTML = '<h1>' + d.title + '</h1>';\
  });\
});</script>";
    match target {
        "/" => (200, "text/html", LIST.as_bytes().to_vec()),
        "/issues/7.json" => (200, "application/json", br#"{"title":"Issue seven is open"}"#.to_vec()),
        _ => (404, "text/plain", b"no".to_vec()),
    }
}

#[test]
fn a_click_that_a_router_hands_to_a_hidden_link_moves_with_push_state() {
    let base = common::serve_route(router_pages);
    let mut s = Server::start();
    let (r, error) = s.tool("open", json!({"url": format!("{base}/"), "snapshot": false, "timeout_ms": 20000}));
    assert!(!error, "{r}");
    let (r, _) = s.tool("find", json!({"role": "link", "name": "Issue seven"}));
    let r = r.as_str().unwrap();
    let rf = r.split("[").nth(1).and_then(|x| x.split(']').next()).unwrap_or_else(|| panic!("no ref in {r}")).to_string();
    let (r, error) = s.tool("act", json!({"op": "click", "ref": rf}));
    let r = r.as_str().unwrap();
    assert!(!error && r.contains(&format!("{base}/issues/7")) && r.contains("heading \"Issue seven is open\""), "{r}");
    let (u, _) = s.tool("eval", json!({"js": "location.pathname"}));
    assert_eq!(u, json!("/issues/7"));
}

#[test]
fn eval_takes_expressions_statements_return_and_await_alike() {
    let base = common::serve_route(eval_pages);
    let mut s = Server::start();
    let (r, error) = s.tool("open", json!({"url": format!("{base}/"), "timeout_ms": 1500}));
    assert!(!error, "{r}");
    let cases = [
        // An expression gives its value.
        ("document.title", json!("eval")),
        // Statements with a return, with or without await.
        ("const a = 1; return a + 1", json!(2)),
        ("return document.title", json!("eval")),
        ("const r = await fetch('/report.csv'); return r.status", json!(200)),
        // An expression with await.
        ("await (await fetch('/report.csv')).text()", json!("a,b\n1,2\n")),
        // Statements without a return give the value of the last one, like a console.
        ("let x = 1; x++; x", json!(2)),
        ("let y = 1; y++", json!(1)),
        // A return inside a function is not a top-level return.
        ("let z = [1, 2].map(function (i) { return i * 2 }); z.join()", json!("2,4")),
        // A comment at the end does not hide the end of the wrapper.
        ("1 + 1 // two", json!(2)),
        ("const c = 3; return c // three", json!(3)),
    ];
    for (js, want) in cases {
        assert_eq!(s.tool("eval", json!({"js": js})), (want, false), "{js}");
    }
    // let and const last one call: the same declaration runs again; window keeps a value.
    for _ in 0..2 {
        assert_eq!(s.tool("eval", json!({"js": "const k = 5; window.kept = k; k"})), (json!(5), false));
        assert_eq!(s.tool("eval", json!({"js": "const k = 6; return k"})), (json!(6), false));
    }
    assert_eq!(s.tool("eval", json!({"js": "typeof k + ' ' + kept"})), (json!("undefined 5"), false));
    // A real syntax error names the error and its line, in each form.
    for js in ["let a = 1;\nlet = = 2", "let a = 1;\nreturn = = 2", "await 1;\nlet = = 2"] {
        let (r, error) = s.tool("eval", json!({"js": js}));
        let r = r.as_str().unwrap().to_string();
        assert!(error && r.contains("SyntaxError") && r.contains("line 2"), "{js}: {r}");
    }
}

const NAV_ONE: &str = "<!doctype html><title>one</title><body><p>page one</p><a id=next href=/b>Next</a>
<form id=f action=/b><input name=q value=x></form></body>";
const NAV_TWO: &str = "<!doctype html><title>two</title><body><p>page two</p></body>";

/// Open the first page and mark its document, so that a later check tells a new document from the same one.
fn nav_open(s: &mut Server, base: &str) {
    let (r, e) = s.tool("open", json!({"url": format!("{base}/"), "timeout_ms": 5000}));
    assert!(!e, "{r}");
    s.tool("eval", json!({"js": "window.mark = 1"}));
}

/// A navigation that code run by `eval` starts is followed, as one that an `act` click starts, and the reply says where it went.
#[test]
fn a_navigation_that_eval_starts_is_followed() {
    let base = pages(&[("/", NAV_ONE), ("/b", NAV_TWO), ("/b?q=x", NAV_TWO)]);
    let mut s = Server::start();
    for (js, to) in [
        ("location.href = '/b'; 1", "/b"),
        ("location.assign('/b'); 1", "/b"),
        ("location.replace('/b'); 1", "/b"),
        ("document.getElementById('next').click(); 1", "/b"),
        ("document.getElementById('f').submit(); 1", "/b?q=x"),
    ] {
        nav_open(&mut s, &base);
        let (r, e) = s.tool("eval", json!({"js": js}));
        assert!(!e, "{js}: {r}");
        assert_eq!(r, json!(format!("1\nnavigated: {base}{to}")), "{js}");
        assert_eq!(s.tool("eval", json!({"js": "location.pathname + location.search + ' ' + document.title + ' ' + window.mark"})).0, json!(format!("{to} two undefined")), "{js}");
    }
    // `history.back()` goes back to the first page.
    let (r, e) = s.tool("eval", json!({"js": "history.back(); 1"}));
    assert!(!e, "{r}");
    assert_eq!(r, json!(format!("1\nnavigated: {base}/")));
    assert_eq!(s.tool("eval", json!({"js": "document.title"})).0, json!("one"));
}

/// A `#hash` change stays in the same document: no load, and no `navigated` line (the line never shows a fragment).
#[test]
fn a_hash_change_from_eval_loads_nothing() {
    let base = pages(&[("/", NAV_ONE), ("/b", NAV_TWO)]);
    let mut s = Server::start();
    nav_open(&mut s, &base);
    assert_eq!(s.tool("eval", json!({"js": "location.hash = 'x'; 1"})), (json!(1), false));
    assert_eq!(s.tool("eval", json!({"js": "location.hash + ' ' + document.title + ' ' + window.mark"})).0, json!("#x one 1"));
}

/// A page script that reads `table.rows`, `tr.cells` and `form.elements` without naming any `HTML*Element` global.
const TABLE_PAGE: &str = "<!doctype html><title>t</title><body><table id=t><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></table><form id=f><input name=q placeholder=Find></form><script>
let t = document.getElementById('t');
let r = t.insertRow(-1); r.insertCell().textContent = 'd';
let f = document.getElementById('f');
document.body.insertAdjacentHTML('beforeend', '<h1>rows ' + t.rows.length + ' cells ' + t.rows[0].cells.length + ' fields ' + f.elements.length + ' ' + f.elements[0].placeholder + '</h1>');
</script></body>";

/// The page renders its heading: the members are there before the page names their interface.
#[test]
fn a_page_that_reads_table_rows_renders() {
    let base = pages(&[("/", TABLE_PAGE)]);
    let mut s = Server::start();
    let (r, e) = s.tool("open", json!({"url": format!("{base}/")}));
    assert!(!e, "{r}");
    assert_eq!(s.tool("eval", json!({"js": "document.querySelector('h1').textContent"})).0, json!("rows 3 cells 2 fields 1 Find"));
}

const META_LIST: &str = "<!doctype html><title>list</title><body><a href=/item>First item</a></body>";

/// Like GitHub after a soft navigation: read a base64 JSON payload from a `<meta>`, change it, write it back, then render.
const META_ITEM: &str = "<!doctype html><title>item</title><meta name=visitor-payload content=eyJyZWZlcnJlciI6IiJ9><body><script>
let m = document.querySelector('meta[name=visitor-payload]');
let t = JSON.parse(atob(m.content));
t.referrer = 'list';
m.content = btoa(JSON.stringify(t));
document.body.innerHTML = '<h1>Item title</h1><p>' + JSON.parse(atob(m.content)).referrer + '</p>';
</script></body>";

/// A page that reads `meta.content` renders after a click on its link.
#[test]
fn a_page_that_reads_meta_content_renders_after_a_click() {
    let base = pages(&[("/", META_LIST), ("/item", META_ITEM)]);
    let mut s = Server::start();
    let (r, e) = s.tool("open", json!({"url": format!("{base}/")}));
    assert!(!e, "{r}");
    let (r, e) = s.tool("act", json!({"op": "click", "loc": "role=link name=\"First item\""}));
    let r = r.as_str().unwrap().to_string();
    assert!(!e && r.contains(&format!("navigated: {base}/item")) && !r.contains("atob"), "{r}");
    assert!(r.contains("heading \"Item title\""), "{r}");
    assert_eq!(s.tool("eval", json!({"js": "document.querySelector('p').textContent"})).0, json!("list"));
}

/// `act type` edits a `contenteditable` element and replaces the text on a second call.
#[test]
fn act_type_edits_a_contenteditable() {
    let base = page_server("<!doctype html><title>mail</title><div contenteditable=true id=body role=textbox aria-label=Body style='min-height:100px'></div>".to_string());
    let mut s = Server::start();
    let (r, error) = s.tool("open", json!({"url": format!("{base}/"), "snapshot": false}));
    assert!(!error, "{r}");
    let (r, error) = s.tool("act", json!({"op": "type", "loc": "css=\"#body\"", "text": "Hello world"}));
    assert!(!error, "{r}");
    let (r, _) = s.tool("eval", json!({"js": "document.getElementById('body').textContent"}));
    assert_eq!(r, json!("Hello world"));
    let (r, error) = s.tool("act", json!({"op": "type", "loc": "css=\"#body\"", "text": "Bye"}));
    assert!(!error, "{r}");
    let (r, _) = s.tool("eval", json!({"js": "document.getElementById('body').textContent"}));
    assert_eq!(r, json!("Bye"));
}

static CROSS_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn frame_pages(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    match target.split('?').next().unwrap_or("/") {
        "/frames" => (200, "text/html", b"<!doctype html><title>Frames</title>\
<button id=a>Attach</button> <button id=p>Page</button> <button id=s>Set</button> <button id=c>Cross</button>\
<script>\
function frame(src) { var f = document.createElement('iframe'); f.style.display = 'none'; f.src = src; document.body.appendChild(f); return f }\
document.getElementById('a').onclick = function () { frame('/files/report.csv?id=1') };\
document.getElementById('p').onclick = function () { frame('/frames') };\
document.getElementById('s').onclick = function () { frame('about:blank').src = '/files/invoice.pdf' };\
document.getElementById('c').onclick = function () { frame('http://localhost:' + location.port + '/files/cross.csv') };\
</script>".to_vec()),
        "/files/report.csv" => (200, "text/csv\r\nContent-Disposition: attachment; filename=\"report.csv\"", b"a,b\n1,2\n".to_vec()),
        "/files/invoice.pdf" => (200, "application/pdf\r\nContent-Disposition: attachment; filename=\"invoice.pdf\"", b"%PDF-1.4 x".to_vec()),
        "/files/cross.csv" => {
            CROSS_HITS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (200, "text/csv\r\nContent-Disposition: attachment; filename=\"cross.csv\"", b"x".to_vec())
        }
        _ => (404, "text/plain", b"no".to_vec()),
    }
}

/// A page saves a file through a hidden iframe (Gmail's Download button): a same-origin `src` that replies with a download is saved,
/// on insertion and on a later `src` change. A frame of a page saves nothing, and a cross-origin frame is never asked for.
#[test]
fn a_hidden_iframe_download_is_saved() {
    let mut s = Server::start();
    let base = common::serve_route(frame_pages);
    let dir = std::env::temp_dir().join(format!("bare-mcp-test-frame-downloads-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (r, error) = s.tool("open", json!({"url": format!("{base}/frames"), "downloads_dir": dir.to_str().unwrap(), "timeout_ms": 20000}));
    assert!(!error, "{r}");
    let click = |s: &mut Server, id: &str| {
        let (r, error) = s.tool("act", json!({"op": "click", "loc": format!("css=\"#{id}\""), "snapshot": false}));
        assert!(!error, "{r}");
        r.as_str().unwrap().to_string()
    };
    let wait = |s: &mut Server, n: usize| {
        let t = std::time::Instant::now();
        loop {
            let (list, _) = s.tool("downloads", json!({}));
            let list = list.as_str().unwrap().to_string();
            if list.matches(" bytes").count() >= n || t.elapsed().as_secs() > 5 {
                return list;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };
    click(&mut s, "a");
    let list = wait(&mut s, 1);
    assert!(list.contains("report.csv ("), "{list}");
    assert_eq!(std::fs::read(dir.join("report.csv")).unwrap(), b"a,b\n1,2\n");
    click(&mut s, "s");
    let list = wait(&mut s, 2);
    assert!(list.contains("invoice.pdf ("), "{list}");
    click(&mut s, "p");
    click(&mut s, "c");
    std::thread::sleep(std::time::Duration::from_millis(500));
    let (list, _) = s.tool("downloads", json!({}));
    assert_eq!(list.as_str().unwrap().lines().count(), 2, "{list}");
    assert_eq!(CROSS_HITS.load(std::sync::atomic::Ordering::SeqCst), 0);
    let (r, _) = s.tool("eval", json!({"js": "location.pathname"}));
    assert_eq!(r, json!("/frames"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A site with two pages that load the same two scripts: `/app.js` is `public` and `/priv.js` is `private`. Gives the base URL and the log of the
/// paths that the server answered.
fn two_page_site() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::Read;
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = log.clone();
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            let seen = seen.clone();
            std::thread::spawn(move || {
                let (mut head, mut b) = (Vec::new(), [0u8; 1]);
                while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
                    head.push(b[0]);
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let path = head.split(' ').nth(1).unwrap_or("").to_string();
                seen.lock().unwrap().push(path.clone());
                let (ty, cache, body) = match path.as_str() {
                    "/app.js" => ("text/javascript", "public, max-age=3600", "document.title='app';"),
                    "/priv.js" => ("text/javascript", "private, max-age=3600", "document.title+='priv';"),
                    _ => ("text/html", "no-cache", "<!doctype html><title>t</title><script src=/app.js></script><script src=/priv.js></script><p>page</p>"),
                };
                let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\nCache-Control: {cache}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            });
        }
    });
    (format!("http://127.0.0.1:{port}"), log)
}

/// The server shares one network client between `open` calls: the second page of a site does not fetch a script that the first one loaded (when
/// the reply is `public`). A `private` reply is kept for the cookie jar of its page only, and each `open` has its own jar. `fresh: true` starts
/// from a clean client.
#[test]
fn open_shares_the_http_cache_between_opens_and_fresh_does_not() {
    let (base, log) = two_page_site();
    let count = |p: &str| log.lock().unwrap().iter().filter(|x| x.as_str() == p).count();
    let mut s = Server::start();
    let (rep, error) = s.tool("open", json!({"url": format!("{base}/a"), "timeout_ms": 10000}));
    assert!(!error, "{rep}");
    assert_eq!((count("/app.js"), count("/priv.js")), (1, 1));
    let (rep, error) = s.tool("open", json!({"url": format!("{base}/b"), "timeout_ms": 10000}));
    assert!(!error, "{rep}");
    assert_eq!(count("/app.js"), 1, "the second open must use the cached script");
    assert_eq!(count("/priv.js"), 2, "a private reply must not cross to the jar of another open");
    let (rep, error) = s.tool("open", json!({"url": format!("{base}/c"), "fresh": true, "timeout_ms": 10000}));
    assert!(!error, "{rep}");
    assert_eq!(count("/app.js"), 2, "a fresh open has a clean client");
    // The fresh open does not empty the shared client.
    let (rep, error) = s.tool("open", json!({"url": format!("{base}/d"), "timeout_ms": 10000}));
    assert!(!error, "{rep}");
    assert_eq!(count("/app.js"), 2);
}
