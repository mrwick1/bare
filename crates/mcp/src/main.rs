//! `bare-mcp`: an MCP server over stdio for end-to-end tests in `bare`.
//!
//! An agent (Claude Code, Codex) starts this program and talks JSON-RPC 2.0 to it: one JSON object on each line of stdin and stdout.
//! The tools use the test driver (`bare-drive`), so each click and key goes through the same hit testing and input path as a user.
//!
//! - `run_steps`: run a whole step file and give its report.
//! - `run_suite`: run a directory of step files with one shared login in K worker processes (`bare-drive --suite`) and give a short summary.
//! - `ensure_session`, `save_state`: a logged-in live page from a saved session (in memory by default), or a new login when it is no longer valid.
//! - `open`, `step`, `close`: a live session. `open` loads a page, each `step` call runs more step lines on the same page, and `close`
//!   ends it. Each call gives the report of its own steps only, so the server keeps no old records.
//!
//! Headless by default. With `window: true` the page shows in a Wayland window on the display that `WAYLAND_DISPLAY` names.
//!
//! Only JSON goes to stdout: the protocol owns it. Progress and errors go to stderr. JS: like a Node script that reads `readline`
//! lines from `process.stdin` and writes `JSON.stringify(reply)` lines to `process.stdout`.
#![recursion_limit = "256"]

mod metrics;
mod recovery;

use bare_drive::{DownloadState, Downloads};
use recovery::Failure;
use bare_drive::locate::{Locator, Text};
use bare_drive::run::{Event, Options, Redactor, Report, Session, StorageState, cancelled, set_cancel};
use bare_drive::snap::{self, Mode};
use bare_drive::steps;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A live session: the page, its options and its secrets, kept between tool calls.
struct Live {
    session: Session,
    opt: Options,
    red: Redactor,
    vars: HashMap<String, String>,
    /// Step lines run so far: each new call numbers its lines after them, so the line numbers in reports stay unique.
    lines: usize,
    /// The files that the session saved from downloads (see `bare::downloads`).
    dl: Downloads,
    /// The downloads that an `act` receipt already named.
    told: std::collections::HashSet<usize>,
}

/// What is known independently of the text receipt, including errors after dispatch.
struct ActionOutcome {
    action_status: &'static str,
    page_status: &'static str,
    verification: &'static str,
}

/// The tools that one worker thread runs for one live session, and also the one-shot calls (`run_steps`, `run_suite`, `sessions`), each on
/// a thread of its own with an empty `live`. A page is not `Send`: it is built on the thread that owns it and never leaves it. Only JSON
/// and plain data cross between threads.
struct Server {
    shared: Arc<Shared>,
    /// A session worker holds its one session here, by name (`session`, default `main`).
    live: HashMap<String, Live>,
    /// The queue of this worker, for the wake of its pages (`Session::wake_on_done`). Weak, so that the worker still ends when its slot goes.
    queue: Weak<SyncSender<Job>>,
}

/// What every thread shares. No lock is held while a page runs: each lock guards a short read or write of a map.
struct Shared {
    /// Saved sessions by name: cookies and storage, in this process only. A file is written only when a call gives `file`.
    saved: Mutex<HashMap<String, StorageState>>,
    /// A counter for the capture folders of this process.
    runs: AtomicU32,
    /// The network client that every `open` forks (see `start`): the connections and the reply cache outlive a session.
    net: Mutex<bare_net::http::Client>,
    /// The writer thread: one complete JSON line at a time.
    out: mpsc::Sender<Out>,
    /// The session workers by name. The only holders of a queue sender, so removing a slot closes its queue.
    sessions: Mutex<HashMap<String, Slot>>,
    /// The cancellation token of each request that has no reply yet, by request id (as JSON text).
    inflight: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// Session workers and one-shot calls, joined at shutdown.
    threads: Mutex<Vec<JoinHandle<()>>>,
    closing: AtomicBool,
}

enum Out {
    Line(String),
    /// Answer when every line before is written.
    Flush(mpsc::Sender<()>),
}

enum Input {
    Line(String),
    Eof,
    WriterGone,
}

/// A tool call on its way: where its reply goes and the token that cancels it.
struct Job {
    /// The request id as JSON text. `None` for an internal job.
    key: Option<String>,
    /// The `params` of the `tools/call` request.
    params: Value,
    cancel: Arc<AtomicBool>,
    /// Whether an owner started this call. Transport refusals cannot have sent input; panics after this point are unknown.
    started: bool,
    /// When the request arrived, for the metrics line.
    t0: Instant,
    done: Box<dyn FnOnce(Value) + Send>,
    /// Not a call: a page of the worker has news (a reply, a WebSocket message). The worker only runs its pages, and clears the flag,
    /// which keeps a second wake out of the queue until then.
    wake: Option<Arc<AtomicBool>>,
}

/// The queue of a session worker. `busy` counts its queued and running jobs; `url` is its last published address or failed-load diagnostic.
struct Slot {
    tx: Arc<SyncSender<Job>>,
    url: Arc<Mutex<String>>,
    busy: Arc<AtomicUsize>,
}

/// The calls that wait for one session. More than this is refused at once with an error, so a client cannot grow the queue without a bound.
const QUEUE: usize = 64;
/// The most one-shot calls at the same time.
const ONESHOT: usize = 16;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn error_result(text: impl Into<String>) -> Value {
    json!({"content": [{"type": "text", "text": text.into()}], "isError": true})
}

fn main() {
    let _monitor = bare_observe::process::Process::from_env(false).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(2) });
    if std::env::args().nth(1).as_deref() == Some("--stats") {
        return metrics::stats(std::env::args().nth(2));
    }
    if std::env::args().len() > 1 {
        return eprintln!("usage: bare-mcp [--stats [FILE]]");
    }
    bare_net::parallel::limit_malloc_arenas(false);
    // This thread only reads requests and routes them (`Shared::handle_line`), so `ping`, `tools/list` and `notifications/cancelled` are
    // answered while pages work. A thread writes the replies, one whole line at a time. Each live session has a thread of its own that runs
    // its calls in order and its page between calls (#471): timers, replies and the requests that they start, as in a browser tab. A page
    // with work due now runs again after 1 ms at the earliest, so it does not spin a core.
    let (otx, orx) = mpsc::channel::<Out>();
    let (itx, irx) = mpsc::channel::<Input>();
    let gone = itx.clone();
    std::thread::spawn(move || {
        let mut out = std::io::stdout().lock();
        for m in orx {
            match m {
                Out::Line(l) => {
                    if writeln!(out, "{l}").and_then(|_| out.flush()).is_err() {
                        let _ = gone.send(Input::WriterGone);
                        return;
                    }
                }
                Out::Flush(ack) => {
                    let _ = ack.send(());
                }
            }
        }
    });
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(l) => {
                    if itx.send(Input::Line(l)).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = itx.send(Input::Eof);
    });
    let shared = Arc::new(Shared {
        saved: Mutex::new(HashMap::new()),
        runs: AtomicU32::new(0),
        net: Mutex::new(bare_net::http::Client::new()),
        out: otx,
        sessions: Mutex::new(HashMap::new()),
        inflight: Mutex::new(HashMap::new()),
        threads: Mutex::new(Vec::new()),
        closing: AtomicBool::new(false),
    });
    while let Ok(Input::Line(line)) = irx.recv() {
        shared.handle_line(&line);
    }
    shared.shutdown();
}

impl Shared {
    fn send(&self, v: Value) {
        let _ = self.out.send(Out::Line(v.to_string()));
    }

    /// Read one line of input. A request gets one reply, except a cancelled one (see `finish`); a notification gets none.
    fn handle_line(self: &Arc<Self>, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        let msg = match serde_json::from_str::<Value>(line) {
            Ok(m) => m,
            Err(e) => return self.send(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}})),
        };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let Some(id) = msg.get("id").cloned() else {
            // A notification: no `id`, and no reply. The id of the request to cancel is `params.requestId`. An unknown id, a request that
            // already ended and a malformed notification are ignored, as the protocol says.
            if method == "notifications/cancelled"
                && let Some(t) = msg.pointer("/params/requestId").and_then(|r| lock(&self.inflight).get(&r.to_string()).cloned()) {
                    t.store(true, Ordering::SeqCst);
                }
            return;
        };
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let result = match method {
            "initialize" => Ok(json!({
                // Answer with the version that the client asked for: this server uses only the tools part, which has not changed.
                "protocolVersion": params.get("protocolVersion").cloned().unwrap_or(json!("2025-06-18")),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "bare-mcp", "version": env!("CARGO_PKG_VERSION")},
                "instructions": instructions(),
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => return self.dispatch(id, params),
            m => Err(json!({"code": -32601, "message": format!("unknown method {m}")})),
        };
        self.send(match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
        });
    }

    /// Start a tool call. It runs on the thread of its session, or on one of its own for the tools that need no session.
    fn dispatch(self: &Arc<Self>, id: Value, params: Value) {
        let tool = params.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        let key = id.to_string();
        let cancel = Arc::new(AtomicBool::new(false));
        lock(&self.inflight).insert(key.clone(), cancel.clone());
        let sh = self.clone();
        let done = Box::new(move |v: Value| sh.send(json!({"jsonrpc": "2.0", "id": id, "result": v})));
        let job = Job { key: Some(key), params, cancel, started: false, t0: Instant::now(), done, wake: None };
        if !tool_list().as_array().is_some_and(|t| t.iter().any(|t| t["name"] == tool.as_str())) {
            let failure = Failure::unknown_tool(&tool);
            let result = json!({"content": [{"type": "text", "text": failure.text}], "isError": true,
                "structuredContent": recovery::structured(&failure, &Redactor::default())});
            return self.finish(job, result);
        }
        match tool.as_str() {
            "run_steps" | "run_suite" | "sessions" => self.oneshot(job),
            _ => self.route(job),
        }
    }

    /// Deliver the result of a job, once. A cancelled job gets no reply: the protocol asks a server to stop and to send none.
    fn finish(&self, job: Job, mut result: Value) {
        if let Some(k) = &job.key {
            lock(&self.inflight).remove(k);
        }
        if job.cancel.load(Ordering::SeqCst) {
            return;
        }
        // Queue refusals and caught panics do not pass through Server::call. Preserve the same error contract here.
        if result["isError"] == true {
            if !result["structuredContent"].is_object() {
                result["structuredContent"] = json!({});
            }
            if result["structuredContent"].get("failure").is_none() {
                let text = result["content"][0]["text"].as_str().unwrap_or("tool failed").to_string();
                let mut failure = Failure::from(text);
                failure.input = if job.started { "unknown" } else { "no" };
                result["structuredContent"]["failure"] = recovery::structured(&failure, &Redactor::default())["failure"].clone();
            }
            if job.params["name"] == "act" && result["structuredContent"].get("action_status").is_none() {
                let status = &mut result["structuredContent"];
                status["action_status"] = json!(if job.started { "unknown" } else { "not_started" });
                status["page_status"] = json!("unknown");
                status["verification"] = json!(if job.params.pointer("/arguments/expect").is_none() { "not_requested" } else if job.started { "unknown" } else { "not_run" });
                let text = status.to_string();
                result["content"].as_array_mut().expect("tool content").push(json!({"type": "text", "text": text}));
            }
        }
        if job.key.is_some() {
            metrics::record(&job.params, job.t0.elapsed(), &result);
        }
        (job.done)(result);
    }

    /// Queue a job on the worker of its session, in arrival order. `open` and `ensure_session` start the worker when there is none. The
    /// queue sender is used under the lock of the map, so a worker that retires (`retire`) cannot lose a job between the two.
    fn route(self: &Arc<Self>, job: Job) {
        let tool = job.params.get("name").and_then(Value::as_str).unwrap_or("");
        let name = session_name(job.params.get("arguments").unwrap_or(&Value::Null));
        let creates = matches!(tool, "open" | "ensure_session");
        if self.closing.load(Ordering::SeqCst) {
            return self.finish(job, error_result("the server is shutting down"));
        }
        let mut map = lock(&self.sessions);
        if let Some(slot) = map.get(&name) {
            slot.busy.fetch_add(1, Ordering::SeqCst);
            return match slot.tx.try_send(job) {
                Ok(()) => {}
                Err(TrySendError::Full(job)) => {
                    slot.busy.fetch_sub(1, Ordering::SeqCst);
                    drop(map);
                    self.finish(job, error_result(format!("session {name} has {QUEUE} calls queued: wait for their replies")));
                }
                Err(TrySendError::Disconnected(job)) => {
                    map.remove(&name);
                    drop(map);
                    self.finish(job, error_result(format!("the worker of session {name} ended: call open first")));
                }
            };
        }
        if !creates {
            drop(map);
            let result = Server { shared: self.clone(), live: HashMap::new(), queue: Weak::new() }.call(&job.params);
            return self.finish(job, result);
        }
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let tx = Arc::new(tx);
        let (url, busy) = (Arc::new(Mutex::new(String::new())), Arc::new(AtomicUsize::new(1)));
        let (sh, n, u, b, q) = (self.clone(), name.clone(), url.clone(), busy.clone(), Arc::downgrade(&tx));
        match std::thread::Builder::new().name(format!("bare-mcp {name}")).spawn(move || worker(sh, n, rx, q, u, b, None)) {
            Ok(h) => {
                self.keep(h);
                let _ = tx.try_send(job);
                map.insert(name, Slot { tx, url, busy });
            }
            Err(e) => {
                drop(map);
                self.finish(job, error_result(format!("cannot start a thread for session {name}: {e}")));
            }
        }
    }

    /// A call that has no session of its own: a thread for this call alone.
    fn oneshot(self: &Arc<Self>, job: Job) {
        // Session creation takes sessions before threads. Do not hold either lock while acquiring the other.
        let sessions = lock(&self.sessions).len();
        let running = lock(&self.threads).iter().filter(|h| !h.is_finished()).count();
        if running >= ONESHOT + sessions {
            return self.finish(job, error_result(format!("{ONESHOT} calls of run_steps, run_suite or sessions are running: wait for one")));
        }
        let sh = self.clone();
        let slot = Arc::new(Mutex::new(Some(job)));
        let moved = slot.clone();
        match std::thread::Builder::new().name("bare-mcp call".into()).spawn(move || {
            if let Some(job) = lock(&moved).take() {
                Server { shared: sh, live: HashMap::new(), queue: Weak::new() }.run_job(job, None);
            }
        }) {
            Ok(h) => self.keep(h),
            Err(e) => {
                if let Some(job) = lock(&slot).take() {
                    self.finish(job, error_result(format!("cannot start a thread: {e}")));
                }
            }
        }
    }

    fn keep(&self, h: JoinHandle<()>) {
        let mut t = lock(&self.threads);
        t.retain(|h| !h.is_finished());
        t.push(h);
    }

    /// An empty worker drains its own pending calls before freeing the name. Checking the queue and removing the slot share the routing
    /// lock, so a newer call cannot jump ahead of calls queued before close.
    fn retire(&self, name: &str, rx: &Receiver<Job>) -> Option<Job> {
        let mut sessions = lock(&self.sessions);
        if let Ok(job) = rx.try_recv() {
            return Some(job);
        }
        sessions.remove(name);
        None
    }

    /// Call `tool` on session `name` with no client request behind it, and wait for the result text. `None` when the session is gone.
    fn ask(self: &Arc<Self>, name: &str, tool: &str) -> Option<Result<String, String>> {
        let (tx, rx) = mpsc::channel();
        let done = Box::new(move |v: Value| {
            let text = v["content"][0]["text"].as_str().unwrap_or("").to_string();
            let _ = tx.send(if v["isError"] == true { Err(text) } else { Ok(text) });
        });
        let params = json!({"name": tool, "arguments": {"session": name}});
        self.route(Job { key: None, params, cancel: Arc::new(AtomicBool::new(false)), started: false, t0: Instant::now(), done, wake: None });
        // A busy worker is not asked (`sessions`), so this returns when one call ends; a cancellation of the caller stops the wait.
        loop {
            match rx.recv_timeout(Duration::from_millis(25)) {
                Ok(r) => return Some(r),
                Err(RecvTimeoutError::Timeout) if !cancelled() => {}
                Err(_) => return None,
            }
        }
    }

    /// Start the worker of a tab of `base` under a free name `BASE~N`, and give the name.
    fn start_tab(self: &Arc<Self>, base: &str, tab: Tab) -> Result<String, String> {
        if self.closing.load(Ordering::SeqCst) {
            return Err("the server is shutting down".into());
        }
        let mut map = lock(&self.sessions);
        let name = (2..).map(|n| format!("{base}~{n}")).find(|n| !map.contains_key(n)).expect("a free name");
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let tx = Arc::new(tx);
        let (url, busy) = (Arc::new(Mutex::new(String::new())), Arc::new(AtomicUsize::new(1)));
        let (sh, n, u, b, q) = (self.clone(), name.clone(), url.clone(), busy.clone(), Arc::downgrade(&tx));
        let h = std::thread::Builder::new().name(format!("bare-mcp {name}")).spawn(move || worker(sh, n, rx, q, u, b, Some(tab))).map_err(|e| format!("cannot start a thread: {e}"))?;
        self.keep(h);
        map.insert(name.clone(), Slot { tx, url, busy });
        Ok(name)
    }

    fn run_no(&self) -> u32 {
        self.runs.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Stop at the end of input: cancel every request, close every queue, wait for the threads (a suite kills and reaps its workers when
    /// it sees the token), and let the writer finish. A thread inside a native call that does not end is left to the exit of the process.
    fn shutdown(&self) {
        self.closing.store(true, Ordering::SeqCst);
        for t in lock(&self.inflight).values() {
            t.store(true, Ordering::SeqCst);
        }
        drop(std::mem::take(&mut *lock(&self.sessions)));
        let end = Instant::now() + Duration::from_secs(10);
        while lock(&self.threads).iter().any(|h| !h.is_finished()) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        let (ack, wait) = mpsc::channel();
        if self.out.send(Out::Flush(ack)).is_ok() {
            let _ = wait.recv_timeout(Duration::from_secs(2));
        }
    }
}

/// The thread of one live session: it runs the calls of its queue in order, and the page between calls. It ends when its session is gone
/// (`close`, or a failed open) or when the queue closes. A tab starts with `tab`: it opens its page first and reports to its opener.
fn worker(shared: Arc<Shared>, name: String, rx: Receiver<Job>, queue: Weak<SyncSender<Job>>, url: Arc<Mutex<String>>, busy: Arc<AtomicUsize>, tab: Option<Tab>) {
    let mut server = Server { shared: shared.clone(), live: HashMap::new(), queue };
    if let Some(t) = tab {
        server.begin_tab(&name, t);
        server.publish(&name, &url);
        busy.fetch_sub(1, Ordering::SeqCst);
    }
    loop {
        let job = if server.live.is_empty() {
            match shared.retire(&name, &rx) {
                Some(job) => job,
                None => return,
            }
        } else {
            match server.tick_live() {
                Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now()).max(Duration::from_millis(1))) {
                    Ok(j) => j,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                None => match rx.recv() {
                    Ok(j) => j,
                    Err(_) => return,
                },
            }
        };
        if let Some(pending) = &job.wake {
            pending.store(false, Ordering::SeqCst);
            continue;
        }
        server.run_job(job, Some(&busy));
        server.publish(&name, &url);
    }
}

/// The start of a tab session: what it copies from its opener, and where it reports the result of its first load.
struct Tab {
    opt: Options,
    red: Redactor,
    vars: HashMap<String, String>,
    state: Option<StorageState>,
    url: String,
    done: mpsc::Sender<Result<(), String>>,
}

impl Server {
    /// The pages of `session` wake this worker when they have news: a reply, or a WebSocket message. Then `tick_live` asks for a tick
    /// each second, not every 4 ms. At most one wake waits in the queue, so wakes never fill it. A full queue drops the wake: the worker has calls to run, and it
    /// ticks its pages after them.
    fn wake_on_done(&self, session: &mut Session) {
        let (queue, pending) = (self.queue.clone(), Arc::new(AtomicBool::new(false)));
        session.wake_on_done(move || {
            let Some(tx) = queue.upgrade() else { return };
            if pending.swap(true, Ordering::SeqCst) {
                return;
            }
            let done: Box<dyn FnOnce(Value) + Send> = Box::new(|_| {});
            let job = Job { key: None, params: Value::Null, cancel: Arc::new(AtomicBool::new(true)), started: false, t0: Instant::now(), done, wake: Some(pending.clone()) };
            if tx.try_send(job).is_err() {
                pending.store(false, Ordering::SeqCst);
            }
        });
    }

    /// One slice of work for each live page. Gives when the next task of a page is due.
    fn tick_live(&mut self) -> Option<Instant> {
        self.live.values_mut().filter_map(|l| l.session.tick()).min()
    }

    /// Run a job on this thread with its token set, and deliver its reply unless it was cancelled. A job cancelled before it starts does
    /// not run. A panic becomes an error reply and does not end the server.
    fn run_job(&mut self, mut job: Job, busy: Option<&AtomicUsize>) {
        let result = if job.cancel.load(Ordering::SeqCst) {
            Value::Null
        } else {
            job.started = true;
            set_cancel(Some(job.cancel.clone()));
            let r = catch_unwind(AssertUnwindSafe(|| self.call(&job.params)));
            set_cancel(None);
            r.unwrap_or_else(|_| error_result("internal error: the call panicked"))
        };
        // `busy` drops before the reply is sent, so a client that has the reply sees an idle session.
        if let Some(b) = busy {
            b.fetch_sub(1, Ordering::SeqCst);
        }
        self.shared.clone().finish(job, result);
    }

    /// Publish the safe page address or diagnostic for `sessions`, which does not wait for a busy session.
    fn publish(&mut self, name: &str, url: &Mutex<String>) {
        *lock(url) = self.live.get_mut(name).map(|l| l.session.url().map(|u| bare_drive::run::safe_url(&u)).unwrap_or_else(|| match l.session.load_error() {
            Some(why) => format!("(no document: the document did not load: {why})"),
            None => "(no document)".into(),
        })).unwrap_or_default();
    }

    /// Run a tool. A tool error is a result with `isError`, not a protocol error, so the agent sees the message.
    fn call(&mut self, params: &Value) -> Value {
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        if name == "act" {
            return self.act(&args);
        }
        // The secrets of the session that the call names, for the evidence of a failure; `close` drops the session, so take them first.
        let red = self.live.get(&session_name(&args)).map(|l| l.red.clone()).unwrap_or_default();
        let fail = |f: Failure, red: &Redactor| json!({"content": [{"type": "text", "text": red.clean(&f.text)}], "isError": true, "structuredContent": recovery::structured(&f, red)});
        if let Err(e) = known_args(name, &args) {
            return fail(Failure::args(e), &red);
        }
        // The tools that act on a live session: the lookup of the session is the source of `missing_session`, before any argument is used.
        if matches!(name, "step" | "close" | "snapshot" | "find" | "read" | "links" | "act" | "downloads" | "eval" | "console" | "network" | "diagnose" | "screenshot" | "storage" | "save_state")
            && !self.live.contains_key(&session_name(&args))
        {
            return fail(Failure::no_session(&session_name(&args)), &red);
        }
        // The failure of a step that did not make the call an error: it is only in the structured part, from this call's own report.
        let mut step_failure = None;
        let done: Result<String, Failure> = match name {
            "run_steps" => self.run_steps(&args).map_err(Failure::from),
            "run_suite" => self.run_suite(&args).map_err(Failure::from),
            "open" => self.open(&args).map_err(Failure::from),
            "step" => self.step(&args).map(|(t, f)| { step_failure = f; t }),
            "save_state" => self.save_state(&args).map_err(Failure::from),
            "ensure_session" => self.ensure_session(&args).map_err(Failure::from),
            "close" => self.close(&args).map_err(Failure::from),
            "snapshot" => self.snapshot(&args).map_err(Failure::from),
            "find" => self.find(&args).map_err(Failure::from),
            "read" => self.read(&args).map_err(Failure::from),
            "links" => self.links(&args).map_err(Failure::from),
            "downloads" => self.downloads(&args).map_err(Failure::from),
            "eval" => self.eval(&args).map_err(Failure::from),
            "console" => self.console(&args).map_err(Failure::from),
            "network" => self.network(&args).map_err(Failure::from),
            "diagnose" => self.diagnose(&args).map_err(Failure::from),
            "sessions" => Ok(self.sessions()),
            "sessions_probe" => self.live_mut(&args).and_then(|l| l.session.collect()).and_then(|_| self.late(&args)).map_err(Failure::from),
            "storage" => self.storage(&args).map_err(Failure::from),
            "screenshot" => return self.screenshot(&args).unwrap_or_else(|e| fail(Failure::from(e), &red)),
            n => Err(Failure::unknown_tool(n)),
        };
        // A step can learn a new password. Use the redactor after execution for its evidence.
        let red = self.live.get(&session_name(&args)).map(|l| l.red.clone()).unwrap_or(red);
        match done {
            Ok(text) => match step_failure {
                Some(f) => json!({"content": [{"type": "text", "text": text}], "structuredContent": recovery::structured(&f, &red)}),
                None => json!({"content": [{"type": "text", "text": text}]}),
            },
            Err(f) => fail(f, &red),
        }
    }

    fn run_steps(&mut self, args: &Value) -> Result<String, String> {
        let src = args.get("steps").and_then(Value::as_str).ok_or("steps (the text of a step file) is required")?;
        let (opt, mut red, vars) = self.options(args)?;
        let list = steps::parse(src, &vars).map_err(|e| red.clean(&e))?;
        let report = bare_drive::run::run(&list, &opt, &mut red);
        Ok(with_out(report.json(&red), &opt, true))
    }

    /// Run a suite directory through the library entry point of `bare-drive --suite` (`suite::run_report`): the coordinator is a thread of this
    /// server. The workers are `bare-drive --suite-worker` processes: the runner starts a program for each worker, and this program is
    /// not one (its `current_exe` is `bare-mcp`), so it gets the path of `bare-drive` (the `BARE_DRIVE` variable, else the file next to this
    /// one). Own processes, own network client, own temp folder: the live sessions of this server keep their state (pages, cookies,
    /// saved sessions) and a suite does not read or write them. The call runs on a thread of its own, so the live pages and the
    /// control path go on meanwhile. A cancellation kills and reaps the workers of this suite only, and gives no reply.
    fn run_suite(&mut self, args: &Value) -> Result<String, String> {
        let dir = args.get("dir").and_then(Value::as_str).ok_or("dir (a suite folder: setup.steps and NAME.steps) is required")?;
        let workers = match args.get("workers") {
            None => None,
            Some(w) if w == "auto" => None,
            Some(w) => Some(w.as_u64().filter(|&n| n > 0).ok_or("workers: a number over 0, or \"auto\"")? as usize),
        };
        let num = |k: &str| args.get(k).and_then(Value::as_u64);
        let out = std::env::temp_dir().join(format!("bare-mcp-{}-{}", std::process::id(), self.shared.run_no()));
        let mut pass: Vec<String> = vec!["--out".into(), out.display().to_string(), "--var".into(), format!("STATE={}", out.join("state.json").display())];
        pass.extend(["--size".into(), format!("{}x{}", num("width").unwrap_or(1280), num("height").unwrap_or(800)), "--timeout".into(), num("timeout_ms").unwrap_or(10_000).to_string()]);
        if let Some(v) = args.get("vars").and_then(Value::as_object) {
            for (k, v) in v {
                pass.extend(["--var".into(), format!("{k}={}", v.as_str().ok_or(format!("vars.{k} must be a string"))?)]);
            }
        }
        // The values of a secrets file go to the workers as the file: they never get into an argument list.
        if let Some(f) = args.get("secrets_file").and_then(Value::as_str) {
            pass.extend(["--secrets".into(), f.into()]);
        }
        let exe = std::env::var_os("BARE_DRIVE").map(std::path::PathBuf::from).or_else(|| std::env::current_exe().ok()?.with_file_name(format!("bare-drive{}", std::env::consts::EXE_SUFFIX)).into()).filter(|p| p.is_file());
        let cfg = bare_drive::suite::Config {
            dir: dir.into(),
            out: out.clone(),
            workers,
            test_limit: Duration::from_millis(num("test_timeout_ms").unwrap_or(60_000)),
            perf: args.get("perf").and_then(Value::as_bool).unwrap_or(false),
            login_each: false,
            only: args.get("filter").and_then(Value::as_str).map(str::to_string),
            args: pass,
            cores: std::thread::available_parallelism().map_or(1, |n| n.get()),
            avail_kib: mem_available_kib(),
            quiet: true,
            exe: Some(exe.ok_or("bare-drive was not found next to bare-mcp: build it (cargo build -p bare-drive) or set BARE_DRIVE")?),
            cancel: Some(cancel_token()),
        };
        std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
        let report = bare_drive::suite::run_report(&cfg);
        let text = report.map(|r| suite_summary(&r, &out));
        // Captures are kept for a failure only; an empty or passed run leaves nothing behind.
        if !matches!(&text, Ok(t) if t.starts_with("FAILED")) {
            let _ = std::fs::remove_dir_all(&out);
        }
        text
    }

    fn open(&mut self, args: &Value) -> Result<String, String> {
        let url = args.get("url").and_then(Value::as_str).ok_or("url is required")?;
        let file = args.get("file").and_then(Value::as_str).filter(|f| std::path::Path::new(f).exists());
        let state = match args.get("state").and_then(Value::as_str) {
            Some(n) => match (lock(&self.shared.saved).get(n).cloned(), file) {
                (Some(st), _) => Some(st),
                (None, Some(f)) => Some(bare_drive::state::read(f)?),
                (None, None) => return Err(format!("no saved session named {n}")),
            },
            None => None,
        };
        // The JSON step report only on request: on a large site it has tens of thousands of characters (#398).
        if args.get("report").and_then(Value::as_bool) == Some(true) {
            let (rep, ok) = self.start(args, url, state)?;
            self.arm(args)?;
            return if ok { Ok(rep) } else { Err(rep) };
        }
        let mut a = args.clone();
        a["compact"] = json!(true);
        let (rep, ok) = self.start(&a, url, state)?;
        if !ok {
            return Err(rep);
        }
        let mark = self.arm(args)?;
        if args.get("snapshot").and_then(Value::as_bool) != Some(true) {
            return Ok(rep);
        }
        let snap = self.snapshot(args)?;
        let errs = self.script_errors(args, mark, &rep)?;
        Ok(rep + &errs + "\n" + &snap)
    }

    /// Start a live session on URL, from a saved session or with an empty profile. One live session at a time: this ends the old one
    /// (and closes its window). Gives the report of the open step and whether it passed.
    fn start(&mut self, args: &Value, url: &str, state: Option<StorageState>) -> Result<(String, bool), String> {
        let name = session_name(args);
        let _ = self.close(args);
        let (opt, red, vars) = self.options(args)?;
        // One client for the server: a script or a style sheet that an earlier `open` loaded is in its reply cache, and its connections stay
        // open. `fresh` gives this open a clean client. Cookies and storage are the document's own either way.
        let net = if args.get("fresh").and_then(Value::as_bool) == Some(true) { bare_net::http::Client::new() } else { lock(&self.shared.net).fork() };
        let mut session = Session::with_client(&opt, net);
        session.ready_wait = true;
        self.wake_on_done(&mut session);
        let dir = args.get("downloads_dir").and_then(Value::as_str).map_or_else(|| std::env::temp_dir().join(format!("bare-mcp-{}-downloads", std::process::id())), std::path::PathBuf::from);
        let dl = Downloads::new(dir);
        session.set_downloads(dl.clone());
        if let Some(st) = state {
            session.preload(st);
        }
        self.live.insert(name, Live { session, opt, red, vars, lines: 0, dl, told: Default::default() });
        let (mut rep, ok) = self.run_live(args, &format!("open {url}"))?;
        // A redirect to another address: the agent must see where the page really is.
        let end = self.live_mut(args)?.session.url().unwrap_or_default();
        let bare = |u: &str| u.split('#').next().unwrap_or("").trim_end_matches('/').to_string();
        if ok && bare(&end) != bare(url) {
            let to = self.live_mut(args)?.red.clean(&bare_drive::run::safe_url(&end));
            if args.get("compact").and_then(Value::as_bool) == Some(true) {
                rep.push_str(&format!("\nredirected: {to}"));
            } else {
                rep.insert_str(rep.len() - 1, &format!(",\"redirected\":{}", json!(to)));
            }
        }
        Ok((rep, ok))
    }

    /// Save the live session under a name, and in a file when asked. Gives counts only: the values never leave this process.
    fn save_state(&mut self, args: &Value) -> Result<String, String> {
        let name = args.get("name").and_then(Value::as_str).ok_or("name is required")?;
        self.store(args, name, args.get("file").and_then(Value::as_str))
    }

    fn store(&mut self, args: &Value, name: &str, file: Option<&str>) -> Result<String, String> {
        if cancelled() { return Err("cancelled".into()); }
        let live = self.live_mut(args)?;
        let st = live.session.export_state().ok_or("the page has no session")?;
        if let Some(f) = file {
            bare_drive::state::write(f, &st).map_err(|e| live.red.clean(&e))?;
        }
        let origins: std::collections::HashSet<&String> = st.local.iter().chain(&st.session).map(|(o, _)| o).collect();
        let reply = json!({"saved": name, "cookies": st.cookies.len(), "origins": origins.len()}).to_string();
        lock(&self.shared.saved).insert(name.to_string(), st);
        Ok(reply)
    }

    /// Like `cy.session`: restore the saved session and run `check`; when there is none or the check fails, log in again with an empty
    /// profile, run `check` and save. The live session stays open on the checked page.
    fn ensure_session(&mut self, args: &Value) -> Result<String, String> {
        let get = |k: &str| args.get(k).and_then(Value::as_str).ok_or(format!("{k} is required"));
        let (name, url, login, check) = (get("name")?, get("url")?, get("login")?, get("check")?);
        let file = args.get("file").and_then(Value::as_str);
        let saved = lock(&self.shared.saved).get(name).cloned();
        let found = match saved {
            Some(st) => Some(st),
            None => match file.filter(|f| std::path::Path::new(f).exists()) {
                Some(f) => Some(bare_drive::state::read(f)?),
                None => None,
            },
        };
        let had = found.is_some();
        if found.is_some() {
            self.start(args, url, found)?;
            let (rep, ok) = self.run_live(args, check)?;
            if ok {
                return Ok(with_session(rep, "restored"));
            }
        }
        // The open of a profile with no state, then the login and the check; a failure gives the report of the failing step.
        let (r, ok) = self.start(args, url, None)?;
        if !ok {
            return Err(r);
        }
        let mut rep = String::new();
        for src in [login, check] {
            let (r, ok) = self.run_live(args, src)?;
            if !ok {
                return Err(r);
            }
            rep = r;
        }
        self.store(args, name, file)?;
        Ok(with_session(rep, if had { "recreated" } else { "created" }))
    }

    fn step(&mut self, args: &Value) -> Result<(String, Option<Failure>), Failure> {
        let src = args.get("steps").and_then(Value::as_str).ok_or_else(|| Failure::args("steps (one or more step lines) is required"))?;
        let (text, _, failure) = self.run_live_typed(args, src)?;
        Ok((text, failure))
    }

    fn close(&mut self, args: &Value) -> Result<String, String> {
        let name = session_name(args);
        let live = self.live.remove(&name).ok_or(format!("no live session {name}: call open first"))?;
        Ok(with_out(live.session.finish().json(&live.red), &live.opt, true))
    }

    /// Run step lines on the live session until one fails, and give the report of these lines.
    fn run_live(&mut self, args: &Value, src: &str) -> Result<(String, bool), String> {
        self.run_live_typed(args, src).map(|(t, ok, _)| (t, ok)).map_err(String::from)
    }

    /// `run_live` with the failure of this run: the driver `kind` and message, taken from the report of these steps.
    fn run_live_typed(&mut self, args: &Value, src: &str) -> Result<(String, bool, Option<Failure>), Failure> {
        let live = self.live_mut(args)?;
        // A `timeout` line lasts only for this call: the next call starts from the default of the session, or from `timeout_ms`.
        let limit = args.get("timeout_ms").and_then(Value::as_u64).map_or(live.opt.timeout, Duration::from_millis);
        live.session.set_timeout(limit);
        let mut list = steps::parse(src, &live.vars).map_err(|e| Failure::args(live.red.clean(&e)))?;
        for s in &mut list {
            s.line += live.lines;
        }
        live.lines += src.lines().count();
        for s in &list {
            if cancelled() {
                live.session.drain();
                return Err("cancelled".into());
            }
            if !live.session.step(s, &live.opt, &mut live.red) {
                break;
            }
        }
        let failure = live.session.rep.failure.as_ref().map(|(_, f, _)| (f.kind, f.message.clone()));
        let ok = failure.is_none();
        let text = if args.get("compact").and_then(Value::as_bool) == Some(true) {
            compact(&live.session.drain(), &live.red)
        } else {
            with_out(live.session.drain().json(&live.red), &live.opt, false)
        };
        let failure = failure.map(|(k, m)| Failure::driver(k, m, text.clone()));
        Ok((text, ok, failure))
    }

    fn live_mut(&mut self, args: &Value) -> Result<&mut Live, String> {
        let name = session_name(args);
        self.live.get_mut(&name).ok_or(format!("no live session {name}: call open first"))
    }

    /// The live sessions. A session that is idle first gets its late tabs and dialogs as a call, so they are listed (and a tab becomes a
    /// session); a session that is busy is not asked and keeps its last address, so a slow call does not stall this one.
    fn sessions(&mut self) -> String {
        let sh = self.shared.clone();
        let names = |sh: &Shared| {
            let mut v: Vec<String> = lock(&sh.sessions).keys().cloned().collect();
            v.sort();
            v
        };
        if names(&sh).is_empty() {
            return "no live sessions".into();
        }
        let mut late = String::new();
        for n in names(&sh) {
            let idle = lock(&sh.sessions).get(&n).is_some_and(|s| s.busy.load(Ordering::SeqCst) == 0);
            if idle && let Some(Ok(text)) = sh.ask(&n, "sessions_probe") {
                late.push_str(&text);
            }
        }
        let list = names(&sh)
            .into_iter()
            .filter_map(|n| {
                let u = lock(&lock(&sh.sessions).get(&n)?.url).clone();
                Some(format!("{n}: {u}"))
            })
            .collect::<Vec<_>>()
            .join("\n");

        if late.is_empty() { list } else { format!("{}\n{list}", &late[1..]) }
    }

    fn max(args: &Value, d: usize) -> usize {
        args.get("max_chars").and_then(Value::as_u64).map_or(d, |n| n as usize)
    }

    fn snapshot(&mut self, args: &Value) -> Result<String, String> {
        let out = self.snapshot_diff(args, false)?;
        self.with_late(args, out)
    }

    /// Put the lines of `late` before the reply of a read tool.
    fn with_late(&mut self, args: &Value, out: String) -> Result<String, String> {
        let late = self.late(args)?;
        Ok(if late.is_empty() { out } else { format!("{}\n{out}", &late[1..]) })
    }

    /// Answer the dialogs that come between tool calls with dismiss, and record them and the tabs (`late` reports them). Gives the
    /// mark of the console history of the session.
    fn arm(&mut self, args: &Value) -> Result<u64, String> {
        let live = self.live_mut(args)?;
        live.session.set_dialog(false, None);
        Ok(live.session.hist.mark())
    }

    /// The dialogs and the tabs since the last reply, one line each, each after a newline. A tab becomes a session. Arms again.
    fn late(&mut self, args: &Value) -> Result<String, String> {
        let events = self.live_mut(args)?.session.events();
        self.arm(args)?;
        let mut out = String::new();
        for e in events {
            match e {
                Event::Dialog { kind, message, accepted, text } => {
                    let answer = if accepted { "accepted" } else { "dismissed" };
                    out.push_str(&format!("\ndialog: {kind} {message:?} -> {answer}{}", text.map(|t| format!(" {t:?}")).unwrap_or_default()));
                }
                Event::Tab(url) => match self.open_tab(&session_name(args), &url) {
                    Ok(name) => out.push_str(&format!("\nopened tab: {name} {}", bare_drive::run::safe_url(&url))),
                    Err(e) => out.push_str(&format!("\ntab not opened: {e}")),
                },
            }
        }
        Ok(self.live_mut(args)?.red.clean(&out))
    }

    /// Lines `script error: ...` for the uncaught errors that the console history got after `mark`, each after a newline. Leaves out
    /// the lines that `have` holds, and stops at 3 script error lines in all.
    fn script_errors(&mut self, args: &Value, mark: u64, have: &str) -> Result<String, String> {
        let live = self.live_mut(args)?;
        let mut room = 3usize.saturating_sub(have.matches("\nscript error: ").count() + usize::from(have.starts_with("script error: ")));
        let mut out = String::new();
        for line in live.session.hist.logs_since(mark).filter_map(script_error) {
            if room == 0 {
                break;
            }
            if !have.lines().chain(out.lines()).any(|l| l == line) {
                out.push('\n');
                out.push_str(&line);
                room -= 1;
            }
        }
        Ok(live.red.clean(&out))
    }

    /// With `diff`: only what changed since the last snapshot, and its parents.
    fn snapshot_diff(&mut self, args: &Value, diff: bool) -> Result<String, String> {
        if cancelled() { return Err("cancelled".into()); }
        // A `mode` in the call wins; else BARE_MCP_SNAPSHOT_MODE (read once) sets the default for the run.
        static DEFAULT_MODE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let all = *DEFAULT_MODE.get_or_init(|| std::env::var("BARE_MCP_SNAPSHOT_MODE").is_ok_and(|v| v == "all"));
        let mode = match args.get("mode").and_then(Value::as_str) {
            Some("all") => Mode::All,
            Some(_) => Mode::Interactive,
            None if all => Mode::All,
            None => Mode::Interactive,
        };
        let root = ref_arg(args, "root")?;
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let max = Self::max(args, 12_000);
        let live = self.live_mut(args)?;
        let out = live.session.with_refs(move |p, refs| snap::snapshot(p, refs, mode, root, offset, max, diff))??;
        Ok(live.red.clean(&out))
    }

    fn find(&mut self, args: &Value) -> Result<String, String> {
        let get = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        let exact = args.get("exact").and_then(Value::as_bool) == Some(true);
        let t = |v: String| if exact { Text::Is(v) } else { Text::Has(v) };
        let loc = if let Some(r) = get("role") {
            Locator::Role(r.to_ascii_lowercase(), get("name").map(t))
        } else if let Some(v) = get("label") {
            Locator::Label(t(v))
        } else if let Some(v) = get("text") {
            Locator::Text(t(v))
        } else if let Some(v) = get("placeholder") {
            Locator::Placeholder(t(v))
        } else if let Some(v) = get("css") {
            Locator::Css(v)
        } else {
            return Err("give one of role (with name), label, text, placeholder or css".into());
        };
        // With another locator, css is the part of the page to look in.
        let scope = get("css").filter(|_| !matches!(loc, Locator::Css(_)));
        let max = Self::max(args, 6_000);
        let live = self.live_mut(args)?;
        let out = live.session.with_refs(move |p, refs| snap::find(p, refs, &loc, scope.as_deref(), max))??;
        let out = live.red.clean(&out);
        self.with_late(args, out)
    }

    fn read(&mut self, args: &Value) -> Result<String, String> {
        let root = ref_arg(args, "ref")?;
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let max = Self::max(args, 8_000);
        let live = self.live_mut(args)?;
        let out = live.session.with_refs(move |p, refs| snap::read(p, refs, root, offset, max))??;
        let out = live.red.clean(&out);
        self.with_late(args, out)
    }

    fn links(&mut self, args: &Value) -> Result<String, String> {
        let max = Self::max(args, 8_000);
        let text = args.get("text").and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_string);
        let root = ref_arg(args, "root")?;
        let external = args.get("external").and_then(Value::as_bool).unwrap_or(false);
        let live = self.live_mut(args)?;
        let out = live.session.with_refs(move |p, refs| snap::links(p, refs, max, text.as_deref(), root, external))??;
        let out = live.red.clean(&out);
        self.with_late(args, out)
    }

    fn eval(&mut self, args: &Value) -> Result<String, String> {
        let code = args.get("js").and_then(Value::as_str).ok_or("js is required")?.to_string();
        let live = self.live_mut(args)?;
        let out = live.session.eval(code);
        out.map(|o| live.red.clean(&o)).map_err(|e| live.red.clean(&e))
    }

    /// The console entries of the live page: since the last call of this tool unless `since_last` is false.
    fn console(&mut self, args: &Value) -> Result<String, String> {
        let level = args.get("level").and_then(Value::as_str).unwrap_or("error");
        if !matches!(level, "error" | "warn" | "all") {
            return Err(format!("level: {level} is not error, warn or all"));
        }
        let (since, max) = (args.get("since_last").and_then(Value::as_bool).unwrap_or(true), args.get("max").and_then(Value::as_u64).unwrap_or(50) as usize);
        let live = self.live_mut(args)?;
        live.session.collect()?;
        let v = live.session.hist.console(level, since, max);
        Ok(live.red.clean(&if v.is_empty() { "no console entries".to_string() } else { v.join("\n") }))
    }

    /// The network records of the live page: the failed ones, since the last call of this tool, unless the arguments say otherwise.
    fn network(&mut self, args: &Value) -> Result<String, String> {
        let flag = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(true);
        let max = args.get("max").and_then(Value::as_u64).unwrap_or(50) as usize;
        let contains = args.get("url_contains").and_then(Value::as_str).map(str::to_string);
        let live = self.live_mut(args)?;
        live.session.collect()?;
        let mut v = live.session.hist.network(flag("failed_only"), contains.as_deref(), flag("since_last"), max);
        // The requests on their way: each call shows them again until they end, then their record shows once.
        let pending = live.session.pending()?;
        v.extend(pending.iter().filter(|w| !w.url.is_empty() && contains.as_deref().is_none_or(|c| w.url.contains(c))).take(max).map(|w| format!("pending {}", bare_drive::diag::waiting(w))));
        Ok(live.red.clean(&if v.is_empty() { "no requests".to_string() } else { v.join("\n") }))
    }

    /// A report of what is wrong with the page, to paste into a bare issue.
    fn diagnose(&mut self, args: &Value) -> Result<String, String> {
        let live = self.live_mut(args)?;
        let red = live.red.clone();
        live.session.diagnose(&red)
    }

    /// One action on an element or the page, as a step line; gives a short receipt and, unless `snapshot` is false, the new snapshot.
    fn act(&mut self, args: &Value) -> Value {
        let mut outcome = ActionOutcome { action_status: "not_started", page_status: "unknown", verification: if args.get("expect").is_some() { "not_run" } else { "not_requested" } };
        let result = known_args("act", args).map_err(Failure::args).and_then(|_| {
            if !self.live.contains_key(&session_name(args)) {
                return Err(Failure::no_session(&session_name(args)));
            }
            self.act_inner(args, &mut outcome)
        });
        if !cancelled() && outcome.action_status != "not_started"
            && let Ok(live) = self.live_mut(args) {
                outcome.page_status = live.session.with_refs(|p, _| {
                    if !p.loading() && p.inspect(|_, _| ()).is_some() { "ready" } else { "pending" }
                }).unwrap_or("unknown");
            }
        let error = result.is_err();
        let red = self.live_mut(args).map(|live| live.red.clone()).unwrap_or_default();
        let mut status = json!({"action_status": outcome.action_status, "page_status": outcome.page_status, "verification": outcome.verification});
        let text = match result {
            Ok(text) => text,
            Err(f) => {
                status["failure"] = recovery::structured(&f, &red)["failure"].clone();
                f.text
            }
        };
        let text = red.clean(&text);
        json!({"content": [{"type": "text", "text": text}, {"type": "text", "text": status.to_string()}], "structuredContent": status, "isError": error})
    }

    fn act_inner(&mut self, args: &Value, outcome: &mut ActionOutcome) -> Result<String, Failure> {
        // Validate every assertion before the operation can change the page.
        let expect = match args.get("expect") {
            None => None,
            Some(v) => {
                let src = v.as_str().ok_or_else(|| Failure::args("expect must be a string of expect steps"))?;
                let live = self.live_mut(args)?;
                let checks = steps::parse(src, &live.vars).map_err(|e| Failure::args(live.red.clean(&e)))?;
                if checks.is_empty() || checks.iter().any(|s| !matches!(&s.action, steps::Action::Expect(c) if !matches!(c, steps::Check::Js(_)))) {
                    return Err(Failure::args("expect needs one or more expect steps, without JavaScript"));
                }
                Some(src)
            }
        };

        let (line, accept) = (|| -> Result<(String, bool), String> {
        let op = args.get("op").and_then(Value::as_str).unwrap_or("click");
        let target = match (args.get("ref").and_then(Value::as_str), args.get("loc").and_then(Value::as_str)) {
            (Some(r), _) => format!("ref={}", r.trim()),
            (None, Some(l)) => l.to_string(),
            (None, None) => String::new(),
        };
        let text = args.get("text").and_then(Value::as_str).unwrap_or("");
        // `text` is for the ops that type or pick; on another op it is most likely the answer to a prompt, which is `prompt_text`.
        if args.get("text").is_some() && !matches!(op, "type" | "fill" | "select" | "press" | "wait") {
            return Err(format!("argument \"text\" is not used by op {op}: did you mean \"prompt_text\"?"));
        }
        let q = |t: &str| format!("{t:?}");
        let need = |what: &str| if target.is_empty() { Err(format!("op {what} needs ref (or loc)")) } else { Ok(()) };
        let line = match op {
            "click" => need("click").map(|_| format!("click {target}"))?,
            "type" | "fill" => need(op).map(|_| {
                let last = op == "fill" && args.get("last_key").and_then(Value::as_bool) == Some(true);
                format!("{op} {target} {}{}", q(text), if last { " keys=last" } else { "" })
            })?,
            "select" => need("select").and_then(|_| {
                let opts: Vec<&str> = args.get("options").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                let opts = if opts.is_empty() && !text.is_empty() { vec![text] } else { opts };
                if opts.is_empty() { return Err("op select needs text (the option label or value) or options".to_string()) }
                Ok(format!("select {target} {}", opts.iter().map(|o| q(o)).collect::<Vec<_>>().join(" ")))
            })?,
            "check" | "uncheck" | "hover" | "dblclick" => need(op).map(|_| format!("{op} {target}"))?,
            "upload" => need("upload").and_then(|_| {
                let files: Vec<&str> = args.get("files").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                if files.is_empty() { return Err("op upload needs files (an array of local paths)".to_string()) }
                Ok(format!("upload {target} {}", files.iter().map(|f| q(f)).collect::<Vec<_>>().join(" ")))
            })?,
            "press" => format!("press {}{}", if target.is_empty() { String::new() } else { format!("{target} ") }, args.get("key").and_then(Value::as_str).unwrap_or(if text.is_empty() { "Enter" } else { text })),
            "scroll" => {
                let dy = args.get("dy").and_then(Value::as_f64).unwrap_or(600.0);
                if target.is_empty() { format!("scroll page {dy}") } else { format!("scroll {target} {dy}") }
            }
            "goto" => format!("goto {}", args.get("url").and_then(Value::as_str).ok_or("op goto needs url")?),
            "reload" => "reload".into(),
            "back" => "back".into(),
            "forward" => "forward".into(),
            "wait" => need("wait").map(|_| format!("wait {target} {}", if text.is_empty() { "visible" } else { text }))?,
            o => return Err(format!("unknown op {o}: click, type, select, check, uncheck, hover, dblclick, upload, press, scroll, goto, reload, back, forward, wait")),
        };
        let accept = match args.get("dialog").and_then(Value::as_str) {
            None | Some("accept") => true,
            Some("dismiss") => false,
            Some(_) => return Err("dialog must be accept or dismiss".into()),
        };
        Ok((line, accept))
        })().map_err(Failure::args)?;
        let mut a = args.clone();
        a["compact"] = json!(true);
        let live = self.live_mut(args)?;
        live.session.set_dialog(accept, args.get("prompt_text").and_then(Value::as_str).map(str::to_string));
        let before = live.session.url();
        // A driver failure can happen after input was dispatched. Do not claim it is safe to retry.
        outcome.action_status = "unknown";
        let (receipt, ok, mut failure) = self.run_live_typed(&a, &line).inspect_err(|f| {
            // A step line that does not parse was rejected before any input.
            if f.input == "no" {
                outcome.action_status = "not_started";
            }
        })?;
        if ok {
            outcome.action_status = "completed";
        }
        // The cancelled request has no reply; do not enter another wait.
        if cancelled() {
            return Err("cancelled".into());
        }
        let mark = self.live_mut(args)?.session.hist.mark();
        let mut out = receipt;
        if ok
            && let Some(expect) = expect {
                outcome.verification = "unknown";
                let (receipt, verified, check_failure) = self.run_live_typed(&a, expect)?;
                failure = check_failure;
                outcome.verification = if verified { "passed" } else { "failed" };
                out.push_str("\nverification: ");
                out.push_str(&receipt);
            }
        if cancelled() {
            return Err("cancelled".into());
        }
        let snap = if ok && args.get("snapshot").and_then(Value::as_bool) != Some(false) {
            // The action ran: a page with no layout yet (a style sheet still on its way) does not make it a failure, or the agent acts twice.
            Some(self.snapshot_diff(args, args.get("diff").and_then(Value::as_bool) != Some(false)).unwrap_or_else(|e| format!("no snapshot: {e}: take one later")))
        } else {
            // No snapshot waits for the page, so let a started navigation finish before the URL check (#382). One tick starts a
            // navigation that the action queued (a link), or a request (a form that a script sends). Without either, a busy page (a
            // timer that always runs) would hold each action up to the 300 ms bound of `settle`.
            self.live_mut(args)?.session.with_refs(|p, _| {
                p.tick();
                if p.loading() || p.browser().is_some_and(|b| b.pending_fetches() > 0) {
                    snap::settle(p);
                }
            })?;
            None
        };
        // A download that started in the action ends first. The page needs ticks for that: a link starts its worker in a tick.
        out.push_str(&self.finish_downloads(args)?);
        // After the snapshot, which waits for the page: a click starts a navigation that ends later.
        let after = self.live_mut(args)?.session.url();
        if before.is_some() && after != before {
            out.push_str(&format!("\nnavigated: {}", bare_drive::run::safe_url(&after.unwrap_or_default())));
        }
        // The errors of the snapshot and the download wait; the receipt has those of the step.
        out.push_str(&self.script_errors(args, mark, &out)?);
        out.push_str(&self.late(args)?);
        let out = self.live_mut(args)?.red.clean(&out);
        let mut out = out;
        if let Some(s) = snap {
            out.push('\n');
            out.push_str(&s);
        }
        match failure {
            None => Ok(out),
            // The receipt stays the text of the error; the driver kind and message are the structured part.
            Some(f) => Err(Failure { text: out, ..f }),
        }
    }

    /// Wait up to the step time limit for the downloads that run, and for a page load that may be one. Then say what finished since the last
    /// call: `downloaded: PATH (BYTES bytes, MIME)`, or the failure.
    fn finish_downloads(&mut self, args: &Value) -> Result<String, String> {
        let live = self.live_mut(args)?;
        let end = std::time::Instant::now() + live.opt.timeout;
        while std::time::Instant::now() < end && !cancelled() {
            let loading = live.session.with_refs(|p, _| {
                p.tick();
                p.loading()
            })?;
            if !loading && live.dl.running() == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut out = String::new();
        for (i, d) in live.dl.list().into_iter().enumerate() {
            if matches!(d.state, DownloadState::Running | DownloadState::Queued | DownloadState::Paused) || !live.told.insert(i) {
                continue;
            }
            out.push('\n');
            out.push_str(&match &d.state {
                DownloadState::Failed(why) => format!("download failed: {}: {why}", bare_drive::run::safe_url(&d.url)),
                DownloadState::Cancelled => format!("download cancelled: {}", bare_drive::run::safe_url(&d.url)),
                _ => format!("downloaded: {} ({} bytes, {})", d.path.display(), d.bytes, d.mime),
            });
        }
        Ok(live.red.clean(&out))
    }

    /// The files that the session saved, with a number for each; or an operation on one of them: pause, resume, cancel, limit.
    fn downloads(&mut self, args: &Value) -> Result<String, String> {
        let live = self.live_mut(args)?;
        let op = args.get("op").and_then(Value::as_str).unwrap_or("list");
        if op != "list" {
            let n = args.get("id").and_then(Value::as_u64).filter(|&n| n >= 1).ok_or("give id: the number of the download in the list")?;
            let id = n as usize - 1;
            match op {
                "pause" => live.dl.pause(id)?,
                "resume" => live.dl.resume(id)?,
                "cancel" => live.dl.cancel(id)?,
                "limit" => live.dl.limit(id, args.get("bytes_per_second").and_then(Value::as_u64))?,
                _ => return Err(format!("unknown op {op}: use list, pause, resume, cancel or limit")),
            }
            return Ok(format!("ok: {op} #{n}"));
        }
        let list = live.dl.list();
        if list.is_empty() {
            return Ok(format!("no downloads (folder: {})", live.dl.dir().display()));
        }
        let lines: Vec<String> = list
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let url = bare_drive::run::safe_url(&d.url);
                let size = match d.total {
                    Some(t) => format!("{} of {t} bytes", d.bytes),
                    None => format!("{} bytes", d.bytes),
                };
                let line = match &d.state {
                    DownloadState::Queued => format!("queued: {url}"),
                    DownloadState::Running => format!("running: {url} ({size})"),
                    DownloadState::Paused => format!("paused: {url} ({size})"),
                    DownloadState::Failed(why) => format!("failed: {url}: {why}"),
                    DownloadState::Cancelled => format!("cancelled: {url}"),
                    DownloadState::Done => format!("{} ({} bytes, {}) from {url}", d.path.display(), d.bytes, d.mime),
                };
                format!("#{} {line}", i + 1)
            })
            .collect();
        Ok(live.red.clean(&lines.join("\n")))
    }

    /// A new live session `BASE~N` for a tab that the page of session `from` opened: the same size, secrets and variables, and a copy of
    /// its cookies and storage. The load starts at once, on the thread of the new session, and this call waits for its first result.
    /// Gives the name.
    fn open_tab(&mut self, from: &str, url: &str) -> Result<String, String> {
        let base = from.split('~').next().unwrap_or(from);
        let l = self.live.get_mut(from).ok_or("the opener is gone")?;
        let (size, timeout, window, red, vars, state) = (l.opt.size, l.opt.timeout, l.opt.window, l.red.clone(), l.vars.clone(), l.session.export_state());
        let (mut opt, _, _) = self.options(&json!({"width": size.0, "height": size.1, "timeout_ms": timeout.as_millis() as u64, "window": window}))?;
        opt.progress = false;
        let (done, wait) = mpsc::channel();
        let name = self.shared.start_tab(base, Tab { opt, red, vars, state, url: url.to_string(), done })?;
        loop {
            if cancelled() {
                return Err("cancelled".into());
            }
            match wait.recv_timeout(Duration::from_millis(25)) {
                Ok(Ok(())) => return Ok(name),
                Ok(Err(e)) => return Err(e),
                Err(RecvTimeoutError::Timeout) => {}
                Err(_) => return Err("the tab thread ended".into()),
            }
        }
    }

    /// The first load of a tab, on its own thread. Reports the result to the opener. A failed load leaves no session.
    fn begin_tab(&mut self, name: &str, t: Tab) {
        let Tab { opt, red, vars, state, url, done } = t;
        let mut session = Session::new(&opt);
        if let Some(st) = state {
            session.preload(st);
        }
        session.ready_wait = true;
        self.wake_on_done(&mut session);
        let dl = Downloads::new(std::env::temp_dir().join(format!("bare-mcp-{}-downloads", std::process::id())));
        session.set_downloads(dl.clone());
        self.live.insert(name.to_string(), Live { session, opt, red, vars, lines: 0, dl, told: Default::default() });
        let args = json!({"session": name, "compact": true});
        let r = match self.run_live(&args, &format!("open {url}")) {
            Ok((_, true)) => self.arm(&args).map(|_| ()),
            Ok((r, false)) | Err(r) => {
                self.live.remove(name);
                Err(r)
            }
        };
        let _ = done.send(r);
    }

    /// Read, set or clear the cookies, `localStorage` or `sessionStorage` of a session. Values go through the redactor.
    fn storage(&mut self, args: &Value) -> Result<String, String> {
        let get = |k: &str| args.get(k).and_then(Value::as_str);
        let (area, key, action) = (get("area").unwrap_or("local"), get("key"), get("action").unwrap_or("get"));
        if !["local", "session", "cookies"].contains(&area) {
            return Err("area must be local, session or cookies".into());
        }
        if !["get", "set", "clear"].contains(&action) {
            return Err("action must be get, set or clear".into());
        }
        let cap = |v: &str| if v.chars().count() > 300 { format!("{}...", v.chars().take(300).collect::<String>()) } else { v.to_string() };
        let live = self.live_mut(args)?;
        let out = match (area, action) {
            ("cookies", "get") => {
                let st = live.session.export_state().ok_or("the page has no session")?;
                let list: Vec<String> = st.cookies.iter().filter(|c| key.is_none_or(|k| k == c.name)).map(|c| format!("{}={}  {}{}{}{}", c.name, cap(&c.value), c.domain, c.path, if c.http_only { "; httponly" } else { "" }, if c.secure { "; secure" } else { "" })).collect();
                if list.is_empty() { "no cookies".into() } else { list.join("\n") }
            }
            ("cookies", "set") => {
                let (k, v) = (key.ok_or("set needs key")?, get("value").ok_or("set needs value")?);
                if k.is_empty() || k.contains(|c: char| c.is_whitespace() || c == '=' || c == '"') {
                    return Err("cookie key must be a name without spaces, = or quotes".into());
                }
                let (rep, ok) = self.run_live(&json!({"session": session_name(args), "compact": true}), &format!("cookie set {k} {v:?}"))?;
                if !ok {
                    return Err(rep);
                }
                format!("cookie {k} set")
            }
            ("cookies", _) => format!("removed {} cookies", live.session.clear_cookies(key.map(str::to_string))?),
            (_, "get") => {
                let js = format!("(() => {{ const s = window.{area}Storage, o = []; for (let i = 0; i < s.length; i++) {{ const k = s.key(i); o.push([k, s.getItem(k)]); }} return JSON.stringify(o); }})()");
                let raw = live.session.eval(js)?;
                let raw = serde_json::from_str::<String>(&raw).unwrap_or(raw);
                let pairs: Vec<(String, String)> = serde_json::from_str(&raw).map_err(|e| format!("storage read: {e}"))?;
                let list: Vec<String> = pairs.iter().filter(|(k, _)| key.is_none_or(|w| w == k)).map(|(k, v)| format!("{k}={}", cap(v))).collect();
                if list.is_empty() { "empty".into() } else { list.join("\n") }
            }
            (_, "set") => {
                let (k, v) = (key.ok_or("set needs key")?, get("value").ok_or("set needs value")?);
                live.session.eval(format!("window.{area}Storage.setItem({}, {})", json!(k), json!(v)))?;
                format!("{area} {k} set")
            }
            (_, _) => {
                live.session.eval(match key {
                    Some(k) => format!("window.{area}Storage.removeItem({})", json!(k)),
                    None => format!("window.{area}Storage.clear()"),
                })?;
                format!("{area} cleared")
            }
        };
        Ok(self.live_mut(args)?.red.clean(&out))
    }

    /// A PNG of the page or of one element: its path, and the picture itself when `inline` is true.
    fn screenshot(&mut self, args: &Value) -> Result<Value, String> {
        let r = ref_arg(args, "ref")?;
        let full = args.get("full_page").and_then(Value::as_bool) == Some(true);
        let inline = args.get("inline").and_then(Value::as_bool) == Some(true);
        let run = self.shared.run_no();
        let dir = std::env::temp_dir().join(format!("bare-mcp-{}-shots", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = match args.get("path").and_then(Value::as_str) {
            Some(p) => std::path::PathBuf::from(p),
            None => dir.join(format!("shot-{run}.png")),
        };
        let live = self.live_mut(args)?;
        let red = live.red.clone();
        let said = live.session.screenshot(path.clone(), r, full, &red)?;
        let said = self.with_late(args, said)?;
        let mut content = vec![json!({"type": "text", "text": said})];
        if inline {
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            content.push(json!({"type": "image", "data": base64(&bytes), "mimeType": "image/png"}));
        }
        Ok(json!({"content": content}))
    }

    /// The driver options of a call, its secrets and its variables. Each call gets its own capture folder.
    fn options(&mut self, args: &Value) -> Result<(Options, Redactor, HashMap<String, String>), String> {
        let num = |k: &str| args.get(k).and_then(Value::as_u64);
        let size = (num("width").unwrap_or(1280) as u32, num("height").unwrap_or(800) as u32);
        if size.0 == 0 || size.1 == 0 {
            return Err("width and height must be over 0".into());
        }
        let out = std::env::temp_dir().join(format!("bare-mcp-{}-{}", std::process::id(), self.shared.run_no()));
        std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
        let opt = Options {
            size,
            timeout: Duration::from_millis(num("timeout_ms").unwrap_or(10_000)),
            out: Some(out),
            window: args.get("window").and_then(Value::as_bool).unwrap_or(false),
            progress: false,
        };
        let mut vars = HashMap::new();
        if let Some(v) = args.get("vars").and_then(Value::as_object) {
            for (k, v) in v {
                vars.insert(k.clone(), v.as_str().ok_or(format!("vars.{k} must be a string"))?.to_string());
            }
        }
        // Secrets come from a file, as for `bare-drive --secrets`, so their values are never in the arguments that the agent logs.
        let mut red = Redactor::default();
        if let Some(path) = args.get("secrets_file").and_then(Value::as_str) {
            let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
            for line in text.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')) {
                let (k, v) = line.split_once('=').ok_or(format!("{path}: a line is not NAME=value"))?;
                red.add(v);
                vars.insert(k.trim().to_string(), v.to_string());
            }
        }
        Ok((opt, red, vars))
    }
}

/// The `initialize` text: how to use the tools. The host that runs this server gives its own rules, such as where to report problems.
fn instructions() -> &'static str {
    "Loop: open a URL (a live session; session=NAME runs more beside main); snapshot or find to get refs; act with a fresh ref or a loc such as label=\"Email\"; \
        then check evidence on the page (step expect, read, links, a screenshot), never the click receipt alone. On a failure read the error and the page before you retry a side effect. \
        Refs end at navigation; act type replaces a field's text; a dialog that act opens is accepted unless dialog=dismiss; a tab the page opens is session NAME~2. \
        Use eval to read, not to click or type. Call ensure_session only for a logged-in journey: it restores a saved login (save_state name) or logs in again; a public page needs only open."
}

/// Add the capture folder to a report, so the agent can read the pictures of `shot` steps and failures. At the `end` of a run an empty
/// folder is removed (`remove_dir` fails on a folder with files), so a server that runs for days leaves only folders with pictures.
fn with_out(mut json: String, opt: &Options, end: bool) -> String {
    if let Some(dir) = &opt.out
        && !(end && std::fs::remove_dir(dir).is_ok())
    {
        json.insert_str(json.len() - 1, &format!(",\"out\":{}", Value::from(dir.display().to_string())));
    }
    json
}

/// The tool list, built once on the first call and kept: like a module-level `const` that is computed lazily in JS.
fn tool_list() -> &'static Value {
    static TOOLS: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    TOOLS.get_or_init(tools)
}

/// The token of the request that this thread runs, for a worker that this call starts (`run_suite`).
fn cancel_token() -> Arc<AtomicBool> {
    bare_drive::run::current_cancel().unwrap_or_default()
}

/// Refuse an argument that the input schema of tool `name` does not list, and name the closest valid key: a key with a typo would
/// else be dropped without a word. An unknown tool passes: `call` says so.
fn known_args(name: &str, args: &Value) -> Result<(), String> {
    let tools = tool_list();
    let Some(tool) = tools.as_array().and_then(|t| t.iter().find(|t| t["name"] == name)) else { return Ok(()) };
    let valid: Vec<&String> = tool["inputSchema"]["properties"].as_object().map(|p| p.keys().collect()).unwrap_or_default();
    let Some(key) = args.as_object().and_then(|a| a.keys().find(|k| !valid.contains(k))) else { return Ok(()) };
    let close = valid.iter().map(|v| (edits(key, v), *v)).min();
    Err(match close {
        Some((d, v)) if d <= 2.max(key.chars().count() / 3) => format!("unknown argument {key:?}: did you mean {v:?}?"),
        Some(_) => format!("unknown argument {key:?}: {name} takes {}", valid.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(", ")),
        None => format!("unknown argument {key:?}: {name} takes no arguments"),
    })
}

/// The edit distance of two texts: how many characters to insert, delete or change to turn `a` into `b`.
fn edits(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    // One row of the table at a time: `row[j]` is the distance of the part of `a` so far and the first `j` characters of `b`.
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for j in 0..b.len() {
            let here = (prev + usize::from(ca != b[j])).min(row[j] + 1).min(row[j + 1] + 1);
            prev = row[j + 1];
            row[j + 1] = here;
        }
    }
    row[b.len()]
}

/// The session name of a call: `session`, default `main`.
fn session_name(args: &Value) -> String {
    args.get("session").and_then(Value::as_str).unwrap_or("main").to_string()
}

/// A ref argument: `e7` (or `7`).
fn ref_arg(args: &Value, k: &str) -> Result<Option<u32>, String> {
    match args.get(k).and_then(Value::as_str) {
        None => Ok(None),
        Some(r) => r.trim().trim_start_matches('e').parse().map(Some).map_err(|_| format!("{k}: not a ref: {r} (use e7)")),
    }
}

/// A short receipt of step lines for an agent: ok or the failure with a hint, then new console errors and failed requests.
/// `MemAvailable` in KiB, or 0 when `/proc/meminfo` is not there (the worker rule then has no memory limit).
fn mem_available_kib() -> u64 {
    let s = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix("MemAvailable:")?.trim().strip_suffix("kB")?.trim().parse().ok()).unwrap_or(0)
}

/// The suite report as a few lines: counts, time, logins, each failure with its step and message, the peak RSS of each worker.
/// At most 20 failures and 3000 characters, like the other tools keep their replies short; the full report is `out/suite.json`.
fn suite_summary(r: &Value, out: &std::path::Path) -> String {
    let n = |k: &str| r[k].as_u64().unwrap_or(0);
    let mut s = format!("{} {}/{} passed, {} workers, {:.0} ms", if r["ok"] == true { "ok" } else { "FAILED" }, n("passed"), n("tests"), n("workers"), r["wall_ms"].as_f64().unwrap_or(0.0));
    s.push_str(&format!(", logins {}", n("logins_expected")));
    if let Some(i) = r["instructions"].as_u64() {
        s.push_str(&format!(", {:.0} M instructions", i as f64 / 1e6));
    }
    if r["setup"]["ok"] == false {
        let f = &r["setup"]["failure"];
        s.push_str(&format!("\nsetup FAILED at line {} {}: {}", f["line"], f["kind"].as_str().unwrap_or(""), f["message"].as_str().unwrap_or("")));
    }
    let rows = r["results"].as_array().map(Vec::as_slice).unwrap_or_default();
    let bad: Vec<&Value> = rows.iter().filter(|x| x["ok"] != true).collect();
    for x in bad.iter().take(20) {
        let f = &x["failure"];
        s.push_str(&format!("\nFAIL {} (step line {}, {}): {}", x["test"].as_str().unwrap_or(""), f["line"], f["kind"].as_str().unwrap_or(""), f["message"].as_str().unwrap_or("").chars().take(300).collect::<String>()));
    }
    if bad.len() > 20 {
        s.push_str(&format!("\n... and {} more failures", bad.len() - 20));
    }
    let peaks: Vec<String> = r["worker_stats"].as_array().map(Vec::as_slice).unwrap_or_default().iter().map(|w| format!("w{}:{}M", w["worker"], w["peak_kib"].as_u64().unwrap_or(0) >> 10)).collect();
    s.push_str(&format!("\npeak RSS {}", peaks.join(" ")));
    if !bad.is_empty() || r["ok"] != true {
        s.push_str(&format!("\nout: {}", out.display()));
    }
    s.chars().take(3000).collect()
}

fn compact(rep: &Report, red: &Redactor) -> String {
    let ms: f64 = rep.steps.iter().map(|s| s.ms).sum();
    let mut out = match &rep.failure {
        None => {
            let notes: Vec<&str> = rep.steps.iter().filter_map(|s| s.note.as_deref()).collect();
            format!("ok ({ms:.0} ms){}", if notes.is_empty() { String::new() } else { format!(": {}", notes.join("; ")) })
        }
        Some((_, f, _)) => {
            let hint = match f.kind {
                "not_found" => "take a snapshot: the element may not exist yet, or has another name",
                "stale_ref" | "detached" => "take a new snapshot and use its refs",
                "hidden" => "open the menu, tab or section that holds it, or scroll",
                "covered" => "close the dialog or overlay first (press Escape), or act on the covering element",
                "disabled" => "fill the required fields first, or wait",
                "duplicate" => "add nth=0 (the first match) to the locator, or use a ref from snapshot or find",
                _ => "",
            };
            let script = rep.logs.iter().find(|(_, l)| bare_drive::diag::uncaught(l)).filter(|_| matches!(f.kind, "not_found" | "hidden" | "page"));
            let hint = match script {
                Some((_, l)) => format!("\nhint: the page script failed: {}", l.text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect::<String>()),
                // The part match of the failed locator: its first `="` (a role is not quoted, so this is the name of `role=x name="y"`).
                None if f.message.contains("; near: ") => match f.message.strip_prefix("no element matches ").and_then(|m| m.split_once("; near: ")) {
                    Some((loc, _)) => format!("\nhint: {} matches a part, in any case", loc.replacen("=\"", "~\"", 1)),
                    None => "\nhint: use ~ instead of = to match a part, in any case".into(),
                },
                None if hint.is_empty() => String::new(),
                None => format!("\nhint: {hint}"),
            };
            format!("FAILED {}: {}{hint}", f.kind, f.message)
        }
    };
    let mut scripts: Vec<String> = Vec::new();
    for s in rep.logs.iter().filter_map(|(_, l)| script_error(l)) {
        if scripts.len() < 3 && !scripts.contains(&s) {
            scripts.push(s);
        }
    }
    let errs: Vec<String> = rep.logs.iter().filter(|(_, l)| matches!(l.level, "error" | "warn") && script_error(l).is_none()).take(5).map(|(_, l)| format!("console {}: {}", l.level, l.text.chars().take(200).collect::<String>())).collect();
    let bad: Vec<String> = rep.net.iter().filter(|(_, r)| r.error.is_some() || r.status >= 400).take(5).map(|(_, r)| format!("request {}: {}", r.error.clone().unwrap_or_else(|| r.status.to_string()), bare_drive::run::safe_url(&r.url))).collect();
    for l in scripts.iter().chain(&errs).chain(&bad) {
        out.push('\n');
        out.push_str(l);
    }
    red.clean(&out)
}

/// An uncaught exception or unhandled rejection of a page script as one line of at most about 200 characters:
/// `script error: MESSAGE (SOURCE:LINE)` when the text names its first stack frame. Other entries give `None`.
fn script_error(l: &bare_drive::diag::Log) -> Option<String> {
    let t = match l.level {
        "exception" => l.text.strip_prefix("listener of ").and_then(|t| t.split_once(": ")).map_or(l.text.as_str(), |(_, m)| m),
        "error" => l.text.strip_prefix("Uncaught ")?,
        _ => return None,
    };
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    // The first stack frame with a line, `at f (src:line:col)` or `at src:line:col`, becomes `(src:line)`; else the first frame.
    let (msg, at) = match t.split_once(" at ") {
        Some((m, frames)) => {
            let places: Vec<&str> = frames.split(" at ").map(|f| f.rsplit_once('(').map_or(f, |(_, p)| p).trim_end_matches(')')).collect();
            let place = places.iter().find_map(|p| p.rsplit_once(':').filter(|(a, c)| c.parse::<u32>().is_ok() && a.rsplit_once(':').is_some_and(|(_, n)| n.parse::<u32>().is_ok())).map(|(a, _)| a)).unwrap_or(places[0]);
            (m.to_string(), format!(" ({place})"))
        }
        None => (t, String::new()),
    };
    let msg: String = msg.chars().take(200usize.saturating_sub(at.chars().count().min(100))).collect();
    let at: String = at.chars().take(100).collect();
    Some(format!("script error: {msg}{at}"))
}

/// Standard base64, for an image in a tool result.
fn base64(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            s.push(if i <= c.len() { A[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    s
}

/// Add the way the session was made to a report, as `with_out` adds `out`.
fn with_session(mut json: String, how: &str) -> String {
    json.insert_str(json.len() - 1, &format!(",\"session\":\"{how}\""));
    json
}

fn tools() -> Value {
    let common = json!({
        "vars": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Values for ${NAME} in the steps."},
        "secrets_file": {"type": "string", "description": "A file of NAME=value lines. Each value is a variable and is [secret] in all output."},
        "window": {"type": "boolean", "description": "Show the page in a Wayland window. Default: headless."},
        "width": {"type": "integer", "description": "Page width in pixels. Default 1280."},
        "height": {"type": "integer", "description": "Page height in pixels. Default 800."},
        "timeout_ms": {"type": "integer", "description": "Time limit of each step. Default 10000."},
        "session": {"type": "string", "description": "Live session name. Default main. Sessions run side by side."},
        "downloads_dir": {"type": "string", "description": "open: the folder for files that the page downloads. Default: a temp folder of this server."},
    });
    let with = |extra: Value, required: &[&str]| {
        let mut p = common.clone();
        p.as_object_mut().expect("object").extend(extra.as_object().expect("object").clone());
        json!({"type": "object", "properties": p, "required": required})
    };
    #[allow(non_snake_case)]
    let S = json!({"type": "string", "description": "Live session name. Default main."});
    let syntax = "Step lines (# starts a comment): open URL, goto URL, reload, click LOC, type LOC \"text\" (one key at a time, one page render per key: use fill for long text; \"\" clears), \
        fill LOC \"text\" [keys=last] (the whole text as one edit, no keys: fast on heavy forms; keys=last types the last character as a key; \"\" clears), \
        select LOC \"label or value\" [\"more\" ...], check LOC, uncheck LOC, hover LOC, dblclick LOC, upload LOC \"path\" [\"path2\" ...], \
        press [LOC] KEY (Enter, Tab, Shift+Tab, Control+a), scroll LOC|page DY, \
        expect LOC visible|hidden|enabled|disabled|checked|unchecked|focused|count N|text=\"x\"|value~\"x\", \
        expect url=\"x\"|url~\"x\", expect title=\"x\"|title~\"x\", expect js \"expression\", expect cookie NAME [absent|value=\"x\"], \
        cookie set NAME VALUE, shot NAME (a PNG in the out folder), timeout MS. \
        LOC: ref=e7 (from snapshot), css=\"...\", role=NAME [name=\"...\"|name~\"...\"], label=\"...\", text=\"...\", placeholder=\"...\"; it must match one element: add nth=N to pick rendered match N (from 0). \
        A step waits for its condition until its time limit; there are no fixed sleeps.";
    json!([
        {
            "name": "run_steps",
            "description": format!("Run a whole end-to-end journey in the bare browser and return a JSON report: each step, the failure with a capture, console logs and network requests. The first step must be open. {syntax}"),
            "inputSchema": with(json!({"steps": {"type": "string", "description": "The step file text, one step per line. # starts a comment."}}), &["steps"]),
        },
        {
            "name": "run_suite",
            "description": "Run a suite of step files that share one login, in K warm worker processes (like bare-drive --suite). The folder holds setup.steps (the login; its last step is save-state \"${STATE}\") and one NAME.steps for each test; a test starts with open URL state=\"${STATE}\". A test that signs out, changes a password or deletes logs in itself. Gives a short summary: passed and failed counts, wall time, logins, each failure with its test, step line and message, and the peak RSS of each worker. Your live sessions are not used or changed. The call returns when the suite ends.",
            "inputSchema": with(json!({
                "dir": {"type": "string", "description": "The suite folder."},
                "workers": {"description": "A number of worker processes, or \"auto\" (default): the smaller of cores - 4, half of the free memory over one worker's peak, and the number of tests.", "oneOf": [{"type": "integer"}, {"type": "string", "enum": ["auto"]}]},
                "filter": {"type": "string", "description": "Run only the tests whose name holds this text."},
                "perf": {"type": "boolean", "description": "Count the instructions of each worker (needs perf). Default false."},
                "test_timeout_ms": {"type": "integer", "description": "Time limit of one whole test; a test past it fails and its worker is replaced. Default 60000."},
            }), &["dir"]),
        },
        {
            "name": "open",
            "description": "Start a live session: load URL in the bare browser. A new open ends the old session; it keeps the HTTP cache and the connections of the earlier opens unless fresh is true. Returns a short receipt: ok or the failure, the time, a redirect, not ready, script errors and failed requests.",
            "inputSchema": with(json!({"url": {"type": "string"}, "state": {"type": "string", "description": "Start from the session that save_state or ensure_session saved under this name, in this server."}, "file": {"type": "string", "description": "With state: read the session from this file when it is not in memory. It holds live credentials."}, "snapshot": {"type": "boolean", "description": "Add the page snapshot after the receipt."}, "report": {"type": "boolean", "description": "Return the full JSON report of the open step (steps, console, network) instead of the receipt. It can be large."}, "fresh": {"type": "boolean", "description": "Use a clean network client for this open: no cached replies and no open connections from earlier opens. The default shares one client between opens, so a script or style sheet that an earlier open loaded (with a fresh Cache-Control) is not fetched again. Cookies and storage are always the page's own."}}), &["url"]),
        },
        {
            "name": "step",
            "description": format!("Run one or more step lines on the live session page, until one fails. Returns the report of these steps only. The session stays open after a failure. {syntax}"),
            "inputSchema": {"type": "object", "properties": {"steps": {"type": "string"}, "session": S, "compact": {"type": "boolean", "description": "A short receipt instead of the JSON report."}, "timeout_ms": {"type": "integer", "description": "Time limit of each step of this call. Default: the timeout_ms of open. A `timeout` line in the steps changes it only until the end of this call."}}, "required": ["steps"]},
        },
        {
            "name": "save_state",
            "description": "Save the cookies, localStorage and sessionStorage of the live session in memory under a name, and in a file (mode 0600) when file is given. Returns counts only, never values. Open it again with open state=name.",
            "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "session": S, "file": {"type": "string", "description": "Also write the session to this file. It holds live credentials."}}, "required": ["name"]},
        },
        {
            "name": "ensure_session",
            "description": "Get a logged-in live page, like cy.session. Restores the session saved under name (from memory, else from file), opens url and runs the check steps. If there is none or the check fails, opens url with an empty profile, runs the login steps, runs the check, and saves the session. The live page stays open: go on with step. Returns the report of the check with session: restored, created or recreated. login and check are step lines (see step); use ${NAME} with secrets_file for the credentials.",
            "inputSchema": with(json!({
                "name": {"type": "string"},
                "url": {"type": "string", "description": "The page to open, where the check and the login steps start."},
                "login": {"type": "string", "description": "Step lines that log in (without open)."},
                "check": {"type": "string", "description": "Step lines that pass only when the session is valid, such as expect on a logged-in element."},
                "file": {"type": "string", "description": "Also read the session from this file when it is not in memory, and write it there when saved. It holds live credentials."},
            }), &["name", "url", "login", "check"]),
        },
        {
            "name": "close",
            "description": "End the live session, close its window, and return the cookie names and flags (never values).",
            "inputSchema": {"type": "object", "properties": {"session": S}},
        },
        {
            "name": "snapshot",
            "description": "Dialogs and tabs that the page opened after the last reply (from a timer, say) show first, one line each: dialog: confirm \"...\" -> dismissed (such a dialog is always dismissed), or opened tab: main~2 URL. This holds for snapshot, act, find, read, links and screenshot. Read the live page as text: the accessibility outline, one element per line, indented by nesting: [e7] role \"name\" = \"value\" (states). Act on an element with its ref (act ref=e7, or ref=e7 in step lines). * marks an element new or changed since the last snapshot. A link line shows its href only when it has no name: links lists them all. Refs stay valid until the page navigates, and no other page or session gets the same ref. The reply names the document and revision (doc: d3 rev 5); an act diff lists the refs that left the outline (removed: e4, e9), and says reset: when it is the full outline because it has no baseline. (offscreen) marks a control other than a link outside the window: act click scrolls it into view. A table row of plain cells is one line with no ref of its own: row: 1 | $3.99 | [e5]. An iframe shows as one line: its content is not loaded, so it is not in the snapshot. Use this instead of a screenshot.",
            "inputSchema": {"type": "object", "properties": {"session": S, "mode": {"type": "string", "enum": ["interactive", "all"], "description": "interactive (default): controls, headings, landmarks, dialogs, alerts. all: also the text between them."}, "root": {"type": "string", "description": "A ref: the outline of that element only."}, "offset": {"type": "integer", "description": "Skip this many outline lines (the cut line gives the next offset)."}, "max_chars": {"type": "integer", "description": "Default 12000. A longer outline keeps 80% from the top and 20% from the end, and a line says which refs were cut."}}},
        },
        {
            "name": "act",
            "outputSchema": {"type": "object", "properties": {
                "action_status": {"type": "string", "enum": ["not_started", "completed", "unknown"], "description": "Completed means the operation succeeded, not that its business result is verified. Unknown may have dispatched input: inspect before retrying."},
                "page_status": {"type": "string", "enum": ["ready", "pending", "unknown"], "description": "Ready means a layout exists and document loading ended; application data may still be pending."},
                "verification": {"type": "string", "enum": ["not_requested", "passed", "failed", "not_run", "unknown"]},
                "failure": {"type": "object", "properties": {"code": {"type": "string"}, "message": {"type": "string"}, "next_action": {"type": "string"}, "input_dispatched": {"type": "string", "enum": ["no", "unknown"]}}, "required": ["code", "message", "next_action", "input_dispatched"]}
            }, "required": ["action_status", "page_status", "verification"]},
            "description": "Act on the live page like a user and get a short receipt (ok, or the failure with a hint, up to 3 script error lines for uncaught errors, new console errors and failed requests, navigation, dialogs and the answer given, tabs the page opened: \"opened tab: main~2 URL\" is a new session that you read with session=main~2 and end with close), then what changed on the page (only the lines marked *, with the lines that hold them; the whole outline after a navigation or with diff false). Ops: click, fill (clears the field, then inserts text as one edit with no key events, like Playwright fill: one render on a heavy React form; use it for plain fields; last_key true types the last character as a key, for autocomplete), type (clears the field, then types text one key at a time; \\n is Enter; for fields that need keys: autocomplete, masks; each key runs the page handlers, so on a heavy form a long text costs one render per character: use fill for long text), press (key: Enter, Escape, Tab, Control+a; with ref it acts on that element, and Enter in a form field submits the form), scroll (dy pixels, over ref or the page), goto (url), reload, back, forward (the history), wait (ref until text state: visible, hidden, enabled, checked, text~\"x\"), select (text = the option label or value; options = several for a multiple select; fires input and change), check and uncheck (a checkbox, a radio, or an ARIA role=switch, checkbox or radio with aria-checked; clicks only when the state differs), hover (pointer over the element: :hover and mouseenter), dblclick, upload (files = local paths for an <input type=file>, at most 100 MiB each; a hidden input needs loc css=\"input[type=file]\"). The action waits for the element up to the timeout_ms that open set (act takes no timeout_ms).",
            "inputSchema": {"type": "object", "properties": {"session": S, "expect": {"type": "string", "description": "Optional postconditions: one or more expect step lines (no JavaScript). Validated before the action. A failed check keeps action_status completed: inspect before retrying."}, "op": {"type": "string", "enum": ["click", "fill", "type", "select", "check", "uncheck", "hover", "dblclick", "upload", "press", "scroll", "goto", "reload", "back", "forward", "wait"]}, "options": {"type": "array", "items": {"type": "string"}, "description": "For select: several options of a multiple select."}, "files": {"type": "array", "items": {"type": "string"}, "description": "For upload: local file paths."}, "dialog": {"type": "string", "enum": ["accept", "dismiss"], "description": "How to answer alert, confirm and prompt dialogs that the action opens. Default accept. The receipt lists each one."}, "prompt_text": {"type": "string", "description": "The answer of an accepted prompt. Default: the default value of the prompt."}, "ref": {"type": "string", "description": "e7 from snapshot or find."}, "loc": {"type": "string", "description": "A step locator instead of ref: role=button name=\"Save\", label=\"Email\", text=\"x\", css=\"...\". Add nth=N to pick rendered match N (from 0) when several match."}, "text": {"type": "string", "description": "For type and fill. A date or time field takes its value format: 2026-11-18, 2026-11, 2026-W47, 10:30 or 2026-11-18T10:30."}, "last_key": {"type": "boolean", "description": "For fill: type the last character as a key (autocomplete sees a key). Default false."}, "key": {"type": "string"}, "dy": {"type": "number"}, "url": {"type": "string"}, "snapshot": {"type": "boolean", "description": "Default true: add the changes of the page after the action."}, "diff": {"type": "boolean", "description": "Default true. False: add the whole outline instead of the changes."}, "mode": {"type": "string", "enum": ["interactive", "all"]}, "max_chars": {"type": "integer"}}, "required": ["op"]},
        },
        {
            "name": "find",
            "description": "Elements that match, as snapshot lines with refs, without a full snapshot. Give role (with name), label, text, placeholder or css. Text matches a part, in any case, unless exact. css with one of the others: only the matches inside it.",
            "inputSchema": {"type": "object", "properties": {"session": S, "role": {"type": "string"}, "name": {"type": "string"}, "label": {"type": "string"}, "text": {"type": "string"}, "placeholder": {"type": "string"}, "css": {"type": "string"}, "exact": {"type": "boolean"}, "max_chars": {"type": "integer"}}},
        },
        {
            "name": "read",
            "description": "The rendered text of the page or of one element (ref), one line per block, in pages of max_chars (default 8000) from offset.",
            "inputSchema": {"type": "object", "properties": {"session": S, "ref": {"type": "string"}, "offset": {"type": "integer"}, "max_chars": {"type": "integer"}}},
        },
        {
            "name": "links",
            "description": "The links of the page: ref, text, URL (the full URL with a text or root filter, else a path for a link on the origin of the page; only ad tracking parameters such as utm_* and fbclid are cut, every other part is exact) and the nearest landmark, such as (navigation \"Top\") or (contentinfo), when a link is in one. To get the URL of one named link, filter: links {text: \"Repository\"}. Filters combine.",
            "inputSchema": {"type": "object", "properties": {"session": S, "text": {"type": "string", "description": "Only links whose text or URL holds this text (any case)."}, "root": {"type": "string", "description": "A ref: only the links inside that element, or the link itself."}, "external": {"type": "boolean", "description": "True: only links to another origin."}, "max_chars": {"type": "integer"}}},
        },
        {
            "name": "screenshot",
            "description": "A PNG of the live page window, of the whole page (full_page true), or of the whole box of one element (ref), however tall: up to 8000 px, and the reply says when that cut it. Returns its path; with inline true also the image. Waits for pictures to load. Refused when a secret shows on the page. Prefer snapshot to read a page.",
            "inputSchema": {"type": "object", "properties": {"session": S, "ref": {"type": "string"}, "path": {"type": "string", "description": "Where to write the PNG. Default: a temp folder."}, "inline": {"type": "boolean"}, "full_page": {"type": "boolean", "description": "Paint the whole page, not only the window. Ignored with ref."}}},
        },
        {
            "name": "eval",
            "description": "Run JavaScript in the live page and return its value as text. A string shows as is. An array or object shows as compact JSON, cut at 4000 chars (a cycle is \"[Circular]\"). A DOM node shows as <tag#id.class>, a NodeList as a list of those, a function as function name(), a Map or Set as a list, a Date as its ISO string. A Promise is awaited up to the time limit. Statements give the value of the last one; with a top-level await or return, statements need a return. let and const last one call; keep a value for later in window.x. A syntax error names its line. A thrown error shows as Name: message. A navigation the code starts is followed; the reply then ends with navigated: URL.",
            "inputSchema": {"type": "object", "properties": {"session": S, "js": {"type": "string"}}, "required": ["js"]},
        },
        {
            "name": "console",
            "description": "The console of the live page, one short line per entry: level and text. Defaults: errors only, entries since the last console call. level: error (errors and uncaught exceptions), warn (and warnings) or all. since_last false: the last 500 entries. max: the newest entries to show, default 50.",
            "inputSchema": {"type": "object", "properties": {"session": S, "level": {"type": "string", "enum": ["error", "warn", "all"]}, "since_last": {"type": "boolean"}, "max": {"type": "integer"}}},
        },
        {
            "name": "network",
            "description": "The requests of the live page, one line per request: failure or status, kind (by URL), URL, size, time. Then the requests still on their way, at each call until they end: pending, kind (fetch, xhr, stream, script, stylesheet, preload, image), method, URL, age. Defaults: failed requests only (an error or HTTP 400 and over), since the last network call. failed_only false: all requests. url_contains: keep URLs with this text. since_last false: the last 500 records. max: the newest records to show, default 50.",
            "inputSchema": {"type": "object", "properties": {"session": S, "failed_only": {"type": "boolean"}, "url_contains": {"type": "string"}, "since_last": {"type": "boolean"}, "max": {"type": "integer"}}},
        },
        {
            "name": "diagnose",
            "description": "A report of what is wrong with the live page, to paste into a bare issue: the document load, uncaught exceptions, missing engine features, failed requests by kind, requests pending over 5 s, an empty-render check, the page size, and a verdict: site problem, bare gap, network or unclear, with the reason. Call it after a step failed.",
            "inputSchema": {"type": "object", "properties": {"session": S}},
        },
        {
            "name": "downloads",
            "description": "The files that the live session saved from downloads (a response with Content-Disposition: attachment or a type the page cannot show, or a link with a download attribute, also a blob: or data: URL made by a script): #number, path, size, type and source URL, or queued, running (bytes so far), paused, failed or cancelled. A download does not replace the page; act names each one that ends in its receipt. A big file downloads over up to 8 connections and resumes after a cut. op: list (default), pause, resume, cancel (removes the file), limit (bytes_per_second; 0 or absent: no limit), with id = the #number. Only a server that allows ranges lets a download pause. At most 3 downloads run at once; more wait as queued.",
            "inputSchema": {"type": "object", "properties": {"session": S, "op": {"type": "string", "enum": ["list", "pause", "resume", "cancel", "limit"]}, "id": {"type": "integer"}, "bytes_per_second": {"type": "integer"}}},
        },
        {
            "name": "sessions",
            "description": "The live sessions and their URLs. A tab that a page opened is listed, also when no reply named it yet.",
            "inputSchema": {"type": "object", "properties": {}},
        },
        {
            "name": "storage",
            "description": "Read, set or clear the cookies, localStorage or sessionStorage of a session (area: local, session, cookies; action: get, set, clear; key; value). Without key, get lists all and clear removes all. Known secrets show as [secret].",
            "inputSchema": {"type": "object", "properties": {"session": S, "area": {"type": "string", "enum": ["local", "session", "cookies"], "description": "Default local."}, "action": {"type": "string", "enum": ["get", "set", "clear"], "description": "Default get."}, "key": {"type": "string"}, "value": {"type": "string"}}},
        },
    ])
}

#[cfg(test)]
mod instructions_tests {
    use super::instructions;

    #[test]
    fn text_names_no_issue_route_or_host() {
        let t = instructions();
        assert!(t.starts_with("Loop: open a URL"));
        for word in ["issue", "report", "GitHub", "offstage", "mrwick1", "docs/"] {
            assert!(!t.contains(word), "{word} in {t}");
        }
    }
}

#[cfg(test)]
mod wake_tests;

#[cfg(test)]
mod dispatch_tests {
    use super::*;

    pub(super) fn test_shared() -> Arc<Shared> {
        let (out, _output) = mpsc::channel();
        Arc::new(Shared {
            saved: Mutex::new(HashMap::new()), runs: AtomicU32::new(0),
            net: Mutex::new(bare_net::http::Client::new()), out,
            sessions: Mutex::new(HashMap::new()), inflight: Mutex::new(HashMap::new()),
            threads: Mutex::new(Vec::new()), closing: AtomicBool::new(false),
        })
    }

    #[test]
    fn an_error_after_dispatch_never_claims_input_was_not_sent() {
        let shared = test_shared();
        let (tx, rx) = mpsc::channel();
        let job = Job {
            key: None, params: json!({"name":"act","arguments":{"expect":"expect title=\"Saved\""}}),
            cancel: Arc::new(AtomicBool::new(false)), started: true, t0: Instant::now(),
            done: Box::new(move |result| { let _ = tx.send(result); }), wake: None,
        };
        shared.finish(job, error_result("internal error: the call panicked"));
        let result = rx.recv().unwrap();
        assert_eq!(result["structuredContent"]["action_status"], "unknown", "{result}");
        assert_eq!(result["structuredContent"]["verification"], "unknown", "{result}");
        assert_eq!(result["structuredContent"]["failure"]["input_dispatched"], "unknown", "{result}");
    }

    #[test]
    fn failed_load_diagnostics_preserve_hash_characters() {
        let shared = test_shared();
        let (tx, _rx) = mpsc::sync_channel(QUEUE);
        lock(&shared.sessions).insert("a".into(), Slot {
            tx: Arc::new(tx), url: Arc::new(Mutex::new("(no document: load failed at #target)".into())),
            busy: Arc::new(AtomicUsize::new(1)),
        });
        let mut server = Server { shared, live: HashMap::new(), queue: Weak::new() };
        assert!(server.sessions().contains("#target"), "a diagnostic was treated as a URL");
    }

    #[test]
    fn cancellation_between_lines_is_not_success() {
        let opt = Options { size: (1280, 800), timeout: Duration::from_secs(1), out: None, window: false, progress: false };
        let session = Session::new(&opt);
        let dl = Downloads::new(std::env::temp_dir().join(format!("bare-cancel-unit-{}", std::process::id())));
        let live = Live { session, opt, dl, red: Redactor::default(), vars: HashMap::new(), lines: 0, told: Default::default() };
        let mut server = Server { shared: test_shared(), live: HashMap::from([("main".into(), live)]), queue: Weak::new() };
        set_cancel(Some(Arc::new(AtomicBool::new(true))));
        let result = server.run_live_typed(&json!({}), "cookie set should_not_run yes");
        set_cancel(None);
        assert!(result.is_err(), "cancelled steps were reported as successful");
    }

    #[test]
    fn retirement_keeps_pending_calls_on_the_owner_queue() {
        let shared = test_shared();
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let (done_tx, done_rx) = mpsc::channel();
        tx.send(Job {
            key: None, params: json!({"name": "snapshot", "arguments": {"session": "a"}}),
            cancel: Arc::new(AtomicBool::new(false)), started: false, t0: Instant::now(),
            done: Box::new(move |_| { let _ = done_tx.send(()); }), wake: None,
        }).unwrap();
        lock(&shared.sessions).insert("a".into(), Slot {
            tx: Arc::new(tx), url: Arc::new(Mutex::new(String::new())), busy: Arc::new(AtomicUsize::new(1)),
        });
        // A pending call must stay with this owner, rather than being dispatched through a newly freed name.
        let _ = shared.retire("a", &rx);
        assert!(lock(&shared.sessions).contains_key("a"), "retirement exposed a free name before draining older calls");
        assert!(done_rx.try_recv().is_err(), "a queued call was rerouted during retirement");
    }
}
