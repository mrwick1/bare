//! The `details` element: `ToggleEvent` and the toggle of the `open` attribute. Sources: HTML Standard "The details element"
//! (attribute change steps, "details notification task steps"), "The summary element" (activation behavior), "Interface
//! ToggleEvent".
//!
//! What is built
//! - A window click, Enter, or Space on the first `summary` child of a `details` toggles the `open` attribute of the `details`, unless
//!   a listener canceled the click. A click inside interactive content of the summary (a link, a button, a field) has that content
//!   as its activation target (`activation_target`), so it does not reach the summary.
//! - A change of `open` by that activation or by the `details.open` setter queues one `toggle` task. The event is a `ToggleEvent`
//!   (does not bubble, not cancelable) with `oldState` and `newState` set to `closed` or `open`. A task that is still queued for
//!   the same `details` is removed first and its old state is kept, as in the standard.
//!
//! Gaps: `setAttribute`, `removeAttribute`, `toggleAttribute` and the parser change `open` with no `toggle` event. The `name`
//! attribute (exclusive accordion) does not close the other `details` of its group. `HTMLElement.click()` on a summary stays gated.

use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, Local};
use crate::browser_reflect::this_of;
use bare_html::dom::NodeId;
use bare_js::eval::Interp;
use bare_js::value::{Value};

type V = R<Value>;

pub(crate) fn toggle_event_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    if a.is_empty() {
        return Err(it.type_error("Failed to construct 'ToggleEvent': 1 argument required, but only 0 present."));
    }
    let ty = d.string_arg(it, a[0])?;
    let (mut bubbles, mut cancelable, mut composed) = (false, false, false);
    let (mut old, mut new) = (String::new(), String::new());
    match a.get(1).copied().unwrap_or(Value::Undefined) {
        Value::Undefined | Value::Null => {}
        init @ Value::Object(_) => {
            bubbles = crate::browser_events::init_flag(d, it, init, "bubbles")?;
            cancelable = crate::browser_events::init_flag(d, it, init, "cancelable")?;
            composed = crate::browser_events::init_flag(d, it, init, "composed")?;
            for (name, out) in [("oldState", &mut old), ("newState", &mut new)] {
                let k = d.key(it, name)?;
                match it.get_value(init, k)? {
                    Value::Undefined => {}
                    v => *out = d.string_arg(it, v)?,
                }
            }
        }
        _ => return Err(it.type_error("The event initializer is not an object.")),
    }
    let proto = it.host_proto_from_new_target(d.p().toggle)?;
    let e = d.new_event_proto(it, &ty, bubbles, cancelable, composed, proto)?;
    d.st.borrow_mut().events.get_mut(&e).expect("just made").toggle = Some((old, new));
    Ok(Value::Object(e))
}

fn state<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, new: bool) -> Res<'s, Local<'s>> {
    let Some(o) = obj_id(s, this).filter(|o| d.st.borrow().events.contains_key(o)) else {
        return Err(s.type_error("Illegal invocation"));
    };
    let st = d.st.borrow().events[&o].toggle.as_ref().map(|(old, n)| if new { n.clone() } else { old.clone() });
    match st {
        Some(st) => d.js_str_s(s, &st),
        None => Err(s.type_error("Illegal invocation")),
    }
}

pub(crate) fn old_state<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    state(d, s, this, false)
}

pub(crate) fn new_state<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    state(d, s, this, true)
}

pub(crate) fn open_get<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = this_of(d, s, this, &["details"])?;
    Ok(s.boolean(crate::browser_reflect::text(d, n, "open").is_some()))
}

pub(crate) fn open_set<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = this_of(d, s, this, &["details"])?;
    let arg0 = arg_s(s, a, 0);
    let on = s.to_boolean(arg0);
    d.set_details_open(s, n, on)?;
    Ok(s.undefined())
}

impl Dom {
    /// Set or remove `open` on the `details` `n`, and queue the `toggle` event when the state changed.
    pub(crate) fn set_details_open<'s>(&self, s: &mut Sc<'s, '_>, n: NodeId, on: bool) -> Res<'s, ()> {
        let was = crate::browser_reflect::text(self, n, "open").is_some();
        if was == on {
            return Ok(());
        }
        if on {
            crate::browser_dom::attr_set_s(self, s, n, "open", "")?;
        } else {
            self.doc.borrow_mut().remove_attr(n, "open");
        }
        lift(s, self.queue_toggle(n, was, on))
    }

    /// HTML "details notification task steps": a queued task of `n` goes, and its old state stays.
    fn queue_toggle(&self, n: NodeId, mut old: bool, new: bool) -> R<()> {
        let pending = self.sched.borrow_mut().toggles.remove(&n);
        if let Some((key, o)) = pending {
            old = o;
            self.unschedule(key);
        }
        let key = self.schedule(self.now_ms(), crate::browser_scheduler::Task::Toggle(n, old, new))?;
        self.sched.borrow_mut().toggles.insert(n, (key, old));
        Ok(())
    }

    pub(crate) fn toggle_task(&self, it: &mut Interp<'_>, n: NodeId, old: bool, new: bool) -> R<()> {
        let ev = self.new_event_proto(it, "toggle", false, false, false, self.p().toggle)?;
        {
            let mut st = self.st.borrow_mut();
            let e = st.events.get_mut(&ev).expect("just made");
            let s = |b: bool| (if b { "open" } else { "closed" }).to_string();
            e.toggle = Some((s(old), s(new)));
            e.trusted = true;
        }
        self.dispatch(it, Target::Node(n), ev).map(|_| ())
    }

    /// The activation behavior of a `summary` (HTML "the summary element"): when it is the first `summary` child of a `details`,
    /// toggle the `details`.
    pub(crate) fn summary_activate(&self, it: &mut Interp<'_>, s: NodeId) -> R<()> {
        let details = {
            let doc = self.doc.borrow();
            let p = doc.parent(s).filter(|&p| doc.is_html(p, "details"));
            p.filter(|&p| doc.children(p).find(|&c| doc.is_html(c, "summary")) == Some(s))
        };
        match details {
            Some(p) => {
                it.charge(1)?;
                let on = crate::browser_reflect::text(self, p, "open").is_none();
                run(it, |s| self.set_details_open(s, p, on))
            }
            None => Ok(()),
        }
    }
}
