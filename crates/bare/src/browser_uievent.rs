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

use crate::browser::*;
use crate::browser_events::init_flag;
use crate::browser_ui::{PointerData, Ui, coord, with_ui};
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, PropertyKey, Value};

type V = R<Value>;

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

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

fn bad(it: &mut Interp<'_>, iface: &str, what: &str) -> Error {
    it.type_error(&format!("Failed to construct '{iface}': {what}"))
}

/// The member `name` of the init dictionary: `None` when it is `undefined`.
fn member(d: &Dom, it: &mut Interp<'_>, init: Value, name: &str) -> R<Option<Value>> {
    let k = d.key(it, name)?;
    Ok(match it.get_value(init, k)? {
        Value::Undefined => None,
        v => Some(v),
    })
}

/// A `double` member: `ToNumber`, and a TypeError for NaN and the infinities.
fn dbl(d: &Dom, it: &mut Interp<'_>, init: Value, name: &str, iface: &str, dict: &str, default: f64) -> R<f64> {
    let Some(v) = member(d, it, init, name)? else { return Ok(default) };
    let n = it.to_js_number(v)?;
    if !n.is_finite() {
        return Err(bad(it, iface, &format!("Failed to read the '{name}' property from '{dict}': The provided double value is non-finite.")));
    }
    Ok(n)
}

/// A `long`, `short` or `unsigned short` member: `ToNumber`, cut to the integer, wrapped to the type by the caller's `as`.
fn int(d: &Dom, it: &mut Interp<'_>, init: Value, name: &str) -> R<i64> {
    let Some(v) = member(d, it, init, name)? else { return Ok(0) };
    let n = it.to_js_number(v)?;
    Ok(if n.is_finite() { n.trunc() as i64 } else { 0 })
}

fn text(d: &Dom, it: &mut Interp<'_>, init: Value, name: &str) -> R<Option<String>> {
    match member(d, it, init, name)? {
        Some(v) => Ok(Some(d.string_arg(it, v)?)),
        None => Ok(None),
    }
}

/// The constructor of interface `K`.
pub(crate) fn ctor<const K: u8>(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let (iface, dict) = (IFACE[K as usize], format!("{}Init", IFACE[K as usize]));
    if a.is_empty() {
        return Err(bad(it, iface, "1 argument required, but only 0 present."));
    }
    let ty = d.string_arg(it, a[0])?;
    let init = arg(a, 1);
    if !matches!(init, Value::Undefined | Value::Null | Value::Object(_)) {
        return Err(bad(it, iface, &format!("The provided value is not of type '{dict}'.")));
    }
    d.ensure_ui_ext(it)?;
    let mut ui = Ui { frac: K == POINTER, ..Ui::default() };
    let (mut bubbles, mut cancelable, mut composed, mut related) = (false, false, false, Value::Null);
    if let Value::Object(_) = init {
        bubbles = init_flag(d, it, init, "bubbles")?;
        cancelable = init_flag(d, it, init, "cancelable")?;
        composed = init_flag(d, it, init, "composed")?;
        match member(d, it, init, "view")? {
            None | Some(Value::Null) => {}
            Some(Value::Object(o)) if o == it.global_object() => ui.view = true,
            Some(_) => return Err(bad(it, iface, "Failed to read the 'view' property from 'UIEventInit': Failed to convert value to 'Window'.")),
        }
        ui.detail = int(d, it, init, "detail")? as i32;
        if K != INPUT {
            for (name, on) in [("ctrlKey", &mut ui.mods.ctrl), ("shiftKey", &mut ui.mods.shift), ("altKey", &mut ui.mods.alt), ("metaKey", &mut ui.mods.meta)] {
                *on = init_flag(d, it, init, name)?;
            }
            for (i, m) in EXTRA_MODS.iter().enumerate() {
                if init_flag(d, it, init, &format!("modifier{m}"))? {
                    ui.extra |= 1 << i;
                }
            }
        }
        match K {
            MOUSE | POINTER => {
                let mouse = "MouseEventInit";
                ui.screen = (dbl(d, it, init, "screenX", iface, mouse, 0.0)?, dbl(d, it, init, "screenY", iface, mouse, 0.0)?);
                ui.x = dbl(d, it, init, "clientX", iface, mouse, 0.0)?;
                ui.y = dbl(d, it, init, "clientY", iface, mouse, 0.0)?;
                ui.movement = (dbl(d, it, init, "movementX", iface, mouse, 0.0)?, dbl(d, it, init, "movementY", iface, mouse, 0.0)?);
                ui.button = int(d, it, init, "button")? as i16;
                ui.buttons = int(d, it, init, "buttons")? as u16;
                match member(d, it, init, "relatedTarget")? {
                    None | Some(Value::Null) => {}
                    Some(v @ Value::Object(o)) if d.st.borrow().by_obj.contains_key(&o) => related = v,
                    Some(_) => return Err(bad(it, iface, "Failed to read the 'relatedTarget' property from 'MouseEventInit': Failed to convert value to 'EventTarget'.")),
                }
                if K == POINTER {
                    let p = "PointerEventInit";
                    let mut pd = PointerData { width: 1.0, height: 1.0, id: 0, primary: false, ..PointerData::MOUSE };
                    pd.id = int(d, it, init, "pointerId")? as i32;
                    pd.width = dbl(d, it, init, "width", iface, p, 1.0)?;
                    pd.height = dbl(d, it, init, "height", iface, p, 1.0)?;
                    pd.pressure = dbl(d, it, init, "pressure", iface, p, 0.0)?;
                    pd.tangential = dbl(d, it, init, "tangentialPressure", iface, p, 0.0)?;
                    pd.tilt = (int(d, it, init, "tiltX")? as i32, int(d, it, init, "tiltY")? as i32);
                    pd.twist = int(d, it, init, "twist")? as i32;
                    pd.kind = text(d, it, init, "pointerType")?.unwrap_or_default();
                    pd.primary = init_flag(d, it, init, "isPrimary")?;
                    ui.pointer = Some(Box::new(pd));
                }
            }
            KEYBOARD => {
                ui.key = text(d, it, init, "key")?.unwrap_or_default();
                ui.code = text(d, it, init, "code")?.unwrap_or_default();
                ui.location = int(d, it, init, "location")? as u32;
                ui.repeat = init_flag(d, it, init, "repeat")?;
                ui.composing = init_flag(d, it, init, "isComposing")?;
                ui.char_code = int(d, it, init, "charCode")? as u32;
                ui.key_code = int(d, it, init, "keyCode")? as u32;
            }
            _ => {
                ui.data = match member(d, it, init, "data")? {
                    None | Some(Value::Null) => None,
                    Some(v) => Some(d.string_arg(it, v)?),
                };
                ui.composing = init_flag(d, it, init, "isComposing")?;
                let t = text(d, it, init, "inputType")?.unwrap_or_default();
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
    let proto = it.host_proto_from_new_target([p.mouse, p.pointer, p.keyboard, p.inputev][K as usize])?;
    let e = d.new_event_proto(it, &ty, bubbles, cancelable, composed, proto)?;
    let mut st = d.st.borrow_mut();
    let ev = st.events.get_mut(&e).expect("just made");
    ev.ui = Some(Box::new(ui));
    ev.related = related;
    Ok(Value::Object(e))
}

// ------------------------------------------------------------------------------------------ members

macro_rules! num {
    ($name:ident, |$u:ident| $e:expr) => {
        fn $name(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
            with_ui(d, it, this, |_, $u| Value::Number($e as f64))
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

fn view(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let on = with_ui(d, it, this, |_, u| u.view)?;
    Ok(if on { Value::Object(it.global_object()) } else { Value::Null })
}

fn offset(d: &Dom, it: &mut Interp<'_>, this: Value, y: bool) -> V {
    let (target, u) = with_ui(d, it, this, |e, u| (e.target, u.clone()))?;
    let (client, page) = if y { (u.y, u.page.1) } else { (u.x, u.page.0) };
    let mut v = page;
    if let Some(n) = d.node_of(target).filter(|&n| d.is_element(n)) && d.in_document(it, n)?
        && let Some(r) = d.with_geo(it, |g, s| g.client_rect(s, n))? {
        v = client - if y { r.y } else { r.x } as f64;
    }
    Ok(Value::Number(if u.frac { v } else { v.floor() }))
}

fn offset_x(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    offset(d, it, this, false)
}

fn offset_y(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    offset(d, it, this, true)
}

fn modifier_state(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], iface: &str) -> V {
    if a.is_empty() {
        return Err(it.type_error(&format!("Failed to execute 'getModifierState' on '{iface}': 1 argument required, but only 0 present.")));
    }
    let name = d.string_arg(it, a[0])?;
    with_ui(d, it, this, |_, u| {
        Value::Bool(match name.as_str() {
            "Control" => u.mods.ctrl,
            "Shift" => u.mods.shift,
            "Alt" => u.mods.alt,
            "Meta" => u.mods.meta,
            n => EXTRA_MODS.iter().position(|m| *m == n).is_some_and(|i| u.extra >> i & 1 == 1),
        })
    })
}

fn mouse_modifier_state(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    modifier_state(d, it, this, a, "MouseEvent")
}

fn key_modifier_state(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    modifier_state(d, it, this, a, "KeyboardEvent")
}

/// `[]`: the events that Chrome coalesces or predicts are never made here.
fn no_events(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    with_ui(d, it, this, |_, _| ())?;
    it.array_from(&[])
}

fn null(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    with_ui(d, it, this, |_, _| Value::Null)
}

// ------------------------------------------------------------------------------------------ pointer capture

/// The pointer id argument. Only the id 1 (the mouse) is active.
fn pointer_id(d: &Dom, it: &mut Interp<'_>, a: &[Value], method: &str) -> R<()> {
    if a.is_empty() {
        return Err(it.type_error(&format!("Failed to execute '{method}' on 'Element': 1 argument required, but only 0 present.")));
    }
    let n = it.to_js_number(a[0])?;
    if (if n.is_finite() { n.trunc() as i64 as i32 } else { 0 }) != 1 {
        return Err(d.dom_err(it, "NotFoundError", &format!("Failed to execute '{method}' on 'Element': No active pointer with the given id is found.")));
    }
    Ok(())
}

fn set_pointer_capture(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.el(it, this)?;
    pointer_id(d, it, a, "setPointerCapture")?;
    if !d.in_document(it, el)? {
        return Err(d.dom_err(it, "InvalidStateError", "Failed to execute 'setPointerCapture' on 'Element': InvalidStateError"));
    }
    let mut st = d.st.borrow_mut();
    if st.ui.held != 0 {
        st.ui.capture = Some(el);
    }
    Ok(Value::Undefined)
}

fn release_pointer_capture(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.el(it, this)?;
    pointer_id(d, it, a, "releasePointerCapture")?;
    let mut st = d.st.borrow_mut();
    if st.ui.capture == Some(el) {
        st.ui.capture = None;
    }
    Ok(Value::Undefined)
}

fn has_pointer_capture(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.el(it, this)?;
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'hasPointerCapture' on 'Element': 1 argument required, but only 0 present."));
    }
    let n = it.to_js_number(a[0])?;
    let id = if n.is_finite() { n.trunc() as i64 as i32 } else { 0 };
    Ok(Value::Bool(id == 1 && d.st.borrow().ui.capture == Some(el)))
}

// ------------------------------------------------------------------------------------------ install

enum To {
    Get(H),
    Method(H, u32),
}

impl Dom {
    /// Define the members that the window events did not need (once). See the header.
    pub(crate) fn ensure_ui_ext(&self, it: &mut Interp<'_>) -> R<()> {
        if std::mem::replace(&mut self.st.borrow_mut().ui.ext, true) {
            return Ok(());
        }
        let p = self.p();
        let (m, pt, k, i, el) = (p.mouse, p.pointer, p.keyboard, p.inputev, p.element);
        let rows: &[(&[ObjId], &str, To)] = &[
            (&[m, k, i], "view", To::Get(view)),
            (&[m], "offsetX", To::Get(offset_x)),
            (&[m], "offsetY", To::Get(offset_y)),
            (&[m], "movementX", To::Get(movement_x)),
            (&[m], "movementY", To::Get(movement_y)),
            (&[m], "getModifierState", To::Method(mouse_modifier_state, 1)),
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
            (&[i], "getTargetRanges", To::Method(no_events, 0)),
            (&[el], "setPointerCapture", To::Method(set_pointer_capture, 1)),
            (&[el], "releasePointerCapture", To::Method(release_pointer_capture, 1)),
            (&[el], "hasPointerCapture", To::Method(has_pointer_capture, 1)),
        ];
        for (on, name, to) in rows {
            let key = self.key(it, name)?;
            match *to {
                To::Get(h) => {
                    let f = self.add_fn(it, h, &format!("get {name}"), 0, false)?;
                    for &o in *on {
                        self.define_accessor(it, o, key, f, None)?;
                    }
                }
                To::Method(h, len) => {
                    let f = self.add_fn(it, h, name, len, false)?;
                    for &o in *on {
                        it.define_data(o, key, Value::Object(f), true, false, true)?;
                    }
                }
            }
        }
        // `KeyboardEvent.DOM_KEY_LOCATION_*`, on the interface and its prototype.
        let kc = self.key(it, "constructor")?;
        if let Value::Object(ctor) = it.get_value(Value::Object(k), kc)? {
            for (n, v) in [("DOM_KEY_LOCATION_STANDARD", 0.0), ("DOM_KEY_LOCATION_LEFT", 1.0), ("DOM_KEY_LOCATION_RIGHT", 2.0), ("DOM_KEY_LOCATION_NUMPAD", 3.0)] {
                let key: PropertyKey = self.key(it, n)?;
                it.define_data(ctor, key, Value::Number(v), false, true, false)?;
                it.define_data(k, key, Value::Number(v), false, true, false)?;
            }
        }
        Ok(())
    }
}
