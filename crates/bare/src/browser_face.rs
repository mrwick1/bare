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

use crate::eng::{Eng, Ops};
use crate::browser_sources::Roots;
use crate::browser::*;
use crate::browser_custom::st;
use crate::browser_scoped::*;
use crate::browser_web::Ent;
use bare_html::dom::{Document, NodeId, PRECUSTOMIZED};
use bare_js_api::{Args, Local, Type};

/// What `setFormValue` kept.
pub(crate) enum FVal<E: Eng> {
    S(String),
    B(E::Obj),
    List(Vec<(String, Ent<E>)>),
}

/// The state of one form-associated element.
pub(crate) struct Face<E: Eng> {
    pub(crate) el: NodeId,
    /// The form owner and the `disabled` state that the callbacks last told.
    pub(crate) owner: Option<NodeId>,
    pub(crate) disabled: bool,
    value: Option<FVal<E>>,
    /// The flags and the message of `setValidity`, in the bits of `browser_controls.rs`.
    flags: u16,
    msg: String,
    ints: Option<E::Obj>,
    states: Option<E::Obj>,
    role: Option<String>,
}

impl<E: Eng> Face<E> {
    pub(crate) fn new(el: NodeId) -> Face<E> {
        Face { el, owner: None, disabled: false, value: None, flags: 0, msg: String::new(), ints: None, states: None, role: None }
    }

    pub(crate) fn roots(&self, out: &mut Roots<'_, '_, E>) {
        match &self.value {
            Some(FVal::B(o)) => out.push(E::obj_raw(*o)),
            Some(FVal::List(l)) => out.extend(l.iter().filter_map(|(_, e)| if let Ent::B(o) = e { Some(E::obj_raw(*o)) } else { None })),
            _ => {}
        }
        out.extend(self.states.map(E::obj_raw));
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

impl<E: Eng> Dom<E> {
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
    pub(crate) fn face_entries(&self, n: NodeId, name: &str) -> Option<Vec<(String, Option<Ent<E>>)>> {
        if self.is_face(n) { self.face_entries_of(n, name) } else { None }
    }

    fn face_entries_of(&self, n: NodeId, name: &str) -> Option<Vec<(String, Option<Ent<E>>)>> {
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
fn target<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, states: bool) -> Res<'s, NodeId> {
    match obj_id::<E>(s, this).and_then(|o| st(d).borrow().ints.get(&o).copied()) {
        Some((n, k)) if k == states => Ok(n),
        _ => Err(s.type_error("Illegal invocation")),
    }
}

/// The element of `this`, which must be a form-associated custom element. `what` starts the message: `Failed to execute 'setFormValue' on`.
fn face_target<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, what: &str) -> Res<'s, NodeId> {
    let n = target(d, s, this, false)?;
    if d.is_face(n) {
        return Ok(n);
    }
    Err(d.dom_err_s(s, "NotSupportedError", &format!("{what} 'ElementInternals': The target element is not a form-associated custom element.")))
}

fn read_member<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, name: &str) -> Res<'s, NodeId> {
    face_target(d, s, this, &format!("Failed to read the '{name}' property from"))
}

fn call_member<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, name: &str) -> Res<'s, NodeId> {
    face_target(d, s, this, &format!("Failed to execute '{name}' on"))
}

fn args_needed<'s, E: Eng>(s: &mut Sc<'s, E>, a: Args<'s>, what: &str) -> Res<'s, ()> {
    if a.is_empty() {
        return Err(s.type_error(&format!("Failed to execute '{what}': 1 argument required, but only 0 present.")));
    }
    Ok(())
}

/// `HTMLElement.prototype.attachInternals()`.
pub(crate) fn attach_internals<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    let fail = |s: &mut Sc<'s, E>, m: &str| d.dom_err_s(s, "NotSupportedError", &format!("Failed to execute 'attachInternals' on 'HTMLElement': {m}"));
    let Some(no_internals) = d.ce_def(n) else { return Err(fail(s, "Unable to attach ElementInternals to non-custom elements.")) };
    if no_internals {
        return Err(fail(s, "ElementInternals is disabled by disabledFeature static field."));
    }
    if st(d).borrow().face.get(&(n.index() as u32)).is_some_and(|f| f.ints.is_some()) {
        return Err(fail(s, "ElementInternals for the specified element was already attached."));
    }
    let proto = old::<E, _>(s, |it| made(d, it))?[0];
    let o = d.obj_s(s, proto)?;
    let l = d.obj_local(s, o);
    let mut sc = st(d).borrow_mut();
    sc.face.entry(n.index() as u32).or_insert_with(|| Face::new(n)).ints = Some(o);
    sc.ints.insert(o, (n, false));
    Ok(l)
}

fn form<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = read_member(d, s, this, "form")?;
    // An upgrade has no form owner until its constructor returned.
    let f = if d.doc.borrow().custom_state(n) == Some(PRECUSTOMIZED) { None } else { old::<E, _>(s, |it| d.form_owner(it, n))? };
    d.wrap_opt_s(s, f)
}

fn labels<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = read_member(d, s, this, "labels")?;
    crate::browser_controls::labels_of(d, s, n)
}

fn will_validate<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = read_member(d, s, this, "willValidate")?;
    let w = old::<E, _>(s, |it| d.will_validate(it, n))?;
    Ok(s.boolean(w))
}

fn validity<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = read_member(d, s, this, "validity")?;
    crate::browser_controls::validity_of(d, s, n)
}

fn validation_message<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = read_member(d, s, this, "validationMessage")?;
    let m = old::<E, _>(s, |it| crate::browser_controls::message_of(d, it, n))?;
    d.js_str_s(s, &m)
}

fn check_validity<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = call_member(d, s, this, "checkValidity")?;
    crate::browser_submit::report_one(d, s, n, false)
}

fn report_validity<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = call_member(d, s, this, "reportValidity")?;
    crate::browser_submit::report_one(d, s, n, true)
}

fn set_form_value<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    args_needed::<E>(s, a, "setFormValue' on 'ElementInternals")?;
    let a0 = arg_s::<E>(s, a, 0);
    let value = if is_nullish::<E>(s, a0) {
        None
    } else {
        let raw = raw_arg::<E>(s, a, 0).expect("one argument");
        Some(if let Some(l) = crate::browser_web::form_entries(d, raw) {
            FVal::List(l)
        } else if crate::browser_web::file_name(d, raw).is_some() {
            FVal::B(obj_id::<E>(s, a0).expect("a File is an object"))
        } else {
            FVal::S(d.string_arg_s(s, a0)?)
        })
    };
    let n = call_member(d, s, this, "setFormValue")?;
    st(d).borrow_mut().face.entry(n.index() as u32).or_insert_with(|| Face::new(n)).value = value;
    Ok(s.undefined())
}

const FLAGS: [&str; 10] = ["valueMissing", "typeMismatch", "patternMismatch", "tooLong", "tooShort", "rangeUnderflow", "rangeOverflow", "stepMismatch", "badInput", "customError"];

fn set_validity<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    args_needed::<E>(s, a, "setValidity' on 'ElementInternals")?;
    let mut flags = 0u16;
    let a0 = arg_s::<E>(s, a, 0);
    if !is_nullish::<E>(s, a0) {
        if !is_object::<E>(s, a0) {
            return Err(s.type_error("Failed to execute 'setValidity' on 'ElementInternals': The provided value is not of type 'ValidityStateFlags'."));
        }
        for (i, name) in FLAGS.iter().enumerate() {
            let v = s.get_str(a0, name)?;
            if s.to_boolean(v) {
                flags |= 1 << i;
            }
        }
    }
    let a1 = arg_s::<E>(s, a, 1);
    let msg = match s.type_of(a1) {
        Type::Undefined => String::new(),
        _ => d.string_arg_s(s, a1)?,
    };
    let a2 = arg_s::<E>(s, a, 2);
    let anchor = if is_nullish::<E>(s, a2) {
        None
    } else {
        match d.node_of_s(s, a2).filter(|&n| d.is_element(n)) {
            Some(n) => Some(n),
            None => return Err(s.type_error("Failed to execute 'setValidity' on 'ElementInternals': parameter 3 is not of type 'HTMLElement'.")),
        }
    };
    let n = call_member(d, s, this, "setValidity")?;
    if flags != 0 && msg.is_empty() {
        return Err(s.type_error("Failed to execute 'setValidity' on 'ElementInternals': The second argument should not be empty if one or more flags in the first argument are true."));
    }
    if let Some(x) = anchor {
        let doc = d.doc.borrow();
        let mut cur = Some(x);
        while let Some(c) = cur.filter(|&c| c != n) {
            s.charge(1)?;
            cur = doc.parent_or_host(c);
        }
        if cur.is_none() {
            drop(doc);
            return Err(d.dom_err_s(s, "NotFoundError", "Failed to execute 'setValidity' on 'ElementInternals': The Element argument should be a shadow-including inclusive descendant of the target element."));
        }
    }
    let mut sc = st(d).borrow_mut();
    let f = sc.face.entry(n.index() as u32).or_insert_with(|| Face::new(n));
    (f.flags, f.msg) = (flags, if flags == 0 { String::new() } else { msg });
    Ok(s.undefined())
}

fn shadow_root<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = target(d, s, this, false)?;
    let r = d.doc.borrow().shadow_root(n);
    d.wrap_opt_s(s, r)
}

fn role<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = target(d, s, this, false)?;
    let r = st(d).borrow().face.get(&(n.index() as u32)).and_then(|f| f.role.clone());
    match r {
        Some(r) => d.js_str_s(s, &r),
        None => Ok(s.null()),
    }
}

fn set_role<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    args_needed::<E>(s, a, "role' on 'ElementInternals")?;
    let a0 = arg_s::<E>(s, a, 0);
    let v = match s.type_of(a0) {
        Type::Null => None,
        _ => Some(d.string_arg_s(s, a0)?),
    };
    let n = target(d, s, this, false)?;
    st(d).borrow_mut().face.entry(n.index() as u32).or_insert_with(|| Face::new(n)).role = v;
    Ok(s.undefined())
}

fn states<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = target(d, s, this, false)?;
    let cached = st(d).borrow().face.get(&(n.index() as u32)).and_then(|f| f.states);
    if let Some(o) = cached {
        return Ok(d.obj_local(s, o));
    }
    let proto = old::<E, _>(s, |it| made(d, it))?[2];
    let o = d.obj_s(s, proto)?;
    let l = d.obj_local(s, o);
    let mut sc = st(d).borrow_mut();
    sc.face.entry(n.index() as u32).or_insert_with(|| Face::new(n)).states = Some(o);
    sc.ints.insert(o, (n, true));
    Ok(l)
}

// ------------------------------------------------------------------------------------------ CustomStateSet

fn state_arg<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>, what: &str) -> Res<'s, String> {
    args_needed::<E>(s, a, &format!("{what}' on 'CustomStateSet"))?;
    d.arg_str_s(s, a, 0)
}

fn state_add<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let v = state_arg(d, s, a, "add")?;
    let n = target(d, s, this, true)?;
    let mut all = d.doc.borrow().states(n).to_vec();
    if !all.iter().any(|x| **x == *v) {
        all.push(v.into());
        d.doc.borrow_mut().set_states(n, all);
    }
    Ok(s.undefined())
}

fn state_delete<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let v = state_arg(d, s, a, "delete")?;
    let n = target(d, s, this, true)?;
    let mut all = d.doc.borrow().states(n).to_vec();
    let Some(i) = all.iter().position(|x| **x == *v) else { return Ok(s.boolean(false)) };
    all.remove(i);
    d.doc.borrow_mut().set_states(n, all);
    Ok(s.boolean(true))
}

fn state_has<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let v = state_arg(d, s, a, "has")?;
    let n = target(d, s, this, true)?;
    let has = d.doc.borrow().states(n).iter().any(|x| **x == *v);
    Ok(s.boolean(has))
}

fn state_clear<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = target(d, s, this, true)?;
    if !d.doc.borrow().states(n).is_empty() {
        d.doc.borrow_mut().set_states(n, Vec::new());
    }
    Ok(s.undefined())
}

fn state_size<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = target(d, s, this, true)?;
    let len = d.doc.borrow().states(n).len();
    Ok(s.number(len as f64))
}

/// The states of the set `this`, as strings.
fn state_list<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, Vec<Local<'s>>> {
    let n = target(d, s, this, true)?;
    let all = d.doc.borrow().states(n).to_vec();
    all.iter().map(|x| d.js_str_s(s, x)).collect()
}

/// An iterator over `items`: the one of a new array.
fn iterate<'s, E: Eng>(s: &mut Sc<'s, E>, items: &[Local<'s>]) -> Res<'s, Local<'s>> {
    let arr = s.new_array(items)?;
    let f = s.get_str(arr, "values")?;
    s.call(f, arr, &[])
}

fn state_values<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let items = state_list(d, s, this)?;
    iterate::<E>(s, &items)
}

fn state_entries<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let items = state_list(d, s, this)?;
    let pairs = items.iter().map(|&v| s.new_array(&[v, v])).collect::<Res<'s, Vec<_>>>()?;
    iterate::<E>(s, &pairs)
}

fn state_for_each<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    args_needed::<E>(s, a, "forEach' on 'CustomStateSet")?;
    let items = state_list(d, s, this)?;
    let cb = arg_s::<E>(s, a, 0);
    if !s.is_callable(cb) {
        return Err(s.type_error("Failed to execute 'forEach' on 'CustomStateSet': The callback provided as parameter 1 is not a function."));
    }
    let this_arg = arg_s::<E>(s, a, 1);
    for v in items {
        s.charge(1)?;
        s.call(cb, this_arg, &[v, v, this])?;
    }
    Ok(s.undefined())
}

// ------------------------------------------------------------------------------------------ the interfaces

fn internals_ctor<'s, E: Eng>(_: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Err(s.type_error("Failed to construct 'ElementInternals': Illegal constructor"))
}

fn states_ctor<'s, E: Eng>(_: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Err(s.type_error("Failed to construct 'CustomStateSet': Illegal constructor"))
}

/// The prototype and the constructor of `ElementInternals`, then those of `CustomStateSet`.
fn made<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, [E::Obj; 4]> {
    if let Some(&m) = d.ce.int.get() {
        return Ok(m);
    }
    let op = it.object_prototype();
    let (ip, sp) = (d.obj(it, op)?, d.obj(it, op)?);
    for (n, g) in [("form", form as HS<E>), ("labels", labels), ("willValidate", will_validate), ("validity", validity), ("validationMessage", validation_message), ("states", states), ("shadowRoot", shadow_root)] {
        d.def_get_s(it, ip, n, g, None)?;
    }
    d.def_get_s(it, ip, "role", role, Some(set_role))?;
    for (n, len, h) in [("checkValidity", 0, check_validity as HS<E>), ("reportValidity", 0, report_validity), ("setFormValue", 1, set_form_value), ("setValidity", 1, set_validity)] {
        d.def_method_s(it, ip, n, len, h)?;
    }
    for (n, len, h) in [("add", 1, state_add as HS<E>), ("delete", 1, state_delete), ("has", 1, state_has), ("clear", 0, state_clear), ("forEach", 1, state_for_each), ("entries", 0, state_entries)] {
        d.def_method_s(it, sp, n, len, h)?;
    }
    d.def_get_s(it, sp, "size", state_size, None)?;
    let values = d.def_method_s(it, sp, "values", 0, state_values)?;
    let k = d.key(it, "keys")?;
    it.define_data(sp, k, E::obj_raw(values), true, true, true)?;
    it.define_data(sp, it.symbol_iterator(), E::obj_raw(values), true, false, true)?;
    let ic = crate::browser_controls::interface_with(d, it, ip, "ElementInternals", internals_ctor)?;
    let sc = crate::browser_controls::interface_with(d, it, sp, "CustomStateSet", states_ctor)?;
    let m = [ip, ic, sp, sc];
    let _ = d.ce.int.set(m);
    Ok(m)
}

fn ensure_internals<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    made(d, it).map(|_| ())
}

crate::browser_lazy::lazy_global!(lazy_internals, "ElementInternals", ensure_internals, |d| d.ce.int.get().expect("made")[1]);
crate::browser_lazy::lazy_global!(lazy_states, "CustomStateSet", ensure_internals, |d| d.ce.int.get().expect("made")[3]);

/// The lazy globals `ElementInternals` and `CustomStateSet`, for `browser_custom::install`.
pub(crate) const fn lazy<E: Eng>() -> [Row<E>; 2] {
    [Row::both(1, "ElementInternals", lazy_internals), Row::both(1, "CustomStateSet", lazy_states)]
}
