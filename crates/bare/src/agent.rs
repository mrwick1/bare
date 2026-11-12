//! Events for a driver that stands in for the person at the window: the dialogs of the page and the tabs that it opens.
//!
//! A driver (`bare-mcp`) calls `set_policy` before it acts. From then on `alert`, `confirm` and `prompt` do not block: each one is
//! answered at once with the policy and recorded as an `Event::Dialog`. `window.open(url)` and a click on a link with `target=_blank`
//! record an `Event::Tab` with the absolute URL, and the page stays where it is (`window.open` still gives `null`). The driver reads
//! the events with `take`, which also ends the policy.
//!
//! Without a policy nothing is recorded and the old behavior stays: `alert` does nothing, `confirm` gives false, `prompt` gives null,
//! `window.open` gives null, and a `_blank` link navigates in place. The state is per thread: a page and its driver call share one.
//! `beforeunload` has no entry here: no navigation of this engine dispatches it, so it cannot block.

use std::cell::RefCell;

/// How many events wait for the driver. Later ones are dropped: a page that loops on `alert` costs no memory.
const MAX_EVENTS: usize = 32;
/// The longest message or text kept, in characters.
const MAX_TEXT: usize = 300;

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// `kind` is `alert`, `confirm` or `prompt`. `text` is the answer of an accepted `prompt`.
    Dialog { kind: &'static str, message: String, accepted: bool, text: Option<String> },
    /// The absolute URL that a page asked to open in a new tab.
    Tab(String),
}

#[derive(Clone)]
struct Policy {
    accept: bool,
    text: Option<String>,
}

thread_local! {
    static POLICY: RefCell<Option<Policy>> = const { RefCell::new(None) };
    static EVENTS: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
}

/// Answer dialogs with accept (`true`) or dismiss. `text` is the answer of an accepted `prompt` (default: the default of the prompt).
pub fn set_policy(accept: bool, text: Option<String>) {
    POLICY.with(|p| *p.borrow_mut() = Some(Policy { accept, text }));
}

/// The events since the last call, and no policy after it.
pub fn take() -> Vec<Event> {
    POLICY.with(|p| *p.borrow_mut() = None);
    EVENTS.with(|e| std::mem::take(&mut *e.borrow_mut()))
}

fn push(e: Event) {
    EVENTS.with(|v| {
        let mut v = v.borrow_mut();
        if v.len() < MAX_EVENTS {
            v.push(e);
        }
    });
}

fn cut(s: &str) -> String {
    s.chars().take(MAX_TEXT).collect()
}

/// A dialog. `None`: there is no policy, so the caller keeps its old result. `Some((accepted, text))` otherwise.
pub(crate) fn dialog(kind: &'static str, message: &str, default: &str) -> Option<(bool, Option<String>)> {
    let p = POLICY.with(|p| p.borrow().clone())?;
    let text = (kind == "prompt" && p.accept).then(|| p.text.unwrap_or_else(|| default.to_string()));
    push(Event::Dialog { kind, message: cut(message), accepted: p.accept, text: text.as_deref().map(cut) });
    Some((p.accept, text))
}

/// A new tab for `url`. False when there is no policy: the caller keeps its old behavior.
pub(crate) fn tab(url: &str) -> bool {
    if POLICY.with(|p| p.borrow().is_none()) {
        return false;
    }
    push(Event::Tab(url.to_string()));
    true
}
