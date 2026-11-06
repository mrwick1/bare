//! `HTMLTextAreaElement`. Sources: HTML Standard "The textarea element", "APIs for the text control selections"; the cases are compared
//! with headless Chrome (`tools/fixtures/ui/textarea-cases.txt`).
//!
//! What is built
//! - The raw value is the text children (`defaultValue`). The API value is the raw value with each CRLF and each lone CR turned into LF.
//!   A dirty value (`value` set by script, or typed in the window) is native state, the same `Input::value` as for an `<input>`. A textarea
//!   that is not dirty shows the text children, so `textContent` and `defaultValue` changes show at once. The dirty value is copied by
//!   `cloneNode`. `form.reset()` clears it (`browser_controls.rs`).
//! - `value`, `defaultValue`, `textLength`, `type`, `form`, `rows` and `cols` (default 2 and 20, limited to numbers above 0), `maxLength`
//!   and `minLength` (-1 when missing, `IndexSizeError` for a negative value or when the pair is out of order), `placeholder`, `name`, `wrap`,
//!   `dirName`, `autocomplete`, `required`, `readOnly`, `disabled`, `selectionStart`, `selectionEnd`, `selectionDirection`,
//!   `setSelectionRange` and `select` (`browser_selection.rs`), and the constraint validation and `labels` of `browser_controls.rs`.
//! - A textarea that was never selected has its caret at 0 with direction `forward`. A `value` that changes puts the caret at the end.
//! - The submitted value is the API value (the form encoder turns LF into CRLF).
//!
//! Deviations (on purpose)
//! - The window draws the lines of the value that end with a line feed. It does not wrap a long line, does not scroll and does not draw
//!   the caret of a line other than the first (`page.rs`).
//! - A change of the text children of a textarea that is not dirty does not move a caret that was set before. `setRangeText` is absent.

use crate::browser::*;
use crate::browser_dom::arg;
use crate::browser_select::{get, reflect};
use crate::browser_selection as sel;
use bare_html::dom::{Document, NodeData, NodeId};
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};
use std::borrow::Cow;

type V = R<Value>;

/// The raw value: the text of the text children.
pub(crate) fn default_text(doc: &Document, n: NodeId) -> String {
    doc.children(n).filter_map(|c| if let NodeData::Text(sp) = *doc.data(c) { Some(doc.text(sp)) } else { None }).collect()
}

/// The API value of a raw value: CRLF and a lone CR become LF.
pub(crate) fn api_value(raw: &str) -> Cow<'_, str> {
    if raw.contains('\r') { Cow::Owned(raw.replace("\r\n", "\n").replace('\r', "\n")) } else { Cow::Borrowed(raw) }
}

fn this_el(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
    d.html_el(it, this, "textarea")
}

fn value(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = this_el(d, it, this)?;
    let s = d.input_value_of(n).unwrap_or_default();
    d.js_str(it, &s)
}

fn set_value(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = this_el(d, it, this)?;
    let v = arg(a, 0);
    let s = if matches!(v, Value::Null) { String::new() } else { d.string_arg(it, v)? };
    d.put_input_value(Some(it), n, api_value(&s).into_owned())?;
    Ok(Value::Undefined)
}

fn default_value(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = this_el(d, it, this)?;
    let s = default_text(&d.doc.borrow(), n);
    d.js_str(it, &s)
}

fn set_default_value(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = this_el(d, it, this)?;
    let s = d.string_arg(it, arg(a, 0))?;
    let v = d.js_str(it, &s)?;
    crate::browser_dom::set_text_content(d, it, this, &[v])?;
    if let Some(i) = d.st.borrow_mut().inputs.get_mut(&n) {
        i.sel = None; // Chrome: the caret goes back to 0
    }
    Ok(Value::Undefined)
}

fn text_length(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = this_el(d, it, this)?;
    Ok(Value::Number(d.with_input_value(n, |v| v.encode_utf16().count()).unwrap_or(0) as f64))
}

fn kind(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    this_el(d, it, this)?;
    d.js_str(it, "textarea")
}

fn form(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = this_el(d, it, this)?;
    let f = d.form_owner(it, n)?;
    d.wrap_opt(it, f)
}

reflect!("textarea", "placeholder", placeholder, set_placeholder, str);
reflect!("textarea", "name", name, set_name, str);
reflect!("textarea", "wrap", wrap, set_wrap, str);
reflect!("textarea", "dirname", dir_name, set_dir_name, str);
reflect!("textarea", "autocomplete", autocomplete, set_autocomplete, str);
reflect!("textarea", "required", required, set_required, bool);
reflect!("textarea", "readonly", read_only, set_read_only, bool);
reflect!("textarea", "disabled", disabled, set_disabled, bool);

/// "Limited to only non-negative numbers greater than zero with fallback": `rows` and `cols`.
fn limited(d: &Dom, it: &mut Interp<'_>, this: Value, name: &str, fallback: u32) -> V {
    let n = this_el(d, it, this)?;
    let v = get(&d.doc.borrow(), n, name).and_then(crate::browser_select::parse_u).filter(|&v| v > 0).unwrap_or(fallback);
    Ok(Value::Number(v as f64))
}

fn set_limited(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], name: &str, fallback: u32) -> V {
    let n = this_el(d, it, this)?;
    let v = bare_js::value::to_uint32(it.to_js_number(arg(a, 0))?);
    let v = if v == 0 || v > i32::MAX as u32 { fallback } else { v };
    crate::browser_dom::attr_set(d, it, n, name, &v.to_string())?;
    Ok(Value::Undefined)
}

fn rows(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    limited(d, it, this, "rows", 2)
}
fn set_rows(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    set_limited(d, it, this, a, "rows", 2)
}
fn cols(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    limited(d, it, this, "cols", 20)
}
fn set_cols(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    set_limited(d, it, this, a, "cols", 20)
}

/// The "rules for parsing integers" on `maxlength` or `minlength`: a number of 0 or more, else -1.
fn length_of(doc: &Document, n: NodeId, name: &str) -> i64 {
    let s = get(doc, n, name).unwrap_or("").trim_start_matches([' ', '\t', '\n', '\x0c', '\r']);
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    match s[..digits].parse::<i64>() {
        Ok(v) if !neg || v == 0 => v.min(i32::MAX as i64),
        _ => -1,
    }
}

fn max_length(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = this_el(d, it, this)?;
    Ok(Value::Number(length_of(&d.doc.borrow(), n, "maxlength") as f64))
}

fn min_length(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = this_el(d, it, this)?;
    Ok(Value::Number(length_of(&d.doc.borrow(), n, "minlength") as f64))
}

/// `maxLength` (`is_max`) or `minLength` setter: a negative value, and a pair out of order, are an `IndexSizeError`.
fn set_length(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], is_max: bool) -> V {
    let n = this_el(d, it, this)?;
    let v = bare_js::value::to_int32(it.to_js_number(arg(a, 0))?) as i64;
    let (name, other) = if is_max { ("maxLength", "minlength") } else { ("minLength", "maxlength") };
    let o = length_of(&d.doc.borrow(), n, other);
    let mut fail = |m: String| d.dom_err(it, "IndexSizeError", &format!("Failed to set the '{name}' property on 'HTMLTextAreaElement': {m}"));
    if v < 0 {
        return Err(fail(format!("The value provided ({v}) is not positive or 0.")));
    }
    if o >= 0 && is_max && v < o {
        return Err(fail(format!("The maxLength provided ({v}) is less than the minimum bound ({o}).")));
    }
    if o >= 0 && !is_max && v > o {
        return Err(fail(format!("The minLength provided ({v}) is greater than the maximum bound ({o}).")));
    }
    crate::browser_dom::attr_set(d, it, n, &name.to_ascii_lowercase(), &v.to_string())?;
    Ok(Value::Undefined)
}

fn set_max_length(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    set_length(d, it, this, a, true)
}
fn set_min_length(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    set_length(d, it, this, a, false)
}

/// Make the properties of the `HTMLTextAreaElement` prototype `proto`.
pub(crate) fn install(d: &Dom, it: &mut Interp<'_>, proto: ObjId) -> R<()> {
    for (name, g, s) in [
        ("value", value as H, Some(set_value as H)),
        ("defaultValue", default_value, Some(set_default_value)),
        ("textLength", text_length, None),
        ("rows", rows, Some(set_rows)),
        ("cols", cols, Some(set_cols)),
        ("maxLength", max_length, Some(set_max_length)),
        ("minLength", min_length, Some(set_min_length)),
        ("placeholder", placeholder, Some(set_placeholder)),
        ("required", required, Some(set_required)),
        ("readOnly", read_only, Some(set_read_only)),
        ("disabled", disabled, Some(set_disabled)),
        ("name", name, Some(set_name)),
        ("wrap", wrap, Some(set_wrap)),
        ("dirName", dir_name, Some(set_dir_name)),
        ("autocomplete", autocomplete, Some(set_autocomplete)),
        ("type", kind, None),
        ("form", form, None),
        ("selectionStart", sel::selection_start, Some(sel::set_selection_start)),
        ("selectionEnd", sel::selection_end, Some(sel::set_selection_end)),
        ("selectionDirection", sel::selection_direction, Some(sel::set_selection_direction)),
    ] {
        d.def_get(it, proto, name, g, s)?;
    }
    d.def_method(it, proto, "setSelectionRange", 2, sel::set_selection_range_method)?;
    d.def_method(it, proto, "select", 0, sel::select_method)?;
    crate::browser_controls::decorate(d, it, proto, true)
}
