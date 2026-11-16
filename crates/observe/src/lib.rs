//! Opt-in JSONL observations. This crate captures no page data automatically.
//! Integration owns the collector and calls `shutdown` before process exit.
//! Field values allow only scalars. Use fixed names and opaque numeric IDs.

use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs::OpenOptions,
    io::{self, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const QUEUE_CAPACITY: usize = 256;
pub const MAX_RECORD_BYTES: usize = 4096;
pub const MAX_FIELDS: usize = 32;
const TERMINAL_RESERVE: u64 = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    Metrics,
    Trace,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}
impl Level {
    fn name(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}
#[derive(Clone, Debug)]
pub struct Config {
    mode: Mode,
    file: Option<PathBuf>,
    interval: Duration,
    categories: Option<BTreeSet<String>>,
    level: Level,
    max_bytes: u64,
}
impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::parse(|key| std::env::var_os(key))
    }
    /// Parse without mutating process environment. Disabled mode ignores other controls.
    pub fn parse(mut get: impl FnMut(&str) -> Option<OsString>) -> Result<Self, String> {
        fn text(
            get: &mut impl FnMut(&str) -> Option<OsString>,
            key: &str,
            default: &str,
        ) -> Result<String, String> {
            get(key)
                .unwrap_or_else(|| default.into())
                .into_string()
                .map_err(|_| format!("{key} must be UTF-8"))
        }
        let mode = match text(&mut get, "BARE_OBSERVE", "off")?.as_str() {
            "off" => Mode::Off,
            "metrics" => Mode::Metrics,
            "trace" => Mode::Trace,
            _ => return Err("BARE_OBSERVE must be off, metrics, or trace".into()),
        };
        let mut cfg = Self {
            mode,
            file: None,
            interval: Duration::from_millis(1000),
            categories: None,
            level: Level::Info,
            max_bytes: 64 * 1024 * 1024,
        };
        if mode == Mode::Off {
            return Ok(cfg);
        }
        cfg.file = Some(
            get("BARE_OBSERVE_FILE")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .ok_or("BARE_OBSERVE_FILE is required")?,
        );
        let interval: u64 = text(&mut get, "BARE_OBSERVE_INTERVAL_MS", "1000")?
            .parse()
            .map_err(|_| "invalid BARE_OBSERVE_INTERVAL_MS")?;
        if !(1..=60_000).contains(&interval) {
            return Err("BARE_OBSERVE_INTERVAL_MS must be 1..=60000".into());
        }
        cfg.interval = Duration::from_millis(interval);
        cfg.max_bytes = text(&mut get, "BARE_OBSERVE_MAX_BYTES", "67108864")?
            .parse()
            .map_err(|_| "invalid BARE_OBSERVE_MAX_BYTES")?;
        if cfg.max_bytes < 1024 {
            return Err("BARE_OBSERVE_MAX_BYTES must be at least 1024".into());
        }
        cfg.level = match text(&mut get, "BARE_OBSERVE_LEVEL", "info")?.as_str() {
            "error" => Level::Error,
            "warn" => Level::Warn,
            "info" => Level::Info,
            "debug" => Level::Debug,
            "trace" => Level::Trace,
            _ => return Err("invalid BARE_OBSERVE_LEVEL".into()),
        };
        if let Some(value) = get("BARE_OBSERVE_CATEGORIES") {
            let value = value
                .into_string()
                .map_err(|_| "BARE_OBSERVE_CATEGORIES must be UTF-8")?;
            let mut categories = BTreeSet::new();
            for category in value.split(',').map(str::trim) {
                if category.is_empty() || category.len() > 64 {
                    return Err("categories must be nonempty and at most 64 bytes".into());
                }
                categories.insert(category.to_owned());
            }
            cfg.categories = Some(categories);
        }
        Ok(cfg)
    }
    pub fn mode(&self) -> Mode {
        self.mode
    }
    pub fn interval(&self) -> Duration {
        self.interval
    }
}

#[derive(Clone, Copy, Default)]
pub struct Ids {
    pub session: Option<u64>,
    pub document: Option<u64>,
    pub request: Option<u64>,
    pub step: Option<u64>,
}
#[derive(Clone, Copy)]
pub enum Scalar {
    U64(u64),
    I64(i64),
    F64(f64),
    Bool(bool),
    Null,
}
#[derive(Clone, Copy)]
pub struct Field(pub &'static str, pub Scalar);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Emission {
    Disabled,
    Filtered,
    Queued,
    Dropped,
    Stopped,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    Running,
    Shutdown,
    FileCap,
    WriteFailed,
}
#[derive(Clone, Debug)]
pub struct Status {
    pub dropped: u64,
    pub reason: StopReason,
    pub write_error: Option<String>,
}
struct Shared {
    config: Config,
    start: Instant,
    closed: AtomicBool,
    dropped: AtomicU64,
    active: AtomicU64,
    reason: AtomicU8,
    error: Mutex<Option<String>>,
}
impl Shared {
    fn status(&self) -> Status {
        Status {
            dropped: self.dropped.load(Ordering::Relaxed),
            reason: match self.reason.load(Ordering::Acquire) {
                1 => StopReason::Shutdown,
                2 => StopReason::FileCap,
                3 => StopReason::WriteFailed,
                _ => StopReason::Running,
            },
            write_error: self.error.lock().unwrap().clone(),
        }
    }
    fn fail(&self, error: io::Error) {
        *self.error.lock().unwrap() = Some(error.to_string());
        self.reason.store(3, Ordering::SeqCst);
        self.closed.store(true, Ordering::SeqCst);
    }
}
#[derive(Clone, Default)]
pub struct Handle {
    inner: Option<(Arc<Shared>, SyncSender<Vec<u8>>)>,
}
impl Handle {
    pub fn accepts(&self, level: Level, category: &str) -> bool {
        self.inner.as_ref().is_some_and(|(s, _)| !s.closed.load(Ordering::Relaxed)
            && level <= s.config.level
            && s.config.categories.as_ref().is_none_or(|c| c.contains(category)))
    }
    pub fn running(&self) -> bool {
        self.inner.as_ref().is_some_and(|(s, _)| !s.closed.load(Ordering::Relaxed))
    }
    pub fn status(&self) -> Status {
        self.inner
            .as_ref()
            .map(|(s, _)| s.status())
            .unwrap_or(Status {
                dropped: 0,
                reason: StopReason::Shutdown,
                write_error: None,
            })
    }
    /// The closure runs only for enabled, matching events. Names must be fixed literals.
    /// Fields cannot contain raw strings, headers, bodies, source, or typed page input.
    pub fn emit<const N: usize>(
        &self,
        level: Level,
        category: &'static str,
        event: &'static str,
        ids: Ids,
        fields: impl FnOnce() -> [Field; N],
    ) -> Emission {
        let Some((s, tx)) = &self.inner else {
            return Emission::Disabled;
        };
        s.active.fetch_add(1, Ordering::SeqCst);
        let _active = Active(&s.active);
        if s.closed.load(Ordering::SeqCst) {
            return Emission::Stopped;
        }
        if level > s.config.level
            || s.config
                .categories
                .as_ref()
                .is_some_and(|c| !c.contains(category))
        {
            return Emission::Filtered;
        }
        let dropped = || {
            s.dropped.fetch_add(1, Ordering::Relaxed);
            Emission::Dropped
        };
        if N > MAX_FIELDS || category.len() > 64 || event.len() > 128 {
            return dropped();
        }
        let fields = fields();
        let mut values = serde_json::Map::new();
        for Field(key, value) in fields {
            if key.is_empty() || key.len() > 64 || values.contains_key(key) {
                return dropped();
            }
            let value = match value {
                Scalar::U64(n) => Value::from(n),
                Scalar::I64(n) => Value::from(n),
                Scalar::F64(n) => {
                    if !n.is_finite() {
                        return dropped();
                    }
                    Value::from(n)
                }
                Scalar::Bool(b) => Value::from(b),
                Scalar::Null => Value::Null,
            };
            values.insert(key.into(), value);
        }
        let mut record = envelope(s, level, category, event, Value::Object(values));
        for (key, value) in [
            ("session_id", ids.session),
            ("document_id", ids.document),
            ("request_id", ids.request),
            ("step_id", ids.step),
        ] {
            if let Some(value) = value {
                record[key] = Value::from(value);
            }
        }
        let mut bytes = Limited(Vec::new());
        if serde_json::to_writer(&mut bytes, &record).is_err() || bytes.write_all(b"\n").is_err() {
            return dropped();
        }
        match tx.try_send(bytes.0) {
            Ok(()) => Emission::Queued,
            Err(TrySendError::Full(_)) => dropped(),
            Err(TrySendError::Disconnected(_)) => {
                dropped();
                Emission::Stopped
            }
        }
    }
}
struct Active<'a>(&'a AtomicU64);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
fn envelope(s: &Shared, level: Level, category: &str, event: &str, fields: Value) -> Value {
    json!({"version": 1, "elapsed_ns": s.start.elapsed().as_nanos().min(u64::MAX as u128) as u64,
        "pid": std::process::id(), "thread_id": format!("{:?}", thread::current().id()),
        "level": level.name(), "category": category, "event": event, "fields": fields})
}
struct Limited(Vec<u8>);
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len() + bytes.len() > MAX_RECORD_BYTES {
            return Err(io::Error::other("record exceeds limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct Collector {
    handle: Handle,
    worker: Option<JoinHandle<()>>,
}
impl Collector {
    pub fn start(config: Config) -> io::Result<Self> {
        if config.mode == Mode::Off {
            return Ok(Self {
                handle: Handle::default(),
                worker: None,
            });
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(config.file.as_ref().expect("validated enabled config"))?;
        Self::with_writer(config, file)
    }
    /// Own a test or embedding sink. Writes and flushes run only on the worker.
    /// Shutdown waits for the sink; the sink must eventually return from I/O.
    pub fn with_writer(config: Config, writer: impl Write + Send + 'static) -> io::Result<Self> {
        if config.mode == Mode::Off {
            return Ok(Self {
                handle: Handle::default(),
                worker: None,
            });
        }
        let shared = Arc::new(Shared {
            config,
            start: Instant::now(),
            closed: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            active: AtomicU64::new(0),
            reason: AtomicU8::new(0),
            error: Mutex::new(None),
        });
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(QUEUE_CAPACITY);
        let s = shared.clone();
        let worker = thread::Builder::new()
            .name("bare-observe".into())
            .spawn(move || run_writer(s, rx, writer))?;
        Ok(Self {
            handle: Handle {
                inner: Some((shared, tx)),
            },
            worker: Some(worker),
        })
    }
    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }
    pub fn status(&self) -> Status {
        self.handle.status()
    }
    pub fn shutdown(mut self) -> Status {
        self.stop();
        self.status()
    }
    fn stop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let shared = &self.handle.inner.as_ref().unwrap().0;
            shared.closed.store(true, Ordering::SeqCst);
            // Wake recv_timeout even when producer clones remain alive.
            let _ = self.handle.inner.as_ref().unwrap().1.try_send(Vec::new());
            if worker.join().is_err() {
                shared.fail(io::Error::other("observation worker panicked"));
            }
        }
    }
}
impl Drop for Collector {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_writer(s: Arc<Shared>, rx: mpsc::Receiver<Vec<u8>>, mut writer: impl Write) {
    let mut bytes = 0u64;
    let mut flush_at = Instant::now() + s.config.interval;
    loop {
        let record = if s.closed.load(Ordering::SeqCst) {
            match rx.try_recv() {
                Ok(record) => record,
                Err(_) if s.active.load(Ordering::SeqCst) != 0 => {
                    thread::yield_now();
                    continue;
                }
                // An in-flight producer can enqueue between the first receive
                // and the active-count read. Check the queue again after zero.
                Err(_) => match rx.try_recv() {
                    Ok(record) => record,
                    Err(_) => break,
                },
            }
        } else {
            match rx.recv_timeout(flush_at.saturating_duration_since(Instant::now())) {
                Ok(record) => record,
                Err(mpsc::RecvTimeoutError::Timeout) => Vec::new(),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        };
        if !record.is_empty() {
            if s.reason.load(Ordering::Acquire) >= 2 {
                s.dropped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if record.len() as u64 > s.config.max_bytes - TERMINAL_RESERVE - bytes {
                s.dropped.fetch_add(1, Ordering::Relaxed);
                s.reason.store(2, Ordering::SeqCst);
                s.closed.store(true, Ordering::SeqCst);
                continue;
            }
            if let Err(error) = writer.write_all(&record) {
                s.dropped.fetch_add(1, Ordering::Relaxed);
                s.fail(error);
                continue;
            }
            bytes += record.len() as u64;
        }
        if Instant::now() >= flush_at {
            if let Err(error) = writer.flush() {
                s.fail(error);
                continue;
            }
            flush_at = Instant::now() + s.config.interval;
        }
    }
    s.closed.store(true, Ordering::SeqCst);
    if s.reason.load(Ordering::Acquire) != 3 {
        s.reason
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .ok();
        let reason = if s.reason.load(Ordering::Acquire) == 2 {
            "file_cap"
        } else {
            "shutdown"
        };
        let record = envelope(
            &s,
            Level::Info,
            "observe",
            "collector_status",
            json!({"reason": reason, "dropped": s.dropped.load(Ordering::Relaxed)}),
        );
        let mut terminal = serde_json::to_vec(&record).expect("scalar JSON");
        terminal.push(b'\n');
        if terminal.len() as u64 > TERMINAL_RESERVE {
            s.fail(io::Error::other("terminal status exceeds reserve"));
        } else if let Err(error) = writer.write_all(&terminal) {
            s.fail(error);
        }
    }
    if let Err(error) = writer.flush() {
        s.fail(error);
    }
}

#[cfg(test)]
mod tests;

pub mod process;
