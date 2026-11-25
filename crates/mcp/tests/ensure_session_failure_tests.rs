//! `ensure_session` keeps the code of its failure in `structuredContent.failure`, like `step` does: a bad step line is the caller's to
//! correct, and a failed step gives the kind of the driver. Offline and headless, against a loopback page of this test.

#[path = "../../drive/tests/common/mod.rs"]
mod common;

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    id: u64,
}

impl Server {
    fn start() -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_bare-mcp")).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let (stdin, stdout) = (child.stdin.take().unwrap(), BufReader::new(child.stdout.take().unwrap()));
        Server { child, stdin, stdout, id: 0 }
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        self.id += 1;
        writeln!(self.stdin, "{}", json!({"jsonrpc": "2.0", "id": self.id, "method": "tools/call", "params": {"name": name, "arguments": args}})).unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], self.id, "{line}");
        reply["result"].clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn page(_: &str, _: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    let html = "<!doctype html><title>Login</title><label for=email>Email</label><input id=email><button>Sign in</button>";
    (200, "text/html", html.as_bytes().to_vec())
}

fn failure(s: &mut Server, name: &str, login: &str, check: &str) -> Value {
    let url = format!("{}/", common::serve_route(page));
    let r = s.call("ensure_session", json!({"name": name, "url": url, "login": login, "check": check, "timeout_ms": 3000}));
    assert_eq!(r["isError"], true, "{r}");
    r["structuredContent"]["failure"].clone()
}

#[test]
fn a_bad_step_line_is_invalid_arguments() {
    let mut s = Server::start();
    let f = failure(&mut s, "a", "fill label=\"Email\" someone@example.test", "expect text=\"Sign in\" visible");
    assert_eq!((f["code"].as_str(), f["next_action"].as_str(), f["input_dispatched"].as_str()), (Some("invalid_arguments"), Some("correct_arguments"), Some("no")), "{f}");
    assert!(f["message"].as_str().unwrap().ends_with("put the text in double quotes"), "{f}");
    let f = failure(&mut s, "b", "click label=\"Email\"", "expect text=\"Sign in\"");
    assert_eq!(f["code"], "invalid_arguments", "{f}");
}

#[test]
fn a_failed_step_gives_the_driver_kind() {
    let mut s = Server::start();
    let f = failure(&mut s, "c", "click label=\"Email\"", "expect text=\"Welcome\" visible");
    assert_eq!(f["code"], "not_found", "{f}");
}
