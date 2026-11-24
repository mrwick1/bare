//! `BroadcastChannel` (HTML Standard, "Broadcasting to other browsing contexts") inside one page.
//!
//! bare has one page per realm, so a message reaches the other open channels of the same name in this page only.
//! Microsoft Teams makes a channel in a class field of its telemetry service: without the interface the service is never made.
//!
//! - A channel is an `EventTarget` and also a port with no peer in the scheduler (`browser_scheduler.rs`), as the window is.
//!   `postMessage(message)` clones the message once for each other open channel of the name, in creation order, and queues a
//!   `message` task at it. The event has `origin` of the document and `source` null. The sender gets nothing.
//! - `close()` takes the channel out of the open list and drops its queued messages. `postMessage` after `close()` throws
//!   `InvalidStateError`. A second `close()` does nothing.
//! - An open channel with a listener is a root: a message can come to it without a reference from the page.
//! - `BroadcastChannel` is an accessor of the global until the first read, as `Worker` is (`browser_worker.rs`).
//! - Not here: channels of other pages and workers, `messageerror` (a clone in one heap does not fail on the receiver side).

use crate::browser::*;
use crate::browser_handlers::{handler_get, handler_set};
use crate::browser_scheduler::Sender;
use crate::browser_scoped::*;
use crate::browser_sources::Roots;
use crate::eng::Eng;
use bare_js_api::{Args, Attrs, Engine, HostFn, Intrinsic, Local};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const TYPES: [&str; 2] = ["message", "messageerror"];

#[derive(Default)]
pub(crate) struct BroadcastHost<E: Eng> {
    /// The prototype and the constructor, once made. Roots.
    made: Cell<Option<[<E as Eng>::Obj; 2]>>,
    /// The live channels with their name.
    names: RefCell<IdMap<<E as Eng>::Obj, Rc<str>>>,
    /// The open channels, oldest first.
    open: RefCell<Vec<<E as Eng>::Obj>>,
}

pub(crate) fn roots<E: Eng>(d: &Dom<E>, out: &mut Roots<'_, '_, E>) {
    if let Some(m) = d.broadcast.made.get() {
        out.extend(m.map(E::obj_raw));
    }
    let open = d.broadcast.open.borrow();
    if open.is_empty() {
        return;
    }
    let st = d.st.borrow();
    out.extend(open.iter().filter(|&&o| st.listeners.get(&Target::Obj(o)).is_some_and(|v| v.iter().any(|l| &*l.ty == "message"))).map(|&o| E::obj_raw(o)));
}

/// The channel `o` is dead: drop its entries. The port entry goes with `Owner::Port`.
pub(crate) fn forget<E: Eng>(d: &Dom<E>, o: E::Obj) {
    if d.broadcast.names.borrow_mut().remove(&o).is_some() {
        d.broadcast.open.borrow_mut().retain(|&c| c != o);
    }
}

pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    let g = s.global();
    let f = d.add_fn_s(s, lazy, "BroadcastChannel", 0, HostFn::Method)?;
    s.define_accessor_str(g, "BroadcastChannel", Some(f), Some(f), Attrs { writable: false, enumerable: false, configurable: true })
}

/// The accessor of the global until the first read. A call with an argument is the setter.
fn lazy<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let g = s.global();
    let attrs = Attrs { writable: true, enumerable: false, configurable: true };
    if let Some(v) = a.get(0) {
        s.define_str(g, "BroadcastChannel", v, attrs)?;
        return Ok(s.undefined());
    }
    let ctor = match d.broadcast.made.get() {
        Some(m) => m[1],
        None => old::<E, _>(s, |it| make(d, it))?,
    };
    let ctor = d.obj_local(s, ctor);
    s.define_str(g, "BroadcastChannel", ctor, attrs)?;
    Ok(ctor)
}

#[cold]
#[inline(never)]
fn make<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, E::Obj> {
    let proto = d.obj(it, d.p().et)?;
    let f = run::<E, _>(it, |s| {
        let f = d.add_fn_s(s, ctor, "BroadcastChannel", 1, HostFn::CtorNewTarget)?;
        let p = d.obj_local(s, proto);
        s.define_str(f, "prototype", p, Attrs { writable: false, enumerable: false, configurable: false })?;
        s.define_str(p, "constructor", f, Attrs { writable: true, enumerable: false, configurable: true })?;
        let tag = s.intrinsic(Intrinsic::SymbolToStringTag);
        let t = s.string("BroadcastChannel")?;
        s.define(p, tag, t, Attrs { writable: false, enumerable: false, configurable: true })?;
        Ok(obj_id::<E>(s, f).expect("a function"))
    })?;
    d.def_get_s(it, proto, "name", name, None)?;
    d.def_method_s(it, proto, "postMessage", 1, post_message)?;
    d.def_method_s(it, proto, "close", 0, close)?;
    d.def_get_s(it, proto, "onmessage", hget::<0, _>, Some(hset::<0, _>))?;
    d.def_get_s(it, proto, "onmessageerror", hget::<1, _>, Some(hset::<1, _>))?;
    d.broadcast.made.set(Some([proto, f]));
    Ok(f)
}

fn ctor<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let default = d.broadcast.made.get().expect("made")[0];
    let default = d.obj_local(s, default);
    let proto = s.proto_from_new_target(default)?;
    let proto = obj_id::<E>(s, proto).expect("a prototype is an object");
    let Some(a0) = a.get(0) else {
        return Err(s.type_error("Failed to construct 'BroadcastChannel': 1 argument required, but only 0 present."));
    };
    let name: Rc<str> = d.string_arg_s(s, a0)?.into();
    if d.st.borrow().by_obj.len() >= d.lim.max_wrappers {
        return Err(d.lim_s(s, "dom-wrappers"));
    }
    old_v::<E>(s, |it| {
        let o = d.obj(it, proto)?;
        d.add_broadcast_port(o)?;
        d.st.borrow_mut().add_target(it, o, Owner::Port(o));
        d.broadcast.names.borrow_mut().insert(o, name);
        d.broadcast.open.borrow_mut().push(o);
        Ok(E::obj_raw(o))
    })
}

impl<E: Eng> Dom<E> {
    fn channel_of<'s>(&self, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, (E::Obj, Rc<str>)> {
        let o = obj_id::<E>(s, this);
        match o.and_then(|o| Some((o, self.broadcast.names.borrow().get(&o)?.clone()))) {
            Some(c) => Ok(c),
            None => Err(s.type_error("Illegal invocation")),
        }
    }
}

fn name<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let (_, n) = d.channel_of(s, this)?;
    s.string(&n)
}

fn post_message<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (me, n) = d.channel_of(s, this)?;
    let Some(v) = a.get(0) else {
        return Err(s.type_error("Failed to execute 'postMessage' on 'BroadcastChannel': 1 argument required, but only 0 present."));
    };
    if !d.broadcast.open.borrow().contains(&me) {
        return Err(d.dom_err_s(s, "InvalidStateError", "Failed to execute 'postMessage' on 'BroadcastChannel': Channel is closed"));
    }
    let to: Vec<E::Obj> = {
        let names = d.broadcast.names.borrow();
        d.broadcast.open.borrow().iter().copied().filter(|&o| o != me && names.get(&o).is_some_and(|m| **m == *n)).collect()
    };
    if to.is_empty() {
        d.clone_payload(s, v, &[])?;
        return Ok(s.undefined());
    }
    let origin = d.cur_rec().ok().map_or_else(|| "null".to_string(), |c| c.origin());
    for o in to {
        let (data, bytes, _) = d.clone_payload(s, v, &[])?;
        let data = s.engine().raw(data);
        let from = Some(Box::new(Sender { origin: origin.clone(), source: None }));
        lift::<E, _>(s, d.queue_port(o, data, Box::default(), bytes, from))?;
    }
    Ok(s.undefined())
}

fn close<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let (me, _) = d.channel_of(s, this)?;
    let mut open = d.broadcast.open.borrow_mut();
    if let Some(i) = open.iter().position(|&o| o == me) {
        open.remove(i);
        drop(open);
        d.sched_forget_port(me);
        d.sched_after_ports();
    }
    Ok(s.undefined())
}

fn hget<'s, const N: usize, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let (o, _) = d.channel_of(s, this)?;
    old_v::<E>(s, |it| Ok(handler_get(d, it, Target::Obj(o), TYPES[N])))
}

fn hset<'s, const N: usize, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (o, _) = d.channel_of(s, this)?;
    let v = raw_arg::<E>(s, a, 0);
    old::<E, _>(s, |it| handler_set(d, it, Target::Obj(o), TYPES[N], v))?;
    Ok(s.undefined())
}
