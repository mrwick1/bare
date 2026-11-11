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
    assert_eq!(names, ["run_steps", "open", "step", "save_state", "ensure_session", "close", "snapshot", "act", "find", "read", "links", "screenshot", "eval", "console", "network", "diagnose", "sessions"]);
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
    let (rep, _) = a.tool("open", json!({"url": args["url"], "vars": args["vars"]}));
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
    let (rep, error) = s.tool("open", json!({"url": args["url"], "vars": args["vars"], "state": "n2"}));
    assert!(!error && rep["ok"] == true, "{rep}");
    let (rep, _) = s.tool("step", json!({"steps": CHECK}));
    assert_eq!(rep["ok"], true, "{rep}");
    let (rep, error) = s.tool("open", json!({"url": args["url"], "state": "nope"}));
    assert!(error && rep.as_str().unwrap().contains("nope"), "{rep}");
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
    let (rep, error) = s.tool("open", json!({"url": url, "timeout_ms": 30000}));
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
    let (r, error) = s.tool("act", json!({"op": "click", "ref": ref_of(&r, "button")}));
    let r = r.as_str().unwrap().to_string();
    assert!(!error && r.contains("navigated:") && r.contains("link \"Agents\""), "{r}");
    // The login button of the old snapshot left the document (the app changed its route without a new document).
    let (r, error) = s.tool("act", json!({"op": "click", "ref": ref_of(&snap, "button")}));
    assert!(error && r.as_str().unwrap().contains("FAILED detached"), "{r}");
    let (r, _) = s.tool("find", json!({"role": "link", "name": "agents"}));
    assert!(r.as_str().unwrap().starts_with("1 match"), "{r}");
    let (r, _) = s.tool("read", json!({"max_chars": 20}));
    assert!(r.as_str().unwrap().contains("read again with offset=20"), "{r}");
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
    // A page that does not exist: the document answered 404.
    s.tool("open", json!({"url": format!("{base}/none"), "timeout_ms": 20000}));
    let (r, _) = s.tool("diagnose", json!({}));
    assert!(r.as_str().unwrap().contains("HTTP 404") && r.as_str().unwrap().contains("render: no visible"), "{r}");
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
