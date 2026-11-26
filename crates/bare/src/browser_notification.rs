//! `Notification` (Notifications API, "https://notifications.spec.whatwg.org/") and the permission of an origin.
//!
//! bare cannot show a notification yet. Microsoft Teams makes one and reads `Notification.permission`, so the global must exist. The
//! constructor returns the object, and a task after the current one fires a plain `error` event at it, as the spec says for a denied
//! permission. See `browser_worker.rs`, which does the same for a worker that cannot start.
//!
//! - `Notification` is an accessor of the global until the first read, as `Worker` is (`browser_worker.rs`).
//! - `new Notification(title[, options])` reads the title (required) and `options.body` and `options.tag` (default ""). `title`,
//!   `body`, `tag` and `close()` read the object. `close()` does nothing.
//! - Permission (`docs/internal/design-notifications.md`): the decision of each origin is in the `WebStorage` of the session
//!   (`WebStorage::permission`), set by `Browser::set_permission`. `Notification.permission` is "granted" only for a decision
//!   "granted" in a secure context. Else it is "denied": with no decision too (owner decision, 2026-11-26), so a page never sees
//!   "default". `requestPermission([callback])` shows no prompt: it gives the state. A function callback gets it at once, not in a later
//!   task. A value that is not a function is ignored. `Notification.maxActions` is 0.
//! - `navigator.permissions.query({name})` resolves a new `PermissionStatus` (an `EventTarget` with `name`, `state` and `onchange`).
//!   "notifications" has the state of `Notification.permission`; another known name is "prompt"; an unknown name rejects with a
//!   `TypeError`, as in Chrome. A status with a listener is a root. `Browser::set_permission` fires a trusted `change` at each live
//!   status whose state moved. There are no `Permissions` and `PermissionStatus` globals.
//! - The notification is an `EventTarget` (a `Target::Obj`) with `onerror`. Its `error` task is `Task::Notification`, which roots the
//!   object. No event object is made when nobody listens. A collected notification drops its entry (`forget`).
//! - Not here: `show` and `onshow`, `onclick` and the other handlers, `ServiceWorkerRegistration.showNotification`, `actions`, `icon`.

use crate::browser::*;
use crate::browser_handlers::{handler_get, handler_set};
use crate::browser_scheduler::Task;
use crate::browser_scoped::*;
use crate::browser_sources::Roots;
use crate::eng::{Eng, Ops};
use bare_js_api::{Args, Attrs, HostFn, Intrinsic, Local, Type};
use std::cell::{Cell, RefCell};

/// The permission names of Chrome's `PermissionName` enum that `query` accepts.
const NAMES: [&str; 22] = [
    "accelerometer", "ambient-light-sensor", "background-fetch", "background-sync", "camera", "clipboard-read", "clipboard-write",
    "display-capture", "geolocation", "gyroscope", "idle-detection", "local-fonts", "magnetometer", "microphone", "midi", "notifications",
    "payment-handler", "persistent-storage", "screen-wake-lock", "storage-access", "window-management", "push",
];

#[derive(Default)]
pub(crate) struct NotificationHost<E: Eng> {
    /// The live notifications, by object.
    notes: RefCell<IdMap<<E as Eng>::Obj, Note>>,
    /// The prototype and the constructor, once made. Roots.
    made: Cell<Option<[<E as Eng>::Obj; 2]>>,
    /// `navigator.permissions` and the `PermissionStatus` prototype, once made. Roots.
    perms: Cell<Option<[<E as Eng>::Obj; 2]>>,
    /// The live `PermissionStatus` objects: the name, the last state and the creation number.
    statuses: RefCell<IdMap<<E as Eng>::Obj, (&'static str, &'static str, u64)>>,
    /// The creation number of the next `PermissionStatus`.
    seq: Cell<u64>,
}

struct Note {
    title: String,
    body: String,
    tag: String,
}

pub(crate) fn roots<E: Eng>(d: &Dom<E>, out: &mut Roots<'_, '_, E>) {
    if let Some(m) = d.notification.made.get() {
        out.extend(m.map(E::obj_raw));
    }
    if let Some(m) = d.notification.perms.get() {
        out.extend(m.map(E::obj_raw));
    }
    let statuses = d.notification.statuses.borrow();
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
    d.def_get_s(it, proto, "title", title, None)?;
    d.def_get_s(it, proto, "body", body, None)?;
    d.def_get_s(it, proto, "tag", tag_get, None)?;
    d.def_method_s(it, proto, "close", 0, close)?;
    d.def_get_s(it, proto, "onerror", hget, Some(hset))?;
    d.def_get_s(it, f, "permission", permission, None)?;
    d.def_get_s(it, f, "maxActions", max_actions, None)?;
    d.def_method_s(it, f, "requestPermission", 0, request_permission)?;
    d.notification.made.set(Some([proto, f]));
    Ok(f)
}

fn ctor<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let default = d.notification.made.get().expect("made")[0];
    let default = d.obj_local(s, default);
    let proto = s.proto_from_new_target(default)?;
    let proto = obj_id::<E>(s, proto).expect("a prototype is an object");
    let Some(a0) = a.get(0) else {
        return Err(s.type_error("Failed to construct 'Notification': 1 argument required, but only 0 present."));
    };
    let title = d.string_arg_s(s, a0)?;
    let opts = arg_s::<E>(s, a, 1);
    let (body, tag) = match s.type_of(opts) {
        Type::Undefined | Type::Null => (String::new(), String::new()),
        Type::Object | Type::Function => (text_member(d, s, opts, "body")?, text_member(d, s, opts, "tag")?),
        _ => return Err(s.type_error("Failed to construct 'Notification': parameter 2 is not of type 'NotificationOptions'.")),
    };
    old_v::<E>(s, |it| {
        let o = d.obj(it, proto)?;
        d.st.borrow_mut().add_target(it, o, Owner::Notification(o));
        d.notification.notes.borrow_mut().insert(o, Note { title, body, tag });
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

impl<E: Eng> Dom<E> {
    /// The task after `new Notification`: fire `error` at `o` (a plain `Event`) unless nobody listens.
    pub(crate) fn notification_failed(&self, it: &mut E::Vm, o: E::Obj) -> R<E, ()> {
        if !self.notification.notes.borrow().contains_key(&o) {
            return Ok(());
        }
        let listening = self.st.borrow().listeners.get(&Target::Obj(o)).is_some_and(|v| v.iter().any(|l| &*l.ty == "error"));
        if listening {
            let ev = self.new_event(it, "error", false, false, false, None)?;
            self.st.borrow_mut().events.get_mut(&ev).expect("just made").trusted = true;
            self.dispatch(it, Target::Obj(o), ev)?;
        }
        Ok(())
    }

    fn note_of<'s>(&self, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, E::Obj> {
        match obj_id::<E>(s, this) {
            Some(o) if self.notification.notes.borrow().contains_key(&o) => Ok(o),
            _ => Err(s.type_error("Illegal invocation")),
        }
    }
}

fn note_text<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, pick: fn(&Note) -> &str) -> Res<'s, Local<'s>> {
    let o = d.note_of(s, this)?;
    let text = pick(&d.notification.notes.borrow()[&o]).to_string();
    s.string(&text)
}

fn title<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    note_text(d, s, this, |n| &n.title)
}

fn body<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    note_text(d, s, this, |n| &n.body)
}

fn tag_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    note_text(d, s, this, |n| &n.tag)
}

fn close<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    d.note_of(s, this)?;
    Ok(s.undefined())
}

fn hget<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.note_of(s, this)?;
    old_v::<E>(s, |it| Ok(handler_get(d, it, Target::Obj(o), "error")))
}

fn hset<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.note_of(s, this)?;
    let v = raw_arg::<E>(s, a, 0);
    old::<E, _>(s, |it| handler_set(d, it, Target::Obj(o), "error", v))?;
    Ok(s.undefined())
}

fn permission<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    s.string(state(d, "notifications"))
}

fn max_actions<'s, E: Eng>(_: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Ok(s.number(0.0))
}

/// The promise resolves at once with the state. A function callback gets it too, before the promise.
fn request_permission<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
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
