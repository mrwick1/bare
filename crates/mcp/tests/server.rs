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
    assert_eq!(names, ["run_steps", "open", "step", "save_state", "ensure_session", "close", "snapshot", "act", "find", "read", "links", "screenshot", "eval", "console", "network", "diagnose", "downloads", "sessions"]);
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
    // Without a snapshot, the receipt still reports the navigation (#382).
    let (r, error) = s.tool("act", json!({"op": "click", "ref": ref_of(&r, "button"), "snapshot": false}));
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
    let act = |s: &mut Server, args: Value| {
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
    let (r, error) = s.tool("run_steps", json!({"steps": format!("open {base}/forms\nselect css=\"#country\" \"Tanzania\"\ncheck label=\"Agree\"\nexpect label=\"Agree\" checked\nuncheck label=\"Agree\"\nhover text=\"Hover target\"\ndblclick css=\"#dbl\"\nexpect css=\"#dbls\" text=\"double\"\nupload css=\"input[type=file]\" \"{file}\"\n"), "compact": true}));
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
