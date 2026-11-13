//! Run the steps. Each step is tried, and tried again until it passes or its time limit ends: between tries the driver runs the tasks of
//! the page that are due (`Page::tick`) and sleeps until the next one, so a step waits for a condition and never for a fixed time. When
//! no task is queued and no reply or document is on its way, nothing can change the page, so the step fails at once.
//!
//! `open`, `goto` and `reload` return at once, as a link click does: the old document ends, and the ticks of the next steps download,
//! parse and run the new one. Until it is in, the page has no document, so a locator or an `expect` waits.
//!
//! Failure kinds: `not_found`, `duplicate` (a locator must match one element), `hidden`, `disabled`, `covered`, `detached` (it matched,
//! then left the document), `not_editable`, `no_focus`, `expect` (a check was false), `page` (a load failed, the page stopped or has no
//! layout). `duplicate`, `not_editable`, `no_focus` and a failed load fail at once; the others wait. `covered` waits at most `COVER_WAIT`, and
//! names the cover by its role and name.
//!
//! Secrets: the values of `--secrets` and each text typed into a password field are replaced by `[secret]` in all the output (also in
//! lower case and URL-encoded). A capture is withheld when a secret is in the text or in a field of the page (a password field shows
//! dots, so it does not count).

use crate::locate::{self, Locator};
use crate::steps::{Action, Check, CookieState, State, Step};
use bare::control::{Handle, Log, Refusal, Remote, Ticks};
pub use bare::agent::Event;
pub use bare::page::StorageState;
use bare::page::Page;
use bare_paint::canvas::Canvas;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct Options {
    pub size: (u32, u32),
    pub timeout: Duration,
    /// Where `result.json` and the captures go.
    pub out: Option<PathBuf>,
    /// Show the page in a Wayland window (`Remote`), else run it headless on this thread.
    pub window: bool,
    /// Print one line for each step on stderr.
    pub progress: bool,
}

/// Replaces each secret in a text by `[secret]`.
#[derive(Clone, Default)]
pub struct Redactor {
    /// ASCII lower case, the longest first.
    words: Vec<String>,
}

impl Redactor {
    pub fn add(&mut self, secret: &str) {
        if secret.is_empty() {
            return;
        }
        let form = percent(secret, true);
        for w in [secret.to_ascii_lowercase(), percent(secret, false).to_ascii_lowercase(), form.to_ascii_lowercase()] {
            if !self.words.contains(&w) {
                self.words.push(w);
            }
        }
        self.words.sort_by_key(|w| std::cmp::Reverse(w.len()));
    }

    pub fn clean(&self, s: &str) -> String {
        let mut out = s.to_string();
        for w in &self.words {
            let mut at = 0;
            while let Some(i) = lower_find(&out[at..], w) {
                out.replace_range(at + i..at + i + w.len(), "[secret]");
                at += i + "[secret]".len();
            }
        }
        out
    }

    pub fn hits(&self, s: &str) -> bool {
        self.words.iter().any(|w| lower_find(s, w).is_some())
    }
}

/// The first place of `w` (lower case) in `s`, in any case. Only ASCII letters change case, so the places of the bytes stay.
fn lower_find(s: &str, w: &str) -> Option<usize> {
    let (s, w) = (s.as_bytes(), w.as_bytes());
    (0..(s.len() + 1).saturating_sub(w.len())).find(|&i| s[i..i + w.len()].eq_ignore_ascii_case(w))
}

/// `encodeURIComponent`, or the form encoding (a space is `+`).
fn percent(s: &str, form: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => out.push(b as char),
            b' ' if form => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The page: none yet, one on this thread, or one in a window on its own thread.
enum Host {
    None,
    Local(Box<Page>),
    Window(Remote),
}

impl Host {
    /// An `open` is a new tab of one browser, in a window too: empty storage, and the connections and the HTTP cache of `net`.
    /// `state`: the saved session to start from (`None`: empty storage).
    fn open(&mut self, url: &str, size: (u32, u32), window: bool, net: &bare_net::http::Client, state: Option<StorageState>, downloads: Option<bare::Downloads>) -> Result<(), String> {
        match self {
            // The new page takes the size of the window, not `size`: the compositor can have resized the window, and the window loop
            // calls `resize` only when the window size changes.
            Host::Window(r) => {
                let (url, net) = (url.to_string(), net.fork());
                r.call(move |p| p.reopen_in_with(&url, &net, state.as_ref(), downloads))?
            }
            _ if window => {
                *self = Host::Window(Remote::open_with(url, size, net, state, downloads)?);
                Ok(())
            }
            _ => {
                // The old page ends first, so the two pages do not share the peak of memory. A failed open leaves no page.
                *self = Host::None;
                let mut p = Page::open_in_with(url, size, net, state.as_ref(), downloads)?;
                p.quiet = true;
                *self = Host::Local(Box::new(p));
                Ok(())
            }
        }
    }

    fn run<R: Send + 'static>(&mut self, f: impl FnOnce(&mut Page) -> R + Send + 'static) -> Result<R, String> {
        match self {
            Host::None => Err("no page: the first step must be open".into()),
            Host::Local(p) => Ok(f(p)),
            Host::Window(r) => r.call(f),
        }
    }

    /// See `Remote::work_count` (0 here).
    fn work_count(&self) -> u64 {
        if let Host::Window(r) = self { r.work_count() } else { 0 }
    }

    /// Sleep until `until`. In a window, also wake when its loop runs a task of the page after `seen`.
    fn sleep(&self, seen: u64, until: Instant) {
        match self {
            Host::Window(r) => r.wait_work(seen, until),
            _ => std::thread::sleep(until.saturating_duration_since(Instant::now())),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Fail {
    pub kind: &'static str,
    pub message: String,
}

fn fail(kind: &'static str, message: impl Into<String>) -> Fail {
    Fail { kind, message: message.into() }
}

enum Try {
    Done(Option<String>),
    /// Not done: try again at once (the next key of a `type`).
    More,
    Again(Fail),
    Stop(Fail),
}

/// What the driver keeps between steps (it goes to the page thread and back).
#[derive(Default)]
struct Memo {
    /// The element that each locator found last: a later miss of a node that left the document is `detached`.
    seen: HashMap<String, Handle>,
    /// The texts typed into password fields since the last step.
    typed: Vec<String>,
    /// The keys of the `type` step of now that are not sent yet, the last first. One goes in each try, so the tasks of the page run
    /// between two keys.
    keys: Option<(Handle, Vec<char>)>,
    /// The refs that snapshots minted for the document of now.
    refs: crate::snap::Refs,
    /// The step of now clicked a link to another address (`link_target`). A page script can take that click and load the next page
    /// itself (a fetch, then `pushState`), so the step waits for the page as after a `goto`.
    link: bool,
}

pub struct StepOut {
    pub line: usize,
    pub text: String,
    pub ok: bool,
    pub ms: f64,
    pub busy_ms: f64,
    pub waited_ms: f64,
    pub tries: u32,
    /// CPU time of the page thread in the step (in a window, the thread of the window loop).
    pub cpu_ms: f64,
    /// The time that the page thread waited inside a network call in the step.
    pub net_ms: f64,
    /// The ticks of the page thread in the step. Headless: each call of the driver on the page (a try and `Page::tick`, or an `open`).
    /// In a window: each round of the window loop (the events, the jobs of the driver, `Page::tick` and the draw).
    pub ticks: Ticks,
    pub note: Option<String>,
}

pub struct Report {
    pub ok: bool,
    pub steps: Vec<StepOut>,
    pub failure: Option<(usize, Fail, Option<String>)>,
    pub logs: Vec<(usize, Log)>,
    pub net: Vec<(usize, bare_net::trace::Rec)>,
    pub ms: f64,
    pub window: bool,
    /// The ticks of all the steps.
    pub ticks: Ticks,
    /// The cookie jar of the last session at the end: names and flags, never a value.
    pub cookies: Vec<CookieOut>,
}

pub struct CookieOut {
    pub name: String,
    pub domain: String,
    pub host_only: bool,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: &'static str,
    pub session: bool,
}

/// The state of a run between its steps: the page, the network client, what the locators found, and the report so far. `run` drives
/// one for a step file; a caller that gets its steps one at a time (the MCP server) keeps one and calls `step` for each.
pub struct Session {
    host: Host,
    net: bare_net::http::Client,
    memo: Memo,
    timeout: Duration,
    // The CPU time of the window thread at the end of the last step.
    window_cpu: u64,
    // The first network record of the last step that loaded a document: the ticks of later steps load it.
    nav_first: usize,
    start: Instant,
    // A session for the next `open` that has no state file (see `preload`).
    preload: Option<StorageState>,
    /// The last console entries and network records, kept after `drain` (the `console`, `network` and `diagnose` tools read it).
    pub hist: crate::diag::History,
    /// Where this session saves downloads (`set_downloads`).
    downloads: Option<bare::Downloads>,
    /// An `open`, `goto` or `reload` ends when the page is ready (or at the time limit), not at once. Off for a step file: its next step waits.
    pub ready_wait: bool,
    pub rep: Report,
}

impl Session {
    pub fn new(opt: &Options) -> Session {
        Session::with_client(opt, bare_net::http::Client::new())
    }

    /// A session that uses `net`: a caller that keeps one client for many sessions (the MCP server) shares its connections and its reply
    /// cache between them. Each document still has its own cookie jar and storage.
    pub fn with_client(opt: &Options, net: bare_net::http::Client) -> Session {
        bare_net::trace::enable();
        let _ = bare_net::trace::take();
        let rep = Report { ok: true, steps: Vec::new(), failure: None, logs: Vec::new(), net: Vec::new(), ms: 0.0, window: opt.window, ticks: Ticks::default(), cookies: Vec::new() };
        Session { host: Host::None, net, memo: Memo::default(), timeout: opt.timeout, window_cpu: 0, nav_first: 0, start: Instant::now(), preload: None, hist: Default::default(), downloads: None, ready_wait: false, rep }
    }

    /// Save the downloads of the pages that later `open` steps make in `d`.
    pub fn set_downloads(&mut self, d: bare::Downloads) {
        self.downloads = Some(d);
    }

    /// Make the next `open` step that has no state file start from `st`.
    pub fn preload(&mut self, st: StorageState) {
        self.preload = Some(st);
    }

    /// Run `f` on the page with the refs of the session (`snap`): a read between steps.
    pub fn with_refs<R: Send + 'static>(&mut self, f: impl FnOnce(&mut Page, &mut crate::snap::Refs) -> R + Send + 'static) -> Result<R, String> {
        let mut m = std::mem::take(&mut self.memo);
        let r = self.host.run(move |p| {
            let r = f(p, &mut m.refs);
            (m, r, p.take_logs())
        });
        match r {
            Ok((m, r, logs)) => {
                self.memo = m;
                // What the read made the page log, and the requests since the last step, belong to the history and not to a step report.
                self.hist.feed(logs, bare_net::trace::take());
                Ok(r)
            }
            Err(e) => Err(e),
        }
    }

    /// A PNG at `path` of the page window, of the whole page (`full`), or of the whole box of the element of ref `r`, whatever its
    /// height: the page is painted from its layout, without a scroll. A picture is at most `SHOT_MAX` pixels tall; the reply says when
    /// that cut it. Waits up to the step time limit for the document and its pictures. Refused when a secret shows on the page.
    pub fn screenshot(&mut self, path: PathBuf, r: Option<u32>, full: bool, red: &Redactor) -> Result<String, String> {
        let (red, limit) = (red.clone(), self.timeout);
        self.with_refs(move |p, refs| {
            let end = Instant::now() + limit;
            let _ = p.wait_load(limit);
            while (p.loading() || p.images_pending()) && Instant::now() < end {
                let (more, next) = p.tick();
                if !more {
                    std::thread::sleep(next.map_or(Duration::from_millis(5), |t| t.saturating_duration_since(Instant::now()).min(Duration::from_millis(20))));
                }
            }
            if !shown(p, &red) {
                return Err("withheld: a secret shows on the page".to_string());
            }
            let (page, scrolled) = p.page_height();
            let (mut c, mut cut, mut clip) = (None, false, None);
            if let Some(r) = r {
                let h = refs.get(r, p.generation())?;
                let b = p.visible_box(h).map_err(|e| format!("e{r} has no visible box: {e:?}"))?;
                // A box above the window is painted from the top of the page, so that its place moves down by the scroll.
                let (top, dy) = if b.y < 0.0 { (true, scrolled) } else { (false, 0.0) };
                let bottom = (b.y + dy + b.h).ceil().max(1.0) as u32;
                cut = bottom > SHOT_MAX;
                c = p.capture_tall(bottom.max(p.size().1).min(SHOT_MAX), top);
                clip = Some((b.x, b.y + dy, b.w, b.h));
            } else if full {
                cut = page > SHOT_MAX;
                c = p.capture_tall(page.min(SHOT_MAX), true);
            }
            let c = match (c, clip) {
                (Some(c), Some(b)) => crop(c, b),
                (Some(c), None) => c,
                (None, _) => p.capture(),
            };
            if c.height == 0 || c.width == 0 {
                return Err("nothing to paint: the box is empty or below the size limit".to_string());
            }
            bare_paint::png::write_file(&c, &path).map_err(|e| format!("{}: {e}", path.display()))?;
            let note = if cut { format!(", cut at the limit of {SHOT_MAX} px") } else { String::new() };
            Ok(format!("{} ({}x{}{note})", path.display(), c.width, c.height))
        })?
    }

    /// Run a script in the page and give its value as text: an object as compact JSON of at most `EVAL_MAX` characters (`Browser::inspect`). A Promise is awaited up to the time limit.
    /// Code runs as a script in a block, so it gives the value of its last statement and its `let` and `const` last one call.
    /// Code with a top-level `await` or `return` runs as an async function: an expression gives its value, statements need a `return`.
    /// A syntax error names its line. Each wrapper starts on the first line of the code, so the lines stay the lines of the agent.
    pub fn eval(&mut self, code: String) -> Result<String, String> {
        let limit = self.timeout;
        self.with_refs(move |p, _| {
            let _ = p.wait_load(Duration::from_secs(10));
            let before = p.url();
            let b = p.browser().ok_or("no document")?;
            // The forms to try in order. A form that does not parse runs nothing, so the next form runs the code once.
            // The `\n` before each closing brace ends a `//` comment on the last line.
            let block = format!("{{{code}\n}}");
            let expr = format!("(async () => {{ return ({code}\n) }})()");
            let body = format!("(async () => {{{code}\n}})()");
            // `await` is a plain name in a script, so `await (x)` would run as a call: code with the word never runs as a script.
            let wait = has_await(&code);
            let forms = if wait { [expr, body] } else { [block, body] };
            // Parse again only after a failure: the engine's error value has no line. A runtime error parses, so it gives `None`.
            let syntax = |src: &str| bare_js::parse::parse_script(src, Default::default()).err();
            let mut src = &forms[0];
            let mut r = b.run_script(src);
            // The expression form fails to parse for statements with `await`: that needs no second look.
            let mut bad = if r.is_err() && !wait { syntax(src) } else { None };
            // A script goes on to the async form only for a top-level `return`.
            if r.is_err() && (wait || bad.is_some_and(|e| e.msg == "'return' outside of a function")) {
                src = &forms[1];
                r = b.run_script(src);
                bad = if r.is_err() { syntax(src) } else { None };
            }
            p.touched();
            let v = match r {
                Ok(v) => v,
                Err(e) => {
                    let m = thrown(p.browser().expect("checked"), &e);
                    let line = bad.map(|e| format!(" (line {})", e.location(src).line)).unwrap_or_default();
                    return Err(format!("the script threw: {m}{line}"));
                }
            };
            let end = Instant::now() + limit;
            let mut v = v;
            let out = loop {
                let b = p.browser().ok_or("no document")?;
                match b.it.promise_status(v) {
                    None => break b.inspect(v, EVAL_MAX),
                    Some(bare_js::eval::PromiseStatus::Fulfilled(x)) => v = x,
                    Some(bare_js::eval::PromiseStatus::Rejected(x)) => return Err(format!("rejected: {}", thrown_value(b, x))),
                    Some(bare_js::eval::PromiseStatus::Pending) => {
                        if Instant::now() >= end {
                            return Err(format!("the promise is still pending after {} ms", limit.as_millis()));
                        }
                        let (more, next) = p.tick();
                        if !more {
                            std::thread::sleep(next.map_or(Duration::from_millis(5), |t| t.saturating_duration_since(Instant::now()).min(Duration::from_millis(20))));
                        }
                    }
                }
            };
            // The code started a navigation (a new address, a link `click()`, `form.submit()`, `history.back()`): the ticks follow it
            // until the next document has loaded, as after an `act`. A `#hash` change loads nothing.
            let started = |p: &mut Page| p.loading() || p.browser().map(|b| b.navigation_stats()).is_some_and(|n| n.pending_requests + n.pending_tasks > 0);
            while started(p) && Instant::now() < end {
                let (more, next) = p.tick();
                if !more {
                    std::thread::sleep(next.map_or(Duration::from_millis(5), |t| t.saturating_duration_since(Instant::now()).min(Duration::from_millis(20))));
                }
            }
            let _ = p.wait_load(end.saturating_duration_since(Instant::now()));
            // The line names the URL without its fragment (`safe_url`), so a `#hash` change gives no line.
            let (before, after) = (safe_url(&before.unwrap_or_default()), safe_url(&p.url().unwrap_or_default()));
            Ok(if after != before { format!("{out}\nnavigated: {after}") } else { out })
        })?
    }

    /// The page facts for `diag::report`, after a tick that collects what arrived.
    pub fn diagnose(&mut self, red: &Redactor) -> Result<String, String> {
        let f = self.with_refs(|p, _| {
            crate::snap::settle(p);
            let cpu = thread_cpu_ns();
            let (nodes, shown) = crate::snap::size(p).unwrap_or((0, 0));
            crate::diag::Facts { url: p.url().unwrap_or_default(), load_error: p.load_error().map(str::to_string), stopped: p.stopped(), nodes, shown, cpu_ms: cpu as f64 / 1e6, pending: p.pending() }
        })?;
        // The logs that the tick made are in the history after `with_refs`: read the report from it now.
        Ok(crate::diag::report(&self.hist, &f, &|s| red.clean(s)))
    }

    /// What the live page waits for (`Page::pending`), after a tick that collects what arrived.
    pub fn pending(&mut self) -> Result<Vec<bare::page::Waiting>, String> {
        self.with_refs(|p, _| {
            p.tick();
            p.pending()
        })
    }

    /// Collect what the page logged and requested since the last call (a tick), into the history.
    pub fn collect(&mut self) -> Result<(), String> {
        self.with_refs(|p, _| {
            let _ = p.tick();
        })
    }

    /// Answer the dialogs of the next actions (`bare::agent`). `events` ends it.
    pub fn set_dialog(&mut self, accept: bool, text: Option<String>) {
        let _ = self.host.run(move |_| bare::agent::set_policy(accept, text));
    }

    /// The dialogs and the tab requests since `set_dialog`, and no dialog policy after it.
    pub fn events(&mut self) -> Vec<bare::agent::Event> {
        self.host.run(|_| bare::agent::take()).unwrap_or_default()
    }

    /// Remove the cookie `name` (all its domains and paths), or every cookie of the jar. Gives how many cookies it removed.
    pub fn clear_cookies(&mut self, name: Option<String>) -> Result<usize, String> {
        self.host.run(move |p| {
            let jar = p.storage().ok_or("the page has no session")?.jar();
            let mut j = jar.lock().unwrap_or_else(|e| e.into_inner());
            let hit: Vec<_> = j.list().into_iter().filter(|c| name.as_ref().is_none_or(|n| *n == c.name)).map(|c| (c.name.clone(), c.domain.clone(), c.host_only, c.path.clone())).collect();
            if name.is_none() {
                j.clear();
            }
            for (n, d, host_only, path) in &hit {
                let text = format!("{n}=; Path={path}; Max-Age=0{}", if *host_only { String::new() } else { format!("; Domain={d}") });
                let at = bare_net::cookie::At { secure: true, host: d, path };
                let now = j.now();
                j.set(&at, &text, bare_net::cookie::Via::Http { third: false }, now);
            }
            Ok(hit.len())
        })?
    }

    /// The URL of the page of now.
    pub fn url(&mut self) -> Option<String> {
        self.host.run(|p| p.url()).ok().flatten()
    }

    /// A copy of the storage of the open page (it holds live credentials). `None` with no page.
    pub fn export_state(&mut self) -> Option<StorageState> {
        self.host.run(|p| p.storage().map(|s| s.export())).ok().flatten()
    }

    /// A recording or a replay (`bare_net::replay`) is a benchmark: the pictures that the page started to load finish before the next step, and at the
    /// end. Without it the work of a run depends on a race between a step (a click that leaves the page) and the queue of pictures. One look at a
    /// document that has no picture costs nothing.
    fn settle_pictures(&mut self) {
        if bare_net::replay::Tape::from_env().is_some() {
            let _ = self.host.run(|p| {
                let end = Instant::now() + Duration::from_secs(5);
                while p.images_pending() && Instant::now() < end {
                    let (more, next) = p.tick();
                    if !more {
                        std::thread::sleep(next.map_or(Duration::from_millis(5), |t| t.saturating_duration_since(Instant::now()).min(Duration::from_millis(20))));
                    }
                }
            });
        }
    }

    /// Run one step, and add its result to `rep`. False when it failed (`rep.failure` says why).
    pub fn step(&mut self, step: &Step, opt: &Options, red: &mut Redactor) -> bool {
        self.settle_pictures();
        let Session { host, net, memo, timeout, window_cpu, nav_first, rep, preload, hist, downloads, ready_wait, .. } = self;
    let t0 = Instant::now();
    let deadline = t0 + *timeout;
    let (mut tries, mut busy, mut waited, mut ticks) = (0u32, Duration::ZERO, Duration::ZERO, Ticks::default());
    // With `ready_wait`, an `open`, `goto` or `reload` waits until the page is ready (`ready`); `calm` counts the looks that found it so.
    // So does an action that started a navigation: a link click, `back`, `forward`, or a script that changed the address.
    let (mut navigating, mut calm) = (false, 0u8);
    // For an action that navigated: the document before it. A page that stays the same document keeps its old HTTP status, so the
    // status is checked only for a new document.
    let mut doc0: Option<usize> = None;
    // When the tries began to fail as `covered` without a break.
    let mut covered: Option<Instant> = None;
    let (cpu0, net0) = (thread_cpu_ns(), bare_net::trace::blocked_us());
    let (log0, net_at) = (rep.logs.len(), rep.net.len());
    if matches!(step.action, Action::Open(..) | Action::Goto(_) | Action::Reload | Action::Back | Action::Forward) {
        *nav_first = rep.net.len();
    }
    let result: Result<Option<String>, Fail> = loop {
        tries += 1;
        let (t, seen) = (Instant::now(), host.work_count());
        let (tried, wait) = match &step.action {
            Action::Timeout(ms) => {
                *timeout = Duration::from_millis(*ms);
                break Ok(None);
            }
            _ if navigating => {
                let at_end = Instant::now() >= deadline;
                match host.run(move |p| ready(p, at_end)) {
                    Ok(r) => {
                        let ms = t0.elapsed().as_millis();
                        let now = Instant::now();
                        let soon = now + Duration::from_millis(5);
                        let pending = |n: usize| format!("{n} request{} pending", if n == 1 { "" } else { "s" });
                        let state = match (r.fail, r.idle) {
                            (Some(f), _) => Err(f),
                            (None, Ok(())) if r.status >= 400 && doc0 != Some(r.doc) => Err(fail("page", format!("status {}: {} (title {:?}): {}", r.status, r.url, r.title, r.text))),
                            (None, idle) => Ok(idle),
                        };
                        let done = match state {
                            Err(f) => Some(Err(f)),
                            Ok(_) if r.stopped.is_some() => Some(Ok(Some(format!("not ready: the page stopped after {ms} ms ({}), body {}", r.stopped.as_deref().unwrap_or(""), if r.text.is_empty() { "empty" } else { "has content" })))),
                            Ok(Ok(())) if !r.text.is_empty() => {
                                calm = if r.ran { 0 } else { calm + 1 };
                                (calm >= 2).then_some(Ok(None))
                            }
                            // Empty and nothing can change it: no task, no request, no timer.
                            Ok(Ok(())) if !r.ran && r.next.is_none() => Some(Ok(Some(format!("not ready: body empty after {ms} ms, {}", pending(0))))),
                            Ok(_) => {
                                calm = 0;
                                at_end.then(|| Ok(Some(format!("not ready: {} after {ms} ms, {}", if r.text.is_empty() { "body empty" } else { "still loading" }, r.waits))))
                            }
                        };
                        match done {
                            Some(Ok(note)) => break Ok(note),
                            Some(Err(f)) => break Err(f),
                            None => {
                                rep.logs.extend(r.logs.into_iter().map(|l| (step.line, l)));
                                (Try::Again(fail("page", "not ready")), (r.ran, Some(r.next.filter(|n| *n < soon).unwrap_or(soon))))
                            }
                        }
                    }
                    Err(e) => break Err(fail("page", e)),
                }
            }
            Action::Open(url, file) => {
                memo.seen.clear();
                let from = file.as_deref().map(crate::state::read).transpose().map(|st| st.or_else(|| preload.take()));
                let opened = from.and_then(|st| host.open(url, opt.size, opt.window, net, st, downloads.clone())).and_then(|_| host.run(|p| p.take_logs()));
                let d = t.elapsed();
                busy += d;
                if !opt.window {
                    ticks.add(d);
                }
                match opened {
                    Ok(logs) => {
                        rep.logs.extend(logs.into_iter().map(|l| (step.line, l)));
                        if !*ready_wait {
                            break Ok(None);
                        }
                        navigating = true;
                        continue;
                    }
                    Err(e) => break Err(fail("page", e)),
                }
            }
            a => {
                let (a, mut m) = (a.clone(), std::mem::take(memo));
                let (out, ws) = (opt.out.clone(), red.clone());
                match host.run(move |p| {
                    let before = (p.generation(), p.url());
                    let mut tried = attempt(p, &mut m, &a, out.as_ref(), &ws);
                    // Did the action start a navigation? A document on its way, a new document, a new address (`pushState`), or a link.
                    let moved = matches!(tried, Try::Done(_)) && (std::mem::take(&mut m.link) || matches!(a, Action::Back | Action::Forward) || p.loading() || (p.generation(), p.url()) != before);
                    let wait = p.tick();
                    // The document of the last `open`, `goto` or `reload` did not load: no later try can pass.
                    if let (Try::Again(_), Some(why)) = (&tried, p.load_error()) {
                        tried = Try::Stop(fail("page", format!("the document did not load: {why}")));
                    }
                    // The page stopped for good (a fatal error, or too many tasks aborted by script budgets): say so in the failure. The
                    // step still tries again: the frame of the page that stays can change (a picture, a layout) after the stop.
                    if let (Try::Again(f), Some(why)) = (&mut tried, p.stopped()) {
                        f.message = format!("{} (the page stopped: {why})", f.message);
                    }
                    (m, (tried, moved, before.0), wait, p.take_logs())
                }) {
                    Ok((m, (tried, moved, doc), wait, logs)) => {
                        *memo = m;
                        for s in memo.typed.drain(..) {
                            red.add(&s);
                        }
                        rep.logs.extend(logs.into_iter().map(|l| (step.line, l)));
                        if moved {
                            doc0 = Some(doc);
                        }
                        (tried, wait)
                    }
                    Err(e) => break Err(fail("page", e)),
                }
            }
        };
        let d = t.elapsed();
        busy += d;
        if !opt.window {
            ticks.add(d);
        }
        match tried {
            Try::Done(_) if *ready_wait && matches!(step.action, Action::Goto(_) | Action::Reload) => {
                (navigating, doc0) = (true, None);
            }
            Try::Done(_) if *ready_wait && doc0.is_some() => navigating = true,
            Try::Done(note) => break Ok(note),
            Try::More => {}
            Try::Stop(f) => break Err(f),
            Try::Again(f) => {
                let now = Instant::now();
                if f.kind != "covered" {
                    covered = None;
                } else if now >= *covered.get_or_insert(now) + COVER_WAIT {
                    break Err(Fail { message: format!("{} (still covered after {} ms)", f.message, COVER_WAIT.as_millis()), ..f });
                }
                let next = match wait {
                    (true, _) => now,
                    (false, Some(at)) => at,
                    // A miss says what is near in its message: that is what the agent can act on, not the idle page.
                    (false, None) if f.kind == "not_found" => break Err(f),
                    (false, None) => {
                        break Err(Fail { message: format!("{} (the page is idle: no task is queued, so nothing can change it)", f.message), ..f });
                    }
                };
                // A navigation ends its wait by itself at the limit, with a note and not a failure.
                if now >= deadline && !navigating {
                    break Err(Fail { message: format!("{} (time limit {} ms)", f.message, timeout.as_millis()), ..f });
                }
                host.sleep(seen, next.min(deadline).min(covered.map_or(deadline, |c| c + COVER_WAIT)));
                waited += now.elapsed();
            }
        }
    };
    // In a window the loop counts its rounds and the page runs on its thread; this call ends the step (its own round counts in the
    // next step).
    let mut cpu_ns = thread_cpu_ns().saturating_sub(cpu0);
    if let (true, Ok((t, c))) = (opt.window, host.run(|p| (std::mem::take(&mut p.ticks), thread_cpu_ns()))) {
        ticks.merge(t);
        cpu_ns = c.saturating_sub(*window_cpu);
        *window_cpu = c;
    }
    rep.ticks.merge(ticks);
    let first = rep.net.len();
    rep.net.extend(bare_net::trace::take().into_iter().map(|r| (step.line, r)));
    hist.feed(rep.logs[log0..].iter().map(|(_, l)| l.clone()), rep.net[net_at..].iter().map(|(_, r)| r.clone()));
    // A failed step names the network failures of this step, and of the document that loads since the last `open`, `goto` or
    // `reload`: the page only sees `Failed to fetch`.
    let result = result.map_err(|f| {
        let causes: Vec<String> = rep.net[first.min(*nav_first)..].iter().filter_map(|(_, r)| Some(format!("{}: {}", safe_url(&r.url), r.error.as_ref()?))).take(3).collect();
        if causes.is_empty() { f } else { Fail { message: format!("{} (network failures: {})", f.message, causes.join("; ")), ..f } }
    });
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let ok = result.is_ok();
    let note = result.as_ref().ok().cloned().flatten();
    let cpu_ms = cpu_ns as f64 / 1e6;
    let net_ms = bare_net::trace::blocked_us().saturating_sub(net0) as f64 / 1000.0;
    rep.steps.push(StepOut { line: step.line, text: step.text.clone(), ok, ms: ms(t0.elapsed()), busy_ms: ms(busy), waited_ms: ms(waited), tries, cpu_ms, net_ms, ticks, note });
    if opt.progress {
        let s = rep.steps.last().expect("pushed");
        let what = match &result {
            Ok(_) => "ok".to_string(),
            Err(f) => format!("FAIL {}: {}", f.kind, f.message),
        };
        let longest = s.ticks.max.as_secs_f64() * 1000.0;
        eprintln!("{}", red.clean(&format!("L{} {} {what} ({:.0} ms, {} tries, longest tick {longest:.1} ms)", s.line, s.text, s.ms, s.tries)));
    }
    if let Err(f) = result {
        rep.ok = false;
        let shot = opt.out.as_ref().and_then(|dir| {
            let ws = red.clone();
            let canvas = host.run(move |p| shown(p, &ws).then(|| p.capture())).ok()?;
            Some(match canvas {
                Some(c) => save(&c, dir, &format!("fail-L{}", step.line)),
                None => "withheld: a secret is on the page".to_string(),
            })
        });
        rep.failure = Some((step.line, f, shot));
        return false;
    }
        true
    }

    /// The report so far, with the cookie jar of the page. The page stays open.
    pub fn report(&mut self) -> &Report {
        self.rep.cookies = self.host.run(|p| p.storage().map(|s| dump(&s.jar().lock().unwrap_or_else(|e| e.into_inner())))).ok().flatten().unwrap_or_default();
        self.rep.ms = self.start.elapsed().as_secs_f64() * 1000.0;
        &self.rep
    }

    /// Run the tasks of the page that are due (`Page::tick`), as a browser runs a tab while nobody acts. Gives when the next task is
    /// due. A page in a window runs on its own thread, so this does nothing for it.
    pub fn tick(&mut self) -> Option<Instant> {
        match &mut self.host {
            Host::Local(p) => p.tick().1,
            _ => None,
        }
    }

    /// Move out the steps, logs, network records and failure so far, and start a new part of the report. The page stays open. A caller
    /// that sends steps one call at a time gives each call its own report, and the session does not keep old records.
    pub fn drain(&mut self) -> Report {
        self.report();
        let ok = std::mem::replace(&mut self.rep.ok, true);
        let r = &mut self.rep;
        Report { ok, steps: std::mem::take(&mut r.steps), failure: r.failure.take(), logs: std::mem::take(&mut r.logs), net: std::mem::take(&mut r.net), ms: r.ms, window: r.window, ticks: std::mem::take(&mut r.ticks), cookies: std::mem::take(&mut r.cookies) }
    }

    /// Close the page (and its window) and give the report. The page first hears that it is hidden, and the requests of its handlers
    /// get up to 2 s (`Page::hide`).
    pub fn finish(mut self) -> Report {
        self.settle_pictures();
        let _ = self.host.run(|p| p.hide(Duration::from_secs(2)));
        self.report();
        drop(self.host);
        self.rep.ms = self.start.elapsed().as_secs_f64() * 1000.0;
        // A recording or a replay (`bare_net::replay`) is a benchmark: let the pictures that still load and decode finish, so that each run does the same work.
        if bare_net::replay::Tape::from_env().is_some() {
            self.net.drain(Duration::from_secs(30));
        }
        self.rep
    }
}

/// Run the steps in order until one fails. `red` gets the secrets that the steps type into password fields.
pub fn run(steps: &[Step], opt: &Options, red: &mut Redactor) -> Report {
    let mut s = Session::new(opt);
    for step in steps {
        if !s.step(step, opt, red) {
            break;
        }
    }
    s.finish()
}

/// The jar for the result: the names and the flags of each cookie. A value never goes in (a cookie is a credential).
fn dump(j: &bare_net::cookie::Jar) -> Vec<CookieOut> {
    use bare_net::cookie::SameSite;
    j.list()
        .into_iter()
        .map(|c| CookieOut {
            name: c.name.clone(),
            domain: c.domain.clone(),
            host_only: c.host_only,
            path: c.path.clone(),
            secure: c.secure,
            http_only: c.http_only,
            same_site: match c.same_site {
                SameSite::Unset => "unset",
                SameSite::None => "none",
                SameSite::Lax => "lax",
                SameSite::Strict => "strict",
            },
            session: c.expires.is_none(),
        })
        .collect()
}

/// Make the options create a file that only the owner can read (mode 0600 on Unix; elsewhere the default). It is here because
/// `tools/portable_check.sh` allows OS code in this file only.
pub(crate) fn private(o: &mut std::fs::OpenOptions) {
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(o, 0o600);
    #[cfg(not(unix))]
    let _ = o;
}

/// The CPU time of this thread in nanoseconds, from `/proc/thread-self/schedstat` (0 where it cannot be read).
fn thread_cpu_ns() -> u64 {
    std::fs::read_to_string("/proc/thread-self/schedstat").ok().and_then(|s| s.split(' ').next()?.parse().ok()).unwrap_or(0)
}

/// How long an action waits for a cover to go (a spinner, a backdrop that fades) before it fails as `covered`. A cover that stays (a cookie
/// banner) is for the caller to close, so it must not cost the whole time limit.
const COVER_WAIT: Duration = Duration::from_secs(1);

/// The most pixels tall that a screenshot is.
const SHOT_MAX: u32 = 8000;

/// The part of `c` under `b`, clipped to the canvas.
fn crop(c: Canvas<'static>, b: (f32, f32, f32, f32)) -> Canvas<'static> {
    let (bx, by, bw, bh) = b;
    let (w, h) = (c.width as i64, c.height as i64);
    let x0 = (bx.floor() as i64).clamp(0, w);
    let y0 = (by.floor() as i64).clamp(0, h);
    let x1 = ((bx + bw).ceil() as i64).clamp(x0, w);
    let y1 = ((by + bh).ceil() as i64).clamp(y0, h);
    let src = c.pixels.into_vec();
    let mut out = Vec::with_capacity(((x1 - x0) * (y1 - y0)) as usize);
    for y in y0..y1 {
        out.extend_from_slice(&src[(y * w + x0) as usize..(y * w + x1) as usize]);
    }
    Canvas { width: (x1 - x0) as u32, height: (y1 - y0) as u32, pixels: bare_paint::canvas::Pixels::Own(out) }
}

fn save(c: &Canvas, dir: &std::path::Path, name: &str) -> String {
    let path = dir.join(format!("{name}.png"));
    match bare_paint::png::write_file(c, &path) {
        Ok(()) => path.display().to_string(),
        Err(e) => format!("not written: {e}"),
    }
}

/// May the page be captured: no secret in its text or in a field that is not a password field?
fn shown(p: &mut Page, red: &Redactor) -> bool {
    let fields = p.query_all("input, textarea").unwrap_or_default();
    for h in fields {
        if !p.is_password(h) && p.value(h).is_some_and(|v| red.hits(&v)) {
            return false;
        }
    }
    p.inspect(|doc, _| {
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if let bare_html::dom::NodeData::Text(span) = doc.data(n)
                && red.hits(doc.text(*span))
            {
                return false;
            }
            stack.extend(doc.children(n));
            stack.extend(doc.shadow_root(n));
        }
        true
    })
    .unwrap_or(true)
}

/// The URL for a message: no fragment (it can hold a token), and the value of a query parameter hidden when its name looks secret.
pub fn safe_url(u: &str) -> String {
    let u = u.split('#').next().unwrap_or("");
    let Some((path, q)) = u.split_once('?') else { return u.to_string() };
    let secret = |k: &str| {
        let k = k.to_ascii_lowercase();
        ["token", "key", "code", "password", "secret", "auth", "session", "sig"].iter().any(|w| k.contains(w))
    };
    let pairs: Vec<String> = q.split('&').map(|kv| match kv.split_once('=') {
        Some((k, _)) if secret(k) => format!("{k}=…"),
        _ => kv.to_string(),
    }).collect();
    format!("{path}?{}", pairs.join("&"))
}

/// The most characters of an `eval` value; a longer value is cut, with its full length.
const EVAL_MAX: usize = 4000;

/// A thrown value as text: `Name: message` for an error object, else its string form.
fn thrown_value(b: &mut bare::browser::Browser, v: bare_js::value::Value) -> String {
    if matches!(v, bare_js::value::Value::Object(_)) {
        let (n, m) = (b.it.get_str(v, "name"), b.it.get_str(v, "message"));
        if n != "?" && !(n == "undefined" && m == "undefined") {
            return if m.is_empty() || m == "undefined" { n } else { format!("{n}: {m}") };
        }
    }
    b.it.display(v)
}

/// A script error as text: a thrown value by `thrown_value`, else the engine's own text.
fn thrown(b: &mut bare::browser::Browser, e: &bare_js::eval::Error) -> String {
    match e {
        bare_js::eval::Error::Throw(v) => thrown_value(b, *v),
        e => format!("{e:?}"),
    }
}

/// True when `code` holds the word `await`.
fn has_await(code: &str) -> bool {
    let id = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    code.match_indices("await").any(|(i, w)| !code[..i].chars().next_back().is_some_and(id) && !code[i + w.len()..].chars().next().is_some_and(id))
}

fn short(s: &str) -> String {
    let s = locate::norm(s);
    if s.chars().count() > 120 { s.chars().take(120).collect::<String>() + "…" } else { s }
}

/// The elements of `loc`, with a ref resolved from the memo.
fn found_in(p: &mut Page, m: &Memo, loc: &Locator) -> Result<locate::Found, Try> {
    if let Locator::Ref(r) = loc {
        let h = m.refs.get(*r, p.generation()).map_err(|e| Try::Stop(fail("stale_ref", e)))?;
        let shown = if p.connected(h) && p.visible_box(h).is_ok() { vec![h] } else { vec![] };
        return Ok(locate::Found { hidden: usize::from(shown.is_empty()), shown });
    }
    locate::find(p, loc).map_err(|e| Try::Again(fail("page", e)))
}

/// The one element that `loc` names now.
fn one(p: &mut Page, m: &mut Memo, loc: &Locator) -> Result<Handle, Try> {
    if let Locator::Ref(r) = loc {
        let h = m.refs.get(*r, p.generation()).map_err(|e| Try::Stop(fail("stale_ref", e)))?;
        if !p.connected(h) {
            return Err(Try::Stop(fail("detached", format!("{loc} ({}) left the document: take a new snapshot", p.describe(h)))));
        }
        return Ok(h);
    }
    let found = locate::find(p, loc).map_err(|e| if e.contains("not valid") { Try::Stop(fail("not_found", e)) } else { Try::Again(fail("page", e)) })?;
    let key = loc.to_string();
    match found.shown.as_slice() {
        [h] => {
            m.seen.insert(key, *h);
            Ok(*h)
        }
        [] => Err(match m.seen.get(&key) {
            Some(&old) if old.doc == p.generation() && !p.connected(old) => Try::Again(fail("detached", format!("{loc} matched {} before; it left the document", p.describe(old)))),
            _ if found.hidden > 0 => Try::Again(fail("hidden", format!("{loc}: {} element(s) match, none is rendered (display: none, visibility: hidden or aria-hidden)", found.hidden))),
            // A rendered element with a near name: the locator is wrong, so waiting would not help.
            _ => match locate::near(p, loc) {
                Some((near, true)) => Try::Stop(fail("not_found", format!("no element matches {loc}; {near}"))),
                Some((near, false)) => Try::Again(fail("not_found", format!("no element matches {loc}; {near}"))),
                None => Try::Again(fail("not_found", format!("no element matches {loc}"))),
            },
        }),
        many => {
            let list: Vec<String> = many.iter().take(5).map(|&h| p.describe(h)).collect();
            Err(Try::Stop(fail("duplicate", format!("{loc} matches {} elements: {}{}", many.len(), list.join(", "), if many.len() > 5 { ", …" } else { "" }))))
        }
    }
}

/// `one`, or the text field that has the focus when the element that `loc` named before left the document (`Page::replaced`). A page that
/// swaps its search field (Wikipedia) leaves the old locator with no match; a person's keys go to the new field. Only for the steps that
/// send keys (`type`, `press`): a click, a select, a check or an upload on a field that is gone must fail.
fn one_or_replaced(p: &mut Page, m: &mut Memo, loc: &Locator) -> Result<Handle, Try> {
    let err = match one(p, m, loc) {
        Ok(h) => return Ok(h),
        Err(e) => e,
    };
    let old = match loc {
        Locator::Ref(r) => m.refs.get(*r, p.generation()).ok(),
        _ => m.seen.get(&loc.to_string()).copied().filter(|h| h.doc == p.generation()),
    };
    old.and_then(|h| p.replaced(h)).ok_or(err)
}

fn refused(p: &mut Page, h: Handle, r: Refusal) -> Try {
    let what = p.describe(h);
    match r {
        Refusal::Detached => Try::Again(fail("detached", format!("{what} left the document"))),
        Refusal::Hidden(why) => Try::Again(fail("hidden", format!("{what} is hidden: {why}"))),
        Refusal::Disabled => Try::Again(fail("disabled", format!("{what} is disabled"))),
        Refusal::Covered { by, top, at } => {
            let by = top.and_then(|t| locate::cover(p, h, t)).unwrap_or(by);
            Try::Again(fail("covered", format!("{what} is covered by {by} at ({}, {})", at.0, at.1)))
        }
        Refusal::NotEditable => Try::Stop(fail("not_editable", format!("{what} is not a text field"))),
        Refusal::NoFocus => Try::Stop(fail("no_focus", format!("{what} did not take the focus after the click"))),
        Refusal::NoPage(e) => Try::Again(fail("page", e)),
        Refusal::Wrong(kind, why) => Try::Stop(fail(kind, why)),
    }
}

/// What one look at a page that a navigation started tells.
struct Look {
    /// A task ran in this look; when the next one is due.
    ran: bool,
    next: Option<Instant>,
    /// `Ok`: loaded, and nothing waits. `Err`: the requests that wait.
    idle: Result<(), usize>,
    /// The document did not load, or the page stopped.
    fail: Option<Fail>,
    /// Why the page stopped for good (a script made a fatal error), if it did: it cannot get ready any more.
    stopped: Option<String>,
    /// The number of the document (`Page::generation`).
    doc: usize,
    status: u16,
    url: String,
    title: String,
    /// The text of the body (a mark for a control or a picture), up to about 200 characters. Only read when the page is idle or `at_end`.
    text: String,
    /// What holds the page, as `waits` says it. Only read when the page is not idle at `at_end`.
    waits: String,
    logs: Vec<Log>,
}

/// The text of the page for a person: no script, no style, and the shadow trees of components. A control or a picture counts as content.
const BODY_TEXT: &str = "(function(){var o=[],n=0;(function f(e){for(var c=e.firstChild;c&&n<200;c=c.nextSibling){if(c.nodeType==3){var t=c.data.replace(/\\s+/g,' ').trim();if(t){o.push(t);n+=t.length}}else if(c.nodeType==1){var g=c.localName;if(g=='script'||g=='style'||g=='noscript'||g=='template')continue;if(/^(img|input|textarea|select|button|canvas|svg|video|iframe)$/.test(g)){o.push('['+g+']');n+=5}f(c);if(c.shadowRoot)f(c.shadowRoot)}}})(document.body||document.documentElement);return o.join(' ')})()";

/// One look: run the tasks that are due, then ask if the page is ready.
fn ready(p: &mut Page, at_end: bool) -> Look {
    let (ran, next) = p.tick();
    let idle = p.idle();
    let fail = p.load_error().map(|why| fail("page", format!("the document did not load: {why}")));
    let stopped = p.stopped();
    let (status, url) = (p.http_status(), p.url().unwrap_or_default());
    let (mut title, mut text) = (String::new(), String::new());
    if idle.is_ok() || at_end || stopped.is_some() {
        title = locate::title(p).unwrap_or_default();
        text = match p.browser().map(|b| b.run_script(BODY_TEXT).map(|v| b.it.display(v))) {
            Some(Ok(t)) => locate::norm(&t),
            _ => String::new(),
        };
        p.touched();
    }
    let waits = if at_end && idle.is_err() { waits(&p.pending()) } else { String::new() };
    Look { ran, next, idle, fail, stopped, doc: p.generation(), status, url: safe_url(&url), title: locate::norm(&title), text, waits, logs: p.take_logs() }
}

/// What holds a page that is not ready, for the `not ready` note: the number of requests, then up to 2 of the things that hold it,
/// the oldest first. For example `1 request pending: fetch GET https://x/api (19.4 s)`.
fn waits(all: &[bare::page::Waiting]) -> String {
    let held: Vec<&bare::page::Waiting> = all.iter().filter(|w| w.holds).collect();
    let n = held.iter().filter(|w| !w.url.is_empty()).count();
    let mut out = format!("{n} request{} pending", if n == 1 { "" } else { "s" });
    if !held.is_empty() {
        let named: Vec<String> = held.iter().take(2).map(|w| crate::diag::waiting(w)).collect();
        out += &format!(": {}", named.join(", "));
        if held.len() > 2 {
            out += &format!(", {} more", held.len() - 2);
        }
    }
    out
}

/// One try of a step, on the page thread.
fn attempt(p: &mut Page, m: &mut Memo, a: &Action, out: Option<&PathBuf>, red: &Redactor) -> Try {
    let r = (|| -> Result<Option<String>, Try> {
        match a {
            Action::Goto(url) => p.navigate(Some(url)).map_err(|e| Try::Stop(fail("page", e)))?,
            Action::Reload => p.navigate(None).map_err(|e| Try::Stop(fail("page", e)))?,
            Action::Back => p.go(-1).map_err(|e| Try::Stop(fail("page", e)))?,
            Action::Forward => p.go(1).map_err(|e| Try::Stop(fail("page", e)))?,
            Action::Click(loc) => {
                let h = one(p, m, loc)?;
                let link = link_target(p, h);
                p.click(h).map_err(|r| refused(p, h, r))?;
                m.link = link;
            }
            Action::Type(loc, text) => {
                if m.keys.is_none() {
                    // The page may have replaced the field after an earlier `type`: the keys go to the new focused field.
                    let h = one_or_replaced(p, m, loc)?;
                    if p.is_password(h) {
                        m.typed.push(text.clone());
                    }
                    // The page can put a new field in the place of `h` on the click: the keys go to that one.
                    let h = p.type_start(h, text.is_empty()).map_err(|r| refused(p, h, r))?;
                    m.keys = Some((h, text.chars().rev().collect()));
                }
                let (h, keys) = m.keys.as_mut().expect("set");
                // A field that the page replaced while it was typed (it loads its widget on focus): the keys go on in the new one.
                if !p.focused(*h)
                    && let Some(new) = p.replaced(*h)
                {
                    *h = new;
                }
                let h = *h;
                // A page script can hand the focus to another field: the keys would go there. Stop, and say where the text went.
                let lost = |p: &mut Page, done: bool| {
                    let now = p.active().map_or_else(|| "nothing".to_string(), |a| p.describe(a));
                    Try::Stop(fail("focus_lost", format!("{} lost the focus to {now} {}: the text is not all in it", p.describe(h), if done { "while it was typed" } else { "before the next key" })))
                };
                // A line feed is Enter: it can submit the form and take the field away, so a text with one is not checked.
                let watch = !text.contains('\n');
                if watch && !p.focused(h) {
                    m.keys = None;
                    return Err(lost(p, false));
                }
                if let Some(c) = keys.pop() {
                    p.type_char(c);
                }
                if watch && !p.focused(h) && p.replaced(h).is_none() {
                    m.keys = None;
                    return Err(lost(p, true));
                }
                if !keys.is_empty() {
                    return Err(Try::More);
                }
                m.keys = None;
                p.after_action();
            }
            Action::Select(loc, wanted) => {
                let h = one(p, m, loc)?;
                p.select(h, wanted).map_err(|r| refused(p, h, r))?;
            }
            Action::Check(loc, on) => {
                let h = one(p, m, loc)?;
                p.set_checked(h, *on).map_err(|r| refused(p, h, r))?;
            }
            Action::Hover(loc) => {
                let h = one(p, m, loc)?;
                p.hover(h).map_err(|r| refused(p, h, r))?;
            }
            Action::DblClick(loc) => {
                let h = one(p, m, loc)?;
                p.dblclick(h).map_err(|r| refused(p, h, r))?;
            }
            Action::Upload(loc, paths) => {
                let h = one(p, m, loc)?;
                p.upload(h, paths).map_err(|r| refused(p, h, r))?;
            }
            Action::Press(loc, key) => {
                // The rule of `type`: when the page put a new text field in the place of the target, the key goes to that field.
                let h = match loc {
                    Some(l) => Some(one_or_replaced(p, m, l)?),
                    None => None,
                };
                let h = match h {
                    Some(h) => match p.focus(h) {
                        Ok(()) => Some(h),
                        // The focus itself made the page swap the field (it loads its widget on focus).
                        Err(r) => Some(p.replaced(h).ok_or_else(|| refused(p, h, r))?),
                    },
                    None => None,
                };
                let before = (p.generation(), p.url(), p.revision(), p.text_revision());
                p.press(key).map_err(|e| Try::Stop(fail("not_found", e)))?;
                // A page that took the key changed (its tree, a field, the focus) or began to load another one.
                if (p.generation(), p.url(), p.revision(), p.text_revision()) == before && !p.loading() {
                    return Ok(Some(format!("no effect: {key} changed nothing{}", h.map_or_else(String::new, |h| format!(" at {}", p.describe(h))))));
                }
            }
            Action::Scroll(loc, dy) => {
                let h = loc.as_ref().map(|l| one(p, m, l)).transpose()?;
                if !p.wheel(h, 0.0, *dy).map_err(|r| refused(p, h.expect("a refusal is about an element"), r))? {
                    return Ok(Some("nothing scrolled".into()));
                }
            }
            Action::Expect(c) => check(p, m, c)?,
            Action::CookieSet { name, value, domain, path } => cookie_set(p, name, value, domain.as_deref(), path.as_deref())?,
            Action::Shot(name) => {
                let Some(dir) = out else { return Ok(Some("no --out: not saved".into())) };
                if p.loading() {
                    return Err(Try::Again(fail("shot", "the page is loading")));
                }
                if p.images_pending() {
                    return Err(Try::Again(fail("shot", "pictures are loading")));
                }
                if !shown(p, red) {
                    return Ok(Some("withheld: a secret is on the page".into()));
                }
                return Ok(Some(save(&p.capture(), dir, name)));
            }
            Action::SaveState(file) => {
                let st = p.storage().ok_or_else(|| Try::Stop(fail("page", "the page has no session")))?.export();
                crate::state::write(file, &st).map_err(|e| Try::Stop(fail("state", e)))?;
                let origins: std::collections::HashSet<&String> = st.local.iter().chain(&st.session).map(|(o, _)| o).collect();
                return Ok(Some(format!("saved {} cookies, {} origins", st.cookies.len(), origins.len())));
            }
            Action::Open(..) | Action::Timeout(_) => unreachable!("the driver runs these"),
        }
        Ok(None)
    })();
    match r {
        Ok(note) => Try::Done(note),
        Err(t) => t,
    }
}

/// Is `h` in a link (`<a>` or `<area>` with `href`) that leads to another address of this tab? Not for a new tab (`target`), a
/// download, a `javascript:` URL, or the same address with another fragment.
fn link_target(p: &mut Page, h: Handle) -> bool {
    let Some(here) = p.url() else { return false };
    let Some(b) = p.browser() else { return false };
    let doc = b.document();
    let d = doc.borrow();
    let attr = |n, name: &str| d.find_atom(name).and_then(|a| d.attr(n, a)).map(str::to_string);
    let mut cur = Some(h.node);
    while let Some(n) = cur {
        if !d.is_foreign(n) && matches!(d.element_name(n), "a" | "area") && let Some(href) = attr(n, "href") {
            let other_tab = attr(n, "target").is_some_and(|t| !matches!(t.trim().to_ascii_lowercase().as_str(), "" | "_self" | "_top" | "_parent"));
            if other_tab || attr(n, "download").is_some() || href.trim_start().to_ascii_lowercase().starts_with("javascript:") {
                return false;
            }
            let base = |u: &str| u.split('#').next().unwrap_or("").to_string();
            // `Url::join` resolves the href against the page, as `new URL(href, location)` does in JS.
            return bare_url::Url::parse(&here).ok().and_then(|u| u.join(&href).ok()).is_some_and(|to| base(&to.to_string()) != base(&here));
        }
        cur = d.parent(n);
    }
    false
}

/// The host and the path of the page, for the jar.
fn page_at(p: &mut Page) -> Option<(String, String)> {
    let u = bare_url::Url::parse(p.url()?.split('#').next()?).ok()?;
    Some((u.host.clone(), u.path.split('?').next().unwrap_or("/").to_string()))
}

/// `cookie set`: put a cookie in the jar of the session as a response of the host would. The value is never in a message.
fn cookie_set(p: &mut Page, name: &str, value: &str, domain: Option<&str>, path: Option<&str>) -> Result<(), Try> {
    let jar = p.storage().ok_or_else(|| Try::Stop(fail("page", "the page has no session")))?.jar();
    let host = match domain {
        Some(d) => d.trim_start_matches('.').to_ascii_lowercase(),
        None => page_at(p).ok_or_else(|| Try::Stop(fail("page", "cookie set needs a page: the first step must be open")))?.0,
    };
    let text = format!("{name}={value}; Path={}", path.unwrap_or("/"));
    let at = bare_net::cookie::At { secure: true, host: &host, path: path.unwrap_or("/") };
    let mut j = jar.lock().unwrap_or_else(|e| e.into_inner());
    let now = j.now();
    j.set(&at, &text, bare_net::cookie::Via::Http { third: false }, now);
    if j.lookup(&at, name, now).is_none() {
        return Err(Try::Stop(fail("cookie", format!("the jar refused the cookie {name} for {host} (a public suffix, a bad name or a bad prefix)"))));
    }
    Ok(())
}

fn check(p: &mut Page, m: &mut Memo, c: &Check) -> Result<(), Try> {
    let no = |msg: String| Err(Try::Again(fail("expect", msg)));
    match c {
        Check::Cookie(name, state) => {
            let Some((host, path)) = page_at(p) else { return Err(Try::Again(fail("page", p.no_document()))) };
            let Some(jar) = p.storage().map(|s| s.jar()) else { return Err(Try::Stop(fail("page", "the page has no session"))) };
            let j = jar.lock().unwrap_or_else(|e| e.into_inner());
            let secure = p.url().is_some_and(|u| u.starts_with("https:")) || bare_net::cookie::loopback(&host);
            let found = j.lookup(&bare_net::cookie::At { secure, host: &host, path: &path }, name, j.now());
            // A message names the cookie and what was asked, never the value of the cookie.
            match (state, found) {
                (CookieState::Present, None) => return no(format!("no cookie {name} would go to this page")),
                (CookieState::Absent, Some(_)) => return no(format!("the cookie {name} goes to this page, and it should not")),
                (CookieState::Value(t), Some(c)) if !t.matches(&c.value) => return no(format!("the value of the cookie {name} is not {t}")),
                (CookieState::Value(t), None) => return no(format!("no cookie {name} would go to this page (expected value{t})")),
                _ => {}
            }
        }
        Check::Url(t) => {
            let url = p.url().unwrap_or_default();
            if !t.matches(&url) {
                return no(format!("expected url{t}, the URL is {}", safe_url(&url)));
            }
        }
        Check::Title(t) => {
            let title = locate::title(p).unwrap_or_default();
            if !t.matches(&title) {
                return no(format!("expected title{t}, the title is {:?}", short(&title)));
            }
        }
        Check::Js(code) => {
            if p.browser().is_none() {
                return Err(Try::Again(fail("page", p.no_document())));
            }
            let b = p.browser().expect("checked");
            let r = b.run_script(&format!("!!({code}\n)"));
            p.touched();
            match r {
                Ok(bare_js::value::Value::Bool(true)) => {}
                Ok(_) => return no(format!("the script gave false: {code}")),
                Err(e) => {
                    let m = thrown(p.browser().expect("checked"), &e);
                    return no(format!("the script threw: {}", short(&m)));
                }
            }
        }
        Check::El(loc, State::Hidden) => {
            let found = found_in(p, m, loc)?;
            let shown: Vec<Handle> = found.shown.into_iter().filter(|&h| p.visible_box(h).is_ok()).collect();
            if let Some(&h) = shown.first() {
                return no(format!("expected {loc} hidden, {} is rendered", p.describe(h)));
            }
        }
        Check::El(loc, State::Count(n)) => {
            let found = found_in(p, m, loc)?;
            if found.shown.len() != *n {
                return no(format!("expected {n} of {loc}, found {}", found.shown.len()));
            }
        }
        Check::El(loc, state) => {
            let h = one(p, m, loc)?;
            let what = p.describe(h);
            let (good, got) = match state {
                State::Visible => match p.visible_box(h) {
                    Ok(_) => (true, String::new()),
                    Err(Refusal::Hidden(why)) => (false, format!("hidden: {why}")),
                    Err(r) => return Err(refused(p, h, r)),
                },
                State::Enabled => (!p.disabled(h), "disabled".into()),
                State::Disabled => (p.disabled(h), "enabled".into()),
                State::Checked => (p.checked(h), "unchecked".into()),
                State::Unchecked => (!p.checked(h), "checked".into()),
                State::Focused => (p.focused(h), "not focused".into()),
                State::Text(t) => {
                    let s = locate::text(p, h).unwrap_or_default();
                    (t.matches(&s), format!("its text is {:?}", short(&s)))
                }
                State::Value(t) => {
                    let s = p.value(h).unwrap_or_default();
                    (t.matches(&s), format!("its value is {:?}", short(&s)))
                }
                State::Hidden | State::Count(_) => unreachable!("above"),
            };
            if !good {
                return no(format!("expected {loc} {}: {what} is {got}", state_name(state)));
            }
        }
    }
    Ok(())
}

fn state_name(s: &State) -> String {
    match s {
        State::Visible => "visible".into(),
        State::Hidden => "hidden".into(),
        State::Enabled => "enabled".into(),
        State::Disabled => "disabled".into(),
        State::Checked => "checked".into(),
        State::Unchecked => "unchecked".into(),
        State::Focused => "focused".into(),
        State::Count(n) => format!("count {n}"),
        State::Text(t) => format!("text{t}"),
        State::Value(t) => format!("value{t}"),
    }
}

/// A JSON string.
fn q(s: &str) -> String {
    let mut out = String::from('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The fields of `t` for a JSON object.
fn ticks_json(t: &Ticks) -> String {
    format!("\"ticks\":{},\"tick_max_ms\":{:.1},\"ticks_over_16\":{},\"ticks_over_50\":{}", t.n, t.max.as_secs_f64() * 1000.0, t.over_16, t.over_50)
}

impl Report {
    /// The result as JSON. Each text goes through `red` here, so a secret learned late is also hidden in the earlier records.
    pub fn json(&self, red: &Redactor) -> String {
        let s = |t: &str| q(&red.clean(t));
        let steps: Vec<String> = self
            .steps
            .iter()
            .map(|x| {
                format!(
                    "{{\"line\":{},\"step\":{},\"ok\":{},\"ms\":{:.1},\"busy_ms\":{:.1},\"waited_ms\":{:.1},\"cpu_ms\":{:.1},\"net_ms\":{:.1},\"tries\":{},{}{}}}",
                    x.line,
                    s(&x.text),
                    x.ok,
                    x.ms,
                    x.busy_ms,
                    x.waited_ms,
                    x.cpu_ms,
                    x.net_ms,
                    x.tries,
                    ticks_json(&x.ticks),
                    x.note.as_ref().map_or(String::new(), |n| format!(",\"note\":{}", s(n)))
                )
            })
            .collect();
        let failure = self.failure.as_ref().map_or("null".to_string(), |(line, f, shot)| {
            format!("{{\"line\":{line},\"kind\":{},\"message\":{},\"capture\":{}}}", q(f.kind), s(&f.message), shot.as_ref().map_or("null".into(), |c| s(c)))
        });
        let logs: Vec<String> = self.logs.iter().map(|(line, l)| format!("{{\"line\":{line},\"level\":{},\"text\":{}}}", q(l.level), s(&l.text))).collect();
        let net: Vec<String> = self
            .net
            .iter()
            .map(|(line, r)| {
                format!(
                    "{{\"line\":{line},\"url\":{},\"status\":{},\"bytes\":{},\"ms\":{:.1},\"at_ms\":{:.1},\"phases_ms\":[{:.1},{:.1},{:.1},{:.1},{:.1},{:.1}],\"reused\":{},\"h2\":{}{}}}",
                    s(&r.url),
                    r.status,
                    r.bytes,
                    (r.wait + r.dns + r.tcp + r.tls + r.ttfb + r.body) as f64 / 1000.0,
                    r.at as f64 / 1000.0,
                    r.wait as f64 / 1000.0,
                    r.dns as f64 / 1000.0,
                    r.tcp as f64 / 1000.0,
                    r.tls as f64 / 1000.0,
                    r.ttfb as f64 / 1000.0,
                    r.body as f64 / 1000.0,
                    r.reused,
                    r.h2,
                    r.error.as_ref().map_or(String::new(), |e| format!(",\"error\":{}", s(e)))
                )
            })
            .collect();
        let cookies: Vec<String> = self
            .cookies
            .iter()
            .map(|c| {
                format!(
                    "{{\"name\":{},\"domain\":{},\"host_only\":{},\"path\":{},\"secure\":{},\"http_only\":{},\"same_site\":{},\"session\":{}}}",
                    s(&c.name),
                    s(&c.domain),
                    c.host_only,
                    s(&c.path),
                    c.secure,
                    c.http_only,
                    q(c.same_site),
                    c.session
                )
            })
            .collect();
        format!(
            "{{\"ok\":{},\"window\":{},\"ms\":{:.1},{},\"steps\":[{}],\"failure\":{},\"console\":[{}],\"network\":[{}],\"cookies\":[{}]}}",
            self.ok,
            self.window,
            self.ms,
            ticks_json(&self.ticks),
            steps.join(","),
            failure,
            logs.join(","),
            net.join(","),
            cookies.join(",")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction() {
        let mut r = Redactor::default();
        r.add("Pa ss@1");
        r.add("Admin@Example.test");
        assert_eq!(r.clean("x Pa ss@1 y pa ss@1"), "x [secret] y [secret]");
        assert_eq!(r.clean("q=Pa%20ss%401&f=Pa+ss%401"), "q=[secret]&f=[secret]");
        assert_eq!(r.clean("hello admin@example.test"), "hello [secret]");
        assert!(r.hits("ADMIN@EXAMPLE.TEST") && !r.hits("admin"));
        r.add("Ünï");
        assert_eq!(r.clean("x Ünï y ünï"), "x [secret] y ünï");
        assert_eq!(Redactor::default().clean("a"), "a");
        assert_eq!(safe_url("http://a.test/x?token=s3cr3t&b=2&Auth_Key=z#t"), "http://a.test/x?token=…&b=2&Auth_Key=…");
    }

    #[test]
    fn not_ready_names_what_holds_the_page() {
        let w = |kind, url: &str, ms, holds| bare::page::Waiting { kind, method: if url.is_empty() { "" } else { "GET" }, url: url.into(), age: Duration::from_millis(ms), holds, state: "" };
        assert_eq!(waits(&[]), "0 requests pending");
        // A picture never holds the page: it is not named.
        assert_eq!(waits(&[w("image", "http://a.test/p.png", 9000, false)]), "0 requests pending");
        assert_eq!(waits(&[w("fetch", "http://a.test/api?token=s", 19400, true)]), "1 request pending: fetch GET http://a.test/api?token=… (19.4 s)");
        let three = [w("xhr", "http://a.test/a", 3000, true), w("script", "http://a.test/b.js", 2000, true), w("inline script", "", 0, true)];
        assert_eq!(waits(&three), "2 requests pending: xhr GET http://a.test/a (3.0 s), script GET http://a.test/b.js (2.0 s), 1 more");
        assert_eq!(waits(&three[2..]), "0 requests pending: inline script");
        let mut done = w("fetch", "http://a.test/api", 5800, true);
        done.state = "reply in";
        assert_eq!(waits(&[done]), "1 request pending: fetch GET http://a.test/api (5.8 s, reply in)");
    }
}
