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

use crate::eng::Eng;
use crate::browser::*;
use crate::browser_scoped::*;
use crate::browser_ui::{coord, ui_value, with_ui};
use bare_css::values::{Keyword, Property, Value as Cv};
use bare_js_api::{Args, Local};
use std::rc::Rc;

/// `which` of a mouse event.
pub(crate) fn mouse_which<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = with_ui(d, s, this, |e, u| match &*e.ty {
        "mousedown" | "mouseup" | "click" | "dblclick" | "auxclick" | "contextmenu" => (u.button as i32 + 1).max(0),
        _ => 0,
    })?;
    Ok(s.number(n as f64))
}

/// `which` of an input event: 0.
pub(crate) fn zero<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    with_ui(d, s, this, |_, _| ())?;
    Ok(s.number(0.0))
}

pub(crate) fn source_capabilities<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    with_ui(d, s, this, |_, _| ())?;
    Ok(s.null())
}

fn layer<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, y: bool) -> Res<'s, Local<'s>> {
    let target = ui_value(d, s, this, |e| Some(&e.target))?;
    let u = with_ui(d, s, this, |_, u| u.clone())?;
    let mut v = if y { u.page.1 } else { u.page.0 };
    if let Some(n) = d.node_of_s(s, target).filter(|&n| d.is_element(n)) && old::<E, _>(s, |it| d.in_document(it, n))?
        && let Some(r) = old::<E, _>(s, |it| d.with_geo(it, |g, st| {
            let mut cur = g.doc.parent(n);
            while let Some(a) = cur {
                if g.styles.style(a).is_some_and(|x| x.get(Property::Position) != Cv::Keyword(Keyword::Static)) {
                    return g.client_rect(st, a);
                }
                cur = g.doc.parent(a);
            }
            None
        }))? {
        v = if y { u.y - r.y as f64 } else { u.x - r.x as f64 };
    }
    Ok(s.number(coord(v, u.frac)))
}

pub(crate) fn layer_x<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    layer(d, s, this, false)
}

pub(crate) fn layer_y<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    layer(d, s, this, true)
}

/// `fromElement` (`to` false) and `toElement` (`to` true).
fn from_to<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, to: bool) -> Res<'s, Local<'s>> {
    ui_value(d, s, this, |e| match (&*e.ty, to) {
        ("mouseover" | "mouseenter", false) | ("mouseout" | "mouseleave", true) => Some(&e.related),
        ("mouseover" | "mouseenter", true) | ("mouseout" | "mouseleave", false) => Some(&e.target),
        _ => None,
    })
}

pub(crate) fn from_element<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    from_to(d, s, this, false)
}

pub(crate) fn to_element<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    from_to(d, s, this, true)
}

/// `event.initMouseEvent(type, bubbles, cancelable, view, detail, screenX, screenY, clientX, clientY, ctrlKey, altKey, shiftKey, metaKey,
/// button, relatedTarget)`.
pub(crate) fn init_mouse_event<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    with_ui(d, s, this, |_, _| ())?;
    if a.is_empty() {
        return Err(s.type_error("Failed to execute 'initMouseEvent' on 'MouseEvent': 1 argument required, but only 0 present."));
    }
    let ty = d.arg_str_s(s, a, 0)?;
    let (b, c) = (arg_s::<E>(s, a, 1), arg_s::<E>(s, a, 2));
    let (bubbles, cancelable) = (s.to_boolean(b), s.to_boolean(c));
    let vw = arg_s::<E>(s, a, 3);
    let view = if is_nullish::<E>(s, vw) {
        false
    } else if d.is_obj_s(s, vw, *d.global.get().expect("installed")) {
        true
    } else {
        return Err(s.type_error("Failed to execute 'initMouseEvent' on 'MouseEvent': Failed to convert value to 'Window'."));
    };
    let mut n = [0.0f64; 5];
    for (i, slot) in n.iter_mut().enumerate() {
        let x = arg_s::<E>(s, a, 4 + i);
        let v = s.to_number(x)?;
        *slot = if v.is_finite() { v.trunc() } else { 0.0 };
    }
    let mut f = [false; 4];
    for (i, slot) in f.iter_mut().enumerate() {
        let x = arg_s::<E>(s, a, 9 + i);
        *slot = s.to_boolean(x);
    }
    let bl = arg_s::<E>(s, a, 13);
    let button = s.to_number(bl)?;
    let rl = arg_s::<E>(s, a, 14);
    let related = match obj_id::<E>(s, rl) {
        Some(o) if d.st.borrow().by_obj.contains_key(&o) => rl,
        None if is_nullish::<E>(s, rl) => s.null(),
        _ => return Err(s.type_error("Failed to execute 'initMouseEvent' on 'MouseEvent': Failed to convert value to 'EventTarget'.")),
    };
    let null = s.null();
    let off = if view { d.page_offset() } else { (0.0, 0.0) };
    let o = d.event_of_s(s, this)?;
    let mut st = d.st.borrow_mut();
    let e = st.events.get_mut(&o).expect("checked");
    if e.dispatching {
        return Ok(s.undefined());
    }
    (e.ty, e.bubbles, e.cancelable) = (Rc::from(ty.as_str()), bubbles, cancelable);
    (e.stop, e.stop_imm, e.canceled) = (false, false, false);
    s.store(&mut e.target, null);
    s.store(&mut e.related, related);
    let u = e.ui.as_mut().expect("checked");
    u.view = view;
    u.detail = n[0] as i32;
    u.screen = (n[1], n[2]);
    (u.x, u.y) = (n[3], n[4]);
    u.page = (n[3] + off.0, n[4] + off.1);
    (u.mods.ctrl, u.mods.alt, u.mods.shift, u.mods.meta) = (f[0], f[1], f[2], f[3]);
    u.button = if button.is_finite() { button.trunc() as i64 as i16 } else { 0 };
    Ok(s.undefined())
}
