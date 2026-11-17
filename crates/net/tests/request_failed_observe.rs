//! The `network/request_failed` record: a request that fails before its response headers. The fixture runs in a child process, because
//! the trace owner is one per process. It uses plain HTTP on loopback: a refused connect, a server that closes with no reply, and one
//! good request that must leave no `request_failed` record. Linux only, like the other observe tests.
#![cfg(target_os = "linux")]

use bare_net::http::{Client, Request};
use bare_observe::{Ids, process};
use bare_url::Url;
use std::io::{Read, Write};
use std::net::TcpListener;

/// The kind code of a server that closes the connection with no reply: `describe` says `protocol: malformed response: no blank line after headers`.
const NO_REPLY: u64 = 12;
const CONNECT_REFUSED: u64 = 2;

/// Runs in the child process: three requests, as the module comment says.
#[test]
fn fixture() {
    if std::env::var_os("REQUEST_FAILED_FIXTURE").is_none() {
        return;
    }
    let owner = process::Process::from_env(false).unwrap();
    let _scope = process::enter(Ids { session: Some(6), ..Ids::default() });
    let mut client = Client::new();

    // (a) a port with no listener.
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
    let e = client.send_stream(&Request::get(&url)).err().expect("connect must fail");
    eprintln!("a: {}", bare_net::cause::describe(&*e));

    // (b) a server that reads the request and closes with no reply.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = Url::parse(&format!("http://127.0.0.1:{}/", listener.local_addr().unwrap().port())).unwrap();
    let t = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = s.read(&mut buf);
    });
    let e = client.send_stream(&Request::get(&url)).err().expect("an empty reply must fail");
    eprintln!("b: {}", bare_net::cause::describe(&*e));
    t.join().unwrap();

    // (c) a good request.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = Url::parse(&format!("http://127.0.0.1:{}/", listener.local_addr().unwrap().port())).unwrap();
    let t = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = s.read(&mut buf);
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").unwrap();
    });
    let mut s = client.send_stream(&Request::get(&url)).map_err(|e| e.to_string()).unwrap();
    assert_eq!(s.response.status, 200);
    let mut body = Vec::new();
    s.body.read_to_end(&mut body).unwrap();
    assert_eq!(body, b"ok");
    drop(s);
    t.join().unwrap();

    drop(client);
    owner.shutdown();
}

fn run(mode: &str, path: &std::path::Path) {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "fixture", "--nocapture", "--test-threads=1"])
        .env("REQUEST_FAILED_FIXTURE", "1")
        .env("BARE_OBSERVE", mode)
        .env("BARE_OBSERVE_FILE", path)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    eprint!("{}", String::from_utf8_lossy(&out.stderr));
}

fn failed(mode: &str, path: &std::path::Path) -> Vec<serde_json::Value> {
    let _ = std::fs::remove_file(path);
    run(mode, path);
    let text = std::fs::read_to_string(path).unwrap();
    std::fs::remove_file(path).unwrap();
    let all: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let n = all.iter().filter(|r| r["category"] == "network" && r["event"] == "request_failed").count();
    let found: Vec<_> = all.into_iter().filter(|r| r["category"] == "network" && r["event"] == "request_failed").collect();
    assert_eq!(n, 2, "records: {found:?}");
    found
}

#[test]
fn a_request_that_fails_before_its_headers_has_a_record() {
    let path = std::env::temp_dir().join(format!("bare-request-failed-{}.jsonl", std::process::id()));
    for (mode, timed) in [("trace", true), ("metrics", false)] {
        let records = failed(mode, &path);
        let kinds: Vec<u64> = records.iter().map(|r| r["fields"]["kind"].as_u64().unwrap()).collect();
        assert_eq!(kinds, [CONNECT_REFUSED, NO_REPLY], "{mode}: {records:?}");
        for r in &records {
            assert_eq!(r["session_id"], 6, "{mode}");
            assert!(r["request_id"].is_u64(), "{mode}");
            assert_eq!(r["fields"]["retried"], false, "{mode}");
            assert_eq!(r["fields"]["replay"], false, "{mode}");
            assert_eq!(r["fields"]["duration_ns"].is_u64(), timed, "{mode}: {r:?}");
            assert!(timed || r["fields"]["duration_ns"].is_null(), "{mode}");
        }
    }
    let _ = std::fs::remove_file(&path);
    run("off", &path);
    assert!(!path.exists());
}
