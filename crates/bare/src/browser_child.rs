//! Child pages: an iframe with an http(s) `src` loads and runs as its own page (step 3a of `docs/design-gmail-frames.md`, rule D1).
//!
//! One child page = one `Browser`: its own heap, DOM, timers and `fetch` queue, on the same thread as its parent. The parent holds it,
//! keyed by the iframe node. There is no object that both pages share. `RemoteWindow` (`browser_remote_window.rs`) is the only link a
//! script sees. Messages between pages work since step 3b ("Messages" below). A `MessagePort` crosses pages since step 3c ("Ports" below).
//!
//! Which frames. An iframe that is connected and has a `src` that resolves to an http(s) URL other than `about:blank` gets a child page at
//! once (rule "eager, after parent work"). It does not matter if the frame is hidden, 0x0, or of the same origin. A frame with no `src`, an
//! empty one, `about:blank` or `srcdoc` is not a child page: it keeps the blank window that shares the parent realm (`browser_frames.rs`).
//! Known deviation: the `contentDocument` of a same-origin child page is `null` (a browser gives the document). Gmail's same-origin frames
//! do not read their parent directly (measured), so the owner chose one rule for both origins.
//!
//! Layers. The `Dom` of the parent only keeps what a script can ask: `Kids::slots`, one slot for each iframe element (its URL, a
//! "load again" mark, its `RemoteWindow`). A slot is made at once, so `contentWindow` works in the task that inserted the iframe. The driver
//! (`Browser::kids_turn`, called by the round loop of `run_page`) reads the slots and keeps the `Child` states in the same order. A
//! slot index is the `FrameId` of the child. An index is not used again while a later slot is live. The `Dom` never runs a child.
//!
//! Load. The document download starts on a worker as soon as the driver sees the slot (the parent's scripts are not delayed). It uses the
//! rules of a top-level load (`browser_session::load_document`: redirects, http(s), status 2xx, `text/html`, size cap) with one
//! difference: the cookie context is `Ctx::Embedded`, with the site of the top page. That is how a browser sends cookies for the
//! navigation of a frame: a cross-site frame gets only `SameSite=None` cookies, and its reply sets only those. `Sec-Fetch-Dest` is
//! `iframe` and no `Sec-Fetch-User` is sent. A reply that is a download is saved as a download and the frame stays empty (the old catch
//! of `Dom::frame_src`). The cookie jar, the connections and the storage are those of the parent.
//! The child's `Browser` is made when its document is in. A failed load leaves the frame empty and fires `load` at the iframe element
//! (a browser does it for an error page). The failure is a diagnostic of the parent page, never fatal.
//!
//! Run. The tasks of a child run only when the parent has no task of its own in a round (`Run::kids_round`): its scripts, timers, events and
//! network tasks. The first turn of a child is `run_page` (all its blocking scripts), a later turn is one round of `resume_page` (poll
//! mode: `Kids::poll`, the round returns after one pass and never waits). Children run in slot order, a child's own children inside
//! its turn. So the order is fixed: the iframe elements in the order that the driver met them (document order for the frames of the
//! parsed page, then the order of insertion), depth first.
//! A turn of a child that waits for a network reply blocks the parent in this turn (the same as the parent does when it is not live).
//!
//! Messages. `postMessage` on a `RemoteWindow` clones the message in the sender and puts it in the outbox of the sender's `Dom`
//! (`Kids::outbox`: target side, target origin, sender origin, a heap-free `State`, the transfer list as data). The driver moves it, so
//! no object crosses a heap. Before a child's turn, the parent moves its messages for that child into it. After the turn, the parent
//! drains the outbox of the child: a message for `Parent` is delivered in this page, with the window of the child as `source`. A message
//! for `Top` is delivered here when this page is the top page, with `source` null (the top page has no window for a grandchild), else it
//! goes up one level in the outbox of this page. Delivery checks the target origin against the origin of the target document ("*"
//! matches all, a mismatch drops the message), builds the clone in the target heap and queues one `message` event as `post_window` does,
//! charged `16 + bytes`. `source` is the window of the sender in the target heap, and it is a GC root while the message is queued. A
//! message for a child that loads or is empty is dropped. Each moved message counts as `Turn::work`, so the parent does not settle with
//! a message in flight. A `MessagePort` in the transfer list crosses as an end of the port hub ("Ports").
//!
//! Ports. All the pages of one top page share one `PortHub` (`browser_port_hub.rs`, `Cfg::hub`). It holds data only. When a port moves to
//! another page (`Dom::export_ports`, called by `postMessage` of a window or of a split port), the pair is split: both ports get an end id
//! in the hub, and a message for a port goes to the queue of its end as a wire (a heap-free `State`). The driver of a page takes the wires
//! of its ends in `kids_turn`: its own at the start, a child's before the turn of that child (`Dom::take_wires`). So a message from a
//! grandchild to the top page needs no routing. A port that arrives (`Dom::import_ends`) starts disabled. A page that ends removes its ends
//! (`Drop for Kids`), and a message that is dropped removes the ends that it carried. A page is not settled while a wire waits
//! (`Kids::blocks`). A page that never moves a port makes no end and does one integer check for each turn.
//!
//! Load events. The iframe element gets `load` in the parent when the child's own `load` event has run (or when its load failed). The
//! `load` of the parent window waits for the children that exist at that time (`Kids::loaded`). A frame added after the parent's `load`
//! does not hold it.
//!
//! Settle and clock. The parent is settled only when its children are: `Browser::blocks_settle`, `next_scheduled_due` and
//! `live_work_of` include them. A child that is still loading blocks. One clock: in the virtual clock the driver moves the
//! clocks of all pages together (`Run::rounds`, the idle step), and a child's clock is the parent's minus a fixed `delta`. In real time
//! each page reads its own monotonic clock, and `delta` is the difference at the start of the child. `Browser::advance_to` called by
//! hand moves the clock of that page only.
//!
//! Bounds (`BrowserLimits::max_child_pages` and `max_child_depth`): the child pages that are live in one top page, all depths, and the
//! depth of the nesting. An iframe over a bound stays empty, and the console gets one warning. A budget of tasks and rounds applies to each
//! child, and its tasks also count in the budget of the page that holds it.
//!
//! Diagnostics. The console records and the listener reports of a child, and the lines of its `PageResult`, go to the parent's console, reports
//! and result after each turn, with the URL of the child (`scheme://host:port/path`) as the source. A child's fatal stop (`Unsupported`,
//! a limit) or navigation failure ends only that child: it becomes empty, and the parent gets a diagnostic.
//!
//! Removal. A sweep before each turn drops the child of an iframe that left the document. A navigation of the parent drops the `Browser`
//! of the parent and with it all its children. `src` or `location` change on the frame: the old child is dropped before the new
//! document is requested.
use crate::eng::{Eng, Ops, Val};
use crate::browser::{Browser, BrowserLimits, Dom};
use crate::browser_console::ConsoleLevel;
use crate::browser_fetch::FetchLimits;
use crate::browser_loader::{PageConfig, PageLimits, Stop, deadline_after, label};
use crate::browser::R;
use crate::browser_remote_window::{Common, Side, Win};
use crate::browser_scoped::run;
use bare_js_api::Engine as _;
use crate::browser_port_hub::PortHub;
use crate::browser_scheduler::Sender;
use crate::browser_session::{SessionLimits, load_document};
use crate::downloads::{Downloads, Grab};
use bare_html::dom::{Document, NodeId};
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_net::http::Client;
use bare_net::parallel::Task;
use bare_url::Url;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// The index of a child page in its parent.
pub(crate) type FrameId = u32;

/// What a top page needs for its children that `run_page` does not know: the session limits, the evaluator and the heap limits, the
/// download list. `Session` sets it before `run_page` (`Browser::set_child_env`); a caller of `run_page` alone gets the defaults.
#[derive(Clone)]
pub struct KidEnv {
    pub eval: EvalLimits,
    pub heap: HeapLimits,
    pub session: SessionLimits,
    pub downloads: Option<Downloads>,
}

impl Default for KidEnv {
    fn default() -> Self {
        KidEnv { eval: EvalLimits::session(), heap: HeapLimits::default(), session: SessionLimits::default(), downloads: None }
    }
}

/// What a page needs to start its children.
pub(crate) struct Cfg {
    env: KidEnv,
    client: Client,
    page: PageLimits,
    fetch: FetchLimits,
    browser: BrowserLimits,
    /// Child pages that are live in the top page (all depths).
    total: Rc<Cell<usize>>,
    /// 0 for the top page.
    depth: u8,
    /// The site of the top page: the site for the cookies of a frame load.
    top_site: Option<String>,
    /// The messages of the ports that are split across the pages of this top page (`browser_port_hub.rs`).
    hub: Rc<PortHub>,
}

struct Slot<E: Eng> {
    node: NodeId,
    url: String,
    /// The document must be requested (new slot, or `src` or `location` changed).
    dirty: bool,
    /// The `RemoteWindow` of this child, once a script read it.
    win: Option<E::Obj>,
}

type Fetched = Task<Result<(String, String, u16), String>>;

enum State<E: Eng> {
    /// The document is on its way.
    Loading(Fetched),
    Run(Box<Browser<E>>),
    /// The load failed, the child stopped, or the download was saved. Nothing runs.
    Empty,
}

struct Child<E: Eng> {
    state: State<E>,
    /// `run_page` ran.
    started: bool,
    /// The iframe element has got its `load`.
    notified: bool,
    /// The page clock minus the child clock.
    delta: f64,
    /// For the diagnostics: `scheme://host:port/path` of the child, or the requested URL while it loads.
    label: String,
    site: Option<String>,
}

/// A message that a script of this page posted to a window of another page, until the driver moves it (`Kids::outbox`).
pub(crate) struct Out {
    pub to: Side,
    /// "*", or the origin that the target document must have ("/" is the origin of this page).
    pub target_origin: String,
    /// The origin of this page, for the `origin` of the event.
    pub origin: String,
    pub cloned: bare_js_api::Cloned,
}

/// The children of one page: the slots (what a script sees), the states (what the driver runs), and the windows.
#[derive(Default)]
pub(crate) struct Kids<E: Eng> {
    cfg: RefCell<Option<Rc<Cfg>>>,
    env: RefCell<Option<KidEnv>>,
    slots: RefCell<Vec<Option<Slot<E>>>>,
    kids: RefCell<Vec<Option<Child<E>>>>,
    /// The remote windows by target. Each entry holds the handles of its objects: they are roots exactly while the entry lives.
    wins: RefCell<HashMap<<E as Eng>::Obj, (Win<E>, Vec<bare_js_api::Persistent>)>>,
    /// The handles of the shared host functions of `Common`. They live as long as the page.
    common_held: RefCell<Vec<bare_js_api::Persistent>>,
    /// Messages to other pages, in post order.
    outbox: RefCell<Vec<Out>>,
    pub(crate) common: Cell<Option<Common<E>>>,
    /// The number of this page in the port hub (0: the top page).
    pub(crate) page: Cell<u32>,
    /// This page is a child page: a round of `run_page` returns after one pass and never waits.
    pub(crate) poll: Cell<bool>,
    /// A child page, virtual clock: the page time that the next idle round moves the clock to.
    pub(crate) poll_to: Cell<Option<f64>>,
}

impl<E: Eng> Drop for Kids<E> {
    fn drop(&mut self) {
        if let Some(cfg) = self.cfg.get_mut() {
            let live = self.slots.get_mut().iter().flatten().count();
            cfg.total.set(cfg.total.get().saturating_sub(live));
            // The page ends: its ends go, and the ports of the messages that never left it.
            cfg.hub.drop_page(self.page.get());
            for m in self.outbox.get_mut().drain(..) {
                cfg.hub.release(m.cloned.moved());
            }
        }
    }
}

fn connected(doc: &Document, n: NodeId) -> bool {
    let mut up = n;
    while let Some(p) = doc.parent(up) {
        up = p;
    }
    up == doc.root()
}

/// The document URL without its fragment, for the initiator of a load.
fn base_url<E: Eng>(d: &Dom<E>) -> String {
    d.cur_rec().map(|r| r.href()).unwrap_or_default().split('#').next().unwrap_or("").to_string()
}

fn start_load(cfg: &Cfg, url: String, from: String) -> Fetched {
    let lim = cfg.env.session.clone();
    let deadline = deadline_after(lim.max_time);
    let grab = cfg.env.downloads.clone().map(|dl| Grab { dl, force: false, name: None, id: None, only: false });
    let mut c = cfg.client.fork();
    c.set_site(cfg.top_site.clone());
    c.spawn(move |c| load_document(c, &url, Some(&from), None, &lim, deadline, grab.as_ref(), false, None, true))
}

impl<E: Eng> Kids<E> {
    /// Does this page have a child page, or a slot for one, or does a wire wait in the port hub? The driver gives such a page a turn.
    #[inline]
    pub(crate) fn any(&self) -> bool {
        !self.slots.borrow().is_empty() || self.hub_busy()
    }

    /// Does a wire wait in the port hub?
    #[inline]
    fn hub_busy(&self) -> bool {
        self.cfg.borrow().as_ref().is_some_and(|c| c.hub.busy())
    }

    pub(crate) fn hub(&self) -> Option<Rc<PortHub>> {
        self.cfg.borrow().as_ref().map(|c| c.hub.clone())
    }

    pub(crate) fn configured(&self) -> bool {
        self.cfg.borrow().is_some()
    }

    fn cfg(&self) -> Option<Rc<Cfg>> {
        self.cfg.borrow().clone()
    }

    pub(crate) fn win_of(&self, target: E::Obj) -> Option<Win<E>> {
        self.wins.borrow().get(&target).map(|e| e.0)
    }

    /// Keep the window `w` with its `target` (and, for a window, its proxy) in persistent handles until `sync` drops the child.
    pub(crate) fn add_win(&self, it: &mut E::Vm, target: E::Obj, w: Win<E>) {
        let held = [Some(target), (!w.location).then_some(w.proxy)].into_iter().flatten().map(|o| it.persist_value(E::obj_raw(o))).collect();
        self.wins.borrow_mut().insert(target, (w, held));
    }

    /// The persistent handles that this page holds for remote windows and their shared functions (a test counts them).
    #[cfg(test)]
    pub(crate) fn handle_count(&self) -> usize {
        self.wins.borrow().values().map(|e| e.1.len()).sum::<usize>() + self.common_held.borrow().len()
    }

    /// Keep the shared host functions of `c` for the life of the page.
    pub(crate) fn set_common(&self, it: &mut E::Vm, c: Common<E>) {
        let objs = [c.handler, c.ctor, c.closed, c.replace, c.href_get, c.href_set].into_iter().chain(c.no_op);
        *self.common_held.borrow_mut() = objs.map(|o| it.persist_value(E::obj_raw(o))).collect();
        self.common.set(Some(c));
    }

    pub(crate) fn send(&self, m: Out) {
        self.outbox.borrow_mut().push(m);
    }

    pub(crate) fn outbox_len(&self) -> usize {
        self.outbox.borrow().len()
    }

    /// Take the messages of the outbox for which `f` is true, in order.
    fn take_out(&self, f: impl Fn(&Out) -> bool) -> Vec<Out> {
        let mut out = self.outbox.borrow_mut();
        let (take, keep) = std::mem::take(&mut *out).into_iter().partition(|m| f(m));
        *out = keep;
        take
    }

    /// The window of `side` in this heap, if a script or the setup made it.
    fn proxy_of(&self, side: Side) -> Option<E::Obj> {
        self.wins.borrow().values().find(|e| e.0.side == side && !e.0.location).map(|e| e.0.proxy)
    }

    /// The target of the window `proxy` (not of its location).
    pub(crate) fn target_of(&self, proxy: E::Obj) -> Option<E::Obj> {
        self.wins.borrow().iter().find(|(_, e)| e.0.proxy == proxy && !e.0.location).map(|(&t, _)| t)
    }

    pub(crate) fn window_of(&self, id: FrameId) -> Option<E::Obj> {
        self.slots.borrow().get(id as usize)?.as_ref()?.win
    }

    pub(crate) fn set_window(&self, id: FrameId, w: E::Obj) {
        if let Some(Some(s)) = self.slots.borrow_mut().get_mut(id as usize) {
            s.win = Some(w);
        }
    }

    /// Is the child `id` still a frame of this page?
    pub(crate) fn alive(&self, id: FrameId) -> bool {
        self.slots.borrow().get(id as usize).is_some_and(|s| s.is_some())
    }

    /// The child page of the iframe element `n`.
    pub(crate) fn id_of(&self, n: NodeId) -> Option<FrameId> {
        self.slots.borrow().iter().position(|s| s.as_ref().is_some_and(|s| s.node == n)).map(|i| i as FrameId)
    }

    /// Make the `Dom` of a top page ready for children. A child page was set up before by its parent, and keeps that.
    pub(crate) fn configure(&self, dom_lim: BrowserLimits, client: Client, page: PageLimits, fetch: FetchLimits, top_site: Option<String>) {
        if self.cfg.borrow().is_some() {
            return;
        }
        let env = self.env.borrow_mut().take().unwrap_or_default();
        *self.cfg.borrow_mut() = Some(Rc::new(Cfg { env, client, page, fetch, browser: dom_lim, total: Rc::new(Cell::new(0)), depth: 0, top_site, hub: Rc::default() }));
    }

    pub(crate) fn set_env(&self, env: KidEnv) {
        *self.env.borrow_mut() = Some(env);
    }

    /// The iframe element `n` has `url` as its document (`None`: no child page: blank, or a `src` that is not http(s)).
    pub(crate) fn request(&self, d: &Dom<E>, it: &mut E::Vm, n: NodeId, url: Option<String>) {
        let Some(cfg) = self.cfg() else { return };
        let mut slots = self.slots.borrow_mut();
        let at = slots.iter().position(|s| s.as_ref().is_some_and(|s| s.node == n));
        match (at, url) {
            (Some(i), Some(url)) => {
                if let Some(s) = slots[i].as_mut() {
                    s.url = url;
                    s.dirty = true;
                }
            }
            (Some(i), None) => {
                slots[i] = None;
                cfg.total.set(cfg.total.get().saturating_sub(1));
            }
            (None, Some(url)) => {
                let lim = cfg.browser;
                if cfg.depth as usize >= lim.max_child_depth || cfg.total.get() >= lim.max_child_pages {
                    crate::browser_console::push_text(d, it, ConsoleLevel::Warn, "An iframe was not loaded: the limit of child pages or of their nesting depth is reached");
                    return;
                }
                cfg.total.set(cfg.total.get() + 1);
                slots.push(Some(Slot { node: n, url, dirty: true, win: None }));
            }
            (None, None) => {}
        }
    }

    /// `location` of the window of child `id` was set.
    pub(crate) fn navigate(&self, d: &Dom<E>, id: FrameId, url: &str) {
        let Some(url) = d.resolve_http(url).filter(|u| !u.starts_with("about:")) else { return };
        if let Some(Some(s)) = self.slots.borrow_mut().get_mut(id as usize) {
            s.url = url;
            s.dirty = true;
        }
    }

    /// The iframe nodes that slots keep: the arena must not reuse them.
    pub(crate) fn held(&self, out: &mut Vec<NodeId>) {
        out.extend(self.slots.borrow().iter().flatten().map(|s| s.node));
    }

    /// Drop the children of iframes that left the document, and request the documents that are marked.
    fn sync(&self, d: &Dom<E>, cfg: &Cfg) {
        let from = base_url(d);
        let mut slots = self.slots.borrow_mut();
        let mut kids = self.kids.borrow_mut();
        kids.resize_with(slots.len(), || None);
        let doc = d.doc.borrow();
        for i in 0..slots.len() {
            if !slots[i].as_ref().is_some_and(|s| connected(&doc, s.node)) {
                if slots[i].take().is_some() {
                    cfg.total.set(cfg.total.get().saturating_sub(1));
                }
                kids[i] = None;
                self.wins.borrow_mut().retain(|_, e| e.0.side != Side::Child(i as FrameId));
                continue;
            }
            let Some(s) = slots[i].as_mut().filter(|s| s.dirty) else { continue };
            s.dirty = false;
            // The old child ends first, so a heap is never held twice.
            kids[i] = None;
            let site = Url::parse(s.url.split('#').next().unwrap_or("")).ok().map(|u| bare_net::cookie::site_of(&u));
            let label = Url::parse(s.url.split('#').next().unwrap_or("")).map_or_else(|_| "?".to_string(), |u| label(&u));
            kids[i] = Some(Child { state: State::Loading(start_load(cfg, s.url.clone(), from.clone())), started: false, notified: false, delta: 0.0, label, site });
        }
        while matches!(slots.last(), Some(None)) {
            slots.pop();
            kids.pop();
        }
    }

    /// Have all the children that are frames of this page now got their `load`?
    pub(crate) fn loaded(&self, d: &Dom<E>) -> bool {
        let slots = self.slots.borrow();
        if slots.is_empty() {
            return true;
        }
        let (kids, doc) = (self.kids.borrow(), d.doc.borrow());
        slots.iter().enumerate().all(|(i, s)| match s {
            Some(s) if connected(&doc, s.node) => kids.get(i).and_then(|k| k.as_ref()).is_some_and(|k| k.notified),
            _ => true,
        })
    }

    /// Does a child keep the page from settling (a queued task of it, due by `limit` on the page clock, that blocks; or a load)?
    pub(crate) fn blocks(&self, d: &Dom<E>, limit: f64) -> bool {
        // A wire in the hub waits for a page to take it.
        if self.hub_busy() {
            return true;
        }
        let slots = self.slots.borrow();
        if slots.is_empty() {
            return false;
        }
        let (kids, doc) = (self.kids.borrow(), d.doc.borrow());
        kids.iter().enumerate().any(|(i, k)| {
            let live = slots.get(i).and_then(|s| s.as_ref()).is_some_and(|s| connected(&doc, s.node));
            live && match k.as_ref().map(|k| (&k.state, k.delta)) {
                Some((State::Loading(_), _)) => true,
                Some((State::Run(b), delta)) => b.blocks_settle(limit - delta),
                _ => false,
            }
        })
    }

    /// Does a live child still load its document?
    pub(crate) fn loading(&self, d: &Dom<E>) -> bool {
        let slots = self.slots.borrow();
        let (kids, doc) = (self.kids.borrow(), d.doc.borrow());
        kids.iter().enumerate().any(|(i, k)| {
            matches!(k.as_ref().map(|k| &k.state), Some(State::Loading(_))) && slots.get(i).and_then(|s| s.as_ref()).is_some_and(|s| connected(&doc, s.node))
        })
    }

    /// The earliest due time of a task of a child, on the page clock.
    pub(crate) fn next_due(&self) -> Option<f64> {
        self.kids.borrow().iter().flatten().filter_map(|k| match &k.state {
            State::Run(b) => b.next_scheduled_due().map(|t| t + k.delta),
            _ => None,
        }).min_by(|a, b| a.total_cmp(b))
    }

    /// Live: can a turn do something for a child now (`Some(true)`), or does a child wait for the network (`Some(false)`)?
    pub(crate) fn live_work(&self, streams: bool) -> Option<bool> {
        let mut out = None;
        for k in self.kids.borrow().iter().flatten() {
            let w = match &k.state {
                State::Loading(t) => Some(t.done()),
                State::Run(b) => b.live_work_of(streams),
                State::Empty => None,
            };
            match w {
                Some(true) => return Some(true),
                Some(false) => out = Some(false),
                None => {}
            }
        }
        out
    }
}

/// What one turn of the children did.
#[derive(Default)]
pub(crate) struct Turn {
    /// Steps that changed a page: a document came in, a task ran, a page went on.
    pub work: usize,
    /// Tasks that ran (they count in the budget of the page).
    pub tasks: usize,
    /// The iframe elements that must get `load`.
    pub loaded: Vec<NodeId>,
    pub diags: Vec<String>,
}

impl<E: Eng> Browser<E> {
    /// The environment that this page gives its children. Call before `run_page`.
    pub fn set_child_env(&mut self, env: KidEnv) {
        self.dom.kids.set_env(env);
    }

    /// Make the child `Browser` for a document that came in.
    fn child_browser(&self, cfg: &Cfg, node: NodeId, url: &str, html: &str) -> Result<(Browser<E>, f64), String> {
        let doc = tree::parse(html);
        let mut b = Browser::<E>::make(doc, cfg.browser, cfg.env.eval, cfg.env.heap).map_err(|_| "the realm could not be made".to_string())?;
        // The size of the iframe box in the parent when a layout exists, else the size of a default iframe.
        let (w, h) = self.dom.cached_box_size(node).unwrap_or((300.0, 150.0));
        b.set_viewport(w, h).map_err(|_| "the viewport is invalid".to_string())?;
        if self.uses_virtual_clock() {
            b.use_virtual_clock();
        }
        let delta = self.now() - b.now();
        b.attach_storage(self.dom.nav.s.borrow().store.clone());
        b.set_document_url(url).map_err(|_| "the document URL could not be configured".to_string())?;
        if cfg.env.downloads.is_some() {
            b.enable_downloads();
        }
        b.start_refresh();
        let depth = cfg.depth + 1;
        let kid = Cfg { env: cfg.env.clone(), client: cfg.client.fork(), page: cfg.page.clone(), fetch: cfg.fetch, browser: cfg.browser, total: cfg.total.clone(), depth, top_site: cfg.top_site.clone(), hub: cfg.hub.clone() };
        *b.dom.kids.cfg.borrow_mut() = Some(Rc::new(kid));
        b.dom.kids.page.set(cfg.hub.new_page());
        b.dom.kids.poll.set(true);
        let dom = b.dom.clone();
        b.it.host_scope_o(|it| crate::browser_remote_window::install_up(&dom, it, depth)).map_err(|_| "the window could not be set up".to_string())?;
        Ok((b, delta))
    }

    /// One turn for each child page, in slot order. The page calls it when it has no task of its own to run. `advance`: the virtual
    /// clock of the children moves to the clock of this page first. `wait`: a child that loads is waited for (this page has nothing else
    /// to do), else only when the page is not live.
    pub(crate) fn kids_turn(&mut self, advance: bool, wait: bool) -> Turn {
        let mut t = Turn::default();
        let Some(cfg) = self.dom.kids.cfg() else { return t };
        self.dom.kids.sync(&self.dom, &cfg);
        let n = self.dom.kids.kids.borrow().len();
        let block = wait || !self.live();
        // The wires for the ports of this page (a page that has no split port has no end: the call does nothing).
        if cfg.hub.busy() {
            match self.it.host_scope_o(|it| run::<E, _>(it, |s| self.dom.take_wires(s))) {
                Ok(k) => t.work += k,
                Err(e) => t.diags.push(format!("a message was dropped: {e:?}")),
            }
        }
        for i in 0..n {
            let Some(mut ch) = self.dom.kids.kids.borrow_mut().get_mut(i).and_then(|k| k.take()) else { continue };
            let Some(node) = self.dom.kids.slots.borrow().get(i).and_then(|s| s.as_ref()).map(|s| s.node) else { continue };
            let mut down = if self.dom.kids.outbox_len() > 0 { self.dom.kids.take_out(|m| m.to == Side::Child(i as FrameId)) } else { Vec::new() };
            let fail = |ch: &mut Child<E>, t: &mut Turn, why: String| {
                t.diags.push(format!("frame {}: {why}", ch.label));
                if !ch.notified {
                    ch.notified = true;
                    t.loaded.push(node);
                }
                State::Empty
            };
            ch.state = match std::mem::replace(&mut ch.state, State::Empty) {
                State::Loading(task) if !block && !task.done() => State::Loading(task),
                State::Loading(task) => {
                    t.work += 1;
                    match task.wait().unwrap_or_else(|| Err("network error".into())) {
                        Ok((url, html, _)) => match self.child_browser(&cfg, node, &url, &html) {
                            Ok((b, delta)) => {
                                ch.delta = delta;
                                ch.site = Url::parse(url.split('#').next().unwrap_or("")).ok().map(|u| bare_net::cookie::site_of(&u));
                                ch.label = Url::parse(url.split('#').next().unwrap_or("")).map_or_else(|_| ch.label.clone(), |u| label(&u));
                                ch.started = false;
                                State::Run(Box::new(b))
                            }
                            Err(why) => fail(&mut ch, &mut t, why),
                        },
                        Err(why) => fail(&mut ch, &mut t, why),
                    }
                }
                State::Run(mut b) => {
                    let parent = b.dom.kids.proxy_of(Side::Parent);
                    if cfg.hub.busy() {
                        match b.it.host_scope_o(|it| run::<E, _>(it, |s| b.dom.take_wires(s))) {
                            Ok(k) => t.work += k,
                            Err(e) => t.diags.push(format!("frame {}: a message was dropped: {e:?}", ch.label)),
                        }
                    }
                    for m in std::mem::take(&mut down) {
                        t.work += 1;
                        if let Err(e) = b.it.host_scope_o(|it| b.dom.deliver(it, m, parent)) {
                            t.diags.push(format!("frame {}: a message was dropped: {e:?}", ch.label));
                        }
                    }
                    let url = b.document_url().unwrap_or_default();
                    let ready = b.ready_level();
                    if advance && self.uses_virtual_clock() {
                        // The due time of the child, moved to the page clock and back, can differ by one ulp. A task that this page
                        // sees as due must be due for the child too, or the page waits for it for ever.
                        let (now, mut to) = (self.now(), self.now() - ch.delta);
                        if let Some(due) = b.next_scheduled_due().filter(|due| due + ch.delta <= now) {
                            to = to.max(due);
                        }
                        b.dom.kids.poll_to.set(Some(to));
                    }
                    let r = if ch.started {
                        let mut c = cfg.client.fork();
                        c.set_site(ch.site.clone());
                        b.resume_page(c, cfg.page.clone(), &[])
                    } else {
                        ch.started = true;
                        let (c, f) = (cfg.client.fork(), cfg.client.fork());
                        b.run_page(PageConfig { url, client: c, limits: cfg.page.clone(), fetch: Some((f, cfg.fetch)) })
                    };
                    t.tasks += r.tasks;
                    if r.tasks > 0 || r.scripts_run > 0 || b.ready_level() != ready {
                        t.work += 1;
                    }
                    for line in &r.diagnostics {
                        t.diags.push(format!("frame {}: {line}", ch.label));
                    }
                    self.take_child_output(&mut b, &ch.label);
                    if b.dom.kids.outbox_len() > 0 {
                        for m in b.dom.kids.take_out(|m| !matches!(m.to, Side::Child(_))) {
                            t.work += 1;
                            self.route_up(&cfg, i as FrameId, m, &mut t);
                        }
                    }
                    match r.stop {
                        Stop::Settled => {
                            if b.loaded() && !ch.notified {
                                ch.notified = true;
                                t.loaded.push(node);
                            }
                            State::Run(b)
                        }
                        Stop::Navigate(nav) => {
                            // The child asked for a new document: the old page ends first.
                            drop(b);
                            t.work += 1;
                            let url = nav.0.url.clone();
                            let from = nav.0.from.split('#').next().unwrap_or("").to_string();
                            ch.started = false;
                            ch.notified = false;
                            ch.label = Url::parse(url.split('#').next().unwrap_or("")).map_or_else(|_| "?".to_string(), |u| label(&u));
                            State::Loading(start_load(&cfg, url, from))
                        }
                        stop => {
                            t.work += 1;
                            fail(&mut ch, &mut t, format!("stopped: {stop:?}"))
                        }
                    }
                }
                State::Empty => State::Empty,
            };
            // A message for a child that did not run is dropped: its ports never arrive.
            for m in down {
                cfg.hub.release(m.cloned.moved());
            }
            if let Some(slot) = self.dom.kids.kids.borrow_mut().get_mut(i) {
                *slot = Some(ch);
            }
        }
        // Each live child took its messages above. What is left is for a frame that was removed: drop it.
        if self.dom.kids.outbox_len() > 0 {
            for m in self.dom.kids.take_out(|m| matches!(m.to, Side::Child(_))) {
                cfg.hub.release(m.cloned.moved());
            }
        }
        t
    }

    /// A message that the child `i` posted to its parent or to the top page. A message for the top page goes up one level in each turn.
    fn route_up(&mut self, cfg: &Cfg, i: FrameId, m: Out, t: &mut Turn) {
        if m.to == Side::Top && cfg.depth > 0 {
            return self.dom.kids.send(m);
        }
        // The top page sees only its own children as windows, so a deeper sender has no `source`.
        let from_child = m.to == Side::Parent;
        let r = self.it.host_scope_o(|it| {
            let source = if from_child {
                match E::kind(self.dom.child_window(it, i)?) {
                    Val::Object(o) => Some(o),
                    _ => None,
                }
            } else {
                None
            };
            self.dom.deliver(it, m, source)
        });
        if let Err(e) = r {
            t.diags.push(format!("a message was dropped: {e:?}"));
        }
    }

    /// Move the console records and the reports of a child into this page, with the label of the child as their source.
    fn take_child_output(&mut self, child: &mut Browser<E>, label: &str) {
        for rec in child.take_console_records() {
            let text = child.format_console_record(&rec).unwrap_or_default();
            crate::browser_console::push_text(&self.dom, &mut self.it, rec.level, &format!("{label}: {text}"));
        }
        for mut r in child.take_reports() {
            r.message = format!("{label}: {}", r.message);
            let mut st = self.dom.st.borrow_mut();
            if st.reports.len() >= self.dom.lim.max_reports {
                st.reports.pop_front();
                st.dropped_reports += 1;
            }
            st.reports.push_back(r);
        }
    }
}

impl<E: Eng> Dom<E> {
    /// A message from another page reaches this page: drop it if the target origin does not match, else queue a `message` event for the
    /// window. `source` is the window of the sender in this heap.
    fn deliver(&self, it: &mut E::Vm, mut m: Out, source: Option<E::Obj>) -> R<E, ()> {
        if m.target_origin != "*" && m.target_origin != self.window_origin() {
            if let Some(h) = self.kids.hub() {
                h.release(m.cloned.moved());
            }
            return Ok(());
        }
        let bytes = 16usize.saturating_add(m.cloned.cost());
        let (data, ports) = run::<E, _>(it, |s| {
            let (data, ports) = crate::browser_navigation_state::materialize_with(self, s, &mut m.cloned)?;
            Ok((s.engine().raw(data), ports))
        })?;
        self.queue_window(data, ports.into_boxed_slice(), bytes, Some(Box::new(Sender { origin: m.origin, source })))
    }

    /// The size of the layout box of `n` in the last layout of this page, if there is one. It never makes a layout.
    pub(crate) fn cached_box_size(&self, n: NodeId) -> Option<(f32, f32)> {
        let shared = self.geometry.shared.borrow();
        let (_, lay) = shared.as_ref()?;
        let r = crate::browser_geometry::border_box(&lay.layout, n)?;
        (r.w > 0.0 && r.h > 0.0).then_some((r.w, r.h))
    }
}
