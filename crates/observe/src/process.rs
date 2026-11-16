//! Process lifecycle and thread-local numeric attribution for engine hooks.
use crate::{Collector, Config, Field, Handle, Ids, Level, Mode, Scalar};
use std::{cell::Cell, sync::{OnceLock, atomic::{AtomicU8, AtomicU64, Ordering}, mpsc}, thread::JoinHandle, time::Instant};
static HANDLE: OnceLock<Handle> = OnceLock::new();
static MODE: AtomicU8 = AtomicU8::new(0);
static NEXT: AtomicU64 = AtomicU64::new(1);
thread_local! { static IDS: Cell<Ids> = Cell::new(Ids::default()); }
pub fn enabled() -> bool { MODE.load(Ordering::Relaxed) != 0 }
pub fn tracing() -> bool { MODE.load(Ordering::Relaxed) == 2 }
pub fn next_id() -> Option<u64> { enabled().then(|| NEXT.fetch_add(1, Ordering::Relaxed)) }
pub fn context() -> Ids { if enabled() { IDS.with(Cell::get) } else { Ids::default() } }
pub struct Context(Option<Ids>);
pub fn enter(ids: Ids) -> Context {
    Context(if enabled() { Some(IDS.with(|v| v.replace(ids))) } else { None })
}
impl Drop for Context { fn drop(&mut self) { if let Some(ids) = self.0 { IDS.with(|v| v.set(ids)); } } }
pub fn emit<const N: usize>(category: &'static str, event: &'static str, fields: impl FnOnce() -> [Field; N]) {
    if enabled() { HANDLE.get().unwrap().emit(Level::Info, category, event, context(), fields); }
}
pub struct Span(Option<(Instant, Ids, &'static str, &'static str)>);
pub fn span(category: &'static str, event: &'static str) -> Span {
    Span(tracing().then(|| (Instant::now(), context(), category, event)))
}
impl Drop for Span {
    fn drop(&mut self) {
        if let Some((start, ids, category, event)) = self.0 {
            HANDLE.get().unwrap().emit(Level::Info, category, event, ids, || [Field("duration_ns", Scalar::U64(start.elapsed().as_nanos().min(u64::MAX as u128) as u64))]);
        }
    }
}
pub struct Process {
    collector: Option<Collector>,
    sampler: Option<(mpsc::Sender<()>, JoinHandle<()>)>,
}
impl Process {
    pub fn from_env(child: bool) -> Result<Self, String> {
        let mut config = Config::from_env()?;
        if child && config.mode() != Mode::Off {
            let path = config.file.as_mut().unwrap();
            let mut name = path.as_os_str().to_os_string();
            name.push(format!(".pid-{}", std::process::id()));
            *path = name.into();
        }
        Self::start(config)
    }
    pub fn start(config: Config) -> Result<Self, String> {
        let mode = config.mode();
        let interval = config.interval();
        let collector = Collector::start(config).map_err(|e| format!("monitoring: {e}"))?;
        if mode == Mode::Off { return Ok(Self { collector: Some(collector), sampler: None }); }
        HANDLE.set(collector.handle()).map_err(|_| "monitoring already initialized")?;
        MODE.store(if mode == Mode::Trace { 2 } else { 1 }, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        let handle = collector.handle();
        let sampler = std::thread::Builder::new().name("bare-metrics".into()).spawn(move || {
            loop {
                handle.emit(Level::Info, "process", "sample", Ids::default(), || {
                    let s = bare_os::metrics::snapshot();
                    let f = |k, v: Option<u64>| Field(k, v.map_or(Scalar::Null, Scalar::U64));
                    [f("rss_bytes", s.rss_bytes), f("peak_rss_bytes", s.peak_rss_bytes), f("allocator_live_bytes", s.allocator_live_bytes), f("cpu_user_us", s.cpu_user_us), f("cpu_system_us", s.cpu_system_us), f("thread_count", s.thread_count)]
                });
                if rx.recv_timeout(interval) != Err(mpsc::RecvTimeoutError::Timeout) { break; }
            }
        }).map_err(|e| e.to_string())?;
        Ok(Self { collector: Some(collector), sampler: Some((tx, sampler)) })
    }
    pub fn shutdown(mut self) { self.stop(); }
    fn stop(&mut self) {
        if let Some((tx, thread)) = self.sampler.take() { let _ = tx.send(()); let _ = thread.join(); }
        if let Some(c) = self.collector.take() {
            if let Some(e) = c.shutdown().write_error { eprintln!("monitoring writer failed: {e}"); }
        }
        MODE.store(0, Ordering::Relaxed);
    }
}
impl Drop for Process { fn drop(&mut self) { self.stop(); } }
