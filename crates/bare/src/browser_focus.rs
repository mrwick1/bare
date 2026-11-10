//! DOM focus: `HTMLElement.focus()` and `blur()`, `Document.activeElement`, `FocusEvent`, and the driver calls
//! `Browser::focus`, `Browser::blur` and `Browser::active_element`.
//!
//! The focused element is native state (`FocusState`), not a JS property. Events go through the ordinary dispatch of
//! `browser_events.rs` (capture, target, bubble, listener error reports, driver checkpoints). Nothing here runs JS
//! while a document or state borrow is held.
//!
//! Event order when focus moves from A to B (UI Events 3.3.2, HTML "focus update steps"): `blur` at A (relatedTarget
//! B), `focusout` at A (B), then `focus` at B (relatedTarget A), `focusin` at B (A). `focus` and `blur` do not bubble,
//! `focusin` and `focusout` do. None is cancelable. All are composed. `isTrusted` is false. With no old element the
//! related target of `focus` is null. `blur()` fires `blur` and `focusout` with a null related target. The window gets
//! no extra `blur` or `focus`: the page is always taken as having system focus.
//!
//! The focus update is applied between the events: focus is cleared before `blur` runs, so `activeElement` is the
//! body inside a `blur` listener, and a `focus()` call there is not a nested blur of the same element. (The HTML text
//! sets the focused area only in the "new chain" loop. This local policy keeps re-entry finite; browser equivalence is unverified.)
//! If a listener moves focus while a transition runs, the rest of that transition stops: the later focus wins.
//! `focusin` is skipped when the element lost focus in its own `focus` listener. A transition that nests deeper than
//! `MAX_DEPTH` ends the call with `Limit("dom-focus-depth")`. A fatal error stops the transition: no later event runs.
//!
//! Focus fixup (HTML "focus fixup rule"): when the focused element is no longer a focusable area (removed from the
//! document, disabled, `hidden`, `inert`, ...), the focused area is the viewport and `activeElement` is the body. No
//! `blur` or `focusout` is fired. (Only the move of focus that follows, while the element is still connected, sends them to it: Chrome does.) The check runs when the focus state is read (`activeElement`, `focus`, `blur`, the
//! driver), not at each mutation: an element that was disabled and enabled again with no read in between stays focused.
//!
//! Focusable area policy (explicit, from the DOM and the UA stylesheet facts that exist here). An element is a
//! focusable area when it is an HTML element, is connected to the document, and:
//! - no inclusive ancestor has the `hidden` or `inert` attribute (`hidden=until-found` too);
//! - it is not inside a `<details>` without `open`, except the first `<summary>` child;
//! - a `button`, `input`, `select` or `textarea` has no `disabled` attribute and is not inside a disabled
//!   `<fieldset>` (except inside its first `<legend>`);
//! - and it is one of: `a` with `href`, `button`, `select`, `textarea`, `input` that is not `type=hidden`, the first
//!   `summary` of a `details`, any element with a `tabindex` attribute that parses as an integer (a negative one is
//!   focusable by script only), or an element with `contenteditable` of `""`, `true` or `plaintext-only`.
//!
//! Not modeled (gaps, none is faked): CSS-dependent rendering (`display:none`, `visibility:hidden`, `content-visibility`,
//! zero-size boxes): the browser host has no cascade access, so author stylesheets and inline `style` never make an
//! element unfocusable. Also absent: `iframe`, `area`, `audio`/`video` controls, scroll containers, shadow hosts and
//! `delegatesFocus`, `<dialog>` modality, `autofocus`, the `change` event of an uncommitted edit on blur, window
//! `focus`/`blur`, `:focus` and `:focus-visible` matching, `FocusEvent.view`, selection and caret
//! effects, Tab navigation, and focus following a click.
//!
//! `focus(options)`: `preventScroll` is read and accepted. No scroll state exists, so nothing is scrolled either way.
//! `focusVisible` is `Unsupported` (nothing reads `:focus-visible`). A non-object, non-null option is a `TypeError`.

use crate::browser::*;
use crate::browser_dom as nd;
use crate::browser_events::init_flag;
use bare_html::dom::{NodeData, NodeId};
use bare_js::eval::{Error, Interp};
use bare_js::value::Value;

type V = R<Value>;

/// The deepest nest of focus transitions (a listener that moves focus again, and so on).
const MAX_DEPTH: u8 = 16;

#[derive(Default)]
pub(crate) struct FocusState {
    pub node: Option<NodeId>,
    /// The text control that holds the page selection: the last one that took focus. A blur and the focus of another kind of element leave it
    /// there; a press on the page takes it away. Chrome sends `selectionchange` to a control that takes the page selection
    /// (`Dom::focus_selection`, `tools/fixtures/ui/chrome-sel.txt`).
    pub sel_owner: Option<NodeId>,
    /// Raised at each change of the focused element. A transition that sees a new value stops.
    pub epoch: u64,
    depth: u8,
}

#[cfg(test)]
impl FocusState {
    pub(crate) fn depth_for_test(&self) -> u8 {
        self.depth
    }
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

/// HTML "rules for parsing integers", only the part that says valid or not: spaces, a sign, then a digit.
fn tabindex_valid(s: &str) -> bool {
    let t = s.trim_start_matches([' ', '\t', '\n', '\x0c', '\r']);
    t.strip_prefix(['-', '+']).unwrap_or(t).starts_with(|c: char| c.is_ascii_digit())
}

impl Dom {
    /// Is `n` a focusable area now? Walks the ancestors: one step is charged for each, and for each sibling looked at.
    pub(crate) fn focusable(&self, it: &mut Interp<'_>, n: NodeId) -> R<bool> {
        let doc = self.doc.borrow();
        let tag = |e: NodeId| match *doc.data(e) {
            NodeData::Element { name, .. } if !doc.is_foreign(e) => doc.atom_str(name),
            _ => "",
        };
        let attr = |e: NodeId, a: &str| doc.find_atom(a).and_then(|x| doc.attr(e, x));
        let t = tag(n);
        if t.is_empty() {
            return Ok(false);
        }
        let control = matches!(t, "button" | "input" | "select" | "textarea");
        if control && attr(n, "disabled").is_some() {
            return Ok(false);
        }
        // The first child element that is named `name`.
        let first = |it: &mut Interp<'_>, parent: NodeId, name: &str| -> R<Option<NodeId>> {
            for c in doc.children(parent) {
                it.charge(1)?;
                if tag(c) == name {
                    return Ok(Some(c));
                }
            }
            Ok(None)
        };
        let (mut cur, mut below) = (n, None);
        loop {
            it.charge(1)?;
            if attr(cur, "hidden").is_some() || attr(cur, "inert").is_some() {
                return Ok(false);
            }
            if let Some(b) = below {
                match tag(cur) {
                    "fieldset" if control && attr(cur, "disabled").is_some() && first(it, cur, "legend")? != Some(b) => return Ok(false),
                    "details" if attr(cur, "open").is_none() && first(it, cur, "summary")? != Some(b) => return Ok(false),
                    _ => {}
                }
            }
            match doc.parent(cur) {
                Some(p) => (cur, below) = (p, Some(cur)),
                None => break,
            }
        }
        if cur != doc.root() {
            return Ok(false);
        }
        let by_tag = match t {
            "a" => attr(n, "href").is_some(),
            "button" | "select" | "textarea" => true,
            "input" => !attr(n, "type").is_some_and(|v| v.eq_ignore_ascii_case("hidden")),
            "summary" => match doc.parent(n) {
                Some(p) if tag(p) == "details" => first(it, p, "summary")? == Some(n),
                _ => false,
            },
            _ => false,
        };
        Ok(by_tag
            || attr(n, "tabindex").is_some_and(tabindex_valid)
            || attr(n, "contenteditable").is_some_and(|v| ["", "true", "plaintext-only"].iter().any(|k| v.eq_ignore_ascii_case(k))))
    }

    fn set_focus(&self, n: Option<NodeId>) {
        let by_pointer = {
            let mut st = self.st.borrow_mut();
            st.focus.node = n;
            st.focus.epoch += 1;
            st.ui.by_pointer
        };
        // `:focus-visible` (Chrome): a text field always shows its ring; another element only after a key or a script, not after a press.
        let mut doc = self.doc.borrow_mut();
        let text = |n: NodeId| doc.is_html(n, "textarea") || (doc.is_html(n, "input") && matches!(crate::browser_dom::input_kind(&doc, n), "text" | "search" | "tel" | "url" | "email" | "password" | "number"));
        let visible = !by_pointer || n.is_some_and(text);
        doc.set_focus(n, visible);
    }

    fn epoch(&self) -> u64 {
        self.st.borrow().focus.epoch
    }

    /// The focused element after the focus fixup rule. `None` means the viewport.
    pub(crate) fn current_focus(&self, it: &mut Interp<'_>) -> R<Option<NodeId>> {
        let Some(n) = self.st.borrow().focus.node else { return Ok(None) };
        if self.focusable(it, n)? {
            return Ok(Some(n));
        }
        self.set_focus(None);
        Ok(None)
    }

    fn fire_focus(&self, it: &mut Interp<'_>, target: NodeId, ty: &str, bubbles: bool, related: Option<NodeId>) -> R<()> {
        let ev = self.new_event_proto(it, ty, bubbles, false, true, self.p().focus)?;
        {
            let mut st = self.st.borrow_mut();
            let trusted = st.ui.active;
            let e = st.events.get_mut(&ev).expect("just made");
            (e.is_focus, e.trusted) = (true, trusted);
        }
        if let Some(r) = related {
            let v = self.wrap(it, r)?;
            self.st.borrow_mut().events.get_mut(&ev).expect("just made").related = v;
        }
        self.dispatch(it, Target::Node(target), ev).map(|_| ())
    }

    /// Move focus to `to` (already checked as focusable) or to the viewport (`None`).
    pub(crate) fn move_focus(&self, it: &mut Interp<'_>, to: Option<NodeId>) -> R<()> {
        {
            let mut st = self.st.borrow_mut();
            if st.focus.depth >= MAX_DEPTH {
                drop(st);
                return Err(self.lim("dom-focus-depth"));
            }
            st.focus.depth += 1;
        }
        let r = self.move_focus_inner(it, to);
        self.st.borrow_mut().focus.depth -= 1;
        r
    }

    fn move_focus_inner(&self, it: &mut Interp<'_>, to: Option<NodeId>) -> R<()> {
        // An element that is still connected but cannot take focus now (hidden, disabled) still gets its `blur` (Chrome 155, checked with
        // `tools/fixtures/ui/drive.html`): the fixup rule does not run before this move. A removed element does not (see the fixup above).
        let raw = self.st.borrow().focus.node;
        let old = match raw {
            Some(n) if self.is_connected_node(it, n)? => raw,
            _ => self.current_focus(it)?,
        };
        if old == to {
            return Ok(());
        }
        if let Some(o) = old {
            self.set_focus(None);
            let e = self.epoch();
            self.fire_focus(it, o, "blur", false, to)?;
            if self.epoch() != e {
                return Ok(());
            }
            self.fire_focus(it, o, "focusout", true, to)?;
            if self.epoch() != e {
                return Ok(());
            }
        }
        let Some(b) = to else { return Ok(()) };
        if !self.focusable(it, b)? {
            return Ok(());
        }
        self.set_focus(Some(b));
        self.focus_selection(b)?;
        let e = self.epoch();
        self.fire_focus(it, b, "focus", false, old)?;
        if self.current_focus(it)? == Some(b) && self.epoch() == e {
            self.fire_focus(it, b, "focusin", true, old)?;
        }
        Ok(())
    }

    /// The focusing steps for `n`: nothing happens when it is not a focusable area.
    pub(crate) fn focus_node(&self, it: &mut Interp<'_>, n: NodeId) -> R<()> {
        if self.focusable(it, n)? {
            self.move_focus(it, Some(n))?;
        }
        Ok(())
    }
}

fn html_el(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
    let n = d.nd(it, this)?;
    if d.is_element(n) && !d.doc.borrow().is_foreign(n) { Ok(n) } else { Err(it.type_error("Illegal invocation")) }
}

pub(crate) fn focus(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = html_el(d, it, this)?;
    match arg(a, 0) {
        Value::Undefined | Value::Null => {}
        o @ Value::Object(_) => {
            init_flag(d, it, o, "preventScroll")?;
            let k = d.key(it, "focusVisible")?;
            if !matches!(it.get_value(o, k)?, Value::Undefined) {
                return Err(Error::Unsupported(unsupported("focus option focusVisible (no :focus-visible state)")));
            }
        }
        _ => return Err(it.type_error("Failed to execute 'focus' on 'HTMLElement': parameter 1 is not of type 'FocusOptions'.")),
    }
    d.focus_node(it, n)?;
    Ok(Value::Undefined)
}

pub(crate) fn blur(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = html_el(d, it, this)?;
    if d.current_focus(it)? == Some(n) {
        d.move_focus(it, None)?;
    }
    Ok(Value::Undefined)
}

pub(crate) fn active_element(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    if d.nd(it, this)? != d.doc.borrow().root() {
        return Err(it.type_error("Illegal invocation"));
    }
    if let Some(n) = d.current_focus(it)? {
        return d.wrap(it, n);
    }
    match nd::body(d, it, this, &[])? {
        Value::Null => nd::document_element(d, it, this, &[]),
        body => Ok(body),
    }
}

pub(crate) fn related_target(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let o = d.event_of(it, this)?;
    if !d.st.borrow().events[&o].is_focus {
        return Err(it.type_error("Illegal invocation"));
    }
    Ok(d.st.borrow().events[&o].related)
}

pub(crate) fn focus_event_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    if a.is_empty() {
        return Err(it.type_error("Failed to construct 'FocusEvent': 1 argument required, but only 0 present."));
    }
    let ty = d.string_arg(it, a[0])?;
    let init = arg(a, 1);
    let (mut bubbles, mut cancelable, mut composed, mut related) = (false, false, false, Value::Null);
    match init {
        Value::Undefined | Value::Null => {}
        Value::Object(_) => {
            bubbles = init_flag(d, it, init, "bubbles")?;
            cancelable = init_flag(d, it, init, "cancelable")?;
            composed = init_flag(d, it, init, "composed")?;
            let k = d.key(it, "relatedTarget")?;
            match it.get_value(init, k)? {
                Value::Undefined | Value::Null => {}
                v @ Value::Object(o) if d.st.borrow().by_obj.contains_key(&o) => related = v,
                _ => return Err(it.type_error("Failed to construct 'FocusEvent': Failed to read the 'relatedTarget' property from 'FocusEventInit': The provided value is not of type 'EventTarget'.")),
            }
        }
        _ => return Err(it.type_error("The event initializer is not an object.")),
    }
    let proto = it.host_proto_from_new_target(d.p().focus)?;
    let e = d.new_event_proto(it, &ty, bubbles, cancelable, composed, proto)?;
    {
        let mut st = d.st.borrow_mut();
        let event = st.events.get_mut(&e).expect("just made");
        event.related = related;
        event.is_focus = true;
    }
    Ok(Value::Object(e))
}

// ------------------------------------------------------------------------------------------ the driver

/// What a driver focus call did.
#[derive(Debug)]
pub struct FocusOutcome {
    /// The element is the active element after the call.
    pub focused: bool,
    /// Listener exceptions of this call (also kept for `take_reports`).
    pub reports: Vec<Report>,
}

impl Browser {
    /// Run `f` as a driver call: a microtask checkpoint follows each listener it starts.
    pub(crate) fn driven<T>(&mut self, f: impl FnOnce(&Dom, &mut Interp<'_>) -> R<T>) -> R<(T, Vec<Report>)> {
        let dom = self.dom.clone();
        let mark = {
            let st = dom.st.borrow();
            st.dropped_reports + st.reports.len() as u64
        };
        dom.st.borrow_mut().driver = true;
        let r = self.it.host_scope(|it| f(&dom, it));
        dom.st.borrow_mut().driver = false;
        let v = r?;
        let st = dom.st.borrow();
        let new = (((st.dropped_reports + st.reports.len() as u64) - mark) as usize).min(st.reports.len());
        Ok((v, st.reports.iter().skip(st.reports.len() - new).cloned().collect()))
    }

    /// The focusing steps for an element, as `el.focus()` runs them, through the ordinary event engine.
    /// An ineligible element changes nothing (`focused` is false). A node that is not an element is `Unsupported`.
    pub fn focus(&mut self, node: NodeId) -> R<FocusOutcome> {
        if !self.dom.is_element(node) {
            return Err(Error::Unsupported(unsupported("test driver: focus needs an element")));
        }
        let (focused, reports) = self.driven(|d, it| {
            d.focus_node(it, node)?;
            Ok(d.current_focus(it)? == Some(node))
        })?;
        Ok(FocusOutcome { focused, reports })
    }

    /// The unfocusing steps: blur the active element, if there is one (`focused` is false afterwards).
    pub fn blur(&mut self) -> R<FocusOutcome> {
        let (_, reports) = self.driven(|d, it| {
            if d.current_focus(it)?.is_some() {
                d.move_focus(it, None)?;
            }
            Ok(())
        })?;
        Ok(FocusOutcome { focused: false, reports })
    }

    /// The focused element, or `None` when the viewport is focused (`document.activeElement` is then the body).
    pub fn active_element(&mut self) -> R<Option<NodeId>> {
        let dom = self.dom.clone();
        self.it.host_scope(|it| dom.current_focus(it))
    }
}
