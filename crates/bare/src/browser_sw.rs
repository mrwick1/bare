//! Service workers, the engine side (design S2 of `docs/internal/design-service-workers.md`). The session data is in `sw_hub.rs`;
//! the objects of a realm are in `shims/sw.js`.
//!
//! - A page: the shim is made at the first read of `navigator.serviceWorker` or of `ServiceWorkerContainer`,
//!   `ServiceWorkerRegistration` or `ServiceWorker`. Then the page is a client of the hub. A page that a registration controls at its
//!   load is a client from the start (`navigated`). A page that is neither pays nothing.
//! - The driver turn of a client page (`Browser::sw_turn`, called by `kids_turn`): poll the script fetches of its jobs, give each
//!   change of its inbox to the shim (`sync`, one call and one microtask checkpoint each), and start the workers that have events and
//!   no host. A worker is a worker page (`Of::Service` in `browser_child.rs`) that runs the script text of its version.
//! - A worker page: the shim runs at its start (worker side). Before each turn the host gives it its events (`dispatch_events`).
//!   The shim reports the end of each event (`waitUntil` settled) with `done`; the host moves it to the hub after the turn
//!   (`take_done`), on its own clock. After `IDLE_MS` with no event in flight the host ends the worker page.
//! - The register job: the checks of Chrome on the URLs (here), the script fetch (`fetch_script`: no redirect, no HTTP cache, a
//!   `Service-Worker: script` header), then the hub. A redirect is a `SecurityError`, as in Chrome.
//! - Gates: no `fetch` event, no `navigationPreload` (a manager that never preloads), `startMessages()` does nothing (messages are
//!   delivered at once), a worker runs on the page thread also for a cross-origin isolated page, and a version runs in the page that
//!   first had work for it (another tab of the session does not share a running worker). `importScripts` loads through the HTTP cache
//!   at each start of a worker.

use crate::browser::*;
use crate::browser_navigation_url as nu;
use crate::browser_scoped::*;
use crate::eng::{Eng, Ops};
use crate::sw_hub::{EvKind, Hub, RegSnap, Script, Start, ToClient, WSnap};
use bare_js_api::{Args, Engine, HostFn, Local, Persistent, Type};
use bare_net::parallel::Task;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const SRC: &str = include_str!("shims/sw.js");
/// The globals of a page that make the shim at their first read.
pub(crate) const NAMES: [&str; 3] = ["ServiceWorkerContainer", "ServiceWorkerRegistration", "ServiceWorker"];
/// The biggest service worker script.
const MAX_SCRIPT: usize = 16 << 20;

/// The service worker state of one page.
#[derive(Default)]
pub(crate) struct SwPage {
    /// The helper object of the shim, once made.
    helper: RefCell<Option<Persistent>>,
    /// The hub, while this page is a client.
    hub: RefCell<Option<Rc<RefCell<Hub>>>>,
    /// The number of this page in the hub (0: not a client).
    pub(crate) client: Cell<u64>,
    /// A worker page: the id of its version (0: not a worker page).
    pub(crate) version: Cell<u64>,
    /// The script fetches of the jobs of this page.
    jobs: RefCell<Vec<(u64, Task<Option<Script>>)>>,
    /// A worker page: the events that ended, `(event id, ok)`, for the host.
    done: RefCell<Vec<(u64, bool)>>,
}

impl Drop for SwPage {
    fn drop(&mut self) {
        if let Some(h) = self.hub.get_mut() {
            h.borrow_mut().remove_client(self.client.get());
        }
    }
}

fn hub_of<E: Eng>(d: &Dom<E>) -> Rc<RefCell<Hub>> {
    d.nav.s.borrow().store.sw_hub()
}

fn page_url<E: Eng>(d: &Dom<E>) -> String {
    d.cur_rec().map(|r| r.href()).unwrap_or_default().split('#').next().unwrap_or("").to_string()
}

/// Make this page a client of the hub (once). `control`: a registration that matches it now controls it.
fn become_client<E: Eng>(d: &Dom<E>, control: bool) -> u64 {
    if d.sw.client.get() != 0 || d.sw.version.get() != 0 {
        return d.sw.client.get();
    }
    let hub = hub_of(d);
    let uuid = crate::browser_web::uuid_v4().unwrap_or_default();
    let id = hub.borrow_mut().add_client(uuid, &page_url(d), control, d.kids.page.get() != 0);
    d.sw.client.set(id);
    *d.sw.hub.borrow_mut() = Some(hub);
    d.kids.sw_on.set(true);
    id
}

/// A document loaded: when a registration with an active worker matches its URL, the page is a controlled client. One length check
/// when the session has no registration.
pub(crate) fn navigated<E: Eng>(d: &Dom<E>) {
    let hub = hub_of(d);
    let has = hub.borrow().has_regs() && hub.borrow().match_url(&page_url(d)).is_some_and(|r| r.active.is_some());
    if has {
        become_client(d, true);
    }
}

/// Define the globals of `NAMES` at the first read of one of them. Gives whether `name` is one of them.
#[cold]
pub(crate) fn define<E: Eng>(d: &Dom<E>, it: &mut E::Vm, name: &str) -> R<E, bool> {
    if !NAMES.contains(&name) || d.sw.version.get() != 0 {
        return Ok(false);
    }
    helper(d, it)?;
    Ok(true)
}

/// The helper of the shim, made at the first call.
fn helper<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, E::Raw> {
    if let Some(p) = d.sw.helper.borrow().as_ref() {
        return Ok(it.persistent_value(p));
    }
    let worker = d.sw.version.get() != 0;
    let s = it.h_intern(SRC)?;
    let made = it.eval_global_o(E::str_raw(s))?;
    let f = d.add_fn_s_obj(it, sw_host, "sw", 2, HostFn::Method)?;
    let h = it.call_o(made, E::undef(), &[E::obj_raw(f), E::bool_raw(worker)])?;
    *d.sw.helper.borrow_mut() = Some(it.persist_value(h));
    Ok(h)
}

/// `navigator.serviceWorker`: the container of the shim.
pub(crate) fn container<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, Local<'s>> {
    let h = old_v::<E>(s, |it| helper(d, it))?;
    s.get_str(h, "container")
}

/// Set up the global of a worker page for version `ver`: the worker scope (`browser_worker.rs`) and the worker side of the shim.
pub(crate) fn install_worker<E: Eng>(d: &Dom<E>, it: &mut E::Vm, ver: u64) -> R<E, ()> {
    d.sw.version.set(ver);
    *d.sw.hub.borrow_mut() = None;
    crate::browser_worker::install_scope_named(d, it, "ServiceWorkerGlobalScope", false)?;
    helper(d, it).map(|_| ())
}

fn w_snap<'s, E: Eng>(s: &mut Sc<'s, E>, w: &Option<WSnap>) -> Res<'s, Local<'s>> {
    let Some((id, url, st)) = w else { return Ok(s.null()) };
    let items = [s.number(*id as f64), s.string(url)?, s.string(st.name())?];
    s.new_array(&items)
}

fn reg_snap<'s, E: Eng>(s: &mut Sc<'s, E>, r: &RegSnap) -> Res<'s, Local<'s>> {
    let items = [s.number(r.id as f64), s.string(&r.scope)?, s.number(f64::from(r.uvc)), w_snap::<E>(s, &r.w[0])?, w_snap::<E>(s, &r.w[1])?, w_snap::<E>(s, &r.w[2])?];
    s.new_array(&items)
}

fn num<'s, E: Eng>(s: &mut Sc<'s, E>, a: Args<'s>, i: usize) -> f64 {
    let v = arg_s::<E>(s, a, i);
    s.as_number(v).unwrap_or(0.0)
}

/// Resolve `u` against the document. `None` when it does not parse.
fn resolve<E: Eng>(d: &Dom<E>, u: &str) -> Option<String> {
    let cur = d.cur_rec().ok();
    nu::parse_any(u, cur.as_ref()).ok().map(|r| r.href().split('#').next().unwrap_or("").to_string())
}

/// `sw(op, ...)` of `shims/sw.js`. The ops are numbered as `OP` in the shim.
fn sw_host<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let op = num::<E>(s, a, 0) as u8;
    let ver = d.sw.version.get();
    let hub = hub_of(d);
    match op {
        // client() -> the controller snapshot or null
        0 => {
            let id = become_client(d, false);
            let ctrl = hub.borrow().client(id).and_then(|c| c.controller).and_then(|v| hub.borrow().ver(v).map(|v| (v.id, v.url.clone(), v.state)));
            w_snap::<E>(s, &ctrl)
        }
        // register(url, scope, uvc, type) -> the job id
        1 => {
            let url = d.arg_str_s(s, a, 1)?;
            let scope = arg_s::<E>(s, a, 2);
            let scope = if s.type_of(scope) == Type::Undefined { None } else { Some(d.string_arg_s(s, scope)?) };
            let (uvc, module) = (num::<E>(s, a, 3) as u8, num::<E>(s, a, 4) == 1.0);
            register(d, s, &hub, &url, scope.as_deref(), uvc, module)
        }
        // regs(0) -> all the snapshots of the origin; regs(1) -> the id of the registration of the page or 0; regs(2, url) -> one
        2 => {
            let mode = num::<E>(s, a, 1) as u8;
            let origin = crate::browser_basics::doc_origin(d);
            match mode {
                1 => {
                    let id = hub.borrow().match_url(&page_url(d)).map_or(0, |r| r.id);
                    Ok(s.number(id as f64))
                }
                // A session with no registration has nothing to match.
                2 if !hub.borrow().has_regs() => Ok(s.null()),
                2 => {
                    let u = d.arg_str_s(s, a, 2)?;
                    let target = if u.is_empty() { page_url(d) } else { resolve(d, &u).unwrap_or_default() };
                    let o = crate::sw_hub::origin_of(&target);
                    if o != origin {
                        return Err(d.dom_err_s(s, "SecurityError", &format!("Failed to get a ServiceWorkerRegistration: The origin of the provided documentURL ('{o}') does not match the current origin ('{origin}').")));
                    }
                    let snap = hub.borrow().match_url(&target).map(|r| r.snap());
                    match snap {
                        Some(r) => reg_snap::<E>(s, &r),
                        None => Ok(s.null()),
                    }
                }
                _ => {
                    let list = hub.borrow().regs_of(&origin);
                    let mut out = Vec::with_capacity(list.len());
                    for r in &list {
                        out.push(reg_snap::<E>(s, r)?);
                    }
                    s.new_array(&out)
                }
            }
        }
        // unregister(id) -> bool
        3 => {
            let id = num::<E>(s, a, 1) as u64;
            let ok = hub.borrow_mut().unregister(id);
            Ok(s.boolean(ok))
        }
        // post(version, message, transfer): a message to a worker
        4 => {
            let to = num::<E>(s, a, 1) as u64;
            let from = if ver != 0 { 0 } else { become_client(d, false) };
            let cloned = clone_arg(d, s, a, 2, "ServiceWorker")?;
            if !hub.borrow_mut().post(to, from, cloned) {
                return Err(d.dom_err_s(s, "InvalidStateError", "Failed to execute 'postMessage' on 'ServiceWorker': The ServiceWorker is in redundant state."));
            }
            Ok(s.undefined())
        }
        // update(id) -> the job id
        5 => {
            let id = num::<E>(s, a, 1) as u64;
            let client = if ver != 0 { 0 } else { become_client(d, false) };
            let start = hub.borrow_mut().update(client, id);
            match start {
                Some(st) => start_job(d, s, st),
                None => Err(d.dom_err_s(s, "InvalidStateError", "Failed to update a ServiceWorker: The registration has no worker.")),
            }
        }
        // done(event id, ok): the host moves it to the hub after the turn
        10 => {
            let (id, ok) = (num::<E>(s, a, 1) as u64, {
                let v = arg_s::<E>(s, a, 2);
                s.to_boolean(v)
            });
            d.sw.done.borrow_mut().push((id, ok));
            Ok(s.undefined())
        }
        11 => {
            hub.borrow_mut().skip_waiting(ver);
            Ok(s.undefined())
        }
        12 => {
            let ok = hub.borrow_mut().claim(ver);
            Ok(s.boolean(ok))
        }
        // clients(includeUncontrolled, type) -> [[uuid, url, type, frameType, visibilityState, focused]]
        13 => {
            let all = {
                let v = arg_s::<E>(s, a, 1);
                s.to_boolean(v)
            };
            let ty = d.arg_str_s(s, a, 2)?;
            let list: Vec<(String, String, bool, bool)> = if ty == "window" || ty == "all" {
                hub.borrow().clients_of(ver, all).iter().map(|c| (c.uuid.clone(), c.url.clone(), c.nested, c.hidden)).collect()
            } else {
                Vec::new()
            };
            let mut out = Vec::with_capacity(list.len());
            for (uuid, url, nested, hidden) in list {
                let items = [s.string(&uuid)?, s.string(&url)?, s.string("window")?, s.string(if nested { "nested" } else { "top-level" })?, s.string(if hidden { "hidden" } else { "visible" })?, s.boolean(!hidden)];
                out.push(s.new_array(&items)?);
            }
            s.new_array(&out)
        }
        // clientPost(uuid, message, transfer)
        14 => {
            let uuid = d.arg_str_s(s, a, 1)?;
            let cloned = clone_arg(d, s, a, 2, "Client")?;
            if let Err(c) = hub.borrow_mut().post_to_client(ver, &uuid, cloned)
                && let Some(h) = d.kids.hub()
            {
                h.release(c.moved());
            }
            Ok(s.undefined())
        }
        // trust(event)
        16 => {
            if let Some(o) = a.get(1).and_then(|v| obj_id::<E>(s, v))
                && let Some(e) = d.st.borrow_mut().events.get_mut(&o)
            {
                e.trusted = true;
            }
            Ok(s.undefined())
        }
        // focus(uuid) -> bool: needs a user activation (a notification click), which no event gives yet
        17 => Ok(s.boolean(false)),
        _ => Ok(s.undefined()),
    }
}

/// Clone `a[i]` with the transfer list `a[i + 1]` for another page.
fn clone_arg<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>, i: usize, _who: &str) -> Res<'s, bare_js_api::Cloned> {
    let msg = arg_s::<E>(s, a, i);
    let t = arg_s::<E>(s, a, i + 1);
    let list = d.transfer_arg(s, t, None)?;
    let mut cloned = crate::browser_navigation_state::clone_state_with(d, s, msg, &list)?;
    d.export_ports(s, &mut cloned)?;
    Ok(cloned)
}

/// The checks of `register` on the URLs (Chrome's messages), then the job.
fn register<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, hub: &Rc<RefCell<Hub>>, url: &str, scope: Option<&str>, uvc: u8, module: bool) -> Res<'s, Local<'s>> {
    const WHO: &str = "Failed to register a ServiceWorker";
    let origin = crate::browser_basics::doc_origin(d);
    let Some(script) = resolve(d, url) else {
        return Err(s.type_error(&format!("{WHO}: The URL provided ('{url}') is not valid.")));
    };
    let scope = match scope {
        Some(sc) => match resolve(d, sc) {
            Some(u) => u,
            None => return Err(s.type_error(&format!("{WHO}: The scope provided ('{sc}') is not valid."))),
        },
        // The default scope is the directory of the script.
        None => nu::parse_any("./", nu::parse_any(&script, None).ok().as_ref()).map(|r| r.href()).unwrap_or_default(),
    };
    for (what, u) in [("script", &script), ("scope", &scope)] {
        if !(u.starts_with("https://") || u.starts_with("http://")) {
            return Err(s.type_error(&format!("{WHO}: The URL protocol of the {what} ('{u}') is not supported.")));
        }
    }
    for (what, u) in [("scriptURL", &script), ("scope", &scope)] {
        let o = crate::sw_hub::origin_of(u);
        if o != origin {
            return Err(d.dom_err_s(s, "SecurityError", &format!("{WHO}: The origin of the provided {what} ('{o}') does not match the current origin ('{origin}').")));
        }
    }
    let bad = |u: &str| {
        let p = crate::sw_hub::path_of(u).to_ascii_lowercase();
        p.contains("%2f") || p.contains("%5c")
    };
    if bad(&script) || bad(&scope) {
        return Err(s.type_error(&format!("{WHO}: The provided scope ('{scope}') or scriptURL ('{script}') includes a disallowed escape character.")));
    }
    let client = become_client(d, false);
    let start = hub.borrow_mut().register(client, &script, &scope, uvc, module);
    start_job(d, s, start)
}

/// Give the job id to the shim: a job that is done replies through the inbox; else its script fetch starts now.
fn start_job<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, start: Start) -> Res<'s, Local<'s>> {
    let hub = hub_of(d);
    match start {
        Start::Done(snap) => {
            let id = hub.borrow_mut().done_job(d.sw.client.get(), snap);
            Ok(s.number(id as f64))
        }
        Start::Fetch { id, script, uvc, .. } => {
            let referrer = page_url(d);
            let task = d.fetch.client.borrow().fork().spawn(move |c| fetch_script(c, &script, &referrer, uvc));
            d.sw.jobs.borrow_mut().push((id, task));
            Ok(s.number(id as f64))
        }
    }
}

/// Download a service worker script: no redirect, the `Service-Worker: script` header, and no HTTP cache (`send_once`).
fn fetch_script(c: &mut bare_net::http::Client, url: &str, referrer: &str, _uvc: u8) -> Option<Script> {
    let u = bare_url::Url::parse(url).ok()?;
    let headers = [("Service-Worker", "script"), ("Referer", referrer), ("Sec-Fetch-Dest", "serviceworker"), ("Sec-Fetch-Mode", "same-origin"), ("Sec-Fetch-Site", "same-origin")];
    let mut req = bare_net::http::Request::get(&u);
    req.headers = &headers;
    let r = c.send_once(&req, MAX_SCRIPT).ok()?;
    Some(Script { status: r.status, mime: r.header("content-type").map(str::to_string), allowed: r.header("service-worker-allowed").map(str::to_string), text: String::from_utf8_lossy(&r.body).into_owned() })
}

impl<E: Eng> Browser<E> {
    /// The service worker part of a driver turn of a client page: jobs, inbox, and the workers to start. Gives the work done.
    /// `block`: wait for the script downloads that are on their way.
    pub(crate) fn sw_turn(&mut self, block: bool) -> usize {
        let dom = self.dom.clone();
        let client = dom.sw.client.get();
        if client == 0 {
            return 0;
        }
        let hub = hub_of(&dom);
        let mut work = 0;
        hub.borrow_mut().set_hidden(client, dom.basics.background.get());
        let done: Vec<(u64, Option<Script>)> = {
            let mut jobs = dom.sw.jobs.borrow_mut();
            let (ready, wait): (Vec<_>, Vec<_>) = std::mem::take(&mut *jobs).into_iter().partition(|j| block || j.1.done());
            *jobs = wait;
            ready.into_iter().map(|(id, t)| (id, t.wait().flatten())).collect()
        };
        for (id, got) in done {
            work += 1;
            hub.borrow_mut().fetched(id, got);
        }
        let items = hub.borrow_mut().take_inbox(client);
        // A page with no shim yet reads the state when it makes it: the changes are not needed.
        let items = if dom.sw.helper.borrow().is_some() { items } else { Vec::new() };
        for item in items {
            work += 1;
            if let Err(e) = self.sync_item(item) {
                crate::browser_console::push_text(&dom, &mut self.it, crate::browser_console::ConsoleLevel::Error, &format!("service worker: a change was dropped: {e:?}"));
            }
        }
        let now = self.now();
        let start = hub.borrow_mut().to_start(client, now);
        for ver in start {
            work += 1;
            let url = hub.borrow().ver(ver).map(|v| v.url.clone()).unwrap_or_default();
            if dom.kids.add_service(ver, url).is_none() {
                hub.borrow_mut().run_ended(ver);
            }
        }
        work
    }

    /// Give one change of the inbox to the shim, then run the microtasks. A message is queued as a `message` event task at the
    /// container, with the `ServiceWorker` object of the sender as `source` (a script cannot make a `MessageEvent` with a source).
    fn sync_item(&mut self, item: ToClient) -> R<E, ()> {
        let dom = self.dom.clone();
        self.it.host_scope_o(|it| {
            let h = helper(&dom, it)?;
            let msg = crate::browser_scoped::run::<E, _>(it, |s| {
                let h = s.engine().raw_local(&h);
                let f = s.get_str(h, "sync")?;
                let mut msg = None;
                let args: Vec<Local<'_>> = match item {
                    ToClient::Job { id, result } => match result {
                        Ok(snap) => vec![s.string("job")?, s.number(id as f64), reg_snap::<E>(s, &snap)?],
                        Err((name, msg)) => vec![s.string("job")?, s.number(id as f64), s.null(), s.string(name)?, s.string(&msg)?],
                    },
                    ToClient::Reg { snap, found } => vec![s.string("reg")?, reg_snap::<E>(s, &snap)?, s.boolean(found)],
                    ToClient::State { ver, state } => vec![s.string("state")?, s.number(ver as f64), s.string(state.name())?],
                    ToClient::Ctrl(w) => vec![s.string("ctrl")?, w_snap::<E>(s, &w)?],
                    ToClient::Msg { from, origin, mut data } => {
                        let bytes = 16usize.saturating_add(data.cost());
                        let got = dom.receive_clone(s, &mut data)?;
                        msg = Some((origin, bytes, got));
                        vec![s.string("msg")?, w_snap::<E>(s, &Some(from))?]
                    }
                };
                let out = s.call(f, h, &args)?;
                // "msg" gives `[container, the ServiceWorker of the sender]`.
                Ok(match msg {
                    Some((origin, bytes, got)) => {
                        let (c, w) = (s.get_str(out, "0")?, s.get_str(out, "1")?);
                        obj_id::<E>(s, c).zip(obj_id::<E>(s, w)).map(|(c, w)| (c, w, origin, bytes, got))
                    }
                    None => None,
                })
            })?;
            if let Some((c, w, origin, bytes, (data, ports, error))) = msg {
                dom.queue_delivered(c, data, ports.into_boxed_slice(), bytes, Some(Box::new(crate::browser_scheduler::Sender { origin, source: Some(w) })), error)?;
            }
            it.host_checkpoint().map(|_| ())
        })
    }

    /// A worker page: dispatch the events of its version that wait, on the clock `now` of its host. Gives the events given.
    pub(crate) fn dispatch_events(&mut self, now: f64) -> usize {
        let dom = self.dom.clone();
        let ver = dom.sw.version.get();
        let hub = hub_of(&dom);
        let evs = hub.borrow_mut().take_events(ver, now);
        let n = evs.len();
        for ev in evs {
            let snaps = hub.borrow().snaps_of(ver);
            let Some((reg, me)) = snaps else { continue };
            let client = match &ev.kind {
                EvKind::Message { client, .. } => hub.borrow().client(*client).map(|c| (c.uuid.clone(), c.url.clone(), c.nested, c.hidden)),
                _ => None,
            };
                let r = self.it.host_scope_o(|it| {
                let h = helper(&dom, it)?;
                crate::browser_scoped::run::<E, _>(it, |s| {
                    let h = s.engine().raw_local(&h);
                    let f = s.get_str(h, "dispatch")?;
                    let mut args = vec![s.number(ev.id as f64), s.string("")?, reg_snap::<E>(s, &reg)?, w_snap::<E>(s, &Some(me))?];
                    let kind = match ev.kind {
                        EvKind::Start => return Ok(()),
                        EvKind::Install => "install",
                        EvKind::Activate => "activate",
                        EvKind::Message { mut data, .. } => {
                            let (v, ports, _) = dom.receive_clone(s, &mut data)?;
                            let v = s.engine().raw_local(&v);
                            let ports: Vec<Local<'_>> = ports.into_iter().map(|p| dom.obj_local(s, p)).collect();
                            args.push(v);
                            args.push(s.new_array(&ports)?);
                            let c = match &client {
                                Some((uuid, url, nested, hidden)) => {
                                    let items = [s.string(uuid)?, s.string(url)?, s.string("window")?, s.string(if *nested { "nested" } else { "top-level" })?, s.string(if *hidden { "hidden" } else { "visible" })?, s.boolean(!hidden)];
                                    s.new_array(&items)?
                                }
                                None => s.null(),
                            };
                            args.push(c);
                            "message"
                        }
                    };
                    args[1] = s.string(kind)?;
                    s.call(f, h, &args)?;
                    Ok(())
                })?;
                it.host_checkpoint().map(|_| ())
            });
                if let Err(e) = r {
                // A throw in the dispatch ends the event as a failure.
                crate::browser_console::push_text(&dom, &mut self.it, crate::browser_console::ConsoleLevel::Error, &format!("service worker: an event failed: {e:?}"));
                dom.sw.done.borrow_mut().push((ev.id, false));
            }
        }
        n
    }

    /// A worker page: the events that ended since the last call.
    pub(crate) fn take_done(&self) -> Vec<(u64, bool)> {
        std::mem::take(&mut *self.dom.sw.done.borrow_mut())
    }
}

/// Does this client page keep the page from settling: a script download, a change to see or an event to host?
pub(crate) fn busy<E: Eng>(d: &Dom<E>) -> bool {
    !d.sw.jobs.borrow().is_empty() || hub_of(d).borrow().busy_for(d.sw.client.get())
}

/// Is a script download of this page on its way?
pub(crate) fn fetching<E: Eng>(d: &Dom<E>) -> bool {
    d.sw.jobs.borrow().iter().any(|j| !j.1.done())
}

/// Live: can a turn do service worker work now (`Some(true)`), or does a script download wait (`Some(false)`)?
pub(crate) fn live_work<E: Eng>(d: &Dom<E>) -> Option<bool> {
    if d.sw.jobs.borrow().iter().any(|j| j.1.done()) || hub_of(d).borrow().busy_for(d.sw.client.get()) {
        return Some(true);
    }
    (!d.sw.jobs.borrow().is_empty()).then_some(false)
}

/// The worker page of `ver` failed or ended.
pub(crate) fn stopped<E: Eng>(d: &Dom<E>, ver: u64) {
    hub_of(d).borrow_mut().run_ended(ver);
}

/// The document of the worker page of `ver`: its URL, its HTML, and the classic script text that runs when the page is made. A module
/// script is inline in the document.
pub(crate) fn worker_doc<E: Eng>(d: &Dom<E>, ver: u64) -> Result<(String, String, Option<String>), String> {
    const EMPTY: &str = "<!doctype html><html><head></head><body></body></html>";
    let hub = hub_of(d);
    let hub = hub.borrow();
    let v = hub.ver(ver).ok_or("the service worker is gone")?;
    if v.module {
        let text = v.text.replace("</script", "<\\/script");
        return Ok((v.url.clone(), format!("<!doctype html><html><head><script type=module>{text}</script></head><body></body></html>"), None));
    }
    Ok((v.url.clone(), EMPTY.to_string(), Some(v.text.to_string())))
}

/// The script of `ver` ran in its new worker page (`ok`: no throw).
pub(crate) fn evaluated<E: Eng>(d: &Dom<E>, ver: u64, ok: bool) {
    hub_of(d).borrow_mut().evaluated(ver, ok);
}

/// After a turn of the worker page `b` of `ver`: move the events that ended to the hub, at `now` on the clock of the host. Gives true
/// when the worker must stop (idle, or its version is gone).
pub(crate) fn after_turn<E: Eng>(d: &Dom<E>, b: &Browser<E>, ver: u64, now: f64) -> bool {
    let hub = hub_of(d);
    let mut h = hub.borrow_mut();
    for (id, ok) in b.take_done() {
        h.done(ver, id, ok, now);
    }
    let stop = h.should_stop(ver, now);
    if stop {
        h.run_ended(ver);
    }
    stop
}
