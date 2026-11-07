//! Events of the browser host: `Event`, `EventTarget` and dispatch. See `browser.rs` for the design and the caps.
//!
//! Dispatch follows the DOM Standard: the path is the target, its ancestors, and `window` (when the tree root is the
//! document and the type is not `load`). Capturing listeners run from the outside in, then the target (capturing
//! listeners first, then the others), then bubbling listeners from the inside out when the event bubbles.
//! `stopPropagation` is checked before each step, `stopImmediatePropagation` before each listener. The listener list
//! is copied when a node's turn starts: a listener added during that turn waits, and one removed during it is skipped.
//!
//! A listener that throws is reported and dispatch goes on. `Limit`, `Unsupported` and heap faults end the dispatch
//! (the event is cleaned up) and are the result of the call. No document borrow is held while a listener runs.

use crate::browser::*;
use bare_js::eval::{Error, Interp};
use bare_js::heap::Get;
use bare_js::value::{ObjId, Value};
use std::rc::Rc;

type V = R<Value>;

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

impl Dom {
    /// `this` of an `EventTarget` method. `undefined` and `null` mean `window`, as in a browser.
    fn target_of(&self, it: &mut Interp<'_>, this: Value) -> R<Target> {
        match this {
            Value::Undefined | Value::Null => Ok(Target::Window),
            Value::Object(o) => match self.st.borrow().by_obj.get(&o) {
                Some(&t) => Ok(t),
                None => Err(it.type_error("Illegal invocation")),
            },
            _ => Err(it.type_error("Illegal invocation")),
        }
    }

    pub(crate) fn target_value(&self, it: &mut Interp<'_>, t: Target) -> V {
        match t {
            Target::Window => Ok(Value::Object(*self.global.get().expect("installed"))),
            Target::Node(n) => self.wrap(it, n),
            Target::Obj(o) => Ok(Value::Object(o)),
        }
    }

    pub(crate) fn event_of(&self, it: &mut Interp<'_>, v: Value) -> R<ObjId> {
        match v {
            Value::Object(o) if self.st.borrow().events.contains_key(&o) => Ok(o),
            _ => Err(it.type_error("Illegal invocation")),
        }
    }

    /// A new event object. `Some(detail)` makes a `CustomEvent`, including an undefined detail.
    pub(crate) fn new_event(&self, it: &mut Interp<'_>, ty: &str, bubbles: bool, cancelable: bool, composed: bool, detail: Option<Value>) -> R<ObjId> {
        let p = self.p();
        let o = self.new_event_proto(it, ty, bubbles, cancelable, composed, if detail.is_none() { p.event } else { p.custom })?;
        if let Some(d) = detail {
            self.st.borrow_mut().events.get_mut(&o).expect("just made").detail = d;
        }
        Ok(o)
    }

    /// Like `new_event`, with the prototype given (a host event interface such as `MessageEvent`).
    pub(crate) fn new_event_proto(&self, it: &mut Interp<'_>, ty: &str, bubbles: bool, cancelable: bool, composed: bool, proto: ObjId) -> R<ObjId> {
        if ty.len() > self.lim.max_string_bytes {
            return Err(self.lim("dom-string"));
        }
        it.charge((ty.len() / 32) as u64)?;
        if self.st.borrow().events.len() >= self.lim.max_events {
            return Err(self.lim("dom-events"));
        }
        let o = self.obj(it, proto)?;
        let e = EventState {
            err: false,
            ty: Rc::from(ty),
            bubbles,
            cancelable,
            composed,
            canceled: false,
            stop: false,
            stop_imm: false,
            dispatching: false,
            in_passive: false,
            phase: 0,
            target: Value::Null,
            current: Value::Null,
            detail: Value::Null,
            path: Vec::new(),
            time: self.now_ms(),
            related: Value::Null,
            is_focus: false,
            is_submit: false,
            ui: None,
            trusted: false,
        };
        self.st.borrow_mut().events.insert(o, e);
        Ok(o)
    }

    /// Run the dispatch algorithm. Gives whether the event was not canceled.
    pub(crate) fn dispatch(&self, it: &mut Interp<'_>, target: Target, ev: ObjId) -> R<bool> {
        let ty = {
            let mut st = self.st.borrow_mut();
            let e = st.events.get_mut(&ev).expect("event is registered");
            if e.dispatching {
                drop(st);
                return Err(self.dom_err(it, "InvalidStateError", "The event is already being dispatched."));
            }
            e.dispatching = true;
            e.ty.clone()
        };
        let r = self.dispatch_inner(it, target, ev, &ty);
        let mut st = self.st.borrow_mut();
        let e = st.events.get_mut(&ev).expect("event is registered");
        let not_canceled = !e.canceled;
        (e.dispatching, e.stop, e.stop_imm, e.in_passive, e.phase, e.current) = (false, false, false, false, 0, Value::Null);
        e.path.clear();
        r.map(|_| not_canceled)
    }

    fn dispatch_inner(&self, it: &mut Interp<'_>, target: Target, ev: ObjId, ty: &str) -> R<()> {
        let mut path = vec![target];
        let mut quiet = false;
        if let Target::Node(n) = target {
            let doc = self.doc.borrow();
            let mut cur = n;
            while let Some(p) = doc.parent(cur) {
                it.charge(1)?;
                if path.len() > self.lim.max_tree_depth + 2 {
                    return Err(self.lim("dom-path"));
                }
                path.push(Target::Node(p));
                cur = p;
            }
            if cur == doc.root() && ty != "load" {
                path.push(Target::Window);
            }
            // The path ends at a shadow root. Not composed: that is the standard path. Composed: the standard goes on at the host and retargets, which
            // this host does not do, so the event is refused before any listener runs (never a wrong path).
            if doc.shadow_host(cur).is_some() && self.st.borrow().events[&ev].composed {
                return Err(Error::Unsupported(unsupported("dispatch of a composed event across a shadow root (retargeting is not implemented)")));
            }
            quiet = !doc.has_on_name();
        }
        let tv = self.target_value(it, target)?;
        let bubbles = {
            let mut st = self.st.borrow_mut();
            let e = st.events.get_mut(&ev).expect("event is registered");
            e.target = tv;
            e.path = path.clone();
            e.bubbles
        };
        // Content attributes `on<type>`: make the slots of the path up to date once, when the document has such a name (a change made by a
        // listener of this dispatch is seen by the next dispatch).
        if !quiet {
            crate::browser_handlers::sync_path_of(self, &path, ty);
        }
        for &t in path[1..].iter().rev() {
            self.fire(it, ev, t, 1, true)?;
        }
        self.fire(it, ev, target, 2, true)?;
        self.fire(it, ev, target, 2, false)?;
        if bubbles {
            for &t in &path[1..] {
                self.fire(it, ev, t, 3, false)?;
            }
        }
        Ok(())
    }

    /// The listeners of `t` for this event, capturing or not, one turn.
    fn fire(&self, it: &mut Interp<'_>, ev: ObjId, t: Target, phase: u8, capture: bool) -> R<()> {
        let (ty, snapshot) = {
            let st = self.st.borrow();
            let e = &st.events[&ev];
            if e.stop {
                return Ok(());
            }
            let list: Vec<Listener> = st.listeners.get(&t).into_iter().flatten().filter(|l| l.capture == capture && *l.ty == *e.ty).cloned().collect();
            (e.ty.clone(), list)
        };
        if snapshot.is_empty() {
            return Ok(());
        }
        let cur = self.target_value(it, t)?;
        {
            let mut st = self.st.borrow_mut();
            let e = st.events.get_mut(&ev).expect("event is registered");
            (e.phase, e.current) = (phase, cur);
        }
        for mut l in snapshot {
            let raw = {
                let st = self.st.borrow();
                if st.events[&ev].stop_imm {
                    break;
                }
                // A property handler is called with its value at its turn (a replacement is seen, a removal skips it).
                match st.listeners.get(&t).and_then(|v| v.iter().find(|x| x.id == l.id)) {
                    Some(x) => l.cb = x.cb,
                    None => continue,
                }
                l.handler && !st.hd.raw.is_empty() && st.hd.raw.contains_key(&l.id)
            };
            if raw {
                match crate::browser_handlers::compile_slot(self, it, t, &ty, l.id)? {
                    Value::Object(f) => l.cb = f,
                    _ => continue,
                }
            }
            if l.once {
                self.remove_listener(t, l.id);
            }
            self.st.borrow_mut().events.get_mut(&ev).expect("event is registered").in_passive = l.passive;
            it.charge(1)?;
            self.st.borrow_mut().ui.epoch += 1;
            let r = if l.handler { self.call_handler(it, t, &l, cur, ev) } else { self.call_listener(it, &l, cur, Value::Object(ev)) };
            self.st.borrow_mut().events.get_mut(&ev).expect("event is registered").in_passive = false;
            match r {
                Ok(_) => {}
                Err(Error::Throw(v)) => self.report(it, v, &ty, t),
                Err(e) => return Err(e),
            }
            if self.st.borrow().driver {
                it.host_checkpoint()?;
            }
        }
        Ok(())
    }

    pub(crate) fn remove_listener(&self, t: Target, id: u64) {
        let mut st = self.st.borrow_mut();
        if let Some(v) = st.listeners.get_mut(&t)
            && let Some(i) = v.iter().position(|l| l.id == id)
        {
            v.remove(i);
            if v.is_empty() {
                st.listeners.remove(&t);
            }
            st.n_listeners -= 1;
        }
    }

    /// HTML "event handler processing algorithm": a value that is not callable is skipped without an error, `this` is
    /// the current target, and a return value of `false` sets the canceled flag (as `preventDefault` would).
    /// Two cases differ: the `error` handler of `window` for an `ErrorEvent` gets `(message, filename, lineno, colno, error)` and `true`
    /// cancels; code of a content attribute looks its names up in the element, its form owner and the document first.
    fn call_handler(&self, it: &mut Interp<'_>, t: Target, l: &Listener, this: Value, ev: ObjId) -> V {
        let cb = Value::Object(l.cb);
        if !it.is_callable_value(cb) {
            return Ok(Value::Undefined);
        }
        if (matches!(t, Target::Window) && &*l.ty == "error") || it.is_scoped_function(l.cb) {
            return self.call_handler_special(it, t, l, this, ev);
        }
        let r = it.call(cb, this, &[Value::Object(ev)])?;
        if matches!(r, Value::Bool(false)) && !(matches!(t, Target::Window) && &*l.ty == "beforeunload") {
            let mut st = self.st.borrow_mut();
            let e = st.events.get_mut(&ev).expect("event is registered");
            e.canceled |= e.cancelable && !e.in_passive;
        }
        Ok(r)
    }

    /// `call_handler` for the `error` handler of `window` or code of a content attribute.
    #[inline(never)]
    fn call_handler_special(&self, it: &mut Interp<'_>, t: Target, l: &Listener, this: Value, ev: ObjId) -> V {
        let cb = Value::Object(l.cb);
        let win_err = matches!(t, Target::Window) && &*l.ty == "error" && self.st.borrow().events[&ev].err;
        let scope = if it.is_scoped_function(l.cb) { crate::browser_handlers::scope_of(self, it, t)? } else { Vec::new() };
        let mut args = [Value::Object(ev); 5];
        let n = if win_err {
            for (i, name) in ["message", "filename", "lineno", "colno", "error"].into_iter().enumerate() {
                let k = self.key(it, name)?;
                args[i] = it.get_value(Value::Object(ev), k)?;
            }
            5
        } else {
            1
        };
        let r = if scope.is_empty() { it.call(cb, this, &args[..n])? } else { it.call_scoped(cb, this, &args[..n], &scope)? };
        if matches!(r, Value::Bool(b) if b == win_err) && !(matches!(t, Target::Window) && &*l.ty == "beforeunload") {
            let mut st = self.st.borrow_mut();
            let e = st.events.get_mut(&ev).expect("event is registered");
            e.canceled |= e.cancelable && !e.in_passive;
        }
        Ok(r)
    }

    fn call_listener(&self, it: &mut Interp<'_>, l: &Listener, this: Value, ev: Value) -> V {
        let cb = Value::Object(l.cb);
        if it.is_callable_value(cb) {
            return it.call(cb, this, &[ev]);
        }
        let k = self.key(it, "handleEvent")?;
        let f = it.get_value(cb, k)?;
        if !it.is_callable_value(f) {
            return Err(it.type_error("The event listener's handleEvent is not a function"));
        }
        it.call(f, cb, &[ev])
    }

    /// Keep the text of a listener exception. It reads data properties only, so no JS runs.
    pub(crate) fn report(&self, it: &mut Interp<'_>, thrown: Value, ty: &str, t: Target) {
        let message = self.error_text(it, thrown);
        self.window_error(it, thrown, &message);
        if self.lim.max_reports == 0 {
            let mut st = self.st.borrow_mut();
            st.dropped_reports = st.dropped_reports.saturating_add(1);
            return;
        }
        self.record_report(t, ty, message);
    }

    /// An uncaught exception: the `error` event of `window` (HTML "report an exception"), when something listens. An exception in it is not reported again.
    fn window_error(&self, it: &mut Interp<'_>, thrown: Value, text: &str) {
        crate::browser_handlers::sync(self, Target::Window, "error");
        {
            let st = self.st.borrow();
            if st.hd.in_error || !st.listeners.get(&Target::Window).is_some_and(|v| v.iter().any(|l| &*l.ty == "error")) {
                return;
            }
        }
        self.st.borrow_mut().hd.in_error = true;
        let message = format!("Uncaught {text}");
        let fields = crate::browser_error_event::Fields { message: &message, filename: "", lineno: 0, colno: 0, error: thrown };
        let _ = crate::browser_error_event::build(self, it, "error", [false, true, false], fields).and_then(|ev| self.dispatch(it, Target::Window, ev));
        self.st.borrow_mut().hd.in_error = false;
    }

    /// `name: message` of an error object, or the text of the thrown value. It reads data properties only, so no JS runs.
    fn error_text(&self, it: &mut Interp<'_>, thrown: Value) -> String {
        let cap = self.lim.max_string_bytes.min(4096);
        let append = |out: &mut String, units: &[u16]| {
            for c in char::decode_utf16(units.iter().copied()) {
                let c = c.unwrap_or(char::REPLACEMENT_CHARACTER);
                if c.len_utf8() > cap.saturating_sub(out.len()) { break; }
                out.push(c);
            }
        };
        let text = |it: &mut Interp<'_>, out: &mut String, o: ObjId, key: &str| {
            let Ok(k) = it.heap.key_from_str(key) else { return };
            if let Ok(Get::Value(Value::String(s))) = it.heap.get(o, k)
                && let Ok(units) = it.heap.str_units(s)
            {
                append(out, units);
            }
        };
        let mut message = String::new();
        match thrown {
            Value::Object(o) => {
                text(it, &mut message, o, "name");
                append(&mut message, &[58, 32]);
                text(it, &mut message, o, "message");
            }
            Value::String(s) => if let Ok(units) = it.heap.str_units(s) { append(&mut message, units); },
            Value::Symbol(_) => append(&mut message, &[83, 121, 109, 98, 111, 108]),
            v => append(&mut message, &it.display(v).encode_utf16().collect::<Vec<_>>()),
        }
        message
    }

    fn record_report(&self, t: Target, ty: &str, message: String) {
        let target = match t {
            Target::Window => "window".to_string(),
            Target::Obj(_) => "object".to_string(),
            Target::Node(n) => match self.data(n) {
                bare_html::dom::NodeData::Element { name, .. } => self.doc.borrow().atom_str(name).to_string(),
                bare_html::dom::NodeData::Text(_) => "#text".into(),
                bare_html::dom::NodeData::Comment(_) => "#comment".into(),
                bare_html::dom::NodeData::Document => "#document".into(),
                bare_html::dom::NodeData::ShadowRoot => "#shadow-root".into(),
                bare_html::dom::NodeData::Doctype(_) => "#doctype".into(),
            },
        };
        let mut st = self.st.borrow_mut();
        if st.reports.len() >= self.lim.max_reports {
            st.reports.pop_front();
            st.dropped_reports += 1;
        }
        st.reports.push_back(Report { event_type: ty.to_string(), target, message });
    }
}

// ------------------------------------------------------------------------------------------ constructors

pub(crate) fn init_flag(d: &Dom, it: &mut Interp<'_>, init: Value, name: &str) -> R<bool> {
    let k = d.key(it, name)?;
    let v = it.get_value(init, k)?;
    it.to_js_bool(v)
}

fn make_event(d: &Dom, it: &mut Interp<'_>, a: &[Value], custom: bool) -> V {
    if a.is_empty() {
        return Err(it.type_error("Failed to construct 'Event': 1 argument required, but only 0 present."));
    }
    let ty = d.string_arg(it, a[0])?;
    let init = arg(a, 1);
    let (mut bubbles, mut cancelable, mut composed, mut detail) = (false, false, false, Value::Null);
    match init {
        Value::Undefined | Value::Null => {}
        Value::Object(_) => {
            bubbles = init_flag(d, it, init, "bubbles")?;
            cancelable = init_flag(d, it, init, "cancelable")?;
            composed = init_flag(d, it, init, "composed")?;
            if custom {
                let k = d.key(it, "detail")?;
                detail = it.get_value(init, k)?;
                if matches!(detail, Value::Undefined) { detail = Value::Null; }
            }
        }
        _ => return Err(it.type_error("The event initializer is not an object.")),
    }
    // A custom event passes its detail (`null` at least); a plain one passes `undefined`.
    let proto = it.host_proto_from_new_target(if custom { d.p().custom } else { d.p().event })?;
    let e = d.new_event_proto(it, &ty, bubbles, cancelable, composed, proto)?;
    if custom {
        d.st.borrow_mut().events.get_mut(&e).expect("just made").detail = detail;
    }
    Ok(Value::Object(e))
}

pub(crate) fn event_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    make_event(d, it, a, false)
}

pub(crate) fn custom_event_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    make_event(d, it, a, true)
}

pub(crate) fn event_target_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    if d.st.borrow().by_obj.len() >= d.lim.max_wrappers {
        return Err(d.lim("dom-wrappers"));
    }
    let proto = it.host_proto_from_new_target(d.p().et)?;
    let o = d.obj(it, proto)?;
    d.st.borrow_mut().by_obj.insert(o, Target::Obj(o));
    Ok(Value::Object(o))
}

// ------------------------------------------------------------------------------------------ EventTarget

pub(crate) fn add_event_listener(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let t = d.target_of(it, this)?;
    let ty = d.string_arg(it, arg(a, 0))?;
    let cb = match arg(a, 1) {
        Value::Null | Value::Undefined => return Ok(Value::Undefined),
        Value::Object(o) => o,
        _ => return Err(it.type_error("Failed to execute 'addEventListener': parameter 2 is not of type 'Object'.")),
    };
    let (mut capture, mut once, mut passive, mut signal) = (false, false, false, None);
    match arg(a, 2) {
        opts @ Value::Object(_) => {
            capture = init_flag(d, it, opts, "capture")?;
            once = init_flag(d, it, opts, "once")?;
            passive = init_flag(d, it, opts, "passive")?;
            let k = d.key(it, "signal")?;
            match it.get_value(opts, k)? {
                Value::Undefined => {}
                Value::Object(o) if d.is_signal(o) => signal = Some(o),
                _ => return Err(it.type_error("Failed to execute 'addEventListener' on 'EventTarget': Failed to read the 'signal' property from 'AddEventListenerOptions': Failed to convert value to 'AbortSignal'.")),
            }
        }
        Value::Undefined | Value::Null => {}
        v => capture = it.to_js_bool(v)?,
    }
    if let Some(s) = signal
        && d.signal_state(s).is_some_and(|s| s.0)
    {
        return Ok(Value::Undefined);
    }
    crate::browser_handlers::sync_all(d, t);
    let mut guard = d.st.borrow_mut();
    let st = &mut *guard;
    let list = st.listeners.entry(t).or_default();
    if list.iter().any(|l| *l.ty == *ty && !l.handler && l.cb == cb && l.capture == capture) {
        return Ok(Value::Undefined);
    }
    if st.n_listeners >= d.lim.max_listeners {
        return Err(d.lim("dom-listeners"));
    }
    if list.len() >= d.lim.max_listeners_per_target {
        return Err(d.lim("dom-listeners-per-target"));
    }
    st.next_id += 1;
    let id = st.next_id;
    list.push(Listener { id, ty: Rc::from(ty.as_str()), cb, capture, once, passive, handler: false });
    st.n_listeners += 1;
    drop(guard);
    if let Some(s) = signal
        && let Err(e) = d.signal_add_algo(s, crate::browser_abort::Algo::Remove(t, id))
    {
        d.remove_listener(t, id);
        return Err(e);
    }
    Ok(Value::Undefined)
}

pub(crate) fn remove_event_listener(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let t = d.target_of(it, this)?;
    let ty = d.string_arg(it, arg(a, 0))?;
    let Value::Object(cb) = arg(a, 1) else { return Ok(Value::Undefined) };
    let capture = match arg(a, 2) {
        opts @ Value::Object(_) => init_flag(d, it, opts, "capture")?,
        v => it.to_js_bool(v)?,
    };
    let found = d.st.borrow().listeners.get(&t).and_then(|v| v.iter().find(|l| !l.handler && *l.ty == *ty && l.cb == cb && l.capture == capture).map(|l| l.id));
    if let Some(id) = found {
        d.remove_listener(t, id);
    }
    Ok(Value::Undefined)
}

pub(crate) fn dispatch_event(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let t = d.target_of(it, this)?;
    let Value::Object(e) = arg(a, 0) else { return Err(it.type_error("Failed to execute 'dispatchEvent': parameter 1 is not of type 'Event'.")) };
    if !d.st.borrow().events.contains_key(&e) {
        return Err(it.type_error("Failed to execute 'dispatchEvent': parameter 1 is not of type 'Event'."));
    }
    Ok(Value::Bool(d.dispatch(it, t, e)?))
}

// ------------------------------------------------------------------------------------------ Event

fn with_event<T>(d: &Dom, it: &mut Interp<'_>, this: Value, f: impl FnOnce(&mut EventState) -> T) -> R<T> {
    let o = d.event_of(it, this)?;
    Ok(f(d.st.borrow_mut().events.get_mut(&o).expect("checked")))
}

pub(crate) fn event_type(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let ty = with_event(d, it, this, |e| e.ty.clone())?;
    d.js_str(it, &ty)
}

pub(crate) fn event_target(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    with_event(d, it, this, |e| e.target)
}

pub(crate) fn event_current_target(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    with_event(d, it, this, |e| e.current)
}

pub(crate) fn event_phase(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    with_event(d, it, this, |e| Value::Number(e.phase as f64))
}

pub(crate) fn event_bubbles(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    with_event(d, it, this, |e| Value::Bool(e.bubbles))
}

pub(crate) fn event_cancelable(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    with_event(d, it, this, |e| Value::Bool(e.cancelable))
}

pub(crate) fn event_composed(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    with_event(d, it, this, |e| Value::Bool(e.composed))
}

pub(crate) fn event_default_prevented(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    with_event(d, it, this, |e| Value::Bool(e.canceled))
}

/// True for the events of the window input (`browser_ui.rs`), false for all others.
pub(crate) fn event_is_trusted(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    with_event(d, it, this, |e| Value::Bool(e.trusted))
}

pub(crate) fn event_time_stamp(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    with_event(d, it, this, |e| Value::Number(e.time))
}

pub(crate) fn prevent_default(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    with_event(d, it, this, |e| e.canceled |= e.cancelable && !e.in_passive)?;
    Ok(Value::Undefined)
}

pub(crate) fn stop_propagation(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    with_event(d, it, this, |e| e.stop = true)?;
    Ok(Value::Undefined)
}

pub(crate) fn stop_immediate_propagation(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    with_event(d, it, this, |e| (e.stop, e.stop_imm) = (true, true))?;
    Ok(Value::Undefined)
}

pub(crate) fn composed_path(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let path = with_event(d, it, this, |e| e.path.clone())?;
    let mut vals = Vec::with_capacity(path.len());
    for t in path {
        vals.push(d.target_value(it, t)?);
    }
    it.array_from(&vals)
}

pub(crate) fn custom_detail(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    with_event(d, it, this, |e| e.detail)
}
