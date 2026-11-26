//! `Worker` and `SharedWorker` (HTML Standard, "Workers").
//!
//! A dedicated worker runs. `new Worker(url)` in a page with a driver makes a worker page (`Of::Worker` in `browser_child.rs`): a
//! child page with its own `Browser` and heap. It runs on the page thread (rule D1 of `design-gmail-frames.md`), or on its own thread
//! when the page is cross-origin isolated (`browser_child_thread.rs`). Its document is one script element that loads the worker
//! script, so the rules of a script load apply. `install_scope` gives the worker global `postMessage`, `close`, `importScripts` and the
//! prototypes `WorkerGlobalScope` and `DedicatedWorkerGlobalScope`. Messages and ports cross heaps as clones (`Kids::send`).
//!
//! A worker that cannot run gets the behavior of a browser whose worker script did not load: the constructor returns the object,
//! and a task after the current one fires a simple `error` event at it. That happens when the page has no driver, when the bounds of
//! child pages are reached, and when the script does not load. `SharedWorker` never runs: it always takes this path.
//!
//! - The constructor reads its arguments in the WebIDL order: the URL (a string; `new URL(..)` works), then the options (`type` is
//!   `classic` or `module`, else `TypeError`; `name` and `credentials` are ignored). Then it parses the URL against the document: a
//!   failure is a `SyntaxError`. A `data:` URL is taken. A `blob:` or `http(s):` URL must have the origin of the document, and any
//!   other scheme fails, both with a `SecurityError`. At most `MAX_WORKERS` worker objects live in a page (a collected one frees its
//!   place); the next one throws `NotSupportedError`.
//! - A worker is an `EventTarget` (a `Target::Obj`, so the listener code and the GC rules are the common ones) with `onmessage`,
//!   `onmessageerror` and `onerror`. `postMessage(message[, transfer])` sends a structured clone to a running worker. A worker that
//!   cannot run sends nothing: it checks only that one argument is given and that it is not a function or a symbol
//!   (`DataCloneError`).
//! - `terminate()` ends the worker page, or stops the queued `error`. A second call does nothing.
//! - The `error` task is `Task::Worker`; a queued task roots its worker. A worker with a live page is a root too. No event object is
//!   made when nobody listens.
//! - The first worker of a page that cannot run logs one console warning, `Worker scripts do not run in bare (<url>)`.
//! - `SharedWorker` is made from the same host record and the same cap. Its constructor takes the URL and then a name (a string) or
//!   the options (`type`, `name`; `credentials` and `extraInfo` are ignored). The object has `onerror` and a read-only `port`: a
//!   `MessagePort` with no peer, that takes messages and answers none. It has no `postMessage` and no `terminate`. A page that tests
//!   `window.SharedWorker` to choose its code (craigslist does) takes the path that a browser takes. The first shared worker of a page
//!   logs `SharedWorker scripts do not run in bare (<url>)`.
//! - `Worker` is an accessor of the global until the first read, as `AbortController` is (`browser_abort.rs`): a realm that never reads
//!   it makes no interface.
//! - Not here: `ServiceWorker`, `ErrorEvent` for the `error` event.

use crate::eng::Eng;
use crate::browser_sources::Roots;
use crate::browser::*;
use crate::browser_console::ConsoleLevel;
use crate::browser_handlers::{handler_get, handler_set};
use crate::browser_navigation_url as nu;
use crate::browser_scoped::*;
use crate::browser_child::Out;
use crate::browser_remote_window::Side;
use bare_js_api::{Args, Attrs, HostFn, Intrinsic, Local, Type};
use std::cell::{Cell, RefCell};

/// The most worker objects that a page keeps alive.
const MAX_WORKERS: usize = 64;
/// The most characters of a URL in the console warning.
const SHOWN_URL: usize = 200;

const TYPES: [&str; 3] = ["message", "messageerror", "error"];

#[derive(Default)]
pub(crate) struct WorkerHost<E: Eng> {
    /// The live workers, each with its pending `error`: true until the task ran or `terminate()` was called.
    objs: RefCell<IdMap<<E as Eng>::Obj, bool>>,
    warned: Cell<bool>,
    warned_shared: Cell<bool>,
    /// The prototype and the constructor, once made. Roots.
    made: Cell<Option<[<E as Eng>::Obj; 2]>>,
    /// The same for `SharedWorker`.
    made_shared: Cell<Option<[<E as Eng>::Obj; 2]>>,
    /// The live shared workers (they are also in `objs`) with their `port`. The ports are roots; `prune` drops a port with its worker.
    ports: RefCell<IdMap<<E as Eng>::Obj, <E as Eng>::Obj>>,
}

pub(crate) fn roots<E: Eng>(d: &Dom<E>, out: &mut Roots<'_, '_, E>) {
    for m in [d.worker.made.get(), d.worker.made_shared.get()].into_iter().flatten() {
        out.extend(m.map(E::obj_raw));
    }
    let mut live = Vec::new();
    d.kids.workers(&mut live);
    out.extend(live.into_iter().map(E::obj_raw));
    out.extend(d.worker.ports.borrow().values().map(|&p| E::obj_raw(p)));
}

/// The worker `o` is dead: drop its entries and its port.
pub(crate) fn forget<E: Eng>(d: &Dom<E>, o: E::Obj) {
    d.worker.objs.borrow_mut().remove(&o);
    d.worker.ports.borrow_mut().remove(&o);
    // The messages that a worker page queued at its object.
    d.sched_forget_port(o);
}

/// The global of a dedicated worker page: `postMessage(message, transfer)` sends to the `Worker` object, `close()` ends the page, and
/// `importScripts(urls…)` runs scripts. The rest of the worker global (no window names, the worker prototype chain) is a later step
/// of `design-workers.md`.
pub(crate) fn install_scope<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    install_scope_named(d, it, "DedicatedWorkerGlobalScope", true)
}

/// The global of a worker page whose scope interface is `name`. `dedicated`: it has `postMessage` and `close()`. A service worker
/// (`browser_sw.rs`) has neither.
pub(crate) fn install_scope_named<E: Eng>(d: &Dom<E>, it: &mut E::Vm, name: &'static str, dedicated: bool) -> R<E, ()> {
    run::<E, _>(it, |s| {
        let g = s.global();
        let attrs = Attrs { writable: true, enumerable: true, configurable: true };
        if dedicated {
            let post = d.add_fn_s(s, scope_post, "postMessage", 1, HostFn::Method)?;
            s.define_str(g, "postMessage", post, attrs)?;
            let close = d.add_fn_s(s, scope_close, "close", 0, HostFn::Method)?;
            s.define_str(g, "close", close, attrs)?;
        }
        let import = d.add_fn_s(s, scope_import, "importScripts", 0, HostFn::Method)?;
        s.define_str(g, "importScripts", import, attrs)?;
        // The chain is `self -> DedicatedWorkerGlobalScope.prototype -> WorkerGlobalScope.prototype -> Window.prototype -> ..
        // -> EventTarget.prototype`. The standard has no `Window.prototype` here; it stays because the functions of the global live on it.
        let mut up = s.get_prototype_of(g)?;
        for name in ["WorkerGlobalScope", name] {
            let p = match up { Some(u) => s.new_object_with(u)?, None => s.new_object()? };
            let f = d.add_fn_s(s, illegal, name, 0, HostFn::Ctor)?;
            s.define_str(f, "prototype", p, Attrs { writable: false, enumerable: false, configurable: false })?;
            s.define_str(p, "constructor", f, Attrs::METHOD)?;
            let key = s.intrinsic(Intrinsic::SymbolToStringTag);
            let tag = s.string(name)?;
            s.define(p, key, tag, Attrs { writable: false, enumerable: false, configurable: true })?;
            s.define_str(g, name, f, Attrs::METHOD)?;
            up = Some(p);
        }
        s.set_prototype_of(g, up)?;
        // The window code puts the tag `Window` on the global itself.
        let key = s.intrinsic(Intrinsic::SymbolToStringTag);
        let tag = s.string(name)?;
        s.define(g, key, tag, Attrs { writable: false, enumerable: false, configurable: true })?;
        Ok(())
    })
}

fn illegal<'s, E: Eng>(_: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Err(s.type_error("Illegal constructor"))
}

/// Clone `a[0]` with the transfer list `a[1]` (a list, or an object with `transfer`) into the outbox, for `to`.
fn send<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>, to: Side, who: &str) -> Res<'s, ()> {
    let Some(msg) = a.get(0) else {
        return Err(s.type_error(&format!("Failed to execute 'postMessage' on '{who}': 1 argument required, but only 0 present.")));
    };
    let t = arg_s::<E>(s, a, 1);
    let list = d.transfer_arg(s, t, None)?;
    if d.kids.outbox_len() >= d.max_messages() {
        return Err(d.lim_s(s, "messages"));
    }
    let mut cloned = crate::browser_navigation_state::clone_state_with(d, s, msg, &list)?;
    d.export_ports(s, &mut cloned)?;
    d.kids.send(Out { to, target_origin: "*".into(), origin: String::new(), cloned });
    Ok(())
}

fn scope_post<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    send(d, s, a, Side::Parent, "DedicatedWorkerGlobalScope")?;
    Ok(s.undefined())
}

/// `importScripts(urls…)` (HTML, "importScripts"): parse all the URLs against the worker URL first (a bad one is a `SyntaxError`). Then,
/// in order, download each one and run it in the worker global. The download blocks the turn of the worker, as the spec makes the
/// call synchronous. It follows the rules of a classic script element (`resources::fetch`: redirects, 2xx, a JavaScript type, the
/// size cap) with the client of the worker page. A `data:` URL is decoded. A failed download is a `NetworkError`. A script that throws
/// stops the call with its exception.
fn scope_import<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    const CTX: &str = "Failed to execute 'importScripts' on 'WorkerGlobalScope'";
    let cur = d.cur_rec().ok();
    let mut urls = Vec::with_capacity(a.len());
    for i in 0..a.len() {
        let v = arg_s::<E>(s, a, i);
        let u = d.string_arg_s(s, v)?;
        match nu::parse_any(&u, cur.as_ref()) {
            Ok(r) => urls.push(r.href()),
            Err(_) => return Err(d.dom_err_s(s, "SyntaxError", &format!("{CTX}: The URL '{u}' is invalid."))),
        }
    }
    for href in urls {
        let Some(text) = import_text(d, &href) else {
            return Err(d.dom_err_s(s, "NetworkError", &format!("{CTX}: The script at '{href}' failed to load.")));
        };
        s.run_script_named(&text, &href)?;
    }
    Ok(s.undefined())
}

/// The text of the script at `href` for `importScripts`, or `None` when it does not load.
fn import_text<E: Eng>(d: &Dom<E>, href: &str) -> Option<String> {
    if href.starts_with("data:") {
        return crate::images::data_url(href).map(|b| String::from_utf8_lossy(&b).into_owned());
    }
    let url = bare_url::Url::parse(href.split('#').next().unwrap_or("")).ok()?;
    let limits = crate::resources::Limits::default();
    let deadline = std::time::Instant::now() + limits.max_time;
    let target = crate::resources::Target { expect: crate::resources::Expect::Script, url };
    let mut client = d.fetch.client.borrow().fork();
    let doc = d.cur_rec().ok().and_then(|r| bare_url::Url::parse(r.href().split('#').next().unwrap_or("")).ok());
    crate::resources::fetch(&mut client, &target, &limits, deadline, 0, doc.as_ref()).ok().map(|r| r.text)
}

fn scope_close<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    d.kids.closed.set(true);
    Ok(s.undefined())
}

fn lazy_dedicated<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    lazy(d, s, a, false)
}

fn lazy_shared<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    lazy(d, s, a, true)
}

/// `Worker` and `SharedWorker` are accessors of the global until the first read. A call with an argument is the setter.
fn lazy<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>, shared: bool) -> Res<'s, Local<'s>> {
    let g = s.global();
    let name = if shared { "SharedWorker" } else { "Worker" };
    let attrs = Attrs { writable: true, enumerable: false, configurable: true };
    if let Some(v) = a.get(0) {
        s.define_str(g, name, v, attrs)?;
        return Ok(s.undefined());
    }
    let made = if shared { d.worker.made_shared.get() } else { d.worker.made.get() };
    let ctor = match made {
        Some(m) => m[1],
        None => old::<E, _>(s, |it| make(d, it, shared))?,
    };
    let ctor = d.obj_local(s, ctor);
    s.define_str(g, name, ctor, attrs)?;
    Ok(ctor)
}

#[cold]
#[inline(never)]
fn make<E: Eng>(d: &Dom<E>, it: &mut E::Vm, shared: bool) -> R<E, E::Obj> {
    let name = if shared { "SharedWorker" } else { "Worker" };
    let proto = d.obj(it, d.p().et)?;
    let f = run::<E, _>(it, |s| {
        let f = d.add_fn_s(s, if shared { shared_worker_ctor } else { worker_ctor }, name, 1, HostFn::CtorNewTarget)?;
        let p = d.obj_local(s, proto);
        s.define_str(f, "prototype", p, Attrs { writable: false, enumerable: false, configurable: false })?;
        s.define_str(p, "constructor", f, Attrs { writable: true, enumerable: false, configurable: true })?;
        let tag = s.intrinsic(Intrinsic::SymbolToStringTag);
        let t = s.string(name)?;
        s.define(p, tag, t, Attrs { writable: false, enumerable: false, configurable: true })?;
        Ok(obj_id::<E>(s, f).expect("a function"))
    })?;
    if shared {
        d.def_get_s(it, proto, "port", shared_port, None)?;
        d.def_get_s(it, proto, "onerror", hget::<2, _>, Some(hset::<2, _>))?;
        d.worker.made_shared.set(Some([proto, f]));
        return Ok(f);
    }
    d.def_method_s(it, proto, "postMessage", 1, post_message)?;
    d.def_method_s(it, proto, "terminate", 0, terminate)?;
    let gets: [HS<E>; 3] = [hget::<0, _>, hget::<1, _>, hget::<2, _>];
    let sets: [HS<E>; 3] = [hset::<0, _>, hset::<1, _>, hset::<2, _>];
    for n in 0..3 {
        d.def_get_s(it, proto, &format!("on{}", TYPES[n]), gets[n], Some(sets[n]))?;
    }
    d.worker.made.set(Some([proto, f]));
    Ok(f)
}

pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    let g = s.global();
    for (name, lazy) in [("Worker", lazy_dedicated as HS<E>), ("SharedWorker", lazy_shared)] {
        let f = d.add_fn_s(s, lazy, name, 0, HostFn::Method)?;
        s.define_accessor_str(g, name, Some(f), Some(f), Attrs { writable: false, enumerable: false, configurable: true })?;
    }
    Ok(())
}

impl<E: Eng> Dom<E> {
    fn worker_of<'s>(&self, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, E::Obj> {
        match obj_id::<E>(s, this) {
            Some(o) if self.worker.objs.borrow().contains_key(&o) => Ok(o),
            _ => Err(s.type_error("Illegal invocation")),
        }
    }

    /// A `Worker` and not a `SharedWorker`: only a worker has `postMessage` and `terminate`.
    fn dedicated_of<'s>(&self, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, E::Obj> {
        let o = self.worker_of(s, this)?;
        if self.worker.ports.borrow().contains_key(&o) {
            return Err(s.type_error("Illegal invocation"));
        }
        Ok(o)
    }

    /// The script of the worker page of `o` did not load: queue `error` at `o`, as for a worker that cannot run.
    pub(crate) fn worker_error(&self, it: &mut E::Vm, o: E::Obj) -> R<E, ()> {
        let _ = it;
        self.worker.objs.borrow_mut().insert(o, true);
        self.schedule(self.now_ms(), crate::browser_scheduler::Task::Worker(o))?;
        Ok(())
    }

    /// The task after `new Worker`: fire `error` unless the worker was terminated.
    pub(crate) fn worker_failed(&self, it: &mut E::Vm, o: E::Obj) -> R<E, ()> {
        match self.worker.objs.borrow_mut().get_mut(&o) {
            Some(p) if *p => *p = false,
            _ => return Ok(()),
        }
        let listening = self.st.borrow().listeners.get(&Target::Obj(o)).is_some_and(|v| v.iter().any(|l| &*l.ty == "error"));
        if listening {
            let ev = self.new_event(it, "error", false, false, false, None)?;
            self.st.borrow_mut().events.get_mut(&ev).expect("just made").trusted = true;
            self.dispatch(it, Target::Obj(o), ev)?;
        }
        Ok(())
    }
}

/// The checks of both constructors after the arguments: the URL against the document, the origin, the caps. Gives the absolute URL.
fn check_url<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, who: &str, url: &str) -> Res<'s, String> {
    let cur = d.cur_rec().ok();
    let rec = match nu::parse_any(url, cur.as_ref()) {
        Ok(r) => r,
        Err(nu::UErr::Invalid) => return Err(d.dom_err_s(s, "SyntaxError", &format!("Failed to construct '{who}': Script at '{url}' is invalid."))),
        Err(nu::UErr::Scheme | nu::UErr::Gate(_)) => return Err(d.dom_err_s(s, "SecurityError", &format!("Failed to construct '{who}': Script at '{url}' cannot be accessed."))),
    };
    let href = rec.href();
    if !href.starts_with("data:") {
        let same = cur.as_ref().is_none_or(|c| rec.origin() == c.origin()) && (href.starts_with("http") || href.starts_with("blob:"));
        if !same {
            let from = cur.as_ref().map_or_else(String::new, |c| c.origin());
            return Err(d.dom_err_s(s, "SecurityError", &format!("Failed to construct '{who}': Script at '{href}' cannot be accessed from origin '{from}'.")));
        }
    }
    if d.worker.objs.borrow().len() >= MAX_WORKERS {
        return Err(d.dom_err_s(s, "NotSupportedError", &format!("Failed to construct '{who}': at most {MAX_WORKERS} workers can be alive in this page (bare cannot run them).")));
    }
    Ok(href)
}

/// Make the object. A dedicated worker in a page with a driver gets a worker page (`browser_child.rs`). Else queue its `error` and
/// log the first warning of its kind.
fn start<E: Eng>(d: &Dom<E>, it: &mut E::Vm, proto: E::Obj, href: &str, shared: bool, module: bool) -> R<E, E::Raw> {
    let port = if shared { Some(d.new_port(it)?) } else { None };
    let o = d.obj(it, proto)?;
    d.st.borrow_mut().add_target(it, o, crate::browser::Owner::Worker(o));
    if !shared && d.kids.add_worker(o, href.to_string(), module).is_some() {
        d.worker.objs.borrow_mut().insert(o, false);
        return Ok(E::obj_raw(o));
    }
    d.schedule(d.now_ms(), crate::browser_scheduler::Task::Worker(o))?;
    d.worker.objs.borrow_mut().insert(o, true);
    if let Some(p) = port {
        d.worker.ports.borrow_mut().insert(o, p);
    }
    let warned = if shared { &d.worker.warned_shared } else { &d.worker.warned };
    if !warned.replace(true) {
        let shown: String = href.chars().take(SHOWN_URL).collect();
        let who = if shared { "SharedWorker" } else { "Worker" };
        crate::browser_console::push_text(d, it, ConsoleLevel::Warn, &format!("{who} scripts do not run in bare ({shown})"));
    }
    Ok(E::obj_raw(o))
}

/// `WorkerOptions.type` is `classic` or `module` (true); `name` and `credentials` are ignored.
fn check_type<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, who: &str, opts: Local<'s>) -> Res<'s, bool> {
    let t = s.get_str(opts, "type")?;
    if s.type_of(t) != Type::Undefined {
        let t = d.string_arg_s(s, t)?;
        if t != "classic" && t != "module" {
            return Err(s.type_error(&format!("Failed to construct '{who}': The provided value '{t}' is not a valid enum value of type WorkerType.")));
        }
        return Ok(t == "module");
    }
    Ok(false)
}

fn worker_ctor<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let default = d.worker.made.get().expect("made")[0];
    let default = d.obj_local(s, default);
    let proto = s.proto_from_new_target(default)?;
    let proto = obj_id::<E>(s, proto).expect("a prototype is an object");
    let Some(a0) = a.get(0) else {
        return Err(s.type_error("Failed to construct 'Worker': 1 argument required, but only 0 present."));
    };
    let url = d.string_arg_s(s, a0)?;
    let a1 = arg_s::<E>(s, a, 1);
    let module = match s.type_of(a1) {
        Type::Undefined | Type::Null => false,
        Type::Object | Type::Function => check_type(d, s, "Worker", a1)?,
        _ => return Err(s.type_error("Failed to construct 'Worker': parameter 2 is not of type 'WorkerOptions'.")),
    };
    let href = check_url(d, s, "Worker", &url)?;
    old_v::<E>(s, |it| start(d, it, proto, &href, false, module))
}

/// `SharedWorker(url, name or options)`: the second argument is an options object, or else a string (the name).
fn shared_worker_ctor<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let default = d.worker.made_shared.get().expect("made")[0];
    let default = d.obj_local(s, default);
    let proto = s.proto_from_new_target(default)?;
    let proto = obj_id::<E>(s, proto).expect("a prototype is an object");
    let Some(a0) = a.get(0) else {
        return Err(s.type_error("Failed to construct 'SharedWorker': 1 argument required, but only 0 present."));
    };
    let url = d.string_arg_s(s, a0)?;
    let a1 = arg_s::<E>(s, a, 1);
    match s.type_of(a1) {
        Type::Undefined | Type::Null => {}
        Type::Object | Type::Function => {
            check_type(d, s, "SharedWorker", a1)?;
        }
        _ => {
            d.string_arg_s(s, a1)?;
        }
    }
    let href = check_url(d, s, "SharedWorker", &url)?;
    old_v::<E>(s, |it| start(d, it, proto, &href, true, false))
}

fn post_message<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.dedicated_of(s, this)?;
    if let Some(id) = d.kids.worker_id(o) {
        send(d, s, a, Side::Child(id), "Worker")?;
        return Ok(s.undefined());
    }
    let Some(m) = a.get(0) else {
        return Err(s.type_error("Failed to execute 'postMessage' on 'Worker': 1 argument required, but only 0 present."));
    };
    match s.type_of(m) {
        Type::Symbol => Err(d.dom_err_s(s, "DataCloneError", "Symbol could not be cloned.")),
        Type::Object | Type::Function if s.is_callable(m) => Err(d.dom_err_s(s, "DataCloneError", "function could not be cloned.")),
        _ => Ok(s.undefined()),
    }
}

fn terminate<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.dedicated_of(s, this)?;
    d.worker.objs.borrow_mut().insert(o, false);
    if let Some(id) = d.kids.worker_id(o) {
        d.kids.end(id);
    }
    Ok(s.undefined())
}

fn hget<'s, const N: usize, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.worker_of(s, this)?;
    old_v::<E>(s, |it| Ok(handler_get(d, it, Target::Obj(o), TYPES[N])))
}

fn hset<'s, const N: usize, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.worker_of(s, this)?;
    let v = raw_arg::<E>(s, a, 0);
    old::<E, _>(s, |it| handler_set(d, it, Target::Obj(o), TYPES[N], v))?;
    Ok(s.undefined())
}

fn shared_port<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.worker_of(s, this)?;
    let p = d.worker.ports.borrow().get(&o).copied();
    match p {
        Some(p) => Ok(d.obj_local(s, p)),
        None => Err(s.type_error("Illegal invocation")),
    }
}
