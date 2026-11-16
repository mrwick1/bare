//! Monitoring uses local fixtures and never captures page strings.
use std::{io::{Read, Write}, net::TcpListener, sync::{Arc, atomic::{AtomicBool, Ordering}}};
#[test]
fn local_journey_modes_and_secrets() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let done = stop.clone();
    let server = std::thread::spawn(move || {
        while !done.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
                    let mut buf = [0; 4096];
                    let _ = stream.read(&mut buf);
                    let body = "<html><body><input id='i'><div id='a'>SOURCE_SECRET</div><script>console.log('CONSOLE_SECRET'); var x = 1;</script></body></html>";
                    let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(std::time::Duration::from_millis(1)),
                Err(e) => panic!("{e}"),
            }
        }
    });
    let dir = std::env::temp_dir().join(format!("bare-monitor-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let steps = dir.join("steps");
    std::fs::write(&steps, format!("open http://{address}/?key=QUERY_SECRET\nexpect css=\"#a\" visible\ntype css=\"#i\" \"INPUT_SECRET\"\n")).unwrap();
    for mode in ["off", "metrics", "trace"] {
        let capture = dir.join(mode);
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_bare-drive"))
            .args(["--quiet", "--repeat", "2"]).arg(&steps).env("BARE_OBSERVE", mode).env("BARE_OBSERVE_FILE", &capture)
            .env("BARE_OBSERVE_INTERVAL_MS", "2").output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let _: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        if mode == "off" { assert!(!capture.exists()); continue; }
        let text = std::fs::read_to_string(capture).unwrap();
        for secret in ["QUERY_SECRET", "SOURCE_SECRET", "CONSOLE_SECRET", "INPUT_SECRET"] { assert!(!text.contains(secret)); }
        let records: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert!(records.iter().any(|r| r["category"] == "process"));
        assert!(records.iter().any(|r| r["category"] == "engine"));
        assert_eq!(records.iter().any(|r| r["fields"].get("duration_ns").is_some()), mode == "trace");
        if mode == "trace" {
            let sessions: std::collections::BTreeSet<_> = records.iter().filter_map(|r| r["session_id"].as_u64()).collect();
            assert_eq!(sessions.len(), 2);
            for category in ["html", "js", "style", "layout", "network", "driver"] {
                assert!(records.iter().any(|r| r["category"] == category), "missing {category}");
            }
            assert!(records.iter().any(|r| r.get("session_id").is_some() && r.get("step_id").is_some() && r["category"] == "network"));
        }
        assert_eq!(records.last().unwrap()["event"], "collector_status");
    }
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
