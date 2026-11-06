//! `oninput` and `onchange`, the event handler IDL attributes of `GlobalEventHandlers` (HTML Standard, "event handlers").
//!
//! - Where: `Window` (own accessors of the global object), `Document.prototype`, `HTMLElement.prototype` and
//!   `SVGElement.prototype`; `onabort` is on `AbortSignal.prototype` only. Each interface has its own getter and setter function. MathML has no `MathMLElement` here
//!   (`browser_namespace.rs`), so a MathML element has no property. Text, Node, Element and a plain `EventTarget` have none.
//! - Storage: the handler is one `Listener` of the owner with `handler: true` and `cb` = the current value. No side table.
//!   Setting a callable or another object stores it. A non-callable object is kept and skipped when the event runs, as the
//!   standard says (`[LegacyTreatNonObjectAsNull]` only turns non-objects into null). Anything else (including a string,
//!   which is never compiled) means null. Setting an existing handler changes `cb` in place (the slot keeps its order).
//!   Null removes the listener ("deactivate"); a later non-null value appends a new one ("activate").
//! - The handler is never the same listener as `addEventListener(type, sameFunction)`: both run, and
//!   `removeEventListener` does not remove the handler.
//! - Dispatch (`browser_events.rs`, `call_handler`): `this` is the current target, `false` cancels a cancelable event, a throw
//!   is reported, a Limit/Unsupported/heap fault ends the dispatch. The list is copied at the start of a turn; the handler
//!   value is read at its own turn.
//! - Caps: a new slot counts against `max_listeners` and `max_listeners_per_target` before anything changes.
//! - Not done: `onerror`, content attributes (`<input oninput="...">`), the other `on*` handlers, and the `body`/`frameset`
//!   forwarding of window handlers. `MediaQueryList.onchange` and `MessagePort.onmessage` keep their own code (they treat a
//!   non-callable object as null and share a slot with `addEventListener` of the same function).

use crate::browser::*;
use bare_html::dom::{NodeData, Ns};
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};
use std::rc::Rc;

type V = R<Value>;

const WINDOW: u8 = 0;
const DOCUMENT: u8 = 1;
const HTML: u8 = 2;
const SVG: u8 = 3;
const SIGNAL: u8 = 4;
const NAMES: [&str; 3] = ["oninput", "onchange", "onabort"];
const TYPES: [&str; 3] = ["input", "change", "abort"];

impl Dom {
    /// The owner of a handler of interface `K`, or `Illegal invocation`. Window also takes `undefined` and `null`.
    fn handler_owner<const K: u8>(&self, it: &mut Interp<'_>, this: Value) -> R<Target> {
        let t = match this {
            Value::Undefined | Value::Null if K == WINDOW => Some(Target::Window),
            Value::Object(o) => self.st.borrow().by_obj.get(&o).copied(),
            _ => None,
        };
        let ok = match t {
            Some(Target::Window) => K == WINDOW,
            Some(Target::Obj(o)) => K == SIGNAL && self.is_signal(o),
            Some(Target::Node(n)) => {
                let doc = self.doc.borrow();
                match (doc.data(n), K) {
                    (NodeData::Document, DOCUMENT) => true,
                    (NodeData::Element { .. }, HTML) => doc.element_ns(n) == Ns::Html,
                    (NodeData::Element { .. }, SVG) => doc.element_ns(n) == Ns::Svg,
                    _ => false,
                }
            }
            _ => false,
        };
        match t {
            Some(t) if ok => Ok(t),
            _ => Err(it.type_error("Illegal invocation")),
        }
    }
}

fn get<const K: u8, const N: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let t = d.handler_owner::<K>(it, this)?;
    let st = d.st.borrow();
    Ok(st.listeners.get(&t).and_then(|v| v.iter().find(|l| l.handler && *l.ty == *TYPES[N])).map_or(Value::Null, |l| Value::Object(l.cb)))
}

fn set<const K: u8, const N: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let t = d.handler_owner::<K>(it, this)?;
    let cb: Option<ObjId> = match a.first() {
        Some(Value::Object(o)) => Some(*o),
        _ => None,
    };
    let mut st = d.st.borrow_mut();
    let st = &mut *st;
    let at = st.listeners.get(&t).and_then(|v| v.iter().position(|l| l.handler && *l.ty == *TYPES[N]));
    match (at, cb) {
        (Some(i), Some(cb)) => st.listeners.get_mut(&t).expect("found")[i].cb = cb,
        (Some(i), None) => {
            let v = st.listeners.get_mut(&t).expect("found");
            v.remove(i);
            if v.is_empty() {
                st.listeners.remove(&t);
            }
            st.n_listeners -= 1;
        }
        (None, Some(cb)) => {
            if st.n_listeners >= d.lim.max_listeners {
                return Err(d.lim("dom-listeners"));
            }
            if st.listeners.get(&t).map_or(0, Vec::len) >= d.lim.max_listeners_per_target {
                return Err(d.lim("dom-listeners-per-target"));
            }
            st.next_id += 1;
            let id = st.next_id;
            st.listeners.entry(t).or_default().push(Listener { id, ty: Rc::from(TYPES[N]), cb, capture: false, once: false, passive: false, handler: true });
            st.n_listeners += 1;
        }
        (None, None) => {}
    }
    Ok(Value::Undefined)
}

/// `onabort` of `AbortSignal.prototype`, made with the lazy `AbortSignal` (`browser_abort.rs`).
pub(crate) fn install_onabort(d: &Dom, it: &mut Interp<'_>, proto: ObjId) -> R<()> {
    d.def_get(it, proto, NAMES[2], get::<SIGNAL, 2>, Some(set::<SIGNAL, 2>))
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let p = d.p();
    let g = *d.global.get().expect("installed");
    let owners: [(ObjId, [(H, H); 2]); 4] = [
        (g, [(get::<WINDOW, 0>, set::<WINDOW, 0>), (get::<WINDOW, 1>, set::<WINDOW, 1>)]),
        (p.document, [(get::<DOCUMENT, 0>, set::<DOCUMENT, 0>), (get::<DOCUMENT, 1>, set::<DOCUMENT, 1>)]),
        (p.html, [(get::<HTML, 0>, set::<HTML, 0>), (get::<HTML, 1>, set::<HTML, 1>)]),
        (p.svg, [(get::<SVG, 0>, set::<SVG, 0>), (get::<SVG, 1>, set::<SVG, 1>)]),
    ];
    for (owner, fns) in owners {
        for (n, (gh, sh)) in fns.into_iter().enumerate() {
            let key = d.key(it, NAMES[n])?;
            let gf = d.add_fn(it, gh, &format!("get {}", NAMES[n]), 0, false)?;
            let sf = d.add_fn(it, sh, &format!("set {}", NAMES[n]), 1, false)?;
            d.define_accessor(it, owner, key, gf, Some(sf))?;
        }
    }
    Ok(())
}
