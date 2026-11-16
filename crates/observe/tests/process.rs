use bare_observe::{process as p, Field, Ids, Scalar};
#[test]
fn fixture() {
    if std::env::var_os("OBSERVE_FIXTURE").is_none() { return; }
    let owner = p::Process::from_env(false).unwrap();
    #[cfg(target_os = "linux")]
    if !p::enabled() {
        for entry in std::fs::read_dir("/proc/self/task").unwrap() {
            let name = std::fs::read_to_string(entry.unwrap().path().join("comm")).unwrap();
            assert!(!name.starts_with("bare-metrics") && !name.starts_with("bare-observe"));
        }
    }
    let _scope = p::enter(Ids { session: Some(7), step: Some(3), ..Ids::default() });
    let ids = p::context();
    std::thread::spawn(move || {
        let _scope = p::enter(ids);
        let _span = p::span("js", "execute");
        // Bounded page-thread surrogate. The sampler must advance independently.
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(40) { std::hint::spin_loop(); }
        p::emit("engine", "stats", || [Field("nodes", Scalar::U64(4))]);
    }).join().unwrap();
    owner.shutdown();
}
#[test]
fn modes_samples_context_and_shutdown() {
    for mode in ["off", "metrics", "trace"] {
        let path = std::env::temp_dir().join(format!("bare-observe-{}-{mode}.jsonl", std::process::id()));
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "fixture", "--nocapture"])
            .env("OBSERVE_FIXTURE", "1").env("BARE_OBSERVE", mode)
            .env("BARE_OBSERVE_FILE", &path).env("BARE_OBSERVE_INTERVAL_MS", "2").output().unwrap();
        assert!(out.status.success(), "{:?}", out);
        if mode == "off" { assert!(!path.exists()); continue; }
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        let records: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let samples: Vec<_> = records.iter().filter(|r| r["event"] == "sample").collect();
        assert!(samples.len() >= 2);
        assert!(samples.iter().all(|r| r.get("session_id").is_none()));
        let stats = records.iter().find(|r| r["event"] == "stats").unwrap();
        assert_eq!(stats["session_id"], 7);
        assert_eq!(stats["step_id"], 3);
        assert_eq!(records.iter().any(|r| r["event"] == "execute"), mode == "trace");
        assert_eq!(records.last().unwrap()["event"], "collector_status");
    }
}

#[test]
fn duplicate_start_preserves_owner() {
    if std::env::var_os("OBSERVE_DUPLICATE").is_some() {
        let first = std::env::var_os("BARE_OBSERVE_FILE").unwrap();
        let owner = p::Process::from_env(false).unwrap();
        let other = std::path::PathBuf::from(first).with_extension("other");
        let config = bare_observe::Config::parse(|k| match k {
            "BARE_OBSERVE" => Some("trace".into()),
            "BARE_OBSERVE_FILE" => Some(other.clone().into_os_string()),
            _ => None,
        }).unwrap();
        assert!(p::Process::start(config).is_err());
        assert!(p::enabled());
        assert!(!other.exists());
        let off = p::Process::start(bare_observe::Config::parse(|_| None).unwrap()).unwrap();
        drop(off);
        assert!(p::enabled());
        owner.shutdown();
        return;
    }
    let path = std::env::temp_dir().join(format!("observe-duplicate-{}", std::process::id()));
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "duplicate_start_preserves_owner", "--nocapture"])
        .env("OBSERVE_DUPLICATE", "1").env("BARE_OBSERVE", "trace")
        .env("BARE_OBSERVE_FILE", &path).output().unwrap();
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("other"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn failed_start_and_capped_sampler() {
    if std::env::var_os("OBSERVE_CAP").is_some() {
        let path = std::path::PathBuf::from(std::env::var_os("BARE_OBSERVE_FILE").unwrap());
        let bad = bare_observe::Config::parse(|k| match k {
            "BARE_OBSERVE" => Some("trace".into()),
            "BARE_OBSERVE_FILE" => Some(path.parent().unwrap().as_os_str().to_owned()),
            _ => None,
        }).unwrap();
        assert!(p::Process::start(bad).is_err());
        assert!(!p::enabled());
        let owner = p::Process::from_env(false).unwrap();
        for _ in 0..100 { p::emit("engine", "fill", || [Field("n", Scalar::U64(1))]); }
        #[cfg(target_os = "linux")]
        {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                let alive = std::fs::read_dir("/proc/self/task").unwrap().any(|e| {
                    std::fs::read_to_string(e.unwrap().path().join("comm")).is_ok_and(|n| n.starts_with("bare-metrics"))
                });
                if !alive { break; }
                assert!(std::time::Instant::now() < end, "sampler did not stop at cap");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        owner.shutdown();
        return;
    }
    let path = std::env::temp_dir().join(format!("observe-cap-process-{}", std::process::id()));
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "failed_start_and_capped_sampler", "--nocapture"])
        .env("OBSERVE_CAP", "1").env("BARE_OBSERVE", "trace")
        .env("BARE_OBSERVE_FILE", &path).env("BARE_OBSERVE_MAX_BYTES", "1024")
        .env("BARE_OBSERVE_INTERVAL_MS", "1").output().unwrap();
    assert!(out.status.success(), "{} {}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("file_cap"));
    std::fs::remove_file(path).unwrap();
}
