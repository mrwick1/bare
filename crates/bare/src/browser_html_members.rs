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

use crate::eng::Eng;
use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, Local, Type};
use crate::browser_reflect::{parse_float, reflect, text, this_of};
use bare_html::dom::NodeId;

/// Fill the prototype of the interface `name` of `browser_html.rs`. Returns `true` when the interface has members of its own here.
pub(crate) fn members<E: Eng>(d: &Dom<E>, it: &mut E::Vm, o: E::Obj, name: &str) -> R<E, bool> {
    use crate::browser_table::{Proto, members as table};
    match name {
        "HTMLTableElement" => table(d, it, o, Proto::Table)?,
        "HTMLTableSectionElement" => table(d, it, o, Proto::Section)?,
        "HTMLTableRowElement" => table(d, it, o, Proto::Row)?,
        "HTMLTableCellElement" => table(d, it, o, Proto::Cell)?,
        "HTMLBaseElement" => {
            d.def_get_s(it, o, "href", base_href, Some(|d, it, t, a| crate::browser_reflect::set(d, it, t, a, &["base"], "href", "href", crate::browser_reflect::Kind::Str)))?;
            reflect!(d, it, o, &["base"], { "target" => "target": Str });
        }
        "HTMLDataElement" => reflect!(d, it, o, &["data"], { "value" => "value": Str }),
        "HTMLTimeElement" => reflect!(d, it, o, &["time"], { "dateTime" => "datetime": Str }),
        "HTMLHtmlElement" => reflect!(d, it, o, &["html"], { "version" => "version": Str }),
        "HTMLDetailsElement" => {
            d.def_get_s(it, o, "open", crate::browser_details::open_get, Some(crate::browser_details::open_set))?;
            reflect!(d, it, o, &["details"], { "name" => "name": Str });
        }
        "HTMLLIElement" => reflect!(d, it, o, &["li"], { "value" => "value": Long(0, i32::MIN) }),
        "HTMLOListElement" => reflect!(d, it, o, &["ol"], { "start" => "start": Long(1, i32::MIN), "reversed" => "reversed": Bool, "type" => "type": Str }),
        "HTMLSourceElement" => reflect!(d, it, o, &["source"], {
            "src" => "src": Url, "type" => "type": Str, "srcset" => "srcset": Str, "sizes" => "sizes": Str, "media" => "media": Str,
        }),
        "HTMLFieldSetElement" => {
            reflect!(d, it, o, &["fieldset"], { "disabled" => "disabled": Bool, "name" => "name": Str });
            d.def_get_s(it, o, "type", |d, s, t, _| { this_of(d, s, t, &["fieldset"])?; d.js_str_s(s, "fieldset") }, None)?;
            d.def_get_s(it, o, "form", |d, s, t, _| form(d, s, t, "fieldset"), None)?;
        }
        "HTMLOutputElement" => {
            reflect!(d, it, o, &["output"], { "name" => "name": Str });
            d.def_get_s(it, o, "type", |d, s, t, _| { this_of(d, s, t, &["output"])?; d.js_str_s(s, "output") }, None)?;
            d.def_get_s(it, o, "form", |d, s, t, _| form(d, s, t, "output"), None)?;
            d.def_get_s(it, o, "value", |d, s, t, a| { this_of(d, s, t, &["output"])?; crate::browser_dom::text_content_s(d, s, t, a) },
                Some(|d, s, t, a| { this_of(d, s, t, &["output"])?; crate::browser_dom::set_text_content_s(d, s, t, a) }))?;
        }
        "HTMLProgressElement" => {
            d.def_get_s(it, o, "value", |d, s, t, _| { let n = progress(d, s, t)?.0; Ok(s.number(n)) }, Some(|d, s, t, a| set_num(d, s, t, a, "progress", "value", false)))?;
            d.def_get_s(it, o, "max", |d, s, t, _| { let n = progress(d, s, t)?.1; Ok(s.number(n)) }, Some(|d, s, t, a| set_num(d, s, t, a, "progress", "max", true)))?;
            d.def_get_s(it, o, "position", |d, s, t, _| { let n = progress(d, s, t)?.2; Ok(s.number(n)) }, None)?;
        }
        "HTMLMeterElement" => {
            d.def_get_s(it, o, "value", meter_get::<0, _>, Some(meter_set::<0, _>))?;
            d.def_get_s(it, o, "min", meter_get::<1, _>, Some(meter_set::<1, _>))?;
            d.def_get_s(it, o, "max", meter_get::<2, _>, Some(meter_set::<2, _>))?;
            d.def_get_s(it, o, "low", meter_get::<3, _>, Some(meter_set::<3, _>))?;
            d.def_get_s(it, o, "high", meter_get::<4, _>, Some(meter_set::<4, _>))?;
            d.def_get_s(it, o, "optimum", meter_get::<5, _>, Some(meter_set::<5, _>))?;
        }
        "HTMLDialogElement" => {
            reflect!(d, it, o, &["dialog"], { "open" => "open": Bool });
            d.def_get_s(it, o, "returnValue", return_value, Some(set_return_value))?;
            d.def_method_s(it, o, "show", 0, |d, s, t, _| open(d, s, t, false))?;
            d.def_method_s(it, o, "showModal", 0, |d, s, t, _| open(d, s, t, true))?;
            d.def_method_s(it, o, "close", 0, close)?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn base_href<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = this_of(d, s, this, &["base"])?;
    let raw = text(d, n, "href").unwrap_or_default();
    let doc_url = d.cur_rec().ok();
    let st = match crate::browser_navigation_url::parse(&raw, doc_url.as_ref()) {
        Ok(r) => r.href(),
        Err(_) => raw,
    };
    d.js_str_s(s, &st)
}

fn form<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, tag: &str) -> Res<'s, Local<'s>> {
    let n = this_of(d, s, this, &[tag])?;
    let f = old::<E, _>(s, |it| d.form_owner(it, n))?;
    d.wrap_opt_s(s, f)
}

fn num<E: Eng>(d: &Dom<E>, n: NodeId, name: &str) -> Option<f64> {
    text(d, n, name).and_then(|s| parse_float(&s))
}

/// `(value, max, position)` of a `progress`.
fn progress<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, (f64, f64, f64)> {
    let n = this_of(d, s, this, &["progress"])?;
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
fn meter_get<'s, const I: usize, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = this_of(d, s, this, &["meter"])?;
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
    Ok(s.number(v))
}

fn meter_set<'s, const I: usize, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    set_num(d, s, this, a, "meter", METER[I], false)
}

/// Write a `double` attribute. `positive`: a value that is not above 0 is ignored.
fn set_num<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>, tag: &str, name: &str, positive: bool) -> Res<'s, Local<'s>> {
    let n = this_of(d, s, this, &[tag])?;
    let arg0 = arg_s::<E>(s, a, 0);
    let x = s.to_number(arg0)?;
    if !x.is_finite() {
        return Err(s.type_error(&format!("Failed to set the '{name}' property: The provided double value is non-finite.")));
    }
    if positive && x <= 0.0 {
        return Ok(s.undefined());
    }
    let xv = s.number(x);
    let st = d.string_arg_s(s, xv)?;
    crate::browser_dom::attr_set_s(d, s, n, name, &st)?;
    Ok(s.undefined())
}

// ------------------------------------------------------------------------------------------ dialog

fn return_value<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = this_of(d, s, this, &["dialog"])?;
    let st = d.dialogs.borrow().get(&n).map(|x| x.0.clone()).unwrap_or_default();
    d.js_str_s(s, &st)
}

fn set_return_value<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = this_of(d, s, this, &["dialog"])?;
    let st = d.arg_str_s(s, a, 0)?;
    d.dialogs.borrow_mut().entry(n).or_default().0 = st;
    Ok(s.undefined())
}

/// `show()` (`modal` false) and `showModal()`.
fn open<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, modal: bool) -> Res<'s, Local<'s>> {
    let n = this_of(d, s, this, &["dialog"])?;
    let is_open = text(d, n, "open").is_some();
    let was_modal = d.dialogs.borrow().get(&n).is_some_and(|x| x.1);
    let what = if modal { "showModal" } else { "show" };
    if is_open && was_modal == modal {
        return Ok(s.undefined());
    }
    if is_open {
        let msg = if modal { "The dialog is already open as a non-modal dialog, and therefore cannot be opened as a modal dialog." } else { "The dialog is already open as a modal dialog, and therefore cannot be opened as a non-modal dialog." };
        return Err(d.dom_err_s(s, "InvalidStateError", &format!("Failed to execute '{what}' on 'HTMLDialogElement': {msg}")));
    }
    if modal && !old::<E, _>(s, |it| d.is_connected_node(it, n))? {
        return Err(d.dom_err_s(s, "InvalidStateError", "Failed to execute 'showModal' on 'HTMLDialogElement': The element is not in a Document."));
    }
    crate::browser_dom::attr_set_s(d, s, n, "open", "")?;
    d.dialogs.borrow_mut().entry(n).or_default().1 = modal;
    Ok(s.undefined())
}

fn close<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = this_of(d, s, this, &["dialog"])?;
    if text(d, n, "open").is_none() {
        return Ok(s.undefined());
    }
    let v = arg_s::<E>(s, a, 0);
    let result = match s.type_of(v) {
        Type::Undefined => None,
        _ => Some(d.string_arg_s(s, v)?),
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
    let ev = old::<E, _>(s, |it| d.new_event(it, "close", false, false, false, None))?;
    old::<E, _>(s, |it| d.dispatch(it, Target::Node(n), ev))?;
    Ok(s.undefined())
}
