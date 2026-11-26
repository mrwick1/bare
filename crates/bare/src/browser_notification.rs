//! `Notification` (Notifications API, "https://notifications.spec.whatwg.org/") and the permission of an origin.
//!
//! bare shows no notification on the desktop. A notification of a page with the grant goes into a list in the page, and an event log
//! records it, so that an agent reads it (`Browser::notifications`, `Browser::take_notification_events`, the bare-mcp tool
//! `notifications`; `docs/internal/design-notifications.md`).
//!
//! - `Notification` is an accessor of the global until the first read, as `Worker` is (`browser_worker.rs`).
//! - `new Notification(title[, options])` reads `NotificationOptions` in name order: `badge` and `icon` (resolved against the
//!   document), `body`, `data` (a structured clone), `dir` (an enum: another value is a `TypeError`), `lang`, `renotify`,
//!   `requireInteraction`, `silent` (null when absent), `tag`, `timestamp` (default: the wall clock) and only the presence of `vibrate`.
//!   `renotify` with an empty tag, and `silent` with `vibrate`, throw a `TypeError`, as in Chrome. `actions`, `image` and `vibrate`
//!   are not kept.
//! - A task after the constructor shows it when the permission was "granted" at the constructor: a live notification with the same
//!   tag leaves the list with no event (the log says "replaced"), the new one joins it, and `show` fires. Without the grant, `error`
//!   fires. No event object is made when nobody listens.
//! - `close()` of a shown (or still pending, granted) notification takes it out of the list, and a task fires `close`. A second
//!   `close()` does nothing. A shown notification is a GC root, so a later click reaches it. A collected notification drops its entry
//!   (`forget`).
//! - Permission: the decision of each origin is in the `WebStorage` of the session (`WebStorage::permission`), set by
//!   `Browser::set_permission`. `Notification.permission` is "granted" only for a decision "granted" in a secure context. Else it is
//!   "denied": with no decision too (owner decision, 2026-11-26), so a page never sees "default". `requestPermission([callback])`
//!   shows no prompt: it gives the state, and with no decision it logs "permission-requested". A function callback gets the state at
//!   once, not in a later task. A value that is not a function is ignored. `Notification.maxActions` is 0.
//! - `navigator.permissions.query({name})` resolves a new `PermissionStatus` (an `EventTarget` with `name`, `state` and `onchange`).
//!   "notifications" has the state of `Notification.permission`; another known name is "prompt"; an unknown name rejects with a
//!   `TypeError`, as in Chrome. A status with a listener is a root. `Browser::set_permission` fires a trusted `change` at each live
//!   status whose state moved. There are no `Permissions` and `PermissionStatus` globals.
//! - Not here: `ServiceWorkerRegistration.showNotification`, `actions`, an OS notification.

use crate::browser::*;
use crate::browser_handlers::{handler_get, handler_set};
use crate::browser_scheduler::Task;
use crate::browser_scoped::*;
use crate::browser_sources::Roots;
use crate::eng::{Eng, Ops};
use bare_js_api::{Args, Attrs, Engine as _, HostFn, Intrinsic, Local, Type};
use std::cell::{Cell, RefCell};

/// The permission names of Chrome's `PermissionName` enum that `query` accepts.
const NAMES: [&str; 22] = [
    "accelerometer", "ambient-light-sensor", "background-fetch", "background-sync", "camera", "clipboard-read", "clipboard-write",
    "display-capture", "geolocation", "gyroscope", "idle-detection", "local-fonts", "magnetometer", "microphone", "midi", "notifications",
    "payment-handler", "persistent-storage", "screen-wake-lock", "storage-access", "window-management", "push",
];

#[derive(Default)]
pub(crate) struct NotificationHost<E: Eng> {
    /// The notifications that a script can still reach, by object.
    notes: RefCell<IdMap<<E as Eng>::Obj, Note<E>>>,
    /// The live (shown) notifications, oldest first. Roots.
    shown: RefCell<Vec<<E as Eng>::Obj>>,
    /// The event log that an agent drains (`Browser::take_notification_events`). No cap (owner decision, 2026-11-26).
    events: RefCell<Vec<NotificationEvent>>,
    /// The id of the last shown notification.
    next_id: Cell<u64>,
    /// The prototype and the constructor, once made. Roots.
    made: Cell<Option<[<E as Eng>::Obj; 2]>>,
    /// `navigator.permissions` and the `PermissionStatus` prototype, once made. Roots.
    perms: Cell<Option<[<E as Eng>::Obj; 2]>>,
    /// The live `PermissionStatus` objects: the name, the last state and the creation number.
    statuses: RefCell<IdMap<<E as Eng>::Obj, (&'static str, &'static str, u64)>>,
    /// The creation number of the next `PermissionStatus`.
    seq: Cell<u64>,
}

/// A notification of the page. `data` is the structured clone of `options.data` (or null), a root while the entry lives.
struct Note<E: Eng> {
    /// The number of the notification in the list and the event log (`Dom::notification_id`), 0 until it shows.
    id: u64,
    title: String,
    body: String,
    tag: String,
    icon: String,
    badge: String,
    lang: String,
    dir: &'static str,
    data: E::Raw,
    silent: Option<bool>,
    renotify: bool,
    require: bool,
    /// `options.timestamp`, or the wall clock at the constructor (ms since the epoch).
    timestamp: f64,
    /// The permission was "granted" at the constructor.
    granted: bool,
    state: State,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// The task after the constructor has not run.
    Pending,
    /// In the list of live notifications.
    Shown,
    /// `close()` ran: the next task fires `close`.
    Closing,
    /// Not in the list: closed, replaced, or never shown.
    Gone,
}

/// A live notification, as an agent sees it (`Browser::notifications`).
#[derive(Clone, Debug, PartialEq)]
pub struct NotificationInfo {
    pub id: u64,
    pub origin: String,
    pub title: String,
    pub body: String,
    pub tag: String,
    pub icon: String,
    pub silent: bool,
    /// `timestamp` of the notification: ms since the epoch.
    pub at_ms: f64,
}

/// An entry of the event log (`Browser::take_notification_events`). `kind` is "shown", "replaced", "closed", "clicked" or
/// "permission-requested". A "permission-requested" entry has id 0 and empty text.
#[derive(Clone, Debug, PartialEq)]
pub struct NotificationEvent {
    pub kind: &'static str,
    pub id: u64,
    pub origin: String,
    pub title: String,
    pub body: String,
    pub tag: String,
    pub at_ms: f64,
}

pub(crate) fn roots<E: Eng>(d: &Dom<E>, out: &mut Roots<'_, '_, E>) {
    let h = &d.notification;
    if let Some(m) = h.made.get() {
        out.extend(m.map(E::obj_raw));
    }
    if let Some(m) = h.perms.get() {
        out.extend(m.map(E::obj_raw));
    }
    out.extend(h.shown.borrow().iter().map(|&o| E::obj_raw(o)));
    out.extend(h.notes.borrow().values().map(|n| n.data));
    let statuses = h.statuses.borrow();
    if !statuses.is_empty() {
        let st = d.st.borrow();
        out.extend(statuses.keys().filter(|&&o| st.listeners.get(&Target::Obj(o)).is_some_and(|l| !l.is_empty())).map(|&o| E::obj_raw(o)));
    }
}

/// The notification or the `PermissionStatus` `o` is dead: drop its entry.
pub(crate) fn forget<E: Eng>(d: &Dom<E>, o: E::Obj) {
    d.notification.notes.borrow_mut().remove(&o);
    d.notification.statuses.borrow_mut().remove(&o);
}

pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    let g = s.global();
    let f = d.add_fn_s(s, lazy, "Notification", 0, HostFn::Method)?;
    s.define_accessor_str(g, "Notification", Some(f), Some(f), Attrs { writable: false, enumerable: false, configurable: true })
}

/// The accessor of the global until the first read. A call with an argument is the setter.
fn lazy<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let g = s.global();
    let attrs = Attrs { writable: true, enumerable: false, configurable: true };
    if let Some(v) = a.get(0) {
        s.define_str(g, "Notification", v, attrs)?;
        return Ok(s.undefined());
    }
    let ctor = match d.notification.made.get() {
        Some(m) => m[1],
        None => old::<E, _>(s, |it| make(d, it))?,
    };
    let ctor = d.obj_local(s, ctor);
    s.define_str(g, "Notification", ctor, attrs)?;
    Ok(ctor)
}

/// The event handler attributes of a notification.
const HANDLERS: [&str; 4] = ["show", "click", "close", "error"];

#[cold]
#[inline(never)]
fn make<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, E::Obj> {
    let proto = d.obj(it, d.p().et)?;
    let f = run::<E, _>(it, |s| {
        let f = d.add_fn_s(s, ctor, "Notification", 1, HostFn::CtorNewTarget)?;
        let p = d.obj_local(s, proto);
        s.define_str(f, "prototype", p, Attrs { writable: false, enumerable: false, configurable: false })?;
        s.define_str(p, "constructor", f, Attrs { writable: true, enumerable: false, configurable: true })?;
        let tag = s.intrinsic(Intrinsic::SymbolToStringTag);
        let t = s.string("Notification")?;
        s.define(p, tag, t, Attrs { writable: false, enumerable: false, configurable: true })?;
        Ok(obj_id::<E>(s, f).expect("a function"))
    })?;
    let getters: [(&str, HS<E>); 12] = [
        ("title", title),
        ("dir", dir_get),
        ("lang", lang_get),
        ("body", body),
        ("tag", tag_get),
        ("icon", icon_get),
        ("badge", badge_get),
        ("data", data_get),
        ("silent", silent_get),
        ("renotify", renotify_get),
        ("requireInteraction", require_get),
        ("timestamp", timestamp_get),
    ];
    for (name, h) in getters {
        d.def_get_s(it, proto, name, h, None)?;
    }
    d.def_method_s(it, proto, "close", 0, close)?;
    let handlers: [(HS<E>, HS<E>); 4] = [(hget::<0, E>, hset::<0, E>), (hget::<1, E>, hset::<1, E>), (hget::<2, E>, hset::<2, E>), (hget::<3, E>, hset::<3, E>)];
    for (ty, (g, st)) in HANDLERS.iter().zip(handlers) {
        d.def_get_s(it, proto, &format!("on{ty}"), g, Some(st))?;
    }
    d.def_get_s(it, f, "permission", permission, None)?;
    d.def_get_s(it, f, "maxActions", max_actions, None)?;
    d.def_method_s(it, f, "requestPermission", 0, request_permission)?;
    d.notification.made.set(Some([proto, f]));
    Ok(f)
}

/// `new Notification(title, options)`: the options as Chrome reads them (`NotificationOptions`, members in name order).
fn ctor<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    const WHO: &str = "Failed to construct 'Notification'";
    let default = d.notification.made.get().expect("made")[0];
    let default = d.obj_local(s, default);
    let proto = s.proto_from_new_target(default)?;
    let proto = obj_id::<E>(s, proto).expect("a prototype is an object");
    let Some(a0) = a.get(0) else {
        return Err(s.type_error(&format!("{WHO}: 1 argument required, but only 0 present.")));
    };
    let title = d.string_arg_s(s, a0)?;
    let opts = arg_s::<E>(s, a, 1);
    let null = s.null();
    let mut n = Note::<E> { id: 0, title, body: String::new(), tag: String::new(), icon: String::new(), badge: String::new(), lang: String::new(), dir: "auto", data: s.engine().raw(null), silent: None, renotify: false, require: false, timestamp: s.wall_now_ms().trunc(), granted: false, state: State::Pending };
    let mut data = None;
    let mut vibrate = false;
    match s.type_of(opts) {
        Type::Undefined | Type::Null => {}
        Type::Object | Type::Function => {
            n.badge = url_member(d, s, opts, "badge")?;
            n.body = text_member(d, s, opts, "body")?;
            let v = s.get_str(opts, "data")?;
            if s.type_of(v) != Type::Undefined {
                data = Some(v);
            }
            let v = s.get_str(opts, "dir")?;
            if s.type_of(v) != Type::Undefined {
                let t = d.string_arg_s(s, v)?;
                n.dir = match t.as_str() {
                    "auto" => "auto",
                    "ltr" => "ltr",
                    "rtl" => "rtl",
                    _ => return Err(s.type_error(&format!("{WHO}: Failed to read the 'dir' property from 'NotificationOptions': The provided value '{t}' is not a valid enum value of type NotificationDirection."))),
                };
            }
            n.icon = url_member(d, s, opts, "icon")?;
            n.lang = text_member(d, s, opts, "lang")?;
            let v = s.get_str(opts, "renotify")?;
            n.renotify = s.to_boolean(v);
            let v = s.get_str(opts, "requireInteraction")?;
            n.require = s.to_boolean(v);
            let v = s.get_str(opts, "silent")?;
            n.silent = (!matches!(s.type_of(v), Type::Undefined | Type::Null)).then(|| s.to_boolean(v));
            n.tag = text_member(d, s, opts, "tag")?;
            let v = s.get_str(opts, "timestamp")?;
            if s.type_of(v) != Type::Undefined {
                let t = s.to_number(v)?;
                n.timestamp = if t.is_finite() { t.trunc().max(0.0) } else { 0.0 };
            }
            let v = s.get_str(opts, "vibrate")?;
            vibrate = s.type_of(v) != Type::Undefined;
        }
        _ => return Err(s.type_error(&format!("{WHO}: parameter 2 is not of type 'NotificationOptions'."))),
    }
    if n.silent == Some(true) && vibrate {
        return Err(s.type_error(&format!("{WHO}: Silent notifications must not specify vibration patterns.")));
    }
    if n.renotify && n.tag.is_empty() {
        return Err(s.type_error(&format!("{WHO}: Notifications which set the renotify flag must specify a non-empty tag.")));
    }
    if let Some(v) = data {
        let c = crate::browser_navigation_state::clone_with(d, s, v, &[])?.0;
        n.data = s.engine().raw(c);
    }
    n.granted = state(d, "notifications") == "granted";
    old_v::<E>(s, |it| {
        let o = d.obj(it, proto)?;
        d.st.borrow_mut().add_target(it, o, Owner::Notification(o));
        d.notification.notes.borrow_mut().insert(o, n);
        d.schedule(d.now_ms(), Task::Notification(o))?;
        Ok(E::obj_raw(o))
    })
}

/// An option member as a string. A missing member is "".
fn text_member<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, opts: Local<'s>, key: &str) -> Res<'s, String> {
    let v = s.get_str(opts, key)?;
    if s.type_of(v) == Type::Undefined {
        return Ok(String::new());
    }
    d.string_arg_s(s, v)
}

/// An option member as a URL, resolved against the document. A URL that does not parse is "". A scheme that bare does not load
/// (`data:`) stays as given.
fn url_member<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, opts: Local<'s>, key: &str) -> Res<'s, String> {
    let t = text_member(d, s, opts, key)?;
    if t.is_empty() {
        return Ok(t);
    }
    let Ok(base) = d.cur_rec() else { return Ok(t) };
    Ok(match crate::browser_navigation_url::parse(&t, Some(&base)) {
        Ok(r) => r.href(),
        Err(crate::browser_navigation_url::UErr::Scheme) => t,
        Err(_) => String::new(),
    })
}

impl<E: Eng> Dom<E> {
    /// The task after `new Notification`, or after `close()`. Pending: show it with the grant (tag replacement, `show`), else fire
    /// `error` unless nobody listens. Closing: fire `close`.
    pub(crate) fn notification_failed(&self, it: &mut E::Vm, o: E::Obj) -> R<E, ()> {
        let h = &self.notification;
        let (state, granted) = match h.notes.borrow().get(&o) {
            Some(n) => (n.state, n.granted),
            None => return Ok(()),
        };
        let ty = match state {
            State::Pending if granted => {
                self.show_note(o);
                "show"
            }
            State::Pending => {
                h.notes.borrow_mut().get_mut(&o).expect("listed").state = State::Gone;
                "error"
            }
            State::Closing => {
                h.notes.borrow_mut().get_mut(&o).expect("listed").state = State::Gone;
                "close"
            }
            State::Shown | State::Gone => return Ok(()),
        };
        self.fire_note(it, o, ty, false).map(|_| ())
    }

    /// Put `o` in the list. An old notification with the same tag leaves the list with no event ("replaced").
    fn show_note(&self, o: E::Obj) {
        let h = &self.notification;
        let id = h.next_id.get() + 1;
        h.next_id.set(id);
        let mut notes = h.notes.borrow_mut();
        let tag = notes[&o].tag.clone();
        if !tag.is_empty() {
            let mut shown = h.shown.borrow_mut();
            if let Some(i) = shown.iter().position(|x| notes[x].tag == tag) {
                let old = shown.remove(i);
                let n = notes.get_mut(&old).expect("listed");
                n.state = State::Gone;
                h.events.borrow_mut().push(event(self, "replaced", n));
            }
        }
        let n = notes.get_mut(&o).expect("listed");
        (n.id, n.state) = (id, State::Shown);
        h.events.borrow_mut().push(event(self, "shown", n));
        h.shown.borrow_mut().push(o);
    }

    /// Fire a trusted event `ty` at the notification `o`, unless nobody listens. `cancelable` for `click`. Gives whether a listener
    /// cancelled it.
    pub(crate) fn fire_note(&self, it: &mut E::Vm, o: E::Obj, ty: &str, cancelable: bool) -> R<E, bool> {
        let listening = self.st.borrow().listeners.get(&Target::Obj(o)).is_some_and(|v| v.iter().any(|l| &*l.ty == ty));
        if !listening {
            return Ok(false);
        }
        let ev = self.new_event(it, ty, false, cancelable, false, None)?;
        self.st.borrow_mut().events.get_mut(&ev).expect("just made").trusted = true;
        self.dispatch(it, Target::Obj(o), ev).map(|ok| !ok)
    }

    fn note_of<'s>(&self, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, E::Obj> {
        match obj_id::<E>(s, this) {
            Some(o) if self.notification.notes.borrow().contains_key(&o) => Ok(o),
            _ => Err(s.type_error("Illegal invocation")),
        }
    }
}

/// The object of the live notification `id`.
fn live_note<E: Eng>(d: &Dom<E>, id: u64) -> Option<E::Obj> {
    let notes = d.notification.notes.borrow();
    d.notification.shown.borrow().iter().copied().find(|o| notes[o].id == id)
}

/// An event log entry for the notification `n` of this page.
fn event<E: Eng>(d: &Dom<E>, kind: &'static str, n: &Note<E>) -> NotificationEvent {
    NotificationEvent { kind, id: n.id, origin: crate::browser_basics::doc_origin(d), title: n.title.clone(), body: n.body.clone(), tag: n.tag.clone(), at_ms: n.timestamp }
}

fn note_text<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, pick: fn(&Note<E>) -> &str) -> Res<'s, Local<'s>> {
    let o = d.note_of(s, this)?;
    let text = pick(&d.notification.notes.borrow()[&o]).to_string();
    s.string(&text)
}

fn title<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    note_text(d, s, this, |n| &n.title)
}

fn dir_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    note_text(d, s, this, |n| n.dir)
}

fn lang_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    note_text(d, s, this, |n| &n.lang)
}

fn body<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    note_text(d, s, this, |n| &n.body)
}

fn tag_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    note_text(d, s, this, |n| &n.tag)
}

fn icon_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    note_text(d, s, this, |n| &n.icon)
}

fn badge_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    note_text(d, s, this, |n| &n.badge)
}

fn data_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.note_of(s, this)?;
    let v = d.notification.notes.borrow()[&o].data;
    Ok(s.engine().raw_local(&v))
}

fn silent_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.note_of(s, this)?;
    let v = d.notification.notes.borrow()[&o].silent;
    Ok(match v {
        Some(b) => s.boolean(b),
        None => s.null(),
    })
}

fn renotify_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.note_of(s, this)?;
    let v = d.notification.notes.borrow()[&o].renotify;
    Ok(s.boolean(v))
}

fn require_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.note_of(s, this)?;
    let v = d.notification.notes.borrow()[&o].require;
    Ok(s.boolean(v))
}

fn timestamp_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.note_of(s, this)?;
    let v = d.notification.notes.borrow()[&o].timestamp;
    Ok(s.number(v))
}

/// `close()`: a shown or pending notification leaves the list, and a task fires `close`. Else nothing.
fn close<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.note_of(s, this)?;
    let closed = old::<E, _>(s, |_| Ok(d.close_note(o, true)))?;
    if closed {
        lift::<E, _>(s, d.schedule(d.now_ms(), Task::Notification(o)))?;
    }
    Ok(s.undefined())
}

impl<E: Eng> Dom<E> {
    /// Take `o` out of the list and log "closed" (when it was shown). Gives whether a `close` event is due: the caller queues the task.
    /// `fire`: false when the user closed it from outside (`Browser::close_notification` fires at once).
    pub(crate) fn close_note(&self, o: E::Obj, fire: bool) -> bool {
        let h = &self.notification;
        let mut notes = h.notes.borrow_mut();
        let Some(n) = notes.get_mut(&o) else { return false };
        match n.state {
            State::Shown => {
                h.shown.borrow_mut().retain(|&x| x != o);
                h.events.borrow_mut().push(event(self, "closed", n));
            }
            State::Pending if n.granted => {}
            State::Pending | State::Closing | State::Gone => return false,
        }
        n.state = if fire { State::Closing } else { State::Gone };
        true
    }
}

fn hget<'s, const I: usize, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.note_of(s, this)?;
    old_v::<E>(s, |it| Ok(handler_get(d, it, Target::Obj(o), HANDLERS[I])))
}

fn hset<'s, const I: usize, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.note_of(s, this)?;
    let v = raw_arg::<E>(s, a, 0);
    old::<E, _>(s, |it| handler_set(d, it, Target::Obj(o), HANDLERS[I], v))?;
    Ok(s.undefined())
}

fn permission<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    s.string(state(d, "notifications"))
}

fn max_actions<'s, E: Eng>(_: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Ok(s.number(0.0))
}

/// The promise resolves at once with the state. A function callback gets it too, before the promise. With no decision of the
/// origin, the event log gets "permission-requested", so an agent sees that the page asked.
fn request_permission<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let origin = crate::browser_basics::doc_origin(d);
    if d.nav.s.borrow().store.permission(&origin, "notifications").is_none() {
        let at_ms = s.wall_now_ms();
        d.notification.events.borrow_mut().push(NotificationEvent { kind: "permission-requested", id: 0, origin, title: String::new(), body: String::new(), tag: String::new(), at_ms });
    }
    let p = s.new_promise()?;
    let v = s.string(state(d, "notifications"))?;
    if let Some(cb) = a.get(0).filter(|&cb| s.type_of(cb) == Type::Function) {
        let this = s.undefined();
        s.call(cb, this, &[v])?;
    }
    s.resolve_promise(p, v)?;
    Ok(p)
}

// ------------------------------------------------------------------------------------------ permissions

/// The state of permission `name` for the document: "granted" or "denied" for "notifications" (see the header), "prompt" else.
fn state<E: Eng>(d: &Dom<E>, name: &str) -> &'static str {
    if name != "notifications" {
        return "prompt";
    }
    let origin = crate::browser_basics::doc_origin(d);
    let granted = d.nav.s.borrow().store.permission(&origin, name) == Some(crate::browser_navigation::Permission::Granted);
    if granted && crate::browser_basics::secure(d) { "granted" } else { "denied" }
}

/// The getter `navigator.permissions`: one object, made on the first read.
pub(crate) fn permissions<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, Local<'s>> {
    let m = match d.notification.perms.get() {
        Some(m) => m,
        None => old::<E, _>(s, |it| make_perms(d, it))?,
    };
    Ok(d.obj_local(s, m[0]))
}

#[cold]
#[inline(never)]
fn make_perms<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, [E::Obj; 2]> {
    let et = d.p().et;
    let [pp, sp] = run::<E, _>(it, |s| {
        let made = [d.plain_s(s)?, d.obj_s(s, et)?];
        let tag = s.intrinsic(Intrinsic::SymbolToStringTag);
        for (o, name) in made.into_iter().zip(["Permissions", "PermissionStatus"]) {
            let o = d.obj_local(s, o);
            let t = s.string(name)?;
            s.define(o, tag, t, Attrs { writable: false, enumerable: false, configurable: true })?;
        }
        Ok(made)
    })?;
    d.def_method_s(it, pp, "query", 1, query)?;
    d.def_get_s(it, sp, "name", status_name, None)?;
    d.def_get_s(it, sp, "state", status_state, None)?;
    d.def_get_s(it, sp, "onchange", status_hget, Some(status_hset))?;
    let m = [d.obj(it, pp)?, sp];
    d.notification.perms.set(Some(m));
    Ok(m)
}

/// `query(descriptor)`: a promise of a new `PermissionStatus`. A bad descriptor rejects it with a `TypeError`.
fn query<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let p = s.new_promise()?;
    let r = query_status(d, s, this, a);
    crate::browser_web::settle::<E>(s, p, r)
}

fn query_status<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let [pp, sp] = d.notification.perms.get().expect("made");
    if obj_id::<E>(s, this) != Some(pp) {
        return Err(s.type_error("Illegal invocation"));
    }
    let desc = arg_s::<E>(s, a, 0);
    if !matches!(s.type_of(desc), Type::Object | Type::Function) {
        return Err(s.type_error("Failed to execute 'query' on 'Permissions': parameter 1 is not of type 'object'."));
    }
    let v = s.get_str(desc, "name")?;
    if s.type_of(v) == Type::Undefined {
        return Err(s.type_error("Failed to execute 'query' on 'Permissions': Failed to read the 'name' property from 'PermissionDescriptor': Required member is undefined."));
    }
    let name = d.string_arg_s(s, v)?;
    let Some(&name) = NAMES.iter().find(|n| **n == name) else {
        return Err(s.type_error(&format!("Failed to execute 'query' on 'Permissions': Failed to read the 'name' property from 'PermissionDescriptor': The provided value '{name}' is not a valid enum value of type PermissionName.")));
    };
    let st = state(d, name);
    old_v::<E>(s, |it| {
        let o = d.obj(it, sp)?;
        d.st.borrow_mut().add_target(it, o, Owner::Notification(o));
        let seq = d.notification.seq.get();
        d.notification.seq.set(seq + 1);
        d.notification.statuses.borrow_mut().insert(o, (name, st, seq));
        Ok(E::obj_raw(o))
    })
}

fn status_of<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, (E::Obj, (&'static str, &'static str, u64))> {
    match obj_id::<E>(s, this).and_then(|o| d.notification.statuses.borrow().get(&o).map(|&v| (o, v))) {
        Some(v) => Ok(v),
        None => Err(s.type_error("Illegal invocation")),
    }
}

fn status_name<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let (_, (name, ..)) = status_of(d, s, this)?;
    s.string(name)
}

fn status_state<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let (_, (_, st, _)) = status_of(d, s, this)?;
    s.string(st)
}

fn status_hget<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let (o, _) = status_of(d, s, this)?;
    old_v::<E>(s, |it| Ok(handler_get(d, it, Target::Obj(o), "change")))
}

fn status_hset<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (o, _) = status_of(d, s, this)?;
    let v = raw_arg::<E>(s, a, 0);
    old::<E, _>(s, |it| handler_set(d, it, Target::Obj(o), "change", v))?;
    Ok(s.undefined())
}

impl<E: Eng> crate::browser::Browser<E> {
    /// The live notifications of the page, oldest first.
    pub fn notifications(&self) -> Vec<NotificationInfo> {
        let h = &self.dom.notification;
        let origin = crate::browser_basics::doc_origin(&self.dom);
        let notes = h.notes.borrow();
        h.shown
            .borrow()
            .iter()
            .map(|o| {
                let n = &notes[o];
                NotificationInfo { id: n.id, origin: origin.clone(), title: n.title.clone(), body: n.body.clone(), tag: n.tag.clone(), icon: n.icon.clone(), silent: n.silent == Some(true), at_ms: n.timestamp }
            })
            .collect()
    }

    /// The event log since the last call (all of it: there is no cap).
    pub fn take_notification_events(&mut self) -> Vec<NotificationEvent> {
        std::mem::take(&mut *self.dom.notification.events.borrow_mut())
    }

    /// The number of entries in the event log, which this does not drain.
    pub fn notification_event_count(&self) -> usize {
        self.dom.notification.events.borrow().len()
    }

    /// The user clicks the live notification `id`: a trusted, cancelable `click` fires at it, and the log says "clicked". It stays in
    /// the list, as in Chrome. Gives whether a listener cancelled the click, or `None` for no live notification `id`.
    pub fn click_notification(&mut self, id: u64) -> R<E, Option<bool>> {
        let dom = self.dom.clone();
        let Some(o) = live_note(&dom, id) else { return Ok(None) };
        let ev = event(&dom, "clicked", &dom.notification.notes.borrow()[&o]);
        dom.notification.events.borrow_mut().push(ev);
        self.fire_from_host(&dom, o, "click", true).map(Some)
    }

    /// The user closes the live notification `id`: it leaves the list, the log says "closed" and `close` fires at once. Gives whether
    /// `id` was live.
    pub fn close_notification(&mut self, id: u64) -> R<E, bool> {
        let dom = self.dom.clone();
        let Some(o) = live_note(&dom, id) else { return Ok(false) };
        dom.close_note(o, false);
        self.fire_from_host(&dom, o, "close", false).map(|_| true)
    }

    /// Fire `ty` at the notification `o` as an input of the user, then run the microtasks. Gives whether a listener cancelled it.
    fn fire_from_host(&mut self, dom: &std::rc::Rc<Dom<E>>, o: E::Obj, ty: &str, cancelable: bool) -> R<E, bool> {
        let was = std::mem::replace(&mut dom.st.borrow_mut().driver, true);
        let r = self.it.host_scope_o(|it| {
            let c = dom.fire_note(it, o, ty, cancelable)?;
            it.host_checkpoint().map(|_| c)
        });
        dom.st.borrow_mut().driver = was;
        r
    }

    /// The notification objects that the page can still reach (for tests).
    #[cfg(test)]
    pub(crate) fn notification_objects(&self) -> usize {
        self.dom.notification.notes.borrow().len()
    }

    /// The origin of the document, serialized ("null" with no URL).
    pub fn document_origin(&self) -> String {
        crate::browser_basics::doc_origin(&self.dom)
    }

    /// Set (or with `None`, remove) the decision for permission `name` of `origin`, in the storage of the session (a profile saves
    /// it). A trusted `change` event fires at each live `PermissionStatus` of this page whose state moved, in creation order. Gives
    /// the number of events fired.
    pub fn set_permission(&mut self, origin: &str, name: &str, to: Option<crate::browser_navigation::Permission>) -> R<E, usize> {
        let dom = self.dom.clone();
        if !dom.nav.s.borrow().store.set_permission(origin, name, to) {
            return Ok(0);
        }
        let mut moved: Vec<(u64, E::Obj)> = Vec::new();
        for (&o, v) in dom.notification.statuses.borrow_mut().iter_mut() {
            let now = state(&dom, v.0);
            if now != v.1 {
                v.1 = now;
                moved.push((v.2, o));
            }
        }
        moved.sort_unstable_by_key(|x| x.0);
        let was = std::mem::replace(&mut dom.st.borrow_mut().driver, true);
        let mut r = Ok(());
        for &(_, o) in &moved {
            r = self.it.host_scope_o(|it| {
                let ev = dom.new_event(it, "change", false, false, false, None)?;
                dom.st.borrow_mut().events.get_mut(&ev).expect("just made").trusted = true;
                dom.dispatch(it, Target::Obj(o), ev)?;
                it.host_checkpoint().map(|_| ())
            });
            if r.is_err() {
                break;
            }
        }
        dom.st.borrow_mut().driver = was;
        r.map(|_| moved.len())
    }
}
