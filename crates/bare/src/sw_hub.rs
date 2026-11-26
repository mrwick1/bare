//! The service workers of a session (design S2 of `docs/internal/design-service-workers.md`): the registrations, the jobs, the
//! clients and the events. It holds data only, no heap object. It lives in the session `WebStorage`, so a navigation keeps it and a
//! profile saves the active registrations (`profile.rs`, `sw.bin`). `browser_sw.rs` is the engine side.
//!
//! - A registration has a scope (a URL) and up to three versions: installing, waiting and active. A version holds its script text,
//!   so a start of its worker loads nothing.
//! - A client is a page that read `navigator.serviceWorker` or that a registration controls. It has an inbox: the changes that its
//!   shim must see (`ToClient`), in order. A client has an id (`Client::id`) and a UUID for scripts.
//! - An event (`Ev`) waits for the worker of its version. A version runs in one page: the first client that takes its events hosts
//!   it (`Run`). The host page starts a worker page for it (`Of::Service` in `browser_child.rs`), and stops it after `IDLE_MS` with
//!   no event in flight.
//! - The register job follows the "Register", "Update", "Install" and "Activate" algorithms of the standard. The script fetch runs
//!   in the page that asked (`browser_sw.rs`); `fetched` and `evaluated` go on with the job.
//! - The lifecycle: the script runs first (a throw rejects the job with a `TypeError`). Then the job resolves, `updatefound` and the
//!   `install` event follow. A good install makes the version waiting. It becomes active at once when the registration has no
//!   active version, when the active one controls no client, or after `skipWaiting()`. Then `activate` runs.
//! - A page that a registration does not control pays nothing: `has_regs` is one length check.

use crate::browser_notification::{NotificationAction, NotificationEvent, NotificationInfo};
use bare_js_api::Cloned;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

/// A worker with no event in flight stops after this time (ms of the host page clock), as in Chrome.
pub(crate) const IDLE_MS: f64 = 30_000.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum St {
    Parsed,
    Installing,
    Installed,
    Activating,
    Activated,
    Redundant,
}

impl St {
    pub(crate) fn name(self) -> &'static str {
        match self {
            St::Parsed => "parsed",
            St::Installing => "installing",
            St::Installed => "installed",
            St::Activating => "activating",
            St::Activated => "activated",
            St::Redundant => "redundant",
        }
    }
}

pub(crate) struct Ver {
    pub id: u64,
    pub url: String,
    pub text: Arc<str>,
    pub module: bool,
    pub state: St,
    /// `skipWaiting()` ran.
    pub skip: bool,
}

impl Ver {
    fn snap(&self) -> WSnap {
        (self.id, self.url.clone(), self.state)
    }
}

pub(crate) struct Reg {
    pub id: u64,
    pub scope: String,
    pub origin: String,
    /// `updateViaCache`: 0 imports, 1 all, 2 none.
    pub uvc: u8,
    /// A new version whose script has not run yet, and the job that waits for it.
    parsed: Option<(Ver, u64)>,
    pub installing: Option<Ver>,
    pub waiting: Option<Ver>,
    pub active: Option<Ver>,
    /// `unregister()` ran while a client used it: it goes with its last client.
    uninstalling: bool,
}

impl Reg {
    pub(crate) fn snap(&self) -> RegSnap {
        RegSnap { id: self.id, scope: self.scope.clone(), uvc: self.uvc, w: [self.installing.as_ref().map(Ver::snap), self.waiting.as_ref().map(Ver::snap), self.active.as_ref().map(Ver::snap)] }
    }

    fn newest(&self) -> Option<&Ver> {
        self.installing.as_ref().or(self.waiting.as_ref()).or(self.active.as_ref())
    }

    fn ver(&self, id: u64) -> Option<&Ver> {
        [self.parsed.as_ref().map(|p| &p.0), self.installing.as_ref(), self.waiting.as_ref(), self.active.as_ref()].into_iter().flatten().find(|v| v.id == id)
    }
}

/// A worker as a script sees it: id, script URL, state.
pub(crate) type WSnap = (u64, String, St);

#[derive(Clone)]
pub(crate) struct RegSnap {
    pub id: u64,
    pub scope: String,
    pub uvc: u8,
    /// Installing, waiting, active.
    pub w: [Option<WSnap>; 3],
}

/// A change that a client shim must see.
pub(crate) enum ToClient {
    /// A job of this client ended: the registration, or an error (a `DOMException` name or "TypeError", and the message).
    Job { id: u64, result: Result<RegSnap, (&'static str, String)> },
    /// The workers of a registration changed. `found`: fire `updatefound`.
    Reg { snap: RegSnap, found: bool },
    /// A worker moved to a state: `statechange`.
    State { ver: u64, state: St },
    /// The controller changed: `controllerchange`.
    Ctrl(Option<WSnap>),
    /// A message from a worker.
    Msg { from: WSnap, origin: String, data: Cloned },
    /// `WindowClient.focus()` in a `notificationclick`: the page comes to the front.
    Focus,
}

pub(crate) struct Client {
    pub id: u64,
    pub uuid: String,
    pub url: String,
    pub origin: String,
    pub controller: Option<u64>,
    pub inbox: Vec<ToClient>,
    /// A page in a frame (`frameType` "nested").
    pub nested: bool,
    /// The page is in the background (`visibilityState` "hidden").
    pub hidden: bool,
}

pub(crate) enum EvKind {
    /// No event: the worker starts and its script runs (a new version, before `install`).
    Start,
    Install,
    Activate,
    Message { client: u64, data: Cloned },
    /// The user clicked (`click`, with the `action` button or "") or closed a notification of the registration.
    Note { click: bool, note: Rc<PNote>, action: String },
}

/// A notification of a registration (`showNotification`). It lives in the session until a close.
pub(crate) struct PNote {
    pub id: u64,
    pub reg: u64,
    pub origin: String,
    pub title: String,
    pub body: String,
    pub tag: String,
    pub icon: String,
    pub badge: String,
    pub lang: String,
    pub dir: &'static str,
    pub silent: Option<bool>,
    pub renotify: bool,
    pub require: bool,
    pub timestamp: f64,
    pub actions: Vec<NotificationAction>,
    /// The structured clone of `options.data` with no transfer, so it can be read again; `None` for null.
    pub data: Option<RefCell<Cloned>>,
}

impl PNote {
    fn event(&self, kind: &'static str) -> NotificationEvent {
        NotificationEvent { kind, id: self.id, origin: self.origin.clone(), title: self.title.clone(), body: self.body.clone(), tag: self.tag.clone(), at_ms: self.timestamp }
    }
}

pub(crate) struct Ev {
    pub id: u64,
    pub ver: u64,
    pub kind: EvKind,
}

/// A running worker: its host client, the events in flight (id, and true for `install` or `activate`), and the time it went idle.
pub(crate) struct Run {
    pub ver: u64,
    pub host: u64,
    inflight: Vec<(u64, Life)>,
    idle_since: f64,
}

#[derive(Clone, Copy, PartialEq)]
enum Life {
    Install,
    Activate,
    Other,
}

/// A register or update job whose script is on its way.
struct Job {
    id: u64,
    client: u64,
    reg: u64,
    script: String,
    module: bool,
}

/// What `register` gives the page.
pub(crate) enum Start {
    /// The job is done: the registration.
    Done(RegSnap),
    /// The script must be fetched for job `id`, with `updateViaCache` `uvc`.
    Fetch { id: u64, script: String, uvc: u8 },
}

/// The script reply of a job: the bytes, the `Content-Type` and the `Service-Worker-Allowed` header.
pub(crate) struct Script {
    pub status: u16,
    pub mime: Option<String>,
    pub allowed: Option<String>,
    pub text: String,
}

/// A registration as the profile keeps it: only one with an active version.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedReg {
    pub scope: String,
    pub script: String,
    pub uvc: u8,
    pub module: bool,
    pub text: String,
}

#[derive(Default)]
pub(crate) struct Hub {
    regs: Vec<Reg>,
    clients: Vec<Client>,
    jobs: Vec<Job>,
    events: Vec<Ev>,
    runs: Vec<Run>,
    next: u64,
    /// Moves when what the profile keeps changes.
    pub rev: u64,
    /// The live notifications of the registrations, oldest first.
    notes: Vec<Rc<PNote>>,
    /// The event log of those notifications, drained per origin by an agent.
    log: Vec<NotificationEvent>,
    /// The id of the last notification of the session: a page notification and a registration notification share the numbers.
    note_last: u64,
}

/// The origin of a URL: `scheme://host[:port]`.
pub(crate) fn origin_of(url: &str) -> String {
    match url.find("://") {
        Some(i) => url[..i + 3 + url[i + 3..].find(['/', '?', '#']).unwrap_or(url.len() - i - 3)].to_ascii_lowercase(),
        None => String::new(),
    }
}

/// The path of a URL (no query, no fragment).
pub(crate) fn path_of(url: &str) -> &str {
    let rest = url.find("://").map_or(url, |i| &url[i + 3..]);
    let p = rest.find('/').map_or("/", |i| &rest[i..]);
    p.split(['?', '#']).next().unwrap_or("/")
}

/// `JS_TYPES` of a classic script, as `resources.rs` checks them.
fn js_mime(m: &str) -> bool {
    let e = m.split(';').next().unwrap_or("").trim();
    ["application/javascript", "application/ecmascript", "application/x-ecmascript", "application/x-javascript", "text/javascript", "text/ecmascript", "text/javascript1.0", "text/javascript1.1", "text/javascript1.2", "text/javascript1.3", "text/javascript1.4", "text/javascript1.5", "text/jscript", "text/livescript", "text/x-ecmascript", "text/x-javascript"]
        .iter()
        .any(|j| e.eq_ignore_ascii_case(j))
}

impl Hub {
    fn id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    /// Is there a registration at all? A page pays this check and nothing else.
    #[inline]
    pub(crate) fn has_regs(&self) -> bool {
        !self.regs.is_empty()
    }

    /// The registration whose scope is the longest prefix of `url` (one that `unregister()` marked is skipped).
    pub(crate) fn match_url(&self, url: &str) -> Option<&Reg> {
        self.regs.iter().filter(|r| !r.uninstalling && url.starts_with(&r.scope)).max_by_key(|r| r.scope.len())
    }

    /// The registrations of `origin`, in creation order.
    pub(crate) fn regs_of(&self, origin: &str) -> Vec<RegSnap> {
        self.regs.iter().filter(|r| r.origin == origin && !r.uninstalling).map(Reg::snap).collect()
    }

    fn reg_of_ver(&self, ver: u64) -> Option<usize> {
        self.regs.iter().position(|r| r.ver(ver).is_some())
    }

    pub(crate) fn ver(&self, ver: u64) -> Option<&Ver> {
        self.regs.iter().find_map(|r| r.ver(ver))
    }

    /// The registration and the worker of version `ver`, for a dispatch in its worker.
    pub(crate) fn snaps_of(&self, ver: u64) -> Option<(RegSnap, WSnap)> {
        let r = &self.regs[self.reg_of_ver(ver)?];
        Some((r.snap(), r.ver(ver)?.snap()))
    }

    // ------------------------------------------------------------------------------------------ clients

    /// A new client at `url`. A registration whose scope matches it and that has an active version controls it, when `control`.
    pub(crate) fn add_client(&mut self, uuid: String, url: &str, control: bool, nested: bool) -> u64 {
        let id = self.id();
        let controller = if control { self.match_url(url).and_then(|r| r.active.as_ref()).map(|v| v.id) } else { None };
        self.clients.push(Client { id, uuid, url: url.to_string(), origin: origin_of(url), controller, inbox: Vec::new(), nested, hidden: false });
        id
    }

    pub(crate) fn set_hidden(&mut self, id: u64, hidden: bool) {
        if let Some(c) = self.clients.iter_mut().find(|c| c.id == id) {
            c.hidden = hidden;
        }
    }

    /// A job that ended at its start (`Start::Done`): its reply goes in the inbox of `client`. Gives the job id.
    pub(crate) fn done_job(&mut self, client: u64, snap: RegSnap) -> u64 {
        let id = self.id();
        self.reply(client, id, Ok(snap));
        id
    }

    pub(crate) fn client(&self, id: u64) -> Option<&Client> {
        self.clients.iter().find(|c| c.id == id)
    }

    /// The page of client `id` ended. Its workers end too (they were its child pages), a waiting version may become active, and a
    /// registration that waited for its last client goes.
    pub(crate) fn remove_client(&mut self, id: u64) {
        self.clients.retain(|c| c.id != id);
        let hosted: Vec<u64> = self.runs.iter().filter(|r| r.host == id).map(|r| r.ver).collect();
        for v in hosted {
            self.run_ended(v);
        }
        for i in 0..self.regs.len() {
            self.try_activate(i);
        }
        let gone: Vec<usize> = (0..self.regs.len()).filter(|&i| self.regs[i].uninstalling && !self.controls(i)).collect();
        for i in gone.into_iter().rev() {
            self.clear(i);
        }
    }

    /// Does the active version of registration `i` control a client?
    fn controls(&self, i: usize) -> bool {
        let Some(a) = self.regs[i].active.as_ref().map(|v| v.id) else { return false };
        self.clients.iter().any(|c| c.controller == Some(a))
    }

    pub(crate) fn take_inbox(&mut self, client: u64) -> Vec<ToClient> {
        self.clients.iter_mut().find(|c| c.id == client).map(|c| std::mem::take(&mut c.inbox)).unwrap_or_default()
    }

    /// Does client `id` have work: a change to see, an event to host, or a worker to stop?
    pub(crate) fn busy_for(&self, id: u64) -> bool {
        self.clients.iter().any(|c| c.id == id && !c.inbox.is_empty()) || self.events.iter().any(|e| self.runs.iter().all(|r| r.ver != e.ver) || self.runs.iter().any(|r| r.ver == e.ver && r.host == id))
    }

    fn push(&mut self, origin: &str, item: impl Fn() -> ToClient) {
        for c in self.clients.iter_mut().filter(|c| c.origin == origin) {
            c.inbox.push(item());
        }
    }

    fn push_reg(&mut self, i: usize, found: bool) {
        let (origin, snap) = (self.regs[i].origin.clone(), self.regs[i].snap());
        self.push(&origin, || ToClient::Reg { snap: snap.clone(), found });
    }

    fn push_state(&mut self, origin: &str, ver: u64, state: St) {
        self.push(origin, || ToClient::State { ver, state });
    }

    // ------------------------------------------------------------------------------------------ jobs

    /// The register job of client `client` for `script` and `scope` (absolute, same origin, checked by the caller). An equal
    /// newest version ends the job at once.
    pub(crate) fn register(&mut self, client: u64, script: &str, scope: &str, uvc: u8, module: bool) -> Start {
        let i = match self.regs.iter().position(|r| r.scope == scope) {
            Some(i) => {
                let r = &mut self.regs[i];
                r.uninstalling = false;
                if r.newest().is_some_and(|v| v.url == script && v.module == module) && r.uvc == uvc {
                    return Start::Done(r.snap());
                }
                r.uvc = uvc;
                i
            }
            None => {
                let id = self.id();
                self.regs.push(Reg { id, scope: scope.to_string(), origin: origin_of(scope), uvc, parsed: None, installing: None, waiting: None, active: None, uninstalling: false });
                self.regs.len() - 1
            }
        };
        self.update_job(client, i, script.to_string(), module)
    }

    /// `registration.update()`: fetch the script of the newest version again.
    pub(crate) fn update(&mut self, client: u64, reg: u64) -> Option<Start> {
        let i = self.regs.iter().position(|r| r.id == reg)?;
        let v = self.regs[i].newest()?;
        let (script, module) = (v.url.clone(), v.module);
        Some(self.update_job(client, i, script, module))
    }

    fn update_job(&mut self, client: u64, i: usize, script: String, module: bool) -> Start {
        let id = self.id();
        let (reg, uvc) = (self.regs[i].id, self.regs[i].uvc);
        self.jobs.push(Job { id, client, reg, script: script.clone(), module });
        Start::Fetch { id, script, uvc }
    }

    /// The script of job `id` came (or `None`: a network error). Check it, and start a new version when it differs from the newest.
    pub(crate) fn fetched(&mut self, id: u64, got: Option<Script>) {
        let Some(k) = self.jobs.iter().position(|j| j.id == id) else { return };
        let job = self.jobs.remove(k);
        let Some(i) = self.regs.iter().position(|r| r.id == job.reg) else {
            return self.reply(job.client, id, Err(("InvalidStateError", "The registration was removed.".into())));
        };
        let who = format!("Failed to register a ServiceWorker for scope ('{}') with script ('{}')", self.regs[i].scope, job.script);
        let bad = match &got {
            None => Some(("TypeError", format!("{who}: An unknown error occurred when fetching the script."))),
            Some(s) if !(200..300).contains(&s.status) => Some(("TypeError", format!("{who}: A bad HTTP response code ({}) was received when fetching the script.", s.status))),
            Some(s) if !s.mime.as_deref().is_some_and(js_mime) => {
                let m = s.mime.as_deref().map_or(String::new(), |m| m.split(';').next().unwrap_or("").trim().to_string());
                Some(("SecurityError", if m.is_empty() { format!("{who}: The script does not have a MIME type.") } else { format!("{who}: The script has an unsupported MIME type ('{m}').") }))
            }
            Some(s) => {
                // The max scope: the directory of the script, or the `Service-Worker-Allowed` header resolved against it.
                let dir = path_of(&job.script).rsplit_once('/').map_or("/".to_string(), |(d, _)| format!("{d}/"));
                let max = s.allowed.as_deref().map_or(dir.clone(), |a| if a.starts_with('/') { a.to_string() } else { format!("{dir}{a}") });
                let scope_path = path_of(&self.regs[i].scope).to_string();
                (!scope_path.starts_with(&max)).then(|| ("SecurityError", format!("{who}: The path of the provided scope ('{scope_path}') is not under the max scope allowed ('{max}'). Adjust the scope, move the Service Worker script, or use the Service-Worker-Allowed HTTP header to allow the scope.")))
            }
        };
        if let Some(err) = bad {
            self.reply(job.client, id, Err(err));
            return self.drop_if_empty(i);
        }
        let text = got.expect("checked").text;
        let same = self.regs[i].newest().is_some_and(|v| v.url == job.script && *v.text == *text);
        if same {
            let snap = self.regs[i].snap();
            return self.reply(job.client, id, Ok(snap));
        }
        let vid = self.id();
        self.regs[i].parsed = Some((Ver { id: vid, url: job.script, text: text.into(), module: job.module, state: St::Parsed, skip: false }, id));
        self.jobs.push(Job { id, client: job.client, reg: job.reg, script: String::new(), module: job.module });
        let eid = self.id();
        self.events.push(Ev { id: eid, ver: vid, kind: EvKind::Start });
    }

    /// The script of version `ver` ran in its new worker (`ok`: no throw). A parsed version goes on to `install`, or its job fails.
    pub(crate) fn evaluated(&mut self, ver: u64, ok: bool) {
        let Some(i) = self.regs.iter().position(|r| r.parsed.as_ref().is_some_and(|p| p.0.id == ver)) else { return };
        let (mut v, job) = self.regs[i].parsed.take().expect("found");
        let client = self.jobs.iter().position(|j| j.id == job).map(|k| self.jobs.remove(k).client).unwrap_or(0);
        if !ok {
            let who = format!("Failed to register a ServiceWorker for scope ('{}') with script ('{}')", self.regs[i].scope, v.url);
            self.reply(client, job, Err(("TypeError", format!("{who}: ServiceWorker script evaluation failed"))));
            self.runs.retain(|r| r.ver != ver);
            return self.drop_if_empty(i);
        }
        // Install: the job resolves, then `updatefound`, then the `install` event.
        if let Some(old) = self.regs[i].installing.take() {
            let origin = self.regs[i].origin.clone();
            self.push_state(&origin, old.id, St::Redundant);
        }
        v.state = St::Installing;
        self.regs[i].installing = Some(v);
        let snap = self.regs[i].snap();
        self.reply(client, job, Ok(snap));
        self.push_reg(i, true);
        let id = self.id();
        self.events.push(Ev { id, ver, kind: EvKind::Install });
    }

    fn reply(&mut self, client: u64, id: u64, result: Result<RegSnap, (&'static str, String)>) {
        if let Some(c) = self.clients.iter_mut().find(|c| c.id == client) {
            c.inbox.push(ToClient::Job { id, result });
        }
    }

    /// A registration with no version left goes ("Try Clear Registration").
    fn drop_if_empty(&mut self, i: usize) {
        let r = &self.regs[i];
        if r.parsed.is_none() && r.newest().is_none() && !self.jobs.iter().any(|j| j.reg == r.id) {
            self.regs.remove(i);
        }
    }

    /// `unregister()`: the registration goes now, or with its last client. Gives false when it was gone already.
    pub(crate) fn unregister(&mut self, reg: u64) -> bool {
        let Some(i) = self.regs.iter().position(|r| r.id == reg && !r.uninstalling) else { return false };
        self.regs[i].uninstalling = true;
        if !self.controls(i) {
            self.clear(i);
        }
        self.rev += 1;
        true
    }

    /// Remove registration `i`: each of its versions becomes redundant.
    fn clear(&mut self, i: usize) {
        let mut r = self.regs.remove(i);
        let (gone, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.notes).into_iter().partition(|n| n.reg == r.id);
        self.notes = keep;
        self.log.extend(gone.iter().map(|n| n.event("closed")));
        let vs: Vec<Ver> = [r.installing.take(), r.waiting.take(), r.active.take()].into_iter().flatten().collect();
        for v in vs {
            self.push_state(&r.origin, v.id, St::Redundant);
            self.events.retain(|e| e.ver != v.id);
        }
        self.rev += 1;
    }

    // ------------------------------------------------------------------------------------------ the lifecycle

    /// The events of `ver` to dispatch now, in order. They count as in flight until `done`.
    pub(crate) fn take_events(&mut self, ver: u64, now: f64) -> Vec<Ev> {
        let (mine, rest): (Vec<Ev>, Vec<Ev>) = std::mem::take(&mut self.events).into_iter().partition(|e| e.ver == ver);
        self.events = rest;
        if !mine.is_empty() && let Some(r) = self.runs.iter_mut().find(|r| r.ver == ver) {
            for e in &mine {
                let life = match e.kind {
                    EvKind::Install => Life::Install,
                    EvKind::Activate => Life::Activate,
                    _ => Life::Other,
                };
                if !matches!(e.kind, EvKind::Start) {
                    r.inflight.push((e.id, life));
                }
            }
            r.idle_since = now;
        }
        mine
    }

    /// The versions with an event and no worker: client `host` starts them now.
    pub(crate) fn to_start(&mut self, host: u64, now: f64) -> Vec<u64> {
        let mut out: Vec<u64> = Vec::new();
        for e in &self.events {
            if !out.contains(&e.ver) && self.runs.iter().all(|r| r.ver != e.ver) {
                out.push(e.ver);
            }
        }
        for &ver in &out {
            self.runs.push(Run { ver, host, inflight: Vec::new(), idle_since: now });
        }
        out
    }

    /// The workers that run now (tests).
    #[cfg(test)]
    pub(crate) fn running(&self) -> usize {
        self.runs.len()
    }

    /// Has the worker of `ver` been idle for `IDLE_MS`, or is its version gone? Then its host stops it.
    pub(crate) fn should_stop(&self, ver: u64, now: f64) -> bool {
        match self.runs.iter().find(|r| r.ver == ver) {
            None => true,
            Some(r) => self.ver(ver).is_none_or(|v| v.state == St::Redundant) && r.inflight.is_empty() || r.inflight.is_empty() && !self.events.iter().any(|e| e.ver == ver) && now - r.idle_since >= IDLE_MS,
        }
    }

    /// The worker of `ver` stopped or its page ended. An `install` in flight fails; an `activate` in flight counts as done.
    pub(crate) fn run_ended(&mut self, ver: u64) {
        let Some(k) = self.runs.iter().position(|r| r.ver == ver) else { return };
        let r = self.runs.remove(k);
        for (id, life) in r.inflight {
            match life {
                Life::Install => self.finish(ver, Life::Install, false),
                Life::Activate => self.finish(ver, Life::Activate, true),
                Life::Other => {}
            }
            let _ = id;
        }
        if self.regs.iter().any(|r| r.parsed.as_ref().is_some_and(|p| p.0.id == ver)) {
            self.evaluated(ver, false);
        }
    }

    /// The event `id` of `ver` ended: `ok` is false when a `waitUntil` promise rejected.
    pub(crate) fn done(&mut self, ver: u64, id: u64, ok: bool, now: f64) {
        let Some(r) = self.runs.iter_mut().find(|r| r.ver == ver) else { return };
        let Some(k) = r.inflight.iter().position(|e| e.0 == id) else { return };
        let (_, life) = r.inflight.remove(k);
        r.idle_since = now;
        self.finish(ver, life, ok);
    }

    fn finish(&mut self, ver: u64, life: Life, ok: bool) {
        let Some(i) = self.reg_of_ver(ver) else { return };
        let origin = self.regs[i].origin.clone();
        match life {
            Life::Install if self.regs[i].installing.as_ref().is_some_and(|v| v.id == ver) => {
                let mut v = self.regs[i].installing.take().expect("checked");
                if !ok {
                    self.push_reg(i, false);
                    self.push_state(&origin, ver, St::Redundant);
                    return self.drop_if_empty(i);
                }
                if let Some(old) = self.regs[i].waiting.take() {
                    self.push_state(&origin, old.id, St::Redundant);
                }
                v.state = St::Installed;
                self.regs[i].waiting = Some(v);
                self.push_reg(i, false);
                self.push_state(&origin, ver, St::Installed);
                self.try_activate(i);
            }
            Life::Activate if self.regs[i].active.as_ref().is_some_and(|v| v.id == ver && v.state == St::Activating) => {
                self.regs[i].active.as_mut().expect("checked").state = St::Activated;
                self.push_state(&origin, ver, St::Activated);
                self.rev += 1;
            }
            _ => {}
        }
    }

    /// Activate the waiting version of registration `i` when nothing holds it: no active version, an active one that controls no
    /// client, or `skipWaiting()`.
    fn try_activate(&mut self, i: usize) {
        let Some(w) = self.regs[i].waiting.as_ref() else { return };
        if !(w.skip || self.regs[i].active.is_none() || !self.controls(i)) {
            return;
        }
        let origin = self.regs[i].origin.clone();
        let mut v = self.regs[i].waiting.take().expect("checked");
        let old = self.regs[i].active.take();
        if let Some(o) = &old {
            self.push_state(&origin, o.id, St::Redundant);
        }
        v.state = St::Activating;
        let (vid, snap) = (v.id, v.snap());
        self.regs[i].active = Some(v);
        self.push_reg(i, false);
        self.push_state(&origin, vid, St::Activating);
        // The clients of the old version get the new one as their controller.
        if let Some(o) = old {
            for c in self.clients.iter_mut().filter(|c| c.controller == Some(o.id)) {
                c.controller = Some(vid);
                c.inbox.push(ToClient::Ctrl(Some(snap.clone())));
            }
        }
        let id = self.id();
        self.events.push(Ev { id, ver: vid, kind: EvKind::Activate });
        self.rev += 1;
    }

    /// `skipWaiting()` of version `ver`.
    pub(crate) fn skip_waiting(&mut self, ver: u64) {
        let Some(i) = self.reg_of_ver(ver) else { return };
        let r = &mut self.regs[i];
        for v in [r.installing.as_mut(), r.waiting.as_mut()].into_iter().flatten() {
            if v.id == ver {
                v.skip = true;
            }
        }
        if r.waiting.as_ref().is_some_and(|v| v.id == ver) {
            self.try_activate(i);
        }
    }

    /// `clients.claim()` of version `ver`: each client in the scope that its registration matches gets it as controller. False when
    /// `ver` is not the active version.
    pub(crate) fn claim(&mut self, ver: u64) -> bool {
        let Some(i) = self.reg_of_ver(ver) else { return false };
        let r = &self.regs[i];
        let Some(v) = r.active.as_ref().filter(|v| v.id == ver && matches!(v.state, St::Activating | St::Activated)) else { return false };
        let (snap, reg) = (v.snap(), r.id);
        let wanted: Vec<u64> = self.clients.iter().filter(|c| c.controller != Some(ver) && self.match_url(&c.url).is_some_and(|m| m.id == reg)).map(|c| c.id).collect();
        for c in self.clients.iter_mut().filter(|c| wanted.contains(&c.id)) {
            c.controller = Some(ver);
            c.inbox.push(ToClient::Ctrl(Some(snap.clone())));
        }
        true
    }

    /// The clients that `ver` sees: of its origin, controlled by it unless `all`. In creation order.
    pub(crate) fn clients_of(&self, ver: u64, all: bool) -> Vec<&Client> {
        let Some(i) = self.reg_of_ver(ver) else { return Vec::new() };
        let r = &self.regs[i];
        self.clients.iter().filter(|c| c.origin == r.origin && (c.controller == Some(ver) || all && c.url.starts_with(&r.scope))).collect()
    }

    /// Queue a message from client `client` to the worker `ver`. False when `ver` is gone.
    pub(crate) fn post(&mut self, ver: u64, client: u64, data: Cloned) -> bool {
        if self.ver(ver).is_none_or(|v| v.state == St::Redundant) {
            return false;
        }
        let id = self.id();
        self.events.push(Ev { id, ver, kind: EvKind::Message { client, data } });
        true
    }

    /// A message from the worker `ver` to the client with `uuid`. False when no such client.
    pub(crate) fn post_to_client(&mut self, ver: u64, uuid: &str, data: Cloned) -> Result<(), Cloned> {
        let Some(from) = self.ver(ver).map(Ver::snap) else { return Err(data) };
        let Some(c) = self.clients.iter_mut().find(|c| c.uuid == uuid) else { return Err(data) };
        let origin = c.origin.clone();
        c.inbox.push(ToClient::Msg { from, origin, data });
        Ok(())
    }

    // ------------------------------------------------------------------------------------------ notifications

    /// The id of a new notification of the session.
    pub(crate) fn note_id(&mut self) -> u64 {
        self.note_last += 1;
        self.note_last
    }

    /// Does registration `reg` have an active version?
    pub(crate) fn has_active(&self, reg: u64) -> bool {
        self.regs.iter().any(|r| r.id == reg && r.active.is_some())
    }

    /// Show `n` (its id from `note_id`). A live notification of the origin with the same tag leaves with no event ("replaced").
    pub(crate) fn show_note(&mut self, n: PNote) {
        if !n.tag.is_empty()
            && let Some(i) = self.notes.iter().position(|o| o.origin == n.origin && o.tag == n.tag)
        {
            let old = self.notes.remove(i);
            self.log.push(old.event("replaced"));
        }
        self.log.push(n.event("shown"));
        self.notes.push(Rc::new(n));
    }

    /// The live notifications of registration `reg` with tag `tag` (all with ""), oldest first.
    pub(crate) fn notes_of(&self, reg: u64, tag: &str) -> Vec<Rc<PNote>> {
        self.notes.iter().filter(|n| n.reg == reg && (tag.is_empty() || n.tag == tag)).cloned().collect()
    }

    /// Take the live notification `id` of `origin` out of the list and log "closed".
    fn take_note(&mut self, id: u64, origin: &str) -> Option<Rc<PNote>> {
        let i = self.notes.iter().position(|n| n.id == id && n.origin == origin)?;
        let n = self.notes.remove(i);
        self.log.push(n.event("closed"));
        Some(n)
    }

    /// `close()` of a notification object: no event.
    pub(crate) fn close_note(&mut self, id: u64, origin: &str) {
        self.take_note(id, origin);
    }

    /// The user clicks the live notification `id` of `origin`, on the button `action` ("" for the notification). The active worker
    /// of its registration gets `notificationclick`. False when no such notification or button.
    pub(crate) fn click_note(&mut self, id: u64, origin: &str, action: &str) -> bool {
        let Some(n) = self.notes.iter().find(|n| n.id == id && n.origin == origin).cloned() else { return false };
        if !action.is_empty() && !n.actions.iter().any(|a| a.action == action) {
            return false;
        }
        self.log.push(n.event("clicked"));
        self.note_event(n, true, action.to_string());
        true
    }

    /// The user closes the live notification `id` of `origin`: it leaves, and the worker gets `notificationclose`.
    pub(crate) fn user_close_note(&mut self, id: u64, origin: &str) -> bool {
        let Some(n) = self.take_note(id, origin) else { return false };
        self.note_event(n, false, String::new());
        true
    }

    fn note_event(&mut self, note: Rc<PNote>, click: bool, action: String) {
        let Some(ver) = self.regs.iter().find(|r| r.id == note.reg).and_then(|r| r.active.as_ref()).map(|v| v.id) else { return };
        let id = self.id();
        self.events.push(Ev { id, ver, kind: EvKind::Note { click, note, action } });
    }

    /// The live notifications of `origin`, as an agent sees them.
    pub(crate) fn note_infos(&self, origin: &str) -> Vec<NotificationInfo> {
        self.notes
            .iter()
            .filter(|n| n.origin == origin)
            .map(|n| NotificationInfo { id: n.id, origin: n.origin.clone(), title: n.title.clone(), body: n.body.clone(), tag: n.tag.clone(), icon: n.icon.clone(), silent: n.silent == Some(true), at_ms: n.timestamp, actions: n.actions.clone() })
            .collect()
    }

    /// Drain the event log entries of `origin`.
    pub(crate) fn take_log(&mut self, origin: &str) -> Vec<NotificationEvent> {
        if self.log.is_empty() {
            return Vec::new();
        }
        let (mine, rest) = std::mem::take(&mut self.log).into_iter().partition(|e| e.origin == origin);
        self.log = rest;
        mine
    }

    pub(crate) fn log_count(&self, origin: &str) -> usize {
        self.log.iter().filter(|e| e.origin == origin).count()
    }

    /// `WindowClient.focus()` of the client `uuid`: its page comes to the front at its next turn. False when no such client.
    pub(crate) fn focus(&mut self, uuid: &str) -> bool {
        let Some(c) = self.clients.iter_mut().find(|c| c.uuid == uuid) else { return false };
        c.hidden = false;
        c.inbox.push(ToClient::Focus);
        true
    }

    // ------------------------------------------------------------------------------------------ the profile

    /// The registrations with an active version, for the profile.
    pub(crate) fn export(&self) -> Vec<SavedReg> {
        self.regs
            .iter()
            .filter(|r| !r.uninstalling)
            .filter_map(|r| r.active.as_ref().map(|v| SavedReg { scope: r.scope.clone(), script: v.url.clone(), uvc: r.uvc, module: v.module, text: v.text.to_string() }))
            .collect()
    }

    /// Registrations from the profile: each comes back with an activated version.
    pub(crate) fn import(&mut self, list: &[SavedReg]) {
        for s in list {
            if self.regs.iter().any(|r| r.scope == s.scope) {
                continue;
            }
            let (id, vid) = (self.id(), self.id());
            let v = Ver { id: vid, url: s.script.clone(), text: s.text.as_str().into(), module: s.module, state: St::Activated, skip: false };
            self.regs.push(Reg { id, scope: s.scope.clone(), origin: origin_of(&s.scope), uvc: s.uvc, parsed: None, installing: None, waiting: None, active: Some(v), uninstalling: false });
        }
    }
}
