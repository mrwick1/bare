//! Run the steps. Each step is tried, and tried again until it passes or its time limit ends: between tries the driver runs the tasks of
//! the page that are due (`Page::tick`) and sleeps until the next one, so a step waits for a condition and never for a fixed time. When
//! no task is queued and no reply or document is on its way, nothing can change the page, so the step fails at once.
//!
//! `open`, `goto` and `reload` return at once, as a link click does: the old document ends, and the ticks of the next steps download,
//! parse and run the new one. Until it is in, the page has no document, so a locator or an `expect` waits.
//!
//! Failure kinds: `not_found`, `duplicate` (a locator must match one element), `hidden`, `disabled`, `covered`, `detached` (it matched,
//! then left the document), `not_editable`, `no_focus`, `expect` (a check was false), `page` (a load failed, the page stopped or has no
//! layout). `duplicate`, `not_editable`, `no_focus` and a failed load fail at once; the others wait.
//!
//! Secrets: the values of `--secrets` and each text typed into a password field are replaced by `[secret]` in all the output (also in
//! lower case and URL-encoded). A capture is withheld when a secret is in the text or in a field of the page (a password field shows
//! dots, so it does not count).

use crate::locate::{self, Locator};
use crate::steps::{Action, Check, CookieState, State, Step};
use bare::control::{Handle, Log, Refusal, Remote, Ticks};
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
    fn open(&mut self, url: &str, size: (u32, u32), window: bool, net: &bare_net::http::Client, state: Option<StorageState>) -> Result<(), String> {
        match self {
            // The new page takes the size of the window, not `size`: the compositor can have resized the window, and the window loop
            // calls `resize` only when the window size changes.
            Host::Window(r) => {
                let (url, net) = (url.to_string(), net.fork());
                r.call(move |p| p.reopen_in_with(&url, &net, state.as_ref()))?
            }
            _ if window => {
                *self = Host::Window(Remote::open_with(url, size, net, state)?);
                Ok(())
            }
            _ => {
                // The old page ends first, so the two pages do not share the peak of memory. A failed open leaves no page.
                *self = Host::None;
                let mut p = Page::open_in_with(url, size, net, state.as_ref())?;
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
    keys: Option<Vec<char>>,
    /// The refs that snapshots minted for the document of now.
    refs: crate::snap::Refs,
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
    pub rep: Report,
}

impl Session {
    pub fn new(opt: &Options) -> Session {
        bare_net::trace::enable();
        let _ = bare_net::trace::take();
        let rep = Report { ok: true, steps: Vec::new(), failure: None, logs: Vec::new(), net: Vec::new(), ms: 0.0, window: opt.window, ticks: Ticks::default(), cookies: Vec::new() };
        Session { host: Host::None, net: bare_net::http::Client::new(), memo: Memo::default(), timeout: opt.timeout, window_cpu: 0, nav_first: 0, start: Instant::now(), preload: None, hist: Default::default(), rep }
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

    /// A PNG of the page, or of the element of ref `r` (its box, clipped to the window), at `path`. Waits up to the step time limit for
    /// the document and its pictures. Refused when a secret shows on the page.
    pub fn screenshot(&mut self, path: PathBuf, r: Option<u32>, red: &Redactor) -> Result<String, String> {
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
            let clip = match r {
                Some(r) => {
                    let h = refs.get(r, p.generation())?;
                    Some(p.visible_box(h).map(|b| (b.x, b.y, b.w, b.h)).map_err(|e| format!("e{r} has no visible box: {e:?}"))?)
                }
                None => None,
            };
            let c = p.capture();
            let c = match clip {
                Some(b) => crop(c, b),
                None => c,
            };
            bare_paint::png::write_file(&c, &path).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(format!("{} ({}x{})", path.display(), c.width, c.height))
        })?
    }

    /// Run a script in the page and give its value as text (`JSON.stringify` it for an object).
    pub fn eval(&mut self, code: String) -> Result<String, String> {
        self.with_refs(move |p, _| {
            let _ = p.wait_load(Duration::from_secs(10));
            let b = p.browser().ok_or("no document")?;
            let r = b.run_script(&code);
            let out = match r {
                Ok(v) => Ok(b.it.display(v)),
                Err(e) => Err(format!("the script threw: {}", short(&format!("{e:?}")))),
            };
            p.touched();
            let _ = p.tick();
            out
        })?
    }

    /// The page facts for `diag::report`, after a tick that collects what arrived.
    pub fn diagnose(&mut self, red: &Redactor) -> Result<String, String> {
        let f = self.with_refs(|p, _| {
            crate::snap::settle(p);
            let cpu = thread_cpu_ns();
            let (nodes, shown) = crate::snap::size(p).unwrap_or((0, 0));
            crate::diag::Facts { url: p.url().unwrap_or_default(), load_error: p.load_error().map(str::to_string), stopped: p.stopped(), nodes, shown, cpu_ms: cpu as f64 / 1e6 }
        })?;
        // The logs that the tick made are in the history after `with_refs`: read the report from it now.
        Ok(crate::diag::report(&self.hist, &f, &|s| red.clean(s)))
    }

    /// Collect what the page logged and requested since the last call (a tick), into the history.
    pub fn collect(&mut self) -> Result<(), String> {
        self.with_refs(|p, _| {
            let _ = p.tick();
        })
    }

    /// The URL of the page of now.
    pub fn url(&mut self) -> Option<String> {
        self.host.run(|p| p.url()).ok().flatten()
    }

    /// A copy of the storage of the open page (it holds live credentials). `None` with no page.
    pub fn export_state(&mut self) -> Option<StorageState> {
        self.host.run(|p| p.storage().map(|s| s.export())).ok().flatten()
    }

    /// Run one step, and add its result to `rep`. False when it failed (`rep.failure` says why).
    pub fn step(&mut self, step: &Step, opt: &Options, red: &mut Redactor) -> bool {
        let Session { host, net, memo, timeout, window_cpu, nav_first, rep, preload, hist, .. } = self;
    let t0 = Instant::now();
    let deadline = t0 + *timeout;
    let (mut tries, mut busy, mut waited, mut ticks) = (0u32, Duration::ZERO, Duration::ZERO, Ticks::default());
    let (cpu0, net0) = (thread_cpu_ns(), bare_net::trace::blocked_us());
    let (log0, net_at) = (rep.logs.len(), rep.net.len());
    if matches!(step.action, Action::Open(..) | Action::Goto(_) | Action::Reload) {
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
            Action::Open(url, file) => {
                memo.seen.clear();
                let from = file.as_deref().map(crate::state::read).transpose().map(|st| st.or_else(|| preload.take()));
                let opened = from.and_then(|st| host.open(url, opt.size, opt.window, net, st)).and_then(|_| host.run(|p| p.take_logs()));
                let d = t.elapsed();
                busy += d;
                if !opt.window {
                    ticks.add(d);
                }
                break opened.map(|logs| rep.logs.extend(logs.into_iter().map(|l| (step.line, l)))).map(|_| None).map_err(|e| fail("page", e));
            }
            a => {
                let (a, mut m) = (a.clone(), std::mem::take(memo));
                let (out, ws) = (opt.out.clone(), red.clone());
                match host.run(move |p| {
                    let mut tried = attempt(p, &mut m, &a, out.as_ref(), &ws);
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
                    (m, tried, wait, p.take_logs())
                }) {
                    Ok((m, tried, wait, logs)) => {
                        *memo = m;
                        for s in memo.typed.drain(..) {
                            red.add(&s);
                        }
                        rep.logs.extend(logs.into_iter().map(|l| (step.line, l)));
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
            Try::Done(note) => break Ok(note),
            Try::More => {}
            Try::Stop(f) => break Err(f),
            Try::Again(f) => {
                let now = Instant::now();
                let next = match wait {
                    (true, _) => now,
                    (false, Some(at)) => at,
                    (false, None) => {
                        break Err(Fail { message: format!("{} (the page is idle: no task is queued, so nothing can change it)", f.message), ..f });
                    }
                };
                if now >= deadline {
                    break Err(Fail { message: format!("{} (time limit {} ms)", f.message, timeout.as_millis()), ..f });
                }
                host.sleep(seen, next.min(deadline));
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

    /// Move out the steps, logs, network records and failure so far, and start a new part of the report. The page stays open. A caller
    /// that sends steps one call at a time gives each call its own report, and the session does not keep old records.
    pub fn drain(&mut self) -> Report {
        self.report();
        let ok = std::mem::replace(&mut self.rep.ok, true);
        let r = &mut self.rep;
        Report { ok, steps: std::mem::take(&mut r.steps), failure: r.failure.take(), logs: std::mem::take(&mut r.logs), net: std::mem::take(&mut r.net), ms: r.ms, window: r.window, ticks: std::mem::take(&mut r.ticks), cookies: std::mem::take(&mut r.cookies) }
    }

    /// Close the page (and its window) and give the report.
    pub fn finish(mut self) -> Report {
        self.report();
        drop(self.host);
        self.rep.ms = self.start.elapsed().as_secs_f64() * 1000.0;
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

/// The URL for a message: no fragment, and the values of the query hidden (a query can hold a token).
pub fn safe_url(u: &str) -> String {
    let u = u.split('#').next().unwrap_or("");
    match u.split_once('?') {
        Some((path, q)) => format!("{path}?{}", q.split('&').map(|kv| kv.split('=').next().unwrap_or("").to_string() + "=…").collect::<Vec<_>>().join("&")),
        None => u.to_string(),
    }
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
        [] => Err(Try::Again(match m.seen.get(&key) {
            Some(&old) if old.doc == p.generation() && !p.connected(old) => fail("detached", format!("{loc} matched {} before; it left the document", p.describe(old))),
            _ if found.hidden > 0 => fail("hidden", format!("{loc}: {} element(s) match, none is rendered (display: none, visibility: hidden or aria-hidden)", found.hidden)),
            _ => fail("not_found", format!("no element matches {loc}")),
        })),
        many => {
            let list: Vec<String> = many.iter().take(5).map(|&h| p.describe(h)).collect();
            Err(Try::Stop(fail("duplicate", format!("{loc} matches {} elements: {}{}", many.len(), list.join(", "), if many.len() > 5 { ", …" } else { "" }))))
        }
    }
}

fn refused(p: &mut Page, h: Handle, r: Refusal) -> Try {
    let what = p.describe(h);
    match r {
        Refusal::Detached => Try::Again(fail("detached", format!("{what} left the document"))),
        Refusal::Hidden(why) => Try::Again(fail("hidden", format!("{what} is hidden: {why}"))),
        Refusal::Disabled => Try::Again(fail("disabled", format!("{what} is disabled"))),
        Refusal::Covered { by, at } => Try::Again(fail("covered", format!("{what} is covered by {by} at ({}, {})", at.0, at.1))),
        Refusal::NotEditable => Try::Stop(fail("not_editable", format!("{what} is not a text field"))),
        Refusal::NoFocus => Try::Stop(fail("no_focus", format!("{what} did not take the focus after the click"))),
        Refusal::NoPage(e) => Try::Again(fail("page", e)),
        Refusal::Wrong(kind, why) => Try::Stop(fail(kind, why)),
    }
}

/// One try of a step, on the page thread.
fn attempt(p: &mut Page, m: &mut Memo, a: &Action, out: Option<&PathBuf>, red: &Redactor) -> Try {
    let r = (|| -> Result<Option<String>, Try> {
        match a {
            Action::Goto(url) => p.navigate(Some(url)).map_err(|e| Try::Stop(fail("page", e)))?,
            Action::Reload => p.navigate(None).map_err(|e| Try::Stop(fail("page", e)))?,
            Action::Click(loc) => {
                let h = one(p, m, loc)?;
                p.click(h).map_err(|r| refused(p, h, r))?;
            }
            Action::Type(loc, text) => {
                if m.keys.is_none() {
                    let h = one(p, m, loc)?;
                    if p.is_password(h) {
                        m.typed.push(text.clone());
                    }
                    p.type_start(h, text.is_empty()).map_err(|r| refused(p, h, r))?;
                    m.keys = Some(text.chars().rev().collect());
                }
                let keys = m.keys.as_mut().expect("set");
                if let Some(c) = keys.pop() {
                    p.type_char(c);
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
            Action::Press(key) => p.press(key).map_err(|e| Try::Stop(fail("not_found", e)))?,
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
                Err(e) => return no(format!("the script threw: {}", short(&format!("{e:?}")))),
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
        assert_eq!(safe_url("http://a.test/x?q=s3cr3t&b=2#t"), "http://a.test/x?q=…&b=…");
    }
}
