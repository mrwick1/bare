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

use crate::eng::{Eng, Ops};
use crate::browser::*;
use crate::browser_dom as nd;
use crate::browser_scoped::*;
use bare_html::dom::{Atom, Document, NodeData, NodeId};
use bare_js_api::{Args, Local};

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

impl<E: Eng> Dom<E> {
    /// `this` of an accessor on an HTML element of one of `tags`.
    fn html_this_s<'s>(&self, s: &mut Sc<'s, E>, this: Local<'s>, tags: &[&str]) -> Res<'s, NodeId> {
        let n = self.el_s(s, this)?;
        if tag_is(&self.doc.borrow(), n, tags) { Ok(n) } else { Err(s.type_error("Illegal invocation")) }
    }

    fn reflect_str_s<'s>(&self, s: &mut Sc<'s, E>, n: NodeId, name: &str) -> Res<'s, Local<'s>> {
        let t = attr(&self.doc.borrow(), n, name).unwrap_or("").to_string();
        self.js_str_s(s, &t)
    }

    fn reflect_set_str_s<'s>(&self, s: &mut Sc<'s, E>, n: NodeId, name: &str, v: Local<'s>) -> Res<'s, Local<'s>> {
        let t = self.string_arg_s(s, v)?;
        nd::attr_set_s(self, s, n, name, &t)?;
        Ok(s.undefined())
    }

    fn reflect_bool_set_s<'s>(&self, s: &mut Sc<'s, E>, n: NodeId, name: &str, v: Local<'s>) -> Res<'s, Local<'s>> {
        if s.to_boolean(v) {
            nd::attr_set_s(self, s, n, name, "")?;
        } else {
            self.doc.borrow_mut().remove_attr(n, name);
        }
        Ok(s.undefined())
    }

    /// Is `n` connected to the document? Charges the walk.
    pub(crate) fn is_connected_node(&self, it: &mut E::Vm, n: NodeId) -> R<E, bool> {
        let doc = self.doc.borrow();
        let mut cur = n;
        while let Some(p) = doc.parent(cur) {
            it.charge_o(1)?;
            cur = p;
        }
        Ok(cur == doc.root())
    }

    /// The form owner of a listed control, found from the tree now (HTML "reset the form owner" leaves the same result: the
    /// stored owner always equals this, except the parser's form element pointer, which is not recorded).
    pub(crate) fn form_owner(&self, it: &mut E::Vm, n: NodeId) -> R<E, Option<NodeId>> {
        let doc = self.doc.borrow();
        if !tag_is(&doc, n, &["button", "fieldset", "input", "object", "output", "select", "textarea"]) && !self.is_face(n) {
            return Ok(None);
        }
        let form = doc.find_atom("form");
        let is_form = |el| {
            matches!(*doc.data(el), NodeData::Element { name, .. } if Some(name) == form) && !doc.is_foreign(el)
        };
        let (mut nearest, mut top) = (None, n);
        while let Some(p) = doc.parent(top) {
            it.charge_o(1)?;
            if nearest.is_none() && is_form(p) {
                nearest = Some(p);
            }
            top = p;
        }
        let Some(want) = form.and_then(|a| doc.attr(n, a)) else { return Ok(nearest) };
        if top != doc.root() {
            return Ok(nearest);
        }
        // Explicit owner: the first element with this ID in tree order, when it is a form. Otherwise none.
        if want.is_empty() {
            return Ok(None);
        }
        let (mut cur, mut depth) = (top, 0);
        while let Some((c, d)) = nd::next_in(&doc, top, cur, depth) {
            it.charge_o(1)?;
            (cur, depth) = (c, d);
            if doc.attr(c, Atom::ID) == Some(want) {
                return Ok(is_form(c).then_some(c));
            }
        }
        Ok(None)
    }

    /// `form.elements`: the listed elements (not `input type=image`) whose form owner is `form`, in tree order of the tree of `form`.
    pub(crate) fn form_elements(&self, it: &mut E::Vm, form: NodeId) -> R<E, Vec<NodeId>> {
        let found = {
            let doc = self.doc.borrow();
            let mut top = form;
            while let Some(p) = doc.parent(top) {
                it.charge_o(1)?;
                top = p;
            }
            let mut found = Vec::new();
            let (mut cur, mut depth) = (top, 0);
            while let Some((c, d)) = nd::next_in(&doc, top, cur, depth) {
                it.charge_o(1)?;
                (cur, depth) = (c, d);
                if tag_is(&doc, c, &["button", "fieldset", "input", "object", "output", "select", "textarea"]) && !(tag_is(&doc, c, &["input"]) && nd::input_kind(&doc, c) == "image") {
                    found.push(c);
                }
            }
            found
        };
        let mut out = Vec::new();
        for c in found {
            if self.form_owner(it, c)? == Some(form) {
                out.push(c);
            }
        }
        Ok(out)
    }

    /// Is the form control `n` disabled (HTML "disabled")? Not a form control: false.
    pub(crate) fn is_disabled_control(&self, it: &mut E::Vm, n: NodeId) -> R<E, bool> {
        let doc = self.doc.borrow();
        if !tag_is(&doc, n, &["button", "input", "select", "textarea"]) && !self.is_face(n) {
            return Ok(false);
        }
        let disabled = doc.find_atom("disabled");
        if disabled.is_some_and(|a| doc.attr(n, a).is_some()) {
            return Ok(true);
        }
        let (mut prev, mut cur) = (n, doc.parent(n));
        while let Some(a) = cur {
            it.charge_o(1)?;
            if disabled.is_some_and(|name| tag_is(&doc, a, &["fieldset"]) && doc.attr(a, name).is_some()) {
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
    pub(crate) fn activation_target(&self, it: &mut E::Vm, n: NodeId) -> R<E, Option<NodeId>> {
        let doc = self.doc.borrow();
        let mut cur = Some(n);
        while let Some(c) = cur {
            it.charge_o(1)?;
            if tag_is(&doc, c, &["a", "area", "label", "input", "button", "summary", "textarea", "select"]) {
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
    pub(crate) fn user_click(&self, it: &mut E::Vm, node: NodeId, ui: Option<crate::browser_ui::Ui>) -> R<E, bool> {
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
    pub(crate) fn activate(&self, it: &mut E::Vm, t: NodeId) -> R<E, ()> {
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
    pub(crate) fn submit_form(&self, it: &mut E::Vm, form: NodeId, submitter: Option<NodeId>) -> R<E, ()> {
        self.submit_with(it, form, submitter, true, true)
    }
}

impl<E: Eng> Browser<E> {
    /// The user driver's click: `click` (bubbles, cancelable) at `node`, then, if no listener canceled it, the activation
    /// behavior of the nearest activation target. A disabled form control gets no click at all. Submit buttons submit their
    /// form owner: see `browser_form.rs` for the exact rules (`browser_submit.rs` has validation and native navigation). Click and
    /// activation share one host scope and one budget. `reports` hold both stages.
    pub fn click(&mut self, node: NodeId) -> R<E, Dispatched> {
        let (not_canceled, reports) = self.driven(|d, it| d.user_click(it, node, None))?;
        Ok(Dispatched { not_canceled, reports })
    }
}

// ------------------------------------------------------------------------------------------ reflection

pub(crate) fn input_name<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.html_this_s(s, this, &["input"])?;
    d.reflect_str_s(s, n, "name")
}

pub(crate) fn set_input_name<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.html_this_s(s, this, &["input"])?;
    let v = arg_s::<E>(s, a, 0);
    d.reflect_set_str_s(s, n, "name", v)
}

pub(crate) fn button_name<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.html_this_s(s, this, &["button"])?;
    d.reflect_str_s(s, n, "name")
}

pub(crate) fn set_button_name<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.html_this_s(s, this, &["button"])?;
    let v = arg_s::<E>(s, a, 0);
    d.reflect_set_str_s(s, n, "name", v)
}

pub(crate) fn button_type<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.html_this_s(s, this, &["button"])?;
    let t = button_state(&d.doc.borrow(), n).1;
    d.js_str_s(s, t)
}

pub(crate) fn set_button_type<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.html_this_s(s, this, &["button"])?;
    let v = arg_s::<E>(s, a, 0);
    d.reflect_set_str_s(s, n, "type", v)
}

/// One boolean reflection pair for `input` (`$tag` "input") or `button`.
macro_rules! bool_attr {
    ($get:ident, $set:ident, $tag:literal, $attr:literal) => {
        pub(crate) fn $get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
            let n = d.html_this_s(s, this, &[$tag])?;
            Ok(s.boolean(attr(&d.doc.borrow(), n, $attr).is_some()))
        }
        pub(crate) fn $set<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
            let n = d.html_this_s(s, this, &[$tag])?;
            let v = arg_s::<E>(s, a, 0);
            d.reflect_bool_set_s(s, n, $attr, v)
        }
    };
}

bool_attr!(input_disabled, set_input_disabled, "input", "disabled");
bool_attr!(input_read_only, set_input_read_only, "input", "readonly");
bool_attr!(button_disabled, set_button_disabled, "button", "disabled");
bool_attr!(input_form_no_validate, set_input_form_no_validate, "input", "formnovalidate");
bool_attr!(button_form_no_validate, set_button_form_no_validate, "button", "formnovalidate");
bool_attr!(form_no_validate, set_form_no_validate, "form", "novalidate");

pub(crate) fn input_form<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.html_this_s(s, this, &["input"])?;
    let f = old::<E, _>(s, |it| d.form_owner(it, n))?;
    d.wrap_opt_s(s, f)
}

pub(crate) fn button_form<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.html_this_s(s, this, &["button"])?;
    let f = old::<E, _>(s, |it| d.form_owner(it, n))?;
    d.wrap_opt_s(s, f)
}

// ------------------------------------------------------------------------------------------ SubmitEvent

pub(crate) fn submit_event_ctor_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    if a.is_empty() {
        return Err(s.type_error("Failed to construct 'SubmitEvent': 1 argument required, but only 0 present."));
    }
    let ty = d.arg_str_s(s, a, 0)?;
    let (mut flags, mut submitter) = ([false; 3], None);
    let init = arg_s::<E>(s, a, 1);
    if !is_nullish::<E>(s, init) {
        if !is_object::<E>(s, init) {
            return Err(s.type_error("The event initializer is not an object."));
        }
        for (flag, name) in flags.iter_mut().zip(["bubbles", "cancelable", "composed"]) {
            let v = s.get_str(init, name)?;
            *flag = s.to_boolean(v);
        }
        let v = s.get_str(init, "submitter")?;
        if !is_nullish::<E>(s, v) {
            match d.node_of_s(s, v) {
                Some(n) if d.is_element(n) && !d.doc.borrow().is_foreign(n) => submitter = Some(v),
                _ => return Err(s.type_error("Failed to construct 'SubmitEvent': Failed to read the 'submitter' property from 'SubmitEventInit': Failed to convert value to 'HTMLElement'.")),
            }
        }
    }
    let default = d.obj_local(s, d.p().submit);
    let proto = s.proto_from_new_target(default)?;
    let proto = obj_id::<E>(s, proto).expect("a prototype is an object");
    let [bubbles, cancelable, composed] = flags;
    let e = old::<E, _>(s, |it| d.new_event_proto(it, &ty, bubbles, cancelable, composed, proto))?;
    let who = match submitter {
        Some(v) => v,
        None => s.null(),
    };
    {
        let mut st = d.st.borrow_mut();
        let ev = st.events.get_mut(&e).expect("just made");
        s.store(&mut ev.related, who);
        ev.is_submit = true;
    }
    Ok(d.obj_local(s, e))
}

pub(crate) fn submit_event_submitter<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let Some(o) = obj_id::<E>(s, this).filter(|o| d.st.borrow().events.contains_key(o)) else { return Err(s.type_error("Illegal invocation")) };
    let st = d.st.borrow();
    let ev = &st.events[&o];
    if ev.is_submit { Ok(s.traced(&ev.related)) } else { drop(st); Err(s.type_error("Illegal invocation")) }
}
