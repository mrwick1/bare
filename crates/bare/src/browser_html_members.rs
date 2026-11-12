//! The own members of the interfaces of `browser_html.rs` (other than tables, see `browser_table.rs`, and `canvas`).
//! Cause: pages read standard members such as `dialog.showModal`, `details.open` or `progress.value`; they were `undefined`.
//!
//! - Reflected attributes (`browser_reflect.rs`): `base.target`, `data.value`, `details.open` and `name`, `dialog.open`, `fieldset.disabled`
//!   and `name`, `html.version`, `li.value`, `ol.start`, `reversed` and `type`, `output.name`, `source.src`, `type`, `srcset`, `sizes` and
//!   `media`, `time.dateTime`.
//! - `base.href`: the attribute resolved against the document URL (not against the base URL, which this element sets).
//! - `fieldset.type` is `"fieldset"`, `output.type` is `"output"`. `fieldset.form` and `output.form` are the form owner. `output.value` reads
//!   and writes the text (deviation: no separate default value).
//! - `progress`: `max` is the attribute when it is a number above 0, else 1; `value` is 0 without a number, else clamped to `[0, max]`;
//!   `position` is `value / max`, or -1 without a `value` number. `meter`: `min` (default 0), `max` (default 1, at least `min`), `value`
//!   (default 0), `low` (default `min`), `high` (default `max`, at least `low`) and `optimum` (default the middle) are clamped to `[min, max]`.
//!   A setter writes the number; `progress.max` ignores a value that is not above 0.
//! - `dialog`: `show()` and `showModal()` add `open`; `close(result)` removes it, sets `returnValue` when `result` is given, and fires
//!   `close`. `showModal()` on a dialog that is open but not modal is an `InvalidStateError`, as is `show()` on a modal one. Deviations: the
//!   `close` event fires at once, not in a task; there is no top layer, no backdrop, no focus move and no `cancel` on Escape.
//!
//! Absent: `progress.labels` and `meter.labels`, `fieldset.elements` and the validity members of `fieldset` and `output`, `output.htmlFor`,
//! `output.defaultValue`, `dialog.requestClose()`.

use crate::browser::*;
use crate::browser_platform::arg;
use crate::browser_reflect::{parse_float, reflect, text, this_of};
use bare_html::dom::NodeId;
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};

/// Fill the prototype of the interface `name` of `browser_html.rs`. Returns `true` when the interface has members of its own here.
pub(crate) fn members(d: &Dom, it: &mut Interp<'_>, o: ObjId, name: &str) -> R<bool> {
    use crate::browser_table::{Proto, members as table};
    match name {
        "HTMLTableElement" => table(d, it, o, Proto::Table)?,
        "HTMLTableSectionElement" => table(d, it, o, Proto::Section)?,
        "HTMLTableRowElement" => table(d, it, o, Proto::Row)?,
        "HTMLTableCellElement" => table(d, it, o, Proto::Cell)?,
        "HTMLBaseElement" => {
            d.def_get(it, o, "href", base_href, Some(|d, it, t, a| crate::browser_reflect::set(d, it, t, a, &["base"], "href", "href", crate::browser_reflect::Kind::Str)))?;
            reflect!(d, it, o, &["base"], { "target" => "target": Str });
        }
        "HTMLDataElement" => reflect!(d, it, o, &["data"], { "value" => "value": Str }),
        "HTMLTimeElement" => reflect!(d, it, o, &["time"], { "dateTime" => "datetime": Str }),
        "HTMLHtmlElement" => reflect!(d, it, o, &["html"], { "version" => "version": Str }),
        "HTMLDetailsElement" => reflect!(d, it, o, &["details"], { "open" => "open": Bool, "name" => "name": Str }),
        "HTMLLIElement" => reflect!(d, it, o, &["li"], { "value" => "value": Long(0, i32::MIN) }),
        "HTMLOListElement" => reflect!(d, it, o, &["ol"], { "start" => "start": Long(1, i32::MIN), "reversed" => "reversed": Bool, "type" => "type": Str }),
        "HTMLSourceElement" => reflect!(d, it, o, &["source"], {
            "src" => "src": Url, "type" => "type": Str, "srcset" => "srcset": Str, "sizes" => "sizes": Str, "media" => "media": Str,
        }),
        "HTMLFieldSetElement" => {
            reflect!(d, it, o, &["fieldset"], { "disabled" => "disabled": Bool, "name" => "name": Str });
            d.def_get(it, o, "type", |d, it, t, _| { this_of(d, it, t, &["fieldset"])?; d.js_str(it, "fieldset") }, None)?;
            d.def_get(it, o, "form", |d, it, t, _| form(d, it, t, "fieldset"), None)?;
        }
        "HTMLOutputElement" => {
            reflect!(d, it, o, &["output"], { "name" => "name": Str });
            d.def_get(it, o, "type", |d, it, t, _| { this_of(d, it, t, &["output"])?; d.js_str(it, "output") }, None)?;
            d.def_get(it, o, "form", |d, it, t, _| form(d, it, t, "output"), None)?;
            d.def_get(it, o, "value", |d, it, t, a| { this_of(d, it, t, &["output"])?; crate::browser_dom::text_content(d, it, t, a) },
                Some(|d, it, t, a| { this_of(d, it, t, &["output"])?; crate::browser_dom::set_text_content(d, it, t, a) }))?;
        }
        "HTMLProgressElement" => {
            d.def_get(it, o, "value", |d, it, t, _| Ok(Value::Number(progress(d, it, t)?.0)), Some(|d, it, t, a| set_num(d, it, t, a, "progress", "value", false)))?;
            d.def_get(it, o, "max", |d, it, t, _| Ok(Value::Number(progress(d, it, t)?.1)), Some(|d, it, t, a| set_num(d, it, t, a, "progress", "max", true)))?;
            d.def_get(it, o, "position", |d, it, t, _| Ok(Value::Number(progress(d, it, t)?.2)), None)?;
        }
        "HTMLMeterElement" => {
            d.def_get(it, o, "value", meter_get::<0>, Some(meter_set::<0>))?;
            d.def_get(it, o, "min", meter_get::<1>, Some(meter_set::<1>))?;
            d.def_get(it, o, "max", meter_get::<2>, Some(meter_set::<2>))?;
            d.def_get(it, o, "low", meter_get::<3>, Some(meter_set::<3>))?;
            d.def_get(it, o, "high", meter_get::<4>, Some(meter_set::<4>))?;
            d.def_get(it, o, "optimum", meter_get::<5>, Some(meter_set::<5>))?;
        }
        "HTMLDialogElement" => {
            reflect!(d, it, o, &["dialog"], { "open" => "open": Bool });
            d.def_get(it, o, "returnValue", return_value, Some(set_return_value))?;
            d.def_method(it, o, "show", 0, |d, it, t, _| open(d, it, t, false))?;
            d.def_method(it, o, "showModal", 0, |d, it, t, _| open(d, it, t, true))?;
            d.def_method(it, o, "close", 0, close)?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn base_href(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let n = this_of(d, it, this, &["base"])?;
    let raw = text(d, n, "href").unwrap_or_default();
    let doc_url = d.cur_rec().ok();
    let s = match crate::browser_navigation_url::parse(&raw, doc_url.as_ref()) {
        Ok(r) => r.href(),
        Err(_) => raw,
    };
    d.js_str(it, &s)
}

fn form(d: &Dom, it: &mut Interp<'_>, this: Value, tag: &str) -> R<Value> {
    let n = this_of(d, it, this, &[tag])?;
    let f = d.form_owner(it, n)?;
    d.wrap_opt(it, f)
}

fn num(d: &Dom, n: NodeId, name: &str) -> Option<f64> {
    text(d, n, name).and_then(|s| parse_float(&s))
}

/// `(value, max, position)` of a `progress`.
fn progress(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<(f64, f64, f64)> {
    let n = this_of(d, it, this, &["progress"])?;
    let max = num(d, n, "max").filter(|&m| m > 0.0).unwrap_or(1.0);
    Ok(match num(d, n, "value") {
        Some(v) => {
            let v = v.clamp(0.0, max);
            (v, max, v / max)
        }
        None => (0.0, max, -1.0),
    })
}

const METER: [&str; 6] = ["value", "min", "max", "low", "high", "optimum"];

/// Member `I` of `METER`. `I` is a constant parameter: one function is made for each value, like six copies of the function.
fn meter_get<const I: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let n = this_of(d, it, this, &["meter"])?;
    let min = num(d, n, "min").unwrap_or(0.0);
    let max = num(d, n, "max").unwrap_or(1.0).max(min);
    let low = num(d, n, "low").unwrap_or(min).clamp(min, max);
    let v = match I {
        0 => num(d, n, "value").unwrap_or(0.0).clamp(min, max),
        1 => min,
        2 => max,
        3 => low,
        4 => num(d, n, "high").unwrap_or(max).clamp(low, max),
        _ => num(d, n, "optimum").unwrap_or((min + max) / 2.0).clamp(min, max),
    };
    Ok(Value::Number(v))
}

fn meter_set<const I: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    set_num(d, it, this, a, "meter", METER[I], false)
}

/// Write a `double` attribute. `positive`: a value that is not above 0 is ignored.
fn set_num(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], tag: &str, name: &str, positive: bool) -> R<Value> {
    let n = this_of(d, it, this, &[tag])?;
    let x = it.to_js_number(arg(a, 0))?;
    if !x.is_finite() {
        return Err(it.type_error(&format!("Failed to set the '{name}' property: The provided double value is non-finite.")));
    }
    if positive && x <= 0.0 {
        return Ok(Value::Undefined);
    }
    let s = d.string_arg(it, Value::Number(x))?;
    crate::browser_dom::attr_set(d, it, n, name, &s)?;
    Ok(Value::Undefined)
}

// ------------------------------------------------------------------------------------------ dialog

fn return_value(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let n = this_of(d, it, this, &["dialog"])?;
    let s = d.dialogs.borrow().get(&n).map(|x| x.0.clone()).unwrap_or_default();
    d.js_str(it, &s)
}

fn set_return_value(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let n = this_of(d, it, this, &["dialog"])?;
    let s = d.string_arg(it, arg(a, 0))?;
    d.dialogs.borrow_mut().entry(n).or_default().0 = s;
    Ok(Value::Undefined)
}

/// `show()` (`modal` false) and `showModal()`.
fn open(d: &Dom, it: &mut Interp<'_>, this: Value, modal: bool) -> R<Value> {
    let n = this_of(d, it, this, &["dialog"])?;
    let is_open = text(d, n, "open").is_some();
    let was_modal = d.dialogs.borrow().get(&n).is_some_and(|x| x.1);
    let what = if modal { "showModal" } else { "show" };
    if is_open && was_modal == modal {
        return Ok(Value::Undefined);
    }
    if is_open {
        let msg = if modal { "The dialog is already open as a non-modal dialog, and therefore cannot be opened as a modal dialog." } else { "The dialog is already open as a modal dialog, and therefore cannot be opened as a non-modal dialog." };
        return Err(d.dom_err(it, "InvalidStateError", &format!("Failed to execute '{what}' on 'HTMLDialogElement': {msg}")));
    }
    if modal && !d.is_connected_node(it, n)? {
        return Err(d.dom_err(it, "InvalidStateError", "Failed to execute 'showModal' on 'HTMLDialogElement': The element is not in a Document."));
    }
    crate::browser_dom::attr_set(d, it, n, "open", "")?;
    d.dialogs.borrow_mut().entry(n).or_default().1 = modal;
    Ok(Value::Undefined)
}

fn close(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let n = this_of(d, it, this, &["dialog"])?;
    if text(d, n, "open").is_none() {
        return Ok(Value::Undefined);
    }
    let result = match arg(a, 0) {
        Value::Undefined => None,
        v => Some(d.string_arg(it, v)?),
    };
    d.doc.borrow_mut().remove_attr(n, "open");
    {
        let mut m = d.dialogs.borrow_mut();
        let e = m.entry(n).or_default();
        e.1 = false;
        if let Some(r) = result {
            e.0 = r;
        }
    }
    let ev = d.new_event(it, "close", false, false, false, None)?;
    d.dispatch(it, Target::Node(n), ev)?;
    Ok(Value::Undefined)
}
