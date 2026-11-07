//! Form controls for the login path: `HTMLFormElement`, `HTMLButtonElement`, the form owner, the driver's click activation
//! and `SubmitEvent`. Sources: HTML Standard "Association of controls and forms", "The button element", "The input element"
//! (activation behavior), "Form submission algorithm", "The SubmitEvent interface"; DOM Standard "dispatch" (activation target).
//!
//! What is built
//! - Brands: `<form>` is an `HTMLFormElement`, `<button>` an `HTMLButtonElement` (HTML namespace only; an SVG `form` is not one).
//! - Reflection on the native null-namespace attributes (there is no second copy): `input.name .disabled .form .formNoValidate`,
//!   `button.name .type .disabled .form .formNoValidate`, `form.noValidate`. `button.type` follows the current standard: the
//!   keywords are ASCII case-insensitive, a missing or invalid value is the Auto state, which is a submit button unless
//!   `command` or `commandfor` is present or the parent is a `<select>`; the getter reads `submit`, `reset` or `button`.
//!   A boolean attribute that is set writes `""`, one that is cleared is removed. Every cap is checked before the write.
//! - The form owner is never stored: it is found again from the tree at each use (explicit `form` attribute when the control is
//!   connected, else the nearest HTML `form` ancestor), so clone, move, insert and remove need no bookkeeping. Listed controls
//!   are `button input fieldset object output select textarea`; only `input` and `button` expose `.form` so far. A foreign
//!   (SVG, MathML) control or `form` never takes part.
//! - A disabled form control (`button input select textarea`) is the `disabled` attribute, or a `<fieldset disabled>`
//!   ancestor unless the control is in that fieldset's first `<legend>` child.
//! - `Browser::click` is the user driver. A click at a disabled form control is not dispatched. Otherwise a `click` (bubbles,
//!   cancelable) is dispatched at the node, and when it is not canceled the activation behavior of the activation target
//!   runs. The target is chosen before any listener runs: the node itself or, for a bubbling click, the nearest ancestor with an
//!   activation behavior (`a area label input button summary`), as in the DOM dispatch algorithm. Only `button` and
//!   `input type=submit|image` act: they read their type, disabled state and form owner after the listeners, then "submit":
//!   the form must be connected, validation must be off (see below), and `submit` (a real `SubmitEvent`, bubbles, cancelable,
//!   `submitter` the control) is dispatched at the owner. Click and activation run in ONE host scope: one step budget, one
//!   call budget. A Limit, Unsupported or heap fault anywhere ends the call at once.
//! - `SubmitEvent` (`submitter`: read-only, null or an HTML element; traced, so a saved event keeps its submitter alive). An
//!   event made by script, with `new SubmitEvent` or any other, only dispatches; it never submits and is never gated.
//!
//! Validation and native navigation (since S4 topic 5)
//! - A submit button validates the form, fires `submit` and, when no listener cancels it, navigates. The rules, and what is not
//!   built, are in `browser_submit.rs`. An app that handles `submit` itself (`preventDefault`, then `fetch`) is unchanged.
//! - `Browser::submit` and `Input::Submit` stay the dispatch-only driver call of `browser.rs`: no activation, no validation.
//!
//! Deviations (on purpose)
//! - `Input::Click` of a session no longer calls `Browser::click`: it is the user click of `browser_drive.rs` (pointer events, mouse events, focus,
//!   `MouseEvent` click). `Browser::click` is kept for the unit tests: it is the dispatch-only click described here.
//! - The driver's `click` is a plain `Event` (there is no `MouseEvent`); the driver marks the activation itself, so a script's
//!   `dispatchEvent(new Event('click'))` never activates. `isTrusted` stays false.
//! - The `firing submission events` guard of a form is not kept: nothing can start a second submit inside a submit listener
//!   (`requestSubmit`, `submit()`, `HTMLElement.click()` do not exist).
//! - The form element pointer of the HTML parser (a control that the parser ties to a form that is not its ancestor, after
//!   `<table><form>`) is not recorded: such a control has the owner that the ancestor rule gives.
//! - `a`/`area`/`summary` activation, `popover` and invoker commands, checkbox and radio
//!   toggles: no default action, as everywhere in `browser.rs`. A click at a descendant of a disabled button is dispatched (the
//!   standard says "on the element"), but the button does not activate.
//! - `form.elements`, `FormData`, `form.action`, `form.method` and the other form attributes are absent. A reset button resets its form
//!   owner (`browser_controls.rs`).

use crate::browser::*;
use crate::browser_dom as nd;
use crate::browser_events::init_flag;
use bare_html::dom::{Atom, Document, NodeData, NodeId};
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value};

type V = R<Value>;

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

/// The content attribute `name` in no namespace.
fn attr<'a>(doc: &'a Document, n: NodeId, name: &str) -> Option<&'a str> {
    doc.find_atom(name).and_then(|a| doc.attr(n, a))
}

fn tag_is(doc: &Document, n: NodeId, names: &[&str]) -> bool {
    matches!(*doc.data(n), NodeData::Element { name, .. } if names.contains(&doc.atom_str(name))) && !doc.is_foreign(n)
}

/// A button's state: `Some(true)` submit button, `Some(false)` another button, by the standard's "submit button" rule.
/// Also gives the `type` getter text.
pub(crate) fn button_state(doc: &Document, n: NodeId) -> (bool, &'static str) {
    let t = attr(doc, n, "type").unwrap_or("");
    let by = |k: &str| t.eq_ignore_ascii_case(k);
    if by("submit") {
        return (true, "submit");
    }
    if by("reset") {
        return (false, "reset");
    }
    if by("button") {
        return (false, "button");
    }
    // Auto: a submit button unless it is an invoker or the first kind of child of a select.
    let invoker = attr(doc, n, "command").is_some() || attr(doc, n, "commandfor").is_some();
    let in_select = doc.parent(n).is_some_and(|p| tag_is(doc, p, &["select"]));
    if invoker || in_select { (false, "button") } else { (true, "submit") }
}

fn illegal(it: &mut Interp<'_>) -> Error {
    it.type_error("Illegal invocation")
}

impl Dom {
    /// `this` of an accessor on an HTML element of one of `tags`.
    fn html_this(&self, it: &mut Interp<'_>, this: Value, tags: &[&str]) -> R<NodeId> {
        let n = self.el(it, this)?;
        if tag_is(&self.doc.borrow(), n, tags) { Ok(n) } else { Err(illegal(it)) }
    }

    fn reflect_str(&self, it: &mut Interp<'_>, n: NodeId, name: &str) -> V {
        let s = attr(&self.doc.borrow(), n, name).unwrap_or("").to_string();
        self.js_str(it, &s)
    }

    fn reflect_set_str(&self, it: &mut Interp<'_>, n: NodeId, name: &str, v: Value) -> V {
        let s = self.string_arg(it, v)?;
        nd::attr_set(self, it, n, name, &s)?;
        Ok(Value::Undefined)
    }

    fn reflect_bool_set(&self, it: &mut Interp<'_>, n: NodeId, name: &str, v: Value) -> V {
        if it.to_js_bool(v)? {
            nd::attr_set(self, it, n, name, "")?;
        } else {
            self.doc.borrow_mut().remove_attr(n, name);
        }
        Ok(Value::Undefined)
    }

    /// Is `n` connected to the document? Charges the walk.
    pub(crate) fn is_connected_node(&self, it: &mut Interp<'_>, n: NodeId) -> R<bool> {
        let doc = self.doc.borrow();
        let mut cur = n;
        while let Some(p) = doc.parent(cur) {
            it.charge(1)?;
            cur = p;
        }
        Ok(cur == doc.root())
    }

    /// The form owner of a listed control, found from the tree now (HTML "reset the form owner" leaves the same result: the
    /// stored owner always equals this, except the parser's form element pointer, which is not recorded).
    pub(crate) fn form_owner(&self, it: &mut Interp<'_>, n: NodeId) -> R<Option<NodeId>> {
        let doc = self.doc.borrow();
        if !tag_is(&doc, n, &["button", "fieldset", "input", "object", "output", "select", "textarea"]) {
            return Ok(None);
        }
        let (mut nearest, mut top) = (None, n);
        while let Some(p) = doc.parent(top) {
            it.charge(1)?;
            if nearest.is_none() && tag_is(&doc, p, &["form"]) {
                nearest = Some(p);
            }
            top = p;
        }
        let Some(want) = attr(&doc, n, "form") else { return Ok(nearest) };
        if top != doc.root() {
            return Ok(nearest);
        }
        // Explicit owner: the first element with this ID in tree order, when it is a form. Otherwise none.
        if want.is_empty() {
            return Ok(None);
        }
        let (mut cur, mut depth) = (top, 0);
        while let Some((c, d)) = nd::next_in(&doc, top, cur, depth) {
            it.charge(1)?;
            (cur, depth) = (c, d);
            if doc.attr(c, Atom::ID) == Some(want) {
                return Ok(tag_is(&doc, c, &["form"]).then_some(c));
            }
        }
        Ok(None)
    }

    /// Is the form control `n` disabled (HTML "disabled")? Not a form control: false.
    pub(crate) fn is_disabled_control(&self, it: &mut Interp<'_>, n: NodeId) -> R<bool> {
        let doc = self.doc.borrow();
        if !tag_is(&doc, n, &["button", "input", "select", "textarea"]) {
            return Ok(false);
        }
        if attr(&doc, n, "disabled").is_some() {
            return Ok(true);
        }
        let (mut prev, mut cur) = (n, doc.parent(n));
        while let Some(a) = cur {
            it.charge(1)?;
            if tag_is(&doc, a, &["fieldset"]) && attr(&doc, a, "disabled").is_some() {
                let first_legend = doc.children(a).find(|&c| tag_is(&doc, c, &["legend"]));
                if first_legend != Some(prev) {
                    return Ok(true);
                }
            }
            (prev, cur) = (a, doc.parent(a));
        }
        Ok(false)
    }

    /// The nearest node from `n` upward with an activation behavior (DOM "dispatch", for a bubbling click).
    pub(crate) fn activation_target(&self, it: &mut Interp<'_>, n: NodeId) -> R<Option<NodeId>> {
        let doc = self.doc.borrow();
        let mut cur = Some(n);
        while let Some(c) = cur {
            it.charge(1)?;
            if tag_is(&doc, c, &["a", "area", "label", "input", "button", "summary"]) {
                return Ok(Some(c));
            }
            cur = doc.parent(c);
        }
        Ok(None)
    }

    /// The driver's click: dispatch, then the activation behavior if the click was not canceled. Gives whether the click was
    /// not canceled. Runs inside the caller's host scope.
    ///
    /// `ui` is set by the window (`browser_ui.rs`): the click is then a trusted `PointerEvent` and the target also has the
    /// default actions of a user click (checkbox, radio, label, link). The driver's click (`None`) keeps the older rules.
    pub(crate) fn user_click(&self, it: &mut Interp<'_>, node: NodeId, ui: Option<crate::browser_ui::Ui>) -> R<bool> {
        if self.is_disabled_control(it, node)? {
            return Ok(true);
        }
        let target = self.activation_target(it, node)?;
        let Some(ui) = ui else {
            let e = self.new_event(it, "click", true, true, true, None)?;
            let not_canceled = self.dispatch(it, Target::Node(node), e)?;
            if let (true, Some(t)) = (not_canceled, target) {
                self.activate(it, t)?;
            }
            return Ok(not_canceled);
        };
        let undo = match target {
            Some(t) => self.pre_activate(it, t)?,
            None => None,
        };
        let not_canceled = self.fire_ui(it, node, "click", self.p().pointer, true, true, ui.clone(), None)?;
        if let Some(t) = target {
            self.ui_activate(it, t, !not_canceled, undo, &ui)?;
        }
        Ok(not_canceled)
    }

    /// The activation behavior of `t`, reading its state now.
    pub(crate) fn activate(&self, it: &mut Interp<'_>, t: NodeId) -> R<()> {
        // 0: nothing, 1: submit, 2: reset
        let act = {
            let doc = self.doc.borrow();
            if tag_is(&doc, t, &["button"]) {
                match button_state(&doc, t) {
                    (true, _) => 1,
                    (_, "reset") => 2,
                    _ => 0,
                }
            } else if tag_is(&doc, t, &["input"]) {
                match nd::input_kind(&doc, t) {
                    "submit" | "image" => 1,
                    "reset" => 2,
                    _ => 0,
                }
            } else {
                0
            }
        };
        if act == 0 || self.is_disabled_control(it, t)? {
            return Ok(());
        }
        match (self.form_owner(it, t)?, act) {
            (Some(form), 1) => self.submit_form(it, form, Some(t)),
            (Some(form), _) => self.reset_form(it, form),
            _ => Ok(()),
        }
    }

    /// HTML "submit" a form from a submit button or from Enter in a field: validation, the `submit` event, the navigation.
    pub(crate) fn submit_form(&self, it: &mut Interp<'_>, form: NodeId, submitter: Option<NodeId>) -> R<()> {
        self.submit_with(it, form, submitter, true, true)
    }
}

impl Browser {
    /// The user driver's click: `click` (bubbles, cancelable) at `node`, then, if no listener canceled it, the activation
    /// behavior of the nearest activation target. A disabled form control gets no click at all. Submit buttons submit their
    /// form owner: see `browser_form.rs` for the exact rules (`browser_submit.rs` has validation and native navigation). Click and
    /// activation share one host scope and one budget. `reports` hold both stages.
    pub fn click(&mut self, node: NodeId) -> R<Dispatched> {
        let (not_canceled, reports) = self.driven(|d, it| d.user_click(it, node, None))?;
        Ok(Dispatched { not_canceled, reports })
    }
}

// ------------------------------------------------------------------------------------------ reflection

pub(crate) fn input_name(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.html_this(it, this, &["input"])?;
    d.reflect_str(it, n, "name")
}

pub(crate) fn set_input_name(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.html_this(it, this, &["input"])?;
    d.reflect_set_str(it, n, "name", arg(a, 0))
}

pub(crate) fn button_name(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.html_this(it, this, &["button"])?;
    d.reflect_str(it, n, "name")
}

pub(crate) fn set_button_name(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.html_this(it, this, &["button"])?;
    d.reflect_set_str(it, n, "name", arg(a, 0))
}

pub(crate) fn button_type(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.html_this(it, this, &["button"])?;
    let t = button_state(&d.doc.borrow(), n).1;
    d.js_str(it, t)
}

pub(crate) fn set_button_type(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.html_this(it, this, &["button"])?;
    d.reflect_set_str(it, n, "type", arg(a, 0))
}

/// One boolean reflection pair for `input` (`$tag` "input") or `button`.
macro_rules! bool_attr {
    ($get:ident, $set:ident, $tag:literal, $attr:literal) => {
        pub(crate) fn $get(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
            let n = d.html_this(it, this, &[$tag])?;
            Ok(Value::Bool(attr(&d.doc.borrow(), n, $attr).is_some()))
        }
        pub(crate) fn $set(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
            let n = d.html_this(it, this, &[$tag])?;
            d.reflect_bool_set(it, n, $attr, arg(a, 0))
        }
    };
}

bool_attr!(input_disabled, set_input_disabled, "input", "disabled");
bool_attr!(button_disabled, set_button_disabled, "button", "disabled");
bool_attr!(input_form_no_validate, set_input_form_no_validate, "input", "formnovalidate");
bool_attr!(button_form_no_validate, set_button_form_no_validate, "button", "formnovalidate");
bool_attr!(form_no_validate, set_form_no_validate, "form", "novalidate");

pub(crate) fn input_form(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.html_this(it, this, &["input"])?;
    let f = d.form_owner(it, n)?;
    d.wrap_opt(it, f)
}

pub(crate) fn button_form(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.html_this(it, this, &["button"])?;
    let f = d.form_owner(it, n)?;
    d.wrap_opt(it, f)
}

// ------------------------------------------------------------------------------------------ SubmitEvent

pub(crate) fn submit_event_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    if a.is_empty() {
        return Err(it.type_error("Failed to construct 'SubmitEvent': 1 argument required, but only 0 present."));
    }
    let ty = d.string_arg(it, a[0])?;
    let (mut bubbles, mut cancelable, mut composed, mut submitter) = (false, false, false, Value::Null);
    match arg(a, 1) {
        Value::Undefined | Value::Null => {}
        init @ Value::Object(_) => {
            bubbles = init_flag(d, it, init, "bubbles")?;
            cancelable = init_flag(d, it, init, "cancelable")?;
            composed = init_flag(d, it, init, "composed")?;
            let k = d.key(it, "submitter")?;
            match it.get_value(init, k)? {
                Value::Undefined | Value::Null => {}
                v => match d.node_of(v) {
                    Some(n) if d.is_element(n) && !d.doc.borrow().is_foreign(n) => submitter = v,
                    _ => return Err(it.type_error("Failed to construct 'SubmitEvent': Failed to read the 'submitter' property from 'SubmitEventInit': Failed to convert value to 'HTMLElement'.")),
                },
            }
        }
        _ => return Err(it.type_error("The event initializer is not an object.")),
    }
    let proto = it.host_proto_from_new_target(d.p().submit)?;
    let e = d.new_event_proto(it, &ty, bubbles, cancelable, composed, proto)?;
    let mut st = d.st.borrow_mut();
    let ev = st.events.get_mut(&e).expect("just made");
    ev.related = submitter;
    ev.is_submit = true;
    Ok(Value::Object(e))
}

pub(crate) fn submit_event_submitter(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let o: ObjId = d.event_of(it, this)?;
    let st = d.st.borrow();
    let ev = &st.events[&o];
    if ev.is_submit { Ok(ev.related) } else { Err(illegal(it)) }
}
