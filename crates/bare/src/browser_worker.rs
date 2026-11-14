//! `Worker` and `SharedWorker` (HTML Standard, "Workers"): a worker that fails to start.
//!
//! bare has one realm and cannot run a worker script. `new Worker(url)` does what a browser does when the worker script cannot load:
//! it returns the object, and a task after the current one fires a simple `error` event at it. A page that handles that event goes on.
//! A page that waits for a message waits for ever, as it would for a worker that never answers. See `docs/decisions.md`.
//!
//! - The constructor reads its arguments in the WebIDL order: the URL (a string; `new URL(..)` works), then the options (`type` is
//!   `classic` or `module`, else `TypeError`; `name` and `credentials` are ignored). Then it parses the URL against the document: a
//!   failure is a `SyntaxError`. A `data:` URL is taken. A `blob:` or `http(s):` URL must have the origin of the document, and any
//!   other scheme fails, both with a `SecurityError`. At most `MAX_WORKERS` worker objects live in a page (a collected one frees its
//!   place); the next one throws `NotSupportedError`.
//! - A worker is an `EventTarget` (a `Target::Obj`, so the listener code and the GC rules are the common ones) with `onmessage`,
//!   `onmessageerror` and `onerror`. `postMessage(message[, transfer])` sends nothing. It checks the arguments that a browser checks
//!   without a worker: one argument is required, and a function or a symbol throws `DataCloneError`. It does not look inside an
//!   object (a function in a property passes), and it does not read a transfer list.
//! - `terminate()` stops the queued `error`. A second call does nothing.
//! - The `error` task is `Task::Worker`; a queued task roots its worker. No event object is made when nobody listens.
//! - The first worker of a page logs one console warning, `Worker scripts do not run in bare (<url>)`.
//! - `SharedWorker` is made the same way, from the same host record and the same cap. Its constructor takes the URL and then a name
//!   (a string) or the options (`type`, `name`; `credentials` and `extraInfo` are ignored). The object has `onerror` and a read-only
//!   `port`: a `MessagePort` with no peer, that takes messages and answers none. It has no `postMessage` and no `terminate`. A page
//!   that tests `window.SharedWorker` to choose its code (craigslist does) takes the path that a browser takes. The first shared
//!   worker of a page logs `SharedWorker scripts do not run in bare (<url>)`.
//! - `Worker` is an accessor of the global until the first read, as `AbortController` is (`browser_abort.rs`): a realm that never reads
//!   it makes no interface.
//! - Not here: `ServiceWorker`, `importScripts`, a real worker (it needs a second realm and a structured clone between
//!   heaps), `Worker.prototype.postMessage` of an object that has a function inside, `ErrorEvent` for the `error` event.

use crate::browser::*;
use crate::browser_console::ConsoleLevel;
use crate::browser_handlers::{handler_get, handler_set};
use crate::browser_navigation_url as nu;
use bare_js::eval::Interp;
use bare_js::heap::PropertyDescriptor;
use bare_js::value::{ObjId, PropertyKey, Value};
use std::cell::{Cell, RefCell};

type V = R<Value>;

/// The most worker objects that a page keeps alive.
const MAX_WORKERS: usize = 64;
/// The most characters of a URL in the console warning.
const SHOWN_URL: usize = 200;

const TYPES: [&str; 3] = ["message", "messageerror", "error"];

#[derive(Default)]
pub(crate) struct WorkerHost {
    /// The live workers, each with its pending `error`: true until the task ran or `terminate()` was called.
    objs: RefCell<IdMap<ObjId, bool>>,
    warned: Cell<bool>,
    warned_shared: Cell<bool>,
    /// The prototype and the constructor, once made. Roots.
    made: Cell<Option<[ObjId; 2]>>,
    /// The same for `SharedWorker`.
    made_shared: Cell<Option<[ObjId; 2]>>,
    /// The live shared workers (they are also in `objs`) with their `port`. The ports are roots; `prune` drops a port with its worker.
    ports: RefCell<IdMap<ObjId, ObjId>>,
}

pub(crate) fn roots(d: &Dom, out: &mut Vec<Value>) {
    for m in [d.worker.made.get(), d.worker.made_shared.get()].into_iter().flatten() {
        out.extend(m.map(Value::Object));
    }
    out.extend(d.worker.ports.borrow().values().map(|&p| Value::Object(p)));
}

pub(crate) fn prune(d: &Dom, live: &dyn Fn(ObjId) -> bool) {
    d.worker.objs.borrow_mut().retain(|&o, _| live(o));
    d.worker.ports.borrow_mut().retain(|&o, _| live(o));
}

fn lazy_dedicated(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    lazy(d, it, a, false)
}

fn lazy_shared(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    lazy(d, it, a, true)
}

/// `Worker` and `SharedWorker` are accessors of the global until the first read. A call with an argument is the setter.
fn lazy(d: &Dom, it: &mut Interp<'_>, a: &[Value], shared: bool) -> V {
    let g = *d.global.get().expect("installed");
    let k = d.key(it, if shared { "SharedWorker" } else { "Worker" })?;
    if let Some(&v) = a.first() {
        it.define_data(g, k, v, true, false, true)?;
        return Ok(Value::Undefined);
    }
    let made = if shared { d.worker.made_shared.get() } else { d.worker.made.get() };
    let ctor = match made {
        Some(m) => m[1],
        None => make(d, it, shared)?,
    };
    it.define_data(g, k, Value::Object(ctor), true, false, true)?;
    Ok(Value::Object(ctor))
}

#[cold]
#[inline(never)]
fn make(d: &Dom, it: &mut Interp<'_>, shared: bool) -> R<ObjId> {
    let g = *d.global.get().expect("installed");
    let name = if shared { "SharedWorker" } else { "Worker" };
    let proto = d.obj(it, d.p().et)?;
    let f = d.add_ctor_nt(it, if shared { shared_worker_ctor } else { worker_ctor }, name, 1)?;
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    it.define_data(f, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(f), true, false, true)?;
    let (ks, kt) = (d.key(it, "Symbol")?, d.key(it, "toStringTag")?);
    let sy = it.get_value(Value::Object(g), ks)?;
    if let Value::Symbol(s) = it.get_value(sy, kt)? {
        let t = d.js_str(it, name)?;
        it.define_data(proto, PropertyKey::Symbol(s), t, false, false, true)?;
    }
    if shared {
        d.def_get(it, proto, "port", shared_port, None)?;
        d.def_get(it, proto, "onerror", hget::<2>, Some(hset::<2>))?;
        d.worker.made_shared.set(Some([proto, f]));
        return Ok(f);
    }
    d.def_method(it, proto, "postMessage", 1, post_message)?;
    d.def_method(it, proto, "terminate", 0, terminate)?;
    let gets: [H; 3] = [hget::<0>, hget::<1>, hget::<2>];
    let sets: [H; 3] = [hset::<0>, hset::<1>, hset::<2>];
    for n in 0..3 {
        d.def_get(it, proto, &format!("on{}", TYPES[n]), gets[n], Some(sets[n]))?;
    }
    d.worker.made.set(Some([proto, f]));
    Ok(f)
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = *d.global.get().expect("installed");
    for (name, lazy) in [("Worker", lazy_dedicated as H), ("SharedWorker", lazy_shared as H)] {
        let key = d.key(it, name)?;
        let f = d.add_fn(it, lazy, name, 0, false)?;
        let desc = PropertyDescriptor { get: Some(Some(f)), set: Some(Some(f)), enumerable: Some(false), configurable: Some(true), ..Default::default() };
        it.heap.define_own_property(g, key, &desc).map(|_| ()).map_err(|e| it.heap_error(e))?;
    }
    Ok(())
}

impl Dom {
    fn worker_of(&self, it: &mut Interp<'_>, this: Value) -> R<ObjId> {
        match this {
            Value::Object(o) if self.worker.objs.borrow().contains_key(&o) => Ok(o),
            _ => Err(it.type_error("Illegal invocation")),
        }
    }

    /// A `Worker` and not a `SharedWorker`: only a worker has `postMessage` and `terminate`.
    fn dedicated_of(&self, it: &mut Interp<'_>, this: Value) -> R<ObjId> {
        let o = self.worker_of(it, this)?;
        if self.worker.ports.borrow().contains_key(&o) {
            return Err(it.type_error("Illegal invocation"));
        }
        Ok(o)
    }

    /// The task after `new Worker`: fire `error` unless the worker was terminated.
    pub(crate) fn worker_failed(&self, it: &mut Interp<'_>, o: ObjId) -> R<()> {
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
fn check_url(d: &Dom, it: &mut Interp<'_>, who: &str, url: &str) -> R<String> {
    let cur = d.cur_rec().ok();
    let rec = match nu::parse_any(url, cur.as_ref()) {
        Ok(r) => r,
        Err(nu::UErr::Invalid) => return Err(d.dom_err(it, "SyntaxError", &format!("Failed to construct '{who}': Script at '{url}' is invalid."))),
        Err(nu::UErr::Scheme | nu::UErr::Gate(_)) => return Err(d.dom_err(it, "SecurityError", &format!("Failed to construct '{who}': Script at '{url}' cannot be accessed."))),
    };
    let href = rec.href();
    if !href.starts_with("data:") {
        let same = cur.as_ref().is_none_or(|c| rec.origin() == c.origin()) && (href.starts_with("http") || href.starts_with("blob:"));
        if !same {
            let from = cur.as_ref().map_or_else(String::new, |c| c.origin());
            return Err(d.dom_err(it, "SecurityError", &format!("Failed to construct '{who}': Script at '{href}' cannot be accessed from origin '{from}'.")));
        }
    }
    if d.worker.objs.borrow().len() >= MAX_WORKERS {
        return Err(d.dom_err(it, "NotSupportedError", &format!("Failed to construct '{who}': at most {MAX_WORKERS} workers can be alive in this page (bare cannot run them).")));
    }
    if d.st.borrow().by_obj.len() >= d.lim.max_wrappers {
        return Err(d.lim("dom-wrappers"));
    }
    Ok(href)
}

/// Make the object, queue its `error` and log the first warning of its kind.
fn start(d: &Dom, it: &mut Interp<'_>, proto: ObjId, href: &str, shared: bool) -> R<Value> {
    let port = if shared { Some(d.new_port(it)?) } else { None };
    let o = d.obj(it, proto)?;
    d.st.borrow_mut().by_obj.insert(o, Target::Obj(o));
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
    Ok(Value::Object(o))
}

/// `WorkerOptions.type` is `classic` or `module`; `name` and `credentials` are ignored.
fn check_type(d: &Dom, it: &mut Interp<'_>, who: &str, opts: Value) -> R<()> {
    let k = d.key(it, "type")?;
    let t = it.get_value(opts, k)?;
    if !matches!(t, Value::Undefined) {
        let t = d.string_arg(it, t)?;
        if t != "classic" && t != "module" {
            return Err(it.type_error(&format!("Failed to construct '{who}': The provided value '{t}' is not a valid enum value of type WorkerType.")));
        }
    }
    Ok(())
}

fn worker_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let proto = it.host_proto_from_new_target(d.worker.made.get().expect("made")[0])?;
    if a.is_empty() {
        return Err(it.type_error("Failed to construct 'Worker': 1 argument required, but only 0 present."));
    }
    let url = d.string_arg(it, a[0])?;
    match a.get(1).copied().unwrap_or(Value::Undefined) {
        Value::Undefined | Value::Null => {}
        opts @ Value::Object(_) => check_type(d, it, "Worker", opts)?,
        _ => return Err(it.type_error("Failed to construct 'Worker': parameter 2 is not of type 'WorkerOptions'.")),
    }
    let href = check_url(d, it, "Worker", &url)?;
    start(d, it, proto, &href, false)
}

/// `SharedWorker(url, name or options)`: the second argument is an options object, or else a string (the name).
fn shared_worker_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let proto = it.host_proto_from_new_target(d.worker.made_shared.get().expect("made")[0])?;
    if a.is_empty() {
        return Err(it.type_error("Failed to construct 'SharedWorker': 1 argument required, but only 0 present."));
    }
    let url = d.string_arg(it, a[0])?;
    match a.get(1).copied().unwrap_or(Value::Undefined) {
        Value::Undefined | Value::Null => {}
        opts @ Value::Object(_) => check_type(d, it, "SharedWorker", opts)?,
        name => {
            d.string_arg(it, name)?;
        }
    }
    let href = check_url(d, it, "SharedWorker", &url)?;
    start(d, it, proto, &href, true)
}

fn post_message(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    d.dedicated_of(it, this)?;
    let Some(&m) = a.first() else {
        return Err(it.type_error("Failed to execute 'postMessage' on 'Worker': 1 argument required, but only 0 present."));
    };
    match m {
        Value::Symbol(_) => Err(d.dom_err(it, "DataCloneError", "Symbol could not be cloned.")),
        Value::Object(_) if it.is_callable_value(m) => Err(d.dom_err(it, "DataCloneError", "function could not be cloned.")),
        _ => Ok(Value::Undefined),
    }
}

fn terminate(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let o = d.dedicated_of(it, this)?;
    d.worker.objs.borrow_mut().insert(o, false);
    Ok(Value::Undefined)
}

fn hget<const N: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let o = d.worker_of(it, this)?;
    Ok(handler_get(d, Target::Obj(o), TYPES[N]))
}

fn hset<const N: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let o = d.worker_of(it, this)?;
    handler_set(d, Target::Obj(o), TYPES[N], a.first().copied())?;
    Ok(Value::Undefined)
}

fn shared_port(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let o = d.worker_of(it, this)?;
    match d.worker.ports.borrow().get(&o) {
        Some(&p) => Ok(Value::Object(p)),
        None => Err(it.type_error("Illegal invocation")),
    }
}
