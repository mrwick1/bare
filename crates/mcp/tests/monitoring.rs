use std::io::Write;
#[test]
fn capture_preserves_protocol_stdout_and_error_exit() {
    let dir = std::env::temp_dir().join(format!("bare-mcp-monitor-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("capture");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_bare-mcp"))
        .env("BARE_OBSERVE", "trace").env("BARE_OBSERVE_FILE", &path)
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let lines = String::from_utf8(out.stdout).unwrap();
    let records: Vec<serde_json::Value> = lines.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["id"], 1);
    let capture = std::fs::read_to_string(&path).unwrap();
    assert!(capture.lines().all(|l| serde_json::from_str::<serde_json::Value>(l).is_ok()));
    assert!(capture.contains("collector_status"));
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bare-mcp"))
        .env("BARE_OBSERVE", "invalid").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}
