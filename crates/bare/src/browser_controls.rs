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
//!   (`multiple` e-mail: around each address); a number that is not a valid floating-point number is the empty string, and so is a date,
//!   month, week, time or local date and time that is not valid (a valid local date and time takes its normalized form). It runs for the `value`
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
//!   written from memory of Chrome; no case checks them. No `input.valueAsNumber`, `valueAsDate`, `stepUp`, `stepDown`, `list` or `showPicker`.
//!   `placeholder`, `min`, `max`, `step`, `pattern`, `autocomplete`, `multiple`, `maxLength`, `minLength`, `size`, `button.value` and the
//!   `form` reflections are made with the first wrapper of their tag (`browser_reflect.rs`), so a page without one does not pay for them.
//!   `form.elements` and `form.length` are the live list of `Dom::form_elements`. `fieldset`, `output` and `object` have no validity members.
//! - A radio group is read when asked (no group state), so two `checked` radios of one group in the markup both read as checked until a
//!   script or a reset checks one. A `checked` write unchecks the other radios of the group.

use crate::eng::{Eng, Ops};
use crate::browser::*;
use crate::browser_dom::input_kind;
use crate::browser_lazy::lazy_global;
use crate::browser_nodelist::Src;
use crate::browser_scoped::*;
use crate::browser_select::{get, has, reflect_s};
use bare_html::dom::{Atom, Document, NodeId};
use bare_js_api::{Args, HostFn, Local};
use std::borrow::Cow;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};

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

/// `n` ASCII digits at the start of `s`: their number and the rest.
fn digits_of(s: &str, n: usize) -> Option<(u32, &str)> {
    let d = s.get(..n)?;
    if !d.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((d.parse().ok()?, &s[n..]))
}

fn leap(y: u32) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

/// A valid month string at the start of `s` (a year of four or more digits above zero, `-`, a month): the year, the month and the rest.
fn month_of(s: &str) -> Option<(u32, u32, &str)> {
    let n = s.bytes().take_while(u8::is_ascii_digit).count();
    let (y, s) = if n >= 4 { digits_of(s, n)? } else { return None };
    let (m, s) = digits_of(s.strip_prefix('-')?, 2)?;
    (y > 0 && (1..=12).contains(&m)).then_some((y, m, s))
}

/// A valid date string at the start of `s`: the rest.
fn date_of(s: &str) -> Option<&str> {
    let (y, m, s) = month_of(s)?;
    let (d, s) = digits_of(s.strip_prefix('-')?, 2)?;
    let last = match m {
        2 => 28 + u32::from(leap(y)),
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=last).contains(&d).then_some(s)
}

/// A valid week string. A year has 53 weeks when it starts on a Thursday, or on a Wednesday in a leap year.
fn valid_week(s: &str) -> bool {
    let n = s.bytes().take_while(u8::is_ascii_digit).count();
    let Some((y, s)) = (n >= 4).then(|| digits_of(s, n)).flatten() else { return false };
    let Some((w, rest)) = s.strip_prefix("-W").and_then(|s| digits_of(s, 2)) else { return false };
    let p = y.wrapping_sub(1);
    // The day of the week of January 1 (0 is Sunday), by Gauss.
    let jan1 = (1 + 5 * (p % 4) + 4 * (p % 100) + 6 * (p % 400)) % 7;
    let last = if jan1 == 4 || (jan1 == 3 && leap(y)) { 53 } else { 52 };
    y > 0 && rest.is_empty() && (1..=last).contains(&w)
}

/// A valid time string at the start of `s`: its normalized form (the seconds and the fraction only when they are not zero) and the rest.
fn time_of(s: &str) -> Option<(String, &str)> {
    let (h, s) = digits_of(s, 2)?;
    let (m, mut s) = digits_of(s.strip_prefix(':')?, 2)?;
    if h > 23 || m > 59 {
        return None;
    }
    let (mut sec, mut frac) = (0, "");
    if let Some(r) = s.strip_prefix(':') {
        (sec, s) = digits_of(r, 2).filter(|&(v, _)| v <= 59)?;
        if let Some(r) = s.strip_prefix('.') {
            let n = r.bytes().take_while(u8::is_ascii_digit).count();
            if !(1..=3).contains(&n) {
                return None;
            }
            (frac, s) = (r[..n].trim_end_matches('0'), &r[n..]);
        }
    }
    let mut out = format!("{h:02}:{m:02}");
    if sec != 0 || !frac.is_empty() {
        out += &format!(":{sec:02}");
    }
    if !frac.is_empty() {
        out = out + "." + frac;
    }
    Some((out, s))
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
        "date" if date_of(s) != Some("") => Cow::Borrowed(""),
        "month" if !month_of(s).is_some_and(|(_, _, r)| r.is_empty()) => Cow::Borrowed(""),
        "week" if !valid_week(s) => Cow::Borrowed(""),
        "time" if !time_of(s).is_some_and(|(_, r)| r.is_empty()) => Cow::Borrowed(""),
        // A valid local date and time string becomes its normalized form: `T`, and the shortest time.
        "datetime-local" => {
            let local = date_of(s).and_then(|r| {
                let (t, rest) = time_of(r.strip_prefix(['T', ' '])?)?;
                rest.is_empty().then(|| format!("{}T{t}", &s[..s.len() - r.len()]))
            });
            match local {
                Some(v) if v == s => Cow::Borrowed(s),
                Some(v) => Cow::Owned(v),
                None => Cow::Borrowed(""),
            }
        }
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
struct St<E: Eng> {
    /// The `setCustomValidity` messages (not empty).
    custom: HashMap<NodeId, String>,
    /// The `ValidityState` of each element, and the element of each `ValidityState`.
    state: IdMap<NodeId, <E as Eng>::Obj>,
    owner: IdMap<<E as Eng>::Obj, NodeId>,
    /// The `labels` list of each control.
    labels: IdMap<NodeId, <E as Eng>::Obj>,
    /// The forms in `reset()` now.
    locked: HashSet<NodeId>,
}

#[derive(Default)]
pub(crate) struct CtlHost<E: Eng> {
    st: RefCell<St<E>>,
    /// The shared objects below, in persistent handles: the engine roots them. The `ObjId`s in the cells are for lookups.
    keep: RefCell<Vec<bare_js_api::Persistent>>,
    /// `willValidate`, `validationMessage`, `validity`, `setCustomValidity`, `labels`, `checkValidity`, `reportValidity`.
    shared: OnceCell<[<E as Eng>::Obj; 7]>,
    /// `input.required`: getter and setter.
    required: OnceCell<[<E as Eng>::Obj; 2]>,
    /// `ValidityState`: prototype and constructor.
    validity: OnceCell<[<E as Eng>::Obj; 2]>,
    /// `HTMLLabelElement`: prototype and constructor.
    label: OnceCell<[<E as Eng>::Obj; 2]>,
    /// Which of `input` (1), `button` (2) and `form` (4) got their members.
    done: Cell<u8>,
}

/// Keep the shared objects `objs` alive for the life of the page, in persistent handles of `keep`.
pub(crate) fn hold<E: Eng>(it: &mut E::Vm, keep: &RefCell<Vec<bare_js_api::Persistent>>, objs: &[E::Obj]) {
    keep.borrow_mut().extend(objs.iter().map(|&o| it.persist_value(E::obj_raw(o))));
}

/// Forget the `ValidityState` `o`, which the collector freed. The entry of its element may name a newer one.
pub(crate) fn prune_validity<E: Eng>(d: &Dom<E>, o: E::Obj) {
    let mut st = d.ctl.st.borrow_mut();
    if let Some(n) = st.owner.remove(&o)
        && st.state.get(&n) == Some(&o)
    {
        st.state.remove(&n);
    }
}

/// Forget the `labels` list `o` of the control `n`, which the collector freed.
pub(crate) fn forget_labels<E: Eng>(d: &Dom<E>, n: NodeId, o: E::Obj) {
    let mut st = d.ctl.st.borrow_mut();
    if st.labels.get(&n) == Some(&o) {
        st.labels.remove(&n);
    }
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
fn pattern_ok<E: Eng>(it: &mut E::Vm, p: &str, value: &str) -> R<E, bool> {
    use bare_vex::regexp::{Scratch, compile, exec, image_groups};
    let pattern: Vec<u16> = format!("^(?:{p})$").encode_utf16().collect();
    let Ok(img) = compile(&pattern, "u") else { return Ok(true) };
    let units: Vec<u16> = value.encode_utf16().collect();
    let (mut sc, mut out) = (Scratch::new(), vec![0u32; 2 * (image_groups(&img) + 1)]);
    let mut fuel = u64::MAX;
    let r = exec(&img, &units, 0, &mut sc, &mut fuel, &mut out);
    it.charge_o(sc.steps / 16 + 1)?;
    Ok(!matches!(r, Ok(false)))
}

const TEXTY: [&str; 11] = ["text", "search", "tel", "url", "email", "password", "number", "date", "month", "week", "time"];

impl<E: Eng> Dom<E> {
    /// Is `n` a candidate for constraint validation?
    pub(crate) fn will_validate(&self, it: &mut E::Vm, n: NodeId) -> R<E, bool> {
        {
            let doc = self.doc.borrow();
            let barred = match tag_of(&doc, n) {
                Some("input") => matches!(input_kind(&doc, n), "hidden" | "reset" | "button" | "image") || has(&doc, n, "readonly"),
                Some("textarea") => has(&doc, n, "readonly"),
                Some("button") => !crate::browser_form::button_state(&doc, n).0,
                Some(_) => false,
                None if self.is_face(n) => has(&doc, n, "readonly"),
                None => true,
            };
            if barred {
                return Ok(false);
            }
            // No `datalist` was ever made: nothing to walk for.
            if doc.find_atom("datalist").is_some() {
                let mut cur = doc.parent(n);
                while let Some(p) = cur {
                    it.charge_o(1)?;
                    if doc.is_html(p, "datalist") {
                        return Ok(false);
                    }
                    cur = doc.parent(p);
                }
            }
        }
        Ok(!self.is_disabled_control(it, n)?)
    }

    fn verdict(&self, it: &mut E::Vm, n: NodeId) -> R<E, Verdict> {
        if let Some((flags, msg)) = self.face_verdict(n) {
            return Ok(Verdict { flags, msg });
        }
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
                        if !pattern_ok::<E>(it, p, piece)? {
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
    pub(crate) fn is_invalid(&self, it: &mut E::Vm, n: NodeId) -> R<E, bool> {
        Ok(self.will_validate(it, n)? && self.verdict(it, n)?.flags != 0)
    }
}

// ------------------------------------------------------------------------------------------ the members

/// `this` is an `input`, `select`, `textarea` or `button`.
pub(crate) fn control<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, NodeId> {
    let n = d.el_s(s, this)?;
    let ok = tag_of(&d.doc.borrow(), n).is_some();
    if ok { Ok(n) } else { Err(s.type_error("Illegal invocation")) }
}

fn will_validate<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = control(d, s, this)?;
    let w = old::<E, _>(s, |it| d.will_validate(it, n))?;
    Ok(s.boolean(w))
}

fn validation_message<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = control(d, s, this)?;
    let m = old::<E, _>(s, |it| message_of(d, it, n))?;
    d.js_str_s(s, &m)
}

/// The `validationMessage` of `n`.
pub(crate) fn message_of<E: Eng>(d: &Dom<E>, it: &mut E::Vm, n: NodeId) -> R<E, String> {
    Ok(if d.will_validate(it, n)? { d.verdict(it, n)?.msg } else { String::new() })
}

fn set_custom_validity<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = control(d, s, this)?;
    let Some(v) = a.get(0) else {
        let name = match tag_of(&d.doc.borrow(), n) {
            Some("input") => "HTMLInputElement",
            Some("select") => "HTMLSelectElement",
            Some("textarea") => "HTMLTextAreaElement",
            _ => "HTMLButtonElement",
        };
        return Err(s.type_error(&format!("Failed to execute 'setCustomValidity' on '{name}': 1 argument required, but only 0 present.")));
    };
    let t = d.string_arg_s(s, v)?;
    let mut st = d.ctl.st.borrow_mut();
    if t.is_empty() {
        st.custom.remove(&n);
    } else {
        st.custom.insert(n, t);
    }
    Ok(s.undefined())
}

fn validity<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = control(d, s, this)?;
    validity_of(d, s, n)
}

/// The `ValidityState` of `n`.
pub(crate) fn validity_of<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, n: NodeId) -> Res<'s, Local<'s>> {
    let cached = d.ctl.st.borrow().state.get(&n).copied();
    if let Some(o) = cached {
        return Ok(d.obj_local(s, o));
    }
    let [proto, _] = old::<E, _>(s, |it| made_validity(d, it))?;
    let o = d.obj_s(s, proto)?;
    let l = d.obj_local(s, o);
    // An owner: the collector prunes the two entries when `o` dies.
    let key = d.st.borrow_mut().owners.key(Owner::Source(o)) | crate::browser_sources::Source::Validity as u32;
    s.set_host_owner(l, key)?;
    let mut st = d.ctl.st.borrow_mut();
    st.state.insert(n, o);
    st.owner.insert(o, n);
    Ok(l)
}

fn flag<'s, const I: u32, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let owner = match obj_id::<E>(s, this) {
        Some(o) => d.ctl.st.borrow().owner.get(&o).copied(),
        None => None,
    };
    let Some(n) = owner else { return Err(s.type_error("Illegal invocation")) };
    let f = old::<E, _>(s, |it| d.verdict(it, n))?.flags;
    Ok(s.boolean(if I == 10 { f == 0 } else { f & (1 << I) != 0 }))
}

fn made_validity<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, [E::Obj; 2]> {
    if let Some(&m) = d.ctl.validity.get() {
        return Ok(m);
    }
    let op = it.object_prototype();
    let proto = d.obj(it, op)?;
    let getters: [(&str, HS<E>); 11] = [
        ("valueMissing", flag::<0, _>),
        ("typeMismatch", flag::<1, _>),
        ("patternMismatch", flag::<2, _>),
        ("tooLong", flag::<3, _>),
        ("tooShort", flag::<4, _>),
        ("rangeUnderflow", flag::<5, _>),
        ("rangeOverflow", flag::<6, _>),
        ("stepMismatch", flag::<7, _>),
        ("badInput", flag::<8, _>),
        ("customError", flag::<9, _>),
        ("valid", flag::<10, _>),
    ];
    for (name, g) in getters {
        d.def_get_s(it, proto, name, g, None)?;
    }
    let ctor = interface(d, it, proto, "ValidityState")?;
    hold::<E>(it, &d.ctl.keep, &[proto, ctor]);
    let _ = d.ctl.validity.set([proto, ctor]);
    Ok([proto, ctor])
}

/// The constructor `name` (illegal to call) of `proto`, with the `toStringTag`.
fn interface<E: Eng>(d: &Dom<E>, it: &mut E::Vm, proto: E::Obj, name: &str) -> R<E, E::Obj> {
    let f = crate::browser_custom::interface_ctor(d, it, name, true)?;
    link(d, it, proto, name, f)
}

/// The same with the constructor function `h`.
pub(crate) fn interface_with<E: Eng>(d: &Dom<E>, it: &mut E::Vm, proto: E::Obj, name: &str, h: HS<E>) -> R<E, E::Obj> {
    let f = d.add_fn_s_obj(it, h, name, 0, HostFn::Ctor)?;
    link(d, it, proto, name, f)
}

fn link<E: Eng>(d: &Dom<E>, it: &mut E::Vm, proto: E::Obj, name: &str, f: E::Obj) -> R<E, E::Obj> {
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    it.define_data(f, kp, E::obj_raw(proto), false, false, false)?;
    it.define_data(proto, kc, E::obj_raw(f), true, false, true)?;
    crate::browser_lazy::tag(d, it, proto, name)?;
    Ok(f)
}

fn labels<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = control(d, s, this)?;
    labels_of(d, s, n)
}

/// The `labels` of `n`: `null` when it is not labelable.
pub(crate) fn labels_of<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, n: NodeId) -> Res<'s, Local<'s>> {
    if !labelable(d, &d.doc.borrow(), n) {
        return Ok(s.null());
    }
    let cached = d.ctl.st.borrow().labels.get(&n).copied();
    if let Some(o) = cached {
        return Ok(d.obj_local(s, o));
    }
    let v = old_v::<E>(s, |it| crate::browser_nodelist::make_list(d, it, crate::browser_nodelist::list_proto(d), Src::Labels(n)))?;
    let Some(o) = obj_id::<E>(s, v) else { unreachable!() };
    d.ctl.st.borrow_mut().labels.insert(n, o);
    Ok(v)
}

fn shared<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, [E::Obj; 7]> {
    if let Some(&s) = d.ctl.shared.get() {
        return Ok(s);
    }
    let hs: [(HS<E>, &str, u32); 7] = [
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
        v.push(d.add_fn_s_obj(it, h, name, len, HostFn::Method)?);
    }
    let out: [E::Obj; 7] = v.try_into().expect("seven");
    hold::<E>(it, &d.ctl.keep, &out);
    let _ = d.ctl.shared.set(out);
    Ok(out)
}

/// Define the validation members and `labels` on the prototype `proto` (`check`: also `checkValidity` and `reportValidity`, which the
/// `input` and `button` prototypes have from the start).
pub(crate) fn decorate<E: Eng>(d: &Dom<E>, it: &mut E::Vm, proto: E::Obj, check: bool) -> R<E, ()> {
    let f = shared(d, it)?;
    for (name, g) in [("willValidate", f[0]), ("validationMessage", f[1]), ("validity", f[2]), ("labels", f[4])] {
        let k = d.key(it, name)?;
        d.define_accessor(it, proto, k, g, None)?;
    }
    let k = d.key(it, "setCustomValidity")?;
    it.define_data(proto, k, E::obj_raw(f[3]), true, false, true)?;
    if check {
        for (name, i) in [("checkValidity", 5), ("reportValidity", 6)] {
            let k = d.key(it, name)?;
            it.define_data(proto, k, E::obj_raw(f[i]), true, false, true)?;
        }
    }
    Ok(())
}

reflect_s!("input", "required", input_required, set_input_required, bool);

/// `form.elements`: one live `HTMLCollection` of the form (`browser_table::FORM`).
fn form_elements<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let f = d.html_el_s(s, this, "form")?;
    old_v::<E>(s, |it| crate::browser_nodelist::of(d, it, f, crate::browser_table::FORM))
}

/// The prototype of a new wrapper of an HTML `input`, `button`, `form` or `label`. The first `input`, the first `button` and the first `form`
/// give their prototype the members of this file.
pub(crate) fn proto_for<E: Eng>(d: &Dom<E>, it: &mut E::Vm, tag: &str) -> R<E, E::Obj> {
    if tag == "label" {
        return Ok(made_label(d, it)?[0]);
    }
    let p = d.p();
    if tag == "form" {
        let done = d.ctl.done.get();
        if done & 4 == 0 {
            d.def_method_s(it, p.form, "reset", 0, form_reset)?;
            crate::browser_reflect::reflect!(d, it, p.form, &["form"], {
                "action" => "action": UrlOrDoc,
                "method" => "method": Enum(&["get", "post", "dialog"], "get"),
                "enctype" => "enctype": Enum(&["application/x-www-form-urlencoded", "multipart/form-data", "text/plain"], "application/x-www-form-urlencoded"),
                "target" => "target": Str,
                "name" => "name": Str,
                "acceptCharset" => "accept-charset": Str,
                "autocomplete" => "autocomplete": Enum(&["on", "off"], "on"),
            });
            d.def_get_s(it, p.form, "elements", form_elements, None)?;
            d.def_get_s(it, p.form, "length", |d, s, t, _| {
                let f = d.html_el_s(s, t, "form")?;
                let n = old::<E, _>(s, |it| d.form_elements(it, f))?.len();
                Ok(s.number(n as f64))
            }, None)?;
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
                crate::browser_reflect::reflect!(d, it, proto, &["input"], {
                    "placeholder" => "placeholder": Str, "min" => "min": Str, "max" => "max": Str, "step" => "step": Str,
                    "pattern" => "pattern": Str, "autocomplete" => "autocomplete": Str, "multiple" => "multiple": Bool,
                    "maxLength" => "maxlength": Long(-1, 0), "minLength" => "minlength": Long(-1, 0), "size" => "size": Long(20, 1),
                });
                let k = d.key(it, "required")?;
                let g = d.add_fn_s_obj(it, input_required, "get required", 0, HostFn::Method)?;
                let s = d.add_fn_s_obj(it, set_input_required, "set required", 1, HostFn::Method)?;
                hold::<E>(it, &d.ctl.keep, &[g, s]);
                let _ = d.ctl.required.set([g, s]);
                d.define_accessor(it, proto, k, g, Some(s))?;
            }
            _ => {
                decorate(d, it, proto, false)?;
                crate::browser_reflect::reflect!(d, it, proto, &["button"], { "value" => "value": Str });
            }
        }
        d.ctl.done.set(done | bit);
    }
    Ok(proto)
}

// ------------------------------------------------------------------------------------------ label

/// Is `n` a labelable element?
fn labelable<E: Eng>(d: &Dom<E>, doc: &Document, n: NodeId) -> bool {
    ["button", "select", "textarea", "meter", "output", "progress"].iter().any(|t| doc.is_html(n, t)) || (doc.is_html(n, "input") && input_kind(doc, n) != "hidden") || d.is_face(n)
}

fn root_of<E: Eng>(doc: &Document, it: &mut E::Vm, n: NodeId) -> R<E, NodeId> {
    let mut top = n;
    while let Some(p) = doc.parent(top) {
        it.charge_o(1)?;
        top = p;
    }
    Ok(top)
}

impl<E: Eng> Dom<E> {
    /// The control of a label: the element that its `for` names (the first with that ID in the tree of the label), else its first labelable
    /// descendant. `None` when it is not labelable.
    pub(crate) fn label_control(&self, it: &mut E::Vm, label: NodeId) -> R<E, Option<NodeId>> {
        let doc = self.doc.borrow();
        if let Some(id) = get(&doc, label, "for") {
            let root = root_of::<E>(&doc, it, label)?;
            let (mut cur, mut depth) = (root, 0);
            while !id.is_empty() && let Some((c, d)) = crate::browser_dom::next_in(&doc, root, cur, depth) {
                it.charge_o(1)?;
                (cur, depth) = (c, d);
                if doc.attr(c, Atom::ID) == Some(id) {
                    return Ok(labelable(self, &doc, c).then_some(c));
                }
            }
            return Ok(None);
        }
        let (mut cur, mut depth) = (label, 0);
        while let Some((c, d)) = crate::browser_dom::next_in(&doc, label, cur, depth) {
            it.charge_o(1)?;
            (cur, depth) = (c, d);
            if labelable(self, &doc, c) {
                return Ok(Some(c));
            }
        }
        Ok(None)
    }
}

/// The labels of `control` in tree order, for `Src::Labels`.
pub(crate) fn label_ids<E: Eng>(d: &Dom<E>, it: &mut E::Vm, control: NodeId) -> R<E, Vec<NodeId>> {
    let (root, all) = {
        let doc = d.doc.borrow();
        let root = root_of::<E>(&doc, it, control)?;
        let (mut cur, mut depth, mut all) = (root, 0, Vec::new());
        while let Some((c, dp)) = crate::browser_dom::next_in(&doc, root, cur, depth) {
            it.charge_o(1)?;
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

fn label_el<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, NodeId> {
    d.html_el_s(s, this, "label")
}

fn label_control<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let l = label_el(d, s, this)?;
    let c = old::<E, _>(s, |it| d.label_control(it, l))?;
    d.wrap_opt_s(s, c)
}

fn label_form<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let l = label_el(d, s, this)?;
    let f = old::<E, _>(s, |it| match d.label_control(it, l)? {
        Some(c) => d.form_owner(it, c),
        None => Ok(None),
    })?;
    d.wrap_opt_s(s, f)
}

reflect_s!("label", "for", html_for, set_html_for, str);

fn made_label<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, [E::Obj; 2]> {
    if let Some(&m) = d.ctl.label.get() {
        return Ok(m);
    }
    let proto = d.obj(it, d.p().html)?;
    d.def_get_s(it, proto, "control", label_control, None)?;
    d.def_get_s(it, proto, "htmlFor", html_for, Some(set_html_for))?;
    d.def_get_s(it, proto, "form", label_form, None)?;
    let ctor = interface(d, it, proto, "HTMLLabelElement")?;
    hold::<E>(it, &d.ctl.keep, &[proto, ctor]);
    let _ = d.ctl.label.set([proto, ctor]);
    Ok([proto, ctor])
}

// ------------------------------------------------------------------------------------------ reset

impl<E: Eng> Dom<E> {
    /// The reset algorithm of a control (HTML "reset algorithm" of input, select and textarea).
    fn reset_control(&self, it: &mut E::Vm, n: NodeId) -> R<E, ()> {
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
                (i.checked, i.typed, i.edited) = (None, false, None);
                // Chrome: a dirty value that goes back to the default puts the caret at the end; a control that was not dirty keeps its selection.
                i.value.take().map_or(0, |v| {
                    i.sel = Some(crate::browser_selection::END);
                    v.len()
                })
            });
            st.input_bytes -= old;
        }
        if kind == "radio" && self.input_checked(n) {
            for o in self.radio_group(it, n)? {
                self.st.borrow_mut().inputs.entry(o).or_default().checked = Some(false);
                self.sync_ui(o);
            }
        }
        self.sync_ui(n);
        Ok(())
    }

    /// `form.reset()` and a reset button: the `reset` event, then the reset of each control. Nothing when the form is already resetting.
    pub(crate) fn reset_form(&self, it: &mut E::Vm, form: NodeId) -> R<E, ()> {
        if !self.ctl.st.borrow_mut().locked.insert(form) {
            return Ok(());
        }
        let r = (|| {
            let e = self.new_event(it, "reset", true, true, false, None)?;
            self.st.borrow_mut().events.get_mut(&e).expect("just made").trusted = true;
            if self.dispatch(it, Target::Node(form), e)? {
                for n in self.owned_controls(it, form)? {
                    if self.is_face(n) {
                        self.ce_form_reset(n);
                    } else {
                        self.reset_control(it, n)?;
                    }
                }
            }
            Ok(())
        })();
        self.ctl.st.borrow_mut().locked.remove(&form);
        r
    }
}

pub(crate) fn form_reset<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let f = d.html_el_s(s, this, "form")?;
    old::<E, _>(s, |it| d.reset_form(it, f))?;
    Ok(s.undefined())
}

// ------------------------------------------------------------------------------------------ install

fn ens_validity<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    made_validity(d, it).map(|_| ())
}

fn ens_label<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    made_label(d, it).map(|_| ())
}

lazy_global!(lazy_validity, "ValidityState", ens_validity, |d| d.ctl.validity.get().expect("made")[1]);
lazy_global!(lazy_label, "HTMLLabelElement", ens_label, |d| d.ctl.label.get().expect("made")[1]);

pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    table!(lazy = [
        Row::both(1, "ValidityState", lazy_validity),
        Row::both(1, "HTMLLabelElement", lazy_label),
    ]);
    let g = s.global();
    d.install_rows(s, &[g], &lazy::T::<E>::V)
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // Moving the two functions below would make a large diff.
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
    fn date_and_time_values_are_valid_or_empty() {
        for (kind, v, want) in [
            ("date", "2026-11-18", "2026-11-18"),
            ("date", "2024-02-29", "2024-02-29"),
            ("date", "2026-02-29", ""),
            ("date", "2026-11-31", ""),
            ("date", "26-11-18", ""),
            ("date", "0000-01-01", ""),
            ("date", "11/18/2026", ""),
            ("month", "2026-11", "2026-11"),
            ("month", "2026-13", ""),
            ("week", "2026-W53", "2026-W53"),
            ("week", "2025-W53", ""),
            ("week", "2026-W00", ""),
            ("time", "10:30", "10:30"),
            ("time", "10:30:00.500", "10:30:00.500"),
            ("time", "24:00", ""),
            ("time", "10:30:00.1234", ""),
            ("datetime-local", "2026-11-18T10:30", "2026-11-18T10:30"),
            ("datetime-local", "2026-11-18 10:30:00", "2026-11-18T10:30"),
            ("datetime-local", "2026-11-18T10:30:05.500", "2026-11-18T10:30:05.5"),
            ("datetime-local", "2026-11-18", ""),
            ("datetime-local", "111820261030", ""),
        ] {
            assert_eq!(sanitize(kind, false, v), want, "{kind} {v:?}");
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

#[cfg(test)]
impl<E: Eng> Dom<E> {
    /// The entries of the `ValidityState` maps (element to object, object to element) and of `labels`. (Tests.)
    pub(crate) fn ctl_entries(&self) -> (usize, usize, usize) {
        let st = self.ctl.st.borrow();
        (st.state.len(), st.owner.len(), st.labels.len())
    }
}

// ------------------------------------------------------------------------------------------ arena reclaim (`browser_reclaim.rs`)

/// The nodes that live `ValidityState` and `labels` objects name.
pub(crate) fn held<E: Eng>(d: &Dom<E>, out: &mut Vec<NodeId>) {
    let st = d.ctl.st.borrow();
    out.extend(st.state.keys().copied());
    out.extend(st.owner.values().copied());
    out.extend(st.labels.keys().copied());
    out.extend(st.locked.iter().copied());
}

/// Drop the state of the nodes that the reclaim freed.
pub(crate) fn forget<E: Eng>(d: &Dom<E>, gone: &dyn Fn(NodeId) -> bool) {
    d.ctl.st.borrow_mut().custom.retain(|&n, _| !gone(n));
}
