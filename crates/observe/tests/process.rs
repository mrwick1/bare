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
