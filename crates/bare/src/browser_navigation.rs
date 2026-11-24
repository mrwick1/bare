//! Navigation and origin-scoped storage for the browser host: `URL`, `URLSearchParams`, `location`, `history`,
//! `document.URL/baseURI`, `localStorage`, `sessionStorage`. No network, no file. Files:
//! `browser_navigation_url.rs` (URL records over `bare_url`), `_params.rs` (JS `URL`/`URLSearchParams`), `_state.rs`
//! (history state cloning), `_storage.rs` (the store and `Storage`), `_tests.rs`.
//!
//! The document URL
//! - `Browser::new` has no URL. Until `Browser::set_document_url(url)` is called, everything that needs one
//!   (`location.*`, `history.*`, `document.URL/baseURI`, `localStorage`, `sessionStorage`, `window.origin`) stops the
//!   script with `Fault::Unsupported("document URL is not configured ...")`. Nothing pretends to be a staging origin.
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
//! GC: URL wrappers, params wrappers and iterators are owners (`Owner::Url`, `Params`, `Iter`). Each keeps its edges in `Traced`
//! fields, `trace_owner` visits them and `prune_owner` drops the entries of dead ones (`docs/internal/design-host-tracing.md`).
//! The permanent objects (`location`, `history`, `localStorage`, `sessionStorage`, their prototypes) are held by persistent handles.
//! The cloned state of an entry is native data; its materialized object is cached in a persistent handle that the entry owns,
//! so it is a root exactly while the entry lives.

use crate::eng::{Eng, Fault, Ops, Val};
use crate::browser::*;
use crate::browser_navigation_params as pr;
use crate::browser_scoped::*;
use crate::browser_navigation_state as st;
use bare_js_api::Cloned;
use crate::browser_navigation_storage as sto;
pub use crate::browser_navigation_storage::{StorageLimits, StoreError, WebStorage};
use crate::browser_navigation_url::{self as nu, Part, Rec, UErr};
use bare_html::dom::NodeData;
use bare_js_api::{Args, HostFn, Local};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::VecDeque;

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
    pub state: Option<Cloned>,
    /// The object that `history.state` returned. A root while the entry lives: the handle goes with the entry.
    pub cache: Option<bare_js_api::Persistent>,
}

impl Entry {
    fn bytes(&self) -> usize {
        self.state.as_ref().map_or(0, Cloned::cost)
    }
}

pub(crate) enum Task {
    /// `history.go(n)`, n not 0. The target is computed when the task runs.
    Traverse(i32),
    /// A fragment navigation that already changed the URL.
    Fragment { old: String, new: String },
}

#[derive(Default)]
pub(crate) struct NavState<E: Eng> {
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
    pub urls: IdMap<<E as Eng>::Obj, pr::UrlObj<E>>,
    pub params: IdMap<<E as Eng>::Obj, pr::ParamsObj<E>>,
    pub iters: IdMap<<E as Eng>::Obj, pr::IterObj<E>>,
    pub param_bytes: usize,
    pub state_bytes: usize,
    pub store: WebStorage,
    pub manual_scroll: bool,
    /// The session keeps a history of documents: `go(n)` past the end of this one is a request (`NavigationKind::Traverse`).
    pub traverse_out: bool,
    pub halted: Option<Fault<E>>,
}

/// The permanent objects. The ids are valid while `held` holds the objects.
pub(crate) struct NavObjs<E: Eng> {
    pub url: E::Obj,
    pub params: E::Obj,
    pub iter: E::Obj,
    pub location: E::Obj,
    pub history: E::Obj,
    pub storage_proto: E::Obj,
    pub _held: [bare_js_api::Persistent; 6],
}

#[derive(Default)]
pub(crate) struct Nav<E: Eng> {
    pub s: RefCell<NavState<E>>,
    pub p: OnceCell<NavObjs<E>>,
    /// `localStorage` and `sessionStorage`, made on the first read of each (`sto::area_get`).
    pub areas: [Cell<Option<<E as Eng>::Obj>>; 2],
    /// The handles that hold the two areas.
    pub area_held: RefCell<Vec<bare_js_api::Persistent>>,
    pub lim: Cell<NavLimits>,
}

pub(crate) const NOURL: &str = "document URL is not configured (call Browser::set_document_url)";

// ------------------------------------------------------------------------------------------ Host hooks

/// The edges of the owner `w`: the `searchParams` of a URL, the URL of a params object, the params object of an iterator.
pub(crate) fn trace_owner<E: Eng>(d: &Dom<E>, w: Owner<E>, t: &mut bare_js_api::Tracer<'_, E::Raw>) {
    let n = d.nav.s.borrow();
    let edge = match w {
        Owner::Url(o) => n.urls.get(&o).and_then(|u| u.sp.as_ref()),
        Owner::Params(o) => n.params.get(&o).and_then(|p| p.url.as_ref()),
        Owner::Iter(o) => n.iters.get(&o).map(|i| &i.sp),
        _ => None,
    };
    if let Some(e) = edge {
        t.visit(e);
    }
}

/// The owner `w` is dead: drop its entry.
pub(crate) fn prune_owner<E: Eng>(d: &Dom<E>, w: Owner<E>) {
    let mut n = d.nav.s.borrow_mut();
    match w {
        Owner::Url(o) => {
            n.urls.remove(&o);
        }
        Owner::Params(o) => {
            if let Some(p) = n.params.remove(&o) {
                n.param_bytes -= p.bytes;
            }
        }
        Owner::Iter(o) => {
            n.iters.remove(&o);
        }
        _ => {}
    }
}

impl<E: Eng> Dom<E> {
    /// `Browser::survive`: the navigation tasks stopped on a script budget; let them run again.
    pub(crate) fn clear_budget_halt_nav(&self) {
        let mut s = self.nav.s.borrow_mut();
        if s.halted.as_ref().is_some_and(crate::browser::is_budget::<E>) {
            s.halted = None;
        }
    }

    /// An object that is not plain data: DOM wrappers, events, and this module's objects.
    pub(crate) fn is_host_object(&self, o: E::Obj) -> bool {
        let n = self.nav.s.borrow();
        let st = self.st.borrow();
        st.by_obj.contains_key(&o) || self.is_abort_controller(o) || self.is_platform_object(o) || st.events.contains_key(&o) || n.urls.contains_key(&o) || n.params.contains_key(&o) || n.iters.contains_key(&o) || self.nav.p.get().is_some_and(|p| [p.location, p.history].contains(&o)) || self.storage_area(o).is_some()
    }

    pub(crate) fn cur_rec(&self) -> R<E, Rec> {
        let n = self.nav.s.borrow();
        match n.entries.get(n.cur) {
            Some(e) => Ok(e.url.clone()),
            None => Err(Fault::Unsupported(NOURL)),
        }
    }

    fn fire_window(&self, it: &mut E::Vm, ty: &str, props: &[(&str, E::Raw)]) -> R<E, ()> {
        let e = self.new_event(it, ty, false, false, false, None)?;
        for &(k, v) in props {
            let key = self.key(it, k)?;
            it.define_data(e, key, v, false, false, true)?;
        }
        self.dispatch(it, Target::Window, e)?;
        Ok(())
    }

    /// The value of `history.state` for entry `idx`.
    fn entry_state(&self, it: &mut E::Vm, idx: usize) -> R<E, E::Raw> {
        let cache = {
            let n = self.nav.s.borrow();
            n.entries[idx].cache.as_ref().map(|p| it.persistent_value(p))
        };
        if let Some(c) = cache {
            return Ok(c);
        }
        // The record leaves the entry while the graph is built, so no borrow of the navigation state is held.
        let Some(mut state) = self.nav.s.borrow_mut().entries[idx].state.take() else { return Ok(E::null()) };
        let v = run_v(it, |s| st::materialize(self, s, &mut state));
        self.nav.s.borrow_mut().entries[idx].state = Some(state);
        let v = v?;
        if let Val::Object(_) = E::kind(v) {
            let h = it.persist_value(v);
            self.nav.s.borrow_mut().entries[idx].cache = Some(h);
        }
        Ok(v)
    }

    fn run_navigation_task(&self, it: &mut E::Vm, t: Task) -> R<E, ()> {
        match t {
            Task::Fragment { old, new } => {
                let (o, n) = (self.js_str(it, &old)?, self.js_str(it, &new)?);
                self.fire_window(it, "popstate", &[("state", E::null())])?;
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
    fn commit(&self, rec: Rec, state: Option<Cloned>, replace: bool) {
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
    fn navigate(&self, it: &mut E::Vm, new: Rec, replace: bool) -> R<E, ()> {
        let lim = self.nav.lim.get();
        let cur = self.cur_rec()?;
        let same = new.href() == cur.href();
        if new.u == cur.u && new.frag.is_some() {
            if self.nav.s.borrow().tasks.len() >= lim.max_tasks {
                return Err(self.lim("nav-tasks"));
            }
            it.charge_o(1)?;
            let task = Task::Fragment { old: cur.href(), new: new.href() };
            self.commit(new, None, replace || same);
            // The same address again replaces the entry and fires no event, as in Chrome (to verify: from the HTML Standard, which fires
            // `hashchange` only when the fragment changes). A page that routes on `hashchange` must not route twice (issue #564).
            if !same {
                self.nav.s.borrow_mut().tasks.push_back(task);
            }
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
    pub(crate) fn queue_download(&self, it: &mut E::Vm, a: bare_html::dom::NodeId, href: &str) -> R<E, bool> {
        if !self.nav.s.borrow().downloads_on {
            return Ok(false);
        }
        let found = { let doc = self.doc.borrow(); doc.find_atom("download").and_then(|k| doc.attr(a, k)).map(str::to_string) };
        let name = match found {
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
        it.charge_o(1 + data.as_ref().map_or(0, |d| d.0.len() / 64) as u64)?;
        self.nav.s.borrow_mut().downloads.push_back(crate::downloads::DownloadRequest { url, name: Some(name), from, data, only: false });
        Ok(true)
    }

    /// An iframe in the tree got a `src` (an insertion or a change). An http(s) `src` other than `about:blank` makes a child page
    /// (`browser_child.rs`). Without the child driver (a `Browser` that `run_page` never ran), a same-origin http(s) `src` is asked for
    /// only to catch a download: the session saves the reply when it is one and drops any other reply unread (Gmail saves a file
    /// through a hidden iframe). A `src` that was removed or made blank drops the child page.
    pub(crate) fn frame_src(&self, it: &mut E::Vm, n: bare_html::dom::NodeId) {
        let kids = self.kids.configured();
        if !kids && !self.nav.s.borrow().downloads_on {
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
            if up != doc.root() {
                return;
            }
            let src = doc.find_atom("src").and_then(|k| doc.attr(n, k));
            match src {
                Some(s) => s.trim_matches(|c: char| c <= ' ').to_string(),
                None if kids => String::new(),
                None => return,
            }
        };
        let Ok(cur) = self.cur_rec() else { return };
        let resolved = if src.is_empty() { None } else { nu::parse(&src, Some(&cur)).ok() };
        let url = resolved.as_ref().map(|r| r.href()).filter(|u| (u.starts_with("http://") || u.starts_with("https://")) && !u.starts_with("about:"));
        // `srcdoc` wins over `src`: such a frame stays blank.
        let srcdoc = { let doc = self.doc.borrow(); doc.find_atom("srcdoc").is_some_and(|k| doc.attr(n, k).is_some()) };
        let url = url.filter(|_| !srcdoc);
        if kids {
            self.kids.request(self, it, n, url);
            return;
        }
        let (Some(url), Some(r)) = (url, resolved) else { return };
        if r.origin() != cur.origin() {
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
    pub(crate) fn navigate_href(&self, it: &mut E::Vm, href: &str) -> R<E, ()> {
        match nu::parse(href, Some(&self.cur_rec()?)) {
            Ok(new) => self.navigate(it, new, false),
            Err(UErr::Gate(w)) => Err(Fault::Unsupported(w)),
            Err(_) => Ok(()),
        }
    }

    /// A form submission: load `action` (an empty one is the document URL) with `query` in the URL (`GET`) or as the body (`POST`).
    /// An action that is not an http(s) URL does nothing.
    pub(crate) fn submit_navigation(&self, it: &mut E::Vm, action: &str, post: bool, query: &str, body: Option<Body>) -> R<E, ()> {
        let cur = self.cur_rec()?;
        let rec = if action.is_empty() {
            cur.clone()
        } else {
            match nu::parse(action, Some(&cur)) {
                Ok(r) => r,
                Err(UErr::Gate(w)) => return Err(Fault::Unsupported(w)),
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
            Err(UErr::Gate(w)) => Err(Fault::Unsupported(w)),
            Err(_) => Ok(()),
        }
    }

    fn reload_request(&self) -> R<E, ()> {
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
pub(crate) fn frame_location<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, E::Obj> {
    let loc = d.nav.p.get().expect("installed").location;
    let proto = it.h_get_proto(loc)?;
    d.obj(it, proto.expect("Location.prototype"))
}

/// Is `this` a `Location`? `true` for the `Location` of a frame window (it reads `about:blank` and does not navigate).
fn loc_check<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, bool> {
    match (obj_id::<E>(s, this), d.nav.p.get()) {
        (Some(o), Some(p)) if o == p.location => Ok(false),
        (Some(o), _) if crate::browser_frames::is_location(d, o) => Ok(true),
        _ => Err(s.type_error("Illegal invocation")),
    }
}

fn loc_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, part: Part) -> Res<'s, Local<'s>> {
    let frame = loc_check(d, s, this)?;
    let text = if frame {
        // `about:blank`: the URL parser here takes http and https only, so the parts are fixed.
        match part {
            Part::Href => "about:blank",
            Part::Protocol => "about:",
            Part::Pathname => "blank",
            _ => "",
        }
        .to_string()
    } else {
        part.get(&lift::<E, _>(s, d.cur_rec())?)
    };
    d.js_str_s(s, &text)
}

fn resolve<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, text: &str) -> Res<'s, Rec> {
    let cur = lift::<E, _>(s, d.cur_rec())?;
    match nu::parse(text, Some(&cur)) {
        Ok(r) => Ok(r),
        Err(e) => Err(pr::uerr(d, s, e, &format!("'{text}' is not a valid URL."), true)),
    }
}

fn loc_set<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>, part: Part) -> Res<'s, Local<'s>> {
    if loc_check(d, s, this)? {
        return Err(s.type_error("Navigating the location of a frame window is not supported"));
    }
    lift::<E, _>(s, d.cur_rec())?;
    let v = arg_s::<E>(s, a, 0);
    let v = pr::usv(d, s, v)?;
    let new = if part == Part::Href {
        resolve(d, s, &v)?
    } else {
        let mut c = lift::<E, _>(s, d.cur_rec())?;
        let before = c.clone();
        if part == Part::Hash {
            // unlike `URL.hash`, an empty value gives an empty fragment, not none
            c.set_fragment(&v);
        } else if let Err(e) = part.set(&mut c, &v) {
            return Err(pr::uerr(d, s, e, "Invalid URL", true));
        }
        if part == Part::Hash && c.frag == before.frag {
            return Ok(s.undefined());
        }
        c
    };
    old::<E, _>(s, |it| d.navigate(it, new, false))?;
    Ok(s.undefined())
}

fn loc_go<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>, replace: bool) -> Res<'s, Local<'s>> {
    if loc_check(d, s, this)? {
        return Err(s.type_error("Navigating the location of a frame window is not supported"));
    }
    lift::<E, _>(s, d.cur_rec())?;
    let Some(arg0) = a.get(0) else {
        let name = if replace { "replace" } else { "assign" };
        return Err(s.type_error(&format!("Failed to execute '{name}' on 'Location': 1 argument required, but only 0 present.")));
    };
    let text = pr::usv(d, s, arg0)?;
    let new = resolve(d, s, &text)?;
    old::<E, _>(s, |it| d.navigate(it, new, replace))?;
    Ok(s.undefined())
}

fn loc_reload<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    if loc_check(d, s, this)? {
        return Err(s.type_error("Navigating the location of a frame window is not supported"));
    }
    lift::<E, _>(s, d.reload_request())?;
    Ok(s.undefined())
}

fn win_set_location<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let loc = d.obj_local(s, d.nav.p.get().expect("installed").location);
    loc_set(d, s, loc, a, Part::Href)
}

fn loc_obj<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Ok(d.obj_local(s, d.nav.p.get().expect("installed").location))
}

fn doc_url<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    // A document made outside the tree (`createHTMLDocument`, a blank frame) is `about:blank`.
    if d.node_of_s(s, this).is_some_and(|n| matches!(d.data(n), NodeData::Document) && n != d.doc.borrow().root()) {
        return d.js_str_s(s, "about:blank");
    }
    let text = lift::<E, _>(s, d.cur_rec())?.href();
    d.js_str_s(s, &text)
}

/// `document.domain` (HTML "effective domain"): the host of the origin, `""` for an opaque origin, an IPv6 address in brackets.
fn doc_domain<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let text = lift::<E, _>(s, d.cur_rec())?.hostname();
    d.js_str_s(s, &text)
}

/// Relaxing the origin is gone from browsers (origin-keyed agent clusters). Like Chrome, the setter ignores any value.
fn doc_set_domain<'s, E: Eng>(_: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Ok(s.undefined())
}

fn win_origin<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let text = lift::<E, _>(s, d.cur_rec())?.origin();
    d.js_str_s(s, &text)
}

// ------------------------------------------------------------------------------------------ History

fn hist_check<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, ()> {
    match (obj_id::<E>(s, this), d.nav.p.get()) {
        (Some(o), Some(p)) if o == p.history => lift::<E, _>(s, d.cur_rec().map(drop)),
        _ => Err(s.type_error("Illegal invocation")),
    }
}

fn sec_err<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, name: &str, url: &str, cur: &Rec) -> bare_js_api::Error<'s> {
    d.dom_err_s(s, "SecurityError", &format!("Failed to execute '{name}' on 'History': A history state object with URL '{url}' cannot be created in a document with origin '{}'.", cur.origin()))
}

fn push_replace<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>, replace: bool) -> Res<'s, Local<'s>> {
    let name = if replace { "replaceState" } else { "pushState" };
    hist_check(d, s, this)?;
    if a.len() < 2 {
        return Err(s.type_error(&format!("Failed to execute '{name}' on 'History': 2 arguments required, but only {} present.", a.len())));
    }
    s.to_string(a.get(1).expect("checked"))?;
    let url = match a.get(2) {
        Some(v) if !is_nullish::<E>(s, v) => Some(pr::usv(d, s, v)?),
        _ => None,
    };
    let lim = d.nav.lim.get();
    let state = st::serialize(d, s, a.get(0).expect("checked"))?;
    // The URL is read after the state getters ran: they may have navigated.
    let cur = lift::<E, _>(s, d.cur_rec())?;
    let new = match &url {
        None => cur.clone(),
        Some(text) => match nu::parse(text, Some(&cur)) {
            Ok(r) => r,
            Err(UErr::Invalid | UErr::Scheme) => return Err(sec_err(d, s, name, text, &cur)),
            Err(UErr::Gate(w)) => return Err(lift_err::<E>(s, Fault::Unsupported::<E>(w))),
        },
    };
    if new.origin() != cur.origin() || new.userinfo.as_deref().filter(|i| !i.0.is_empty() || !i.1.is_empty()) != cur.userinfo.as_deref().filter(|i| !i.0.is_empty() || !i.1.is_empty()) {
        return Err(sec_err(d, s, name, &new.href(), &cur));
    }
    {
        let n = d.nav.s.borrow();
        let old = if replace { n.entries[n.cur].bytes() } else { n.entries[n.cur + 1..].iter().map(Entry::bytes).sum() };
        if n.state_bytes - old + state.cost() > lim.max_total_state_bytes {
            drop(n);
            return Err(d.dom_err_s(s, "DataCloneError", "The states of this document are over their size cap."));
        }
    }
    d.commit(new, Some(state), replace);
    Ok(s.undefined())
}

fn go<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, delta: f64) -> Res<'s, Local<'s>> {
    let delta = crate::js_num::to_int32(delta);
    if delta == 0 {
        lift::<E, _>(s, d.reload_request())?;
        return Ok(s.undefined());
    }
    if d.nav.s.borrow().tasks.len() >= d.nav.lim.get().max_tasks {
        return Err(d.lim_s(s, "nav-tasks"));
    }
    s.charge(1)?;
    d.nav.s.borrow_mut().tasks.push_back(Task::Traverse(delta));
    Ok(s.undefined())
}

fn hist_go<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    hist_check(d, s, this)?;
    let v = arg_s::<E>(s, a, 0);
    let n = s.to_number(v)?;
    go(d, s, n)
}

fn hist_back<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    hist_check(d, s, this)?;
    go(d, s, -1.0)
}

fn hist_forward<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    hist_check(d, s, this)?;
    go(d, s, 1.0)
}

fn hist_length<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    hist_check(d, s, this)?;
    let n = d.nav.s.borrow().entries.len();
    Ok(s.number(n as f64))
}

fn hist_state<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    hist_check(d, s, this)?;
    let idx = d.nav.s.borrow().cur;
    old_v::<E>(s, |it| d.entry_state(it, idx))
}

fn hist_scroll<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    hist_check(d, s, this)?;
    let text = if d.nav.s.borrow().manual_scroll { "manual" } else { "auto" };
    d.js_str_s(s, text)
}

fn hist_set_scroll<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    hist_check(d, s, this)?;
    let v = arg_s::<E>(s, a, 0);
    let t = if s.as_str(v).is_some() { v } else { s.to_string(v)? };
    let text = s.as_str(t).map(|t| t.to_string_lossy()).unwrap_or_default();
    match text.as_str() {
        "auto" => d.nav.s.borrow_mut().manual_scroll = false,
        "manual" => d.nav.s.borrow_mut().manual_scroll = true,
        _ => {}
    }
    Ok(s.undefined())
}

// ------------------------------------------------------------------------------------------ install

/// A row for a part of a URL: the getter and the setter of `$part`.
macro_rules! part {
    ($get:ident, $set:ident, $name:expr, $part:expr) => {
        Row::get(1, $name, |d, s, t, _| $get(d, s, t, $part), Some(|d, s, t, a| $set(d, s, t, a, $part)))
    };
}

pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    let g = s.global();
    let mut protos = Vec::new();
    for _ in 0..6 {
        protos.push(d.plain_s(s)?);
    }
    let (url, params, iter, loc_proto, hist_proto, storage_proto) = (protos[0], protos[1], protos[2], protos[3], protos[4], protos[5]);
    let (location, history) = (d.obj_s(s, loc_proto)?, d.obj_s(s, hist_proto)?);
    let held = [url, params, iter, location, history, storage_proto].map(|o| {
        let l = d.obj_local(s, o);
        s.persist(l)
    });
    let _ = d.nav.p.set(NavObjs { url, params, iter, location, history, storage_proto, _held: held });

    // URL
    table!(rows1 = [
        part!(url_get_h, url_set_h, "href", Part::Href),
        part!(url_get_h, url_set_h, "protocol", Part::Protocol),
        part!(url_get_h, url_set_h, "host", Part::Host),
        part!(url_get_h, url_set_h, "hostname", Part::Hostname),
        part!(url_get_h, url_set_h, "port", Part::Port),
        part!(url_get_h, url_set_h, "pathname", Part::Pathname),
        part!(url_get_h, url_set_h, "search", Part::Search),
        part!(url_get_h, url_set_h, "hash", Part::Hash),
        part!(url_get_h, url_set_h, "username", Part::Username),
        part!(url_get_h, url_set_h, "password", Part::Password),
        Row::get(1, "origin", pr::url_origin, None),
        Row::get(1, "searchParams", pr::url_search_params, None),
        Row::method(1, "toString", 0, pr::url_to_string),
        Row::method(1, "toJSON", 0, pr::url_to_string),
    ]);
    let targets = [d.obj_local(s, url)];
    d.install_rows(s, &targets, &rows1::T::<E>::V)?;

    // URLSearchParams
    table!(rows_params = [
        Row::method(1, "append", 2, pr::p_append),
        Row::method(1, "delete", 1, pr::p_delete),
        Row::method(1, "get", 1, pr::p_get),
        Row::method(1, "getAll", 1, pr::p_get_all),
        Row::method(1, "has", 1, pr::p_has),
        Row::method(1, "set", 2, pr::p_set),
        Row::method(1, "sort", 0, pr::p_sort),
        Row::method(1, "toString", 0, pr::p_to_string),
        Row::method(1, "forEach", 1, pr::p_for_each),
        Row::method(1, "keys", 0, pr::p_keys),
        Row::method(1, "values", 0, pr::p_values),
        Row::method(1, "entries", 0, pr::p_entries),
    ]);
    let po = d.obj_local(s, params);
    d.install_rows(s, &[po], &rows_params::T::<E>::V)?;
    // `[Symbol.iterator]` is the same function object as `entries`. The key is the realm's own symbol.
    let entries = s.get_str(po, "entries")?;
    let sym = s.intrinsic(Intrinsic::SymbolIterator);
    s.define(po, sym, entries, Attrs::METHOD)?;
    // The iterator objects inherit from %IteratorPrototype%, so `iterator[Symbol.iterator]() === iterator`.
    let (io, ip) = (d.obj_local(s, iter), s.intrinsic(Intrinsic::IteratorPrototype));
    s.set_prototype_of(io, Some(ip))?;
    table!(rows2 = [
        Row::get(1, "size", pr::p_size, None),
        Row::method(2, "next", 0, pr::iter_next),
    ]);
    let targets = [d.obj_local(s, params), d.obj_local(s, iter)];
    d.install_rows(s, &targets, &rows2::T::<E>::V)?;

    // Location
    table!(rows3 = [
        part!(loc_get, loc_set, "href", Part::Href),
        part!(loc_get, loc_set, "protocol", Part::Protocol),
        part!(loc_get, loc_set, "host", Part::Host),
        part!(loc_get, loc_set, "hostname", Part::Hostname),
        part!(loc_get, loc_set, "port", Part::Port),
        part!(loc_get, loc_set, "pathname", Part::Pathname),
        part!(loc_get, loc_set, "search", Part::Search),
        part!(loc_get, loc_set, "hash", Part::Hash),
        Row::get(1, "origin", |d, s, t, a| if loc_check(d, s, t)? { d.js_str_s(s, "null") } else { win_origin(d, s, t, a) }, None),
        Row::method(1, "assign", 1, |d, s, t, a| loc_go(d, s, t, a, false)),
        Row::method(1, "replace", 1, |d, s, t, a| loc_go(d, s, t, a, true)),
        Row::method(1, "reload", 0, loc_reload),
        Row::method(1, "toString", 0, |d, s, t, _| loc_get(d, s, t, Part::Href)),
    ]);
    let targets = [d.obj_local(s, loc_proto)];
    d.install_rows(s, &targets, &rows3::T::<E>::V)?;

    // History
    table!(rows4 = [
        Row::method(1, "pushState", 2, |d, s, t, a| push_replace(d, s, t, a, false)),
        Row::method(1, "replaceState", 2, |d, s, t, a| push_replace(d, s, t, a, true)),
        Row::method(1, "go", 0, hist_go),
        Row::method(1, "back", 0, hist_back),
        Row::method(1, "forward", 0, hist_forward),
        Row::get(1, "length", hist_length, None),
        Row::get(1, "state", hist_state, None),
        Row::get(1, "scrollRestoration", hist_scroll, Some(hist_set_scroll)),
    ]);
    let targets = [d.obj_local(s, hist_proto)];
    d.install_rows(s, &targets, &rows4::T::<E>::V)?;

    // Storage
    // The order of Chrome (a `for in` over a `Storage` lists them so).
    // WebIDL operations are enumerable (a `for in` over a `Storage` lists them).
    table!(rows_storage = [
        Row::get(1, "length", sto::length, None),
        Row::method(1, "clear", 0, sto::clear).enumerable(),
        Row::method(1, "getItem", 1, sto::get_item).enumerable(),
        Row::method(1, "key", 1, sto::key).enumerable(),
        Row::method(1, "removeItem", 1, sto::remove_item).enumerable(),
        Row::method(1, "setItem", 2, sto::set_item).enumerable(),
    ]);
    let sp = d.obj_local(s, storage_proto);
    d.install_rows(s, &[sp], &rows_storage::T::<E>::V)?;

    // document and window
    let p = d.p();
    table!(rows5 = [
        Row::get(1, "URL", doc_url, None),
        Row::get(1, "documentURI", doc_url, None),
        Row::get(1, "cookie", crate::browser_cookie::doc_cookie_get, Some(crate::browser_cookie::doc_cookie_set)),
        Row::get(1, "domain", doc_domain, Some(doc_set_domain)),
        Row::get(2, "baseURI", doc_url, None),
        Row::get(1, "location", loc_obj, Some(win_set_location)),
        Row::get(4, "location", loc_obj, Some(win_set_location)),
        Row::get(4, "origin", win_origin, None),
    ]);
    let targets = [d.obj_local(s, p.document), d.obj_local(s, p.node), g];
    d.install_rows(s, &targets, &rows5::T::<E>::V)?;

    // constructors and globals
    let mut fixed = vec![loc_proto, hist_proto];
    let ifaces: [(&str, E::Obj, HS<E>, HostFn); 5] = [
        ("URL", url, pr::url_ctor, HostFn::CtorNewTarget),
        ("URLSearchParams", params, pr::params_ctor, HostFn::CtorNewTarget),
        ("Location", loc_proto, illegal, HostFn::Method),
        ("History", hist_proto, illegal, HostFn::Method),
        ("Storage", storage_proto, illegal, HostFn::Ctor),
    ];
    for (name, proto, h, kind) in ifaces {
        let len = match name {
            "URL" | "Location" | "History" => 1,
            _ => 0,
        };
        let f = d.add_fn_s(s, h, name, len, kind)?;
        fixed.push(d.link_s(s, f, proto, name)?);
        if name == "URL" {
            table!(rows6 = [
                Row::method(1, "canParse", 1, pr::url_can_parse),
                Row::method(1, "createObjectURL", 1, crate::browser_web::create_object_url),
                Row::method(1, "revokeObjectURL", 1, crate::browser_web::revoke_object_url),
            ]);
            let targets = [f];
            d.install_rows(s, &targets, &rows6::T::<E>::V)?;
        }
    }
    // `Storage.prototype[Symbol.toStringTag]` is "Storage".
    d.tag_s(s, storage_proto, "Storage")?;
    let ho = d.obj_local(s, history);
    s.define_str(g, "history", ho, Attrs::ALL)?;
    // The two areas are host named objects (`localStorage.x` is `getItem('x')`). Each is made on its first read: a page that reads
    // neither keeps `Heap::has_exotics` false, and every property write skips the exotic checks.
    table!(rows7 = [
        Row::get(1, "localStorage", |d, s, _, _| sto::area_get(d, s, false), None),
        Row::get(1, "sessionStorage", |d, s, _, _| sto::area_get(d, s, true), None),
    ]);
    let targets = [g];
    d.install_rows(s, &targets, &rows7::T::<E>::V)?;
    d.fixed.borrow_mut().extend(fixed);
    Ok(())
}

fn url_get_h<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, part: Part) -> Res<'s, Local<'s>> {
    pr::url_get(d, s, t, part)
}

fn url_set_h<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, t: Local<'s>, a: Args<'s>, part: Part) -> Res<'s, Local<'s>> {
    pr::url_set(d, s, t, a, part)
}

fn illegal<'s, E: Eng>(_: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Err(s.type_error("Illegal constructor"))
}

// ------------------------------------------------------------------------------------------ Browser API

impl<E: Eng> Browser<E> {
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
    pub fn navigate_same_document(&mut self, url: &str) -> R<E, bool> {
        let dom = self.dom.clone();
        let cur = dom.cur_rec()?;
        let Ok(new) = nu::parse(url, Some(&cur)) else { return Ok(false) };
        if new.u != cur.u || new.frag.is_none() {
            return Ok(false);
        }
        self.it.host_scope_o(|it| dom.navigate(it, new, false))?;
        Ok(true)
    }

    /// Raised each time the document URL changes (script, traversal, fragment navigation, `set_document_url`).
    pub fn url_generation(&self) -> u64 {
        self.dom.nav.s.borrow().generation
    }

    /// Run the queued traversal and fragment tasks that were queued when this call started: `popstate`, `hashchange`.
    /// Each task shares its step budget with a microtask checkpoint. A failed checkpoint, `Limit`, `Unsupported`
    /// or fault halts this pump permanently. The remaining tasks stay queued. Returns the tasks that ran.
    pub fn run_navigation_tasks(&mut self) -> R<E, usize> {
        self.run_navigation_tasks_up_to(usize::MAX)
    }

    /// Run at most `max` of the tasks queued at entry. Preserve the pump's checkpoint and failure rules.
    pub fn run_navigation_tasks_up_to(&mut self, max: usize) -> R<E, usize> {
        let dom = self.dom.clone();
        if let Some(e) = dom.nav.s.borrow().halted { return Err(e); }
        let result = self.run_navigation_tasks_inner(max);
        if let Err(e) = result { dom.nav.s.borrow_mut().halted = Some(e); }
        result
    }

    fn run_navigation_tasks_inner(&mut self, max: usize) -> R<E, usize> {
        let dom = self.dom.clone();
        // An empty queue can still hold a sticky failed-job error.
        self.it.host_scope_o(|it| it.host_checkpoint().map(|_| ()))?;
        let n = dom.nav.s.borrow().tasks.len().min(max);
        let mut done = 0;
        for _ in 0..n {
            let t = dom.nav.s.borrow_mut().tasks.pop_front();
            let Some(t) = t else { break };
            done += 1;
            dom.st.borrow_mut().driver = true;
            let r = self.it.host_scope_o(|it| {
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
    pub fn history_go(&mut self, delta: i32) -> R<E, ()> {
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
