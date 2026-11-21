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

use crate::eng::{Eng, Fault, Got, Ops, Val};
use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, Local, Type};
use std::rc::Rc;

type V<E: Eng> = R<E, E::Raw>;

/// A listener of one turn of `fire`. Its callback is read at its turn.
struct Turn {
    id: u64,
    once: bool,
    passive: bool,
    handler: bool,
}

impl<E: Eng> Dom<E> {
    pub(crate) fn target_value(&self, it: &mut E::Vm, t: Target<E>) -> V<E> {
        match t {
            Target::Window => Ok(E::obj_raw(*self.global.get().expect("installed"))),
            Target::Node(n) => self.wrap(it, n),
            Target::Obj(o) => Ok(E::obj_raw(o)),
        }
    }

    /// A new event object. `Some(detail)` makes a `CustomEvent`, including an undefined detail.
    pub(crate) fn new_event(&self, it: &mut E::Vm, ty: &str, bubbles: bool, cancelable: bool, composed: bool, detail: Option<E::Raw>) -> R<E, E::Obj> {
        let p = self.p();
        let o = self.new_event_proto(it, ty, bubbles, cancelable, composed, if detail.is_none() { p.event } else { p.custom })?;
        if let Some(d) = detail {
            it.store_value(&mut self.st.borrow_mut().events.get_mut(&o).expect("just made").detail, d);
        }
        Ok(o)
    }

    /// Like `new_event`, with the prototype given (a host event interface such as `MessageEvent`).
    pub(crate) fn new_event_proto(&self, it: &mut E::Vm, ty: &str, bubbles: bool, cancelable: bool, composed: bool, proto: E::Obj) -> R<E, E::Obj> {
        if ty.len() > self.lim.max_string_bytes {
            return Err(self.lim("dom-string"));
        }
        it.charge_o((ty.len() / 32) as u64)?;
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
            target: it.trace_value(E::null()),
            current: it.trace_value(E::null()),
            detail: it.trace_value(E::null()),
            path: Vec::new(),
            time: self.now_ms(),
            related: it.trace_value(E::null()),
            is_focus: false,
            is_submit: false,
            toggle: None,
            ui: None,
            trusted: false,
        };
        let mut st = self.st.borrow_mut();
        st.events.insert(o, e);
        let k = st.owners.key(crate::browser::Owner::Event(o));
        it.set_host_owner_obj(o, k);
        Ok(o)
    }

    /// Run the dispatch algorithm. Gives whether the event was not canceled.
    pub(crate) fn dispatch(&self, it: &mut E::Vm, target: Target<E>, ev: E::Obj) -> R<E, bool> {
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
        let prev = crate::browser_gaps::swap_event(self, Some(ev));
        let r = self.dispatch_inner(it, target, ev, &ty);
        let mut st = self.st.borrow_mut();
        st.hd.event = prev;
        let e = st.events.get_mut(&ev).expect("event is registered");
        let not_canceled = !e.canceled;
        (e.dispatching, e.stop, e.stop_imm, e.in_passive, e.phase) = (false, false, false, false, 0);
        it.store_value(&mut e.current, E::null());
        e.path.clear();
        r.map(|_| not_canceled)
    }

    fn dispatch_inner(&self, it: &mut E::Vm, target: Target<E>, ev: E::Obj, ty: &str) -> R<E, ()> {
        let mut path = vec![target];
        let mut quiet = false;
        if let Target::Node(n) = target {
            let doc = self.doc.borrow();
            let mut cur = n;
            while let Some(p) = doc.parent(cur) {
                it.charge_o(1)?;
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
                return Err(Fault::Unsupported("dispatch of a composed event across a shadow root (retargeting is not implemented)"));
            }
            quiet = !doc.has_on_name();
        }
        let tv = self.target_value(it, target)?;
        let bubbles = {
            let mut st = self.st.borrow_mut();
            let e = st.events.get_mut(&ev).expect("event is registered");
            it.store_value(&mut e.target, tv);
            e.path = path.clone();
            e.bubbles
        };
        // Content attributes `on<type>`: make the slots of the path up to date once, when the document has such a name (a change made by a
        // listener of this dispatch is seen by the next dispatch).
        if !quiet {
            crate::browser_handlers::sync_path_of(self, &path, ty);
        }
        for &t in path[1..].iter().rev() {
            self.fire(it, ev, ty, t, 1, true)?;
        }
        self.fire(it, ev, ty, target, 2, true)?;
        self.fire(it, ev, ty, target, 2, false)?;
        if bubbles {
            for &t in &path[1..] {
                self.fire(it, ev, ty, t, 3, false)?;
            }
        }
        Ok(())
    }

    /// The listeners of `t` for this event, capturing or not, one turn.
    fn fire(&self, it: &mut E::Vm, ev: E::Obj, ty: &str, t: Target<E>, phase: u8, capture: bool) -> R<E, ()> {
        // Most calls find no listener: they leave before the event is looked up. The callback is read at each turn, so the snapshot
        // keeps only the flags.
        let (first, rest) = {
            let st = self.st.borrow();
            let Some(listeners) = st.listeners.get(&t) else { return Ok(()) };
            // A plain loop: the iterator chain keeps `Cloned::next` out of line, and its cost moves with unrelated code.
            let mut first = None;
            let mut rest = Vec::new();
            for l in listeners {
                if l.capture == capture && *l.ty == *ty {
                    let turn = Turn { id: l.id, once: l.once, passive: l.passive, handler: l.handler };
                    if first.is_none() {
                        first = Some(turn);
                    } else {
                        rest.push(turn);
                    }
                }
            }
            (first, rest)
        };
        let Some(first) = first else {
            return Ok(());
        };
        self.fire_snapshot(it, ev, ty, t, phase, first, rest)
    }

    #[inline(never)]
    fn fire_snapshot(&self, it: &mut E::Vm, ev: E::Obj, ty: &str, t: Target<E>, phase: u8, first: Turn, rest: Vec<Turn>) -> R<E, ()> {
        if self.st.borrow().events[&ev].stop {
            return Ok(());
        }
        let cur = self.target_value(it, t)?;
        {
            let mut st = self.st.borrow_mut();
            let e = st.events.get_mut(&ev).expect("event is registered");
            e.phase = phase;
            it.store_value(&mut e.current, cur);
        }
        for l in std::iter::once(first).chain(rest) {
            let (mut cb, raw) = {
                let st = self.st.borrow();
                if st.events[&ev].stop_imm {
                    break;
                }
                // A property handler is called with its value at its turn (a replacement is seen, a removal skips it).
                let cb = match st.listeners.get(&t).and_then(|v| v.iter().find(|x| x.id == l.id)) {
                    Some(x) => it.traced_value(&x.cb),
                    None => continue,
                };
                (cb, l.handler && !st.hd.raw.is_empty() && st.hd.raw.contains_key(&l.id))
            };
            if raw {
                let f = crate::browser_handlers::compile_slot(self, it, t, ty, l.id)?;
                if matches!(E::kind(f), Val::Object(_)) {
                    cb = f;
                } else {
                    continue;
                }
            }
            if l.once {
                self.remove_listener(t, l.id);
            }
            self.st.borrow_mut().events.get_mut(&ev).expect("event is registered").in_passive = l.passive;
            it.charge_o(1)?;
            self.st.borrow_mut().ui.epoch += 1;
            let r = if l.handler { self.call_handler(it, t, ty, cb, cur, ev) } else { self.call_listener(it, cb, cur, E::obj_raw(ev)) };
            self.st.borrow_mut().events.get_mut(&ev).expect("event is registered").in_passive = false;
            match r {
                Ok(_) => {}
                Err(Fault::Throw(v)) => self.report(it, v, ty, t),
                Err(e) => return Err(e),
            }
            if self.st.borrow().driver {
                it.host_checkpoint()?;
            }
        }
        Ok(())
    }

    pub(crate) fn remove_listener(&self, t: Target<E>, id: u64) {
        let mut st = self.st.borrow_mut();
        if let Some(v) = st.listeners.get_mut(&t)
            && let Some(i) = v.iter().position(|l| l.id == id)
        {
            v.remove(i);
            if v.removable() {
                st.listeners.remove(&t);
            }
            st.n_listeners -= 1;
        }
    }

    /// HTML "event handler processing algorithm": a value that is not callable is skipped without an error, `this` is
    /// the current target, and a return value of `false` sets the canceled flag (as `preventDefault` would).
    /// Two cases differ: the `error` handler of `window` for an `ErrorEvent` gets `(message, filename, lineno, colno, error)` and `true`
    /// cancels; code of a content attribute looks its names up in the element, its form owner and the document first.
    fn call_handler(&self, it: &mut E::Vm, t: Target<E>, ty: &str, cb: E::Raw, this: E::Raw, ev: E::Obj) -> V<E> {
        let Val::Object(f) = E::kind(cb) else { return Ok(E::undef()) };
        if !it.is_callable_value(cb) {
            return Ok(E::undef());
        }
        if (matches!(t, Target::Window) && ty == "error") || it.is_scoped_function(f) {
            return self.call_handler_special(it, t, ty, f, this, ev);
        }
        let r = it.call_o(cb, this, &[E::obj_raw(ev)])?;
        if matches!(E::kind(r), Val::Bool(false)) && !(matches!(t, Target::Window) && ty == "beforeunload") {
            let mut st = self.st.borrow_mut();
            let e = st.events.get_mut(&ev).expect("event is registered");
            e.canceled |= e.cancelable && !e.in_passive;
        }
        Ok(r)
    }

    /// `call_handler` for the `error` handler of `window` or code of a content attribute.
    #[inline(never)]
    fn call_handler_special(&self, it: &mut E::Vm, t: Target<E>, ty: &str, f: E::Obj, this: E::Raw, ev: E::Obj) -> V<E> {
        let cb = E::obj_raw(f);
        let win_err = matches!(t, Target::Window) && ty == "error" && self.st.borrow().events[&ev].err;
        let scope = if it.is_scoped_function(f) { crate::browser_handlers::scope_of(self, it, t)? } else { Vec::new() };
        let mut args = [E::obj_raw(ev); 5];
        let n = if win_err {
            for (i, name) in ["message", "filename", "lineno", "colno", "error"].into_iter().enumerate() {
                let k = self.key(it, name)?;
                args[i] = it.get_value(E::obj_raw(ev), k)?;
            }
            5
        } else {
            1
        };
        let r = if scope.is_empty() { it.call_o(cb, this, &args[..n])? } else { it.call_scoped_o(cb, this, &args[..n], &scope)? };
        if matches!(E::kind(r), Val::Bool(b) if b == win_err) && !(matches!(t, Target::Window) && ty == "beforeunload") {
            let mut st = self.st.borrow_mut();
            let e = st.events.get_mut(&ev).expect("event is registered");
            e.canceled |= e.cancelable && !e.in_passive;
        }
        Ok(r)
    }

    fn call_listener(&self, it: &mut E::Vm, cb: E::Raw, this: E::Raw, ev: E::Raw) -> V<E> {
        if it.is_callable_value(cb) {
            return it.call_o(cb, this, &[ev]);
        }
        let k = self.key(it, "handleEvent")?;
        let f = it.get_value(cb, k)?;
        if !it.is_callable_value(f) {
            return Err(it.type_error_o("The event listener's handleEvent is not a function"));
        }
        it.call_o(f, cb, &[ev])
    }

    /// Keep the text of a listener exception. It reads data properties only, so no JS runs.
    pub(crate) fn report(&self, it: &mut E::Vm, thrown: E::Raw, ty: &str, t: Target<E>) {
        let message = self.error_text(it, thrown);
        let mut diagnostic = self.error_text_inner(it, thrown, true);
        if let Some(frames) = diagnostic.find("\n    at ") {
            diagnostic = format!("{message}{}", &diagnostic[frames..]);
            let cap = self.lim.max_string_bytes.min(4096);
            let mut end = diagnostic.len().min(cap);
            while !diagnostic.is_char_boundary(end) { end -= 1; }
            diagnostic.truncate(end);
        }
        self.window_error(it, thrown, &message);
        if self.lim.max_reports == 0 {
            let mut st = self.st.borrow_mut();
            st.dropped_reports = st.dropped_reports.saturating_add(1);
            return;
        }
        self.record_report(t, ty, diagnostic);
    }

    /// An uncaught exception: the `error` event of `window` (HTML "report an exception"), when something listens. An exception in it is not reported again.
    fn window_error(&self, it: &mut E::Vm, thrown: E::Raw, text: &str) {
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
    pub(crate) fn error_text(&self, it: &mut E::Vm, thrown: E::Raw) -> String {
        self.error_text_inner(it, thrown, false)
    }

    #[cold]
    fn error_text_inner(&self, it: &mut E::Vm, thrown: E::Raw, stack: bool) -> String {
        let cap = self.lim.max_string_bytes.min(4096);
        let append = |out: &mut String, units: &[u16]| {
            for c in char::decode_utf16(units.iter().copied()) {
                let c = c.unwrap_or(char::REPLACEMENT_CHARACTER);
                if c.len_utf8() > cap.saturating_sub(out.len()) { break; }
                out.push(c);
            }
        };
        let text = |it: &mut E::Vm, out: &mut String, o: E::Obj, key: &str| {
            let Ok(k) = it.h_key(key) else { return };
            if let Ok(Got::Value(v)) = it.h_get(o, k)
                && let Val::String(s) = E::kind(v)
                && let Ok(units) = it.h_str_units(s)
            {
                append(out, &units);
            }
        };
        let mut message = String::new();
        match E::kind(thrown) {
            Val::Object(o) => {
                if stack {
                    text(it, &mut message, o, "stack");
                    if !message.is_empty() { return message; }
                }
                text(it, &mut message, o, "name");
                append(&mut message, &[58, 32]);
                text(it, &mut message, o, "message");
            }
            Val::String(s) => if let Ok(units) = it.h_str_units(s) { append(&mut message, &units); },
            Val::Symbol(_) => append(&mut message, &[83, 121, 109, 98, 111, 108]),
            _ => append(&mut message, &it.display_o(thrown).encode_utf16().collect::<Vec<_>>()),
        }
        message
    }

    fn record_report(&self, t: Target<E>, ty: &str, message: String) {
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

pub(crate) fn init_flag_s<'s, E: Eng>(s: &mut Sc<'s, E>, init: Local<'s>, name: &str) -> Res<'s, bool> {
    s.get_bool_str(init, name)
}

fn make_event<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>, custom: bool) -> Res<'s, Local<'s>> {
    if a.is_empty() {
        return Err(s.type_error("Failed to construct 'Event': 1 argument required, but only 0 present."));
    }
    let ty = d.arg_str_s(s, a, 0)?;
    let init = arg_s::<E>(s, a, 1);
    let (mut bubbles, mut cancelable, mut composed, mut detail) = (false, false, false, None);
    match s.type_of(init) {
        Type::Undefined | Type::Null => {}
        Type::Object | Type::Function => {
            bubbles = init_flag_s::<E>(s, init, "bubbles")?;
            cancelable = init_flag_s::<E>(s, init, "cancelable")?;
            composed = init_flag_s::<E>(s, init, "composed")?;
            if custom {
                let v = s.get_str(init, "detail")?;
                detail = Some(if s.type_of(v) == Type::Undefined { s.null() } else { v });
            }
        }
        _ => return Err(s.type_error("The event initializer is not an object.")),
    }
    // A custom event passes its detail (`null` at least); a plain one passes `undefined`.
    let default = d.obj_local(s, if custom { d.p().custom } else { d.p().event });
    let proto = s.proto_from_new_target(default)?;
    let proto = obj_id::<E>(s, proto).expect("a prototype is an object");
    let e = old::<E, _>(s, |it| d.new_event_proto(it, &ty, bubbles, cancelable, composed, proto))?;
    if let Some(v) = detail {
        s.store(&mut d.st.borrow_mut().events.get_mut(&e).expect("just made").detail, v);
    }
    Ok(d.obj_local(s, e))
}

pub(crate) fn event_ctor<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    make_event(d, s, a, false)
}

pub(crate) fn custom_event_ctor<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    make_event(d, s, a, true)
}

pub(crate) fn event_target_ctor<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    if d.st.borrow().by_obj.len() >= d.lim.max_wrappers {
        return Err(d.lim_s(s, "dom-wrappers"));
    }
    let default = d.obj_local(s, d.p().et);
    let proto = s.proto_from_new_target(default)?;
    let proto = obj_id::<E>(s, proto).expect("a prototype is an object");
    let o = d.obj_s(s, proto)?;
    d.st.borrow_mut().add_target(s.engine(), o, crate::browser::Owner::Target(Target::Obj(o)));
    Ok(d.obj_local(s, o))
}

// ------------------------------------------------------------------------------------------ EventTarget

impl<E: Eng> Dom<E> {
    /// `target_of` on a local.
    fn target_of_s<'s>(&self, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, Target<E>> {
        match obj_id::<E>(s, this) {
            Some(o) => match self.st.borrow().by_obj.get(&o) {
                Some(&t) => Ok(t),
                None => Err(s.type_error("Illegal invocation")),
            },
            None if is_nullish::<E>(s, this) => Ok(Target::Window),
            None => Err(s.type_error("Illegal invocation")),
        }
    }

    /// `target_value` as a local.
    pub(crate) fn target_local<'s>(&self, s: &mut Sc<'s, E>, t: Target<E>) -> Res<'s, Local<'s>> {
        match t {
            Target::Window => Ok(s.global()),
            Target::Node(n) => self.wrap_s(s, n),
            Target::Obj(o) => Ok(self.obj_local(s, o)),
        }
    }

    /// `event_of` on a local.
    pub(crate) fn event_of_s<'s>(&self, s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, E::Obj> {
        match obj_id::<E>(s, v).filter(|o| self.st.borrow().events.contains_key(o)) {
            Some(o) => Ok(o),
            None => Err(s.type_error("Illegal invocation")),
        }
    }
}

pub(crate) fn add_event_listener<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let t = d.target_of_s(s, this)?;
    let ty = d.arg_str_s(s, a, 0)?;
    let cbl = arg_s::<E>(s, a, 1);
    let cb = match obj_id::<E>(s, cbl) {
        Some(o) => o,
        None if is_nullish::<E>(s, cbl) => return Ok(s.undefined()),
        None => return Err(s.type_error("Failed to execute 'addEventListener': parameter 2 is not of type 'Object'.")),
    };
    let (mut once, mut passive, mut signal) = (false, false, None);
    let opts = arg_s::<E>(s, a, 2);
    let capture;
    if is_object::<E>(s, opts) {
        capture = init_flag_s::<E>(s, opts, "capture")?;
        once = init_flag_s::<E>(s, opts, "once")?;
        passive = init_flag_s::<E>(s, opts, "passive")?;
        let sg = s.get_str(opts, "signal")?;
        if s.type_of(sg) != Type::Undefined {
            match obj_id::<E>(s, sg).filter(|&o| d.is_signal(o)) {
                Some(o) => signal = Some(o),
                None => return Err(s.type_error("Failed to execute 'addEventListener' on 'EventTarget': Failed to read the 'signal' property from 'AddEventListenerOptions': Failed to convert value to 'AbortSignal'.")),
            }
        }
    } else {
        capture = s.to_boolean(opts);
    }
    if let Some(sg) = signal
        && d.signal_state(sg).is_some_and(|x| x.0)
    {
        return Ok(s.undefined());
    }
    crate::browser_handlers::sync_all(d, t);
    let traced = s.trace_ref(cbl);
    let mut guard = d.st.borrow_mut();
    let st = &mut *guard;
    let list = st.listeners.entry(t).or_default();
    if list.iter().any(|l| *l.ty == *ty && !l.handler && matches!(E::kind(*l.cb.raw()), Val::Object(x) if x == cb) && l.capture == capture) {
        return Ok(s.undefined());
    }
    if st.n_listeners >= d.lim.max_listeners {
        return Err(d.lim_s(s, "dom-listeners"));
    }
    if list.len() >= d.lim.max_listeners_per_target {
        return Err(d.lim_s(s, "dom-listeners-per-target"));
    }
    st.next_id += 1;
    let id = st.next_id;
    list.push(Listener { id, ty: Rc::from(ty.as_str()), cb: traced, capture, once, passive, handler: false });
    st.n_listeners += 1;
    d.own(&mut st.owners, s.engine(), t, list);
    drop(guard);
    if let Some(sg) = signal
        && let Err(e) = d.signal_add_algo(sg, crate::browser_abort::Algo::Remove(t, id))
    {
        d.remove_listener(t, id);
        return Err(lift_err::<E>(s, e));
    }
    Ok(s.undefined())
}

pub(crate) fn remove_event_listener<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let t = d.target_of_s(s, this)?;
    let ty = d.arg_str_s(s, a, 0)?;
    let cbl = arg_s::<E>(s, a, 1);
    let Some(cb) = obj_id::<E>(s, cbl) else { return Ok(s.undefined()) };
    let opts = arg_s::<E>(s, a, 2);
    let capture = if is_object::<E>(s, opts) { init_flag_s::<E>(s, opts, "capture")? } else { s.to_boolean(opts) };
    let found = d.st.borrow().listeners.get(&t).and_then(|v| v.iter().find(|l| !l.handler && *l.ty == *ty && matches!(E::kind(*l.cb.raw()), Val::Object(x) if x == cb) && l.capture == capture).map(|l| l.id));
    if let Some(id) = found {
        d.remove_listener(t, id);
    }
    Ok(s.undefined())
}

pub(crate) fn dispatch_event<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let t = d.target_of_s(s, this)?;
    let el = arg_s::<E>(s, a, 0);
    let Some(e) = obj_id::<E>(s, el).filter(|e| d.st.borrow().events.contains_key(e)) else {
        return Err(s.type_error("Failed to execute 'dispatchEvent': parameter 1 is not of type 'Event'."));
    };
    let not_canceled = old::<E, _>(s, |it| d.dispatch(it, t, e))?;
    Ok(s.boolean(not_canceled))
}

// ------------------------------------------------------------------------------------------ Event

fn with_event<'s, T, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, f: impl FnOnce(&mut EventState<E>) -> T) -> Res<'s, T> {
    let o = d.event_of_s(s, this)?;
    Ok(f(d.st.borrow_mut().events.get_mut(&o).expect("checked")))
}

/// A traced value of the event `this`.
pub(crate) fn event_value<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, f: impl FnOnce(&EventState<E>) -> &bare_js_api::Traced<E::Raw>) -> Res<'s, Local<'s>> {
    let o = d.event_of_s(s, this)?;
    let st = d.st.borrow();
    Ok(s.traced(f(&st.events[&o])))
}

/// A boolean field of the event `this`.
fn event_flag<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, f: impl FnOnce(&mut EventState<E>) -> bool) -> Res<'s, Local<'s>> {
    let b = with_event(d, s, this, f)?;
    Ok(s.boolean(b))
}

pub(crate) fn event_type<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let ty = with_event(d, s, this, |e| e.ty.clone())?;
    d.js_str_s(s, &ty)
}

pub(crate) fn event_target<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    event_value(d, s, this, |e| &e.target)
}

pub(crate) fn event_current_target<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    event_value(d, s, this, |e| &e.current)
}

pub(crate) fn event_phase<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let p = with_event(d, s, this, |e| e.phase)?;
    Ok(s.number(p as f64))
}

pub(crate) fn event_bubbles<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    event_flag(d, s, this, |e| e.bubbles)
}

pub(crate) fn event_cancelable<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    event_flag(d, s, this, |e| e.cancelable)
}

pub(crate) fn event_composed<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    event_flag(d, s, this, |e| e.composed)
}

pub(crate) fn event_default_prevented<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    event_flag(d, s, this, |e| e.canceled)
}

/// True for the events of the window input (`browser_ui.rs`), false for all others.
pub(crate) fn event_is_trusted<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    event_flag(d, s, this, |e| e.trusted)
}

pub(crate) fn event_time_stamp<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let t = with_event(d, s, this, |e| e.time)?;
    Ok(s.number(t))
}

/// `Event.srcElement`: the legacy name of `target`.
pub(crate) fn event_src_element<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    event_target(d, s, this, a)
}

/// `Event.returnValue`: `!defaultPrevented`. Setting it to `false` is `preventDefault()`; `true` does nothing.
pub(crate) fn event_return_value<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    event_flag(d, s, this, |e| !e.canceled)
}

pub(crate) fn set_event_return_value<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let v = arg_s::<E>(s, a, 0);
    let keep = s.to_boolean(v);
    with_event(d, s, this, |e| e.canceled |= !keep && e.cancelable && !e.in_passive)?;
    Ok(s.undefined())
}

/// `Event.cancelBubble`: whether propagation is stopped. Setting it to `true` is `stopPropagation()`; `false` does nothing.
pub(crate) fn event_cancel_bubble<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    event_flag(d, s, this, |e| e.stop)
}

pub(crate) fn set_event_cancel_bubble<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let v = arg_s::<E>(s, a, 0);
    let on = s.to_boolean(v);
    with_event(d, s, this, |e| e.stop |= on)?;
    Ok(s.undefined())
}

pub(crate) fn prevent_default<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    with_event(d, s, this, |e| e.canceled |= e.cancelable && !e.in_passive)?;
    Ok(s.undefined())
}

pub(crate) fn stop_propagation<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    with_event(d, s, this, |e| e.stop = true)?;
    Ok(s.undefined())
}

pub(crate) fn stop_immediate_propagation<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    with_event(d, s, this, |e| (e.stop, e.stop_imm) = (true, true))?;
    Ok(s.undefined())
}

pub(crate) fn composed_path<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let path = with_event(d, s, this, |e| e.path.clone())?;
    let mut vals = Vec::with_capacity(path.len());
    for t in path {
        vals.push(d.target_local(s, t)?);
    }
    s.new_array(&vals)
}

pub(crate) fn custom_detail<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    event_value(d, s, this, |e| &e.detail)
}

// ------------------------------------------------------------------------------------------ createEvent

/// `document.createEvent(name)` (DOM Standard, legacy): an event with an empty type that `initEvent` or `initCustomEvent` sets. The name is
/// not case sensitive. `UIEvent` and `UIEvents` give a plain `Event`, because bare has no `UIEvent` interface. The event is not marked
/// uninitialized: dispatching it before `initEvent` sends an event of type `""`.
pub(crate) fn create_event<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    if a.is_empty() {
        return Err(s.type_error("Failed to execute 'createEvent' on 'Document': 1 argument required, but only 0 present."));
    }
    let name = d.arg_str_s(s, a, 0)?;
    let o = match name.to_ascii_lowercase().as_str() {
        "event" | "events" | "htmlevents" | "svgevents" | "uievent" | "uievents" => old::<E, _>(s, |it| d.new_event(it, "", false, false, false, None))?,
        "customevent" => old::<E, _>(s, |it| d.new_event(it, "", false, false, false, Some(E::null())))?,
        "mouseevent" | "mouseevents" => return crate::browser_uievent::ctor_empty::<0, _>(d, s),
        "keyboardevent" => return crate::browser_uievent::ctor_empty::<2, _>(d, s),
        _ => return Err(d.dom_err_s(s, "NotSupportedError", &format!("Failed to execute 'createEvent' on 'Document': The provided event type ('{name}') is invalid."))),
    };
    Ok(d.obj_local(s, o))
}

/// `event.initEvent(type, bubbles, cancelable)` and, with a fourth argument, `initCustomEvent`. Nothing happens while the event dispatches.
fn init<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>, what: &str, detail: bool) -> Res<'s, Local<'s>> {
    if a.is_empty() {
        return Err(s.type_error(&format!("Failed to execute '{what}' on '{}': 1 argument required, but only 0 present.", if detail { "CustomEvent" } else { "Event" })));
    }
    let ty = d.arg_str_s(s, a, 0)?;
    let (b, c) = (arg_s::<E>(s, a, 1), arg_s::<E>(s, a, 2));
    let (bubbles, cancelable) = (s.to_boolean(b), s.to_boolean(c));
    let o = d.event_of_s(s, this)?;
    let null = s.null();
    let dl = if detail {
        let v = arg_s::<E>(s, a, 3);
        if s.type_of(v) == Type::Undefined { null } else { v }
    } else {
        null
    };
    let mut st = d.st.borrow_mut();
    let e = st.events.get_mut(&o).expect("checked");
    if !e.dispatching {
        (e.ty, e.bubbles, e.cancelable) = (Rc::from(ty.as_str()), bubbles, cancelable);
        (e.stop, e.stop_imm, e.canceled, e.trusted) = (false, false, false, false);
        s.store(&mut e.target, null);
        if detail {
            s.store(&mut e.detail, dl);
        }
    }
    Ok(s.undefined())
}

pub(crate) fn init_event<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    init(d, s, this, a, "initEvent", false)
}

pub(crate) fn init_custom_event<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    init(d, s, this, a, "initCustomEvent", true)
}
