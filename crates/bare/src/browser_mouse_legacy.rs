//! The legacy members of `UIEvent` and `MouseEvent` that Closure and Gmail read: `which`, `sourceCapabilities`, `layerX`, `layerY`,
//! `fromElement`, `toElement` and `initMouseEvent`. Sources: UI Events (`UIEvent.which`, `sourceCapabilities`, `initMouseEvent`), CSSOM View
//! (`MouseEvent`), and the members that Chrome keeps for old pages. Every member is computed in its getter: an event stores nothing for them.
//! They are defined with the other late members (`ensure_ui_ext` in `browser_uievent.rs`).
//!
//! - bare has no `UIEvent` interface, so `which` and `sourceCapabilities` sit on `MouseEvent`, `KeyboardEvent` and `InputEvent`. The
//!   `KeyboardEvent.which` getter (the key code) is in `browser.rs`.
//! - `MouseEvent.which` is `button + 1` for `mousedown`, `mouseup`, `click`, `dblclick`, `auxclick` and `contextmenu`, and 0 for the other
//!   types. `InputEvent.which` is 0.
//! - `sourceCapabilities` is always `null`. Deviation: Chrome gives an `InputDeviceCapabilities`. bare has no such interface.
//! - `layerX` and `layerY` are measured from the border box of the nearest ancestor whose `position` is not `static`, and from the page when
//!   there is none. Deviation: Chrome measures from the nearest paint layer, which a transform, an opacity or a `will-change` also makes.
//! - `fromElement` and `toElement`: `mouseover` and `mouseenter` give the `relatedTarget` and the target. `mouseout` and `mouseleave` give
//!   the target and the `relatedTarget`. Every other type gives `null`.
//! - `initMouseEvent` does nothing while the event dispatches, and it does not read a `PointerEvent` pointer id.

use crate::browser::*;
use crate::browser_ui::{coord, with_ui};
use bare_css::values::{Keyword, Property, Value as Cv};
use bare_js::eval::Interp;
use bare_js::value::Value;
use std::rc::Rc;

type V = R<Value>;

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

/// `which` of a mouse event.
pub(crate) fn mouse_which(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = with_ui(d, it, this, |e, u| match &*e.ty {
        "mousedown" | "mouseup" | "click" | "dblclick" | "auxclick" | "contextmenu" => (u.button as i32 + 1).max(0),
        _ => 0,
    })?;
    Ok(Value::Number(n as f64))
}

/// `which` of an input event: 0.
pub(crate) fn zero(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    with_ui(d, it, this, |_, _| Value::Number(0.0))
}

pub(crate) fn source_capabilities(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    with_ui(d, it, this, |_, _| Value::Null)
}

fn layer(d: &Dom, it: &mut Interp<'_>, this: Value, y: bool) -> V {
    let target = crate::browser_ui::ui_value(d, it, this, |e| Some(&e.target))?;
    let u = with_ui(d, it, this, |_, u| u.clone())?;
    let mut v = if y { u.page.1 } else { u.page.0 };
    if let Some(n) = d.node_of(target).filter(|&n| d.is_element(n)) && d.in_document(it, n)?
        && let Some(r) = d.with_geo(it, |g, s| {
            let mut cur = g.doc.parent(n);
            while let Some(a) = cur {
                if g.styles.style(a).is_some_and(|st| st.get(Property::Position) != Cv::Keyword(Keyword::Static)) {
                    return g.client_rect(s, a);
                }
                cur = g.doc.parent(a);
            }
            None
        })? {
        v = if y { u.y - r.y as f64 } else { u.x - r.x as f64 };
    }
    Ok(Value::Number(coord(v, u.frac)))
}

pub(crate) fn layer_x(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    layer(d, it, this, false)
}

pub(crate) fn layer_y(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    layer(d, it, this, true)
}

/// `fromElement` (`to` false) and `toElement` (`to` true).
fn from_to(d: &Dom, it: &mut Interp<'_>, this: Value, to: bool) -> V {
    crate::browser_ui::ui_value(d, it, this, |e| match (&*e.ty, to) {
        ("mouseover" | "mouseenter", false) | ("mouseout" | "mouseleave", true) => Some(&e.related),
        ("mouseover" | "mouseenter", true) | ("mouseout" | "mouseleave", false) => Some(&e.target),
        _ => None,
    })
}

pub(crate) fn from_element(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    from_to(d, it, this, false)
}

pub(crate) fn to_element(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    from_to(d, it, this, true)
}

/// `event.initMouseEvent(type, bubbles, cancelable, view, detail, screenX, screenY, clientX, clientY, ctrlKey, altKey, shiftKey, metaKey,
/// button, relatedTarget)`.
pub(crate) fn init_mouse_event(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    with_ui(d, it, this, |_, _| ())?;
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'initMouseEvent' on 'MouseEvent': 1 argument required, but only 0 present."));
    }
    let ty = d.string_arg(it, a[0])?;
    let (bubbles, cancelable) = (it.to_js_bool(arg(a, 1))?, it.to_js_bool(arg(a, 2))?);
    let view = match arg(a, 3) {
        Value::Undefined | Value::Null => false,
        Value::Object(o) if o == it.global_object() => true,
        _ => return Err(it.type_error("Failed to execute 'initMouseEvent' on 'MouseEvent': Failed to convert value to 'Window'.")),
    };
    let mut n = [0.0f64; 5];
    for (i, slot) in n.iter_mut().enumerate() {
        let v = it.to_js_number(arg(a, 4 + i))?;
        *slot = if v.is_finite() { v.trunc() } else { 0.0 };
    }
    let mut f = [false; 4];
    for (i, slot) in f.iter_mut().enumerate() {
        *slot = it.to_js_bool(arg(a, 9 + i))?;
    }
    let button = it.to_js_number(arg(a, 13))?;
    let related = match arg(a, 14) {
        Value::Undefined | Value::Null => Value::Null,
        v @ Value::Object(o) if d.st.borrow().by_obj.contains_key(&o) => v,
        _ => return Err(it.type_error("Failed to execute 'initMouseEvent' on 'MouseEvent': Failed to convert value to 'EventTarget'.")),
    };
    let off = if view { d.page_offset() } else { (0.0, 0.0) };
    let o = d.event_of(it, this)?;
    let mut st = d.st.borrow_mut();
    let e = st.events.get_mut(&o).expect("checked");
    if e.dispatching {
        return Ok(Value::Undefined);
    }
    (e.ty, e.bubbles, e.cancelable) = (Rc::from(ty.as_str()), bubbles, cancelable);
    (e.stop, e.stop_imm, e.canceled) = (false, false, false);
    it.store_value(&mut e.target, Value::Null);
    it.store_value(&mut e.related, related);
    let u = e.ui.as_mut().expect("checked");
    u.view = view;
    u.detail = n[0] as i32;
    u.screen = (n[1], n[2]);
    (u.x, u.y) = (n[3], n[4]);
    u.page = (n[3] + off.0, n[4] + off.1);
    (u.mods.ctrl, u.mods.alt, u.mods.shift, u.mods.meta) = (f[0], f[1], f[2], f[3]);
    u.button = if button.is_finite() { button.trunc() as i64 as i16 } else { 0 };
    Ok(Value::Undefined)
}
