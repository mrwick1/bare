//! Form submission and constraint validation: the part of the HTML "form submission algorithm" that follows the `submit` event,
//! `requestSubmit`, `submit()`, `checkValidity` and `reportValidity`, and the `maxlength` limit of typing.
//!
//! How it works (HTML Standard, "Form submission algorithm", "Constraint validation", "Constructing the entry list",
//! "application/x-www-form-urlencoded")
//! - A submit button, Enter in a field (implicit submission) and `requestSubmit()` validate the form unless `novalidate` (on the
//!   form) or `formnovalidate` (on the submitter) is set. An invalid form fires `invalid` (cancelable, no bubbling) at each
//!   invalid control in tree order, focuses the first one, and stops: no `submit` event, no navigation. `form.submit()` neither
//!   validates nor fires `submit`.
//! - When no listener cancels `submit`, the form navigates. The entry list is built from the controls that the form owns, in tree
//!   order: not disabled, with a name; a checkbox or radio only when checked (value `on` if it has none); only the submitter among
//!   buttons; an image submitter adds `name.x` and `name.y` (0 and 0). `GET` replaces the query of the action URL (the fragment is
//!   dropped) and loads it. `POST` sends `application/x-www-form-urlencoded`. The action is the submitter's `formaction`, else the
//!   form's `action`, else the document URL. The method is `formmethod`, else `method` (`post`, else `get`; `dialog` does nothing).
//!   The navigation is a `NavigationRequest` for the session, like `location.assign`.
//! - Checks: `required` (text, checkbox, radio), `type=email` and `type=url`, `type=number` (a text that is no number, `min`,
//!   `max`), `minlength` (only for a value that a user typed, as the standard says) and `maxlength`.
//!   The `maxlength` limit of typing cuts the typed text to fit (UTF-16 units, no split pair) and sends no event when nothing fits.
//!
//! Gates and differences (recorded, none is faked)
//! - `pattern`, `step`, `type=date` and the other date kinds, `setCustomValidity`, `validity`, `validationMessage`, `<select>` and
//!   `<textarea>` as controls to validate: not built. `<textarea>` and `<select>` are sent with the value that their markup gives.
//! - No message bubble is drawn for an invalid field. The window focuses the first invalid field and scrolls to it.
//! - `enctype="multipart/form-data"` and `text/plain` with `POST` are `Unsupported`; `GET` ignores `enctype`. `target` is ignored (the
//!   document is replaced). `<input type=file>` sends nothing. `form.elements`, `form.action` and `form.method` are absent. `new FormData(form)` (`browser_web.rs`) uses the same entry list, and keeps a file input as an empty File.
//! - A form with a `javascript:` or other non-http(s) action does nothing.

use crate::browser::*;
use crate::browser_dom as nd;
use bare_html::dom::{Document, NodeData, NodeId};
use bare_js::eval::{Error, Interp};
use bare_js::value::Value;

type V = R<Value>;

fn attr<'a>(doc: &'a Document, n: NodeId, name: &str) -> Option<&'a str> {
    doc.find_atom(name).and_then(|a| doc.attr(n, a))
}

fn tag_is(doc: &Document, n: NodeId, names: &[&str]) -> bool {
    matches!(*doc.data(n), NodeData::Element { name, .. } if names.contains(&doc.atom_str(name))) && !doc.is_foreign(n)
}

const TEXT_KINDS: [&str; 7] = ["text", "search", "tel", "url", "email", "password", "number"];

/// The text of `s` for a form: a lone CR or LF becomes CRLF, then percent-encoded the `application/x-www-form-urlencoded` way.
fn urlencode(s: &str, out: &mut String) {
    let mut prev_cr = false;
    let push = |b: u8, out: &mut String| match b {
        b' ' => out.push('+'),
        b'*' | b'-' | b'.' | b'_' | b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' => out.push(b as char),
        _ => out.push_str(&format!("%{b:02X}")),
    };
    for c in s.chars() {
        match c {
            '\r' => {
                push(b'\r', out);
                push(b'\n', out);
                prev_cr = true;
                continue;
            }
            '\n' if prev_cr => {}
            '\n' => {
                push(b'\r', out);
                push(b'\n', out);
            }
            c => {
                let mut b = [0; 4];
                for &x in c.encode_utf8(&mut b).as_bytes() {
                    push(x, out);
                }
            }
        }
        prev_cr = false;
    }
}

/// The body of an entry list as `name=value&name=value`.
pub(crate) fn encode_entries(entries: &[(String, String)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in entries.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        urlencode(k, &mut out);
        out.push('=');
        urlencode(v, &mut out);
    }
    out
}

/// HTML "valid e-mail address": the `a@b` shape that the standard gives as a regular expression.
fn valid_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else { return false };
    let atext = |c: char| c.is_ascii_alphanumeric() || ".!#$%&'*+/=?^_`{|}~-".contains(c);
    if local.is_empty() || !local.chars().all(atext) {
        return false;
    }
    !domain.is_empty()
        && domain.split('.').all(|l| {
            !l.is_empty() && l.len() <= 63 && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && !l.starts_with('-') && !l.ends_with('-')
        })
}

/// A valid absolute URL for `type=url`: a scheme, then something.
fn valid_url(s: &str) -> bool {
    let Some((scheme, rest)) = s.split_once(':') else { return false };
    let mut c = scheme.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic()) && c.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) && !rest.is_empty()
}

/// The text with line breaks removed (the value sanitization of a single line control).
fn single_line(s: &str) -> String {
    s.chars().filter(|&c| c != '\n' && c != '\r').collect()
}

fn units(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

/// The longest start of `s` that is at most `room` UTF-16 units and does not split a pair.
pub(crate) fn fit_units(s: &str, room: u32) -> String {
    let (mut n, mut out) = (0, String::new());
    for c in s.chars() {
        n += c.len_utf16() as u32;
        if n > room {
            break;
        }
        out.push(c);
    }
    out
}

/// The nodes below `n` in tree order.
fn below(doc: &Document, n: NodeId) -> Vec<NodeId> {
    let (mut cur, mut depth, mut out) = (n, 0, Vec::new());
    while let Some((c, d)) = nd::next_in(doc, n, cur, depth) {
        (cur, depth) = (c, d);
        out.push(c);
    }
    out
}

/// The text of the text nodes below `n`.
fn text_below(doc: &Document, n: NodeId) -> String {
    below(doc, n).into_iter().filter_map(|c| if let NodeData::Text(sp) = *doc.data(c) { Some(doc.text(sp).to_string()) } else { None }).collect()
}

impl Dom {
    /// The `maxlength` of a text field, if it has a valid one.
    pub(crate) fn max_length(&self, n: NodeId) -> Option<u32> {
        let doc = self.doc.borrow();
        if !tag_is(&doc, n, &["input"]) || !TEXT_KINDS.contains(&nd::input_kind(&doc, n)) {
            return None;
        }
        attr(&doc, n, "maxlength").and_then(|v| v.trim().parse::<u32>().ok())
    }

    /// The listed controls that `form` owns, in tree order. The walk starts at the top of the tree of `form`: it is the document, or the top of a detached tree.
    fn owned_controls(&self, it: &mut Interp<'_>, form: NodeId) -> R<Vec<NodeId>> {
        let mut all = Vec::new();
        {
            let doc = self.doc.borrow();
            let mut root = form;
            while let Some(p) = doc.parent(root) {
                it.charge(1)?;
                root = p;
            }
            let (mut cur, mut depth) = (root, 0);
            while let Some((c, d)) = nd::next_in(&doc, root, cur, depth) {
                it.charge(1)?;
                (cur, depth) = (c, d);
                if tag_is(&doc, c, &["button", "input", "select", "textarea"]) {
                    all.push(c);
                }
            }
        }
        let mut out = Vec::new();
        for n in all {
            if self.form_owner(it, n)? == Some(form) {
                out.push(n);
            }
        }
        Ok(out)
    }

    /// The entry list of `form` for `submitter` (HTML "constructing the entry list"). A file input has one entry with no value: an empty file.
    pub(crate) fn entry_list(&self, it: &mut Interp<'_>, form: NodeId, submitter: Option<NodeId>) -> R<Vec<(String, Option<String>)>> {
        let mut out = Vec::new();
        for n in self.owned_controls(it, form)? {
            if self.is_disabled_control(it, n)? {
                continue;
            }
            let (name, tag, kind) = {
                let doc = self.doc.borrow();
                let name = attr(&doc, n, "name").unwrap_or("").to_string();
                let tag = ["button", "input", "select", "textarea"].into_iter().find(|t| tag_is(&doc, n, &[t])).unwrap_or("input");
                (name, tag, if tag == "input" { nd::input_kind(&doc, n) } else { "" })
            };
            if name.is_empty() {
                continue;
            }
            let attr_value = || attr(&self.doc.borrow(), n, "value").unwrap_or("").to_string();
            match (tag, kind) {
                ("button", _) => {
                    if Some(n) == submitter {
                        out.push((name, Some(attr_value())));
                    }
                }
                ("input", "submit") => {
                    if Some(n) == submitter {
                        out.push((name, Some(attr_value())));
                    }
                }
                ("input", "image") => {
                    if Some(n) == submitter {
                        let p = if name.is_empty() { String::new() } else { format!("{name}.") };
                        out.push((format!("{p}x"), Some("0".into())));
                        out.push((format!("{p}y"), Some("0".into())));
                    }
                }
                ("input", "file") => out.push((name, None)),
                ("input", "reset" | "button") => {}
                ("input", "checkbox" | "radio") => {
                    if self.input_checked(n) {
                        out.push((name, Some(self.input_value_of(n).unwrap_or_else(|| "on".into()))));
                    }
                }
                ("input", _) => out.push((name, Some(self.input_value_of(n).map(|v| single_line(&v)).unwrap_or_default()))),
                ("textarea", _) => out.push((name, Some(self.text_of(n).replace("\r\n", "\n").replace('\r', "\n")))),
                _ => {
                    // A `<select>`: the options that the markup marks as selected (a single select keeps the last one), else the first enabled option of a
                    // single select. A disabled option is not sent.
                    let opts: Vec<NodeId> = {
                        let doc = self.doc.borrow();
                        below(&doc, n).into_iter().filter(|&o| tag_is(&doc, o, &["option"])).collect()
                    };
                    let doc = self.doc.borrow();
                    let enabled = |o: &NodeId| attr(&doc, *o, "disabled").is_none();
                    let mut picked: Vec<NodeId> = opts.iter().copied().filter(|&o| attr(&doc, o, "selected").is_some()).collect();
                    if attr(&doc, n, "multiple").is_none() {
                        picked = match picked.last() {
                            Some(&o) => vec![o],
                            None => opts.iter().copied().filter(enabled).take(1).collect(),
                        };
                    }
                    for o in picked.into_iter().filter(enabled) {
                        let v = attr(&doc, o, "value").map(str::to_string).unwrap_or_else(|| text_below(&doc, o).split_whitespace().collect::<Vec<_>>().join(" "));
                        out.push((name.clone(), Some(v)));
                    }
                }
            }
        }
        Ok(out)
    }

    fn text_of(&self, n: NodeId) -> String {
        text_below(&self.doc.borrow(), n)
    }

    /// Does the constraint of `n` fail? (See the header for what is checked.)
    fn is_invalid(&self, it: &mut Interp<'_>, n: NodeId) -> R<bool> {
        if self.is_disabled_control(it, n)? {
            return Ok(false);
        }
        let (kind, required, readonly) = {
            let doc = self.doc.borrow();
            if !tag_is(&doc, n, &["input"]) {
                return Ok(false);
            }
            (nd::input_kind(&doc, n), attr(&doc, n, "required").is_some(), attr(&doc, n, "readonly").is_some())
        };
        match kind {
            "checkbox" => return Ok(required && !self.input_checked(n)),
            "radio" => {
                if !required {
                    return Ok(false);
                }
                let mut any = self.input_checked(n);
                for o in self.radio_group(it, n)? {
                    any |= self.input_checked(o);
                }
                return Ok(!any);
            }
            k if !TEXT_KINDS.contains(&k) || readonly => return Ok(false),
            _ => {}
        }
        let value = single_line(&self.input_value_of(n).unwrap_or_default());
        let value = if matches!(kind, "email" | "url" | "number") { value.trim().to_string() } else { value };
        if value.is_empty() {
            return Ok(required);
        }
        let doc = self.doc.borrow();
        let num = |name: &str| attr(&doc, n, name).and_then(|v| v.trim().parse::<f64>().ok());
        let bad = match kind {
            "email" => {
                if attr(&doc, n, "multiple").is_some() {
                    !value.split(',').all(|p| valid_email(p.trim()))
                } else {
                    !valid_email(&value)
                }
            }
            "url" => !valid_url(&value),
            "number" => match value.parse::<f64>() {
                Ok(v) if v.is_finite() && !value.starts_with('+') && !value.ends_with('.') => num("min").is_some_and(|m| v < m) || num("max").is_some_and(|m| v > m),
                _ => true,
            },
            _ => false,
        };
        if bad {
            return Ok(true);
        }
        // `minlength` and `maxlength` count only what a user typed.
        let typed = self.st.borrow().inputs.get(&n).is_some_and(|i| i.typed);
        let len = units(&value);
        let limit = |name: &str| attr(&doc, n, name).and_then(|v| v.trim().parse::<u32>().ok());
        Ok(typed && (limit("minlength").is_some_and(|m| len < m) || limit("maxlength").is_some_and(|m| len > m)))
    }

    /// The invalid controls of `form`, in tree order.
    fn invalid_controls(&self, it: &mut Interp<'_>, form: NodeId) -> R<Vec<NodeId>> {
        let mut out = Vec::new();
        for n in self.owned_controls(it, form)? {
            if self.is_invalid(it, n)? {
                out.push(n);
            }
        }
        Ok(out)
    }

    /// Fire `invalid` at each of `bad`; when none was canceled, focus the first. Gives whether the list is empty.
    fn report_invalid(&self, it: &mut Interp<'_>, bad: &[NodeId], focus: bool) -> R<bool> {
        let mut canceled = false;
        for &n in bad {
            let e = self.new_event(it, "invalid", false, true, false, None)?;
            let trusted = self.st.borrow().ui.active;
            self.st.borrow_mut().events.get_mut(&e).expect("just made").trusted = trusted;
            canceled |= !self.dispatch(it, Target::Node(n), e)?;
        }
        if focus && !canceled && let Some(&first) = bad.first() {
            self.focus_node(it, first)?;
        }
        Ok(bad.is_empty())
    }

    /// HTML "submit" a form: validate (unless `skip`), fire `submit` (unless `event` is false), then navigate.
    pub(crate) fn submit_with(&self, it: &mut Interp<'_>, form: NodeId, submitter: Option<NodeId>, validate: bool, event: bool) -> R<()> {
        if !self.is_connected_node(it, form)? {
            return Ok(()); // cannot navigate
        }
        if validate {
            let skip = {
                let doc = self.doc.borrow();
                submitter.is_some_and(|s| attr(&doc, s, "formnovalidate").is_some()) || attr(&doc, form, "novalidate").is_some()
            };
            if !skip {
                let bad = self.invalid_controls(it, form)?;
                if !self.report_invalid(it, &bad, true)? {
                    return Ok(());
                }
            }
        }
        if event {
            let who = match submitter {
                Some(s) => self.wrap(it, s)?,
                None => Value::Null,
            };
            let e = self.new_event_proto(it, "submit", true, true, false, self.p().submit)?;
            {
                let mut st = self.st.borrow_mut();
                let trusted = st.ui.active;
                let ev = st.events.get_mut(&e).expect("just made");
                ev.related = who;
                ev.is_submit = true;
                ev.trusted = trusted;
            }
            if !self.dispatch(it, Target::Node(form), e)? || !self.is_connected_node(it, form)? {
                return Ok(());
            }
        }
        self.navigate_form(it, form, submitter)
    }

    fn navigate_form(&self, it: &mut Interp<'_>, form: NodeId, submitter: Option<NodeId>) -> R<()> {
        let (action, method, enc) = {
            let doc = self.doc.borrow();
            let pick = |sub: &str, own: &str| submitter.and_then(|s| attr(&doc, s, sub)).or_else(|| attr(&doc, form, own)).map(str::to_string);
            (
                pick("formaction", "action").unwrap_or_default(),
                pick("formmethod", "method").unwrap_or_default().to_ascii_lowercase(),
                pick("formenctype", "enctype").unwrap_or_default().to_ascii_lowercase(),
            )
        };
        if method == "dialog" {
            return Ok(());
        }
        let post = method == "post";
        if post && !(enc.is_empty() || enc == "application/x-www-form-urlencoded") {
            return Err(Error::Unsupported(unsupported("form POST with an enctype other than application/x-www-form-urlencoded")));
        }
        let entries: Vec<(String, String)> = self.entry_list(it, form, submitter)?.into_iter().filter_map(|(n, v)| Some((n, v?))).collect();
        it.charge(entries.len() as u64 + 1)?;
        self.submit_navigation(it, action.trim(), post, &encode_entries(&entries))
    }

    /// Is `n` a submit button (the kind that `requestSubmit` accepts)?
    fn is_submit_button(&self, n: NodeId) -> bool {
        let doc = self.doc.borrow();
        self.submit_button(&doc, n)
    }
}

// ---------------------------------------------------------------------------------------------- JS API

fn form_this(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
    let n = d.el(it, this)?;
    if tag_is(&d.doc.borrow(), n, &["form"]) { Ok(n) } else { Err(it.type_error("Illegal invocation")) }
}

pub(crate) fn request_submit(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let form = form_this(d, it, this)?;
    let submitter = match a.first().copied() {
        None | Some(Value::Undefined) | Some(Value::Null) => None,
        Some(v) => {
            let Some(s) = d.node_of(v).filter(|&s| d.is_element(s)) else {
                return Err(it.type_error("Failed to execute 'requestSubmit' on 'HTMLFormElement': parameter 1 is not of type 'HTMLElement'."));
            };
            if !d.is_submit_button(s) {
                return Err(it.type_error("Failed to execute 'requestSubmit' on 'HTMLFormElement': The specified element is not a submit button."));
            }
            if d.form_owner(it, s)? != Some(form) {
                return Err(d.dom_err(it, "NotFoundError", "Failed to execute 'requestSubmit' on 'HTMLFormElement': The specified element is not owned by this form element."));
            }
            Some(s)
        }
    };
    d.submit_with(it, form, submitter, true, true)?;
    Ok(Value::Undefined)
}

pub(crate) fn submit_method(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let form = form_this(d, it, this)?;
    d.submit_with(it, form, None, false, false)?;
    Ok(Value::Undefined)
}

pub(crate) fn form_check_validity(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let form = form_this(d, it, this)?;
    let bad = d.invalid_controls(it, form)?;
    Ok(Value::Bool(d.report_invalid(it, &bad, false)?))
}

pub(crate) fn form_report_validity(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let form = form_this(d, it, this)?;
    let bad = d.invalid_controls(it, form)?;
    Ok(Value::Bool(d.report_invalid(it, &bad, true)?))
}

fn control_validity(d: &Dom, it: &mut Interp<'_>, this: Value, focus: bool) -> V {
    let n = d.el(it, this)?;
    let bad = if d.is_invalid(it, n)? { vec![n] } else { vec![] };
    Ok(Value::Bool(d.report_invalid(it, &bad, focus)?))
}

pub(crate) fn control_check_validity(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    control_validity(d, it, this, false)
}

pub(crate) fn control_report_validity(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    control_validity(d, it, this, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencoding_follows_the_form_serializer() {
        let mut s = String::new();
        urlencode("a b*-._é\n&=", &mut s);
        assert_eq!(s, "a+b*-._%C3%A9%0D%0A%26%3D");
        let mut s = String::new();
        urlencode("x\r\ny\rz", &mut s);
        assert_eq!(s, "x%0D%0Ay%0D%0Az");
        assert_eq!(encode_entries(&[("q".into(), "a b".into()), ("n".into(), "".into())]), "q=a+b&n=");
    }

    #[test]
    fn email_and_url_shapes() {
        assert!(valid_email("a.b+c@example.com") && valid_email("a@b"));
        assert!(!valid_email("a@") && !valid_email("@b") && !valid_email("a b@c.d") && !valid_email("a@-b.c") && !valid_email("a@b..c"));
        assert!(valid_url("https://a.test") && valid_url("mailto:a@b"));
        assert!(!valid_url("a.test") && !valid_url("1http://x") && !valid_url("http:"));
    }

    #[test]
    fn maxlength_cuts_by_utf16_units_without_a_split_pair() {
        assert_eq!(fit_units("abcd", 2), "ab");
        assert_eq!(fit_units("a😀b", 2), "a");
        assert_eq!(fit_units("a😀b", 3), "a😀");
        assert_eq!(fit_units("abc", 0), "");
    }
}
