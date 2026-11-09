//! The document session: the outer driver that loads a document, runs it with [`Browser::run_page`], and acts on the
//! navigations that its scripts ask for. One `Session` is one browsing session: the same [`WebStorage`] (local and
//! session storage) lives through every document, and nothing else does.
//!
//! One document = one fresh `Browser` (a new JS realm, a new DOM, new timers, a new `fetch` queue, a new fork of the session client: the connections stay, the cache does not).
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
//! 404 page is not run), `Content-Type` `text/html` (XML/XHTML is unsupported), no `Content-Encoding` that `bare_net` cannot undo, at most
//! `max_document_bytes`. The text is decoded by `bare_html::encoding`. A form `POST` sends its body once; a 301, 302 or 303
//! redirect of it is a `GET` without the body, a 307 or 308 is refused (a body is never sent twice). Gates: no cookies
//! (the client has none); no `Referer`; a failed load ends the session (the old page stays gone, as a browser error page would).
//!
//! History of documents: the session keeps the URL of each document it loaded, and where it is (`hist`, `at`). A `back` or `forward`
//! (`history.go(n)`, Alt+Left, a mouse button) past the end of the document's own entries loads the neighbor document again from
//! its URL (no cache: the state of that page, its `history.state` and its form values are not restored). A neighbor that a form
//! `POST` made is not loaded again (a body is never sent twice; Chrome asks first): the traversal does nothing. The scroll offset of
//! a document is kept when it ends and put back when a traversal loads it again. A new navigation drops the entries after it.
//!
//! Settled (the rule of `Browser::run_page`; the same for `run` and `resume`). A page is settled when a round does no work and
//! none of the tasks that are due within the quiet window (`page.quiet`) blocks. Work is a script, a network task (`fetch`,
//! XHR, script, style sheet, preload), a timer, a message, a frame or a traversal that ran in the round. All network I/O
//! happens inside the rounds, one request at a time, so "no request in flight" holds by construction: a settled page has no
//! request queued or running. A queued task blocks, with two exceptions:
//! - A recurring timer whose last run was quiet. A timer is recurring if it is a `setInterval`, or a `setTimeout` whose own
//!   callback (the same function object) armed it again, and its delay is at least 16 ms (one frame; a faster timer is a
//!   work loop and always blocks). A run is quiet if it changed the DOM at most 8 times (node added or removed, attribute or
//!   text written), queued no fetch, script or traversal, and queued no task except its own re-arm. A recurring timer has
//!   no verdict before its first run, so that run is waited for (at most `quiet` away); a later busy run takes the verdict back.
//! - The rendering step (`requestAnimationFrame`) after a quiet frame, judged the same way. The step re-queues itself while
//!   callbacks wait. A call to `requestAnimationFrame` from outside a frame (a click, a timer) takes the verdict back.
//!
//! The skipped tasks stay queued and keep their due times. The loader waits for the next task of any kind, so a skipped timer
//! runs on time while the page waits for a blocking one, and a `resume` runs it when its time comes (the virtual clock moves only
//! while a blocking task is near). `PageResult::idle_timers` (the period in ms of each skipped timer) and `idle_frames` say what a
//! settled page left running. Not detected: a timeout that arms a new closure each time (`setTimeout(() => tick(), 1000)`)
//! is a chain of one-shots and blocks while it is within the window. A timer beyond the window never blocks, as before.
//! Results and diagnostics name `scheme://host:port/path` only: no query, fragment, header, body or source.

use crate::browser::{Browser, BrowserLimits};
use crate::browser_fetch::FetchLimits;
use crate::browser_loader::{Input, PageConfig, PageLimits, PageResult, SLICE, Scan, Stop, deadline_after, label};
use crate::browser_navigation::{Body, NavLimits, NavigationKind, WebStorage};
use bare_html::{encoding, tree};
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_net::http::{Client, Cookies, Ctx, Request};
use bare_net::parallel::Task;
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
    /// Makes the client of the session, once. Every document download, script download and `fetch` of the session uses a fork of it, so
    /// they share its connections (`Client::fork`). A test passes a `Client::with_roots` factory.
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
    /// The window and the control interface: `resume(&[])` is one turn of an event loop, not a run to the settle. It waits for no
    /// reply (`fetch`, XHR and dynamic scripts start on workers and settle in a later turn when they are in), stops after a slice of
    /// work once the page has loaded, and a navigation that the page asks for downloads the next document on a worker while the old
    /// one stays. `run` and `navigate` also return at once: the document downloads on a worker, and the turns of `resume` load it.
    /// See `docs/decisions.md` ("Input that does not wait").
    pub live: bool,
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
            eval: EvalLimits::session(),
            heap: HeapLimits::default(),
            viewport: None,
            virtual_clock: false,
            storage: None,
            live: false,
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

/// One document in the history of the session.
struct Hist {
    url: String,
    /// A form `POST` made it: it is never loaded again.
    post: bool,
    /// How far it was scrolled when it ended.
    scroll: (f32, f32),
}

/// How a load changes the history.
#[derive(Clone, Copy)]
enum How {
    Push,
    Replace,
    Reload,
    /// A traversal to this index.
    Move(usize),
}

/// A document to load.
struct Next {
    /// The site of the page that asked for this load (`cookie::site_of`), or `None`: the user did (the address bar, a reload, history).
    from: Option<String>,
    url: String,
    body: Option<Body>,
    how: How,
}

/// The site of the page that started a navigation, for the `SameSite` rules.
fn initiator(from: &str) -> Option<String> {
    // A URL that does not parse counts as another site.
    Some(Url::parse(from.split('#').next().unwrap_or("")).map_or_else(|_| String::new(), |u| bare_net::cookie::site_of(&u)))
}

pub struct Session {
    hist: Vec<Hist>,
    at: usize,
    cfg: SessionConfig,
    store: WebStorage,
    /// The last document. Dropped before the next one is requested.
    browser: Option<Browser>,
    /// What the session has used so far, shared by `run` and every `resume`.
    used: Used,
    /// The first client that `cfg.client` made. Each document and each `resume` gets a fork: they share the connections, as the
    /// sockets of a browser outlive a navigation.
    client: Option<Client>,
    /// Live: the next document, while a worker downloads it. The old document stays until it is in.
    loading: Option<(Next, Task<Result<(String, String), String>>)>,
    /// Live: the next document after its download, while turns parse it. The old document stays until the parse is done.
    parsing: Option<Box<Parsing>>,
}

/// Live: a downloaded document that turns parse in slices (`step`), as a browser does. No script runs before the whole tree is built (the
/// loader keeps its pre-parsed model), so the tree, the order of the scripts and the events are those of `tree::parse`.
struct Parsing {
    next: Next,
    url: String,
    html: String,
    /// Bytes of `html` given to the parser.
    at: usize,
    parser: tree::Parser,
    /// Starts the downloads of the scripts and sheets ahead of the parser.
    scan: Scan,
}

/// The text that one parse step feeds. A step costs well under a millisecond, so a turn ends close to the end of its slice.
const CHUNK: usize = 16 << 10;

impl Parsing {
    /// Scan ahead for half of the time that is left, then parse until `end`. True: the parser has the whole text.
    fn step(&mut self, end: Instant) -> bool {
        let now = Instant::now();
        self.scan.step(&self.html, now + end.saturating_duration_since(now) / 2);
        while self.at < self.html.len() {
            let mut cut = (self.at + CHUNK).min(self.html.len());
            while !self.html.is_char_boundary(cut) {
                cut += 1;
            }
            self.parser.feed(&self.html[self.at..cut]);
            self.at = cut;
            if Instant::now() >= end {
                break;
            }
        }
        self.at == self.html.len()
    }
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

/// The document for a response that is not HTML, as Chrome shows it: text (plain, JSON, XML, CSS, JS) in a `<pre>`, an
/// image as a document with one `<img>`. Any other type is a download: no page. A response without a type is refused.
pub(crate) fn non_html_document(content_type: Option<&str>, url: &str, body: &[u8]) -> Result<String, String> {
    let Some(ct) = content_type else { return Err("the content type is not HTML".into()) };
    let mime = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let esc = |t: &str, quote: bool| {
        let mut o = String::with_capacity(t.len() + 16);
        for c in t.chars() {
            match c {
                '&' => o.push_str("&amp;"),
                '<' => o.push_str("&lt;"),
                '>' => o.push_str("&gt;"),
                '"' if quote => o.push_str("&quot;"),
                c => o.push(c),
            }
        }
        o
    };
    if mime.starts_with("image/") {
        return Ok(format!("<img src=\"{}\">", esc(url, true)));
    }
    let text = mime.starts_with("text/") || mime.ends_with("+json") || mime.ends_with("+xml") || matches!(mime.as_str(), "application/json" | "application/xml" | "application/javascript" | "application/x-javascript");
    if !text {
        return Err("the content is a download".into());
    }
    Ok(format!("<pre style=\"white-space:pre-wrap;word-wrap:break-word\">{}</pre>", esc(&encoding::decode(body, Some(ct)), false)))
}

/// May a redirect (or a navigation) go from `from` to `to`? http(s) only, and https never goes down to http.
pub(crate) fn redirect_allowed(from: &Url, to: &Url) -> bool {
    use bare_url::Scheme;
    !(from.scheme == Scheme::Https && to.scheme == Scheme::Http)
}

/// Download one document with the caps of `lim`. Gives the final URL (with the fragment) and the decoded text.
fn load_document(client: &mut Client, url: &str, from: Option<&str>, body: Option<&Body>, lim: &SessionLimits, deadline: Instant) -> Result<(String, String), String> {
    let max_url = NavLimits::default().max_url_bytes;
    if url.len() > max_url { return Err("the document URL is too big".into()); }
    let (bare, fragment) = match url.split_once('#') {
        Some((b, f)) => (b, Some(f.to_string())),
        None => (url, None),
    };
    let mut cur = Url::parse(bare).map_err(|_| "the URL is not an http(s) URL".to_string())?;
    let mut fragment = fragment;
    let mut body = body;
    // Cookies: a navigation that a page of another site started sends `Lax` cookies on a GET, and a chain that leaves that site stays cross-site.
    let mut from = from;
    for hop in 0..=lim.max_redirects {
        if Instant::now() >= deadline {
            return Err("the time limit passed".into());
        }
        let cookies = Cookies { on: true, ctx: Ctx::Navigation(from) };
        let resp = match body {
            Some(b) => bare_net::trace::blocked(|| client.send_once(&Request { cookies, ..Request::post(&cur, &[("Content-Type", b.content_type.as_str())], &b.bytes) }, lim.max_document_bytes)),
            None => bare_net::trace::blocked(|| client.send_once(&Request { cookies, ..Request::get(&cur) }, lim.max_document_bytes)),
        }
        .map_err(|_| "network error".to_string())?;
        if matches!(resp.status, 301 | 302 | 303 | 307 | 308) {
            if body.is_some() && matches!(resp.status, 307 | 308) {
                return Err("a redirect would send the form body again".into());
            }
            body = None;
            let Some(loc) = resp.header("location") else { return Err("redirect without a location".into()) };
            if hop == lim.max_redirects {
                return Err("too many redirects".into());
            }
            let next = cur.join(loc).map_err(|_| "a redirect leads to a bad URL".to_string())?;
            if !redirect_allowed(&cur, &next) {
                return Err("a redirect leads from https to http".into());
            }
            if from.is_some_and(|f| !f.is_empty() && !bare_net::cookie::same_site(f, &next)) {
                from = Some("");
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
        let html = is_html(ct);
        if resp.encoded() {
            return Err(if html || ct.is_some() { "the content encoding is not supported" } else { "the content type is not HTML" }.into());
        }
        let text = if html { encoding::decode(&resp.body, ct) } else { non_html_document(ct, &cur.to_string(), &resp.body)? };
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
        Session { cfg, store, browser: None, used: Used::default(), hist: Vec::new(), at: 0, client: None, loading: None, parsing: None }
    }

    /// A client that shares the connections of the session.
    pub(crate) fn fork(&mut self) -> Client {
        let (make, store) = (&mut self.cfg.client, &self.store);
        // The client uses the jar of the storage, so `document.cookie` and the network see the same cookies, and a new session of the same
        // storage keeps them (`Page::open_with`). A session with a new storage starts with no cookie.
        let mut c = self.client.get_or_insert_with(|| {
            let mut c = make();
            c.use_jar(store.jar());
            c
        }).fork();
        c.set_site(self.browser.as_ref().and_then(|b| b.document_url()).and_then(|u| Url::parse(u.split('#').next().unwrap_or("")).ok()).map(|u| bare_net::cookie::site_of(&u)));
        c
    }

    /// The storage of the session: it lives through every document.
    pub fn storage(&self) -> WebStorage {
        self.store.clone()
    }

    /// How many documents the session has loaded so far. A node of an older one means nothing in the newest.
    pub fn documents(&self) -> usize {
        self.used.documents
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
        self.loading = None;
        self.parsing = None;
        let first = self.cfg.url.clone();
        self.hist.clear();
        self.at = 0;
        let next = Next { from: None, url: first, body: None, how: How::Push };
        if self.cfg.live {
            return self.start(next);
        }
        self.drive(Some(next), &[])
    }

    /// Live: download `next` on a worker and return at once. The turns of `resume` parse the document and run it, as after a link.
    fn start(&mut self, next: Next) -> SessionResult {
        let (url, body, from, lim) = (next.url.clone(), next.body.clone(), next.from.clone(), self.cfg.limits.clone());
        let deadline = deadline_after(lim.max_time.saturating_sub(self.used.time));
        let task = self.fork().spawn(move |c| load_document(c, &url, from.as_deref(), body.as_ref(), &lim, deadline));
        self.loading = Some((next, task));
        SessionResult { stop: SessionStop::Page(Stop::Settled), pages: Vec::new(), urls: Vec::new(), tasks: 0, rounds: 0 }
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
        } else if self.browser.is_none() && self.loading.is_none() && self.parsing.is_none() {
            out.stop = SessionStop::LoadFailed("there is no document to resume".into());
        } else {
            return self.drive(None, steps);
        }
        out
    }

    /// Load `url` as a user does from the address bar (a new history entry), or load the current document again (`None`: reload,
    /// at the URL that `pushState` left). The old document ends first. This also works after a stop that ended the session, because a
    /// new document is a new realm; the session budgets stay spent. Same stack rule as `run`. Live: the old document ends, the next
    /// one downloads on a worker and the call returns at once (`start`); until it is in, the session has no document.
    pub fn navigate(&mut self, url: Option<&str>) -> SessionResult {
        let current = self.browser.as_ref().and_then(|b| b.document_url());
        if let (Some(h), Some(u)) = (self.hist.get_mut(self.at), &current) {
            h.url = u.clone();
            h.scroll = self.browser.as_ref().map_or((0.0, 0.0), |b| (b.scroll().x, b.scroll().y));
        }
        let next = match url {
            Some(u) => Some(Next { from: None, url: u.to_string(), body: None, how: How::Push }),
            None => self.hist.get(self.at).map(|h| Next { from: None, url: h.url.clone(), body: None, how: How::Reload }),
        };
        let Some(next) = next else {
            return SessionResult { stop: SessionStop::LoadFailed("there is no document to reload".into()), pages: Vec::new(), urls: Vec::new(), tasks: 0, rounds: 0 };
        };
        self.used.ended = None;
        self.loading = None;
        self.parsing = None;
        if self.cfg.live {
            // The old document ends now, so a heap is never held twice.
            self.browser = None;
            return self.start(next);
        }
        self.drive(Some(next), &[])
    }

    /// End the document now, and forget the next one that is on its way. Its realm is dropped; the history and the storage stay.
    pub fn end_document(&mut self) {
        self.browser = None;
        self.loading = None;
        self.parsing = None;
    }

    /// Live: is a next document on its way (`Some(false)`), or in (`Some(true)`: the next `resume` parses a slice of it or shows it)?
    pub fn loading(&self) -> Option<bool> {
        if self.parsing.is_some() {
            return Some(true);
        }
        self.loading.as_ref().map(|l| l.1.done())
    }

    /// Why the session cannot go on (`run` and `resume` give this stop and run nothing), or `None`.
    pub fn ended(&self) -> Option<&SessionStop> {
        self.used.ended.as_ref()
    }

    fn drive(&mut self, next: Option<Next>, steps: &[Input]) -> SessionResult {
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

    fn drive_loop(&mut self, mut next: Option<Next>, mut steps: &[Input], out: &mut SessionResult) -> SessionStop {
        let lim = self.cfg.limits.clone();
        let deadline = deadline_after(lim.max_time.saturating_sub(self.used.time));
        // Live: the document that a worker downloaded, parsed in slices. Until it is parsed, the old document does not run (its navigation
        // stops each run).
        let (mut fetched, mut scan) = (None, None);
        if next.is_none() && self.parsing.is_none() && self.loading.is_some() {
            if self.loading.as_ref().is_some_and(|l| !l.1.done()) {
                return SessionStop::Page(Stop::Settled);
            }
            let (n, task) = self.loading.take().expect("checked");
            match task.wait().unwrap_or_else(|| Err("network error".into())) {
                Ok((url, html)) => {
                    let scan = Url::parse(url.split('#').next().unwrap_or("")).map(|page| {
                        let mut c = self.fork();
                        c.set_site(Some(bare_net::cookie::site_of(&page)));
                        Scan::new(c, page, self.cfg.page.resources.clone(), deadline)
                    });
                    let Ok(scan) = scan else { return SessionStop::LoadFailed("the URL is not an http(s) URL".into()) };
                    self.parsing = Some(Box::new(Parsing { next: n, url, html, at: 0, parser: tree::Parser::new(), scan }));
                }
                Err(why) => {
                    fetched = Some(Err(why));
                    next = Some(n);
                }
            }
        }
        if next.is_none() && self.parsing.is_some() {
            if !self.parsing.as_mut().expect("checked").step(Instant::now() + SLICE) {
                return SessionStop::Page(Stop::Settled);
            }
            let p = *self.parsing.take().expect("checked");
            bare_net::trace::mark(|| "document parsed in slices".into(), 0);
            fetched = Some(Ok((p.url, p.parser.finish())));
            scan = Some(p.scan);
            next = Some(p.next);
        }
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
                    let r = b.resume_page(self.fork(), page, steps);
                    (b, r)
                }
                Some(Next { from, url, body, how }) => {
                    // The old document ends here: its realm, timers and callbacks are gone before the next load starts.
                    self.browser = None;
                    let mut base = self.fork();
                    let doc = match fetched.take() {
                        Some(d) => d,
                        None => load_document(&mut base, &url, from.as_deref(), body.as_ref(), &lim, deadline).map(|(url, html)| (url, tree::parse(&html))),
                    };
                    let (url, dom) = match doc {
                        Ok(d) => d,
                        Err(why) => return SessionStop::LoadFailed(why),
                    };
                    bare_net::trace::mark(|| "document loaded".into(), 0);
                    let scroll = match how {
                        How::Push => {
                            self.hist.truncate(if self.hist.is_empty() { 0 } else { self.at + 1 });
                            self.hist.push(Hist { url: url.clone(), post: body.is_some(), scroll: (0.0, 0.0) });
                            self.at = self.hist.len() - 1;
                            (0.0, 0.0)
                        }
                        How::Replace => {
                            self.hist[self.at] = Hist { url: url.clone(), post: body.is_some(), scroll: (0.0, 0.0) };
                            (0.0, 0.0)
                        }
                        How::Reload => self.hist[self.at].scroll,
                        How::Move(i) => {
                            self.at = i;
                            self.hist[i].url = url.clone();
                            self.hist[i].scroll
                        }
                    };
                    let Ok(mut b) = Browser::new(dom, self.cfg.browser, self.cfg.eval, self.cfg.heap) else {
                        return SessionStop::LoadFailed("the realm could not be made".into());
                    };
                    bare_net::trace::mark(|| "realm made (parse + new)".into(), 0);
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
                    b.cross_document_history(true);
                    if scroll != (0.0, 0.0) {
                        b.set_scroll(bare_paint::Scroll { x: scroll.0, y: scroll.1, ..Default::default() });
                    }
                    self.used.documents += 1;
                    // The document, its scripts and its `fetch` calls share the connections and the TLS roots of one client.
                    let client = base.fork();
                    let fetch = (base.fork(), self.cfg.fetch);
                    if self.cfg.live {
                        b.dom.fetch.live.set(true);
                        b.dom.scripts.split.set(true);
                        if let Some(s) = scan.take() {
                            b.adopt_scan(s);
                        }
                    }
                    // Live: the document runs its scripts across turns and takes input between them.
                    let r = b.run_page(PageConfig { url, client, limits: page.clone(), fetch: Some(fetch) });
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
            steps = &[];
            let Stop::Navigate(n) = stop else { return SessionStop::Page(stop) };
            let req = n.0;
            // The document ends: keep where it was (its URL may have changed by `pushState`, and it was scrolled).
            if let (Some(h), Some(b)) = (self.hist.get_mut(self.at), self.browser.as_mut()) {
                if let Some(u) = b.document_url() {
                    h.url = u;
                }
                let s = b.scroll();
                h.scroll = (s.x, s.y);
            }
            next = match req.kind {
                NavigationKind::Assign => Some(Next { from: initiator(&req.from), url: req.url, body: req.body, how: How::Push }),
                NavigationKind::Replace => Some(Next { from: initiator(&req.from), url: req.url, body: req.body, how: How::Replace }),
                NavigationKind::Reload => Some(Next { from: None, url: req.url, body: None, how: How::Reload }),
                NavigationKind::Traverse(n) => {
                    let to = self.at as i64 + n as i64;
                    match self.hist.get(usize::try_from(to).unwrap_or(usize::MAX)) {
                        // Past the oldest or the newest document, or a page that a `POST` made: nothing to load. The document goes on.
                        Some(h) if !h.post => Some(Next { from: None, url: h.url.clone(), body: None, how: How::Move(to as usize) }),
                        _ => {
                            // The loader only looks at the first request: take it, or the page stops on it again.
                            if let Some(b) = self.browser.as_mut() {
                                b.take_navigation_request();
                            }
                            None
                        }
                    }
                }
            };
            // Live: the next document downloads on a worker, and the turn ends. The old document stays on screen until it is in.
            if self.cfg.live && let Some(n) = next.take() {
                return self.start(n).stop;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The saved pages of the repository, each repeated until it is several parse steps long, give the tree of `tree::parse` when turns
    /// parse them in slices: the cuts fall inside tags, texts and scripts.
    #[test]
    fn a_page_parsed_in_slices_gives_the_tree_of_a_one_step_parse() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut pages = Vec::new();
        for dir in ["tools/fixtures/staging", "tools/fixtures/ui", "tools/fixtures/drive", "crates/paint/tests/fixtures"] {
            for e in std::fs::read_dir(root.join(dir)).unwrap().flatten() {
                if e.path().extension().is_some_and(|x| x == "html") {
                    pages.push(std::fs::read_to_string(e.path()).unwrap());
                }
            }
        }
        assert!(pages.len() >= 10, "{} saved pages", pages.len());
        for page in pages {
            let html = page.repeat(3 * CHUNK / page.len() + 2);
            let mut lim = crate::resources::Limits::default();
            lim.max_fetches = 0;
            let scan = Scan::new(Client::new(), Url::parse("http://127.0.0.1:1/").unwrap(), lim, Instant::now());
            let next = Next { from: None, url: String::new(), body: None, how: How::Push };
            let mut p = Parsing { next, url: String::new(), html: html.clone(), at: 0, parser: tree::Parser::new(), scan };
            let mut steps = 1;
            // An end in the past: each step feeds one chunk.
            while !p.step(Instant::now()) {
                steps += 1;
            }
            assert!(steps >= 3, "{steps} steps");
            assert!(p.parser.finish().dump() == tree::parse(&html).dump(), "a page parsed in slices gives another tree");
        }
    }
}
