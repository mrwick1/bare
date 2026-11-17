use super::*;
use std::sync::{Condvar, Mutex};

#[derive(Clone, Default)]
struct Sink(Arc<Mutex<(Vec<u8>, usize)>>);
impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let n = bytes.len().min(7); // Force partial writes.
        self.0.lock().unwrap().0.extend_from_slice(&bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.lock().unwrap().1 += 1;
        Ok(())
    }
}
fn config(extra: &[(&str, &str)]) -> Config {
    let mut vars = vec![("BARE_OBSERVE", "trace"), ("BARE_OBSERVE_FILE", "unused")];
    vars.extend_from_slice(extra);
    Config::parse(|key| {
        vars.iter()
            .rev()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| (*v).into())
    })
    .unwrap()
}
fn lines(sink: &Sink) -> Vec<Value> {
    let data = sink.0.lock().unwrap();
    std::str::from_utf8(&data.0)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}
#[test]
fn configuration_is_strict_when_enabled() {
    assert_eq!(Config::parse(|_| None).unwrap().mode(), Mode::Off);
    for (key, value) in [
        ("BARE_OBSERVE", "0"),
        ("BARE_OBSERVE_INTERVAL_MS", "0"),
        ("BARE_OBSERVE_INTERVAL_MS", "60001"),
        ("BARE_OBSERVE_INTERVAL_MS", "-1"),
        ("BARE_OBSERVE_LEVEL", "verbose"),
        ("BARE_OBSERVE_MAX_BYTES", "1023"),
        ("BARE_OBSERVE_CATEGORIES", "net,,js"),
    ] {
        assert!(
            Config::parse(|k| if k == key {
                Some(value.into())
            } else if k == "BARE_OBSERVE" {
                Some("metrics".into())
            } else if k == "BARE_OBSERVE_FILE" {
                Some("x".into())
            } else {
                None
            })
            .is_err(),
            "{key}"
        );
    }
    assert!(Config::parse(|k| (k == "BARE_OBSERVE").then(|| "metrics".into())).is_err());
}
#[test]
fn disabled_and_filtered_are_lazy() {
    let owner = Collector::start(Config::parse(|_| None).unwrap()).unwrap();
    assert_eq!(
        owner.handle().emit(
            Level::Info,
            "net",
            "test",
            Ids::default(),
            || -> [Field; 0] { panic!("disabled") }
        ),
        Emission::Disabled
    );
    let sink = Sink::default();
    let owner = Collector::with_writer(
        config(&[
            ("BARE_OBSERVE_CATEGORIES", "net"),
            ("BARE_OBSERVE_LEVEL", "warn"),
        ]),
        sink,
    )
    .unwrap();
    for (level, category) in [(Level::Info, "net"), (Level::Error, "js")] {
        assert_eq!(
            owner
                .handle()
                .emit(level, category, "test", Ids::default(), || -> [Field; 0] {
                    panic!("filtered")
                }),
            Emission::Filtered
        );
    }
    owner.shutdown();
}
#[test]
fn escaping_types_ids_and_shutdown_with_live_handle() {
    let sink = Sink::default();
    let owner = Collector::with_writer(config(&[]), sink.clone()).unwrap();
    let handle = owner.handle();
    assert_eq!(
        handle.emit(
            Level::Info,
            "net",
            "fixed\"\n",
            Ids {
                request: Some(9),
                ..Ids::default()
            },
            || [
                Field("count", Scalar::U64(3)),
                Field("unknown", Scalar::Null),
                Field("ok", Scalar::Bool(true))
            ]
        ),
        Emission::Queued
    );
    let status = owner.shutdown();
    assert_eq!(status.reason, StopReason::Shutdown);
    let records = lines(&sink);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["event"], "fixed\"\n");
    assert_eq!(records[0]["fields"]["count"], 3);
    assert_eq!(records[0]["fields"]["unknown"], Value::Null);
    assert_eq!(records[0]["request_id"], 9);
    assert!(records[0].get("session_id").is_none());
    assert_eq!(
        handle.emit(Level::Info, "net", "late", Ids::default(), || []),
        Emission::Stopped
    );
    assert!(sink.0.lock().unwrap().1 > 0);
}
#[test]
fn file_cap_reserves_terminal_status() {
    let sink = Sink::default();
    let owner = Collector::with_writer(config(&[("BARE_OBSERVE_MAX_BYTES", "1024")]), sink.clone())
        .unwrap();
    for _ in 0..20 {
        owner
            .handle()
            .emit(Level::Info, "net", "sample", Ids::default(), || []);
    }
    let status = owner.shutdown();
    assert_eq!(status.reason, StopReason::FileCap);
    assert!(status.dropped > 0);
    assert!(sink.0.lock().unwrap().0.len() <= 1024);
    assert_eq!(lines(&sink).last().unwrap()["fields"]["reason"], "file_cap");
}
struct Broken;
impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::StorageFull))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn writer_failure_is_exposed() {
    let owner = Collector::with_writer(config(&[]), Broken).unwrap();
    owner
        .handle()
        .emit(Level::Info, "net", "test", Ids::default(), || []);
    let status = owner.shutdown();
    assert_eq!(status.reason, StopReason::WriteFailed);
    assert!(status.write_error.unwrap().contains("storage"));
}
#[test]
fn periodic_flush() {
    let sink = Sink::default();
    let owner =
        Collector::with_writer(config(&[("BARE_OBSERVE_INTERVAL_MS", "1")]), sink.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while sink.0.lock().unwrap().1 == 0 {
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    owner.shutdown();
}
struct Blocked {
    gate: Arc<(Mutex<bool>, Condvar)>,
}
impl Write for Blocked {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let (lock, wake) = &*self.gate;
        let mut open = lock.lock().unwrap();
        while !*open {
            open = wake.wait(open).unwrap();
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn overflow_and_oversize_are_counted_without_waiting() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let owner = Collector::with_writer(config(&[]), Blocked { gate: gate.clone() }).unwrap();
    let h = owner.handle();
    assert_eq!(
        h.emit(
            Level::Info,
            "net",
            "x",
            Ids::default(),
            || [Field("x", Scalar::U64(0)); MAX_FIELDS + 1]
        ),
        Emission::Dropped
    );
    let mut full = false;
    for _ in 0..QUEUE_CAPACITY + 10 {
        full |= h.emit(Level::Info, "net", "x", Ids::default(), || []) == Emission::Dropped;
    }
    assert!(full);
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    assert!(owner.shutdown().dropped >= 2);
}
#[test]
fn existing_files_are_preserved_and_permissions_are_private() {
    let path = std::env::temp_dir().join(format!(
        "bare-observe-{}-{:?}",
        std::process::id(),
        thread::current().id()
    ));
    let cfg = config(&[("BARE_OBSERVE_FILE", path.to_str().unwrap())]);
    let owner = Collector::start(cfg.clone()).unwrap();
    assert!(Collector::start(cfg).is_err());
    owner.shutdown();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn serialized_size_is_bounded() {
    let sink = Sink::default();
    let owner = Collector::with_writer(config(&[]), sink.clone()).unwrap();
    // JSON escaping expands these static keys beyond the record limit.
    const KEY: &str = "\u{1}\u{2}\u{3}\u{4}\u{5}\u{6}\u{7}\u{8}\u{9}\u{a}\u{b}\u{c}\u{d}\u{e}\u{f}\u{10}\u{11}\u{12}\u{13}\u{14}\u{15}\u{16}\u{17}\u{18}\u{19}\u{1a}\u{1b}\u{1c}\u{1d}\u{1e}\u{1f}";
    let keys: Vec<&'static str> = (0..32)
        .map(|i| Box::leak(format!("{KEY}{i}").into_boxed_str()) as &str)
        .collect();
    let result = owner
        .handle()
        .emit(Level::Info, "net", "oversize", Ids::default(), || {
            std::array::from_fn::<_, 32, _>(|i| Field(keys[i], Scalar::Null))
        });
    assert_eq!(result, Emission::Dropped);
    assert_eq!(owner.shutdown().dropped, 1);
    assert_eq!(lines(&sink).len(), 1);
}
struct FlushFailure;
impl Write for FlushFailure {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("flush failed"))
    }
}
#[test]
fn flush_failure_is_exposed() {
    let owner = Collector::with_writer(config(&[]), FlushFailure).unwrap();
    assert_eq!(
        owner.shutdown().write_error.as_deref(),
        Some("flush failed")
    );
}

#[test]
fn shutdown_waits_for_inflight_emission() {
    let sink = Sink::default();
    let owner = Collector::with_writer(config(&[]), sink.clone()).unwrap();
    let handle = owner.handle();
    let (entered, ready) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let producer = thread::spawn(move || {
        handle.emit(Level::Info, "net", "inflight", Ids::default(), || {
            entered.send(()).unwrap();
            wait.recv().unwrap();
            []
        })
    });
    ready.recv().unwrap();
    let h = owner.handle();
    let shutdown = thread::spawn(move || owner.shutdown());
    while !h.inner.as_ref().unwrap().0.closed.load(Ordering::SeqCst) {
        thread::yield_now();
    }
    release.send(()).unwrap();
    assert_eq!(producer.join().unwrap(), Emission::Queued);
    assert_eq!(shutdown.join().unwrap().reason, StopReason::Shutdown);
    assert_eq!(lines(&sink).len(), 2);
}

struct PartialFailure(bool);
impl Write for PartialFailure {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0 {
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        self.0 = true;
        Ok(bytes.len().min(7))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn failure_after_partial_write_stops_capture() {
    let owner = Collector::with_writer(config(&[]), PartialFailure(false)).unwrap();
    owner
        .handle()
        .emit(Level::Info, "net", "test", Ids::default(), || []);
    let status = owner.shutdown();
    assert_eq!(status.reason, StopReason::WriteFailed);
    assert_eq!(status.dropped, 1);
}

struct Gated {
    gate: Arc<(Mutex<bool>, Condvar)>,
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
}
impl Write for Gated {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let (lock, wake) = &*self.gate;
        let mut open = lock.lock().unwrap();
        while !*open {
            open = wake.wait(open).unwrap();
        }
        self.writes.lock().unwrap().push(bytes.to_vec());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn queued_memory_is_counted_and_released() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let writes = Arc::new(Mutex::new(Vec::new()));
    let owner = Collector::with_writer(
        config(&[]),
        Gated { gate: gate.clone(), writes: writes.clone() },
    )
    .unwrap();
    let h = owner.handle();
    assert_eq!(h.queued_bytes(), Some(0));
    for _ in 0..10 {
        assert_eq!(h.emit(Level::Info, "net", "x", Ids::default(), || []), Emission::Queued);
    }
    // The blocked writer holds at most one received record.
    let held = h.status();
    assert!(held.queued_bytes > 0);
    assert_eq!(h.queued_bytes(), Some(held.queued_bytes));
    assert!(held.peak_queued_records >= 9);
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    let done = owner.shutdown();
    let writes = writes.lock().unwrap();
    let smallest = writes[..10].iter().map(Vec::len).min().unwrap() as u64;
    assert!(held.peak_queued_bytes >= 9 * smallest);
    assert_eq!(done.queued_bytes, 0);
    assert_eq!(done.peak_queued_bytes, held.peak_queued_bytes);
    assert_eq!(done.peak_queued_records, held.peak_queued_records);
    let terminal: Value = serde_json::from_slice(writes.last().unwrap()).unwrap();
    assert_eq!(terminal["fields"]["peak_queued_bytes"], done.peak_queued_bytes);
    assert_eq!(terminal["fields"]["peak_queued_records"], done.peak_queued_records);
}
