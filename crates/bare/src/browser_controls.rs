//! Constraint validation, `labels`, `HTMLLabelElement` and `form.reset()` for `input`, `select`, `textarea` and `button`.
//! Sources: HTML Standard "Constraint validation", "The constraint validation API", "Labelable elements", "The label element", "Resetting a
//! form", "Value sanitization algorithm"; the cases (messages too) are compared with headless Chrome (`tools/fixtures/ui/{validity,labels,reset}-cases.txt`).
//!
//! What is built
//! - `validity` (a `ValidityState`, one object for each element), `willValidate`, `validationMessage`, `setCustomValidity()`,
//!   `checkValidity()` and `reportValidity()` on `input`, `select`, `textarea` and `button`; `form.checkValidity()` and `form.reportValidity()`
//!   (`browser_submit.rs`). `invalid` is a cancelable event that does not bubble, and it is trusted (Chrome does that for a script call too).
//!   There is no message bubble. The state is read again at each call; nothing is cached but the custom message.
//! - An element that is barred from validation (`disabled`, `readonly`, a `hidden`, `button`, `reset` or `image` input, a button that is not a
//!   submit button, a `datalist` descendant) has `willValidate` false, an empty `validationMessage` and `checkValidity() === true`. Its flags are
//!   still computed, as in Chrome.
//! - Flags: `valueMissing` (`required`, mutable: text kinds, number, date kinds, file, textarea; checkbox; a radio group; a select), `typeMismatch`
//!   (`email` with `multiple`, `url`), `patternMismatch` (the engine's regular expression, the `u` flag, wrapped in `^(?:...)$`; a pattern that
//!   does not compile is ignored), `tooLong` and `tooShort` (only for a value that a user typed), `rangeUnderflow`, `rangeOverflow` and
//!   `stepMismatch` (`type=number`; the step arithmetic is decimal, so 0.3 is on a step of 0.1), `badInput` (a number field with text that
//!   is no number), `customError`.
//! - The message is the one of Chrome 155, picked in the order custom, missing, type, pattern, too long, too short, underflow, overflow, step.
//!   The text of the e-mail messages follows Chrome (missing `@`, incomplete part, wrong symbol, wrong dot, else "Please enter an email
//!   address."). `title` does not change the pattern message in Chrome, so it does not here.
//! - Value sanitization (`sanitize`): text, search, tel and password lose line breaks; url and email also lose the white space at both ends
//!   (`multiple` e-mail: around each address); a number that is not a valid floating-point number is the empty string. It runs for the `value`
//!   setter and for the `value` attribute that a clean input shows. The text that a user types is not sanitized.
//! - `labels` (a live `NodeList`, one object for each control; `null` for a `hidden` input), `HTMLLabelElement` with `control`, `htmlFor` and
//!   `form`. The label and its control are found by the HTML rules (`for` names the first element with that ID of the label's tree; else the
//!   first labelable descendant).
//! - `form.reset()` and the reset buttons: `reset` (cancelable, bubbling, trusted) fires first; when it is not canceled each listed control
//!   takes its default value, checkedness, selectedness and caret. A call from inside the `reset` listener does nothing.
//! - `input.required`. The prototypes get these members when the first element of the kind gets a wrapper (`proto_for`), and the functions are
//!   shared between the prototypes, so a page without a form control pays nothing.
//!
//! Deviations (on purpose)
//! - `badInput` is only for number text. The date kinds, `range`, `color` and `file` have no value checks. `tooLong` and `tooShort` texts are
//!   written from memory of Chrome; no case checks them. No `input.valueAsNumber`, `min`, `max`, `step`, `pattern`, `minLength`, `maxLength`,
//!   `readOnly`, `placeholder` or `multiple` property on an input (script uses the attributes; each is a function that every page with an
//!   input would pay for). `fieldset`, `output` and `object` have no validity members.
//! - A radio group is read when asked (no group state), so two `checked` radios of one group in the markup both read as checked until a
//!   script or a reset checks one. A `checked` write unchecks the other radios of the group.

use crate::browser::*;
use crate::browser_dom::{arg, input_kind};
use crate::browser_lazy::lazy_global;
use crate::browser_nodelist::Src;
use crate::browser_select::{get, has, reflect};
use bare_html::dom::{Atom, Document, NodeId};
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value};
use std::borrow::Cow;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};

type V = R<Value>;

// ------------------------------------------------------------------------------------------ sanitization

fn blank(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0c' | '\r')
}

/// Does `b` hold a line feed or a carriage return? A fixed chunk with no early exit compiles to wide compares.
fn has_break(b: &[u8]) -> bool {
    let mut c = b.chunks_exact(32);
    for ch in c.by_ref() {
        let mut a = 0u8;
        for &x in ch {
            a |= ((x == b'\n') | (x == b'\r')) as u8;
        }
        if a != 0 {
            return true;
        }
    }
    c.remainder().iter().any(|&x| x == b'\n' || x == b'\r')
}

fn no_breaks(s: &str) -> Cow<'_, str> {
    if has_break(s.as_bytes()) { Cow::Owned(s.chars().filter(|&c| c != '\n' && c != '\r').collect()) } else { Cow::Borrowed(s) }
}

/// A valid floating-point number (`-`, digits, a fraction with digits, an exponent) that is finite.
fn valid_float(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    let digits = |mut i: usize| {
        let at = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        (i, i - at)
    };
    let (i, int) = digits(usize::from(b.first() == Some(&b'-')));
    let (mut i, mut frac) = (i, 0);
    if b.get(i) == Some(&b'.') {
        (i, frac) = digits(i + 1);
        if frac == 0 {
            return None;
        }
    }
    if int + frac == 0 {
        return None;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let (j, n) = digits(i + 1 + usize::from(matches!(b.get(i + 1), Some(b'-' | b'+'))));
        if n == 0 {
            return None;
        }
        i = j;
    }
    if i != b.len() { None } else { s.parse::<f64>().ok().filter(|v| v.is_finite()) }
}

/// The value sanitization algorithm of an input of `kind`.
pub(crate) fn sanitize<'a>(kind: &str, multiple: bool, s: &'a str) -> Cow<'a, str> {
    match kind {
        "text" | "search" | "tel" | "password" => no_breaks(s),
        "url" => match no_breaks(s) {
            Cow::Borrowed(b) => Cow::Borrowed(b.trim_matches(blank)),
            Cow::Owned(o) => Cow::Owned(o.trim_matches(blank).to_string()),
        },
        "email" if multiple => Cow::Owned(no_breaks(s).split(',').map(|p| p.trim_matches(blank)).collect::<Vec<_>>().join(",")),
        "email" => match no_breaks(s) {
            Cow::Borrowed(b) => Cow::Borrowed(b.trim_matches(blank)),
            Cow::Owned(o) => Cow::Owned(o.trim_matches(blank).to_string()),
        },
        "number" if valid_float(s).is_none() => Cow::Borrowed(""),
        _ => Cow::Borrowed(s),
    }
}

// ------------------------------------------------------------------------------------------ messages

/// Is `s` an absolute URL? A special scheme (not `file`) needs a host and a port that fits; any other scheme needs nothing more.
fn valid_url(s: &str) -> bool {
    let Some((scheme, rest)) = s.split_once(':') else { return false };
    let mut c = scheme.chars();
    if !c.next().is_some_and(|f| f.is_ascii_alphabetic()) || !c.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) {
        return false;
    }
    if !["http", "https", "ftp", "ws", "wss"].iter().any(|k| scheme.eq_ignore_ascii_case(k)) {
        return true;
    }
    let auth = rest.trim_start_matches(['/', '\\']).split(['/', '\\', '?', '#']).next().unwrap_or("");
    let hostport = auth.rsplit_once('@').map_or(auth, |x| x.1);
    let (host, port) = match hostport.strip_prefix('[') {
        Some(r) => r.split_once(']').map_or(("", ""), |(h, t)| (h, t.strip_prefix(':').unwrap_or(""))),
        None => hostport.rsplit_once(':').unwrap_or((hostport, "")),
    };
    !host.is_empty() && (port.is_empty() || port.parse::<u16>().is_ok())
}

/// `a@b` by the HTML regular expression; the message is Chrome's text for the first thing that is wrong.
fn email_problem(p: &str, multiple: bool) -> Option<String> {
    if p.is_empty() && multiple {
        return Some("Please enter a non-empty email address.".into());
    }
    let Some((local, domain)) = p.split_once('@') else { return Some(format!("Please include an '@' in the email address. '{p}' is missing an '@'.")) };
    if local.is_empty() {
        return Some(format!("Please enter a part followed by '@'. '{p}' is incomplete."));
    }
    if domain.is_empty() {
        return Some(format!("Please enter a part following '@'. '{p}' is incomplete."));
    }
    if let Some(c) = local.chars().find(|c| !(c.is_ascii_alphanumeric() || ".!#$%&'*+/=?^_`{|}~-".contains(*c))) {
        return Some(format!("A part followed by '@' should not contain the symbol '{c}'."));
    }
    if let Some(c) = domain.chars().find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '.')) {
        return Some(format!("A part following '@' should not contain the symbol '{c}'."));
    }
    if domain.starts_with('.') || domain.ends_with('.') || domain.contains("..") {
        return Some(format!("'.' is used at a wrong position in '{domain}'."));
    }
    let bad = domain.split('.').any(|l| l.len() > 63 || l.starts_with('-') || l.ends_with('-'));
    bad.then(|| "Please enter an email address.".to_string())
}

/// Decimal numbers for the step check: `(digits, number of fraction digits)`. Only the numbers that fit.
fn dec(s: &str) -> Option<(i128, u32)> {
    let (m, exp) = match s.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i32>().ok()?),
        None => (s, 0),
    };
    let (int, frac) = m.split_once('.').unwrap_or((m, ""));
    let neg = int.starts_with('-');
    let digits = format!("{}{frac}", int.trim_start_matches('-'));
    let scale = frac.len() as i32 - exp;
    if digits.len() > 18 || !(-18..=18).contains(&scale) {
        return None;
    }
    let mut v = digits.parse::<i128>().ok()?;
    if scale < 0 {
        v *= 10i128.pow(-scale as u32);
    }
    Some((if neg { -v } else { v }, scale.max(0) as u32))
}

fn show(v: i128, scale: u32) -> String {
    let mut s = format!("{:0>w$}", v.unsigned_abs(), w = scale as usize + 1);
    s.insert(s.len() - scale as usize, '.');
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if v < 0 && s != "0" { format!("-{s}") } else { s.to_string() }
}

fn valid(s: Option<&str>) -> Option<&str> {
    s.filter(|s| valid_float(s).is_some())
}

/// The message of a step mismatch, or `None` when `value` is on the step. `base` is `min`, else the `value` attribute, else 0.
fn step_message(value: &str, min: Option<&str>, max: Option<&str>, step: Option<&str>, vattr: Option<&str>) -> Option<String> {
    let step = match step {
        Some(s) if s.eq_ignore_ascii_case("any") => return None,
        s => valid(s).filter(|s| valid_float(s).is_some_and(|v| v > 0.0)).unwrap_or("1"),
    };
    let base = valid(min).or(valid(vattr)).unwrap_or("0");
    let (v, b, st) = (dec(value)?, dec(base)?, dec(step)?);
    let (lo, hi) = (valid(min).and_then(dec), valid(max).and_then(dec));
    let scale = [Some(v), Some(b), Some(st), lo, hi].into_iter().flatten().map(|x| x.1).max()?;
    let at = |x: (i128, u32)| x.0 * 10i128.pow(scale - x.1);
    let (v, b, st) = (at(v), at(b), at(st));
    let rem = (v - b).rem_euclid(st);
    if rem == 0 {
        return None;
    }
    let (below, above) = (v - rem, v - rem + st);
    let ok_below = lo.is_none_or(|l| below >= at(l));
    let ok_above = hi.is_none_or(|h| above <= at(h));
    Some(match (ok_below, ok_above) {
        (true, true) => format!("Please enter a valid value. The two nearest valid values are {} and {}.", show(below, scale), show(above, scale)),
        (true, false) => format!("Please enter a valid value. The nearest valid value is {}.", show(below, scale)),
        (false, true) => format!("Please enter a valid value. The nearest valid value is {}.", show(above, scale)),
        _ => "Please enter a valid value.".into(),
    })
}

// ------------------------------------------------------------------------------------------ host state

#[derive(Default)]
struct St {
    /// The `setCustomValidity` messages (not empty).
    custom: HashMap<NodeId, String>,
    /// The `ValidityState` of each element, and the element of each `ValidityState`.
    state: HashMap<NodeId, ObjId>,
    owner: HashMap<ObjId, NodeId>,
    /// The `labels` list of each control.
    labels: HashMap<NodeId, ObjId>,
    /// The forms in `reset()` now.
    locked: HashSet<NodeId>,
}

#[derive(Default)]
pub(crate) struct CtlHost {
    st: RefCell<St>,
    /// `willValidate`, `validationMessage`, `validity`, `setCustomValidity`, `labels`, `checkValidity`, `reportValidity`.
    shared: OnceCell<[ObjId; 7]>,
    /// `input.required`: getter and setter.
    required: OnceCell<[ObjId; 2]>,
    /// `ValidityState`: prototype and constructor.
    validity: OnceCell<[ObjId; 2]>,
    /// `HTMLLabelElement`: prototype and constructor.
    label: OnceCell<[ObjId; 2]>,
    /// Which of `input` (1), `button` (2) and `form` (4) got their members.
    done: Cell<u8>,
}

pub(crate) fn roots(d: &Dom, out: &mut Vec<Value>) {
    let c = &d.ctl;
    let all = c.shared.get().into_iter().flatten().chain(c.required.get().into_iter().flatten()).chain(c.validity.get().into_iter().flatten()).chain(c.label.get().into_iter().flatten());
    out.extend(all.map(|&o| Value::Object(o)));
}

/// Forget the objects that the collector freed.
pub(crate) fn prune(d: &Dom, live: &dyn Fn(ObjId) -> bool) {
    let mut st = d.ctl.st.borrow_mut();
    st.state.retain(|_, &mut o| live(o));
    st.labels.retain(|_, &mut o| live(o));
    let st = &mut *st;
    st.owner.retain(|o, _| live(*o));
}

fn illegal(it: &mut Interp<'_>) -> Error {
    it.type_error("Illegal invocation")
}

// ------------------------------------------------------------------------------------------ the check

const MISSING: u16 = 1;
const TYPE: u16 = 2;
const PATTERN: u16 = 4;
const LONG: u16 = 8;
const SHORT: u16 = 16;
const UNDER: u16 = 32;
const OVER: u16 = 64;
const STEP: u16 = 128;
const BAD: u16 = 256;
const CUSTOM: u16 = 512;

/// The flags (in the order of the `ValidityState` members) and the message that Chrome shows for them.
struct Verdict {
    flags: u16,
    msg: String,
}

impl Verdict {
    fn add(&mut self, flag: u16, msg: impl FnOnce() -> String) {
        self.flags |= flag;
        if self.msg.is_empty() {
            self.msg = msg();
        }
    }
}

fn tag_of(doc: &Document, n: NodeId) -> Option<&'static str> {
    ["input", "select", "textarea", "button"].into_iter().find(|t| doc.is_html(n, t))
}

/// The pattern `p` matches all of `value`. A pattern that does not compile matches.
fn pattern_ok(it: &mut Interp<'_>, p: &str, value: &str) -> R<bool> {
    use bare_js::regexp::{Limits, Regex, Scratch};
    let Ok(re) = Regex::from_str(&format!("^(?:{p})$"), "u") else { return Ok(true) };
    let units: Vec<u16> = value.encode_utf16().collect();
    let mut sc = Scratch::new();
    let r = re.exec(&units, 0, &Limits::default(), None, &mut sc);
    it.charge(sc.steps / 16 + 1)?;
    Ok(!matches!(r, Ok(None)))
}

const TEXTY: [&str; 11] = ["text", "search", "tel", "url", "email", "password", "number", "date", "month", "week", "time"];

impl Dom {
    /// Is `n` a candidate for constraint validation?
    pub(crate) fn will_validate(&self, it: &mut Interp<'_>, n: NodeId) -> R<bool> {
        {
            let doc = self.doc.borrow();
            let barred = match tag_of(&doc, n) {
                Some("input") => matches!(input_kind(&doc, n), "hidden" | "reset" | "button" | "image") || has(&doc, n, "readonly"),
                Some("textarea") => has(&doc, n, "readonly"),
                Some("button") => !crate::browser_form::button_state(&doc, n).0,
                Some(_) => false,
                None => true,
            };
            if barred {
                return Ok(false);
            }
            // No `datalist` was ever made: nothing to walk for.
            if doc.find_atom("datalist").is_some() {
                let mut cur = doc.parent(n);
                while let Some(p) = cur {
                    it.charge(1)?;
                    if doc.is_html(p, "datalist") {
                        return Ok(false);
                    }
                    cur = doc.parent(p);
                }
            }
        }
        Ok(!self.is_disabled_control(it, n)?)
    }

    fn verdict(&self, it: &mut Interp<'_>, n: NodeId) -> R<Verdict> {
        let mut v = Verdict { flags: 0, msg: String::new() };
        let (tag, kind, required) = {
            let doc = self.doc.borrow();
            let Some(tag) = tag_of(&doc, n) else { return Ok(v) };
            (tag, if tag == "input" { input_kind(&doc, n) } else { "" }, has(&doc, n, "required"))
        };
        let value = self.input_value_of(n).unwrap_or_default();
        let texty = tag == "textarea" || TEXTY.contains(&kind) || kind == "datetime-local";
        // The attributes are read only by the checks that use them: a control with no value and no `required` is one flag-free test.
        if required || tag == "select" || kind == "radio" {
            let mutable = !has(&self.doc.borrow(), n, "readonly") && !self.is_disabled_control(it, n)?;
            match (tag, kind) {
                ("select", _) if required && crate::browser_select::value_missing(self, it, n)? => v.add(MISSING, || "Please select an item in the list.".into()),
                ("input", "checkbox") if !self.input_checked(n) => v.add(MISSING, || "Please check this box if you want to proceed.".into()),
                ("input", "radio") => {
                    let group = self.radio_group(it, n)?;
                    let required = required || group.iter().any(|&o| has(&self.doc.borrow(), o, "required"));
                    if required && !self.input_checked(n) && !group.iter().any(|&o| self.input_checked(o)) {
                        v.add(MISSING, || "Please select one of these options.".into());
                    }
                }
                ("input", "file") if mutable => v.add(MISSING, || "Please select a file.".into()),
                _ if mutable && texty && value.is_empty() => v.add(MISSING, || "Please fill out this field.".into()),
                _ => {}
            }
        }
        if !value.is_empty() && texty {
            let doc = self.doc.borrow();
            let at = |k: &str| get(&doc, n, k).map(str::to_string);
            if tag == "input" {
                let multiple = kind == "email" && has(&doc, n, "multiple");
                let pieces: Vec<&str> = if multiple { value.split(',').collect() } else { vec![value.as_str()] };
                match kind {
                    "email" => {
                        if let Some(m) = pieces.iter().find_map(|p| email_problem(p, multiple)) {
                            v.add(TYPE, || m);
                        }
                    }
                    "url" if !valid_url(&value) => v.add(TYPE, || "Please enter a URL.".into()),
                    _ => {}
                }
                let pattern = if matches!(kind, "text" | "search" | "tel" | "url" | "email" | "password") { at("pattern") } else { None };
                drop(doc);
                if let Some(p) = pattern.as_deref().filter(|p| !p.is_empty()) {
                    for piece in &pieces {
                        if !pattern_ok(it, p, piece)? {
                            v.add(PATTERN, || "Please match the requested format.".into());
                            break;
                        }
                    }
                }
                if kind == "number" {
                    let doc = self.doc.borrow();
                    let (min, max, step, vattr) = (get(&doc, n, "min"), get(&doc, n, "max"), get(&doc, n, "step"), get(&doc, n, "value"));
                    match valid_float(&value) {
                        None => v.add(BAD, || "Please enter a number.".into()),
                        Some(x) => {
                            let num = |s: Option<&str>| s.and_then(valid_float);
                            if let Some(m) = num(min).filter(|&m| x < m) {
                                v.add(UNDER, || format!("Value must be greater than or equal to {m}."));
                            }
                            if let Some(m) = num(max).filter(|&m| x > m) {
                                v.add(OVER, || format!("Value must be less than or equal to {m}."));
                            }
                            if let Some(m) = step_message(&value, min, max, step, vattr) {
                                v.add(STEP, || m);
                            }
                        }
                    }
                }
            }
        }
        // `minlength` and `maxlength` flag only a value that a user typed.
        if !value.is_empty() && texty && matches!(kind, "" | "text" | "search" | "tel" | "url" | "email" | "password") && self.st.borrow().inputs.get(&n).is_some_and(|i| i.typed) {
            let units = value.encode_utf16().count();
            let doc = self.doc.borrow();
            let limit = |k: &str| get(&doc, n, k).and_then(crate::browser_select::parse_u);
            if let Some(m) = limit("maxlength").filter(|&m| units > m as usize) {
                v.add(LONG, || format!("Please shorten this text to {m} characters or less (you are currently using {units} characters)."));
            }
            if let Some(m) = limit("minlength").filter(|&m| units < m as usize) {
                v.add(SHORT, || format!("Please lengthen this text to {m} characters or more (you are currently using {units} characters)."));
            }
        }
        let custom = {
            let st = self.ctl.st.borrow();
            if st.custom.is_empty() { None } else { st.custom.get(&n).cloned() }
        };
        if let Some(c) = custom {
            v.flags |= CUSTOM;
            v.msg = c;
        }
        Ok(v)
    }

    /// Does `n` fail its constraints, and is it a candidate? (The check of `checkValidity`, of the form and of submission.)
    pub(crate) fn is_invalid(&self, it: &mut Interp<'_>, n: NodeId) -> R<bool> {
        Ok(self.will_validate(it, n)? && self.verdict(it, n)?.flags != 0)
    }
}

// ------------------------------------------------------------------------------------------ the members

/// `this` is an `input`, `select`, `textarea` or `button`.
pub(crate) fn control(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
    let n = d.el(it, this)?;
    let ok = tag_of(&d.doc.borrow(), n).is_some();
    if ok { Ok(n) } else { Err(illegal(it)) }
}

fn will_validate(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = control(d, it, this)?;
    Ok(Value::Bool(d.will_validate(it, n)?))
}

fn validation_message(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = control(d, it, this)?;
    let m = if d.will_validate(it, n)? { d.verdict(it, n)?.msg } else { String::new() };
    d.js_str(it, &m)
}

fn set_custom_validity(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = control(d, it, this)?;
    if a.is_empty() {
        let name = match tag_of(&d.doc.borrow(), n) {
            Some("input") => "HTMLInputElement",
            Some("select") => "HTMLSelectElement",
            Some("textarea") => "HTMLTextAreaElement",
            _ => "HTMLButtonElement",
        };
        return Err(it.type_error(&format!("Failed to execute 'setCustomValidity' on '{name}': 1 argument required, but only 0 present.")));
    }
    let s = d.string_arg(it, a[0])?;
    let mut st = d.ctl.st.borrow_mut();
    if s.is_empty() {
        st.custom.remove(&n);
    } else {
        st.custom.insert(n, s);
    }
    Ok(Value::Undefined)
}

fn validity(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = control(d, it, this)?;
    let cached = d.ctl.st.borrow().state.get(&n).copied();
    if let Some(o) = cached {
        return Ok(Value::Object(o));
    }
    let [proto, _] = made_validity(d, it)?;
    let o = d.obj(it, proto)?;
    let mut st = d.ctl.st.borrow_mut();
    st.state.insert(n, o);
    st.owner.insert(o, n);
    Ok(Value::Object(o))
}

fn flag<const I: u32>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let owner = match this {
        Value::Object(o) => d.ctl.st.borrow().owner.get(&o).copied(),
        _ => None,
    };
    let Some(n) = owner else { return Err(illegal(it)) };
    let f = d.verdict(it, n)?.flags;
    Ok(Value::Bool(if I == 10 { f == 0 } else { f & (1 << I) != 0 }))
}

fn made_validity(d: &Dom, it: &mut Interp<'_>) -> R<[ObjId; 2]> {
    if let Some(&m) = d.ctl.validity.get() {
        return Ok(m);
    }
    let op = it.object_prototype();
    let proto = d.obj(it, op)?;
    let getters: [(&str, H); 11] = [
        ("valueMissing", flag::<0>),
        ("typeMismatch", flag::<1>),
        ("patternMismatch", flag::<2>),
        ("tooLong", flag::<3>),
        ("tooShort", flag::<4>),
        ("rangeUnderflow", flag::<5>),
        ("rangeOverflow", flag::<6>),
        ("stepMismatch", flag::<7>),
        ("badInput", flag::<8>),
        ("customError", flag::<9>),
        ("valid", flag::<10>),
    ];
    for (name, g) in getters {
        d.def_get(it, proto, name, g, None)?;
    }
    let ctor = interface(d, it, proto, "ValidityState")?;
    let _ = d.ctl.validity.set([proto, ctor]);
    Ok([proto, ctor])
}

/// The constructor `name` (illegal to call) of `proto`, with the `toStringTag`.
fn interface(d: &Dom, it: &mut Interp<'_>, proto: ObjId, name: &str) -> R<ObjId> {
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    let f = d.add_fn(it, illegal_constructor, name, 0, true)?;
    it.define_data(f, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(f), true, false, true)?;
    crate::browser_lazy::tag(d, it, proto, name)?;
    Ok(f)
}

fn labels(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = control(d, it, this)?;
    if !labelable(&d.doc.borrow(), n) {
        return Ok(Value::Null);
    }
    let cached = d.ctl.st.borrow().labels.get(&n).copied();
    if let Some(o) = cached {
        return Ok(Value::Object(o));
    }
    let v = crate::browser_nodelist::make_list(d, it, crate::browser_nodelist::list_proto(d), Src::Labels(n))?;
    let Value::Object(o) = v else { unreachable!() };
    d.ctl.st.borrow_mut().labels.insert(n, o);
    Ok(v)
}

fn shared(d: &Dom, it: &mut Interp<'_>) -> R<[ObjId; 7]> {
    if let Some(&s) = d.ctl.shared.get() {
        return Ok(s);
    }
    let hs: [(H, &str, u32); 7] = [
        (will_validate, "get willValidate", 0),
        (validation_message, "get validationMessage", 0),
        (validity, "get validity", 0),
        (set_custom_validity, "setCustomValidity", 1),
        (labels, "get labels", 0),
        (crate::browser_submit::control_check_validity, "checkValidity", 0),
        (crate::browser_submit::control_report_validity, "reportValidity", 0),
    ];
    let mut v = Vec::with_capacity(7);
    for (h, name, len) in hs {
        v.push(d.add_fn(it, h, name, len, false)?);
    }
    let out: [ObjId; 7] = v.try_into().expect("seven");
    let _ = d.ctl.shared.set(out);
    Ok(out)
}

/// Define the validation members and `labels` on the prototype `proto` (`check`: also `checkValidity` and `reportValidity`, which the
/// `input` and `button` prototypes have from the start).
pub(crate) fn decorate(d: &Dom, it: &mut Interp<'_>, proto: ObjId, check: bool) -> R<()> {
    let f = shared(d, it)?;
    for (name, g) in [("willValidate", f[0]), ("validationMessage", f[1]), ("validity", f[2]), ("labels", f[4])] {
        let k = d.key(it, name)?;
        d.define_accessor(it, proto, k, g, None)?;
    }
    let k = d.key(it, "setCustomValidity")?;
    it.define_data(proto, k, Value::Object(f[3]), true, false, true)?;
    if check {
        for (name, i) in [("checkValidity", 5), ("reportValidity", 6)] {
            let k = d.key(it, name)?;
            it.define_data(proto, k, Value::Object(f[i]), true, false, true)?;
        }
    }
    Ok(())
}

reflect!("input", "required", input_required, set_input_required, bool);

/// The prototype of a new wrapper of an HTML `input`, `button`, `form` or `label`. The first `input`, the first `button` and the first `form`
/// give their prototype the members of this file.
pub(crate) fn proto_for(d: &Dom, it: &mut Interp<'_>, tag: &str) -> R<ObjId> {
    if tag == "label" {
        return Ok(made_label(d, it)?[0]);
    }
    let p = d.p();
    if tag == "form" {
        let done = d.ctl.done.get();
        if done & 4 == 0 {
            d.def_method(it, p.form, "reset", 0, form_reset)?;
            d.ctl.done.set(done | 4);
        }
        return Ok(p.form);
    }
    let (bit, proto) = if tag == "input" { (1, p.input) } else { (2, p.button) };
    let done = d.ctl.done.get();
    if done & bit == 0 {
        match tag {
            "input" => {
                decorate(d, it, proto, false)?;
                let k = d.key(it, "required")?;
                let g = d.add_fn(it, input_required, "get required", 0, false)?;
                let s = d.add_fn(it, set_input_required, "set required", 1, false)?;
                let _ = d.ctl.required.set([g, s]);
                d.define_accessor(it, proto, k, g, Some(s))?;
            }
            _ => decorate(d, it, proto, false)?,
        }
        d.ctl.done.set(done | bit);
    }
    Ok(proto)
}

// ------------------------------------------------------------------------------------------ label

/// Is `n` a labelable element?
fn labelable(doc: &Document, n: NodeId) -> bool {
    ["button", "select", "textarea", "meter", "output", "progress"].iter().any(|t| doc.is_html(n, t)) || (doc.is_html(n, "input") && input_kind(doc, n) != "hidden")
}

fn root_of(doc: &Document, it: &mut Interp<'_>, n: NodeId) -> R<NodeId> {
    let mut top = n;
    while let Some(p) = doc.parent(top) {
        it.charge(1)?;
        top = p;
    }
    Ok(top)
}

impl Dom {
    /// The control of a label: the element that its `for` names (the first with that ID in the tree of the label), else its first labelable
    /// descendant. `None` when it is not labelable.
    pub(crate) fn label_control(&self, it: &mut Interp<'_>, label: NodeId) -> R<Option<NodeId>> {
        let doc = self.doc.borrow();
        if let Some(id) = get(&doc, label, "for") {
            let root = root_of(&doc, it, label)?;
            let (mut cur, mut depth) = (root, 0);
            while !id.is_empty() && let Some((c, d)) = crate::browser_dom::next_in(&doc, root, cur, depth) {
                it.charge(1)?;
                (cur, depth) = (c, d);
                if doc.attr(c, Atom::ID) == Some(id) {
                    return Ok(labelable(&doc, c).then_some(c));
                }
            }
            return Ok(None);
        }
        let (mut cur, mut depth) = (label, 0);
        while let Some((c, d)) = crate::browser_dom::next_in(&doc, label, cur, depth) {
            it.charge(1)?;
            (cur, depth) = (c, d);
            if labelable(&doc, c) {
                return Ok(Some(c));
            }
        }
        Ok(None)
    }
}

/// The labels of `control` in tree order, for `Src::Labels`.
pub(crate) fn label_ids(d: &Dom, it: &mut Interp<'_>, control: NodeId) -> R<Vec<NodeId>> {
    let (root, all) = {
        let doc = d.doc.borrow();
        let root = root_of(&doc, it, control)?;
        let (mut cur, mut depth, mut all) = (root, 0, Vec::new());
        while let Some((c, dp)) = crate::browser_dom::next_in(&doc, root, cur, depth) {
            it.charge(1)?;
            (cur, depth) = (c, dp);
            if doc.is_html(c, "label") {
                all.push(c);
            }
        }
        (root, all)
    };
    let _ = root;
    let mut out = Vec::new();
    for l in all {
        if d.label_control(it, l)? == Some(control) {
            if out.len() >= d.lim.max_results {
                return Err(d.lim("dom-results"));
            }
            out.push(l);
        }
    }
    Ok(out)
}

fn label_el(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
    d.html_el(it, this, "label")
}

fn label_control(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let l = label_el(d, it, this)?;
    let c = d.label_control(it, l)?;
    d.wrap_opt(it, c)
}

fn label_form(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let l = label_el(d, it, this)?;
    let f = match d.label_control(it, l)? {
        Some(c) => d.form_owner(it, c)?,
        None => None,
    };
    d.wrap_opt(it, f)
}

reflect!("label", "for", html_for, set_html_for, str);

fn made_label(d: &Dom, it: &mut Interp<'_>) -> R<[ObjId; 2]> {
    if let Some(&m) = d.ctl.label.get() {
        return Ok(m);
    }
    let proto = d.obj(it, d.p().html)?;
    d.def_get(it, proto, "control", label_control, None)?;
    d.def_get(it, proto, "htmlFor", html_for, Some(set_html_for))?;
    d.def_get(it, proto, "form", label_form, None)?;
    let ctor = interface(d, it, proto, "HTMLLabelElement")?;
    let _ = d.ctl.label.set([proto, ctor]);
    Ok([proto, ctor])
}

// ------------------------------------------------------------------------------------------ reset

impl Dom {
    /// The reset algorithm of a control (HTML "reset algorithm" of input, select and textarea).
    fn reset_control(&self, it: &mut Interp<'_>, n: NodeId) -> R<()> {
        let (tag, kind) = {
            let doc = self.doc.borrow();
            (tag_of(&doc, n), if doc.is_html(n, "input") { input_kind(&doc, n) } else { "" })
        };
        match tag {
            Some("select") => return self.sel_reset(it, n),
            Some("input" | "textarea") => {}
            _ => return Ok(()),
        }
        {
            let mut st = self.st.borrow_mut();
            let old = st.inputs.get_mut(&n).map_or(0, |i| {
                (i.checked, i.sel, i.typed, i.edited) = (None, None, false, None);
                i.value.take().map_or(0, |v| v.len())
            });
            st.input_bytes -= old;
        }
        if tag == Some("textarea") {
            let end = self.with_input_value(n, |v| crate::browser_selection::caret_end(v, true)).expect("a textarea");
            self.st.borrow_mut().inputs.entry(n).or_default().sel = Some(end);
        } else if kind == "radio" && self.input_checked(n) {
            for o in self.radio_group(it, n)? {
                self.st.borrow_mut().inputs.entry(o).or_default().checked = Some(false);
            }
        }
        Ok(())
    }

    /// `form.reset()` and a reset button: the `reset` event, then the reset of each control. Nothing when the form is already resetting.
    pub(crate) fn reset_form(&self, it: &mut Interp<'_>, form: NodeId) -> R<()> {
        if !self.ctl.st.borrow_mut().locked.insert(form) {
            return Ok(());
        }
        let r = (|| {
            let e = self.new_event(it, "reset", true, true, false, None)?;
            self.st.borrow_mut().events.get_mut(&e).expect("just made").trusted = true;
            if self.dispatch(it, Target::Node(form), e)? {
                for n in self.owned_controls(it, form)? {
                    self.reset_control(it, n)?;
                }
            }
            Ok(())
        })();
        self.ctl.st.borrow_mut().locked.remove(&form);
        r
    }
}

pub(crate) fn form_reset(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let f = d.html_el(it, this, "form")?;
    d.reset_form(it, f)?;
    Ok(Value::Undefined)
}

// ------------------------------------------------------------------------------------------ install

fn ens_validity(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    made_validity(d, it).map(|_| ())
}

fn ens_label(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    made_label(d, it).map(|_| ())
}

lazy_global!(lazy_validity, "ValidityState", ens_validity, |d| d.ctl.validity.get().expect("made")[1]);
lazy_global!(lazy_label, "HTMLLabelElement", ens_label, |d| d.ctl.label.get().expect("made")[1]);

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    crate::browser_lazy::install(d, it, &[("ValidityState", lazy_validity as H), ("HTMLLabelElement", lazy_label)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizing_follows_the_value_sanitization_algorithm() {
        assert_eq!(sanitize("text", false, "a\r\nb"), "ab");
        assert_eq!(sanitize("url", false, " a\nb "), "ab");
        assert_eq!(sanitize("email", true, " a@b , c@d "), "a@b,c@d");
        for (v, ok) in [("5", true), (".5", true), ("-.5", true), ("1e5", true), ("5.", false), ("+5", false), ("1e", false), ("1e400", false), ] {
            assert_eq!(sanitize("number", false, v) == v, ok, "{v:?}");
        }
    }

    #[test]
    fn email_problems_follow_the_standard_shape() {
        for ok in ["a.b+c@example.com", "a@b"] {
            assert_eq!(email_problem(ok, false), None, "{ok}");
        }
        for bad in ["a@", "@b", "a b@c.d", "a@-b.c", "a@b..c", "ab"] {
            assert!(email_problem(bad, false).is_some(), "{bad}");
        }
    }

    #[test]
    fn urls_need_a_scheme_and_for_special_schemes_a_host() {
        for ok in ["http://a.test", "mailto:a@b", "x:y", "file:///a", "ftp://x", "http://[::1]", "http://a:80", "data:,x", "http://u@a"] {
            assert!(valid_url(ok), "{ok}");
        }
        for bad in ["a.test", "http:", "http://", "1http://x", "//a.test", "/a", "http://a:99999", "http://:80"] {
            assert!(!valid_url(bad), "{bad}");
        }
    }

    #[test]
    fn steps_use_decimal_numbers() {
        assert_eq!(step_message("0.3", None, None, Some("0.1"), None), None);
        assert_eq!(step_message("0.25", None, None, Some("0.1"), None).as_deref(), Some("Please enter a valid value. The two nearest valid values are 0.2 and 0.3."));
        assert_eq!(step_message("-0.05", None, None, Some("0.1"), None).as_deref(), Some("Please enter a valid value. The two nearest valid values are -0.1 and 0."));
        assert_eq!(step_message("10", Some("0"), Some("10"), Some("3"), None).as_deref(), Some("Please enter a valid value. The nearest valid value is 9."));
        assert_eq!(step_message("1.5", None, None, Some("any"), None), None);
    }
}
