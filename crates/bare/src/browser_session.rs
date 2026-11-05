//! The document session: the outer driver that loads a document, runs it with [`Browser::run_page`], and acts on the
//! navigations that its scripts ask for. One `Session` is one browsing session: the same [`WebStorage`] (local and
//! session storage) lives through every document, and nothing else does.
//!
//! One document = one fresh `Browser` (a new JS realm, a new DOM, new timers, a new `fetch` queue, a new client).
//! `location.assign/replace/reload`, `history.go(0)` and a changed `location.href` end the run of the old document at
//! once ([`Stop::Navigate`]): no old script, timer, event or `fetch` callback runs after the request. The old `Browser` is
//! dropped BEFORE the next document is requested, so a heap is never held twice. A navigation to another origin gets the
//! storage of that origin (storage is keyed by origin) and a `fetch` that reaches only that origin.
//! `history.pushState/replaceState`, fragment changes and `back/forward` inside one document stay in `run_page`.
//!
//! Bounds (`SessionLimits`), all shared by the documents of one run: documents, wall time, tasks and rounds in total
//! (each document gets what is left), redirects of one navigation, bytes of one document. A limit ends the session with
//! its own [`SessionStop`]; the reason of the last document is kept (`Page`). A fatal error of a document ends the
//! session and nothing is loaded after it, also when a navigation is queued.
//!
//! `Session::resume(&[Input])` goes on with the settled document: the existing `click`, `type_text` and `submit` primitives,
//! then the same rounds of `run_page` (no reload, no second loop). The budgets above are session-wide: documents, tasks,
//! rounds and time (time inside `run` and `resume` only) stay spent. Every stop but `Settled` ends the session for good.
//!
//! Document load: GET, redirects followed here one by one (`Client::send_once`: a redirect is checked before any socket
//! opens): at most `max_redirects`, http or https only, no downgrade from https to http. The status must be 2xx (a
//! 404 page is not run), `Content-Type` `text/html` (XML/XHTML is unsupported), no `Content-Encoding`, at most
//! `max_document_bytes`. The text is decoded by `bare_html::encoding`. Gates: no request body, so no form POST; no cookies
//! (the client has none); no `Referer`; no cross-document history (`back` from the first entry of a document stays
//! in that document); a failed load ends the session (the old page stays gone, as a browser error page would).
//! Results and diagnostics name `scheme://host:port/path` only: no query, fragment, header, body or source.

use crate::browser::{Browser, BrowserLimits};
use crate::browser_fetch::FetchLimits;
use crate::browser_loader::{Input, PageConfig, PageLimits, PageResult, Stop, deadline_after, label};
use crate::browser_navigation::{NavLimits, WebStorage};
use bare_html::{encoding, tree};
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_net::http::{Client, Request};
use bare_url::Url;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct SessionLimits {
    /// Documents loaded in one run (the first one counts).
    pub max_documents: usize,
    /// Redirects of one navigation.
    pub max_redirects: usize,
    /// Bytes of one document body.
    pub max_document_bytes: usize,
    /// Wall time of the whole run.
    pub max_time: Duration,
    /// Tasks and rounds of all documents together.
    pub max_tasks: usize,
    pub max_rounds: usize,
}

impl Default for SessionLimits {
    fn default() -> Self {
        SessionLimits { max_documents: 8, max_redirects: 10, max_document_bytes: 8 << 20, max_time: Duration::from_secs(60), max_tasks: 200_000, max_rounds: 200_000 }
    }
}

pub struct SessionConfig {
    /// The first document (http or https). A fragment is kept for `location.hash`.
    pub url: String,
    /// Makes the client of a document (document download, script download, `fetch`: three clients each). A test passes
    /// a `Client::with_roots` factory.
    pub client: Box<dyn FnMut() -> Client>,
    /// The limits of each document. `max_time`, `max_tasks` and `max_rounds` are cut to what the session has left.
    pub page: PageLimits,
    pub fetch: FetchLimits,
    pub limits: SessionLimits,
    pub browser: BrowserLimits,
    /// The evaluator limits of each document. `new` raises the depth caps to their ceilings (768 frames, 256 calls).
    pub eval: EvalLimits,
    pub heap: HeapLimits,
    /// Override the browser's default media-query viewport, in CSS pixels.
    pub viewport: Option<(f32, f32)>,
    /// Timers use the virtual clock (a test finishes at once).
    pub virtual_clock: bool,
    /// The storage to start from (a session that goes on). `None`: an empty one.
    pub storage: Option<WebStorage>,
}

impl SessionConfig {
    pub fn new(url: &str) -> SessionConfig {
        SessionConfig {
            url: url.to_string(),
            client: Box::new(Client::new),
            page: PageLimits::default(),
            fetch: FetchLimits::default(),
            limits: SessionLimits::default(),
            browser: BrowserLimits::default(),
            eval: EvalLimits { max_traversal_depth: 768, max_call_depth: 256, max_statement_frames: 768, ..Default::default() },
            heap: HeapLimits::default(),
            viewport: None,
            virtual_clock: false,
            storage: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionStop {
    /// The last document stopped for this reason: `Settled`, a page budget or `Fatal`. (`Navigate` never is the
    /// reason: the session acts on it.)
    Page(Stop),
    /// A navigation was asked for, and `max_documents` documents were loaded.
    DocumentLimit,
    TimeBudget,
    TaskBudget,
    RoundBudget,
    /// The document could not be loaded. A fixed label, never a URL query or a body.
    LoadFailed(String),
}

#[derive(Debug)]
pub struct SessionResult {
    pub stop: SessionStop,
    /// One result for each document that ran, in order, and its `scheme://host:port/path`.
    pub pages: Vec<PageResult>,
    pub urls: Vec<String>,
    pub tasks: usize,
    pub rounds: usize,
}

pub struct Session {
    cfg: SessionConfig,
    store: WebStorage,
    /// The last document. Dropped before the next one is requested.
    browser: Option<Browser>,
    /// What the session has used so far, shared by `run` and every `resume`.
    used: Used,
}

#[derive(Default)]
struct Used {
    documents: usize,
    tasks: usize,
    rounds: usize,
    /// Time spent inside `run` and `resume`. Time between calls is not charged.
    time: Duration,
    /// Why the session cannot go on. Set by every stop but `Settled` and `NoTarget`.
    ended: Option<SessionStop>,
}

fn is_html(content_type: Option<&str>) -> bool {
    let m = content_type.unwrap_or("").split(';').next().unwrap_or("").trim();
    m.eq_ignore_ascii_case("text/html")
}

/// May a redirect (or a navigation) go from `from` to `to`? http(s) only, and https never goes down to http.
pub(crate) fn redirect_allowed(from: &Url, to: &Url) -> bool {
    use bare_url::Scheme;
    !(from.scheme == Scheme::Https && to.scheme == Scheme::Http)
}

/// Download one document with the caps of `lim`. Gives the final URL (with the fragment) and the decoded text.
fn load_document(client: &mut Client, url: &str, lim: &SessionLimits, deadline: Instant) -> Result<(String, String), String> {
    let max_url = NavLimits::default().max_url_bytes;
    if url.len() > max_url { return Err("the document URL is too big".into()); }
    let (bare, fragment) = match url.split_once('#') {
        Some((b, f)) => (b, Some(f.to_string())),
        None => (url, None),
    };
    let mut cur = Url::parse(bare).map_err(|_| "the URL is not an http(s) URL".to_string())?;
    let mut fragment = fragment;
    for hop in 0..=lim.max_redirects {
        if Instant::now() >= deadline {
            return Err("the time limit passed".into());
        }
        let resp = client.send_once(&Request::get(&cur), lim.max_document_bytes).map_err(|_| "network error".to_string())?;
        if matches!(resp.status, 301 | 302 | 303 | 307 | 308) {
            let Some(loc) = resp.header("location") else { return Err("redirect without a location".into()) };
            if hop == lim.max_redirects {
                return Err("too many redirects".into());
            }
            let next = cur.join(loc).map_err(|_| "a redirect leads to a bad URL".to_string())?;
            if !redirect_allowed(&cur, &next) {
                return Err("a redirect leads from https to http".into());
            }
            if let Some((_, f)) = loc.split_once('#') {
                fragment = Some(f.to_string());
            }
            if next.to_string().len().saturating_add(fragment.as_ref().map_or(0, |f| f.len() + 1)) > max_url {
                return Err("the document URL is too big".into());
            }
            cur = next;
            continue;
        }
        if !(200..300).contains(&resp.status) {
            return Err(format!("status {}", resp.status));
        }
        let ct = resp.header("content-type");
        if !is_html(ct) {
            return Err("the content type is not HTML".into());
        }
        if resp.header("content-encoding").is_some_and(|e| !e.trim().eq_ignore_ascii_case("identity")) {
            return Err("the content encoding is not supported".into());
        }
        let text = encoding::decode(&resp.body, ct);
        if text.len() > lim.max_document_bytes {
            return Err("the document is too big".into());
        }
        let mut final_url = cur.to_string();
        if let Some(f) = fragment {
            final_url.push('#');
            final_url.push_str(&f);
        }
        return Ok((final_url, text));
    }
    Err("too many redirects".into())
}

impl Session {
    pub fn new(mut cfg: SessionConfig) -> Session {
        let store = cfg.storage.take().unwrap_or_default();
        Session { cfg, store, browser: None, used: Used::default() }
    }

    /// The storage of the session: it lives through every document.
    pub fn storage(&self) -> WebStorage {
        self.store.clone()
    }

    /// The last document, while its `Browser` is alive (after `run`).
    pub fn browser(&mut self) -> Option<&mut Browser> {
        self.browser.as_mut()
    }

    /// Load the first document and every document that a navigation asks for, within the limits. It never panics on
    /// page input. Call it on a thread with a big stack (8 MiB release, 64 MiB debug): the VM recurses. A second call
    /// starts a new run with new budgets.
    pub fn run(&mut self) -> SessionResult {
        self.used = Used::default();
        let first = self.cfg.url.clone();
        self.drive(Some(first), &[])
    }

    /// Go on with the settled document of the last `run` or `resume`: take `steps` through `Browser::click`, `type_text`
    /// and `submit` (one task each), then run the same network, microtask, timer and navigation pumps. The realm is not
    /// reloaded; a navigation that a listener asks for makes a fresh realm as in `run`. The documents, tasks, rounds and
    /// time that `run` and earlier resumes used stay used. After any stop but `Settled` (and `Page(NoTarget)`, which
    /// leaves the page intact) the session is ended: a call gives the same stop and runs nothing. The result covers this
    /// call only. Same stack rule as `run`.
    pub fn resume(&mut self, steps: &[Input]) -> SessionResult {
        let mut out = SessionResult { stop: SessionStop::Page(Stop::Settled), pages: Vec::new(), urls: Vec::new(), tasks: 0, rounds: 0 };
        if let Some(stop) = &self.used.ended {
            out.stop = stop.clone();
        } else if self.browser.is_none() {
            out.stop = SessionStop::LoadFailed("there is no document to resume".into());
        } else {
            return self.drive(None, steps);
        }
        out
    }

    fn drive(&mut self, next: Option<String>, steps: &[Input]) -> SessionResult {
        let start = Instant::now();
        let before = self.used.time;
        let mut out = SessionResult { stop: SessionStop::Page(Stop::Settled), pages: Vec::new(), urls: Vec::new(), tasks: 0, rounds: 0 };
        out.stop = self.drive_loop(next, steps, &mut out);
        self.used.time = before + start.elapsed();
        if !matches!(out.stop, SessionStop::Page(Stop::Settled | Stop::NoTarget(_))) {
            self.used.ended = Some(out.stop.clone());
        }
        out
    }

    fn drive_loop(&mut self, mut next: Option<String>, steps: &[Input], out: &mut SessionResult) -> SessionStop {
        let lim = self.cfg.limits.clone();
        let deadline = deadline_after(lim.max_time.saturating_sub(self.used.time));
        loop {
            if next.is_some() && self.used.documents >= lim.max_documents {
                return SessionStop::DocumentLimit;
            }
            if self.used.time >= lim.max_time || Instant::now() >= deadline {
                return SessionStop::TimeBudget;
            }
            if self.used.tasks >= lim.max_tasks {
                return SessionStop::TaskBudget;
            }
            if self.used.rounds >= lim.max_rounds {
                return SessionStop::RoundBudget;
            }
            let mut page = self.cfg.page.clone();
            page.max_time = page.max_time.min(deadline.saturating_duration_since(Instant::now()));
            page.max_tasks = page.max_tasks.min(lim.max_tasks - self.used.tasks);
            page.max_rounds = page.max_rounds.min(lim.max_rounds - self.used.rounds);
            let (b, r) = match next.take() {
                None => {
                    // The settled document goes on: same realm, no load.
                    let mut b = self.browser.take().expect("resume checked the browser");
                    let r = b.resume_page((self.cfg.client)(), page, steps);
                    (b, r)
                }
                Some(url) => {
                    // The old document ends here: its realm, timers and callbacks are gone before the next load starts.
                    self.browser = None;
                    let (url, html) = match load_document(&mut (self.cfg.client)(), &url, &lim, deadline) {
                        Ok(d) => d,
                        Err(why) => return SessionStop::LoadFailed(why),
                    };
                    let Ok(mut b) = Browser::new(tree::parse(&html), self.cfg.browser, self.cfg.eval, self.cfg.heap) else {
                        return SessionStop::LoadFailed("the realm could not be made".into());
                    };
                    drop(html);
                    if let Some((w, h)) = self.cfg.viewport
                        && b.set_viewport(w, h).is_err() {
                        return SessionStop::LoadFailed("the viewport is invalid".into());
                    }
                    if self.cfg.virtual_clock {
                        b.use_virtual_clock();
                    }
                    b.attach_storage(self.store.clone());
                    if b.set_document_url(&url).is_err() {
                        return SessionStop::LoadFailed("the document URL could not be configured".into());
                    }
                    self.used.documents += 1;
                    let client = (self.cfg.client)();
                    let fetch = ((self.cfg.client)(), self.cfg.fetch);
                    let r = b.run_page(PageConfig { url, client, limits: page, fetch: Some(fetch) });
                    (b, r)
                }
            };
            let page_url = b.document_url().and_then(|u| Url::parse(u.split('#').next().unwrap_or("")).ok()).map_or_else(|| "?".to_string(), |u| label(&u));
            self.used.tasks += r.tasks;
            self.used.rounds += r.rounds;
            out.tasks += r.tasks;
            out.rounds += r.rounds;
            out.urls.push(page_url);
            let stop = r.stop.clone();
            out.pages.push(r);
            self.browser = Some(b);
            match stop {
                Stop::Navigate(n) => next = Some(n.0.url),
                stop => return SessionStop::Page(stop),
            }
        }
    }
}
