//! `Notification` (Notifications API, "https://notifications.spec.whatwg.org/") where permission is "denied".
//!
//! bare cannot show a notification. Microsoft Teams makes one and reads `Notification.permission`, so the global must exist and say
//! "denied". The constructor returns the object, and a task after the current one fires a plain `error` event at it, as the spec says for
//! a denied permission. See `browser_worker.rs`, which does the same for a worker that cannot start.
//!
//! - `Notification` is an accessor of the global until the first read, as `Worker` is (`browser_worker.rs`).
//! - `new Notification(title[, options])` reads the title (required) and `options.body` and `options.tag` (default ""). `title`,
//!   `body`, `tag` and `close()` read the object. `close()` does nothing.
//! - `Notification.permission` is "denied", `Notification.maxActions` is 0. `requestPermission([callback])` returns a resolved promise
//!   of "denied". A function callback gets "denied" at once, not in a later task. A value that is not a function is ignored.
//! - The notification is an `EventTarget` (a `Target::Obj`) with `onerror`. Its `error` task is `Task::Notification`, which roots the
//!   object. No event object is made when nobody listens. A collected notification drops its entry (`forget`).
//! - Not here: `show` and `onshow`, `onclick` and the other handlers, `ServiceWorkerRegistration.showNotification`, `actions`, `icon`.

use crate::browser::*;
use crate::browser_handlers::{handler_get, handler_set};
use crate::browser_scheduler::Task;
use crate::browser_scoped::*;
use crate::browser_sources::Roots;
use crate::eng::Eng;
use bare_js_api::{Args, Attrs, HostFn, Intrinsic, Local, Type};
use std::cell::{Cell, RefCell};

const PERMISSION: &str = "denied";

#[derive(Default)]
pub(crate) struct NotificationHost<E: Eng> {
    /// The live notifications, by object.
    notes: RefCell<IdMap<<E as Eng>::Obj, Note>>,
    /// The prototype and the constructor, once made. Roots.
    made: Cell<Option<[<E as Eng>::Obj; 2]>>,
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
}

/// The notification `o` is dead: drop its entry.
pub(crate) fn forget<E: Eng>(d: &Dom<E>, o: E::Obj) {
    d.notification.notes.borrow_mut().remove(&o);
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
    if d.st.borrow().by_obj.len() >= d.lim.max_wrappers {
        return Err(d.lim_s(s, "dom-wrappers"));
    }
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

fn permission<'s, E: Eng>(_: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    s.string(PERMISSION)
}

fn max_actions<'s, E: Eng>(_: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Ok(s.number(0.0))
}

/// The promise resolves at once with "denied". A function callback gets "denied" too, before the promise.
fn request_permission<'s, E: Eng>(_: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let p = s.new_promise()?;
    let v = s.string(PERMISSION)?;
    if let Some(cb) = a.get(0).filter(|&cb| s.type_of(cb) == Type::Function) {
        let this = s.undefined();
        s.call(cb, this, &[v])?;
    }
    s.resolve_promise(p, v)?;
    Ok(p)
}
