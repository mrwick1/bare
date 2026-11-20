//! `RemoteWindow`: the window of another page, as a page sees it across the origin line (HTML "cross-origin window").
//!
//! A page has no object of its child page, and the child has none of its parent (`browser_child.rs`: one `Browser`, one heap each). So
//! `iframe.contentWindow` of a child page frame, and `window.parent` and `window.top` in a child page, are a `Proxy` in the heap of the page
//! that reads them. The proxy has a null-prototype target that holds the cross-origin window API only:
//! `window`, `self`, `frames`, `length`, `top`, `parent`, `opener`, `closed`, `postMessage`, `blur`, `focus`, `close`, `location`.
//! Any other read, write, `in`, define or delete throws `SecurityError` (a `DOMException`). One window object is made for each child
//! and each side; it stays the same object while the frame lives.
//!
//! - The five traps (`get`, `set`, `has`, `defineProperty`, `deleteProperty`) are host functions that are shared by all the windows of
//!   a realm. A trap finds its window by the target (`Kids::win_of`).
//! - `postMessage` puts the message in the outbox of the page (`Kids::send`). The driver moves it to the other page, which checks the
//!   target origin and queues a `message` event (`browser_child.rs`, "Messages"). A `MessagePort` in the transfer list crosses as an end
//!   of the port hub (`Dom::export_ports`, `browser_child.rs` "Ports"). `blur`, `focus` and `close` do nothing yet.
//! - GC: each window entry of `Kids::wins` holds persistent handles of its objects (target, proxy, `location` object), and `Kids` holds the
//!   shared functions of `Common` the same way. `Kids::sync` drops the entries of a child that left, and so frees their handles. There is no
//!   hook root for windows.
//! - `length` is 0: the child frames of a child are not counted yet.
//! - `location`: a write to `iframe.contentWindow.location` navigates the child. A read gives an object with `replace(url)` and a `href`
//!   setter (the getter throws `SecurityError`). In a child, `parent.location = url` does nothing yet: the parent is not reachable
//!   from the heap of the child except by a message (3b).
//! - `top` and `parent` of a window of a child, read in the parent: the global of the parent, and its own `top`. In a child page the
//!   parent window and the top window are one object when the child is a frame of the top page. In a deeper child, the `parent` of
//!   the parent window is taken as the top window (the exact grandparent needs 3b).
use crate::browser::*;
use crate::browser_scoped::*;
use crate::browser_child::{FrameId, Out};
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};
use bare_js_api::{Args, Engine as _, HostFn, Local};

/// Which page a window stands for, seen from the page that holds it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Side {
    /// The child page `FrameId` of the page that holds the window.
    Child(FrameId),
    /// The page that embeds the page that holds the window.
    Parent,
    /// The top page.
    Top,
}

/// What a trap knows about its target.
#[derive(Clone, Copy)]
pub(crate) struct Win {
    pub side: Side,
    pub proxy: ObjId,
    /// The target is the `location` object of the window, not the window.
    pub location: bool,
}

/// The host functions that all windows of one realm share.
#[derive(Clone, Copy)]
pub(crate) struct Common {
    pub handler: ObjId,
    /// `Proxy.revocable`: only the engine's own proxy creation prepares it to find the traps (`Heap::new_proxy` alone does not).
    /// A host call cannot `new Proxy`, because the host check of a constructor refuses it.
    pub ctor: ObjId,
    pub no_op: [ObjId; 4],
    pub closed: ObjId,
    pub replace: ObjId,
    pub href_get: ObjId,
    pub href_set: ObjId,
}

const API: [&str; 13] = ["window", "self", "frames", "length", "top", "parent", "opener", "closed", "postMessage", "blur", "focus", "close", "location"];

fn text(s: &Sc<'_, '_>, v: Local<'_>) -> Option<String> {
    s.as_str(v).map(|t| t.to_string_lossy())
}

fn blocked<'s>(d: &Dom, s: &mut Sc<'s, '_>, verb: &str, key: &str) -> bare_js_api::Error<'s> {
    let origin = d.cur_rec().map_or_else(|_| "null".to_string(), |r| r.origin());
    d.dom_err_s(s, "SecurityError", &format!("Failed to {verb} a named property '{key}' {} 'Window': Blocked a frame with origin \"{origin}\" from accessing a cross-origin frame.", if verb == "read" { "from" } else { "on" }))
}

fn key_text(s: &Sc<'_, '_>, v: Local<'_>) -> String {
    text(s, v).unwrap_or_default()
}

fn trap_get<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (target, k) = (arg_s(s, a, 0), arg_s(s, a, 1));
    // A symbol key (`Symbol.toPrimitive`, `Symbol.toStringTag`) and `then` read as `undefined`, as in a browser.
    let Some(name) = text(s, k) else { return Ok(s.undefined()) };
    if name == "then" {
        return Ok(s.undefined());
    }
    if !API.contains(&name.as_str()) {
        return Err(blocked(d, s, "read", &name));
    }
    s.get(target, k)
}

fn trap_set<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (target, k, v) = (arg_s(s, a, 0), arg_s(s, a, 1), arg_s(s, a, 2));
    let name = key_text(s, k);
    let win = obj_id(s, target).and_then(|t| d.kids.win_of(t));
    match (name.as_str(), win) {
        ("location", Some(w)) if !w.location => {
            let url = d.string_arg_s(s, v)?;
            if let Side::Child(id) = w.side {
                d.kids.navigate(d, id, &url);
            }
            Ok(s.boolean(true))
        }
        _ => Err(blocked(d, s, "set", &name)),
    }
}

fn trap_has<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let k = arg_s(s, a, 1);
    match text(s, k) {
        Some(name) if !API.contains(&name.as_str()) => Err(blocked(d, s, "read", &name)),
        Some(_) => Ok(s.boolean(true)),
        None => Ok(s.boolean(false)),
    }
}

fn trap_define<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let k = arg_s(s, a, 1);
    let name = key_text(s, k);
    Err(blocked(d, s, "define", &name))
}

fn trap_delete<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let k = arg_s(s, a, 1);
    let name = key_text(s, k);
    Err(blocked(d, s, "delete", &name))
}

fn win_closed<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let Some(t) = obj_id(s, this) else { return Ok(s.boolean(true)) };
    let closed = match d.kids.win_of(t).map(|w| w.side) {
        Some(Side::Child(id)) => !d.kids.alive(id),
        _ => false,
    };
    Ok(s.boolean(closed))
}

/// `postMessage` of a remote window: the message goes to the outbox of this page. The driver routes it (`Browser::kids_turn`).
fn win_post<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let win = obj_id(s, this).and_then(|p| d.kids.target_of(p)).and_then(|t| d.kids.win_of(t));
    let Some(Win { side: to, .. }) = win else { return Err(s.type_error("Illegal invocation")) };
    // The message walk and the port export take the old engine.
    let args: Vec<Value> = a.iter().map(|l| s.engine().raw(l)).collect();
    old(s, |it| post(d, it, to, &args))?;
    Ok(s.undefined())
}

fn post(d: &Dom, it: &mut Interp<'_>, to: Side, a: &[Value]) -> R<()> {
    let (target, list) = crate::browser_basics::post_args(d, it, a)?;
    let origin = d.window_origin();
    let target_origin = match crate::browser_basics::target_origin(it, d, &target)? {
        Some(o) => o,
        None if target == "*" => target,
        None => origin.clone(),
    };
    if matches!(to, Side::Child(id) if !d.kids.alive(id)) {
        return Ok(());
    }
    if d.kids.outbox_len() >= d.max_messages() {
        return Err(d.lim("messages"));
    }
    let mut cloned = crate::browser_navigation_state::clone_state_with(d, it, a[0], &list)?;
    d.export_ports(it, &mut cloned)?;
    d.kids.send(Out { to, target_origin, origin, cloned });
    Ok(())
}

fn loc_target<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, Win> {
    match obj_id(s, this).and_then(|t| d.kids.win_of(t)).filter(|w| w.location) {
        Some(w) => Ok(w),
        None => Err(s.type_error("Illegal invocation")),
    }
}

fn loc_replace<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let w = loc_target(d, s, this)?;
    let url = d.arg_str_s(s, a, 0)?;
    if let Side::Child(id) = w.side {
        d.kids.navigate(d, id, &url);
    }
    Ok(s.undefined())
}

fn loc_href_get<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Err(blocked(d, s, "read", "href"))
}

fn loc_href_set<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    loc_replace(d, s, this, a)
}

fn common(d: &Dom, it: &mut Interp<'_>) -> R<Common> {
    if let Some(c) = d.kids.common.get() {
        return Ok(c);
    }
    let handler = it.heap.new_object(None).map_err(|e| it.heap_error(e))?;
    let traps: [(&str, HS); 5] = [("get", trap_get), ("set", trap_set), ("has", trap_has), ("defineProperty", trap_define), ("deleteProperty", trap_delete)];
    for (name, h) in traps {
        let f = d.add_fn_s_obj(it, h, name, 3, HostFn::Method)?;
        let k = d.key(it, name)?;
        it.define_data(handler, k, Value::Object(f), true, false, true)?;
    }
    let mut no_op = [handler; 4];
    for (slot, name) in no_op.iter_mut().zip(["postMessage", "blur", "focus", "close"]) {
        *slot = if name == "postMessage" { d.add_fn_s_obj(it, win_post, name, 1, HostFn::Method)? } else { d.add_fn_s_obj(it, |_, s, _, _| Ok(s.undefined()), name, 0, HostFn::Method)? };
    }
    let closed = d.add_fn_s_obj(it, win_closed, "get closed", 0, HostFn::Method)?;
    let replace = d.add_fn_s_obj(it, loc_replace, "replace", 1, HostFn::Method)?;
    let href_get = d.add_fn_s_obj(it, loc_href_get, "get href", 0, HostFn::Method)?;
    let href_set = d.add_fn_s_obj(it, loc_href_set, "set href", 1, HostFn::Method)?;
    let g = Value::Object(it.global_object());
    let kp = d.key(it, "Proxy")?;
    let px = it.get_value(g, kp)?;
    let kr = d.key(it, "revocable")?;
    let Value::Object(ctor) = it.get_value(px, kr)? else { return Err(it.type_error("Proxy is not available")) };
    let c = Common { handler, ctor, no_op, closed, replace, href_get, href_set };
    d.kids.set_common(it, c);
    Ok(c)
}

/// The window object for `side` in this realm: a new proxy over a fresh target. `top` and `parent` are the values for the properties of the
/// same names (`None`: the window itself).
pub(crate) fn make(d: &Dom, it: &mut Interp<'_>, side: Side, top: Option<Value>, parent: Option<Value>) -> R<ObjId> {
    let c = common(d, it)?;
    let target = it.heap.new_object(None).map_err(|e| it.heap_error(e))?;
    let made = it.call(Value::Object(c.ctor), Value::Undefined, &[Value::Object(target), Value::Object(c.handler)])?;
    let kp = d.key(it, "proxy")?;
    let Value::Object(proxy) = it.get_value(made, kp)? else { return Err(it.type_error("Proxy failed")) };
    let me = Value::Object(proxy);
    for (name, v) in [("window", me), ("self", me), ("frames", me), ("length", Value::Number(0.0)), ("top", top.unwrap_or(me)), ("parent", parent.unwrap_or(me)), ("opener", Value::Null)] {
        let k = d.key(it, name)?;
        it.define_data(target, k, v, false, true, true)?;
    }
    for (name, f) in ["postMessage", "blur", "focus", "close"].into_iter().zip(c.no_op) {
        let k = d.key(it, name)?;
        it.define_data(target, k, Value::Object(f), true, true, true)?;
    }
    let k = d.key(it, "closed")?;
    d.define_accessor(it, target, k, c.closed, None)?;
    // The `location` object, with its own target so that its functions find the window.
    let loc = it.heap.new_object(None).map_err(|e| it.heap_error(e))?;
    let k = d.key(it, "replace")?;
    it.define_data(loc, k, Value::Object(c.replace), true, true, true)?;
    let k = d.key(it, "href")?;
    d.define_accessor(it, loc, k, c.href_get, Some(c.href_set))?;
    let k = d.key(it, "location")?;
    it.define_data(target, k, Value::Object(loc), true, true, true)?;
    d.kids.add_win(it, target, Win { side, proxy, location: false });
    d.kids.add_win(it, loc, Win { side, proxy, location: true });
    Ok(proxy)
}

/// In a child page: `window.parent` and `window.top` are windows of the page that embeds it, and of the top page. They are one object for
/// a frame of the top page.
pub(crate) fn install_up(d: &Dom, it: &mut Interp<'_>, depth: u8) -> R<()> {
    let parent = make(d, it, Side::Parent, None, None)?;
    let top = if depth <= 1 { parent } else { make(d, it, Side::Top, None, None)? };
    if depth > 1 {
        // The parent window of a deeper child reports the top window as its `parent`.
        let k = d.key(it, "parent")?;
        if let Some(t) = d.kids.target_of(parent) {
            it.define_data(t, k, Value::Object(top), false, true, true)?;
        }
    }
    let g = it.global_object();
    let (kp, kt) = (d.key(it, "parent")?, d.key(it, "top")?);
    it.define_data(g, kp, Value::Object(parent), true, true, true)?;
    it.define_data(g, kt, Value::Object(top), false, true, false)?;
    Ok(())
}

impl Dom {
    /// The window of the child page `id` of this page, in this heap. The same object each time while the frame lives.
    pub(crate) fn child_window(&self, it: &mut Interp<'_>, id: FrameId) -> R<Value> {
        if let Some(w) = self.kids.window_of(id) {
            return Ok(Value::Object(w));
        }
        let g = Value::Object(it.global_object());
        let kt = self.key(it, "top")?;
        let top = it.get_value(g, kt)?;
        let w = make(self, it, Side::Child(id), Some(top), Some(g))?;
        self.kids.set_window(id, w);
        Ok(Value::Object(w))
    }
}
