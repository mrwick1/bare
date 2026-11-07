//! Form-associated custom elements: `static formAssociated`, `attachInternals()`, `ElementInternals` and `CustomStateSet`. The model is the HTML
//! Standard "form-associated custom elements"; the cases (messages too) are compared with headless Chrome 155 (`tools/fixtures/ui/custom-form-cases.txt`).
//!
//! What is built
//! - A form-associated element is a listed, submittable and labelable element. `Dom::is_face` says so; the form owner, `disabled`, the entry list
//!   of `FormData` and of a submission, `form.reset()`, `form.checkValidity()` and `labels` read it (`browser_form.rs`, `browser_submit.rs`,
//!   `browser_controls.rs`).
//! - The callbacks. `formAssociatedCallback` and `formDisabledCallback` come from one reaction, `FormCheck` (`browser_custom.rs`): it reads the form
//!   owner and the `disabled` state from the tree when it runs and calls for what differs from `Face::owner` and `Face::disabled`. The watcher puts it
//!   on an element that entered a tree, left one, or changed `form` or `disabled`, on the elements under a `fieldset` that changed `disabled`, and on
//!   the elements with a `form=` attribute when a form came or went. A removal also reports `null` at once, as Chrome does for a move.
//!   `formResetCallback` is queued by `form.reset()`. `formStateRestoreCallback` is read at `define` and never called: nothing restores state.
//! - `ElementInternals`: `form`, `labels`, `willValidate`, `validity`, `validationMessage`, `checkValidity()`, `reportValidity()`,
//!   `setFormValue()`, `setValidity()`, `states`, `shadowRoot` and `role`. The members that need a form-associated element throw
//!   `NotSupportedError` on another one. `attachInternals` is defined on `HTMLElement.prototype` by the first `define`.
//! - `setFormValue(value)`: a string, a File or a FormData (its entries go in as they are); `null` and `undefined` remove the value. The second
//!   argument is not read. A file or a string needs a `name` attribute to be in the entry list.
//! - `CustomStateSet` and `:state(name)`: the states are in the native document (`Document::states`), so the selector engine reads them. A state set
//!   is a snapshot for its iterators.
//!
//! Deviations
//! - No `aria*` reflection of `ElementInternals` (bare has none on `Element` either), only `role`. `disabledFeatures` reads `internals`; `shadow`
//!   does not stop `attachShadow`.
//! - A submission sends the name of a File value, not its bytes (bare sends no file at all for a file input).
//! - The anchor of `setValidity` is checked and not used (no message bubble). `form.elements`, `form.length` and named access do not exist for any control.
//! - A move that takes an element out of the shadow-including tree of a form that its `form=` names is seen when the reaction runs, not at the removal.

use crate::browser::*;
use crate::browser_custom::st;
use crate::browser_dom::arg;
use crate::browser_web::Ent;
use bare_html::dom::{Document, NodeId, PRECUSTOMIZED};
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};

type V = R<Value>;

/// What `setFormValue` kept.
pub(crate) enum FVal {
    S(String),
    B(ObjId),
    List(Vec<(String, Ent)>),
}

/// The state of one form-associated element.
pub(crate) struct Face {
    pub(crate) el: NodeId,
    /// The form owner and the `disabled` state that the callbacks last told.
    pub(crate) owner: Option<NodeId>,
    pub(crate) disabled: bool,
    value: Option<FVal>,
    /// The flags and the message of `setValidity`, in the bits of `browser_controls.rs`.
    flags: u16,
    msg: String,
    ints: Option<ObjId>,
    states: Option<ObjId>,
    role: Option<String>,
}

impl Face {
    pub(crate) fn new(el: NodeId) -> Face {
        Face { el, owner: None, disabled: false, value: None, flags: 0, msg: String::new(), ints: None, states: None, role: None }
    }

    pub(crate) fn roots(&self, out: &mut Vec<Value>) {
        match &self.value {
            Some(FVal::B(o)) => out.push(Value::Object(*o)),
            Some(FVal::List(l)) => out.extend(l.iter().filter_map(|(_, e)| if let Ent::B(o) = e { Some(Value::Object(*o)) } else { None })),
            _ => {}
        }
        out.extend(self.states.map(Value::Object));
    }
}

/// The nearest form that holds `e` inside the tree of `top`, a tree that is about to leave its parent.
pub(crate) fn owner_within(doc: &Document, e: NodeId, top: NodeId) -> Option<NodeId> {
    let mut cur = e;
    while cur != top {
        cur = doc.parent(cur)?;
        if doc.is_html(cur, "form") {
            return Some(cur);
        }
    }
    None
}

impl Dom {
    /// The flags and the message of a form-associated element; `None` for any other element.
    #[inline]
    pub(crate) fn face_verdict(&self, n: NodeId) -> Option<(u16, String)> {
        if self.is_face(n) { self.face_verdict_of(n) } else { None }
    }

    fn face_verdict_of(&self, n: NodeId) -> Option<(u16, String)> {
        Some(st(self).borrow().face.get(&(n.index() as u32)).map_or((0, String::new()), |f| (f.flags, f.msg.clone())))
    }

    /// The entries of a form-associated element for the entry list. The name is `name`.
    #[inline]
    pub(crate) fn face_entries(&self, n: NodeId, name: &str) -> Option<Vec<(String, Option<Ent>)>> {
        if self.is_face(n) { self.face_entries_of(n, name) } else { None }
    }

    fn face_entries_of(&self, n: NodeId, name: &str) -> Option<Vec<(String, Option<Ent>)>> {
        let s = st(self).borrow();
        let out = match s.face.get(&(n.index() as u32)).and_then(|f| f.value.as_ref()) {
            Some(FVal::S(v)) if !name.is_empty() => vec![(name.to_string(), Some(Ent::S(v.clone())))],
            Some(FVal::B(o)) if !name.is_empty() => vec![(name.to_string(), Some(Ent::B(*o)))],
            Some(FVal::List(l)) => l.iter().map(|(n, e)| (n.clone(), Some(crate::browser_web::clone_ent(e)))).collect(),
            _ => Vec::new(),
        };
        Some(out)
    }
}

// ------------------------------------------------------------------------------------------ the members

/// The element of `ElementInternals` (`states` false) or `CustomStateSet` (`states` true) `this`.
fn target(d: &Dom, it: &mut Interp<'_>, this: Value, states: bool) -> R<NodeId> {
    match this {
        Value::Object(o) => match st(d).borrow().ints.get(&o) {
            Some(&(n, k)) if k == states => Ok(n),
            _ => Err(it.type_error("Illegal invocation")),
        },
        _ => Err(it.type_error("Illegal invocation")),
    }
}

/// The element of `this`, which must be a form-associated custom element. `what` starts the message: `Failed to execute 'setFormValue' on`.
fn face_target(d: &Dom, it: &mut Interp<'_>, this: Value, what: &str) -> R<NodeId> {
    let n = target(d, it, this, false)?;
    if d.is_face(n) {
        return Ok(n);
    }
    Err(d.dom_err(it, "NotSupportedError", &format!("{what} 'ElementInternals': The target element is not a form-associated custom element.")))
}

fn read_member(d: &Dom, it: &mut Interp<'_>, this: Value, name: &str) -> R<NodeId> {
    face_target(d, it, this, &format!("Failed to read the '{name}' property from"))
}

fn call_member(d: &Dom, it: &mut Interp<'_>, this: Value, name: &str) -> R<NodeId> {
    face_target(d, it, this, &format!("Failed to execute '{name}' on"))
}

fn args_needed(it: &mut Interp<'_>, a: &[Value], what: &str) -> R<()> {
    if a.is_empty() {
        return Err(it.type_error(&format!("Failed to execute '{what}': 1 argument required, but only 0 present.")));
    }
    Ok(())
}

/// `HTMLElement.prototype.attachInternals()`.
pub(crate) fn attach_internals(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.el(it, this)?;
    let fail = |it: &mut Interp<'_>, m: &str| d.dom_err(it, "NotSupportedError", &format!("Failed to execute 'attachInternals' on 'HTMLElement': {m}"));
    let Some(no_internals) = d.ce_def(n) else { return Err(fail(it, "Unable to attach ElementInternals to non-custom elements.")) };
    if no_internals {
        return Err(fail(it, "ElementInternals is disabled by disabledFeature static field."));
    }
    if st(d).borrow().face.get(&(n.index() as u32)).is_some_and(|f| f.ints.is_some()) {
        return Err(fail(it, "ElementInternals for the specified element was already attached."));
    }
    let proto = made(d, it)?[0];
    let o = d.obj(it, proto)?;
    let mut s = st(d).borrow_mut();
    s.face.entry(n.index() as u32).or_insert_with(|| Face::new(n)).ints = Some(o);
    s.ints.insert(o, (n, false));
    Ok(Value::Object(o))
}

fn form(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = read_member(d, it, this, "form")?;
    // An upgrade has no form owner until its constructor returned.
    let f = if d.doc.borrow().custom_state(n) == Some(PRECUSTOMIZED) { None } else { d.form_owner(it, n)? };
    d.wrap_opt(it, f)
}

fn labels(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = read_member(d, it, this, "labels")?;
    crate::browser_controls::labels_of(d, it, n)
}

fn will_validate(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = read_member(d, it, this, "willValidate")?;
    Ok(Value::Bool(d.will_validate(it, n)?))
}

fn validity(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = read_member(d, it, this, "validity")?;
    crate::browser_controls::validity_of(d, it, n)
}

fn validation_message(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = read_member(d, it, this, "validationMessage")?;
    let m = crate::browser_controls::message_of(d, it, n)?;
    d.js_str(it, &m)
}

fn check_validity(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = call_member(d, it, this, "checkValidity")?;
    crate::browser_submit::report_one(d, it, n, false)
}

fn report_validity(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = call_member(d, it, this, "reportValidity")?;
    crate::browser_submit::report_one(d, it, n, true)
}

fn set_form_value(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    args_needed(it, a, "setFormValue' on 'ElementInternals")?;
    let value = match a[0] {
        Value::Undefined | Value::Null => None,
        v => Some(if let Some(l) = crate::browser_web::form_entries(d, v) {
            FVal::List(l)
        } else if crate::browser_web::file_name(d, v).is_some() {
            let Value::Object(o) = v else { unreachable!("a File is an object") };
            FVal::B(o)
        } else {
            FVal::S(d.string_arg(it, v)?)
        }),
    };
    let n = call_member(d, it, this, "setFormValue")?;
    st(d).borrow_mut().face.entry(n.index() as u32).or_insert_with(|| Face::new(n)).value = value;
    Ok(Value::Undefined)
}

const FLAGS: [&str; 10] = ["valueMissing", "typeMismatch", "patternMismatch", "tooLong", "tooShort", "rangeUnderflow", "rangeOverflow", "stepMismatch", "badInput", "customError"];

fn set_validity(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    args_needed(it, a, "setValidity' on 'ElementInternals")?;
    let mut flags = 0u16;
    match a[0] {
        Value::Undefined | Value::Null => {}
        o @ Value::Object(_) => {
            for (i, name) in FLAGS.iter().enumerate() {
                let k = d.key(it, name)?;
                let v = it.get_value(o, k)?;
                if it.to_js_bool(v)? {
                    flags |= 1 << i;
                }
            }
        }
        _ => return Err(it.type_error("Failed to execute 'setValidity' on 'ElementInternals': The provided value is not of type 'ValidityStateFlags'.")),
    }
    let msg = match arg(a, 1) {
        Value::Undefined => String::new(),
        v => d.string_arg(it, v)?,
    };
    let anchor = match arg(a, 2) {
        Value::Undefined | Value::Null => None,
        v => match d.node_of(v).filter(|&n| d.is_element(n)) {
            Some(n) => Some(n),
            None => return Err(it.type_error("Failed to execute 'setValidity' on 'ElementInternals': parameter 3 is not of type 'HTMLElement'.")),
        },
    };
    let n = call_member(d, it, this, "setValidity")?;
    if flags != 0 && msg.is_empty() {
        return Err(it.type_error("Failed to execute 'setValidity' on 'ElementInternals': The second argument should not be empty if one or more flags in the first argument are true."));
    }
    if let Some(x) = anchor {
        let doc = d.doc.borrow();
        let mut cur = Some(x);
        while let Some(c) = cur.filter(|&c| c != n) {
            it.charge(1)?;
            cur = doc.parent_or_host(c);
        }
        if cur.is_none() {
            drop(doc);
            return Err(d.dom_err(it, "NotFoundError", "Failed to execute 'setValidity' on 'ElementInternals': The Element argument should be a shadow-including inclusive descendant of the target element."));
        }
    }
    let mut s = st(d).borrow_mut();
    let f = s.face.entry(n.index() as u32).or_insert_with(|| Face::new(n));
    (f.flags, f.msg) = (flags, if flags == 0 { String::new() } else { msg });
    Ok(Value::Undefined)
}

fn shadow_root(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = target(d, it, this, false)?;
    let r = d.doc.borrow().shadow_root(n);
    d.wrap_opt(it, r)
}

fn role(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = target(d, it, this, false)?;
    let r = st(d).borrow().face.get(&(n.index() as u32)).and_then(|f| f.role.clone());
    match r {
        Some(r) => d.js_str(it, &r),
        None => Ok(Value::Null),
    }
}

fn set_role(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    args_needed(it, a, "role' on 'ElementInternals")?;
    let v = match a[0] {
        Value::Null => None,
        v => Some(d.string_arg(it, v)?),
    };
    let n = target(d, it, this, false)?;
    st(d).borrow_mut().face.entry(n.index() as u32).or_insert_with(|| Face::new(n)).role = v;
    Ok(Value::Undefined)
}

fn states(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = target(d, it, this, false)?;
    let cached = st(d).borrow().face.get(&(n.index() as u32)).and_then(|f| f.states);
    if let Some(o) = cached {
        return Ok(Value::Object(o));
    }
    let proto = made(d, it)?[2];
    let o = d.obj(it, proto)?;
    let mut s = st(d).borrow_mut();
    s.face.entry(n.index() as u32).or_insert_with(|| Face::new(n)).states = Some(o);
    s.ints.insert(o, (n, true));
    Ok(Value::Object(o))
}

// ------------------------------------------------------------------------------------------ CustomStateSet

fn state_arg(d: &Dom, it: &mut Interp<'_>, a: &[Value], what: &str) -> R<String> {
    args_needed(it, a, &format!("{what}' on 'CustomStateSet"))?;
    d.string_arg(it, a[0])
}

fn state_add(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let v = state_arg(d, it, a, "add")?;
    let n = target(d, it, this, true)?;
    let mut all = d.doc.borrow().states(n).to_vec();
    if !all.iter().any(|s| **s == *v) {
        all.push(v.into());
        d.doc.borrow_mut().set_states(n, all);
    }
    Ok(Value::Undefined)
}

fn state_delete(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let v = state_arg(d, it, a, "delete")?;
    let n = target(d, it, this, true)?;
    let mut all = d.doc.borrow().states(n).to_vec();
    let Some(i) = all.iter().position(|s| **s == *v) else { return Ok(Value::Bool(false)) };
    all.remove(i);
    d.doc.borrow_mut().set_states(n, all);
    Ok(Value::Bool(true))
}

fn state_has(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let v = state_arg(d, it, a, "has")?;
    let n = target(d, it, this, true)?;
    Ok(Value::Bool(d.doc.borrow().states(n).iter().any(|s| **s == *v)))
}

fn state_clear(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = target(d, it, this, true)?;
    if !d.doc.borrow().states(n).is_empty() {
        d.doc.borrow_mut().set_states(n, Vec::new());
    }
    Ok(Value::Undefined)
}

fn state_size(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = target(d, it, this, true)?;
    Ok(Value::Number(d.doc.borrow().states(n).len() as f64))
}

/// The states of the set `this`, as strings.
fn state_list(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<Vec<Value>> {
    let n = target(d, it, this, true)?;
    let all = d.doc.borrow().states(n).to_vec();
    all.iter().map(|s| d.js_str(it, s)).collect()
}

/// An iterator over `items`: the one of a new array.
fn iterate(d: &Dom, it: &mut Interp<'_>, items: &[Value]) -> V {
    let arr = it.array_from(items)?;
    let k = d.key(it, "values")?;
    let f = it.get_value(arr, k)?;
    it.call(f, arr, &[])
}

fn state_values(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let items = state_list(d, it, this)?;
    iterate(d, it, &items)
}

fn state_entries(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let items = state_list(d, it, this)?;
    let pairs = items.iter().map(|&v| it.array_from(&[v, v])).collect::<R<Vec<_>>>()?;
    iterate(d, it, &pairs)
}

fn state_for_each(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    args_needed(it, a, "forEach' on 'CustomStateSet")?;
    let items = state_list(d, it, this)?;
    if !it.is_callable_value(a[0]) {
        return Err(it.type_error("Failed to execute 'forEach' on 'CustomStateSet': The callback provided as parameter 1 is not a function."));
    }
    for v in items {
        it.charge(1)?;
        it.call(a[0], arg(a, 1), &[v, v, this])?;
    }
    Ok(Value::Undefined)
}

// ------------------------------------------------------------------------------------------ the interfaces

fn internals_ctor(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Err(it.type_error("Failed to construct 'ElementInternals': Illegal constructor"))
}

fn states_ctor(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Err(it.type_error("Failed to construct 'CustomStateSet': Illegal constructor"))
}

/// The prototype and the constructor of `ElementInternals`, then those of `CustomStateSet`.
fn made(d: &Dom, it: &mut Interp<'_>) -> R<[ObjId; 4]> {
    if let Some(&m) = d.ce.int.get() {
        return Ok(m);
    }
    let op = it.object_prototype();
    let (ip, sp) = (d.obj(it, op)?, d.obj(it, op)?);
    for (n, g) in [("form", form as H), ("labels", labels), ("willValidate", will_validate), ("validity", validity), ("validationMessage", validation_message), ("states", states), ("shadowRoot", shadow_root)] {
        d.def_get(it, ip, n, g, None)?;
    }
    d.def_get(it, ip, "role", role, Some(set_role))?;
    for (n, len, h) in [("checkValidity", 0, check_validity as H), ("reportValidity", 0, report_validity), ("setFormValue", 1, set_form_value), ("setValidity", 1, set_validity)] {
        d.def_method(it, ip, n, len, h)?;
    }
    for (n, len, h) in [("add", 1, state_add as H), ("delete", 1, state_delete), ("has", 1, state_has), ("clear", 0, state_clear), ("forEach", 1, state_for_each), ("entries", 0, state_entries)] {
        d.def_method(it, sp, n, len, h)?;
    }
    d.def_get(it, sp, "size", state_size, None)?;
    let values = d.def_method(it, sp, "values", 0, state_values)?;
    let k = d.key(it, "keys")?;
    it.define_data(sp, k, Value::Object(values), true, true, true)?;
    it.define_data(sp, it.symbol_iterator(), Value::Object(values), true, false, true)?;
    let ic = crate::browser_controls::interface_with(d, it, ip, "ElementInternals", internals_ctor)?;
    let sc = crate::browser_controls::interface_with(d, it, sp, "CustomStateSet", states_ctor)?;
    let m = [ip, ic, sp, sc];
    let _ = d.ce.int.set(m);
    Ok(m)
}

fn ensure_internals(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    made(d, it).map(|_| ())
}

crate::browser_lazy::lazy_global!(lazy_internals, "ElementInternals", ensure_internals, |d| d.ce.int.get().expect("made")[1]);
crate::browser_lazy::lazy_global!(lazy_states, "CustomStateSet", ensure_internals, |d| d.ce.int.get().expect("made")[3]);

/// The lazy globals `ElementInternals` and `CustomStateSet`, for `browser_custom::install`.
pub(crate) const LAZY: [(&str, H); 2] = [("ElementInternals", lazy_internals), ("CustomStateSet", lazy_states)];
