//! `new MouseEvent`, `new PointerEvent`, `new KeyboardEvent` and `new InputEvent`, the members that the window events did not need, and
//! pointer capture. Sources: UI Events (the four interfaces, `EventModifierInit`), Pointer Events 3 (`PointerEvent`, capture), Input Events
//! Level 2 (`inputType`). Every answer was checked against headless Chrome 155 (`tools/fixtures/ui/uievent-cases.txt`).
//!
//! - An event that script makes is not trusted and has no default action: `dispatchEvent(new KeyboardEvent('keydown', {key: 'a'}))` types
//!   nothing. The same `Ui` record holds the fields of these events and of the events that the window makes (`browser_ui.rs`).
//! - A `MouseEvent` floors its coordinates and a `PointerEvent` keeps the fraction (Chrome). `pageX` is `clientX` plus the scroll offset of the
//!   window when `view` is the window. `offsetX` is read when asked: `clientX` minus the left of the target's border box.
//!   Deviation: Chrome measures from the padding edge; the two differ by the border width.
//! - `inputType` keeps only the names that Chrome knows (the list below); any other value reads as `""`.
//! - The members after `ensure_ui_ext` (`offsetX`, `movementX`, `getModifierState`, the `PointerEvent` sizes, `charCode`, the constants, `view`,
//!   `getCoalescedEvents`, the three pointer capture methods) are defined on the first constructor call or the first event of the window or
//!   the driver, not at start: a page that never makes such an event pays nothing for them. Deviation: before that, `'hasPointerCapture' in
//!   Element.prototype` is false.
//! - Pointer capture (`setPointerCapture`, `hasPointerCapture`, `releasePointerCapture`): only the pointer 1 (the mouse) exists. The call
//!   does nothing while no button is down. The events and the retargeting are in `browser_ui.rs`.
//! - `UIEvent` and `WheelEvent` are not built: `view` and `detail` live on the four interfaces. `initMouseEvent` and the other `init*Event`
//!   methods are not built. A `PointerEvent` of the window has `width`, `height` 1 and `pressure` 0.5 while a button is down.

use crate::eng::{Eng, Ops, Val};
use crate::browser::*;
use crate::browser_events::init_flag_s;
use crate::browser_mouse_legacy as lg;
use crate::browser_scoped::*;
use crate::browser_ui::{PointerData, Ui, coord, with_ui};
use bare_js_api::{Args, HostFn, Local, Type};

const MOUSE: u8 = 0;
const POINTER: u8 = 1;
const KEYBOARD: u8 = 2;
const INPUT: u8 = 3;
const IFACE: [&str; 4] = ["MouseEvent", "PointerEvent", "KeyboardEvent", "InputEvent"];

/// The modifier keys that have no `...Key` member: the bit of each is the index here. `getModifierState` takes the name after `modifier`.
const EXTRA_MODS: [&str; 10] = ["AltGraph", "CapsLock", "Fn", "FnLock", "Hyper", "NumLock", "ScrollLock", "Super", "Symbol", "SymbolLock"];

/// The `inputType` values of Chrome 155 (Input Events Level 2, less the ones Chrome leaves out).
const INPUT_TYPES: [&str; 39] = [
    "insertText", "insertReplacementText", "insertLineBreak", "insertParagraph", "insertOrderedList", "insertUnorderedList", "insertHorizontalRule",
    "insertFromYank", "insertFromDrop", "insertFromPaste", "insertTranspose", "insertCompositionText", "insertLink", "deleteWordBackward",
    "deleteWordForward", "deleteSoftLineBackward", "deleteSoftLineForward", "deleteHardLineBackward", "deleteHardLineForward", "deleteByDrag",
    "deleteByCut", "deleteContentBackward", "deleteContentForward", "historyUndo", "historyRedo", "formatBold", "formatItalic", "formatUnderline",
    "formatStrikeThrough", "formatSuperscript", "formatSubscript", "formatJustifyFull", "formatJustifyCenter", "formatJustifyRight",
    "formatJustifyLeft", "formatIndent", "formatOutdent", "formatRemove", "formatSetBlockTextDirection",
];

fn bad<'s, E: Eng>(s: &mut Sc<'s, E>, iface: &str, what: &str) -> bare_js_api::Error<'s> {
    s.type_error(&format!("Failed to construct '{iface}': {what}"))
}

/// The member `name` of the init dictionary: `None` when it is `undefined`.
fn member<'s, E: Eng>(s: &mut Sc<'s, E>, init: Local<'s>, name: &str) -> Res<'s, Option<Local<'s>>> {
    let v = s.get_str(init, name)?;
    Ok(if s.type_of(v) == Type::Undefined { None } else { Some(v) })
}

/// A `double` member: `ToNumber`, and a TypeError for NaN and the infinities.
fn dbl<'s, E: Eng>(s: &mut Sc<'s, E>, init: Local<'s>, name: &str, iface: &str, dict: &str, default: f64) -> Res<'s, f64> {
    let Some(v) = member::<E>(s, init, name)? else { return Ok(default) };
    let n = s.to_number(v)?;
    if !n.is_finite() {
        return Err(bad::<E>(s, iface, &format!("Failed to read the '{name}' property from '{dict}': The provided double value is non-finite.")));
    }
    Ok(n)
}

/// A `long`, `short` or `unsigned short` member: `ToNumber`, cut to the integer, wrapped to the type by the caller's `as`.
fn int<'s, E: Eng>(s: &mut Sc<'s, E>, init: Local<'s>, name: &str) -> Res<'s, i64> {
    let Some(v) = member::<E>(s, init, name)? else { return Ok(0) };
    let n = s.to_number(v)?;
    Ok(if n.is_finite() { n.trunc() as i64 } else { 0 })
}

fn text<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, init: Local<'s>, name: &str) -> Res<'s, Option<String>> {
    match member::<E>(s, init, name)? {
        Some(v) => Ok(Some(d.string_arg_s(s, v)?)),
        None => Ok(None),
    }
}

/// The constructor of interface `K`.
pub(crate) fn ctor<'s, const K: u8, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (iface, dict) = (IFACE[K as usize], format!("{}Init", IFACE[K as usize]));
    if a.is_empty() {
        return Err(bad::<E>(s, iface, "1 argument required, but only 0 present."));
    }
    let ty = d.arg_str_s(s, a, 0)?;
    let init = arg_s::<E>(s, a, 1);
    if !is_nullish::<E>(s, init) && !is_object::<E>(s, init) {
        return Err(bad::<E>(s, iface, &format!("The provided value is not of type '{dict}'.")));
    }
    build::<K, _>(d, s, ty, init)
}

/// `ctor` with an empty type and no init dictionary (`document.createEvent`).
pub(crate) fn ctor_empty<'s, const K: u8, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, Local<'s>> {
    let init = s.undefined();
    build::<K, _>(d, s, String::new(), init)
}

fn build<'s, const K: u8, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, ty: String, init: Local<'s>) -> Res<'s, Local<'s>> {
    let (iface, init_obj) = (IFACE[K as usize], is_object::<E>(s, init));
    old::<E, _>(s, |it| d.ensure_ui_ext(it))?;
    let mut ui = Ui { frac: K == POINTER, ..Ui::default() };
    let (mut bubbles, mut cancelable, mut composed, mut related) = (false, false, false, s.null());
    if init_obj {
        bubbles = init_flag_s::<E>(s, init, "bubbles")?;
        cancelable = init_flag_s::<E>(s, init, "cancelable")?;
        composed = init_flag_s::<E>(s, init, "composed")?;
        match member::<E>(s, init, "view")? {
            None => {}
            Some(v) if is_nullish::<E>(s, v) => {}
            Some(v) if d.is_obj_s(s, v, *d.global.get().expect("installed")) => ui.view = true,
            Some(_) => return Err(bad::<E>(s, iface, "Failed to read the 'view' property from 'UIEventInit': Failed to convert value to 'Window'.")),
        }
        ui.detail = int::<E>(s, init, "detail")? as i32;
        if K != INPUT {
            for (name, on) in [("ctrlKey", &mut ui.mods.ctrl), ("shiftKey", &mut ui.mods.shift), ("altKey", &mut ui.mods.alt), ("metaKey", &mut ui.mods.meta)] {
                *on = init_flag_s::<E>(s, init, name)?;
            }
            for (i, m) in EXTRA_MODS.iter().enumerate() {
                if init_flag_s::<E>(s, init, &format!("modifier{m}"))? {
                    ui.extra |= 1 << i;
                }
            }
        }
        match K {
            MOUSE | POINTER => {
                let mouse = "MouseEventInit";
                ui.screen = (dbl::<E>(s, init, "screenX", iface, mouse, 0.0)?, dbl::<E>(s, init, "screenY", iface, mouse, 0.0)?);
                ui.x = dbl::<E>(s, init, "clientX", iface, mouse, 0.0)?;
                ui.y = dbl::<E>(s, init, "clientY", iface, mouse, 0.0)?;
                ui.movement = (dbl::<E>(s, init, "movementX", iface, mouse, 0.0)?, dbl::<E>(s, init, "movementY", iface, mouse, 0.0)?);
                ui.button = int::<E>(s, init, "button")? as i16;
                ui.buttons = int::<E>(s, init, "buttons")? as u16;
                match member::<E>(s, init, "relatedTarget")? {
                    None => {}
                    Some(v) if is_nullish::<E>(s, v) => {}
                    Some(v) if obj_id::<E>(s, v).is_some_and(|o| d.st.borrow().by_obj.contains_key(&o)) => related = v,
                    Some(_) => return Err(bad::<E>(s, iface, "Failed to read the 'relatedTarget' property from 'MouseEventInit': Failed to convert value to 'EventTarget'.")),
                }
                if K == POINTER {
                    let p = "PointerEventInit";
                    let mut pd = PointerData { width: 1.0, height: 1.0, id: 0, primary: false, ..PointerData::MOUSE };
                    pd.id = int::<E>(s, init, "pointerId")? as i32;
                    pd.width = dbl::<E>(s, init, "width", iface, p, 1.0)?;
                    pd.height = dbl::<E>(s, init, "height", iface, p, 1.0)?;
                    pd.pressure = dbl::<E>(s, init, "pressure", iface, p, 0.0)?;
                    pd.tangential = dbl::<E>(s, init, "tangentialPressure", iface, p, 0.0)?;
                    pd.tilt = (int::<E>(s, init, "tiltX")? as i32, int::<E>(s, init, "tiltY")? as i32);
                    pd.twist = int::<E>(s, init, "twist")? as i32;
                    pd.kind = text(d, s, init, "pointerType")?.unwrap_or_default();
                    pd.primary = init_flag_s::<E>(s, init, "isPrimary")?;
                    ui.pointer = Some(Box::new(pd));
                }
            }
            KEYBOARD => {
                ui.key = text(d, s, init, "key")?.unwrap_or_default();
                ui.code = text(d, s, init, "code")?.unwrap_or_default();
                ui.location = int::<E>(s, init, "location")? as u32;
                ui.repeat = init_flag_s::<E>(s, init, "repeat")?;
                ui.composing = init_flag_s::<E>(s, init, "isComposing")?;
                ui.char_code = int::<E>(s, init, "charCode")? as u32;
                ui.key_code = int::<E>(s, init, "keyCode")? as u32;
            }
            _ => {
                ui.data = match member::<E>(s, init, "data")? {
                    None => None,
                    Some(v) if is_nullish::<E>(s, v) => None,
                    Some(v) => Some(d.string_arg_s(s, v)?),
                };
                ui.composing = init_flag_s::<E>(s, init, "isComposing")?;
                let t = text(d, s, init, "inputType")?.unwrap_or_default();
                ui.input_type = if INPUT_TYPES.contains(&t.as_str()) { t } else { String::new() };
            }
        }
    } else if K == POINTER {
        ui.pointer = Some(Box::new(PointerData { id: 0, primary: false, ..PointerData::MOUSE }));
    }
    if K <= POINTER {
        let off = if ui.view { d.page_offset() } else { (0.0, 0.0) };
        ui.page = (ui.x + off.0, ui.y + off.1);
    }
    let p = d.p();
    let default = d.obj_local(s, [p.mouse, p.pointer, p.keyboard, p.inputev][K as usize]);
    let proto = s.proto_from_new_target(default)?;
    let proto = obj_id::<E>(s, proto).expect("a prototype is an object");
    let e = old::<E, _>(s, |it| d.new_event_proto(it, &ty, bubbles, cancelable, composed, proto))?;
    let mut st = d.st.borrow_mut();
    let ev = st.events.get_mut(&e).expect("just made");
    ev.ui = Some(Box::new(ui));
    s.store(&mut ev.related, related);
    drop(st);
    Ok(d.obj_local(s, e))
}

// ------------------------------------------------------------------------------------------ members

macro_rules! num {
    ($name:ident, |$u:ident| $e:expr) => {
        fn $name<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
            let n = with_ui(d, s, this, |_, $u| $e as f64)?;
            Ok(s.number(n))
        }
    };
}

static MOUSE_POINTER: PointerData = PointerData::MOUSE;

fn pd(u: &Ui) -> &PointerData {
    u.pointer.as_deref().unwrap_or(&MOUSE_POINTER)
}

/// The sizes of a mouse pointer: 1 by 1, and 0.5 of pressure while a button is down.
fn pressure(u: &Ui) -> f64 {
    match &u.pointer {
        Some(p) => p.pressure,
        None if u.buttons != 0 => 0.5,
        None => 0.0,
    }
}

num!(movement_x, |u| coord(u.movement.0, u.frac));
num!(movement_y, |u| coord(u.movement.1, u.frac));
num!(width, |u| pd(u).width);
num!(height, |u| pd(u).height);
num!(pressure_of, |u| pressure(u));
num!(tangential, |u| pd(u).tangential);
num!(tilt_x, |u| pd(u).tilt.0);
num!(tilt_y, |u| pd(u).tilt.1);
num!(twist, |u| pd(u).twist);
num!(char_code, |u| u.char_code);

fn view<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let on = with_ui(d, s, this, |_, u| u.view)?;
    Ok(if on { s.global() } else { s.null() })
}

fn offset<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, y: bool) -> Res<'s, Local<'s>> {
    let _caller = crate::force::tag("event offsetX/Y");
    let target = crate::browser_ui::ui_value(d, s, this, |e| Some(&e.target))?;
    let u = with_ui(d, s, this, |_, u| u.clone())?;
    let (client, page) = if y { (u.y, u.page.1) } else { (u.x, u.page.0) };
    let mut v = page;
    if let Some(n) = d.node_of_s(s, target).filter(|&n| d.is_element(n)) && old::<E, _>(s, |it| d.in_document(it, n))?
        && let Some(r) = old::<E, _>(s, |it| d.with_geo(it, |g, st| g.client_rect(st, n)))? {
        v = client - if y { r.y } else { r.x } as f64;
    }
    Ok(s.number(if u.frac { v } else { v.floor() }))
}

fn offset_x<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    offset(d, s, this, false)
}

fn offset_y<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    offset(d, s, this, true)
}

fn modifier_state<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>, iface: &str) -> Res<'s, Local<'s>> {
    if a.is_empty() {
        return Err(s.type_error(&format!("Failed to execute 'getModifierState' on '{iface}': 1 argument required, but only 0 present.")));
    }
    let name = d.arg_str_s(s, a, 0)?;
    let on = with_ui(d, s, this, |_, u| match name.as_str() {
        "Control" => u.mods.ctrl,
        "Shift" => u.mods.shift,
        "Alt" => u.mods.alt,
        "Meta" => u.mods.meta,
        n => EXTRA_MODS.iter().position(|m| *m == n).is_some_and(|i| u.extra >> i & 1 == 1),
    })?;
    Ok(s.boolean(on))
}

fn mouse_modifier_state<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    modifier_state(d, s, this, a, "MouseEvent")
}

fn key_modifier_state<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    modifier_state(d, s, this, a, "KeyboardEvent")
}

/// `[]`: the events that Chrome coalesces or predicts are never made here.
fn no_events<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    with_ui(d, s, this, |_, _| ())?;
    s.new_array(&[])
}

fn target_ranges<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let r = with_ui(d, s, this, |_, u| u.targets)?;
    crate::browser_range::static_ranges(d, s, r)
}

fn null<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    with_ui(d, s, this, |_, _| ())?;
    Ok(s.null())
}

// ------------------------------------------------------------------------------------------ pointer capture

/// The pointer id argument. Only the id 1 (the mouse) is active.
fn pointer_id<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>, method: &str) -> Res<'s, ()> {
    let Some(v) = a.get(0) else {
        return Err(s.type_error(&format!("Failed to execute '{method}' on 'Element': 1 argument required, but only 0 present.")));
    };
    let n = s.to_number(v)?;
    if (if n.is_finite() { n.trunc() as i64 as i32 } else { 0 }) != 1 {
        return Err(d.dom_err_s(s, "NotFoundError", &format!("Failed to execute '{method}' on 'Element': No active pointer with the given id is found.")));
    }
    Ok(())
}

fn set_pointer_capture<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.el_s(s, this)?;
    pointer_id(d, s, a, "setPointerCapture")?;
    if !old::<E, _>(s, |it| d.in_document(it, el))? {
        return Err(d.dom_err_s(s, "InvalidStateError", "Failed to execute 'setPointerCapture' on 'Element': InvalidStateError"));
    }
    let mut st = d.st.borrow_mut();
    if st.ui.held != 0 {
        st.ui.capture = Some(el);
    }
    Ok(s.undefined())
}

fn release_pointer_capture<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.el_s(s, this)?;
    pointer_id(d, s, a, "releasePointerCapture")?;
    let mut st = d.st.borrow_mut();
    if st.ui.capture == Some(el) {
        st.ui.capture = None;
    }
    Ok(s.undefined())
}

fn has_pointer_capture<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.el_s(s, this)?;
    let Some(v) = a.get(0) else {
        return Err(s.type_error("Failed to execute 'hasPointerCapture' on 'Element': 1 argument required, but only 0 present."));
    };
    let n = s.to_number(v)?;
    let id = if n.is_finite() { n.trunc() as i64 as i32 } else { 0 };
    let on = id == 1 && d.st.borrow().ui.capture == Some(el);
    Ok(s.boolean(on))
}

// ------------------------------------------------------------------------------------------ install

enum To<E: Eng> {
    Get(HS<E>),
    Method(HS<E>, u32),
}

impl<E: Eng> Dom<E> {
    /// Define the members that the window events did not need (once). See the header.
    pub(crate) fn ensure_ui_ext(&self, it: &mut E::Vm) -> R<E, ()> {
        if std::mem::replace(&mut self.st.borrow_mut().ui.ext, true) {
            return Ok(());
        }
        let p = self.p();
        let (m, pt, k, i, el) = (p.mouse, p.pointer, p.keyboard, p.inputev, p.element);
        let rows: &[(&[E::Obj], &str, To<E>)] = &[
            (&[m, k, i], "view", To::Get(view)),
            (&[m], "offsetX", To::Get(offset_x)),
            (&[m], "offsetY", To::Get(offset_y)),
            (&[m], "movementX", To::Get(movement_x)),
            (&[m], "movementY", To::Get(movement_y)),
            (&[m], "getModifierState", To::Method(mouse_modifier_state, 1)),
            (&[m], "which", To::Get(lg::mouse_which)),
            (&[i], "which", To::Get(lg::zero)),
            (&[m, k, i], "sourceCapabilities", To::Get(lg::source_capabilities)),
            (&[m], "layerX", To::Get(lg::layer_x)),
            (&[m], "layerY", To::Get(lg::layer_y)),
            (&[m], "fromElement", To::Get(lg::from_element)),
            (&[m], "toElement", To::Get(lg::to_element)),
            (&[m], "initMouseEvent", To::Method(lg::init_mouse_event, 15)),
            (&[k], "getModifierState", To::Method(key_modifier_state, 1)),
            (&[pt], "width", To::Get(width)),
            (&[pt], "height", To::Get(height)),
            (&[pt], "pressure", To::Get(pressure_of)),
            (&[pt], "tangentialPressure", To::Get(tangential)),
            (&[pt], "tiltX", To::Get(tilt_x)),
            (&[pt], "tiltY", To::Get(tilt_y)),
            (&[pt], "twist", To::Get(twist)),
            (&[pt], "getCoalescedEvents", To::Method(no_events, 0)),
            (&[pt], "getPredictedEvents", To::Method(no_events, 0)),
            (&[k], "charCode", To::Get(char_code)),
            (&[i], "dataTransfer", To::Get(null)),
            (&[i], "getTargetRanges", To::Method(target_ranges, 0)),
            (&[el], "setPointerCapture", To::Method(set_pointer_capture, 1)),
            (&[el], "releasePointerCapture", To::Method(release_pointer_capture, 1)),
            (&[el], "hasPointerCapture", To::Method(has_pointer_capture, 1)),
        ];
        for (on, name, to) in rows {
            let key = self.key(it, name)?;
            match *to {
                To::Get(h) => {
                    let f = self.add_fn_s_obj(it, h, &format!("get {name}"), 0, HostFn::Method)?;
                    for &o in *on {
                        self.define_accessor(it, o, key, f, None)?;
                    }
                }
                To::Method(h, len) => {
                    let f = self.add_fn_s_obj(it, h, name, len, HostFn::Method)?;
                    for &o in *on {
                        it.define_data(o, key, E::obj_raw(f), true, false, true)?;
                    }
                }
            }
        }
        // `KeyboardEvent.DOM_KEY_LOCATION_*`, on the interface and its prototype.
        let kc = self.key(it, "constructor")?;
        if let Val::Object(ctor) = E::kind(it.get_value(E::obj_raw(k), kc)?) {
            for (n, v) in [("DOM_KEY_LOCATION_STANDARD", 0.0), ("DOM_KEY_LOCATION_LEFT", 1.0), ("DOM_KEY_LOCATION_RIGHT", 2.0), ("DOM_KEY_LOCATION_NUMPAD", 3.0)] {
                let key: E::Key = self.key(it, n)?;
                it.define_data(ctor, key, E::num(v), false, true, false)?;
                it.define_data(k, key, E::num(v), false, true, false)?;
            }
        }
        Ok(())
    }
}
