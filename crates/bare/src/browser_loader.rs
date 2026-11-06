//! The page script loader and task driver: it runs the classic scripts of a document in ONE realm, then drives the
//! tasks that those scripts queue (script loads, `fetch`, timers) until the page is quiet or a budget ends.
//!
//! Use: [`Browser::run_page`] with the real document URL, an HTTP client and [`PageLimits`]. It sets the realm's document
//! URL (`Browser::set_document_url`) before the first script, unless the caller set one of the same origin. With
//! `PageConfig::fetch` it also configures `fetch` (the page URL is its origin); with `None` the caller configured it.
//! A new document is a new `Browser`: [`crate::browser_session::Session`] does that (fresh realm, same `WebStorage`,
//! verified network load) and consumes [`Stop::Navigate`]. `run_page` alone never loads a second document.
//!
//! The document is pre-parsed (the whole DOM exists before the first script runs). So this is NOT the parser-blocking
//! model of HTML: a script cannot `document.write`, cannot see a half-built DOM, and a script that removes a later
//! `<script>` does not stop it. The scheduling that is kept:
//! 0. Active style sheets are loaded first (`sheets.rs`).
//! 1. Blocking classic scripts (inline, and external with no `async`/`defer`) run one by one in document order. An external
//!    one is fetched at its turn. A microtask checkpoint follows each script (and each `error`/`load` event listener).
//! 2. `readyState` becomes `interactive` (`readystatechange`). Defer scripts run in document order.
//! 3. `DOMContentLoaded` is dispatched at the document (bubbles to the window).
//! 4. Async scripts run as separate tasks, in document order (a real browser runs them in arrival order, at any point
//!    before `load`). Then rounds of tasks: queued scripts, `fetch` requests, timers, each category at most `batch` tasks per
//!    round, so a zero-delay timer loop cannot starve the network or scripts. When no script is queued, `readyState`
//!    becomes `complete` and `load` is dispatched at the window (it does not wait for timers or `fetch`).
//! 5. The page is settled when nothing is queued and no task that blocks is within `quiet`. A quiet recurring timer or frame
//!    loop does not block (the rule is in the header of `browser_session.rs`; the code is `Browser::blocks_settle`).
//!
//! Same-document traversal and fragment tasks (`popstate`, `hashchange`) are a fourth task category of each round.
//! A navigation request (`location.assign/replace/reload`, `history.go(0)`) ends the run before the next script, event,
//! task or checkpoint: [`Stop::Navigate`]. The task that asked finishes (its microtask checkpoint too). A fatal error in
//! that task wins. No old callback runs after it.
//!
//!
//! KNOWN LIMITATION (kept on purpose): scheduling follows a pre-parsed DOM, not the arrival times of a real browser.
//! Blocking, then defer, then async scripts run in document order. A real browser runs async scripts in the order of
//! arrival, and may run them before the parser ends. A page that depends on that order (a runtime chunk that must run
//! before or after others) can behave differently here.
//!
//! Scripts
//! - Classic only. A `type=module` script is `Fatal::Unsupported("module script")`: the engine has no modules. A `nomodule`
//!   script is skipped (`resources::discover`). Other `type` values are data. Every `<script>` element runs separately, even
//!   when two elements name one URL (the bytes are fetched once: they are kept for the repeats and freed after the last).
//! - A response is used only as `resources::fetch` allows: 2xx, a JavaScript `Content-Type` (a 404 or HTML page never runs),
//!   no `Content-Encoding`, size caps. A failed external script is never run. It gets an `error` event, a successful one
//!   a `load` event (also when the script threw). Inline scripts get no event. Events go through the DOM dispatch: listeners
//!   and `onload`/`onerror` handlers run as normal listeners.
//! - `document.currentScript` is the element while its script runs (inline, external, dynamic), else `null`. It is cleared
//!   when the script throws too. `document.readyState` is `loading`, `interactive` or `complete`.
//! - A script that throws is reported (`Browser::take_reports`, event type `error`) and loading goes on. `window.onerror` and
//!   `ErrorEvent` are absent. Diagnostics hold only the error name.
//! - Fatal: any `Unsupported`, `Limit` or heap fault, a failed microtask checkpoint, a module script. The run stops at once
//!   and no JS runs and no I/O starts after that (a second `run_page` returns the same stop).
//!
//! Dynamic scripts (`document.createElement('script')`)
//! - `HTMLScriptElement` has `src`, `type`, `async`, `defer`, `text`, `onload`, `onerror`. Inserting a connected classic
//!   script (or a subtree that holds one), and setting `src` on a connected script that has not started, "prepares" it once.
//!   The element is marked started, so removing and inserting it again runs nothing. `src`, `type` and the inline text are
//!   read at that moment. An empty `src` or one that does not resolve to http(s) queues an `error` event.
//! - External dynamic scripts are tasks, in the order of insertion (all are `async`; the "force async" flag is not modelled).
//!   Inline dynamic scripts cannot run inside `appendChild` (the engine does not nest scripts): they run right after the
//!   current script (before its microtask checkpoint) or, from a timer, `fetch` or event task, after the pump step (whose own
//!   checkpoint already ran). This is a deviation: code after `appendChild` runs first.
//! - The URL base is the document base (`<base href>` or the page URL). A pending script element is a GC root until it has run.
//!
//! Budgets (`PageLimits`): tasks, rounds, wall time, fetches and bytes (`resources::Limits`), pending dynamic scripts and their
//! inline bytes, kept diagnostics. A blocking socket call is not interrupted: one read can pass `max_time` by the client timeout.
//! Style sheets: the driver fetches the active static and added sheets and supplies them to `getComputedStyle` before the next script
//! (`browser_loader/sheets.rs` has the rules, the order and the caps). Their downloads share the fetch and byte budgets above. A
//! sheet that a script adds and that has not loaded adds no rules to a read in the same task (Chrome does the same).
//! Preload links: a connected `<link rel=preload>` that a script added is fetched by its `as` and gets `load` or `error` as a task
//! (`browser_loader/preload.rs`).
//! Not here: `window.onload`, `DOMContentLoaded` for scripts that were added late, `defer` on dynamic scripts,
//! `integrity`, `crossorigin`, `nonce`, CSP, `<noscript>`, a service worker.

use crate::browser::{Browser, Dom, H, Listener, R, Target as EvTarget, illegal_constructor, unsupported};
use crate::browser_dom as nd;
use crate::browser_fetch::{FetchConfig, FetchLimits};
use crate::browser_navigation::NavigationRequest;
use crate::resources::{self, Expect, JS_TYPES, Kind, Limits, LoadError, Source, Timing, WS};
use bare_html::dom::{NodeData, NodeId};
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value};
use bare_net::http::Client;
use bare_net::parallel::{MAX_WORKERS, Task};
use bare_url::Url;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

mod preload;
#[cfg(test)]
mod preload_tests;
#[cfg(test)]
mod parallel_tests;
mod sheets;
#[cfg(test)]
mod sheets_tests;
pub use preload::PreloadStats;
pub use sheets::CssStats;

type V = R<Value>;

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

// ---------------------------------------------------------------------------------------------- public types

/// Bounds of one `run_page`. Each is checked before the thing grows or starts.
#[derive(Clone, Debug)]
pub struct PageLimits {
    /// References, distinct URLs, one resource, all kept text. `max_time` here is not used: see `max_time` below.
    pub resources: Limits,
    /// All tasks together: scripts, network tasks, timers, messages.
    pub max_tasks: usize,
    /// Loop rounds after the page is parsed.
    pub max_rounds: usize,
    /// Wall time of the whole run.
    pub max_time: Duration,
    /// Tasks of one kind in one round.
    pub batch: usize,
    /// Stop when nothing is queued and the next timer is further away than this.
    pub quiet: Duration,
    /// Dynamic scripts that wait to run, and the bytes of their inline text.
    pub max_pending_scripts: usize,
    pub max_pending_script_bytes: usize,
    /// Collect the heap every this many rounds.
    pub collect_every: usize,
    /// Kept diagnostics. More are counted, not kept.
    pub max_diagnostics: usize,
}

impl Default for PageLimits {
    fn default() -> Self {
        PageLimits {
            resources: Limits::default(),
            max_tasks: 100_000,
            max_rounds: 100_000,
            max_time: Duration::from_secs(60),
            batch: 8,
            quiet: Duration::from_secs(1),
            max_pending_scripts: 256,
            max_pending_script_bytes: 8 << 20,
            collect_every: 32,
            max_diagnostics: 64,
        }
    }
}

pub struct PageConfig {
    /// The real document URL (http or https). Relative script URLs resolve against it (or against `<base href>`).
    pub url: String,
    /// The client that downloads scripts. `Client::new()` trusts the OS roots; a test passes `Client::with_roots`.
    pub client: Client,
    pub limits: PageLimits,
    /// The client and limits of `fetch` for this page. `Some` makes `run_page` configure `fetch` with the page URL as its
    /// origin (so a page URL and a `fetch` origin cannot differ). `None` keeps what the caller configured.
    pub fetch: Option<(Client, FetchLimits)>,
}

/// Why a run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    /// Nothing is queued and no timer is near.
    Settled,
    TaskBudget,
    RoundBudget,
    TimeBudget,
    /// A script asked for a new document (`location.assign`, `replace`, `reload`, `history.go(0)`, a changed `location.href`
    /// or `search`). The run ended at once: no old timer, event, script or `fetch` runs after it. Only a driver that loads
    /// documents ([`crate::browser_session::Session`]) acts on it. A fatal error in the same task wins over it.
    Navigate(NavStop),
    Fatal(Fatal),
    /// Only from [`crate::browser_session::Session::resume`]: step `n` found no element (or its selector was bad). Nothing
    /// after that step ran. The page is intact and the session is not ended.
    NoTarget(usize),
}

/// One input of [`crate::browser_session::Session::resume`]: a selector (first match, as `querySelector`) and the existing
/// `Browser` primitive that acts on it. No default action runs (see `browser.rs`), except that `Click` at a submit button
/// dispatches `submit` at its form owner behind the gates of `browser_form.rs` (a gate ends the run with `Fatal::Unsupported`).
#[derive(Clone, Copy, Debug)]
pub enum Input<'a> {
    Click(&'a str),
    /// A text-like `<input>` and the text: sets its value, then fires `input`.
    Type(&'a str, &'a str),
    Submit(&'a str),
}

/// The request of [`Stop::Navigate`]. `Debug` prints the kind and `origin + path` only: a query, a fragment or a
/// credential in the URL never reaches a log.
#[derive(Clone, PartialEq, Eq)]
pub struct NavStop(pub NavigationRequest);

impl std::fmt::Debug for NavStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let to = Url::parse(&self.0.url).map_or_else(|_| "bad url".into(), |u| label(&u));
        write!(f, "{:?} {to}", self.0.kind)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fatal {
    /// The engine or the host does not do this yet (a label, never source).
    Unsupported(&'static str),
    /// A hard cap of the evaluator or the host. The name of the cap.
    Limit(String),
    Heap,
    /// A microtask job failed in a way that cannot be undone, or a checkpoint threw.
    Poisoned,
    /// The URL or the page could not be set up.
    Setup(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageResult {
    pub stop: Stop,
    /// `loading`, `interactive` or `complete`.
    pub ready_state: &'static str,
    /// Script runs (a repeated element counts each time).
    pub scripts_run: usize,
    /// Scripts that threw (reported, loading went on).
    pub script_throws: usize,
    /// External scripts that did not load and were not run.
    pub load_errors: usize,
    pub tasks: usize,
    pub rounds: usize,
    pub fetches: usize,
    /// Bytes of script and style sheet text downloaded.
    pub bytes: usize,
    /// Style sheet loading (see `browser_loader/sheets.rs`). Its fetches and bytes are also in `fetches` and `bytes`.
    pub css: CssStats,
    /// `<link rel=preload>` loading (see `browser_loader/preload.rs`). Its fetches and bytes are also in `fetches` and `bytes`.
    pub preload: PreloadStats,
    /// Short sanitized lines (origin and path of a script, an error name, a label): never source, headers, bodies or queries.
    pub diagnostics: Vec<String>,
    pub dropped_diagnostics: usize,
    /// Only when `stop` is `Settled`: the period in ms of each recurring timer that the settle rule skipped, in due order.
    pub idle_timers: Vec<u32>,
    /// Only when `stop` is `Settled`: a quiet `requestAnimationFrame` loop was left running.
    pub idle_frames: bool,
}

// ---------------------------------------------------------------------------------------------- host state

enum Ext {
    Fetch(NodeId, Url),
    /// `src` is empty or does not resolve: an `error` event.
    Fail(NodeId),
}

/// The state that the DOM host needs: script elements, the lifecycle, the queues of dynamic scripts.
#[derive(Default)]
pub(crate) struct Scripts {
    enabled: Cell<bool>,
    pub(crate) proto: OnceCell<ObjId>,
    fixed: RefCell<Vec<ObjId>>,
    /// 0 `loading`, 1 `interactive`, 2 `complete`.
    ready: Cell<u8>,
    current: Cell<Option<NodeId>>,
    pub(crate) base: RefCell<Option<String>>,
    started: RefCell<HashSet<NodeId>>,
    inline: RefCell<VecDeque<(NodeId, String)>>,
    external: RefCell<VecDeque<Ext>>,
    pending_bytes: Cell<usize>,
    /// (max pending scripts, max pending inline bytes)
    caps: Cell<(usize, usize)>,
    /// `(element, is_error)` to the id of its `onload` / `onerror` listener.
    handlers: RefCell<HashMap<(NodeId, bool), u64>>,
    fatal: RefCell<Option<Fatal>>,
    /// Script downloads and bytes so far, kept between runs so that a resume cannot reset the fetch budget.
    used: Cell<(usize, usize)>,
    page_caps: Cell<(usize, usize, Duration)>,
    page_spent: Cell<(usize, usize, Duration)>,
    /// Style sheets that the driver fetched and supplied to `getComputedStyle`. Lives as long as the document.
    sheets: RefCell<sheets::Sheets>,
    /// `<link rel=preload>` results and the preloaded script texts. Lives as long as the document.
    preloads: RefCell<preload::Preloads>,
}

impl Dom {
    /// Scripts that wait to run (inline and external dynamic scripts).
    pub(crate) fn scripts_queued(&self) -> usize {
        self.scripts.inline.borrow().len() + self.scripts.external.borrow().len()
    }

    pub(crate) fn scripts_roots(&self, out: &mut Vec<Value>) {
        let s = &self.scripts;
        out.extend(s.fixed.borrow().iter().map(|&o| Value::Object(o)));
        let st = self.st.borrow();
        let mut keep = |n: &NodeId| out.extend(st.by_node.get(n).map(|&o| Value::Object(o)));
        s.inline.borrow().iter().for_each(|(n, _)| keep(n));
        s.external.borrow().iter().for_each(|e| match e {
            Ext::Fetch(n, _) | Ext::Fail(n) => keep(n),
        });
        s.current.get().iter().for_each(keep);
    }

    /// The "already started" flag of a script element that the fragment parser made (HTML Standard, 4.12.1.1): it never runs, wherever it goes.
    pub(crate) fn copy_script_started(&self, source: NodeId, copy: NodeId) {
        let mut started = self.scripts.started.borrow_mut();
        if started.contains(&source) { started.insert(copy); }
    }

    pub(crate) fn mark_started(&self, el: NodeId) {
        self.scripts.started.borrow_mut().insert(el);
    }

    /// A node went into the tree. Every classic `<script>` of it (it may be a subtree) is prepared.
    pub(crate) fn script_inserted(&self, it: &mut Interp<'_>, node: NodeId) -> R<()> {
        if !self.scripts.enabled.get() || !self.is_element(node) {
            return Ok(());
        }
        let found = {
            let doc = self.doc.borrow();
            let mut up = node;
            while let Some(p) = doc.parent(up) {
                it.charge(1)?;
                up = p;
            }
            if up != doc.root() {
                return Ok(());
            }
            let (script, template) = (doc.find_atom("script"), doc.find_atom("template"));
            let mut found = Vec::new();
            let mut stack = vec![node];
            while let Some(n) = stack.pop() {
                it.charge(1)?;
                if let NodeData::Element { name, .. } = *doc.data(n) {
                    if Some(name) == script && !doc.is_foreign(n) {
                        found.push(n);
                    }
                    if Some(name) == template && !doc.is_foreign(n) {
                        continue;
                    }
                }
                let mut c = doc.last_child(n);
                while let Some(k) = c {
                    stack.push(k);
                    c = doc.prev_sibling(k);
                }
            }
            found
        };
        for s in found {
            self.prepare_script(it, s)?;
        }
        Ok(())
    }

    /// `src` was set on `node`.
    pub(crate) fn script_src_changed(&self, it: &mut Interp<'_>, node: NodeId) -> R<()> {
        let is_script = {
            let doc = self.doc.borrow();
            doc.is_html(node, "script")
        };
        if self.scripts.enabled.get() && is_script {
            // Only a script in the tree is prepared (`script_inserted` checks it and walks one node).
            self.script_inserted(it, node)?;
        }
        Ok(())
    }

    /// The "prepare the script element" steps for a classic script that is in the document.
    fn prepare_script(&self, _it: &mut Interp<'_>, el: NodeId) -> R<()> {
        let s = &self.scripts;
        if s.started.borrow().contains(&el) {
            return Ok(());
        }
        let (src, text) = {
            let doc = self.doc.borrow();
            let attr = |n: &str| doc.find_atom(n).and_then(|a| doc.attr(el, a));
            match attr("type").map(|t| t.trim_matches(WS)) {
                None | Some("") => {}
                Some(t) if t.eq_ignore_ascii_case("module") => return Err(Error::Unsupported(unsupported("module script"))),
                Some(t) if JS_TYPES.iter().any(|j| t.eq_ignore_ascii_case(j)) => {}
                Some(_) => return Ok(()),
            }
            if attr("nomodule").is_some() {
                return Ok(());
            }
            match attr("src") {
                Some(src) => (Some(src.to_string()), String::new()),
                None => (None, resources::inline_source(&doc, el).into_owned()),
            }
        };
        let (max_scripts, max_bytes) = s.caps.get();
        if s.inline.borrow().len() + s.external.borrow().len() >= max_scripts {
            return Err(self.lim("pending-scripts"));
        }
        match src {
            Some(src) => {
                s.started.borrow_mut().insert(el);
                let raw = src.trim_matches(WS);
                let url = s.base.borrow().as_deref().and_then(|b| Url::parse(b).ok()).filter(|_| !raw.is_empty()).and_then(|b| b.join(raw).ok());
                s.external.borrow_mut().push_back(match url {
                    Some(u) => Ext::Fetch(el, u),
                    None => Ext::Fail(el),
                });
            }
            None if text.is_empty() => {}
            None => {
                if s.pending_bytes.get() + text.len() > max_bytes {
                    return Err(self.lim("pending-script-bytes"));
                }
                s.started.borrow_mut().insert(el);
                s.pending_bytes.set(s.pending_bytes.get() + text.len());
                s.inline.borrow_mut().push_back((el, text));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------- JS surface

/// The IDL value of a URL attribute: the resolved URL when it resolves against the document base, else the text.
fn url_attr(d: &Dom, it: &mut Interp<'_>, n: NodeId, name: &str) -> V {
    let raw = match nd::attr_get(d, it, n, name)? {
        Value::Null => String::new(),
        v => d.string_arg(it, v)?,
    };
    let resolved = d.scripts.base.borrow().as_deref().and_then(|b| Url::parse(b).ok()).filter(|_| !raw.trim_matches(WS).is_empty()).and_then(|b| b.join(raw.trim_matches(WS)).ok());
    d.js_str(it, &resolved.map_or(raw, |u| u.to_string()))
}

fn script_src(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.el(it, this)?;
    url_attr(d, it, n, "src")
}

pub(crate) fn link_href(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = nd::link_el(d, it, this)?;
    url_attr(d, it, n, "href")
}

pub(crate) fn set_link_href(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = nd::link_el(d, it, this)?;
    let s = d.string_arg(it, arg(a, 0))?;
    nd::attr_set(d, it, n, "href", &s)?;
    Ok(Value::Undefined)
}

fn set_script_src(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.el(it, this)?;
    let s = d.string_arg(it, arg(a, 0))?;
    nd::attr_set(d, it, n, "src", &s)?;
    d.script_src_changed(it, n)?;
    Ok(Value::Undefined)
}

fn script_type(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.el(it, this)?;
    match nd::attr_get(d, it, n, "type")? {
        Value::Null => d.js_str(it, ""),
        v => Ok(v),
    }
}

fn set_script_type(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.el(it, this)?;
    let s = d.string_arg(it, arg(a, 0))?;
    nd::attr_set(d, it, n, "type", &s)?;
    Ok(Value::Undefined)
}

fn flag(d: &Dom, it: &mut Interp<'_>, this: Value, name: &str) -> V {
    let n = d.el(it, this)?;
    let doc = d.doc.borrow();
    Ok(Value::Bool(doc.find_atom(name).and_then(|a| doc.attr(n, a)).is_some()))
}

fn set_flag(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], name: &str) -> V {
    let n = d.el(it, this)?;
    if it.to_js_bool(arg(a, 0))? {
        nd::attr_set(d, it, n, name, "")?;
    } else {
        d.doc.borrow_mut().remove_attr(n, name);
    }
    Ok(Value::Undefined)
}

fn script_async(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    flag(d, it, this, "async")
}

fn set_script_async(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    set_flag(d, it, this, a, "async")
}

fn script_defer(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    flag(d, it, this, "defer")
}

fn set_script_defer(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    set_flag(d, it, this, a, "defer")
}

/// The listener of an `on...` handler: one per element and event, kept in the normal listener list.
fn handler_get(d: &Dom, it: &mut Interp<'_>, this: Value, is_error: bool) -> V {
    let n = d.el(it, this)?;
    let Some(id) = d.scripts.handlers.borrow().get(&(n, is_error)).copied() else { return Ok(Value::Null) };
    let st = d.st.borrow();
    Ok(st.listeners.get(&EvTarget::Node(n)).and_then(|v| v.iter().find(|l| l.id == id)).map_or(Value::Null, |l| Value::Object(l.cb)))
}

fn handler_set(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], is_error: bool) -> V {
    let n = d.el(it, this)?;
    let cb = match arg(a, 0) {
        v @ Value::Object(o) if it.is_callable_value(v) => Some(o),
        _ => None,
    };
    let (t, ty) = (EvTarget::Node(n), if is_error { "error" } else { "load" });
    let old = d.scripts.handlers.borrow().get(&(n, is_error)).copied();
    let mut st = d.st.borrow_mut();
    let st = &mut *st;
    let at = old.and_then(|id| st.listeners.get(&t).and_then(|v| v.iter().position(|l| l.id == id)));
    match (at, cb) {
        (Some(i), Some(cb)) => st.listeners.get_mut(&t).expect("found")[i].cb = cb,
        (Some(i), None) => {
            let v = st.listeners.get_mut(&t).expect("found");
            v.remove(i);
            if v.is_empty() {
                st.listeners.remove(&t);
            }
            st.n_listeners -= 1;
            d.scripts.handlers.borrow_mut().remove(&(n, is_error));
        }
        (None, Some(cb)) => {
            if st.n_listeners >= d.lim.max_listeners {
                return Err(d.lim("dom-listeners"));
            }
            let list = st.listeners.entry(t).or_default();
            if list.len() >= d.lim.max_listeners_per_target {
                return Err(d.lim("dom-listeners-per-target"));
            }
            st.next_id += 1;
            let id = st.next_id;
            list.push(Listener { id, ty: ty.into(), cb, capture: false, once: false, passive: false, handler: false });
            st.n_listeners += 1;
            d.scripts.handlers.borrow_mut().insert((n, is_error), id);
        }
        (None, None) => {}
    }
    Ok(Value::Undefined)
}

fn script_onload(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    handler_get(d, it, this, false)
}

fn set_script_onload(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    handler_set(d, it, this, a, false)
}

fn script_onerror(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    handler_get(d, it, this, true)
}

fn set_script_onerror(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    handler_set(d, it, this, a, true)
}

fn script_text(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    d.el(it, this)?;
    nd::text_content(d, it, this, a)
}

fn set_script_text(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    d.el(it, this)?;
    nd::set_text_content(d, it, this, a)
}

fn doc_ready_state(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    d.nd(it, this)?;
    d.js_str(it, ["loading", "interactive", "complete"][d.scripts.ready.get() as usize])
}

fn doc_current_script(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    d.nd(it, this)?;
    d.wrap_opt(it, d.scripts.current.get())
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let proto = d.obj(it, d.p().html)?;
    let _ = d.scripts.proto.set(proto);
    let acc = |it: &mut Interp<'_>, on: ObjId, name: &str, g: H, s: Option<H>| -> R<()> {
        let gf = d.add_fn(it, g, &format!("get {name}"), 0, false)?;
        let sf = s.map(|h| d.add_fn(it, h, &format!("set {name}"), 1, false)).transpose()?;
        let k = d.key(it, name)?;
        d.define_accessor(it, on, k, gf, sf)
    };
    for (name, g, s) in [
        ("src", script_src as H, set_script_src as H),
        ("type", script_type, set_script_type),
        ("async", script_async, set_script_async),
        ("defer", script_defer, set_script_defer),
        ("text", script_text, set_script_text),
        ("onload", script_onload, set_script_onload),
        ("onerror", script_onerror, set_script_onerror),
    ] {
        acc(it, proto, name, g, Some(s))?;
    }
    let document = d.p().document;
    acc(it, document, "readyState", doc_ready_state, None)?;
    acc(it, document, "currentScript", doc_current_script, None)?;
    let f = d.add_fn(it, illegal_constructor, "HTMLScriptElement", 0, false)?;
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    it.define_data(f, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(f), true, false, true)?;
    let k = d.key(it, "HTMLScriptElement")?;
    let g = it.global_object();
    it.define_data(g, k, Value::Object(f), true, false, true)?;
    *d.scripts.fixed.borrow_mut() = vec![proto, f];
    Ok(())
}

// ---------------------------------------------------------------------------------------------- the driver

impl Browser {
    /// Dispatch a new non-cancelable event at `t`. A checkpoint follows each listener (the driver flag).
    fn fire(&mut self, t: EvTarget, ty: &str, bubbles: bool) -> R<()> {
        let dom = self.dom.clone();
        let was = std::mem::replace(&mut dom.st.borrow_mut().driver, true);
        let r = self.it.host_scope(|it| {
            let e = dom.new_event(it, ty, bubbles, false, false, None)?;
            dom.dispatch(it, t, e).map(|_| ())
        });
        dom.st.borrow_mut().driver = was;
        r
    }

    /// Run the classic scripts of the page, then its tasks, within `cfg.limits`. See the module comment for the order.
    /// It never panics on page input and never returns an `Err`: the result says why it stopped.
    pub fn run_page(&mut self, cfg: PageConfig) -> PageResult {
        let dom = self.dom.clone();
        let mut res = PageResult { stop: Stop::Settled, ready_state: "loading", scripts_run: 0, script_throws: 0, load_errors: 0, tasks: 0, rounds: 0, fetches: 0, bytes: 0, css: CssStats::default(), preload: PreloadStats::default(), diagnostics: Vec::new(), dropped_diagnostics: 0, idle_timers: Vec::new(), idle_frames: false };
        let prior = dom.scripts.fatal.borrow().clone();
        if let Some(f) = prior {
            res.stop = Stop::Fatal(f);
            return res;
        }
        let setup = |res: &mut PageResult, why: String| {
            res.stop = Stop::Fatal(Fatal::Setup(why));
        };
        if dom.scripts.enabled.replace(true) {
            setup(&mut res, "run_page was already called".into());
            return res;
        }
        let page = match Url::parse(&cfg.url) {
            Ok(u) => u,
            Err(_) => {
                setup(&mut res, "the page URL is not an http(s) URL".into());
                return res;
            }
        };
        if let Some((client, limits)) = cfg.fetch
            && self.configure_fetch(FetchConfig { base_url: Some(cfg.url.clone()), client, limits }).is_err() {
            setup(&mut res, "the fetch origin could not be configured".into());
            return res;
        }
        // The realm knows no URL until now (`Browser::new` has none). A URL that the caller set stays, but it must be
        // the origin of the page: the URL of the page and the origin of `fetch` and storage are one thing.
        match self.document_url().map(|u| Url::parse(&u)) {
            None => {
                if self.set_document_url(&cfg.url).is_err() {
                    setup(&mut res, "the document URL could not be configured".into());
                    return res;
                }
            }
            Some(Ok(cur)) if cur.same_origin(&page) => {}
            Some(_) => {
                setup(&mut res, "the document URL is not the origin of the page URL".into());
                return res;
            }
        }
        let disc = match resources::discover(&dom.doc.borrow(), &page, &cfg.limits.resources) {
            Ok(d) => d,
            Err(e) => {
                setup(&mut res, e.to_string());
                return res;
            }
        };
        let s = &dom.scripts;
        *s.base.borrow_mut() = Some(disc.base.clone());
        s.caps.set((cfg.limits.max_pending_scripts, cfg.limits.max_pending_script_bytes));
        s.started.borrow_mut().extend(disc.items.iter().map(|i| i.node));
        let mut refs = vec![0usize; disc.targets.len()];
        let mut items = Vec::new();
        for (n, item) in disc.items.iter().enumerate() {
            if let Kind::Script { module, timing, source } = item.kind {
                if let Source::External(t) = source {
                    refs[t] += 1;
                }
                items.push(Parsed { n, node: item.node, module, timing, source });
            }
        }
        dom.scripts.page_caps.set((cfg.limits.max_tasks, cfg.limits.max_rounds, cfg.limits.max_time));
        let started = Instant::now();
        bare_net::trace::page_start();
        let deadline = deadline_after(cfg.limits.max_time);
        let mut run = Run {
            b: self,
            dom: dom.clone(),
            net: Net::new(cfg.client),
            cursor: 0,
            https_only: page.scheme == bare_url::Scheme::Https,
            lim: cfg.limits,
            deadline,
            cache: (0..disc.targets.len()).map(|_| None).collect(),
            refs,
            retained: 0,
            res,
            disc,
            items,
            async_queue: VecDeque::new(),
            loaded: false,
        };
        let stop = run.go();
        let mut res = run.res;
        res.css = dom.scripts.sheets.borrow().stats;
        res.preload = dom.scripts.preloads.borrow().stats;
        dom.scripts.page_spent.set((res.tasks, res.rounds, started.elapsed()));
        dom.scripts.used.set((res.fetches, res.bytes));
        if let Stop::Fatal(f) = &stop {
            *dom.scripts.fatal.borrow_mut() = Some(f.clone());
        }
        if stop == Stop::Settled {
            (res.idle_timers, res.idle_frames) = self.idle_left();
        }
        res.stop = stop;
        res.ready_state = ["loading", "interactive", "complete"][dom.scripts.ready.get() as usize];
        res
    }
}

impl Browser {
    /// Take the inputs of `steps` in order, then run the rounds again on a page that `run_page` settled: the same realm,
    /// DOM, queues and pumps, no reload. A sticky fatal stops it before any JS or I/O. `limits` is what is left of the
    /// session; the fetch budget of the page carries over. A page that did not run, or has no URL, is a `Setup` fatal.
    pub(crate) fn resume_page(&mut self, client: Client, mut limits: PageLimits, steps: &[Input]) -> PageResult {
        let dom = self.dom.clone();
        let (fetches, bytes) = dom.scripts.used.get();
        let spent = dom.scripts.page_spent.get();
        let caps = dom.scripts.page_caps.get();
        limits.max_tasks = limits.max_tasks.min(caps.0.saturating_sub(spent.0));
        limits.max_rounds = limits.max_rounds.min(caps.1.saturating_sub(spent.1));
        limits.max_time = limits.max_time.min(caps.2.saturating_sub(spent.2));
        let started = Instant::now();
        let mut res = PageResult { stop: Stop::Settled, ready_state: "loading", scripts_run: 0, script_throws: 0, load_errors: 0, tasks: 0, rounds: 0, fetches, bytes, css: dom.scripts.sheets.borrow().stats, preload: dom.scripts.preloads.borrow().stats, diagnostics: Vec::new(), dropped_diagnostics: 0, idle_timers: Vec::new(), idle_frames: false };
        let prior = dom.scripts.fatal.borrow().clone();
        if let Some(f) = prior {
            res.stop = Stop::Fatal(f);
            return res;
        }
        let page = self.document_url().and_then(|u| Url::parse(&u).ok());
        let Some(page) = page.filter(|_| dom.scripts.enabled.get()) else {
            res.stop = Stop::Fatal(Fatal::Setup("resume needs a page that ran".into()));
            return res;
        };
        let mut run = Run {
            b: self,
            dom: dom.clone(),
            net: Net::new(client),
            cursor: 0,
            https_only: page.scheme == bare_url::Scheme::Https,
            deadline: deadline_after(limits.max_time),
            lim: limits,
            cache: Vec::new(),
            refs: Vec::new(),
            retained: 0,
            res,
            disc: resources::Discovery { items: Vec::new(), targets: Vec::new(), base: String::new() },
            items: Vec::new(),
            async_queue: VecDeque::new(),
            loaded: dom.scripts.ready.get() == 2,
        };
        let stop = run.resume(steps).err().unwrap_or(Stop::Settled);
        let mut res = run.res;
        res.css = dom.scripts.sheets.borrow().stats;
        res.preload = dom.scripts.preloads.borrow().stats;
        dom.scripts.page_spent.set((spent.0.saturating_add(res.tasks), spent.1.saturating_add(res.rounds), spent.2.saturating_add(started.elapsed())));
        dom.scripts.used.set((res.fetches, res.bytes));
        if let Stop::Fatal(f) = &stop {
            *dom.scripts.fatal.borrow_mut() = Some(f.clone());
        }
        if stop == Stop::Settled {
            (res.idle_timers, res.idle_frames) = self.idle_left();
        }
        res.stop = stop;
        res.ready_state = ["loading", "interactive", "complete"][dom.scripts.ready.get() as usize];
        res
    }
}

#[derive(Clone, Copy)]
struct Parsed {
    /// Index in the discovery items (for labels).
    n: usize,
    node: NodeId,
    module: bool,
    timing: Timing,
    source: Source,
}

/// The client of the page, and the downloads that run on worker threads: started early, taken when the page needs them.
struct Net {
    client: Client,
    /// By the kind and the URL of the target. A download that finished waits here, with its text, until it is taken.
    started: HashMap<String, Task<Result<resources::Resource, LoadError>>>,
}

/// Downloads that are started and not taken at once: the connections that a server gets (Chrome opens 6 for HTTP/1.1). It also bounds
/// the text that finished early and waits to be taken.
const WINDOW: usize = MAX_WORKERS;

impl Net {
    fn new(client: Client) -> Net {
        Net { client, started: HashMap::new() }
    }

    fn key(target: &resources::Target) -> String {
        format!("{:?} {}", target.expect, target.url)
    }

    /// Start the download of `target` on a worker thread. Nothing happens when `WINDOW` downloads wait, or this one runs already.
    fn start(&mut self, target: &resources::Target, lim: &Limits, deadline: Instant, retained: usize) {
        let key = Net::key(target);
        if self.started.len() >= WINDOW || self.started.contains_key(&key) {
            return;
        }
        let url = Url { scheme: target.url.scheme, host: target.url.host.clone(), port: target.url.port, path: target.url.path.clone() };
        let (target, lim) = (resources::Target { expect: target.expect, url }, lim.clone());
        self.started.insert(key, self.client.spawn(move |client| resources::fetch(client, &target, &lim, deadline, retained)));
    }

    /// The result for `target`: the download that was started, or a new one that this thread waits for.
    fn fetch(&mut self, target: &resources::Target, lim: &Limits, deadline: Instant, retained: usize) -> Result<resources::Resource, LoadError> {
        match self.started.remove(&Net::key(target)) {
            // The text counts against the total cap now, with what the page keeps now (it was read with the room of the time it started).
            Some(task) => match task.wait() {
                Some(Ok(r)) if r.text.len() > lim.max_total_bytes.saturating_sub(retained) => Err(LoadError::TotalTooLarge),
                Some(r) => r,
                None => Err(LoadError::Network("a download thread failed".into())),
            },
            None => resources::fetch(&mut self.client, target, lim, deadline, retained),
        }
    }
}

struct Run<'a> {
    b: &'a mut Browser,
    dom: Rc<Dom>,
    net: Net,
    /// The next target of `disc.targets` that `prefetch` looks at.
    cursor: usize,
    https_only: bool,
    lim: PageLimits,
    deadline: Instant,
    /// The text of an external script target that more parsed elements still wait for, or why it failed.
    cache: Vec<Option<Result<String, String>>>,
    refs: Vec<usize>,
    retained: usize,
    res: PageResult,
    disc: resources::Discovery,
    items: Vec<Parsed>,
    async_queue: VecDeque<Parsed>,
    loaded: bool,
}

type Flow = Result<(), Stop>;

/// `now + d` without an overflow panic: a `Duration::MAX` means "about a century".
pub(crate) fn deadline_after(d: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(d).unwrap_or_else(|| now + Duration::from_secs(100 * 365 * 86_400))
}

fn fatal_of(e: Error) -> Stop {
    Stop::Fatal(match e {
        Error::Unsupported(u) => Fatal::Unsupported(u.what),
        Error::Limit(l) => Fatal::Limit(format!("{l:?}")),
        Error::Heap(_) => Fatal::Heap,
        Error::Throw(_) => Fatal::Poisoned,
    })
}

pub(crate) fn label(u: &Url) -> String {
    format!("{}://{}:{}{}", u.scheme, u.host, u.port, u.path.split('?').next().unwrap_or(""))
}

fn reason(e: &LoadError) -> String {
    match e {
        // The text of a network error can name a host or an address: a fixed label instead.
        LoadError::Network(_) => "network error".into(),
        LoadError::Mime(_) => "content type is not JavaScript".into(),
        LoadError::Encoding(_) => "content encoding is not supported".into(),
        e => e.to_string(),
    }
}

/// One download, counted. Free function so that the caller can borrow other fields of `Run` at the same time.
fn download(net: &mut Net, https_only: bool, lim: &PageLimits, deadline: Instant, retained: usize, res: &mut PageResult, target: &resources::Target) -> Result<String, String> {
    if https_only && target.url.scheme == bare_url::Scheme::Http { return Err("insecure script URL refused".into()); }
    if res.fetches >= lim.resources.max_fetches {
        return Err("fetch budget used".into());
    }
    res.fetches += 1;
    match bare_net::trace::blocked(|| net.fetch(target, &lim.resources, deadline, retained)) {
        Ok(r) => {
            res.bytes += r.text.len();
            Ok(r.text)
        }
        Err(e) => Err(reason(&e)),
    }
}

impl Run<'_> {
    fn diag(&mut self, line: String) {
        if self.res.diagnostics.len() < self.lim.max_diagnostics {
            self.res.diagnostics.push(line.chars().take(200).collect());
        } else {
            self.res.dropped_diagnostics += 1;
        }
    }

    /// A navigation that a script asked for ends the run. Checked before every task, event and checkpoint.
    fn nav_stop(&self) -> Flow {
        match self.dom.nav.s.borrow().requests.front() {
            Some(r) => Err(Stop::Navigate(NavStop(r.clone()))),
            None => Ok(()),
        }
    }

    fn budget(&self) -> Flow {
        self.nav_stop()?;
        if self.res.tasks >= self.lim.max_tasks {
            return Err(Stop::TaskBudget);
        }
        if Instant::now() >= self.deadline {
            return Err(Stop::TimeBudget);
        }
        Ok(())
    }

    fn fatal(&mut self, e: Error, what: &str) -> Stop {
        let stop = fatal_of(e);
        self.diag(format!("fatal in {what}: {stop:?}"));
        stop
    }

    /// Run one script in the realm with `currentScript` set. A throw is reported and ends only this script.
    fn run_one(&mut self, node: NodeId, source: &str, what: &str) -> Flow {
        self.budget()?;
        self.dom.scripts.current.set(Some(node));
        let t = bare_net::trace::on().then(Instant::now);
        let r = self.b.run_script(source);
        if let Some(t) = t {
            bare_net::trace::mark(|| format!("script {what}"), t.elapsed().as_micros() as u64);
        }
        self.dom.scripts.current.set(None);
        self.res.scripts_run += 1;
        self.res.tasks += 1;
        match r {
            Ok(_) => Ok(()),
            Err(Error::Throw(v)) => {
                self.dom.report(&mut self.b.it, v, "error", EvTarget::Window);
                self.res.script_throws += 1;
                let name = if matches!(v, Value::Object(_)) {
                    let reports = self.dom.st.borrow();
                    match reports.reports.back().and_then(|r| r.message.split(':').next()) {
                        Some(n @ ("Error" | "TypeError" | "ReferenceError" | "SyntaxError" | "RangeError" | "URIError" | "EvalError" | "AggregateError" | "SecurityError" | "QuotaExceededError" | "InvalidStateError")) => n.to_string(),
                        _ => "Error".into(),
                    }
                } else { "non-error value".into() };
                self.diag(format!("script threw {name}: {what}"));
                Ok(())
            }
            Err(e) => Err(self.fatal(e, what)),
        }
    }

    /// After a script or task: dynamic inline scripts that it queued run now, then the microtask checkpoint.
    fn after_js(&mut self) -> Flow {
        loop {
            if !self.dom.nav.s.borrow().requests.is_empty() { break; }
            let next = self.dom.scripts.inline.borrow_mut().pop_front();
            let Some((node, text)) = next else { break };
            let pending = &self.dom.scripts.pending_bytes;
            pending.set(pending.get().saturating_sub(text.len()));
            self.run_one(node, &text, "dynamic inline script")?;
        }
        match self.b.drain() {
            Ok(_) => self.nav_stop(),
            Err(e) => Err(self.fatal(e, "microtask checkpoint")),
        }
    }

    fn fire(&mut self, t: EvTarget, ty: &str, bubbles: bool) -> Flow {
        self.nav_stop()?;
        match self.b.fire(t, ty, bubbles) {
            Ok(()) => Ok(()),
            Err(e) => Err(self.fatal(e, ty)),
        }
    }

    fn set_ready(&mut self, state: u8) -> Flow {
        self.dom.scripts.ready.set(state);
        let root = self.dom.doc.borrow().root();
        self.fire(EvTarget::Node(root), "readystatechange", false)?;
        self.after_js()
    }

    /// An external script has its result: run it and send `load`, or send `error`. Nothing failed is ever run.
    fn finish_external(&mut self, node: NodeId, what: &str, text: Result<String, String>) -> Flow {
        match text {
            Ok(text) => {
                self.run_one(node, &text, what)?;
                self.after_js()?;
                self.fire(EvTarget::Node(node), "load", false)?;
            }
            Err(why) => {
                self.res.tasks += 1;
                self.res.load_errors += 1;
                self.diag(format!("script failed: {what}: {why}"));
                self.fire(EvTarget::Node(node), "error", false)?;
            }
        }
        self.after_js()
    }

    /// Run one script element that was in the page when it was parsed.
    fn run_parsed(&mut self, p: Parsed) -> Flow {
        self.budget()?;
        self.feed_if_ran()?;
        if p.module {
            self.diag(format!("module script #{}", p.n));
            return Err(Stop::Fatal(Fatal::Unsupported("module script")));
        }
        match p.source {
            Source::Inline => {
                let text = resources::inline_source(&self.dom.doc.borrow(), p.node).into_owned();
                self.run_one(p.node, &text, &format!("inline script #{}", p.n))?;
                self.after_js()
            }
            Source::External(t) => {
                let what = label(&self.disc.targets[t].url);
                let text = match self.cache[t].take() {
                    Some(Ok(text)) => {
                        self.retained -= text.len();
                        Ok(text)
                    }
                    Some(Err(why)) => Err(why),
                    None => {
                        let preloaded = self.dom.scripts.preloads.borrow_mut().take_script(&self.disc.targets[t].url);
                        match preloaded {
                            Some(r) => r,
                            None => {
                                let kept = self.kept();
                                let r = download(&mut self.net, self.https_only, &self.lim, self.deadline, kept, &mut self.res, &self.disc.targets[t]);
                                self.prefetch();
                                r
                            }
                        }
                    }
                };
                self.refs[t] -= 1;
                // Keep what a later element still needs: the text, or the reason (a failure is not retried).
                let keep = if self.refs[t] == 0 {
                    None
                } else {
                    match &text {
                        Ok(t) => Some(Ok(t.clone())),
                        Err(why) => Some(Err(why.clone())),
                    }
                };
                if let Some(k) = keep {
                    if let Ok(t) = &k {
                        self.retained += t.len();
                    }
                    self.cache[t] = Some(k);
                }
                self.finish_external(p.node, &what, text)
            }
        }
    }

    /// One queued dynamic external script. Gives false when none is queued.
    fn run_dynamic(&mut self) -> Result<bool, Stop> {
        self.prefetch();
        let next = self.dom.scripts.external.borrow_mut().pop_front();
        let Some(next) = next else { return Ok(false) };
        self.budget()?;
        self.feed_if_ran()?;
        match next {
            Ext::Fail(node) => self.finish_external(node, "dynamic script", Err("empty or bad src".into()))?,
            Ext::Fetch(node, url) => {
                let what = label(&url);
                let target = resources::Target { expect: Expect::Script, url };
                let preloaded = self.dom.scripts.preloads.borrow_mut().take_script(&target.url);
                let text = match preloaded {
                    Some(r) => r,
                    None => {
                        let kept = self.kept();
                        let r = download(&mut self.net, self.https_only, &self.lim, self.deadline, kept, &mut self.res, &target);
                        self.prefetch();
                        r
                    }
                };
                self.finish_external(node, &what, text)?;
            }
        }
        Ok(true)
    }

    /// The inputs, one task each, then the rounds. A navigation that an input queued ends the run before the next input.
    /// Always an `Err`: the reason to stop.
    fn resume(&mut self, steps: &[Input]) -> Flow {
        self.feed_styles()?;
        for (i, step) in steps.iter().enumerate() {
            self.budget()?;
            let sel = match *step { Input::Click(s) | Input::Type(s, _) | Input::Submit(s) => s };
            let node = match self.b.query(sel) {
                Ok(Some(n)) => n,
                Ok(None) | Err(Error::Throw(_)) => return Err(Stop::NoTarget(i)),
                Err(e) => return Err(self.fatal(e, "input selector")),
            };
            self.res.tasks += 1;
            let r = match *step {
                Input::Click(_) => self.b.click(node),
                Input::Type(_, t) => self.b.type_text(node, t),
                Input::Submit(_) => self.b.submit(node),
            };
            if let Err(e) = r {
                return Err(self.fatal(e, "input"));
            }
            self.after_js()?;
        }
        let stop = self.rounds().unwrap_err();
        if stop == Stop::Settled && self.res.tasks > 0
            && let Err(e) = self.b.collect(&[]) {
            return Err(self.fatal(e, "collect"));
        }
        Err(stop)
    }

    fn go(&mut self) -> Stop {
        bare_net::trace::mark(|| "go".into(), 0);
        match self.go_inner() {
            Ok(()) => unreachable!("go_inner ends with a stop"),
            Err(stop) => stop,
        }
    }

    /// Start the downloads that the page will need, while fewer than `WINDOW` wait: first the files of the parsed document, in its order,
    /// then the dynamic scripts in the queue. A script is taken when it is its turn to run, so the order of the scripts does not change.
    /// Nothing starts that the page would refuse to load, and no more than the fetch budget allows. One download alone gains nothing from
    /// a worker thread: the page thread makes it when its turn comes.
    fn prefetch(&mut self) {
        let lim = self.lim.resources.clone();
        let room = WINDOW.saturating_sub(self.net.started.len()).min(lim.max_fetches.saturating_sub(self.res.fetches + self.net.started.len()));
        let allowed = |https_only: bool, u: &Url| !(https_only && u.scheme == bare_url::Scheme::Http);
        let copy = |u: &Url| Url { scheme: u.scheme, host: u.host.clone(), port: u.port, path: u.path.clone() };
        let mut cursor = self.cursor;
        let mut todo = Vec::new();
        while cursor < self.disc.targets.len() && todo.len() < room {
            let t = &self.disc.targets[cursor];
            if allowed(self.https_only, &t.url) {
                todo.push(resources::Target { expect: t.expect, url: copy(&t.url) });
            }
            cursor += 1;
        }
        let preloads = self.dom.scripts.preloads.borrow();
        for e in self.dom.scripts.external.borrow().iter().take(WINDOW) {
            if let Ext::Fetch(_, u) = e && todo.len() < room && allowed(self.https_only, u) && !preloads.has_script(u) {
                todo.push(resources::Target { expect: Expect::Script, url: copy(u) });
            }
        }
        drop(preloads);
        if todo.len() + self.net.started.len() < 2 {
            return;
        }
        self.cursor = cursor;
        let kept = self.kept();
        for t in &todo {
            let mut lim = lim.clone();
            if t.expect == Expect::Style {
                lim.max_resource_bytes = lim.max_resource_bytes.min(sheets::MAX_BODY);
            }
            self.net.start(t, &lim, self.deadline, kept);
        }
    }

    fn go_inner(&mut self) -> Flow {
        self.budget()?;
        self.prefetch();
        if let Err(e) = self.b.drain() { return Err(self.fatal(e, "initial checkpoint")); }
        self.feed_styles()?;
        let items = self.items.clone();
        for p in items.iter().filter(|p| p.timing == Timing::Blocking) {
            self.run_parsed(*p)?;
        }
        self.set_ready(1)?;
        for p in items.iter().filter(|p| p.timing == Timing::Defer) {
            self.run_parsed(*p)?;
        }
        let root = self.dom.doc.borrow().root();
        self.fire(EvTarget::Node(root), "DOMContentLoaded", true)?;
        self.after_js()?;
        self.async_queue.extend(items.iter().filter(|p| p.timing == Timing::Async));
        self.rounds()
    }

    /// The rounds of tasks. Returns the reason to stop (always an `Err`).
    fn rounds(&mut self) -> Flow {
        let batch = self.lim.batch.max(1);
        loop {
            if self.res.rounds >= self.lim.max_rounds {
                return Err(Stop::RoundBudget);
            }
            self.res.rounds += 1;
            self.budget()?;
            self.feed_if_ran()?;
            let mut did = 0usize;
            if !self.loaded && self.async_queue.is_empty() && self.dom.scripts.external.borrow().is_empty() {
                self.loaded = true;
                self.set_ready(2)?;
                self.fire(EvTarget::Window, "load", false)?;
                self.after_js()?;
                did += 1;
            }
            for _ in 0..batch {
                if let Some(p) = self.async_queue.pop_front() {
                    self.run_parsed(p)?;
                } else if !self.run_dynamic()? {
                    break;
                }
                did += 1;
            }
            for _ in 0..batch {
                if self.b.pending_fetches() == 0 {
                    break;
                }
                self.budget()?;
                let n = self.b.run_network_tasks(1).map_err(fatal_of)?;
                self.res.tasks += n;
                did += n;
                self.after_js()?;
                if n == 0 {
                    break;
                }
            }
            for _ in 0..batch {
                self.budget()?;
                let pump = self.b.run_scheduled_tasks(1).map_err(fatal_of)?;
                if pump.ran == 0 {
                    break;
                }
                self.res.tasks += pump.ran;
                did += pump.ran;
                self.after_js()?;
            }
            // `popstate` and `hashchange` of same-document traversals.
            for _ in 0..batch {
                if self.b.navigation_stats().pending_tasks == 0 { break; }
                self.budget()?;
                let n = self.b.run_navigation_tasks_up_to(1).map_err(fatal_of)?;
                self.res.tasks += n;
                did += n;
                self.after_js()?;
            }
            if self.res.rounds.is_multiple_of(self.lim.collect_every.max(1)) {
                self.b.collect(&[]).map_err(fatal_of)?;
            }
            if did > 0 {
                continue;
            }
            // Idle: wait for the next task if one that blocks the settle is near, else the page is settled. The wait is
            // for the next task of any kind, so a recurring timer that the rule skips still runs on time on the way.
            let now = self.b.now();
            if !self.b.blocks_settle(now + self.lim.quiet.as_secs_f64() * 1000.0) {
                return Err(Stop::Settled);
            }
            let due = self.b.next_scheduled_due().unwrap_or(now);
            let gap = due - now;
            if self.b.uses_virtual_clock() {
                let pump = self.b.advance_to(due, 1).map_err(fatal_of)?;
                self.res.tasks += pump.ran;
                self.after_js()?;
            } else {
                let wait = Duration::from_secs_f64(gap.max(0.0) / 1000.0);
                // `gap` is at most `quiet` here, so this cannot overflow.
                if Instant::now() + wait >= self.deadline {
                    return Err(Stop::TimeBudget);
                }
                bare_net::trace::slept(wait);
                std::thread::sleep(wait);
            }
        }
    }
}
