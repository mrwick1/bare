//! The form pseudo-classes: `:disabled`, `:enabled`, `:required`, `:optional`, `:read-only`, `:read-write`,
//! `:placeholder-shown`, `:checked`, `:default` and `:indeterminate`. The rules are the ones of the HTML Standard ("Selectors"),
//! and the answers are the ones of Chrome. The matcher reads attributes and the tree. Two things come from the host through
//! `Document::ui`: the state that a script or the user set (checkedness, value, the `indeterminate` property).
//!
//! Left out: the `form` attribute (the form owner is the nearest `form` ancestor), the selectedness of an `option` that script
//! changed (`:checked` and `:default` read the `selected` attribute), and `contenteditable` on a shadow tree.

use bare_html::dom::{
    Document, NodeData, NodeId, Ns, UI_CHECKED, UI_CHECKED_DIRTY, UI_INDETERMINATE, UI_VALUE_DIRTY,
    UI_VALUE_SET,
};

/// A form pseudo-class.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Form {
    Disabled,
    Enabled,
    Required,
    Optional,
    ReadOnly,
    ReadWrite,
    PlaceholderShown,
    Checked,
    Default,
    Indeterminate,
}

pub fn matches(doc: &Document, el: NodeId, form: Form) -> bool {
    match form {
        Form::Disabled => disabled(doc, el) == Some(true),
        Form::Enabled => disabled(doc, el) == Some(false),
        Form::Required => required(doc, el),
        Form::Optional => {
            is_any(doc, el, &["input", "select", "textarea", "button"]) && !required(doc, el)
        }
        Form::ReadWrite => read_write(doc, el),
        Form::ReadOnly => doc.element_ns(el) == Ns::Html && !read_write(doc, el),
        Form::PlaceholderShown => placeholder_shown(doc, el),
        Form::Checked => checked(doc, el, true),
        Form::Default => default(doc, el),
        Form::Indeterminate => indeterminate(doc, el),
    }
}

fn has(doc: &Document, el: NodeId, name: &str) -> bool {
    doc.find_atom(name)
        .is_some_and(|a| doc.attr(el, a).is_some())
}

fn is_any(doc: &Document, el: NodeId, names: &[&str]) -> bool {
    names.iter().any(|n| doc.is_html(el, n))
}

/// The `type` of an `input`, in lowercase; a missing or unknown one is `text`.
fn kind(doc: &Document, el: NodeId) -> String {
    let t = doc
        .find_atom("type")
        .and_then(|a| doc.attr(el, a))
        .unwrap_or("")
        .to_ascii_lowercase();
    const KINDS: [&str; 21] = [
        "search",
        "tel",
        "url",
        "email",
        "password",
        "date",
        "month",
        "week",
        "time",
        "datetime-local",
        "number",
        "range",
        "color",
        "checkbox",
        "radio",
        "file",
        "submit",
        "image",
        "reset",
        "button",
        "hidden",
    ];
    if KINDS.contains(&&*t) {
        t
    } else {
        "text".into()
    }
}

const TEXTY: [&str; 12] = [
    "text",
    "search",
    "tel",
    "url",
    "email",
    "password",
    "date",
    "month",
    "week",
    "time",
    "datetime-local",
    "number",
];

/// `Some(true)` for a control that is disabled, `Some(false)` for one that is enabled, `None` for an element that is neither.
fn disabled(doc: &Document, el: NodeId) -> Option<bool> {
    if doc.is_html(el, "optgroup") {
        return Some(has(doc, el, "disabled"));
    }
    if doc.is_html(el, "option") {
        return Some(
            has(doc, el, "disabled")
                || doc
                    .parent(el)
                    .is_some_and(|p| doc.is_html(p, "optgroup") && has(doc, p, "disabled")),
        );
    }
    if !is_any(
        doc,
        el,
        &["button", "input", "select", "textarea", "fieldset"],
    ) {
        return None;
    }
    if has(doc, el, "disabled") {
        return Some(true);
    }
    // A disabled `fieldset` ancestor disables the control, except inside its first `legend` child.
    let mut from = el;
    while let Some(p) = doc.parent(from) {
        if doc.is_html(p, "fieldset")
            && has(doc, p, "disabled")
            && !(doc.is_html(from, "legend")
                && doc.children(p).find(|&c| doc.is_html(c, "legend")) == Some(from))
        {
            return Some(true);
        }
        from = p;
    }
    Some(false)
}

fn required(doc: &Document, el: NodeId) -> bool {
    has(doc, el, "required")
        && (doc.is_html(el, "select")
            || doc.is_html(el, "textarea")
            || doc.is_html(el, "input")
                && !matches!(
                    &*kind(doc, el),
                    "hidden" | "range" | "color" | "submit" | "image" | "reset" | "button"
                ))
}

/// Is `el`, or the nearest ancestor with a valid `contenteditable`, editable?
fn editable(doc: &Document, el: NodeId) -> bool {
    let Some(a) = doc.find_atom("contenteditable") else {
        return false;
    };
    let mut at = Some(el);
    while let Some(e) = at {
        match doc.attr(e, a).map(str::to_ascii_lowercase).as_deref() {
            Some("" | "true" | "plaintext-only") => return true,
            Some("false") => return false,
            _ => at = doc.parent(e),
        }
    }
    false
}

fn read_write(doc: &Document, el: NodeId) -> bool {
    if doc.is_html(el, "input") {
        return TEXTY.contains(&&*kind(doc, el))
            && !has(doc, el, "readonly")
            && disabled(doc, el) == Some(false);
    }
    if doc.is_html(el, "textarea") {
        return !has(doc, el, "readonly") && disabled(doc, el) == Some(false);
    }
    doc.element_ns(el) == Ns::Html && !is_any(doc, el, &["select", "button"]) && editable(doc, el)
}

fn placeholder_shown(doc: &Document, el: NodeId) -> bool {
    let input = doc.is_html(el, "input");
    if !(input
        && matches!(
            &*kind(doc, el),
            "text" | "search" | "tel" | "url" | "email" | "password" | "number"
        )
        || doc.is_html(el, "textarea"))
        || !has(doc, el, "placeholder")
    {
        return false;
    }
    let ui = doc.ui(el);
    if ui & UI_VALUE_DIRTY != 0 {
        return ui & UI_VALUE_SET == 0;
    }
    if input {
        return doc
            .find_atom("value")
            .and_then(|a| doc.attr(el, a))
            .is_none_or(str::is_empty);
    }
    doc.children(el).all(|c| match *doc.data(c) {
        NodeData::Text(s) => doc.text(s).is_empty(),
        _ => true,
    })
}

/// The checkedness of a checkbox or radio, or (with `live` false) its default: the `checked` attribute.
fn checked(doc: &Document, el: NodeId, live: bool) -> bool {
    if doc.is_html(el, "option") {
        return has(doc, el, "selected") || live && first_enabled_option(doc, el);
    }
    if !doc.is_html(el, "input") || !matches!(&*kind(doc, el), "checkbox" | "radio") {
        return false;
    }
    let ui = doc.ui(el);
    if live && ui & UI_CHECKED_DIRTY != 0 {
        ui & UI_CHECKED != 0
    } else {
        has(doc, el, "checked")
    }
}

/// Is `el` the option that a single select with no `selected` option selects: its first option that is not disabled?
fn first_enabled_option(doc: &Document, el: NodeId) -> bool {
    let Some(select) = doc.parent(el).and_then(|p| if doc.is_html(p, "optgroup") { doc.parent(p) } else { Some(p) }).filter(|&p| doc.is_html(p, "select")) else { return false };
    let size = doc.find_atom("size").and_then(|a| doc.attr(select, a)).and_then(|v| v.trim().parse::<i32>().ok()).unwrap_or(0);
    if has(doc, select, "multiple") || size > 1 {
        return false;
    }
    let mut at = select;
    while let Some(n) = next(doc, select, at) {
        at = n;
        if doc.is_html(n, "option") && (has(doc, n, "selected") || disabled(doc, n) == Some(false)) {
            return n == el;
        }
    }
    false
}

/// The next node after `at` in tree order, inside `root`.
fn next(doc: &Document, root: NodeId, at: NodeId) -> Option<NodeId> {
    if let Some(c) = doc.children(at).next() {
        return Some(c);
    }
    let mut at = at;
    while at != root {
        if let Some(s) = doc.next_sibling(at) {
            return Some(s);
        }
        at = doc.parent(at)?;
    }
    None
}

fn owner(doc: &Document, el: NodeId) -> Option<NodeId> {
    let mut at = doc.parent(el);
    while let Some(p) = at {
        if doc.is_html(p, "form") {
            return Some(p);
        }
        at = doc.parent(p);
    }
    None
}

fn submit_button(doc: &Document, el: NodeId) -> bool {
    if doc.is_html(el, "input") {
        return matches!(&*kind(doc, el), "submit" | "image");
    }
    doc.is_html(el, "button")
        && !matches!(
            doc.find_atom("type")
                .and_then(|a| doc.attr(el, a))
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str(),
            "button" | "reset"
        )
}

fn default(doc: &Document, el: NodeId) -> bool {
    if checked(doc, el, false) {
        return true;
    }
    if !submit_button(doc, el) {
        return false;
    }
    let Some(form) = owner(doc, el) else {
        return false;
    };
    let mut at = form;
    while let Some(n) = next(doc, form, at) {
        if submit_button(doc, n) {
            return n == el;
        }
        at = n;
    }
    false
}

fn indeterminate(doc: &Document, el: NodeId) -> bool {
    if doc.is_html(el, "progress") {
        return !has(doc, el, "value");
    }
    if !doc.is_html(el, "input") {
        return false;
    }
    match &*kind(doc, el) {
        "checkbox" => doc.ui(el) & UI_INDETERMINATE != 0,
        "radio" => {
            if checked(doc, el, true) {
                return false;
            }
            let name = doc
                .find_atom("name")
                .and_then(|a| doc.attr(el, a))
                .unwrap_or("");
            if name.is_empty() {
                return true;
            }
            // The group: the radios with this name that have the same form owner.
            let (form, mut at) = (owner(doc, el), doc.root());
            let top = form.unwrap_or(at);
            if form.is_some() {
                at = top;
            }
            while let Some(n) = next(doc, top, at) {
                at = n;
                if doc.is_html(n, "input")
                    && kind(doc, n) == "radio"
                    && owner(doc, n) == form
                    && doc.find_atom("name").and_then(|a| doc.attr(n, a)) == Some(name)
                    && checked(doc, n, true)
                {
                    return false;
                }
            }
            true
        }
        _ => false,
    }
}
