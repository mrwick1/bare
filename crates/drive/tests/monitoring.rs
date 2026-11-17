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
                    let body = "<html><body><input id='i'><div id='a'>SOURCE_SECRET</div><script>console.log('CONSOLE_SECRET'); var x = 0; for(var j=0;j<100000;j++) x += j; setTimeout(function(){x++;},0);</script></body></html>";
                    let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nX-Secret: HEADER_SECRET\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
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
        for secret in ["QUERY_SECRET", "SOURCE_SECRET", "CONSOLE_SECRET", "INPUT_SECRET", "HEADER_SECRET"] { assert!(!text.contains(secret)); }
        let records: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert!(records.iter().any(|r| r["category"] == "process"));
        assert!(records.iter().any(|r| r["category"] == "engine"));
        assert_eq!(records.iter().any(|r| r["fields"].get("duration_ns").is_some()), mode == "trace");
        if mode == "trace" {
            let sessions: std::collections::BTreeSet<_> = records.iter().filter_map(|r| r["session_id"].as_u64()).collect();
            assert_eq!(sessions.len(), 2);
            for category in ["html", "js", "style", "layout", "network", "driver", "scheduler"] {
                assert!(records.iter().any(|r| r["category"] == category), "missing {category}");
            }
            assert!(records.iter().any(|r| r.get("session_id").is_some() && r.get("step_id").is_some() && r["category"] == "network"));
        }
        let bodies: Vec<_> = records.iter().filter(|r| r["event"] == "body").collect();
        assert!(!bodies.is_empty());
        assert!(bodies.iter().all(|r| r["fields"]["bytes"].as_u64().unwrap() > 0 && r["fields"]["status"] == 200 && r["fields"]["outcome"] == 1));
        assert!(bodies.iter().all(|r| r["request_id"].is_u64() && r["session_id"].is_u64()));
        assert_eq!(records.last().unwrap()["event"], "collector_status");
    }
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn concurrent_engine_fixture() {
    if std::env::var_os("BARE_ENGINE_FIXTURE").is_none() { return; }
    use bare_observe::{process as p, Ids};
    let owner = p::Process::from_env(false).unwrap();
    let threads: Vec<_> = (1..=2).map(|session| std::thread::spawn(move || {
        let _context = p::enter(Ids { session: Some(session), ..Ids::default() });
        let mut browser = bare::browser::Browser::new(bare_html::tree::parse("<body><div>fixture</div></body>"), Default::default(), Default::default(), Default::default()).unwrap();
        browser.run_script("var x=0; for(var i=0;i<100000;i++) x+=i;").unwrap();
        browser.collect(&[]).unwrap();
        let frame = browser.render().unwrap();
        let doc = browser.document();
        let _context = p::enter(Ids { document: Some(session), ..p::context() });
        let pixels = bare_paint::capture(&doc.borrow(), &frame.styles, &frame.layout, 320, 240);
        assert!(!pixels.pixels.is_empty());
    })).collect();
    for thread in threads { thread.join().unwrap(); }
    owner.shutdown();
}

#[test]
fn long_scripts_gc_and_concurrent_documents() {
    let path = std::env::temp_dir().join(format!("engine-monitor-{}", std::process::id()));
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "concurrent_engine_fixture", "--nocapture"])
        .env("BARE_ENGINE_FIXTURE", "1").env("BARE_OBSERVE", "trace")
        .env("BARE_OBSERVE_FILE", &path).env("BARE_OBSERVE_INTERVAL_MS", "1").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    let records: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    for session in [1, 2] {
        let stats: Vec<_> = records.iter().filter(|r| r["event"] == "browser_stats" && r["session_id"] == session).collect();
        assert!(stats.len() >= 2);
        assert!(stats.iter().all(|r| r["document_id"].is_u64()));
        for category in ["gc", "paint"] {
            assert!(records.iter().any(|r| r["category"] == category && r["session_id"] == session), "missing {category}");
        }
    }
    let executions: Vec<_> = records.iter().filter(|r| r["category"] == "js" && r["fields"]["duration_ns"].as_u64().is_some_and(|n| n > 2_000_000)).collect();
    assert!(!executions.is_empty(), "no long script captured");
    assert!(executions.iter().any(|e| {
        let end = e["elapsed_ns"].as_u64().unwrap();
        let start = end - e["fields"]["duration_ns"].as_u64().unwrap();
        records.iter().filter(|r| r["event"] == "sample" && r["elapsed_ns"].as_u64().is_some_and(|t| t > start && t < end)).count() >= 2
    }), "no independent samples during script");
}

#[test]
fn suite_worker_paths_and_error_flush() {
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
                    let _ = stream.read(&mut [0; 4096]);
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\nConnection: close\r\n\r\n<body>ok</body>");
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(std::time::Duration::from_millis(1)),
                Err(e) => panic!("{e}"),
            }
        }
    });

    let dir = std::env::temp_dir().join(format!("monitor-suite-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    for name in ["one", "two"] {
        std::fs::write(dir.join(format!("{name}.steps")), format!("open http://{address}/\n")).unwrap();
    }
    let path = dir.join("capture");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bare-drive"))
        .arg("--suite").arg(&dir).args(["--workers", "2", "--out"]).arg(dir.join("out"))
        .env("BARE_OBSERVE", "metrics").env("BARE_OBSERVE_FILE", &path).output().unwrap();
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let workers: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("capture.pid-")).collect();
    assert_eq!(workers.len(), 2);
    for path in workers.iter().chain(std::iter::once(&path)) {
        let text = std::fs::read_to_string(path).unwrap();
        let records: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(records.last().unwrap()["event"], "collector_status");
        if let Some(pid) = path.to_string_lossy().split(".pid-").nth(1) {
            assert!(records.iter().all(|r| r["pid"].as_u64() == pid.parse().ok()));
        }
    }
    let error = dir.join("error");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bare-drive"))
        .arg("--unknown-option").env("BARE_OBSERVE", "trace").env("BARE_OBSERVE_FILE", &error).output().unwrap();
    assert!(!out.status.success());
    assert!(std::fs::read_to_string(error).unwrap().contains("collector_status"));
    std::fs::remove_dir_all(dir).unwrap();
}
