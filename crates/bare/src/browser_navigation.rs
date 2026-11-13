//! Navigation and origin-scoped storage for the browser host: `URL`, `URLSearchParams`, `location`, `history`,
//! `document.URL/baseURI`, `localStorage`, `sessionStorage`. No network, no file. Files:
//! `browser_navigation_url.rs` (URL records over `bare_url`), `_params.rs` (JS `URL`/`URLSearchParams`), `_state.rs`
//! (history state cloning), `_storage.rs` (the store and `Storage`), `_tests.rs`.
//!
//! The document URL
//! - `Browser::new` has no URL. Until `Browser::set_document_url(url)` is called, everything that needs one
//!   (`location.*`, `history.*`, `document.URL/baseURI`, `localStorage`, `sessionStorage`, `window.origin`) stops the
//!   script with `Error::Unsupported("document URL is not configured ...")`. Nothing pretends to be a staging origin.
//!   `new URL(absolute)`, `URL.canParse` and `URLSearchParams` need no document URL.
//! - `set_document_url` resets navigation state: one history entry, empty queues. It does not replace the realm.
//!   The loader must create a new Browser for a new document, then configure its URL
//!   (fetch uses this URL immediately when networking is configured). Every URL change raises `url_generation()`.
//! - `document_url()`, `base_uri()` and `resolve_url()` give the loader the URL state. `<base href>` is not read:
//!   `baseURI` is always the document URL (a gate, not a claim).
//!
//! History (same-document only)
//! - `pushState`/`replaceState` clone the state (see `_state.rs`) and parse the URL against the document URL. Order:
//!   `title`, `url` coercion, then state cloning (a `DataCloneError` wins over a bad URL), then URL checks. A URL of
//!   another origin (or another scheme) throws `SecurityError` and commits nothing. A history holds `max_history_entries`
//!   entries (the oldest drops, as in a browser); pushing drops the entries after the current one.
//! - `back`/`forward`/`go(n)` and fragment navigation are QUEUED. They run in `Browser::run_navigation_tasks`: the
//!   URL changes then, `popstate` fires on `window` (a plain `Event` with an own `state`), and `hashchange` (own
//!   `oldURL`/`newURL`) when only the fragment differs. A run handles the tasks queued when it started. `go(0)` is a reload.
//!   A fragment change by `location.hash`, `location.href = "#x"` or `location.assign("#x")` updates the URL at once, makes
//!   an entry, and queues `popstate` then `hashchange` (to verify: this order is from the HTML Standard by reading).
//! - Anything else that changes the URL (`location.assign/replace/reload`, `location.href = other`, `location.search = x`,
//!   `go(0)`) is a `NavigationRequest` for the loader (`take_navigation_request`). No document is loaded here. The
//!   current URL does not change until the loader calls `set_document_url`.
//!
//! Gates: no `onpopstate` property handler, no `PopStateEvent`/`HashChangeEvent` classes, no `history.length` cap other
//! than `max_history_entries`, no scroll restoration inside one document (the `scrollRestoration` value is only stored; the session restores the scroll of a document that a traversal loads again), no `location.ancestorOrigins`,
//! no `document.referrer`/`domain`, no `storage` event. Not compared with Chrome except through Node's `URL` (see
//! `tools/js/navigation_check.sh`): history, location and storage are checked against hand-written structural expectations.
//!
//! GC: URL wrappers, params wrappers and iterators are traced to each other (`trace`) and pruned when unreachable.
//! The permanent objects (`location`, `history`, `localStorage`, `sessionStorage`, prototypes) are roots. The cloned state
//! of an entry is native data; its materialized object is cached and rooted only while the entry lives.

use crate::browser::*;
use crate::browser_navigation_params as pr;
use crate::browser_navigation_state::{self as st, State};
use crate::browser_navigation_storage as sto;
pub use crate::browser_navigation_storage::{StorageLimits, StoreError, WebStorage};
use crate::browser_navigation_url::{self as nu, Part, Rec, UErr};
use bare_html::dom::NodeData;
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

/// Bounds of the navigation module. Each is checked before the thing grows (`Limit(Host(name))`).
#[derive(Clone, Copy, Debug)]
pub struct NavLimits {
    /// Live `URL`, `URLSearchParams` and iterator wrappers together.
    pub max_objects: usize,
    /// Bytes of one URL or one URL string argument.
    pub max_url_bytes: usize,
    /// Pairs in one `URLSearchParams`.
    pub max_params: usize,
    /// Bytes of all live `URLSearchParams` lists (a pair costs its text plus 32).
    pub max_param_bytes: usize,
    /// Entries of the history (the oldest drops).
    pub max_history_entries: usize,
    /// Bytes of one history state, of all states, and the nesting depth and objects of one state. A state over the bytes, total or objects cap is a
    /// `DataCloneError` that the page can catch. The depth cap guards the native stack of the walk: it stays a `Limit`.
    pub max_state_bytes: usize,
    pub max_total_state_bytes: usize,
    pub max_state_depth: usize,
    pub max_state_nodes: usize,
    /// Queued traversal/fragment tasks, and queued navigation requests.
    pub max_tasks: usize,
    pub max_requests: usize,
    /// UTF-16 units of one storage key or value. A longer value is a `QuotaExceededError` of the write. The default is the origin quota of
    /// `StorageLimits`: a browser has no cap on one item, so each item that fits the quota fits here too.
    pub max_item_units: usize,
}

impl Default for NavLimits {
    fn default() -> Self {
        NavLimits {
            max_objects: 1 << 16,
            max_url_bytes: 1 << 20,
            max_params: 1 << 14,
            max_param_bytes: 8 << 20,
            max_history_entries: 50,
            max_state_bytes: 16 << 20,
            max_total_state_bytes: 32 << 20,
            max_state_depth: 64,
            max_state_nodes: 1 << 18,
            max_tasks: 64,
            max_requests: 64,
            max_item_units: 5 << 20,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationKind {
    /// `location.assign(url)` or `location.href = url`.
    Assign,
    /// `location.replace(url)`, or an assign to the URL that is already current.
    Replace,
    /// `location.reload()` and `history.go(0)`: `url` is the current URL.
    Reload,
    /// `history.go(n)` past the first or the last entry of this document, when the session keeps a history of documents
    /// (`Browser::cross_document_history`). The number is how many entries past the end the target is. `url` is the current URL.
    Traverse(i32),
}

/// The body of a form `POST`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Body {
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// A document load that script asked for. The loader decides what to do (this module never loads).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavigationRequest {
    pub kind: NavigationKind,
    /// The absolute target URL, with its fragment.
    pub url: String,
    /// The document URL when the request was made.
    pub from: String,
    /// `Some` for a form `POST`: the session sends it. Never sent twice.
    pub body: Option<Body>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NavigationStats {
    pub history_len: usize,
    pub history_index: usize,
    pub pending_tasks: usize,
    pub pending_requests: usize,
    pub urls: usize,
    pub params: usize,
    pub iterators: usize,
    pub param_bytes: usize,
    pub state_bytes: usize,
    pub url_generation: u64,
}

pub(crate) struct Entry {
    pub url: Rec,
    pub state: Option<Rc<State>>,
    /// The object that `history.state` returned. A root while the entry lives.
    pub cache: Option<ObjId>,
}

impl Entry {
    fn bytes(&self) -> usize {
        self.state.as_ref().map_or(0, |s| s.bytes)
    }
}

pub(crate) enum Task {
    /// `history.go(n)`, n not 0. The target is computed when the task runs.
    Traverse(i32),
    /// A fragment navigation that already changed the URL.
    Fragment { old: String, new: String },
}

#[derive(Default)]
pub(crate) struct NavState {
    /// Empty until the document URL is configured.
    pub entries: Vec<Entry>,
    pub cur: usize,
    pub generation: u64,
    pub tasks: VecDeque<Task>,
    pub requests: VecDeque<NavigationRequest>,
    /// Links with a `download` attribute that were activated: the session saves them (`downloads.rs`).
    pub downloads: VecDeque<crate::downloads::DownloadRequest>,
    /// The session saves downloads (`Browser::enable_downloads`). Off: a `download` link navigates as any link.
    pub downloads_on: bool,
    pub urls: IdMap<ObjId, pr::UrlObj>,
    pub params: IdMap<ObjId, pr::ParamsObj>,
    pub iters: IdMap<ObjId, pr::IterObj>,
    pub param_bytes: usize,
    pub state_bytes: usize,
    pub store: WebStorage,
    pub manual_scroll: bool,
    /// The session keeps a history of documents: `go(n)` past the end of this one is a request (`NavigationKind::Traverse`).
    pub traverse_out: bool,
    pub halted: Option<Error>,
}

#[derive(Clone, Copy)]
pub(crate) struct NavObjs {
    pub url: ObjId,
    pub params: ObjId,
    pub iter: ObjId,
    pub location: ObjId,
    pub history: ObjId,
    pub storage_proto: ObjId,
}

#[derive(Default)]
pub(crate) struct Nav {
    pub s: RefCell<NavState>,
    pub p: OnceCell<NavObjs>,
    /// `localStorage` and `sessionStorage`, made on the first read of each (`sto::area_get`).
    pub areas: [Cell<Option<ObjId>>; 2],
    pub lim: Cell<NavLimits>,
}

pub(crate) const NOURL: &str = "document URL is not configured (call Browser::set_document_url)";

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

// ------------------------------------------------------------------------------------------ Host hooks

pub(crate) fn roots(d: &Dom, out: &mut Vec<Value>) {
    let n = d.nav.s.borrow();
    out.extend(n.entries.iter().filter_map(|e| e.cache).map(Value::Object));
}

pub(crate) fn trace(d: &Dom, owner: ObjId, out: &mut Vec<Value>) {
    let n = d.nav.s.borrow();
    if let Some(u) = n.urls.get(&owner) {
        out.extend(u.sp.map(Value::Object));
    } else if let Some(p) = n.params.get(&owner) {
        out.extend(p.url.map(Value::Object));
    } else if let Some(i) = n.iters.get(&owner) {
        out.push(Value::Object(i.sp));
    }
}

pub(crate) fn prune(d: &Dom, live: &dyn Fn(ObjId) -> bool) {
    let mut n = d.nav.s.borrow_mut();
    let n = &mut *n;
    n.urls.retain(|&o, _| live(o));
    n.iters.retain(|&o, _| live(o));
    n.params.retain(|&o, _| live(o));
    n.param_bytes = n.params.values().map(|p| p.bytes).sum();
}

impl Dom {
    /// `Browser::survive`: the navigation tasks stopped on a script budget; let them run again.
    pub(crate) fn clear_budget_halt_nav(&self) {
        let mut s = self.nav.s.borrow_mut();
        if s.halted.as_ref().is_some_and(crate::browser::is_budget) {
            s.halted = None;
        }
    }

    /// An object that is not plain data: DOM wrappers, events, and this module's objects.
    pub(crate) fn is_host_object(&self, o: ObjId) -> bool {
        let n = self.nav.s.borrow();
        let st = self.st.borrow();
        st.by_obj.contains_key(&o) || self.is_abort_controller(o) || self.is_platform_object(o) || st.events.contains_key(&o) || n.urls.contains_key(&o) || n.params.contains_key(&o) || n.iters.contains_key(&o) || self.nav.p.get().is_some_and(|p| [p.location, p.history].contains(&o)) || self.storage_area(o).is_some()
    }

    pub(crate) fn cur_rec(&self) -> R<Rec> {
        let n = self.nav.s.borrow();
        match n.entries.get(n.cur) {
            Some(e) => Ok(e.url.clone()),
            None => Err(Error::Unsupported(unsupported(NOURL))),
        }
    }

    fn fire_window(&self, it: &mut Interp<'_>, ty: &str, props: &[(&str, Value)]) -> R<()> {
        let e = self.new_event(it, ty, false, false, false, None)?;
        for &(k, v) in props {
            let key = self.key(it, k)?;
            it.define_data(e, key, v, false, false, true)?;
        }
        self.dispatch(it, Target::Window, e)?;
        Ok(())
    }

    /// The value of `history.state` for entry `idx`.
    fn entry_state(&self, it: &mut Interp<'_>, idx: usize) -> R<Value> {
        let (state, cache) = {
            let n = self.nav.s.borrow();
            let e = &n.entries[idx];
            (e.state.clone(), e.cache)
        };
        if let Some(c) = cache {
            return Ok(Value::Object(c));
        }
        let Some(state) = state else { return Ok(Value::Null) };
        let v = st::materialize(self, it, &state)?;
        if let Value::Object(o) = v {
            self.nav.s.borrow_mut().entries[idx].cache = Some(o);
        }
        Ok(v)
    }

    fn run_navigation_task(&self, it: &mut Interp<'_>, t: Task) -> R<()> {
        match t {
            Task::Fragment { old, new } => {
                let (o, n) = (self.js_str(it, &old)?, self.js_str(it, &new)?);
                self.fire_window(it, "popstate", &[("state", Value::Null)])?;
                self.fire_window(it, "hashchange", &[("oldURL", o), ("newURL", n)])
            }
            Task::Traverse(delta) => {
                let (old, new, idx) = {
                    let mut n = self.nav.s.borrow_mut();
                    let to = n.cur as i64 + delta as i64;
                    if n.entries.is_empty() || to < 0 || to >= n.entries.len() as i64 {
                        if n.traverse_out && !n.entries.is_empty() && n.requests.len() < self.nav.lim.get().max_requests {
                            let past = if to < 0 { to } else { to - (n.entries.len() as i64 - 1) };
                            let url = n.entries[n.cur].url.href();
                            n.requests.push_back(NavigationRequest { kind: NavigationKind::Traverse(past as i32), from: url.clone(), url, body: None });
                        }
                        return Ok(());
                    }
                    let old = n.entries[n.cur].url.clone();
                    n.cur = to as usize;
                    let new = n.entries[n.cur].url.clone();
                    if old.href() != new.href() {
                        n.generation += 1;
                    }
                    (old, new, n.cur)
                };
                let state = self.entry_state(it, idx)?;
                self.fire_window(it, "popstate", &[("state", state)])?;
                if old.u == new.u && old.frag != new.frag {
                    let (o, n) = (self.js_str(it, &old.href())?, self.js_str(it, &new.href())?);
                    self.fire_window(it, "hashchange", &[("oldURL", o), ("newURL", n)])?;
                }
                Ok(())
            }
        }
    }

    /// Make `rec` the current entry (`replace`) or a new one after it. Caps were checked by the caller.
    fn commit(&self, rec: Rec, state: Option<Rc<State>>, replace: bool) {
        let lim = self.nav.lim.get();
        let mut n = self.nav.s.borrow_mut();
        let n = &mut *n;
        let changed = n.entries[n.cur].url.href() != rec.href();
        let new = Entry { url: rec, state, cache: None };
        let add = new.bytes();
        if replace {
            let old = std::mem::replace(&mut n.entries[n.cur], new);
            n.state_bytes -= old.bytes();
        } else {
            for e in n.entries.drain(n.cur + 1..) {
                n.state_bytes -= e.bytes();
            }
            n.entries.push(new);
            while n.entries.len() > lim.max_history_entries.max(1) {
                n.state_bytes -= n.entries.remove(0).bytes();
            }
            n.cur = n.entries.len() - 1;
        }
        n.state_bytes += add;
        if changed {
            n.generation += 1;
        }
    }

    /// `Location` navigation to `new` (already resolved against the document URL).
    fn navigate(&self, it: &mut Interp<'_>, new: Rec, replace: bool) -> R<()> {
        let lim = self.nav.lim.get();
        let cur = self.cur_rec()?;
        let same = new.href() == cur.href();
        if new.u == cur.u && new.frag.is_some() {
            if self.nav.s.borrow().tasks.len() >= lim.max_tasks {
                return Err(self.lim("nav-tasks"));
            }
            it.charge(1)?;
            let task = Task::Fragment { old: cur.href(), new: new.href() };
            self.commit(new, None, replace || same);
            self.nav.s.borrow_mut().tasks.push_back(task);
            return Ok(());
        }
        if self.nav.s.borrow().requests.len() >= lim.max_requests {
            return Err(self.lim("nav-requests"));
        }
        let kind = if replace || same { NavigationKind::Replace } else { NavigationKind::Assign };
        self.nav.s.borrow_mut().requests.push_back(NavigationRequest { kind, url: new.href(), from: cur.href(), body: None });
        Ok(())
    }

    /// The activation of a link that has a `download` attribute: ask the session to save the target instead of navigating. A `blob:` or `data:`
    /// URL is read now (the Blob store is in this document). False when `a` has no `download`, or no valid URL: the caller navigates.
    pub(crate) fn queue_download(&self, it: &mut Interp<'_>, a: bare_html::dom::NodeId, href: &str) -> R<bool> {
        if !self.nav.s.borrow().downloads_on {
            return Ok(false);
        }
        let name = match { let doc = self.doc.borrow(); doc.find_atom("download").and_then(|k| doc.attr(a, k)).map(str::to_string) } {
            Some(n) => n,
            None => return Ok(false),
        };
        let from = self.cur_rec()?.href();
        let h = href.trim_matches(|c: char| c <= ' ');
        let (url, data) = if crate::browser_web::is_blob_url(h) {
            let Some((bytes, ty)) = crate::browser_web::blob_url_read(self, h) else { return Ok(true) };
            (h.to_string(), Some((bytes, ty)))
        } else if h.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("data:")) {
            let Some(bytes) = crate::images::data_url(h) else { return Ok(true) };
            let head = h[5..].split_once(',').map_or("", |x| x.0).split(';').next().unwrap_or("");
            (h.to_string(), Some((bytes, if head.is_empty() { "text/plain".to_string() } else { head.to_string() })))
        } else {
            match nu::parse(h, Some(&self.cur_rec()?)) {
                Ok(r) => (r.href(), None),
                Err(_) => return Ok(false),
            }
        };
        if self.nav.s.borrow().downloads.len() >= self.nav.lim.get().max_requests {
            return Err(self.lim("nav-requests"));
        }
        it.charge(1 + data.as_ref().map_or(0, |d| d.0.len() / 64) as u64)?;
        self.nav.s.borrow_mut().downloads.push_back(crate::downloads::DownloadRequest { url, name: Some(name), from, data, only: false });
        Ok(true)
    }

    /// An iframe in the tree got a `src` (an insertion or a change). bare loads no child document, but a page may save a file through a
    /// hidden iframe whose `src` replies with a download (Gmail does). So a same-origin http(s) `src` is asked for, and the session
    /// saves the reply only when it is a download; it drops any other reply unread. A cross-origin frame is never asked for: half of a
    /// third-party frame (a sign-in cookie rotation, an ad) could do harm, and no script of it would run.
    pub(crate) fn frame_src(&self, n: bare_html::dom::NodeId) {
        if !self.nav.s.borrow().downloads_on {
            return;
        }
        let src = {
            let doc = self.doc.borrow();
            if !doc.is_html(n, "iframe") {
                return;
            }
            let mut up = n;
            while let Some(p) = doc.parent(up) {
                up = p;
            }
            match (up == doc.root()).then(|| doc.find_atom("src").and_then(|k| doc.attr(n, k))).flatten() {
                Some(s) => s.trim_matches(|c: char| c <= ' ').to_string(),
                None => return,
            }
        };
        let Ok(cur) = self.cur_rec() else { return };
        let Ok(r) = nu::parse(&src, Some(&cur)) else { return };
        let url = r.href();
        if src.is_empty() || r.origin() != cur.origin() || !(url.starts_with("http://") || url.starts_with("https://")) {
            return;
        }
        let mut s = self.nav.s.borrow_mut();
        if s.downloads.len() < self.nav.lim.get().max_requests {
            s.downloads.push_back(crate::downloads::DownloadRequest { url, name: None, from: cur.href(), data: None, only: true });
        }
    }

    /// `href` resolved against the document URL, when the result is an http(s) URL.
    pub(crate) fn resolve_http(&self, href: &str) -> Option<String> {
        let u = nu::parse(href, Some(&self.cur_rec().ok()?)).ok()?.href();
        (u.starts_with("http://") || u.starts_with("https://")).then_some(u)
    }

    /// A click on a link: navigate to `href`, resolved against the document URL. An `href` that is not a valid URL does nothing.
    pub(crate) fn navigate_href(&self, it: &mut Interp<'_>, href: &str) -> R<()> {
        match nu::parse(href, Some(&self.cur_rec()?)) {
            Ok(new) => self.navigate(it, new, false),
            Err(UErr::Gate(w)) => Err(Error::Unsupported(unsupported(w))),
            Err(_) => Ok(()),
        }
    }

    /// A form submission: load `action` (an empty one is the document URL) with `query` in the URL (`GET`) or as the body (`POST`).
    /// An action that is not an http(s) URL does nothing.
    pub(crate) fn submit_navigation(&self, it: &mut Interp<'_>, action: &str, post: bool, query: &str, body: Option<Body>) -> R<()> {
        let cur = self.cur_rec()?;
        let rec = if action.is_empty() {
            cur.clone()
        } else {
            match nu::parse(action, Some(&cur)) {
                Ok(r) => r,
                Err(UErr::Gate(w)) => return Err(Error::Unsupported(unsupported(w))),
                Err(_) => return Ok(()),
            }
        };
        let mut href = rec.href();
        href.truncate(href.find('#').unwrap_or(href.len()));
        if post {
            if self.nav.s.borrow().requests.len() >= self.nav.lim.get().max_requests {
                return Err(self.lim("nav-requests"));
            }
            let body = body.unwrap_or_else(|| Body { content_type: "application/x-www-form-urlencoded".into(), bytes: query.as_bytes().to_vec() });
            self.nav.s.borrow_mut().requests.push_back(NavigationRequest { kind: NavigationKind::Assign, url: href, from: cur.href(), body: Some(body) });
            return Ok(());
        }
        href.truncate(href.find('?').unwrap_or(href.len()));
        href.push('?');
        href.push_str(query);
        match nu::parse(&href, None) {
            Ok(new) => self.navigate(it, new, false),
            Err(UErr::Gate(w)) => Err(Error::Unsupported(unsupported(w))),
            Err(_) => Ok(()),
        }
    }

    fn reload_request(&self) -> R<()> {
        let cur = self.cur_rec()?;
        if self.nav.s.borrow().requests.len() >= self.nav.lim.get().max_requests {
            return Err(self.lim("nav-requests"));
        }
        let href = cur.href();
        self.nav.s.borrow_mut().requests.push_back(NavigationRequest { kind: NavigationKind::Reload, url: href.clone(), from: href, body: None });
        Ok(())
    }
}

// ------------------------------------------------------------------------------------------ Location

/// The `Location` of a frame window: it reads `about:blank` and does not navigate.
pub(crate) fn frame_location(d: &Dom, it: &mut Interp<'_>) -> R<ObjId> {
    let loc = d.nav.p.get().expect("installed").location;
    let proto = it.heap.get_prototype_of(loc).map_err(|e| it.heap_error(e))?;
    d.obj(it, proto.expect("Location.prototype"))
}

fn loc_check(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<()> {
    match (this, d.nav.p.get()) {
        (Value::Object(o), Some(p)) if o == p.location => Ok(()),
        (Value::Object(o), _) if crate::browser_frames::is_location(d, o) => Ok(()),
        _ => Err(it.type_error("Illegal invocation")),
    }
}

/// Is `this` the `Location` of a frame window?
fn loc_is_frame(d: &Dom, this: Value) -> bool {
    matches!(this, Value::Object(o) if crate::browser_frames::is_location(d, o))
}

fn loc_get(d: &Dom, it: &mut Interp<'_>, this: Value, part: Part) -> R<Value> {
    loc_check(d, it, this)?;
    let s = if loc_is_frame(d, this) {
        // `about:blank`: the URL parser here takes http and https only, so the parts are fixed.
        match part {
            Part::Href => "about:blank",
            Part::Protocol => "about:",
            Part::Pathname => "blank",
            _ => "",
        }
        .to_string()
    } else {
        part.get(&d.cur_rec()?)
    };
    d.js_str(it, &s)
}

fn resolve(d: &Dom, it: &mut Interp<'_>, s: &str) -> R<Rec> {
    let cur = d.cur_rec()?;
    nu::parse(s, Some(&cur)).map_err(|e| pr::uerr(d, it, e, &format!("'{s}' is not a valid URL."), true))
}

fn loc_set(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], part: Part) -> R<Value> {
    loc_check(d, it, this)?;
    if loc_is_frame(d, this) {
        return Err(it.type_error("Navigating the location of a frame window is not supported"));
    }
    d.cur_rec()?;
    let v = pr::usv(d, it, arg(a, 0))?;
    let new = if part == Part::Href {
        resolve(d, it, &v)?
    } else {
        let mut c = d.cur_rec()?;
        let before = c.clone();
        if part == Part::Hash {
            // unlike `URL.hash`, an empty value gives an empty fragment, not none
            c.set_fragment(&v);
        } else {
            part.set(&mut c, &v).map_err(|e| pr::uerr(d, it, e, "Invalid URL", true))?;
        }
        if part == Part::Hash && c.frag == before.frag {
            return Ok(Value::Undefined);
        }
        c
    };
    d.navigate(it, new, false)?;
    Ok(Value::Undefined)
}

fn loc_go(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], replace: bool) -> R<Value> {
    loc_check(d, it, this)?;
    if loc_is_frame(d, this) {
        return Err(it.type_error("Navigating the location of a frame window is not supported"));
    }
    d.cur_rec()?;
    if a.is_empty() {
        let name = if replace { "replace" } else { "assign" };
        return Err(it.type_error(&format!("Failed to execute '{name}' on 'Location': 1 argument required, but only 0 present.")));
    }
    let s = pr::usv(d, it, arg(a, 0))?;
    let new = resolve(d, it, &s)?;
    d.navigate(it, new, replace)?;
    Ok(Value::Undefined)
}

fn loc_reload(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    loc_check(d, it, this)?;
    if loc_is_frame(d, this) {
        return Err(it.type_error("Navigating the location of a frame window is not supported"));
    }
    d.reload_request()?;
    Ok(Value::Undefined)
}

fn win_set_location(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let loc = Value::Object(d.nav.p.get().expect("installed").location);
    loc_set(d, it, loc, a, Part::Href)
}

fn loc_obj(d: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    Ok(Value::Object(d.nav.p.get().expect("installed").location))
}

fn doc_url(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    // A document made outside the tree (`createHTMLDocument`, a blank frame) is `about:blank`.
    if d.nd(it, this).is_ok_and(|n| matches!(d.data(n), NodeData::Document) && n != d.doc.borrow().root()) {
        return d.js_str(it, "about:blank");
    }
    let s = d.cur_rec()?.href();
    d.js_str(it, &s)
}

/// `document.domain` (HTML "effective domain"): the host of the origin, `""` for an opaque origin, an IPv6 address in brackets.
fn doc_domain(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    let s = d.cur_rec()?.hostname();
    d.js_str(it, &s)
}

/// Relaxing the origin is gone from browsers (origin-keyed agent clusters). Like Chrome, the setter ignores any value.
fn doc_set_domain(_: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    Ok(Value::Undefined)
}

fn win_origin(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    let s = d.cur_rec()?.origin();
    d.js_str(it, &s)
}

// ------------------------------------------------------------------------------------------ History

fn hist_check(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<()> {
    match (this, d.nav.p.get()) {
        (Value::Object(o), Some(p)) if o == p.history => d.cur_rec().map(drop),
        _ => Err(it.type_error("Illegal invocation")),
    }
}

fn sec_err(d: &Dom, it: &mut Interp<'_>, name: &str, url: &str, cur: &Rec) -> Error {
    d.dom_err(it, "SecurityError", &format!("Failed to execute '{name}' on 'History': A history state object with URL '{url}' cannot be created in a document with origin '{}'.", cur.origin()))
}

fn push_replace(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], replace: bool) -> R<Value> {
    let name = if replace { "replaceState" } else { "pushState" };
    hist_check(d, it, this)?;
    if a.len() < 2 {
        return Err(it.type_error(&format!("Failed to execute '{name}' on 'History': 2 arguments required, but only {} present.", a.len())));
    }
    it.to_js_string(a[1])?;
    let url = match arg(a, 2) {
        Value::Undefined | Value::Null => None,
        v => Some(pr::usv(d, it, v)?),
    };
    let lim = d.nav.lim.get();
    let state = st::serialize(d, it, a[0])?;
    // The URL is read after the state getters ran: they may have navigated.
    let cur = d.cur_rec()?;
    let new = match &url {
        None => cur.clone(),
        Some(s) => match nu::parse(s, Some(&cur)) {
            Ok(r) => r,
            Err(UErr::Invalid | UErr::Scheme) => return Err(sec_err(d, it, name, s, &cur)),
            Err(UErr::Gate(w)) => return Err(Error::Unsupported(unsupported(w))),
        },
    };
    if new.origin() != cur.origin() || new.userinfo.as_deref().filter(|i| !i.0.is_empty() || !i.1.is_empty()) != cur.userinfo.as_deref().filter(|i| !i.0.is_empty() || !i.1.is_empty()) {
        return Err(sec_err(d, it, name, &new.href(), &cur));
    }
    {
        let n = d.nav.s.borrow();
        let old = if replace { n.entries[n.cur].bytes() } else { n.entries[n.cur + 1..].iter().map(Entry::bytes).sum() };
        if n.state_bytes - old + state.bytes > lim.max_total_state_bytes {
            return Err(d.dom_err(it, "DataCloneError", "The states of this document are over their size cap."));
        }
    }
    d.commit(new, Some(Rc::new(state)), replace);
    Ok(Value::Undefined)
}

fn go(d: &Dom, it: &mut Interp<'_>, delta: f64) -> R<Value> {
    let delta = bare_js::value::to_int32(delta);
    if delta == 0 {
        d.reload_request()?;
        return Ok(Value::Undefined);
    }
    if d.nav.s.borrow().tasks.len() >= d.nav.lim.get().max_tasks {
        return Err(d.lim("nav-tasks"));
    }
    it.charge(1)?;
    d.nav.s.borrow_mut().tasks.push_back(Task::Traverse(delta));
    Ok(Value::Undefined)
}

fn hist_go(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    hist_check(d, it, this)?;
    let n = it.to_js_number(arg(a, 0))?;
    go(d, it, n)
}

fn hist_back(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    hist_check(d, it, this)?;
    go(d, it, -1.0)
}

fn hist_forward(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    hist_check(d, it, this)?;
    go(d, it, 1.0)
}

fn hist_length(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    hist_check(d, it, this)?;
    Ok(Value::Number(d.nav.s.borrow().entries.len() as f64))
}

fn hist_state(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    hist_check(d, it, this)?;
    let idx = d.nav.s.borrow().cur;
    d.entry_state(it, idx)
}

fn hist_scroll(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    hist_check(d, it, this)?;
    let s = if d.nav.s.borrow().manual_scroll { "manual" } else { "auto" };
    d.js_str(it, s)
}

fn hist_set_scroll(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    hist_check(d, it, this)?;
    let s = it.to_js_string(arg(a, 0))?;
    let s = it.heap.to_string_lossy(s).map_err(Error::Heap)?;
    match s.as_str() {
        "auto" => d.nav.s.borrow_mut().manual_scroll = false,
        "manual" => d.nav.s.borrow_mut().manual_scroll = true,
        _ => {}
    }
    Ok(Value::Undefined)
}

// ------------------------------------------------------------------------------------------ install

fn def_fn(d: &Dom, it: &mut Interp<'_>, o: ObjId, name: &str, len: u32, h: H) -> R<ObjId> {
    let f = d.add_fn(it, h, name, len, false)?;
    let k = d.key(it, name)?;
    it.define_data(o, k, Value::Object(f), true, false, true)?;
    Ok(f)
}

fn def_acc(d: &Dom, it: &mut Interp<'_>, o: ObjId, name: &str, get: H, set: Option<H>) -> R<()> {
    let g = d.add_fn(it, get, &format!("get {name}"), 0, false)?;
    let s = match set {
        Some(h) => Some(d.add_fn(it, h, &format!("set {name}"), 1, false)?),
        None => None,
    };
    let k = d.key(it, name)?;
    d.define_accessor(it, o, k, g, s)
}

macro_rules! parts {
    ($d:expr, $it:expr, $o:expr, $get:ident, $set:ident, [$(($name:expr, $part:expr)),*]) => {
        $( def_acc($d, $it, $o, $name, |d, it, t, _| $get(d, it, t, $part), Some(|d, it, t, a| $set(d, it, t, a, $part)))?; )*
    };
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = *d.global.get().expect("installed");
    let op = it.object_prototype();
    let mut protos = Vec::new();
    for _ in 0..6 {
        protos.push(d.obj(it, op)?);
    }
    let (url, params, iter, loc_proto, hist_proto, storage_proto) = (protos[0], protos[1], protos[2], protos[3], protos[4], protos[5]);
    let (location, history) = (d.obj(it, loc_proto)?, d.obj(it, hist_proto)?);
    let _ = d.nav.p.set(NavObjs { url, params, iter, location, history, storage_proto });

    // URL
    parts!(d, it, url, url_get_h, url_set_h, [
        ("href", Part::Href), ("protocol", Part::Protocol), ("host", Part::Host), ("hostname", Part::Hostname),
        ("port", Part::Port), ("pathname", Part::Pathname), ("search", Part::Search), ("hash", Part::Hash), ("username", Part::Username), ("password", Part::Password)
    ]);
    def_acc(d, it, url, "origin", pr::url_origin, None)?;
    def_acc(d, it, url, "searchParams", pr::url_search_params, None)?;
    def_fn(d, it, url, "toString", 0, pr::url_to_string)?;
    def_fn(d, it, url, "toJSON", 0, pr::url_to_string)?;

    // URLSearchParams
    for (name, len, h) in [
        ("append", 2, pr::p_append as H), ("delete", 1, pr::p_delete), ("get", 1, pr::p_get), ("getAll", 1, pr::p_get_all),
        ("has", 1, pr::p_has), ("set", 2, pr::p_set), ("sort", 0, pr::p_sort), ("toString", 0, pr::p_to_string),
        ("forEach", 1, pr::p_for_each), ("keys", 0, pr::p_keys), ("values", 0, pr::p_values), ("entries", 0, pr::p_entries),
    ] {
        let f = def_fn(d, it, params, name, len, h)?;
        // `[Symbol.iterator]` is the same function object as `entries`. The key is the realm's own symbol.
        if name == "entries" {
            it.define_data(params, it.symbol_iterator(), Value::Object(f), true, false, true)?;
        }
    }
    // The iterator objects inherit from %IteratorPrototype%, so `iterator[Symbol.iterator]() === iterator`.
    if let Err(e) = it.heap.set_prototype_of(iter, Some(it.iterator_prototype())) {
        return Err(it.heap_error(e));
    }
    def_acc(d, it, params, "size", pr::p_size, None)?;
    def_fn(d, it, iter, "next", 0, pr::iter_next)?;

    // Location
    parts!(d, it, loc_proto, loc_get, loc_set, [
        ("href", Part::Href), ("protocol", Part::Protocol), ("host", Part::Host), ("hostname", Part::Hostname),
        ("port", Part::Port), ("pathname", Part::Pathname), ("search", Part::Search), ("hash", Part::Hash)
    ]);
    def_acc(d, it, loc_proto, "origin", |d, it, t, _| loc_check(d, it, t).and_then(|_| if loc_is_frame(d, t) { d.js_str(it, "null") } else { win_origin(d, it, t, &[]) }), None)?;
    def_fn(d, it, loc_proto, "assign", 1, |d, it, t, a| loc_go(d, it, t, a, false))?;
    def_fn(d, it, loc_proto, "replace", 1, |d, it, t, a| loc_go(d, it, t, a, true))?;
    def_fn(d, it, loc_proto, "reload", 0, loc_reload)?;
    def_fn(d, it, loc_proto, "toString", 0, |d, it, t, _| loc_get(d, it, t, Part::Href))?;

    // History
    def_fn(d, it, hist_proto, "pushState", 2, |d, it, t, a| push_replace(d, it, t, a, false))?;
    def_fn(d, it, hist_proto, "replaceState", 2, |d, it, t, a| push_replace(d, it, t, a, true))?;
    def_fn(d, it, hist_proto, "go", 0, hist_go)?;
    def_fn(d, it, hist_proto, "back", 0, hist_back)?;
    def_fn(d, it, hist_proto, "forward", 0, hist_forward)?;
    def_acc(d, it, hist_proto, "length", hist_length, None)?;
    def_acc(d, it, hist_proto, "state", hist_state, None)?;
    def_acc(d, it, hist_proto, "scrollRestoration", hist_scroll, Some(hist_set_scroll))?;

    // Storage
    // The order of Chrome (a `for in` over a `Storage` lists them so).
    def_acc(d, it, storage_proto, "length", sto::length, None)?;
    for (name, len, h) in [("clear", 0, sto::clear as H), ("getItem", 1, sto::get_item), ("key", 1, sto::key), ("removeItem", 1, sto::remove_item), ("setItem", 2, sto::set_item)] {
        // WebIDL operations are enumerable (a `for in` over a `Storage` lists them).
        let f = def_fn(d, it, storage_proto, name, len, h)?;
        let k = d.key(it, name)?;
        it.define_data(storage_proto, k, Value::Object(f), true, true, true)?;
    }

    // document and window
    let p = d.p();
    def_acc(d, it, p.document, "URL", doc_url, None)?;
    def_acc(d, it, p.document, "documentURI", doc_url, None)?;
    def_acc(d, it, p.document, "cookie", crate::browser_cookie::doc_cookie_get, Some(crate::browser_cookie::doc_cookie_set))?;
    def_acc(d, it, p.document, "domain", doc_domain, Some(doc_set_domain))?;
    def_acc(d, it, p.node, "baseURI", doc_url, None)?;
    def_acc(d, it, p.document, "location", loc_obj, Some(win_set_location))?;
    def_acc(d, it, g, "location", loc_obj, Some(win_set_location))?;
    def_acc(d, it, g, "origin", win_origin, None)?;

    // constructors and globals
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    let mut fixed = vec![url, params, iter, loc_proto, hist_proto, storage_proto, location, history];
    let ifaces: [(&str, ObjId, H, bool); 5] = [
        ("URL", url, pr::url_ctor, true),
        ("URLSearchParams", params, pr::params_ctor, true),
        ("Location", loc_proto, illegal, false),
        ("History", hist_proto, illegal, false),
        ("Storage", storage_proto, illegal, true),
    ];
    for (name, proto, h, ctor) in ifaces {
        let f = match name {
            "URL" => d.add_ctor_nt(it, h, name, 1)?,
            "URLSearchParams" => d.add_ctor_nt(it, h, name, 0)?,
            "Storage" => d.add_fn(it, h, name, 0, ctor)?,
            _ => d.add_fn(it, h, name, 1, ctor)?,
        };
        it.define_data(f, kp, Value::Object(proto), false, false, false)?;
        it.define_data(proto, kc, Value::Object(f), true, false, true)?;
        let k = d.key(it, name)?;
        it.define_data(g, k, Value::Object(f), true, false, true)?;
        if name == "URL" {
            def_fn(d, it, f, "canParse", 1, pr::url_can_parse)?;
            def_fn(d, it, f, "createObjectURL", 1, crate::browser_web::create_object_url)?;
            def_fn(d, it, f, "revokeObjectURL", 1, crate::browser_web::revoke_object_url)?;
        }
        fixed.push(f);
    }
    // `Storage.prototype[Symbol.toStringTag]` is "Storage".
    let (ks, kt) = (d.key(it, "Symbol")?, d.key(it, "toStringTag")?);
    let sy = it.get_value(Value::Object(g), ks)?;
    if let Value::Symbol(sym) = it.get_value(sy, kt)? {
        let tag = Value::String(it.heap.intern_str("Storage").map_err(|e| it.heap_error(e))?);
        it.define_data(storage_proto, bare_js::value::PropertyKey::Symbol(sym), tag, false, false, true)?;
    }
    let k = d.key(it, "history")?;
    it.define_data(g, k, Value::Object(history), true, true, true)?;
    // The two areas are host named objects (`localStorage.x` is `getItem('x')`). Each is made on its first read: a page that reads
    // neither keeps `Heap::has_exotics` false, and every property write skips the exotic checks.
    def_acc(d, it, g, "localStorage", |d, it, _, _| sto::area_get(d, it, false), None)?;
    def_acc(d, it, g, "sessionStorage", |d, it, _, _| sto::area_get(d, it, true), None)?;
    d.fixed.borrow_mut().extend(fixed);
    Ok(())
}

fn url_get_h(d: &Dom, it: &mut Interp<'_>, t: Value, part: Part) -> R<Value> {
    pr::url_get(d, it, t, part)
}

fn url_set_h(d: &Dom, it: &mut Interp<'_>, t: Value, a: &[Value], part: Part) -> R<Value> {
    pr::url_set(d, it, t, a, part)
}

fn illegal(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    Err(it.type_error("Illegal constructor"))
}

// ------------------------------------------------------------------------------------------ Browser API

impl Browser {
    /// Replace the bounds of this module. Call it before scripts run.
    pub fn set_navigation_limits(&mut self, l: NavLimits) {
        self.dom.nav.lim.set(l);
    }

    /// Reset navigation at `url` (absolute `http` or `https`): one history entry with a null state, empty queues.
    /// This does not replace the realm. Create a new Browser for a new document.
    /// Pending navigation requests and tasks of the old document are dropped.
    pub fn set_document_url(&mut self, url: &str) -> Result<(), String> {
        let rec = nu::parse(url, None).map_err(|e| match e {
            UErr::Invalid => format!("not an absolute URL: {url}"),
            UErr::Scheme => "only http and https URLs are supported".to_string(),
            UErr::Gate(w) => format!("not supported: {w}"),
        })?;
        let mut n = self.dom.nav.s.borrow_mut();
        n.entries.clear();
        n.entries.push(Entry { url: rec, state: None, cache: None });
        n.cur = 0;
        n.tasks.clear();
        n.requests.clear();
        n.state_bytes = 0;
        n.generation += 1;
        Ok(())
    }

    /// The current document URL (with its fragment), or `None` until it is configured.
    pub fn document_url(&self) -> Option<String> {
        self.dom.cur_rec().ok().map(|r| r.href())
    }

    /// `document.baseURI`. It is the document URL (`<base>` is not read).
    pub fn base_uri(&self) -> Option<String> {
        self.document_url()
    }

    /// Resolve `input` against the document URL the way `location.assign` does. For the loader and fetch.
    pub fn resolve_url(&self, input: &str) -> Result<String, String> {
        let cur = self.dom.cur_rec().map_err(|_| NOURL.to_string())?;
        match nu::parse(input, Some(&cur)) {
            Ok(r) => Ok(r.href()),
            Err(UErr::Invalid) => Err(format!("not a valid URL: {input}")),
            Err(UErr::Scheme) => Err("only http and https URLs are supported".to_string()),
            Err(UErr::Gate(w)) => Err(format!("not supported: {w}")),
        }
    }

    /// The address bar with `url`. When it is the document URL with a fragment, it is a fragment navigation in this document, as in
    /// Chrome: a new entry, and `hashchange` at the next navigation turn. The page keeps its state and its requests. `false`: `url` is
    /// another document (or not a valid URL), and the caller loads it.
    pub fn navigate_same_document(&mut self, url: &str) -> R<bool> {
        let dom = self.dom.clone();
        let cur = dom.cur_rec()?;
        let Ok(new) = nu::parse(url, Some(&cur)) else { return Ok(false) };
        if new.u != cur.u || new.frag.is_none() {
            return Ok(false);
        }
        self.it.host_scope(|it| dom.navigate(it, new, false))?;
        Ok(true)
    }

    /// Raised each time the document URL changes (script, traversal, fragment navigation, `set_document_url`).
    pub fn url_generation(&self) -> u64 {
        self.dom.nav.s.borrow().generation
    }

    /// Run the queued traversal and fragment tasks that were queued when this call started: `popstate`, `hashchange`.
    /// Each task shares its step budget with a microtask checkpoint. A failed checkpoint, `Limit`, `Unsupported`
    /// or fault halts this pump permanently. The remaining tasks stay queued. Returns the tasks that ran.
    pub fn run_navigation_tasks(&mut self) -> R<usize> {
        self.run_navigation_tasks_up_to(usize::MAX)
    }

    /// Run at most `max` of the tasks queued at entry. Preserve the pump's checkpoint and failure rules.
    pub fn run_navigation_tasks_up_to(&mut self, max: usize) -> R<usize> {
        let dom = self.dom.clone();
        if let Some(e) = dom.nav.s.borrow().halted { return Err(e); }
        let result = self.run_navigation_tasks_inner(max);
        if let Err(e) = result { dom.nav.s.borrow_mut().halted = Some(e); }
        result
    }

    fn run_navigation_tasks_inner(&mut self, max: usize) -> R<usize> {
        let dom = self.dom.clone();
        // An empty queue can still hold a sticky failed-job error.
        self.it.host_scope(|it| it.host_checkpoint().map(|_| ()))?;
        let n = dom.nav.s.borrow().tasks.len().min(max);
        let mut done = 0;
        for _ in 0..n {
            let t = dom.nav.s.borrow_mut().tasks.pop_front();
            let Some(t) = t else { break };
            done += 1;
            dom.st.borrow_mut().driver = true;
            let r = self.it.host_scope(|it| {
                dom.run_navigation_task(it, t)?;
                it.host_checkpoint().map(|_| ())
            });
            dom.st.borrow_mut().driver = false;
            r?;
        }
        Ok(done)
    }

    /// The oldest document load that script asked for, if any.
    /// The session keeps a history of documents: `history.go(n)` past the end of this document becomes a request.
    pub fn cross_document_history(&mut self, on: bool) {
        self.dom.nav.s.borrow_mut().traverse_out = on;
    }

    /// `history.go(delta)` as the user does it (Alt+Left, the back button of a mouse): the same queued task.
    pub fn history_go(&mut self, delta: i32) -> R<()> {
        if self.dom.nav.s.borrow().entries.is_empty() {
            return Ok(());
        }
        self.dom.nav.s.borrow_mut().tasks.push_back(Task::Traverse(delta));
        Ok(())
    }

    pub fn take_navigation_request(&mut self) -> Option<NavigationRequest> {
        self.dom.nav.s.borrow_mut().requests.pop_front()
    }

    /// The session saves the targets of links with a `download` attribute.
    pub fn enable_downloads(&mut self) {
        self.dom.nav.s.borrow_mut().downloads_on = true;
    }

    /// A link asked for a download that the session has not started.
    pub fn has_download_request(&self) -> bool {
        !self.dom.nav.s.borrow().downloads.is_empty()
    }

    pub fn take_download_request(&mut self) -> Option<crate::downloads::DownloadRequest> {
        self.dom.nav.s.borrow_mut().downloads.pop_front()
    }

    pub fn navigation_stats(&self) -> NavigationStats {
        let n = self.dom.nav.s.borrow();
        NavigationStats {
            history_len: n.entries.len(),
            history_index: n.cur,
            pending_tasks: n.tasks.len(),
            pending_requests: n.requests.len(),
            urls: n.urls.len(),
            params: n.params.len(),
            iterators: n.iters.len(),
            param_bytes: n.param_bytes,
            state_bytes: n.state_bytes,
            url_generation: n.generation,
        }
    }

    /// The storage of this browser. Clones share the data (`WebStorage::new_session` shares only `localStorage`).
    pub fn storage(&self) -> WebStorage {
        self.dom.nav.s.borrow().store.clone()
    }

    /// Use `store` (a new browser for the same session, for example). The old store is not touched.
    pub fn attach_storage(&mut self, store: WebStorage) {
        self.dom.nav.s.borrow_mut().store = store;
    }
}
