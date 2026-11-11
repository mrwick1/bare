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
//! - The checks are the constraint validation of `browser_controls.rs` (the constraints, `validity` and the messages are there). This
//!   file asks it which controls are invalid, fires `invalid` (trusted, since Chrome does that for a script call too) and focuses the
//!   first. `owned_controls` walks the tree that holds the form, so a detached form validates and resets too. An `input`, `select`
//!   and `textarea` are all checked; a disabled one, a `readonly` one and a `datalist` descendant are not candidates.
//! - The entry list takes the API value of a textarea (CRLF and lone CR become LF; the encoder turns LF into CRLF) and the selectedness
//!   of a select (its selected options that are not disabled), not the markup.
//!   The `maxlength` limit of typing (`input` and `textarea`) cuts the typed text to fit (UTF-16 units, no split pair) and sends no event
//!   when nothing fits.
//!
//! Gates and differences (recorded, none is faked)
//! - No message bubble is drawn for an invalid field. The window focuses the first invalid field and scrolls to it.
//! - `enctype="text/plain"` with `POST` is `Unsupported`; `multipart/form-data` with `POST` sends the files of a file input (a driver sets them: `Page::upload`; script cannot). `GET` ignores `enctype` and sends the file name. `target` is ignored (the
//!   document is replaced). `form.elements`, `form.action` and `form.method` are absent. `new FormData(form)` (`browser_web.rs`) uses the same entry list, and keeps a file input as an empty File.
//! - A form with a `javascript:` or other non-http(s) action does nothing.

use crate::browser::*;
use crate::browser_dom as nd;
use bare_html::dom::{Document, NodeData, NodeId};
use bare_js::eval::{Error, Interp};
use crate::browser_web::Ent;
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

/// The text with line breaks removed (the value sanitization of a single line control).
fn single_line(s: &str) -> String {
    s.chars().filter(|&c| c != '\n' && c != '\r').collect()
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

impl Dom {
    /// The `maxlength` of a text field, if it has a valid one.
    pub(crate) fn max_length(&self, n: NodeId) -> Option<u32> {
        let doc = self.doc.borrow();
        if !(tag_is(&doc, n, &["textarea"]) || (tag_is(&doc, n, &["input"]) && TEXT_KINDS.contains(&nd::input_kind(&doc, n)))) {
            return None;
        }
        attr(&doc, n, "maxlength").and_then(|v| v.trim().parse::<u32>().ok())
    }

    /// The listed controls that `form` owns, in tree order. The walk starts at the top of the tree of `form`: it is the document, or the top of a detached tree.
    pub(crate) fn owned_controls(&self, it: &mut Interp<'_>, form: NodeId) -> R<Vec<NodeId>> {
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
                if tag_is(&doc, c, &["button", "input", "select", "textarea"]) || self.is_face(c) {
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
    pub(crate) fn entry_list(&self, it: &mut Interp<'_>, form: NodeId, submitter: Option<NodeId>) -> R<Vec<(String, Option<Ent>)>> {
        let mut out = Vec::new();
        for n in self.owned_controls(it, form)? {
            if self.is_disabled_control(it, n)? {
                continue;
            }
            let (name, tag, kind) = {
                let doc = self.doc.borrow();
                let name = attr(&doc, n, "name").unwrap_or("").to_string();
                drop(doc);
                if let Some(e) = self.face_entries(n, &name) {
                    out.extend(e);
                    continue;
                }
                let doc = self.doc.borrow();
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
                        out.push((name, Some(Ent::S(attr_value()))));
                    }
                }
                ("input", "submit") => {
                    if Some(n) == submitter {
                        out.push((name, Some(Ent::S(attr_value()))));
                    }
                }
                ("input", "image") => {
                    if Some(n) == submitter {
                        let p = if name.is_empty() { String::new() } else { format!("{name}.") };
                        out.push((format!("{p}x"), Some(Ent::S("0".into()))));
                        out.push((format!("{p}y"), Some(Ent::S("0".into()))));
                    }
                }
                ("input", "file") => {
                    let files = self.st.borrow().inputs.get(&n).map(|i| i.files.clone()).unwrap_or_default();
                    if files.is_empty() {
                        out.push((name.clone(), None));
                    }
                    for f in &files {
                        let Value::Object(o) = crate::browser_web::file_from_ref(self, it, f)? else { unreachable!() };
                        out.push((name.clone(), Some(Ent::B(o))));
                    }
                }
                ("input", "reset" | "button") => {}
                ("input", "checkbox" | "radio") => {
                    if self.input_checked(n) {
                        out.push((name, Some(Ent::S(self.input_value_of(n).unwrap_or_else(|| "on".into())))));
                    }
                }
                ("input", _) => out.push((name, Some(Ent::S(self.input_value_of(n).map(|v| single_line(&v)).unwrap_or_default())))),
                ("textarea", _) => out.push((name, Some(Ent::S(self.input_value_of(n).unwrap_or_default())))),
                _ => {
                    // A `<select>`: its selected options that are not disabled.
                    for o in crate::browser_select::list_ids(self, it, n, true)? {
                        let doc = self.doc.borrow();
                        if !crate::browser_select::is_disabled(&doc, o) {
                            out.push((name.clone(), Some(Ent::S(crate::browser_select::value_of(&doc, o)))));
                        }
                    }
                }
            }
        }
        Ok(out)
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
            self.st.borrow_mut().events.get_mut(&e).expect("just made").trusted = true;
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
        let multipart = post && enc == "multipart/form-data";
        if post && !(multipart || enc.is_empty() || enc == "application/x-www-form-urlencoded") {
            return Err(Error::Unsupported(unsupported("form POST with an enctype other than application/x-www-form-urlencoded and multipart/form-data")));
        }
        if multipart {
            let list = self.entry_list(it, form, submitter)?;
            let fd = crate::browser_web::form_from_entries(self, it, list)?;
            let (bytes, ct) = self.web_body(it, fd, 256 << 20)?;
            let body = crate::browser_navigation::Body { content_type: ct.expect("multipart has a type"), bytes };
            return self.submit_navigation(it, action.trim(), post, "", Some(body));
        }
        let entries: Vec<(String, String)> = self
            .entry_list(it, form, submitter)?
            .into_iter()
            .filter_map(|(n, v)| match v? {
                Ent::S(v) => Some((n, v)),
                // A file value sends its name.
                Ent::B(f) => Some((n, crate::browser_web::file_name(self, Value::Object(f))?)),
            })
            .collect();
        it.charge(entries.len() as u64 + 1)?;
        self.submit_navigation(it, action.trim(), post, &encode_entries(&entries), None)
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
    let n = crate::browser_controls::control(d, it, this)?;
    report_one(d, it, n, focus)
}

/// `checkValidity()` and `reportValidity()` of the one element `n`.
pub(crate) fn report_one(d: &Dom, it: &mut Interp<'_>, n: NodeId, focus: bool) -> V {
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
    fn maxlength_cuts_by_utf16_units_without_a_split_pair() {
        assert_eq!(fit_units("abcd", 2), "ab");
        assert_eq!(fit_units("a😀b", 2), "a");
        assert_eq!(fit_units("a😀b", 3), "a😀");
        assert_eq!(fit_units("abc", 0), "");
    }
}
